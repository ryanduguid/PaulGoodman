// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: FSL-1.1-ALv2

//! `fadden witness`: fabricated EPUB and Word renditions of one compilation,
//! built here. The EPUB marks section numbers as the Register's EPUBs do
//! (`ActHead5` paragraphs, `CharSectno` spans, U+2011 hyphens); the Word
//! volumes as its Word compilations do (split `CharSectno` runs,
//! `<w:noBreakHyphen/>`, a `TofSectsSection` contents entry, the
//! `CompilationNumber` and `ShortT` properties), with the Register's
//! `documents` listing beside them.
use std::path::PathBuf;
use std::process::Command;

use serde_json::{Value, json};
use tax_radar_au::fadden::{Run, extract};
use tax_radar_au::pyjson::Json;
use tax_radar_au::pyzlib::crc32;

const ACT: &str = "C2099A00001";
const OTHER: &str = "C2099A00002";

/// A ZIP of stored members, laid out as `zipfile.writestr` writes one.
fn zip<D: AsRef<[u8]>>(members: &[(&str, D)]) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for (name, data) in members {
        let (name, data) = (name.as_bytes(), data.as_ref());
        let sizes = [crc32(data, 0), data.len() as u32, data.len() as u32];
        let offset = out.len() as u32;
        out.extend(b"PK\x03\x04");
        for v in [20u16, 0, 0, 0, 33] {
            out.extend(v.to_le_bytes());
        }
        sizes.iter().for_each(|v| out.extend(v.to_le_bytes()));
        for v in [name.len() as u16, 0] {
            out.extend(v.to_le_bytes());
        }
        out.extend(name);
        out.extend(data);
        central.extend(b"PK\x01\x02");
        for v in [20u16, 20, 0, 0, 0, 33] {
            central.extend(v.to_le_bytes());
        }
        sizes.iter().for_each(|v| central.extend(v.to_le_bytes()));
        for v in [name.len() as u16, 0, 0, 0, 0] {
            central.extend(v.to_le_bytes());
        }
        for v in [0u32, offset] {
            central.extend(v.to_le_bytes());
        }
        central.extend(name);
    }
    let (start, size, count) = (out.len() as u32, central.len() as u32, members.len() as u16);
    out.extend(central);
    out.extend(b"PK\x05\x06");
    for v in [0u16, 0, count, count] {
        out.extend(v.to_le_bytes());
    }
    for v in [size, start] {
        out.extend(v.to_le_bytes());
    }
    out.extend(0u16.to_le_bytes());
    out
}

/// One EPUB heading: its paragraph class and identifier.
type Heading<'a> = (&'a str, &'a str);

fn numbered(ids: &[&'static str]) -> Vec<Heading<'static>> {
    ids.iter().map(|id| ("ActHead5", *id)).collect()
}

/// An EPUB with one XHTML document per volume, as the Register writes them.
fn epub(volumes: &[Vec<Heading>]) -> Vec<u8> {
    let mut members = vec![("mimetype".to_string(), "application/epub+zip".to_string())];
    for (i, headings) in volumes.iter().enumerate() {
        let mut html = String::from(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <html xmlns=\"http://www.w3.org/1999/xhtml\"><body>",
        );
        for (class, id) in headings {
            let number = id.replace('-', "\u{2011}");
            html.push_str(&format!(
                "<p class=\"TOC5\">{number} Heading {id}</p>\
                 <p id=\"navPoint_{id}\" class=\"{class}\"><a><span class=\"CharSectno\">{number}</span>\
                 <span>&#xa0; Heading {id}</span></a></p>\
                 <p class=\"subsection\">Text of section {id}.</p>"
            ));
        }
        html.push_str("</body></html>");
        members.push((
            format!("OEBPS/document_{n}/document_{n}.html", n = i + 1),
            html,
        ));
    }
    let refs: Vec<(&str, String)> = members
        .iter()
        .map(|(n, d)| (n.as_str(), d.clone()))
        .collect();
    zip(&refs)
}

const RUN_NUMBER: &str = "<w:r><w:rPr><w:rStyle w:val=\"CharSectno\"/></w:rPr>";

/// The custom properties part of a Word volume: `Compilation` and each of
/// `properties` in order.
fn custom(properties: &[(&str, &str)]) -> String {
    let mut body = String::new();
    for (pid, (name, value)) in std::iter::once(&("Compilation", "Yes"))
        .chain(properties)
        .enumerate()
    {
        body.push_str(&format!(
            "<property fmtid=\"{{D5CDD505-2E9C-101B-9397-08002B2CF9AE}}\" pid=\"{}\" name=\"{name}\">\
             <vt:lpwstr>{value}</vt:lpwstr></property>",
            pid + 2
        ));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <Properties xmlns:vt=\"http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes\">\
         {body}</Properties>"
    )
}

/// The name the fixture's sample gives `rid`, which its Word volumes carry as `ShortT`.
fn title(rid: &str) -> String {
    format!("Synthetic Act {rid}")
}

/// A Word volume numbering `ids`, with `compilation` and `title` as its
/// properties; with neither, it has no custom properties part.
fn docx(ids: &[&str], compilation: Option<&str>, title: Option<&str>) -> Vec<u8> {
    let mut members = vec![
        ("[Content_Types].xml", "<Types/>".to_string()),
        ("word/document.xml", document(ids)),
    ];
    let mut properties = Vec::new();
    properties.extend(compilation.map(|n| ("CompilationNumber", n)));
    properties.extend(title.map(|t| ("ShortT", t)));
    if !properties.is_empty() {
        members.push(("docProps/custom.xml", custom(&properties)));
    }
    zip(&members)
}

/// A Word volume's `word/document.xml`, numbering `ids`.
fn document(ids: &[&str]) -> String {
    let mut body = String::new();
    if let Some(first) = ids.first() {
        // The table of sections numbers its entries with the heading's run style.
        body.push_str(&format!(
            "<w:p><w:pPr><w:pStyle w:val=\"TofSectsSection\"/></w:pPr>\
             {RUN_NUMBER}<w:t>{first}</w:t></w:r></w:p>"
        ));
    }
    for id in ids {
        let mut runs = String::new();
        for (i, part) in id.split('-').enumerate() {
            if i > 0 {
                runs.push_str(&format!("{RUN_NUMBER}<w:noBreakHyphen/></w:r>"));
            }
            runs.push_str(&format!("{RUN_NUMBER}<w:t>{part}</w:t></w:r>"));
        }
        body.push_str(&format!(
            "<w:p><w:pPr><w:pStyle w:val=\"ActHead5\"/></w:pPr><w:bookmarkStart w:id=\"1\" w:name=\"_Toc1\"/>\
             {runs}<w:r><w:t xml:space=\"preserve\">  Heading {id}</w:t></w:r><w:bookmarkEnd w:id=\"1\"/></w:p>\
             <w:p><w:pPr><w:pStyle w:val=\"subsection\"/></w:pPr><w:r><w:t>Text of section {id}.</w:t></w:r></w:p>"
        ));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:body>{body}</w:body></w:document>"
    )
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir(root.join("epub")).unwrap();
        std::fs::create_dir(root.join("word")).unwrap();
        Fixture { _dir: dir, root }
    }

    fn sample(&self, sample: &Value) {
        std::fs::write(self.root.join("sample.json"), sample.to_string()).unwrap();
    }

    fn titles(&self, rids: &[&str]) {
        let items: Vec<Value> = rids
            .iter()
            .map(|rid| json!({"id": rid, "name": title(rid)}))
            .collect();
        self.sample(&Value::Array(items));
    }

    fn epub(&self, rid: &str, volumes: &[Vec<Heading>], sidecar: &Value) {
        let dir = self.root.join("epub");
        std::fs::write(dir.join(format!("{rid}.epub")), epub(volumes)).unwrap();
        std::fs::write(
            dir.join(format!("{rid}.epub.meta.json")),
            sidecar.to_string(),
        )
        .unwrap();
    }

    /// `WORD_DIR/<rid>/documents.json` as the Register API returns it: the
    /// EPUB row and one Word row per volume number.
    fn listing(&self, rid: &str, compilation: &str, volumes: &[u32]) {
        let mut rows = vec![row(rid, compilation, "Epub", 0)];
        rows.extend(volumes.iter().map(|v| row(rid, compilation, "Word", *v)));
        self.listing_rows(rid, rows);
    }

    fn listing_rows(&self, rid: &str, rows: Vec<Value>) {
        let dir = self.root.join("word").join(rid);
        std::fs::create_dir_all(&dir).unwrap();
        let listing = json!({"@odata.context": "https://api.prod.legislation.gov.au/v1/$metadata#Documents",
                             "value": rows});
        std::fs::write(dir.join("documents.json"), listing.to_string()).unwrap();
    }

    /// Volumes 1, 2 and so on of `rid`, titled as the sample names it, and a
    /// listing of them at the first volume's compilation (32 when it has none).
    fn word(&self, rid: &str, volumes: &[(&[&str], Option<&str>)]) {
        let numbers: Vec<u32> = (1..=volumes.len() as u32).collect();
        let compilation = volumes.first().and_then(|v| v.1).unwrap_or("32");
        self.listing(rid, compilation, &numbers);
        for (number, (ids, compilation)) in numbers.iter().zip(volumes) {
            self.volume(rid, *number, &docx(ids, *compilation, Some(&title(rid))));
        }
    }

    fn volume(&self, rid: &str, number: u32, bytes: &[u8]) {
        let dir = self.root.join("word").join(rid);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{rid}.vol{number:02}.docx")), bytes).unwrap();
    }

    fn out(&self) -> PathBuf {
        self.root.join("witness.json")
    }

    fn run(&self) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_fadden"))
            .arg("witness")
            .arg(self.root.join("sample.json"))
            .arg("--epub-dir")
            .arg(self.root.join("epub"))
            .arg("--word-dir")
            .arg(self.root.join("word"))
            .arg("--out")
            .arg(self.out())
            .output()
            .unwrap();
        let text = |x: &[u8]| String::from_utf8_lossy(x).replace("\r\n", "\n");
        (
            out.status.code().unwrap(),
            text(&out.stdout),
            text(&out.stderr),
        )
    }

    fn report(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.out()).unwrap()).unwrap()
    }
}

fn sidecar(number: &str) -> Value {
    json!({"versionStart": "2099-07-01", "compilationNumber": number, "compilationRegisterId": "C2099C00001"})
}

/// A listing row of the original text (rectification 0) of compilation
/// C2099C00001, starting 1 July 2099, as the Register API gives it.
fn row(rid: &str, compilation: &str, format: &str, volume: u32) -> Value {
    json!({"titleId": rid, "registerId": "C2099C00001", "start": "2099-07-01T00:00:00",
           "retrospectiveStart": "2099-07-01T00:00:00", "rectificationVersionNumber": 0,
           "type": "Primary", "uniqueTypeNumber": 0, "volumeNumber": volume, "format": format,
           "compilationNumber": compilation, "sizeInBytes": 1000})
}

/// `row` with `key` set to `value`.
fn row_with(format: &str, volume: u32, key: &str, value: Value) -> Value {
    let mut row = row(ACT, "32", format, volume);
    row[key] = value;
    row
}

#[test]
fn renditions_that_agree_give_no_review_items_once_word_hyphens_are_read() {
    let f = Fixture::new();
    f.titles(&[ACT]);
    f.epub(
        ACT,
        &[numbered(&["1", "2A", "8-1"]), numbered(&["8-5", "355-25"])],
        &sidecar("32"),
    );
    f.word(
        ACT,
        &[
            (&["1", "2A", "8-1"], Some("32")),
            (&["8-5", "355-25"], Some("32")),
        ],
    );
    let (code, stdout, stderr) = f.run();
    assert_eq!((code, stderr.as_str()), (0, ""));
    assert!(
        stdout.contains(
            "C2099A00001 compilation 32 (2099-07-01): 5 EPUB sections, 5 Word sections in 2 volumes, 0 review items"
        ),
        "{stdout}"
    );
    let report = f.report();
    assert_eq!(report["review_items"], json!([]));
    assert_eq!(
        report["titles"],
        json!([{"title": ACT, "compilation": {"number": "32", "date": "2099-07-01"},
                "word_volumes": 2, "epub_sections": 5, "word_sections": 5, "review_items": 0}])
    );
}

#[test]
fn a_section_the_epub_extraction_loses_or_gains_is_named() {
    let f = Fixture::new();
    // Sample order is not report order.
    f.titles(&[OTHER, ACT]);
    // The EPUB renames one heading's class, which folds section 8-5 into the
    // text of 8-1 without any error, and carries a section 9 Word lacks.
    f.epub(
        ACT,
        &[vec![
            ("ActHead5", "1"),
            ("ActHead5", "8-1"),
            ("ActHead5Drift", "8-5"),
            ("ActHead5", "9"),
        ]],
        &sidecar("32"),
    );
    f.word(ACT, &[(&["1", "8-1", "8-5"], Some("32"))]);
    f.epub(OTHER, &[numbered(&["1", "2"])], &sidecar("4"));
    f.word(OTHER, &[(&["1", "2"], Some("4"))]);
    let (code, stdout, _) = f.run();
    assert_eq!(code, 0);
    assert!(
        stdout.contains(
            "C2099A00002 compilation 4 (2099-07-01): 2 EPUB sections, 2 Word sections in 1 volume, 0 review items"
        ) && stdout.contains("2 review items across 2 titles"),
        "{stdout}"
    );
    let compilation = json!({"number": "32", "date": "2099-07-01"});
    let report = f.report();
    assert_eq!(
        report["review_items"],
        json!([
            {"title": ACT, "compilation": compilation, "identifier": "8-5", "side": "word",
             "epub_count": 0, "word_count": 1,
             "reason": "an ActHead5 heading in the Word rendition is numbered so; the EPUB extraction yields no section row for it"},
            {"title": ACT, "compilation": compilation, "identifier": "9", "side": "epub",
             "epub_count": 1, "word_count": 0,
             "reason": "the EPUB extraction yields this section; no ActHead5 heading in the Word rendition is numbered so"},
        ])
    );
    let titles: Vec<&Value> = report["titles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| &t["title"])
        .collect();
    assert_eq!(titles, [ACT, OTHER]);
}

#[test]
fn the_epub_side_is_what_extract_writes() {
    // The witness reads the EPUB through the extract stage's own passes; its
    // identifiers must be the `section` of the rows extract writes.
    let dir = tempfile::tempdir().unwrap();
    let (build, epubs) = (dir.path().join("build"), dir.path().join("epub"));
    std::fs::create_dir(&build).unwrap();
    std::fs::create_dir(&epubs).unwrap();
    let volumes = [
        vec![
            ("ActHead5", "1"),
            ("ActHead5Drift", "2"),
            ("ActHead5", "8-1"),
        ],
        numbered(&["3A"]),
    ];
    let path = epubs.join(format!("{ACT}.epub"));
    std::fs::write(&path, epub(&volumes)).unwrap();
    let entry = json!([{"id": ACT, "name": "Synthetic Act", "epub": format!("{ACT}.epub"),
                        "versionStart": "2099-07-01", "collection": "Act"}]);
    std::fs::write(build.join("manifest_raw.json"), entry.to_string()).unwrap();
    let mut run = Run::default();
    extract::run(&build.to_string_lossy(), Some("2099-07-02"), &mut run).unwrap();
    let rows =
        std::fs::read_to_string(dir.path().join("markdown").join(ACT).join("sections.jsonl"))
            .unwrap();
    let written: Vec<String> = rows
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|r| r["kind"] == "section")
        .map(|r| r["section"].as_str().unwrap().to_string())
        .collect();
    let ids =
        extract::section_ids(&path.to_string_lossy(), ACT, &Json::str("Synthetic Act")).unwrap();
    assert_eq!(written, ["1", "8-1", "3A"]);
    assert_eq!(ids, written);
}

fn refused(f: &Fixture, message: &str) {
    let (code, stdout, stderr) = f.run();
    assert_eq!(code, 1, "{stdout}{stderr}");
    assert!(
        stderr.starts_with("error: ") && stderr.contains(message),
        "{stderr}"
    );
    assert!(!f.out().exists());
}

fn agreeing(f: &Fixture) {
    f.titles(&[ACT]);
    f.epub(ACT, &[numbered(&["1", "8-1"])], &sidecar("32"));
}

#[test]
fn a_word_rendition_of_another_compilation_is_refused() {
    let f = Fixture::new();
    agreeing(&f);
    f.word(ACT, &[(&["1"], Some("32")), (&["8-1"], Some("31"))]);
    refused(
        &f,
        "vol02.docx is compilation 31; the EPUB is compilation 32",
    );

    let f = Fixture::new();
    agreeing(&f);
    f.word(ACT, &[(&["1", "8-1"], None)]);
    refused(&f, "records no CompilationNumber");
}

/// A refusal's expected message and the change that causes it.
type Case = (&'static str, fn(&Fixture));

#[test]
fn malformed_inputs_are_refused() {
    let cases: [Case; 9] = [
        ("is listed twice", |f| {
            f.sample(&json!([{"id": ACT, "name": "A"}, {"id": ACT, "name": "A"}]));
        }),
        ("invalid Federal Register identifier", |f| {
            f.sample(&json!([{"id": "C2099A0001", "name": "A"}]));
        }),
        ("the sample lists no titles", |f| f.sample(&json!([]))),
        ("has no name", |f| f.sample(&json!([{"id": ACT}]))),
        ("documents.json", |f| {
            std::fs::create_dir(f.root.join("word").join(ACT)).unwrap();
        }),
        ("no word/document.xml; not a Word document", |f| {
            f.listing(ACT, "32", &[1]);
            f.volume(ACT, 1, &zip(&[("[Content_Types].xml", "<Types/>")]));
        }),
        ("no complete ZIP directory record", |f| {
            f.listing(ACT, "32", &[1]);
            f.volume(ACT, 1, b"not an archive");
        }),
        ("records no compilationNumber", |f| {
            f.epub(
                ACT,
                &[numbered(&["1"])],
                &json!({"versionStart": "2099-07-01"}),
            );
            f.word(ACT, &[(&["1"], Some("32"))]);
        }),
        ("records no compilationRegisterId", |f| {
            f.epub(
                ACT,
                &[numbered(&["1"])],
                &json!({"versionStart": "2099-07-01", "compilationNumber": "32"}),
            );
            f.word(ACT, &[(&["1"], Some("32"))]);
        }),
    ];
    for (message, setup) in cases {
        let f = Fixture::new();
        agreeing(&f);
        setup(&f);
        refused(&f, message);
    }
}

#[test]
fn the_volumes_must_be_those_the_register_lists() {
    let cases: [Case; 11] = [
        (
            "documents.json lists C2099A00001.vol02.docx, which is missing",
            |f| {
                f.word(ACT, &[(&["1", "8-1"], Some("32"))]);
                f.listing(ACT, "32", &[1, 2]);
            },
        ),
        // A second copy of a volume is not a volume the listing names.
        (
            "C2099A00001.vol02.docx is not a Word volume documents.json lists (C2099A00001.vol01.docx)",
            |f| {
                f.word(ACT, &[(&["1"], Some("32")), (&["8-1"], Some("32"))]);
                f.listing(ACT, "32", &[1]);
            },
        ),
        ("lists Word volume 1 twice", |f| {
            f.word(ACT, &[(&["1", "8-1"], Some("32"))]);
            f.listing(ACT, "32", &[1, 1]);
        }),
        ("lists a document of C2099A00002, not C2099A00001", |f| {
            f.word(ACT, &[(&["1", "8-1"], Some("32"))]);
            f.listing_rows(ACT, vec![json!({"titleId": OTHER, "start": "2099-07-01T00:00:00",
                                            "volumeNumber": 1, "format": "Word", "compilationNumber": "32"})]);
        }),
        // A listing of another compilation holds no row of the EPUB's version.
        (
            "lists 0 EPUB rows of C2099C00001 compilation 32 of 2099-07-01, rectification 0, the version download fetches; it must list exactly 1",
            |f| {
                f.word(ACT, &[(&["1", "8-1"], Some("32"))]);
                f.listing(ACT, "31", &[1]);
            },
        ),
        ("lists 0 EPUB rows", |f| {
            f.word(ACT, &[(&["1", "8-1"], Some("32"))]);
            f.listing_rows(
                ACT,
                vec![row_with("Epub", 0, "start", json!("2099-01-01T00:00:00"))],
            );
        }),
        // A row whose retrospective start differs is another version (the Register had none on 3 November 2026).
        ("lists 0 EPUB rows", |f| {
            f.word(ACT, &[(&["1", "8-1"], Some("32"))]);
            f.listing_rows(
                ACT,
                vec![
                    row_with(
                        "Epub",
                        0,
                        "retrospectiveStart",
                        json!("2099-07-02T00:00:00"),
                    ),
                    row(ACT, "32", "Word", 1),
                ],
            );
        }),
        ("lists 2 EPUB rows", |f| {
            f.word(ACT, &[(&["1", "8-1"], Some("32"))]);
            f.listing_rows(
                ACT,
                vec![
                    row(ACT, "32", "Epub", 0),
                    row(ACT, "32", "Epub", 0),
                    row(ACT, "32", "Word", 1),
                ],
            );
        }),
        (
            "lists no Word document of C2099C00001 compilation 32 of 2099-07-01, rectification 0",
            |f| {
                f.word(ACT, &[(&["1", "8-1"], Some("32"))]);
                f.listing(ACT, "32", &[]);
            },
        ),
        // The rectification's Word volume shares every field but the
        // rectification number, so it is not the EPUB's.
        ("lists no Word document of", |f| {
            f.word(ACT, &[(&["1", "8-1"], Some("32"))]);
            f.listing_rows(
                ACT,
                vec![
                    row(ACT, "32", "Epub", 0),
                    row_with("Word", 1, "rectificationVersionNumber", json!(1)),
                ],
            );
        }),
        (
            "is one page of a longer listing (it has @odata.nextLink)",
            |f| {
                f.word(ACT, &[(&["1", "8-1"], Some("32"))]);
                let dir = f.root.join("word").join(ACT);
                let listing = json!({"value": [row(ACT, "32", "Epub", 0), row(ACT, "32", "Word", 1)],
                                     "@odata.nextLink": "https://api.prod.legislation.gov.au/v1/documents?$skip=510"});
                std::fs::write(dir.join("documents.json"), listing.to_string()).unwrap();
            },
        ),
    ];
    for (message, setup) in cases {
        let f = Fixture::new();
        agreeing(&f);
        setup(&f);
        refused(&f, message);
    }
}

/// The Register lists every version starting on a date together: here a
/// rectification of the compilation, with a second volume, and the text as
/// made, which carries the title's own register id. Only the original
/// text's Word rows are the EPUB's volumes, so neither counts as a volume
/// the directory lacks or one listed twice.
#[test]
fn only_the_epubs_version_gives_the_volumes() {
    let f = Fixture::new();
    agreeing(&f);
    f.word(ACT, &[(&["1", "8-1"], Some("32"))]);
    let rectified =
        |format, volume| row_with(format, volume, "rectificationVersionNumber", json!(1));
    let mut made = row(ACT, "0", "Word", 1);
    made["registerId"] = json!(ACT);
    f.listing_rows(
        ACT,
        vec![
            rectified("Epub", 0),
            rectified("Word", 1),
            rectified("Word", 2),
            made,
            row(ACT, "32", "Pdf", 1),
            row(ACT, "32", "Epub", 0),
            row(ACT, "32", "Word", 1),
        ],
    );
    let (code, stdout, stderr) = f.run();
    assert_eq!((code, stderr.as_str()), (0, ""));
    assert!(
        stdout.contains("2 Word sections in 1 volume, 0 review items"),
        "{stdout}"
    );
}

#[test]
fn a_word_volume_of_another_title_is_refused() {
    let f = Fixture::new();
    agreeing(&f);
    f.listing(ACT, "32", &[1]);
    f.volume(
        ACT,
        1,
        &docx(&["1", "8-1"], Some("32"), Some("Other Act 1999")),
    );
    refused(
        &f,
        "vol01.docx is titled Other Act 1999; the sample names C2099A00001 Synthetic Act C2099A00001",
    );

    let f = Fixture::new();
    agreeing(&f);
    f.listing(ACT, "32", &[1]);
    f.volume(ACT, 1, &docx(&["1", "8-1"], Some("32"), None));
    refused(&f, "vol01.docx is titled nothing (no ShortT property)");
}

/// A volume with `document` as its body and `custom` as its properties part.
fn volume_parts(document: &str, custom: &[u8]) -> Vec<u8> {
    zip(&[
        ("word/document.xml", document.as_bytes()),
        ("docProps/custom.xml", custom),
    ])
}

#[test]
fn a_malformed_word_part_is_refused() {
    let properties = custom(&[("CompilationNumber", "32"), ("ShortT", &title(ACT))]);
    let agreeing_body = document(&["1", "8-1"]);
    let cases: [(&str, Vec<u8>); 6] = [
        (
            "word/document.xml: the document ends inside <w:p>",
            volume_parts(
                "<w:document><w:body><w:p><w:pPr><w:pStyle w:val=\"ActHead5\"/></w:pPr>",
                properties.as_bytes(),
            ),
        ),
        (
            "word/document.xml: <w:r> is closed by </w:p>",
            volume_parts(
                "<w:document><w:body><w:p><w:r><w:t>1</w:t></w:p></w:body></w:document>",
                properties.as_bytes(),
            ),
        ),
        (
            "word/document.xml: more than one root element",
            volume_parts(
                &format!("{agreeing_body}<w:document/>"),
                properties.as_bytes(),
            ),
        ),
        (
            "docProps/custom.xml is not UTF-8",
            volume_parts(&agreeing_body, &[properties.as_bytes(), b"\xff"].concat()),
        ),
        (
            "docProps/custom.xml: <property> is closed by </Properties>",
            volume_parts(
                &agreeing_body,
                properties
                    .replace("</property></Properties>", "</Properties>")
                    .as_bytes(),
            ),
        ),
        (
            "docProps/custom.xml: the property CompilationNumber is set twice",
            volume_parts(
                &agreeing_body,
                custom(&[
                    ("CompilationNumber", "32"),
                    ("ShortT", &title(ACT)),
                    ("CompilationNumber", "31"),
                ])
                .as_bytes(),
            ),
        ),
    ];
    for (message, bytes) in cases {
        let f = Fixture::new();
        agreeing(&f);
        f.listing(ACT, "32", &[1]);
        f.volume(ACT, 1, &bytes);
        refused(&f, message);
    }
    // The same parts, well formed, agree.
    let f = Fixture::new();
    agreeing(&f);
    f.listing(ACT, "32", &[1]);
    f.volume(ACT, 1, &volume_parts(&agreeing_body, properties.as_bytes()));
    let (code, _, stderr) = f.run();
    assert_eq!((code, stderr.as_str()), (0, ""));
    assert_eq!(f.report()["review_items"], json!([]));
}
