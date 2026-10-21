// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `fill-paragraph` (M-q): refill the paragraph at the primary caret so its
//! lines run as close to the fill column as their words allow.
//!
//! A paragraph is the run of non-blank lines around the caret that share
//! one comment leader (`//`, `#`, `>` and the rest of [`LEADERS`]), so a
//! comment block fills as a comment and a commit message's body fills as
//! prose. A list item (`- `, `* `, `+ `) starts a paragraph of its own, and
//! its later lines hang under its text. Words are separated by one space;
//! nothing inside a word changes. The refill is one edit, so one undo takes
//! it back, and the caret stays on the character it was on.

use std::ops::Range;

use super::projection::{document_line_break, editor_display_column, line_end, line_start};
use super::view::{EditorBufferView, Selection};
use super::workspace::{EditorEffect, EditorError, EditorWorkspace, snapshot_for_view};
use crate::store::{DocumentStore, TextEdit};

/// The comment leaders a paragraph's lines can share, longest first so `///`
/// is never read as `//`.
const LEADERS: &[&str] = &["///", "//!", "//", ";;", "--", "#", ">"];

/// The markers that start a list item.
const BULLETS: &[&str] = &["- ", "* ", "+ "];

/// The columns left on a line `line_width` columns wide before
/// `fill_column`: none once the line reaches it.
fn room(line_width: u64, fill_column: u64) -> u64 {
    fill_column.saturating_sub(line_width)
}

trust_contract_fn! {
    /// Whether a word `word_width` columns wide fits in the `room` a line
    /// has left before the fill column ([`room`]), after the one space that
    /// separates it from the word before: it needs `word_width + 1`
    /// columns, so it fits exactly when it is narrower than the room. With
    /// `room = fill_column - line_width` that is
    /// `line_width + 1 + word_width <= fill_column`, decided without the
    /// addition, which could overflow.
    #[must_use]
    pub(super) fn word_fits(room: u64, word_width: u64) -> bool
    clauses [ensures result == (word_width < room) by word_fits_is_narrower_than_the_room]
    {
        word_width < room
    }
}

// Spec `word-fits` (SPECS.md): a word joins a line exactly when it is
// narrower than the room the line has left, its separating space included.
// Kernel-checked against the compiled body. (The comparison takes the room,
// not the line and the column: E6 mints no kernel import for a body with a
// checked subtraction's overflow assert, so the subtraction stays in
// `room`, which saturates.)
verify_island! {
    clean {
        theorem word_fits_is_narrower_than_the_room (room word_width : UInt64) :
            trust_import_amacs__editor__fill__word_fits room word_width =
              decide (word_width < room) := by
          simp only [trust_import_amacs__editor__fill__word_fits]
    }
}

/// One line of a paragraph: its byte range in the document (up to its
/// `\n`), its leader (indent, comment marker and the spaces after it) and
/// where its body ends: before the `\r` of a CRLF ending, so a CRLF file's
/// `\r` never reads as a space between words.
#[derive(Clone, Debug)]
struct ParagraphLine {
    range: Range<usize>,
    leader: Range<usize>,
    body_end: usize,
}

impl ParagraphLine {
    fn read(text: &str, start: usize) -> Self {
        let end = line_end(text, start);
        let line = &text[start..end];
        let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
        let marker = LEADERS
            .iter()
            .find(|leader| line[indent..].starts_with(*leader))
            .map_or(0, |leader| leader.len());
        let after_marker = &line[indent + marker..];
        let spaces = if marker == 0 {
            0
        } else {
            after_marker.len() - after_marker.trim_start_matches([' ', '\t']).len()
        };
        Self {
            range: start..end,
            leader: start..start + indent + marker + spaces,
            // A leader is spaces, tabs and a marker, never `\r`, so it
            // ends at or before the body's end.
            body_end: if line.ends_with('\r') {
                end.saturating_sub(1)
            } else {
                end
            },
        }
    }

    /// The line break after this line: `"\r\n"`, `"\n"`, or `None` for the
    /// document's last line.
    fn ending(&self, text: &str) -> Option<&'static str> {
        (self.range.end < text.len()).then_some(if self.body_end < self.range.end {
            "\r\n"
        } else {
            "\n"
        })
    }

    /// The comment marker, without the indent or the spaces around it.
    fn marker<'a>(&self, text: &'a str) -> &'a str {
        text[self.leader.clone()].trim_matches([' ', '\t'])
    }

    fn body<'a>(&self, text: &'a str) -> &'a str {
        &text[self.leader.end..self.body_end]
    }

    fn is_blank(&self, text: &str) -> bool {
        self.body(text).trim().is_empty()
    }

    fn bullet(&self, text: &str) -> Option<&'static str> {
        BULLETS
            .iter()
            .copied()
            .find(|bullet| self.body(text).starts_with(bullet))
    }

    /// Whether `next`, the line below this one, continues this line's
    /// paragraph: both have text, under one comment marker, and `next` is
    /// not a list item of its own.
    fn continues_into(&self, next: &Self, text: &str) -> bool {
        !self.is_blank(text)
            && !next.is_blank(text)
            && next.bullet(text).is_none()
            && next.marker(text) == self.marker(text)
    }
}

/// The display width of `text` set from column 0.
fn width(text: &str) -> u64 {
    u64::try_from(editor_display_column(text, text.len(), 0)).unwrap_or(u64::MAX)
}

impl EditorWorkspace {
    pub(super) fn fill_paragraph(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let text: &str = &snapshot.text;
        let caret = view.primary_selection().head.min(text.len());
        let here = ParagraphLine::read(text, line_start(text, caret));
        if here.is_blank(text) {
            return Ok(vec![EditorEffect::Status(
                "No paragraph to fill here".into(),
            )]);
        }
        // Up to the paragraph's first line, collected bottom up and turned
        // over once, so a paragraph of many lines costs one pass.
        let mut lines = Vec::new();
        let mut first = here;
        while first.bullet(text).is_none() && first.range.start > 0 {
            let above = ParagraphLine::read(text, line_start(text, first.range.start - 1));
            if !above.continues_into(&first, text) {
                break;
            }
            lines.push(std::mem::replace(&mut first, above));
        }
        lines.push(first);
        lines.reverse();
        while let Some(last) = lines.last()
            && last.range.end < text.len()
        {
            let below = ParagraphLine::read(text, last.range.end + 1);
            if !last.continues_into(&below, text) {
                break;
            }
            lines.push(below);
        }
        // The lines break as the paragraph's own do (CRLF stays CRLF), or
        // as the document's, for a last line with no ending.
        let line_break = lines
            .iter()
            .find_map(|line| line.ending(text))
            .unwrap_or_else(|| document_line_break(text));

        // The first line keeps its leader (and its bullet); the lines after it
        // take the second line's leader, or hang under a bullet's text.
        let first = &lines[0];
        let bullet = first.bullet(text);
        let first_prefix = format!(
            "{}{}",
            &text[first.leader.clone()],
            bullet.unwrap_or_default()
        );
        let rest_prefix = match (bullet, lines.get(1)) {
            (Some(bullet), _) => format!(
                "{}{}",
                &text[first.leader.clone()],
                " ".repeat(bullet.len())
            ),
            (None, Some(second)) => text[second.leader.clone()].to_string(),
            (None, None) => text[first.leader.clone()].to_string(),
        };
        let mut words = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            let body_start = line.leader.end
                + if index == 0 {
                    bullet.map_or(0, str::len)
                } else {
                    0
                };
            let body = &text[body_start..line.body_end];
            let mut word_start = None;
            for (at, character) in body.char_indices() {
                if !character.is_whitespace() {
                    word_start.get_or_insert(at);
                } else if let Some(start) = word_start.take() {
                    words.push(body_start + start..body_start + at);
                }
            }
            if let Some(start) = word_start {
                words.push(body_start + start..line.body_end);
            }
        }
        let paragraph = lines[0].range.start..lines[lines.len() - 1].body_end;

        let fill_column = u64::try_from(self.options.fill_column).unwrap_or(u64::MAX);
        let mut filled = first_prefix.clone();
        let mut line_width = width(&first_prefix);
        let mut line_has_word = false;
        let mut placed = Vec::with_capacity(words.len());
        for word in &words {
            let word_text = &text[word.clone()];
            let word_width = width(word_text);
            if line_has_word && !word_fits(room(line_width, fill_column), word_width) {
                filled.push_str(line_break);
                filled.push_str(&rest_prefix);
                line_width = width(&rest_prefix);
                line_has_word = false;
            }
            if line_has_word {
                filled.push(' ');
                line_width = line_width.saturating_add(1);
            }
            placed.push(filled.len());
            filled.push_str(word_text);
            line_width = line_width.saturating_add(word_width);
            line_has_word = true;
        }
        if filled == text[paragraph.clone()] {
            return Ok(Vec::new());
        }

        // The caret stays on its character: the word it is in (or the next
        // one, from the space before it) at the same offset.
        let caret_at = words
            .iter()
            .zip(&placed)
            .find(|(word, _)| caret <= word.end)
            .map_or(paragraph.start + filled.len(), |(word, placed)| {
                paragraph
                    .start
                    .saturating_add(*placed)
                    .saturating_add(caret.saturating_sub(word.start))
            });
        let edit = TextEdit {
            range: paragraph,
            insert: filled,
        };
        self.apply_recorded(store, view, vec![edit])?;
        view.selections = vec![Selection::caret(caret_at)];
        view.primary = 0;
        self.last_yank = None;
        Ok(Vec::new())
    }
}
