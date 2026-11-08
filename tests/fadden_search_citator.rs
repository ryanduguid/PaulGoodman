// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

//! `fadden search` as a citator: the rulings that cite a section. Every title, row and
//! ruling is fabricated.
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tax_radar_au::fadden::rulings::NOTICE;

const ACT: &str = "C2099A00001";
const NAME: &str = "Sample Levy Act 2099";

/// A finished corpus of one title holding sections 1, 2-1, 2-5, 3-1, 82KZMA and 82KZMD.
fn write_corpus(root: &Path) {
    let sections = [
        (
            "1",
            "1 Short title",
            "This Act is the Sample Levy Act 2099.",
        ),
        (
            "2-1",
            "2-1 Widgets",
            "A widget attracts the sample levy at the rate in section 3-1.",
        ),
        ("2-5", "2-5 Gadgets", "A gadget attracts no sample levy."),
        (
            "3-1",
            "3-1 Rate",
            "The rate of sample levy is 10% of the value.",
        ),
        (
            "82KZMA",
            "82KZMA Prepaid widgets",
            "A prepaid widget is a widget.",
        ),
        (
            "82KZMD",
            "82KZMD Leased widgets",
            "A leased widget is a widget.",
        ),
    ];
    let directory = root.join("markdown").join(ACT);
    std::fs::create_dir_all(&directory).unwrap();
    let mut jsonl = String::new();
    let mut markdown = format!("# {NAME}\n\n");
    for (ordinal, (section, heading, text)) in (1..).zip(sections) {
        let row = json!({
            "register_id": ACT, "act": NAME, "collection": "Act",
            "compilation_number": "1", "compilation_date": "2099-07-01",
            "version_is_current": true, "retrieved": "2099-08-01",
            "row_id": format!("{ACT}:{ordinal:04}:{section}"), "section": section,
            "heading": heading, "container": null, "kind": "section",
            "granularity": "section", "text": text,
            "source_url": format!("https://example.invalid/{ACT}.epub"),
            "register_page": format!("https://example.invalid/{ACT}"),
            "authorised": false,
        });
        jsonl.push_str(&format!("{row}\n"));
        markdown.push_str(&format!("##### {heading}\n\n{text}\n\n"));
    }
    std::fs::write(directory.join("sections.jsonl"), jsonl).unwrap();
    std::fs::write(directory.join(format!("{ACT}.md")), markdown).unwrap();
    let title = json!({
        "register_id": ACT, "name": NAME, "collection": "Act",
        "compilation_number": "1", "compilation_date": "2099-07-01",
        "version_is_current": true, "retrieved": "2099-08-01", "jsonl_rows": 6,
        "register_page": format!("https://example.invalid/{ACT}"),
        "source_url": format!("https://example.invalid/{ACT}.epub"),
    });
    let sources = json!({"retrieved": "2099-08-01", "titles": [title]});
    std::fs::write(root.join("sources.json"), sources.to_string()).unwrap();
}

/// A rulings row as the `rulings` stage writes it.
fn ruling(docid: &str, title: &str, paragraph: &str, text: &str, withdrawal: &str) -> Value {
    json!({
        "docid": docid, "family": "Taxation Ruling", "title": title,
        "paragraph": paragraph, "numbering": "document", "heading": "Ruling",
        "text": text, "source_url": format!("https://example.invalid/{docid}"),
        "source_sha256": "0".repeat(64), "fetched_on": "2099-08-02",
        "licence_basis": "document-notice", "withdrawal_status": withdrawal,
        "withdrawal_notice": "",
    })
}

/// A run directory holding `rows`, the stage's licence notice and a manifest that
/// counts them and excludes nothing.
fn write_run(dir: &Path, rows: &[Value]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let jsonl: String = rows.iter().map(|row| format!("{row}\n")).collect();
    std::fs::write(dir.join("rulings.jsonl"), jsonl).unwrap();
    std::fs::write(
        dir.join("LICENCE-NOTICE.md"),
        format!("# ATO Legal Database material\n\n> {NOTICE}\n"),
    )
    .unwrap();
    write_manifest(dir, json!(rows.len()), &[]);
    dir.to_path_buf()
}

/// The fields of the stage's `manifest.json` that `build` reads.
fn write_manifest(dir: &Path, rows: Value, excluded: &[&str]) {
    let excluded: Vec<Value> = excluded
        .iter()
        .map(|docid| json!({"docid": docid, "reason": "fabricated"}))
        .collect();
    let manifest = json!({"stage": "rulings", "rows": rows, "excluded": excluded});
    std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
}

const TR: &str = "TXR/TR20991/NAT/ATO/00001";
const TD: &str = "TXD/TD20992/NAT/ATO/00001";

/// 2 runs: TR 2099/1 cites section 2-1 by name and once bare; TD 2099/2 cites
/// subsection 2-1(1) in the first run and carries a withdrawal notice in the second.
fn write_runs(base: &Path) -> (PathBuf, PathBuf) {
    let tr = "TR 2099/1 Income tax: fabricated widgets";
    let td = "TD 2099/2 Income tax: fabricated gadgets";
    let first = write_run(
        &base.join("run1"),
        &[
            ruling(
                TR,
                tr,
                "12",
                "Section 2-1 of the Sample Levy Act 2099 applies to widgets.",
                "UNKNOWN",
            ),
            ruling(TR, tr, "13", "So section 2-1 applies.", "UNKNOWN"),
            ruling(
                TD,
                td,
                "4",
                "See subsection 2-1(1) of the Sample Levy Act 2099.",
                "UNKNOWN",
            ),
        ],
    );
    let second = write_run(
        &base.join("run2"),
        &[ruling(
            TD,
            td,
            "1",
            "This determination is withdrawn.",
            "NOTICE_PRESENT",
        )],
    );
    (first, second)
}

fn fadden(args: &[&str], cwd: &Path) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fadden"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace("\r\n", "\n");
    (
        out.status.code().unwrap(),
        text(&out.stdout),
        text(&out.stderr),
    )
}

fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A temporary folder holding `corpus`, and an index built with `rulings`.
fn indexed(rulings: &[&Path]) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    write_corpus(&dir.path().join("corpus"));
    let mut args = vec!["search".to_string(), "build".into(), "corpus".into()];
    args.extend(["--out".into(), "index.sqlite".into()]);
    for run in rulings {
        args.extend(["--rulings".into(), s(run)]);
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, out, err) = fadden(&args, dir.path());
    assert_eq!(code, 0, "{out}{err}");
    (dir, out)
}

#[test]
fn refs_lists_the_rulings_that_cite_a_section_by_its_act() {
    let runs = tempfile::tempdir().unwrap();
    let (first, second) = write_runs(runs.path());
    write_manifest(&second, json!(1), &["TXR/TR20993/NAT/ATO/00001"]);
    let (dir, out) = indexed(&[&first, &second]);
    assert!(
        out.contains(
            "indexed 4 ruling paragraphs from 2 runs with 3 section references\n\
             excluded by the rulings stage and not indexed: 1 documents\n"
        ),
        "{out}"
    );
    let row = format!("{ACT}:0002:2-1");
    let (code, out, err) = fadden(
        &["search", "refs", "index.sqlite", &row, "--json"],
        dir.path(),
    );
    assert_eq!(code, 0, "{err}");
    let report: Value = serde_json::from_str(out.trim()).unwrap();
    let cited: Vec<(&str, &str, &str, &str)> = report["cited_by_rulings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            let f = |k: &str| r[k].as_str().unwrap();
            (
                f("docid"),
                f("paragraph"),
                f("citation"),
                f("withdrawal_status"),
            )
        })
        .collect();
    // The bare `section 2-1` names no Act, so it is not read as this one.
    assert_eq!(
        cited,
        [
            (
                TR,
                "12",
                "Section 2-1 of the Sample Levy Act 2099",
                "UNKNOWN"
            ),
            (
                TD,
                "4",
                "subsection 2-1(1) of the Sample Levy Act 2099",
                "NOTICE_PRESENT"
            ),
        ]
    );
    let first = &report["cited_by_rulings"][0];
    assert_eq!(first["title"], "TR 2099/1 Income tax: fabricated widgets");
    assert_eq!(first["fetched_on"], "2099-08-02");
    assert_eq!(first["source_url"], format!("https://example.invalid/{TR}"));
    let (code, out, _) = fadden(&["search", "refs", "index.sqlite", &row], dir.path());
    assert_eq!(code, 0);
    assert!(
        out.contains(
            "  cited by ruling TR 2099/1 Income tax: fabricated widgets, paragraph 12 \
         (Section 2-1 of the Sample Levy Act 2099): captured 2099-08-02, https://example.invalid/"
        ),
        "{out}"
    );
    assert!(
        out.contains("captured 2099-08-02, withdrawal notice captured, "),
        "{out}"
    );
    assert!(out.contains("status is unknown, not current"), "{out}");
    // No ruling cites section 2-5: the text says so and keeps the notice, which says a
    // citation naming no Act is not read.
    let (_, out, _) = fadden(
        &["search", "refs", "index.sqlite", &format!("{ACT}:0003:2-5")],
        dir.path(),
    );
    assert!(!out.contains("cited by ruling"), "{out}");
    assert!(
        out.contains("\n  no captured ruling cites this by its Act\nRuling paragraphs come"),
        "{out}"
    );
    assert!(
        out.contains("A citation naming no Act is not read."),
        "{out}"
    );
    assert!(!out.contains("holds no rulings"), "{out}");
}

/// An index built without `--rulings` is byte for byte the index an earlier build
/// wrote (the golden search fixtures pin its digest), so this covers both.
#[test]
fn an_index_without_rulings_says_so_and_has_no_ruling_field() {
    let (dir, out) = indexed(&[]);
    assert!(!out.contains("ruling"), "{out}");
    let row = format!("{ACT}:0002:2-1");
    let (code, out, _) = fadden(
        &["search", "refs", "index.sqlite", &row, "--json"],
        dir.path(),
    );
    assert_eq!(code, 0);
    let report: Value = serde_json::from_str(out.trim()).unwrap();
    assert!(report.get("cited_by_rulings").is_none(), "{report}");
    let (code, out, _) = fadden(&["search", "refs", "index.sqlite", &row], dir.path());
    assert_eq!(code, 0);
    assert!(
        out.contains("\n  this index holds no rulings; build with --rulings to list them\nIndex of the corpus"),
        "{out}"
    );
    assert!(
        !out.contains("no captured ruling") && !out.contains("Ruling paragraphs"),
        "{out}"
    );
}

#[test]
fn a_run_the_stage_could_not_have_written_stops_the_build() {
    let good = ruling(
        TR,
        "TR 2099/1 Fabricated",
        "1",
        "Section 2-1 of the Sample Levy Act 2099.",
        "UNKNOWN",
    );
    let mut basis = good.clone();
    basis["licence_basis"] = json!("crown-copyright");
    let mut status = good.clone();
    status.as_object_mut().unwrap().remove("withdrawal_status");
    let mut paragraph = good.clone();
    paragraph["paragraph"] = json!(1);
    let manifest =
        |rows: &str| format!(r#"{{"stage": "rulings", "rows": {rows}, "excluded": []}}"#);
    let short = manifest("2");
    let no_count = manifest("\"1\"");
    // Each case: what it tests, the rows, a file the run then lacks (no text) or holds
    // instead, and the refusal.
    let cases: Vec<(&str, Vec<Value>, Option<(&str, Option<&str>)>, &str)> = vec![
        (
            "no notice",
            vec![good.clone()],
            Some(("LICENCE-NOTICE.md", None)),
            "cannot read LICENCE-NOTICE.md",
        ),
        (
            "other notice",
            vec![good.clone()],
            Some(("LICENCE-NOTICE.md", Some("All rights reserved.\n"))),
            "LICENCE-NOTICE.md is not the rulings stage's",
        ),
        (
            "basis",
            vec![basis],
            None,
            "rulings.jsonl:1 has no licence basis the rulings stage writes",
        ),
        (
            "status",
            vec![status],
            None,
            "rulings.jsonl:1 is not a rulings row",
        ),
        (
            "paragraph",
            vec![paragraph],
            None,
            "rulings.jsonl:1 is not a rulings row",
        ),
        ("empty", vec![], None, "rulings.jsonl has no rows"),
        (
            "no manifest",
            vec![good.clone()],
            Some(("manifest.json", None)),
            "cannot read manifest.json",
        ),
        (
            "other stage",
            vec![good.clone()],
            Some((
                "manifest.json",
                Some(r#"{"stage": "discover", "rows": 1, "excluded": []}"#),
            )),
            "manifest.json is not the rulings stage's",
        ),
        (
            "no count",
            vec![good.clone()],
            Some(("manifest.json", Some(&no_count))),
            "manifest.json is not the rulings stage's",
        ),
        (
            "rows lost",
            vec![good.clone()],
            Some(("manifest.json", Some(&short))),
            "manifest.json counts 2 rows but rulings.jsonl holds 1",
        ),
    ];
    for (what, rows, change, message) in cases {
        let dir = tempfile::tempdir().unwrap();
        write_corpus(&dir.path().join("corpus"));
        let run = write_run(&dir.path().join("run"), &rows);
        match change {
            None => {}
            Some((name, None)) => std::fs::remove_file(run.join(name)).unwrap(),
            Some((name, Some(text))) => std::fs::write(run.join(name), text).unwrap(),
        }
        let args = [
            "search",
            "build",
            "corpus",
            "--out",
            "index.sqlite",
            "--rulings",
            "run",
        ];
        let (code, out, err) = fadden(&args, dir.path());
        assert_eq!(code, 1, "{what}: {out}{err}");
        assert!(err.contains(message), "{what}: {err}");
        assert!(!dir.path().join("index.sqlite").exists(), "{what}");
    }
}

fn quoted(dir: &Path, row: &str, quotation: &str) -> (i32, Value) {
    let (code, out, err) = fadden(
        &["search", "quote", "index.sqlite", row, quotation, "--json"],
        dir,
    );
    let report = serde_json::from_str(out.trim()).unwrap_or_else(|_| panic!("{out}{err}"));
    (code, report)
}

#[test]
fn quote_grades_a_quotation_against_one_row_and_exits_0_only_if_it_is_held() {
    let (dir, _) = indexed(&[]);
    let row = format!("{ACT}:0002:2-1");
    let cases = [
        ("A widget attracts the sample levy", 0, "exact", json!(null)),
        (
            "A  widget attracts\nthe sample levy",
            0,
            "normalised",
            json!(null),
        ),
        ("A widget attracts the sample tax", 1, "partial", json!(5)),
        ("Gadgets are exempt", 1, "none", json!(null)),
    ];
    let row_ids = [&row; 4].map(String::to_string);
    // Edits that reverse the law: a changed rate, and a dropped 'no'.
    let edits = [
        (
            format!("{ACT}:0004:3-1"),
            (
                "The rate of sample levy is 15% of the value",
                1,
                "partial",
                json!(6),
            ),
        ),
        (
            format!("{ACT}:0003:2-5"),
            ("A gadget attracts sample levy", 1, "none", json!(null)),
        ),
    ];
    let all = row_ids.into_iter().zip(cases).chain(edits);
    for (row, (quotation, code, grade, matched)) in all {
        let (exit, report) = quoted(dir.path(), &row, quotation);
        assert_eq!(
            (exit, report["grade"].as_str()),
            (code, Some(grade)),
            "{quotation}"
        );
        assert_eq!(report["matched_words"], matched, "{quotation}");
        assert_eq!(report["quote"], quotation);
    }
    let (_, report) = quoted(dir.path(), &row, "A widget attracts the sample levy");
    assert_eq!(report["row_id"], row);
    assert_eq!(report["register_id"], ACT);
    assert_eq!(report["quote_words"], 6);
    assert_eq!(
        (report["other_rows"].clone(), report["distinct"].clone()),
        (json!(0), json!(false))
    );
    assert_eq!(report["compilation_number"], "1");
    assert_eq!(report["compilation_date"], "2099-07-01");
    assert_eq!(report["version_is_current"], true);
    assert_eq!(report["sections_sha256"].as_str().unwrap().len(), 64);
    // The same digest cite-check reports, which ties the grade to this build.
    assert_eq!(report["sources_sha256"].as_str().unwrap().len(), 64);
    // Sections 2-5 and 3-1 also hold these words.
    let (_, report) = quoted(dir.path(), &row, "sample levy");
    assert_eq!(
        (report["grade"].as_str(), report["other_rows"].as_u64()),
        (Some("exact"), Some(2))
    );
    let (_, report) = quoted(dir.path(), &row, "Gadgets are exempt");
    assert_eq!(
        (report["other_rows"].clone(), report["distinct"].clone()),
        (json!(null), json!(null))
    );
}

#[test]
fn quote_prints_the_row_grade_and_what_a_grade_does_not_show() {
    let (dir, _) = indexed(&[]);
    let row = format!("{ACT}:0002:2-1");
    let args = [
        "search",
        "quote",
        "index.sqlite",
        &row,
        "A widget attracts the sample levy",
    ];
    let (code, out, _) = fadden(&args, dir.path());
    assert_eq!(code, 0);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[0],
        format!("{row}: compilation 1 (2099-07-01), current, retrieved 2099-08-01")
    );
    assert_eq!(
        lines[1],
        "  quotation exact, not distinct, 0 other rows hold it: A widget attracts the sample levy"
    );
    assert!(
        lines[2].starts_with("Exact and normalised show")
            && lines[2].contains("Partial and none are not matches"),
        "{out}"
    );
    assert!(
        lines[3].starts_with("Index of the corpus retrieved 2099-08-01"),
        "{out}"
    );
}

#[test]
fn quote_refuses_an_unknown_row_or_a_quotation_without_letters_or_digits() {
    let (dir, _) = indexed(&[]);
    let row = format!("{ACT}:0002:2-1");
    for (args, code, message) in [
        (
            vec![
                "search",
                "quote",
                "index.sqlite",
                "C2099A00001:0009:9",
                "A widget",
            ],
            1,
            "C2099A00001:0009:9 is not in the index",
        ),
        (
            vec!["search", "quote", "index.sqlite", &row, " "],
            1,
            "the quotation has no letters or digits",
        ),
        // The row ends with a full stop, so `.` would otherwise grade `exact`.
        (
            vec!["search", "quote", "index.sqlite", &row, "."],
            1,
            "the quotation has no letters or digits",
        ),
        (
            vec!["search", "quote", "index.sqlite", &row, "*"],
            1,
            "the quotation has no letters or digits",
        ),
        (
            vec!["search", "quote", "absent.sqlite", &row, "A widget"],
            1,
            "the index file does not exist",
        ),
        (
            vec!["search", "quote", "index.sqlite", &row],
            2,
            "usage: python -m fadden search quote",
        ),
    ] {
        let (exit, out, err) = fadden(&args, dir.path());
        assert_eq!((exit, out.as_str()), (code, ""), "{args:?}");
        assert!(err.contains(message), "{args:?}: {err}");
    }
}

fn cite_check(dir: &Path, draft: &str, extra: &[&str]) -> (i32, String) {
    std::fs::write(dir.join("draft.md"), draft).unwrap();
    let mut args = vec!["search", "cite-check", "index.sqlite", "draft.md"];
    args.extend(extra);
    let (code, out, err) = fadden(&args, dir);
    assert!(err.is_empty(), "{err}");
    (code, out)
}

#[test]
fn a_section_the_title_does_not_hold_names_the_held_sections_either_side() {
    let (dir, _) = indexed(&[]);
    let draft = [
        "Section 2-3 of the Sample Levy Act 2099 applies.",
        "Section 2-10 of the Sample Levy Act 2099 applies.",
        "Section 9 of the Sample Levy Act 2099 applies.",
        "Section 2-1 of the Sample Levy Act 2099 applies.",
        "Section 2-3 of the Missing Act 2099 applies.",
        "Section 82KZMB of the Sample Levy Act 2099 applies.",
        "Division 7 of the Sample Levy Act 2099 applies.",
    ]
    .join("\n");
    let (code, out) = cite_check(dir.path(), &draft, &["--json"]);
    assert_eq!(code, 1);
    let found: Vec<(String, Value)> = out
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .map(|item| {
            (
                item["outcome"].as_str().unwrap().to_string(),
                item["nearest"].clone(),
            )
        })
        .collect();
    let expected = [
        ("not_held", json!(["2-1", "2-5"])),
        // By number, not as text: 2-10 follows 2-5.
        ("not_held", json!(["2-5", "3-1"])),
        ("not_held", json!(["3-1", "82KZMA"])),
        ("resolved", json!(null)),
        ("not_held", json!(null)),
        // By the letters after the number: 82KZMB falls between 82KZMA and 82KZMD.
        ("not_held", json!(["82KZMA", "82KZMD"])),
        ("not_held", json!(null)),
    ];
    let expected: Vec<(String, Value)> = expected
        .into_iter()
        .map(|(o, n)| (o.to_string(), n))
        .collect();
    assert_eq!(found, expected);
    let (_, out) = cite_check(dir.path(), &draft, &[]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[0],
        "line 1: not_held: Section 2-3 of the Sample Levy Act 2099: "
    );
    assert_eq!(lines[1], "  nearest held sections: 2-1, 2-5");
    assert_eq!(out.matches("nearest held sections").count(), 4, "{out}");
}
