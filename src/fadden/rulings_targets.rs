// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! `fadden rulings_targets FEED --out TARGETS.json`: the target list for a
//! `rulings` run, from an ATO Legal Database RSS feed such as 'ATO - All Rulings
//! and Determinations' (<https://www.ato.gov.au/law/view/rss?fileid=pbr_all.rss>).
//!
//! The feed is a saved file or its URL; the URL is the only thing fetched. Each
//! item's `<link>` names a document by docid. A docid in a family `rulings`
//! accepts becomes a target, at most `rulings::MAX_TARGETS` of them, each once;
//! a draft, any other family, a repeat and every target past the cap is refused,
//! and an item whose link this cannot read is reported as malformed. Every
//! refusal and malformed item is printed with its reason, and a malformed item
//! makes the stage exit 1.
use std::sync::LazyLock;

use regex::Regex;
use sha2::{Digest, Sha256};

use super::http::{self, Http};
use super::rulings::{self, BASE_URL, MAX_BYTES, MAX_TARGETS};
use super::{Exc, Run, authorities};
use crate::argparse::{Action, Exit, Get, Parser};
use crate::pyjson::{self, Json, Layout};
use crate::pypath::PyPath;
use crate::pyurl;

/// Where the Legal Database serves its feeds.
const FEED_PATH: &str = "/law/view/rss";

// shortcut: patterns over the flat RSS 2.0 the Legal Database serves, not an XML
// parser; an item they cannot read is reported as malformed, never dropped. Use an
// XML crate if the feed gains namespaces, nested items or links in attributes.
static ITEM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<(/?)item\b([^>]*)>").unwrap());
/// An item tag with a namespace prefix, which `ITEM` would not see.
static PREFIXED_ITEM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"</?[^\s<>/:!?]+:item\b").unwrap());
static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<(/?)link\b[^>]*>").unwrap());
/// A draft's number, as in `DTR/TR2025D1/NAT/ATO/00001` (TR 2025/D1) and
/// `DPC/PCG2025D1/NAT/ATO/00001` (PCG 2025/D1), both read on 17 November 2026.
static DRAFT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^[A-Z]+\d{4}D\d+$").unwrap());

/// What one feed item came to.
#[derive(Debug, PartialEq)]
pub enum Entry {
    Target(String),
    Refused { docid: String, reason: String },
    Malformed(String),
}

/// The feed with every CDATA section's and comment's content blanked, byte for
/// byte, so a tag quoted in a title or description is not read as markup.
fn masked(feed: &str) -> Result<String, String> {
    const QUOTED: [(&str, &str, &str); 2] = [
        ("<![CDATA[", "]]>", "CDATA section"),
        ("<!--", "-->", "comment"),
    ];
    let mut out = feed.as_bytes().to_vec();
    let mut at = 0;
    loop {
        let next = QUOTED
            .iter()
            .filter_map(|&(open, close, name)| {
                feed[at..]
                    .find(open)
                    .map(|i| (at + i + open.len(), close, name))
            })
            .min_by_key(|&(start, ..)| start);
        let Some((start, close, name)) = next else {
            break;
        };
        let end = feed[start..]
            .find(close)
            .map(|i| start + i)
            .ok_or_else(|| format!("it has an unclosed {name}"))?;
        out[start..end].fill(b' ');
        at = end + close.len();
    }
    // Every byte between 2 ASCII delimiters became a space, so whole characters went.
    Ok(String::from_utf8(out).expect("blanking whole characters keeps UTF-8"))
}

/// Each item's span in the feed, or why the items cannot be told apart.
fn item_spans(masked: &str) -> Result<Vec<(usize, usize)>, String> {
    if let Some(tag) = PREFIXED_ITEM.find(masked) {
        return Err(format!(
            "it has a prefixed item tag {:?} at byte {}, which this stage does not read",
            tag.as_str(),
            tag.start()
        ));
    }
    let mut spans = Vec::new();
    let mut open: Option<usize> = None;
    for tag in ITEM.captures_iter(masked) {
        let whole = tag.get(0).unwrap();
        match (tag[1].is_empty(), open) {
            // An empty `<item/>` is an item with no link, reported as malformed.
            (true, None) if tag[2].ends_with('/') => spans.push((whole.end(), whole.end())),
            (true, None) => open = Some(whole.end()),
            (false, Some(start)) => {
                spans.push((start, whole.start()));
                open = None;
            }
            _ => {
                return Err(format!(
                    "its <item> tags do not pair up at byte {}",
                    whole.start()
                ));
            }
        }
    }
    if open.is_some() {
        return Err("its last <item> is not closed".into());
    }
    Ok(spans)
}

/// The text of the item's one `<link>`, unescaped, or why it has none.
fn link_text(feed: &str, masked: &str) -> Result<String, String> {
    let tags: Vec<_> = LINK.captures_iter(masked).collect();
    let [open, close] = tags.as_slice() else {
        return Err(format!(
            "it has {} <link> tags, not one element",
            tags.len()
        ));
    };
    if !open[1].is_empty() || close[1].is_empty() {
        return Err("its <link> element is not closed".into());
    }
    let raw = feed[open.get(0).unwrap().end()..close.get(0).unwrap().start()].trim();
    if let Some(inner) = raw
        .strip_prefix("<![CDATA[")
        .and_then(|r| r.strip_suffix("]]>"))
    {
        return Ok(inner.trim().to_string());
    }
    if raw.contains('<') {
        return Err("its <link> holds markup".into());
    }
    let mut text = String::new();
    let mut rest = raw;
    while let Some(i) = rest.find('&') {
        text.push_str(&rest[..i]);
        rest = &rest[i..];
        let entity = rest.find(';').map_or(rest, |end| &rest[..=end]);
        text.push(match entity {
            "&amp;" => '&',
            "&lt;" => '<',
            "&gt;" => '>',
            "&quot;" => '"',
            "&apos;" => '\'',
            _ => return Err("its <link> holds an entity this stage does not read".into()),
        });
        rest = &rest[entity.len()..];
    }
    text.push_str(rest);
    Ok(text)
}

/// The docid a Legal Database document link names, or why it names none.
fn docid_of(link: &str) -> Result<String, String> {
    let Some(docid) = link.strip_prefix(BASE_URL) else {
        return Err(format!(
            "its link {link:?} is not a Legal Database document link"
        ));
    };
    if !rulings::is_docid(docid) {
        return Err(format!(
            "its link names {docid:?}, which is not a Legal Database docid"
        ));
    }
    Ok(docid.to_string())
}

/// Each item of the feed, in the feed's order, or why the feed cannot be read.
pub fn entries(feed: &str) -> Result<Vec<Entry>, String> {
    let blank = masked(feed)?;
    if !blank.contains("<rss") || !blank.contains("<channel") {
        return Err("it is not an RSS feed".into());
    }
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for (number, (start, end)) in item_spans(&blank)?.into_iter().enumerate() {
        let docid =
            match link_text(&feed[start..end], &blank[start..end]).and_then(|l| docid_of(&l)) {
                Ok(docid) => docid,
                Err(reason) => {
                    out.push(Entry::Malformed(format!("item {}: {reason}", number + 1)));
                    continue;
                }
            };
        let id_part = docid.split('/').nth(1).unwrap_or_default();
        let reason = if DRAFT.is_match(id_part) {
            Some("a draft; rulings takes final documents only".to_string())
        } else if rulings::family_of(&docid).is_none() {
            Some("not a family rulings accepts".to_string())
        } else if let Some(first) = seen.iter().position(|d| d.eq_ignore_ascii_case(&docid)) {
            Some(format!("repeats target {}", first + 1))
        } else if seen.len() == MAX_TARGETS {
            Some(format!("past the cap of {MAX_TARGETS} targets a run"))
        } else {
            None
        };
        out.push(match reason {
            Some(reason) => Entry::Refused { docid, reason },
            None => {
                seen.push(docid.clone());
                Entry::Target(docid)
            }
        });
    }
    Ok(out)
}

/// Whether `url` is a Legal Database feed on `https://www.ato.gov.au`.
fn is_feed_url(url: &str) -> bool {
    pyurl::urlsplit(url).is_ok_and(|parts| {
        parts.scheme == "https"
            && pyurl::hostname(&parts.netloc).as_deref() == Some("www.ato.gov.au")
            && matches!(pyurl::port(&parts.netloc), Ok(None | Some(443)))
            && parts.path == FEED_PATH
    })
}

fn verify(response: http::Response) -> Result<Vec<u8>, String> {
    if !is_feed_url(&response.url) {
        return Err(format!("the feed redirected to {}", response.url));
    }
    if !response.content_type.contains("xml") {
        return Err(format!(
            "the feed came as {:?}, not XML",
            response.content_type
        ));
    }
    if response.body.len() > MAX_BYTES {
        return Err(format!("the feed is larger than {MAX_BYTES} bytes"));
    }
    Ok(response.body)
}

/// The bytes of `feed`: a saved file, or a Legal Database feed URL fetched once.
fn read_feed(http: &mut dyn Http, feed: &str) -> Result<Vec<u8>, Exc> {
    if !feed.contains("://") {
        return authorities::read(feed);
    }
    if !is_feed_url(feed) {
        return Err(Exc::value(format!(
            "{feed} is not a Legal Database feed at https://www.ato.gov.au{FEED_PATH}"
        )));
    }
    rulings::get(http, feed)?
        .map_err(|failure| format!("fetching the feed failed ({failure})"))
        .and_then(verify)
        .map_err(Exc::value)
}

/// Read `feed` (a saved file or a Legal Database feed URL) and write its targets
/// to the new file `out`; 1 when the feed held a malformed item.
pub fn run(http: &mut dyn Http, feed: &str, out: &str, run: &mut Run) -> Result<i32, Exc> {
    if std::fs::metadata(PyPath::new(out).fs()).is_ok() {
        return Err(Exc::value(format!("{out} already exists; name a new file")));
    }
    let bytes = read_feed(http, feed)?;
    let digest: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let text = String::from_utf8(bytes).map_err(|_| Exc::value(format!("{feed} is not UTF-8")))?;
    let entries =
        entries(&text).map_err(|reason| Exc::value(format!("{feed} cannot be read: {reason}")))?;
    let targets: Vec<Json> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Target(docid) => Some(Json::str(docid)),
            _ => None,
        })
        .collect();
    let written = targets.len();
    authorities::write_new(
        out,
        &(pyjson::dumps(&Json::Array(targets), Layout::Indented) + "\n"),
    )?;
    run.print(&format!(
        "feed {feed} sha256 {digest}: {} item(s)",
        entries.len()
    ));
    let (mut refused, mut malformed) = (0, 0);
    for entry in &entries {
        match entry {
            Entry::Target(_) => {}
            Entry::Refused { docid, reason } => {
                refused += 1;
                run.print(&format!("refused {docid}: {reason}"));
            }
            Entry::Malformed(reason) => {
                malformed += 1;
                run.print(&format!("malformed {reason}"));
            }
        }
    }
    run.print(&format!(
        "wrote {written} target(s) to {out}; {refused} refused, {malformed} malformed"
    ));
    Ok(i32::from(malformed > 0))
}

fn parser() -> Parser {
    Parser::new("python -m fadden rulings_targets")
        .description("Write a rulings target list from an ATO Legal Database RSS feed.")
        .arg(Action::positional("feed").help(&format!(
            "a saved feed, or its https://www.ato.gov.au{FEED_PATH} URL"
        )))
        .arg(Action::option("--out").required().help(&format!(
            "new JSON file for at most {MAX_TARGETS} docids; must not exist"
        )))
}

pub fn main(argv: &[String], run_state: &mut Run) -> Result<i32, Exit> {
    let args = parser().parse(argv)?;
    let feed = args.str("feed").unwrap_or_default();
    let out = args.str("out").unwrap_or_default();
    let mut http = http::connect();
    Ok(match run(http.as_mut(), &feed, &out, run_state) {
        Ok(code) => code,
        Err(error @ (Exc::Value { .. } | Exc::Os(_))) => {
            run_state.eprint(&format!("rulings_targets: {error}"));
            1
        }
        Err(error) => run_state.uncaught(&error),
    })
}
