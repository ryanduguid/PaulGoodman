// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `fadden/quick.py`: build and index a preset of principal Acts in an empty
//! folder, optionally holding earlier compilations under `compilations/<date>/`.
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use rusqlite::{Connection, OpenFlags};

use super::corpus_paths::{module_dir, write_text};
use super::http::{self, Http};
use super::{Exc, Run, download, iterate, json_error, probe13, retry13, round_half_even, versions};
use crate::argparse::{Action, Exit, Get, Parser, Value as ArgValue};
use crate::py;
use crate::pyjson::{self, Json};
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

/// Files copied into `<staging>/build/`: the contact allowlist, and the
/// staleness checker finalize ships at the corpus root with the 2 modules it imports.
pub const SUPPORT: [&str; 4] = [
    "pii_contact_allowlist.json",
    "check_current.py",
    "corpus_paths.py",
    "http_fetch.py",
];
pub const SMOKE_QUERY: [&str; 2] = ["general", "deductions"];

pub const OMISSION_REASONS: [&str; 3] = [
    "the Register published no compilation for the version in force then",
    "no version was in force then",
    "the version then starts after the current one, so is not in force yet",
];

const OUTPUT_DEPTH: usize = 990;

mod dated;

use dated::add_compilations;
pub use dated::check_windows;

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
    let build_dir = pypath::join(&staging, "build");
    lay_out(module, &build_dir, &Json::Array(titles))?;
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
        let exit = if stage == "versions" {
            versions::run_cli(self.http, scratch, &argv, &mut child_run)
        } else {
            match probe13::run(self.http, scratch, &mut child_run) {
                Ok(()) => Ok(0),
                Err(err) => Ok(child_run.uncaught(&err)),
            }
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
                class: "fadden.download.DownloadError" | "QuickError",
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
    // Local stages run in child processes of this same build, as Python's
    // quick ran the build `python -m fadden` chose. The path only re-runs
    // this program; nothing is trusted because of it.
    // nosemgrep: rust.lang.security.current-exe.current-exe
    let own = std::env::current_exe();
    let binary = own.map_or_else(|_| "fadden".into(), |p| p.to_string_lossy().into_owned());
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

#[cfg(test)]
mod tests {
    use super::*;

    struct TypeErrRunner;
    impl Runner for TypeErrRunner {
        fn stage(&mut self, _: &str, _: &[&str], _: bool) -> Result<String, Exc> {
            Err(Exc::Type("boom".into()))
        }
        fn download(&mut self, _: &str, _: &str) -> Result<(), Exc> {
            Ok(())
        }
        fn retry13(&mut self, _: &str, _: &str) -> Result<(), Exc> {
            Ok(())
        }
        fn print(&mut self, _: &str) {}
    }

    struct NoopHttp;
    impl Http for NoopHttp {
        fn get(
            &mut self,
            _: &str,
            _: Option<usize>,
            _: &mut http::Request,
        ) -> Result<http::Response, http::Failure> {
            Err(http::Failure::Transport("unused".into()))
        }
        fn sleep(&mut self, _: f64) {}
        fn today(&mut self) -> String {
            "2026-01-01".into()
        }
    }

    #[test]
    fn quick_runner_inprocess_and_unwrapped_error_paths() {
        let mut tr = TypeErrRunner;
        assert_eq!(tr.monotonic(), 0.0);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let mut r = Run::default();
        let argv = vec!["--out".into(), out.to_string_lossy().into()];
        assert_eq!(run_cli(&mut tr, &module_dir(), &argv, &mut r).unwrap(), 1);
        assert!(String::from_utf8_lossy(&r.stderr).contains("TypeError"));
        let mut http = NoopHttp;
        let mut rr_run = Run::default();
        let mut rr = RealRunner {
            binary: "fadden".into(),
            http: &mut http,
            run: &mut rr_run,
        };
        let s = dir.path().to_string_lossy().into_owned();
        rr.run_inprocess("versions", &s, &["--help"]).unwrap();
        assert!(
            rr.run_inprocess("versions", &s, &["--no-such-flag"])
                .is_err()
        );
        assert!(rr.run_inprocess("probe13", &s, &[]).is_err());
    }
}
