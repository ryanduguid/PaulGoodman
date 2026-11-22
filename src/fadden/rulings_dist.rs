// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

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

use super::sha256_hex as sha256;

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
/// contact. A JSON or JSONL file whose every value parses with no repeated
/// member and nothing after it is scanned as its decoded values, which a JSON
/// escape cannot hide from, less the stage's machine values (`free_text`). Any
/// other file is scanned as written and in each value that parses. The
/// allowlist is not consulted: a run can be named like a title it approves
/// contacts for.
fn private(path: &str, run: &str) -> Result<bool, Exc> {
    let text = crate::pyio::read_text(path)?;
    let values: Vec<&str> = if path.ends_with(".jsonl") {
        text.lines().filter(|l| !l.trim().is_empty()).collect()
    } else if path.ends_with(".json") {
        vec![text.as_str()]
    } else {
        Vec::new()
    };
    let file = pypath::split(path).1;
    let parsed: Option<Vec<Json>> = values
        .iter()
        .map(|v| pyjson::loads(v).ok().map(|v| free_text(&v, &file)))
        .collect();
    if let Some(parsed) = parsed.filter(|p| !p.is_empty()) {
        // Nothing in the file is left unread, so its values stand for it whole.
        pii::refuse_controls(&text)?;
        return Ok(holds_private(&Json::Array(parsed), run));
    }
    let (pair, contacts) = pii::privacy_findings_in_file(path, run, &[])?;
    Ok(pair
        || !contacts.is_empty()
        || values.into_iter().any(|value| {
            pyjson::loads_standard(value).is_ok_and(|value| holds_private(&value, run))
        }))
}

/// Whether `value`'s decoded keys and values pair a name with a registration
/// number or hold a contact.
fn holds_private(value: &Json, register_id: &str) -> bool {
    let (pair, contacts) = pii::privacy_findings_in_row(value, register_id, &[]);
    pair || !contacts.is_empty()
}

/// A machine value the stage or `dist` writes: a count or a SHA-256 hex digest.
#[derive(Clone, Copy)]
enum Machine {
    Count,
    Digest,
}

impl Machine {
    fn holds(self, value: &Json) -> bool {
        match (self, value) {
            // The stage never writes a negative count.
            (Machine::Count, Json::Int(n)) => n.sign() != num_bigint::Sign::Minus,
            (Machine::Digest, Json::Str(_)) => value.as_str().is_some_and(|hex| {
                hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            }),
            _ => false,
        }
    }
}

/// Where each published file holds a machine value, as member paths from one
/// of its values (`*` is each item of an array): as `rulings::run` writes a
/// manifest and its rows, `leave_out` recounts `rows` and `listing_entry`
/// writes a `sources.json` run.
fn machine_paths(file: &str) -> &'static [(&'static [&'static str], Machine)] {
    use Machine::{Count, Digest};
    match file {
        "manifest.json" => &[
            (&["rows"], Count),
            (&["documents", "*", "bytes"], Count),
            (&["documents", "*", "paragraphs"], Count),
            (&["documents", "*", "windows_1252_bytes"], Count),
            (&["documents", "*", "sha256"], Digest),
        ],
        "rulings.jsonl" => &[(&["source_sha256"], Digest)],
        "sources.json" => &[
            (&["runs", "*", "documents"], Count),
            (&["runs", "*", "rows"], Count),
            (&["runs", "*", "files", "LICENCE-NOTICE.md"], Digest),
            (&["runs", "*", "files", "manifest.json"], Digest),
            (&["runs", "*", "files", "rulings.jsonl"], Digest),
        ],
        _ => &[],
    }
}

/// A value of `file` without the machine values at `machine_paths`, which no
/// person or page wrote: a byte count such as 134620 reads as a 13 phone
/// number. Only a value of the right type at one of those paths goes; every
/// other member, a new or nested one included, stays and is scanned.
fn free_text(value: &Json, file: &str) -> Json {
    fn strip(value: &mut Json, path: &[&str], kind: Machine) {
        match (value, path) {
            (Json::Array(items), ["*", rest @ ..]) => {
                items.iter_mut().for_each(|item| strip(item, rest, kind));
            }
            (Json::Object(members), [key]) => {
                members.retain(|(k, v)| k.as_str() != Some(*key) || !kind.holds(v));
            }
            (Json::Object(members), [key, rest @ ..]) => members
                .iter_mut()
                .filter(|(k, _)| k.as_str() == Some(*key))
                .for_each(|(_, v)| strip(v, rest, kind)),
            _ => {}
        }
    }
    let mut value = value.clone();
    for (path, kind) in machine_paths(file) {
        strip(&mut value, path, *kind);
    }
    value
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
        if holds_private(&free_text(row, "rulings.jsonl"), docid) {
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
         The rows are not the authorised text, advice or the Commissioner's current view:\n\
         read each document at its `source_url` before relying on it. Legislation, court\n\
         judgments, edited versions of private advice and documents whose text carries a\n\
         copyright notice of its own are left out, and REMOVED.md lists each one.\n\
         `rulings/sources.json` gives each run's file digests.\n",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Contacts split across members, rows or a lone carriage return, digits
    /// beside each other across a member boundary, controls that hold a
    /// contact, and count or digest keys away from where the stage writes them
    /// (`HEX` is a digest holding a 13 number, `ESC` a backslash).
    const SCANNED: [(&str, &str); 18] = [
        ("manifest.json", r#"{"a": "Call 13 46 20."}"#),
        ("manifest.json", r#"{"a": "Call 13 46", "b": "20"}"#),
        ("manifest.json", r#"{"a":"13","b":"4620"}"#),
        ("manifest.json", r#"{"a": 13, "b": 4620}"#),
        ("manifest.json", "[13,46,20]"),
        ("manifest.json", "[1300, 123, 456]"),
        ("manifest.json", r#"["0412","345","678"]"#),
        ("manifest.json", r#"{"a": "alice@", "b": "example.org"}"#),
        ("manifest.json", r#"{"alice": "@example.org"}"#),
        ("manifest.json", r#"{"TFN": "123 456 782"}"#),
        ("manifest.json", r#"{"a": "Call 13ESCu002046 20"}"#),
        (
            "rulings.jsonl",
            "{\"text\": \"Call 13 46\"}\n{\"text\": \"20 today\"}\n",
        ),
        ("rulings.jsonl", "{\"n\": 1346}\n{\"n\": 20}\n"),
        ("rulings.jsonl", "{\"a\": \"13 46\"}\r{\"b\": \"20\"}\n"),
        (
            "rulings.jsonl",
            "{\"a\": \"Jane Citizen\"}\n{\"b\": \"12345678\"}\n",
        ),
        ("manifest.json", r#"{"metadata": {"bytes": 134620}}"#),
        ("manifest.json", r#"{"manifest.json": "HEX"}"#),
        (
            "rulings.jsonl",
            "{\"documents\": [{\"sha256\": \"HEX\"}]}\n",
        ),
    ];

    /// A machine value where the stage or `dist` writes one, and nothing else.
    const MACHINE: [(&str, &str); 4] = [
        ("manifest.json", r#"{"rows": 134620}"#),
        (
            "manifest.json",
            r#"{"documents": [{"bytes": 134620, "sha256": "HEX"}]}"#,
        ),
        ("rulings.jsonl", "{\"source_sha256\": \"HEX\"}\n"),
        (
            "sources.json",
            r#"{"runs": [{"rows": 134620, "files": {"manifest.json": "HEX"}}]}"#,
        ),
    ];

    /// `private` as it was before it skipped machine values, the file as
    /// written then each value decoded, and `private` now, over `text`.
    fn both_scans(dir: &std::path::Path, file: &str, text: &str) -> (bool, bool) {
        let found = |(pair, contacts): (bool, Vec<pii::Unapproved>)| pair || !contacts.is_empty();
        let hex = "a134620baaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let text = text
            .replace("HEX", hex)
            .replace("ESC", &char::from(92u8).to_string());
        let folder = tempfile::tempdir_in(dir).unwrap().keep();
        let path = folder.join(file);
        std::fs::write(&path, &text).unwrap();
        let path = path.to_str().unwrap();
        let values: Vec<&str> = if file.ends_with(".jsonl") {
            text.lines().collect()
        } else {
            vec![text.as_str()]
        };
        let before = found(pii::privacy_findings_in_file(path, "run", &[]).unwrap())
            || values.into_iter().any(|value| {
                pyjson::loads_standard(value)
                    .is_ok_and(|value| found(pii::privacy_findings_in_row(&value, "run", &[])))
            });
        let now = private(path, "run").unwrap();
        eprintln!("{file} {text:?}: whole text {before}, decoded {now}");
        (before, now)
    }

    /// The decoded scan finds everything the whole-text scan found, except a
    /// machine value where the stage writes one.
    #[test]
    fn the_decoded_scan_misses_only_machine_values() {
        let dir = tempfile::tempdir().unwrap();
        for (file, text) in SCANNED {
            let (before, now) = both_scans(dir.path(), file, text);
            assert!(!before || now, "{text}");
        }
        for (file, text) in MACHINE {
            assert_eq!(both_scans(dir.path(), file, text), (true, false), "{text}");
        }
    }
}
