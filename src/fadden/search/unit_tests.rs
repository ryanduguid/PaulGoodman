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
