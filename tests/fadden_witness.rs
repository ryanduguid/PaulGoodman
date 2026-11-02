//! `fadden witness`: fabricated EPUB and Word renditions of one compilation,
//! built here. The EPUB marks section numbers as the Register's EPUBs do
//! (`ActHead5` paragraphs, `CharSectno` spans, U+2011 hyphens); the Word
//! volumes as its Word compilations do (split `CharSectno` runs,
//! `<w:noBreakHyphen/>`, a `TofSectsSection` contents entry, the
//! `CompilationNumber` property).
use std::path::PathBuf;
use std::process::Command;

use serde_json::{Value, json};
use tax_radar_au::fadden::{Run, extract};
use tax_radar_au::pyjson::Json;
use tax_radar_au::pyzlib::crc32;

const ACT: &str = "C2099A00001";
const OTHER: &str = "C2099A00002";

/// A ZIP of stored members, laid out as `zipfile.writestr` writes one.
fn zip(members: &[(&str, String)]) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for (name, data) in members {
        let (name, data) = (name.as_bytes(), data.as_bytes());
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

/// A Word volume numbering `ids`, with `compilation` as its property.
fn docx(ids: &[&str], compilation: Option<&str>) -> Vec<u8> {
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
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:body>{body}</w:body></w:document>"
    );
    let mut members = vec![
        ("[Content_Types].xml", "<Types/>".to_string()),
        ("word/document.xml", document),
    ];
    if let Some(number) = compilation {
        members.push((
            "docProps/custom.xml",
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
                 <Properties xmlns:vt=\"http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes\">\
                 <property fmtid=\"{{D5CDD505-2E9C-101B-9397-08002B2CF9AE}}\" pid=\"2\" name=\"Compilation\"><vt:lpwstr>Yes</vt:lpwstr></property>\
                 <property fmtid=\"{{D5CDD505-2E9C-101B-9397-08002B2CF9AE}}\" pid=\"3\" name=\"CompilationNumber\"><vt:lpwstr>{number}</vt:lpwstr></property>\
                 </Properties>"
            ),
        ));
    }
    zip(&members)
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
            .map(|rid| json!({"id": rid, "name": format!("Synthetic Act {rid}")}))
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

    fn word(&self, rid: &str, volumes: &[(&[&str], Option<&str>)]) {
        let dir = self.root.join("word").join(rid);
        std::fs::create_dir_all(&dir).unwrap();
        for (i, (ids, compilation)) in volumes.iter().enumerate() {
            let name = format!("{rid}.vol{:02}.docx", i + 1);
            std::fs::write(dir.join(name), docx(ids, *compilation)).unwrap();
        }
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
    let cases: [Case; 7] = [
        ("is listed twice", |f| {
            f.sample(&json!([{"id": ACT, "name": "A"}, {"id": ACT, "name": "A"}]));
        }),
        ("invalid Federal Register identifier", |f| {
            f.sample(&json!([{"id": "C2099A0001", "name": "A"}]));
        }),
        ("the sample lists no titles", |f| f.sample(&json!([]))),
        ("has no name", |f| f.sample(&json!([{"id": ACT}]))),
        ("no Word volumes (*.docx)", |f| {
            std::fs::create_dir(f.root.join("word").join(ACT)).unwrap();
        }),
        ("no word/document.xml; not a Word document", |f| {
            let dir = f.root.join("word").join(ACT);
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(
                dir.join("x.docx"),
                zip(&[("[Content_Types].xml", "<Types/>".into())]),
            )
            .unwrap();
        }),
        ("records no compilationNumber", |f| {
            f.epub(
                ACT,
                &[numbered(&["1"])],
                &json!({"versionStart": "2099-07-01"}),
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
fn a_truncated_word_volume_is_refused() {
    let f = Fixture::new();
    agreeing(&f);
    let dir = f.root.join("word").join(ACT);
    std::fs::create_dir(&dir).unwrap();
    let document = "<w:document><w:body><w:p><w:pPr><w:pStyle w:val=\"ActHead5\"/></w:pPr>";
    let custom = "<Properties><property name=\"CompilationNumber\"><vt:lpwstr>32</vt:lpwstr></property></Properties>";
    let bytes = zip(&[
        ("word/document.xml", document.into()),
        ("docProps/custom.xml", custom.into()),
    ]);
    std::fs::write(dir.join("v.docx"), bytes).unwrap();
    refused(&f, "the document ends inside a paragraph or run");
}
