// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: LicenseRef-Duguid-Public-Attribution-1.0

//! Find what a draft marks as a quotation, and grade it against one row's text.
//!
//! The grades are aasb-corpus `verify-quote`'s: `exact`, `normalised`, `partial` and
//! `none`, strongest first. Comparison is case-sensitive and runs on whole words, so
//! `deductible` is not found in `nondeductible`, nor `1` in `1.5`. A grade compares
//! words with a row of the EPUB reading view. It does not show the quotation is fair
//! to its context, that the row is authorised text or that it applied at any date.
use super::as_at::literal;
use crate::pyre;

/// Fewer consecutive words than this are too common to locate a quotation.
const PARTIAL_MIN_WORDS: usize = 4;

/// One passage in a draft's quotation marks, with its byte range, marks included.
pub struct Quotation {
    pub text: String,
    pub start: usize,
    pub end: usize,
    /// False if no closing mark was found. `text` then runs to the paragraph's end,
    /// so it is reported but never graded, and `end` stops after the opening mark.
    pub closed: bool,
}

/// The marks read, each opening mark with its closing one.
const MARKS: [(char, char); 4] = [('“', '”'), ('"', '"'), ('‘', '’'), ('\'', '\'')];

/// Every quotation in `draft`. None runs past a blank line.
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
/// mark. Reading on can only join text the author did not quote, which lowers a grade;
/// stopping early could grade a fragment of the quotation as `exact`.
fn scan(paragraph: &str, base: usize, found: &mut Vec<Quotation>) {
    let chars: Vec<(usize, char)> = paragraph.char_indices().collect();
    let at = |i: usize| chars.get(i).map(|&(_, c)| c);
    let opens = |i: usize| {
        !i.checked_sub(1)
            .and_then(at)
            .is_some_and(char::is_alphanumeric)
            && at(i + 1).is_some_and(|c| !c.is_whitespace())
    };
    let closes = |i: usize| {
        i.checked_sub(1)
            .and_then(at)
            .is_some_and(|c| !c.is_whitespace())
            && !at(i + 1).is_some_and(char::is_alphanumeric)
    };
    let mut i = 0;
    while i < chars.len() {
        let (place, mark) = chars[i];
        let pair = MARKS.iter().find(|(open, _)| *open == mark);
        let Some(&(open, close)) = pair.filter(|_| opens(i)) else {
            i += 1;
            continue;
        };
        let single = matches!(open, '‘' | '\'');
        let limit = (i + 1..chars.len())
            .find(|&j| single && chars[j].1 == open && opens(j))
            .unwrap_or(chars.len());
        let mut closing = (i + 1..limit).filter(|&j| chars[j].1 == close && closes(j));
        let closing = if single {
            closing.next_back()
        } else {
            closing.next()
        };
        let inside = place + mark.len_utf8();
        let (text, end, next) = match closing {
            Some(j) => (
                &paragraph[inside..chars[j].0],
                chars[j].0 + close.len_utf8(),
                j + 1,
            ),
            None => (&paragraph[inside..], inside, i + 1),
        };
        if text.chars().any(char::is_alphanumeric) {
            found.push(Quotation {
                text: text.to_string(),
                start: base + place,
                end: base + end,
                closed: closing.is_some(),
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
        Some('.' | ',') => {
            edge.is_ascii_digit() && beyond.next().is_some_and(|c| c.is_ascii_digit())
        }
        Some('\'' | '’') => {
            edge.is_alphanumeric() && beyond.next().is_some_and(char::is_alphanumeric)
        }
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

/// What `normalised` compares: `literal`'s Unicode folding, one kind of quotation mark
/// and apostrophe, single spaces, and without what the corpus or the Register adds to the
/// words: the Markdown mark opening a line and the asterisk before a defined term.
fn folded(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(|line| {
            let line = line.trim_start();
            let line = line.strip_prefix("> ").unwrap_or(line);
            line.strip_prefix("- ").unwrap_or(line)
        })
        .collect();
    let plain = literal(&lines.join(" "));
    let mut out = String::with_capacity(plain.len());
    let mut chars = plain.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek().is_some_and(|n| n.is_alphanumeric()) => {}
            '‘' | '’' => out.push('\''),
            '“' | '”' => out.push('"'),
            _ => out.push(c),
        }
    }
    crate::py::split_whitespace(&out).join(" ")
}

/// A quotation's grade, with (matched, total) words for `partial`.
pub(super) fn grade(quote: &str, text: &str) -> (&'static str, Option<(usize, usize)>) {
    if holds(text, quote) {
        return ("exact", None);
    }
    let (quote, text) = (folded(quote), folded(text));
    if holds(&text, &quote) {
        return ("normalised", None);
    }
    let quote_words = crate::py::split_whitespace(&quote);
    // Longest common run of words: run[j] is the run ending at quotation word j.
    let mut run = vec![0; quote_words.len() + 1];
    let mut longest = 0;
    for word in crate::py::split_whitespace(&text) {
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
        return ("none", None);
    }
    ("partial", Some((longest, quote_words.len())))
}
