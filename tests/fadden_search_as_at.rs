// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

//! `fadden search --as-at`: rows only from a title the build holds for the date, a
//! refusal for any other. Every title and date is fabricated.
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use tax_radar_au::fadden::as_at::CAPTURED_MEANS;
use tax_radar_au::fadden::search::{self, DatedReport};
use tax_radar_au::pyjson::{Json, Layout, dumps};

const CURRENT: &str = "C2099A00001";
const SUPERSEDED: &str = "C2099A00002";
/// Each title's compilation starts here and was retrieved at the end of the window.
const START: &str = "2099-07-01";
const RETRIEVED: &str = "2099-08-01";
const NEXT_VERSION: &str = "2099-07-20";

fn row(rid: &str, ordinal: u32, section: &str, heading: &str, text: &str) -> Value {
    json!({
        "register_id": rid, "compilation_number": "1", "compilation_date": START,
        "row_id": format!("{rid}:{ordinal:04}:{section}"), "section": section,
        "heading": heading, "text": text, "authorised": false,
    })
}

fn write_title(root: &Path, rid: &str, rows: &[Value]) {
    let directory = root.join("markdown").join(rid);
    std::fs::create_dir_all(&directory).unwrap();
    let lines: String = rows.iter().map(|r| format!("{r}\n")).collect();
    std::fs::write(directory.join("sections.jsonl"), lines).unwrap();
    std::fs::write(directory.join(format!("{rid}.md")), "# Fabricated\n").unwrap();
}

/// A build of one current title and one whose successor started on `NEXT_VERSION`.
fn build(dir: &Path, superseded_from: Option<&str>) -> String {
    let root = dir.join("corpus");
    write_title(
        &root,
        CURRENT,
        &[
            row(
                CURRENT,
                1,
                "1",
                "1 Widgets",
                "A widget attracts the sample levy.",
            ),
            row(
                CURRENT,
                2,
                "2",
                "2 Definitions",
                "widget means a small device.",
            ),
        ],
    );
    write_title(
        &root,
        SUPERSEDED,
        &[row(
            SUPERSEDED,
            1,
            "9",
            "9 Definitions",
            "widget means a large device.",
        )],
    );
    let title = |rid: &str, name: &str, rows: u32, current: bool, next: Option<&str>| {
        json!({
            "register_id": rid, "name": name, "collection": "Act",
            "compilation_number": "1", "compilation_date": START,
            "version_is_current": current, "current_version_start": next,
            "retrieved": RETRIEVED, "jsonl_rows": rows,
            "register_page": format!("https://example.invalid/{rid}"),
            "source_url": format!("https://example.invalid/{rid}.epub"),
        })
    };
    let titles = json!({"retrieved": RETRIEVED, "titles": [
        title(CURRENT, "Sample Levy Act 2099", 2, true, None),
        title(SUPERSEDED, "Other Sample Act 2099", 1, false, superseded_from),
    ]});
    std::fs::write(root.join("sources.json"), titles.to_string()).unwrap();
    let out = dir.join("index.sqlite");
    search::build(root.to_str().unwrap(), out.to_str().unwrap()).unwrap();
    out.to_str().unwrap().to_string()
}

fn value(json: &Json) -> Value {
    serde_json::from_str(&dumps(json, Layout::Compact)).unwrap()
}

fn cli(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fadden"))
        .arg("search")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace("\r\n", "\n");
    (
        out.status.code().unwrap(),
        text(&out.stdout),
        text(&out.stderr),
    )
}

fn json_lines(text: &str) -> Vec<Value> {
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn query(index: &str, title: Option<&str>, date: &str) -> DatedReport {
    let connection = search::open_index(index).unwrap();
    search::query_as_at(&connection, &["widget".to_string()], title, 10, date).unwrap()
}

/// (rows, refused status, next step) for the one title `title` names.
fn answer(index: &str, title: &str, date: &str) -> (usize, Option<Value>, Option<Value>) {
    let report = query(index, Some(title), date);
    let refusal = report.refusals.first().map(value);
    let standing = refusal.as_ref().map(|r| r["as_at"].clone());
    (
        report.hits.len(),
        standing.as_ref().map(|s| s["status"].clone()),
        standing.as_ref().map(|s| s["next_step"].clone()),
    )
}

#[test]
fn each_status_gives_rows_only_when_captured_and_otherwise_names_the_next_step() {
    let dir = tempfile::tempdir().unwrap();
    let index = build(dir.path(), Some(NEXT_VERSION));
    assert_eq!(answer(&index, CURRENT, START), (2, None, None));
    assert_eq!(answer(&index, CURRENT, RETRIEVED), (2, None, None));
    assert_eq!(answer(&index, SUPERSEDED, "2099-07-19"), (1, None, None));
    let cases = [
        (
            CURRENT,
            "2099-06-30",
            "before_compilation",
            "read the Register's earlier compilations",
        ),
        (
            SUPERSEDED,
            NEXT_VERSION,
            "superseded",
            "a later version applies; read the Register",
        ),
        (
            CURRENT,
            "2099-08-02",
            "after_retrieval",
            "run check_current and read the Register",
        ),
    ];
    for (title, date, status, step) in cases {
        assert_eq!(
            answer(&index, title, date),
            (0, Some(json!(status)), Some(json!(step))),
            "{title} on {date}"
        );
    }
}

#[test]
fn a_date_before_the_compilation_is_reported_even_when_no_word_matches() {
    let dir = tempfile::tempdir().unwrap();
    let index = build(dir.path(), Some(NEXT_VERSION));
    let connection = search::open_index(&index).unwrap();
    let words = ["absentword".to_string()];
    let report = search::query_as_at(&connection, &words, Some(CURRENT), 10, "2000-01-01").unwrap();
    assert!(report.hits.is_empty());
    let refusal = value(&report.refusals[0]);
    assert_eq!(refusal["as_at"]["status"], "before_compilation");
    assert_eq!(refusal["matching_rows"], 0);
    let summary = value(&report.summary);
    assert_eq!(summary["as_at_summary"]["titles"]["before_compilation"], 2);
}

#[test]
fn no_row_is_hidden_when_a_search_spans_titles() {
    let dir = tempfile::tempdir().unwrap();
    let index = build(dir.path(), Some(NEXT_VERSION));
    // On this date the current title is captured and the other superseded.
    let report = query(&index, None, NEXT_VERSION);
    let rids: Vec<Value> = report
        .hits
        .iter()
        .map(|h| value(h)["register_id"].clone())
        .collect();
    assert_eq!(rids, [CURRENT, CURRENT]);
    let refusal = value(&report.refusals[0]);
    assert_eq!(
        (
            &refusal["register_id"],
            &refusal["refused"],
            &refusal["matching_rows"]
        ),
        (&json!(SUPERSEDED), &json!(true), &json!(1))
    );
    let summary = value(&report.summary);
    assert_eq!(summary["as_at_summary"]["rows"], 2);
    assert_eq!(summary["as_at_summary"]["withheld_rows"], 1);
    assert_eq!(summary["as_at_summary"]["titles"]["captured"], 1);
    assert_eq!(summary["as_at_summary"]["titles"]["superseded"], 1);
    assert_eq!(summary["as_at_summary"]["covers"], CAPTURED_MEANS);
}

#[test]
fn the_limit_counts_only_rows_it_may_show() {
    let dir = tempfile::tempdir().unwrap();
    let index = build(dir.path(), Some(NEXT_VERSION));
    let connection = search::open_index(&index).unwrap();
    let words = ["widget".to_string()];
    let report = search::query_as_at(&connection, &words, None, 1, NEXT_VERSION).unwrap();
    assert_eq!(report.hits.len(), 1);
    assert_eq!(value(&report.refusals[0])["matching_rows"], 1);
}

#[test]
fn a_hit_carries_its_status_window_and_what_the_status_covers() {
    let dir = tempfile::tempdir().unwrap();
    let index = build(dir.path(), Some(NEXT_VERSION));
    let hit = value(&query(&index, Some(SUPERSEDED), "2099-07-19").hits[0]);
    assert_eq!(
        hit["as_at"],
        json!({
            "date": "2099-07-19", "status": "captured", "compilation_date": START,
            "superseded_from": NEXT_VERSION, "retrieved": RETRIEVED,
            "covers": CAPTURED_MEANS, "next_step": null,
        })
    );
}

#[test]
fn a_query_with_no_date_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let index = build(dir.path(), None);
    let (code, out, _) = cli(dir.path(), &["query", &index, "widget", "--json"]);
    assert_eq!(code, 0);
    let lines = json_lines(&out);
    assert_eq!(lines.len(), 3);
    assert!(lines.iter().all(|line| line.get("as_at").is_none()));
}

#[test]
fn query_as_at_prints_the_refusal_and_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let index = build(dir.path(), Some(NEXT_VERSION));
    let (code, out, _) = cli(
        dir.path(),
        &[
            "query",
            &index,
            "widget",
            "--title",
            CURRENT,
            "--as-at",
            "2099-06-30",
        ],
    );
    assert_eq!(code, 1);
    assert!(out.contains(&format!(
        "refused {CURRENT} (Sample Levy Act 2099): before_compilation on 2099-06-30, \
         2 matching rows withheld: read the Register's earlier compilations"
    )));
    assert!(out.contains("0 captured, 2 before_compilation"));
    assert!(out.contains(CAPTURED_MEANS));
    assert!(out.contains("0 rows."));
    let (code, out, _) = cli(
        dir.path(),
        &[
            "query", &index, "widget", "--title", CURRENT, "--as-at", START,
        ],
    );
    assert_eq!(code, 0);
    assert!(out.contains("2 rows."));
    assert!(out.contains(CAPTURED_MEANS));
}

#[test]
fn query_as_at_json_lists_hits_then_refusals_then_the_summary() {
    let dir = tempfile::tempdir().unwrap();
    let index = build(dir.path(), Some(NEXT_VERSION));
    let (code, out, _) = cli(
        dir.path(),
        &["query", &index, "widget", "--as-at", NEXT_VERSION, "--json"],
    );
    assert_eq!(code, 1);
    let lines = json_lines(&out);
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0]["as_at"]["status"], "captured");
    assert_eq!(lines[1]["as_at"]["status"], "captured");
    assert_eq!(lines[2]["refused"], true);
    assert_eq!(lines[3]["as_at_summary"]["withheld_rows"], 1);
}

#[test]
fn a_date_or_title_that_cannot_be_read_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let index = build(dir.path(), Some(NEXT_VERSION));
    let bad = ["1 July 2099", "2099-7-1", "2099-02-30"];
    for date in bad {
        for words in [&["query", &index, "widget"], &["define", &index, "widget"]] {
            let args = [words.as_slice(), &["--as-at", date]].concat();
            let (code, out, err) = cli(dir.path(), &args);
            assert_eq!((code, out.as_str()), (1, ""), "{date}");
            assert!(err.contains("--as-at must be YYYY-MM-DD"), "{err}");
        }
    }
    let (code, _, err) = cli(
        dir.path(),
        &[
            "query",
            &index,
            "widget",
            "--title",
            "C2099A00009",
            "--as-at",
            START,
        ],
    );
    assert_eq!(code, 1);
    assert!(err.contains("C2099A00009 is not in the index"), "{err}");
}

#[test]
fn dates_a_title_cannot_hold_stop_a_dated_query() {
    let dir = tempfile::tempdir().unwrap();
    let index = build(dir.path(), None);
    let (code, _, err) = cli(dir.path(), &["query", &index, "widget", "--as-at", START]);
    assert_eq!(code, 1);
    assert!(
        err.contains("current_version_start is not a YYYY-MM-DD date"),
        "{err}"
    );
    let (code, _, _) = cli(dir.path(), &["query", &index, "widget"]);
    assert_eq!(code, 0);
}

#[test]
fn define_as_at_refuses_a_date_outside_the_compilation() {
    let dir = tempfile::tempdir().unwrap();
    let index = build(dir.path(), Some(NEXT_VERSION));
    for (date, status) in [
        ("2099-06-30", "before_compilation"),
        ("2099-08-02", "after_retrieval"),
    ] {
        let (code, out, _) = cli(
            dir.path(),
            &[
                "define", &index, "widget", "--title", CURRENT, "--as-at", date, "--json",
            ],
        );
        assert_eq!(code, 1, "{date}");
        let lines = json_lines(&out);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["refused"], true);
        assert_eq!(lines[0]["as_at"]["status"], status);
        assert_eq!(lines[0]["matching_rows"], 1);
    }
    let (code, out, _) = cli(
        dir.path(),
        &[
            "define",
            &index,
            "widget",
            "--title",
            CURRENT,
            "--as-at",
            "2099-06-30",
        ],
    );
    assert_eq!(code, 1);
    assert!(out.contains("refused C2099A00001"));
    assert!(out.contains(CAPTURED_MEANS));
    assert!(!out.contains("small device"));
    assert!(!out.contains("No definition paragraph found"));
}

#[test]
fn define_as_at_answers_from_captured_titles_and_names_the_others() {
    let dir = tempfile::tempdir().unwrap();
    let index = build(dir.path(), Some(NEXT_VERSION));
    let connection = search::open_index(&index).unwrap();
    let report = search::define_as_at(&connection, "widget", None, NEXT_VERSION).unwrap();
    assert_eq!(report.hits.len(), 1);
    let hit = value(&report.hits[0]);
    assert_eq!(hit["register_id"], CURRENT);
    assert_eq!(hit["as_at"]["status"], "captured");
    assert_eq!(value(&report.refusals[0])["register_id"], SUPERSEDED);
}

fn cited(dir: &Path, index: &str, date: Option<&str>) -> (i32, String) {
    let draft = dir.join("draft.md");
    std::fs::write(
        &draft,
        "See section 1 of the Sample Levy Act 2099. See section 7 of the Unheld Act 2099.\n",
    )
    .unwrap();
    let mut args = vec!["cite-check", index, draft.to_str().unwrap(), "--json"];
    args.extend(date.iter().flat_map(|date| ["--as-at", date]));
    let (code, out, _) = cli(dir, &args);
    (code, out)
}

#[test]
fn cite_check_reports_each_cited_titles_status_on_the_date() {
    let dir = tempfile::tempdir().unwrap();
    let index = build(dir.path(), Some(NEXT_VERSION));
    let (_, out) = cited(dir.path(), &index, Some("2099-06-30"));
    let lines = json_lines(&out);
    assert_eq!(lines[0]["outcome"], "resolved");
    assert_eq!(lines[0]["rows"].as_array().unwrap().len(), 1);
    assert_eq!(lines[0]["as_at"]["status"], "before_compilation");
    assert_eq!(lines[1]["outcome"], "not_held");
    assert_eq!(lines[1]["as_at"], Value::Null);
}

#[test]
fn cite_check_exits_1_on_any_status_but_captured() {
    let dir = tempfile::tempdir().unwrap();
    let index = build(dir.path(), Some(NEXT_VERSION));
    let draft = dir.path().join("clean.md");
    std::fs::write(&draft, "See section 1 of the Sample Levy Act 2099.\n").unwrap();
    let draft = draft.to_str().unwrap();
    for (date, code) in [(START, 0), ("2099-06-30", 1), ("2099-08-02", 1)] {
        let (got, out, _) = cli(dir.path(), &["cite-check", &index, draft, "--as-at", date]);
        assert_eq!(got, code, "{date}");
        assert!(out.contains(&format!("as at {date}: ")), "{out}");
        assert!(out.contains(CAPTURED_MEANS));
    }
    let (_, out, _) = cli(
        dir.path(),
        &["cite-check", &index, draft, "--as-at", "2099-06-30"],
    );
    assert!(out.contains("before_compilation: read the Register's earlier compilations"));
}

#[test]
fn cite_check_without_a_date_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let index = build(dir.path(), Some(NEXT_VERSION));
    let (_, out) = cited(dir.path(), &index, None);
    assert!(
        json_lines(&out)
            .iter()
            .all(|line| line.get("as_at").is_none())
    );
}

#[test]
fn the_readme_example_prints_what_the_readme_shows() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let readme = std::fs::read_to_string(repository.join("README.md")).unwrap();
    let (_, shown) = readme.split_once("```text\n").unwrap();
    let (shown, _) = shown.split_once("```").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let index = dir.path().join("demo.sqlite");
    let index = index.to_str().unwrap();
    let demo = repository.join("samples").join("search-demo");
    let (code, _, _) = cli(
        repository,
        &["build", demo.to_str().unwrap(), "--out", index],
    );
    assert_eq!(code, 0);
    let (code, found, _) = cli(repository, &["query", index, "taxable", "sale"]);
    assert_eq!(code, 0);
    assert!(found.contains("2 rows."));
    let draft = demo.join("draft.md");
    let (code, checked, _) = cli(repository, &["cite-check", index, draft.to_str().unwrap()]);
    assert_eq!(code, 1);
    for line in shown.lines() {
        let held = found.contains(line) || checked.contains(line);
        assert!(held, "the README shows a line no command printed: {line}");
    }
}
