// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `fadden/search.py`: build and query a local search index over a finished corpus.
//!
//! `build` checks every row of a finished corpus against `sources.json` before
//! anything is written, then writes one SQLite file: a full-text table (FTS5),
//! the SHA-256 of every `sections.jsonl` read, the defined terms found in
//! definitions rows and the section references found in row text. `query`
//! ranks rows by BM25, `define` looks a term up among definitions paragraphs
//! and `refs` lists a row's citations both ways. All of it is a finding aid
//! over the EPUB reading view, not the authorised text.
//!
//! The index is built with the transaction pattern Python's `sqlite3` used:
//! the schema script in autocommit mode, then every insert in one transaction.
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OpenFlags, params_from_iter};
use unicode_normalization::UnicodeNormalization;

use super::as_at;
use super::corpus_paths::{child, register_id, register_id_str};
use super::quotes::{self, Quotation};
use super::rulings::canonical_docid;
use super::state::{self, Source};
use super::{Exc, Run, sha256_hex as hex};
use crate::argparse::{Action, Exit, Get, Parser};
use crate::pyjson::{self, Json, PyStr};
use crate::pyos::{OsError, Style};
use crate::pypath::PyPath;
use crate::pyre::{self, S as DOTALL};
use crate::pyvalue::{self, dumps_line, py_eq, repr, str_for_stderr, type_name};

const SCHEMA: &str = "4";
pub const STEM_NOTICE: &str = "Word-form matches are not quotation matches.";

#[derive(Clone, Copy)]
enum SearchMode {
    Exact,
    Stemmed,
    /// Every word in some form, the query's own words favoured.
    WordForms,
}

impl SearchMode {
    /// Both identifiers are fixed here; query words remain bound parameters.
    fn sql(self, exact: &'static str) -> Cow<'static, str> {
        match self {
            Self::Exact => Cow::Borrowed(exact),
            Self::Stemmed | Self::WordForms => Cow::Owned(exact.replace("rows_fts", "rows_stem")),
        }
    }

    /// `QUERY_SQL` in this mode. Word forms match in the Porter table and add each
    /// row's exact BM25, so a row holding the query's own words is favoured;
    /// `word_form` is 1 for a row only a word form matched. Takes the match twice.
    fn query_sql(self) -> Cow<'static, str> {
        match self {
            Self::WordForms => Cow::Owned(format!(
                "WITH e AS (SELECT rowid AS id, bm25(rows_fts, 4.0, 1.0) AS score FROM rows_fts \
                 WHERE rows_fts MATCH ?) {}",
                QUERY_SQL
                    .replace("AS snippet", "AS snippet, e.id IS NULL AS word_form")
                    .replace(
                        "bm25(rows_fts, 4.0, 1.0)",
                        "bm25(rows_stem, 4.0, 1.0) + coalesce(e.score, 0)"
                    )
                    .replace(
                        "rows_fts.rowid",
                        "rows_fts.rowid LEFT JOIN e ON e.id = r.id"
                    )
                    .replace("rows_fts", "rows_stem")
            )),
            _ => self.sql(QUERY_SQL),
        }
    }

    fn mark(self, hit: Json, word_form: bool) -> Json {
        match self {
            Self::Exact => hit,
            Self::WordForms if !word_form => hit,
            Self::Stemmed | Self::WordForms => with_field(
                with_field(hit, "stemmed", Json::Bool(true)),
                "stem_boundary",
                Json::str(STEM_NOTICE),
            ),
        }
    }
}
pub const NOTICE: &str = "Finding aid derived from the Register's EPUB reading view, not authorised \
legislation or tax advice. Read the authorised compilation on the Register before relying on a row.";

const DEFINITIONS_HEADING: &str = r"(?i)\b(?:definitions?|dictionary|interpretation)\b";
const LEAD_IN: &str =
    r"In (?:this|these) [^:,\n]{1,60}(?:, (?:unless|except) [^:,\n]{1,60})?[,:]\s+";
const VERB: &str = r"(?P<term>[^\n]{1,150}?)\s+(?:means|has the same meaning as|has (?:the|a) meanings?|includes|does not include)\b";
const COLON: &str = r"(?P<term>[^:\n(]{1,80}):\s+\S";
/// Dictionary entries without a defining verb, read only where the paragraph before
/// ends an entry: `IRU is an ...`, and a term alone before its paragraphs, as in
/// `head company:` in ITAA 1997 s 995-1 or `GST inclusive market value of:`.
const IS: &str = r"(?P<term>[^:\n]{1,150}?)\s+(?:is|are)\b";
const BARE: &str = r"(?P<term>[^:\n(]{1,80}?)(?:,?\s+(?:of|on))?:\Z";
/// A paragraph ending like this runs on into the next one.
const RUNS_ON: &str = r"(?:[;,]|\b(?:and|or))\Z";
/// Opening words of a paragraph that continues an entry rather than starts one; not
/// the start of a hyphenated word such as `in-house`.
const CONTINUES: &str = r"(?i)(?:a|an|the|that|this|those|then|if|where|whether|until|being|less|plus|means|subsection|at|is|are|to|for|both|within|in|except|unless|reduced|exceeds)\b(?![‐‑-])";
const QUALIFIER: &str = r",\s+(?:in relation to|in respect of|for the purposes of)\b.*\Z";
const NOT_A_TERM: &str =
    r"(?i)(?:note|example|however|for example|in this|but|and|or|other than)\b";
const MAX_TERM_WORDS: usize = 30;

const SECTION_RE: &str = r"\d+[A-Z]{0,4}(?:[-‐‑]\d+[A-Z]{0,4})?";
const YEAR: &str = r"(?:18|19|20)\d{2}\b";
const LABEL: &str = r"\d+[A-Z]{0,4}(?:[-‐‑](?:\d+[A-Z]{0,4}|[A-Z]{1,3}\b))?(?![\w‐‑-]|\.\d)(?:\([0-9A-Za-z]{1,6}\))*";
const LIST_SEPARATOR: &str = r"(?:,\s+(?:(?:and|or)\s+)?|\s+(?:and|or)\s+)";
const SUBDIVISION: &str = r"\d+[A-Z]{0,4}[-‐‑][A-Z]{1,3}\b";
const MARKDOWN_HEADING: &str = r"^#{1,6} (.+)$";

/// Cited names that stand for a title's exact name, each checked against the
/// Register on 4 November 2026. A cited name resolves only as a title's name or
/// through this list, so any other abbreviation stays unresolved.
const ABBREVIATIONS: [(&str, &str); 5] = [
    ("ITAA 1997", "Income Tax Assessment Act 1997"),
    ("ITAA 1936", "Income Tax Assessment Act 1936"),
    ("TAA 1953", "Taxation Administration Act 1953"),
    (
        "GST Act",
        "A New Tax System (Goods and Services Tax) Act 1999",
    ),
    ("FBTAA 1986", "Fringe Benefits Tax Assessment Act 1986"),
];

fn cited() -> String {
    format!(
        r"(?:the\s+)?\*?([A-Z][A-Za-z]*(?:\s+[A-Z][A-Za-z]*){{0,3}}\s+{YEAR}|(?=[A-Z])[^.;:,\n]{{0,150}}?\b(?:Act|Regulations?|Rules)\b(?:\s+{YEAR})?)"
    )
}

/// Lower-case words that name a source, in a sentence or straight after a title's name.
const SOURCE_WORDS: &str = "bills?|regs?|regulations?|rules?|ordinances?|determinations?|instruments?|orders?|codes?|schedules?|acts?|laws?|legislation|statutes?|by-?laws?|rulings?|enactments?|treaty|treaties";

/// A lower-case word other than `and`, `or` or a source word, or a provision word: either
/// may follow a name without carrying it on.
fn name_ending_word() -> String {
    format!(
        r"(?!(?:and|or|{SOURCE_WORDS})\b)[a-z]|(?:Division|Subdivision|Part|Schedule|Chapter)\b"
    )
}

/// Whether an abbreviation ending at `end` ends its name there, rather than begin a
/// longer one such as `ITAA 1997 Amendment Act 2025`. After any whitespace, line breaks
/// included, only these may follow, past any closing `*`: the end of the text; `,` `.` `;`
/// `:` `)` or `]`; a
/// `name_ending_word`; a parenthesised part, then any of these; or `and` or `or`
/// (optionally `the`) before a `name_ending_word` or before a listed abbreviation or
/// held title that itself ends its name. Anything else, such as a capitalised word, a
/// digit or the `-2` of `ITAA 1997-2`, does not.
fn ends_name(text: &str, mut end: usize, lookup: impl Fn(&str) -> Option<String>) -> bool {
    let word_pattern = name_ending_word();
    let ending = pyre::re(&format!(r"\s*(?:\Z|[,.;:)\]])|\s+(?:{word_pattern})"), 0);
    let part = pyre::re(r"\s*\((?:[^()\n]|\([^()\n]*\))*\)", 0);
    let join = pyre::re(r"\s+(?:and|or)(?:\s+the)?\s+", 0);
    let word = pyre::re(&word_pattern, 0);
    let name = pyre::re(&cited(), 0);
    loop {
        end += text[end..].len() - text[end..].trim_start_matches('*').len();
        if ending.match_at(text, end).is_some() {
            return true;
        }
        if let Some(found) = part.match_at(text, end) {
            end = found.get(0).expect("group 0").end();
            continue;
        }
        let Some(found) = join.match_at(text, end) else {
            return false;
        };
        let after = found.get(0).expect("group 0").end();
        if word.match_at(text, after).is_some() {
            return true;
        }
        let Some(next) = name.match_at(text, after).and_then(|found| found.get(1)) else {
            return false;
        };
        let written = crate::py::split_whitespace(next.as_str()).join(" ");
        if !ABBREVIATIONS
            .iter()
            .any(|(abbreviation, _)| *abbreviation == written)
            && lookup(&written).is_none()
        {
            return false;
        }
        end = next.end();
    }
}

/// The title a citation names: `own` for a bare citation or one `of this Act`,
/// otherwise by exact name or through a listed abbreviation that ends its name.
fn cited_title(
    text: &str,
    end: usize,
    act: Option<&str>,
    own: Option<&str>,
    lookup: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let Some(written) = act else {
        return own.map(str::to_string);
    };
    match ABBREVIATIONS
        .iter()
        .find(|(abbreviation, _)| *abbreviation == written)
    {
        // A blocked abbreviation stays unresolved rather than reach a wrong title.
        Some(_) if !ends_name(text, end, &lookup) => None,
        Some((_, name)) => lookup(name),
        None => lookup(written),
    }
}

const KEYWORD: &str = r"(?i:(?:sub)?(?:section|paragraph|subparagraph)|subdivision|division)";
/// A subsection or paragraph named after its section, as `(4)` in `6(1) and (4)`.
const PART: &str = r"\([0-9A-Za-z]{1,6}\)(?:\([0-9A-Za-z]{1,6}\))*";

/// `of` or `in` and the title cited; its 2 groups hold a cited name.
fn in_title() -> String {
    let c = cited();
    format!(
        r"\s+(?:of|in)\s+(?:Schedule\s+\d+[A-Z]?\s+to\s+{c}|this\s+\w+|Schedule\s+\d+[A-Z]?\b|{c})"
    )
}

fn citation_pattern() -> String {
    format!(
        r"\b(?P<keyword>{KEYWORD})s?\s+(?P<labels>{LABEL}(?:{LIST_SEPARATOR}{LABEL})*)(?:{})?",
        in_title()
    )
}

/// The rest of a list that a citation stops short of, through the title named at its
/// end: ` and (4) and 6BA(5) and section 94L of the ITAA 1936` after `subsections
/// 6(1)`. `rest` holds the items before any that has its own keyword; such an item is
/// read as a citation of its own, and `more` holds those items. Such an item joins the
/// list only through `and` or `or` with no comma before it, which would mark a new
/// clause (`applies to section 6, and section 7 of the X Act applies`).
fn list_rest_pattern() -> String {
    let item = format!(r"(?:{LABEL}|{PART})");
    format!(
        r"(?P<rest>(?:{LIST_SEPARATOR}{item})*)(?P<more>(?:\s+(?:and|or)\s+{KEYWORD}s?\s+{LABEL}(?:{LIST_SEPARATOR}{item})*)*){}",
        in_title()
    )
}

/// A verb straight after a list's title that a plural subject would not take, so the
/// last item, not the list, is its subject: in `It applies to section 6 and section 7
/// of the X Act applies`, `section 7 of the X Act` starts a clause of its own. A plural
/// verb (`do not apply`) keeps the list whole. A modal verb, or a singular verb after
/// `or`, does not show which, so the earlier items keep their own title then too.
const CLAUSE_VERB: &str = r"\s+(?:(?:also|not|only|still)\s+)?(?:applies|does|is|has|was|provides|requires|prevails|operates|ceases|continues|extends|includes|excludes|affects|may|must|will|would|can|cannot|could|should|shall)\b";

fn level_pattern() -> String {
    format!(
        r"(Chapter|Part|Schedule|Division|Subdivision)\b(?:\s+({SUBDIVISION}|{SECTION_RE})(?![.\w]))?"
    )
}

/// `unicodedata.normalize("NFKC", text)`.
pub fn normalise(text: &str) -> String {
    text.nfkc().collect()
}

/// `normalise(value)` for a JSON value, or the TypeError a non-str raises.
fn normalise_value(value: &Json) -> Result<String, Exc> {
    match value {
        Json::Str(PyStr::Text(s)) => Ok(normalise(s)),
        // A lone surrogate passes through NFKC; the value fails later, at binding.
        Json::Str(PyStr::Lone(points)) => Ok(pyvalue::points_lossy(points)),
        other => Err(Exc::Type(format!(
            "normalize() argument 2 must be str, not {}",
            type_name(other)
        ))),
    }
}

fn hyphens(text: &str) -> String {
    text.replace(['\u{2010}', '\u{2011}'], "-")
}

/// Lookup key for a defined term: folded, unstarred, single-spaced, plain hyphens.
pub fn term_key(term: &str) -> String {
    let folded = crate::py::casefold(&hyphens(&normalise(term)).replace('*', ""));
    crate::py::split_whitespace(&folded).join(" ")
}

pub fn section_key(section: &str) -> String {
    hyphens(&normalise(section)).replace(' ', "")
}

/// `value or ""` for a value later normalised.
fn or_empty(value: &Json) -> Json {
    if value.truthy() {
        value.clone()
    } else {
        Json::str("")
    }
}

fn definition_start(paragraph: &str, entry_ended: bool) -> Option<String> {
    let lead_in = pyre::re(LEAD_IN, 0);
    let paragraph = match lead_in.search(paragraph) {
        Some(m) => {
            let m = m.get(0).expect("group 0");
            format!("{}{}", &paragraph[..m.start()], &paragraph[m.end()..])
        }
        None => paragraph.to_string(),
    };
    let verb = pyre::re(VERB, 0)
        .match_at(&paragraph, 0)
        .or_else(|| pyre::re(COLON, 0).match_at(&paragraph, 0));
    let unmarked = verb.is_none();
    let found = verb.or_else(|| {
        entry_ended
            .then(|| {
                pyre::re(IS, 0)
                    .match_at(&paragraph, 0)
                    .or_else(|| pyre::re(BARE, 0).match_at(&paragraph, 0))
            })
            .flatten()
    })?;
    let term = found.name("term").expect("term group").as_str();
    let term = pyre::re(QUALIFIER, DOTALL).sub("", term).replace('*', "");
    let term = term
        .trim_matches(|c| " \t\"'\u{2018}\u{2019}\u{201c}\u{201d}".contains(c))
        .to_string();
    if term.is_empty()
        || pyre::re(NOT_A_TERM, 0).match_at(&term, 0).is_some()
        || (unmarked && pyre::re(CONTINUES, 0).match_at(&term, 0).is_some())
        || crate::py::split_whitespace(&term).len() > MAX_TERM_WORDS
    {
        return None;
    }
    Some(term)
}

/// Split a definitions row into (term, definition text) pairs.
pub fn definitions(text: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, Vec<String>)> = Vec::new();
    let mut current: Option<usize> = None;
    let mut previous: Option<&str> = None;
    for paragraph in text.split("\n\n") {
        let paragraph = crate::py::strip(paragraph);
        if paragraph.is_empty() {
            continue;
        }
        let entry_ended = previous.is_some_and(|p| pyre::re(RUNS_ON, 0).search(p).is_none());
        previous = Some(paragraph);
        if paragraph.starts_with("- ") || paragraph.starts_with('>') {
            if let Some(index) = current {
                found[index].1.push(paragraph.to_string());
            }
            continue;
        }
        if paragraph.starts_with('(') {
            current = None;
            continue;
        }
        match definition_start(paragraph, entry_ended) {
            Some(term) => {
                found.push((term, vec![paragraph.to_string()]));
                current = Some(found.len() - 1);
            }
            None => {
                if let Some(index) = current {
                    found[index].1.push(paragraph.to_string());
                }
            }
        }
    }
    found
        .into_iter()
        .map(|(term, body)| (term, body.join("\n\n")))
        .collect()
}

/// Whether a cited label's section number holds a hyphen, as `8-1(2)` does.
fn hyphenated_label(label: &str) -> bool {
    section_key(label.split('(').next().unwrap_or("")).contains('-')
}

/// The cited name in the first of `groups` that holds one, single-spaced.
fn cited_in(found: &pyre::Caps<'_>, groups: [usize; 2]) -> Option<String> {
    groups
        .into_iter()
        .find_map(|g| found.get(g).map(|m| m.as_str()).filter(|s| !s.is_empty()))
        .map(|a| crate::py::split_whitespace(a).join(" "))
}

/// (kind, key, cited act or None, citation text, where the citation ends in `text`)
/// for each reference in `text`.
pub fn citations(text: &str) -> Vec<(String, String, Option<String>, String, usize)> {
    let pattern = pyre::re(&citation_pattern(), 0);
    let list_rest = pyre::re(&list_rest_pattern(), 0);
    let keyed_label = pyre::re(&format!(r"{KEYWORD}s?\s+({LABEL})"), 0);
    let separator = pyre::re(LIST_SEPARATOR, 0);
    let clause_verb = pyre::re(CLAUSE_VERB, 0);
    let mut out = Vec::new();
    for found in pattern.captures_iter(text) {
        let keyword = crate::py::casefold(found.name("keyword").expect("keyword").as_str());
        let kind = if keyword == "division" || keyword == "subdivision" {
            keyword.clone()
        } else {
            "section".to_string()
        };
        let mut act = cited_in(&found, [3, 4]);
        let whole = found.get(0).expect("group 0");
        let labels = found.name("labels").expect("labels");
        let mut items = separator.split(labels.as_str());
        let mut end = whole.end();
        // A title named only after the whole list binds every item in it, unless an
        // item is numbered unlike the first (`section 23AI or section 768-5 of the
        // ITAA 1997`), which marks items of different titles.
        if labels.end() == end
            && let Some(rest) = list_rest.match_at(text, end)
        {
            let bare: Vec<&str> = separator
                .split(rest.name("rest").expect("rest").as_str())
                .into_iter()
                .filter(|item| !item.is_empty() && !item.starts_with('('))
                .collect();
            let more = rest.name("more").expect("more").as_str();
            let keyed: Vec<String> = keyed_label
                .captures_iter(more)
                .filter_map(|found| found.get(1).map(|m| m.as_str().to_string()))
                .collect();
            let first = hyphenated_label(items[0]);
            let rest_end = rest.get(0).expect("group 0").end();
            if bare
                .iter()
                .copied()
                .chain(keyed.iter().map(String::as_str))
                .all(|label| hyphenated_label(label) == first)
                && (keyed.is_empty() || clause_verb.match_at(text, rest_end).is_none())
            {
                act = cited_in(&rest, [3, 4]);
                end = rest_end;
                items.extend(bare);
            }
        }
        let citation = crate::py::split_whitespace(&text[whole.start()..end]).join(" ");
        let mut hyphenated: Option<bool> = None;
        for label in items {
            let key = section_key(label.split('(').next().unwrap_or(""));
            let has_hyphen = key.contains('-');
            match hyphenated {
                None => hyphenated = Some(has_hyphen),
                Some(first) if first != has_hyphen => break,
                Some(_) => {}
            }
            let fits = if pyre::fullmatch(SUBDIVISION, 0, &key) {
                kind == "subdivision" && !label.contains('(')
            } else {
                kind == "section"
                    || (kind == "division" && !format!("{key}{label}").contains(['-', '(']))
            };
            if fits {
                out.push((kind.clone(), key, act.clone(), citation.clone(), end));
            }
        }
    }
    out
}

pub type Place = (Option<String>, Option<String>);

/// The (Division, Subdivision) each row of one title sits in, as far as the
/// Python generator got: its places, and the error that stopped it at the
/// next row, if any.
pub fn structure(rows: &[Json], markdown: &str) -> (Vec<Place>, Option<Exc>) {
    let level = pyre::re(&level_pattern(), 0);
    let mut places: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    let (mut division, mut subdivision): (Option<String>, Option<String>) = (None, None);
    for text in pyre::re(MARKDOWN_HEADING, pyre::M).findall(markdown) {
        let heading = crate::py::split_whitespace(&normalise(&text)).join(" ");
        if let Some(found) = level.match_at(&heading, 0) {
            let label = found.get(2).map(|m| section_key(m.as_str()));
            match found.get(1).map(|m| m.as_str()) {
                Some("Division") => {
                    division = label;
                    subdivision = None;
                }
                Some("Subdivision") => {
                    subdivision = label.filter(|l| pyre::fullmatch(SUBDIVISION, 0, l));
                }
                _ => {
                    division = None;
                    subdivision = None;
                }
            }
        }
        places.push((heading, division.clone(), subdivision.clone()));
    }
    let mut position = 0;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let heading = match normalise_value(&or_empty(row.get("heading").unwrap_or(&Json::Null))) {
            Ok(heading) => heading,
            Err(error) => return (out, Some(error)),
        };
        let heading = match row.get("heading") {
            Some(Json::Str(PyStr::Lone(_))) => None,
            _ => Some(crate::py::split_whitespace(&heading).join(" ")),
        };
        let found = heading.and_then(|h| (position..places.len()).find(|&i| places[i].0 == h));
        match found {
            Some(index) => {
                position = index + 1;
                out.push((places[index].1.clone(), places[index].2.clone()));
            }
            None => out.push((None, None)),
        }
    }
    (out, None)
}

/// A SearchIndexError: the corpus or the index failed a check.
fn refused(message: impl Into<String>) -> Exc {
    Exc::Value {
        class: "fadden.search.SearchIndexError",
        message: message.into(),
    }
}

fn sqlite_error(error: &rusqlite::Error) -> Exc {
    let message = match error {
        rusqlite::Error::SqliteFailure(_, Some(message)) => message.clone(),
        rusqlite::Error::SqliteFailure(code, None) => code.to_string(),
        other => other.to_string(),
    };
    Exc::Sqlite(message)
}

fn strerror(error: &OsError) -> String {
    error.strerror.clone()
}

fn read_bytes(root: &str, rid: &str, name: &str) -> Result<Vec<u8>, Exc> {
    let path = child(root, &["markdown", rid, name])?;
    let file = crate::pyio::open(&path)
        .map_err(|e| refused(format!("{rid}: cannot read {name} ({})", strerror(&e))))?;
    read_all(file, &path)
        .map_err(|e| refused(format!("{rid}: cannot read {name} ({})", strerror(&e))))
}

fn read_all(mut file: std::fs::File, path: &str) -> Result<Vec<u8>, OsError> {
    use std::io::Read;
    let mut data = Vec::new();
    file.read_to_end(&mut data)
        .map_err(|e| OsError::new(&e, Some(path), Style::Errno))?;
    Ok(data)
}

pub struct Title {
    title: Json,
    rid: String,
    rows: Vec<Json>,
    markdown: String,
    sections_sha256: String,
    markdown_sha256: String,
}

fn get<'a>(value: &'a Json, key: &str) -> &'a Json {
    value.get(key).unwrap_or(&Json::Null)
}

/// A title id as `source` spells them: Federal Register ids when it is None.
fn title_id(source: Option<&Source>, value: &Json) -> Result<String, Exc> {
    match source {
        None => register_id(value),
        Some(_) => state::title_id(value),
    }
}

/// One `sections.jsonl` line as a row object with a text.
fn load_row(line: &str, place: &str) -> Result<Json, Exc> {
    let row = match pyjson::loads_standard(line) {
        Ok(row) => row,
        Err(pyjson::DecodeError::Invalid(message)) if message.starts_with("Exceeds the limit") => {
            return Err(Exc::value(message));
        }
        Err(error @ pyjson::DecodeError::Recursion(_)) => return Err(super::json_error(error)),
        Err(_) => return Err(refused(format!("{place} is not JSON"))),
    };
    if !matches!(row.get("text"), Some(Json::Str(_))) {
        return Err(refused(format!("{place} is not a sections.jsonl row")));
    }
    Ok(row)
}

/// Refuses a row whose ids, compilation or `authorised` flag disagree with
/// its title in `sources.json`.
fn check_row(row: &Json, place: &str, rid: &str, ordinal: usize, title: &Json) -> Result<(), Exc> {
    let section = get(row, "section");
    let mut expected: Vec<u32> = format!("{rid}:{ordinal:04}:")
        .chars()
        .map(u32::from)
        .collect();
    if section.truthy() {
        expected.extend(pyvalue::str_points(section));
    } else {
        expected.push(u32::from('-'));
    }
    let expected = Json::Str(PyStr::from_code_points(expected));
    if !py_eq(get(row, "register_id"), &Json::str(rid)) {
        return Err(refused(format!("{place} names another title")));
    }
    if !py_eq(get(row, "row_id"), &expected) {
        return Err(refused(format!(
            "{place} has row_id {}, expected {}",
            repr(get(row, "row_id")),
            repr(&expected)
        )));
    }
    for field in ["compilation_number", "compilation_date"] {
        if !py_eq(get(row, field), get(title, field)) {
            return Err(refused(format!(
                "{place} {field} differs from sources.json"
            )));
        }
    }
    if !matches!(row.get("authorised"), Some(Json::Bool(false))) {
        return Err(refused(format!("{place} is not marked authorised: false")));
    }
    Ok(())
}

/// `"%s" % value`: `str()`, with a lone surrogate kept for binding to refuse.
fn read_title(root: &str, title: &Json, source: Option<&Source>) -> Result<Title, Exc> {
    let rid = title_id(source, get(title, "register_id"))?;
    let data = read_bytes(root, &rid, "sections.jsonl")?;
    let markdown = read_bytes(root, &rid, &format!("{rid}.md"))?;
    let (text, markdown_text) = match (std::str::from_utf8(&data), std::str::from_utf8(&markdown)) {
        (Ok(a), Ok(b)) => (a.to_string(), b.to_string()),
        _ => {
            return Err(refused(format!(
                "{rid}: sections.jsonl or {rid}.md is not UTF-8"
            )));
        }
    };
    let mut rows: Vec<Json> = Vec::new();
    for (number, line) in crate::py::splitlines(&text)
        .into_iter()
        .enumerate()
        .map(|(i, l)| (i + 1, l))
    {
        if crate::py::strip(line).is_empty() {
            continue;
        }
        let place = format!("{rid}/sections.jsonl:{number}");
        let row = load_row(line, &place)?;
        check_row(&row, &place, &rid, rows.len() + 1, title)?;
        rows.push(row);
    }
    if !py_eq(&Json::Int(rows.len().into()), get(title, "jsonl_rows")) {
        return Err(refused(format!(
            "{rid}: sections.jsonl holds {} rows; sources.json records {}",
            rows.len(),
            str_for_stderr(get(title, "jsonl_rows"))
        )));
    }
    Ok(Title {
        title: title.clone(),
        rid,
        rows,
        markdown: markdown_text,
        sections_sha256: hex(&data),
        markdown_sha256: hex(&markdown),
    })
}

type Corpus = (Vec<u8>, Json, Vec<Title>, Option<&'static Source>);

/// The corpus at `root`. A `sources.json` without `state_register` is a
/// Commonwealth corpus (whose `source` names the Federal Register); one with it
/// must name a state register `state` knows.
fn read_corpus(root: &str) -> Result<Corpus, Exc> {
    let path = child(root, &["sources.json"])?;
    let sources_bytes = crate::pyio::open(&path)
        .and_then(|file| read_all(file, &path))
        .map_err(|e| refused(format!("cannot read sources.json ({})", strerror(&e))))?;
    let not_finished = || refused("sources.json is not a finished corpus inventory");
    let sources = match pyjson::loads_bytes(&sources_bytes) {
        Ok(sources) => sources,
        Err(Some(error @ pyjson::DecodeError::Recursion(_))) => {
            return Err(super::json_error(error));
        }
        Err(_) => return Err(not_finished()),
    };
    let source = sources
        .get("state_register")
        .map(|value| {
            value.as_str().and_then(state::source).ok_or_else(|| {
                refused(format!(
                    "sources.json names an unknown state register: {}",
                    repr(value)
                ))
            })
        })
        .transpose()?;
    let listed = match sources.get("titles") {
        Some(Json::Array(items)) if items.iter().all(|t| matches!(t, Json::Object(_))) => {
            items.clone()
        }
        _ => return Err(not_finished()),
    };
    let ids = listed
        .iter()
        .map(|t| title_id(source, get(t, "register_id")))
        .collect::<Result<BTreeSet<String>, Exc>>()?;
    super::corpus_paths::refuse_unlisted_titles(root, &ids).map_err(|e| match e {
        Exc::Runtime(message) => refused(message),
        other => other,
    })?;
    let read = listed
        .iter()
        .map(|title| read_title(root, title, source))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((sources_bytes, sources, read, source))
}

const SCHEMA_SQL: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE titles (
  register_id TEXT PRIMARY KEY, name TEXT NOT NULL, collection TEXT,
  compilation_number TEXT, compilation_date TEXT, version_is_current INTEGER NOT NULL,
  retrieved TEXT, superseded_from TEXT, register_page TEXT, source_url TEXT, rows INTEGER NOT NULL,
  sections_sha256 TEXT NOT NULL, markdown_sha256 TEXT NOT NULL);
CREATE TABLE rows (
  id INTEGER PRIMARY KEY, row_id TEXT NOT NULL UNIQUE,
  register_id TEXT NOT NULL REFERENCES titles, section TEXT, section_key TEXT,
  division_key TEXT, subdivision_key TEXT, heading TEXT, container TEXT, text TEXT NOT NULL);
CREATE INDEX rows_section ON rows (register_id, section_key);
CREATE INDEX rows_division ON rows (register_id, division_key);
CREATE INDEX rows_subdivision ON rows (register_id, subdivision_key);
CREATE VIRTUAL TABLE rows_fts USING fts5 (
  heading, text, content='rows', content_rowid='id',
  tokenize='unicode61 remove_diacritics 2');
CREATE VIRTUAL TABLE rows_stem USING fts5 (
  heading, text, content='rows', content_rowid='id',
  tokenize='porter unicode61 remove_diacritics 2');
CREATE TABLE definitions (
  term TEXT NOT NULL, term_key TEXT NOT NULL, row_id TEXT NOT NULL REFERENCES rows (row_id),
  position INTEGER NOT NULL, text TEXT NOT NULL);
CREATE INDEX definitions_key ON definitions (term_key);
CREATE TABLE refs (
  from_row_id TEXT NOT NULL REFERENCES rows (row_id), kind TEXT NOT NULL,
  to_register_id TEXT, to_key TEXT NOT NULL, cited_act TEXT, citation TEXT NOT NULL);
CREATE INDEX refs_to ON refs (to_register_id, kind, to_key);
CREATE INDEX refs_from ON refs (from_row_id);
";

/// Written only when `build` is given a rulings run, so an index without rulings is
/// byte for byte what it was. A reader checks for `ruling_refs` before using it.
const RULINGS_SQL: &str = "
CREATE TABLE ruling_runs (run INTEGER PRIMARY KEY, rulings_sha256 TEXT NOT NULL,
  licence_notice TEXT NOT NULL);
CREATE TABLE ruling_refs (
  run INTEGER NOT NULL REFERENCES ruling_runs, docid TEXT NOT NULL, title TEXT NOT NULL,
  paragraph TEXT NOT NULL, source_url TEXT NOT NULL, fetched_on TEXT NOT NULL,
  withdrawal_status TEXT NOT NULL, kind TEXT NOT NULL, to_register_id TEXT,
  to_key TEXT NOT NULL, cited_act TEXT, citation TEXT NOT NULL);
CREATE INDEX ruling_refs_to ON ruling_refs (to_register_id, kind, to_key);
";

/// The values of `licence_basis` the `rulings` stage writes.
const LICENCE_BASES: [&str; 2] = ["document-notice", "site-notice-via-dc-rights"];

/// One paragraph of a `rulings` run, as `build` keeps it.
struct RulingRow {
    docid: String,
    title: String,
    paragraph: String,
    text: String,
    source_url: String,
    source_sha256: String,
    fetched_on: String,
    withdrawal_status: String,
}

/// A `rulings` run directory: its rows, the digest of its `rulings.jsonl` and its
/// `LICENCE-NOTICE.md`, which the index keeps with the rows, and the docids its manifest
/// says the stage excluded.
struct RulingsRun {
    rows: Vec<RulingRow>,
    rulings_sha256: String,
    notice: String,
    excluded: Vec<String>,
}

fn read_run_file(dir: &str, name: &str) -> Result<Vec<u8>, Exc> {
    let path = child(dir, &[name])?;
    let unreadable = |e: OsError| refused(format!("{dir}: cannot read {name} ({})", strerror(&e)));
    read_all(crate::pyio::open(&path).map_err(unreadable)?, &path).map_err(unreadable)
}

/// Read a `rulings` run directory, refusing anything the stage could not have written.
/// The licence notice must be present and carry the ATO's reuse sentence.
fn read_rulings_run(dir: &str) -> Result<RulingsRun, Exc> {
    let notice = String::from_utf8(read_run_file(dir, "LICENCE-NOTICE.md")?)
        .ok()
        .filter(|text| text.contains(super::rulings::NOTICE))
        .ok_or_else(|| {
            refused(format!(
                "{dir}: LICENCE-NOTICE.md is not the rulings stage's"
            ))
        })?;
    let data = read_run_file(dir, "rulings.jsonl")?;
    let text = std::str::from_utf8(&data)
        .map_err(|_| refused(format!("{dir}: rulings.jsonl is not UTF-8")))?;
    let mut rows = Vec::new();
    for (number, line) in crate::py::splitlines(text).into_iter().enumerate() {
        if crate::py::strip(line).is_empty() {
            continue;
        }
        let place = format!("{dir}/rulings.jsonl:{}", number + 1);
        rows.push(ruling_row(&place, line)?);
    }
    let excluded = check_run_manifest(dir, &rows)?;
    Ok(RulingsRun {
        excluded,
        rows,
        rulings_sha256: hex(&data),
        notice,
    })
}

/// Refuse a `rulings` run directory the stage could not have written, as
/// `search build --rulings` does; `dist` publishes only a run that passes.
pub(super) fn check_rulings_run(dir: &str) -> Result<(), Exc> {
    read_rulings_run(dir).map(|_| ())
}

/// One line of `rulings.jsonl`, at `place`, as the stage writes it.
fn ruling_row(place: &str, line: &str) -> Result<RulingRow, Exc> {
    let row = pyjson::loads_standard(line).map_err(|_| refused(format!("{place} is not JSON")))?;
    let field = |key: &str| row.get(key).and_then(Json::as_str).map(str::to_string);
    let fields = [
        "docid",
        "title",
        "paragraph",
        "text",
        "source_url",
        "source_sha256",
        "fetched_on",
        "licence_basis",
        "withdrawal_status",
    ]
    .map(field);
    let [
        Some(docid),
        Some(title),
        Some(paragraph),
        Some(text),
        Some(source_url),
        Some(source_sha256),
        Some(fetched_on),
        Some(basis),
        Some(withdrawal_status),
    ] = fields
    else {
        return Err(refused(format!("{place} is not a rulings row")));
    };
    check_row_values(place, &basis, &withdrawal_status, &fetched_on)?;
    Ok(RulingRow {
        docid,
        title,
        paragraph,
        text,
        source_url,
        source_sha256,
        fetched_on,
        withdrawal_status,
    })
}

/// Refuse a row whose licence basis, withdrawal status or capture date the stage would
/// not have written.
fn check_row_values(
    place: &str,
    basis: &str,
    withdrawal_status: &str,
    fetched_on: &str,
) -> Result<(), Exc> {
    if !LICENCE_BASES.contains(&basis) {
        return Err(refused(format!(
            "{place} has no licence basis the rulings stage writes"
        )));
    }
    if !matches!(withdrawal_status, "UNKNOWN" | "NOTICE_PRESENT") {
        return Err(refused(format!(
            "{place} has no withdrawal status the rulings stage writes"
        )));
    }
    if !as_at::is_date(fetched_on) {
        return Err(refused(format!(
            "{place} has no capture date the rulings stage writes"
        )));
    }
    Ok(())
}

/// Check the run's `manifest.json` against its `rows`, refusing a manifest the stage
/// could not have written. Returns the docids it lists as excluded.
fn check_run_manifest(dir: &str, rows: &[RulingRow]) -> Result<Vec<String>, Exc> {
    let manifest = std::str::from_utf8(&read_run_file(dir, "manifest.json")?)
        .ok()
        .and_then(|text| pyjson::loads_standard(text).ok())
        .filter(|manifest| manifest.get("stage") == Some(&Json::str("rulings")));
    let member = |key: &str| manifest.as_ref().and_then(|m| m.get(key));
    let excluded = match member("excluded") {
        Some(Json::Array(entries)) => entries
            .iter()
            .map(|entry| {
                entry
                    .get("docid")
                    .and_then(Json::as_str)
                    .filter(|docid| !docid.is_empty())
                    .map(str::to_string)
            })
            .collect::<Option<Vec<_>>>(),
        _ => None,
    };
    let run_date = member("fetched_on")
        .and_then(Json::as_str)
        .filter(|date| as_at::is_date(date));
    let (Some(stated @ Json::Int(_)), Some(excluded), Some(Json::Array(documents)), Some(run_date)) =
        (member("rows"), excluded, member("documents"), run_date)
    else {
        return Err(refused(format!(
            "{dir}: manifest.json is not the rulings stage's"
        )));
    };
    check_exclusions(dir, documents, &excluded)?;
    // A short or padded rulings.jsonl would otherwise index as a whole run.
    if *stated != Json::Int(rows.len().into()) {
        return Err(refused(format!(
            "{dir}: manifest.json counts {} rows but rulings.jsonl holds {}",
            pyvalue::py_str(stated),
            rows.len()
        )));
    }
    if let Some(difference) = manifest_difference(rows, documents, run_date) {
        return Err(refused(format!("{dir}: {difference}")));
    }
    Ok(excluded)
}

/// Refuse a manifest that both lists and excludes a document, or excludes one twice:
/// the stage either captures a target or excludes it, never both.
fn check_exclusions(dir: &str, documents: &[Json], excluded: &[String]) -> Result<(), Exc> {
    let listed: BTreeSet<_> = documents
        .iter()
        .filter_map(|document| document.get("docid").and_then(Json::as_str))
        .map(canonical_docid)
        .collect();
    let mut excluded_ids = BTreeSet::new();
    for docid in excluded {
        let key = canonical_docid(docid);
        if listed.contains(&key) {
            return Err(refused(format!(
                "{dir}: manifest.json both lists and excludes {docid}"
            )));
        }
        if !excluded_ids.insert(key) {
            return Err(refused(format!(
                "{dir}: manifest.json excludes {docid} more than once"
            )));
        }
    }
    Ok(())
}

/// The first way `rows` differ from the documents a run's manifest lists, if any. The
/// stage writes each document's paragraphs together, in the manifest's order, so a row
/// count that matches can still hide a swapped or truncated document. Each row must
/// carry its document's title, source URL and digest, capture date and withdrawal
/// status. The document capture date must match the run's.
fn manifest_difference(rows: &[RulingRow], documents: &[Json], run_date: &str) -> Option<String> {
    let held: Vec<&[RulingRow]> = rows.chunk_by(|a, b| a.docid == b.docid).collect();
    let repeated = repeated_document(&held);
    if repeated.is_some() {
        return repeated;
    }
    let mut listed = Vec::new();
    for document in documents {
        let docid = document.get("docid").and_then(Json::as_str);
        let (Some(docid), Some(count @ Json::Int(_))) = (docid, document.get("paragraphs")) else {
            return Some(
                "manifest.json lists a document without a docid and paragraph count".into(),
            );
        };
        if *count == Json::Int(0.into()) {
            return Some(format!(
                "manifest.json lists {docid} without any paragraphs"
            ));
        }
        listed.push((docid, count, document));
    }
    for index in 0..held.len().max(listed.len()) {
        match (listed.get(index), held.get(index)) {
            (Some((docid, count, document)), Some(group)) => {
                let difference = document_difference(docid, count, document, group, run_date);
                if difference.is_some() {
                    return difference;
                }
            }
            (Some((docid, _, _)), None) => {
                return Some(format!(
                    "manifest.json lists {docid}, which rulings.jsonl does not hold"
                ));
            }
            (None, Some(group)) => {
                return Some(format!(
                    "rulings.jsonl holds {} after the last document manifest.json lists",
                    group[0].docid
                ));
            }
            (None, None) => {}
        }
    }
    None
}

/// The first document whose rows come in 2 groups, if any. The stage refuses a repeated
/// target, so a document's rows never do.
fn repeated_document(held: &[&[RulingRow]]) -> Option<String> {
    let mut seen = BTreeSet::new();
    for group in held {
        if !seen.insert(canonical_docid(&group[0].docid)) {
            return Some(format!(
                "rulings.jsonl holds {} in 2 places, but the stage writes a document's paragraphs together",
                group[0].docid
            ));
        }
    }
    None
}

/// The first way the `group` of rows in one document's place differs from the
/// `document` the manifest lists there with `docid` and paragraph `count`, if any.
fn document_difference(
    docid: &str,
    count: &Json,
    document: &Json,
    group: &[RulingRow],
    run_date: &str,
) -> Option<String> {
    let row_docid = &group[0].docid;
    if docid != row_docid {
        return Some(format!(
            "manifest.json lists {docid} where rulings.jsonl holds {row_docid}"
        ));
    }
    if *count != Json::Int(group.len().into()) {
        return Some(format!(
            "manifest.json counts {} paragraphs of {docid} but rulings.jsonl holds {}",
            pyvalue::py_str(count),
            group.len()
        ));
    }
    for row in group {
        let fields = [
            ("title", &row.title),
            ("source_url", &row.source_url),
            ("sha256", &row.source_sha256),
            ("fetched_on", &row.fetched_on),
            ("withdrawal_status", &row.withdrawal_status),
        ];
        for (key, value) in fields {
            let Some(recorded) = document.get(key) else {
                return Some(format!("manifest.json's record of {docid} has no {key}"));
            };
            if recorded.as_str() != Some(value.as_str()) {
                return Some(format!(
                    "rulings.jsonl gives paragraph {} of {docid} a {key} its manifest.json record does not",
                    row.paragraph
                ));
            }
        }
    }
    if document.get("fetched_on").and_then(Json::as_str) != Some(run_date) {
        return Some(format!(
            "manifest.json's record of {docid} has a different capture date from the run"
        ));
    }
    None
}

/// How `sqlite3` binds one parameter (1-based `index`).
fn bind(value: &Json, index: usize) -> Result<Sql, Exc> {
    Ok(match value {
        Json::Null => Sql::Null,
        Json::Bool(b) => Sql::Integer(i64::from(*b)),
        Json::Int(i) => Sql::Integer(i64::try_from(i).map_err(|_| {
            Exc::Overflow("Python int too large to convert to SQLite INTEGER".into())
        })?),
        Json::Float(f) => Sql::Real(*f),
        Json::Str(PyStr::Text(s)) => Sql::Text(s.clone()),
        Json::Str(PyStr::Lone(points)) => {
            return Err(Exc::UnicodeEncode(
                crate::pyvalue::utf8_strict_error(points).unwrap_or_default(),
            ));
        }
        other => {
            return Err(Exc::Sqlite(format!(
                "Error binding parameter {index}: type '{}' is not supported",
                type_name(other)
            )));
        }
    })
}

fn bind_all(values: &[Json]) -> Result<Vec<Sql>, Exc> {
    values
        .iter()
        .enumerate()
        .map(|(i, v)| bind(v, i + 1))
        .collect()
}

fn text_or_null(value: Option<String>) -> Json {
    value.map_or(Json::Null, |s| Json::str(&s))
}

/// `int(value)` for `title.get("version_is_current", True)`.
fn py_int(value: &Json) -> Result<Json, Exc> {
    match value {
        Json::Bool(b) => Ok(Json::Int(u8::from(*b).into())),
        Json::Int(i) => Ok(Json::Int(i.clone())),
        Json::Float(f) if f.is_nan() => Err(Exc::value("cannot convert float NaN to integer")),
        Json::Float(f) if f.is_infinite() => Err(Exc::Overflow(
            "cannot convert float infinity to integer".into(),
        )),
        Json::Float(f) => Ok(Json::Int(pyvalue::float_to_int(f.trunc()))),
        Json::Str(PyStr::Text(s)) => crate::py::parse_int(s, 10)
            .map(Json::Int)
            .map_err(Exc::value),
        Json::Str(PyStr::Lone(points)) => Err(Exc::value(crate::py::invalid_int_literal(
            10,
            &pyvalue::repr_points(points),
        ))),
        other => Err(Exc::Type(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            type_name(other)
        ))),
    }
}

#[derive(Default)]
struct Counts {
    rows: usize,
    definitions: usize,
    refs: usize,
    ruling_paragraphs: usize,
    ruling_refs: usize,
}

/// Check the corpus at `root` and write a new index file at `out`.
pub fn build(root: &str, out: &str) -> Result<[usize; 4], Exc> {
    build_with_rulings(root, out, &[]).map(|(counts, _)| counts)
}

/// `build`, also reading the section references in each `rulings` run directory in
/// `rulings`. Returns the corpus counts, then the ruling paragraphs read, the
/// references stored and the distinct documents the runs' manifests list as excluded
/// that no run holds.
pub fn build_with_rulings(
    root: &str,
    out: &str,
    rulings: &[String],
) -> Result<([usize; 4], [usize; 3]), Exc> {
    let destination = PyPath::new(out);
    if std::fs::symlink_metadata(destination.fs()).is_ok() {
        return Err(refused("the index destination must not already exist"));
    }
    if !crate::pyos::isdir(&destination.parent().display()) {
        return Err(refused(
            "the index destination's parent directory does not exist",
        ));
    }
    let (sources_bytes, sources, read, source) = read_corpus(root)?;
    if let Some(source) = source
        && !rulings.is_empty()
    {
        return Err(refused(format!(
            "rulings runs cite Commonwealth law; index them with a Commonwealth corpus, not {}",
            source.code
        )));
    }
    let runs = rulings
        .iter()
        .map(|dir| read_rulings_run(dir))
        .collect::<Result<Vec<_>, _>>()?;
    // Two titles sharing a name leave a citation of that name unresolved.
    let mut names: Vec<(pyvalue::Key, Option<String>)> = Vec::new();
    for entry in &read {
        let name = entry
            .title
            .get("name")
            .ok_or_else(|| Exc::Key("'name'".into()))?;
        let key = pyvalue::key(name).map_err(|m| {
            Exc::Type(format!(
                "cannot use '{}' as a dict key ({m})",
                type_name(name)
            ))
        })?;
        match names.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = None,
            None => names.push((key, Some(entry.rid.clone()))),
        }
    }
    let counts = publish(&destination, |temporary| {
        write_index(
            temporary,
            &read,
            &sources,
            &sources_bytes,
            &names,
            &runs,
            source,
        )
    })?;
    Ok((
        [read.len(), counts.rows, counts.definitions, counts.refs],
        [
            counts.ruling_paragraphs,
            counts.ruling_refs,
            uncaptured_exclusions(&runs),
        ],
    ))
}

/// Write the index with `write` to a temporary file beside `destination`, then link it
/// there.
fn publish(
    destination: &PyPath,
    write: impl FnOnce(&str) -> Result<Counts, Exc>,
) -> Result<Counts, Exc> {
    let temporary = destination
        .parent()
        .join(&format!(
            ".{}.{}.tmp",
            destination.name(),
            crate::pyos::random_hex(32)
        ))
        .display();
    let result = write(&temporary);
    let cleanup = |path: &str| {
        let _ = std::fs::remove_file(path);
    };
    let counts = match result {
        Ok(counts) => counts,
        Err(error) => {
            cleanup(&temporary);
            return Err(error);
        }
    };
    if let Err(error) = crate::pyos::link(&temporary, &destination.display()) {
        cleanup(&temporary);
        return Err(Exc::Os(error));
    }
    crate::pyos::unlink(&temporary)?;
    Ok(counts)
}

/// The distinct documents the runs' manifests list as excluded that no run holds. A
/// document excluded by one run but captured by another is indexed, and one excluded by
/// several runs is one document.
fn uncaptured_exclusions(runs: &[RulingsRun]) -> usize {
    let captured: BTreeSet<_> = runs
        .iter()
        .flat_map(|run| &run.rows)
        .map(|row| canonical_docid(&row.docid))
        .collect();
    let excluded: BTreeSet<_> = runs
        .iter()
        .flat_map(|run| &run.excluded)
        .map(|docid| canonical_docid(docid))
        .collect();
    excluded.difference(&captured).count()
}

/// The register ID a cited name stands for, if exactly one title has it.
fn named(names: &[(pyvalue::Key, Option<String>)], name: &str) -> Option<String> {
    let key = pyvalue::Key::Str(name.chars().map(u32::from).collect());
    names
        .iter()
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v.clone())
}

/// Store each run and the section references in its paragraphs. A paragraph names no
/// title of its own, so a bare citation stays unresolved. A document with a withdrawal
/// notice in any run is stored as `NOTICE_PRESENT` in every run.
fn insert_rulings(
    connection: &Connection,
    runs: &[RulingsRun],
    names: &[(pyvalue::Key, Option<String>)],
    counts: &mut Counts,
) -> Result<(), Exc> {
    let withdrawn: BTreeSet<_> = runs
        .iter()
        .flat_map(|run| &run.rows)
        .filter(|row| row.withdrawal_status == "NOTICE_PRESENT")
        .map(|row| canonical_docid(&row.docid))
        .collect();
    for (number, capture) in runs.iter().enumerate() {
        let number = number as i64 + 1;
        run(
            connection,
            "INSERT INTO ruling_runs VALUES (?,?,?)",
            rusqlite::params![number, capture.rulings_sha256, capture.notice],
        )
        .map_err(|e| sqlite_error(&e))?;
        for row in &capture.rows {
            let status = if withdrawn.contains(&canonical_docid(&row.docid)) {
                "NOTICE_PRESENT"
            } else {
                "UNKNOWN"
            };
            counts.ruling_paragraphs += 1;
            insert_ruling_refs(connection, number, row, status, names, counts)?;
        }
    }
    Ok(())
}

/// Store the section references in one paragraph of run `number`.
fn insert_ruling_refs(
    connection: &Connection,
    number: i64,
    row: &RulingRow,
    status: &str,
    names: &[(pyvalue::Key, Option<String>)],
    counts: &mut Counts,
) -> Result<(), Exc> {
    for (kind, key, act, citation, end) in citations(&row.text) {
        let target = cited_title(&row.text, end, act.as_deref(), None, |n| named(names, n));
        run(
            connection,
            "INSERT INTO ruling_refs VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
            rusqlite::params![
                number,
                row.docid,
                row.title,
                row.paragraph,
                row.source_url,
                row.fetched_on,
                status,
                kind,
                target,
                key,
                act,
                citation
            ],
        )
        .map_err(|e| sqlite_error(&e))?;
        counts.ruling_refs += 1;
    }
    Ok(())
}

fn write_index(
    temporary: &str,
    read: &[Title],
    sources: &Json,
    sources_bytes: &[u8],
    names: &[(pyvalue::Key, Option<String>)],
    runs: &[RulingsRun],
    source: Option<&Source>,
) -> Result<Counts, Exc> {
    let connection = Connection::open(temporary).map_err(|e| sqlite_error(&e))?;
    connection
        .execute_batch(SCHEMA_SQL)
        .map_err(|e| sqlite_error(&e))?;
    let holds_rulings = runs.iter().any(|run| !run.rows.is_empty());
    if holds_rulings {
        connection
            .execute_batch(RULINGS_SQL)
            .map_err(|e| sqlite_error(&e))?;
    }
    connection
        .execute_batch("BEGIN")
        .map_err(|e| sqlite_error(&e))?;
    let mut counts = Counts::default();
    let outcome = (|| -> Result<(), Exc> {
        for entry in read {
            let title = &entry.title;
            let current = py_int(title.get("version_is_current").unwrap_or(&Json::Bool(true)))?;
            let values = [
                Json::str(&entry.rid),
                get(title, "name").clone(),
                get(title, "collection").clone(),
                get(title, "compilation_number").clone(),
                get(title, "compilation_date").clone(),
                current,
                get(title, "retrieved").clone(),
                get(title, "current_version_start").clone(),
                get(title, "register_page").clone(),
                get(title, "source_url").clone(),
                Json::Int(entry.rows.len().into()),
                Json::str(&entry.sections_sha256),
                Json::str(&entry.markdown_sha256),
            ];
            run(
                &connection,
                "INSERT INTO titles VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
                params_from_iter(bind_all(&values)?),
            )
            .map_err(|e| sqlite_error(&e))?;
            let (places, mut failure) = structure(&entry.rows, &entry.markdown);
            for (index, row) in entry.rows.iter().enumerate() {
                // zip draws row i, then its place: a failure stops before row i.
                let Some(place) = places.get(index).cloned() else {
                    return Err(failure.take().expect("structure stopped early"));
                };
                insert_row(&connection, &entry.rid, row, place, names, &mut counts)?;
            }
        }
        if holds_rulings {
            insert_rulings(&connection, runs, names, &mut counts)?;
        }
        let mut meta = vec![
            ("schema".to_string(), Json::str(SCHEMA)),
            (
                "corpus_retrieved".to_string(),
                Json::Str(PyStr::from_code_points(pyvalue::str_points(get(
                    sources,
                    "retrieved",
                )))),
            ),
            ("sources_sha256".to_string(), Json::str(&hex(sources_bytes))),
        ];
        // Only a state index names its source, so a Commonwealth index is unchanged.
        if let Some(source) = source {
            meta.push(("source".to_string(), Json::str(source.code)));
        }
        for (name, value) in [
            ("titles", read.len()),
            ("rows", counts.rows),
            ("definitions", counts.definitions),
            ("refs", counts.refs),
        ] {
            meta.push((format!("{name}_count"), Json::str(&value.to_string())));
        }
        if holds_rulings {
            for (name, value) in [
                ("ruling_paragraphs", counts.ruling_paragraphs),
                ("ruling_refs", counts.ruling_refs),
            ] {
                meta.push((format!("{name}_count"), Json::str(&value.to_string())));
            }
        }
        for (key, value) in meta {
            run(
                &connection,
                "INSERT INTO meta VALUES (?, ?)",
                params_from_iter(bind_all(&[Json::str(&key), value])?),
            )
            .map_err(|e| sqlite_error(&e))?;
        }
        Ok(())
    })();
    match outcome {
        Ok(()) => {
            connection
                .execute_batch("COMMIT")
                .map_err(|e| sqlite_error(&e))?;
            connection.close().map_err(|(_, e)| sqlite_error(&e))?;
            Ok(counts)
        }
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK");
            drop(connection);
            Err(error)
        }
    }
}

/// `connection.execute(sql, params)` through the statement cache: Python's
/// `sqlite3` reuses a statement it has compiled, and compiling each insert
/// again was most of the build's time.
fn run(
    connection: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> rusqlite::Result<usize> {
    connection.prepare_cached(sql)?.execute(params)
}

fn insert_row(
    connection: &Connection,
    rid: &str,
    row: &Json,
    place: (Option<String>, Option<String>),
    names: &[(pyvalue::Key, Option<String>)],
    counts: &mut Counts,
) -> Result<(), Exc> {
    let section = get(row, "section");
    let key = if section.truthy() {
        let normal = normalise_value(section)?;
        Json::str(&hyphens(&normal).replace(' ', ""))
    } else {
        Json::Null
    };
    let text = get(row, "text").as_str().unwrap_or_default().to_string();
    let values = [
        get(row, "row_id").clone(),
        Json::str(rid),
        section.clone(),
        key,
        text_or_null(place.0),
        text_or_null(place.1),
        get(row, "heading").clone(),
        get(row, "container").clone(),
        get(row, "text").clone(),
    ];
    run(connection,
            "INSERT INTO rows (row_id, register_id, section, section_key, division_key, subdivision_key, heading, container, text) VALUES (?,?,?,?,?,?,?,?,?)",
            params_from_iter(bind_all(&values)?),
        )
        .map_err(|e| sqlite_error(&e))?;
    let id = connection.last_insert_rowid();
    let heading = normalise_value(&or_empty(get(row, "heading")))?;
    let normal_text = normalise(&text);
    for sql in [
        "INSERT INTO rows_fts (rowid, heading, text) VALUES (?,?,?)",
        "INSERT INTO rows_stem (rowid, heading, text) VALUES (?,?,?)",
    ] {
        run(connection, sql, rusqlite::params![id, heading, normal_text])
            .map_err(|e| sqlite_error(&e))?;
    }
    counts.rows += 1;
    let row_id = get(row, "row_id").as_str().unwrap_or_default();
    let heading_text = match or_empty(get(row, "heading")) {
        Json::Str(s) => pyvalue::points_lossy(&s.code_points()),
        _ => String::new(),
    };
    if pyre::re(DEFINITIONS_HEADING, 0)
        .search(&heading_text)
        .is_some()
    {
        for (position, (term, body)) in definitions(&text).into_iter().enumerate() {
            run(
                connection,
                "INSERT INTO definitions VALUES (?,?,?,?,?)",
                rusqlite::params![term, term_key(&term), row_id, position as i64 + 1, body],
            )
            .map_err(|e| sqlite_error(&e))?;
            counts.definitions += 1;
        }
    }
    let lookup = |name: &str| named(names, name);
    for (kind, cited, act, citation, end) in citations(&text) {
        let target = cited_title(&text, end, act.as_deref(), Some(rid), lookup);
        run(
            connection,
            "INSERT INTO refs VALUES (?,?,?,?,?,?)",
            rusqlite::params![row_id, kind, target, cited, act, citation],
        )
        .map_err(|e| sqlite_error(&e))?;
        counts.refs += 1;
    }
    Ok(())
}

/// Open an index read-only and check its schema.
pub fn open_index(path: &str) -> Result<Connection, Exc> {
    let py = PyPath::new(path);
    if !std::fs::metadata(py.fs()).is_ok_and(|m| m.is_file()) {
        return Err(refused("the index file does not exist"));
    }
    let resolved = py.resolve().map_err(Exc::Os)?;
    let connection = Connection::open_with_flags(
        resolved.fs(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| sqlite_error(&e))?;
    let found: Result<Option<Sql>, rusqlite::Error> = connection
        .query_row("SELECT value FROM meta WHERE key = 'schema'", [], |r| {
            r.get::<_, Sql>(0)
        })
        .map(Some)
        .or_else(|e| {
            if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                Ok(None)
            } else {
                Err(e)
            }
        });
    match found {
        Err(_) => Err(refused("the file is not a search index")),
        Ok(Some(Sql::Text(value))) if value == SCHEMA => Ok(connection),
        Ok(_) => Err(refused("the index was built by a different schema version")),
    }
}

fn to_json(value: Sql) -> Json {
    match value {
        Sql::Null => Json::Null,
        Sql::Integer(i) => Json::Int(i.into()),
        Sql::Real(f) => Json::Float(f),
        Sql::Text(s) => Json::str(&s),
        Sql::Blob(b) => Json::str(&crate::py::repr_bytes(&b)),
    }
}

/// Every row of a statement as (column, value) pairs.
fn rows_of(
    connection: &Connection,
    sql: &str,
    params: &[Sql],
) -> Result<Vec<Vec<(String, Json)>>, Exc> {
    let mut statement = connection.prepare(sql).map_err(|e| sqlite_error(&e))?;
    let columns: Vec<String> = statement.column_names().map(str::to_string).collect();
    let mut rows = statement
        .query(params_from_iter(params.iter()))
        .map_err(|e| sqlite_error(&e))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(|e| sqlite_error(&e))? {
        let mut pairs = Vec::with_capacity(columns.len());
        for (i, name) in columns.iter().enumerate() {
            pairs.push((
                name.clone(),
                to_json(row.get::<_, Sql>(i).map_err(|e| sqlite_error(&e))?),
            ));
        }
        out.push(pairs);
    }
    Ok(out)
}

fn object(pairs: Vec<(String, Json)>) -> Json {
    Json::Object(
        pairs
            .into_iter()
            .map(|(k, v)| (PyStr::Text(k), v))
            .collect(),
    )
}

/// `_hit(row)`: the row as a dict, `version_is_current` as a bool, and `authorised: False`.
fn hit(mut pairs: Vec<(String, Json)>) -> Json {
    for (name, value) in pairs.iter_mut() {
        if name == "version_is_current" {
            *value = Json::Bool(value.truthy());
        }
    }
    pairs.push(("authorised".into(), Json::Bool(false)));
    object(pairs)
}

/// Quote each query word as an FTS5 phrase so no input is read as syntax.
pub fn match_expression(words: &[String]) -> Result<String, Exc> {
    let word = pyre::re(r"\w+", 0);
    let phrases: Vec<String> = words
        .iter()
        .map(|w| word.findall(&normalise(w)))
        .filter(|tokens| !tokens.is_empty())
        .map(|tokens| format!("\"{}\"", tokens.join(" ")))
        .collect();
    if phrases.is_empty() {
        return Err(refused("the query has no searchable words"));
    }
    Ok(phrases.join(" "))
}

const QUERY_SQL: &str = "SELECT r.row_id, r.register_id, r.section, r.heading, r.container, r.text, \
t.name AS act, t.collection, t.compilation_number, t.compilation_date, \
t.version_is_current, t.retrieved, t.register_page, t.source_url, t.sections_sha256, \
snippet(rows_fts, 1, '[', ']', '...', 16) AS snippet \
FROM rows_fts JOIN rows r ON r.id = rows_fts.rowid \
JOIN titles t ON t.register_id = r.register_id \
WHERE rows_fts MATCH ? AND (? IS NULL OR r.register_id = ?) \
AND (? IS NULL OR r.register_id IN (SELECT value FROM json_each(?))) \
ORDER BY r.section_key IN (SELECT value FROM json_each(?)) DESC, \
t.version_is_current DESC, bm25(rows_fts, 4.0, 1.0), r.id LIMIT ?";
const DEFINE_SQL: &str = "SELECT d.term, d.text AS definition, r.row_id, r.register_id, r.section, r.heading, \
t.name AS act, t.collection, t.compilation_number, t.compilation_date, \
t.version_is_current, t.retrieved, t.register_page, t.source_url, t.sections_sha256 \
FROM definitions d JOIN rows r ON r.row_id = d.row_id \
JOIN titles t ON t.register_id = r.register_id \
WHERE ((? = 0 AND d.term_key = ?) OR (? = 1 AND d.term_key LIKE ? ESCAPE '\\')) \
AND (? IS NULL OR r.register_id = ?) \
ORDER BY t.version_is_current DESC, t.name, r.id, d.position";

fn target_sql(kind: &str) -> String {
    let column = match kind {
        "division" => "division_key",
        "subdivision" => "subdivision_key",
        _ => "section_key",
    };
    format!(
        "SELECT r.row_id, r.heading, r.container, r.text, t.register_page, t.source_url FROM rows r \
JOIN titles t ON t.register_id = r.register_id WHERE r.register_id = ? AND r.{column} = ? \
ORDER BY r.id"
    )
}

fn optional_rid(connection: &Connection, title: Option<&str>) -> Result<Sql, Exc> {
    match title.filter(|t| !t.is_empty()) {
        Some(t) => Ok(Sql::Text(match index_source(connection)? {
            None => register_id_str(t)?,
            Some(_) => state::title_id(&Json::str(t))?,
        })),
        None => Ok(Sql::Null),
    }
}

const COMMONWEALTH: &str = "commonwealth";

/// The state register an index was built from; None for a Commonwealth index.
pub fn index_source(connection: &Connection) -> Result<Option<&'static Source>, Exc> {
    let found = rows_of(
        connection,
        "SELECT value FROM meta WHERE key = 'source'",
        &[],
    )?;
    let Some(row) = found.first() else {
        return Ok(None);
    };
    let value = field(row, "value");
    value
        .as_str()
        .and_then(state::source)
        .map(Some)
        .ok_or_else(|| {
            refused(format!(
                "the index names an unknown source: {}",
                repr(value)
            ))
        })
}

/// Refuse an index whose source is not `wanted`, so no command mixes sources.
fn check_source(connection: &Connection, wanted: &str, command: &str) -> Result<(), Exc> {
    let held = index_source(connection)?.map_or(COMMONWEALTH, |s| s.code);
    if held == wanted {
        return Ok(());
    }
    Err(refused(if matches!(command, "query" | "define" | "refs") {
        format!(
            "the index holds {held} legislation, not {wanted}; pass --source {held} to search it"
        )
    } else {
        format!("{command} reads only {COMMONWEALTH} indexes; this one holds {held} legislation")
    }))
}

pub fn query(
    connection: &Connection,
    words: &[String],
    title: Option<&str>,
    limit: i64,
) -> Result<Vec<Json>, Exc> {
    ranked(
        connection,
        words,
        title,
        limit,
        Sql::Null,
        SearchMode::Exact,
    )
}

/// Match every word in some English form, favouring rows that hold the query's own
/// words and marking each row that only a word form matched.
pub fn query_word_forms(
    connection: &Connection,
    words: &[String],
    title: Option<&str>,
    limit: i64,
) -> Result<Vec<Json>, Exc> {
    ranked(
        connection,
        words,
        title,
        limit,
        Sql::Null,
        SearchMode::WordForms,
    )
}

/// Match English word forms, retaining original text and marking every hit.
pub fn query_stemmed(
    connection: &Connection,
    words: &[String],
    title: Option<&str>,
    limit: i64,
) -> Result<Vec<Json>, Exc> {
    ranked(
        connection,
        words,
        title,
        limit,
        Sql::Null,
        SearchMode::Stemmed,
    )
}

/// `query`, among only the register ids `only` lists as JSON, if it is not null.
fn ranked(
    connection: &Connection,
    words: &[String],
    title: Option<&str>,
    limit: i64,
    only: Sql,
    mode: SearchMode,
) -> Result<Vec<Json>, Exc> {
    let rid = optional_rid(connection, title)?;
    let mut sections: Vec<String> = words
        .iter()
        .filter(|w| pyre::fullmatch(SECTION_RE, 0, &normalise(w)))
        .map(|w| section_key(w))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    sections.sort();
    let encoded: Vec<Json> = sections.iter().map(|s| Json::str(s)).collect();
    let sections_json: String = pyvalue::points_lossy(&dumps_line(&Json::Array(encoded), true));
    let expression = match_expression(words)?;
    let mut params = vec![
        Sql::Text(expression.clone()),
        rid.clone(),
        rid,
        only.clone(),
        only,
        Sql::Text(sections_json),
        Sql::Integer(limit),
    ];
    if let SearchMode::WordForms = mode {
        params.insert(0, Sql::Text(expression));
    }
    Ok(rows_of(connection, &mode.query_sql(), &params)?
        .into_iter()
        .map(|mut row| {
            let word_form = row
                .iter()
                .position(|(name, _)| name == "word_form")
                .is_some_and(|at| row.remove(at).1.truthy());
            mode.mark(hit(row), word_form)
        })
        .collect())
}

/// Each title's standing against one date, classified as the `as_at` stage does.
pub struct Dated {
    date: String,
    titles: Vec<Json>,
}

/// What `query` and `define` return for a date: rows from `captured` titles, one
/// refusal for each other title that matched or that `--title` named, and a summary.
pub struct DatedReport {
    pub hits: Vec<Json>,
    pub refusals: Vec<Json>,
    pub summary: Json,
}

impl Dated {
    pub fn read(connection: &Connection, date: &str) -> Result<Dated, Exc> {
        if !as_at::is_date(date) {
            return Err(refused(format!("--as-at must be YYYY-MM-DD, not {date}")));
        }
        let sql = "SELECT register_id, name, compilation_date, superseded_from, retrieved, \
                   version_is_current FROM titles ORDER BY register_id";
        let mut titles = Vec::new();
        for row in rows_of(connection, sql, &[])? {
            let start = field(&row, "compilation_date");
            let (superseded, retrieved) =
                (field(&row, "superseded_from"), field(&row, "retrieved"));
            // The manifest's names for these fields, so `as_at` applies its own checks.
            let entry = Json::obj([
                ("id", field(&row, "register_id").clone()),
                ("versionStart", start.clone()),
                ("retrieved", retrieved.clone()),
                (
                    "version_is_current",
                    Json::Bool(field(&row, "version_is_current").truthy()),
                ),
                ("current_version_start", superseded.clone()),
            ]);
            titles.push(Json::obj([
                ("register_id", field(&row, "register_id").clone()),
                ("name", field(&row, "name").clone()),
                ("status", Json::str(as_at::classify(&entry, date)?)),
                ("compilation_date", start.clone()),
                ("superseded_from", superseded.clone()),
                ("retrieved", retrieved.clone()),
            ]));
        }
        Ok(Dated {
            date: date.to_string(),
            titles,
        })
    }

    fn title(&self, rid: &str) -> Option<&Json> {
        self.titles
            .iter()
            .find(|title| title.at("register_id").as_str() == Some(rid))
    }

    /// The title's status, window and what the status covers, or the next step.
    fn standing(&self, rid: &str) -> Json {
        let Some(title) = self.title(rid) else {
            return Json::Null;
        };
        let status = title.at("status").as_str().unwrap_or_default();
        Json::obj([
            ("date", Json::str(&self.date)),
            ("status", title.at("status").clone()),
            ("compilation_date", title.at("compilation_date").clone()),
            ("superseded_from", title.at("superseded_from").clone()),
            ("retrieved", title.at("retrieved").clone()),
            ("covers", Json::str(as_at::CAPTURED_MEANS)),
            (
                "next_step",
                text_or_null(as_at::next_step(status).map(str::to_string)),
            ),
        ])
    }

    fn captured(&self, rid: &str) -> bool {
        self.title(rid)
            .is_some_and(|title| title.at("status").as_str() == Some(as_at::CAPTURED))
    }

    /// `hits` from captured titles, the refusals and the summary.
    fn report(
        &self,
        hits: Vec<Json>,
        matched: &BTreeMap<String, usize>,
        named: Option<&str>,
    ) -> DatedReport {
        let hits: Vec<Json> = hits
            .into_iter()
            .filter(|hit| self.captured(hit.at("register_id").as_str().unwrap_or_default()))
            .map(|hit| {
                let standing = self.standing(hit.at("register_id").as_str().unwrap_or_default());
                with_field(hit, "as_at", standing)
            })
            .collect();
        let refusals = self.refusals(hits.is_empty(), matched, named);
        let summary = self.summary(hits.len(), &refusals);
        DatedReport {
            hits,
            refusals,
            summary,
        }
    }

    /// A refusal for each title not captured that `matched` rows or that `named`. With
    /// no row to show, a search of all titles refuses every title not captured, so an
    /// empty answer with exit 0 means each title in scope was held for the date.
    fn refusals(
        &self,
        no_rows: bool,
        matched: &BTreeMap<String, usize>,
        named: Option<&str>,
    ) -> Vec<Json> {
        let mut refusals = Vec::new();
        for title in &self.titles {
            let rid = title.at("register_id").as_str().unwrap_or_default();
            let rows = matched.get(rid).copied().unwrap_or_default();
            let held = title.at("status").as_str() == Some(as_at::CAPTURED);
            if !held && (rows > 0 || named == Some(rid) || (no_rows && named.is_none())) {
                refusals.push(Json::obj([
                    ("refused", Json::Bool(true)),
                    ("register_id", title.at("register_id").clone()),
                    ("act", title.at("name").clone()),
                    ("matching_rows", Json::Int(rows.into())),
                    ("as_at", self.standing(rid)),
                ]));
            }
        }
        refusals
    }

    fn summary(&self, rows: usize, refusals: &[Json]) -> Json {
        let count = |status: &'static str| {
            let of = |title: &&Json| title.at("status").as_str() == Some(status);
            (
                status,
                Json::Int(self.titles.iter().filter(of).count().into()),
            )
        };
        let withheld: usize = refusals
            .iter()
            .map(|refusal| match refusal.at("matching_rows") {
                Json::Int(rows) => usize::try_from(rows).unwrap_or_default(),
                _ => 0,
            })
            .sum();
        Json::obj([(
            "as_at_summary",
            Json::obj([
                ("date", Json::str(&self.date)),
                ("rows", Json::Int(rows.into())),
                ("withheld_rows", Json::Int(withheld.into())),
                ("titles", Json::obj(as_at::STATUSES.map(count))),
                ("covers", Json::str(as_at::CAPTURED_MEANS)),
            ]),
        )])
    }
}

/// `value` with one more member, if it is an object.
fn with_field(mut value: Json, key: &str, member: Json) -> Json {
    if let Json::Object(pairs) = &mut value {
        pairs.push((PyStr::Text(key.into()), member));
    }
    value
}

/// `query` for a date: rows only from titles the build holds for it.
pub fn query_as_at(
    connection: &Connection,
    words: &[String],
    title: Option<&str>,
    limit: i64,
    date: &str,
) -> Result<DatedReport, Exc> {
    dated_query(connection, words, title, limit, date, SearchMode::Exact)
}

/// `query_word_forms` with the same captured-window restriction as exact search.
pub fn query_word_forms_as_at(
    connection: &Connection,
    words: &[String],
    title: Option<&str>,
    limit: i64,
    date: &str,
) -> Result<DatedReport, Exc> {
    dated_query(connection, words, title, limit, date, SearchMode::WordForms)
}

/// Word-form search with the same captured-window restriction as exact search.
pub fn query_stemmed_as_at(
    connection: &Connection,
    words: &[String],
    title: Option<&str>,
    limit: i64,
    date: &str,
) -> Result<DatedReport, Exc> {
    dated_query(connection, words, title, limit, date, SearchMode::Stemmed)
}

fn dated_query(
    connection: &Connection,
    words: &[String],
    title: Option<&str>,
    limit: i64,
    date: &str,
    mode: SearchMode,
) -> Result<DatedReport, Exc> {
    let dated = Dated::read(connection, date)?;
    let named = given_title(connection, title)?;
    let rid = named.clone().map_or(Sql::Null, Sql::Text);
    let sql = "SELECT r.register_id, count(*) AS matched FROM rows_fts \
               JOIN rows r ON r.id = rows_fts.rowid \
               WHERE rows_fts MATCH ? AND (? IS NULL OR r.register_id = ?) GROUP BY r.register_id";
    let params = [Sql::Text(match_expression(words)?), rid.clone(), rid];
    let mut matched = BTreeMap::new();
    for row in rows_of(connection, &mode.sql(sql), &params)? {
        let rows = match field(&row, "matched") {
            Json::Int(rows) => usize::try_from(rows).unwrap_or_default(),
            _ => 0,
        };
        let title = field(&row, "register_id").as_str().unwrap_or_default();
        matched.insert(title.to_string(), rows);
    }
    let captured = dated
        .titles
        .iter()
        .filter(|title| dated.captured(title.at("register_id").as_str().unwrap_or_default()));
    let ids = Json::Array(
        captured
            .map(|title| title.at("register_id").clone())
            .collect(),
    );
    let only = Sql::Text(pyvalue::points_lossy(&dumps_line(&ids, true)));
    let hits = ranked(connection, words, title, limit, only, mode)?;
    Ok(dated.report(hits, &matched, named.as_deref()))
}

/// `define` for a date: definitions only from titles the build holds for it.
pub fn define_as_at(
    connection: &Connection,
    term: &str,
    title: Option<&str>,
    date: &str,
) -> Result<DatedReport, Exc> {
    let dated = Dated::read(connection, date)?;
    let named = given_title(connection, title)?;
    let hits = define(connection, term, title)?;
    let mut matched = BTreeMap::new();
    for hit in &hits {
        let rid = hit.at("register_id").as_str().unwrap_or_default();
        *matched.entry(rid.to_string()).or_default() += 1;
    }
    Ok(dated.report(hits, &matched, named.as_deref()))
}

/// `cite_check`, with each citation reporting its title's standing on a date, and a
/// refusal if `--title` names one not captured, whatever the draft cites. A citation
/// that reached no title has no standing.
pub fn cite_check_as_at(
    connection: &Connection,
    draft: &str,
    title: Option<&str>,
    date: &str,
) -> Result<(Vec<Json>, Vec<Json>), Exc> {
    let dated = Dated::read(connection, date)?;
    let report = cite_check(connection, draft, title)?;
    let named = given_title(connection, title)?;
    let refusals = match named.as_deref() {
        Some(rid) => dated.refusals(true, &BTreeMap::new(), Some(rid)),
        None => Vec::new(),
    };
    let dated_item = |item: Json| {
        let standing = item
            .at("register_id")
            .as_str()
            .map(|rid| dated.standing(rid));
        with_field(item, "as_at", standing.unwrap_or(Json::Null))
    };
    Ok((report.into_iter().map(dated_item).collect(), refusals))
}

pub fn define(connection: &Connection, term: &str, title: Option<&str>) -> Result<Vec<Json>, Exc> {
    let key = term_key(term);
    if key.is_empty() {
        return Err(refused("the term is empty"));
    }
    let rid = optional_rid(connection, title)?;
    let exact = rows_of(
        connection,
        DEFINE_SQL,
        &[
            Sql::Integer(0),
            Sql::Text(key.clone()),
            Sql::Integer(0),
            Sql::Null,
            rid.clone(),
            rid.clone(),
        ],
    )?;
    if !exact.is_empty() {
        return Ok(exact.into_iter().map(hit).collect());
    }
    let prefix = format!(
        "{} %",
        key.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    Ok(rows_of(
        connection,
        DEFINE_SQL,
        &[
            Sql::Integer(1),
            Sql::Null,
            Sql::Integer(1),
            Sql::Text(prefix),
            rid.clone(),
            rid,
        ],
    )?
    .into_iter()
    .map(hit)
    .collect())
}

fn field<'a>(pairs: &'a [(String, Json)], name: &str) -> &'a Json {
    pairs
        .iter()
        .find(|(k, _)| k == name)
        .map_or(&Json::Null, |(_, v)| v)
}

/// One `refs` reference, `kind`, `to_key`, `citation`, `cited_act` and
/// `to_register_id`, as `refs` reports it: with the rows it reaches.
fn cited_entry(connection: &Connection, reference: &[(String, Json)]) -> Result<Json, Exc> {
    let to = field(reference, "to_register_id").clone();
    let kind = field(reference, "kind").as_str().unwrap_or_default();
    let key = field(reference, "to_key").as_str().unwrap_or_default();
    let targets = match &to {
        Json::Str(PyStr::Text(target)) => rows_of(
            connection,
            &target_sql(kind),
            &[Sql::Text(target.clone()), Sql::Text(key.into())],
        )?
        .into_iter()
        .map(object)
        .collect(),
        _ => Vec::new(),
    };
    Ok(object(vec![
        ("kind".into(), field(reference, "kind").clone()),
        ("key".into(), field(reference, "to_key").clone()),
        ("citation".into(), field(reference, "citation").clone()),
        ("cited_act".into(), field(reference, "cited_act").clone()),
        ("register_id".into(), to),
        ("rows".into(), Json::Array(targets)),
    ]))
}

/// Each `define` hit with `refers_to`: the provisions its definition cites, read and
/// resolved as `build` reads a row's references, so `has the meaning given by
/// section 9-5` reaches the row of section 9-5.
pub fn follow_definitions(connection: &Connection, hits: Vec<Json>) -> Result<Vec<Json>, Exc> {
    let names = title_names(connection)?;
    let lookup = |name: &str| names.get(name).filter(|rid| *rid != SHARED_NAME).cloned();
    hits.into_iter()
        .map(|hit| {
            let text = hit
                .at("definition")
                .as_str()
                .unwrap_or_default()
                .to_string();
            let own = hit.at("register_id").as_str().map(str::to_string);
            let mut cited = Vec::new();
            for (kind, key, act, citation, end) in citations(&text) {
                let to = cited_title(&text, end, act.as_deref(), own.as_deref(), lookup);
                let reference = vec![
                    ("kind".to_string(), Json::str(&kind)),
                    ("to_key".into(), Json::str(&key)),
                    ("citation".into(), Json::str(&citation)),
                    (
                        "cited_act".into(),
                        act.as_deref().map_or(Json::Null, Json::str),
                    ),
                    (
                        "to_register_id".into(),
                        to.as_deref().map_or(Json::Null, Json::str),
                    ),
                ];
                cited.push(cited_entry(connection, &reference)?);
            }
            Ok(with_field(hit, "refers_to", Json::Array(cited)))
        })
        .collect()
}

/// The title whose definitions a row's asterisked terms also use when its own title
/// defines none. TAA 1953 s 3AA(2) gives an expression in Schedule 1 the meaning it has
/// in the ITAA 1997 (read in the 2026-11-18 snapshot). Schedule 1's sections, and only
/// they in that Act, are numbered like `8-1`, so a section number with a hyphen marks
/// a Schedule 1 row.
fn borrowed_terms(rid: &str, section_key: &str) -> Option<&'static str> {
    (rid == "C1953A00001" && section_key.contains('-')).then_some("C2004A05138")
}
/// Where a stored term's qualifier starts: `cost base` of `cost base of a CGT asset`.
const TERM_QUALIFIER: &str = r",| (?:of|in|for|to|from|with|by|on) (?:a|an|the|any|another)\b";
const TERM_WORD: &str = r"[\w‐‑&'’-]+";
/// A term never runs past these, nor into the next asterisk.
const TERM_STOP: [char; 10] = [',', ';', ':', '.', '(', ')', '[', ']', '\n', '*'];

/// The forms of a folded term to look up: as written, then singular, dropping `s`
/// before `es` so `rates` reads as `rate`, not `rat`.
fn term_forms(key: &str) -> Vec<String> {
    let mut forms = vec![key.to_string()];
    for (plural, singular) in [("s", ""), ("es", ""), ("ies", "y")] {
        if let Some(stem) = key.strip_suffix(plural) {
            forms.push(format!("{stem}{singular}"));
        }
    }
    forms
}

/// Definitions by folded term.
type TermTable = BTreeMap<String, Vec<Json>>;

/// A table for each title in `scopes`, then one for each keyed by the term without its
/// qualifier; each entry is `(by_head, table)`.
fn term_tables(connection: &Connection, scopes: &[&str]) -> Result<Vec<(bool, TermTable)>, Exc> {
    let qualifier = pyre::re(TERM_QUALIFIER, 0);
    let (mut wholes, mut qualified) = (Vec::new(), Vec::new());
    for scope in scopes {
        let (mut whole, mut heads) = (TermTable::new(), TermTable::new());
        for found in rows_of(
            connection,
            "SELECT d.term, d.term_key, d.row_id, r.register_id, r.heading FROM definitions d \
JOIN rows r ON r.row_id = d.row_id WHERE r.register_id = ? ORDER BY r.id, d.position",
            &[Sql::Text(scope.to_string())],
        )? {
            let key = field(&found, "term_key")
                .as_str()
                .unwrap_or_default()
                .to_string();
            let head = qualifier.split(&key)[0].trim().to_string();
            let entry = object(
                found
                    .into_iter()
                    .filter(|(name, _)| name != "term_key")
                    .collect(),
            );
            if head != key && !head.is_empty() {
                heads.entry(head).or_default().push(entry.clone());
            }
            whole.entry(key).or_default().push(entry);
        }
        wholes.push((false, whole));
        qualified.push((true, heads));
    }
    wholes.extend(qualified);
    Ok(wholes)
}

/// The term an asterisk at the start of `span` marks: the longest run of its words found
/// in the first table that holds one, with how it matched: `term` as written,
/// `singular`, or `without qualifier` from a table keyed without qualifiers.
fn marked_term(
    span: &str,
    tables: &[(bool, TermTable)],
) -> Option<(&'static str, String, Vec<Json>)> {
    let words: Vec<String> = pyre::re(TERM_WORD, 0)
        .findall(span)
        .into_iter()
        .take(MAX_TERM_WORDS)
        .collect();
    tables.iter().find_map(|(by_head, table)| {
        (1..=words.len()).rev().find_map(|n| {
            let written = words[..n].join(" ");
            let forms = term_forms(&term_key(&written));
            let (at, definitions) = forms
                .iter()
                .enumerate()
                .find_map(|(at, form)| table.get(form).map(|found| (at, found.clone())))?;
            let how = match (by_head, at) {
                (true, _) => "without qualifier",
                (false, 0) => "term",
                (false, _) => "singular",
            };
            Some((how, written, definitions))
        })
    })
}

/// The defined terms a row marks with an asterisk, each with the definitions it
/// reaches, and how many asterisks reached none. Most defined terms are marked by an
/// asterisk at their start (ITAA 1997 s 2-10, GST Act s 3-1), so where a term ends is
/// read here: the longest run of following words, up to punctuation, that is a term
/// the row's title defines (else the title `borrowed_terms` names), as written or
/// singular; failing that, the longest that is such a term without its qualifier. A
/// longer term the index does not hold therefore reads as its longest defined start.
pub fn defined_terms(connection: &Connection, row_id: &str) -> Result<(Vec<Json>, usize), Exc> {
    let sql = "SELECT register_id, section_key, text FROM rows WHERE row_id = ?";
    let Some(row) = rows_of(connection, sql, &[Sql::Text(row_id.into())])?
        .into_iter()
        .next()
    else {
        return Err(refused(format!("{row_id} is not in the index")));
    };
    let rid = field(&row, "register_id").as_str().unwrap_or_default();
    let text = field(&row, "text").as_str().unwrap_or_default();
    let section = field(&row, "section_key").as_str().unwrap_or_default();
    let scopes: Vec<&str> = [Some(rid), borrowed_terms(rid, section)]
        .into_iter()
        .flatten()
        .collect();
    let tables = term_tables(connection, &scopes)?;
    let mut terms: Vec<(String, Json)> = Vec::new();
    let mut unresolved = 0;
    for (at, _) in text.match_indices('*') {
        let rest = &text[at + 1..];
        let span = &rest[..rest.find(TERM_STOP).unwrap_or(rest.len())];
        if !span.starts_with(|c: char| c.is_alphanumeric()) {
            continue;
        }
        let Some((how, written, definitions)) = marked_term(span, &tables) else {
            unresolved += 1;
            continue;
        };
        let key = term_key(&written);
        if terms.iter().any(|(seen, _)| *seen == key) {
            continue;
        }
        terms.push((
            key,
            object(vec![
                ("term".into(), Json::str(&written)),
                ("match".into(), Json::str(how)),
                ("definitions".into(), Json::Array(definitions)),
            ]),
        ));
    }
    Ok((
        terms.into_iter().map(|(_, term)| term).collect(),
        unresolved,
    ))
}

/// What `row_id` cites, and rows that cite its section, Division or Subdivision.
pub fn refs(connection: &Connection, row_id: &str) -> Result<Json, Exc> {
    let found = rows_of(
        connection,
        "SELECT r.*, t.register_page, t.source_url FROM rows r \
JOIN titles t ON t.register_id = r.register_id WHERE r.row_id = ?",
        &[Sql::Text(row_id.into())],
    )?;
    let Some(row) = found.into_iter().next() else {
        return Err(refused(format!("{row_id} is not in the index")));
    };
    let mut outbound = Vec::new();
    for reference in rows_of(
        connection,
        "SELECT * FROM refs WHERE from_row_id = ? ORDER BY rowid",
        &[Sql::Text(row_id.into())],
    )? {
        outbound.push(cited_entry(connection, &reference)?);
    }
    let sql_value = |value: &Json| match value {
        Json::Str(PyStr::Text(s)) => Sql::Text(s.clone()),
        Json::Int(i) => Sql::Integer(i64::try_from(i).unwrap_or_default()),
        _ => Sql::Null,
    };
    let inbound: Vec<Json> = rows_of(
        connection,
        "SELECT DISTINCT r.row_id, r.heading, r.text, f.kind, f.citation, t.register_page, \
t.source_url FROM refs f JOIN rows r ON r.row_id = f.from_row_id \
JOIN titles t ON t.register_id = r.register_id WHERE f.to_register_id = ? AND r.row_id != ? \
AND ((f.kind = 'section' AND f.to_key = ?) OR (f.kind = 'division' AND f.to_key = ?) \
OR (f.kind = 'subdivision' AND f.to_key = ?)) ORDER BY r.id",
        &[
            sql_value(field(&row, "register_id")),
            Sql::Text(row_id.into()),
            sql_value(field(&row, "section_key")),
            sql_value(field(&row, "division_key")),
            sql_value(field(&row, "subdivision_key")),
        ],
    )?
    .into_iter()
    .map(object)
    .collect();
    let mut report = vec![
        ("row_id".into(), Json::str(row_id)),
        ("register_id".into(), field(&row, "register_id").clone()),
        ("section".into(), field(&row, "section").clone()),
        ("division".into(), field(&row, "division_key").clone()),
        ("subdivision".into(), field(&row, "subdivision_key").clone()),
        ("heading".into(), field(&row, "heading").clone()),
        ("text".into(), field(&row, "text").clone()),
        ("register_page".into(), field(&row, "register_page").clone()),
        ("source_url".into(), field(&row, "source_url").clone()),
        ("cites".into(), Json::Array(outbound)),
        ("cited_by".into(), Json::Array(inbound)),
    ];
    if holds_rulings(connection)? {
        let rulings = rows_of(
            connection,
            "SELECT DISTINCT docid, title, paragraph, withdrawal_status, fetched_on, \
source_url, kind, citation FROM ruling_refs WHERE to_register_id = ? \
AND ((kind = 'section' AND to_key = ?) OR (kind = 'division' AND to_key = ?) \
OR (kind = 'subdivision' AND to_key = ?)) ORDER BY rowid",
            &[
                sql_value(field(&row, "register_id")),
                sql_value(field(&row, "section_key")),
                sql_value(field(&row, "division_key")),
                sql_value(field(&row, "subdivision_key")),
            ],
        )?;
        let rulings = rulings.into_iter().map(object).collect();
        report.push(("cited_by_rulings".into(), Json::Array(rulings)));
    }
    Ok(object(report))
}

/// Whether the index was built with a rulings run.
fn holds_rulings(connection: &Connection) -> Result<bool, Exc> {
    let sql = "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ruling_refs'";
    Ok(!rows_of(connection, sql, &[])?.is_empty())
}

/// What `cite-check` calls a citation, in the order its summary lists them.
const OUTCOMES: [&str; 7] = [
    "resolved",
    "assumed",
    "container_level",
    "ambiguous",
    "multi_section",
    "not_held",
    "superseded",
];
/// The 3 that let `cite-check` exit 0.
const CLEAN_OUTCOMES: [&str; 3] = ["resolved", "assumed", "container_level"];
/// What a quotation is graded, in the order the summary lists them. A quotation not
/// compared has no grade.
const GRADES: [&str; 4] = ["exact", "normalised", "partial", "none"];
/// The 2 that let `cite-check` exit 0.
const CLEAN_GRADES: [&str; 2] = ["exact", "normalised"];
/// The shortest quotation called `distinct`, in words. The share of word runs of each
/// length that only one row holds is in docs/build-evidence.md; this is the first length
/// at which 4 in 5 did on that build. It is a measurement of that build, not a guarantee.
const DISTINCT_FLOOR_WORDS: usize = 10;
/// Stands in for the register ID of a name two titles share, so the name still counts
/// as held.
const SHARED_NAME: &str = "";

/// A full stop, or the like, that ends a sentence, or a blank line. A stop after a listed
/// lower-case abbreviation does not, and anything unsure runs on: the wider scan is safer.
const SENTENCE_END: &str = r"(?<!\bcf)(?<!\bviz)(?<!\betc)(?<!\be\.g)(?<!\bi\.e)(?<!\breg)(?<!\bregs)[.!?](?=\s+[A-Z(]|\s*\Z)|\n\s*\n";

/// What may follow `this` and stay in the title.
const THIS_PROVISION: &str = "Act|Division|Subdivision|Part|Schedule|Chapter|division|subdivision|part|schedule|chapter|section|subsection";

/// Anything in a sentence that may name a source. `this` provisions and numbered
/// provisions are skipped first; then any capitalised word counts, as do `of the` and
/// `of this` phrases and the listed instrument words.
/// shortcut: the instrument words are a fixed list. What it misses stays `assumed`.
fn source_pattern() -> String {
    format!(
        r"\bthis\s+(?:{THIS_PROVISION})\b|\b(?:Sections?|Subsections?|Paragraphs?|Subparagraphs?|Divisions?|Subdivisions?|Parts?|Schedules?|Chapters?)\s+[0-9IVX][0-9A-Z-]*\b|(?P<source>(?<![A-Za-z0-9])[A-Z]|\bof\s+(?:the\s+[a-z]|that\b)|\b(?:of|in|under)\s+this\s+(?!(?:{THIS_PROVISION})\b)|(?<![A-Za-z0-9])(?:{SOURCE_WORDS})(?![A-Za-z0-9]))"
    )
}

/// The byte range of the sentence around a citation ending at `end`.
fn sentence_around(draft: &str, end: usize) -> (usize, usize) {
    let (mut start, mut stop) = (0, draft.len());
    for found in pyre::re(SENTENCE_END, 0).captures_iter(draft) {
        let boundary = found.get(0).expect("group 0");
        if boundary.end() <= end {
            start = boundary.end();
        } else {
            stop = boundary.start();
            break;
        }
    }
    (start, stop)
}

/// Whether the sentence around a citation ending at `end` may name a source, so that
/// `--title` cannot stand in for the Act. A sentence's own capitalised first word does
/// not count, but `ITAA 1936 section 8-1` opens with a name and does.
fn sentence_names_a_source(draft: &str, end: usize) -> bool {
    let (start, stop) = sentence_around(draft, end);
    let sentence = &draft[start..stop];
    let opening = pyre::re(r"\W*[A-Z]?[a-z]*\b", 0)
        .match_at(sentence, 0)
        .map_or(0, |found| found.get(0).expect("group 0").end());
    pyre::re(&source_pattern(), 0)
        .captures_iter(&sentence[opening..])
        .any(|found| found.name("source").is_some())
}

/// What a grade shows, in both `cite-check`'s and `quote`'s notices.
macro_rules! grade_meaning {
    () => {
        "Exact and normalised show those words are in that row of this build, not that they \
are fairly quoted, authorised text or the law at any date. Distinct means no other row of this build holds at least 10 such words, and shows no more about any other source."
    };
}

const CITE_CHECK_NOTICE: &str = concat!(
    "A resolved citation names its Act and a provision this build holds. An \
assumed citation names no Act in its sentence, so it shows only that --title holds the \
provision, not that the author meant that title. Neither shows \
that the provision says what the draft claims or applied at any date. A quotation is read \
only in quotation marks in the citation's sentence, and only against a resolved or assumed \
section. ",
    grade_meaning!(),
    " Current and \
superseded are this build's record from the Register, not a check today; a later amendment can \
outdate them. Forms refs does \
not read, such as s 8-1, are not counted."
);

const QUOTE_NOTICE: &str = concat!(
    grade_meaning!(),
    " Partial and none are not matches. A section's row is compared whole, whatever subsection is quoted."
);

/// The register ID `title` gives, refused unless the index holds it.
fn given_title(connection: &Connection, title: Option<&str>) -> Result<Option<String>, Exc> {
    let Some(t) = title.filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    let rid = register_id_str(t)?;
    let sql = "SELECT 1 FROM titles WHERE register_id = ?";
    if rows_of(connection, sql, &[Sql::Text(rid.clone())])?.is_empty() {
        return Err(refused(format!("{rid} is not in the index")));
    }
    Ok(Some(rid))
}

/// Each title name and its register ID. Two titles sharing a name leave a citation of
/// that name ambiguous, not absent, so the name maps to `SHARED_NAME`.
fn title_names(connection: &Connection) -> Result<BTreeMap<String, String>, Exc> {
    let mut names = BTreeMap::new();
    for row in rows_of(connection, "SELECT name, register_id FROM titles", &[])? {
        let rid = field(&row, "register_id").as_str().unwrap_or_default();
        names
            .entry(field(&row, "name").as_str().unwrap_or_default().to_string())
            .and_modify(|slot: &mut String| *slot = SHARED_NAME.to_string())
            .or_insert_with(|| rid.to_string());
    }
    Ok(names)
}

/// The title a citation reaches and, when it cannot reach one, why that is `ambiguous`.
/// A bare citation reaches `own` only if nothing in its sentence may name a source. A
/// named title must end where the name does. A name that is not held has no reason.
fn citation_target(
    draft: &str,
    end: usize,
    act: Option<&str>,
    own: Option<&str>,
    names: &BTreeMap<String, String>,
) -> (Option<String>, Option<&'static str>) {
    let lookup = |name: &str| names.get(name).cloned();
    if act.is_some() {
        if !ends_name(draft, end, lookup) {
            return (None, None);
        }
    } else if own.is_none() {
        return (None, Some("no title is named"));
    } else if sentence_names_a_source(draft, end) {
        return (None, Some("its sentence may name another source"));
    }
    match cited_title(draft, end, act, own, lookup) {
        Some(rid) if rid == SHARED_NAME => (None, Some("2 titles share this name")),
        target => (target, None),
    }
}

/// A row of `rows_of`.
type Row = Vec<(String, Json)>;

/// The rows a citation reaches in a title, and that title's row.
fn cited_rows(
    connection: &Connection,
    rid: &str,
    kind: &str,
    key: &str,
) -> Result<(Vec<Row>, Option<Row>), Exc> {
    let title = Sql::Text(rid.to_string());
    let rows = rows_of(
        connection,
        &target_sql(kind),
        &[title.clone(), Sql::Text(key.to_string())],
    )?;
    let sql = "SELECT * FROM titles WHERE register_id = ?";
    let held = rows_of(connection, sql, &[title])?.into_iter().next();
    Ok((rows, held))
}

fn citation_outcome(
    in_range: bool,
    no_one_title: bool,
    rows: usize,
    current: bool,
    section: bool,
    bare: bool,
) -> &'static str {
    if in_range {
        // `citations` reads only the first item of a range.
        "multi_section"
    } else if no_one_title {
        "ambiguous"
    } else if rows == 0 {
        "not_held"
    } else if !current {
        "superseded"
    } else if !section && !bare {
        "container_level"
    } else if section && rows > 1 {
        "ambiguous"
    } else if bare {
        "assumed"
    } else {
        "resolved"
    }
}

/// A section label as it sorts: each hyphen-separated part by its number, then by
/// the letters after it, so `8-5` comes before `8-10` and `82` before `82A`.
fn label_order(key: &str) -> Vec<(usize, String, String)> {
    key.split('-')
        .map(|part| {
            let digits = part.len() - part.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            let number = part[..digits].trim_start_matches('0');
            (number.len(), number.to_string(), part[digits..].to_string())
        })
        .collect()
}

/// The sections `rid` holds either side of `key`, which it does not hold: the nearest
/// valid range, to show a mistyped number. Neither is a guess at what the author meant.
fn nearest_sections(connection: &Connection, rid: &str, key: &str) -> Result<Vec<Json>, Exc> {
    let sql =
        "SELECT DISTINCT section_key FROM rows WHERE register_id = ? AND section_key IS NOT NULL";
    let target = label_order(key);
    let found = rows_of(connection, sql, &[Sql::Text(rid.to_string())])?;
    let held = found
        .iter()
        .map(|row| field(row, "section_key").as_str().unwrap_or_default());
    let below = held
        .clone()
        .filter(|h| label_order(h) < target)
        .max_by_key(|h| label_order(h));
    let above = held
        .filter(|h| label_order(h) > target)
        .min_by_key(|h| label_order(h));
    Ok([below, above]
        .into_iter()
        .flatten()
        .map(Json::str)
        .collect())
}

/// The columns of the cited title that pin a result to the build.
const TITLE_FIELDS: [&str; 6] = [
    "compilation_number",
    "compilation_date",
    "retrieved",
    "sections_sha256",
    "register_page",
    "source_url",
];

/// One citation found in a draft: kind, key, the Act it names, its text and where it ends.
type FoundCitation = (String, String, Option<String>, String, usize);

/// How many rows other than `row_id` hold all of `quote`. FTS finds the rows with its
/// words in order, reading the NFKC quotation with the tokeniser that read each row, so a
/// row that holds it is never missed. `quotes::grade` then applies the same test as the
/// row reached.
fn other_rows(connection: &Connection, quote: &str, row_id: &str) -> Result<usize, Exc> {
    let sql = "SELECT r.row_id, r.text FROM rows_fts JOIN rows r ON r.id = rows_fts.rowid \
               WHERE rows_fts MATCH ?";
    let phrase = format!("text : \"{}\"", normalise(quote).replace('"', "\"\""));
    let found = rows_of(connection, sql, &[Sql::Text(phrase)])?;
    let other = |row: &&Row| {
        field(row, "row_id").as_str() != Some(row_id)
            && quotes::grade(quote, field(row, "text").as_str().unwrap_or_default()).held()
    };
    Ok(found.iter().filter(other).count())
}

/// `quote` graded against the row `row_id`, which holds `text`, and, when that row holds
/// it whole, how many other rows do.
fn grade_row(
    connection: &Connection,
    quote: &str,
    row_id: &str,
    text: &str,
) -> Result<(quotes::Grade, Option<usize>), Exc> {
    let found = quotes::grade(quote, text);
    let others = if found.held() {
        Some(other_rows(connection, quote, row_id)?)
    } else {
        None
    };
    Ok((found, others))
}

/// `distinct`: held by no other row and at least `DISTINCT_FLOOR_WORDS` long; `null`
/// for a quotation the row does not hold.
fn distinct_of(found: &quotes::Grade, others: Option<usize>) -> Json {
    others.map_or(Json::Null, |n| {
        Json::Bool(n == 0 && found.quote_words >= DISTINCT_FLOOR_WORDS)
    })
}

/// The digest of the build's `sources.json`, which ties a result to its build.
fn sources_digest(connection: &Connection) -> Result<Json, Exc> {
    let sql = "SELECT value FROM meta WHERE key = 'sources_sha256'";
    Ok(rows_of(connection, sql, &[])?
        .first()
        .map_or(Json::Null, |row| field(row, "value").clone()))
}

/// `quote` graded against the row `row_id`, pinned to the row's title in this build.
pub fn quote(connection: &Connection, row_id: &str, quote: &str) -> Result<Json, Exc> {
    let sql = "SELECT r.row_id, r.register_id, r.text, t.compilation_number, \
               t.compilation_date, t.retrieved, t.sections_sha256, t.version_is_current, \
               t.register_page, t.source_url \
               FROM rows r JOIN titles t ON t.register_id = r.register_id WHERE r.row_id = ?";
    let found = rows_of(connection, sql, &[Sql::Text(row_id.to_string())])?;
    let Some(row) = found.into_iter().next() else {
        return Err(refused(format!("{row_id} is not in the index")));
    };
    let text = field(&row, "text").as_str().unwrap_or_default();
    // The test `quotes::quotations` applies to a draft: punctuation alone, such as `.`,
    // is in almost every row and would grade `exact`.
    if !quote.chars().any(char::is_alphanumeric) {
        return Err(refused("the quotation has no letters or digits"));
    }
    let (graded, others) = grade_row(connection, quote, row_id, text)?;
    let count = |n: Option<usize>| n.map_or(Json::Null, |n| Json::Int(n.into()));
    let mut report = vec![
        ("row_id".into(), Json::str(row_id)),
        ("register_id".into(), field(&row, "register_id").clone()),
        ("quote".into(), Json::str(quote)),
        ("text".into(), Json::str(text)),
        ("grade".into(), Json::str(graded.grade)),
        ("matched_words".into(), count(graded.matched_words)),
        ("quote_words".into(), count(Some(graded.quote_words))),
        ("other_rows".into(), count(others)),
        ("distinct".into(), distinct_of(&graded, others)),
    ];
    for name in TITLE_FIELDS {
        report.push((name.into(), field(&row, name).clone()));
    }
    let current = field(&row, "version_is_current").truthy();
    report.push(("version_is_current".into(), Json::Bool(current)));
    report.push(("sources_sha256".into(), sources_digest(connection)?));
    Ok(object(report))
}

/// Why a quotation was not compared, or `None` if it can be.
fn not_compared(quotation: &Quotation, row: Option<&Json>, end: usize) -> Option<&'static str> {
    if !quotation.closed {
        Some("it has no closing mark")
    } else if quotation.surrounds(end) {
        Some("the citation is inside this quotation")
    } else if row.is_none() {
        Some("the citation reached no one section")
    } else {
        None
    }
}

/// The report object for one quotation beside a citation ending at `end`. `row` is the
/// row the citation reached. A quotation not compared has no grade and a reason.
fn quotation_report(
    connection: &Connection,
    quotation: &Quotation,
    row: Option<&Json>,
    end: usize,
) -> Result<Json, Exc> {
    let mut report = vec![
        ("quote".into(), Json::str(&quotation.text)),
        ("start".into(), Json::Int(quotation.start.into())),
    ];
    let count = |n: Option<usize>| n.map_or(Json::Null, |n| Json::Int(n.into()));
    let reason = not_compared(quotation, row, end);
    let (mut grade, mut matched, mut words, mut others, mut distinct) =
        (Json::Null, None, None, None, Json::Null);
    if let (None, Some(row)) = (reason, row) {
        let row_id = row.as_str().unwrap_or_default();
        let sql = "SELECT text FROM rows WHERE row_id = ?";
        let held = rows_of(connection, sql, &[Sql::Text(row_id.to_string())])?;
        let text = held.first().and_then(|r| field(r, "text").as_str());
        let (found, others_found) = grade_row(
            connection,
            &quotation.text,
            row_id,
            text.unwrap_or_default(),
        )?;
        grade = Json::str(found.grade);
        (matched, words) = (found.matched_words, Some(found.quote_words));
        others = others_found;
        distinct = distinct_of(&found, others_found);
    }
    report.extend([
        ("grade".into(), grade),
        ("reason".into(), text_or_null(reason.map(str::to_string))),
        ("matched_words".into(), count(matched)),
        ("quote_words".into(), count(words)),
        ("other_rows".into(), count(others)),
        ("distinct".into(), distinct),
    ]);
    Ok(object(report))
}

/// A draft's quotations, and each citation's end with the sentence around it.
struct DraftQuotes {
    all: Vec<Quotation>,
    citations: Vec<(usize, (usize, usize))>,
}

/// Each quotation in the sentence around a citation ending at `end`, compared with `row`
/// if there is one. A citation inside a quotation is what is quoted, not its source, so
/// it does not answer for that quotation if another citation beside it can; if none can,
/// the quotation is listed uncompared rather than lost.
fn sentence_quotations(
    connection: &Connection,
    draft: &str,
    end: usize,
    quotes: &DraftQuotes,
    row: Option<&Json>,
) -> Result<Json, Exc> {
    if quotes.all.is_empty() {
        return Ok(Json::Array(Vec::new()));
    }
    let sentence = sentence_around(draft, end);
    let answered = |q: &Quotation| {
        let outside = |c: &&(usize, (usize, usize))| !q.surrounds(c.0) && q.in_sentence(c.1);
        quotes.citations.iter().find(outside).is_some()
    };
    let near = quotes.all.iter().filter(|q| q.in_sentence(sentence));
    let reports = near
        .filter(|q| !(q.surrounds(end) && answered(q)))
        .map(|q| quotation_report(connection, q, row, end));
    Ok(Json::Array(reports.collect::<Result<_, _>>()?))
}

/// The report object for one citation: the outcome, then the rows and build behind it.
fn citation_report(
    connection: &Connection,
    draft: &str,
    item: FoundCitation,
    own: Option<&str>,
    names: &BTreeMap<String, String>,
    digest: &Json,
    quotes: &DraftQuotes,
) -> Result<Json, Exc> {
    let (kind, key, act, citation, end) = item;
    let (target, reason) = citation_target(draft, end, act.as_deref(), own, names);
    let (rows, held) = match &target {
        Some(rid) => cited_rows(connection, rid, &kind, &key)?,
        None => (Vec::new(), None),
    };
    let from_title = |name: &str| held.as_ref().map_or(Json::Null, |t| field(t, name).clone());
    let range = pyre::re(&format!(r"\s+to\s+(?:{LABEL})"), 0);
    let outcome = citation_outcome(
        range.match_at(draft, end).is_some(),
        reason.is_some(),
        rows.len(),
        from_title("version_is_current").truthy(),
        kind == "section",
        act.is_none(),
    );
    let newlines = draft.as_bytes()[..end].iter().filter(|b| **b == b'\n');
    let reason = reason.or((outcome == "ambiguous").then_some("several rows share this number"));
    let row_ids: Vec<Json> = rows.iter().map(|r| field(r, "row_id").clone()).collect();
    let graded = matches!(outcome, "resolved" | "assumed") && kind == "section";
    let row = row_ids.first().filter(|_| graded);
    let quotations = sentence_quotations(connection, draft, end, quotes, row)?;
    // The verbatim text of the one row a section citation reached.
    let text = rows
        .first()
        .filter(|_| graded)
        .map_or(Json::Null, |r| field(r, "text").clone());
    let nearest = match &target {
        Some(rid) if outcome == "not_held" && kind == "section" => {
            Json::Array(nearest_sections(connection, rid, &key)?)
        }
        _ => Json::Null,
    };
    let mut fields = vec![
        ("line".into(), Json::Int((newlines.count() + 1).into())),
        ("citation".into(), Json::str(&citation)),
        ("kind".into(), Json::str(&kind)),
        ("key".into(), Json::str(&key)),
        ("cited_act".into(), text_or_null(act)),
        ("outcome".into(), Json::str(outcome)),
        ("reason".into(), text_or_null(reason.map(str::to_string))),
        ("register_id".into(), text_or_null(target)),
        ("rows".into(), Json::Array(row_ids)),
        ("text".into(), text),
        ("nearest".into(), nearest),
        ("quotations".into(), quotations),
    ];
    for name in TITLE_FIELDS {
        fields.push((name.into(), from_title(name)));
    }
    fields.push(("sources_sha256".into(), digest.clone()));
    Ok(object(fields))
}

/// One object per citation in `draft`, in its order, with the outcome and the rows and
/// build it resolved against. `title` is what a citation naming no title means; without
/// it that citation is `ambiguous`.
pub fn cite_check(
    connection: &Connection,
    draft: &str,
    title: Option<&str>,
) -> Result<Vec<Json>, Exc> {
    let own = given_title(connection, title)?;
    let names = title_names(connection)?;
    let digest = sources_digest(connection)?;
    let found = citations(draft);
    let all = quotes::quotations(draft);
    let spans = |item: &FoundCitation| (item.4, sentence_around(draft, item.4));
    let spans = found.iter().filter(|_| !all.is_empty()).map(spans);
    let draft_quotes = DraftQuotes {
        citations: spans.collect(),
        all,
    };
    found
        .into_iter()
        .map(|item| {
            let own = own.as_deref();
            citation_report(connection, draft, item, own, &names, &digest, &draft_quotes)
        })
        .collect()
}

fn source_line(connection: &Connection) -> Result<String, Exc> {
    let meta = rows_of(connection, "SELECT key, value FROM meta", &[])?;
    let lookup = |key: &str| -> Option<Json> {
        meta.iter()
            .rev()
            .find(|row| field(row, "key").as_str() == Some(key))
            .map(|row| field(row, "value").clone())
    };
    let retrieved =
        lookup("corpus_retrieved").map_or_else(|| "None".to_string(), |v| pyvalue::py_str(&v));
    let digest = lookup("sources_sha256").map_or_else(String::new, |v| pyvalue::py_str(&v));
    let short: String = digest.chars().take(16).collect();
    let notice = match index_source(connection)? {
        None => NOTICE.to_string(),
        Some(source) => source.notice(&retrieved),
    };
    Ok(format!(
        "Index of the corpus retrieved {retrieved}, sources.json sha256 {short}. {notice}"
    ))
}

/// A hit's provenance line; a state title has an as-at date, not a compilation.
fn describe(hit: &Json, state_title: bool) -> String {
    let s = |k: &str| pyvalue::py_str(hit.get(k).unwrap_or(&Json::Null));
    if state_title {
        return format!(
            "{}, as at {}, retrieved {}\n   {}",
            s("row_id"),
            s("compilation_date"),
            s("retrieved"),
            s("register_page")
        );
    }
    let state = if hit.get("version_is_current").is_some_and(Json::truthy) {
        "current"
    } else {
        "not current"
    };
    format!(
        "{}, compilation {} ({}), {state}, retrieved {}\n   {}",
        s("row_id"),
        s("compilation_number"),
        s("compilation_date"),
        s("retrieved"),
        s("register_page")
    )
}

const AS_AT_HELP: &str =
    "answer only from titles the build holds for this YYYY-MM-DD; refuse the others";

fn parser() -> Parser {
    Parser::new("python -m fadden search")
        .description(
            "Build and query a local full-text, definitions and reference index. \
             search cite-check INDEX DRAFT checks a draft's citations against one, and \
             search quote INDEX ROW_ID QUOTATION grades a quotation against one row.",
        )
        .subcommands("command")
        .sub("build", Some("index a finished corpus root"), |p| {
            p.arg(Action::positional("corpus_root"))
                .arg(
                    Action::option("--out")
                        .required()
                        .help("new index file; must not exist"),
                )
                .arg(
                    Action::append("--rulings", crate::argparse::Value::None)
                        .metavar("RUN_DIR")
                        .help("a rulings run directory whose section references refs lists; repeat for each run"),
                )
        })
        .sub(
            "query",
            Some("rank rows that contain every word, in any English form"),
            query_arguments,
        )
        .sub("define", Some("look up a defined term"), |p| {
            p.arg(Action::positional("index"))
                .arg(Action::positional("term").one_or_more())
                .arg(Action::option("--title").help("look only in this register id"))
                .arg(Action::option("--as-at").metavar("DATE").help(AS_AT_HELP))
                .arg(Action::flag("--follow").help("also list the provisions each definition cites"))
                .arg(limit_option(20))
                .arg(max_chars_option())
                .arg(Action::flag("--json"))
                .arg(source_option())
        })
        .sub(
            "refs",
            Some("sections a row cites and rows citing it"),
            |p| {
                p.arg(Action::positional("index"))
                    .arg(Action::positional("row_id"))
                    .arg(
                        Action::flag("--terms")
                            .help("also list the asterisked defined terms the row uses"),
                    )
                    .arg(limit_option(100).help("at most this many entries in each list"))
                    .arg(max_chars_option())
                    .arg(Action::flag("--json"))
                    .arg(source_option())
            },
        )
}

/// `--source`: the legislation the index must hold; Commonwealth unless named.
fn source_option() -> Action {
    let mut codes = vec![COMMONWEALTH];
    codes.extend(state::SOURCES.iter().map(|s| s.code));
    Action::option("--source")
        .default(crate::argparse::Value::Str(COMMONWEALTH.to_string()))
        .choices(&codes)
        .help("the index's source; an index of another source is refused")
}

/// `--limit 1-100`, defaulting to `default`.
fn limit_option(default: i64) -> Action {
    let limits: Vec<String> = (1..=100).map(|n| n.to_string()).collect();
    let limits: Vec<&str> = limits.iter().map(String::as_str).collect();
    Action::option("--limit")
        .int()
        .default(crate::argparse::Value::Int(default))
        .choices(&limits)
        .metavar("1-100")
}

fn max_chars_option() -> Action {
    Action::option("--max-chars")
        .int()
        .default(crate::argparse::Value::Int(MAX_CHARS))
        .metavar("N")
        .help("withhold the rows past N characters of JSON, 1000 to 100000")
}

fn query_arguments(parser: Parser) -> Parser {
    parser
        .arg(Action::positional("index"))
        .arg(Action::positional("words").one_or_more())
        .arg(Action::option("--title").help("search only this register id"))
        .arg(Action::option("--as-at").metavar("DATE").help(AS_AT_HELP))
        .arg(Action::flag("--exact").help("match only the words as typed"))
        .arg(Action::flag("--stem").help("rank by English word forms alone"))
        .arg(limit_option(10))
        .arg(max_chars_option())
        .arg(Action::flag("--json").help("print one object per hit"))
        .arg(source_option())
}

/// Apart from `parser`, whose usage line is frozen by the Python parity fixtures.
fn cite_check_parser() -> Parser {
    Parser::new("python -m fadden search cite-check")
        .description("Report how each citation in a UTF-8 draft resolves against an index, and how any quotation in its sentence compares with the row reached.")
        .arg(Action::positional("index"))
        .arg(Action::positional("draft"))
        .arg(Action::option("--title").help("register id to check a citation naming no Act against; it is reported as assumed, never resolved"))
        .arg(
            Action::option("--as-at")
                .metavar("DATE")
                .help("report each cited title's status on this YYYY-MM-DD; any status but captured exits 1"),
        )
        .arg(Action::flag("--json").help("print one object per citation"))
}

/// Apart from `parser` for the same reason as `cite_check_parser`.
fn quote_parser() -> Parser {
    Parser::new("python -m fadden search quote")
        .description(
            "Grade a quotation against one row of an index: exact, normalised, partial or none.",
        )
        .arg(Action::positional("index"))
        .arg(Action::positional("row_id").help("the row the quotation is said to come from"))
        .arg(
            Action::positional("quotation").help("the quoted words, without their quotation marks"),
        )
        .arg(Action::flag("--json").help("print the result as one object"))
}

fn list(args: &crate::argparse::Namespace, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(crate::argparse::Value::List(items)) => items.clone(),
        _ => Vec::new(),
    }
}

pub fn main(argv: &[String], run: &mut Run) -> Result<i32, Exit> {
    let outcome = match argv.first().map(String::as_str) {
        Some("cite-check") => execute_cite_check(&cite_check_parser().parse(&argv[1..])?, run),
        Some("quote") => execute_quote(&quote_parser().parse(&argv[1..])?, run),
        _ => execute(&parser().parse(argv)?, run),
    };
    Ok(match outcome {
        Ok(code) => code,
        // `except (SearchIndexError, ValueError, OSError, sqlite3.Error)`.
        Err(error @ (Exc::Value { .. } | Exc::UnicodeEncode(_) | Exc::Os(_) | Exc::Sqlite(_))) => {
            run.eprint(&format!("error: {error}"));
            1
        }
        Err(error) => run.uncaught(&error),
    })
}

/// Up to 5 row ids, then how many more.
fn shown_rows(rows: &[Json]) -> String {
    let mut shown: Vec<String> = rows
        .iter()
        .take(5)
        .map(|r| pyvalue::py_str(r.at("row_id")))
        .collect();
    if rows.len() > 5 {
        shown.push(format!("and {} more", rows.len() - 5));
    }
    shown.join(", ")
}

fn outcome_of(item: &Json) -> String {
    pyvalue::py_str(item.at("outcome"))
}

/// A citation's quotations.
fn quotations_of(item: &Json) -> &[Json] {
    match item.at("quotations") {
        Json::Array(found) => found,
        _ => &[],
    }
}

fn print_quotation(quotation: &Json, run: &mut Run) {
    let s = |key: &str| pyvalue::py_str(quotation.at(key));
    let quote = crate::py::split_whitespace(&s("quote")).join(" ");
    let line = match quotation.at("grade").as_str() {
        Some(grade @ ("exact" | "normalised")) => {
            let distinct = if quotation.at("distinct").truthy() {
                "distinct"
            } else {
                "not distinct"
            };
            format!(
                "  quotation {grade}, {distinct}, {} other rows hold it: {quote}",
                s("other_rows")
            )
        }
        Some("partial") => format!(
            "  quotation partial, longest run {} of {} words: {quote}",
            s("matched_words"),
            s("quote_words")
        ),
        Some(grade) => format!("  quotation {grade}: {quote}"),
        None => format!("  quotation not compared, {}: {quote}", s("reason")),
    };
    run.print(&line);
}

fn read_draft(path: &str) -> Result<String, Exc> {
    let unreadable = |e: OsError| refused(format!("cannot read the draft ({})", strerror(&e)));
    let bytes = read_all(crate::pyio::open(path).map_err(unreadable)?, path).map_err(unreadable)?;
    String::from_utf8(bytes).map_err(|_| refused("the draft is not UTF-8 text"))
}

/// A citation's standing on the date, when it has one.
fn standing_line(standing: &Json) -> Option<String> {
    let s = |key: &str| pyvalue::py_str(standing.at(key));
    if matches!(standing, Json::Null) {
        return None;
    }
    let next = match standing.at("next_step") {
        Json::Null => String::new(),
        step => format!(": {}", pyvalue::py_str(step)),
    };
    Some(format!("  as at {}: {}{next}", s("date"), s("status")))
}

/// The quotation totals: each quotation once, and each comparison with a citation.
fn print_quotation_totals(report: &[Json], run: &mut Run) {
    let pairs: Vec<&Json> = report.iter().flat_map(quotations_of).collect();
    if pairs.is_empty() {
        return;
    }
    let count = |grade: Option<&str>| {
        pairs
            .iter()
            .filter(|q| q.at("grade").as_str() == grade)
            .count()
    };
    let mut parts: Vec<String> = GRADES
        .iter()
        .map(|grade| format!("{} {grade}", count(Some(grade))))
        .collect();
    parts.push(format!("{} not compared", count(None)));
    parts.retain(|part| !part.starts_with("0 "));
    let quotes: BTreeSet<String> = pairs
        .iter()
        .map(|q| pyvalue::py_str(q.at("start")))
        .collect();
    run.print(&format!(
        "{} quotations, {} comparisons with citations: {}.",
        quotes.len(),
        pairs.len(),
        parts.join(", ")
    ));
}

fn print_citations(report: &[Json], connection: &Connection, run: &mut Run) -> Result<(), Exc> {
    for item in report {
        let s = |k: &str| pyvalue::py_str(item.at(k));
        let why = item.at("reason").as_str().unwrap_or_default();
        let rows = match item.at("rows") {
            Json::Array(ids) => ids
                .iter()
                .map(|id| object(vec![("row_id".into(), id.clone())]))
                .collect(),
            _ => Vec::new(),
        };
        run.print(&format!(
            "line {}: {}: {}: {}",
            s("line"),
            s("outcome"),
            s("citation"),
            format!("{} {why}", shown_rows(&rows)).trim()
        ));
        if let Json::Array(nearest) = item.at("nearest")
            && !nearest.is_empty()
        {
            let held: Vec<String> = nearest.iter().map(pyvalue::py_str).collect();
            run.print(&format!("  nearest held sections: {}", held.join(", ")));
        }
        if let Some(line) = standing_line(item.at("as_at")) {
            run.print(&line);
        }
        for quotation in quotations_of(item) {
            print_quotation(quotation, run);
        }
    }
    print_quotation_totals(report, run);
    let counted: Vec<String> = OUTCOMES
        .iter()
        .map(|name| {
            let count = report.iter().filter(|i| outcome_of(i) == *name).count();
            format!("{count} {name}")
        })
        .filter(|part| !part.starts_with("0 "))
        .collect();
    run.print(&format!(
        "{} citations: {}. {CITE_CHECK_NOTICE}",
        report.len(),
        counted.join(", ")
    ));
    if report.iter().any(|item| item.get("as_at").is_some()) {
        run.print(as_at::CAPTURED_MEANS);
    }
    run.print(&source_line(connection)?);
    Ok(())
}

fn execute_cite_check(args: &crate::argparse::Namespace, run: &mut Run) -> Result<i32, Exc> {
    let connection = open_index(&args.str("index").unwrap_or_default())?;
    check_source(&connection, COMMONWEALTH, "cite-check")?;
    let draft = read_draft(&args.str("draft").unwrap_or_default())?;
    let title = args.str("title");
    let (report, refusals) = match args.str("as_at") {
        Some(date) => cite_check_as_at(&connection, &draft, title.as_deref(), &date)?,
        None => (
            cite_check(&connection, &draft, title.as_deref())?,
            Vec::new(),
        ),
    };
    if args.flag("json") {
        for item in &report {
            run.print_points(&dumps_line(item, false))?;
        }
    } else {
        print_citations(&report, &connection, run)?;
    }
    print_refusals(&refusals, args.flag("json"), run)?;
    let clean = report.iter().all(|item| {
        let standing = item.at("as_at");
        let unheld = !matches!(standing, Json::Null)
            && standing.at("status").as_str() != Some(as_at::CAPTURED);
        !unheld
            && CLEAN_OUTCOMES.contains(&outcome_of(item).as_str())
            && quotations_of(item).iter().all(|quotation| {
                let grade = quotation.at("grade").as_str();
                grade.is_some_and(|grade| CLEAN_GRADES.contains(&grade))
            })
    });
    Ok(i32::from(!clean || !refusals.is_empty()))
}

/// Exits 0 only if the row holds the quotation, `exact` or `normalised`.
fn execute_quote(args: &crate::argparse::Namespace, run: &mut Run) -> Result<i32, Exc> {
    let connection = open_index(&args.str("index").unwrap_or_default())?;
    check_source(&connection, COMMONWEALTH, "quote")?;
    let row_id = args.str("row_id").unwrap_or_default();
    let report = quote(
        &connection,
        &row_id,
        &args.str("quotation").unwrap_or_default(),
    )?;
    let held = report
        .at("grade")
        .as_str()
        .is_some_and(|grade| CLEAN_GRADES.contains(&grade));
    if args.flag("json") {
        run.print_points(&dumps_line(&report, false))?;
    } else {
        let s = |key: &str| pyvalue::py_str(report.at(key));
        let state = if report.at("version_is_current").truthy() {
            "current"
        } else {
            "not current"
        };
        run.print(&format!(
            "{row_id}: compilation {} ({}), {state}, retrieved {}",
            s("compilation_number"),
            s("compilation_date"),
            s("retrieved")
        ));
        print_quotation(&report, run);
        run.print(QUOTE_NOTICE);
        run.print(&source_line(&connection)?);
    }
    Ok(i32::from(!held))
}

fn print_refusals(refusals: &[Json], json: bool, run: &mut Run) -> Result<(), Exc> {
    for refusal in refusals {
        if json {
            run.print_points(&dumps_line(refusal, false))?;
            continue;
        }
        let s = |key: &str| pyvalue::py_str(refusal.at(key));
        let standing = refusal.at("as_at");
        run.print(&format!(
            "refused {} ({}): {} on {}, {} matching rows withheld: {}",
            s("register_id"),
            s("act"),
            pyvalue::py_str(standing.at("status")),
            pyvalue::py_str(standing.at("date")),
            s("matching_rows"),
            pyvalue::py_str(standing.at("next_step"))
        ));
    }
    Ok(())
}

/// Prints a dated report's refusals, capped after the rows, and its summary.
fn print_dated(
    report: &DatedReport,
    json: bool,
    caps: &mut Caps,
    run: &mut Run,
) -> Result<(), Exc> {
    let (refusals, withheld) = caps.keep(report.refusals.clone());
    print_refusals(&refusals, json, run)?;
    caps.marker("refusals", refusals.len(), withheld, json, run)?;
    if json {
        return run.print_points(&dumps_line(&report.summary, false));
    }
    let summary = report.summary.at("as_at_summary");
    let titles = as_at::STATUSES.map(|status| {
        format!(
            "{} {status}",
            pyvalue::py_str(summary.at("titles").at(status))
        )
    });
    run.print(&format!(
        "Titles against {}: {}. {}",
        pyvalue::py_str(summary.at("date")),
        titles.join(", "),
        as_at::CAPTURED_MEANS
    ));
    Ok(())
}

/// Exit 1 if a dated report refused a title, so no caller reads a refusal as no rows.
fn refused_code(dated: Option<&DatedReport>) -> i32 {
    i32::from(dated.is_some_and(|report| !report.refusals.is_empty()))
}

/// The default `--max-chars`, and the least and most it accepts.
const MAX_CHARS: i64 = 20_000;
const MAX_CHARS_RANGE: std::ops::RangeInclusive<i64> = 1_000..=100_000;

/// What `--limit` and `--max-chars` let a command print: rows in order, while
/// each list stays within the limit and the JSON form of every row kept so far
/// stays within the character budget, shared by all of a command's lists. Once
/// a row passes the budget, every later row and list is withheld.
struct Caps {
    limit: i64,
    max_chars: i64,
    left: usize,
    spent: bool,
}

impl Caps {
    fn of(args: &crate::argparse::Namespace) -> Result<Self, Exc> {
        let int = |key: &str| match args.get(key) {
            Some(crate::argparse::Value::Int(n)) => *n,
            _ => MAX_CHARS,
        };
        let max_chars = int("max_chars");
        if !MAX_CHARS_RANGE.contains(&max_chars) {
            return Err(refused(format!(
                "--max-chars must be from {} to {}",
                MAX_CHARS_RANGE.start(),
                MAX_CHARS_RANGE.end()
            )));
        }
        Ok(Self {
            limit: int("limit"),
            max_chars,
            left: usize::try_from(max_chars).unwrap_or_default(),
            spent: false,
        })
    }

    /// The rows kept, and how many were withheld.
    fn keep(&mut self, rows: Vec<Json>) -> (Vec<Json>, usize) {
        let total = rows.len();
        let mut kept = Vec::new();
        for row in rows {
            // Code points, as `dumps_line` gives them.
            let size = dumps_line(&row, false).len();
            self.spent = self.spent || size > self.left;
            if self.spent || i64::try_from(kept.len()).unwrap_or(i64::MAX) >= self.limit {
                break;
            }
            self.left -= size;
            kept.push(row);
        }
        let withheld = total - kept.len();
        (kept, withheld)
    }

    fn past(&self) -> String {
        format!(
            "past --limit {} or --max-chars {}",
            self.limit, self.max_chars
        )
    }

    /// The marker after a list that lost rows: a JSON line, or a text line.
    fn marker(
        &self,
        list: &str,
        shown: usize,
        withheld: usize,
        json: bool,
        run: &mut Run,
    ) -> Result<(), Exc> {
        if withheld == 0 {
            return Ok(());
        }
        if !json {
            run.print(&format!(
                "[truncated] {shown} {list} shown, {withheld} withheld: {}",
                self.past()
            ));
            return Ok(());
        }
        let count = |n: usize| Json::Int(n.into());
        let marker = object(vec![
            ("truncated".into(), Json::Bool(true)),
            ("list".into(), Json::str(list)),
            ("shown".into(), count(shown)),
            ("withheld".into(), count(withheld)),
            ("limit".into(), Json::Int(self.limit.into())),
            ("max_chars".into(), Json::Int(self.max_chars.into())),
        ]);
        run.print_points(&dumps_line(&marker, false))
    }

    /// `refs`: caps each list in the report, in its order, and adds `truncated`
    /// naming the lists that lost entries. A report that lost none is unchanged.
    fn report(&mut self, mut report: Json) -> (Json, Vec<(String, usize)>) {
        let mut withheld = Vec::new();
        if let Json::Object(pairs) = &mut report {
            for (key, value) in pairs {
                if let Json::Array(items) = value {
                    let (kept, lost) = self.keep(std::mem::take(items));
                    *items = kept;
                    if lost > 0 {
                        withheld.push((key.as_str().unwrap_or_default().to_string(), lost));
                    }
                }
            }
        }
        if withheld.is_empty() {
            return (report, withheld);
        }
        let lists = withheld
            .iter()
            .map(|(name, lost)| (name.clone(), Json::Int((*lost).into())))
            .collect();
        let truncated = object(vec![
            ("withheld".into(), object(lists)),
            ("limit".into(), Json::Int(self.limit.into())),
            ("max_chars".into(), Json::Int(self.max_chars.into())),
        ]);
        (with_field(report, "truncated", truncated), withheld)
    }
}

type QueryFn = fn(&Connection, &[String], Option<&str>, i64) -> Result<Vec<Json>, Exc>;
type DatedQueryFn = fn(&Connection, &[String], Option<&str>, i64, &str) -> Result<DatedReport, Exc>;

fn query_results(
    args: &crate::argparse::Namespace,
    connection: &Connection,
) -> Result<(Vec<Json>, Option<DatedReport>), Exc> {
    let title = args.str("title");
    let limit = match args.get("limit") {
        Some(crate::argparse::Value::Int(n)) => *n,
        _ => 10,
    };
    let words = list(args, "words");
    if args.flag("stem") && args.flag("exact") {
        return Err(refused("--stem and --exact cannot be combined"));
    }
    let (plain, dated_by): (QueryFn, DatedQueryFn) = if args.flag("stem") {
        (query_stemmed, query_stemmed_as_at)
    } else if args.flag("exact") {
        (query, query_as_at)
    } else {
        (query_word_forms, query_word_forms_as_at)
    };
    let dated = args
        .str("as_at")
        .map(|date| dated_by(connection, &words, title.as_deref(), limit, &date))
        .transpose()?;
    let hits = match &dated {
        Some(report) => report.hits.clone(),
        None => plain(connection, &words, title.as_deref(), limit)?,
    };
    Ok((hits, dated))
}

fn execute_query(
    args: &crate::argparse::Namespace,
    connection: &Connection,
    run: &mut Run,
) -> Result<i32, Exc> {
    let mut caps = Caps::of(args)?;
    let (hits, dated) = query_results(args, connection)?;
    let (hits, withheld) = caps.keep(hits);
    if args.flag("json") {
        for hit in &hits {
            run.print_points(&dumps_line(hit, false))?;
        }
        caps.marker("rows", hits.len(), withheld, true, run)?;
        if let Some(report) = &dated {
            print_dated(report, true, &mut caps, run)?;
        }
        return Ok(refused_code(dated.as_ref()));
    }
    if args.flag("stem") {
        run.print(&format!("Stemmed search. {STEM_NOTICE}"));
    } else if hits.iter().any(|hit| hit.get("stemmed").is_some()) {
        run.print(&format!("Rows marked * matched a word form. {STEM_NOTICE}"));
    }
    for (number, hit) in hits.iter().enumerate() {
        let s = |k: &str| pyvalue::py_str(hit.get(k).unwrap_or(&Json::Null));
        let mark = if !args.flag("stem") && hit.get("stemmed").is_some() {
            "* "
        } else {
            ""
        };
        run.print(&format!(
            "{}. {mark}{}: {}",
            number + 1,
            s("act"),
            s("heading")
        ));
        let state_title = index_source(connection)?.is_some();
        run.print(&format!("   {}", describe(hit, state_title)));
        run.print(&format!(
            "   {}",
            crate::py::split_whitespace(&s("snippet")).join(" ")
        ));
    }
    caps.marker("rows", hits.len(), withheld, false, run)?;
    if let Some(report) = &dated {
        print_dated(report, false, &mut caps, run)?;
    }
    run.print(&format!(
        "{} rows. {}",
        hits.len(),
        source_line(connection)?
    ));
    Ok(refused_code(dated.as_ref()))
}

/// What `define --json` says of a term it found no definition paragraph for.
const UNDEFINED_NOTE: &str = "No definition paragraph found. The term may be defined another way, \
in a title this index does not hold or in a row it does not read as definitions; this is not a \
finding that it has its ordinary meaning.";

fn execute_define(
    args: &crate::argparse::Namespace,
    connection: &Connection,
    run: &mut Run,
) -> Result<i32, Exc> {
    let mut caps = Caps::of(args)?;
    let title = args.str("title");
    let term = list(args, "term").join(" ");
    let dated = args
        .str("as_at")
        .map(|date| define_as_at(connection, &term, title.as_deref(), &date))
        .transpose()?;
    let found = match &dated {
        Some(report) => report.hits.clone(),
        None => define(connection, &term, title.as_deref())?,
    };
    let found = if args.flag("follow") {
        follow_definitions(connection, found)?
    } else {
        found
    };
    // Only a lookup that refused no title can say the term itself is not defined.
    let key = term_key(&term);
    let undefined = refused_code(dated.as_ref()) == 0
        && !found
            .iter()
            .any(|item| term_key(&pyvalue::py_str(item.at("term"))) == key);
    let longer_terms = found.len();
    let (found, withheld) = caps.keep(found);
    if args.flag("json") {
        if undefined {
            let status = object(vec![
                ("term".into(), Json::str(&term)),
                ("defined".into(), Json::Bool(false)),
                (
                    "register_id".into(),
                    to_json(optional_rid(connection, title.as_deref())?),
                ),
                ("longer_terms".into(), Json::Int(longer_terms.into())),
                ("note".into(), Json::str(UNDEFINED_NOTE)),
            ]);
            run.print_points(&dumps_line(&status, false))?;
        }
        for item in &found {
            run.print_points(&dumps_line(item, false))?;
        }
        caps.marker("rows", found.len(), withheld, true, run)?;
        if let Some(report) = &dated {
            print_dated(report, true, &mut caps, run)?;
        }
        return Ok(refused_code(dated.as_ref()));
    }
    if undefined && longer_terms > 0 {
        run.print(&format!(
            "{term} has no definition of its own here; the defined terms below begin with it."
        ));
    }
    for item in &found {
        let s = |k: &str| pyvalue::py_str(item.get(k).unwrap_or(&Json::Null));
        run.print(&format!("{} ({}, {})", s("term"), s("act"), s("heading")));
        let state_title = index_source(connection)?.is_some();
        run.print(&format!("   {}", describe(item, state_title)));
        run.print(&format!("   {}", s("definition").replace("\n\n", "\n   ")));
        print_refers_to(item, run);
    }
    caps.marker("rows", found.len(), withheld, false, run)?;
    if undefined && longer_terms == 0 {
        run.print(
            "No definition paragraph found. The term may be defined another way or in a row this \
             index does not read as definitions; this is not a finding that it has its ordinary meaning.",
        );
    }
    if let Some(report) = &dated {
        print_dated(report, false, &mut caps, run)?;
    }
    run.print(&source_line(connection)?);
    Ok(refused_code(dated.as_ref()))
}

fn describe_reference(reference: &Json) -> String {
    let s = |v: &Json| pyvalue::py_str(v);
    let rows = match reference.at("rows") {
        Json::Array(rows) => rows.clone(),
        _ => Vec::new(),
    };
    let joined = shown_rows(&rows);
    let place = if !joined.is_empty() {
        joined
    } else if matches!(reference.at("register_id"), Json::Null) {
        "not in this index".into()
    } else {
        format!("no row for that {}", s(reference.at("kind")))
    };
    format!(
        "  cites {} {} ({}): {place}",
        s(reference.at("kind")),
        s(reference.at("key")),
        s(reference.at("citation"))
    )
}

/// `define --follow`'s text lines for one definition.
fn print_refers_to(item: &Json, run: &mut Run) {
    if let Json::Array(cited) = item.at("refers_to") {
        for reference in cited {
            run.print(&format!("  {}", describe_reference(reference)));
        }
    }
}

/// `refs --terms`'s text lines.
fn print_terms(report: &Json, run: &mut Run) {
    let Json::Array(terms) = report.at("terms") else {
        return;
    };
    let s = |v: &Json| pyvalue::py_str(v);
    for term in terms {
        let Json::Array(definitions) = term.at("definitions") else {
            continue;
        };
        let places: Vec<String> = definitions
            .iter()
            .map(|d| format!("{} ({})", s(d.at("row_id")), s(d.at("heading"))))
            .collect();
        let qualifier = match term.at("match").as_str() {
            Some("singular") => ", read in the singular",
            Some("without qualifier") => ", matched without its qualifier",
            _ => "",
        };
        run.print(&format!(
            "  uses *{}{qualifier}: defined in {}",
            s(term.at("term")),
            places.join(", ")
        ));
    }
    let unresolved = s(report.at("unresolved_terms"));
    if unresolved != "0" {
        run.print(&format!(
            "  {unresolved} asterisked terms matched no definition this index reads"
        ));
    }
}

fn execute_refs(
    args: &crate::argparse::Namespace,
    connection: &Connection,
    run: &mut Run,
) -> Result<i32, Exc> {
    let mut caps = Caps::of(args)?;
    let row_id = args.str("row_id").unwrap_or_default();
    let mut report = refs(connection, &row_id)?;
    if args.flag("terms") {
        let (terms, unresolved) = defined_terms(connection, &row_id)?;
        report = with_field(report, "terms", Json::Array(terms));
        report = with_field(report, "unresolved_terms", Json::Int(unresolved.into()));
    }
    let (report, withheld) = caps.report(report);
    if args.flag("json") {
        run.print_points(&dumps_line(&report, false))?;
        return Ok(0);
    }
    let s = |v: &Json| pyvalue::py_str(v);
    run.print(&format!(
        "{}: {}",
        s(report.at("row_id")),
        s(report.at("heading"))
    ));
    if let Json::Array(cites) = report.at("cites") {
        for reference in cites {
            run.print(&describe_reference(reference));
        }
    }
    print_terms(&report, run);
    if let Json::Array(sources) = report.at("cited_by") {
        for source in sources {
            run.print(&format!(
                "  cited by {} ({}): {}",
                s(source.at("row_id")),
                s(source.at("heading")),
                s(source.at("citation"))
            ));
        }
    }
    let rulings_withheld = withheld.iter().any(|(name, _)| name == "cited_by_rulings");
    // Text output tells an index holding no rulings from one where none cites the row.
    match report.at("cited_by_rulings") {
        Json::Array(rulings) if rulings.is_empty() && !rulings_withheld => {
            run.print("  no captured ruling cites this by its Act");
            run.print(RULINGS_NOTICE);
        }
        Json::Array(rulings) => {
            for ruling in rulings {
                let withdrawn = if ruling.at("withdrawal_status").as_str() == Some("NOTICE_PRESENT")
                {
                    ", withdrawal notice captured"
                } else {
                    ""
                };
                run.print(&format!(
                    "  cited by ruling {}, paragraph {} ({}): captured {}{withdrawn}, {}",
                    s(ruling.at("title")),
                    s(ruling.at("paragraph")),
                    s(ruling.at("citation")),
                    s(ruling.at("fetched_on")),
                    s(ruling.at("source_url"))
                ));
            }
            run.print(RULINGS_NOTICE);
        }
        _ => run.print("  this index holds no rulings; build with --rulings to list them"),
    }
    if !withheld.is_empty() {
        let lists: Vec<String> = withheld
            .iter()
            .map(|(name, lost)| format!("{lost} {name}"))
            .collect();
        run.print(&format!(
            "  [truncated] withheld {}: {}",
            lists.join(", "),
            caps.past()
        ));
    }
    run.print(&source_line(connection)?);
    Ok(0)
}

const RULINGS_NOTICE: &str = "Ruling paragraphs come from the ATO Legal Database under the ATO \
copyright notice, kept in this index. A ruling \
listed cited this provision in the capture shown. Without a captured withdrawal notice its status \
is unknown, not current: check the ruling and its period on the live Legal Database before \
reliance. A citation naming no Act is not read.";

fn execute(args: &crate::argparse::Namespace, run: &mut Run) -> Result<i32, Exc> {
    let command = args.str("command").unwrap_or_default();
    if command == "build" {
        let rulings = args.list("rulings");
        let ([titles, rows, definitions, refs], [paragraphs, ruling_refs, excluded]) =
            build_with_rulings(
                &args.str("corpus_root").unwrap_or_default(),
                &args.str("out").unwrap_or_default(),
                &rulings,
            )?;
        run.print(&format!(
            "indexed {titles} titles, {rows} rows, {definitions} definitions and {refs} section references"
        ));
        if !rulings.is_empty() {
            run.print(&format!(
                "indexed {paragraphs} ruling paragraphs from {} runs with {ruling_refs} section references",
                rulings.len()
            ));
            // An excluded document's citations are absent, which is not the same as none.
            run.print(&format!(
                "excluded by the rulings stage and not indexed: {excluded} documents"
            ));
        }
        return Ok(0);
    }
    let connection = open_index(&args.str("index").unwrap_or_default())?;
    let wanted = args
        .str("source")
        .unwrap_or_else(|| COMMONWEALTH.to_string());
    check_source(&connection, &wanted, &command)?;
    if wanted != COMMONWEALTH && args.str("as_at").is_some_and(|date| !date.is_empty()) {
        return Err(refused(format!(
            "--as-at reads the Commonwealth build's compilation windows; {wanted} titles carry only the register's as-at date"
        )));
    }
    match command.as_str() {
        "query" => execute_query(args, &connection, run),
        "define" => execute_define(args, &connection, run),
        _ => execute_refs(args, &connection, run),
    }
}
