// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

//! Find what a draft marks as a quotation, and grade it against one row's text.
//!
//! The grades are aasb-corpus `verify-quote`'s: `exact`, `normalised`, `partial` and
//! `none`, strongest first. Comparison is case-sensitive and runs on whole words, so
//! `deductible` is not found in `nondeductible`, nor `10` in `10%`. A grade compares
//! words with a row of the EPUB reading view. It does not show the quotation is fair
//! to its context, that the row is authorised text or that it applied at any date.
use super::as_at::literal;
use crate::pyre;

/// Fewer consecutive words than this are too common to locate a quotation.
const PARTIAL_MIN_WORDS: usize = 4;

/// How much of a quotation with no closing mark is reported. Nothing grades it, and
/// copying the rest of the paragraph for each opening mark would be quadratic.
const UNCLOSED_SHOWN: usize = 200;

/// One passage in a draft's quotation marks, with its byte range, marks included.
pub struct Quotation {
    pub text: String,
    pub start: usize,
    pub end: usize,
    /// False if no closing mark was found. `text` then holds the start of the
    /// paragraph's remainder, so it is reported but never graded, and `end` stops
    /// after the opening mark.
    pub closed: bool,
    /// True for a single quotation with several candidates for its closing mark. Where
    /// it ends is then a guess, so `text` may hold more than the author quoted.
    pub loose: bool,
}

impl Quotation {
    /// Whether this quotation overlaps the byte range of a sentence.
    pub(super) fn in_sentence(&self, (start, stop): (usize, usize)) -> bool {
        self.start < stop && start < self.end
    }

    /// Whether a citation ending at `end` is known to be inside this quotation.
    pub(super) fn surrounds(&self, end: usize) -> bool {
        self.closed && !self.loose && self.start < end && end <= self.end
    }
}

/// The marks read, each opening mark with its closing one.
const MARKS: [(char, char); 4] = [('“', '”'), ('"', '"'), ('‘', '’'), ('\'', '\'')];

/// Every quotation in `draft`, in order. None runs past a blank line.
pub(super) fn quotations(draft: &str) -> Vec<Quotation> {
    let mut found = Vec::new();
    let mut start = 0;
    for gap in pyre::re(r"\n\s*\n", 0).captures_iter(draft) {
        let gap = gap.get(0).expect("group 0");
        scan(&draft[start..gap.start()], start, &mut found);
        start = gap.end();
    }
    scan(&draft[start..], start, &mut found);
    found
}

/// shortcut: a single mark closing a quotation looks like the apostrophe in `taxpayers'
/// income`, so a single quotation closes at the last candidate before the next opening
/// mark and is marked `loose` if there were several. Reading on can only join text the
/// author did not quote, which lowers a grade; stopping early could grade a fragment of
/// the quotation as `exact`. A loose quotation never excuses a citation it runs over.
fn scan(paragraph: &str, base: usize, found: &mut Vec<Quotation>) {
    let chars: Vec<(usize, char)> = paragraph.char_indices().collect();
    let at = |i: usize| chars.get(i).map(|&(_, c)| c);
    let before = |i: usize| i.checked_sub(1).and_then(at);
    // A mark before white space opens too, so a stray space cannot hide a quotation. A
    // straight single mark must then also stand clear of the text before it, because
    // after punctuation it is the closing mark in `'costs.' and`.
    let opens = |i: usize| {
        let tight = at(i + 1).is_some_and(|c| !c.is_whitespace());
        let clear = chars[i].1 != '\''
            || before(i).is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '['));
        !before(i).is_some_and(char::is_alphanumeric) && (tight || clear)
    };
    let closes = |i: usize| {
        before(i).is_some_and(|c| !c.is_whitespace())
            && !at(i + 1).is_some_and(char::is_alphanumeric)
    };
    let places = |wanted: char, test: &dyn Fn(usize) -> bool| -> Vec<usize> {
        let hits = |i: &usize| chars[*i].1 == wanted && test(*i);
        (0..chars.len()).filter(hits).collect()
    };
    let marks: Vec<(Vec<usize>, Vec<usize>)> = MARKS
        .iter()
        .map(|&(open, close)| (places(open, &opens), places(close, &closes)))
        .collect();
    let mut i = 0;
    while i < chars.len() {
        let (place, mark) = chars[i];
        let kind = MARKS.iter().position(|(open, _)| *open == mark);
        let Some(kind) = kind.filter(|_| opens(i)) else {
            i += 1;
            continue;
        };
        let single = matches!(mark, '‘' | '\'');
        let (openers, closers) = &marks[kind];
        let later = openers.partition_point(|&j| j <= i);
        let limit = openers.get(later).filter(|_| single);
        let limit = limit.copied().unwrap_or(chars.len());
        let first = closers.partition_point(|&j| j <= i);
        let candidates = &closers[first..closers.partition_point(|&j| j < limit)];
        let closing = if single {
            candidates.last()
        } else {
            candidates.first()
        };
        let inside = place + mark.len_utf8();
        let (text, end, next) = match closing {
            Some(&j) => {
                let close = MARKS[kind].1.len_utf8();
                (
                    paragraph[inside..chars[j].0].trim().to_string(),
                    chars[j].0 + close,
                    j + 1,
                )
            }
            None => {
                let shown = paragraph[inside..].trim_start().chars();
                (shown.take(UNCLOSED_SHOWN).collect(), inside, i + 1)
            }
        };
        if closing.is_none() || text.chars().any(char::is_alphanumeric) {
            found.push(Quotation {
                text,
                start: base + place,
                end: base + end,
                closed: closing.is_some(),
                loose: single && candidates.len() > 1,
            });
        }
        i = next;
    }
}

/// Whether what lies beyond a quotation's `edge` character carries on its word or
/// number, so that the quotation stops part way through one.
fn runs_on(edge: char, mut beyond: impl Iterator<Item = char>) -> bool {
    let word = |c: char| c.is_alphanumeric() || matches!(c, '-' | '\u{2010}' | '\u{2011}');
    match beyond.next() {
        Some(c) if matches!(edge, '.' | ',') && c.is_ascii_digit() => true,
        Some('.' | ',') => {
            edge.is_ascii_digit() && beyond.next().is_some_and(|c| c.is_ascii_digit())
        }
        Some('\'' | '’') => {
            edge.is_alphanumeric() && beyond.next().is_some_and(char::is_alphanumeric)
        }
        Some('%' | '/' | '–' | '—') => edge.is_ascii_digit(),
        Some(c) => word(c) && word(edge),
        None => false,
    }
}

/// Whether `quote` appears in `text` from the start of a word to the end of one.
fn holds(text: &str, quote: &str) -> bool {
    let (Some(first), Some(last)) = (quote.chars().next(), quote.chars().next_back()) else {
        return false;
    };
    text.match_indices(quote).any(|(at, _)| {
        !runs_on(first, text[..at].chars().rev())
            && !runs_on(last, text[at + quote.len()..].chars())
    })
}

/// `literal`'s Unicode folding, one kind of quotation mark and apostrophe, single spaces
/// and no asterisk marking a defined term. A `*` after a letter or digit stays, so `5*3`
/// never reads as `53`.
fn folded(text: &str) -> String {
    let plain = literal(text);
    let mut out = String::with_capacity(plain.len());
    let mut chars = plain.chars().peekable();
    let mut previous = None;
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek().is_some_and(|n| n.is_alphanumeric())
                && !previous.is_some_and(char::is_alphanumeric) => {}
            '‘' | '’' => out.push('\''),
            '“' | '”' => out.push('"'),
            _ => out.push(c),
        }
        previous = Some(c);
    }
    crate::py::split_whitespace(&out).join(" ")
}

/// A row as `normalised` compares it: `folded`, without the Markdown mark the corpus
/// opens a line with. Only a row loses these: they are never in a quotation's favour.
pub fn folded_row(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(|line| {
            let line = line.trim_start();
            let line = line.strip_prefix("> ").unwrap_or(line);
            line.strip_prefix("- ").unwrap_or(line)
        })
        .collect();
    folded(&lines.join(" "))
}

/// What comparing a quotation with one row gave.
pub struct Grade {
    pub grade: &'static str,
    /// The longest run of matching words, for `partial`.
    pub matched_words: Option<usize>,
    pub quote_words: usize,
}

impl Grade {
    /// Whether the row holds the whole quotation.
    pub(super) fn held(&self) -> bool {
        matches!(self.grade, "exact" | "normalised")
    }
}

/// A quotation's grade against one row's text.
pub(super) fn grade(quote: &str, text: &str) -> Grade {
    let folded_quote = folded(quote);
    let quote_words = crate::py::split_whitespace(&folded_quote);
    let result = |grade, matched_words| Grade {
        grade,
        matched_words,
        quote_words: quote_words.len(),
    };
    if holds(text, quote) {
        return result("exact", None);
    }
    let folded_text = folded_row(text);
    if holds(&folded_text, &folded_quote) {
        return result("normalised", None);
    }
    // Longest common run of words: run[j] is the run ending at quotation word j.
    let mut run = vec![0; quote_words.len() + 1];
    let mut longest = 0;
    for word in crate::py::split_whitespace(&folded_text) {
        for j in (1..=quote_words.len()).rev() {
            run[j] = if quote_words[j - 1] == word {
                run[j - 1] + 1
            } else {
                0
            };
            longest = longest.max(run[j]);
        }
    }
    if longest < PARTIAL_MIN_WORDS {
        return result("none", None);
    }
    result("partial", Some(longest))
}
