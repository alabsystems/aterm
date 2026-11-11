// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The kill ring: the kills (kill-region, kill-line, kill-word and
//! backward-kill-word) push to it once their deletion commits, kill-ring-save
//! copies to it, and yank and yank-pop insert from it. Input without clipboard
//! authority (in aterm, a control client's) has a ring of its own
//! ([`EditorWorkspace::kills`]), as the person's ring mirrors the clipboard.

use std::collections::VecDeque;
use std::ops::Range;

use aterm_buffer::Seq;

use super::command::{CommandAuthority, EditorCommand, PrefixArgument};
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
        let (ring, _) = self.kills();
        let ring_index = match argument {
            Some(PrefixArgument::Numeric { .. }) => count.saturating_sub(1) % ring.len().max(1),
            Some(PrefixArgument::Raw(_)) | None => 0,
        };
        let text = ring
            .get(ring_index)
            .map(|kill| self.held_kill(kill))
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
        let (ring, _) = self.kills();
        let next_index = previous.ring_index.saturating_add(1) % ring.len().max(1);
        let replacement = ring
            .get(next_index)
            .map(|kill| self.held_kill(kill))
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
    pub(super) fn record_kill(
        &mut self,
        text: String,
        previous: Option<EditorCommand>,
        backward: bool,
    ) {
        if text.is_empty() {
            return;
        }
        // Only the person's ring reaches the clipboard (`EditorEffect::KillToClipboard`).
        self.kill_ring_changed |= self.reaches_persons_kills();
        let (ring, grid_kill) = self.kills_mut();
        *grid_kill = None;
        if appends_kill(previous)
            && let Some(newest) = ring.front_mut()
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
        ring.push_front(text);
        ring.truncate(KILL_RING_LIMIT);
    }

    /// Whether the input being reduced reaches the person's kill ring: only input that carries
    /// [`CommandAuthority::Clipboard`] does, as that ring is what the clipboard mirrors.
    fn reaches_persons_kills(&self) -> bool {
        self.granted.contains(&CommandAuthority::Clipboard)
    }

    /// The kill ring the input being reduced kills to and yanks from, and the value the grid's
    /// last copy or cut put on it: the person's ([`Self::kill_ring`], [`Self::grid_kill`]) for
    /// input with clipboard authority, else [`Self::guest_kill_ring`] and
    /// [`Self::guest_grid_kill`], so input without it never reads or adds to the person's kills.
    pub(super) fn kills(&self) -> (&VecDeque<String>, Option<&str>) {
        if self.reaches_persons_kills() {
            (&self.kill_ring, self.grid_kill.as_deref())
        } else {
            (&self.guest_kill_ring, self.guest_grid_kill.as_deref())
        }
    }

    /// [`Self::kills`], to change.
    pub(super) fn kills_mut(&mut self) -> (&mut VecDeque<String>, &mut Option<String>) {
        if self.reaches_persons_kills() {
            (&mut self.kill_ring, &mut self.grid_kill)
        } else {
            (&mut self.guest_kill_ring, &mut self.guest_grid_kill)
        }
    }
}

impl EditorWorkspace {
    /// `kill` as a yank writes it into the document the input goes to: its CRLF as LF when the
    /// document is held with LF for CRLF on disk ([`Self::crlf_on_disk`]), else as it is.
    pub(super) fn held_kill(&self, kill: &str) -> String {
        if self.crlf_on_disk {
            kill.replace("\r\n", "\n")
        } else {
            kill.to_string()
        }
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
#[path = "kill_tests.rs"]
mod tests;
