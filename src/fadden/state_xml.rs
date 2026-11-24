// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! A state register's whole-title XML as section rows, for `fadden state`.
use super::Exc;
use super::state::refused;
use crate::htmlparser::{self, Event};

/// One title parsed from its XML.
#[derive(Debug)]
pub struct Parsed {
    pub id: String,
    pub name: String,
    pub collection: &'static str,
    /// The register's as-at date for this version, `YYYY-MM-DD`.
    pub as_at: String,
    pub in_force: String,
    /// The notice the XML carries in its leading comments, if any.
    pub disclaimer: Option<String>,
    pub rows: Vec<Row>,
    /// Structural headings and rows in document order.
    pub(super) items: Vec<Item>,
}

#[derive(Debug, Default, Clone)]
pub struct Row {
    pub section: Option<String>,
    pub heading: String,
    pub container: Option<String>,
    pub kind: &'static str,
    pub anchor: Option<String>,
    pub text: String,
}

#[derive(Debug)]
pub(super) enum Item {
    Heading(usize, String),
    Row(usize, usize),
}

const LEVELS: [&str; 5] = ["chapter", "part", "division", "subdivision", "schedule"];
const SKIPPED: [&str; 3] = ["coverdata", "front", "colspec"];
const BREAKS: [&str; 15] = [
    "txt",
    "li",
    "subclause",
    "definition",
    "row",
    "historynote",
    "note",
    "penalty",
    "repealed",
    "sourceref",
    "tli",
    "formula",
    "eqn",
    "heading",
    "graphicblock",
];

/// Text gathered from block elements: one paragraph per block (a list item
/// marked `- `), a number joined to the block it labels, table cells joined
/// with ` | `.
#[derive(Debug, Default)]
pub(super) struct Text {
    lines: Vec<String>,
    current: String,
    join: bool,
}

impl Text {
    pub(super) fn brk(&mut self) {
        if std::mem::take(&mut self.join) {
            return;
        }
        let line = std::mem::take(&mut self.current);
        if !line.trim().is_empty() {
            self.lines.push(line);
        }
    }

    /// Text joins a following block only until text of its own arrives.
    pub(super) fn push(&mut self, text: &str) {
        if !text.trim().is_empty() {
            self.join = false;
        }
        self.current.push_str(text);
    }

    pub(super) fn glue(&mut self, text: &str) {
        self.current.push_str(text);
        self.join = true;
    }

    pub(super) fn finish(mut self) -> String {
        self.join = false;
        self.brk();
        self.lines
            .iter()
            .map(|line| collapse(line))
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

pub(super) fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Debug, Default)]
struct Level {
    id: Option<String>,
    no: String,
    heading: String,
    item: usize,
    /// The row holding this level's own text, until a child level or clause opens.
    row: Option<usize>,
}

impl Level {
    fn label(&self) -> String {
        collapse(&format!("{} {}", self.no, self.heading))
    }
}

#[derive(Debug, PartialEq)]
enum Capture {
    No,
    Heading,
}

#[derive(Default)]
struct Walker {
    stack: Vec<String>,
    skip: usize,
    levels: Vec<Level>,
    clause: Option<(usize, Level, Text)>,
    capture: Option<Capture>,
    entries: Vec<usize>,
    texts: Vec<Text>,
    rows: Vec<Row>,
    items: Vec<Item>,
}

pub(super) type Attrs = Vec<(String, Option<String>)>;

pub(super) fn attribute<'a>(attrs: &'a [(String, Option<String>)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(key, _)| key == name)
        .and_then(|(_, value)| value.as_deref())
}

impl Walker {
    /// The text a data event belongs to: the open clause's, else the innermost
    /// level's own row, opened on first use.
    fn target(&mut self, create: bool) -> Option<&mut Text> {
        if let Some((_, _, text)) = self.clause.as_mut() {
            return Some(text);
        }
        let depth = self.levels.len();
        let level = self.levels.last_mut()?;
        let index = match level.row {
            Some(index) => index,
            None if !create => return None,
            None => {
                let index = self.rows.len();
                self.rows.push(Row {
                    heading: level.label(),
                    kind: "container",
                    anchor: level.id.clone(),
                    ..Row::default()
                });
                self.texts.push(Text::default());
                self.items.push(Item::Row(depth, index));
                level.row = Some(index);
                index
            }
        };
        self.texts.get_mut(index)
    }

    fn owner(&mut self) -> Option<&mut Level> {
        match self.clause.as_mut() {
            Some((_, level, _)) => Some(level),
            None => self.levels.last_mut(),
        }
    }

    fn start(&mut self, name: &str, attrs: &[(String, Option<String>)], empty: bool) {
        if self.skip > 0 || SKIPPED.contains(&name) {
            if !empty {
                self.skip += 1;
            }
            return;
        }
        if !empty {
            self.stack.push(name.to_string());
        }
        let parent = self
            .stack
            .iter()
            .rev()
            .nth(usize::from(!empty))
            .map(String::as_str);
        let own = parent.is_some_and(|p| LEVELS.contains(&p) || p == "clause");
        if own && matches!(name, "no" | "heading") && self.capture.is_none() {
            self.capture = Some(if name == "no" {
                Capture::No
            } else {
                Capture::Heading
            });
            return;
        }
        if LEVELS.contains(&name) {
            self.open_level(attrs);
        } else if name == "clause" {
            self.open_clause(attrs);
        } else {
            self.open_block(name);
        }
    }

    /// A chapter, part, division, subdivision or schedule: its heading's place.
    fn open_level(&mut self, attrs: &[(String, Option<String>)]) {
        if let Some(level) = self.levels.last_mut() {
            level.row = None;
        }
        let item = self.items.len();
        self.items
            .push(Item::Heading(self.levels.len() + 1, String::new()));
        self.levels.push(Level {
            id: attribute(attrs, "id").map(str::to_string),
            item,
            ..Level::default()
        });
    }

    /// A section: its row, filled in when it closes.
    fn open_clause(&mut self, attrs: &[(String, Option<String>)]) {
        if let Some(level) = self.levels.last_mut() {
            level.row = None;
        }
        let index = self.rows.len();
        self.rows.push(Row {
            container: self.levels.last().map(Level::label),
            kind: "section",
            anchor: attribute(attrs, "id").map(str::to_string),
            ..Row::default()
        });
        self.texts.push(Text::default());
        self.items.push(Item::Row(self.levels.len() + 1, index));
        self.clause = Some((index, Level::default(), Text::default()));
    }

    /// A block inside the text: a paragraph break, a list item's marker or a
    /// table cell's separator.
    fn open_block(&mut self, name: &str) {
        let in_entry = !self.entries.is_empty();
        if name == "entry" {
            let first = self.entries.last().is_none_or(|n| *n == 0);
            if let Some(count) = self.entries.last_mut() {
                *count += 1;
            }
            if !first && let Some(text) = self.target(false) {
                text.glue(" | ");
            }
            return;
        }
        if name == "row" {
            self.entries.push(0);
        }
        if BREAKS.contains(&name)
            && let Some(text) = self.target(false)
        {
            if in_entry && name != "row" {
                text.push(" ");
            } else {
                text.brk();
                // A list item is marked as the Commonwealth rows mark one.
                if name == "li" {
                    text.push("- ");
                }
            }
        }
    }

    fn end(&mut self, name: &str) {
        if self.skip > 0 {
            self.skip -= 1;
            return;
        }
        if self.stack.last().map(String::as_str) != Some(name) {
            return;
        }
        self.stack.pop();
        if self.capture.is_some() && matches!(name, "no" | "heading") {
            let parent = self.stack.last().map(String::as_str);
            if parent.is_some_and(|p| LEVELS.contains(&p) || p == "clause") {
                self.capture = None;
                if let Some(level) = self.levels.last()
                    && self.clause.is_none()
                    && let Some(Item::Heading(_, text)) = self.items.get_mut(level.item)
                {
                    *text = level.label();
                }
                return;
            }
        }
        if name == "no" {
            if let Some(text) = self.target(false) {
                text.glue(" ");
            }
            return;
        }
        if name == "row" {
            self.entries.pop();
        }
        if LEVELS.contains(&name) {
            self.levels.pop();
            if let Some(level) = self.levels.last_mut() {
                level.row = None;
            }
            return;
        }
        if name == "clause"
            && let Some((index, own, text)) = self.clause.take()
        {
            let row = &mut self.rows[index];
            let no = collapse(&own.no);
            row.section = (!no.is_empty()).then_some(no);
            row.heading = own.label();
            self.texts[index] = text;
        }
    }

    fn data(&mut self, data: &str) {
        if self.skip > 0 {
            return;
        }
        match self.capture {
            Some(Capture::No) => {
                if let Some(level) = self.owner() {
                    level.no.push_str(data);
                }
            }
            Some(Capture::Heading) => {
                if let Some(level) = self.owner() {
                    level.heading.push_str(data);
                }
            }
            None => {
                if let Some(text) = self.target(!data.trim().is_empty()) {
                    text.push(data);
                }
            }
        }
    }
}

/// The disclaimer in the XML's leading comments: their text, without the
/// banner lines, joined.
fn disclaimer(comments: &[String]) -> Option<String> {
    let lines: Vec<String> = comments
        .iter()
        .map(|c| collapse(c))
        .filter(|c| !c.is_empty() && !c.chars().all(|ch| ch == '*') && c != "DISCLAIMER")
        .collect();
    (!lines.is_empty()).then(|| lines.join(" "))
}

/// The collection the document element names, and its attributes.
fn document(root: Option<(String, Attrs)>, id: &str) -> Result<(&'static str, Attrs), Exc> {
    let Some((tag, attrs)) = root else {
        return Err(refused(format!("{id}: no XML document element")));
    };
    match tag.as_str() {
        "act" => Ok(("Act", attrs)),
        "subordleg" => Ok(("SubordinateLegislation", attrs)),
        other => Err(refused(format!(
            "{id}: unexpected document element <{other}>"
        ))),
    }
}

/// Parse one title's whole-title XML, which must be the title `id` names.
pub fn parse(xml: &str, id: &str, expected_name: &str) -> Result<Parsed, Exc> {
    let (walker, root, comments) = walk(xml);
    let (collection, attrs) = document(root, id)?;
    let field = |name: &str| attribute(&attrs, name).unwrap_or_default().to_string();
    if field("id") != id {
        return Err(refused(format!("{id}: the XML is title {:?}", field("id"))));
    }
    let name = collapse(&field("title"));
    if name != expected_name {
        return Err(refused(format!(
            "{id}: the register names it {name:?}, not {expected_name:?}; review the list"
        )));
    }
    let valid = field("first.valid.date");
    let as_at = valid.get(..10).unwrap_or_default().to_string();
    if !super::iso::iso_date(&as_at.chars().map(u32::from).collect::<Vec<_>>()) {
        return Err(refused(format!(
            "{id}: no as-at date (first.valid.date {valid:?})"
        )));
    }
    let Walker {
        mut rows,
        texts,
        items,
        ..
    } = walker;
    for (row, text) in rows.iter_mut().zip(texts) {
        row.text = text.finish();
    }
    if rows.iter().all(|row| row.kind != "section") {
        return Err(refused(format!("{id}: no sections found")));
    }
    Ok(Parsed {
        id: id.to_string(),
        name,
        collection,
        as_at,
        in_force: field("in.force"),
        disclaimer: disclaimer(&comments),
        rows,
        items,
    })
}

/// Walks the document: the walker's rows, the root element and the comments
/// before it.
fn walk(xml: &str) -> (Walker, Option<(String, Attrs)>, Vec<String>) {
    let mut walker = Walker::default();
    let mut root: Option<(String, Attrs)> = None;
    let mut comments: Vec<String> = Vec::new();
    htmlparser::parse_each(xml, true, |positioned| {
        match positioned.event {
            Event::StartTag(name, attrs) => {
                if root.is_none() {
                    root = Some((name, attrs));
                } else {
                    walker.start(&name, &attrs, false);
                }
            }
            Event::StartEndTag(name, attrs) => {
                if root.is_some() {
                    walker.start(&name, &attrs, true);
                }
            }
            Event::EndTag(name) => walker.end(&name),
            Event::Data(data) => walker.data(&data),
            Event::Comment(text) if root.is_none() => comments.push(text),
            _ => {}
        }
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap_or_else(|never| match never {});
    (walker, root, comments)
}
