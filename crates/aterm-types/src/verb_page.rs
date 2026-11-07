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
mod tests {
    use super::*;
    use crate::control_verbs::{CATALOG_TEXT_COLUMN, TURN_OPTIONS, VERBS, VerbSpec, spec};

    /// The verbs whose entry is a page.
    fn paged() -> impl Iterator<Item = (&'static VerbSpec, &'static Page)> {
        VERBS.iter().filter_map(|s| s.page.map(|p| (s, p)))
    }

    /// The words of `lines`, a piece a long word broke after its `|` joined back to the rest.
    fn words(lines: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut glue = false;
        for line in lines {
            for (i, word) in line.split_whitespace().enumerate() {
                match out.last_mut() {
                    Some(last) if glue && i == 0 => last.push_str(word),
                    _ => out.push(word.to_string()),
                }
            }
            glue = out.last().is_some_and(|w| w.ends_with('|'));
        }
        out
    }

    /// Whether `needle` stands in `hay` as one run.
    fn holds_run(hay: &[String], needle: &[&str]) -> bool {
        needle.is_empty()
            || hay
                .windows(needle.len())
                .any(|w| w.iter().zip(needle).all(|(a, b)| a == b))
    }

    /// Every page renders as a catalog entry: its first line leads with the name, every later
    /// line starts with whitespace (how a reader of `help --full` finds the next entry), every
    /// line fits the wrap, the first screen fits one screen, and nothing a row says is lost.
    #[test]
    fn every_page_renders_whole_within_the_wrap_and_one_screen() {
        assert!(paged().count() >= 10, "the long entries are pages");
        for (verb, page) in paged() {
            assert!(
                verb.detail.is_empty(),
                "{}: a page and a paragraph",
                verb.name
            );
            let mut all = vec![verb.entry_lines()];
            all.extend(page.sections.iter().map(|s| {
                verb.section_lines(s.name)
                    .unwrap_or_else(|| panic!("{} {}: a listed section renders", verb.name, s.name))
            }));
            for (lines, head) in all.iter().zip(
                std::iter::once(verb.name.to_string()).chain(
                    page.sections
                        .iter()
                        .map(|s| format!("{} {}", verb.name, s.name)),
                ),
            ) {
                assert!(
                    lines[0].starts_with(&format!("{head:<w$} ", w = CATALOG_TEXT_COLUMN - 1)),
                    "{head}: {lines:#?}"
                );
                for line in lines {
                    assert!(
                        line.chars().count() <= ENTRY_WRAP_COLUMNS,
                        "`help {head}` has a line of {} columns: {line}",
                        line.chars().count()
                    );
                    assert!(
                        line.trim_end() == line && !line.trim().is_empty(),
                        "{head}: {line:?}"
                    );
                }
                for line in &lines[1..] {
                    assert!(line.starts_with(' '), "{head}: {line:?} starts at column 0");
                }
            }
            let first = &all[0];
            let basics = first
                .iter()
                .take_while(|l| !l.trim_start().starts_with("MORE"))
                .count();
            assert!(
                basics <= 66,
                "`help {}`'s basics are {basics} lines",
                verb.name
            );
            let rendered: Vec<String> = all.iter().flat_map(|l| words(l)).collect();
            for r in [page.usage, page.options, page.reply, page.examples]
                .into_iter()
                .chain(page.sections.iter().map(|s| s.rows))
                .flatten()
            {
                for part in [r.term, r.text] {
                    let want: Vec<&str> = part.split_whitespace().collect();
                    assert!(holds_run(&rendered, &want), "{}: lost {part:?}", verb.name);
                }
            }
            assert_eq!(verb.full_lines(), all.concat(), "{}", verb.name);
        }
    }

    /// Each page lists every section in its MORE block, once, and no section's word is a sub-form
    /// row of the verb (`help inbox get` is a row, so a section named `get` could not be asked
    /// for); an unknown section is none.
    #[test]
    fn every_section_is_listed_once_and_shadows_no_row() {
        for (verb, page) in paged() {
            let first = verb.entry_lines();
            let more = first
                .iter()
                .position(|l| l.trim_start().starts_with("MORE"))
                .unwrap_or_else(|| panic!("{}: no MORE block", verb.name));
            for (i, s) in page.sections.iter().enumerate() {
                assert!(
                    first[more + 1..]
                        .iter()
                        .any(|l| l.trim_start().starts_with(&format!("{} ", s.name))),
                    "{} {}: not in MORE",
                    verb.name,
                    s.name
                );
                assert!(
                    page.sections[..i].iter().all(|t| t.name != s.name),
                    "{} {}: twice",
                    verb.name,
                    s.name
                );
                assert!(
                    spec(&format!("{} {}", verb.name, s.name)).is_none(),
                    "{} {}: a row's name",
                    verb.name,
                    s.name
                );
                assert!(!s.rows.is_empty(), "{} {}: empty", verb.name, s.name);
            }
            assert!(verb.section_lines("no-such-section").is_none());
        }
        assert!(
            spec("text")
                .and_then(|s| s.section_lines("anything"))
                .is_none()
        );
    }

    /// `help turn` has an OPTIONS row for every option the server reads before a turn's text
    /// ([`TURN_OPTIONS`]), and none for a word it does not read: a new option cannot go unlisted.
    #[test]
    fn the_turn_page_lists_exactly_the_options_the_server_reads() {
        let page = spec("turn").and_then(|s| s.page).expect("turn is a page");
        let listed: Vec<&str> = page
            .options
            .iter()
            .map(|r| r.term.split_once('=').map_or(r.term, |(k, _)| k))
            .collect();
        assert_eq!(listed, TURN_OPTIONS, "help turn's OPTIONS");
    }

    /// The renderer: a term within [`TERM_MAX`] keeps its text beside it in one column, a wider
    /// one takes its own line, a paragraph wraps at the row indent, and a word wider than a line
    /// breaks after a `|` and nowhere else.
    #[test]
    fn rows_align_wrap_and_break_long_words_after_a_bar() {
        let wide: &'static str = Box::leak("t".repeat(TERM_MAX + 1).into_boxed_str());
        let mut out = Vec::new();
        rows_into(
            &mut out,
            &[
                row("a=<n>", "one"),
                row("bb", "two"),
                row(wide, "below"),
                para("x"),
            ],
        );
        assert_eq!(
            out,
            [
                "    a=<n>  one".to_string(),
                "    bb     two".to_string(),
                format!("    {}", "t".repeat(TERM_MAX + 1)),
                "        below".to_string(),
                "    x".to_string(),
            ]
        );
        let long = format!(
            "<{}>",
            (0..40)
                .map(|i| format!("w{i}"))
                .collect::<Vec<_>>()
                .join("|")
        );
        let mut out = Vec::new();
        wrap_into(
            &mut out,
            "    ".to_string(),
            4,
            &format!("lead {long} tail"),
        );
        assert!(
            out.iter().all(|l| l.chars().count() <= ENTRY_WRAP_COLUMNS),
            "{out:#?}"
        );
        assert_eq!(words(&out), ["lead", long.as_str(), "tail"]);
        assert!(out.len() >= 3, "{out:#?}");
    }
}
