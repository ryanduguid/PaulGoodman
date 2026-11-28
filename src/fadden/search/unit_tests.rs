// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! The conversions `build` applies to corpus values, against what CPython
//! raises for the same values, and the manifest's document order.
use super::*;

fn raised(error: Exc) -> String {
    format!("{}: {error}", error.class())
}

fn lone() -> Json {
    pyjson::loads_standard(r#""\ud800""#).unwrap()
}

#[test]
fn version_flags_convert_as_python_int_converts_them() {
    for (value, expected) in [
        (Json::Bool(true), 1),
        (Json::Int(4.into()), 4),
        (Json::Float(2.7), 2),
        (Json::Float(-2.7), -2),
        (Json::str(" 12 "), 12),
    ] {
        assert_eq!(py_int(&value).ok(), Some(Json::Int(expected.into())));
    }
    let not_real =
        "TypeError: int() argument must be a string, a bytes-like object or a real number, not";
    for (value, message) in [
        (
            Json::str("yes"),
            "ValueError: invalid literal for int() with base 10: 'yes'".to_string(),
        ),
        (
            lone(),
            r"ValueError: invalid literal for int() with base 10: '\ud800'".to_string(),
        ),
        (
            Json::Float(f64::NAN),
            "ValueError: cannot convert float NaN to integer".to_string(),
        ),
        (
            Json::Float(f64::INFINITY),
            "OverflowError: cannot convert float infinity to integer".to_string(),
        ),
        (Json::Null, format!("{not_real} 'NoneType'")),
        (Json::Array(vec![]), format!("{not_real} 'list'")),
    ] {
        assert_eq!(py_int(&value).err().map(raised), Some(message));
    }
}

#[test]
fn parameters_bind_as_sqlite3_binds_them() {
    for (value, expected) in [
        (Json::Null, Sql::Null),
        (Json::Bool(false), Sql::Integer(0)),
        (Json::Int(5.into()), Sql::Integer(5)),
        (Json::Float(1.5), Sql::Real(1.5)),
        (Json::str("text"), Sql::Text("text".into())),
    ] {
        assert_eq!(bind(&value, 1).ok(), Some(expected));
    }
    for (value, message) in [
        (
            Json::Int(num_bigint::BigInt::from(1u128 << 70)),
            "OverflowError: Python int too large to convert to SQLite INTEGER",
        ),
        (
            lone(),
            r"UnicodeEncodeError: 'utf-8' codec can't encode character '\ud800' in position 0: surrogates not allowed",
        ),
        (
            Json::Array(vec![]),
            "sqlite3.Error: Error binding parameter 3: type 'list' is not supported",
        ),
    ] {
        assert_eq!(bind(&value, 3).err().map(raised), Some(message.into()));
    }
}

#[test]
fn headings_are_normalised_only_when_they_are_str() {
    assert_eq!(
        normalise_value(&Json::str("\u{fb01}le")).ok(),
        Some("file".into())
    );
    // A lone surrogate passes NFKC untouched and fails later, when bound.
    assert!(normalise_value(&lone()).is_ok());
    assert_eq!(
        normalise_value(&Json::Int(3.into())).err().map(raised),
        Some("TypeError: normalize() argument 2 must be str, not int".into())
    );
}

#[test]
fn sqlite_errors_carry_the_library_message() {
    let connection = Connection::open_in_memory().unwrap();
    // A statement that fails to prepare, as an index of another shape makes
    // one: Python shows sqlite's message alone.
    let error = connection.execute("SELEC 1", []).unwrap_err();
    assert_eq!(
        raised(sqlite_error(&error)),
        "sqlite3.Error: near \"SELEC\": syntax error"
    );
    let error = connection
        .query_row("SELECT value FROM meta", [], |r| r.get::<_, String>(0))
        .unwrap_err();
    assert_eq!(
        raised(sqlite_error(&error)),
        "sqlite3.Error: no such table: meta"
    );
    let code = rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY);
    let error = rusqlite::Error::SqliteFailure(code, None);
    assert_eq!(
        raised(sqlite_error(&error)),
        format!("sqlite3.Error: {code}")
    );
    assert_eq!(
        raised(sqlite_error(&rusqlite::Error::QueryReturnedNoRows)),
        "sqlite3.Error: Query returned no rows"
    );
}

fn row(docid: &str) -> RulingRow {
    RulingRow {
        docid: docid.into(),
        title: "A ruling".into(),
        paragraph: "1".into(),
        text: "Text.".into(),
        source_url: "https://example.test/ruling".into(),
        source_sha256: "0".repeat(64),
        fetched_on: "2026-11-01".into(),
        withdrawal_status: "current".into(),
    }
}

fn document(docid: &str, paragraphs: &str) -> Json {
    pyjson::loads_standard(&format!(
        r#"{{"docid": "{docid}", "paragraphs": {paragraphs}, "title": "A ruling",
             "source_url": "https://example.test/ruling", "sha256": "{}",
             "fetched_on": "2026-11-01", "withdrawal_status": "current"}}"#,
        "0".repeat(64)
    ))
    .unwrap()
}

#[test]
fn the_manifest_must_list_each_held_document_in_order() {
    let difference =
        |rows: &[RulingRow], documents: &[Json]| manifest_difference(rows, documents, "2026-11-01");
    assert_eq!(
        difference(&[row("TR 2026/1")], &[document("TR 2026/1", "1")]),
        None
    );
    let unlisted = "manifest.json lists a document without a docid and paragraph count";
    for broken in [
        document("TR 2026/1", "\"1\""),
        Json::obj([("paragraphs", Json::Int(1.into()))]),
    ] {
        assert_eq!(
            difference(&[row("TR 2026/1")], &[broken]),
            Some(unlisted.into())
        );
    }
    assert_eq!(
        difference(&[], &[document("TR 2026/1", "1")]),
        Some("manifest.json lists TR 2026/1, which rulings.jsonl does not hold".into())
    );
    assert_eq!(
        difference(
            &[row("TR 2026/1"), row("TR 2026/2")],
            &[document("TR 2026/1", "1")]
        ),
        Some("rulings.jsonl holds TR 2026/2 after the last document manifest.json lists".into())
    );
}

#[test]
fn search_helpers_cover_edge_branches() {
    assert!(!ends_name("section 1 and !bad", 9, |_| None));
    let (places, err) = structure(&[Json::obj([("heading", Json::Int(1.into()))])], "# H\n");
    assert!(places.is_empty() && err.is_some());
    let lone = Json::obj([("heading", Json::Str(PyStr::Lone(vec![0xD800])))]);
    assert_eq!(structure(&[lone], "# H\n").0, vec![(None, None)]);
    let big = format!("{{\"text\":\"x\",\"n\":{}}}", "1".repeat(4301));
    assert!(matches!(load_row(&big, "p"), Err(Exc::Value { .. })));
    assert!(matches!(
        load_row(&"[".repeat(2 * pyjson::MAX_DEPTH), "p"),
        Err(Exc::Recursion(_))
    ));
    assert!(check_row_values("p", LICENCE_BASES[0], "BAD", "2026-01-01").is_err());
    assert_eq!(to_json(Sql::Real(1.5)), Json::Float(1.5));
    assert_eq!(to_json(Sql::Blob(b"hi".to_vec())), Json::str("b'hi'"));
    assert_eq!(sql_value(&Json::Int(7.into())), Sql::Integer(7));
    assert_eq!(json_usize(&Json::Null), 0);
    let mut null_v = Json::Null;
    drop_source_fields(&mut null_v);
    assert_eq!(entries_mut(&mut null_v, "x").count(), 0);
    let mut obj_v = Json::obj([("x", Json::Null)]);
    assert_eq!(entries_mut(&mut obj_v, "x").count(), 0);
    let dated = Dated {
        date: "2026-01-01".into(),
        titles: vec![],
    };
    assert_eq!(dated.standing("MISSING"), Json::Null);
    let empty_ns = crate::argparse::Namespace::default();
    assert!(list(&empty_ns, "missing").is_empty());
    let mut caps = Caps::of(&empty_ns).unwrap();
    assert_eq!(caps.report(Json::Null).0, Json::Null);
    assert!(quotations_of(&Json::Null).is_empty());
    assert!(describe_reference(&Json::obj([])).contains("not in this index"));
}

#[test]
fn search_printers_cover_edge_branches() {
    let mut r = Run::default();
    print_quotation(
        &Json::obj([
            ("grade", Json::str("exact")),
            ("distinct", Json::Bool(true)),
            ("other_rows", Json::Int(0.into())),
            ("quote", Json::str("a b")),
        ]),
        &mut r,
    );
    print_quote_summary(
        "R1",
        &Json::obj([
            ("version_is_current", Json::Bool(false)),
            ("grade", Json::str("miss")),
            ("quote", Json::str("q")),
        ]),
        &mut r,
    );
    print_terms(
        &Json::obj([(
            "terms",
            Json::Array(vec![Json::obj([("definitions", Json::Null)])]),
        )]),
        &mut r,
    );
    print_refs_lines(
        &Json::obj([
            ("row_id", Json::str("R1")),
            ("heading", Json::str("H")),
            ("cites", Json::Null),
            ("cited_by", Json::Null),
        ]),
        &mut r,
    );
    let out = String::from_utf8_lossy(&r.stdout);
    assert!(out.contains("R1") && out.contains("exact"));
}

#[test]
fn search_read_corpus_and_build_cover_error_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("corpus");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("sources.json"), "[".repeat(2 * pyjson::MAX_DEPTH)).unwrap();
    assert!(matches!(
        read_corpus(root.to_str().unwrap()),
        Err(Exc::Recursion(_))
    ));
    let argv = vec![
        "build".into(),
        root.to_str().unwrap().into(),
        "--out".into(),
        dir.path().join("i.db").to_str().unwrap().into(),
    ];
    assert_eq!(main(&argv, &mut Run::default()).unwrap(), 1);
    std::fs::write(root.join("sources.json"), "{bad").unwrap();
    assert!(read_corpus(root.to_str().unwrap()).is_err());
    std::fs::write(root.join("sources.json"), r#"{"titles":[]}"#).unwrap();
    let bad_md = root.join("markdown");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir(&bad_md).unwrap();
        std::fs::set_permissions(&bad_md, std::fs::Permissions::from_mode(0o000)).unwrap();
        let err = read_corpus(root.to_str().unwrap());
        std::fs::set_permissions(&bad_md, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(err, Err(Exc::Os(_))));
    }
    std::fs::create_dir_all(bad_md.join("C2099A00001")).unwrap();
    std::fs::write(bad_md.join("C2099A00001/C2099A00001.md"), "# H\n").unwrap();
    std::fs::write(
        bad_md.join("C2099A00001/sections.jsonl"),
        "{\"register_id\":\"C2099A00001\",\"row_id\":\"C2099A00001:0001:-\",\"authorised\":false,\"heading\":1,\"text\":\"t\"}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("sources.json"),
        r#"{"titles":[{"register_id":"C2099A00001","name":[],"jsonl_rows":1}]}"#,
    )
    .unwrap();
    let out_u = dir.path().join("u.db");
    assert!(matches!(
        build_with_rulings(root.to_str().unwrap(), out_u.to_str().unwrap(), &[]),
        Err(Exc::Type(_))
    ));
}

#[test]
fn search_build_and_publish_cover_error_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("corpus");
    std::fs::create_dir_all(root.join("markdown/C2099A00001")).unwrap();
    std::fs::write(root.join("markdown/C2099A00001/C2099A00001.md"), "# H\n").unwrap();
    std::fs::write(
        root.join("markdown/C2099A00001/sections.jsonl"),
        "{\"register_id\":\"C2099A00001\",\"row_id\":\"C2099A00001:0001:-\",\"authorised\":false,\"heading\":1,\"text\":\"t\"}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("sources.json"),
        r#"{"titles":[{"register_id":"C2099A00001","name":"Act","jsonl_rows":1}]}"#,
    )
    .unwrap();
    let out_e = dir.path().join("e.db");
    assert!(matches!(
        build_with_rulings(root.to_str().unwrap(), out_e.to_str().unwrap(), &[]),
        Err(Exc::Type(_))
    ));
    let existing = dir.path().join("exists.db");
    std::fs::write(&existing, "x").unwrap();
    assert!(matches!(
        publish(&PyPath::new(existing.to_str().unwrap()), |p| {
            std::fs::write(p, "y").unwrap();
            Ok(Counts::default())
        }),
        Err(Exc::Os(_))
    ));
}

#[test]
fn search_rulings_meta_and_citations_cover_edge_paths() {
    let dir = tempfile::tempdir().unwrap();
    let rdir = dir.path().join("rrun");
    std::fs::create_dir(&rdir).unwrap();
    std::fs::write(
        rdir.join("LICENCE-NOTICE.md"),
        super::super::rulings::NOTICE,
    )
    .unwrap();
    std::fs::write(rdir.join("rulings.jsonl"), "\n").unwrap();
    std::fs::write(
        rdir.join("manifest.json"),
        r#"{"stage":"rulings","rows":0,"fetched_on":"2026-01-01","documents":[],"excluded":[]}"#,
    )
    .unwrap();
    assert!(read_rulings_run(rdir.to_str().unwrap()).is_ok());
    let db = dir.path().join("meta.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT)", [])
        .unwrap();
    drop(conn);
    assert!(open_index(db.to_str().unwrap()).is_err());
    let conn = Connection::open(&db).unwrap();
    conn.execute("INSERT INTO meta VALUES ('source', 'unknown')", [])
        .unwrap();
    assert!(index_source(&conn).is_err());
    conn.execute("DELETE FROM meta", []).unwrap();
    let mut r = Run::default();
    print_citations(
        &[Json::obj([
            ("line", Json::Int(1.into())),
            ("outcome", Json::str("ambiguous")),
            ("citation", Json::str("s 1")),
            ("rows", Json::Null),
        ])],
        &conn,
        &mut r,
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&r.stdout).contains("ambiguous"));
}
