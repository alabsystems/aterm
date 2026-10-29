// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Mark and global mark-ring commands for editor buffers.

use super::view::{EditorBufferView, Selection, clamp_to_grapheme_boundary};
use super::workspace::{EditorEffect, EditorError, EditorWorkspace, GlobalMark, snapshot_for_view};
use crate::store::{DocumentId, DocumentStore, EditDelta, rebase_position};

const MARK_RING_LIMIT: usize = 64;

const NO_MARK: &str = "No mark set in this buffer";

impl EditorWorkspace {
    fn push_mark(&mut self, document: DocumentId, offset: usize) {
        self.global_mark_ring
            .push_front(GlobalMark { document, offset });
        self.global_mark_ring.truncate(MARK_RING_LIMIT);
    }

    /// Carry `document`'s marks through a commit the workspace's one writer
    /// made (a command's, an agent's, a reload's), so a mark stays on the
    /// text it was set at.
    pub(super) fn rebase_marks(&mut self, document: DocumentId, deltas: &[EditDelta]) {
        for mark in &mut self.global_mark_ring {
            if mark.document == document {
                mark.offset = rebase_position(mark.offset, deltas);
            }
        }
    }

    fn last_mark(&self, document: DocumentId, text: &str) -> Option<usize> {
        self.global_mark_ring
            .iter()
            .find(|mark| mark.document == document)
            .map(|mark| clamp_to_grapheme_boundary(text, mark.offset.min(text.len())))
    }

    /// `set-mark-command` (C-SPC): the primary caret's place is the mark,
    /// the region is active from it, and the ring remembers it.
    pub(super) fn set_mark(&mut self, view: &mut EditorBufferView) -> Vec<EditorEffect> {
        if view.selections.is_empty() {
            view.selections.push(Selection::caret(0));
        }
        view.primary = view.primary.min(view.selections.len().saturating_sub(1));
        let mark = view.primary_selection().head;
        if let Some(selection) = view.selections.get_mut(view.primary) {
            selection.anchor = mark;
        }
        view.mark_active = true;
        self.push_mark(view.document, mark);
        vec![EditorEffect::Status("Mark set".into())]
    }

    /// `pop-to-mark-command` (C-u C-SPC): the caret goes back to the most
    /// recent mark set in this buffer, and that mark moves to the back of
    /// the ring, so repeating it visits the earlier marks in turn.
    pub(super) fn pop_mark(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let Some(mark) = self
            .global_mark_ring
            .iter()
            .position(|mark| mark.document == view.document)
            .and_then(|index| self.global_mark_ring.remove(index))
        else {
            return Ok(vec![EditorEffect::Status(NO_MARK.to_string())]);
        };
        self.global_mark_ring.push_back(mark);
        let target =
            clamp_to_grapheme_boundary(&snapshot.text, mark.offset.min(snapshot.text.len()));
        view.selections = vec![Selection::caret(target)];
        view.primary = 0;
        view.mark_active = false;
        view.desired_column = None;
        self.last_yank = None;
        Ok(Vec::new())
    }

    /// `exchange-point-and-mark` (C-x C-x): the primary caret and its mark
    /// trade places, and the region between them is active. Without a
    /// region the mark is the last one set in this buffer.
    pub(super) fn exchange_point_and_mark(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let primary = view.primary_selection().clone();
        let mark = if view.mark_active || primary.anchor != primary.head {
            Some(primary.anchor)
        } else {
            self.last_mark(view.document, &snapshot.text)
        };
        let Some(mark) = mark else {
            return Ok(vec![EditorEffect::Status(NO_MARK.to_string())]);
        };
        view.primary = view.primary.min(view.selections.len().saturating_sub(1));
        if let Some(selection) = view.selections.get_mut(view.primary) {
            *selection = Selection {
                anchor: primary.head,
                head: mark,
            };
        }
        view.mark_active = true;
        view.desired_column = None;
        Ok(Vec::new())
    }

    /// `mark-whole-buffer` (C-x h): the region is the whole buffer, with the
    /// caret at its start and the mark at its end. The ring keeps where the
    /// caret was.
    pub(super) fn mark_whole_buffer(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        self.push_mark(view.document, view.primary_selection().head);
        view.selections = vec![Selection {
            anchor: snapshot.text.len(),
            head: 0,
        }];
        view.primary = 0;
        view.mark_active = true;
        view.desired_column = None;
        Ok(vec![EditorEffect::Status("Mark set".into())])
    }

    /// `beginning-of-buffer` (M-<) and `end-of-buffer` (M->): the caret to
    /// one end of the buffer. An active region stretches there; otherwise the
    /// mark stays behind where the caret was, so C-u C-SPC goes back.
    pub(super) fn buffer_end(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
        end: bool,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let target = if end { snapshot.text.len() } else { 0 };
        let primary = view.primary_selection().clone();
        view.primary = 0;
        view.desired_column = None;
        self.last_yank = None;
        if view.mark_active {
            view.selections = vec![Selection {
                anchor: primary.anchor,
                head: target,
            }];
            return Ok(Vec::new());
        }
        self.push_mark(view.document, primary.head);
        view.selections = vec![Selection::caret(target)];
        Ok(vec![EditorEffect::Status("Mark set".into())])
    }
}
