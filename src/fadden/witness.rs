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
use super::{Exc, Run, epub, extract, json_error};
use crate::argparse::{Action, Exit, Get, Parser};
use crate::htmlparser::{self, Event, Positioned};
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

fn is_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\x0c' | ' ')
}

/// The tokenizer gives tag and attribute names in lower case, but XML names
/// are case-sensitive, so the readers below read each name back from the
/// source as written and match only the exact spelling. A name read back must
/// lower-case to the tokenizer's, or the part is refused. Events must be read
/// in the order the tokenizer gives them.
struct Spelling<'a> {
    xml: &'a str,
    line: usize,
    offset: usize,
    byte: usize,
}

impl<'a> Spelling<'a> {
    fn new(xml: &'a str) -> Self {
        Self {
            xml,
            line: 1,
            offset: 0,
            byte: 0,
        }
    }

    /// The source from the event's position, which for a tag is its `<`. The
    /// cursor only moves forward, as a Word part is often one long line.
    fn at(&mut self, at: &Positioned) -> &'a str {
        while self.line < at.line {
            let Some(newline) = self.xml[self.byte..].find('\n') else {
                break;
            };
            (self.byte, self.line, self.offset) = (self.byte + newline + 1, self.line + 1, 0);
        }
        let rest = &self.xml[self.byte..];
        let step = at.offset.saturating_sub(self.offset);
        self.byte += rest.char_indices().nth(step).map_or(rest.len(), |(i, _)| i);
        self.offset = at.offset;
        &self.xml[self.byte..]
    }

    /// The tag's name as written.
    fn name(&mut self, at: &Positioned, lower: &str) -> Result<&'a str, String> {
        let tag = self.at(at);
        let tag = tag
            .strip_prefix("</")
            .or_else(|| tag.strip_prefix('<'))
            .unwrap_or_default();
        let name = &tag[..tag
            .find(|c| is_space(c) || matches!(c, '/' | '>'))
            .unwrap_or(tag.len())];
        if name.to_lowercase() == lower {
            Ok(name)
        } else {
            Err(format!("<{lower}> could not be read as written"))
        }
    }

    /// The value of the start tag's first attribute named exactly `wanted`.
    fn attribute(
        &mut self,
        at: &Positioned,
        attrs: &[(String, Option<String>)],
        wanted: &str,
    ) -> Result<Option<String>, String> {
        let tag = self.at(at);
        let mut rest = &tag[1..];
        rest = &rest[rest
            .find(|c| is_space(c) || matches!(c, '/' | '>'))
            .unwrap_or(rest.len())..];
        let mut found = None;
        for (lower, value) in attrs {
            rest = rest.trim_start_matches(|c| is_space(c) || c == '/');
            let end = rest
                .find(|c| is_space(c) || matches!(c, '/' | '=' | '>'))
                .unwrap_or(rest.len());
            let name = &rest[..end];
            if name.to_lowercase() != *lower {
                return Err(format!(
                    "the attribute {lower} could not be read as written"
                ));
            }
            if name == wanted && found.is_none() {
                found = Some(value.clone());
            }
            rest = rest[end..].trim_start_matches(is_space);
            if let Some(value) = rest.strip_prefix('=') {
                let value = value.trim_start_matches(is_space);
                rest = match value.chars().next() {
                    Some(quote @ ('"' | '\'')) => {
                        value[1..].find(quote).map_or("", |i| &value[i + 2..])
                    }
                    _ => {
                        &value[value
                            .find(|c| is_space(c) || c == '>')
                            .unwrap_or(value.len())..]
                    }
                };
            }
        }
        Ok(found.flatten())
    }
}

/// The name of a tag event as written, or `""` for any other event.
fn written<'a>(spelling: &mut Spelling<'a>, positioned: &Positioned) -> Result<&'a str, String> {
    match &positioned.event {
        Event::StartTag(tag, _) | Event::StartEndTag(tag, _) | Event::EndTag(tag) => {
            spelling.name(positioned, tag)
        }
        _ => Ok(""),
    }
}

/// Every element closes in order inside a single root, with only whitespace,
/// comments and declarations outside it; names match as written. The
/// tokenizer is not an XML parser, so the readers below check this structure
/// before trusting what it yields.
fn well_formed(xml: &str) -> Result<(), String> {
    let (mut open, mut roots, mut spelling) = (Vec::<&str>::new(), 0usize, Spelling::new(xml));
    htmlparser::parse_each(xml, true, |positioned| {
        let name = written(&mut spelling, &positioned)?;
        match &positioned.event {
            Event::StartTag(_, _) => {
                roots += usize::from(open.is_empty());
                open.push(name);
            }
            Event::StartEndTag(_, _) => roots += usize::from(open.is_empty()),
            Event::EndTag(_) => match open.pop() {
                Some(top) if top == name => {}
                Some(top) => return Err(format!("<{top}> is closed by </{name}>")),
                None => return Err(format!("</{name}> closes an element that never opened")),
            },
            Event::Data(text) if open.is_empty() && !text.trim().is_empty() => {
                return Err("text stands outside the root element".into());
            }
            _ => {}
        }
        if roots > 1 {
            return Err("more than one root element".into());
        }
        Ok(())
    })?;
    match (open.last(), roots) {
        (Some(tag), _) => Err(format!("the document ends inside <{tag}>")),
        (None, 0) => Err("no root element".into()),
        _ => Ok(()),
    }
}

/// The identifiers a Word body's `ActHead5` paragraphs number, in document
/// order: each paragraph's `CharSectno` run text, joined, with
/// `<w:noBreakHyphen/>` read as `-`. The HTML tokenizer the EPUB reader uses
/// reads this XML; only the exact OOXML spellings of names count (`Spelling`).
/// A document that is not well formed, or a paragraph or run that closes
/// without opening or is still open at the end, is refused.
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
    well_formed(xml)?;
    let (mut paras, mut runs, mut in_text, mut found) =
        (Vec::<Para>::new(), Vec::<WordRun>::new(), false, Vec::new());
    // Open w:pPrChange and w:rPrChange elements: a tracked change's earlier
    // properties, whose styles are not the current ones.
    let (mut changes, mut spelling) = (0usize, Spelling::new(xml));
    htmlparser::parse_each(xml, true, |positioned| {
        let numbering = runs
            .last()
            .is_some_and(|r| r.depth == paras.len() && r.style.as_deref() == Some(NUMBER_STYLE));
        let name = written(&mut spelling, &positioned)?;
        let mut val = |attrs: &[(String, Option<String>)]| {
            spelling
                .attribute(&positioned, attrs, "w:val")
                .map(Option::unwrap_or_default)
        };
        match &positioned.event {
            Event::StartTag(_, _) if name == "w:p" => paras.push(Para::default()),
            Event::StartTag(_, _) if name == "w:r" => runs.push(WordRun {
                depth: paras.len(),
                style: None,
            }),
            Event::StartTag(_, _) if name == "w:t" => in_text = true,
            Event::StartTag(_, _) if name == "w:pPrChange" || name == "w:rPrChange" => {
                changes += 1;
            }
            Event::EndTag(_) if name == "w:pPrChange" || name == "w:rPrChange" => {
                changes -= 1;
            }
            Event::StartTag(_, attrs) | Event::StartEndTag(_, attrs) => match name {
                "w:pStyle" if changes == 0 => {
                    if let Some(para) = paras.last_mut().filter(|p| p.style.is_none()) {
                        para.style = Some(val(attrs)?);
                    }
                }
                // A paragraph mark's run properties sit outside any run of it.
                "w:rStyle" if changes == 0 => {
                    let depth = paras.len();
                    if let Some(run) = runs
                        .last_mut()
                        .filter(|r| r.depth == depth && r.style.is_none())
                    {
                        run.style = Some(val(attrs)?);
                    }
                }
                "w:noBreakHyphen" if numbering => {
                    if let Some(para) = paras.last_mut() {
                        para.number.push('-');
                    }
                }
                _ => {}
            },
            Event::EndTag(_) if name == "w:p" => {
                let para = paras.pop().ok_or("a paragraph closes that never opened")?;
                if runs.last().is_some_and(|r| r.depth > paras.len()) {
                    return Err("a paragraph closes inside one of its runs".to_string());
                }
                if para.style.as_deref() == Some(HEADING_STYLE) && !para.number.is_empty() {
                    found.push(para.number);
                }
            }
            Event::EndTag(_) if name == "w:r" => {
                runs.pop().ok_or("a run closes that never opened")?;
            }
            Event::EndTag(_) if name == "w:t" => in_text = false,
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

/// The custom document properties the Register's Word compilations carry,
/// such as `CompilationNumber` and `ShortT` (the title's name), by name. A
/// property set twice is refused.
fn properties(xml: &str) -> Result<BTreeMap<String, String>, String> {
    well_formed(xml)?;
    let (mut current, mut found) = (None::<(String, String)>, BTreeMap::new());
    let mut spelling = Spelling::new(xml);
    htmlparser::parse_each(xml, true, |positioned| {
        let tag = written(&mut spelling, &positioned)?;
        match &positioned.event {
            Event::StartTag(_, attrs) if tag == "property" => {
                let name = spelling.attribute(&positioned, attrs, "name")?;
                current = Some((name.unwrap_or_default(), String::new()));
            }
            Event::Data(text) => {
                if let Some((_, value)) = current.as_mut() {
                    value.push_str(text);
                }
            }
            Event::EndTag(_) if tag == "property" => {
                if let Some((name, value)) = current.take()
                    && found
                        .insert(name.clone(), value.trim().to_string())
                        .is_some()
                {
                    return Err(format!("the property {name} is set twice"));
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(found)
}

/// One Word volume: its `CompilationNumber` and `ShortT` properties and the
/// section identifiers its body numbers. The archive passes the bounded
/// reader the EPUBs pass; both parts must be UTF-8 and well formed.
pub struct WordVolume {
    pub compilation: Option<String>,
    pub title: Option<String>,
    pub sections: Vec<String>,
}

pub fn word_volume(path: &str) -> Result<WordVolume, Exc> {
    let at = |e: Exc| refuse(format!("{path}: {e}"));
    let archive = epub::open(path).map_err(at)?;
    let text = |part: &str| {
        String::from_utf8(archive.read(part).map_err(at)?)
            .map_err(|_| refuse(format!("{path}: {part} is not UTF-8")))
    };
    if !archive.namelist().any(|n| n == DOCUMENT) {
        return Err(refuse(format!(
            "{path}: no {DOCUMENT}; not a Word document"
        )));
    }
    let sections =
        word_sections(&text(DOCUMENT)?).map_err(|e| refuse(format!("{path}: {DOCUMENT}: {e}")))?;
    let mut properties = if archive.namelist().any(|n| n == PROPERTIES) {
        properties(&text(PROPERTIES)?).map_err(|e| refuse(format!("{path}: {PROPERTIES}: {e}")))?
    } else {
        BTreeMap::new()
    };
    Ok(WordVolume {
        compilation: properties.remove("CompilationNumber"),
        title: properties.remove("ShortT"),
        sections,
    })
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
    let listing = pyjson::loads_standard(&crate::pyio::read_text(path)?).map_err(json_error)?;
    if listing.get("@odata.nextLink").is_some() {
        return Err(refuse(format!(
            "{path} is one page of a longer listing (it has @odata.nextLink); filter it to the compilation's start date so it fits on one page"
        )));
    }
    let Some(Json::Array(rows)) = listing.get("value") else {
        return Err(refuse(format!(
            "{path}: not a documents listing (no \"value\" list)"
        )));
    };
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
    for row in rows {
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
            continue;
        }
        match listed(row, "format").as_deref() {
            Some("Epub") => epubs += 1,
            Some("Word") => {
                let volume = listed(row, "volumeNumber")
                    .and_then(|v| v.parse::<u32>().ok())
                    .ok_or_else(|| refuse(format!("{path}: a Word row has no volumeNumber")))?;
                if volumes.contains(&volume) {
                    return Err(refuse(format!("{path} lists Word volume {volume} twice")));
                }
                volumes.push(volume);
            }
            _ => {}
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
    // The volumes on disk must be exactly those the Register lists.
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
    // load_sample admits only a string name.
    let sampled = name.as_str().unwrap_or_default();
    let mut word = Vec::new();
    for file in &names {
        let path = child(&volumes_dir, &[file])?;
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
        if volume.title.as_deref() != Some(sampled) {
            return Err(refuse(format!(
                "{path} is titled {}; the sample names {rid} {sampled}",
                volume
                    .title
                    .as_deref()
                    .unwrap_or("nothing (no ShortT property)"),
            )));
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
    fn a_style_inside_a_tracked_change_is_not_current() {
        // Before the change the first paragraph was a heading and both runs
        // numbered it; now neither is, and only the last run numbers the second.
        let earlier_number = "<w:rPr><w:rPrChange><w:rPr><w:rStyle w:val=\"CharSectno\"/></w:rPr></w:rPrChange></w:rPr>";
        let xml = body(&format!(
            "<w:p><w:pPr><w:pPrChange><w:pPr><w:pStyle w:val=\"ActHead5\"/></w:pPr></w:pPrChange></w:pPr>\
             <w:r>{earlier_number}<w:t>4</w:t></w:r></w:p>\
             <w:p><w:pPr><w:pStyle w:val=\"ActHead5\"/></w:pPr>\
             <w:r>{earlier_number}<w:t>5</w:t></w:r>{NUMBER}6</w:t></w:r></w:p>"
        ));
        assert_eq!(word_sections(&xml).unwrap(), ["6"]);
    }

    #[test]
    fn markup_that_is_not_well_formed_is_refused() {
        for (xml, message) in [
            ("<a><b></a></b>", "<b> is closed by </a>"),
            ("<a>", "the document ends inside <a>"),
            ("<a/><b/>", "more than one root element"),
            ("text<a/>", "text stands outside the root element"),
            ("<?xml version=\"1.0\"?>", "no root element"),
            ("</a>", "</a> closes an element that never opened"),
            // XML names are case-sensitive.
            (
                "<w:p><w:pStyle w:val=\"ActHead5\"></w:pstyle></w:p>",
                "<w:pStyle> is closed by </w:pstyle>",
            ),
            ("<a></A>", "<a> is closed by </A>"),
        ] {
            assert_eq!(well_formed(xml).unwrap_err(), message, "{xml}");
        }
        assert!(well_formed("<?xml version=\"1.0\"?>\n<a><b/>x</a>\n").is_ok());
        assert!(word_sections("<w:document><w:body></w:document>").is_err());
    }

    #[test]
    fn only_the_exact_spellings_of_names_count() {
        let heading = |p_style: &str, r_style: &str| {
            body(&format!(
                "<w:p><w:pPr>{p_style}</w:pPr><w:r><w:rPr>{r_style}</w:rPr><w:t>9</w:t></w:r></w:p>"
            ))
        };
        let p_style = "<w:pStyle w:val=\"ActHead5\"/>";
        let r_style = "<w:rStyle w:val=\"CharSectno\"/>";
        assert_eq!(word_sections(&heading(p_style, r_style)).unwrap(), ["9"]);
        for (p, r) in [
            ("<w:pstyle w:val=\"ActHead5\"/>", r_style),
            ("<w:PStyle w:val=\"ActHead5\"/>", r_style),
            ("<w:pStyle W:val=\"ActHead5\"/>", r_style),
            ("<w:pStyle w:Val='ActHead5'/>", r_style),
            (p_style, "<w:rstyle w:val=\"CharSectno\"/>"),
            (p_style, "<w:rStyle w:VAL=\"CharSectno\"/>"),
        ] {
            let xml = heading(p, r);
            assert!(word_sections(&xml).unwrap().is_empty(), "{xml}");
        }
        // A miscased paragraph or text element is another element.
        let xml = body(&format!(
            "<w:P><w:pPr>{p_style}</w:pPr><w:r><w:rPr>{r_style}</w:rPr><w:t>9</w:t></w:r></w:P>\
             <w:p><w:pPr>{p_style}</w:pPr><w:r><w:rPr>{r_style}</w:rPr><w:T>9</w:T></w:r></w:p>"
        ));
        assert!(word_sections(&xml).unwrap().is_empty());
        // Names after multi-byte characters, on a later line and after an
        // attribute value holding `>` and `/` are still read as written.
        let xml = body(&format!(
            "<w:p><w:pPr>{p_style}</w:pPr><w:r><w:t>\u{2011}\u{e9}</w:t></w:r>\n\
             <w:r><w:rPr><w:rStyle w:x=\"a>b/\" w:val=\"CharSectno\"/></w:rPr><w:t>9</w:t></w:r></w:p>"
        ));
        assert_eq!(word_sections(&xml).unwrap(), ["9"]);
        let props = "<Properties><property NAME=\"ShortT\">A</property>\
                     <Property name=\"CompilationNumber\">1</Property></Properties>";
        let found = properties(props).unwrap();
        assert!(
            !found.contains_key("ShortT") && !found.contains_key("CompilationNumber"),
            "{found:?}"
        );
    }

    #[test]
    fn properties_are_read_by_name_and_set_once() {
        let xml = "<Properties><property name=\"ShortT\"><vt:lpwstr>A &amp; B Act</vt:lpwstr></property>\
                   <property name=\"CompilationNumber\"><vt:lpwstr> 32 </vt:lpwstr></property></Properties>";
        let found = properties(xml).unwrap();
        assert_eq!(
            (
                found["ShortT"].as_str(),
                found["CompilationNumber"].as_str()
            ),
            ("A & B Act", "32")
        );
        let twice = "<Properties><property name=\"ShortT\">A</property>\
                     <property name=\"ShortT\">B</property></Properties>";
        assert_eq!(
            properties(twice).unwrap_err(),
            "the property ShortT is set twice"
        );
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
