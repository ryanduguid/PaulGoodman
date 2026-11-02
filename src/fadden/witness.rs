//! `fadden witness`: for a small committed sample of titles, the section
//! identifiers the `extract` stage yields from each title's EPUB against those
//! the Register's Word rendition of the same compilation marks up.
//!
//! The Word side reads each volume's `word/document.xml`: the text of the
//! `CharSectno` runs in every `ActHead5` paragraph, the markup the Acts'
//! drafting template gives a section heading's number. Both renditions come
//! from one Register source, so agreement shows that the EPUB conversion and
//! the extractor kept the section structure; it is no check of the source.
//!
//! The reviewed tolerance (BUILD.md, "Witnessing section structure"): Word
//! writes the hyphen in a section number such as `8-1` as a
//! `<w:noBreakHyphen/>` element where the EPUB has U+2011 and the extractor
//! writes `-`, so the element reads as `-`. Nothing else is normalised: any
//! other difference in an identifier's count is a review item. Only counts
//! are compared, not the order of the sections.
use std::collections::BTreeMap;

use super::corpus_paths::{child, register_id, write_json_atomic};
use super::{Exc, Run, extract, json_error};
use crate::argparse::{Action, Exit, Get, Parser};
use crate::pyjson::{self, Json};
use word::word_volume;

mod word;

const TOLERANCE: &str = "the <w:noBreakHyphen/> element in a Word section number reads as '-'";

fn refuse(message: impl Into<String>) -> Exc {
    Exc::value(message)
}

/// The sampled titles as (register id, name), in register id order. A list
/// that is empty, or holds an invalid id, a title without a name or a
/// repeated id, is refused.
pub fn load_sample(path: &str) -> Result<Vec<(String, Json)>, Exc> {
    let sample = pyjson::loads_standard(&crate::pyio::read_text(path)?).map_err(json_error)?;
    let Json::Array(items) = &sample else {
        return Err(refuse(format!(
            "{path}: the sample must be a JSON list of titles"
        )));
    };
    if items.is_empty() {
        return Err(refuse(format!("{path}: the sample lists no titles")));
    }
    let mut titles = BTreeMap::new();
    for item in items {
        let rid = register_id(item.get("id").unwrap_or(&Json::Null))
            .map_err(|e| refuse(format!("{path}: {e}")))?;
        let Some(name @ Json::Str(_)) = item.get("name") else {
            return Err(refuse(format!("{path}: {rid} has no name")));
        };
        if titles.insert(rid.clone(), name.clone()).is_some() {
            return Err(refuse(format!("{path}: {rid} is listed twice")));
        }
    }
    Ok(titles.into_iter().collect())
}

/// A listing value as text: the Register gives numbers as JSON strings or
/// integers depending on the field.
fn listed(row: &Json, key: &str) -> Option<String> {
    match row.get(key)? {
        Json::Str(s) => s.as_str().map(str::to_string),
        Json::Int(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The rows of a saved `documents` listing. A page of a longer listing is
/// refused.
fn listing_rows(path: &str) -> Result<Vec<Json>, Exc> {
    let listing = pyjson::loads_standard(&crate::pyio::read_text(path)?).map_err(json_error)?;
    if listing.get("@odata.nextLink").is_some() {
        return Err(refuse(format!(
            "{path} is one page of a longer listing (it has @odata.nextLink); filter it to the compilation's start date so it fits on one page"
        )));
    }
    match listing.get("value") {
        Some(Json::Array(rows)) => Ok(rows.clone()),
        _ => Err(refuse(format!(
            "{path}: not a documents listing (no \"value\" list)"
        ))),
    }
}

/// A listing row of the EPUB's version: its EPUB, or a Word volume.
enum Document {
    Epub,
    Word(u32),
}

/// The row's document when the row has the whole version key and is an EPUB
/// or Word row; `None` for another version's row or another format. A row of
/// another title, or a Word row with no volume number, is refused.
fn version_document(
    row: &Json,
    path: &str,
    rid: &str,
    version: &[(&str, &str)],
) -> Result<Option<Document>, Exc> {
    let title = listed(row, "titleId");
    if title.as_deref() != Some(rid) {
        return Err(refuse(format!(
            "{path} lists a document of {}, not {rid}",
            title.as_deref().unwrap_or("no title")
        )));
    }
    if !version
        .iter()
        .all(|(key, value)| listed(row, key).as_deref() == Some(*value))
    {
        return Ok(None);
    }
    Ok(match listed(row, "format").as_deref() {
        Some("Epub") => Some(Document::Epub),
        Some("Word") => Some(Document::Word(
            listed(row, "volumeNumber")
                .and_then(|v| v.parse::<u32>().ok())
                .ok_or_else(|| refuse(format!("{path}: a Word row has no volumeNumber")))?,
        )),
        _ => None,
    })
}

/// The volume numbers of the compilation's Word documents, from
/// `<id>/documents.json`: the Register API's `documents` listing for the
/// compilation, saved as returned. Every row must be this title's. The EPUB's
/// version is the one `download` fetches: the website's
/// `<id>/<date>/<date>/text/original/epub` serves the original text
/// (rectification 0) of the compilation's primary document. The listing must
/// hold exactly one EPUB row with that whole version key, and the Word rows
/// with the same key are the volumes; rows of other versions on the same date
/// (the text as made, a rectification) are not. A page of a longer listing, a
/// listing with no such Word row, or one that lists a volume twice is refused.
fn listed_volumes(
    path: &str,
    rid: &str,
    register: &str,
    number: &str,
    date: &str,
) -> Result<Vec<u32>, Exc> {
    let start = format!("{date}T00:00:00");
    let version = [
        ("registerId", register),
        ("compilationNumber", number),
        ("start", start.as_str()),
        ("retrospectiveStart", start.as_str()),
        ("rectificationVersionNumber", "0"),
        ("type", "Primary"),
        ("uniqueTypeNumber", "0"),
    ];
    let described = format!("{register} compilation {number} of {date}, rectification 0");
    let (mut epubs, mut volumes) = (0, Vec::new());
    for row in &listing_rows(path)? {
        match version_document(row, path, rid, &version)? {
            Some(Document::Epub) => epubs += 1,
            Some(Document::Word(volume)) if volumes.contains(&volume) => {
                return Err(refuse(format!("{path} lists Word volume {volume} twice")));
            }
            Some(Document::Word(volume)) => volumes.push(volume),
            None => {}
        }
    }
    if epubs != 1 {
        return Err(refuse(format!(
            "{path} lists {epubs} EPUB rows of {described}, the version download fetches; it must list exactly 1"
        )));
    }
    if volumes.is_empty() {
        return Err(refuse(format!(
            "{path} lists no Word document of {described}"
        )));
    }
    volumes.sort_unstable();
    Ok(volumes)
}

/// The identifiers whose counts differ, as (identifier, EPUB count, Word
/// count), in identifier order.
pub fn divergences(epub: &[String], word: &[String]) -> Vec<(String, usize, usize)> {
    let mut counts: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for id in epub {
        counts.entry(id).or_default().0 += 1;
    }
    for id in word {
        counts.entry(id).or_default().1 += 1;
    }
    counts
        .into_iter()
        .filter(|(_, (e, w))| e != w)
        .map(|(id, (e, w))| (id.to_string(), e, w))
        .collect()
}

struct Witnessed {
    rid: String,
    number: String,
    date: String,
    volumes: usize,
    epub: usize,
    word: usize,
    items: Vec<(String, usize, usize)>,
}

fn sidecar_text(meta: &Json, key: &str, side: &str) -> Result<String, Exc> {
    meta.get(key)
        .and_then(Json::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| refuse(format!("{side} records no {key}")))
}

/// The `.docx` files in a title's Word directory, which must be exactly the
/// volumes the Register lists (`expected`), in name order.
fn volume_files(volumes_dir: &str, expected: &[String]) -> Result<Vec<String>, Exc> {
    let os_error = |e: std::io::Error| {
        crate::pyos::OsError::new(&e, Some(volumes_dir), crate::pyos::Style::Native)
    };
    let mut names = Vec::new();
    for entry in std::fs::read_dir(volumes_dir).map_err(os_error)? {
        let name = entry
            .map_err(os_error)?
            .file_name()
            .into_string()
            .map_err(|_| refuse(format!("{volumes_dir}: a file name is not valid Unicode")))?;
        if name.ends_with(".docx") {
            names.push(name);
        }
    }
    names.sort();
    if let Some(extra) = names.iter().find(|n| !expected.contains(n)) {
        return Err(refuse(format!(
            "{volumes_dir}: {extra} is not a Word volume documents.json lists ({})",
            expected.join(", ")
        )));
    }
    if let Some(missing) = expected.iter().find(|n| !names.contains(n)) {
        return Err(refuse(format!(
            "{volumes_dir}: documents.json lists {missing}, which is missing"
        )));
    }
    Ok(names)
}

/// One volume's section identifiers. The volume must be of the EPUB's
/// compilation (`number`) and titled as the sample names the title.
fn volume_sections(path: &str, number: &str, rid: &str, sampled: &str) -> Result<Vec<String>, Exc> {
    let volume = word_volume(path)?;
    match volume.compilation {
        Some(found) if found == number => {}
        Some(found) => {
            return Err(refuse(format!(
                "{path} is compilation {found}; the EPUB is compilation {number}"
            )));
        }
        None => {
            return Err(refuse(format!(
                "{path} records no CompilationNumber, so it cannot be matched to the EPUB's compilation"
            )));
        }
    }
    if volume.title.as_deref() != Some(sampled) {
        return Err(refuse(format!(
            "{path} is titled {}; the sample names {rid} {sampled}",
            volume
                .title
                .as_deref()
                .unwrap_or("nothing (no ShortT property)"),
        )));
    }
    Ok(volume.sections)
}

fn witness_title(rid: &str, name: &Json, epub_dir: &str, word_dir: &str) -> Result<Witnessed, Exc> {
    let side = child(epub_dir, &[&format!("{rid}.epub.meta.json")])?;
    let meta = pyjson::loads_standard(&crate::pyio::read_text(&side)?).map_err(json_error)?;
    let number = sidecar_text(&meta, "compilationNumber", &side)?;
    let date = sidecar_text(&meta, "versionStart", &side)?;
    let register = sidecar_text(&meta, "compilationRegisterId", &side)?;

    let volumes_dir = child(word_dir, &[rid])?;
    let expected: Vec<String> = listed_volumes(
        &child(&volumes_dir, &["documents.json"])?,
        rid,
        &register,
        &number,
        &date,
    )?
    .iter()
    .map(|v| format!("{rid}.vol{v:02}.docx"))
    .collect();
    let names = volume_files(&volumes_dir, &expected)?;
    // load_sample admits only a string name.
    let sampled = name.as_str().unwrap_or_default();
    let mut word = Vec::new();
    for file in &names {
        word.extend(volume_sections(
            &child(&volumes_dir, &[file])?,
            &number,
            rid,
            sampled,
        )?);
    }
    let epub_path = child(epub_dir, &[&format!("{rid}.epub")])?;
    let epub = extract::section_ids(&epub_path, rid, name)?;
    Ok(Witnessed {
        rid: rid.to_string(),
        number,
        date,
        volumes: names.len(),
        epub: epub.len(),
        word: word.len(),
        items: divergences(&epub, &word),
    })
}

fn many(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn count(n: usize) -> Json {
    Json::Int(n.into())
}

fn reason(epub: usize, word: usize) -> String {
    if word == 0 {
        "the EPUB extraction yields this section; no ActHead5 heading in the Word rendition is numbered so".into()
    } else if epub == 0 {
        "an ActHead5 heading in the Word rendition is numbered so; the EPUB extraction yields no section row for it".into()
    } else {
        format!(
            "the EPUB extraction yields {epub} section rows numbered so and the Word rendition {word} headings"
        )
    }
}

fn report(titles: &[Witnessed]) -> Json {
    let compilation = |t: &Witnessed| {
        Json::obj([
            ("number", Json::str(&t.number)),
            ("date", Json::str(&t.date)),
        ])
    };
    let summaries = titles
        .iter()
        .map(|t| {
            Json::obj([
                ("title", Json::str(&t.rid)),
                ("compilation", compilation(t)),
                ("word_volumes", count(t.volumes)),
                ("epub_sections", count(t.epub)),
                ("word_sections", count(t.word)),
                ("review_items", count(t.items.len())),
            ])
        })
        .collect();
    let items = titles
        .iter()
        .flat_map(|t| {
            t.items.iter().map(move |(id, e, w)| {
                Json::obj([
                    ("title", Json::str(&t.rid)),
                    ("compilation", compilation(t)),
                    ("identifier", Json::str(id)),
                    ("side", Json::str(if e > w { "epub" } else { "word" })),
                    ("epub_count", count(*e)),
                    ("word_count", count(*w)),
                    ("reason", Json::str(&reason(*e, *w))),
                ])
            })
        })
        .collect();
    Json::obj([
        ("tolerance", Json::str(TOLERANCE)),
        ("titles", Json::Array(summaries)),
        ("review_items", Json::Array(items)),
    ])
}

fn witness(
    sample: &str,
    epub_dir: &str,
    word_dir: &str,
    out: &str,
    run: &mut Run,
) -> Result<(), Exc> {
    let mut titles = Vec::new();
    for (rid, name) in load_sample(sample)? {
        titles.push(
            witness_title(&rid, &name, epub_dir, word_dir)
                .map_err(|e| refuse(format!("{rid}: {e}")))?,
        );
    }
    write_json_atomic(out, &report(&titles), Some(1), 8)?;
    for t in &titles {
        run.print(&format!(
            "{} compilation {} ({}): {}, {} in {}, {}",
            t.rid,
            t.number,
            t.date,
            many(t.epub, "EPUB section"),
            many(t.word, "Word section"),
            many(t.volumes, "volume"),
            many(t.items.len(), "review item")
        ));
    }
    let total: usize = titles.iter().map(|t| t.items.len()).sum();
    run.print(&format!(
        "{} across {}; report written to {out}",
        many(total, "review item"),
        many(titles.len(), "title")
    ));
    run.print(
        "This compares 2 renditions of one Register compilation for a fixed sample; \
         agreement is not proof that the extraction is correct.",
    );
    Ok(())
}

fn parser() -> Parser {
    Parser::new("python -m fadden witness")
        .description(
            "Compare the section identifiers extract yields from each sampled title's EPUB \
             with those its Word rendition numbers, and write the differences as review items.",
        )
        .arg(
            Action::positional("sample")
                .help("JSON list of the sampled titles, each with an id and name"),
        )
        .arg(
            Action::option("--epub-dir")
                .required()
                .help("directory holding <id>.epub and the <id>.epub.meta.json download wrote"),
        )
        .arg(
            Action::option("--word-dir")
                .required()
                .help("directory holding <id>/*.docx, the Word volumes of the same compilation"),
        )
        .arg(
            Action::option("--out")
                .required()
                .help("the JSON report to write"),
        )
}

pub fn main(argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let args = parser().parse(argv)?;
    let get = |key: &str| args.str(key).unwrap_or_default();
    match witness(
        &get("sample"),
        &get("epub_dir"),
        &get("word_dir"),
        &get("out"),
        run,
    ) {
        Ok(()) => Ok(0),
        Err(error) => {
            run.eprint(&format!("error: {error}"));
            Ok(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divergences_compare_counts_in_identifier_order() {
        let ids = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            divergences(&ids(&["2", "1", "1", "3"]), &ids(&["1", "3", "4"])),
            [
                ("1".to_string(), 2, 1),
                ("2".to_string(), 1, 0),
                ("4".to_string(), 0, 1)
            ]
        );
    }
}
