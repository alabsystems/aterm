// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Undo and redo: per-document linear stacks of atomic frames.

use aterm_buffer::Seq;

use super::view::EditorBufferView;
use super::workspace::{EditorError, EditorWorkspace, snapshot_for_view};
use crate::store::{DocumentId, DocumentStore, DocumentTxnOutcome, EditDelta, TextEdit};

#[derive(Clone, Debug)]
struct HistoryFrame {
    expected_seq: Seq,
    undo: Vec<TextEdit>,
    redo: Vec<TextEdit>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct DocumentHistory {
    undo: Vec<HistoryFrame>,
    redo: Vec<HistoryFrame>,
}

impl EditorWorkspace {
    pub fn can_undo(&self, document: DocumentId) -> bool {
        self.histories
            .get(&document)
            .is_some_and(|history| !history.undo.is_empty())
    }

    pub fn can_redo(&self, document: DocumentId) -> bool {
        self.histories
            .get(&document)
            .is_some_and(|history| !history.redo.is_empty())
    }

    /// Take every unreported commit of `document` in application order. Each delta list uses its own transaction's coordinates; rebase through the lists separately.
    pub fn take_commits(&mut self, document: DocumentId) -> Vec<(Seq, Vec<EditDelta>)> {
        let mut commits = Vec::new();
        self.pending_commits.retain_mut(|(committed, seq, deltas)| {
            if *committed == document {
                commits.push((*seq, std::mem::take(deltas)));
                false
            } else {
                true
            }
        });
        commits
    }

    pub(super) fn apply_recorded(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        edits: Vec<TextEdit>,
    ) -> Result<(Seq, Vec<EditDelta>), EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let inverse = inverse_edits(&snapshot.text, &edits)?;
        let redo = edits.clone();
        let (seq, deltas) = transact(store, view.document, view.anchor_seq, edits)?;
        view.observe_own(seq, &deltas);
        self.pending_commits
            .push((view.document, seq, deltas.clone()));
        self.rebase_marks(view.document, &deltas);
        let history = self.histories.entry(view.document).or_default();
        history.undo.push(HistoryFrame {
            expected_seq: seq,
            undo: inverse,
            redo,
        });
        history.redo.clear();
        Ok((seq, deltas))
    }

    pub(super) fn undo(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<(), EditorError> {
        let frame = self
            .histories
            .entry(view.document)
            .or_default()
            .undo
            .pop()
            .ok_or(EditorError::NothingToUndo)?;
        let current = snapshot_for_view(store, view)?.seq;
        if current != frame.expected_seq {
            self.histories
                .entry(view.document)
                .or_default()
                .undo
                .push(frame.clone());
            return Err(EditorError::HistoryDiverged {
                expected: frame.expected_seq,
                current,
            });
        }
        let (seq, deltas) = transact(store, view.document, current, frame.undo.clone())?;
        view.observe_own(seq, &deltas);
        self.pending_commits
            .push((view.document, seq, deltas.clone()));
        self.rebase_marks(view.document, &deltas);
        let history = self.histories.entry(view.document).or_default();
        if let Some(next) = history.undo.last_mut() {
            next.expected_seq = seq;
        }
        history.redo.push(HistoryFrame {
            expected_seq: seq,
            ..frame
        });
        self.last_yank = None;
        Ok(())
    }

    pub(super) fn redo(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<(), EditorError> {
        let frame = self
            .histories
            .entry(view.document)
            .or_default()
            .redo
            .pop()
            .ok_or(EditorError::NothingToRedo)?;
        let current = snapshot_for_view(store, view)?.seq;
        if current != frame.expected_seq {
            self.histories
                .entry(view.document)
                .or_default()
                .redo
                .push(frame.clone());
            return Err(EditorError::HistoryDiverged {
                expected: frame.expected_seq,
                current,
            });
        }
        let (seq, deltas) = transact(store, view.document, current, frame.redo.clone())?;
        view.observe_own(seq, &deltas);
        self.pending_commits
            .push((view.document, seq, deltas.clone()));
        self.rebase_marks(view.document, &deltas);
        let history = self.histories.entry(view.document).or_default();
        if let Some(next) = history.redo.last_mut() {
            next.expected_seq = seq;
        }
        history.undo.push(HistoryFrame {
            expected_seq: seq,
            ..frame
        });
        self.last_yank = None;
        Ok(())
    }
}

fn transact(
    store: &mut DocumentStore,
    document: DocumentId,
    base: Seq,
    edits: Vec<TextEdit>,
) -> Result<(Seq, Vec<EditDelta>), EditorError> {
    match store.transact(document, base, edits) {
        DocumentTxnOutcome::Committed { seq, deltas, .. } => Ok((seq, deltas)),
        DocumentTxnOutcome::Conflict { current } => {
            Err(EditorError::TransactionConflict { current })
        }
        DocumentTxnOutcome::Rejected(_) => Err(EditorError::TransactionRejected),
    }
}

fn inverse_edits(source: &str, edits: &[TextEdit]) -> Result<Vec<TextEdit>, EditorError> {
    let mut growth = 0isize;
    let mut inverse = Vec::with_capacity(edits.len());
    for edit in edits {
        if edit.range.end > source.len()
            || !source.is_char_boundary(edit.range.start)
            || !source.is_char_boundary(edit.range.end)
        {
            return Err(EditorError::InvalidSelections);
        }
        let start = edit.range.start.saturating_add_signed(growth);
        inverse.push(TextEdit {
            range: start..start.saturating_add(edit.insert.len()),
            insert: source[edit.range.clone()].to_owned(),
        });
        let removed = edit.range.end.saturating_sub(edit.range.start);
        growth =
            growth.saturating_add((edit.insert.len() as isize).saturating_sub(removed as isize));
    }
    Ok(inverse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{EditorCommand, Selection, editor};

    #[test]
    fn undo_redo_round_trips_atomic_frame() {
        let (mut store, mut workspace, mut view, document) = editor("abc");
        view.selections = vec![Selection { anchor: 1, head: 2 }];
        workspace.insert_text(&mut store, &mut view, "XYZ").unwrap();
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "aXYZc");
        workspace
            .execute(&mut store, &mut view, EditorCommand::Undo)
            .unwrap();
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "abc");
        workspace
            .execute(&mut store, &mut view, EditorCommand::Redo)
            .unwrap();
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "aXYZc");
    }

    #[test]
    fn undo_and_redo_step_through_more_than_one_edit() {
        let (mut store, mut workspace, mut view, document) = editor("abc");
        workspace.insert_text(&mut store, &mut view, "x").unwrap();
        workspace.insert_text(&mut store, &mut view, "y").unwrap();
        let text = |store: &DocumentStore| store.snapshot(document).unwrap().text.to_string();
        assert_eq!(text(&store), "xyabc");
        for expected in ["xabc", "abc"] {
            workspace
                .execute(&mut store, &mut view, EditorCommand::Undo)
                .unwrap();
            assert_eq!(text(&store), expected);
        }
        for expected in ["xabc", "xyabc"] {
            workspace
                .execute(&mut store, &mut view, EditorCommand::Redo)
                .unwrap();
            assert_eq!(text(&store), expected);
        }
    }

    #[test]
    fn undo_redo_availability_tracks_the_real_document_stacks() {
        let (mut store, mut workspace, mut view, document) = editor("abc");
        assert!(!workspace.can_undo(document));
        assert!(!workspace.can_redo(document));

        workspace.insert_text(&mut store, &mut view, "x").unwrap();
        assert!(workspace.can_undo(document));
        assert!(!workspace.can_redo(document));

        workspace
            .execute(&mut store, &mut view, EditorCommand::Undo)
            .unwrap();
        assert!(!workspace.can_undo(document));
        assert!(workspace.can_redo(document));

        workspace
            .execute(&mut store, &mut view, EditorCommand::Redo)
            .unwrap();
        assert!(workspace.can_undo(document));
        assert!(!workspace.can_redo(document));
    }

    #[test]
    fn commit_publication_keeps_document_and_undo_redo_transaction_boundaries() {
        let (mut store, mut workspace, mut view, document) = editor("abc");
        let other_document = store.open("mem://other".to_string(), "other".to_string());
        let mut other = workspace
            .attach(&mut store, other_document, crate::store::DocumentViewId(92))
            .unwrap();
        workspace.insert_text(&mut store, &mut view, "x").unwrap();
        let inserted = store.snapshot(document).unwrap().seq;
        workspace.insert_text(&mut store, &mut other, "y").unwrap();
        let other_seq = store.snapshot(other_document).unwrap().seq;
        workspace
            .execute(&mut store, &mut view, EditorCommand::Undo)
            .unwrap();
        let undone = store.snapshot(document).unwrap().seq;
        workspace
            .execute(&mut store, &mut view, EditorCommand::Redo)
            .unwrap();
        let redone = store.snapshot(document).unwrap().seq;

        assert_eq!(
            workspace.take_commits(document),
            vec![
                (
                    inserted,
                    vec![EditDelta {
                        old: 0..0,
                        inserted_len: 1
                    }]
                ),
                (
                    undone,
                    vec![EditDelta {
                        old: 0..1,
                        inserted_len: 0
                    }]
                ),
                (
                    redone,
                    vec![EditDelta {
                        old: 0..0,
                        inserted_len: 1
                    }]
                ),
            ]
        );
        assert!(workspace.take_commits(document).is_empty());
        assert_eq!(
            workspace.take_commits(other_document),
            vec![(
                other_seq,
                vec![EditDelta {
                    old: 0..0,
                    inserted_len: 1
                }]
            )]
        );
        assert!(workspace.take_commits(other_document).is_empty());
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "xabc");
        assert_eq!(
            store.snapshot(other_document).unwrap().text.as_ref(),
            "yother"
        );
    }
}
