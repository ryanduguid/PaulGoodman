// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

//! `fadden authorities`: write the closed list llm-tax-guardrails' `gate
//! --authorities` reads, from a search index and any rulings stage output.
//!
//! The file lists each section of a current Act the index holds, as `s 8-1 of the
//! Income Tax Assessment Act 1997`, and each ruling id a rulings run captured, as
//! `TR 2022/1`. The gate refuses a whole file over one entry it cannot read and
//! any key but `authorities`, so an entry it could not read is left out and
//! reported here, and the provenance is printed rather than stored in the file.
//!
//! A listed authority exists in this build. It is not thereby current law for the
//! draft's date or correctly applied. Run this on your own build: the output
//! describes one build and stays out of git, like the rest of a corpus.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use regex::Regex;
use rusqlite::{Connection, Statement};
use sha2::{Digest, Sha256};

use super::{Exc, Run, search};
use crate::argparse::{Action, Exit, Get, Parser, Value};
use crate::py;
use crate::pyjson::{self, Json, Layout, PyStr};
use crate::pypath::PyPath;

// Copied from `tools/drdebits_build/src/gate.rs` in llm-tax-guardrails, with the
// same regex engine, so what is written here is what the gate reads.
const SERIES: &str = r"TR|TD|LCR|GSTR|GSTD|MT|CR|PR|SGR|PCG|PS\s+LA|TPB\(GS\)";
const ACT: &str = r"[A-Z][\w'’-]*(?:\s+(?:[A-Z][\w'’-]*|and|of|for|the|to|on|in|an|a|\([^()]*\)))*\s+Act\s+\d{4}\b";
const SECTION: &str = r"^\d+[A-Z]*(?:-\d+[A-Z]*)?(?:\([0-9A-Za-z]+\))*$";
const RULING_ID: &str = r"(?:\d{2}|\d{4})/D?\d{1,4}(?:[A-Z]+\d*)?";

/// What the file will hold, and what was left out, each with its reason.
#[derive(Default)]
pub struct Export {
    pub sections: BTreeSet<String>,
    pub rulings: BTreeSet<String>,
    pub skipped: Vec<String>,
}

fn pattern(text: &str) -> Regex {
    Regex::new(text).expect("pattern")
}

fn sqlite(error: &rusqlite::Error) -> Exc {
    Exc::Sqlite(error.to_string())
}

/// Each title's register id, name and currency, and how many titles share its name.
fn titles(connection: &Connection) -> Result<Vec<(String, String, bool, i64)>, Exc> {
    let mut titles = connection
        .prepare(
            "SELECT register_id, name, version_is_current, \
             (SELECT count(*) FROM titles AS other WHERE other.name = titles.name) \
             FROM titles ORDER BY register_id",
        )
        .map_err(|e| sqlite(&e))?;
    titles
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .map_err(|e| sqlite(&e))
}

/// One title's numbered rows: row id, section key and container.
fn section_rows(
    rows: &mut Statement,
    rid: &str,
) -> Result<Vec<(String, String, Option<String>)>, Exc> {
    rows.query_map([rid], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })
    .and_then(Iterator::collect::<Result<Vec<_>, _>>)
    .map_err(|e| sqlite(&e))
}

/// List one title's sections, reporting the rows left out.
fn add_title_rows(
    found: Vec<(String, String, Option<String>)>,
    (rid, name): (&str, &str),
    section: &Regex,
    export: &mut Export,
) {
    let mut in_schedule = 0;
    for (row_id, key, container) in found {
        // A numbered row directly under a Schedule is a clause or item, so
        // `s 1 of the Act` would name a section the Act may not have.
        if container.is_some_and(|c| c.starts_with("Schedule")) {
            in_schedule += 1;
        } else if section.is_match(&key) {
            export.sections.insert(format!("s {key} of the {name}"));
        } else {
            export
                .skipped
                .push(format!("row {row_id}: the gate does not read this label"));
        }
    }
    if in_schedule > 0 {
        export.skipped.push(format!(
            "title {rid} ({name}): {in_schedule} rows directly under a Schedule"
        ));
    }
}

/// Each section of each title the gate can read and only that title can answer to.
pub fn add_sections(connection: &Connection, export: &mut Export) -> Result<(), Exc> {
    let act = pattern(&format!("^{ACT}$"));
    let section = pattern(SECTION);
    let titles = titles(connection)?;
    let mut rows = connection
        .prepare(
            "SELECT row_id, section_key, container FROM rows \
             WHERE register_id = ?1 AND section_key IS NOT NULL ORDER BY id",
        )
        .map_err(|e| sqlite(&e))?;
    for (rid, name, current, sharing) in titles {
        let refusal = if !act.is_match(&name) {
            Some("the gate does not read this name as an Act")
        } else if sharing > 1 {
            Some("another title has this name")
        } else if !current {
            Some("this build's compilation is not the current one")
        } else {
            None
        };
        if let Some(reason) = refusal {
            export
                .skipped
                .push(format!("title {rid} ({name}): {reason}"));
            continue;
        }
        let name = py::split_whitespace(&name).join(" ");
        let found = section_rows(&mut rows, &rid)?;
        add_title_rows(found, (&rid, &name), &section, export);
    }
    Ok(())
}

/// Each document in `files`, with the ids its captures' titles name and whether any
/// capture carries a withdrawal notice. A file with no rows, or a row the stage could
/// not have written, is an error.
fn read_documents(
    files: &[(String, String)],
) -> Result<BTreeMap<String, (BTreeSet<String>, bool)>, Exc> {
    let id = pattern(&format!(r"^({SERIES})\s+({RULING_ID})(?:\s|$)"));
    let mut documents: BTreeMap<String, (BTreeSet<String>, bool)> = BTreeMap::new();
    for (path, text) in files {
        let lines = py::splitlines(text);
        if lines.iter().all(|line| py::strip(line).is_empty()) {
            return Err(Exc::value(format!("{path} has no rulings rows")));
        }
        for (index, line) in lines.into_iter().enumerate() {
            if py::strip(line).is_empty() {
                continue;
            }
            let place = format!("{path}:{}", index + 1);
            let row = pyjson::loads_standard(line)
                .map_err(|_| Exc::value(format!("{place} is not JSON")))?;
            let field = |key| row.get(key).and_then(Json::as_str);
            let (Some(docid), Some(title)) = (field("docid"), field("title")) else {
                return Err(Exc::value(format!("{place} is not a rulings row")));
            };
            let document = documents.entry(docid.to_string()).or_default();
            document.0.extend(id.captures(title).map(|found| {
                let series = py::split_whitespace(&found[1]).join(" ");
                format!("{series} {}", &found[2])
            }));
            let status = field("withdrawal_status");
            if !matches!(status, Some("UNKNOWN" | "NOTICE_PRESENT")) {
                return Err(Exc::value(format!(
                    "{place} has no withdrawal status the rulings stage writes"
                )));
            }
            document.1 |= status == Some("NOTICE_PRESENT");
        }
    }
    Ok(documents)
}

/// Each ruling id named at the start of a captured document's title. `files` holds
/// each `rulings.jsonl` with the path to report; a file with no rows, or a row the
/// stage could not have written, stops the export. A document's ids are all those its
/// captures' titles name, and a withdrawal notice in any capture leaves them all out,
/// even where another document lists one.
pub fn add_rulings(files: &[(String, String)], export: &mut Export) -> Result<(), Exc> {
    let mut withdrawn_ids = BTreeSet::new();
    for (docid, (ids, withdrawn)) in read_documents(files)? {
        if withdrawn {
            withdrawn_ids.extend(ids);
            let reason = format!("ruling {docid}: it carries a withdrawal notice");
            export.skipped.push(reason);
        } else if ids.is_empty() {
            export.skipped.push(format!(
                "ruling {docid}: its title does not start with an id the gate reads"
            ));
        } else {
            export.rulings.extend(ids);
        }
    }
    for ruling in withdrawn_ids {
        if export.rulings.remove(&ruling) {
            let reason = format!("ruling id {ruling}: another document with it is withdrawn");
            export.skipped.push(reason);
        }
    }
    Ok(())
}

/// The file's text: the only key the gate accepts, listing every entry in order.
pub fn render(export: &Export) -> String {
    let all = export.rulings.iter().chain(&export.sections);
    let list = Json::Array(all.map(|entry| Json::str(entry)).collect());
    let top = Json::Object(vec![(PyStr::Text("authorities".into()), list)]);
    pyjson::dumps(&top, Layout::Indented) + "\n"
}

fn read(path: &str) -> Result<Vec<u8>, Exc> {
    std::fs::read(PyPath::new(path).fs())
        .map_err(|e| Exc::value(format!("cannot read {path}: {e}")))
}

fn write_new(path: &str, text: &str) -> Result<(), Exc> {
    let target = PyPath::new(path).fs();
    let fail = |e: std::io::Error| Exc::value(format!("cannot write {path}: {e}"));
    let mut file = std::fs::File::create_new(&target).map_err(fail)?;
    file.write_all(text.as_bytes()).map_err(|e| {
        let _ = std::fs::remove_file(&target);
        fail(e)
    })
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn parser() -> Parser {
    Parser::new("python -m fadden authorities")
        .description("Write the closed list of authorities llm-tax-guardrails' gate reads.")
        .arg(Action::positional("index").help("a search index"))
        .arg(
            Action::option("--out")
                .required()
                .help("new JSON file; must not exist"),
        )
        .arg(
            Action::append("--rulings", Value::None)
                .help("a rulings run's rulings.jsonl; repeat for each run"),
        )
}

fn execute(args: &crate::argparse::Namespace, run: &mut Run) -> Result<(), Exc> {
    let connection = search::open_index(&args.str("index").unwrap_or_default())?;
    let mut export = Export::default();
    add_sections(&connection, &mut export)?;
    let mut files = Vec::new();
    let mut digests = Vec::new();
    for path in args.list("rulings") {
        let bytes = read(&path)?;
        digests.push(format!("{path} sha256 {}", sha256(&bytes)));
        let text =
            String::from_utf8(bytes).map_err(|_| Exc::value(format!("{path} is not UTF-8")))?;
        files.push((path, text));
    }
    add_rulings(&files, &mut export)?;
    if export.sections.is_empty() {
        // A list of rulings alone would send every section a draft cites to review.
        return Err(Exc::value(
            "nothing to export: no section the gate can read",
        ));
    }
    let meta = |key: &str| {
        connection.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
            row.get::<_, String>(0)
        })
    };
    let source = format!(
        "From the index of the corpus retrieved {}, sources.json sha256 {}.",
        meta("corpus_retrieved").map_err(|e| sqlite(&e))?,
        meta("sources_sha256").map_err(|e| sqlite(&e))?
    );
    let out = args.str("out").unwrap_or_default();
    write_new(&out, &render(&export))?;
    for line in &export.skipped {
        run.print(&format!("skipped {line}"));
    }
    run.print(&format!(
        "wrote {} sections and {} ruling ids to {out}",
        export.sections.len(),
        export.rulings.len()
    ));
    run.print(&source);
    for digest in &digests {
        run.print(digest);
    }
    run.print(&format!(
        "A listed authority exists in this build; it is not thereby current or correctly applied. {}",
        search::NOTICE
    ));
    Ok(())
}

pub fn main(argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let args = parser().parse(argv)?;
    Ok(match execute(&args, run) {
        Ok(()) => 0,
        Err(error @ (Exc::Value { .. } | Exc::Os(_) | Exc::Sqlite(_))) => {
            run.eprint(&format!("error: {error}"));
            1
        }
        Err(error) => run.uncaught(&error),
    })
}
