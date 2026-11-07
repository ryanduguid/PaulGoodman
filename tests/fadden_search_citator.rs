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

/// A finished corpus of one title holding sections 1, 2-1, 2-5 and 3-1.
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
        "version_is_current": true, "retrieved": "2099-08-01", "jsonl_rows": 4,
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

/// A run directory holding `rows` and the stage's licence notice.
fn write_run(dir: &Path, rows: &[Value]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let jsonl: String = rows.iter().map(|row| format!("{row}\n")).collect();
    std::fs::write(dir.join("rulings.jsonl"), jsonl).unwrap();
    std::fs::write(
        dir.join("LICENCE-NOTICE.md"),
        format!("# ATO Legal Database material\n\n> {NOTICE}\n"),
    )
    .unwrap();
    dir.to_path_buf()
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
    let (dir, out) = indexed(&[&first, &second]);
    assert!(
        out.contains("indexed 4 ruling paragraphs from 2 runs with 3 section references"),
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
    let (_, out, _) = fadden(
        &["search", "refs", "index.sqlite", &format!("{ACT}:0003:2-5")],
        dir.path(),
    );
    assert!(
        !out.contains("cited by ruling") && !out.contains("not current"),
        "{out}"
    );
}

#[test]
fn an_index_without_rulings_reports_no_ruling_field() {
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
    let cases: Vec<(&str, Vec<Value>, Option<&str>, &str)> = vec![
        (
            "no notice",
            vec![good.clone()],
            None,
            "cannot read LICENCE-NOTICE.md",
        ),
        (
            "other notice",
            vec![good.clone()],
            Some("All rights reserved.\n"),
            "LICENCE-NOTICE.md is not the rulings stage's",
        ),
        (
            "basis",
            vec![basis],
            Some(""),
            "rulings.jsonl:1 has no licence basis the rulings stage writes",
        ),
        (
            "status",
            vec![status],
            Some(""),
            "rulings.jsonl:1 is not a rulings row",
        ),
        (
            "paragraph",
            vec![paragraph],
            Some(""),
            "rulings.jsonl:1 is not a rulings row",
        ),
        ("empty", vec![], Some(""), "rulings.jsonl has no rows"),
    ];
    for (what, rows, notice, message) in cases {
        let dir = tempfile::tempdir().unwrap();
        write_corpus(&dir.path().join("corpus"));
        let run = write_run(&dir.path().join("run"), &rows);
        match notice {
            None => std::fs::remove_file(run.join("LICENCE-NOTICE.md")).unwrap(),
            Some("") => {}
            Some(text) => std::fs::write(run.join("LICENCE-NOTICE.md"), text).unwrap(),
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
