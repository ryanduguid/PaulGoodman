//! The Word side of `fadden witness`: the section numbers and custom
//! properties of each volume of the Register's Word rendition.
use std::collections::BTreeMap;

use super::refuse;
use crate::fadden::{Exc, epub};
use crate::htmlparser::{self, Event, Positioned};

const HEADING_STYLE: &str = "ActHead5";
const NUMBER_STYLE: &str = "CharSectno";
const DOCUMENT: &str = "word/document.xml";
const PROPERTIES: &str = "docProps/custom.xml";

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

/// The open paragraphs and runs of a Word body as its events are read, and
/// the section numbers found so far.
#[derive(Default)]
struct Body {
    paras: Vec<Para>,
    runs: Vec<WordRun>,
    in_text: bool,
    /// Open w:pPrChange and w:rPrChange elements: a tracked change's earlier
    /// properties, whose styles are not the current ones.
    changes: usize,
    found: Vec<String>,
}

impl Body {
    /// The innermost run is the current paragraph's and numbers a section.
    fn numbering(&self) -> bool {
        self.runs.last().is_some_and(|r| {
            r.depth == self.paras.len() && r.style.as_deref() == Some(NUMBER_STYLE)
        })
    }

    /// A start tag that opens a paragraph, run, text or tracked change;
    /// `false` for any other.
    fn open(&mut self, name: &str) -> bool {
        match name {
            "w:p" => self.paras.push(Para::default()),
            "w:r" => self.runs.push(WordRun {
                depth: self.paras.len(),
                style: None,
            }),
            "w:t" => self.in_text = true,
            "w:pPrChange" | "w:rPrChange" => self.changes += 1,
            _ => return false,
        }
        true
    }

    /// Any other start or empty tag: a current style, whose `w:val` `val`
    /// reads, or a section number's hyphen.
    fn mark(
        &mut self,
        name: &str,
        val: impl FnOnce() -> Result<String, String>,
    ) -> Result<(), String> {
        let numbering = self.numbering();
        match name {
            "w:pStyle" if self.changes == 0 => {
                if let Some(para) = self.paras.last_mut().filter(|p| p.style.is_none()) {
                    para.style = Some(val()?);
                }
            }
            // A paragraph mark's run properties sit outside any run of it.
            "w:rStyle" if self.changes == 0 => {
                let depth = self.paras.len();
                if let Some(run) = self
                    .runs
                    .last_mut()
                    .filter(|r| r.depth == depth && r.style.is_none())
                {
                    run.style = Some(val()?);
                }
            }
            "w:noBreakHyphen" if numbering => {
                if let Some(para) = self.paras.last_mut() {
                    para.number.push('-');
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn close(&mut self, name: &str) -> Result<(), String> {
        match name {
            "w:pPrChange" | "w:rPrChange" => self.changes -= 1,
            "w:p" => {
                let para = self
                    .paras
                    .pop()
                    .ok_or("a paragraph closes that never opened")?;
                if self.runs.last().is_some_and(|r| r.depth > self.paras.len()) {
                    return Err("a paragraph closes inside one of its runs".to_string());
                }
                if para.style.as_deref() == Some(HEADING_STYLE) && !para.number.is_empty() {
                    self.found.push(para.number);
                }
            }
            "w:r" => {
                self.runs.pop().ok_or("a run closes that never opened")?;
            }
            "w:t" => self.in_text = false,
            _ => {}
        }
        Ok(())
    }

    fn text(&mut self, text: &str) {
        if self.in_text
            && self.numbering()
            && let Some(para) = self.paras.last_mut()
        {
            para.number.push_str(text);
        }
    }
}

/// The identifiers a Word body's `ActHead5` paragraphs number, in document
/// order: each paragraph's `CharSectno` run text, joined, with
/// `<w:noBreakHyphen/>` read as `-`. The HTML tokenizer the EPUB reader uses
/// reads this XML; only the exact OOXML spellings of names count (`Spelling`).
/// A document that is not well formed, or a paragraph or run that closes
/// without opening or is still open at the end, is refused.
pub(super) fn word_sections(xml: &str) -> Result<Vec<String>, String> {
    well_formed(xml)?;
    let (mut body, mut spelling) = (Body::default(), Spelling::new(xml));
    htmlparser::parse_each(xml, true, |positioned| {
        let name = written(&mut spelling, &positioned)?;
        let opens = matches!(positioned.event, Event::StartTag(_, _));
        match &positioned.event {
            Event::StartTag(_, attrs) | Event::StartEndTag(_, attrs)
                if !(opens && body.open(name)) =>
            {
                body.mark(name, || {
                    spelling
                        .attribute(&positioned, attrs, "w:val")
                        .map(Option::unwrap_or_default)
                })?;
            }
            Event::EndTag(_) => body.close(name)?,
            Event::Data(text) => body.text(text),
            _ => {}
        }
        Ok::<(), String>(())
    })?;
    if !body.paras.is_empty() || !body.runs.is_empty() {
        return Err("the document ends inside a paragraph or run".into());
    }
    Ok(body.found)
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
pub(super) struct WordVolume {
    pub(super) compilation: Option<String>,
    pub(super) title: Option<String>,
    pub(super) sections: Vec<String>,
}

pub(super) fn word_volume(path: &str) -> Result<WordVolume, Exc> {
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
}
