// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `dist_verify`'s checks on a distribution's `rulings/`: only the listed runs
//! and their files, their digests and privacy, each run's agreement with its
//! manifest and its `sources.json` entry, coverage by the ATO notice, and each
//! left-out document's `REMOVED.md` row.
use std::collections::BTreeSet;

use super::{
    FOLDER, NOTICE_HEADING, REMOVED_HEADING, RUN_FILES, absent, iterate_or_empty, listing,
    listing_entry, load, private, read, reason_left_out, removed_line, sha256, text,
};
use crate::fadden::Exc;
use crate::fadden::compilations::{Check, names};
use crate::fadden::corpus_paths::child;
use crate::fadden::dist_verify::entry_kind;
use crate::fadden::pii;
use crate::fadden::rulings::{NOTICE, licence_notice};
use crate::fadden::search::check_rulings_run;
use crate::pyjson::{self, Json, PyStr};
use crate::pypath;
use crate::pyvalue::dumps_sorted;

/// What the checks found under a distribution's `rulings/`.
#[derive(Default)]
struct Findings {
    stray: Vec<String>,
    digests: Vec<String>,
    private: Vec<String>,
    runs: Vec<String>,
    uncovered: Vec<String>,
    unlisted: Vec<String>,
}

/// The checks on a distribution's `rulings/`, or none when it holds none and
/// neither its licence notice nor `REMOVED.md` has a rulings section.
pub fn verify(dist_root: &str) -> Result<Vec<Check>, Exc> {
    let folder = pypath::join(dist_root, FOLDER);
    let section = |file: &str, heading: &str| {
        child(dist_root, &[file])
            .ok()
            .and_then(|path| crate::pyio::read_text(&path).ok())
            .is_some_and(|text| text.contains(&format!("\n{heading}\n")))
    };
    let mut found = Findings::default();
    if absent(&folder)? {
        if !section("LICENCE-NOTICE.md", NOTICE_HEADING) && !section("REMOVED.md", REMOVED_HEADING)
        {
            return Ok(Vec::new());
        }
        found.stray.push(format!("{FOLDER} missing"));
    }
    let listing_path = pypath::join(&folder, "sources.json");
    let runs = read_listing(&folder, &listing_path, &mut found)?;
    let present = present_entries(&folder, &runs, &mut found)?;
    let removed = crate::pyio::read_text(&child(dist_root, &["REMOVED.md"])?)?;
    for (name, run) in &runs {
        if !present.contains(name) {
            found.stray.push(format!("{name} missing"));
            continue;
        }
        for left in iterate_or_empty(run.at("left_out")) {
            let (docid, reason) = (text(left, "docid"), text(left, "reason"));
            let row = removed_line(name, docid, reason);
            if !removed.lines().any(|l| l == row) {
                found.unlisted.push(format!("{name}/{docid}"));
            }
        }
        verify_run(&pypath::join(&folder, name), name, run, &mut found)?;
    }
    if entry_kind(&listing_path) == "file" && private(&listing_path, FOLDER)? {
        found.private.push("sources.json".into());
    }
    let notice = crate::pyio::read_text(&child(dist_root, &["LICENCE-NOTICE.md"])?)?;
    let noticed = notice.contains(&format!("\n{NOTICE_HEADING}\n")) && notice.contains(NOTICE);
    Ok(report(&found, noticed))
}

/// `rulings/sources.json`'s runs, recording a missing listing, a run listed
/// twice and fields other than the runs that differ from those `dist` writes.
fn read_listing(
    folder: &str,
    listing_path: &str,
    found: &mut Findings,
) -> Result<Vec<(String, Json)>, Exc> {
    let mut runs = Vec::new();
    if entry_kind(folder) == "directory" && entry_kind(listing_path) == "file" {
        let listed = load(listing_path)?;
        runs = listed_runs(&listed);
        let names: BTreeSet<&str> = runs.iter().map(|(name, _)| name.as_str()).collect();
        if names.len() != runs.len() {
            found.stray.push("sources.json lists a run twice".into());
        }
        let rebuilt = listing(runs.iter().map(|(_, run)| run.clone()).collect());
        if dumps_sorted(&rebuilt, None) != dumps_sorted(&listed, None) {
            found
                .stray
                .push("sources.json's source or reuse notice".into());
        }
    } else {
        found.stray.push(format!("{FOLDER} has no sources.json"));
    }
    if runs.is_empty() {
        found.stray.push("sources.json lists no run".into());
    }
    Ok(runs)
}

/// The entries of `rulings/`, recording each that is neither its listing nor a
/// listed run.
fn present_entries(
    folder: &str,
    runs: &[(String, Json)],
    found: &mut Findings,
) -> Result<Vec<String>, Exc> {
    if entry_kind(folder) != "directory" {
        return Ok(Vec::new());
    }
    let present = names(folder)?;
    for entry in &present {
        if entry != "sources.json" && !runs.iter().any(|(n, _)| n == entry) {
            found.stray.push(entry.clone());
        }
    }
    Ok(present)
}

/// `rulings/sources.json`'s runs, by name.
fn listed_runs(listing: &Json) -> Vec<(String, Json)> {
    iterate_or_empty(listing.at("runs"))
        .iter()
        .map(|run| (text(run, "name").to_string(), run.clone()))
        .collect()
}

/// One listed run: its files, their digests and privacy, its agreement with its
/// manifest, and the coverage of every document it publishes.
fn verify_run(directory: &str, name: &str, run: &Json, found: &mut Findings) -> Result<(), Exc> {
    let entries = names(directory)?;
    let regular = entries
        .iter()
        .all(|e| entry_kind(&pypath::join(directory, e)) == "file");
    if entries != RUN_FILES || !regular {
        found.stray.push(format!("{name}: {}", entries.join(" ")));
        return Ok(());
    }
    let files = verify_files(directory, name, run, found)?;
    match check_rulings_run(directory) {
        Ok(()) => {}
        Err(Exc::Value { message, .. }) => {
            // Coverage is read from a run that agrees with its manifest.
            found.runs.push(message);
            return Ok(());
        }
        Err(error) => return Err(error),
    }
    let manifest = load(&pypath::join(directory, "manifest.json"))?;
    if read(&pypath::join(directory, "LICENCE-NOTICE.md"))? != licence_notice().into_bytes() {
        found.runs.push(format!(
            "{name}/LICENCE-NOTICE.md is not the stage's notice"
        ));
    }
    // Rebuilt from the published manifest and bytes alone, never from the entry itself.
    let expected = listing_entry(name, &manifest, files);
    if dumps_sorted(&expected, None) != dumps_sorted(run, None) {
        found.runs.push(format!(
            "sources.json's entry for {name} differs from its manifest"
        ));
    }
    let jsonl =
        String::from_utf8(read(&pypath::join(directory, "rulings.jsonl"))?).unwrap_or_default();
    let rows: Vec<Json> = jsonl
        .lines()
        .filter_map(|l| pyjson::loads_standard(l).ok())
        .collect();
    for document in iterate_or_empty(manifest.at("documents")) {
        let docid = text(document, "docid");
        let own: Vec<&Json> = rows.iter().filter(|r| text(r, "docid") == docid).collect();
        if let Some(reason) = reason_left_out(document, &own) {
            found.uncovered.push(format!("{name}/{docid}: {reason}"));
        }
    }
    Ok(())
}

/// Each of a run's files against its listed digest and the privacy patterns;
/// returns the files' digests as `rulings/sources.json` lists them.
fn verify_files(
    directory: &str,
    name: &str,
    run: &Json,
    found: &mut Findings,
) -> Result<Json, Exc> {
    let mut digests = Vec::new();
    for file in RUN_FILES {
        let path = pypath::join(directory, file);
        let digest = sha256(&read(&path)?);
        if run.at("files").at(file).as_str() != Some(digest.as_str()) {
            found.digests.push(format!("{name}/{file}"));
        }
        digests.push((PyStr::Text(file.into()), Json::str(&digest)));
        match private(&path, name) {
            Ok(false) => {}
            Ok(true) => found.private.push(format!("{name}/{file}")),
            Err(error) if pii::unreadable(&error) => {
                found.private.push(format!("{name}/{file} unreadable"));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(Json::Object(digests.into_iter().collect()))
}

fn report(found: &Findings, noticed: bool) -> Vec<Check> {
    fn check(label: &'static str, items: &[String]) -> Check {
        let shown: Vec<&str> = items.iter().take(5).map(String::as_str).collect();
        (label, items.is_empty(), shown.join(", "))
    }
    vec![
        check(
            "rulings holds only the listed runs and their files",
            &found.stray,
        ),
        check("rulings files match their listed digests", &found.digests),
        check("no rulings file names people or contacts", &found.private),
        check("each rulings run agrees with its manifest", &found.runs),
        check(
            "every published ruling is under the ATO notice and not private advice",
            &found.uncovered,
        ),
        check("REMOVED.md lists every ruling left out", &found.unlisted),
        (
            "licence notice covers the ATO rulings",
            noticed,
            String::new(),
        ),
    ]
}
