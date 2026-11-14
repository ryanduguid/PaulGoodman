// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

//! The `rulings` runs a corpus holds under `rulings/<run>/`, as `dist` publishes
//! them and `dist_verify` checks them. A published run keeps the stage's
//! `rulings.jsonl`, `manifest.json` and `LICENCE-NOTICE.md`; its `raw/` pages stay
//! behind, as the EPUBs do, because their ATO site header and footer were never
//! scanned. `rulings/sources.json` gives each run's file digests and the
//! documents left out. A document the ATO's reuse notice does not cover is left
//! out and listed; a row that names a person beside a registration number or
//! holds a contact stops the distribution, as an unapproved contact in a title
//! does.
use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use super::compilations::{Check, names};
use super::corpus_paths::{child, linked};
use super::dist_verify::entry_kind;
use super::pii::{self, Approved};
use super::rulings::{COPYRIGHT_NOTICE_URL, NOTICE, family_of};
use super::search::{LICENCE_BASES, check_rulings_run};
use super::{Exc, json_error};
use crate::pyjson::{self, Json, PyStr};
use crate::pyos;
use crate::pypath;
use crate::pyre;
use crate::pyvalue::dumps_sorted;

/// The folder under a corpus root, and so under a distribution.
pub const FOLDER: &str = "rulings";

/// The heading of LICENCE-NOTICE.md's section on the published rulings.
pub const NOTICE_HEADING: &str = "## ATO rulings";

/// The files a published run holds, as the rulings stage wrote them.
const RUN_FILES: [&str; 3] = ["LICENCE-NOTICE.md", "manifest.json", "rulings.jsonl"];

/// A run folder name the `aus-accounting-mcp` rulings reader accepts.
const RUN_NAME: &str = r"^[A-Za-z0-9][A-Za-z0-9._-]{0,119}$";

/// Text marking material with a copyright notice of its own, which the ATO's
/// notice does not license.
const OWN_COPYRIGHT: &str = r"©|\(c\)\s*\d{4}|\bcopyright\s+(?:\(c\)|\d{4})|all rights reserved";

/// What `dist` published: each run's name and capture date, and the documents
/// it left out as `(run, docid, reason)`.
#[derive(Default)]
pub struct Published {
    pub runs: Vec<(String, String)>,
    pub left_out: Vec<(String, String, String)>,
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn read(path: &str) -> Result<Vec<u8>, Exc> {
    std::fs::read(path).map_err(|e| pyos::OsError::new(&e, Some(path), pyos::Style::Native).into())
}

fn load(path: &str) -> Result<Json, Exc> {
    let text = String::from_utf8(read(path)?)
        .map_err(|_| Exc::value(format!("{path} is not UTF-8")))?;
    pyjson::loads_standard(&text).map_err(json_error)
}

fn write_new(path: &str, bytes: &[u8]) -> Result<(), Exc> {
    use std::io::Write;
    let mut file = pyos::create_new(path, false)?;
    file.write_all(bytes)
        .map_err(|e| pyos::OsError::new(&e, Some(path), pyos::Style::Native).into())
}

/// The JSON as the rulings stage writes its manifest: sorted, indented, one newline.
fn json_bytes(value: &Json) -> Vec<u8> {
    let mut text: String = dumps_sorted(value, Some(2))
        .into_iter()
        .filter_map(char::from_u32)
        .collect();
    text.push('\n');
    text.into_bytes()
}

fn text<'a>(value: &'a Json, key: &str) -> &'a str {
    value.at(key).as_str().unwrap_or_default()
}

/// Why the ATO's reuse notice does not cover a captured document, if it does not:
/// a family it does not cover, a label its docid contradicts, no recorded notice,
/// or text carrying a copyright notice of its own.
pub fn uncovered(document: &Json, rows: &[&Json]) -> Option<String> {
    let docid = text(document, "docid");
    let Some(family) = family_of(docid) else {
        return Some("not an ATO-authored document family; the ATO notice does not cover it".into());
    };
    let labels = std::iter::once(document).chain(rows.iter().copied());
    if let Some(label) = labels.map(|r| text(r, "family")).find(|l| *l != family) {
        return Some(format!(
            "labelled {label:?}, but its docid is a {family}; capture it again"
        ));
    }
    let bases = std::iter::once(document).chain(rows.iter().copied());
    if bases.map(|r| text(r, "licence_basis")).any(|b| !LICENCE_BASES.contains(&b)) {
        return Some("records no ATO reuse notice".into());
    }
    let marked = pyre::re(OWN_COPYRIGHT, pyre::I);
    let texts = std::iter::once(text(document, "title")).chain(rows.iter().flat_map(|row| {
        ["title", "heading", "text", "withdrawal_notice"].map(|key| text(row, key))
    }));
    for found in texts {
        if marked.is_match(found) {
            return Some(
                "marked with a copyright notice of its own; the ATO notice does not cover it"
                    .into(),
            );
        }
    }
    None
}

/// Whether `path` names a person beside a registration number or holds a
/// contact, in the file as written or, for JSON and JSONL, in the decoded values
/// a JSON escape could hide. No allowlist entry binds a ruling, so every contact
/// is unapproved.
fn private(path: &str, run: &str, approved: &[Approved]) -> Result<bool, Exc> {
    let found = |(pair, contacts): (bool, Vec<pii::Unapproved>)| pair || !contacts.is_empty();
    if found(pii::privacy_findings_in_file(path, run, approved)?) {
        return Ok(true);
    }
    let text = crate::pyio::read_text(path)?;
    let values: Vec<&str> = if path.ends_with(".jsonl") {
        text.lines().collect()
    } else if path.ends_with(".json") {
        vec![text.as_str()]
    } else {
        Vec::new()
    };
    Ok(values.into_iter().any(|value| {
        pyjson::loads_standard(value)
            .is_ok_and(|value| found(pii::privacy_findings_in_row(&value, run, approved)))
    }))
}

/// Copy each run under the corpus's `rulings/` into `staging`, leaving out and
/// listing what the ATO notice does not cover. None when the corpus holds none.
pub fn copy(root: &str, staging: &str, approved: &[Approved]) -> Result<Option<Published>, Exc> {
    let folder = pypath::join(root, FOLDER);
    if std::fs::symlink_metadata(&folder).is_err() {
        return Ok(None);
    }
    if linked(&folder)? || !pyos::isdir(&folder) {
        return Err(Exc::Runtime("rulings is not a real directory".into()));
    }
    let target = pypath::join(staging, FOLDER);
    pyos::makedirs_new(&target)?;
    let mut published = Published::default();
    let mut listed = Vec::new();
    for name in names(&folder)? {
        let source = pypath::join(&folder, &name);
        if !pyre::re(RUN_NAME, 0).is_match(&name) || linked(&source)? || !pyos::isdir(&source) {
            return Err(Exc::Runtime(format!(
                "rulings/{name} is not a run directory the rulings reader accepts"
            )));
        }
        let run_target = pypath::join(&target, &name);
        listed.push(copy_run(&source, &run_target, &name, approved, &mut published)?);
    }
    if listed.is_empty() {
        return Err(Exc::Runtime("rulings holds no run".into()));
    }
    let listing = Json::obj([
        ("source", Json::str("ATO Legal Database")),
        ("reuse_notice", Json::str(NOTICE)),
        ("reuse_notice_url", Json::str(COPYRIGHT_NOTICE_URL)),
        ("runs", Json::Array(listed)),
    ]);
    let path = pypath::join(&target, "sources.json");
    write_new(&path, &json_bytes(&listing))?;
    if private(&path, FOLDER, approved)? {
        return Err(Exc::Runtime(
            "rulings/sources.json names a private person or holds a contact".into(),
        ));
    }
    Ok(Some(published))
}

/// One run: checked as `search build --rulings` checks it, its uncovered
/// documents left out, its rows scanned, then written and scanned again.
/// Returns its listing for `rulings/sources.json`.
fn copy_run(
    source: &str,
    target: &str,
    name: &str,
    approved: &[Approved],
    published: &mut Published,
) -> Result<Json, Exc> {
    if let Some(stray) = names(source)?
        .into_iter()
        .find(|n| !RUN_FILES.contains(&n.as_str()) && n != "raw")
    {
        return Err(Exc::Runtime(format!(
            "rulings/{name}/{stray} is not a file the rulings stage writes"
        )));
    }
    check_rulings_run(source)?;
    let mut manifest = load(&child(source, &["manifest.json"])?)?;
    let jsonl = String::from_utf8(read(&child(source, &["rulings.jsonl"])?)?)
        .map_err(|_| Exc::value(format!("rulings/{name}/rulings.jsonl is not UTF-8")))?;
    let lines: Vec<&str> = jsonl.lines().filter(|l| !l.trim().is_empty()).collect();
    let rows = lines
        .iter()
        .map(|line| pyjson::loads_standard(line).map_err(json_error))
        .collect::<Result<Vec<Json>, Exc>>()?;
    let left_out = leave_out(&mut manifest, &rows, name, published);
    let mut kept = String::new();
    for (line, row) in lines.iter().zip(&rows) {
        let docid = text(row, "docid");
        if left_out.contains(docid) {
            continue;
        }
        let (pair, contacts) = pii::privacy_findings_in_row(row, docid, approved);
        if pair || !contacts.is_empty() {
            return Err(Exc::Runtime(format!(
                "rulings/{name}: {docid} names a private person or holds a contact; \
                 remove the run or capture the document again once it is clean"
            )));
        }
        kept.push_str(line);
        kept.push('\n');
    }
    pyos::makedirs_new(target)?;
    write_new(&pypath::join(target, "rulings.jsonl"), kept.as_bytes())?;
    write_new(&pypath::join(target, "manifest.json"), &json_bytes(&manifest))?;
    let notice = read(&child(source, &["LICENCE-NOTICE.md"])?)?;
    write_new(&pypath::join(target, "LICENCE-NOTICE.md"), &notice)?;
    let mut files = Vec::new();
    for file in RUN_FILES {
        let path = pypath::join(target, file);
        if private(&path, name, approved)? {
            return Err(Exc::Runtime(format!(
                "rulings/{name}/{file} names a private person or holds a contact"
            )));
        }
        files.push((PyStr::Text(file.into()), Json::str(&sha256(&read(&path)?))));
    }
    let fetched_on = text(&manifest, "fetched_on").to_string();
    published.runs.push((name.to_string(), fetched_on.clone()));
    Ok(Json::obj([
        ("name", Json::str(name)),
        ("fetched_on", Json::str(&fetched_on)),
        ("documents", Json::Int(iterate_or_empty(manifest.at("documents")).len().into())),
        ("rows", manifest.at("rows").clone()),
        ("files", Json::Object(files)),
        ("left_out", Json::Array(listed_left_out(&manifest, &left_out))),
    ]))
}

/// Move each uncovered document from the manifest's `documents` to its
/// `excluded`, with the reason, and recount its rows. Returns their docids.
fn leave_out(
    manifest: &mut Json,
    rows: &[Json],
    name: &str,
    published: &mut Published,
) -> BTreeSet<String> {
    let documents = match manifest.at("documents") {
        Json::Array(items) => items.clone(),
        _ => Vec::new(),
    };
    let mut excluded = match manifest.at("excluded") {
        Json::Array(items) => items.clone(),
        _ => Vec::new(),
    };
    let (mut kept, mut left_out) = (Vec::new(), BTreeSet::new());
    for document in documents {
        let docid = text(&document, "docid").to_string();
        let own: Vec<&Json> = rows.iter().filter(|r| text(r, "docid") == docid).collect();
        match uncovered(&document, &own) {
            None => kept.push(document),
            Some(reason) => {
                excluded.push(Json::obj([
                    ("docid", Json::str(&docid)),
                    ("reason", Json::str(&reason)),
                ]));
                published.left_out.push((name.to_string(), docid.clone(), reason));
                left_out.insert(docid);
            }
        }
    }
    let count = rows.iter().filter(|r| !left_out.contains(text(r, "docid"))).count();
    if let Json::Object(pairs) = manifest {
        for (key, value) in pairs.iter_mut() {
            match key.as_str() {
                Some("documents") => *value = Json::Array(kept.clone()),
                Some("excluded") => *value = Json::Array(excluded.clone()),
                Some("rows") => *value = Json::Int(count.into()),
                _ => {}
            }
        }
    }
    left_out
}

/// The manifest's exclusions for the documents `dist` left out.
fn listed_left_out(manifest: &Json, left_out: &BTreeSet<String>) -> Vec<Json> {
    match manifest.at("excluded") {
        Json::Array(items) => items
            .iter()
            .filter(|e| left_out.contains(text(e, "docid")))
            .cloned()
            .collect(),
        _ => Vec::new(),
    }
}

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

/// The checks on a distribution's `rulings/`, or none when it holds none.
pub fn verify(dist_root: &str, approved: &[Approved]) -> Result<Vec<Check>, Exc> {
    let folder = pypath::join(dist_root, FOLDER);
    if std::fs::symlink_metadata(&folder).is_err() {
        return Ok(Vec::new());
    }
    let mut found = Findings::default();
    let listing_path = pypath::join(&folder, "sources.json");
    let runs = if entry_kind(&folder) == "directory" && entry_kind(&listing_path) == "file" {
        listed_runs(&load(&listing_path)?)
    } else {
        found.stray.push(format!("{FOLDER} has no sources.json"));
        Vec::new()
    };
    let present = if entry_kind(&folder) == "directory" { names(&folder)? } else { Vec::new() };
    for entry in &present {
        if entry != "sources.json" && !runs.iter().any(|(n, _)| n == entry) {
            found.stray.push(entry.clone());
        }
    }
    let removed = crate::pyio::read_text(&child(dist_root, &["REMOVED.md"])?)?;
    for (name, run) in &runs {
        if !present.contains(name) {
            found.stray.push(format!("{name} missing"));
            continue;
        }
        verify_run(&pypath::join(&folder, name), name, run, approved, &mut found)?;
        for left in iterate_or_empty(run.at("left_out")) {
            if !removed.contains(text(left, "docid")) {
                found.unlisted.push(format!("{name}/{}", text(left, "docid")));
            }
        }
    }
    if entry_kind(&listing_path) == "file" && private(&listing_path, FOLDER, approved)? {
        found.private.push("sources.json".into());
    }
    let notice = crate::pyio::read_text(&child(dist_root, &["LICENCE-NOTICE.md"])?)?;
    let noticed = notice.contains(&format!("\n{NOTICE_HEADING}\n")) && notice.contains(NOTICE);
    Ok(report(&found, noticed))
}

fn iterate_or_empty(value: &Json) -> &[Json] {
    match value {
        Json::Array(items) => items,
        _ => &[],
    }
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
fn verify_run(
    directory: &str,
    name: &str,
    run: &Json,
    approved: &[Approved],
    found: &mut Findings,
) -> Result<(), Exc> {
    let entries = names(directory)?;
    let regular = entries.iter().all(|e| entry_kind(&pypath::join(directory, e)) == "file");
    if entries != RUN_FILES || !regular {
        found.stray.push(format!("{name}: {}", entries.join(" ")));
        return Ok(());
    }
    for file in RUN_FILES {
        let path = pypath::join(directory, file);
        if run.at("files").at(file).as_str() != Some(sha256(&read(&path)?).as_str()) {
            found.digests.push(format!("{name}/{file}"));
        }
        match private(&path, name, approved) {
            Ok(false) => {}
            Ok(true) => found.private.push(format!("{name}/{file}")),
            Err(error) if pii::unreadable(&error) => {
                found.private.push(format!("{name}/{file} unreadable"));
            }
            Err(error) => return Err(error),
        }
    }
    match check_rulings_run(directory) {
        Ok(()) => {}
        Err(Exc::Value { message, .. }) => found.runs.push(message),
        Err(error) => return Err(error),
    }
    let manifest = load(&pypath::join(directory, "manifest.json"))?;
    let jsonl = String::from_utf8(read(&pypath::join(directory, "rulings.jsonl"))?).unwrap_or_default();
    let rows: Vec<Json> = jsonl.lines().filter_map(|l| pyjson::loads_standard(l).ok()).collect();
    for document in iterate_or_empty(manifest.at("documents")) {
        let docid = text(document, "docid");
        let own: Vec<&Json> = rows.iter().filter(|r| text(r, "docid") == docid).collect();
        if let Some(reason) = uncovered(document, &own) {
            found.uncovered.push(format!("{name}/{docid}: {reason}"));
        }
    }
    Ok(())
}

fn report(found: &Findings, noticed: bool) -> Vec<Check> {
    fn check(label: &'static str, items: &[String]) -> Check {
        let shown: Vec<&str> = items.iter().take(5).map(String::as_str).collect();
        (label, items.is_empty(), shown.join(", "))
    }
    vec![
        check("rulings holds only the listed runs and their files", &found.stray),
        check("rulings files match their listed digests", &found.digests),
        check("no rulings file names people or contacts", &found.private),
        check("each rulings run agrees with its manifest", &found.runs),
        check("every published ruling is under the ATO notice", &found.uncovered),
        check("REMOVED.md lists every ruling left out", &found.unlisted),
        ("licence notice covers the ATO rulings", noticed, String::new()),
    ]
}

/// LICENCE-NOTICE.md's section on the published rulings.
pub fn notice_section(published: &Published) -> String {
    let mut dates: Vec<&str> = published.runs.iter().map(|(_, d)| d.as_str()).collect();
    dates.sort_unstable();
    dates.dedup();
    format!(
        "{NOTICE_HEADING}\n\n\
         The folders under `rulings/` hold paragraphs of ATO Legal Database documents\n\
         captured on {}. They are \u{a9} Australian Taxation Office for the Commonwealth\n\
         of Australia and are not covered by the CC BY 4.0 licence above. Each document\n\
         printed, or linked to, the ATO's copyright notice ({COPYRIGHT_NOTICE_URL})\n\
         when it was captured:\n\n\
         > {NOTICE}\n\n\
         Neither the ATO nor the Commonwealth endorses this corpus. The rows are not the\n\
         authorised text, advice or the Commissioner's current view: read each document\n\
         at its `source_url` before relying on it. Legislation, court judgments and\n\
         material carrying a copyright notice of its own are not included, and REMOVED.md\n\
         lists each document left out. `rulings/sources.json` gives each run's file digests.\n",
        dates.join(", ")
    )
}

/// REMOVED.md's section on the rulings left out, one line each.
pub fn removed_lines(published: &Published) -> Vec<String> {
    let mut lines = vec![String::new(), "## ATO rulings left out".into(), String::new()];
    if published.left_out.is_empty() {
        lines.push("No captured document was left out.".into());
        return lines;
    }
    lines.push("| Run | Document | Reason |".into());
    lines.push("|---|---|---|".into());
    for (run, docid, reason) in &published.left_out {
        lines.push(format!("| {run} | {docid} | {} |", reason.replace('|', "\|")));
    }
    lines
}
