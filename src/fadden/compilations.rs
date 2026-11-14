// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

//! The earlier compilations `quick --in-force-on` holds under `compilations/<date>/`,
//! as `dist` lists them and `dist_verify` checks them. A dated folder ships its
//! `markdown/` and a `sources.json` naming each compilation's source, window and
//! file digests; its `build/` and `epub/` stay behind, as the root's do.
use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use super::corpus_paths::{child, iso_date, register_id, relative};
use super::dist_verify::{LICENCE_EXCLUDED, entry_kind, expected_title_files, title_tree};
use super::pii::{self, Approved};
use super::{Exc, get, getitem, iterate, json_error};
use crate::py;
use crate::pyio::Lines;
use crate::pyjson::{self, Json};
use crate::pyos::{self, OsError, Style};
use crate::pypath;

/// The folder under a corpus root, and so under a distribution.
pub const FOLDER: &str = "compilations";

/// The heading of LICENCE-NOTICE.md's section on the earlier compilations.
pub const NOTICE_HEADING: &str = "## Earlier compilations";

/// One check's label, whether it passed, and its detail.
pub type Check = (&'static str, bool, String);

/// The names in a directory, sorted.
pub fn names(path: &str) -> Result<Vec<String>, Exc> {
    let error = |e: std::io::Error| OsError::new(&e, Some(path), Style::Native);
    let mut names = Vec::new();
    for entry in pyos::scandir(path).map_err(error)? {
        names.push(pyos::entry_name(&entry.map_err(error)?)?);
    }
    names.sort();
    Ok(names)
}

/// The overlaps amongst one title's compilation windows, each a start and an
/// end (None while in force). A window may end where the next starts.
/// `fadden/quick.py`'s `check_windows` applies the same rule before a build is
/// published.
pub fn overlaps(rid: &str, mut windows: Vec<(String, Option<String>)>) -> Vec<String> {
    windows.sort();
    windows
        .windows(2)
        .filter(|pair| pair[0].1.as_ref().is_none_or(|end| *end > pair[1].0))
        .map(|pair| {
            format!(
                "2 compilations of {rid} overlap: the one from {} starts inside the window of the one from {}",
                pair[1].0, pair[0].0
            )
        })
        .collect()
}

/// A dated folder's listing of one `manifest_md.json` row, refusing a row that
/// does not describe a compilation in force on `on`.
pub fn listing(row: &Json, on: &str) -> Result<Json, Exc> {
    let rid = register_id(getitem(row, "id")?)?;
    let field = Field { row, on, rid: &rid };
    let (start, end) = (
        field.date("versionStart")?,
        field.date("current_version_start")?,
    );
    if !(start.as_str() <= on && on < end.as_str()) {
        return Err(field.bad(&format!(
            "is not in force then: it runs from {start} to {end}"
        )));
    }
    if !matches!(row.get("version_is_current"), Some(Json::Bool(false))) {
        return Err(field.bad("is not marked as an earlier compilation"));
    }
    let copied = |name: &str| row.get(name).cloned().unwrap_or(Json::Null);
    let path = |name: &str| Json::str(&format!("markdown/{rid}/{name}"));
    let endnotes = row.get("endnotes").is_some_and(Json::truthy);
    Ok(Json::obj([
        ("register_id", Json::str(&rid)),
        ("name", Json::str(&field.text("name")?)),
        ("collection", copied("collection")),
        ("compilation_number", copied("compilationNumber")),
        (
            "compilation_register_id",
            Json::str(&field.text("compilationRegisterId")?),
        ),
        ("compilation_date", Json::str(&start)),
        ("superseded_from", Json::str(&end)),
        ("version_is_current", Json::Bool(false)),
        ("retrieved", Json::str(&field.date("retrieved")?)),
        ("is_authorised", copied("isAuthorised")),
        ("source_url", Json::str(&field.text("sourceUrl")?)),
        ("markdown", path(&format!("{rid}.md"))),
        ("sections_jsonl", path("sections.jsonl")),
        (
            "endnotes",
            if endnotes {
                path("endnotes.md")
            } else {
                Json::Null
            },
        ),
    ]))
}

/// A `manifest_md.json` row's required fields, refused by name when absent.
struct Field<'a> {
    row: &'a Json,
    on: &'a str,
    rid: &'a str,
}

impl Field<'_> {
    fn bad(&self, what: &str) -> Exc {
        Exc::Runtime(format!(
            "compilations/{}: manifest row {} {what}",
            self.on, self.rid
        ))
    }

    fn text(&self, name: &str) -> Result<String, Exc> {
        self.row
            .get(name)
            .and_then(Json::as_str)
            .map(str::to_string)
            .ok_or_else(|| self.bad(&format!("has no {name}")))
    }

    fn date(&self, name: &str) -> Result<String, Exc> {
        iso_date(&self.text(name)?).map_err(|_| self.bad(&format!("has an invalid {name}")))
    }
}

/// The SHA-256 of each file in a title directory, by its name there.
pub fn digests(directory: &str) -> Result<Json, Exc> {
    let (files, _) = title_tree(directory)?;
    let mut pairs = Vec::new();
    for path in files {
        let bytes =
            std::fs::read(&path).map_err(|e| OsError::new(&e, Some(&path), Style::Native))?;
        pairs.push((relative(&path, directory), Json::str(&sha256(&bytes))));
    }
    Ok(Json::Object(
        pairs
            .into_iter()
            .map(|(k, v)| (crate::pyjson::PyStr::Text(k), v))
            .collect(),
    ))
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What the checks found across every dated folder.
#[derive(Default)]
struct Findings {
    stray: Vec<String>,
    titles: Vec<String>,
    digests: BTreeSet<String>,
    private: BTreeSet<String>,
    rows: BTreeSet<String>,
    removed: BTreeSet<String>,
    windows: Vec<String>,
    /// Each title's windows: the root's first, then each date's.
    held: BTreeMap<String, Vec<(String, Option<String>)>>,
}

/// The dated folders' checks, or none when the distribution holds no
/// `compilations/` and its sources.json lists none. `removed` is the root's
/// removed titles, which no dated folder may hold either.
pub fn verify(
    dist_root: &str,
    src: &Json,
    removed: &BTreeSet<String>,
    approved: &[Approved],
) -> Result<Vec<Check>, Exc> {
    let listed = listed_dates(src)?;
    let folder = pypath::join(dist_root, FOLDER);
    let present = std::fs::symlink_metadata(&folder).is_ok();
    if listed.is_empty() && !present {
        return Ok(Vec::new());
    }
    let mut found = Findings::default();
    let dated = dated_folders(&folder, &listed, &mut found.stray)?;
    let undated = root_windows(src, &mut found.held)?;
    for on in &dated {
        verify_date(
            &pypath::join(&folder, on),
            on,
            removed,
            approved,
            &mut found,
        )?;
    }
    for (rid, windows) in std::mem::take(&mut found.held) {
        if undated.contains(&rid) {
            found
                .windows
                .push(format!("{rid}: the root title's window is not recorded"));
        }
        found.windows.extend(overlaps(&rid, windows));
    }
    let notice = crate::pyio::read_text(&child(dist_root, &["LICENCE-NOTICE.md"])?)?;
    Ok(report(
        &found,
        notice.contains(&format!("\n{NOTICE_HEADING}\n")),
    ))
}

/// The listed dates that are real folders under `folder`, recording in `stray`
/// each entry that is not one and each listed date that is missing.
fn dated_folders(
    folder: &str,
    listed: &[String],
    stray: &mut Vec<String>,
) -> Result<Vec<String>, Exc> {
    let kind = entry_kind(folder);
    let entries = match kind {
        "directory" => names(folder)?,
        _ if std::fs::symlink_metadata(folder).is_ok() => {
            stray.push(format!("{FOLDER} [{kind}]"));
            Vec::new()
        }
        _ => Vec::new(),
    };
    let mut dated = Vec::new();
    for name in &entries {
        if listed.contains(name) && entry_kind(&pypath::join(folder, name)) == "directory" {
            dated.push(name.clone());
        } else {
            stray.push(name.clone());
        }
    }
    stray.extend(
        listed
            .iter()
            .filter(|d| !entries.contains(d))
            .map(|d| format!("{d} missing")),
    );
    Ok(dated)
}

fn report(found: &Findings, noticed: bool) -> Vec<Check> {
    fn check<'a>(label: &'static str, items: impl IntoIterator<Item = &'a String>) -> Check {
        let items: Vec<&String> = items.into_iter().collect();
        let shown: Vec<&str> = items.iter().take(5).map(|s| s.as_str()).collect();
        (label, items.is_empty(), shown.join(", "))
    }
    vec![
        check(
            "compilations holds only the listed dates and their files",
            &found.stray,
        ),
        check(
            "each dated folder holds its listed titles only",
            &found.titles,
        ),
        check(
            "compilation files match their listed digests",
            &found.digests,
        ),
        check(
            "no compilation file names people or unapproved contacts",
            &found.private,
        ),
        check("compilation rows carry their listed window", &found.rows),
        check("no removed title in a dated folder", &found.removed),
        check(
            "compilation windows hold their date, without overlap",
            &found.windows,
        ),
        (
            "licence notice covers the earlier compilations",
            noticed,
            String::new(),
        ),
    ]
}

/// sources.json's `compilations`: the dates the distribution holds.
fn listed_dates(src: &Json) -> Result<Vec<String>, Exc> {
    let Some(value) = get(src, FOLDER)? else {
        return Ok(Vec::new());
    };
    let mut dates = Vec::new();
    for item in iterate(value)? {
        let date = item
            .as_str()
            .and_then(|t| iso_date(t).ok())
            .ok_or_else(|| Exc::value("sources.json compilations must list YYYY-MM-DD dates"))?;
        if dates.contains(&date) {
            return Err(Exc::value(format!(
                "sources.json lists compilations/{date} twice"
            )));
        }
        dates.push(date);
    }
    Ok(dates)
}

/// The root titles' windows: from `compilation_date`, to `current_version_start`
/// when the root holds an earlier compilation (retry13's fallback). Returns the
/// titles with no valid `compilation_date`, whose windows cannot be compared.
fn root_windows(
    src: &Json,
    held: &mut BTreeMap<String, Vec<(String, Option<String>)>>,
) -> Result<BTreeSet<String>, Exc> {
    let mut undated = BTreeSet::new();
    for title in iterate(getitem(src, "titles")?)? {
        let rid = register_id(getitem(&title, "register_id")?)?;
        let date = |name: &str| {
            title
                .get(name)
                .and_then(Json::as_str)
                .and_then(|t| iso_date(t).ok())
        };
        let earlier = matches!(title.get("version_is_current"), Some(Json::Bool(false)));
        let end = date("current_version_start");
        match date("compilation_date") {
            Some(start) if !earlier || end.is_some() => {
                held.entry(rid)
                    .or_default()
                    .push((start, end.filter(|_| earlier)));
            }
            _ => {
                undated.insert(rid);
            }
        }
    }
    Ok(undated)
}

fn verify_date(
    directory: &str,
    on: &str,
    removed: &BTreeSet<String>,
    approved: &[Approved],
    found: &mut Findings,
) -> Result<(), Exc> {
    let entries = names(directory)?;
    if entries != ["markdown", "sources.json"]
        || entry_kind(&pypath::join(directory, "markdown")) != "directory"
    {
        found.stray.push(format!("{on}: {}", entries.join(" ")));
        return Ok(());
    }
    let text = crate::pyio::read_text(&child(directory, &["sources.json"])?)?;
    let listing = pyjson::loads_standard(&text).map_err(json_error)?;
    if listing.get("in_force_on").and_then(Json::as_str) != Some(on) {
        found
            .windows
            .push(format!("{on}: sources.json names another date"));
    }
    // The folder's own exclusions, as well as the root's, keep a title out.
    let mut excluded = removed.clone();
    for title in iterate(getitem(&listing, "excluded_titles")?)? {
        excluded.insert(register_id(getitem(&title, "register_id")?)?);
    }
    let markdown = pypath::join(directory, "markdown");
    let mut ids = Vec::new();
    for title in iterate(getitem(&listing, "titles")?)? {
        ids.push(verify_listed(
            &markdown, on, &title, &excluded, approved, found,
        )?);
    }
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != ids.len() || names(&markdown)? != sorted {
        found.titles.push(on.to_string());
    }
    Ok(())
}

/// One listed title of a dated folder, checked; returns its Register id.
fn verify_listed(
    markdown: &str,
    on: &str,
    title: &Json,
    removed: &BTreeSet<String>,
    approved: &[Approved],
    found: &mut Findings,
) -> Result<String, Exc> {
    let rid = register_id(getitem(title, "register_id")?)?;
    let label = format!("{on}/{rid}");
    if removed.contains(&rid) || LICENCE_EXCLUDED.contains(&rid.as_str()) {
        found.removed.insert(label.clone());
    }
    let (start, end) = window(title, on, &rid)?;
    if !(start.as_str() <= on && on < end.as_str()) {
        found
            .windows
            .push(format!("{label} runs from {start} to {end}"));
    }
    let held = found.held.entry(rid.clone()).or_default();
    held.push((start.clone(), Some(end.clone())));
    let directory = pypath::join(markdown, &rid);
    verify_title(&directory, &rid, title, approved, found, &label)?;
    verify_rows(&directory, &rid, (&start, &end), approved, found, &label)?;
    Ok(rid)
}

/// A listed title's `compilation_date` and `superseded_from`, as dates.
fn window(title: &Json, on: &str, rid: &str) -> Result<(String, String), Exc> {
    let date = |name: &str| {
        title
            .get(name)
            .and_then(Json::as_str)
            .and_then(|t| iso_date(t).ok())
            .ok_or_else(|| {
                Exc::value(format!(
                    "compilations/{on}/sources.json: {rid} has no valid {name}"
                ))
            })
    };
    Ok((date("compilation_date")?, date("superseded_from")?))
}

/// Whether a title's files are exactly those its listing names, with their
/// digests, and free of private-person pairs and unapproved contacts.
fn verify_title(
    directory: &str,
    rid: &str,
    title: &Json,
    approved: &[Approved],
    found: &mut Findings,
    label: &str,
) -> Result<(), Exc> {
    let tree = match title_tree(directory) {
        Ok(tree) => tree,
        Err(Exc::Os(_) | Exc::Value { .. }) => {
            found.digests.insert(label.to_string());
            return Ok(());
        }
        Err(other) => return Err(other),
    };
    let (expected, metadata_ok) = expected_title_files(rid, title)?;
    let recorded = title.get("files").cloned().unwrap_or(Json::Null);
    let actual = digests(directory)?;
    let named: BTreeSet<String> = tree.0.iter().map(|f| relative(f, directory)).collect();
    if !tree.1.is_empty() || named != expected || !metadata_ok || !same_object(&recorded, &actual) {
        found.digests.insert(label.to_string());
    }
    for path in &tree.0 {
        match pii::privacy_findings_in_file(path, rid, approved) {
            Ok((pair, contacts)) if pair || !contacts.is_empty() => {
                found.private.insert(label.to_string());
            }
            Ok(_) => {}
            Err(error) if pii::unreadable(&error) => {
                found.private.insert(format!("{label} unreadable"));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Two JSON objects with the same keys and values, in any order.
fn same_object(a: &Json, b: &Json) -> bool {
    let (Json::Object(a), Json::Object(b)) = (a, b) else {
        return false;
    };
    a.len() == b.len()
        && a.iter().all(|(k, v)| {
            b.iter()
                .any(|(bk, bv)| bk == k && crate::pyvalue::py_eq(bv, v))
        })
}

/// Whether every row of a dated title names it, carries its window and, once
/// decoded, names no private person and holds no unapproved contact.
fn verify_rows(
    directory: &str,
    rid: &str,
    (start, end): (&str, &str),
    approved: &[Approved],
    found: &mut Findings,
    label: &str,
) -> Result<(), Exc> {
    let path = pypath::join(directory, "sections.jsonl");
    if !std::fs::metadata(&path).is_ok_and(|m| m.is_file()) {
        return Ok(()); // the digest check reports the missing file
    }
    let mut lines = Lines::open(&path)?;
    while let Some(line) = lines.next_line() {
        let line = line?;
        if py::strip(&line).is_empty() {
            continue;
        }
        let carries = |row: &Json| {
            row.get("register_id").and_then(Json::as_str) == Some(rid)
                && row.get("compilation_date").and_then(Json::as_str) == Some(start)
                && row.get("superseded_from").and_then(Json::as_str) == Some(end)
                && matches!(row.get("version_is_current"), Some(Json::Bool(false)))
        };
        let Some(row) = pyjson::loads_standard(&line).ok().filter(carries) else {
            found.rows.insert(label.to_string());
            return Ok(());
        };
        if names_people(&row, rid, approved) {
            found.private.insert(label.to_string());
        }
    }
    Ok(())
}

/// Whether a row's decoded strings pair a name with a registration number,
/// in any of its fields as the file scan would, or hold an unapproved contact.
/// JSON escapes can hide either from the file scan. Contacts are sought in
/// each string alone: joined, the end of one and the start of the next can
/// read as a phone number.
fn names_people(row: &Json, rid: &str, approved: &[Approved]) -> bool {
    let strings = decoded_strings(row);
    let (mut names, mut numbers) = (false, false);
    for text in &strings {
        let (n, r) = pii::private_person_registration_details(text);
        names |= !n.is_empty();
        numbers |= !r.is_empty();
    }
    (names && numbers)
        || strings
            .iter()
            .any(|text| !pii::unapproved_contact_fingerprints(text, rid, approved).is_empty())
}

/// A row's top-level string values, decoded.
fn decoded_strings(row: &Json) -> Vec<String> {
    let Json::Object(fields) = row else {
        return Vec::new();
    };
    fields
        .iter()
        .filter_map(|(_, value)| match value {
            Json::Str(s) => Some(crate::pyvalue::points_lossy(&s.code_points())),
            _ => None,
        })
        .collect()
}
