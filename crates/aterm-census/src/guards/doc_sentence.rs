// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! doc-sentence: a comment does not end mid-sentence.
//!
//! A doc comment whose last line ends on a joining word (`and`, `the`, `of`, …) or a comma
//! was cut short: a prune that keeps only each paragraph's first line leaves exactly that,
//! rustdoc shows the fragment as the item's summary, and fmt, lint and doc all pass it. Read
//! over the `///` and `//!` blocks of every Rust file under `crates/` and `apps/`, at each
//! block's last line and, outside a code fence, at the line before a heading (`# Errors`):
//! markdown lets a heading interrupt a paragraph, so a paragraph cut above one runs straight
//! into it. A last line ending on `e.g.` or `i.e.` was cut at the abbreviation's period. A
//! heading names a section, not a sentence. A doc's last line ending on a colon introduced a
//! list or an example that a prune deleted; a plain `//` comment may end on one to introduce
//! the code below.
//!
//! A cut at a period inside parentheses, a code span or a quote (`(its journal.`) leaves
//! what it opened open, and a cut that adds its own period leaves an article or conjunction
//! before it (`…, or.`), so every paragraph is also read whole at its end for either. A
//! `// SAFETY:` note, the proof its `unsafe` block stands on, is read the same way, and so is
//! every other `//` comment on lines of its own (a URL's tail copied out of a string,
//! `// 127.0.0.1:9".into()`, is one). A SAFETY note with no word outside its code spans
//! (``SAFETY: `-setContentMinSize:`.``) is cut too: it names what it is about and states
//! nothing of it.

use super::rules::PRODUCT;
use super::{Finding, Rule, Tree};

pub(super) const DOC_SENTENCE: Rule = Rule {
    id: "doc-sentence",
    why: "a comment that ends on a joining word or a comma, or leaves a parenthesis, code span \
          or quote open, was cut mid-sentence, and a doc that ends on a colon lost what it \
          introduced; finish the sentence or put the list back (code it shows goes in a span)",
    scope: &[],
    check: doc_sentence,
};

/// Words a sentence never ends on. `in`, `on` and `so` are left out: a sentence can end on
/// one as a particle ("zoomed in", "turned on", "does so").
const JOINING: &[&str] = &[
    "a", "an", "and", "as", "but", "by", "for", "from", "is", "its", "of", "or", "that", "the",
    "this", "to", "when", "where", "which", "whose", "with",
];

/// Abbreviations whose period ends no sentence: a prune that cuts at the first period cuts
/// right after one.
const ABBREVIATIONS: &[&str] = &["e.g.", "i.e.", "cf.", "vs."];

/// Words a period never follows: a cut that adds its own period leaves one before it.
const BEFORE_NO_PERIOD: &[&str] = &["a", "an", "and", "nor", "or", "the"];

fn doc_sentence(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if !file.mentions(&["//"]) {
            continue;
        }
        let raw: Vec<&str> = file.raw.lines().collect();
        let marker = |no: usize| {
            let line = raw.get(no.wrapping_sub(1))?.trim_start();
            ["///", "//!", "//"]
                .into_iter()
                .find(|marker| line.starts_with(marker) && !line.starts_with("////"))
        };
        let body_of = |no: usize, own: &str| raw[no - 1].trim_start()[own.len()..].trim();
        let (mut fenced, mut safety, mut just_opened) = (false, false, false);
        let mut first_in_fence: Vec<&str> = Vec::new();
        let mut paragraph = String::new();
        for line in file.lines() {
            let code_free = line.text.trim().is_empty();
            let Some(own) = marker(line.no).filter(|_| code_free) else {
                (fenced, safety, just_opened) = (false, false, false);
                first_in_fence.clear();
                paragraph.clear();
                continue;
            };
            let body = body_of(line.no, own);
            let next = (marker(line.no + 1) == Some(own)).then(|| body_of(line.no + 1, own));
            safety = own == "//" && (safety || body.starts_with("SAFETY"));
            if body.starts_with("```") {
                fenced = !fenced;
                just_opened = fenced;
                paragraph.clear();
                if !fenced
                    && let Some(next_body) = next.filter(|s| !s.is_empty())
                    && first_in_fence
                        .iter()
                        .any(|first| first.starts_with(next_body))
                {
                    out.push(file.finding(
                        DOC_SENTENCE.id,
                        line.no + 1,
                        format!("a closing code fence is followed by its own code: `{next_body}`"),
                    ));
                }
                continue;
            }
            if just_opened {
                first_in_fence.push(body);
                just_opened = false;
            }
            let carried = next.is_none() && continues_in_attribute(&raw[line.no..]);
            if next.is_none() && !carried {
                just_opened = false;
                first_in_fence.clear();
            }
            let ends = next.map_or(!carried, |next| !fenced && heading(next));
            let paragraph_ends = next.map_or(!carried, |next| {
                next.is_empty()
                    || heading(next)
                    || next.starts_with("```")
                    || (own == "//" && next.starts_with("SAFETY"))
            });
            if !fenced && !heading(body) {
                paragraph.push_str(body);
                paragraph.push(' ');
            }
            let left_open = paragraph_ends && !fenced && open(paragraph.trim_end());
            let bare = paragraph_ends && safety && names_only_code(paragraph.trim_end());
            if paragraph_ends {
                paragraph.clear();
            }
            let colon = ends && own != "//" && !heading(body) && body.ends_with(':');
            if (ends && cut(body)) || colon || left_open || bare {
                let tail = &body[body.char_indices().rev().nth(47).map_or(0, |(at, _)| at)..];
                let what = match own {
                    "//" if safety => "a SAFETY note",
                    "//" => "a comment",
                    _ => "a doc comment",
                };
                let says = if bare {
                    "names only code"
                } else if colon {
                    "ends on a colon with nothing after it"
                } else {
                    "ends mid-sentence"
                };
                out.push(file.finding(
                    DOC_SENTENCE.id,
                    line.no,
                    format!("{what} {says}: `…{tail}`"),
                ));
            }
        }
    }
}

/// Whether the lines after a doc comment, `after`, open an attribute that carries on its
/// text: `#[doc = …]`, or a `#[cfg_attr(…)]` whose `doc = …` picks the next words per
/// platform (winit's crate doc links a method only where it exists).
fn continues_in_attribute(after: &[&str]) -> bool {
    let Some(first) = after.first().map(|line| line.trim_start()) else {
        return false;
    };
    let first = first.strip_prefix("#!").or_else(|| first.strip_prefix('#'));
    let Some(attr) = first.and_then(|rest| rest.strip_prefix('[')) else {
        return false;
    };
    if attr.starts_with("doc") {
        return true;
    }
    attr.starts_with("cfg_attr(")
        && after
            .iter()
            .take_while(|line| line.trim() != ")]")
            .any(|line| line.contains("doc ="))
}

/// Whether a doc line, `body`, is a markdown heading: `#`s, then a space or nothing.
fn heading(body: &str) -> bool {
    let rest = body.trim_start_matches('#');
    rest.len() < body.len() && (rest.is_empty() || rest.starts_with(' '))
}

/// Whether a paragraph's last line, `body`, stops mid-sentence.
fn cut(body: &str) -> bool {
    if heading(body) {
        return false;
    }
    body.ends_with(',')
        || body
            .rsplit(|c: char| c.is_whitespace() || c == '(')
            .next()
            .is_some_and(|word| ABBREVIATIONS.contains(&word))
        || body
            .rsplit(|c: char| !c.is_ascii_alphabetic())
            .next()
            .is_some_and(|word| JOINING.contains(&word))
}

/// Whether a whole paragraph, `text`, was cut inside something it opened (a code span, a
/// parenthesis or a double quote it never closes) or cut where it added a period.
fn open(text: &str) -> bool {
    let Some(prose) = outside_code(text) else {
        return true;
    };
    let prose = unquoted(&prose);
    prose.matches('(').count() > prose.matches(')').count()
        || prose.matches('"').count() % 2 == 1
        || text
            .strip_suffix('.')
            .and_then(|text| text.rsplit(|c: char| !c.is_ascii_alphabetic()).next())
            .is_some_and(|word| BEFORE_NO_PERIOD.contains(&word))
}

/// Whether a SAFETY note, `text`, has no word outside its code spans: it names what it is about
/// and states nothing of it.
fn names_only_code(text: &str) -> bool {
    let note = text.strip_prefix("SAFETY:").unwrap_or(text);
    outside_code(note).is_some_and(|prose| {
        !prose
            .split(|c: char| !c.is_ascii_alphabetic())
            .any(|word| word.len() > 1)
    })
}

/// `text` without its code spans, or `None` when one never closes: a run of backticks opens
/// a span that only a run of the same length closes, and a backslash makes one literal.
fn outside_code(text: &str) -> Option<String> {
    let mut prose = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('`') {
        let (before, from) = rest.split_at(at);
        prose.push_str(before);
        if before.ends_with('\\') {
            rest = &from[1..];
            continue;
        }
        let run = from.len() - from.trim_start_matches('`').len();
        let mut body = &from[run..];
        loop {
            let tail = &body[body.find(&from[..run])?..];
            let len = tail.len() - tail.trim_start_matches('`').len();
            body = &tail[len..];
            if len == run {
                break;
            }
        }
        rest = body;
    }
    prose.push_str(rest);
    Some(prose)
}

/// `prose` without the characters it names in single quotes (`'"'`, `'('`): a quoted
/// character is shown, not opened.
fn unquoted(prose: &str) -> String {
    let chars: Vec<char> = prose.chars().collect();
    let mut out = String::with_capacity(prose.len());
    let mut at = 0;
    while let Some(&c) = chars.get(at) {
        if c == '\'' && chars.get(at + 2) == Some(&'\'') {
            at += 3;
            continue;
        }
        out.push(c);
        at += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    #[test]
    fn a_doc_comment_does_not_end_mid_sentence() {
        let rust = "//! A module that\n\
                    //! does one thing.\n\
                    \n\
                    /// Reads the vault and\n\
                    fn cut() {}\n\
                    /// The current index,\n\
                    struct Comma;\n\
                    /// Reads the vault.\n\
                    fn whole() {}\n\
                    /// Each cell, except that\n\
                    ///\n\
                    /// * a zero-width one joins the cell before it.\n\
                    fn list() {}\n\
                    /// # Where it comes from\n\
                    fn heading() {}\n\
                    // A plain comment ends on the\n\
                    fn plain() { let x = 1; // a trailing one is code's\n\
                    }\n\
                    const S: &str = \"\n\
                    /// a string's text and\n\
                    \";\n\
                    /// The band's base\n\
                    fn noun() {}\n\
                    /// Something can be zoomed in\n\
                    const ZOOM_IN: u32 = 0x21;\n\
                    /// Built in the\n\
                    //! module doc after an item doc\n\
                    fn mixed() {}\n\
                    /// Reads the vault and\n\
                    /// # Errors\n\
                    /// Each cell, except that\n\
                    ///\n\
                    /// When it cannot.\n\
                    fn errs() {}\n\
                    /// ```\n\
                    /// let x = f(1,\n\
                    /// # 2);\n\
                    /// ```\n\
                    fn fenced() {}\n\
                    /// Phase 1 of\n\
                    /// #7877.\n\
                    fn issue() {}\n\
                    /// Human semver, e.g.\n\
                    const V: &str = \"0.2.0\";\n\
                    //! A model re-implemented with\n\
                    #![cfg_attr(\n\
                        unix,\n\
                        doc = \"[`pump`]\"\n\
                    )]\n\
                    //! on every desktop.\n\
                    /// Read by the\n\
                    #[doc = \"loop.\"]\n\
                    fn spelled() {}\n\
                    /// Reads the ledger and\n\
                    #[inline]\n\
                    fn attributed() {}\n\
                    /// Each lane, checked by:\n\
                    fn listless() {}\n\
                    /// Each lane, checked by:\n\
                    ///\n\
                    /// * its own baseline.\n\
                    fn listed() {}\n\
                    /// The steps:\n\
                    /// # Errors\n\
                    /// When it cannot.\n\
                    fn above_heading() {}\n\
                    // The layout:\n\
                    fn plain_colon() {}\n\
                    /// # Example:\n\
                    fn heading_colon() {}\n\
                    /// ```\n\
                    /// use aterm_x::f;\n\
                    /// ```\n\
                    /// use aterm_x::f;\n\
                    fn echoed_fence() {}\n";
        let found = run(&DOC_SENTENCE, &[("crates/aterm-x/src/lib.rs", rust)]);
        assert_eq!(
            spots(&found),
            [
                "crates/aterm-x/src/lib.rs:4",
                "crates/aterm-x/src/lib.rs:6",
                "crates/aterm-x/src/lib.rs:16",
                "crates/aterm-x/src/lib.rs:26",
                "crates/aterm-x/src/lib.rs:29",
                "crates/aterm-x/src/lib.rs:43",
                "crates/aterm-x/src/lib.rs:54",
                "crates/aterm-x/src/lib.rs:57",
                "crates/aterm-x/src/lib.rs:63",
                "crates/aterm-x/src/lib.rs:74",
            ]
        );
        assert_eq!(
            found[0].detail,
            "a doc comment ends mid-sentence: `…Reads the vault and`"
        );
        assert_eq!(
            found[7].detail,
            "a doc comment ends on a colon with nothing after it: `…Each lane, checked by:`"
        );
    }

    #[test]
    fn a_paragraph_or_safety_note_is_not_left_open() {
        let rust = "/// Whether a run writes beside its loop (its journal.\n\
                    fn paren() {}\n\
                    /// Encode `` into a 20-byte snapshot.\n\
                    fn emptied() {}\n\
                    /// The answer to \"a build is staged.\n\
                    fn quote() {}\n\
                    /// Exit (the cure), or.\n\
                    enum OnPoison {}\n\
                    /// Encode `(self, a)` into a snapshot (`u32`, then `u64`).\n\
                    fn closed() {}\n\
                    /// The run `[from, to)`; a literal \\` stays prose.\n\
                    fn range() {}\n\
                    /// German Shift+2 types '\"' and Shift+9 types '('.\n\
                    fn quoted() {}\n\
                    /// A first paragraph (cut here.\n\
                    ///\n\
                    /// A second one.\n\
                    fn two() {}\n\
                    fn f() {\n\
                    // SAFETY: `hwnd` is live and\n\
                    unsafe { g() };\n\
                    // SAFETY: the out-pointers name locals (whose lengths.\n\
                    unsafe { g() };\n\
                    // SAFETY: both pointers are live; the call is synchronous.\n\
                    unsafe { g() };\n\
                    // SAFETY: `-setContentMinSize:`.\n\
                    unsafe { g() };\n\
                    // SAFETY: `-frame` is `{CGRect}@:`.\n\
                    unsafe { g() };\n\
                    // A plain note (cut here.\n\
                    g();\n\
                    // The ids in `(lo, head]`, then `ESC ( B`.\n\
                    g();\n\
                    // 127.0.0.1:9\".into()\n\
                    }\n";
        let found = run(&DOC_SENTENCE, &[("crates/aterm-x/src/lib.rs", rust)]);
        assert_eq!(
            spots(&found),
            [1, 3, 5, 7, 15, 20, 22, 26, 30, 34]
                .map(|no| format!("crates/aterm-x/src/lib.rs:{no}"))
        );
        assert_eq!(
            found[5].detail,
            "a SAFETY note ends mid-sentence: `…SAFETY: `hwnd` is live and`"
        );
        assert_eq!(
            found[7].detail,
            "a SAFETY note names only code: `…SAFETY: `-setContentMinSize:`.`"
        );
        assert_eq!(
            found[9].detail,
            "a comment ends mid-sentence: `…127.0.0.1:9\".into()`"
        );
    }
}
