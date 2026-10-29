// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Undo and redo under one writer: every change to a document, whoever made
//! it, is a labelled frame on that document's one linear history.
//!
//! [`EditorWorkspace::apply`] is the only way a document's text changes. A
//! person's keys (through `apply_recorded`), an agent's request, a reload
//! from disk, a recovered draft and a revert all commit through it, so the
//! history always knows the document's head, and an undo never meets text it
//! did not see.
//!
//! A frame holds the edits that undo it, in the coordinates of the head. Each
//! later commit maps those edits through its deltas, unless it touches them
//! (overlaps or abuts them): then that commit *blocks* the frame, which keeps
//! its coordinates as they stood, because the text it would put back is no
//! longer next to what it removed.
//!
//! Undo (`C-/`) takes the newest frame of any origin, so an agent's edit and a
//! reload from disk are undone as a person's typing is. Nothing after the
//! newest frame can have blocked it, and undoing a frame unblocks every frame
//! it blocked: their kept coordinates are right again, because every frame
//! above it was undone first and the text is exactly what it was when it
//! committed. `undo-agent-edit` (`C-c u`) undoes the newest agent frame out
//! of order, as a frame of its own (so `C-/` puts the agent's edit back), and
//! refuses one a later change blocked, naming that change.
//!
//! A recovered draft is where a document's undo starts: `C-/` stops at its
//! frame and names it, so one key never puts the file's text over the only
//! copy of the draft (`revert-buffer` does that knowingly, as a frame of its
//! own).
//!
//! Consecutive self-inserts of one character in one view, with nothing
//! between them (no command, no other commit, the caret where the last one
//! left it), join the newest frame, up to [`AMALGAM_CHARS`] characters, so
//! `C-/` takes back a run of typing as Emacs does. A history keeps at most
//! [`UNDO_FRAME_LIMIT`] frames and [`UNDO_BYTE_LIMIT`] bytes of text to put
//! back; past either, the oldest frames go, and the next command says so
//! once, until an undo takes the history under its bound again.

use std::collections::VecDeque;

use aterm_buffer::Seq;

use super::view::EditorBufferView;
use super::workspace::{EditorEffect, EditorError, EditorWorkspace, snapshot_for_view};
use crate::store::{
    DocumentId, DocumentStore, DocumentTxnOutcome, DocumentViewId, EditDelta, TextEdit,
    rebase_position,
};

/// The most frames one document's history keeps; past it the oldest go.
pub const UNDO_FRAME_LIMIT: usize = 4096;

/// The most bytes of text one document's frames hold to put back (32 MiB);
/// past it the oldest frames go, though the newest always stays.
pub const UNDO_BYTE_LIMIT: usize = 32 * 1024 * 1024;

/// The most self-inserted characters one frame joins.
pub const AMALGAM_CHARS: usize = 20;

/// What `C-/` says at a recovered draft's frame.
pub const RECOVERED_FLOOR: &str =
    "Undo stops at the recovered draft; revert-buffer takes the file's text instead";

/// Who made a change: the label each undo frame carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A person's keys or command in one view.
    Keyboard { view: DocumentViewId },
    /// An agent's request (`doc apply`, an edit script), with who sent it and
    /// what it called the change.
    Agent { principal: String, label: String },
    /// The file changed on disk and the buffer took it (the file watch, a
    /// repeated open).
    Disk,
    /// A draft the journal recovered when the document opened: the floor
    /// `C-/` stops at ([`RECOVERED_FLOOR`]).
    Recovered,
    /// The person asked for the file's text back (`revert-buffer`, Discard
    /// Changes and Reload from Disk).
    Revert,
}

impl Origin {
    /// How a status line names a change of this origin.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Keyboard { .. } => "your edit".to_string(),
            Self::Agent { principal, label } if label.is_empty() => {
                format!("agent edit by {principal}")
            }
            Self::Agent { label, .. } => format!("agent edit '{label}'"),
            Self::Disk => "the reload from disk".to_string(),
            Self::Recovered => "the recovered draft".to_string(),
            Self::Revert => "the revert to the file on disk".to_string(),
        }
    }
}

/// One change to a document through the one writer ([`EditorWorkspace::apply`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Byte edits against one sequence of the document; they may come in any
    /// order but may not overlap.
    pub edits: Vec<TextEdit>,
    pub origin: Origin,
}

/// What stopped a frame from being undone out of order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Blocker {
    /// A later frame touched this one's text; undoing that frame frees it.
    Frame(u64),
    /// The document changed at this sequence without the writer recording
    /// it, so no frame's coordinates can be trusted again.
    Unrecorded(Seq),
}

#[derive(Clone, Debug)]
struct Frame {
    id: u64,
    origin: Origin,
    /// The agent frame this one undid (`undo-agent-edit`).
    undoes: Option<u64>,
    seq_before: Seq,
    seq_after: Seq,
    /// The edits that undo this frame: in the head's coordinates while it is
    /// not blocked, else in those of the moment its blocker committed.
    inverse: Vec<TextEdit>,
    blocked_by: Option<Blocker>,
    /// The text before this frame, which the `SnapshotRestore` rig puts back.
    #[cfg(test)]
    before: std::sync::Arc<str>,
}

impl Frame {
    fn bytes(&self) -> usize {
        self.inverse.iter().map(|edit| edit.insert.len()).sum()
    }
}

/// An undone frame, which redo commits again.
#[derive(Clone, Debug)]
struct RedoFrame {
    origin: Origin,
    undoes: Option<u64>,
    /// The frame's own edits, in the coordinates of the text after its undo.
    edits: Vec<TextEdit>,
}

/// A self-insert of one character: where it went and its length in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Typed {
    view: DocumentViewId,
    at: usize,
    len: usize,
}

/// The run of self-inserts the newest frame holds, which the next one joins
/// while nothing came between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    frame: u64,
    view: DocumentViewId,
    /// The document's sequence after the run's last insert.
    seq: Seq,
    /// Where the next insert must land: the caret the last one left.
    caret: usize,
    chars: usize,
}

/// How one commit goes onto the history.
#[derive(Clone, Copy, Debug, Default)]
struct Step {
    /// The agent frame an `undo-agent-edit` undid.
    undoes: Option<u64>,
    /// Redo itself, which keeps the frames after the one it commits.
    keeps_redo: bool,
    /// A self-insert, which may join the newest frame's run.
    typed: Option<Typed>,
}

/// Which frames a commit carries through its deltas (`#[cfg(test)]` rigs
/// break the rule).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mapping {
    /// Every unblocked frame: the rule.
    Unblocked,
    /// None (the `SkipRebase` rig).
    #[cfg(test)]
    Nothing,
    /// Blocked frames too (the `MapBlocked` rig).
    #[cfg(test)]
    AlsoBlocked,
}

impl Mapping {
    const fn maps_unblocked(self) -> bool {
        match self {
            Self::Unblocked => true,
            #[cfg(test)]
            Self::AlsoBlocked => true,
            #[cfg(test)]
            Self::Nothing => false,
        }
    }

    const fn maps_blocked(self) -> bool {
        match self {
            Self::Unblocked => false,
            #[cfg(test)]
            Self::AlsoBlocked => true,
            #[cfg(test)]
            Self::Nothing => false,
        }
    }
}

/// Carry `edits` through a commit's `deltas`.
fn map_through(edits: &mut [TextEdit], deltas: &[EditDelta]) {
    for edit in edits {
        edit.range =
            rebase_position(edit.range.start, deltas)..rebase_position(edit.range.end, deltas);
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct DocumentHistory {
    /// Oldest first.
    undo: VecDeque<Frame>,
    redo: Vec<RedoFrame>,
    /// The sequence the last recorded commit left the document at.
    head: Option<Seq>,
    /// Text `undo`'s frames hold to put back.
    bytes: usize,
    next_id: u64,
    /// Frames the bounds dropped since a status line last said so.
    dropped: usize,
    /// Whether a status line said the history is full since it last went
    /// under its bounds.
    said_full: bool,
    run: Option<Run>,
}

impl DocumentHistory {
    /// Meet the document at `current`. A head the history did not record came
    /// from a commit that bypassed the writer: every frame's coordinates are
    /// unknown from here, so each is blocked, never guessed at, and redo goes.
    fn adopt_head(&mut self, current: Seq) {
        if self.head.is_some_and(|head| head != current) {
            for frame in &mut self.undo {
                if frame.blocked_by.is_none() {
                    frame.blocked_by = Some(Blocker::Unrecorded(current));
                }
            }
            self.redo.clear();
            self.run = None;
        }
        self.head = Some(current);
    }

    /// Carry every unblocked frame through a commit's `deltas`; one the
    /// commit touches is blocked by `by` and keeps its coordinates.
    fn rebase(&mut self, deltas: &[EditDelta], by: Blocker, mapping: Mapping) {
        for frame in &mut self.undo {
            if frame.blocked_by.is_some() {
                if mapping.maps_blocked() {
                    map_through(&mut frame.inverse, deltas);
                }
                continue;
            }
            if touches(&frame.inverse, deltas) {
                frame.blocked_by = Some(by);
            } else if mapping.maps_unblocked() {
                map_through(&mut frame.inverse, deltas);
            }
        }
    }

    /// Whether `typed`, committed at `base`, joins the newest frame's run.
    fn joins(&self, typed: Typed, base: Seq) -> bool {
        let Some(run) = self.run else {
            return false;
        };
        run.view == typed.view
            && run.seq == base
            && run.caret == typed.at
            && run.chars < AMALGAM_CHARS
            && self.undo.back().is_some_and(|top| {
                top.id == run.frame
                    && top.blocked_by.is_none()
                    && matches!(&top.inverse[..], [edit] if edit.insert.is_empty())
            })
    }

    fn push(&mut self, frame: Frame) {
        self.bytes = self.bytes.saturating_add(frame.bytes());
        self.undo.push_back(frame);
        while self.undo.len() > 1
            && (self.undo.len() > UNDO_FRAME_LIMIT || self.bytes > UNDO_BYTE_LIMIT)
        {
            if let Some(oldest) = self.undo.pop_front() {
                self.bytes = self.bytes.saturating_sub(oldest.bytes());
                self.dropped = self.dropped.saturating_add(1);
            }
        }
    }

    /// Take the newest frame off (an undo): the history is under its bounds
    /// again, so the next drop is said again.
    fn pop(&mut self) -> Option<Frame> {
        let frame = self.undo.pop_back()?;
        self.bytes = self.bytes.saturating_sub(frame.bytes());
        self.said_full = false;
        self.run = None;
        Some(frame)
    }

    /// Whether `frame` is an agent frame a later `undo-agent-edit` undid (it
    /// stays, blocked by that undo, until `C-/` takes the undo back).
    fn undone(&self, frame: &Frame) -> bool {
        matches!(frame.blocked_by, Some(Blocker::Frame(by))
            if self.undo.iter().any(|undo| undo.id == by && undo.undoes == Some(frame.id)))
    }

    /// The words for what blocks a frame.
    fn blocker_words(&self, blocker: Blocker) -> String {
        match blocker {
            Blocker::Frame(id) => self.undo.iter().find(|frame| frame.id == id).map_or_else(
                || "a later edit".to_string(),
                |frame| {
                    let what = if frame.undoes.is_some() {
                        "your undo of it".to_string()
                    } else {
                        frame.origin.describe()
                    };
                    format!("{what} (seq {})", frame.seq_after.0)
                },
            ),
            Blocker::Unrecorded(seq) => {
                format!("a change made outside the editor (seq {})", seq.0)
            }
        }
    }
}

/// Whether any edit's range overlaps or abuts any delta's replaced range.
fn touches(inverse: &[TextEdit], deltas: &[EditDelta]) -> bool {
    inverse.iter().any(|edit| {
        deltas
            .iter()
            .any(|delta| edit.range.start <= delta.old.end && delta.old.start <= edit.range.end)
    })
}

/// Which rule the conformance rigs break (`#[cfg(test)]` only).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HistoryMutant {
    /// A commit leaves earlier frames' coordinates where they were.
    SkipRebase,
    /// `undo-agent-edit` applies a blocked frame.
    ApplyBlocked,
    /// `undo-agent-edit` puts back the whole text from before the frame.
    SnapshotRestore,
    /// A commit carries blocked frames through its deltas too, so a frame
    /// loses the coordinates it was blocked at.
    MapBlocked,
    /// Undoing a frame leaves the frames it blocked blocked.
    NoUnblock,
}

impl EditorWorkspace {
    /// Whether `C-/` has a frame to take: there is one, and it is not a
    /// recovered draft's ([`RECOVERED_FLOOR`]).
    pub fn can_undo(&self, document: DocumentId) -> bool {
        self.histories.get(&document).is_some_and(|history| {
            history
                .undo
                .back()
                .is_some_and(|top| top.origin != Origin::Recovered)
        })
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

    /// Commit `request` to `document` at `base` as one labelled undo frame:
    /// the one writer for a change made outside a view's own commands (an
    /// agent's request, a reload, a recovered draft, a revert). The
    /// workspace's marks follow the edits; the caller carries each view
    /// through the returned deltas ([`EditorBufferView::observe_external`]).
    ///
    /// # Errors
    /// `UnknownDocument` for a document the store does not hold,
    /// `TransactionConflict` when the document is no longer at `base`,
    /// `InvalidSelections` for an edit outside the text or off a character
    /// boundary, and `TransactionRejected` for overlapping edits or a
    /// closing document.
    pub fn apply(
        &mut self,
        store: &mut DocumentStore,
        document: DocumentId,
        base: Seq,
        request: Request,
    ) -> Result<(Seq, Vec<EditDelta>), EditorError> {
        self.commit(store, document, base, request, Step::default())
    }

    /// Make `document`'s text `text` at `base` through [`Self::apply`], as the
    /// smallest edits ([`crate::diff::minimal_edits`]), so a caret, mark or
    /// scroll anchor outside what changed stays on its text. The same text
    /// commits nothing and returns `base` with no deltas.
    ///
    /// # Errors
    /// As [`Self::apply`].
    pub fn replace_text(
        &mut self,
        store: &mut DocumentStore,
        document: DocumentId,
        base: Seq,
        text: &str,
        origin: Origin,
    ) -> Result<(Seq, Vec<EditDelta>), EditorError> {
        let snapshot = store
            .snapshot(document)
            .ok_or(EditorError::UnknownDocument)?;
        if snapshot.seq != base {
            return Err(EditorError::TransactionConflict {
                current: snapshot.seq,
            });
        }
        let edits = crate::diff::minimal_edits(&snapshot.text, text);
        if edits.is_empty() {
            return Ok((base, Vec::new()));
        }
        self.apply(store, document, base, Request { edits, origin })
    }

    /// The status line for the frames the bounds dropped from `document`'s
    /// history: said once when it fills, and again only after an undo took
    /// it under its bounds and it filled again.
    pub fn take_history_note(&mut self, document: DocumentId) -> Option<String> {
        let history = self.histories.get_mut(&document)?;
        let dropped = std::mem::take(&mut history.dropped);
        if dropped == 0 || std::mem::replace(&mut history.said_full, true) {
            return None;
        }
        Some(format!(
            "Undo history is full: the oldest {dropped} edit{} can no longer be undone",
            if dropped == 1 { "" } else { "s" }
        ))
    }

    /// End `document`'s run of self-inserts: a command came between.
    pub(super) fn end_run(&mut self, document: DocumentId) {
        if let Some(history) = self.histories.get_mut(&document) {
            history.run = None;
        }
    }

    pub(super) fn apply_recorded(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        edits: Vec<TextEdit>,
    ) -> Result<(Seq, Vec<EditDelta>), EditorError> {
        self.apply_typed(store, view, edits, false)
    }

    /// [`Self::apply_recorded`] for typed text: a `self_insert` of one
    /// character (not a newline) at a caret may join the newest frame's run.
    pub(super) fn apply_typed(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        edits: Vec<TextEdit>,
        self_insert: bool,
    ) -> Result<(Seq, Vec<EditDelta>), EditorError> {
        let base = snapshot_for_view(store, view)?.seq;
        let typed = self_insert
            .then(|| one_char_insertion(&edits))
            .flatten()
            .map(|(at, len)| Typed {
                view: view.document_view,
                at,
                len,
            });
        let request = Request {
            edits,
            origin: Origin::Keyboard {
                view: view.document_view,
            },
        };
        let step = Step {
            typed,
            ..Step::default()
        };
        let (seq, deltas) = self.commit(store, view.document, base, request, step)?;
        view.observe_own(seq, &deltas);
        self.pending_commits
            .push((view.document, seq, deltas.clone()));
        Ok((seq, deltas))
    }

    /// The writer itself: commit `request` at `base`, carry the marks and
    /// every earlier frame through its deltas, and push its frame, or join a
    /// self-insert to the newest frame's run ([`Step`]).
    fn commit(
        &mut self,
        store: &mut DocumentStore,
        document: DocumentId,
        base: Seq,
        request: Request,
        step: Step,
    ) -> Result<(Seq, Vec<EditDelta>), EditorError> {
        let Request { mut edits, origin } = request;
        let snapshot = store
            .snapshot(document)
            .ok_or(EditorError::UnknownDocument)?;
        if snapshot.seq != base {
            return Err(EditorError::TransactionConflict {
                current: snapshot.seq,
            });
        }
        edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
        let inverse = inverse_edits(&snapshot.text, &edits)?;
        let (seq, deltas) = transact(store, document, base, edits)?;
        if deltas.is_empty() {
            return Ok((seq, deltas));
        }
        self.rebase_marks(document, &deltas);
        let mapping = self.mapping();
        let history = self.histories.entry(document).or_default();
        history.adopt_head(base);
        let joined = step.typed.filter(|typed| history.joins(*typed, base));
        if let Some(typed) = joined
            && let Some(mut top) = history.undo.pop_back()
        {
            // The run's frame takes this character too: it undoes the
            // whole run, and the other frames carry through as usual.
            history.rebase(&deltas, Blocker::Frame(top.id), mapping);
            if let Some(edit) = top.inverse.first_mut() {
                edit.range.end = edit.range.end.saturating_add(typed.len);
            }
            top.seq_after = seq;
            history.undo.push_back(top);
            history.run = history.run.map(|run| Run {
                seq,
                caret: typed.at.saturating_add(typed.len),
                chars: run.chars.saturating_add(1),
                ..run
            });
        } else {
            let id = history.next_id;
            history.next_id = history.next_id.wrapping_add(1);
            history.rebase(&deltas, Blocker::Frame(id), mapping);
            history.push(Frame {
                id,
                origin,
                undoes: step.undoes,
                seq_before: base,
                seq_after: seq,
                inverse,
                blocked_by: None,
                #[cfg(test)]
                before: snapshot.text.clone(),
            });
            history.run = step.typed.map(|typed| Run {
                frame: id,
                view: typed.view,
                seq,
                caret: typed.at.saturating_add(typed.len),
                chars: 1,
            });
        }
        history.head = Some(seq);
        if !step.keeps_redo {
            history.redo.clear();
        }
        Ok((seq, deltas))
    }

    /// Which frames a commit carries through its deltas.
    #[cfg(test)]
    fn mapping(&self) -> Mapping {
        match self.history_mutant {
            Some(HistoryMutant::SkipRebase) => Mapping::Nothing,
            Some(HistoryMutant::MapBlocked) => Mapping::AlsoBlocked,
            _ => Mapping::Unblocked,
        }
    }

    /// Which frames a commit carries through its deltas.
    #[cfg(not(test))]
    const fn mapping(&self) -> Mapping {
        Mapping::Unblocked
    }

    /// `undo` (`C-/`): undo the newest frame, whoever made it.
    pub(super) fn undo(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let history = self.histories.entry(view.document).or_default();
        history.adopt_head(snapshot.seq);
        let top = match history.undo.back() {
            None => return Err(EditorError::NothingToUndo),
            Some(Frame {
                origin: Origin::Recovered,
                ..
            }) => {
                return Err(EditorError::UndoRefused {
                    why: RECOVERED_FLOOR.to_string(),
                });
            }
            Some(Frame {
                blocked_by: Some(blocker),
                ..
            }) => {
                return Err(EditorError::UndoRefused {
                    why: format!("Undo blocked by {}", history.blocker_words(*blocker)),
                });
            }
            Some(top) => top,
        };
        let redo_edits = inverse_edits(&snapshot.text, &top.inverse)?;
        let (seq, deltas) = transact(store, view.document, snapshot.seq, top.inverse.clone())?;
        let Some(top) = history.pop() else {
            return Err(EditorError::NothingToUndo);
        };
        let maps = self.mapping().maps_unblocked();
        #[cfg(test)]
        let unblocks = self.history_mutant != Some(HistoryMutant::NoUnblock);
        #[cfg(not(test))]
        let unblocks = true;
        let history = self.histories.entry(view.document).or_default();
        for frame in &mut history.undo {
            match frame.blocked_by {
                Some(Blocker::Frame(by)) if by == top.id && unblocks => frame.blocked_by = None,
                Some(_) => {}
                None if !maps => {}
                // Not reachable: a frame the undone one did not touch is
                // strictly apart from what it inserted. Kept blocked, never
                // corrupted, if it ever is.
                None if touches(&frame.inverse, &deltas) => {
                    frame.blocked_by = Some(Blocker::Unrecorded(seq));
                }
                None => map_through(&mut frame.inverse, &deltas),
            }
        }
        history.head = Some(seq);
        let said = undid_words(&top);
        history.redo.push(RedoFrame {
            origin: top.origin,
            undoes: top.undoes,
            edits: redo_edits,
        });
        view.observe_own(seq, &deltas);
        self.pending_commits
            .push((view.document, seq, deltas.clone()));
        self.rebase_marks(view.document, &deltas);
        self.last_yank = None;
        Ok(said.map(EditorEffect::Status).into_iter().collect())
    }

    /// `redo`: commit the last undone frame again, as the frame it was.
    pub(super) fn redo(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        let history = self.histories.entry(view.document).or_default();
        history.adopt_head(snapshot.seq);
        let frame = history.redo.pop().ok_or(EditorError::NothingToRedo)?;
        let request = Request {
            edits: frame.edits.clone(),
            origin: frame.origin.clone(),
        };
        let committed = self.commit(
            store,
            view.document,
            snapshot.seq,
            request,
            Step {
                undoes: frame.undoes,
                keeps_redo: true,
                typed: None,
            },
        );
        let (seq, deltas) = match committed {
            Ok(committed) => committed,
            Err(error) => {
                self.histories
                    .entry(view.document)
                    .or_default()
                    .redo
                    .push(frame);
                return Err(error);
            }
        };
        view.observe_own(seq, &deltas);
        self.pending_commits
            .push((view.document, seq, deltas.clone()));
        self.last_yank = None;
        Ok(match frame.origin {
            Origin::Keyboard { .. } if frame.undoes.is_none() => Vec::new(),
            origin => vec![EditorEffect::Status(format!("Redid {}", origin.describe()))],
        })
    }

    /// `undo-agent-edit` (`C-c u`): undo the newest agent frame not already
    /// undone, wherever it sits, as a frame of its own; refuse one a later
    /// change blocked, naming that change.
    pub(super) fn undo_agent_edit(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let snapshot = snapshot_for_view(store, view)?;
        #[cfg(test)]
        let mutant = self.history_mutant;
        let history = self.histories.entry(view.document).or_default();
        history.adopt_head(snapshot.seq);
        let Some(target) =
            history.undo.iter().rev().find(|frame| {
                matches!(frame.origin, Origin::Agent { .. }) && !history.undone(frame)
            })
        else {
            return Err(EditorError::UndoRefused {
                why: "No agent edit to undo".to_string(),
            });
        };
        #[cfg(test)]
        let applies_blocked = mutant == Some(HistoryMutant::ApplyBlocked);
        #[cfg(not(test))]
        let applies_blocked = false;
        if let Some(blocker) = target.blocked_by
            && !applies_blocked
        {
            let hint = if matches!(blocker, Blocker::Frame(_)) {
                "; C-/ undoes that first"
            } else {
                ""
            };
            return Err(EditorError::UndoRefused {
                why: format!(
                    "Undo of {} blocked by {}{hint}",
                    target.origin.describe(),
                    history.blocker_words(blocker)
                ),
            });
        }
        let (id, what, seq_after) = (target.id, target.origin.describe(), target.seq_after);
        #[cfg(test)]
        let edits = if mutant == Some(HistoryMutant::SnapshotRestore) {
            crate::diff::minimal_edits(&snapshot.text, &target.before)
        } else {
            target.inverse.clone()
        };
        #[cfg(not(test))]
        let edits = target.inverse.clone();
        let request = Request {
            edits,
            origin: Origin::Keyboard {
                view: view.document_view,
            },
        };
        let (seq, deltas) = self.commit(
            store,
            view.document,
            snapshot.seq,
            request,
            Step {
                undoes: Some(id),
                ..Step::default()
            },
        )?;
        view.observe_external(seq, &deltas);
        self.pending_commits
            .push((view.document, seq, deltas.clone()));
        self.last_yank = None;
        Ok(vec![EditorEffect::Status(format!(
            "Undid {what} (seq {}); C-/ puts it back",
            seq_after.0
        ))])
    }
}

/// Where a self-insert of one character (not a newline) at a caret goes,
/// and its length in bytes; `None` for any other edit.
fn one_char_insertion(edits: &[TextEdit]) -> Option<(usize, usize)> {
    let [edit] = edits else {
        return None;
    };
    let mut chars = edit.insert.chars();
    let typed = chars.next().filter(|&typed| typed != '\n')?;
    (edit.range.is_empty() && chars.next().is_none())
        .then_some((edit.range.start, typed.len_utf8()))
}

/// What an undo of `frame` says: nothing for a person's own edit.
fn undid_words(frame: &Frame) -> Option<String> {
    match (&frame.origin, frame.undoes) {
        (Origin::Keyboard { .. }, None) => None,
        (Origin::Keyboard { .. }, Some(_)) => Some(format!(
            "Put the agent edit back (seq {})",
            frame.seq_before.0
        )),
        (origin, _) => Some(format!(
            "Undid {} (seq {})",
            origin.describe(),
            frame.seq_after.0
        )),
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
        if edit.range.start > edit.range.end
            || edit.range.end > source.len()
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

/// One frame as the tests see it: its id, origin, the agent frame it undid,
/// and the id of the frame blocking it (`u64::MAX` for an unrecorded change).
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FrameProbe {
    pub(super) id: u64,
    pub(super) origin: Origin,
    pub(super) undoes: Option<u64>,
    pub(super) blocked_by: Option<u64>,
}

#[cfg(test)]
impl EditorWorkspace {
    /// `document`'s frames, oldest first.
    pub(super) fn frame_probe(&self, document: DocumentId) -> Vec<FrameProbe> {
        self.histories
            .get(&document)
            .map(|history| {
                history
                    .undo
                    .iter()
                    .map(|frame| FrameProbe {
                        id: frame.id,
                        origin: frame.origin.clone(),
                        undoes: frame.undoes,
                        blocked_by: frame.blocked_by.map(|blocker| match blocker {
                            Blocker::Frame(id) => id,
                            Blocker::Unrecorded(_) => u64::MAX,
                        }),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
