// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! The fuzz targets' seed inputs, replayed through the same invariant checks
//! on stable Rust, so ordinary CI catches a broken check. fuzz/README.md
//! explains the targets; `privacy_patterns`'s seeds replay in fadden_pii.rs.
use std::path::Path;

fn seeds(target: &str) -> Vec<Vec<u8>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fuzz")
        .join("seeds")
        .join(target);
    let found: Vec<Vec<u8>> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| std::fs::read(e.unwrap().path()).unwrap())
        .collect();
    assert!(!found.is_empty(), "{target} has no seeds");
    found
}

#[test]
fn json_decode_seeds_round_trip() {
    for seed in seeds("json_decode") {
        tax_radar_au::pyjson::check_round_trip(&seed).unwrap();
    }
}

#[test]
fn python_sort_seeds_sort() {
    for seed in seeds("python_sort") {
        tax_radar_au::pysort::check_one_input(&seed).unwrap();
    }
}

#[test]
fn pattern_engines_seeds_agree() {
    for seed in seeds("pattern_engines") {
        tax_radar_au::fadden::pii::check_engines(&seed).unwrap();
    }
}

/// The epub_read fuzz target's invariant: an archive is admitted or refused
/// without a panic, and each member that reads back has its declared length
/// and CRC-32.
fn reads_back_as_declared(data: &[u8]) {
    let Ok(book) = tax_radar_au::fadden::epub::from_bytes(data.to_vec()) else {
        return;
    };
    for info in &book.members {
        if let Ok(content) = book.read(&info.filename) {
            let read = (
                content.len() as u64,
                tax_radar_au::pyzlib::crc32(&content, 0),
            );
            assert_eq!(read, (info.file_size, info.crc), "{:?}", info.filename);
        }
    }
}

#[test]
fn epub_read_seeds_read() {
    for seed in seeds("epub_read") {
        reads_back_as_declared(&seed);
    }
}

#[test]
fn html_parse_seeds_keep_positions_in_order() {
    for seed in seeds("html_parse") {
        tax_radar_au::htmlparser::check_positions(&seed).unwrap();
    }
}

fn seed(target: &str, name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fuzz")
        .join("seeds")
        .join(target)
        .join(name);
    std::fs::read(path).unwrap()
}

// reads_back_as_declared and check_positions pass when nothing opens or nothing parses,
// so these pin one valid seed each to the result it must give.

#[test]
fn epub_read_minimal_seed_reads_every_member() {
    use tax_radar_au::fadden::epub;
    let data = seed("epub_read", "minimal");
    let book = epub::from_bytes(data.clone()).unwrap();
    let names: Vec<&str> = book.namelist().collect();
    assert_eq!(
        names,
        ["mimetype", "META-INF/container.xml", "OEBPS/s6-5.xhtml"]
    );
    let sizes: Vec<usize> = names.iter().map(|n| book.read(n).unwrap().len()).collect();
    // Sizes from Python's zipfile; reads_back_as_declared then matches each CRC-32.
    assert_eq!(sizes, [20, 221, 79]);
    assert_eq!(book.read("mimetype").unwrap(), b"application/epub+zip");
    reads_back_as_declared(&data);
}

#[test]
fn html_parse_multiline_seed_matches_cpython_positions() {
    use tax_radar_au::htmlparser::{Event, parse};
    let text = String::from_utf8(seed("html_parse", "multiline")).unwrap();
    let start = |tag: &str| Event::StartTag(tag.into(), vec![]);
    let end = |tag: &str| Event::EndTag(tag.into());
    let data = |text: &str| Event::Data(text.into());
    let img = Event::StartTag(
        "img".into(),
        vec![
            ("src".into(), Some("x".into())),
            ("alt".into(), Some("y".into())),
        ],
    );
    // CPython 3.15 html.parser getpos() in each handler, convert_charrefs=True.
    let expected = vec![
        (start("table"), 1, 0),
        (data("\r\n"), 1, 7),
        (start("tr"), 2, 0),
        (start("td"), 2, 4),
        (data("a"), 2, 8),
        (end("td"), 2, 9),
        (data("\r"), 2, 14),
        (start("td"), 2, 15),
        (data("b"), 2, 19),
        (end("td"), 2, 20),
        (data("\n"), 2, 25),
        (end("tr"), 3, 0),
        (data("\n"), 3, 5),
        (end("table"), 4, 0),
        (data("\n"), 4, 8),
        (Event::StartEndTag("br".into(), vec![]), 5, 0),
        (data("\n"), 5, 5),
        (img, 6, 0),
    ];
    let got: Vec<_> = parse(&text, true)
        .into_iter()
        .map(|p| (p.event, p.line, p.offset))
        .collect();
    assert_eq!(got, expected);
}
