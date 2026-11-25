// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `fadden/quick.py`: build and index a preset of principal Acts in an empty
//! folder, optionally holding earlier compilations under `compilations/<date>/`.
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::process::Command;

use rusqlite::{Connection, OpenFlags};

use super::compilations::overlaps;
use super::corpus_paths::{module_dir, write_json_atomic, write_text};
use super::http::{self, Http};
use super::{
    Exc, Run, download, getitem, iterate, json_error, probe13, retry13, round_half_even, versions,
};
use crate::argparse::{Action, Exit, Get, Parser, Value as ArgValue};
use crate::py;
use crate::pyjson::{self, Json, set_field};
use crate::pyos::{OsError, Style};
use crate::pypath::{self, PyPath};
use crate::pyre;
use crate::pyvalue::dumps_with;

/// The 8 principal Acts the MCP server ranks first, then the SIS Act.
pub const PRESET: [&str; 9] = [
    "C2004A05138",
    "C2004A00446",
    "C1936A00027",
    "C1953A00001",
    "C2004A03280",
    "C2004A04402",
    "C2004A05128",
    "C2004A03348",
    "C2004A04633",
];

/// Data file copied alongside stage outputs in `<staging>/build/`.
pub const SUPPORT: [&str; 1] = ["pii_contact_allowlist.json"];
pub const SMOKE_QUERY: [&str; 2] = ["general", "deductions"];

pub const OMISSION_REASONS: [&str; 3] = [
    "the Register published no compilation for the version in force then",
    "no version was in force then",
    "the version then starts after the current one, so is not in force yet",
];

const OUTPUT_DEPTH: usize = 990;

pub fn quick_error(message: impl Into<String>) -> Exc {
    Exc::Other {
        class: "QuickError",
        message: message.into(),
    }
}

fn copy_file(source: &str, destination: &str) -> Result<(), Exc> {
    std::fs::copy(source, destination).map_err(|e| OsError::new(&e, Some(source), Style::Errno))?;
    Ok(())
}

/// Lay out a stage folder holding `SUPPORT` and `titles_all.json`.
pub fn lay_out(module: &str, scratch: &str, titles: &Json) -> Result<(), Exc> {
    crate::pyos::makedirs(&PyPath::new(scratch))?;
    for name in SUPPORT {
        copy_file(&pypath::join(module, name), &pypath::join(scratch, name))?;
    }
    let points = dumps_with(titles, Some(1), true, false);
    write_text(&pypath::join(scratch, "titles_all.json"), &points)
}

/// Validate `out` is absent or empty, select the 9 preset titles from
/// `titles_principal.json`, and create `<out>.building/build/`.
pub fn prepare(module: &str, out: &str) -> Result<String, Exc> {
    let out_path = Path::new(out);
    if out_path.exists()
        && std::fs::read_dir(out_path)
            .map_err(|e| OsError::new(&e, Some(out), Style::Errno))?
            .next()
            .is_some()
    {
        return Err(quick_error(format!(
            "{out} is not empty; name an absent or empty folder"
        )));
    }
    let principal_path = pypath::join(module, "titles_principal.json");
    let principal_text = crate::pyio::read_text(&principal_path)?;
    let rows = pyjson::loads_standard(&principal_text).map_err(json_error)?;
    let mut titles = Vec::new();
    for row in iterate(&rows)? {
        if let Some(id) = row.get("id").and_then(Json::as_str)
            && PRESET.contains(&id)
        {
            titles.push(row);
        }
    }
    if titles.len() != PRESET.len() {
        return Err(quick_error(
            "titles_principal.json no longer holds every preset Act",
        ));
    }
    let out_p = PyPath::new(out);
    let staging = out_p
        .parent()
        .join(&format!("{}.building", out_p.name()))
        .display();
    if Path::new(&staging).exists() {
        return Err(quick_error(format!(
            "{staging} holds an earlier partial build; remove it first"
        )));
    }
    std::fs::create_dir(&staging).map_err(|e| OsError::new(&e, Some(&staging), Style::Errno))?;
    lay_out(
        module,
        &pypath::join(&staging, "build"),
        &Json::Array(titles),
    )?;
    Ok(staging)
}

/// Abstraction over sub-stage execution so unit tests can mock stages cleanly
/// whilst production runs HTTP stages over one shared `Http` and local stages
/// in subprocesses with `FADDEN_MODULE_DIR=scratch`.
pub trait Runner {
    fn stage(&mut self, scratch: &str, args: &[&str], capture: bool) -> Result<String, Exc>;
    fn download(&mut self, scratch: &str, epub_dir: &str) -> Result<(), Exc>;
    fn retry13(&mut self, scratch: &str, epub_dir: &str) -> Result<(), Exc>;
    fn monotonic(&mut self) -> f64 {
        0.0
    }
    fn print(&mut self, line: &str);
}

/// Live runner using one shared `Http` instance and `binary` for local stages.
pub struct RealRunner<'a> {
    pub binary: String,
    pub http: &'a mut dyn Http,
    pub run: &'a mut Run,
}

impl<'a> RealRunner<'a> {
    fn run_inprocess(&mut self, stage: &str, scratch: &str, args: &[&str]) -> Result<(), Exc> {
        let mut child_run = Run::default();
        let argv: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        let exit = match stage {
            "versions" => versions::run_cli(self.http, scratch, &argv, &mut child_run),
            "probe13" => match probe13::run(self.http, scratch, &mut child_run) {
                Ok(()) => Ok(0),
                Err(err) => Ok(child_run.uncaught(&err)),
            },
            _ => unreachable!(),
        };
        let stdout = String::from_utf8_lossy(&child_run.stdout).replace("\r\n", "\n");
        let stderr = String::from_utf8_lossy(&child_run.stderr).replace("\r\n", "\n");
        self.run.stdout.extend_from_slice(stdout.as_bytes());
        self.run.stderr.extend_from_slice(stderr.as_bytes());
        let code = match exit {
            Ok(c) => c,
            Err(Exit::Help(text)) => {
                self.run.stdout.extend_from_slice(text.as_bytes());
                0
            }
            Err(Exit::Error(text)) => {
                self.run.stderr.extend_from_slice(text.as_bytes());
                2
            }
        };
        if code != 0 {
            return Err(quick_error(format!("the {stage} stage exited {code}")));
        }
        Ok(())
    }
}

/// Run an external executable with `FADDEN_MODULE_DIR=scratch`, returning stdout if `capture`.
pub fn run_subprocess(
    binary: &str,
    scratch: &str,
    args: &[&str],
    capture: bool,
    run: &mut Run,
) -> Result<String, Exc> {
    let mut cmd = Command::new(binary);
    cmd.args(args).env("FADDEN_MODULE_DIR", scratch);
    let output = cmd
        .output()
        .map_err(|e| OsError::new(&e, Some(binary), Style::Errno))?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|_| Exc::value("stage output is not valid UTF-8"))?
        .replace("\r\n", "\n");
    if !capture {
        run.stdout.extend_from_slice(stdout.as_bytes());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n");
    run.stderr.extend_from_slice(stderr.as_bytes());
    if !output.status.success() {
        let code = output.status.code().unwrap_or(1);
        let name = args.first().copied().unwrap_or(binary);
        return Err(quick_error(format!("the {name} stage exited {code}")));
    }
    Ok(if capture { stdout } else { String::new() })
}

impl<'a> Runner for RealRunner<'a> {
    fn stage(&mut self, scratch: &str, args: &[&str], capture: bool) -> Result<String, Exc> {
        let name = args.first().copied().unwrap_or("");
        if matches!(name, "versions" | "probe13") && !capture {
            self.run_inprocess(name, scratch, &args[1..])?;
            return Ok(String::new());
        }
        run_subprocess(&self.binary, scratch, args, capture, self.run)
    }

    fn download(&mut self, scratch: &str, epub_dir: &str) -> Result<(), Exc> {
        let mut child_run = Run::default();
        let outcome = download::run(
            self.http,
            scratch,
            epub_dir,
            download::CRAWL_DELAY,
            &mut child_run,
        );
        let stdout = String::from_utf8_lossy(&child_run.stdout).replace("\r\n", "\n");
        let stderr = String::from_utf8_lossy(&child_run.stderr).replace("\r\n", "\n");
        self.run.stdout.extend_from_slice(stdout.as_bytes());
        self.run.stderr.extend_from_slice(stderr.as_bytes());
        outcome
    }

    fn retry13(&mut self, scratch: &str, epub_dir: &str) -> Result<(), Exc> {
        let mut child_run = Run::default();
        let outcome = retry13::run(
            self.http,
            scratch,
            epub_dir,
            download::CRAWL_DELAY,
            &mut child_run,
        );
        let stdout = String::from_utf8_lossy(&child_run.stdout).replace("\r\n", "\n");
        let stderr = String::from_utf8_lossy(&child_run.stderr).replace("\r\n", "\n");
        self.run.stdout.extend_from_slice(stdout.as_bytes());
        self.run.stderr.extend_from_slice(stderr.as_bytes());
        outcome
    }

    fn monotonic(&mut self) -> f64 {
        self.http.monotonic()
    }

    fn print(&mut self, line: &str) {
        self.run.print(line);
    }
}

fn recover_missing(runner: &mut dyn Runner, scratch: &str, epub_dir: &str) -> Result<(), Exc> {
    let manifest_text = crate::pyio::read_text(&pypath::join(scratch, "manifest_raw.json"))?;
    let manifest = pyjson::loads_standard(&manifest_text).map_err(json_error)?;
    let has_missing = iterate(&manifest)?
        .into_iter()
        .any(|row| row.get("status").and_then(Json::as_str) == Some("no_epub"));
    if has_missing {
        runner.stage(scratch, &["probe13"], false)?;
        runner.retry13(scratch, epub_dir)?;
    }
    Ok(())
}

/// Every preset Act must hold indexed rows, or the build is not the preset.
pub fn check_preset(index: &str) -> Result<(), Exc> {
    let conn = Connection::open_with_flags(index, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| Exc::Sqlite(e.to_string()))?;
    let mut stmt = conn
        .prepare("SELECT DISTINCT register_id FROM rows")
        .map_err(|e| Exc::Sqlite(e.to_string()))?;
    let mut held = HashSet::new();
    let mut rows = stmt.query([]).map_err(|e| Exc::Sqlite(e.to_string()))?;
    while let Some(row) = rows.next().map_err(|e| Exc::Sqlite(e.to_string()))? {
        let rid: String = row.get(0).map_err(|e| Exc::Sqlite(e.to_string()))?;
        held.insert(rid);
    }
    let missing: Vec<&str> = PRESET
        .iter()
        .copied()
        .filter(|rid| !held.contains(*rid))
        .collect();
    if !missing.is_empty() {
        return Err(quick_error(format!(
            "the index holds no rows for {}",
            missing.join(", ")
        )));
    }
    Ok(())
}

fn assemble(runner: &mut dyn Runner, staging: &str) -> Result<String, Exc> {
    let scratch = pypath::join(staging, "build");
    let epub_dir = pypath::join(staging, "epub");
    runner.stage(&scratch, &["versions"], false)?;
    runner.download(&scratch, &epub_dir)?;
    recover_missing(runner, &scratch, &epub_dir)?;
    for name in ["extract", "pii_scan", "pii_scan2"] {
        runner.stage(&scratch, &[name], false)?;
    }
    runner.stage(&scratch, &["finalize", "--preset"], false)?;
    let index = pypath::join(staging, "index.sqlite");
    runner.stage(
        &scratch,
        &["search", "build", staging, "--out", &index],
        false,
    )?;
    check_preset(&index)?;
    let hit = runner.stage(
        &scratch,
        &[
            "search",
            "query",
            &index,
            SMOKE_QUERY[0],
            SMOKE_QUERY[1],
            "--limit",
            "1",
        ],
        true,
    )?;
    if !hit.starts_with("1. ") {
        return Err(quick_error(format!(
            "the build holds no row for '{}'",
            SMOKE_QUERY.join(" ")
        )));
    }
    Ok(hit)
}

fn extracted(scratch: &str) -> Result<Vec<Json>, Exc> {
    let text = crate::pyio::read_text(&pypath::join(scratch, "manifest_md.json"))?;
    let val = pyjson::loads_standard(&text).map_err(json_error)?;
    iterate(&val)
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
        let msgs = overlaps(&rid, held);
        if let Some(first) = msgs.into_iter().next() {
            return Err(quick_error(first));
        }
    }
    Ok(())
}

fn add_compilations(
    runner: &mut dyn Runner,
    module: &str,
    staging: &str,
    dates: &[String],
) -> Result<(), Exc> {
    let root_scratch = pypath::join(staging, "build");
    let titles_text = crate::pyio::read_text(&pypath::join(&root_scratch, "titles_all.json"))?;
    let titles_json = pyjson::loads_standard(&titles_text).map_err(json_error)?;
    let titles = iterate(&titles_json)?;
    let expected: BTreeSet<String> = titles
        .iter()
        .filter_map(|t| t.get("id").and_then(Json::as_str).map(str::to_string))
        .collect();
    let mut rows = extracted(&root_scratch)?;
    let mut root_current = HashSet::new();
    for row in &rows {
        let is_current = matches!(row.get("current_version_start"), None | Some(Json::Null));
        if is_current {
            let id = row
                .get("id")
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_string();
            let start = row
                .get("versionStart")
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_string();
            let comp = row
                .get("compilationRegisterId")
                .and_then(Json::as_str)
                .map(str::to_string);
            root_current.insert((id, start, comp));
        }
    }
    let mut unique_dates: Vec<String> = dates.to_vec();
    unique_dates.sort();
    unique_dates.dedup();
    let mut omissions = Vec::new();
    let folder = pypath::join(staging, "compilations");

    for on in &unique_dates {
        let build_dir = pypath::join(&folder, on);
        let scratch = pypath::join(&build_dir, "build");
        lay_out(module, &scratch, &titles_json)?;
        runner.stage(
            &scratch,
            &["versions", "--in-force-on", on, "--report-omissions"],
            false,
        )?;
        let report_text = crate::pyio::read_text(&pypath::join(&scratch, "dated_resolution.json"))?;
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
            let id = getitem(row, "id")?
                .as_str()
                .ok_or_else(|| Exc::Type("id must be str".into()))?;
            ids.push(id.to_string());
        }
        for row in skipped {
            let id = getitem(row, "register_id")?
                .as_str()
                .ok_or_else(|| Exc::Type("register_id must be str".into()))?;
            ids.push(id.to_string());
        }
        let id_set: BTreeSet<String> = ids.iter().cloned().collect();
        if report.get("in_force_on").and_then(Json::as_str) != Some(on.as_str())
            || ids.len() != expected.len()
            || id_set != expected
        {
            return Err(quick_error(format!(
                "compilations/{on} has an incomplete title resolution report"
            )));
        }
        for row in skipped {
            let ok = row
                .get("reason")
                .and_then(Json::as_str)
                .is_some_and(|r| OMISSION_REASONS.contains(&r));
            if !ok {
                return Err(quick_error(format!(
                    "compilations/{on} has an invalid omission reason"
                )));
            }
        }
        for row in current {
            let id = getitem(row, "id")?
                .as_str()
                .ok_or_else(|| Exc::Type("id must be str".into()))?
                .to_string();
            let start = getitem(row, "versionStart")?
                .as_str()
                .ok_or_else(|| Exc::Type("versionStart must be str".into()))?
                .to_string();
            let comp = getitem(row, "compilationRegisterId")?
                .as_str()
                .map(str::to_string);
            if start.as_str() > on.as_str() || !root_current.contains(&(id, start, comp)) {
                return Err(quick_error(format!(
                    "compilations/{on} needs a current compilation missing from the root"
                )));
            }
        }
        omissions.push(Json::obj([
            ("in_force_on", Json::str(on)),
            ("omissions", Json::Array(skipped.clone())),
        ]));

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
        let held_count = current.len() + resolved.len() - wanted.len();
        runner.print(&format!(
            "compilations/{on}: {} to fetch, {held_count} already held, {} with none to fetch, listed above",
            wanted.len(),
            skipped.len(),
        ));
        if wanted.is_empty() {
            std::fs::remove_dir_all(&build_dir)
                .map_err(|e| OsError::new(&e, Some(&build_dir), Style::Errno))?;
            continue;
        }
        let target = pypath::join(&scratch, "acts_resolved.json");
        write_json_atomic(&target, &Json::Array(wanted.clone()), Some(1), OUTPUT_DEPTH)?;
        let epub_dir = pypath::join(&build_dir, "epub");
        runner.download(&scratch, &epub_dir)?;
        for name in ["extract", "pii_scan", "pii_scan2"] {
            runner.stage(&scratch, &[name], false)?;
        }
        let got = extracted(&scratch)?;
        let got_ids: BTreeSet<String> = got
            .iter()
            .filter_map(|r| r.get("id").and_then(Json::as_str).map(str::to_string))
            .collect();
        let missing: Vec<String> = wanted
            .iter()
            .filter_map(|r| r.get("id").and_then(Json::as_str).map(str::to_string))
            .collect::<BTreeSet<_>>()
            .difference(&got_ids)
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(quick_error(format!(
                "compilations/{on} holds no rows for {}",
                missing.join(", ")
            )));
        }
        rows.extend(got);
    }

    check_windows(&rows)?;
    let folder_path = Path::new(&folder);
    if folder_path.is_dir()
        && std::fs::read_dir(folder_path)
            .map_err(|e| OsError::new(&e, Some(&folder), Style::Errno))?
            .next()
            .is_none()
    {
        std::fs::remove_dir(folder_path)
            .map_err(|e| OsError::new(&e, Some(&folder), Style::Errno))?;
    }
    let sources_path = pypath::join(staging, "sources.json");
    let sources_text = crate::pyio::read_text(&sources_path)?;
    let mut source = pyjson::loads_standard(&sources_text).map_err(json_error)?;
    set_field(&mut source, "historical_omissions", Json::Array(omissions));
    write_json_atomic(&sources_path, &source, Some(1), OUTPUT_DEPTH)
}

/// Validate a date string as `YYYY-MM-DD`, returning Python's exact argparse
/// type-error wording without prefixing `'argument ...'`.
pub fn validate_in_force_date(value: &str) -> Result<(), String> {
    let q = py::repr_str(value);
    if !pyre::fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", 0, value) {
        return Err(format!("{q} is not a date as YYYY-MM-DD"));
    }
    let year: i64 = value[0..4].parse().unwrap_or(0);
    let month: i64 = value[5..7].parse().unwrap_or(0);
    let day: i64 = value[8..10].parse().unwrap_or(0);
    py::Date::new(year, month, day)
        .map(|_| ())
        .map_err(|err| format!("{q} is not a date: {err}"))
}

fn publish(staging: &str, out: &str) -> Result<(), Exc> {
    let out_path = Path::new(out);
    if out_path.exists() {
        std::fs::remove_dir(out_path).map_err(|e| OsError::new(&e, Some(out), Style::Errno))?;
    }
    std::fs::rename(staging, out).map_err(|e| OsError::new(&e, Some(staging), Style::Errno))?;
    Ok(())
}

fn is_wrapped_failure(err: &Exc) -> bool {
    matches!(
        err,
        Exc::Os(_)
            | Exc::Value { .. }
            | Exc::Runtime(_)
            | Exc::Sqlite(_)
            | Exc::Other {
                class: "DownloadError" | "QuickError",
                ..
            }
    )
}

/// Build beside `out`, then move the checked result to it; returns a query hit.
pub fn build(
    runner: &mut dyn Runner,
    module: &str,
    out: &str,
    dates: &[String],
) -> Result<String, Exc> {
    let staging = prepare(module, out)?;
    let attempt = (|| -> Result<String, Exc> {
        let hit = assemble(runner, &staging)?;
        if !dates.is_empty() {
            add_compilations(runner, module, &staging, dates)?;
        }
        publish(&staging, out)?;
        Ok(hit)
    })();
    match attempt {
        Ok(hit) => Ok(hit),
        Err(err) if is_wrapped_failure(&err) => Err(quick_error(format!(
            "{err}; the partial build stays in {staging}"
        ))),
        Err(err) => Err(err),
    }
}

/// Run `quick` CLI arguments with an explicit `Runner` and module directory.
pub fn run_cli(
    runner: &mut dyn Runner,
    module: &str,
    argv: &[String],
    run: &mut Run,
) -> Result<i32, Exit> {
    let parser = Parser::new("python -m fadden quick")
        .description("Build and index a preset of principal Acts, then print the MCP folder setting.")
        .arg(
            Action::option("--out")
                .required()
                .help("an absent or empty folder"),
        )
        .arg(
            Action::append("--in-force-on", ArgValue::List(Vec::new()))
                .metavar("YYYY-MM-DD")
                .help("also hold each Act's earlier compilation in force on this date; repeat for more"),
        );
    let args = parser.parse(argv)?;
    let out_raw = args.str("out").unwrap_or_default();
    let dates = args.list("in_force_on");
    for d in &dates {
        if let Err(reason) = validate_in_force_date(d) {
            return Err(parser.error(&format!("argument --in-force-on: {reason}")));
        }
    }
    let out = pypath::realpath(&out_raw).unwrap_or_else(|_| pypath::abspath(&out_raw));
    let started = runner.monotonic();
    match build(runner, module, &out, &dates) {
        Ok(hit) => {
            let elapsed = (runner.monotonic() - started).max(0.0);
            let secs = round_half_even(elapsed);
            run.stdout.extend_from_slice(hit.as_bytes());
            run.print(&format!(
                "Built and indexed {} Acts in {secs} seconds.",
                PRESET.len()
            ));
            run.print(
                "Set this for the aus-accounting-mcp server, then call search_tax_legislation:",
            );
            run.print(&format!("AUS_ACCOUNTING_CORPUS_ROOT={out}"));
            Ok(0)
        }
        Err(err) if is_wrapped_failure(&err) => {
            run.eprint(&format!("error: {err}"));
            Ok(1)
        }
        Err(err) => Ok(run.uncaught(&err)),
    }
}

/// CLI entrypoint (`python -m fadden quick`).
pub fn main(argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let module = module_dir();
    let binary = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "fadden".to_string());
    let mut http = http::connect();
    let mut runner_run = Run::default();
    let code = {
        let mut runner = RealRunner {
            binary,
            http: http.as_mut(),
            run: &mut runner_run,
        };
        run_cli(&mut runner, &module, argv, run)
    };
    run.stdout.splice(0..0, runner_run.stdout);
    run.stderr.splice(0..0, runner_run.stderr);
    code
}
