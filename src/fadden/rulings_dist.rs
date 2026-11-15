// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

//! The `rulings` runs a corpus holds under `rulings/<run>/`, as `dist` publishes
//! them and `dist_verify` checks them. A published run keeps the stage's
//! `rulings.jsonl`, `manifest.json` and `LICENCE-NOTICE.md`; its `raw/` pages stay
//! behind, as the EPUBs do, because their ATO site header and footer were never
//! scanned. `rulings/sources.json` gives each run's file digests and the
//! documents left out. A document the ATO's reuse notice does not cover, or an
//! edited version of private advice, is left out and listed; a row that names a
//! person beside a registration number or holds a contact stops the
//! distribution, as an unapproved contact in a title does. No allowlist entry
//! approves a contact in a ruling.
use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use super::compilations::names;
use super::corpus_paths::{child, linked};
use super::pii;
use super::rulings::{COPYRIGHT_NOTICE_URL, EDITED_VERSION, NOTICE, family_of, licence_notice};
use super::search::check_rulings_run;
use super::{Exc, json_error};
use crate::pyjson::{self, Json, PyStr};
use crate::pyos;
use crate::pypath;
use crate::pyre;
use crate::pyvalue::dumps_sorted;

mod verify;
pub use verify::verify;

/// The folder under a corpus root, and so under a distribution.
pub const FOLDER: &str = "rulings";

/// The heading of LICENCE-NOTICE.md's section on the published rulings.
pub const NOTICE_HEADING: &str = "## ATO rulings";

/// The heading of REMOVED.md's section on the rulings left out.
const REMOVED_HEADING: &str = "## ATO rulings left out";

/// The files a published run holds, as the rulings stage wrote them.
const RUN_FILES: [&str; 3] = ["LICENCE-NOTICE.md", "manifest.json", "rulings.jsonl"];

/// A run folder name the `aus-accounting-mcp` rulings reader accepts.
const RUN_NAME: &str = r"[A-Za-z0-9][A-Za-z0-9._-]{0,119}";

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
    let text =
        String::from_utf8(read(path)?).map_err(|_| Exc::value(format!("{path} is not UTF-8")))?;
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

/// Why `dist` leaves a captured document out, if it does: a family the ATO's
/// reuse notice does not cover, an edited version of private advice, a label its
/// docid contradicts, a page that did not print the notice, or text carrying a
/// copyright notice of its own.
fn reason_left_out(document: &Json, rows: &[&Json]) -> Option<String> {
    let docid = text(document, "docid");
    let Some(family) = family_of(docid) else {
        return Some(
            "not an ATO-authored document family; the ATO notice does not cover it".into(),
        );
    };
    if family == EDITED_VERSION {
        return Some(
            "an edited version of private advice, about one taxpayer's affairs; not republished"
                .into(),
        );
    }
    let labels = std::iter::once(document).chain(rows.iter().copied());
    if let Some(label) = labels.map(|r| text(r, "family")).find(|l| *l != family) {
        return Some(format!(
            "labelled {label:?}, but its docid is a {family}; capture it again"
        ));
    }
    // A `site-notice-via-dc-rights` page only links to a copyright page, and that
    // link's target, read on 15 November 2026, no longer resolves.
    let bases = std::iter::once(document).chain(rows.iter().copied());
    if bases
        .map(|r| text(r, "licence_basis"))
        .any(|b| b != "document-notice")
    {
        return Some("its page did not print the ATO's reuse notice".into());
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
/// a JSON escape could hide. The allowlist is not consulted: a run can be named
/// like a title it approves contacts for.
fn private(path: &str, run: &str) -> Result<bool, Exc> {
    let found = |(pair, contacts): (bool, Vec<pii::Unapproved>)| pair || !contacts.is_empty();
    if found(pii::privacy_findings_in_file(path, run, &[])?) {
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
            .is_ok_and(|value| found(pii::privacy_findings_in_row(&value, run, &[])))
    }))
}

/// Copy each run under the corpus's `rulings/` into `staging`, leaving out and
/// listing what the ATO notice does not cover. None when the corpus holds none.
pub fn copy(root: &str, staging: &str) -> Result<Option<Published>, Exc> {
    let folder = pypath::join(root, FOLDER);
    if absent(&folder)? {
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
        if !pyre::fullmatch(RUN_NAME, 0, &name) || linked(&source)? || !pyos::isdir(&source) {
            return Err(Exc::Runtime(format!(
                "rulings/{name} is not a run directory the rulings reader accepts"
            )));
        }
        let run_target = pypath::join(&target, &name);
        listed.push(copy_run(&source, &run_target, &name, &mut published)?);
    }
    if listed.is_empty() {
        return Err(Exc::Runtime("rulings holds no run".into()));
    }
    let path = pypath::join(&target, "sources.json");
    write_new(&path, &json_bytes(&listing(listed)))?;
    if private(&path, FOLDER)? {
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
    let kept = kept_lines(&lines, &rows, &left_out, name)?;
    // The stage's notice as it writes it, whatever a hand edit added to the copy.
    let contents = [
        licence_notice().into_bytes(),
        json_bytes(&manifest),
        kept.into_bytes(),
    ];
    let files = write_run(target, name, &contents)?;
    published
        .runs
        .push((name.to_string(), text(&manifest, "fetched_on").to_string()));
    Ok(listing_entry(
        name,
        &manifest,
        Json::Object(files.into_iter().collect()),
    ))
}

/// `rulings/sources.json`, holding the entries of `runs`.
fn listing(runs: Vec<Json>) -> Json {
    Json::obj([
        ("source", Json::str("ATO Legal Database")),
        ("reuse_notice", Json::str(NOTICE)),
        ("reuse_notice_url", Json::str(COPYRIGHT_NOTICE_URL)),
        ("runs", Json::Array(runs)),
    ])
}

/// A run's entry in `rulings/sources.json`, from its published manifest: every
/// document left out, by the stage or by `dist`, is the manifest's `excluded`.
fn listing_entry(name: &str, manifest: &Json, files: Json) -> Json {
    Json::obj([
        ("name", Json::str(name)),
        ("fetched_on", Json::str(text(manifest, "fetched_on"))),
        (
            "documents",
            Json::Int(iterate_or_empty(manifest.at("documents")).len().into()),
        ),
        ("rows", manifest.at("rows").clone()),
        ("files", files),
        ("left_out", manifest.at("excluded").clone()),
    ])
}

/// The run's lines for the documents kept, each row's decoded values scanned first.
fn kept_lines(
    lines: &[&str],
    rows: &[Json],
    left_out: &BTreeSet<String>,
    name: &str,
) -> Result<String, Exc> {
    let mut kept = String::new();
    for (line, row) in lines.iter().zip(rows) {
        let docid = text(row, "docid");
        if left_out.contains(docid) {
            continue;
        }
        let (pair, contacts) = pii::privacy_findings_in_row(row, docid, &[]);
        if pair || !contacts.is_empty() {
            return Err(Exc::Runtime(format!(
                "rulings/{name}: {docid} names a private person or holds a contact; \
                 remove the run or capture the document again once it is clean"
            )));
        }
        kept.push_str(line);
        kept.push('\n');
    }
    Ok(kept)
}

/// Write a run's files, `contents` in `RUN_FILES` order, and scan each written
/// file; returns their digests.
fn write_run(target: &str, name: &str, contents: &[Vec<u8>; 3]) -> Result<Vec<(PyStr, Json)>, Exc> {
    pyos::makedirs_new(target)?;
    let mut files = Vec::new();
    for (file, bytes) in RUN_FILES.into_iter().zip(contents) {
        let path = pypath::join(target, file);
        write_new(&path, bytes)?;
        if private(&path, name)? {
            return Err(Exc::Runtime(format!(
                "rulings/{name}/{file} names a private person or holds a contact"
            )));
        }
        files.push((PyStr::Text(file.into()), Json::str(&sha256(bytes))));
    }
    Ok(files)
}

/// Move each uncovered document from the manifest's `documents` to its
/// `excluded`, with the reason, and recount its rows. Returns their docids.
fn leave_out(
    manifest: &mut Json,
    rows: &[Json],
    name: &str,
    published: &mut Published,
) -> BTreeSet<String> {
    let documents = iterate_or_empty(manifest.at("documents")).to_vec();
    let mut excluded = iterate_or_empty(manifest.at("excluded")).to_vec();
    let (mut kept, mut left_out) = (Vec::new(), BTreeSet::new());
    for document in documents {
        let docid = text(&document, "docid").to_string();
        let own: Vec<&Json> = rows.iter().filter(|r| text(r, "docid") == docid).collect();
        match reason_left_out(&document, &own) {
            None => kept.push(document),
            Some(reason) => {
                excluded.push(Json::obj([
                    ("docid", Json::str(&docid)),
                    ("reason", Json::str(&reason)),
                ]));
                left_out.insert(docid);
            }
        }
    }
    for entry in &excluded {
        let (docid, reason) = (text(entry, "docid"), text(entry, "reason"));
        published
            .left_out
            .push((name.to_string(), docid.to_string(), reason.to_string()));
    }
    let count = rows
        .iter()
        .filter(|r| !left_out.contains(text(r, "docid")))
        .count();
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

/// Whether `path` does not exist; any other failure to read its metadata is an error.
fn absent(path: &str) -> Result<bool, Exc> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(pyos::OsError::new(&e, Some(path), pyos::Style::Native).into()),
    }
}

fn iterate_or_empty(value: &Json) -> &[Json] {
    match value {
        Json::Array(items) => items,
        _ => &[],
    }
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
         printed the ATO's copyright notice ({COPYRIGHT_NOTICE_URL}) when it was\n\
         captured:\n\n\
         > {NOTICE}\n\n\
         Neither the ATO nor the Commonwealth endorses this corpus. The rows are not the\n\
         authorised text, advice or the Commissioner's current view: read each document\n\
         at its `source_url` before relying on it. Legislation, court judgments, edited\n\
         versions of private advice and documents whose text carries a copyright notice\n\
         of its own are left out, and REMOVED.md lists each one. `rulings/sources.json`\n\
         gives each run's file digests.\n",
        dates.join(", ")
    )
}

/// REMOVED.md's section on the rulings left out, one line each.
pub fn removed_lines(published: &Published) -> Vec<String> {
    let mut lines = vec![String::new(), REMOVED_HEADING.into(), String::new()];
    if published.left_out.is_empty() {
        lines.push("No captured document was left out.".into());
        return lines;
    }
    lines.push("| Run | Document | Reason |".into());
    lines.push("|---|---|---|".into());
    for (run, docid, reason) in &published.left_out {
        lines.push(removed_line(run, docid, reason));
    }
    lines
}

/// `REMOVED.md`'s table row for one document left out, on one line.
fn removed_line(run: &str, docid: &str, reason: &str) -> String {
    let cell = |value: &str| value.replace('|', r"\|").replace(['\r', '\n'], " ");
    format!("| {run} | {} | {} |", cell(docid), cell(reason))
}
