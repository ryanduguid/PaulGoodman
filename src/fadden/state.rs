// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `fadden state`: a corpus root of section rows from a state register's
//! whole-title XML, for the reviewed revenue titles in `SOURCES`.
//!
//! Each register's terms are recorded in docs/scope.md; only a register whose
//! terms allow a derived corpus with attribution is listed here. The stage
//! reads the register's robots.txt first and refuses a path it disallows, sends
//! one request at a time with a pause between requests (the robots.txt crawl
//! delay when that is longer) and a generic User-Agent naming the tool only.
//! Every title must arrive, parse and name the title the list expects, or
//! nothing is written. The output directory must not exist; it holds the exact
//! bytes each title returned (`raw/`), `markdown/<id>/` and a `sources.json`
//! whose `state_register` names the source, which `search build` reads.
use sha2::{Digest, Sha256};

use super::http::{self, Failure, Http};
use super::{Exc, Run};
use crate::argparse::{Action, Exit, Get, Parser};
use crate::pyjson::{Json, obj};
use crate::pypath::PyPath;
use crate::pyvalue::{dumps_line, dumps_sorted};

pub use super::state_robots::Robots;
use super::state_xml::Item;
pub use super::state_xml::{Parsed, Row, parse};

/// The User-Agent: the tool's name, no person or address.
pub const AGENT: &str = "au-legislation-corpus/1.0";
/// Seconds between requests, unless robots.txt asks for longer.
pub const REQUEST_SPACING: f64 = 5.0;
const MAX_XML_BYTES: usize = 32 * 1024 * 1024;
const MAX_ROBOTS_BYTES: usize = 512 * 1024;
const LICENCE: &str = "CC BY 4.0";
const LICENCE_URL: &str = "https://creativecommons.org/licenses/by/4.0/";

/// A state register the stage reads, with the attribution its terms require.
pub struct Source {
    /// The `--source` value and `sources.json`'s `source`.
    pub code: &'static str,
    pub name: &'static str,
    pub host: &'static str,
    pub website: &'static str,
    pub terms_url: &'static str,
    pub copyright: &'static str,
    /// The attribution the terms give for changed content; `{date}` is the
    /// day the title was downloaded.
    pub attribution: &'static str,
    /// Reviewed register ids and the title each must carry.
    pub titles: &'static [(&'static str, &'static str)],
}

/// Queensland: terms at <https://www.legislation.qld.gov.au/copyright>, read on
/// 24 November 2026 (docs/scope.md). Titles confirmed on the register that day;
/// no regulation under the First Home Owner Grant Act is in force.
const QLD: Source = Source {
    code: "qld",
    name: "Queensland",
    host: "www.legislation.qld.gov.au",
    website: "Queensland Legislation website",
    terms_url: "https://www.legislation.qld.gov.au/copyright",
    copyright: "© The State of Queensland (Office of the Queensland Parliamentary Counsel)",
    attribution: "Based on content from the Queensland Legislation website at {date}. \
For the latest information on Queensland Government legislation please go to \
https://www.legislation.qld.gov.au.",
    titles: &[
        ("act-2001-071", "Duties Act 2001"),
        ("sl-2023-0104", "Duties Regulation 2023"),
        ("act-2010-015", "Land Tax Act 2010"),
        ("sl-2021-0019", "Land Tax Regulation 2021"),
        ("act-1971-037", "Payroll Tax Act 1971"),
        ("sl-2019-0147", "Payroll Tax Regulation 2019"),
        (
            "act-2000-014",
            "First Home Owner Grant and Other Home Owner Grants Act 2000",
        ),
    ],
};

pub const SOURCES: [Source; 1] = [QLD];

/// The source named `code`.
pub fn source(code: &str) -> Option<&'static Source> {
    SOURCES.iter().find(|s| s.code == code)
}

impl Source {
    pub fn attribution_on(&self, date: &str) -> String {
        self.attribution.replace("{date}", date)
    }

    /// What a search over this source prints with its results.
    pub fn notice(&self, retrieved: &str) -> String {
        format!(
            "Finding aid derived from the {}'s XML, not authorised legislation or tax advice. \
             Read the authorised version on the {} before relying on a row. {}",
            self.website,
            self.website,
            self.attribution_on(retrieved)
        )
    }

    fn base(&self) -> String {
        format!("https://{}", self.host)
    }

    pub fn xml_url(&self, id: &str) -> String {
        format!("{}/view/whole/xml/inforce/current/{id}", self.base())
    }

    pub fn title_page(&self, id: &str) -> String {
        format!("{}/view/html/inforce/current/{id}", self.base())
    }

    fn anchor_page(&self, id: &str, anchor: &str) -> String {
        format!(
            "{}/view/whole/html/inforce/current/{id}#{anchor}",
            self.base()
        )
    }
}

/// A state title id: `act-YYYY-NNN` or `sl-YYYY-NNNN`.
pub fn title_id(value: &Json) -> Result<String, Exc> {
    let valid = value.as_str().filter(|s| {
        let (kind, rest) = s.split_once('-').unwrap_or_default();
        let (year, number) = rest.split_once('-').unwrap_or_default();
        matches!(kind, "act" | "sl")
            && year.len() == 4
            && (3..=4).contains(&number.len())
            && year
                .bytes()
                .chain(number.bytes())
                .all(|b| b.is_ascii_digit())
    });
    valid
        .map(str::to_string)
        .ok_or_else(|| Exc::value("invalid state register title id"))
}

pub(super) fn refused(message: impl Into<String>) -> Exc {
    Exc::class_value("fadden.state.StateError", message)
}

// ---------------------------------------------------------------- fetching

/// `url` within `http::TRIES` attempts and `limit` bytes; a rate limit, server
/// fault or transport failure is retried, any other status is not.
fn get(
    http: &mut dyn Http,
    url: &str,
    limit: usize,
) -> Result<Result<http::Response, String>, Exc> {
    let mut failure = "no attempt made".to_string();
    let mut request = http::Request::default();
    for number in 1..=http::TRIES {
        http::attempt(http, number);
        match http.get(url, Some(limit + 1), &mut request) {
            Ok(response) => return Ok(Ok(response)),
            Err(Failure::Status(code)) => {
                failure = format!("HTTP {code}");
                if code != 429 && code < 500 {
                    break;
                }
            }
            Err(Failure::Transport(name)) => failure = name,
            Err(Failure::Uncaught { class, message }) => {
                return Err(http::uncaught(class, message));
            }
        }
    }
    Ok(Err(failure))
}

/// The response body, refused when it is too large or came from another site.
fn checked(
    source: &Source,
    url: &str,
    response: http::Response,
    limit: usize,
) -> Result<Vec<u8>, Exc> {
    let expected = format!("{}/", source.base());
    if !response.url.starts_with(&expected) {
        return Err(refused(format!("{url}: answered from {}", response.url)));
    }
    if response.body.len() > limit {
        return Err(refused(format!("{url}: larger than {limit} bytes")));
    }
    Ok(response.body)
}

fn robots(http: &mut dyn Http, source: &Source) -> Result<Robots, Exc> {
    let url = format!("{}/robots.txt", source.base());
    match get(http, &url, MAX_ROBOTS_BYTES)? {
        Ok(response) => {
            let body = checked(source, &url, response, MAX_ROBOTS_BYTES)?;
            Ok(Robots::parse(&String::from_utf8_lossy(&body)))
        }
        Err(failure) if failure == "HTTP 404" => Ok(Robots::default()),
        Err(failure) => Err(refused(format!("{url}: {failure}; nothing fetched"))),
    }
}

// ---------------------------------------------------------------- output

fn s(text: &str) -> Json {
    Json::str(text)
}

fn or_null(value: Option<&str>) -> Json {
    value.map_or(Json::Null, s)
}

fn text_bytes(points: &[u32]) -> Vec<u8> {
    points
        .iter()
        .filter_map(|&cp| char::from_u32(cp))
        .collect::<String>()
        .into_bytes()
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `sections.jsonl` for one title, in the Commonwealth row model.
pub fn jsonl(source: &Source, parsed: &Parsed, retrieved: &str) -> Vec<u8> {
    let attribution = source.attribution_on(retrieved);
    let mut out = Vec::new();
    for (index, row) in parsed.rows.iter().enumerate() {
        let row_id = format!(
            "{}:{:04}:{}",
            parsed.id,
            index + 1,
            row.section.as_deref().unwrap_or("-")
        );
        let page = row.anchor.as_deref().map_or_else(
            || source.title_page(&parsed.id),
            |anchor| source.anchor_page(&parsed.id, anchor),
        );
        let value = obj(vec![
            ("register_id", s(&parsed.id)),
            ("act", s(&parsed.name)),
            ("collection", s(parsed.collection)),
            ("compilation_number", Json::Null),
            ("compilation_date", s(&parsed.as_at)),
            ("version_is_current", Json::Bool(true)),
            ("superseded_from", Json::Null),
            ("retrieved", s(retrieved)),
            ("row_id", s(&row_id)),
            ("section", or_null(row.section.as_deref())),
            ("heading", s(&row.heading)),
            ("container", or_null(row.container.as_deref())),
            ("kind", s(row.kind)),
            ("granularity", s("section")),
            ("text", s(&row.text)),
            ("source_url", s(&source.xml_url(&parsed.id))),
            ("register_page", s(&page)),
            ("state_register", s(source.code)),
            ("licence", s(LICENCE)),
            ("licence_url", s(LICENCE_URL)),
            ("copyright", s(source.copyright)),
            ("authorised", Json::Bool(false)),
            ("attribution", s(&attribution)),
            ("disclaimer", or_null(parsed.disclaimer.as_deref())),
        ]);
        out.extend(text_bytes(&dumps_line(&value, false)));
        out.push(b'\n');
    }
    out
}

/// The title's Markdown: front matter carrying the licence, attribution and
/// disclaimer, then each structural heading and row.
pub fn markdown(source: &Source, parsed: &Parsed, retrieved: &str) -> String {
    let quoted = |text: &str| text_bytes(&dumps_line(&s(text), false));
    let quoted = |text: &str| String::from_utf8(quoted(text)).unwrap_or_default();
    let mut out = vec![
        "---".to_string(),
        format!("title: {}", quoted(&parsed.name)),
        format!("register_id: {}", quoted(&parsed.id)),
        format!("state_register: {}", quoted(source.code)),
        format!("as_at: {}", quoted(&parsed.as_at)),
        format!("retrieved: {}", quoted(retrieved)),
        format!("register_page: {}", quoted(&source.title_page(&parsed.id))),
        format!("source_url: {}", quoted(&source.xml_url(&parsed.id))),
        format!("licence: {}", quoted(LICENCE)),
        format!("licence_url: {}", quoted(LICENCE_URL)),
        format!("copyright: {}", quoted(source.copyright)),
        format!("attribution: {}", quoted(&source.attribution_on(retrieved))),
    ];
    if let Some(text) = &parsed.disclaimer {
        out.push(format!("disclaimer: {}", quoted(text)));
    }
    out.extend([
        "authorised: false".to_string(),
        "---".to_string(),
        String::new(),
        format!("# {}", parsed.name),
        String::new(),
    ]);
    let hashes = |depth: usize| "#".repeat((depth + 1).min(6));
    for item in &parsed.items {
        match item {
            Item::Heading(depth, text) => {
                out.push(format!("{} {text}", hashes(*depth)));
                out.push(String::new());
            }
            Item::Row(depth, index) => {
                let row = &parsed.rows[*index];
                if row.kind == "section" {
                    out.push(format!("{} {}", hashes(*depth), row.heading));
                    out.push(String::new());
                }
                if !row.text.is_empty() {
                    out.push(row.text.clone());
                    out.push(String::new());
                }
            }
        }
    }
    out.join("\n")
}

struct Fetched {
    parsed: Parsed,
    raw: Vec<u8>,
}

fn write_new(path: &PyPath, bytes: &[u8]) -> Result<(), Exc> {
    use std::io::Write;
    let mut file = crate::pyos::create_new(&path.display(), false)?;
    file.write_all(bytes)
        .map_err(|e| crate::pyos::OsError::new(&e, None, crate::pyos::Style::Errno))?;
    Ok(())
}

fn mkdir(path: &PyPath) -> Result<(), Exc> {
    std::fs::create_dir(path.fs()).map_err(|e| {
        Exc::Os(crate::pyos::OsError::new(
            &e,
            Some(&path.display()),
            crate::pyos::Style::Native,
        ))
    })
}

/// One title, fetched after the pause, parsed and checked for personal data.
fn fetch_title(
    http: &mut dyn Http,
    source: &Source,
    rules: &Robots,
    spacing: f64,
    id: &str,
    name: &str,
) -> Result<Fetched, Exc> {
    let url = source.xml_url(id);
    let path = url.trim_start_matches(&source.base());
    if !rules.allows(path) {
        return Err(refused(format!(
            "{url}: robots.txt disallows it; nothing written"
        )));
    }
    http.sleep(spacing);
    let raw = match get(http, &url, MAX_XML_BYTES)? {
        Ok(response) => checked(source, &url, response, MAX_XML_BYTES)?,
        Err(failure) => return Err(refused(format!("{url}: {failure}; nothing written"))),
    };
    let xml =
        std::str::from_utf8(&raw).map_err(|_| refused(format!("{id}: the XML is not UTF-8")))?;
    let parsed = parse(xml, id, name)?;
    let texts: Vec<&str> = parsed
        .rows
        .iter()
        .flat_map(|row| [row.heading.as_str(), row.text.as_str()])
        .collect();
    let kinds = super::rulings::pii_findings(&texts);
    if !kinds.is_empty() {
        return Err(refused(format!(
            "{id}: personal data patterns ({}); nothing written",
            kinds.join(", ")
        )));
    }
    Ok(Fetched { parsed, raw })
}

/// A title's entry in `sources.json`.
fn title_entry(source: &Source, parsed: &Parsed, raw: &[u8], retrieved: &str) -> Json {
    let id = parsed.id.as_str();
    obj(vec![
        ("register_id", s(id)),
        ("name", s(&parsed.name)),
        ("collection", s(parsed.collection)),
        ("compilation_number", Json::Null),
        ("compilation_date", s(&parsed.as_at)),
        ("as_at", s(&parsed.as_at)),
        ("in_force", s(&parsed.in_force)),
        ("version_is_current", Json::Bool(true)),
        ("current_version_start", Json::Null),
        ("retrieved", s(retrieved)),
        ("jsonl_rows", Json::Int(parsed.rows.len().into())),
        ("register_page", s(&source.title_page(id))),
        ("source_url", s(&source.xml_url(id))),
        ("source_sha256", s(&hex(raw))),
        ("disclaimer", or_null(parsed.disclaimer.as_deref())),
    ])
}

/// Writes the fetched titles under `out` and returns the row count.
fn write_corpus(
    source: &Source,
    out: &PyPath,
    fetched: &[Fetched],
    retrieved: &str,
) -> Result<usize, Exc> {
    mkdir(out)?;
    mkdir(&out.join("raw"))?;
    mkdir(&out.join("markdown"))?;
    let mut titles = Vec::new();
    let mut total = 0;
    for Fetched { parsed, raw } in fetched {
        let id = parsed.id.as_str();
        write_new(&out.join(&format!("raw/{id}.xml")), raw)?;
        let dir = out.join(&format!("markdown/{id}"));
        mkdir(&dir)?;
        write_new(
            &dir.join("sections.jsonl"),
            &jsonl(source, parsed, retrieved),
        )?;
        write_new(
            &dir.join(&format!("{id}.md")),
            markdown(source, parsed, retrieved).as_bytes(),
        )?;
        total += parsed.rows.len();
        titles.push(title_entry(source, parsed, raw, retrieved));
    }
    let sources = obj(vec![
        ("state_register", s(source.code)),
        ("source", s(source.website)),
        ("retrieved", s(retrieved)),
        ("terms_url", s(source.terms_url)),
        ("licence", s(LICENCE)),
        ("licence_url", s(LICENCE_URL)),
        ("copyright", s(source.copyright)),
        ("attribution", s(&source.attribution_on(retrieved))),
        ("user_agent", s(AGENT)),
        ("titles", Json::Array(titles)),
    ]);
    let mut bytes = text_bytes(&dumps_sorted(&sources, Some(1)));
    bytes.push(b'\n');
    write_new(&out.join("sources.json"), &bytes)?;
    Ok(total)
}

/// Fetch, check and parse every reviewed title of `source`, then write the
/// corpus root `out`, which must not exist and whose parent must.
pub fn run(http: &mut dyn Http, source: &Source, out: &PyPath, run: &mut Run) -> Result<i32, Exc> {
    if std::fs::symlink_metadata(out.fs()).is_ok() {
        return Err(refused(format!(
            "{out} already exists; every run writes a new directory"
        )));
    }
    if !crate::pyos::isdir(&out.parent().display()) {
        return Err(refused(format!(
            "{out}: the parent directory does not exist"
        )));
    }
    let retrieved = http.today();
    let rules = robots(http, source)?;
    let spacing = rules.crawl_delay.unwrap_or(0.0).max(REQUEST_SPACING);
    let mut fetched = Vec::new();
    for (id, name) in source.titles {
        fetched.push(fetch_title(http, source, &rules, spacing, id, name)?);
    }
    let total = write_corpus(source, out, &fetched, &retrieved)?;
    run.print(&format!(
        "state {}: {} titles, {total} rows -> {out}",
        source.code,
        fetched.len()
    ));
    Ok(0)
}

fn parser() -> Parser {
    let codes: Vec<&str> = SOURCES.iter().map(|s| s.code).collect();
    Parser::new("python -m fadden state")
        .description(
            "Fetch a state register's reviewed revenue titles as whole-title XML and write \
             a corpus root that search build reads.",
        )
        .arg(Action::option("--source").required().choices(&codes))
        .arg(
            Action::option("--out")
                .required()
                .help("new corpus root; must not exist"),
        )
}

pub fn main(argv: &[String], run_state: &mut Run) -> Result<i32, Exit> {
    let args = parser().parse(argv)?;
    let code = args.str("source").unwrap_or_default();
    let out = PyPath::new(&args.str("out").unwrap_or_default());
    let Some(source) = source(&code) else {
        return Ok(2);
    };
    let mut http = http::connect_as(AGENT);
    Ok(match run(http.as_mut(), source, &out, run_state) {
        Ok(code) => code,
        Err(error @ (Exc::Value { .. } | Exc::Os(_))) => {
            run_state.eprint(&format!("state: {error}"));
            1
        }
        Err(error) => run_state.uncaught(&error),
    })
}
