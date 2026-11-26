// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `quick --in-force-on`: hold each preset Act's earlier compilation in force
//! on a date under `compilations/<date>/`, reusing what the root build holds.
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use super::{OMISSION_REASONS, OUTPUT_DEPTH, Runner, lay_out, quick_error};
use crate::fadden::compilations::overlaps;
use crate::fadden::corpus_paths::write_json_atomic;
use crate::fadden::{Exc, getitem, iterate, json_error};
use crate::pyjson::{self, Json, set_field};
use crate::pyos::{OsError, Style};
use crate::pypath;

/// A title's current compilation: id, version start and compilation id.
type Held = (String, String, Option<String>);

fn extracted(scratch: &str) -> Result<Vec<Json>, Exc> {
    let text = crate::pyio::read_text(&pypath::join(scratch, "manifest_md.json"))?;
    let val = pyjson::loads_standard(&text).map_err(json_error)?;
    iterate(&val)
}

/// `row[key]` as text, or the TypeError Python's comparisons raised.
fn text_field(row: &Json, key: &str) -> Result<String, Exc> {
    getitem(row, key)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| Exc::Type(format!("{key} must be str")))
}

/// Refuse a build in which 2 compilations of one title cover the same date.
pub fn check_windows(rows: &[Json]) -> Result<(), Exc> {
    let mut windows: BTreeMap<String, Vec<(String, Option<String>)>> = BTreeMap::new();
    for row in rows {
        let rid = getitem(row, "id")?
            .as_str()
            .ok_or_else(|| Exc::Type("expected string id".into()))?
            .to_string();
        let start = getitem(row, "versionStart")?
            .as_str()
            .ok_or_else(|| Exc::Type("expected string versionStart".into()))?
            .to_string();
        let end = match row.get("current_version_start") {
            Some(Json::Str(s)) => s.as_str().map(str::to_string),
            Some(Json::Null) | None => None,
            _ => return Err(Exc::Type("expected string current_version_start".into())),
        };
        windows.entry(rid).or_default().push((start, end));
    }
    for (rid, held) in windows {
        if let Some(first) = overlaps(&rid, held).into_iter().next() {
            return Err(quick_error(first));
        }
    }
    Ok(())
}

/// The current compilations the root build extracted.
fn root_current(rows: &[Json]) -> HashSet<Held> {
    let text = |row: &Json, key: &str| row.get(key).and_then(Json::as_str).map(str::to_string);
    rows.iter()
        .filter(|row| matches!(row.get("current_version_start"), None | Some(Json::Null)))
        .map(|row| {
            (
                text(row, "id").unwrap_or_default(),
                text(row, "versionStart").unwrap_or_default(),
                text(row, "compilationRegisterId"),
            )
        })
        .collect()
}

/// One date's title resolution report: resolved, current and omitted rows.
struct Report {
    resolved: Vec<Json>,
    current: Vec<Json>,
    skipped: Vec<Json>,
}

/// Read `dated_resolution.json` and refuse one that does not account for
/// every expected title exactly once with a known omission reason.
fn read_report(scratch: &str, on: &str, expected: &BTreeSet<String>) -> Result<Report, Exc> {
    let report_text = crate::pyio::read_text(&pypath::join(scratch, "dated_resolution.json"))?;
    let report = pyjson::loads_standard(&report_text).map_err(json_error)?;
    let (Some(Json::Array(resolved)), Some(Json::Array(current)), Some(Json::Array(skipped))) = (
        report.get("resolved"),
        report.get("current"),
        report.get("omissions"),
    ) else {
        return Err(quick_error(format!(
            "compilations/{on} has an invalid title resolution report"
        )));
    };
    let mut ids = Vec::new();
    for row in resolved.iter().chain(current.iter()) {
        ids.push(text_field(row, "id")?);
    }
    for row in skipped {
        ids.push(text_field(row, "register_id")?);
    }
    let id_set: BTreeSet<String> = ids.iter().cloned().collect();
    if report.get("in_force_on").and_then(Json::as_str) != Some(on)
        || ids.len() != expected.len()
        || &id_set != expected
    {
        return Err(quick_error(format!(
            "compilations/{on} has an incomplete title resolution report"
        )));
    }
    let known = |row: &Json| {
        row.get("reason")
            .and_then(Json::as_str)
            .is_some_and(|r| OMISSION_REASONS.contains(&r))
    };
    if !skipped.iter().all(known) {
        return Err(quick_error(format!(
            "compilations/{on} has an invalid omission reason"
        )));
    }
    Ok(Report {
        resolved: resolved.clone(),
        current: current.clone(),
        skipped: skipped.clone(),
    })
}

/// Refuse a date whose current compilations the root build does not hold.
fn check_current_held(report: &Report, on: &str, held: &HashSet<Held>) -> Result<(), Exc> {
    for row in &report.current {
        let id = text_field(row, "id")?;
        let start = text_field(row, "versionStart")?;
        let comp = getitem(row, "compilationRegisterId")?
            .as_str()
            .map(str::to_string);
        if start.as_str() > on || !held.contains(&(id, start, comp)) {
            return Err(quick_error(format!(
                "compilations/{on} needs a current compilation missing from the root"
            )));
        }
    }
    Ok(())
}

/// Resolved compilations that no extracted row already holds.
fn wanted(rows: &[Json], resolved: &[Json]) -> Result<Vec<Json>, Exc> {
    let kept: HashSet<(String, String)> = rows
        .iter()
        .filter_map(|r| {
            Some((
                r.get("id")?.as_str()?.to_string(),
                r.get("versionStart")?.as_str()?.to_string(),
            ))
        })
        .collect();
    let mut wanted = Vec::new();
    for row in resolved {
        let id = getitem(row, "id")?.as_str().unwrap_or("").to_string();
        let start = getitem(row, "versionStart")?
            .as_str()
            .unwrap_or("")
            .to_string();
        if !kept.contains(&(id, start)) {
            wanted.push(row.clone());
        }
    }
    Ok(wanted)
}

/// Download and extract `wanted` into `build_dir`; returns the extracted rows.
fn fetch_dated(
    runner: &mut dyn Runner,
    build_dir: &str,
    on: &str,
    wanted: Vec<Json>,
) -> Result<Vec<Json>, Exc> {
    let scratch = pypath::join(build_dir, "build");
    let ids: BTreeSet<String> = wanted
        .iter()
        .filter_map(|r| r.get("id").and_then(Json::as_str).map(str::to_string))
        .collect();
    let target = pypath::join(&scratch, "acts_resolved.json");
    write_json_atomic(&target, &Json::Array(wanted), Some(1), OUTPUT_DEPTH)?;
    runner.download(&scratch, &pypath::join(build_dir, "epub"))?;
    for name in ["extract", "pii_scan", "pii_scan2"] {
        runner.stage(&scratch, &[name], false)?;
    }
    let got = extracted(&scratch)?;
    let got_ids: BTreeSet<String> = got
        .iter()
        .filter_map(|r| r.get("id").and_then(Json::as_str).map(str::to_string))
        .collect();
    let missing: Vec<String> = ids.difference(&got_ids).cloned().collect();
    if !missing.is_empty() {
        return Err(quick_error(format!(
            "compilations/{on} holds no rows for {}",
            missing.join(", ")
        )));
    }
    Ok(got)
}

/// Remove an empty `compilations/` and record each date's omissions in `sources.json`.
fn finish(staging: &str, folder: &str, omissions: Vec<Json>) -> Result<(), Exc> {
    let folder_path = Path::new(folder);
    if folder_path.is_dir()
        && std::fs::read_dir(folder_path)
            .map_err(|e| OsError::new(&e, Some(folder), Style::Errno))?
            .next()
            .is_none()
    {
        std::fs::remove_dir(folder_path)
            .map_err(|e| OsError::new(&e, Some(folder), Style::Errno))?;
    }
    let sources_path = pypath::join(staging, "sources.json");
    let sources_text = crate::pyio::read_text(&sources_path)?;
    let mut source = pyjson::loads_standard(&sources_text).map_err(json_error)?;
    set_field(&mut source, "historical_omissions", Json::Array(omissions));
    write_json_atomic(&sources_path, &source, Some(1), OUTPUT_DEPTH)
}

/// Build `compilations/<date>/` for each date in `dates`.
pub(super) fn add_compilations(
    runner: &mut dyn Runner,
    module: &str,
    staging: &str,
    dates: &[String],
) -> Result<(), Exc> {
    let root_scratch = pypath::join(staging, "build");
    let titles_text = crate::pyio::read_text(&pypath::join(&root_scratch, "titles_all.json"))?;
    let titles_json = pyjson::loads_standard(&titles_text).map_err(json_error)?;
    let expected: BTreeSet<String> = iterate(&titles_json)?
        .iter()
        .filter_map(|t| t.get("id").and_then(Json::as_str).map(str::to_string))
        .collect();
    let mut rows = extracted(&root_scratch)?;
    let held = root_current(&rows);
    let unique_dates: BTreeSet<&String> = dates.iter().collect();
    let mut omissions = Vec::new();
    let folder = pypath::join(staging, "compilations");
    for on in unique_dates {
        let build_dir = pypath::join(&folder, on);
        let scratch = pypath::join(&build_dir, "build");
        lay_out(module, &scratch, &titles_json)?;
        let args = ["versions", "--in-force-on", on, "--report-omissions"];
        runner.stage(&scratch, &args, false)?;
        let report = read_report(&scratch, on, &expected)?;
        check_current_held(&report, on, &held)?;
        omissions.push(Json::obj([
            ("in_force_on", Json::str(on)),
            ("omissions", Json::Array(report.skipped.clone())),
        ]));
        let wanted = wanted(&rows, &report.resolved)?;
        let held_count = report.current.len() + report.resolved.len() - wanted.len();
        runner.print(&format!(
            "compilations/{on}: {} to fetch, {held_count} already held, {} with none to fetch, listed above",
            wanted.len(),
            report.skipped.len(),
        ));
        if wanted.is_empty() {
            std::fs::remove_dir_all(&build_dir)
                .map_err(|e| OsError::new(&e, Some(&build_dir), Style::Errno))?;
            continue;
        }
        rows.extend(fetch_dated(runner, &build_dir, on, wanted)?);
    }
    check_windows(&rows)?;
    finish(staging, &folder, omissions)
}
