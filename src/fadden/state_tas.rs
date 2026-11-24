// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! The Tasmanian Legislation website's whole-title XML as section rows, for
//! `fadden state --source tas`. Its vocabulary differs from Queensland's:
//! numbers sit in attributes (`SECNO`, `PARTNO`, `PARANO`), a section's heading
//! is its `HEADNOTE`, a date is an empty `DATE` element, and amendment history
//! (`HOWAFFECTED`) sits between provisions, so it is left out of the rows.
use super::Exc;
use super::state::refused;
use super::state_xml::{Attrs, Item, Parsed, Row, Text, attribute, collapse};
use crate::htmlparser::{self, Event};

/// Structural levels: element, number attribute and the word before the number.
const LEVELS: [(&str, &str, &str); 5] = [
    ("chapter", "chapno", "Chapter"),
    ("part", "partno", "Part"),
    ("division", "divno", "Division"),
    ("subdivision", "subdivno", "Subdivision"),
    ("schedule", "scheduleno", "Schedule"),
];
/// A section, and a schedule's clause: each a row, numbered by the attribute.
const SECTIONS: [(&str, &str); 2] = [("section", "secno"), ("s.clause", "clauseno")];
const SKIPPED: [&str; 4] = ["front", "howaffected", "colspec", "fragspec"];
const BREAKS: [&str; 8] = [
    "text",
    "definition",
    "penalty",
    "para",
    "row",
    "note",
    "example",
    "quotedtext",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn level_of(name: &str) -> Option<(&'static str, &'static str)> {
    LEVELS
        .iter()
        .find(|(element, _, _)| *element == name)
        .map(|(_, no, word)| (*no, *word))
}

fn section_of(name: &str) -> Option<&'static str> {
    SECTIONS
        .iter()
        .find(|(element, _)| *element == name)
        .map(|(_, no)| *no)
}

/// The number a block shows before its text, `(1)` or `(a)`: its number
/// attribute, bracketed when its `noformat` starts with `BR`.
fn block_number(attrs: &[(String, Option<String>)]) -> Option<String> {
    let format = attribute(attrs, "noformat")?;
    let no = attrs.iter().find_map(|(key, value)| {
        (key.ends_with("no") && key != "noformat")
            .then_some(value.as_deref())
            .flatten()
    })?;
    let no = collapse(no);
    if no.is_empty() {
        return None;
    }
    Some(if format.to_ascii_lowercase().starts_with("br") {
        format!("({no})")
    } else {
        no
    })
}

/// `<DATE DAY="1" MONTH="JULY" YEAR="2001"/>` as `1 July 2001`.
fn date_text(attrs: &[(String, Option<String>)]) -> String {
    let month = attribute(attrs, "month").unwrap_or_default();
    let month = MONTHS
        .iter()
        .find(|m| m.eq_ignore_ascii_case(month))
        .map_or(month, |m| m);
    collapse(&format!(
        "{} {month} {}",
        attribute(attrs, "day").unwrap_or_default(),
        attribute(attrs, "year").unwrap_or_default()
    ))
}

/// The register's page anchor for an XML id: `GS3/EN` is `GS3@EN`.
fn anchor(attrs: &[(String, Option<String>)]) -> Option<String> {
    attribute(attrs, "id").map(|id| id.replace('/', "@"))
}

#[derive(Debug, Default)]
struct Level {
    id: Option<String>,
    no: String,
    heading: String,
    item: usize,
    /// The row holding this level's own text, until a child level or section opens.
    row: Option<usize>,
}

impl Level {
    fn label(&self) -> String {
        collapse(&format!("{} {}", self.no, self.heading))
    }
}

#[derive(Default)]
struct Walker {
    stack: Vec<String>,
    skip: usize,
    levels: Vec<Level>,
    section: Option<(usize, Level, Text)>,
    heading: bool,
    entries: Vec<usize>,
    texts: Vec<Text>,
    rows: Vec<Row>,
    items: Vec<Item>,
    /// Rows of sections the register marks omitted (repealed), shown without text.
    omitted: Vec<usize>,
}

impl Walker {
    /// The text a data event belongs to: the open section's, else the innermost
    /// level's own row, opened on first use.
    fn target(&mut self, create: bool) -> Option<&mut Text> {
        if let Some((_, _, text)) = self.section.as_mut() {
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
        match self.section.as_mut() {
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
        if name == "date" {
            let date = date_text(attrs);
            self.data(&date);
        } else if name == "equation"
            && let Some(text) = self.target(true)
        {
            text.brk();
            text.push("[formula]");
            text.brk();
        }
        if empty {
            return;
        }
        let own = self
            .stack
            .last()
            .is_some_and(|p| level_of(p).is_some() || section_of(p).is_some());
        self.stack.push(name.to_string());
        if own && matches!(name, "heading" | "headnote") && !self.heading {
            self.heading = true;
            return;
        }
        if let Some((no, word)) = level_of(name) {
            self.open_level(attrs, no, word);
        } else if let Some(no) = section_of(name) {
            self.open_section(attrs, no);
        } else {
            self.open_block(name, attrs);
        }
    }

    fn open_level(&mut self, attrs: &[(String, Option<String>)], no: &str, word: &str) {
        if let Some(level) = self.levels.last_mut() {
            level.row = None;
        }
        let item = self.items.len();
        self.items
            .push(Item::Heading(self.levels.len() + 1, String::new()));
        let number = attribute(attrs, no).map(collapse).unwrap_or_default();
        self.levels.push(Level {
            id: anchor(attrs),
            no: collapse(&format!("{word} {number}")),
            item,
            ..Level::default()
        });
        self.set_heading();
    }

    fn open_section(&mut self, attrs: &[(String, Option<String>)], no: &str) {
        if let Some(level) = self.levels.last_mut() {
            level.row = None;
        }
        let index = self.rows.len();
        self.rows.push(Row {
            container: self.levels.last().map(Level::label),
            kind: "section",
            anchor: anchor(attrs),
            ..Row::default()
        });
        self.texts.push(Text::default());
        self.items.push(Item::Row(self.levels.len() + 1, index));
        let own = Level {
            no: attribute(attrs, no).map(collapse).unwrap_or_default(),
            ..Level::default()
        };
        if attribute(attrs, "omitted").is_some_and(|v| v.eq_ignore_ascii_case("yesomit")) {
            self.omitted.push(index);
        }
        self.section = Some((index, own, Text::default()));
    }

    /// A block inside the text: a paragraph break, a number before its text or
    /// a table cell's separator.
    fn open_block(&mut self, name: &str, attrs: &[(String, Option<String>)]) {
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
        let number = block_number(attrs);
        if (number.is_some() || BREAKS.contains(&name))
            && let Some(text) = self.target(number.is_some())
        {
            if in_entry && name != "row" {
                text.push(" ");
            } else {
                text.brk();
            }
            if let Some(number) = number {
                text.push(&number);
                text.glue(" ");
            }
        }
    }

    /// The level's heading item, once its number or heading is known.
    fn set_heading(&mut self) {
        if self.section.is_some() {
            return;
        }
        if let Some(level) = self.levels.last()
            && let Some(Item::Heading(_, text)) = self.items.get_mut(level.item)
        {
            *text = level.label();
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
        if self.heading && matches!(name, "heading" | "headnote") {
            self.heading = false;
            self.set_heading();
            return;
        }
        if name == "definedterm"
            && let Some(text) = self.target(false)
        {
            // The term runs on into the text that defines it.
            text.glue("");
            return;
        }
        if name == "row" {
            self.entries.pop();
        }
        if level_of(name).is_some() {
            self.levels.pop();
            if let Some(level) = self.levels.last_mut() {
                level.row = None;
            }
            return;
        }
        if section_of(name).is_some()
            && let Some((index, own, text)) = self.section.take()
        {
            let row = &mut self.rows[index];
            row.section = (!own.no.is_empty()).then(|| own.no.clone());
            row.heading = own.label();
            self.texts[index] = text;
        }
    }

    fn data(&mut self, data: &str) {
        if self.skip > 0 {
            return;
        }
        if self.heading {
            if let Some(level) = self.owner() {
                level.heading.push_str(data);
            }
            return;
        }
        if let Some(text) = self.target(!data.trim().is_empty()) {
            text.push(data);
        }
    }
}

/// Parse one Tasmanian Act's whole-title XML, which must be the title `id` names.
pub fn parse(xml: &str, id: &str, expected_name: &str) -> Result<Parsed, Exc> {
    let (walker, root) = walk(xml);
    let Some((tag, attrs)) = root else {
        return Err(refused(format!("{id}: no XML document element")));
    };
    if tag != "act" {
        return Err(refused(format!(
            "{id}: unexpected document element <{tag}>"
        )));
    }
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
        omitted,
        ..
    } = walker;
    for (row, text) in rows.iter_mut().zip(texts) {
        row.text = text.finish();
    }
    // The register shows an omitted section's number only; say so in its row.
    for index in omitted {
        if rows[index].text.is_empty() {
            rows[index].text = "[omitted]".to_string();
        }
    }
    if rows.iter().all(|row| row.kind != "section") {
        return Err(refused(format!("{id}: no sections found")));
    }
    Ok(Parsed {
        id: id.to_string(),
        name,
        collection: "Act",
        as_at,
        // The stage reads the register's in-force view of the title.
        in_force: "inforce".to_string(),
        disclaimer: None,
        rows,
        items,
    })
}

fn walk(xml: &str) -> (Walker, Option<(String, Attrs)>) {
    let mut walker = Walker::default();
    let mut root: Option<(String, Attrs)> = None;
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
            _ => {}
        }
        Ok::<(), std::convert::Infallible>(())
    })
    .unwrap_or_else(|never| match never {});
    (walker, root)
}
