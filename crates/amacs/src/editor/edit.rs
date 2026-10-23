// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Caret insertion commands: `newline`, `open-line`, `transpose-chars`, and `indent-for-tab-command`.

use super::command::PrefixArgument;
use super::projection::{document_line_break, editor_display_column, line_start, line_text_end};
use super::view::{EditorBufferView, Selection, next_boundary, previous_boundary};
use super::workspace::{
    EditorEffect, EditorError, EditorWorkspace, edits_for_selections, normalize_edits,
    snapshot_for_view,
};
use crate::store::{DocumentStore, TextEdit, rebase_position};

/// Distance in columns from `column` to the next multiple of `tab_width` (with zero treated as 1).
pub(super) fn tab_stop_columns(column: usize, tab_width: usize) -> usize {
    let width = tab_width.max(1);
    width - column % width
}

impl EditorWorkspace {
    /// `newline` (Enter): inserts `count` document line breaks at every caret, replacing selections.
    pub(super) fn newline(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        count: usize,
    ) -> Result<(), EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let line_break = document_line_break(&snapshot.text);
        let edits = edits_for_selections(&view.selections, &line_break.repeat(count))?;
        self.apply_recorded(store, view, edits)?;
        self.last_yank = None;
        Ok(())
    }

    /// `open-line` (C-o): inserts `count` document line breaks after every caret without moving it.
    pub(super) fn open_line(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        count: usize,
    ) -> Result<(), EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let heads = view
            .selections
            .iter()
            .map(|selection| selection.head)
            .collect::<Vec<_>>();
        let insert = document_line_break(&snapshot.text).repeat(count);
        let edits = heads
            .iter()
            .map(|head| TextEdit {
                range: *head..*head,
                insert: insert.clone(),
            })
            .collect();
        let (_, deltas) = self.apply_recorded(store, view, normalize_edits(edits)?)?;
        for (selection, head) in view.selections.iter_mut().zip(heads) {
            selection.collapse(rebase_position(head, &deltas).saturating_sub(insert.len()));
        }
        self.last_yank = None;
        Ok(())
    }

    /// `transpose-chars` (C-t): moves the grapheme cluster before each caret past `count` after it (or swaps the two before line end when `argument` is `None`).
    pub(super) fn transpose_chars(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        argument: Option<PrefixArgument>,
        count: usize,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let text = &snapshot.text;
        let mut edits = Vec::with_capacity(view.selections.len());
        let mut carets = Vec::with_capacity(view.selections.len());
        for selection in &view.selections {
            let mut point = selection.head.min(text.len());
            if argument.is_none() && point > 0 && point == line_text_end(text, point) {
                point = previous_boundary(text, point);
            }
            if point == 0 {
                return Ok(vec![EditorEffect::Status("Beginning of buffer".into())]);
            }
            let before = previous_boundary(text, point);
            let mut after = point;
            for _ in 0..count {
                let next = next_boundary(text, after);
                if next == after {
                    break;
                }
                after = next;
            }
            if after == point {
                return Ok(vec![EditorEffect::Status("End of buffer".into())]);
            }
            let moved = format!("{}{}", &text[point..after], &text[before..point]);
            edits.push(TextEdit {
                range: before..after,
                insert: moved,
            });
            carets.push(after);
        }
        self.apply_recorded(store, view, normalize_edits(edits)?)?;
        for (selection, caret) in view.selections.iter_mut().zip(carets) {
            selection.collapse(caret);
        }
        self.last_yank = None;
        Ok(Vec::new())
    }

    /// `indent-for-tab-command` (TAB): indents to the next tab stop at every caret.
    pub(super) fn indent_for_tab(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<(), EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let heads = view
            .selections
            .iter()
            .map(|selection| selection.head)
            .collect::<Vec<_>>();
        let mut order = heads.clone();
        order.sort_unstable();
        order.dedup();
        let mut edits = Vec::with_capacity(order.len());
        let mut line = None;
        let mut indented = String::new();
        let mut copied = 0;
        for head in order {
            let start = line_start(&snapshot.text, head);
            if line != Some(start) {
                line = Some(start);
                indented.clear();
                copied = start;
            }
            indented.push_str(&snapshot.text[copied..head]);
            copied = head;
            let insert = if self.options.indent_tabs {
                "\t".to_owned()
            } else {
                let column = editor_display_column(&indented, indented.len(), 0);
                " ".repeat(tab_stop_columns(column, self.options.tab_width))
            };
            indented.push_str(&insert);
            edits.push(TextEdit {
                range: head..head,
                insert,
            });
        }
        let (_, deltas) = self.apply_recorded(store, view, normalize_edits(edits)?)?;
        for (selection, head) in view.selections.iter_mut().zip(heads) {
            *selection = Selection::caret(rebase_position(head, &deltas));
        }
        view.desired_column = None;
        self.last_yank = None;
        Ok(())
    }
}
