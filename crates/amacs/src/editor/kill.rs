// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The kill ring: kill-region and kill-line push to it once their deletion
//! commits, and yank and yank-pop insert from it.

use std::ops::Range;

use aterm_buffer::Seq;

use super::command::PrefixArgument;
use super::projection::line_end;
use super::view::{EditorBufferView, next_boundary};
use super::workspace::{
    EditorEffect, EditorError, EditorWorkspace, edits_for_selections, normalize_edits,
    snapshot_for_view,
};
use crate::store::{DocumentId, DocumentStore, TextEdit};

const KILL_RING_LIMIT: usize = 60;

#[derive(Clone, Debug)]
pub(super) struct YankRecord {
    document: DocumentId,
    seq: Seq,
    ranges: Vec<Range<usize>>,
    ring_index: usize,
}

impl YankRecord {
    /// The document the yank inserted into.
    pub(super) fn document(&self) -> DocumentId {
        self.document
    }
}

/// M-y replaces the text the previous C-y inserted, so it needs one first.
pub(super) const YANK_POP_NEEDS_YANK: &str = "Yank first (C-y), then M-y";
const YANK_POP_STALE: &str = "The buffer changed since C-y; yank again, then M-y";

impl EditorWorkspace {
    /// `kill-region` (C-w): delete every non-empty selection in one
    /// transaction, and push the killed text, joined by newlines.
    pub(super) fn kill_region(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let mut killed = String::new();
        let mut edits = Vec::new();
        for selection in &view.selections {
            let range = selection.range();
            if range.is_empty() {
                continue;
            }
            if !killed.is_empty() {
                killed.push('\n');
            }
            killed.push_str(&snapshot.text[range.clone()]);
            edits.push(TextEdit {
                range,
                insert: String::new(),
            });
        }
        if edits.is_empty() {
            return Ok(vec![EditorEffect::Status("No region selected".to_string())]);
        }
        // The kill ring takes the text only once the deletion has
        // committed, so a refused edit (overlapping ranges) kills nothing.
        self.apply_recorded(store, view, normalize_edits(edits)?)?;
        self.push_kill(killed);
        self.last_yank = None;
        Ok(Vec::new())
    }

    /// `kill-line` (C-k): kill to the end of the line, or with an argument
    /// that many whole lines.
    pub(super) fn kill_line(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        argument: Option<PrefixArgument>,
        count: usize,
    ) -> Result<(), EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let mut killed = String::new();
        let mut edits = Vec::new();
        for selection in &view.selections {
            let start = selection.range().start;
            // With an argument, C-k kills that many whole lines,
            // newlines included (C-u 2 C-k); without one, to the end
            // of the line, or the newline itself at the end.
            let end = if argument.is_some() {
                line_kill_end(&snapshot.text, selection.range().end, count)
            } else {
                let end = line_end(&snapshot.text, selection.range().end);
                if end == start && end < snapshot.text.len() {
                    next_boundary(&snapshot.text, end)
                } else {
                    end
                }
            };
            if !killed.is_empty() {
                killed.push('\n');
            }
            killed.push_str(&snapshot.text[start..end]);
            edits.push(TextEdit {
                range: start..end,
                insert: String::new(),
            });
        }
        // As for kill-region: no kill without the deletion. With a
        // count, carets on neighbouring lines overlap and are refused.
        self.apply_recorded(store, view, normalize_edits(edits)?)?;
        self.push_kill(killed);
        self.last_yank = None;
        Ok(())
    }

    /// `yank` (C-y): insert a kill at every selection.
    pub(super) fn yank(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        argument: Option<PrefixArgument>,
        count: usize,
    ) -> Result<(), EditorError> {
        // As in Emacs: C-u 2 C-y yanks the second most recent kill (and
        // M-y goes on from there), while a bare C-u yanks the latest
        // and leaves point before it.
        let ring_index = match argument {
            Some(PrefixArgument::Numeric { .. }) => (count - 1) % self.kill_ring.len().max(1),
            Some(PrefixArgument::Raw(_)) | None => 0,
        };
        let text = self
            .kill_ring
            .get(ring_index)
            .cloned()
            .ok_or(EditorError::NoKill)?;
        let edits = edits_for_selections(&view.selections, &text)?;
        let ranges = resulting_insert_ranges(&edits);
        let (seq, _) = self.apply_recorded(store, view, edits)?;
        if matches!(argument, Some(PrefixArgument::Raw(_))) {
            for selection in &mut view.selections {
                if let Some(range) = ranges.iter().find(|range| range.end == selection.head) {
                    selection.collapse(range.start);
                }
            }
        }
        self.last_yank = Some(YankRecord {
            document: view.document,
            seq,
            ranges,
            ring_index,
        });
        Ok(())
    }

    /// `yank-pop` (M-y): replace what the last yank inserted with the next
    /// kill in the ring.
    pub(super) fn yank_pop(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let Some(previous) = self.last_yank.clone() else {
            return Ok(vec![EditorEffect::Status(YANK_POP_NEEDS_YANK.to_string())]);
        };
        let snapshot = snapshot_for_view(store, view)?;
        if previous.document != view.document || previous.seq != snapshot.seq {
            self.last_yank = None;
            return Ok(vec![EditorEffect::Status(YANK_POP_STALE.to_string())]);
        }
        let next_index = (previous.ring_index + 1) % self.kill_ring.len().max(1);
        let replacement = self
            .kill_ring
            .get(next_index)
            .cloned()
            .ok_or(EditorError::NoKill)?;
        let edits = previous
            .ranges
            .iter()
            .cloned()
            .map(|range| TextEdit {
                range,
                insert: replacement.clone(),
            })
            .collect::<Vec<_>>();
        let ranges = resulting_insert_ranges(&edits);
        let (seq, _) = self.apply_recorded(store, view, edits)?;
        self.last_yank = Some(YankRecord {
            document: view.document,
            seq,
            ranges,
            ring_index: next_index,
        });
        Ok(Vec::new())
    }

    fn push_kill(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.kill_ring.push_front(text);
        self.kill_ring.truncate(KILL_RING_LIMIT);
    }
}

fn resulting_insert_ranges(edits: &[TextEdit]) -> Vec<Range<usize>> {
    let mut growth = 0isize;
    edits
        .iter()
        .map(|edit| {
            let start = edit.range.start.saturating_add_signed(growth);
            let removed = edit.range.end.saturating_sub(edit.range.start);
            growth = growth.saturating_add(edit.insert.len() as isize - removed as isize);
            start..start.saturating_add(edit.insert.len())
        })
        .collect()
}

/// Where `kill-line` with an argument of `lines` stops: the start of the
/// `lines`-th line after `position`'s, newlines included, or the end of the text.
fn line_kill_end(text: &str, position: usize, lines: usize) -> usize {
    let mut end = position.min(text.len());
    for _ in 0..lines {
        let Some(relative) = text[end..].find('\n') else {
            return text.len();
        };
        end = end.saturating_add(relative).saturating_add(1);
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{EditorCommand, Selection, editor};
    use std::collections::VecDeque;

    /// A kill the document refuses takes nothing into the kill ring: with
    /// carets on neighbouring lines, C-u 2 C-k asks for overlapping ranges.
    #[test]
    fn a_refused_kill_leaves_the_kill_ring_alone() {
        let text = "aa\nbb\ncc\ndd\n";
        let (mut store, mut workspace, mut view, document) = editor(text);
        view.selections = vec![Selection::caret(0), Selection::caret(3)];
        workspace
            .execute(&mut store, &mut view, EditorCommand::UniversalArgument)
            .unwrap();
        workspace.insert_text(&mut store, &mut view, "2").unwrap();
        assert_eq!(
            workspace.execute(&mut store, &mut view, EditorCommand::KillLine),
            Err(EditorError::InvalidSelections)
        );
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), text);
        assert!(workspace.kill_ring.is_empty());
        assert_eq!(view.prefix_hud, None);
    }

    /// C-y takes the argument as Emacs does: C-u 2 C-y yanks the second most
    /// recent kill and M-y goes on from there; a bare C-u yanks the latest and
    /// leaves point before it.
    #[test]
    fn universal_argument_picks_the_kill_to_yank() {
        let (mut store, mut workspace, mut view, document) = editor("ab");
        workspace.kill_ring = VecDeque::from(["newest".to_string(), "older".to_string()]);
        view.selections = vec![Selection::caret(1)];
        workspace
            .execute(&mut store, &mut view, EditorCommand::UniversalArgument)
            .unwrap();
        workspace.insert_text(&mut store, &mut view, "2").unwrap();
        workspace
            .execute(&mut store, &mut view, EditorCommand::Yank)
            .unwrap();
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "aolderb");
        workspace
            .execute(&mut store, &mut view, EditorCommand::YankPop)
            .unwrap();
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "anewestb");

        let (mut store, mut workspace, mut view, document) = editor("ab");
        workspace.kill_ring = VecDeque::from(["newest".to_string(), "older".to_string()]);
        view.selections = vec![Selection::caret(1)];
        workspace
            .execute(&mut store, &mut view, EditorCommand::UniversalArgument)
            .unwrap();
        workspace
            .execute(&mut store, &mut view, EditorCommand::Yank)
            .unwrap();
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "anewestb");
        assert_eq!(view.primary_selection(), &Selection::caret(1));
    }

    #[test]
    fn kill_yank_pop_and_macro_are_workspace_state() {
        let (mut store, mut workspace, mut view, document) = editor("first\nsecond");
        view.selections = vec![Selection { anchor: 0, head: 5 }];
        workspace
            .execute(&mut store, &mut view, EditorCommand::KillRegion)
            .unwrap();
        workspace.kill_ring.push_back("alternate".into());
        workspace
            .execute(&mut store, &mut view, EditorCommand::Yank)
            .unwrap();
        workspace
            .execute(&mut store, &mut view, EditorCommand::YankPop)
            .unwrap();
        assert_eq!(
            store.snapshot(document).unwrap().text.as_ref(),
            "alternate\nsecond"
        );

        workspace
            .execute(&mut store, &mut view, EditorCommand::StartMacro)
            .unwrap();
        workspace.insert_text(&mut store, &mut view, "!").unwrap();
        workspace
            .execute(&mut store, &mut view, EditorCommand::EndMacro)
            .unwrap();
        workspace
            .execute(&mut store, &mut view, EditorCommand::PlayMacro)
            .unwrap();
        assert!(
            store
                .snapshot(document)
                .unwrap()
                .text
                .ends_with("!!\nsecond")
        );
    }
}
