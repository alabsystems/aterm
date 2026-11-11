// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A long control verb's entry laid out for a reader ([`Page`]). `help <verb>` prints its first
//! screen: the summary, USAGE (one row per form), OPTIONS (one row per option), REPLY, a few
//! EXAMPLES and the list of its sections; `help <verb> <section>` prints one section, a part of
//! the deep material. The server and `aterm ctl <verb> --help [<section>]` render the same
//! table ([`crate::control_verbs::VerbSpec::entry_lines`]), so the two never differ for one build.
//!
//! Every rendered line after an entry's first starts with whitespace, as every catalog entry's
//! continuation does: a reader of `help --full` finds an entry by the word at column 0. Every line
//! fits [`ENTRY_WRAP_COLUMNS`]: a word longer than the room breaks after a `|`.

use crate::control_verbs::ENTRY_WRAP_COLUMNS;

pub(crate) mod aos;
pub(crate) mod await_;
pub(crate) mod harness;
pub(crate) mod log;
pub(crate) mod meta;
pub(crate) mod spawn;
pub(crate) mod status;
pub(crate) mod survey;
pub(crate) mod task;
pub(crate) mod turn;

/// One row of a block: `term` in the left column, `text` wrapped beside it (an empty `term`:
/// `text` is a paragraph of its own).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    /// The form, option, field or request the row is about.
    pub term: &'static str,
    /// What it does, means or shows.
    pub text: &'static str,
}

/// A [`Row`]: `term`, then `text` beside it.
#[must_use]
pub const fn row(term: &'static str, text: &'static str) -> Row {
    Row { term, text }
}

/// A [`Row`] with no term: `text` as a paragraph.
#[must_use]
pub const fn para(text: &'static str) -> Row {
    Row { term: "", text }
}

/// One part of a page's deep material, opened by `help <verb> <name>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section {
    /// The word typed after `help <verb>`.
    pub name: &'static str,
    /// What it holds, in one line: its row in the page's MORE list and its own head.
    pub about: &'static str,
    /// Its body.
    pub rows: &'static [Row],
}

/// A verb's entry laid out for a reader: the first screen's blocks, in the order they print,
/// and the sections behind them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page {
    /// USAGE: one row per form of the request.
    pub usage: &'static [Row],
    /// OPTIONS: one row per option (empty: the block is left out).
    pub options: &'static [Row],
    /// REPLY: what the server answers.
    pub reply: &'static [Row],
    /// EXAMPLES: real requests, each with what it shows.
    pub examples: &'static [Row],
    /// The deep material, each part opened by `help <verb> <name>`.
    pub sections: &'static [Section],
}

/// The column a block's heading starts in.
const HEAD_INDENT: usize = 2;
/// The column a block's rows start in.
const ROW_INDENT: usize = 4;
/// The widest term that keeps its text beside it; a wider one takes a line of its own, its text
/// on the lines below at [`ROW_INDENT`] + 4.
const TERM_MAX: usize = 24;

impl Page {
    /// The first screen's lines after the entry's head: USAGE, OPTIONS, REPLY, EXAMPLES, then the
    /// MORE list naming each section of `verb`.
    #[must_use]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn body_lines(&self, verb: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (heading, rows) in [
            ("USAGE", self.usage),
            ("OPTIONS", self.options),
            ("REPLY", self.reply),
            ("EXAMPLES", self.examples),
        ] {
            if rows.is_empty() {
                continue;
            }
            out.push(format!("{}{heading}", " ".repeat(HEAD_INDENT)));
            rows_into(&mut out, rows);
        }
        if !self.sections.is_empty() {
            out.push(format!(
                "{}MORE — aterm ctl help {verb} <section>",
                " ".repeat(HEAD_INDENT)
            ));
            let more: Vec<Row> = self
                .sections
                .iter()
                .map(|s| Row {
                    term: s.name,
                    text: s.about,
                })
                .collect();
            rows_into(&mut out, &more);
        }
        out
    }

    /// The section named `name`, if the page has one.
    #[must_use]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn section(&self, name: &str) -> Option<&'static Section> {
        self.sections.iter().find(|s| s.name == name)
    }

    /// Every word of the page, sections included, on one line: the summary, then each row's term
    /// and text, then each section's head and rows, joined by single spaces. What a search of the
    /// whole entry reads ([`crate::control_verbs::VerbSpec::help_line`]).
    #[must_use]
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn text(&self, summary: &str) -> String {
        let mut words: Vec<&str> = vec![summary];
        let mut push = |rows: &[Row]| {
            for r in rows {
                words.extend([r.term, r.text].into_iter().filter(|w| !w.is_empty()));
            }
        };
        for rows in [self.usage, self.options, self.reply, self.examples] {
            push(rows);
        }
        for s in self.sections {
            push(&[Row {
                term: s.name,
                text: s.about,
            }]);
            push(s.rows);
        }
        words.join(" ")
    }
}

/// A section's body lines, after its head.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn section_body_lines(section: &Section) -> Vec<String> {
    let mut out = Vec::new();
    rows_into(&mut out, section.rows);
    out
}

/// `rows` laid out at [`ROW_INDENT`]: each term in a column as wide as the block's widest term
/// within [`TERM_MAX`], its text beside it; a wider term on a line of its own.
#[cfg_attr(trust_verify, trust::skip)]
fn rows_into(out: &mut Vec<String>, rows: &[Row]) {
    let col = rows
        .iter()
        .map(|r| r.term.chars().count())
        .filter(|w| *w <= TERM_MAX)
        .max()
        .unwrap_or(0);
    for r in rows {
        let width = r.term.chars().count();
        if r.term.is_empty() {
            wrap_into(out, " ".repeat(ROW_INDENT), ROW_INDENT, r.text);
        } else if width <= TERM_MAX {
            let lead = format!(
                "{}{}{}",
                " ".repeat(ROW_INDENT),
                r.term,
                " ".repeat(col - width + 2)
            );
            if r.text.is_empty() {
                out.push(lead.trim_end().to_string());
            } else {
                wrap_into(out, lead, ROW_INDENT + col + 2, r.text);
            }
        } else {
            wrap_into(out, " ".repeat(ROW_INDENT), ROW_INDENT + 2, r.term);
            if !r.text.is_empty() {
                wrap_into(out, " ".repeat(ROW_INDENT + 4), ROW_INDENT + 4, r.text);
            }
        }
    }
}

/// `text`'s words after `lead` on the first line and after `hang` columns on the rest, each line
/// at most [`ENTRY_WRAP_COLUMNS`] wide; a word wider than a whole line's room breaks after a `|`,
/// each later piece starting a line.
#[cfg_attr(trust_verify, trust::skip)]
fn wrap_into(out: &mut Vec<String>, lead: String, hang: usize, text: &str) {
    let room = ENTRY_WRAP_COLUMNS.saturating_sub(hang).max(1);
    let mut line = lead;
    let mut width = line.chars().count();
    let mut fresh = true;
    let mut break_line = |line: &mut String, width: &mut usize, fresh: &mut bool| {
        out.push(std::mem::replace(line, " ".repeat(hang)));
        *width = hang;
        *fresh = true;
    };
    for word in text.split(' ').filter(|w| !w.is_empty()) {
        for (i, piece) in pieces(word, room).into_iter().enumerate() {
            let w = piece.chars().count();
            if !fresh && (i > 0 || width + 1 + w > ENTRY_WRAP_COLUMNS) {
                break_line(&mut line, &mut width, &mut fresh);
            }
            if !fresh {
                line.push(' ');
                width += 1;
            }
            line.push_str(piece);
            width += w;
            fresh = false;
        }
    }
    if !line.trim().is_empty() {
        out.push(line);
    }
}

/// `word` as it can be laid out in lines of `room` columns: whole when it fits, else cut after
/// the last `|` that keeps each piece within `room` (a piece with no `|` to cut at stays whole).
#[cfg_attr(trust_verify, trust::skip)]
fn pieces(word: &str, room: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = word;
    while rest.chars().count() > room {
        let mut cut = None;
        for (i, c) in rest.char_indices() {
            if rest[..i].chars().count() >= room {
                break;
            }
            if c == '|' {
                cut = Some(i + 1);
            }
        }
        match cut {
            Some(at) if at < rest.len() => {
                out.push(&rest[..at]);
                rest = &rest[at..];
            }
            _ => break,
        }
    }
    out.push(rest);
    out
}

#[cfg(test)]
#[path = "verb_page_tests.rs"]
mod tests;
