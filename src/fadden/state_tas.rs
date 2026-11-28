// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! The Tasmanian Legislation website's whole-title XML as section rows, for
//! `fadden state --source tas`. Its vocabulary differs from Queensland's:
//! numbers sit in attributes (`SECNO`, `PARTNO`, `PARANO`), a section's heading
//! is its `HEADNOTE`, a date is an empty `DATE` element, and amendment history
//! (`HOWAFFECTED`) sits between provisions, so it is left out of the rows.
use super::Exc;
use super::state::refused;
use super::state_xml::{Handler, Parsed, Rows, attribute, collapse, title_fields, walk};

/// Structural levels: element, number attribute and the word before the number.
const LEVELS: [(&str, &str, &str); 7] = [
    ("chapter", "chapno", "Chapter"),
    ("part", "partno", "Part"),
    ("s.part", "partno", "Part"),
    ("division", "divno", "Division"),
    ("s.division", "divno", "Division"),
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

#[derive(Default)]
struct Walker {
    stack: Vec<String>,
    skip: usize,
    heading: bool,
    rows: Rows,
    /// Rows of sections the register marks omitted (repealed), shown without text.
    omitted: Vec<usize>,
}

impl Walker {
    fn close_element(&mut self, name: &str) {
        if name == "definedterm"
            && let Some(text) = self.rows.target(false)
        {
            // The term runs on into the text that defines it.
            text.glue("");
            return;
        }
        if name == "row" {
            self.rows.end_row();
        }
        if level_of(name).is_some() {
            self.rows.pop_level();
            return;
        }
        if section_of(name).is_some() {
            self.rows.close_section();
        }
    }
}

impl Handler for Walker {
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
            && let Some(text) = self.rows.target(true)
        {
            text.brk();
            text.push("[formula]");
            text.brk();
        }
        let own = self
            .stack
            .last()
            .is_some_and(|p| level_of(p).is_some() || section_of(p).is_some());
        if !empty {
            self.stack.push(name.to_string());
        }
        if own && matches!(name, "heading" | "headnote") && !self.heading {
            if !empty {
                self.heading = true;
            }
            return;
        }
        if let Some((no, word)) = level_of(name) {
            let number = attribute(attrs, no).map(collapse).unwrap_or_default();
            self.rows
                .open_level(anchor(attrs), collapse(&format!("{word} {number}")));
            self.rows.set_level_heading();
        } else if let Some(no) = section_of(name) {
            let number = attribute(attrs, no).map(collapse).unwrap_or_default();
            let index = self.rows.open_section(anchor(attrs), number);
            if attribute(attrs, "omitted").is_some_and(|v| v.eq_ignore_ascii_case("yesomit")) {
                self.omitted.push(index);
            }
        } else {
            let number = block_number(attrs);
            self.rows
                .open_block(name, BREAKS.contains(&name), number.as_deref(), None);
        }
        if empty {
            self.close_element(name);
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
            self.rows.set_level_heading();
            return;
        }
        self.close_element(name);
    }

    fn data(&mut self, data: &str) {
        if self.skip > 0 {
            return;
        }
        if self.heading {
            if let Some(level) = self.rows.owner() {
                level.heading.push_str(data);
            }
            return;
        }
        self.rows.push_data(data);
    }
}

/// Parse one Tasmanian Act's whole-title XML, which must be the title `id` names.
pub fn parse(xml: &str, id: &str, expected_name: &str) -> Result<Parsed, Exc> {
    let mut walker = Walker::default();
    let (root, _) = walk(xml, &mut walker);
    let Some((tag, attrs)) = root else {
        return Err(refused(format!("{id}: no XML document element")));
    };
    if tag != "act" {
        return Err(refused(format!(
            "{id}: unexpected document element <{tag}>"
        )));
    }
    let (name, as_at) = title_fields(&attrs, id, expected_name)?;
    let (mut rows, items) = walker.rows.finish(id)?;
    // The register shows an omitted section's number only; say so in its row.
    for index in walker.omitted {
        if rows[index].text.is_empty() {
            rows[index].text = "[omitted]".to_string();
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tas_no_element_and_empty_document_edge_cases() {
        let empty = parse("", "tas-1", "Example Act").unwrap_err();
        assert!(empty.to_string().contains("no XML document element"));
        let xml = r#"<act id="tas-1" title="Example Act" first.valid.date="2026-01-01"><section secno="1"><headnote>One</headnote><para noformat="br" parano=" "><text>a</text></para><para noformat="plain" parano="2"><text>b</text></para></section></act>"#;
        let parsed = parse(xml, "tas-1", "Example Act").unwrap();
        assert!(parsed.rows[0].text.contains("2"));
    }
}
