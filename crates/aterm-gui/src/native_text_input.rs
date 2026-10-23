// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Grapheme-safe native text-field state shared by Settings and document apps.

use std::ops::Range;

use aterm_grapheme::GraphemeClusters;

const HISTORY_LIMIT: usize = 100;
/// Maximum byte length of a native text input field.
pub(crate) const MAX_TEXT_INPUT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextSelection {
    pub(crate) anchor: usize,
    pub(crate) head: usize,
}

impl TextSelection {
    pub(crate) const fn caret(position: usize) -> Self {
        Self {
            anchor: position,
            head: position,
        }
    }

    pub(crate) fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub(crate) const fn is_caret(&self) -> bool {
        self.anchor == self.head
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImePreedit {
    pub(crate) text: String,
    /// Selection/caret supplied by the platform, in bytes within `text`.
    pub(crate) selection: Option<Range<usize>>,
}

/// Immutable field projection consumed by paint and accessibility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextInputProjection {
    pub(crate) text: String,
    pub(crate) selection: TextSelection,
    pub(crate) preedit: Option<Range<usize>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HistoryFrame {
    value: String,
    selection: TextSelection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextInputState {
    value: String,
    selection: TextSelection,
    preedit: Option<ImePreedit>,
    undo: Vec<HistoryFrame>,
    redo: Vec<HistoryFrame>,
}

impl TextInputState {
    pub(crate) fn new(value: String) -> Self {
        let value = truncate_graphemes(value, MAX_TEXT_INPUT_BYTES);
        let end = value.len();
        Self {
            value,
            selection: TextSelection::caret(end),
            preedit: None,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    pub(crate) const fn selection(&self) -> &TextSelection {
        &self.selection
    }

    pub(crate) const fn preedit(&self) -> Option<&ImePreedit> {
        self.preedit.as_ref()
    }

    pub(crate) fn selected_text(&self) -> &str {
        &self.value[self.selection.range()]
    }

    pub(crate) fn set_selection(&mut self, anchor: usize, head: usize) {
        self.selection = TextSelection {
            anchor: nearest_boundary(&self.value, anchor),
            head: nearest_boundary(&self.value, head),
        };
        self.preedit = None;
    }

    pub(crate) fn select_all(&mut self) {
        self.selection = TextSelection {
            anchor: 0,
            head: self.value.len(),
        };
        self.preedit = None;
    }

    pub(crate) fn move_left(&mut self, extend: bool) {
        let head = previous_grapheme(&self.value, self.selection.head);
        if extend {
            self.selection.head = head;
        } else {
            let collapsed = if self.selection.is_caret() {
                head
            } else {
                self.selection.range().start
            };
            self.selection = TextSelection::caret(collapsed);
        }
        self.preedit = None;
    }

    pub(crate) fn move_right(&mut self, extend: bool) {
        let head = next_grapheme(&self.value, self.selection.head);
        if extend {
            self.selection.head = head;
        } else {
            let collapsed = if self.selection.is_caret() {
                head
            } else {
                self.selection.range().end
            };
            self.selection = TextSelection::caret(collapsed);
        }
        self.preedit = None;
    }

    /// Move caret one word left (⌥← / ⌥B).
    pub(crate) fn move_word_left(&mut self, extend: bool) {
        let head = previous_word_boundary(&self.value, self.selection.head);
        if extend {
            self.selection.head = head;
        } else {
            self.selection = TextSelection::caret(head);
        }
        self.preedit = None;
    }

    /// Caret one WORD right (⌥→ / ⌥F), landing at the END of the next word.
    pub(crate) fn move_word_right(&mut self, extend: bool) {
        let head = next_word_boundary(&self.value, self.selection.head);
        if extend {
            self.selection.head = head;
        } else {
            self.selection = TextSelection::caret(head);
        }
        self.preedit = None;
    }

    /// Move caret to the start of the value (Ctrl-A).
    pub(crate) fn move_to_start(&mut self, extend: bool) {
        if extend {
            self.selection.head = 0;
        } else {
            self.selection = TextSelection::caret(0);
        }
        self.preedit = None;
    }

    /// Caret to the end of the value (readline/macOS Ctrl-E).
    pub(crate) fn move_to_end(&mut self, extend: bool) {
        if extend {
            self.selection.head = self.value.len();
        } else {
            self.selection = TextSelection::caret(self.value.len());
        }
        self.preedit = None;
    }

    /// Delete from the caret (or selection start) to the end of the value as one undo frame (Ctrl-K).
    pub(crate) fn kill_to_end(&mut self) {
        let start = self.selection.range().start;
        self.selection = TextSelection {
            anchor: start,
            head: self.value.len(),
        };
        self.replace_selection("");
    }

    /// Delete from the start of the value to the caret (or selection end) as one undo frame (Ctrl-U).
    pub(crate) fn kill_to_start(&mut self) {
        let end = self.selection.range().end;
        self.selection = TextSelection {
            anchor: 0,
            head: end,
        };
        self.replace_selection("");
    }

    /// Delete the whitespace-delimited word before the caret (an active selection is deleted
    /// as-is) as one undo frame (Ctrl-W).
    pub(crate) fn delete_word_backward(&mut self) {
        if self.selection.is_caret() {
            self.selection.anchor = previous_word_boundary(&self.value, self.selection.head);
        }
        self.replace_selection("");
    }

    #[cfg(any(a11y_tree, test))]
    pub(crate) fn insert(&mut self, text: &str) {
        self.replace_selection(text);
    }

    pub(crate) fn delete_backward(&mut self) {
        if self.selection.is_caret() {
            self.selection.anchor = previous_grapheme(&self.value, self.selection.head);
        }
        self.replace_selection("");
    }

    pub(crate) fn delete_forward(&mut self) {
        if self.selection.is_caret() {
            self.selection.head = next_grapheme(&self.value, self.selection.head);
        }
        self.replace_selection("");
    }

    pub(crate) fn set_preedit(&mut self, text: String, selection: Option<Range<usize>>) {
        let replaced = self.selection.range();
        let room = MAX_TEXT_INPUT_BYTES.saturating_sub(self.value.len() - replaced.len());
        let text = truncate_graphemes(text, room);
        let selection = normalize_preedit_selection(&text, selection);
        self.preedit = (!text.is_empty()).then_some(ImePreedit { text, selection });
    }

    pub(crate) fn commit_preedit(&mut self, text: &str) {
        self.preedit = None;
        self.replace_selection(text);
    }

    pub(crate) fn cancel_preedit(&mut self) {
        self.preedit = None;
    }

    pub(crate) fn undo(&mut self) -> bool {
        let Some(frame) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.frame());
        self.restore(frame);
        true
    }

    pub(crate) fn redo(&mut self) -> bool {
        let Some(frame) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.frame());
        self.restore(frame);
        true
    }

    /// Paint projection with preedit inserted at the committed selection.
    pub(crate) fn display_projection(&self) -> (String, Option<Range<usize>>) {
        let projection = self.projection();
        (projection.text, projection.preedit)
    }

    /// Project committed text, marked text, selection, and caret.
    pub(crate) fn projection(&self) -> TextInputProjection {
        let Some(preedit) = &self.preedit else {
            return TextInputProjection {
                text: self.value.clone(),
                selection: self.selection.clone(),
                preedit: None,
            };
        };
        let replaced = self.selection.range();
        let mut text = self.value.clone();
        text.replace_range(replaced.clone(), &preedit.text);
        let marked = replaced.start..replaced.start.saturating_add(preedit.text.len());
        let selection = preedit.selection.as_ref().map_or_else(
            || TextSelection::caret(marked.end),
            |selection| TextSelection {
                anchor: marked.start.saturating_add(selection.start),
                head: marked.start.saturating_add(selection.end),
            },
        );
        TextInputProjection {
            text,
            selection,
            preedit: Some(marked),
        }
    }

    fn replace_selection(&mut self, text: &str) {
        let range = self.selection.range();
        if range.is_empty() && text.is_empty() {
            self.preedit = None;
            return;
        }
        self.push_undo();
        let room = MAX_TEXT_INPUT_BYTES.saturating_sub(self.value.len() - range.len());
        let inserted = truncate_grapheme_slice(text, room);
        self.value.replace_range(range.clone(), inserted);
        self.selection = TextSelection::caret(range.start.saturating_add(inserted.len()));
        self.preedit = None;
    }

    fn push_undo(&mut self) {
        self.undo.push(self.frame());
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn frame(&self) -> HistoryFrame {
        HistoryFrame {
            value: self.value.clone(),
            selection: self.selection.clone(),
        }
    }

    fn restore(&mut self, frame: HistoryFrame) {
        self.value = frame.value;
        self.selection = frame.selection;
        self.preedit = None;
    }
}

/// Clamp valid UTF-8 preedit selection ranges to grapheme boundaries.
pub(crate) fn normalize_preedit_selection(
    text: &str,
    selection: Option<Range<usize>>,
) -> Option<Range<usize>> {
    selection.and_then(|range| {
        (range.start <= range.end
            && range.end <= text.len()
            && text.is_char_boundary(range.start)
            && text.is_char_boundary(range.end))
        .then(|| nearest_boundary(text, range.start)..nearest_boundary(text, range.end))
    })
}

fn truncate_graphemes(mut text: String, maximum: usize) -> String {
    if text.len() <= maximum {
        return text;
    }
    let end = nearest_boundary(&text, maximum);
    text.truncate(end);
    text
}

fn truncate_grapheme_slice(text: &str, maximum: usize) -> &str {
    if text.len() <= maximum {
        text
    } else {
        &text[..nearest_boundary(text, maximum)]
    }
}

fn nearest_boundary(text: &str, requested: usize) -> usize {
    let requested = requested.min(text.len());
    if requested == text.len() {
        return text.len();
    }
    text.grapheme_indices()
        .map(|(offset, _)| offset)
        .take_while(|offset| *offset <= requested)
        .last()
        .unwrap_or(0)
}

fn previous_grapheme(text: &str, position: usize) -> usize {
    let position = nearest_boundary(text, position);
    text[..position]
        .grapheme_indices()
        .map(|(offset, _)| offset)
        .last()
        .unwrap_or(0)
}

fn previous_word_boundary(text: &str, position: usize) -> usize {
    let before = &text[..nearest_boundary(text, position)];
    let word_end = before.trim_end().len();
    let start = before[..word_end]
        .trim_end_matches(|c: char| !c.is_whitespace())
        .len();
    nearest_boundary(text, start)
}

fn next_word_boundary(text: &str, position: usize) -> usize {
    let start = nearest_boundary(text, position);
    let after = &text[start..];
    let ws = after.len() - after.trim_start().len();
    let rest = &after[ws..];
    let word = rest.find(char::is_whitespace).unwrap_or(rest.len());
    nearest_boundary(text, start + ws + word)
}

fn next_grapheme(text: &str, position: usize) -> usize {
    let position = nearest_boundary(text, position);
    if position >= text.len() {
        return text.len();
    }
    position.saturating_add(text[position..].graphemes().next().map_or(0, str::len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_replace_and_local_undo_redo_are_atomic() {
        let mut input = TextInputState::new("hello world".into());
        input.set_selection(6, 11);
        input.insert("aterm");
        assert_eq!(input.value(), "hello aterm");
        assert_eq!(input.selection(), &TextSelection::caret(11));
        assert!(input.undo());
        assert_eq!(input.value(), "hello world");
        assert_eq!(input.selection().range(), 6..11);
        assert!(input.redo());
        assert_eq!(input.value(), "hello aterm");
    }

    #[test]
    fn grapheme_navigation_and_delete_keep_clusters_whole() {
        let text = "e\u{301}👩‍💻z";
        let mut input = TextInputState::new(text.into());
        input.move_left(false);
        input.delete_backward();
        assert_eq!(input.value(), "e\u{301}z");
        input.delete_backward();
        assert_eq!(input.value(), "z");
    }

    #[test]
    fn preedit_is_visible_but_not_committed_until_ime_commit() {
        let mut input = TextInputState::new("ab".into());
        input.set_selection(1, 1);
        input.set_preedit("に".into(), Some(3..3));
        let (display, range) = input.display_projection();
        assert_eq!(display, "aにb");
        assert_eq!(range, Some(1..4));
        assert_eq!(input.value(), "ab");
        input.commit_preedit("日");
        assert_eq!(input.value(), "a日b");
        assert!(input.undo());
        assert_eq!(input.value(), "ab");
    }

    #[test]
    fn invalid_platform_preedit_selection_fails_closed() {
        let mut input = TextInputState::new(String::new());
        input.set_preedit("é".into(), Some(1..99));
        assert_eq!(input.preedit().unwrap().selection, None);
    }

    #[test]
    fn shift_motion_extends_and_plain_motion_collapses() {
        let mut input = TextInputState::new("abc".into());
        input.set_selection(1, 1);
        input.move_right(true);
        assert_eq!(input.selection().range(), 1..2);
        input.move_right(false);
        assert_eq!(input.selection(), &TextSelection::caret(2));
    }

    #[test]
    fn projection_maps_ime_selection_to_grapheme_safe_display_offsets() {
        let mut input = TextInputState::new("aZb".into());
        input.set_selection(1, 2);
        input.set_preedit("e\u{301}日".into(), Some(1..3));
        let projection = input.projection();
        assert_eq!(projection.text, "ae\u{301}日b");
        assert_eq!(projection.preedit, Some(1..7));
        assert_eq!(projection.selection, TextSelection { anchor: 1, head: 4 });
    }

    #[test]
    fn readline_home_end_and_kills_edit_at_the_caret() {
        let mut input = TextInputState::new("alpha beta".into());
        input.move_to_start(false);
        assert_eq!(input.selection(), &TextSelection::caret(0));
        input.move_to_end(false);
        assert_eq!(input.selection(), &TextSelection::caret(10));
        input.move_to_start(true);
        assert_eq!(input.selected_text(), "alpha beta");

        input.set_selection(5, 5);
        input.kill_to_end();
        assert_eq!(input.value(), "alpha");
        assert_eq!(input.selection(), &TextSelection::caret(5));
        assert!(input.undo());
        assert_eq!(input.value(), "alpha beta");

        input.set_selection(6, 6);
        input.kill_to_start();
        assert_eq!(input.value(), "beta");
        assert_eq!(input.selection(), &TextSelection::caret(0));

        let mut idle = TextInputState::new("x".into());
        idle.move_to_end(false);
        idle.kill_to_end();
        assert_eq!(idle.value(), "x");
        assert!(!idle.undo());
    }

    #[test]
    fn ctrl_w_deletes_the_previous_word_and_keeps_clusters_whole() {
        let mut input = TextInputState::new("one two  three ".into());
        input.delete_word_backward();
        assert_eq!(input.value(), "one two  ");
        input.delete_word_backward();
        assert_eq!(input.value(), "one ");
        input.delete_word_backward();
        assert_eq!(input.value(), "");
        input.delete_word_backward();
        assert_eq!(input.value(), "");

        let mut input = TextInputState::new("hello world".into());
        input.set_selection(8, 8);
        input.delete_word_backward();
        assert_eq!(input.value(), "hello rld");

        let mut input = TextInputState::new("alpha beta".into());
        input.set_selection(2, 4);
        input.delete_word_backward();
        assert_eq!(input.value(), "ala beta");

        let mut input = TextInputState::new("cat 👩‍💻e\u{301}".into());
        input.delete_word_backward();
        assert_eq!(input.value(), "cat ");
    }

    #[test]
    fn oversized_insert_is_bounded_without_splitting_the_last_grapheme() {
        let mut input = TextInputState::new(String::new());
        let mut paste = "x".repeat(MAX_TEXT_INPUT_BYTES - 1);
        paste.push_str("e\u{301}");
        input.insert(&paste);
        assert!(input.value().len() <= MAX_TEXT_INPUT_BYTES);
        assert!(input.value().is_char_boundary(input.value().len()));
        assert!(!input.value().ends_with('e'));
    }

    #[test]
    fn alt_word_motion_walks_whole_words_and_round_trips() {
        let mut input = TextInputState::new("hello brave world".into());
        input.set_selection(0, 0);

        input.move_word_right(false);
        assert_eq!(
            input.selection().head,
            5,
            "⌥→ must land at the END of 'hello'"
        );
        input.move_word_right(false);
        assert_eq!(
            input.selection().head,
            11,
            "⌥→ must cross to the end of 'brave'"
        );

        input.move_word_left(false);
        assert_eq!(
            input.selection().head,
            6,
            "⌥← must land at the START of 'brave'"
        );

        input.set_selection(0, 0);
        input.move_word_left(false);
        assert_eq!(input.selection().head, 0, "⌥← at the start is inert");
        let end = input.value().len();
        input.set_selection(end, end);
        input.move_word_right(false);
        assert_eq!(input.selection().head, end, "⌥→ at the end is inert");

        input.set_selection(0, 0);
        input.move_word_right(true);
        assert!(
            !input.selection().is_caret(),
            "shift-⌥→ must select, not just move"
        );
        assert_eq!(input.selection().head, 5);
    }
}
