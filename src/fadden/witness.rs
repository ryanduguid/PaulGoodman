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
//! other difference in an identifier's count is a review item.
use std::collections::BTreeMap;

use super::corpus_paths::{child, register_id, write_json_atomic};
use super::{Exc, Run, epub, extract, json_error};
use crate::argparse::{Action, Exit, Get, Parser};
use crate::htmlparser::{self, Event};
use crate::pyjson::{self, Json};

const TOLERANCE: &str = "the <w:noBreakHyphen/> element in a Word section number reads as '-'";
const HEADING_STYLE: &str = "ActHead5";
const NUMBER_STYLE: &str = "CharSectno";
const DOCUMENT: &str = "word/document.xml";
const PROPERTIES: &str = "docProps/custom.xml";

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

fn val(attrs: &[(String, Option<String>)]) -> String {
    attrs
        .iter()
        .find(|(name, _)| name == "w:val")
        .and_then(|(_, value)| value.clone())
        .unwrap_or_default()
}

/// The identifiers a Word body's `ActHead5` paragraphs number, in document
/// order: each paragraph's `CharSectno` run text, joined, with
/// `<w:noBreakHyphen/>` read as `-`. The HTML tokenizer the EPUB reader uses
/// reads this well-formed XML and gives tag and attribute names in lower case.
/// A paragraph or run that closes without opening, or is still open at the
/// end, is refused.
pub fn word_sections(xml: &str) -> Result<Vec<String>, String> {
    #[derive(Default)]
    struct Para {
        style: Option<String>,
        number: String,
    }
    struct WordRun {
        /// The number of open paragraphs when the run opened: a run belongs to
        /// the innermost, not to a text-box paragraph nested inside it.
        depth: usize,
        style: Option<String>,
    }
    let (mut paras, mut runs, mut in_text, mut found) =
        (Vec::<Para>::new(), Vec::<WordRun>::new(), false, Vec::new());
    htmlparser::parse_each(xml, true, |positioned| {
        let numbering = runs
            .last()
            .is_some_and(|r| r.depth == paras.len() && r.style.as_deref() == Some(NUMBER_STYLE));
        match &positioned.event {
            Event::StartTag(tag, _) if tag == "w:p" => paras.push(Para::default()),
            Event::StartTag(tag, _) if tag == "w:r" => runs.push(WordRun {
                depth: paras.len(),
                style: None,
            }),
            Event::StartTag(tag, _) if tag == "w:t" => in_text = true,
            Event::StartTag(tag, attrs) | Event::StartEndTag(tag, attrs) => match tag.as_str() {
                // The first style wins: a tracked change's earlier properties follow it.
                "w:pstyle" => {
                    if let Some(para) = paras.last_mut() {
                        para.style.get_or_insert_with(|| val(attrs));
                    }
                }
                // A paragraph mark's run properties sit outside any run of it.
                "w:rstyle" => {
                    if let Some(run) = runs.last_mut().filter(|r| r.depth == paras.len()) {
                        run.style.get_or_insert_with(|| val(attrs));
                    }
                }
                "w:nobreakhyphen" if numbering => {
                    if let Some(para) = paras.last_mut() {
                        para.number.push('-');
                    }
                }
                _ => {}
            },
            Event::EndTag(tag) if tag == "w:p" => {
                let para = paras.pop().ok_or("a paragraph closes that never opened")?;
                if runs.last().is_some_and(|r| r.depth > paras.len()) {
                    return Err("a paragraph closes inside one of its runs".to_string());
                }
                if para.style.as_deref() == Some(HEADING_STYLE) && !para.number.is_empty() {
                    found.push(para.number);
                }
            }
            Event::EndTag(tag) if tag == "w:r" => {
                runs.pop().ok_or("a run closes that never opened")?;
            }
            Event::EndTag(tag) if tag == "w:t" => in_text = false,
            Event::Data(text) if in_text && numbering => {
                if let Some(para) = paras.last_mut() {
                    para.number.push_str(text);
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    if !paras.is_empty() || !runs.is_empty() {
        return Err("the document ends inside a paragraph or run".into());
    }
    Ok(found)
}

/// The `CompilationNumber` custom document property the Register's Word
/// compilations carry.
fn compilation_number(xml: &str) -> Option<String> {
    let (mut inside, mut value) = (false, String::new());
    for positioned in htmlparser::parse(xml, true) {
        match positioned.event {
            Event::StartTag(tag, attrs) if tag == "property" => {
                inside = attrs
                    .iter()
                    .any(|(k, v)| k == "name" && v.as_deref() == Some("CompilationNumber"));
            }
            Event::Data(text) if inside => value.push_str(&text),
            Event::EndTag(tag) if tag == "property" && inside => {
                return Some(value.trim().to_string());
            }
            _ => {}
        }
    }
    None
}

/// One Word volume: its `CompilationNumber` property and the section
/// identifiers its body numbers. The archive passes the bounded reader the
/// EPUBs pass.
pub struct WordVolume {
    pub compilation: Option<String>,
    pub sections: Vec<String>,
}

pub fn word_volume(path: &str) -> Result<WordVolume, Exc> {
    let at = |e: Exc| refuse(format!("{path}: {e}"));
    let archive = epub::open(path).map_err(at)?;
    if !archive.namelist().any(|n| n == DOCUMENT) {
        return Err(refuse(format!(
            "{path}: no {DOCUMENT}; not a Word document"
        )));
    }
    let document = String::from_utf8(archive.read(DOCUMENT).map_err(at)?)
        .map_err(|_| refuse(format!("{path}: {DOCUMENT} is not UTF-8")))?;
    let sections = word_sections(&document).map_err(|e| refuse(format!("{path}: {e}")))?;
    let compilation = if archive.namelist().any(|n| n == PROPERTIES) {
        compilation_number(&String::from_utf8_lossy(
            &archive.read(PROPERTIES).map_err(at)?,
        ))
    } else {
        None
    };
    Ok(WordVolume {
        compilation,
        sections,
    })
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

fn witness_title(rid: &str, name: &Json, epub_dir: &str, word_dir: &str) -> Result<Witnessed, Exc> {
    let side = child(epub_dir, &[&format!("{rid}.epub.meta.json")])?;
    let meta = pyjson::loads_standard(&crate::pyio::read_text(&side)?).map_err(json_error)?;
    let number = sidecar_text(&meta, "compilationNumber", &side)?;
    let date = sidecar_text(&meta, "versionStart", &side)?;

    let volumes_dir = child(word_dir, &[rid])?;
    let entries = std::fs::read_dir(&volumes_dir).map_err(|e| {
        crate::pyos::OsError::new(&e, Some(&volumes_dir), crate::pyos::Style::Native)
    })?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| {
            crate::pyos::OsError::new(&e, Some(&volumes_dir), crate::pyos::Style::Native)
        })?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| refuse(format!("{volumes_dir}: a file name is not valid Unicode")))?;
        if name.ends_with(".docx") {
            names.push(name);
        }
    }
    names.sort();
    if names.is_empty() {
        return Err(refuse(format!("no Word volumes (*.docx) in {volumes_dir}")));
    }
    let mut word = Vec::new();
    for name in &names {
        let path = child(&volumes_dir, &[name])?;
        let volume = word_volume(&path)?;
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
        word.extend(volume.sections);
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

    fn body(paragraphs: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
             <w:body>{paragraphs}</w:body></w:document>"
        )
    }

    const NUMBER: &str = "<w:r><w:rPr><w:rStyle w:val=\"CharSectno\"/></w:rPr><w:t>";

    #[test]
    fn a_split_number_and_its_word_hyphen_join() {
        let xml = body(&format!(
            "<w:p><w:pPr><w:pStyle w:val=\"ActHead5\"/></w:pPr>\
             {NUMBER}8</w:t></w:r>\
             <w:r><w:rPr><w:rStyle w:val=\"CharSectno\"/></w:rPr><w:noBreakHyphen/><w:t>1</w:t></w:r>\
             <w:r><w:t xml:space=\"preserve\">  Who must pay</w:t></w:r></w:p>"
        ));
        assert_eq!(word_sections(&xml).unwrap(), ["8-1"]);
    }

    #[test]
    fn only_numbered_section_headings_count() {
        let xml = body(&format!(
            // A table of sections numbers its entries with the same run style.
            "<w:p><w:pPr><w:pStyle w:val=\"TofSectsSection\"/></w:pPr>{NUMBER}26</w:t></w:r></w:p>\
             <w:p><w:pPr><w:pStyle w:val=\"ActHead5\"/></w:pPr><w:r><w:t>Unnumbered</w:t></w:r></w:p>\
             <w:p><w:pPr><w:pStyle w:val=\"ActHead5\"/></w:pPr>{NUMBER}5B</w:t></w:r></w:p>"
        ));
        assert_eq!(word_sections(&xml).unwrap(), ["5B"]);
    }

    #[test]
    fn a_paragraph_mark_style_and_a_tracked_change_do_not_number() {
        let xml = body(&format!(
            "<w:p><w:pPr><w:pStyle w:val=\"ActHead5\"/>\
             <w:rPr><w:rStyle w:val=\"CharSectno\"/></w:rPr>\
             <w:pPrChange><w:pPr><w:pStyle w:val=\"subsection\"/></w:pPr></w:pPrChange></w:pPr>\
             <w:r><w:t>Heading text</w:t></w:r>{NUMBER}7</w:t></w:r></w:p>"
        ));
        assert_eq!(word_sections(&xml).unwrap(), ["7"]);
    }

    #[test]
    fn a_text_box_paragraph_keeps_its_own_runs() {
        let xml = body(&format!(
            "<w:p><w:pPr><w:pStyle w:val=\"ActHead5\"/></w:pPr>{NUMBER}3</w:t>\
             <w:txbxContent><w:p><w:pPr><w:pStyle w:val=\"Tabletext\"/></w:pPr>\
             <w:r><w:t>boxed</w:t></w:r></w:p></w:txbxContent></w:r></w:p>"
        ));
        assert_eq!(word_sections(&xml).unwrap(), ["3"]);
    }

    #[test]
    fn unbalanced_markup_is_refused() {
        assert!(word_sections(&body("<w:p><w:r><w:t>1</w:t></w:p>")).is_err());
        assert!(word_sections(&body("</w:p>")).is_err());
        assert!(word_sections(&body("<w:p><w:r>")).is_err());
    }

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
