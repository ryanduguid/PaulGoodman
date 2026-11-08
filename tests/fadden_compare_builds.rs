// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

//! `fadden compare_builds`: ported from tests/corpus/test_compare_builds.py.
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tax_radar_au::fadden::{Exc, compare_builds};
use tax_radar_au::pyjson::{Json, Layout, dumps, loads_standard};

const ACT: &str = "C2099A00001";
const INSTRUMENT: &str = "F2099L00002";

struct Row<'a> {
    rid: &'a str,
    ordinal: u32,
    section: Option<&'a str>,
    text: &'a str,
    heading: &'a str,
    container: Option<&'a str>,
    compilation: u32,
}

fn row(rid: &str, ordinal: u32, section: Option<&str>, text: &str) -> Value {
    make(Row {
        rid,
        ordinal,
        section,
        text,
        heading: "Heading",
        container: None,
        compilation: 1,
    })
}

fn make(r: Row) -> Value {
    json!({
        "register_id": r.rid, "act": "Sample Act 2099", "compilation_number": r.compilation.to_string(),
        "compilation_date": format!("2026-0{}-01", r.compilation),
        "row_id": format!("{}:{:04}:{}", r.rid, r.ordinal, r.section.unwrap_or("-")),
        "section": r.section, "heading": r.heading, "container": r.container, "text": r.text,
    })
}

fn write_build(root: &Path, titles: &[(&str, Vec<Value>)]) {
    for (rid, rows) in titles {
        let directory = root.join("markdown").join(rid);
        std::fs::create_dir_all(&directory).unwrap();
        let text: String = rows.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(directory.join("sections.jsonl"), text).unwrap();
    }
}

fn as_json(rows: &[Value]) -> Vec<Json> {
    rows.iter()
        .map(|r| loads_standard(&r.to_string()).unwrap())
        .collect()
}

fn report(old: &[Value], new: &[Value]) -> Value {
    let built = compare_builds::compare_title(&as_json(old), &as_json(new)).unwrap();
    serde_json::from_str(&dumps(&built, Layout::Canonical)).unwrap()
}

fn sections(places: &Value, field: &str) -> Vec<Value> {
    places
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p[field].clone())
        .collect()
}

struct Builds {
    _dir: tempfile::TempDir,
    old: PathBuf,
    new: PathBuf,
}

fn builds() -> Builds {
    let dir = tempfile::tempdir().unwrap();
    Builds {
        old: dir.path().join("old"),
        new: dir.path().join("new"),
        _dir: dir,
    }
}

fn run(b: &Builds, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fadden"))
        .arg("compare_builds")
        .arg(&b.old)
        .arg(&b.new)
        .args(args)
        .output()
        .unwrap();
    let text = |x: &[u8]| String::from_utf8_lossy(x).replace("\r\n", "\n");
    (
        out.status.code().unwrap(),
        text(&out.stdout),
        text(&out.stderr),
    )
}

#[test]
fn an_inserted_section_does_not_shift_its_neighbours_into_changes() {
    let b = builds();
    let new_row = |ordinal, section, text| {
        make(Row {
            rid: ACT,
            ordinal,
            section: Some(section),
            text,
            heading: "Heading",
            container: None,
            compilation: 2,
        })
    };
    write_build(
        &b.old,
        &[(
            ACT,
            vec![row(ACT, 1, Some("1"), "one"), row(ACT, 2, Some("2"), "two")],
        )],
    );
    write_build(
        &b.new,
        &[(
            ACT,
            vec![
                new_row(1, "1", "one"),
                new_row(2, "1A", "inserted"),
                new_row(3, "2", "two"),
            ],
        )],
    );
    let old = compare_builds::load_rows(&b.old.to_string_lossy(), ACT).unwrap();
    let new = compare_builds::load_rows(&b.new.to_string_lossy(), ACT).unwrap();
    let r: Value = serde_json::from_str(&dumps(
        &compare_builds::compare_title(&old, &new).unwrap(),
        Layout::Canonical,
    ))
    .unwrap();
    assert_eq!(sections(&r["added"], "section"), [json!("1A")]);
    assert_eq!(r["removed"], json!([]));
    assert_eq!(r["changed"], json!([]));
    assert_eq!(
        r["old_compilation"],
        json!({"number": "1", "date": "2026-01-01"})
    );
    assert_eq!(
        r["new_compilation"],
        json!({"number": "2", "date": "2026-02-01"})
    );
}

#[test]
fn text_and_heading_changes_are_named() {
    let renamed = |ordinal, section, text| {
        make(Row {
            rid: ACT,
            ordinal,
            section: Some(section),
            text,
            heading: "Renamed",
            container: None,
            compilation: 1,
        })
    };
    let r = report(
        &[
            row(ACT, 1, Some("1"), "one"),
            row(ACT, 2, Some("2"), "two"),
            row(ACT, 3, Some("3"), "three"),
            row(ACT, 4, Some("4"), "gone"),
        ],
        &[
            row(ACT, 1, Some("1"), "one amended"),
            renamed(2, "2", "two"),
            renamed(3, "3", "three amended"),
        ],
    );
    let changes: Vec<(Value, Value)> = r["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["section"].clone(), p["change"].clone()))
        .collect();
    assert_eq!(
        changes,
        [
            (json!("1"), json!("text")),
            (json!("2"), json!("heading")),
            (json!("3"), json!("text and heading"))
        ]
    );
    assert_eq!(sections(&r["removed"], "section"), [json!("4")]);
}

#[test]
fn schedules_restart_numbering_and_repeated_labels_match_in_order() {
    let schedule = |ordinal, text| {
        make(Row {
            rid: ACT,
            ordinal,
            section: Some("1"),
            text,
            heading: "Heading",
            container: Some("Schedule 1"),
            compilation: 1,
        })
    };
    let r = report(
        &[
            row(ACT, 1, Some("1"), "body one"),
            schedule(2, "schedule one"),
            schedule(3, "schedule one again"),
        ],
        &[
            row(ACT, 1, Some("1"), "body one"),
            schedule(2, "schedule one"),
            schedule(3, "schedule one again, amended"),
        ],
    );
    assert_eq!(r["changed"].as_array().unwrap().len(), 1);
    assert_eq!(r["changed"][0]["container"], "Schedule 1");
    assert_eq!(
        (r["added"].clone(), r["removed"].clone()),
        (json!([]), json!([]))
    );
}

#[test]
fn rows_without_a_section_label_match_on_their_heading() {
    let headed = |ordinal, text, heading| {
        make(Row {
            rid: ACT,
            ordinal,
            section: None,
            text,
            heading,
            container: None,
            compilation: 1,
        })
    };
    let r = report(
        &[headed(1, "intro", "Introductory material")],
        &[
            headed(1, "intro amended", "Introductory material"),
            headed(2, "table", "Table of rates"),
        ],
    );
    assert_eq!(
        sections(&r["changed"], "heading"),
        [json!("Introductory material")]
    );
    assert_eq!(sections(&r["added"], "heading"), [json!("Table of rates")]);
}

#[test]
fn text_report_lists_changes_and_titles_in_one_build() {
    let b = builds();
    write_build(
        &b.old,
        &[
            (ACT, vec![row(ACT, 1, Some("1"), "one")]),
            (INSTRUMENT, vec![row(INSTRUMENT, 1, Some("1"), "same")]),
        ],
    );
    let amended = make(Row {
        rid: ACT,
        ordinal: 1,
        section: Some("1"),
        text: "one amended",
        heading: "Heading",
        container: None,
        compilation: 2,
    });
    write_build(
        &b.new,
        &[
            (ACT, vec![amended]),
            ("C2099A00003", vec![row("C2099A00003", 1, Some("1"), "new")]),
        ],
    );
    let (code, out, err) = run(&b, &[]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(out.contains("C2099A00001: compilation 1 (2026-01-01) to 2 (2026-02-01)"));
    assert!(out.contains("  changed (text): s 1"));
    assert!(out.contains(&format!("{INSTRUMENT}: only in the old build\n")));
    assert!(out.contains("C2099A00003: only in the new build\n"));
    assert!(out.contains("1 titles compared, 1 with section changes; 1 only in the old build, 1 only in the new build."));
    assert!(out.contains("read both compilations on the Register"));
}

#[test]
fn json_and_title_filter() {
    let b = builds();
    write_build(
        &b.old,
        &[
            (ACT, vec![row(ACT, 1, Some("1"), "one")]),
            (INSTRUMENT, vec![row(INSTRUMENT, 1, Some("1"), "same")]),
        ],
    );
    let reheaded = make(Row {
        rid: ACT,
        ordinal: 1,
        section: Some("1"),
        text: "one",
        heading: "New heading",
        container: None,
        compilation: 1,
    });
    write_build(
        &b.new,
        &[
            (ACT, vec![reheaded]),
            (INSTRUMENT, vec![row(INSTRUMENT, 1, Some("1"), "same")]),
        ],
    );
    let (code, out, _) = run(&b, &["--title", ACT, "--json"]);
    assert_eq!(code, 0);
    let lines: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        lines
            .iter()
            .map(|l| l["register_id"].clone())
            .collect::<Vec<_>>(),
        [json!(ACT)]
    );
    assert_eq!(lines[0]["changed"][0]["change"], "heading");
}

#[test]
fn json_lists_titles_present_in_only_one_build() {
    let b = builds();
    write_build(&b.old, &[(ACT, vec![row(ACT, 1, Some("1"), "one")])]);
    write_build(
        &b.new,
        &[(INSTRUMENT, vec![row(INSTRUMENT, 1, Some("1"), "same")])],
    );
    let (code, out, _) = run(&b, &["--json"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "{\"register_id\": \"C2099A00001\", \"only_in\": \"old\"}\n{\"register_id\": \"F2099L00002\", \"only_in\": \"new\"}\n"
    );
}

#[test]
fn unusable_input_returns_one_with_the_reason() {
    let b = builds();
    write_build(&b.old, &[(ACT, vec![row(ACT, 1, Some("1"), "one")])]);
    write_build(&b.new, &[(ACT, vec![row(ACT, 1, Some("1"), "one")])]);
    for (message, args) in [
        ("not in both builds", ["--title", INSTRUMENT]),
        (
            "invalid Federal Register identifier",
            ["--title", "../escape"],
        ),
    ] {
        let (code, out, err) = run(&b, &args);
        assert_eq!((code, out.as_str()), (1, ""));
        assert!(err.contains(message), "{err}");
    }
    std::fs::write(
        b.new.join("markdown").join(ACT).join("sections.jsonl"),
        "[\"not a row\"]\n",
    )
    .unwrap();
    let (code, _, err) = run(&b, &[]);
    assert_eq!(code, 1);
    assert!(err.contains("is not a sections.jsonl row"));
}

#[test]
fn directories_that_are_not_titles_are_ignored() {
    let b = builds();
    write_build(&b.new, &[(ACT, vec![row(ACT, 1, Some("1"), "one")])]);
    std::fs::create_dir(b.new.join("markdown").join("notes")).unwrap();
    std::fs::create_dir(b.new.join("markdown").join(INSTRUMENT)).unwrap();
    let found = compare_builds::titles(&b.new.to_string_lossy()).unwrap();
    assert_eq!(found.into_iter().collect::<Vec<_>>(), [ACT]);
}

#[test]
fn blank_lines_between_rows_are_skipped() {
    let b = builds();
    let directory = b.old.join("markdown").join(ACT);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("sections.jsonl"),
        format!("\n{}\n  \n", row(ACT, 1, Some("1"), "one")),
    )
    .unwrap();
    let rows = compare_builds::load_rows(&b.old.to_string_lossy(), ACT).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("text").and_then(Json::as_str), Some("one"));
}

#[test]
fn an_empty_title_has_no_compilation() {
    let r = report(&[], &[row(ACT, 1, Some("1"), "one")]);
    assert_eq!(r["old_compilation"], Value::Null);
    assert_eq!(sections(&r["added"], "section"), [json!("1")]);
}

#[test]
fn text_report_lists_added_and_removed_sections_and_skips_unchanged_titles() {
    let b = builds();
    write_build(
        &b.old,
        &[
            (
                ACT,
                vec![
                    row(ACT, 1, Some("1"), "one"),
                    row(ACT, 2, Some("2"), "gone"),
                ],
            ),
            (INSTRUMENT, vec![row(INSTRUMENT, 1, Some("1"), "same")]),
        ],
    );
    let table = make(Row {
        rid: ACT,
        ordinal: 2,
        section: None,
        text: "new",
        heading: "Table",
        container: Some("Part 2"),
        compilation: 1,
    });
    write_build(
        &b.new,
        &[
            (ACT, vec![row(ACT, 1, Some("1"), "one"), table]),
            (INSTRUMENT, vec![row(INSTRUMENT, 1, Some("1"), "same")]),
        ],
    );
    let (code, out, err) = run(&b, &[]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(out.contains("  added: Part 2, Table"));
    assert!(out.contains("  removed: s 2"));
    assert!(!out.contains(&format!("{INSTRUMENT}:")));
    assert!(out.contains("2 titles compared, 1 with section changes;"));
}

#[test]
fn python_value_semantics_decide_matches_and_failures() {
    // A container of 1 and of true are one dict key in Python; a list is unhashable.
    let mut a = row(ACT, 1, Some("1"), "one");
    a["container"] = json!(1);
    let mut b_row = row(ACT, 1, Some("1"), "one");
    b_row["container"] = json!(true);
    let r = report(&[a.clone()], &[b_row]);
    assert_eq!(
        (r["added"].clone(), r["removed"].clone()),
        (json!([]), json!([]))
    );
    a["container"] = json!(["x"]);
    let error = compare_builds::compare_title(&as_json(&[a]), &[]).unwrap_err();
    assert_eq!(
        error.to_string(),
        "cannot use 'tuple' as a dict key (unhashable type: 'list')"
    );
}

fn sections_path(root: &Path, rid: &str) -> PathBuf {
    root.join("markdown").join(rid).join("sections.jsonl")
}

fn lines(rows: &[Value]) -> String {
    rows.iter().map(|r| format!("{r}\n")).collect()
}

fn same_builds(rows: &[Value]) -> Builds {
    let b = builds();
    write_build(&b.old, &[(ACT, rows.to_vec())]);
    write_build(&b.new, &[(ACT, rows.to_vec())]);
    b
}

/// Write `source` as the new build's rows for ACT and expect both outputs to
/// refuse it with `message`, printing nothing and leaving the file as written.
fn refused(b: &Builds, source: &str, message: &str) {
    let path = sections_path(&b.new, ACT);
    std::fs::write(&path, source).unwrap();
    for args in [&[][..], &["--json"][..]] {
        let (code, out, err) = run(b, args);
        assert_eq!((code, out.as_str()), (1, ""), "{source}");
        assert!(err.contains(message), "{source}: {err}");
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
}

#[test]
fn section_metadata_that_is_not_text_is_refused() {
    let first = row(ACT, 1, Some("1"), "one");
    let b = same_builds(std::slice::from_ref(&first));
    for name in ["container", "section", "heading"] {
        for value in [
            json!(false),
            json!(0),
            json!(1),
            json!([]),
            json!(["label"]),
            json!({}),
            json!({"label": "one"}),
        ] {
            let mut second = row(ACT, 2, Some("2"), "two");
            second[name] = value;
            refused(
                &b,
                &lines(&[first.clone(), second]),
                "sections.jsonl:2 is not a sections.jsonl row",
            );
        }
    }
}

#[test]
fn section_metadata_may_be_text_null_or_missing() {
    let base = row(ACT, 1, Some("1"), "one");
    let mut rows = Vec::new();
    for value in [json!("label"), json!(""), Value::Null] {
        let mut r = base.clone();
        for name in ["container", "section", "heading"] {
            r[name] = value.clone();
        }
        rows.push(r);
    }
    let mut bare = base;
    for name in ["container", "section", "heading"] {
        bare.as_object_mut().unwrap().remove(name);
    }
    rows.push(bare);
    let b = same_builds(&rows);
    let (code, out, err) = run(&b, &["--json"]);
    assert_eq!((code, err.as_str()), (0, ""));
    let r: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        (&r["added"], &r["removed"], &r["changed"]),
        (&json!([]), &json!([]), &json!([]))
    );
}

#[test]
fn a_row_naming_another_title_is_refused() {
    let original = [row(ACT, 1, Some("1"), "one"), row(ACT, 2, Some("2"), "two")];
    let b = same_builds(&original);
    for position in 0..2 {
        for identity in [json!(INSTRUMENT), Value::Null, json!(0), json!({})] {
            let mut rows = original.to_vec();
            rows[position]["register_id"] = identity;
            refused(
                &b,
                &lines(&rows),
                &format!(
                    "sections.jsonl:{} is not a sections.jsonl row",
                    position + 1
                ),
            );
        }
    }
}

#[test]
fn rows_from_two_compilations_are_refused() {
    let original = [row(ACT, 1, Some("1"), "one"), row(ACT, 2, Some("2"), "two")];
    let b = same_builds(&original);
    for (name, different) in [
        ("compilation_number", "2"),
        ("compilation_date", "2026-02-01"),
    ] {
        for change in [Some(json!(different)), Some(Value::Null), None] {
            let mut second = original[1].clone();
            match change {
                Some(value) => second[name] = value,
                None => {
                    second.as_object_mut().unwrap().remove(name);
                }
            }
            refused(
                &b,
                &lines(&[original[0].clone(), second]),
                "sections.jsonl:2 has inconsistent compilation metadata",
            );
        }
    }
}

#[test]
fn uniform_legacy_and_unnumbered_compilations_are_accepted() {
    for kind in ["null", "missing", "integer"] {
        let mut rows = vec![row(ACT, 1, Some("1"), "one"), row(ACT, 2, Some("2"), "two")];
        for r in &mut rows {
            match kind {
                "missing" => {
                    for name in ["register_id", "compilation_number", "compilation_date"] {
                        r.as_object_mut().unwrap().remove(name);
                    }
                }
                "null" => {
                    r["compilation_number"] = Value::Null;
                    r["compilation_date"] = Value::Null;
                }
                _ => r["compilation_number"] = json!(1),
            }
        }
        let b = same_builds(&rows);
        let (code, out, err) = run(&b, &["--json"]);
        assert_eq!((code, err.as_str()), (0, ""), "{kind}");
        let r: Value = serde_json::from_str(&out).unwrap();
        let expected = json!({
            "number": rows[0].get("compilation_number"),
            "date": rows[0].get("compilation_date"),
        });
        assert_eq!(
            (&r["old_compilation"], &r["new_compilation"]),
            (&expected, &expected)
        );
        assert_eq!(r["changed"], json!([]));
    }
}

#[test]
fn repeated_json_members_are_refused_at_every_depth() {
    let original = row(ACT, 1, Some("1"), "one");
    let b = same_builds(std::slice::from_ref(&original));
    let tail = original.to_string()[1..].to_string();
    let mut cases: Vec<String> = [
        ("text", json!("amended")),
        ("register_id", json!(INSTRUMENT)),
        ("container", json!([])),
        ("section", json!(0)),
        ("heading", json!({})),
        ("compilation_number", json!("2")),
        ("compilation_date", json!("2026-02-01")),
    ]
    .iter()
    .map(|(name, value)| format!("{{\"{name}\":{value},{tail}"))
    .collect();
    cases.extend([
        format!("{{\"text\":\"one\",{tail}"),
        format!("{{\"te\\u0078t\":\"amended\",{tail}"),
        format!("{{\"extra\":1,\"extra\":2,{tail}"),
        format!("{{\"extra\":{{\"x\":1,\"x\":2}},{tail}"),
        format!("{{\"extra\":[{{\"x\":1,\"x\":2}}],{tail}"),
    ]);
    let mut unnumbered = original.clone();
    unnumbered
        .as_object_mut()
        .unwrap()
        .remove("compilation_number");
    cases.push(format!(
        "{{\"compilation_number\":{{\"n\":\"2\",\"n\":\"1\"}},{}",
        &unnumbered.to_string()[1..]
    ));
    let path = sections_path(&b.new, ACT);
    for source in cases {
        std::fs::write(&path, format!("{source}\n")).unwrap();
        let error = compare_builds::load_rows(&b.new.to_string_lossy(), ACT).unwrap_err();
        assert!(
            error
                .to_string()
                .ends_with("sections.jsonl:1 contains duplicate JSON members"),
            "{source}: {error}"
        );
    }
    refused(
        &b,
        &format!("{original}\n \t\n{{\"text\":\"hidden\",{tail}\n"),
        "sections.jsonl:3 contains duplicate JSON members",
    );
}

#[test]
fn a_json_syntax_error_stays_a_decode_error() {
    let b = same_builds(&[row(ACT, 1, Some("1"), "one")]);
    std::fs::write(sections_path(&b.new, ACT), "{\"text\":").unwrap();
    let error = compare_builds::load_rows(&b.new.to_string_lossy(), ACT).unwrap_err();
    assert!(
        matches!(
            error,
            Exc::Value {
                class: "json.decoder.JSONDecodeError",
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn a_duplicate_in_a_later_title_stops_all_output() {
    let b = builds();
    let rows = [
        row(INSTRUMENT, 1, Some("1"), "one"),
        row(INSTRUMENT, 2, Some("2"), "two"),
    ];
    write_build(
        &b.old,
        &[
            (ACT, vec![row(ACT, 1, Some("1"), "original")]),
            (INSTRUMENT, rows.to_vec()),
        ],
    );
    write_build(
        &b.new,
        &[
            (ACT, vec![row(ACT, 1, Some("1"), "amended")]),
            (INSTRUMENT, rows.to_vec()),
        ],
    );
    let paths: Vec<PathBuf> = [&b.old, &b.new]
        .into_iter()
        .flat_map(|root| [ACT, INSTRUMENT].map(|rid| sections_path(root, rid)))
        .collect();
    for failing in [&b.old, &b.new] {
        for root in [&b.old, &b.new] {
            std::fs::write(sections_path(root, INSTRUMENT), lines(&rows)).unwrap();
        }
        std::fs::write(
            sections_path(failing, INSTRUMENT),
            format!(
                "{}\n{{\"text\":\"hidden\",{}\n",
                rows[0],
                &rows[1].to_string()[1..]
            ),
        )
        .unwrap();
        let before: Vec<Vec<u8>> = paths.iter().map(|p| std::fs::read(p).unwrap()).collect();
        for args in [&[][..], &["--json"][..]] {
            let (code, out, err) = run(&b, args);
            assert_eq!((code, out.as_str()), (1, ""));
            assert!(
                err.contains("sections.jsonl:2 contains duplicate JSON members"),
                "{err}"
            );
        }
        let after: Vec<Vec<u8>> = paths.iter().map(|p| std::fs::read(p).unwrap()).collect();
        assert_eq!(after, before);
    }
}

#[test]
fn a_missing_compilation_equals_a_null_one() {
    let mut first = row(ACT, 1, Some("1"), "one");
    first.as_object_mut().unwrap().remove("compilation_number");
    let mut second = row(ACT, 2, Some("2"), "two");
    second["compilation_number"] = Value::Null;
    let b = same_builds(&[first, second]);
    let (code, out, err) = run(&b, &["--json"]);
    assert_eq!((code, err.as_str()), (0, ""));
    let r: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(r["new_compilation"]["number"], Value::Null);
}

#[test]
fn distinct_and_escaped_member_names_are_not_duplicates() {
    let mut original = row(ACT, 1, Some("1"), "one");
    original["extra"] = json!({
        "objects": [{"x": 1}, {"x": 2}], "x": 1, "X": 2,
        "\u{e9}": 3, "e\u{301}": 4, "literal": "{\"x\":1,\"x\":2}",
    });
    let b = same_builds(std::slice::from_ref(&original));
    let escaped = original
        .to_string()
        .replacen("\"text\":", "\"te\\u0078t\":", 1);
    assert!(escaped.contains("te\\u0078t"));
    std::fs::write(sections_path(&b.new, ACT), format!("{escaped}\n")).unwrap();
    let rows = compare_builds::load_rows(&b.new.to_string_lossy(), ACT).unwrap();
    assert_eq!(rows, as_json(&[original]));
    let (code, out, err) = run(&b, &["--json"]);
    assert_eq!((code, err.as_str()), (0, ""));
    let r: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(r["changed"], json!([]));
}

#[test]
fn titles_not_compared_are_not_read() {
    let b = builds();
    let common = row(ACT, 1, Some("1"), "one");
    let new_only = "C2099A00003";
    write_build(
        &b.old,
        &[
            (ACT, vec![common.clone()]),
            (INSTRUMENT, vec![row(INSTRUMENT, 1, Some("1"), "old")]),
        ],
    );
    write_build(
        &b.new,
        &[
            (ACT, vec![common]),
            (new_only, vec![row(new_only, 1, Some("1"), "new")]),
        ],
    );
    for (root, rid) in [(&b.old, INSTRUMENT), (&b.new, new_only)] {
        let path = sections_path(root, rid);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{{\"text\":\"hidden\",{}", &text[1..])).unwrap();
    }
    let (code, out, err) = run(&b, &[]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(out.contains(&format!("{INSTRUMENT}: only in the old build\n")));
    assert!(out.contains(&format!("{new_only}: only in the new build\n")));
    let (code, out, err) = run(&b, &["--title", ACT, "--json"]);
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(out.lines().count(), 1);
}

/// One sections.jsonl line with raw JSON for the fields a case varies.
fn raw_row(
    ordinal: u32,
    text: &str,
    number: &str,
    container: &str,
    section: &str,
    heading: &str,
) -> String {
    format!(
        r#"{{"register_id": "{ACT}", "act": "Sample Act 2099", "compilation_number": {number}, "compilation_date": "2026-01-01", "row_id": "{ACT}:{ordinal:04}:-", "section": {section}, "heading": {heading}, "container": {container}, "text": "{text}"}}"#
    )
}

/// Write the builds, run text and --json, and compare exit, stdout and stderr
/// with what the stricter Python printed (stderr as the traceback's last line,
/// paths under the case directory as `ROOT/...`).
fn same_as_python(
    old: &[String],
    new: &[String],
    text: (i32, &str, &str),
    json: (i32, &str, &str),
) {
    let b = builds();
    for (root, rows) in [(&b.old, old), (&b.new, new)] {
        let directory = root.join("markdown").join(ACT);
        std::fs::create_dir_all(&directory).unwrap();
        let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(directory.join("sections.jsonl"), body).unwrap();
    }
    let parent = b.old.parent().unwrap();
    let canonical = std::fs::canonicalize(parent).unwrap();
    let canonical = canonical.to_string_lossy();
    let mut forms = vec![
        parent.to_string_lossy().into_owned(),
        canonical
            .strip_prefix(r"\\?\")
            .unwrap_or(&canonical)
            .to_string(),
    ];
    forms.sort_by_key(|f| std::cmp::Reverse(f.len()));
    for (args, want) in [(&[][..], text), (&["--json"][..], json)] {
        let (code, out, mut err) = run(&b, args);
        if forms.iter().any(|f| err.contains(f.as_str())) {
            for form in &forms {
                err = err.replace(form.as_str(), "ROOT");
            }
            err = err.replace('\\', "/");
        }
        assert_eq!((code, out.as_str(), err.as_str()), want, "{args:?}");
    }
}

const SUMMARY: &str = "1 titles compared, 1 with section changes; 0 only in the old build, 0 only in the new build.\nThis compares the derived reading view; read both compilations on the Register before relying on a change.\n";

#[test]
fn a_nan_compilation_number_differs_from_itself_as_in_python() {
    let rows = [
        raw_row(1, "one", "NaN", "null", r#""1""#, r#""Heading""#),
        raw_row(2, "two", "NaN", "null", r#""1""#, r#""Heading""#),
    ];
    let refused = "error: ROOT/old/markdown/C2099A00001/sections.jsonl:2 has inconsistent compilation metadata\n";
    same_as_python(&rows, &rows, (1, "", refused), (1, "", refused));
}

#[test]
fn compilation_numbers_python_calls_equal_are_one_compilation() {
    for (first, second, shown, dumped) in [("true", "1", "True", "true"), ("1", "1.0", "1", "1")] {
        let build = |text| {
            [
                raw_row(1, text, first, "null", r#""1""#, r#""Heading""#),
                raw_row(2, "two", second, "null", r#""1""#, r#""Heading""#),
            ]
        };
        let text = format!(
            "C2099A00001: compilation {shown} (2026-01-01) to {shown} (2026-01-01)\n  changed (text): s 1\n{SUMMARY}"
        );
        let json = format!(
            r#"{{"register_id": "C2099A00001", "old_compilation": {{"number": {dumped}, "date": "2026-01-01"}}, "new_compilation": {{"number": {dumped}, "date": "2026-01-01"}}, "added": [], "removed": [], "changed": [{{"container": null, "section": "1", "heading": "Heading", "change": "text"}}]}}"#
        ) + "\n";
        same_as_python(
            &build("one"),
            &build("uno"),
            (0, text.as_str(), ""),
            (0, json.as_str(), ""),
        );
    }
}

#[test]
fn a_lone_surrogate_in_section_metadata_fails_where_python_prints_it() {
    // The stricter Python admits a lone surrogate as text and then fails to
    // print it; the binary keeps that behaviour rather than refusing the row.
    let heading = r#""Heading""#;
    for (container, section, heading, point, text_at, json_at) in [
        (r#""\ud800""#, r#""1""#, heading, "ud800", 18, 202),
        ("null", r#""\udc00""#, heading, "udc00", 20, 219),
        ("null", "null", r#""\ud83d""#, "ud83d", 18, 236),
    ] {
        let build = |text| [raw_row(1, text, r#""1""#, container, section, heading)];
        let error = |at| {
            format!(
                "UnicodeEncodeError: 'utf-8' codec can't encode character '\\{point}' in position {at}: surrogates not allowed\n"
            )
        };
        let (text_error, json_error) = (error(text_at), error(json_at));
        same_as_python(
            &build("one"),
            &build("uno"),
            (
                1,
                "C2099A00001: compilation 1 (2026-01-01) to 1 (2026-01-01)\n",
                text_error.as_str(),
            ),
            (1, "", json_error.as_str()),
        );
    }
}
