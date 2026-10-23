// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The kill ring: the kills (kill-region, kill-line, kill-word and
//! backward-kill-word) push to it once their deletion commits, kill-ring-save
//! copies to it, and yank and yank-pop insert from it.

use std::ops::Range;

use aterm_buffer::Seq;

use super::command::{EditorCommand, PrefixArgument};
use super::projection::line_text_end;
use super::view::{
    EditorBufferView, Selection, next_boundary, next_word_boundary, previous_word_boundary,
};
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
    pub(super) const fn document(&self) -> DocumentId {
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
        previous: Option<EditorCommand>,
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
        self.apply_recorded(store, view, normalize_edits(edits)?)?;
        self.record_kill(killed, previous, false);
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
        previous: Option<EditorCommand>,
    ) -> Result<(), EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let mut killed = String::new();
        let mut edits = Vec::new();
        for selection in &view.selections {
            let start = selection.range().start;
            let end = if argument.is_some() {
                line_kill_end(&snapshot.text, selection.range().end, count)
            } else {
                let end = line_text_end(&snapshot.text, selection.range().end);
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
        self.apply_recorded(store, view, normalize_edits(edits)?)?;
        self.record_kill(killed, previous, false);
        self.last_yank = None;
        Ok(())
    }

    /// `kill-word` (M-d) and, `backward`, `backward-kill-word` (M-DEL): kill
    /// from each caret to the end (the start) of the `count`-th word, the
    /// words M-f and M-b move by.
    pub(super) fn kill_word(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        count: usize,
        previous: Option<EditorCommand>,
        backward: bool,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let mut killed = String::new();
        let mut edits = Vec::new();
        for selection in &view.selections {
            let head = selection.head;
            let range = if backward {
                previous_word_boundary(&snapshot.text, head, count)..head
            } else {
                head..next_word_boundary(&snapshot.text, head, count)
            };
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
            return Ok(vec![EditorEffect::Status(
                if backward {
                    "Beginning of buffer"
                } else {
                    "End of buffer"
                }
                .to_string(),
            )]);
        }
        self.apply_recorded(store, view, normalize_edits(edits)?)?;
        self.record_kill(killed, previous, backward);
        self.last_yank = None;
        Ok(Vec::new())
    }

    /// `kill-ring-save` (M-w): copy every non-empty selection to the kill
    /// ring, joined by newlines, leaving the text as it is.
    pub(super) fn kill_ring_save(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
        previous: Option<EditorCommand>,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let copied = view
            .selections
            .iter()
            .map(Selection::range)
            .filter(|range| !range.is_empty())
            .map(|range| &snapshot.text[range])
            .collect::<Vec<_>>();
        if copied.is_empty() {
            return Ok(vec![EditorEffect::Status("No region selected".to_string())]);
        }
        self.record_kill(copied.join("\n"), previous, false);
        for selection in &mut view.selections {
            selection.collapse(selection.head);
        }
        view.mark_active = false;
        self.last_yank = None;
        Ok(Vec::new())
    }

    /// `yank` (C-y): insert a kill at every selection.
    pub(super) fn yank(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        argument: Option<PrefixArgument>,
        count: usize,
    ) -> Result<(), EditorError> {
        let ring_index = match argument {
            Some(PrefixArgument::Numeric { .. }) => {
                count.saturating_sub(1) % self.kill_ring.len().max(1)
            }
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
        let next_index = previous.ring_index.saturating_add(1) % self.kill_ring.len().max(1);
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

    /// Take `text` into the kill ring: joined to the newest entry when the
    /// command before was a kill (`backward` puts it in front, as a
    /// backward kill reads), else as a new entry, the oldest falling off
    /// past [`KILL_RING_LIMIT`].
    fn record_kill(&mut self, text: String, previous: Option<EditorCommand>, backward: bool) {
        if text.is_empty() {
            return;
        }
        self.kill_ring_changed = true;
        if appends_kill(previous)
            && let Some(newest) = self.kill_ring.front_mut()
        {
            // A backward kill goes in front: the new text with the entry
            // appended, which no index can make panic (`insert_str` can).
            if backward {
                let mut joined = text;
                joined.push_str(newest);
                *newest = joined;
            } else {
                newest.push_str(&text);
            }
            return;
        }
        self.kill_ring.push_front(text);
        self.kill_ring.truncate(KILL_RING_LIMIT);
    }
}

/// Whether a kill after `previous` joins the newest kill-ring entry: it does
/// straight after another kill, and after nothing else (a copy, a motion,
/// typing, C-g).
const fn appends_kill(previous: Option<EditorCommand>) -> bool {
    matches!(
        previous,
        Some(
            EditorCommand::KillRegion
                | EditorCommand::KillLine
                | EditorCommand::KillWord
                | EditorCommand::BackwardKillWord
        )
    )
}

pub(super) fn resulting_insert_ranges(edits: &[TextEdit]) -> Vec<Range<usize>> {
    let mut growth = 0isize;
    edits
        .iter()
        .map(|edit| {
            let start = edit.range.start.saturating_add_signed(growth);
            let removed = edit.range.end.saturating_sub(edit.range.start);
            growth = growth
                .saturating_add((edit.insert.len() as isize).saturating_sub(removed as isize));
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
    use crate::editor::{EditorCommand, EditorWorkspace, KeyChord, Selection, editor};
    use crate::store::DocumentViewId;
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

    fn press(
        store: &mut DocumentStore,
        workspace: &mut EditorWorkspace,
        view: &mut EditorBufferView,
        keys: &str,
    ) {
        for key in keys.split_whitespace() {
            workspace
                .handle_chord(
                    store,
                    view,
                    KeyChord::parse(key).expect("test chord parses"),
                )
                .unwrap();
        }
    }

    /// A kill straight after a kill joins its kill-ring entry, as Emacs's
    /// kill-append does: `C-k C-k` yanks back the line and its newline as one,
    /// a backward kill goes in front, and `C-u` between kills keeps the run.
    #[test]
    fn kill_append_joins_consecutive_kills() {
        let (mut store, mut workspace, mut view, document) = editor("one\ntwo\nthree\n");
        press(&mut store, &mut workspace, &mut view, "C-k C-k C-u 1 C-k");
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "three\n");
        assert_eq!(workspace.kill_ring, ["one\ntwo\n"]);
        press(&mut store, &mut workspace, &mut view, "C-y");
        assert_eq!(
            store.snapshot(document).unwrap().text.as_ref(),
            "one\ntwo\nthree\n"
        );

        let (mut store, mut workspace, mut view, _) = editor("alpha beta gamma");
        view.selections = vec![Selection::caret(16)];
        press(
            &mut store,
            &mut workspace,
            &mut view,
            "M-backspace M-backspace",
        );
        assert_eq!(workspace.kill_ring, ["beta gamma"]);
        press(&mut store, &mut workspace, &mut view, "C-a M-d");
        assert_eq!(
            workspace.kill_ring,
            ["alpha", "beta gamma"],
            "a motion ends the run"
        );

        for between in ["C-g", "C-f C-b"] {
            let (mut store, mut workspace, mut view, _) = editor("ab\ncd\n");
            press(&mut store, &mut workspace, &mut view, "C-k");
            press(&mut store, &mut workspace, &mut view, between);
            press(&mut store, &mut workspace, &mut view, "C-k");
            assert_eq!(workspace.kill_ring.len(), 2, "{between}");
        }
        let (mut store, mut workspace, mut view, _) = editor("ab\ncd\n");
        press(&mut store, &mut workspace, &mut view, "C-k");
        workspace.insert_text(&mut store, &mut view, "x").unwrap();
        press(&mut store, &mut workspace, &mut view, "C-k");
        assert_eq!(workspace.kill_ring, ["\n", "ab"], "typing ends the run");

        let (mut store, mut workspace, mut view, _) = editor("one two three");
        view.selections = vec![Selection { anchor: 0, head: 3 }];
        press(&mut store, &mut workspace, &mut view, "M-w C-k");
        assert_eq!(workspace.kill_ring, [" two three", "one"]);

        let (mut store, mut workspace, mut view, _) = editor("one\ntwo\n");
        press(&mut store, &mut workspace, &mut view, "C-k");
        workspace.select_whole_document(&store, &mut view).unwrap();
        press(&mut store, &mut workspace, &mut view, "C-w");
        assert_eq!(workspace.kill_ring, ["\ntwo\n", "one"]);

        let mut store = DocumentStore::for_test();
        let mut workspace = EditorWorkspace::new();
        let one = store.open("mem://one".into(), "one\n".into());
        let two = store.open("mem://two".into(), "two\n".into());
        let mut first = workspace
            .attach(&mut store, one, DocumentViewId(1))
            .unwrap();
        let mut second = workspace
            .attach(&mut store, two, DocumentViewId(2))
            .unwrap();
        press(&mut store, &mut workspace, &mut first, "C-k");
        press(&mut store, &mut workspace, &mut second, "C-k");
        press(&mut store, &mut workspace, &mut first, "C-k");
        assert_eq!(workspace.kill_ring, ["\n", "two", "one"]);
    }

    /// The kill ring keeps the newest sixty kills, newest first.
    #[test]
    fn the_kill_ring_stays_bounded_at_sixty() {
        let text = (0..70)
            .map(|line| format!("line{line}\n"))
            .collect::<String>();
        let (mut store, mut workspace, mut view, _) = editor(&text);
        for _ in 0..70 {
            press(&mut store, &mut workspace, &mut view, "C-k C-g C-d");
        }
        assert_eq!(workspace.kill_ring.len(), KILL_RING_LIMIT);
        assert_eq!(
            workspace.kill_ring.front().map(String::as_str),
            Some("line69")
        );
        assert_eq!(
            workspace.kill_ring.back().map(String::as_str),
            Some("line10")
        );
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
