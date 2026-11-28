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
pub(super) struct Level {
    pub(super) id: Option<String>,
    pub(super) no: String,
    pub(super) heading: String,
    pub(super) item: usize,
    /// The row holding this level's own text, until a child level or section opens.
    pub(super) row: Option<usize>,
}

impl Level {
    pub(super) fn label(&self) -> String {
        collapse(&format!("{} {}", self.no, self.heading))
    }
}

pub(super) type Attrs = Vec<(String, Option<String>)>;

pub(super) fn attribute<'a>(attrs: &'a [(String, Option<String>)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(key, _)| key == name)
        .and_then(|(_, value)| value.as_deref())
}

/// Shared row, level, table and text builder for state-register XML walkers.
#[derive(Default)]
pub(super) struct Rows {
    levels: Vec<Level>,
    section: Option<(usize, Level, Text)>,
    entries: Vec<usize>,
    texts: Vec<Text>,
    rows: Vec<Row>,
    items: Vec<Item>,
}

impl Rows {
    /// The text a data event belongs to: the open section's, else the innermost
    /// level's own row, opened on first use.
    pub(super) fn target(&mut self, create: bool) -> Option<&mut Text> {
        if let Some((_, _, text)) = self.section.as_mut() {
            return Some(text);
        }
        let depth = self.levels.len();
        let level = self.levels.last_mut()?;
        let index = match level.row {
            Some(index) => index,
            None if !create => return None,
            None => {
                let row = Row {
                    heading: level.label(),
                    kind: "container",
                    anchor: level.id.clone(),
                    ..Row::default()
                };
                let index = push_row(&mut self.rows, &mut self.texts, &mut self.items, depth, row);
                level.row = Some(index);
                index
            }
        };
        self.texts.get_mut(index)
    }

    pub(super) fn owner(&mut self) -> Option<&mut Level> {
        match self.section.as_mut() {
            Some((_, level, _)) => Some(level),
            None => self.levels.last_mut(),
        }
    }

    /// Opens a chapter, part, division, subdivision or schedule and reserves its heading.
    pub(super) fn open_level(&mut self, id: Option<String>, no: String) {
        if let Some(level) = self.levels.last_mut() {
            level.row = None;
        }
        let item = self.items.len();
        self.items
            .push(Item::Heading(self.levels.len() + 1, String::new()));
        self.levels.push(Level {
            id,
            no,
            item,
            ..Level::default()
        });
    }

    /// Updates the innermost level's heading item when outside a section.
    pub(super) fn set_level_heading(&mut self) {
        if self.section.is_none()
            && let Some(level) = self.levels.last()
            && let Some(Item::Heading(_, text)) = self.items.get_mut(level.item)
        {
            *text = level.label();
        }
    }

    pub(super) fn pop_level(&mut self) {
        self.levels.pop();
        if let Some(level) = self.levels.last_mut() {
            level.row = None;
        }
    }

    /// Opens a section row, returning its index, filled in on `close_section`.
    pub(super) fn open_section(&mut self, anchor: Option<String>, no: String) -> usize {
        if let Some(level) = self.levels.last_mut() {
            level.row = None;
        }
        let row = Row {
            container: self.levels.last().map(Level::label),
            kind: "section",
            anchor,
            ..Row::default()
        };
        let depth = self.levels.len() + 1;
        let index = push_row(&mut self.rows, &mut self.texts, &mut self.items, depth, row);
        let own = Level {
            no,
            ..Level::default()
        };
        self.section = Some((index, own, Text::default()));
        index
    }

    pub(super) fn close_section(&mut self) {
        if let Some((index, own, text)) = self.section.take() {
            let row = &mut self.rows[index];
            let no = collapse(&own.no);
            row.section = (!no.is_empty()).then_some(no);
            row.heading = own.label();
            self.texts[index] = text;
        }
    }

    /// Starts a block break, optional numbered prefix, list marker or table cell separator.
    pub(super) fn open_block(
        &mut self,
        name: &str,
        is_break: bool,
        number: Option<&str>,
        list_marker: Option<&str>,
    ) {
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
        if (is_break || number.is_some())
            && let Some(text) = self.target(number.is_some())
        {
            if in_entry && name != "row" {
                text.push(" ");
            } else {
                text.brk();
                if let Some(marker) = list_marker {
                    text.push(marker);
                }
            }
            if let Some(number) = number {
                text.push(number);
                text.glue(" ");
            }
        }
    }

    pub(super) fn end_row(&mut self) {
        self.entries.pop();
    }

    pub(super) fn push_data(&mut self, data: &str) {
        if let Some(text) = self.target(!data.trim().is_empty()) {
            text.push(data);
        }
    }

    pub(super) fn finish(self, id: &str) -> Result<(Vec<Row>, Vec<Item>), Exc> {
        let rows = finished(self.rows, self.texts, id)?;
        Ok((rows, self.items))
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
    capture: Option<Capture>,
    rows: Rows,
}

impl Handler for Walker {
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
            self.rows
                .open_level(attribute(attrs, "id").map(str::to_string), String::new());
        } else if name == "clause" {
            self.rows
                .open_section(attribute(attrs, "id").map(str::to_string), String::new());
        } else {
            self.rows.open_block(
                name,
                BREAKS.contains(&name),
                None,
                (name == "li").then_some("- "),
            );
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
                self.rows.set_level_heading();
                return;
            }
        }
        if name == "no" {
            if let Some(text) = self.rows.target(false) {
                text.glue(" ");
            }
            return;
        }
        if name == "row" {
            self.rows.end_row();
        }
        if LEVELS.contains(&name) {
            self.rows.pop_level();
            return;
        }
        if name == "clause" {
            self.rows.close_section();
        }
    }

    fn data(&mut self, data: &str) {
        if self.skip > 0 {
            return;
        }
        match self.capture {
            Some(Capture::No) => {
                if let Some(level) = self.rows.owner() {
                    level.no.push_str(data);
                }
            }
            Some(Capture::Heading) => {
                if let Some(level) = self.rows.owner() {
                    level.heading.push_str(data);
                }
            }
            None => self.rows.push_data(data),
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
    let mut walker = Walker::default();
    let (root, comments) = walk(xml, &mut walker);
    let (collection, attrs) = document(root, id)?;
    let (name, as_at) = title_fields(&attrs, id, expected_name)?;
    let field = |name: &str| attribute(&attrs, name).unwrap_or_default().to_string();
    let (rows, items) = walker.rows.finish(id)?;
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

/// The id, title and as-at date a title's root element must carry: its name
/// and as-at date, or why it is refused.
pub(super) fn title_fields(
    attrs: &[(String, Option<String>)],
    id: &str,
    expected_name: &str,
) -> Result<(String, String), Exc> {
    let field = |name: &str| attribute(attrs, name).unwrap_or_default().to_string();
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
    Ok((name, as_at))
}

/// Each row with its gathered text; a title with no section is refused.
fn finished(mut rows: Vec<Row>, texts: Vec<Text>, id: &str) -> Result<Vec<Row>, Exc> {
    for (row, text) in rows.iter_mut().zip(texts) {
        row.text = text.finish();
    }
    if rows.iter().all(|row| row.kind != "section") {
        return Err(refused(format!("{id}: no sections found")));
    }
    Ok(rows)
}

/// Opens a row and its text, placed at `depth` in document order.
fn push_row(
    rows: &mut Vec<Row>,
    texts: &mut Vec<Text>,
    items: &mut Vec<Item>,
    depth: usize,
    row: Row,
) -> usize {
    let index = rows.len();
    rows.push(row);
    texts.push(Text::default());
    items.push(Item::Row(depth, index));
    index
}

/// What a register's XML walker does with each event after the root element.
pub(super) trait Handler {
    fn start(&mut self, name: &str, attrs: &[(String, Option<String>)], empty: bool);
    fn end(&mut self, name: &str);
    fn data(&mut self, data: &str);
}

/// Walks the document through `walker`: the root element and the comments
/// before it.
pub(super) fn walk(xml: &str, walker: &mut impl Handler) -> (Option<(String, Attrs)>, Vec<String>) {
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
    (root, comments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_parser_refuses_empty_wrong_root_and_sectionless_documents() {
        assert!(parse("", "x", "X").is_err());
        assert!(parse("<other/>", "x", "X").is_err());
        let empty = r#"<act id="x" title="X" date.as.at="2026-01-01"><level><heading><no>1</no>Title</heading></level></act>"#;
        let err = parse(empty, "x", "X").unwrap_err();
        assert!(err.to_string().contains("no "), "{err}");
    }
}
