// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

//! `fadden authorities`. Every title, row and ruling is fabricated, and the
//! expected entries are written out in the form llm-tax-guardrails' gate reads.
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tax_radar_au::fadden::authorities::{Export, add_rulings, add_sections, render};
use tax_radar_au::fadden::search;

struct Title {
    rid: &'static str,
    name: &'static str,
    current: bool,
    /// Section label and container, one per row.
    rows: Vec<(Option<&'static str>, Option<&'static str>)>,
}

fn act(rid: &'static str, name: &'static str) -> Title {
    Title {
        rid,
        name,
        current: true,
        rows: vec![(Some("5"), None)],
    }
}

fn write_corpus(root: &Path, titles: &[Title]) {
    let mut listed = Vec::new();
    for title in titles {
        let directory = root.join("markdown").join(title.rid);
        std::fs::create_dir_all(&directory).unwrap();
        let mut rows = String::new();
        let mut markdown = format!("# {}\n\n", title.name);
        for (index, (section, container)) in title.rows.iter().enumerate() {
            let heading = format!("{} Heading", section.unwrap_or("Part"));
            let row = json!({
                "register_id": title.rid,
                "row_id": format!("{}:{:04}:{}", title.rid, index + 1, section.unwrap_or("-")),
                "compilation_number": "1", "compilation_date": "2099-07-01",
                "section": section, "heading": heading, "container": container,
                "text": "Fabricated text.", "authorised": false,
            });
            rows.push_str(&format!("{row}\n"));
            markdown.push_str(&format!("##### {heading}\n\nFabricated text.\n\n"));
        }
        std::fs::write(directory.join("sections.jsonl"), rows).unwrap();
        std::fs::write(directory.join(format!("{}.md", title.rid)), markdown).unwrap();
        listed.push(json!({
            "register_id": title.rid, "name": title.name, "collection": "Act",
            "compilation_number": "1", "compilation_date": "2099-07-01",
            "version_is_current": title.current, "retrieved": "2099-08-01",
            "jsonl_rows": title.rows.len(),
            "register_page": format!("https://example.invalid/{}", title.rid),
            "source_url": format!("https://example.invalid/{}.epub", title.rid),
        }));
    }
    std::fs::write(
        root.join("sources.json"),
        json!({"retrieved": "2099-08-01", "titles": listed}).to_string(),
    )
    .unwrap();
}

struct Case {
    dir: tempfile::TempDir,
    index: PathBuf,
}

fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn indexed(titles: &[Title]) -> Case {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("corpus");
    std::fs::create_dir(&root).unwrap();
    write_corpus(&root, titles);
    let index = dir.path().join("index.sqlite");
    search::build(&s(&root), &s(&index)).unwrap();
    Case { dir, index }
}

fn exported(titles: &[Title]) -> Export {
    let case = indexed(titles);
    let mut export = Export::default();
    let connection = search::open_index(&s(&case.index)).unwrap();
    add_sections(&connection, &mut export).unwrap();
    export
}

fn sections(export: &Export) -> Vec<&str> {
    export.sections.iter().map(String::as_str).collect()
}

fn ruling(docid: &str, title: &str, withdrawal: &str) -> String {
    json!({"docid": docid, "title": title, "withdrawal_status": withdrawal}).to_string()
}

fn file(lines: &[String]) -> Vec<(String, String)> {
    vec![("rulings.jsonl".into(), lines.join("\n"))]
}

fn rulings(files: &[(String, String)]) -> Export {
    let mut export = Export::default();
    add_rulings(files, &mut export).unwrap();
    export
}

fn listed(export: &Export) -> Vec<&str> {
    export.rulings.iter().map(String::as_str).collect()
}

fn fadden(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fadden"))
        .arg("authorities")
        .args(args)
        .output()
        .unwrap();
    let text = |b: &[u8]| String::from_utf8_lossy(b).replace("\r\n", "\n");
    (
        out.status.code().unwrap(),
        text(&out.stdout),
        text(&out.stderr),
    )
}

#[test]
fn sections_of_acts_are_written_with_plain_hyphens_and_whole_names() {
    let export = exported(&[Title {
        rows: vec![
            (Some("8\u{2011}1"), None),
            (Some("6AB"), None),
            (None, None),
        ],
        ..act("C2099A00001", "A New Tax System (Sample Levy) Act 2099")
    }]);
    assert_eq!(
        sections(&export),
        [
            "s 6AB of the A New Tax System (Sample Levy) Act 2099",
            "s 8-1 of the A New Tax System (Sample Levy) Act 2099",
        ]
    );
    assert!(export.skipped.is_empty());
}

#[test]
fn a_name_the_gate_cannot_read_is_reported_and_left_out() {
    let export = exported(&[
        act("F2099L00001", "Sample Levy Rules 2099"),
        act("C2099A00002", "Sample Levy Act (No. 2) 2099"),
        act("C2099A00003", "Sample Levy Act 2099"),
    ]);
    assert_eq!(sections(&export), ["s 5 of the Sample Levy Act 2099"]);
    assert_eq!(
        export.skipped,
        [
            "title C2099A00002 (Sample Levy Act (No. 2) 2099): the gate does not read this name as an Act",
            "title F2099L00001 (Sample Levy Rules 2099): the gate does not read this name as an Act",
        ]
    );
}

#[test]
fn a_shared_name_and_an_old_compilation_are_reported_and_left_out() {
    let export = exported(&[
        act("C2099A00001", "Shared Name Act 2099"),
        act("C2099A00002", "Shared Name Act 2099"),
        Title {
            current: false,
            ..act("C2099A00003", "Replaced Levy Act 2099")
        },
    ]);
    assert!(export.sections.is_empty());
    assert_eq!(
        export.skipped,
        [
            "title C2099A00001 (Shared Name Act 2099): another title has this name",
            "title C2099A00002 (Shared Name Act 2099): another title has this name",
            "title C2099A00003 (Replaced Levy Act 2099): this build's compilation is not the current one",
        ]
    );
}

#[test]
fn a_row_directly_under_a_schedule_is_not_a_section() {
    let export = exported(&[Title {
        rows: vec![
            (Some("9"), Some("Part 1\u{2014}Levy")),
            (Some("1"), Some("Schedule 1\u{2014}Items")),
            (Some("2"), Some("Schedule 1\u{2014}Items")),
        ],
        ..act("C2099A00001", "Sample Levy Act 2099")
    }]);
    assert_eq!(sections(&export), ["s 9 of the Sample Levy Act 2099"]);
    assert_eq!(
        export.skipped,
        ["title C2099A00001 (Sample Levy Act 2099): 2 rows directly under a Schedule"]
    );
}

#[test]
fn a_label_the_gate_cannot_read_is_reported_and_left_out() {
    let export = exported(&[Title {
        rows: vec![(Some("9"), None), (Some("A1"), None)],
        ..act("C2099A00001", "Sample Levy Act 2099")
    }]);
    assert_eq!(sections(&export), ["s 9 of the Sample Levy Act 2099"]);
    assert_eq!(
        export.skipped,
        ["row C2099A00001:0002:A1: the gate does not read this label"]
    );
}

#[test]
fn ruling_ids_are_read_from_titles_in_the_gates_series() {
    let export = rulings(&file(&[
        ruling("TXR/1", "TR 2099/1 - Fabricated ruling", "UNKNOWN"),
        ruling("TXR/1", "TR 2099/1 - Fabricated ruling", "UNKNOWN"),
        ruling("COG/1", "LCR 2099/D1 - Fabricated draft", "UNKNOWN"),
        ruling("PSR/1", "PS  LA 2099/2 Fabricated statement", "UNKNOWN"),
        ruling("TXD/1", "TD 99/10", "UNKNOWN"),
        ruling("OLD/1", "TR 2099/1A2 - Fabricated addendum", "UNKNOWN"),
    ]));
    assert_eq!(
        listed(&export),
        [
            "LCR 2099/D1",
            "PS LA 2099/2",
            "TD 99/10",
            "TR 2099/1",
            "TR 2099/1A2"
        ]
    );
    assert!(export.skipped.is_empty());
}

#[test]
fn a_document_with_no_readable_id_is_reported_and_left_out() {
    let export = rulings(&file(&[
        ruling("EV/1", "Edited version of private advice", "UNKNOWN"),
        ruling("LIT/1", "Sample v Commissioner", "UNKNOWN"),
        ruling("TXR/2", "IT 2099 - Fabricated old series", "UNKNOWN"),
        ruling("TXR/3", "TR 2099/1: no space before the colon", "UNKNOWN"),
    ]));
    assert!(export.rulings.is_empty());
    assert_eq!(export.skipped.len(), 4);
    assert_eq!(
        export.skipped[0],
        "ruling EV/1: its title does not start with an id the gate reads"
    );
}

#[test]
fn a_withdrawal_in_any_capture_leaves_the_ruling_out() {
    let later = ruling("TXR/1", "TR 2099/1 - Fabricated", "NOTICE_PRESENT");
    let export = rulings(&[
        file(&[ruling("TXR/1", "TR 2099/1 - Fabricated", "UNKNOWN")]).remove(0),
        ("later.jsonl".to_string(), later),
    ]);
    assert!(export.rulings.is_empty());
    assert_eq!(
        export.skipped,
        ["ruling TXR/1: it carries a withdrawal notice"]
    );
}

#[test]
fn a_malformed_rulings_row_stops_the_export() {
    for (line, message) in [
        ("not json".to_string(), "rulings.jsonl:2 is not JSON"),
        (
            r#"{"docid": "TXR/2"}"#.to_string(),
            "rulings.jsonl:2 is not a rulings row",
        ),
        ("[1]".to_string(), "rulings.jsonl:2 is not a rulings row"),
    ] {
        let good = ruling("TXR/1", "TR 2099/1", "UNKNOWN");
        let mut export = Export::default();
        let error = add_rulings(&file(&[good, line]), &mut export).unwrap_err();
        assert_eq!(error.to_string(), message);
    }
}

#[test]
fn the_file_holds_one_key_with_rulings_then_sections_in_order() {
    let mut export = exported(&[Title {
        rows: vec![(Some("9"), None), (Some("10"), None)],
        ..act("C2099A00001", "Sample Levy Act 2099")
    }]);
    add_rulings(
        &file(&[ruling("TXR/1", "TR 2099/1 - F", "UNKNOWN")]),
        &mut export,
    )
    .unwrap();
    let parsed: Value = serde_json::from_str(&render(&export)).unwrap();
    assert_eq!(
        parsed,
        json!({"authorities": [
            "TR 2099/1",
            "s 10 of the Sample Levy Act 2099",
            "s 9 of the Sample Levy Act 2099",
        ]})
    );
}

#[test]
fn the_command_writes_a_new_file_and_prints_its_provenance() {
    let case = indexed(&[
        act("C2099A00001", "Sample Levy Act 2099"),
        act("F2099L00001", "Sample Levy Rules 2099"),
    ]);
    let rulings_path = case.dir.path().join("rulings.jsonl");
    std::fs::write(&rulings_path, ruling("TXR/1", "TR 2099/1 - F", "UNKNOWN")).unwrap();
    let out = case.dir.path().join("authorities.json");
    let (code, stdout, stderr) = fadden(&[
        &s(&case.index),
        "--out",
        &s(&out),
        "--rulings",
        &s(&rulings_path),
    ]);
    assert_eq!((code, stderr.as_str()), (0, ""));
    let parsed: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(
        parsed,
        json!({"authorities": ["TR 2099/1", "s 5 of the Sample Levy Act 2099"]})
    );
    assert!(stdout.contains("skipped title F2099L00001"), "{stdout}");
    assert!(
        stdout.contains("wrote 1 sections and 1 ruling ids"),
        "{stdout}"
    );
    assert!(
        stdout.contains("the corpus retrieved 2099-08-01"),
        "{stdout}"
    );
    assert!(stdout.contains("rulings.jsonl sha256 "), "{stdout}");
    assert!(stdout.contains("not thereby current"), "{stdout}");
}

#[test]
fn an_existing_destination_is_kept() {
    let case = indexed(&[act("C2099A00001", "Sample Levy Act 2099")]);
    let out = case.dir.path().join("authorities.json");
    std::fs::write(&out, "earlier").unwrap();
    let (code, _, stderr) = fadden(&[&s(&case.index), "--out", &s(&out)]);
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.starts_with("error: cannot write "), "{stderr}");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "earlier");
}

#[test]
fn nothing_to_export_writes_no_file() {
    let case = indexed(&[act("F2099L00001", "Sample Levy Rules 2099")]);
    let out = case.dir.path().join("authorities.json");
    let (code, _, stderr) = fadden(&[&s(&case.index), "--out", &s(&out)]);
    assert_eq!(
        (code, stderr.as_str()),
        (1, "error: nothing to export: no entry the gate can read\n")
    );
    assert!(!out.exists());
}

#[test]
fn an_index_or_rulings_file_it_cannot_use_stops_it() {
    let case = indexed(&[act("C2099A00001", "Sample Levy Act 2099")]);
    let out = case.dir.path().join("authorities.json");
    let missing = case.dir.path().join("missing");
    let (code, _, stderr) = fadden(&[&s(&missing), "--out", &s(&out)]);
    assert_eq!(code, 1, "{stderr}");
    let (code, _, stderr) = fadden(&[
        &s(&case.index),
        "--out",
        &s(&out),
        "--rulings",
        &s(&missing),
    ]);
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.starts_with("error: cannot read "), "{stderr}");
    assert!(!out.exists());
}
