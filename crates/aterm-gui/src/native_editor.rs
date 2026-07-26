// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native text-editor interaction core.
//!
//! This is an editor over [`crate::document_store::DocumentStore`], not a terminal
//! emulator. Commands, key sequences, prefix state, selections, kill/yank state,
//! registers, marks, undo frames, and keyboard macros are structured data. Canonical
//! bytes and commit ordering remain exclusively in the document store.

#![allow(dead_code, reason = "native editor host integration lands in stages")]

use std::collections::{BTreeMap, VecDeque};
use std::ops::Range;

use aterm_buffer::Seq;
use aterm_grapheme::{GraphemeClusters, grapheme_display_width};

use crate::document_store::{
    DocumentId, DocumentStore, DocumentTxnOutcome, DocumentViewId, EditDelta, TextEdit,
    rebase_position,
};

const KILL_RING_LIMIT: usize = 60;
const COMMAND_HISTORY_LIMIT: usize = 200;
const COMMAND_COMPLETION_LIMIT: usize = 8;
/// A minibuffer is UI state, not an unbounded document.  This cap bounds paste,
/// IME commit, command lookup, and incremental-search work per interaction while
/// still leaving ample room for canonical file URIs and editor command names.
const MINIBUFFER_QUERY_LIMIT: usize = 4 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selection {
    pub(crate) anchor: usize,
    pub(crate) head: usize,
}

impl Selection {
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

    fn collapse(&mut self, position: usize) {
        self.anchor = position;
        self.head = position;
    }

    fn move_head(&mut self, position: usize, preserve_anchor: bool) {
        if preserve_anchor {
            self.head = position;
        } else {
            self.collapse(position);
        }
    }

    fn rebase(&mut self, deltas: &[EditDelta]) {
        self.anchor = rebase_position(self.anchor, deltas);
        self.head = rebase_position(self.head, deltas);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Minibuffer {
    #[default]
    Inactive,
    Command {
        query: String,
        selected: usize,
    },
    Search {
        query: String,
        origin: usize,
    },
    Buffer {
        query: String,
    },
    GotoLine {
        query: String,
        origin: usize,
    },
    Message(String),
}

/// State that deliberately belongs to one visible editor view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditorBufferView {
    pub(crate) document: DocumentId,
    pub(crate) document_view: DocumentViewId,
    pub(crate) selections: Vec<Selection>,
    pub(crate) primary: usize,
    pub(crate) viewport_anchor: usize,
    /// Renderer-derived body capacity installed by the host for this exact
    /// view/window. Caret reveal never guesses a desktop-sized row count.
    viewport_lines: usize,
    pub(crate) folds: Vec<Range<usize>>,
    pub(crate) minibuffer: Minibuffer,
    /// `set-mark-command` activates a persistent region anchor. Ordinary motion
    /// advances only `head` while this bit is set; document mutations and abort
    /// deactivate it. A non-empty pointer/Shift selection does not need this bit
    /// for `kill-region`, but subsequent unshifted motion may collapse it.
    pub(crate) mark_active: bool,
    pub(crate) prefix_hud: Option<String>,
    desired_column: Option<usize>,
    chord_prefix: Vec<KeyChord>,
    prefix_argument: Option<i64>,
    anchor_seq: Seq,
}

impl EditorBufferView {
    pub(crate) fn new(
        document: DocumentId,
        document_view: DocumentViewId,
        anchor_seq: Seq,
    ) -> Self {
        Self {
            document,
            document_view,
            selections: vec![Selection::caret(0)],
            primary: 0,
            viewport_anchor: 0,
            viewport_lines: 4,
            folds: Vec::new(),
            minibuffer: Minibuffer::Inactive,
            mark_active: false,
            prefix_hud: None,
            desired_column: None,
            chord_prefix: Vec::new(),
            prefix_argument: None,
            anchor_seq,
        }
    }

    pub(crate) fn anchor_seq(&self) -> Seq {
        self.anchor_seq
    }

    pub(crate) fn primary_selection(&self) -> &Selection {
        &self.selections[self.primary.min(self.selections.len().saturating_sub(1))]
    }

    pub(crate) fn scroll_lines(&mut self, text: &str, delta: i32) {
        let total_lines = text.bytes().filter(|byte| *byte == b'\n').count() + 1;
        // Store the same stable anchor the renderer can actually present.  A
        // wheel fling used to retain an EOF line here while paint clamped only
        // its temporary clone to the last full viewport.  Reverse scrolling
        // then appeared inert until that invisible debt had been repaid.
        // `viewport_lines()` also gives a not-yet-reconciled zero-capacity view
        // the only useful interpretation: one visible line.
        let last_full_viewport_anchor = total_lines.saturating_sub(self.viewport_lines());
        let current = line_number(text, self.viewport_anchor).min(last_full_viewport_anchor);
        let target = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            current.saturating_add(delta as usize)
        }
        .min(last_full_viewport_anchor);
        self.viewport_anchor = byte_of_line(text, target);
    }

    pub(crate) fn viewport_lines(&self) -> usize {
        self.viewport_lines.max(1)
    }

    pub(crate) fn reconcile_viewport(&mut self, text: &str, visible_lines: usize) -> bool {
        let before_anchor = self.viewport_anchor;
        let before_lines = self.viewport_lines;
        self.viewport_lines = visible_lines.clamp(1, 256);
        self.ensure_primary_visible(text, self.viewport_lines);
        before_anchor != self.viewport_anchor || before_lines != self.viewport_lines
    }

    pub(crate) fn ensure_primary_visible(&mut self, text: &str, visible_lines: usize) {
        let visible_lines = visible_lines.clamp(1, 256);
        self.viewport_lines = visible_lines;
        let caret_line = line_number(text, self.primary_selection().head);
        let total_lines = text.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let last_full_viewport_anchor = total_lines.saturating_sub(visible_lines);
        let mut anchor_line =
            line_number(text, self.viewport_anchor).min(last_full_viewport_anchor);
        if caret_line < anchor_line {
            anchor_line = caret_line;
        } else if caret_line >= anchor_line.saturating_add(visible_lines) {
            anchor_line = caret_line.saturating_sub(visible_lines.saturating_sub(3));
        }
        // A document replacement can map both the caret and the old anchor to
        // EOF. Clamp after caret reveal as well: a short document must start at
        // line zero when the viewport can show it in full, rather than leaving
        // authored diagnostics above a synthetic trailing blank line.
        anchor_line = anchor_line.min(last_full_viewport_anchor);
        self.viewport_anchor = byte_of_line(text, anchor_line);
    }

    /// Place or extend the primary selection from a pointer hit expressed as a
    /// canonical document byte offset. The UI hit mapper already emits UTF-8
    /// boundaries, but clamping here keeps the editor safe when this seam is
    /// driven by accessibility or a future platform adapter.
    pub(crate) fn pointer_select(
        &mut self,
        text: &str,
        position: usize,
        extend: bool,
        visible_lines: usize,
    ) -> bool {
        if self.minibuffer_active() {
            return false;
        }
        let position = clamp_to_grapheme_boundary(text, position.min(text.len()));
        let before = self.selections.clone();
        let before_primary = self.primary;
        if extend && !self.selections.is_empty() {
            self.primary = self.primary.min(self.selections.len() - 1);
            self.selections[self.primary].head = position;
        } else {
            self.selections.clear();
            self.selections.push(Selection::caret(position));
            self.primary = 0;
        }
        self.desired_column = None;
        self.mark_active = false;
        self.chord_prefix.clear();
        self.prefix_hud = None;
        self.prefix_argument = None;
        self.ensure_primary_visible(text, visible_lines);
        before_primary != self.primary || before != self.selections
    }

    pub(crate) fn cancel_transient(&mut self) {
        if let Minibuffer::Search { origin, .. } | Minibuffer::GotoLine { origin, .. } =
            &self.minibuffer
        {
            let primary = self.primary.min(self.selections.len().saturating_sub(1));
            if let Some(selection) = self.selections.get_mut(primary) {
                selection.collapse(*origin);
            }
        }
        self.chord_prefix.clear();
        self.prefix_hud = None;
        self.prefix_argument = None;
        self.minibuffer = Minibuffer::Inactive;
        self.mark_active = false;
    }

    pub(crate) fn minibuffer_active(&self) -> bool {
        matches!(
            self.minibuffer,
            Minibuffer::Command { .. }
                | Minibuffer::Search { .. }
                | Minibuffer::Buffer { .. }
                | Minibuffer::GotoLine { .. }
        )
    }

    pub(crate) fn chord_pending(&self) -> bool {
        !self.chord_prefix.is_empty()
    }

    /// Rebase view-local coordinates after a commit initiated by another controller.
    pub(crate) fn observe_external(&mut self, seq: Seq, deltas: &[EditDelta]) {
        for selection in &mut self.selections {
            selection.rebase(deltas);
        }
        self.viewport_anchor = rebase_position(self.viewport_anchor, deltas);
        self.anchor_seq = seq;
        self.desired_column = None;
    }

    fn observe_own(&mut self, seq: Seq, deltas: &[EditDelta]) {
        for selection in &mut self.selections {
            let position = rebase_position(selection.range().start, deltas);
            selection.collapse(position);
        }
        self.viewport_anchor = rebase_position(self.viewport_anchor, deltas);
        self.anchor_seq = seq;
        self.desired_column = None;
        self.mark_active = false;
    }

    fn take_count(&mut self) -> usize {
        self.prefix_argument
            .take()
            .unwrap_or(1)
            .unsigned_abs()
            .clamp(1, 10_000) as usize
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RegisterValue {
    Text(String),
    Position { document: DocumentId, offset: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GlobalMark {
    pub(crate) document: DocumentId,
    pub(crate) offset: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EditorEffect {
    SaveDocument {
        document: DocumentId,
        seq: Seq,
    },
    /// Resolve an exact, already-open editor buffer at the host boundary. The
    /// workspace never fabricates a document or retargets an app instance.
    SwitchBuffer {
        query: String,
    },
    ShowCommands,
    RevertDocument {
        document: DocumentId,
    },
    Status(String),
    Bell,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EditorError {
    UnknownDocument,
    StaleView { view: Seq, current: Seq },
    InvalidSelections,
    TransactionConflict { current: Seq },
    TransactionRejected,
    NothingToUndo,
    NothingToRedo,
    HistoryDiverged { expected: Seq, current: Seq },
    NoKill,
    MacroRecursion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorCommand {
    MoveBackward,
    MoveForward,
    MoveLineUp,
    MoveLineDown,
    MoveLineStart,
    MoveLineEnd,
    MoveWordBackward,
    MoveWordForward,
    DeleteBackward,
    DeleteForward,
    SetMark,
    KillRegion,
    KillLine,
    Yank,
    YankPop,
    Undo,
    Redo,
    Save,
    Abort,
    UniversalArgument,
    ExecuteCommand,
    IncrementalSearch,
    GotoLine,
    SwitchBuffer,
    RevertBuffer,
    StartMacro,
    EndMacro,
    PlayMacro,
}

impl EditorCommand {
    pub(crate) const ALL: [Self; 28] = [
        Self::Save,
        Self::Undo,
        Self::Redo,
        Self::IncrementalSearch,
        Self::GotoLine,
        Self::SwitchBuffer,
        Self::RevertBuffer,
        Self::SetMark,
        Self::KillRegion,
        Self::KillLine,
        Self::Yank,
        Self::YankPop,
        Self::MoveBackward,
        Self::MoveForward,
        Self::MoveLineUp,
        Self::MoveLineDown,
        Self::MoveLineStart,
        Self::MoveLineEnd,
        Self::MoveWordBackward,
        Self::MoveWordForward,
        Self::DeleteBackward,
        Self::DeleteForward,
        Self::Abort,
        Self::UniversalArgument,
        Self::ExecuteCommand,
        Self::StartMacro,
        Self::EndMacro,
        Self::PlayMacro,
    ];

    pub(crate) const fn name(&self) -> &'static str {
        match self {
            Self::MoveBackward => "backward-char",
            Self::MoveForward => "forward-char",
            Self::MoveLineUp => "previous-line",
            Self::MoveLineDown => "next-line",
            Self::MoveLineStart => "move-beginning-of-line",
            Self::MoveLineEnd => "move-end-of-line",
            Self::MoveWordBackward => "backward-word",
            Self::MoveWordForward => "forward-word",
            Self::DeleteBackward => "backward-delete-char",
            Self::DeleteForward => "delete-char",
            Self::SetMark => "set-mark-command",
            Self::KillRegion => "kill-region",
            Self::KillLine => "kill-line",
            Self::Yank => "yank",
            Self::YankPop => "yank-pop",
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::Save => "save-buffer",
            Self::Abort => "keyboard-quit",
            Self::UniversalArgument => "universal-argument",
            Self::ExecuteCommand => "execute-extended-command",
            Self::IncrementalSearch => "isearch-forward",
            Self::GotoLine => "goto-line",
            Self::SwitchBuffer => "switch-to-buffer",
            Self::RevertBuffer => "revert-buffer",
            Self::StartMacro => "start-kbd-macro",
            Self::EndMacro => "end-kbd-macro",
            Self::PlayMacro => "call-last-kbd-macro",
        }
    }

    /// Resolve the exact public command vocabulary shown by M-x. Keeping this
    /// table next to `name` prevents fuzzy/partial input from invoking a more
    /// destructive command than the user actually entered.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|command| command.name() == name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorCompletionAction {
    Previous,
    Next,
    Complete,
    Choose(usize),
}

/// Bounded prefix/fuzzy projection used by reducer, renderer, accessibility,
/// touch actions, and introspection. Ranking never authorizes a command by
/// itself: the result is still a typed `EditorCommand` from the closed table.
pub(crate) fn command_completions(query: &str) -> Vec<EditorCommand> {
    let normalized = query.trim().to_ascii_lowercase();
    let mut ranked = EditorCommand::ALL
        .into_iter()
        .enumerate()
        .filter_map(|(default_rank, command)| {
            command_match_score(command.name(), &normalized)
                .map(|score| (score, default_rank, command.name(), command))
        })
        .collect::<Vec<_>>();
    ranked.sort_by_key(|(score, default_rank, name, _)| (*score, *default_rank, *name));
    ranked
        .into_iter()
        .take(COMMAND_COMPLETION_LIMIT)
        .map(|(_, _, _, command)| command)
        .collect()
}

fn command_match_score(name: &str, query: &str) -> Option<(u8, usize, usize)> {
    if query.is_empty() {
        // A blank M-x shows every command in the CURATED table order (Save
        // first): the score must be constant so the `default_rank` tiebreak
        // decides — a `name.len()` component here would instead surface the
        // shortest names ("undo") ahead of the curation.
        return Some((5, 0, 0));
    }
    let hyphenated = query
        .split_whitespace()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if name == hyphenated {
        return Some((0, 0, name.len()));
    }
    if name.starts_with(&hyphenated) {
        // Prefix matches tie as a CLASS and fall to the curated table order:
        // ranking by the unmatched remainder would surface the shortest
        // completion ("move-end-of-line") over the curation ("move-beginning-
        // of-line" first).
        return Some((1, 0, 0));
    }
    if let Some(position) = name.find(&hyphenated) {
        return Some((2, position, name.len()));
    }
    let needle = hyphenated
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<Vec<_>>();
    if needle.is_empty() {
        // Punctuation-only queries degenerate to the blank-query broad list:
        // constant score, curated order (see above).
        return Some((5, 0, 0));
    }
    let mut matched = 0;
    let mut first = None;
    for (index, character) in name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .enumerate()
    {
        if needle.get(matched).copied() == Some(character) {
            first.get_or_insert(index);
            matched += 1;
            if matched == needle.len() {
                // `index` is the position of the LAST matched character: the
                // return runs in the very iteration that completed the needle.
                let first = first.unwrap_or(0);
                return Some((
                    3,
                    index.saturating_sub(first + needle.len() - 1),
                    name.len(),
                ));
            }
        }
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum MacroStep {
    Command(EditorCommand),
    Text(String),
}

#[derive(Clone, Debug)]
struct HistoryFrame {
    expected_seq: Seq,
    undo: Vec<TextEdit>,
    redo: Vec<TextEdit>,
}

#[derive(Clone, Debug, Default)]
struct DocumentHistory {
    undo: Vec<HistoryFrame>,
    redo: Vec<HistoryFrame>,
}

#[derive(Clone, Debug)]
struct YankRecord {
    document: DocumentId,
    seq: Seq,
    ranges: Vec<Range<usize>>,
    ring_index: usize,
}

/// State shared across every editor document and view in one workspace/window group.
pub(crate) struct EditorWorkspace {
    pub(crate) buffers: Vec<DocumentId>,
    pub(crate) kill_ring: VecDeque<String>,
    pub(crate) registers: BTreeMap<char, RegisterValue>,
    pub(crate) global_mark_ring: VecDeque<GlobalMark>,
    pub(crate) command_history: VecDeque<String>,
    pub(crate) keymap: Keymap,
    histories: BTreeMap<DocumentId, DocumentHistory>,
    recording: Option<Vec<MacroStep>>,
    last_macro: Vec<MacroStep>,
    playing_macro: bool,
    last_yank: Option<YankRecord>,
    last_commit: Option<(DocumentId, Seq, Vec<EditDelta>)>,
}

impl Default for EditorWorkspace {
    fn default() -> Self {
        Self {
            buffers: Vec::new(),
            kill_ring: VecDeque::new(),
            registers: BTreeMap::new(),
            global_mark_ring: VecDeque::new(),
            command_history: VecDeque::new(),
            keymap: Keymap::emacs(),
            histories: BTreeMap::new(),
            recording: None,
            last_macro: Vec::new(),
            playing_macro: false,
            last_yank: None,
            last_commit: None,
        }
    }
}

impl EditorWorkspace {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn can_undo(&self, document: DocumentId) -> bool {
        self.histories
            .get(&document)
            .is_some_and(|history| !history.undo.is_empty())
    }

    pub(crate) fn can_redo(&self, document: DocumentId) -> bool {
        self.histories
            .get(&document)
            .is_some_and(|history| !history.redo.is_empty())
    }

    /// Take the synchronous mutation lane's most recent commit projection. The host
    /// uses this to rebase every other view of the same canonical document before
    /// routing another input event.
    pub(crate) fn take_last_commit(
        &mut self,
        document: DocumentId,
    ) -> Option<(Seq, Vec<EditDelta>)> {
        let (committed_document, seq, deltas) = self.last_commit.take()?;
        if committed_document == document {
            Some((seq, deltas))
        } else {
            self.last_commit = Some((committed_document, seq, deltas));
            None
        }
    }

    pub(crate) fn attach(
        &mut self,
        store: &mut DocumentStore,
        document: DocumentId,
        document_view: DocumentViewId,
    ) -> Result<EditorBufferView, EditorError> {
        store
            .attach_view(document, document_view)
            .map_err(|_| EditorError::UnknownDocument)?;
        let seq = store
            .snapshot(document)
            .ok_or(EditorError::UnknownDocument)?
            .seq;
        if !self.buffers.contains(&document) {
            self.buffers.push(document);
        }
        Ok(EditorBufferView::new(document, document_view, seq))
    }

    pub(crate) fn insert_text(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        text: &str,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        if view.minibuffer_active() {
            return self.insert_minibuffer_text(store, view, text);
        }
        if let Some(steps) = &mut self.recording
            && !self.playing_macro
        {
            steps.push(MacroStep::Text(text.to_owned()));
        }
        let edits = edits_for_selections(&view.selections, text)?;
        self.apply_recorded(store, view, edits)?;
        self.last_yank = None;
        Ok(Vec::new())
    }

    /// Route one committed text span exclusively into the active minibuffer.
    /// Pasted/IME text is bounded and truncated only at UTF-8 boundaries.
    pub(crate) fn insert_minibuffer_text(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
        text: &str,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let (complete, search) = match &mut view.minibuffer {
            Minibuffer::Command { query, selected } => {
                let complete = push_bounded(query, text, MINIBUFFER_QUERY_LIMIT);
                *selected = 0;
                (complete, false)
            }
            Minibuffer::Search { query, .. } => {
                (push_bounded(query, text, MINIBUFFER_QUERY_LIMIT), true)
            }
            Minibuffer::Buffer { query } | Minibuffer::GotoLine { query, .. } => {
                (push_bounded(query, text, MINIBUFFER_QUERY_LIMIT), false)
            }
            Minibuffer::Inactive | Minibuffer::Message(_) => {
                return Ok(vec![EditorEffect::Bell]);
            }
        };
        if search {
            let mut effects = self.update_incremental_search(store, view, false)?;
            if !complete {
                effects.push(EditorEffect::Status(format!(
                    "Minibuffer input is limited to {MINIBUFFER_QUERY_LIMIT} bytes"
                )));
            }
            return Ok(effects);
        }
        if complete {
            Ok(Vec::new())
        } else {
            Ok(vec![EditorEffect::Status(format!(
                "Minibuffer input is limited to {MINIBUFFER_QUERY_LIMIT} bytes"
            ))])
        }
    }

    /// Delete the previous complete grapheme from a modal query. This never
    /// enters the canonical document transaction lane.
    pub(crate) fn minibuffer_backspace(
        &mut self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let search = matches!(view.minibuffer, Minibuffer::Search { .. });
        let query = match &mut view.minibuffer {
            Minibuffer::Command { query, selected } => {
                *selected = 0;
                query
            }
            Minibuffer::Search { query, .. }
            | Minibuffer::Buffer { query }
            | Minibuffer::GotoLine { query, .. } => query,
            Minibuffer::Inactive | Minibuffer::Message(_) => {
                return Ok(vec![EditorEffect::Bell]);
            }
        };
        let boundary = previous_boundary(query, query.len());
        query.truncate(boundary);
        if search {
            self.update_incremental_search(store, view, false)
        } else {
            Ok(Vec::new())
        }
    }

    /// Accept the active minibuffer. M-x resolves only the selected member of
    /// the current bounded typed completion projection; a query with no match
    /// remains editable and cannot mutate the document. Buffer resolution is a
    /// host effect because tabs/windows own that authority.
    pub(crate) fn submit_minibuffer(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        match std::mem::take(&mut view.minibuffer) {
            Minibuffer::Command { query, selected } => {
                let candidates = command_completions(&query);
                let Some(command) = candidates.get(selected).copied() else {
                    view.minibuffer = Minibuffer::Command {
                        query: query.clone(),
                        selected: 0,
                    };
                    return Ok(vec![EditorEffect::Status(format!(
                        "No editor command matches `{query}`"
                    ))]);
                };
                self.execute(store, view, command)
            }
            Minibuffer::Search { query, .. } => {
                Ok(vec![EditorEffect::Status(if query.is_empty() {
                    "Search cancelled: empty query".to_string()
                } else {
                    format!("Search accepted: {query}")
                })])
            }
            Minibuffer::Buffer { query } => Ok(vec![EditorEffect::SwitchBuffer { query }]),
            Minibuffer::GotoLine { query, origin } => {
                let Ok(line) = query.trim().parse::<usize>() else {
                    view.minibuffer = Minibuffer::GotoLine { query, origin };
                    return Ok(vec![EditorEffect::Status(
                        "Line number must be a positive integer".to_string(),
                    )]);
                };
                if line == 0 {
                    view.minibuffer = Minibuffer::GotoLine { query, origin };
                    return Ok(vec![EditorEffect::Status(
                        "Line numbers start at 1".to_string(),
                    )]);
                }
                let snapshot = snapshot_for_view(store, view)?;
                let target = byte_of_line(&snapshot.text, line.saturating_sub(1));
                view.selections = vec![Selection::caret(target)];
                view.primary = 0;
                view.viewport_anchor = target;
                view.mark_active = false;
                Ok(vec![EditorEffect::Status(format!("Line {line}"))])
            }
            Minibuffer::Message(_) | Minibuffer::Inactive => Ok(vec![EditorEffect::Bell]),
        }
    }

    /// Navigate, complete, or activate the one semantic M-x result list. This
    /// same reducer is driven by arrows/Tab, pointer buttons, accessibility,
    /// and control-socket actions.
    pub(crate) fn command_completion_action(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        action: EditorCompletionAction,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let (query, selected) = match &view.minibuffer {
            Minibuffer::Command { query, selected } => (query.clone(), *selected),
            _ => return Ok(vec![EditorEffect::Bell]),
        };
        let candidates = command_completions(&query);
        if candidates.is_empty() {
            return Ok(vec![EditorEffect::Status(format!(
                "No editor command matches `{query}`"
            ))]);
        }
        match action {
            EditorCompletionAction::Previous | EditorCompletionAction::Next => {
                let next = if action == EditorCompletionAction::Previous {
                    selected
                        .checked_sub(1)
                        .unwrap_or(candidates.len().saturating_sub(1))
                } else {
                    selected.saturating_add(1) % candidates.len()
                };
                if let Minibuffer::Command { selected, .. } = &mut view.minibuffer {
                    *selected = next;
                }
                Ok(Vec::new())
            }
            EditorCompletionAction::Complete => {
                let chosen = selected.min(candidates.len() - 1);
                if let Minibuffer::Command {
                    query,
                    selected: current,
                } = &mut view.minibuffer
                {
                    *query = candidates[chosen].name().to_string();
                    *current = 0;
                }
                Ok(Vec::new())
            }
            EditorCompletionAction::Choose(index) => {
                if index >= candidates.len() {
                    return Ok(vec![EditorEffect::Bell]);
                }
                if let Minibuffer::Command { selected, .. } = &mut view.minibuffer {
                    *selected = index;
                }
                self.submit_minibuffer(store, view)
            }
        }
    }

    /// Consume a non-text editing gesture while a minibuffer owns input. The
    /// query is intentionally append/backspace based for this milestone; the
    /// important contract is that navigation/delete/undo never leaks into bytes.
    pub(crate) fn minibuffer_blocks_document_input(
        &self,
        view: &EditorBufferView,
    ) -> Option<Vec<EditorEffect>> {
        view.minibuffer_active().then(|| {
            vec![EditorEffect::Status(
                "Minibuffer active — type, Backspace, Enter, or C-g".to_string(),
            )]
        })
    }

    fn update_incremental_search(
        &self,
        store: &DocumentStore,
        view: &mut EditorBufferView,
        after_current: bool,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let (query, origin) = match &view.minibuffer {
            Minibuffer::Search { query, origin } => (query.clone(), *origin),
            _ => return Ok(vec![EditorEffect::Bell]),
        };
        let snapshot = snapshot_for_view(store, view)?;
        let origin = clamp_to_char_boundary(&snapshot.text, origin.min(snapshot.text.len()));
        let start = if after_current && !query.is_empty() {
            clamp_to_char_boundary(
                &snapshot.text,
                view.primary_selection().head.min(snapshot.text.len()),
            )
        } else {
            origin
        };
        let found = if query.is_empty() {
            Some(origin..origin)
        } else {
            snapshot.text[start..]
                .find(&query)
                .map(|offset| start + offset..start + offset + query.len())
        };
        view.mark_active = false;
        if let Some(range) = found {
            view.selections = vec![Selection {
                anchor: range.start,
                head: range.end,
            }];
            view.primary = 0;
            return Ok(if query.is_empty() {
                Vec::new()
            } else {
                vec![EditorEffect::Status(format!("I-search: {query}"))]
            });
        }
        view.selections = vec![Selection::caret(origin)];
        view.primary = 0;
        Ok(vec![EditorEffect::Status(format!(
            "Failing I-search: {query}"
        ))])
    }

    pub(crate) fn handle_chord(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        chord: KeyChord,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        if chord == KeyChord::parse("C-g").expect("static chord") {
            view.cancel_transient();
            return Ok(vec![EditorEffect::Status("Quit".into())]);
        }
        if view.minibuffer_active() {
            if matches!(view.minibuffer, Minibuffer::Search { .. })
                && chord == KeyChord::parse("C-s").expect("static chord")
            {
                return self.update_incremental_search(store, view, true);
            }
            return Ok(vec![EditorEffect::Status(
                "Minibuffer active — Enter accepts; C-g cancels".to_string(),
            )]);
        }
        view.chord_prefix.push(chord);
        match self.keymap.resolve(&view.chord_prefix) {
            KeyResolution::Command(command) => {
                view.chord_prefix.clear();
                view.prefix_hud = None;
                self.execute(store, view, command)
            }
            KeyResolution::Prefix { display, reachable } => {
                view.prefix_hud = Some(format!("{display} — {}", reachable.join("  ")));
                Ok(Vec::new())
            }
            KeyResolution::Unbound => {
                view.chord_prefix.clear();
                view.prefix_hud = None;
                Ok(vec![EditorEffect::Bell])
            }
        }
    }

    pub(crate) fn execute(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        command: EditorCommand,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        if view.minibuffer_active() {
            if command == EditorCommand::Abort {
                view.cancel_transient();
                return Ok(vec![EditorEffect::Status("Quit".into())]);
            }
            if command == EditorCommand::IncrementalSearch
                && matches!(&view.minibuffer, Minibuffer::Search { .. })
            {
                return self.update_incremental_search(store, view, true);
            }
            return Ok(vec![EditorEffect::Status(
                "Minibuffer active — Enter accepts; C-g cancels".to_string(),
            )]);
        }
        if let Some(steps) = &mut self.recording
            && !self.playing_macro
            && !matches!(
                command,
                EditorCommand::StartMacro | EditorCommand::EndMacro | EditorCommand::PlayMacro
            )
        {
            steps.push(MacroStep::Command(command));
        }
        self.remember_command(command.name());

        match command {
            EditorCommand::MoveBackward => {
                let snapshot = snapshot_for_view(store, view)?;
                let count = view.take_count();
                let preserve_anchor = view.mark_active;
                for selection in &mut view.selections {
                    let mut at = selection.head;
                    for _ in 0..count {
                        at = previous_boundary(&snapshot.text, at);
                    }
                    selection.move_head(at, preserve_anchor);
                }
                view.desired_column = None;
                self.last_yank = None;
            }
            EditorCommand::MoveForward => {
                let snapshot = snapshot_for_view(store, view)?;
                let count = view.take_count();
                let preserve_anchor = view.mark_active;
                for selection in &mut view.selections {
                    let mut at = selection.head;
                    for _ in 0..count {
                        at = next_boundary(&snapshot.text, at);
                    }
                    selection.move_head(at, preserve_anchor);
                }
                view.desired_column = None;
                self.last_yank = None;
            }
            EditorCommand::MoveLineUp => {
                let snapshot = snapshot_for_view(store, view)?;
                let desired = view.desired_column.unwrap_or_else(|| {
                    let head = view.primary_selection().head;
                    let start = line_start(&snapshot.text, head);
                    editor_display_column(
                        &snapshot.text[start..line_end(&snapshot.text, head)],
                        head.saturating_sub(start),
                        0,
                    )
                });
                view.desired_column = Some(desired);
                let preserve_anchor = view.mark_active;
                for selection in &mut view.selections {
                    let position = previous_line_position(&snapshot.text, selection.head, desired);
                    selection.move_head(position, preserve_anchor);
                }
                self.last_yank = None;
            }
            EditorCommand::MoveLineDown => {
                let snapshot = snapshot_for_view(store, view)?;
                let desired = view.desired_column.unwrap_or_else(|| {
                    let head = view.primary_selection().head;
                    let start = line_start(&snapshot.text, head);
                    editor_display_column(
                        &snapshot.text[start..line_end(&snapshot.text, head)],
                        head.saturating_sub(start),
                        0,
                    )
                });
                view.desired_column = Some(desired);
                let preserve_anchor = view.mark_active;
                for selection in &mut view.selections {
                    let position = next_line_position(&snapshot.text, selection.head, desired);
                    selection.move_head(position, preserve_anchor);
                }
                self.last_yank = None;
            }
            EditorCommand::MoveLineStart => {
                let snapshot = snapshot_for_view(store, view)?;
                let preserve_anchor = view.mark_active;
                for selection in &mut view.selections {
                    let position = line_start(&snapshot.text, selection.head);
                    selection.move_head(position, preserve_anchor);
                }
                view.desired_column = None;
                self.last_yank = None;
            }
            EditorCommand::MoveLineEnd => {
                let snapshot = snapshot_for_view(store, view)?;
                let preserve_anchor = view.mark_active;
                for selection in &mut view.selections {
                    let position = line_end(&snapshot.text, selection.head);
                    selection.move_head(position, preserve_anchor);
                }
                view.desired_column = None;
                self.last_yank = None;
            }
            EditorCommand::MoveWordBackward | EditorCommand::MoveWordForward => {
                let snapshot = snapshot_for_view(store, view)?;
                let count = view.take_count();
                let preserve_anchor = view.mark_active;
                for selection in &mut view.selections {
                    let at = if command == EditorCommand::MoveWordBackward {
                        previous_word_boundary(&snapshot.text, selection.head, count)
                    } else {
                        let mut at = selection.head;
                        for _ in 0..count {
                            at = next_word_boundary(&snapshot.text, at);
                        }
                        at
                    };
                    selection.move_head(at, preserve_anchor);
                }
                view.desired_column = None;
                self.last_yank = None;
            }
            EditorCommand::DeleteBackward | EditorCommand::DeleteForward => {
                let snapshot = snapshot_for_view(store, view)?;
                let count = view.take_count();
                let mut edits = Vec::new();
                for selection in &view.selections {
                    let mut range = selection.range();
                    if range.is_empty() {
                        if command == EditorCommand::DeleteBackward {
                            for _ in 0..count {
                                range.start = previous_boundary(&snapshot.text, range.start);
                            }
                        } else {
                            for _ in 0..count {
                                range.end = next_boundary(&snapshot.text, range.end);
                            }
                        }
                    }
                    edits.push(TextEdit {
                        range,
                        insert: String::new(),
                    });
                }
                self.apply_recorded(store, view, normalize_edits(edits)?)?;
                self.last_yank = None;
            }
            EditorCommand::SetMark => {
                if view.selections.is_empty() {
                    view.selections.push(Selection::caret(0));
                }
                view.primary = view.primary.min(view.selections.len().saturating_sub(1));
                let mark = view.primary_selection().head;
                if let Some(selection) = view.selections.get_mut(view.primary) {
                    selection.anchor = mark;
                }
                view.mark_active = true;
                self.global_mark_ring.push_front(GlobalMark {
                    document: view.document,
                    offset: mark,
                });
                self.global_mark_ring.truncate(64);
                return Ok(vec![EditorEffect::Status("Mark set".into())]);
            }
            EditorCommand::KillRegion => {
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
                    return Ok(vec![EditorEffect::Bell]);
                }
                self.push_kill(killed);
                self.apply_recorded(store, view, normalize_edits(edits)?)?;
                self.last_yank = None;
            }
            EditorCommand::KillLine => {
                let snapshot = snapshot_for_view(store, view)?;
                let mut killed = String::new();
                let mut edits = Vec::new();
                for selection in &view.selections {
                    let start = selection.range().start;
                    let mut end = line_end(&snapshot.text, selection.range().end);
                    if end == start && end < snapshot.text.len() {
                        end = next_boundary(&snapshot.text, end);
                    }
                    if !killed.is_empty() {
                        killed.push('\n');
                    }
                    killed.push_str(&snapshot.text[start..end]);
                    edits.push(TextEdit {
                        range: start..end,
                        insert: String::new(),
                    });
                }
                self.push_kill(killed);
                self.apply_recorded(store, view, normalize_edits(edits)?)?;
                self.last_yank = None;
            }
            EditorCommand::Yank => {
                let text = self.kill_ring.front().cloned().ok_or(EditorError::NoKill)?;
                let edits = edits_for_selections(&view.selections, &text)?;
                let ranges = resulting_insert_ranges(&edits);
                let (seq, _) = self.apply_recorded(store, view, edits)?;
                self.last_yank = Some(YankRecord {
                    document: view.document,
                    seq,
                    ranges,
                    ring_index: 0,
                });
            }
            EditorCommand::YankPop => {
                let Some(previous) = self.last_yank.clone() else {
                    return Ok(vec![EditorEffect::Bell]);
                };
                let snapshot = snapshot_for_view(store, view)?;
                if previous.document != view.document || previous.seq != snapshot.seq {
                    self.last_yank = None;
                    return Ok(vec![EditorEffect::Bell]);
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
            }
            EditorCommand::Undo => self.undo(store, view)?,
            EditorCommand::Redo => self.redo(store, view)?,
            EditorCommand::Save => {
                let snapshot = snapshot_for_view(store, view)?;
                return Ok(vec![EditorEffect::SaveDocument {
                    document: view.document,
                    seq: snapshot.seq,
                }]);
            }
            EditorCommand::Abort => {
                view.cancel_transient();
                return Ok(vec![EditorEffect::Status("Quit".into())]);
            }
            EditorCommand::UniversalArgument => {
                view.prefix_argument = Some(view.prefix_argument.unwrap_or(1).saturating_mul(4));
                view.prefix_hud = Some(format!(
                    "C-u {} — awaiting command (C-g cancels)",
                    view.prefix_argument.unwrap_or(4)
                ));
            }
            EditorCommand::ExecuteCommand => {
                view.minibuffer = Minibuffer::Command {
                    query: String::new(),
                    selected: 0,
                };
                return Ok(vec![EditorEffect::ShowCommands]);
            }
            EditorCommand::IncrementalSearch => {
                view.mark_active = false;
                view.minibuffer = Minibuffer::Search {
                    query: String::new(),
                    origin: view.primary_selection().head,
                };
            }
            EditorCommand::GotoLine => {
                view.mark_active = false;
                view.minibuffer = Minibuffer::GotoLine {
                    query: String::new(),
                    origin: view.primary_selection().head,
                };
            }
            EditorCommand::SwitchBuffer => {
                view.minibuffer = Minibuffer::Buffer {
                    query: String::new(),
                };
                return Ok(Vec::new());
            }
            EditorCommand::RevertBuffer => {
                return Ok(vec![EditorEffect::RevertDocument {
                    document: view.document,
                }]);
            }
            EditorCommand::StartMacro => {
                self.recording = Some(Vec::new());
                return Ok(vec![EditorEffect::Status(
                    "Defining keyboard macro…".into(),
                )]);
            }
            EditorCommand::EndMacro => {
                if let Some(recording) = self.recording.take() {
                    self.last_macro = recording;
                }
                return Ok(vec![EditorEffect::Status("Keyboard macro defined".into())]);
            }
            EditorCommand::PlayMacro => self.play_macro(store, view)?,
        }
        Ok(Vec::new())
    }

    fn apply_recorded(
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
        self.last_commit = Some((view.document, seq, deltas.clone()));
        let history = self.histories.entry(view.document).or_default();
        history.undo.push(HistoryFrame {
            expected_seq: seq,
            undo: inverse,
            redo,
        });
        history.redo.clear();
        Ok((seq, deltas))
    }

    fn undo(
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
        self.last_commit = Some((view.document, seq, deltas.clone()));
        self.histories
            .entry(view.document)
            .or_default()
            .redo
            .push(HistoryFrame {
                expected_seq: seq,
                ..frame
            });
        self.last_yank = None;
        Ok(())
    }

    fn redo(
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
        self.last_commit = Some((view.document, seq, deltas.clone()));
        self.histories
            .entry(view.document)
            .or_default()
            .undo
            .push(HistoryFrame {
                expected_seq: seq,
                ..frame
            });
        self.last_yank = None;
        Ok(())
    }

    fn play_macro(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
    ) -> Result<(), EditorError> {
        if self.playing_macro {
            return Err(EditorError::MacroRecursion);
        }
        self.playing_macro = true;
        let steps = self.last_macro.clone();
        let result = (|| {
            for step in steps {
                match step {
                    MacroStep::Command(command) => {
                        self.execute(store, view, command)?;
                    }
                    MacroStep::Text(text) => {
                        self.insert_text(store, view, &text)?;
                    }
                }
            }
            Ok(())
        })();
        self.playing_macro = false;
        result
    }

    fn push_kill(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.kill_ring.push_front(text);
        self.kill_ring.truncate(KILL_RING_LIMIT);
    }

    fn remember_command(&mut self, command: &str) {
        if self
            .command_history
            .back()
            .is_some_and(|last| last == command)
        {
            return;
        }
        self.command_history.push_back(command.to_owned());
        while self.command_history.len() > COMMAND_HISTORY_LIMIT {
            self.command_history.pop_front();
        }
    }
}

fn snapshot_for_view(
    store: &DocumentStore,
    view: &EditorBufferView,
) -> Result<crate::document_store::DocumentSnapshot, EditorError> {
    let snapshot = store
        .snapshot(view.document)
        .ok_or(EditorError::UnknownDocument)?;
    if snapshot.seq != view.anchor_seq {
        return Err(EditorError::StaleView {
            view: view.anchor_seq,
            current: snapshot.seq,
        });
    }
    Ok(snapshot)
}

/// Append as much as fits while preserving UTF-8 addressability. Returns true
/// only when the complete input was accepted.
fn push_bounded(target: &mut String, input: &str, limit: usize) -> bool {
    let remaining = limit.saturating_sub(target.len());
    if input.len() <= remaining {
        target.push_str(input);
        return true;
    }
    let mut end = remaining.min(input.len());
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    target.push_str(&input[..end]);
    false
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

fn edits_for_selections(
    selections: &[Selection],
    insert: &str,
) -> Result<Vec<TextEdit>, EditorError> {
    normalize_edits(
        selections
            .iter()
            .map(|selection| TextEdit {
                range: selection.range(),
                insert: insert.to_owned(),
            })
            .collect(),
    )
}

fn normalize_edits(mut edits: Vec<TextEdit>) -> Result<Vec<TextEdit>, EditorError> {
    edits.sort_by_key(|edit| (edit.range.start, edit.range.end));
    edits.dedup_by(|right, left| right.range == left.range && right.insert == left.insert);
    if edits
        .windows(2)
        .any(|pair| pair[0].range.end > pair[1].range.start)
    {
        return Err(EditorError::InvalidSelections);
    }
    Ok(edits)
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
        growth = growth.saturating_add(edit.insert.len() as isize - removed as isize);
    }
    Ok(inverse)
}

fn previous_boundary(text: &str, position: usize) -> usize {
    let position = position.min(text.len());
    text[..position]
        .grapheme_indices()
        .map(|(at, _)| at)
        .last()
        .unwrap_or(0)
}

fn next_boundary(text: &str, position: usize) -> usize {
    let position = position.min(text.len());
    if position == text.len() {
        return position;
    }
    position.saturating_add(text[position..].graphemes().next().map_or(0, str::len))
}

fn editor_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

fn previous_word_boundary(text: &str, position: usize, count: usize) -> usize {
    let position = clamp_to_char_boundary(text, position.min(text.len()));
    let count = count.clamp(1, 10_000);
    let mut recent_starts = VecDeque::with_capacity(count);
    let mut in_word = false;
    for (byte, grapheme) in text.grapheme_indices() {
        if byte.saturating_add(grapheme.len()) > position {
            break;
        }
        let is_word = grapheme.chars().next().is_some_and(editor_word_character);
        if is_word && !in_word {
            if recent_starts.len() == count {
                recent_starts.pop_front();
            }
            recent_starts.push_back(byte);
        }
        in_word = is_word;
    }
    if recent_starts.len() == count {
        recent_starts.front().copied().unwrap_or(0)
    } else {
        0
    }
}

fn next_word_boundary(text: &str, position: usize) -> usize {
    let mut at = clamp_to_char_boundary(text, position.min(text.len()));
    while at < text.len() {
        let next = next_boundary(text, at);
        let character = text[at..next].chars().next().unwrap_or(' ');
        if editor_word_character(character) {
            break;
        }
        at = next;
    }
    while at < text.len() {
        let next = next_boundary(text, at);
        let character = text[at..next].chars().next().unwrap_or(' ');
        if !editor_word_character(character) {
            break;
        }
        at = next;
    }
    at
}

fn line_start(text: &str, position: usize) -> usize {
    text[..position.min(text.len())]
        .rfind('\n')
        .map_or(0, |at| at.saturating_add(1))
}

fn line_end(text: &str, position: usize) -> usize {
    let position = position.min(text.len());
    text[position..]
        .find('\n')
        .map_or(text.len(), |relative| position.saturating_add(relative))
}

fn previous_line_position(text: &str, position: usize, desired_column: usize) -> usize {
    let position = position.min(text.len());
    let current_start = line_start(text, position);
    if current_start == 0 {
        return position;
    }
    let previous_end = current_start.saturating_sub(1);
    let previous_start = line_start(text, previous_end);
    byte_at_editor_display_column(text, previous_start, previous_end, desired_column)
}

fn next_line_position(text: &str, position: usize, desired_column: usize) -> usize {
    let position = position.min(text.len());
    let current_end = line_end(text, position);
    if current_end >= text.len() {
        return position;
    }
    let next_start = current_end.saturating_add(1);
    let next_end = line_end(text, next_start);
    byte_at_editor_display_column(text, next_start, next_end, desired_column)
}

fn clamp_to_char_boundary(text: &str, mut position: usize) -> usize {
    position = position.min(text.len());
    while position > 0 && !text.is_char_boundary(position) {
        position -= 1;
    }
    position
}

/// Clamp an externally supplied editor position to the preceding user-visible
/// character boundary. Pointer mapping already emits these boundaries; keeping
/// the reducer seam defensive prevents accessibility or a future adapter from
/// installing a caret between a base, combining mark, emoji joiner, or flag RI.
fn clamp_to_grapheme_boundary(text: &str, position: usize) -> usize {
    let position = clamp_to_char_boundary(text, position);
    if position == 0 || position == text.len() {
        return position;
    }
    let start = line_start(text, position);
    let end = line_end(text, position);
    // Include the line-feed in segmentation when one exists. Its grapheme
    // start proves an ordinary LF line-end is addressable, while UAX #29 keeps
    // CRLF together and therefore retreats a position between CR and LF to the
    // start of that pair.
    let scan_end = end.saturating_add(usize::from(end < text.len()));
    text[start..scan_end]
        .grapheme_indices()
        .map(|(offset, _)| start.saturating_add(offset))
        .take_while(|boundary| *boundary <= position)
        .last()
        .unwrap_or(start)
}

fn byte_at_editor_display_column(text: &str, start: usize, end: usize, target: usize) -> usize {
    let mut column = 0usize;
    let mut byte = start;
    for (offset, grapheme) in text[start..end].grapheme_indices() {
        let width = editor_grapheme_columns(grapheme, column);
        if column.saturating_add(width) > target {
            break;
        }
        column = column.saturating_add(width);
        byte = start.saturating_add(offset).saturating_add(grapheme.len());
    }
    byte
}

fn line_number(text: &str, position: usize) -> usize {
    text.as_bytes()[..position.min(text.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
}

fn byte_of_line(text: &str, line: usize) -> usize {
    if line == 0 {
        return 0;
    }
    text.as_bytes()
        .iter()
        .enumerate()
        .filter_map(|(index, byte)| (*byte == b'\n').then_some(index + 1))
        .nth(line - 1)
        .unwrap_or(text.len())
}

/// A selected byte span within one projected editor line. `bytes` is relative
/// to [`EditorViewportLine::source`]; `continues` means the selection also owns
/// the line break (or later text) and therefore paints through one trailing
/// cell even when the visible line itself is empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditorSelectionSpan {
    pub(crate) bytes: Range<usize>,
    pub(crate) continues: bool,
    pub(crate) primary: bool,
}

/// One bounded, source-addressable line prepared for native editor paint.
/// Keeping source byte ranges beside the display text lets pointer and
/// accessibility adapters map back to the canonical document without a second
/// independently wrapped representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorSyntaxClass {
    Table,
    Key,
    String,
    Number,
    Boolean,
    Comment,
}

/// One UTF-8-safe syntax run relative to [`EditorViewportLine::text`]. Language
/// services prepare these runs before paint; the renderer only lowers the
/// already-bounded visible projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditorSyntaxSpan {
    pub(crate) bytes: Range<usize>,
    pub(crate) class: EditorSyntaxClass,
}

/// One UTF-8-safe diagnostic underline relative to the visible line. The
/// human-readable diagnostic remains in the editor modeline/assist projection;
/// paint needs only severity and source cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditorDiagnosticSpan {
    pub(crate) bytes: Range<usize>,
    pub(crate) error: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditorViewportLine {
    pub(crate) number: usize,
    pub(crate) source: Range<usize>,
    /// Display column of `source.start` in the unsliced logical line. Paint and
    /// pointer mapping seed tab stops from this shared phase after horizontal
    /// caret reveal shifts the source window.
    pub(crate) column_start: usize,
    pub(crate) text: String,
    pub(crate) selections: Vec<EditorSelectionSpan>,
    pub(crate) carets: Vec<(usize, bool)>,
    pub(crate) syntax: Vec<EditorSyntaxSpan>,
    pub(crate) diagnostics: Vec<EditorDiagnosticSpan>,
}

/// Visible-only editor projection. Large documents never copy or lay out more
/// than the requested bounded line window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EditorViewportProjection {
    pub(crate) first_line: usize,
    pub(crate) total_lines: usize,
    pub(crate) lines: Vec<EditorViewportLine>,
}

/// Project a stable, line-aligned editor viewport from canonical document bytes.
///
/// The returned text and byte coordinates are UTF-8 safe. A trailing newline
/// produces its real final empty line, and a selection crossing a newline keeps
/// enough information to paint the selected end-of-line cell. Work is capped so
/// a corrupt geometry value cannot turn one frame into whole-document layout.
pub(crate) fn project_viewport(
    text: &str,
    view: &EditorBufferView,
    requested_lines: usize,
    requested_columns: usize,
) -> EditorViewportProjection {
    const MAX_PROJECTED_LINES: usize = 512;
    const MAX_PROJECTED_LINE_BYTES: usize = 32 * 1024;
    const MAX_PROJECTED_TOTAL_BYTES: usize = 512 * 1024;

    let anchor = line_start(text, clamp_to_char_boundary(text, view.viewport_anchor));
    let first_line = line_number(text, anchor);
    let total_lines = text.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let line_limit = requested_lines.clamp(1, MAX_PROJECTED_LINES);
    let column_limit = requested_columns.clamp(4, 4_096);
    let primary_caret = clamp_to_char_boundary(text, view.primary_selection().head);
    let primary_line_start = line_start(text, primary_caret);
    let horizontal_anchor = horizontal_window_anchor(
        text,
        primary_line_start,
        primary_caret,
        MAX_PROJECTED_LINE_BYTES,
        column_limit,
    );
    let mut lines = Vec::with_capacity(line_limit.min(total_lines.saturating_sub(first_line)));
    let mut start = anchor;
    let mut number = first_line;
    let mut projected_bytes = 0usize;

    for _ in 0..line_limit {
        let full_end = line_end(text, start);
        let remaining = MAX_PROJECTED_TOTAL_BYTES.saturating_sub(projected_bytes);
        if remaining == 0 && !lines.is_empty() {
            break;
        }
        let budget = remaining.clamp(1, MAX_PROJECTED_LINE_BYTES);
        let display = bounded_line_window(
            text,
            start,
            full_end,
            horizontal_anchor,
            budget,
            column_limit,
        );
        let display_start = display.source.start;
        let display_end = display.source.end;
        let mut selections = Vec::new();
        let mut carets = Vec::new();
        for (index, selection) in view.selections.iter().enumerate() {
            let primary = index == view.primary;
            let range = selection.range();
            if !range.is_empty() && range.start <= display_end && range.end > display_start {
                let selected_start = range.start.max(display_start).min(display_end);
                let selected_end = range.end.min(display_end).max(selected_start);
                selections.push(EditorSelectionSpan {
                    bytes: selected_start.saturating_sub(display_start)
                        ..selected_end.saturating_sub(display_start),
                    continues: range.end > display_end,
                    primary,
                });
            }
            if selection.head >= display_start && selection.head <= display_end {
                carets.push((selection.head.saturating_sub(display_start), primary));
            }
        }
        let line_text = text[display.source.clone()].to_string();
        projected_bytes = projected_bytes.saturating_add(line_text.len());
        lines.push(EditorViewportLine {
            number,
            source: display.source,
            column_start: display.column_start,
            text: line_text,
            selections,
            carets,
            syntax: Vec::new(),
            diagnostics: Vec::new(),
        });

        if full_end == text.len() {
            break;
        }
        start = full_end.saturating_add(1);
        number = number.saturating_add(1);
    }

    EditorViewportProjection {
        first_line,
        total_lines,
        lines,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HorizontalWindowAnchor {
    /// Exact display-column offset for ordinary lines. Every visible row uses
    /// the same offset, so vertical movement keeps columns visually aligned.
    Columns(usize),
    /// Bounded fallback for a pathological line whose prefix alone exceeds the
    /// per-line projection budget. The byte offset remains UTF-8-clamped per row
    /// and, crucially, keeps the primary caret in the retained source band.
    ByteOffset {
        line_start: usize,
        window_start: usize,
    },
}

/// Display cells occupied by one complete editor grapheme at `column`.
///
/// Tabs retain the editor's canonical four-cell phase. Every other cluster
/// uses the same Unicode width authority as the terminal, including CJK,
/// combining sequences, flags, and ZWJ emoji. This function is crate-visible
/// so native UI hit testing and paint geometry cannot drift from projection.
pub(crate) fn editor_grapheme_columns(grapheme: &str, column: usize) -> usize {
    if grapheme == "\t" {
        4 - column % 4
    } else if grapheme.chars().any(char::is_control) {
        // The editor suppresses control clusters rather than asking the font
        // rasterizer to render a terminal-style replacement cell. Geometry
        // must make the same choice or projection, hit testing, and paint
        // disagree (and a control-only line defeats the visible-cell bound).
        0
    } else {
        grapheme_display_width(grapheme)
    }
}

/// Display-cell offset of the complete graphemes ending at or before `byte`.
/// `column_start` supplies tab phase for a horizontally projected row; the
/// returned value is relative to that origin.
pub(crate) fn editor_display_column(text: &str, byte: usize, column_start: usize) -> usize {
    let requested = clamp_to_char_boundary(text, byte.min(text.len()));
    let mut column = column_start;
    for (offset, grapheme) in text.grapheme_indices() {
        let end = offset.saturating_add(grapheme.len());
        if end > requested {
            break;
        }
        column = column.saturating_add(editor_grapheme_columns(grapheme, column));
    }
    column.saturating_sub(column_start)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HorizontalLineWindow {
    source: Range<usize>,
    column_start: usize,
}

fn horizontal_window_anchor(
    text: &str,
    line_start: usize,
    caret: usize,
    byte_budget: usize,
    column_budget: usize,
) -> HorizontalWindowAnchor {
    let left_target = column_budget.saturating_mul(3) / 4;
    if caret.saturating_sub(line_start) <= byte_budget {
        let mut columns = 0usize;
        let end = line_end(text, caret);
        for (offset, grapheme) in text[line_start..end].grapheme_indices() {
            if line_start
                .saturating_add(offset)
                .saturating_add(grapheme.len())
                > caret
            {
                break;
            }
            columns = columns.saturating_add(editor_grapheme_columns(grapheme, columns));
        }
        return HorizontalWindowAnchor::Columns(columns.saturating_sub(left_target));
    }

    let search_start =
        clamp_to_char_boundary(text, caret.saturating_sub(byte_budget).max(line_start));
    let mut window_start = caret;
    let mut left_columns = 0usize;
    // The workspace grapheme iterator is forward-only. Materialize at most the
    // already-bounded 32 KiB fallback window, then walk it backwards. Do not
    // select the artificial first cluster when the scan begins mid-line: it
    // may be the suffix of a pathological cluster that started before the
    // byte budget. If no later boundary exists, showing from the caret is the
    // bounded fail-safe and never emits a partial grapheme.
    let graphemes = text[search_start..caret]
        .grapheme_indices()
        .collect::<Vec<_>>();
    for (relative, grapheme) in graphemes.into_iter().rev() {
        if relative == 0 && search_start > line_start {
            break;
        }
        // Reverse traversal cannot recover the absolute phase of an earlier
        // tab without scanning the unbounded prefix. Four is its conservative
        // maximum; the exact forward pass begins again at the projected origin.
        let columns = if grapheme == "\t" {
            4
        } else {
            editor_grapheme_columns(grapheme, 0)
        };
        if left_columns.saturating_add(columns) > left_target {
            break;
        }
        window_start = search_start.saturating_add(relative);
        left_columns = left_columns.saturating_add(columns);
    }
    HorizontalWindowAnchor::ByteOffset {
        line_start,
        window_start,
    }
}

/// Choose one UTF-8-safe, width-bounded slice using the workspace's shared
/// horizontal anchor. Source ranges remain canonical: no visual ellipsis bytes
/// are injected, so pointer, selection, IME, and accessibility all map through
/// the exact text that paint receives.
fn bounded_line_window(
    text: &str,
    start: usize,
    end: usize,
    anchor: HorizontalWindowAnchor,
    byte_budget: usize,
    column_budget: usize,
) -> HorizontalLineWindow {
    let byte_budget = byte_budget.max(1);
    let column_budget = column_budget.max(1);
    let (window_start, column_start) = match anchor {
        HorizontalWindowAnchor::Columns(target) => {
            let mut at = start;
            let mut columns = 0usize;
            for (relative, grapheme) in text[start..end].grapheme_indices() {
                if columns >= target {
                    break;
                }
                let grapheme_columns = editor_grapheme_columns(grapheme, columns);
                if columns.saturating_add(grapheme_columns) > target {
                    break;
                }
                columns = columns.saturating_add(grapheme_columns);
                at = start
                    .saturating_add(relative)
                    .saturating_add(grapheme.len());
            }
            (at, columns)
        }
        HorizontalWindowAnchor::ByteOffset {
            line_start,
            window_start,
        } => {
            // Only the pathological primary line owns a proven bounded
            // grapheme start. Other visible rows begin at their real line
            // boundary rather than reusing a byte offset that could bisect a
            // different cluster (and would require scanning an unbounded
            // prefix merely to recover tab phase).
            if start == line_start {
                (window_start.clamp(start, end), 0)
            } else {
                (start, 0)
            }
        }
    };

    let mut window_end = window_start;
    let mut columns = column_start;
    for (relative, grapheme) in text[window_start..end].grapheme_indices() {
        let next = window_start
            .saturating_add(relative)
            .saturating_add(grapheme.len());
        if next.saturating_sub(window_start) > byte_budget {
            break;
        }
        let grapheme_columns = editor_grapheme_columns(grapheme, columns);
        if columns
            .saturating_add(grapheme_columns)
            .saturating_sub(column_start)
            > column_budget
            && window_end > window_start
        {
            break;
        }
        columns = columns.saturating_add(grapheme_columns);
        window_end = next;
        if columns.saturating_sub(column_start) >= column_budget {
            break;
        }
    }
    if window_end == window_start && window_start < end {
        window_end = text[window_start..end]
            .graphemes()
            .next()
            .filter(|grapheme| grapheme.len() <= byte_budget)
            .map_or(window_start, |grapheme| {
                window_start.saturating_add(grapheme.len()).min(end)
            });
    }
    HorizontalLineWindow {
        source: window_start..window_end,
        column_start,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct KeyChord {
    pub(crate) control: bool,
    pub(crate) meta: bool,
    pub(crate) shift: bool,
    pub(crate) key: String,
}

impl KeyChord {
    pub(crate) fn parse(source: &str) -> Option<Self> {
        let mut control = false;
        let mut meta = false;
        let mut shift = false;
        let mut parts = source.split('-').peekable();
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                return (!part.is_empty()).then(|| Self {
                    control,
                    meta,
                    shift,
                    key: part.to_ascii_lowercase(),
                });
            }
            match part {
                "C" => control = true,
                "M" => meta = true,
                "S" => shift = true,
                _ => return None,
            }
        }
        None
    }

    fn display(&self) -> String {
        let mut out = String::new();
        if self.control {
            out.push_str("C-");
        }
        if self.meta {
            out.push_str("M-");
        }
        if self.shift {
            out.push_str("S-");
        }
        out.push_str(&self.key);
        out
    }
}

#[derive(Clone, Debug, Default)]
struct KeyNode {
    command: Option<EditorCommand>,
    children: BTreeMap<KeyChord, KeyNode>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Keymap {
    root: KeyNode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeyResolution {
    Command(EditorCommand),
    Prefix {
        display: String,
        reachable: Vec<String>,
    },
    Unbound,
}

impl Keymap {
    pub(crate) fn emacs() -> Self {
        let mut map = Self::default();
        let bindings = [
            (&["C-b"][..], EditorCommand::MoveBackward),
            (&["C-f"][..], EditorCommand::MoveForward),
            (&["C-p"][..], EditorCommand::MoveLineUp),
            (&["C-n"][..], EditorCommand::MoveLineDown),
            (&["C-a"][..], EditorCommand::MoveLineStart),
            (&["C-e"][..], EditorCommand::MoveLineEnd),
            // Both spellings of word motion: the emacs one and the macOS one.
            // ⌥←/⌥→ are what a Mac user reaches for; `M-b`/`M-f` are what an
            // emacs user reaches for. Binding only the latter made the arrows
            // silently dead.
            (&["M-b", "M-left"][..], EditorCommand::MoveWordBackward),
            (&["M-f", "M-right"][..], EditorCommand::MoveWordForward),
            (&["Backspace"][..], EditorCommand::DeleteBackward),
            (&["C-d"][..], EditorCommand::DeleteForward),
            (&["C-Space"][..], EditorCommand::SetMark),
            (&["C-w"][..], EditorCommand::KillRegion),
            (&["C-k"][..], EditorCommand::KillLine),
            (&["C-y"][..], EditorCommand::Yank),
            (&["M-y"][..], EditorCommand::YankPop),
            (&["C-/"][..], EditorCommand::Undo),
            (&["C-x", "u"][..], EditorCommand::Undo),
            (&["C-x", "C-s"][..], EditorCommand::Save),
            (&["C-x", "b"][..], EditorCommand::SwitchBuffer),
            (&["C-x", "("][..], EditorCommand::StartMacro),
            (&["C-x", ")"][..], EditorCommand::EndMacro),
            (&["C-x", "e"][..], EditorCommand::PlayMacro),
            (&["C-u"][..], EditorCommand::UniversalArgument),
            (&["M-x"][..], EditorCommand::ExecuteCommand),
            (&["C-s"][..], EditorCommand::IncrementalSearch),
            (&["M-g", "g"][..], EditorCommand::GotoLine),
            (&["C-g"][..], EditorCommand::Abort),
        ];
        for (sequence, command) in bindings {
            map.bind(sequence, command)
                .expect("built-in editor key sequence is valid");
        }
        map
    }

    pub(crate) fn bind(
        &mut self,
        sequence: &[&str],
        command: EditorCommand,
    ) -> Result<(), &'static str> {
        if sequence.is_empty() {
            return Err("empty key sequence");
        }
        let mut node = &mut self.root;
        for source in sequence {
            let chord = KeyChord::parse(source).ok_or("invalid chord")?;
            node = node.children.entry(chord).or_default();
        }
        node.command = Some(command);
        Ok(())
    }

    pub(crate) fn resolve(&self, sequence: &[KeyChord]) -> KeyResolution {
        let mut node = &self.root;
        for chord in sequence {
            let Some(next) = node.children.get(chord) else {
                return KeyResolution::Unbound;
            };
            node = next;
        }
        if let Some(command) = &node.command {
            return KeyResolution::Command(*command);
        }
        if node.children.is_empty() {
            return KeyResolution::Unbound;
        }
        KeyResolution::Prefix {
            display: sequence
                .iter()
                .map(KeyChord::display)
                .collect::<Vec<_>>()
                .join(" "),
            reachable: node
                .children
                .iter()
                .map(|(chord, child)| {
                    let suffix = child.command.as_ref().map_or("prefix", EditorCommand::name);
                    format!("{}  {suffix}", chord.display())
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_spec::derive::{
        Model, native_editor_command_palette_model, native_editor_viewport_model,
    };
    use aterm_spec::interp::{State, admits};

    fn editor(text: &str) -> (DocumentStore, EditorWorkspace, EditorBufferView, DocumentId) {
        let mut store = DocumentStore::new();
        let document = store.open("mem://editor".into(), text.into());
        let mut workspace = EditorWorkspace::new();
        let view = workspace
            .attach(&mut store, document, DocumentViewId(91))
            .unwrap();
        (store, workspace, view, document)
    }

    fn palette_projection(
        model: &Model,
        view: &EditorBufferView,
        query_phase: i64,
        query_changed: i64,
        submitted: i64,
        exact_selected_dispatch: i64,
    ) -> State {
        let mut state = model.init_state();
        match &view.minibuffer {
            Minibuffer::Command { query, selected } => {
                state.insert("mode", 1);
                state.insert("query_phase", query_phase);
                state.insert(
                    "results",
                    i64::try_from(command_completions(query).len().min(4))
                        .expect("bounded completion count"),
                );
                state.insert(
                    "selected",
                    i64::try_from(*selected).expect("bounded selection"),
                );
            }
            _ => {
                state.insert("mode", 0);
                state.insert("query_phase", 0);
                state.insert("results", 0);
                state.insert("selected", 0);
            }
        }
        state.insert("query_changed", query_changed);
        state.insert("submitted", submitted);
        state.insert("exact_selected_dispatch", exact_selected_dispatch);
        state
    }

    fn assert_palette_transition(
        model: &Model,
        before: &State,
        after: &State,
        action: &'static str,
    ) {
        assert_eq!(admits(model, before, after), Some(action));
        for invariant in &model.invariants {
            assert!(model.check_invariant(invariant.name, after));
        }
    }

    #[test]
    fn multi_selection_edit_is_one_document_transaction() {
        let (mut store, mut workspace, mut view, document) = editor("one two");
        let before = store.snapshot(document).unwrap().seq;
        view.selections = vec![Selection::caret(0), Selection::caret(4)];
        workspace.insert_text(&mut store, &mut view, "_").unwrap();
        let snapshot = store.snapshot(document).unwrap();
        assert_eq!(snapshot.seq.0, before.0 + 1);
        assert_eq!(snapshot.text.as_ref(), "_one _two");
        assert_eq!(
            view.selections,
            vec![Selection::caret(1), Selection::caret(6)]
        );
    }

    #[test]
    fn stale_view_changes_neither_text_nor_selection() {
        let (mut store, mut workspace, mut view, document) = editor("abc");
        let base = store.snapshot(document).unwrap().seq;
        assert!(matches!(
            store.transact(
                document,
                base,
                vec![TextEdit {
                    range: 0..0,
                    insert: "x".into(),
                }]
            ),
            DocumentTxnOutcome::Committed { .. }
        ));
        let before_selection = view.selections.clone();
        assert!(matches!(
            workspace.insert_text(&mut store, &mut view, "y"),
            Err(EditorError::StaleView { .. })
        ));
        assert_eq!(view.selections, before_selection);
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "xabc");
    }

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
    fn prefix_trie_never_times_out_and_c_g_cancels() {
        let (mut store, mut workspace, mut view, _) = editor("abc");
        workspace
            .handle_chord(&mut store, &mut view, KeyChord::parse("C-x").unwrap())
            .unwrap();
        assert!(view.prefix_hud.as_deref().unwrap().contains("C-s"));
        workspace
            .handle_chord(&mut store, &mut view, KeyChord::parse("C-g").unwrap())
            .unwrap();
        assert_eq!(view.prefix_hud, None);
        assert!(matches!(view.minibuffer, Minibuffer::Inactive));
    }

    #[test]
    fn vertical_motion_preserves_a_bounded_text_column() {
        let (mut store, mut workspace, mut view, _) = editor("alpha\nβ\ngamma");
        view.selections = vec![Selection::caret(4)];
        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveLineDown)
            .unwrap();
        // The shorter UTF-8 line clamps to its final boundary, never into β.
        assert_eq!(view.primary_selection().head, "alpha\nβ".len());
        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveLineDown)
            .unwrap();
        // The original desired column survives the short middle line.
        assert_eq!(view.primary_selection().head, "alpha\nβ\ngamm".len());
        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveLineUp)
            .unwrap();
        assert_eq!(view.primary_selection().head, "alpha\nβ".len());
    }

    #[test]
    fn vertical_motion_preserves_display_cells_across_zwj_and_cjk_lines() {
        let text = "abcd\n👩‍💻xyz\n中xyz";
        let (mut store, mut workspace, mut view, _) = editor(text);
        view.selections = vec![Selection::caret("abcd".len())];

        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveLineDown)
            .unwrap();
        let emoji_target = "abcd\n".len() + "👩‍💻xy".len();
        assert_eq!(view.primary_selection().head, emoji_target);
        assert!(
            text.grapheme_indices()
                .any(|(byte, _)| byte == emoji_target)
        );

        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveLineDown)
            .unwrap();
        let cjk_target = "abcd\n👩‍💻xyz\n".len() + "中xy".len();
        assert_eq!(view.primary_selection().head, cjk_target);
        assert!(text.grapheme_indices().any(|(byte, _)| byte == cjk_target));

        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveLineUp)
            .unwrap();
        assert_eq!(view.primary_selection().head, emoji_target);
    }

    #[test]
    fn editor_cell_widths_share_tab_unicode_and_suppressed_control_semantics() {
        assert_eq!(editor_grapheme_columns("\t", 0), 4);
        assert_eq!(editor_grapheme_columns("\t", 3), 1);
        assert_eq!(editor_grapheme_columns("e\u{301}", 0), 1);
        assert_eq!(editor_grapheme_columns("中", 0), 2);
        assert_eq!(editor_grapheme_columns("👩‍💻", 0), 2);
        assert_eq!(editor_grapheme_columns("\u{0001}", 17), 0);
    }

    #[test]
    fn viewport_scroll_and_caret_reveal_use_stable_line_anchors() {
        let text = (0..100)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let (_, _, mut view, _) = editor(&text);
        view.scroll_lines(&text, 40);
        assert_eq!(&text[view.viewport_anchor..][..8], "line 40\n");
        view.selections = vec![Selection::caret(byte_of_line(&text, 90))];
        view.ensure_primary_visible(&text, 20);
        assert_eq!(line_number(&text, view.viewport_anchor), 73);
        view.selections = vec![Selection::caret(byte_of_line(&text, 5))];
        view.ensure_primary_visible(&text, 20);
        assert_eq!(line_number(&text, view.viewport_anchor), 5);
    }

    #[test]
    fn viewport_scroll_stores_only_presentable_anchors_at_every_capacity() {
        let text = (0..12)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let (_, _, mut view, _) = editor(&text);
        view.reconcile_viewport(&text, 4);

        view.scroll_lines(&text, i32::MAX);
        assert_eq!(line_number(&text, view.viewport_anchor), 9);
        let bottom = view.viewport_anchor;
        view.scroll_lines(&text, -1);
        assert_eq!(line_number(&text, view.viewport_anchor), 8);
        assert!(view.viewport_anchor < bottom, "one reverse step moves now");

        // Short and empty documents have no scrollable full-viewport anchor.
        for short in ["", "one", "one\ntwo"] {
            let (_, _, mut short_view, _) = editor(short);
            short_view.reconcile_viewport(short, 40);
            short_view.scroll_lines(short, i32::MAX);
            assert_eq!(short_view.viewport_anchor, 0);
            short_view.scroll_lines(short, i32::MIN);
            assert_eq!(short_view.viewport_anchor, 0);
        }

        // A view may receive wheel input before its first geometry reconcile.
        // Zero renderer capacity is normalized to one row, never underflowed.
        let (_, _, mut zero_view, _) = editor(&text);
        zero_view.viewport_lines = 0;
        zero_view.scroll_lines(&text, i32::MAX);
        assert_eq!(line_number(&text, zero_view.viewport_anchor), 12);
        zero_view.scroll_lines(&text, -1);
        assert_eq!(line_number(&text, zero_view.viewport_anchor), 11);
    }

    #[test]
    fn short_replacement_documents_clamp_the_viewport_to_the_first_line() {
        for text in ["# Manual\nfont_px = [ \n", "# Manual\nfont_px = 14\n"] {
            let (_, _, mut view, _) = editor(text);
            // External replacement maps a former tail caret and scroll anchor
            // to the new EOF. The renderer has much more room than this file.
            view.viewport_anchor = text.len();
            view.selections = vec![Selection::caret(text.len())];

            assert!(view.reconcile_viewport(text, 40));
            assert_eq!(line_number(text, view.viewport_anchor), 0);
            let projection = project_viewport(text, &view, 40, usize::MAX);
            assert_eq!(projection.first_line, 0);
            assert_eq!(projection.total_lines, 3);
            assert_eq!(projection.lines.len(), 3);
        }
    }

    #[test]
    fn real_viewport_reconciliation_conforms_to_renderer_capacity_model() {
        let text = (0..30)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let (_, _, mut view, _) = editor(&text);
        view.selections = vec![Selection::caret(byte_of_line(&text, 20))];
        let short_text = "# Manual\nfont_px = 14\n";
        let (_, _, mut short_view, _) = editor(short_text);
        short_view.viewport_anchor = short_text.len();
        short_view.selections = vec![Selection::caret(short_text.len())];

        let model = native_editor_viewport_model();
        let before = model.init_state();
        assert!(view.reconcile_viewport(&text, 8));
        assert!(short_view.reconcile_viewport(short_text, 40));
        let mut after = before.clone();
        after.insert(
            "anchor_line",
            line_number(&text, view.viewport_anchor) as i64,
        );
        after.insert("visible_lines", view.viewport_lines() as i64);
        after.insert(
            "short_anchor_line",
            line_number(short_text, short_view.viewport_anchor) as i64,
        );
        after.insert("short_visible_lines", short_view.viewport_lines() as i64);
        after.insert("resized", 1);
        assert_eq!(admits(&model, &before, &after), Some("Resize"));
        assert!(model.check_invariant("CaretVisibleAfterResize", &after));
        assert!(model.check_invariant("ShortDocumentFullyVisible", &after));

        // The shipping scroll transition stores the same bottom anchor paint
        // presents, then makes immediate progress on one reverse input.
        let scroll_text = (0..12)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let (_, _, mut scroll_view, _) = editor(&scroll_text);
        scroll_view.reconcile_viewport(&scroll_text, 4);
        scroll_view.scroll_lines(&scroll_text, i32::MAX);
        let scroll_before = model.init_state();
        let mut at_bottom = scroll_before.clone();
        at_bottom.insert(
            "scroll_anchor_line",
            line_number(&scroll_text, scroll_view.viewport_anchor) as i64,
        );
        at_bottom.insert("scroll_phase", 1);
        assert_eq!(
            admits(&model, &scroll_before, &at_bottom),
            Some("Overscroll")
        );
        assert!(model.check_invariant("StoredScrollAnchorPresentable", &at_bottom));
        scroll_view.scroll_lines(&scroll_text, -1);
        let mut one_line_up = at_bottom.clone();
        one_line_up.insert(
            "scroll_anchor_line",
            line_number(&scroll_text, scroll_view.viewport_anchor) as i64,
        );
        one_line_up.insert("scroll_phase", 2);
        assert_eq!(
            admits(&model, &at_bottom, &one_line_up),
            Some("ReverseScroll")
        );
        assert!(model.check_invariant("FirstReverseStepMoves", &one_line_up));

        // Negative control: retaining the old desktop anchor after installing
        // the compact row count hides the caret and is not a real transition.
        let mut fixed_desktop = before.clone();
        fixed_desktop.insert("visible_lines", 8);
        fixed_desktop.insert("resized", 1);
        assert_eq!(admits(&model, &before, &fixed_desktop), None);
        assert!(!model.check_invariant("CaretVisibleAfterResize", &fixed_desktop));

        // Negative control: leaving a short replacement anchored at its EOF is
        // not the shipping transition and violates full-document visibility.
        let mut stranded_short = after;
        stranded_short.insert("short_anchor_line", 2);
        assert_eq!(admits(&model, &before, &stranded_short), None);
        assert!(!model.check_invariant("ShortDocumentFullyVisible", &stranded_short));
    }

    #[test]
    fn viewport_projection_is_visible_only_utf8_safe_and_source_addressable() {
        let text = "zero\nβeta\nlast\n";
        let (_, _, mut view, _) = editor(text);
        view.viewport_anchor = "zero\n".len() + 1; // deliberately inside β
        view.selections = vec![
            Selection {
                anchor: "zero\n".len(),
                head: "zero\nβeta\nlast".len(),
            },
            Selection::caret("zero\nβ".len()),
        ];
        view.primary = 0;

        let projection = project_viewport(text, &view, 2, usize::MAX);
        assert_eq!(projection.first_line, 1);
        assert_eq!(projection.total_lines, 4);
        assert_eq!(projection.lines.len(), 2, "projection stays visible-only");
        assert_eq!(projection.lines[0].number, 1);
        assert_eq!(
            projection.lines[0].source,
            "zero\n".len().."zero\nβeta".len()
        );
        assert_eq!(projection.lines[0].text, "βeta");
        assert_eq!(projection.lines[0].carets, vec![("β".len(), false)]);
        assert_eq!(
            projection.lines[0].selections,
            vec![EditorSelectionSpan {
                bytes: 0.."βeta".len(),
                continues: true,
                primary: true,
            }]
        );
        assert_eq!(projection.lines[1].text, "last");
    }

    #[test]
    fn viewport_projection_keeps_trailing_empty_line_and_selected_newline_cell() {
        let text = "a\n";
        let (_, _, mut view, _) = editor(text);
        view.selections = vec![Selection { anchor: 0, head: 2 }];

        let projection = project_viewport(text, &view, usize::MAX, usize::MAX);
        assert_eq!(projection.total_lines, 2);
        assert_eq!(projection.lines.len(), 2);
        assert_eq!(projection.lines[0].text, "a");
        assert_eq!(projection.lines[0].selections[0].bytes, 0..1);
        assert!(projection.lines[0].selections[0].continues);
        assert_eq!(projection.lines[1].source, 2..2);
        assert_eq!(projection.lines[1].text, "");
        assert_eq!(projection.lines[1].carets, vec![(0, true)]);
    }

    #[test]
    fn compact_horizontal_window_follows_caret_and_is_shared_across_rows() {
        let line = "0123456789".repeat(24);
        let text = format!("{line}\n{line}\n{line}");
        let (_, _, mut view, _) = editor(&text);
        let caret = 173;
        view.selections = vec![Selection::caret(caret)];

        let projection = project_viewport(&text, &view, 3, 40);
        assert_eq!(projection.lines.len(), 3);
        let expected_start = caret - 30;
        for (row, projected) in projection.lines.iter().enumerate() {
            let line_start = row * (line.len() + 1);
            assert_eq!(projected.source.start, line_start + expected_start);
            assert_eq!(projected.column_start, expected_start);
            assert!(projected.text.chars().count() <= 40);
            assert!(text.is_char_boundary(projected.source.start));
            assert!(text.is_char_boundary(projected.source.end));
        }
        assert_eq!(projection.lines[0].carets, vec![(30, true)]);
        assert!(
            projection.lines[0].source.start > 0,
            "negative control: the old projection always began ordinary lines at byte zero"
        );
    }

    #[test]
    fn compact_horizontal_window_is_utf8_and_tab_safe() {
        let line = format!("{}\t{}", "α".repeat(80), "終".repeat(80));
        let text = format!("{line}\n{line}");
        let (_, _, mut view, _) = editor(&text);
        let caret = "α".len() * 70;
        view.selections = vec![Selection::caret(caret)];

        let projection = project_viewport(&text, &view, 2, 32);
        assert_eq!(projection.lines.len(), 2);
        assert!(projection.lines[0].source.start > 0);
        assert_eq!(projection.lines[0].column_start, 46);
        assert_eq!(projection.lines[1].column_start, 46);
        assert!(
            projection.lines[0]
                .carets
                .iter()
                .any(|(byte, primary)| { *primary && *byte <= projection.lines[0].text.len() })
        );
        for line in &projection.lines {
            assert!(text.is_char_boundary(line.source.start));
            assert!(text.is_char_boundary(line.source.end));
        }
    }

    #[test]
    fn compact_horizontal_window_never_splits_zwj_or_combining_graphemes() {
        let text = format!("{}👩‍💻x", "a".repeat(31));
        let (_, _, mut view, _) = editor(&text);
        view.selections = vec![Selection::caret(text.len())];

        let projection = project_viewport(&text, &view, 1, 4);
        let line = &projection.lines[0];
        assert_eq!(line.text, "👩‍💻x");
        assert_eq!(line.source.start, 31);
        assert!(
            line.source.start == text.len()
                || text
                    .grapheme_indices()
                    .any(|(boundary, _)| boundary == line.source.start)
        );
        assert!(
            line.source.end == text.len()
                || text
                    .grapheme_indices()
                    .any(|(boundary, _)| boundary == line.source.end)
        );

        let text = "abce\u{301}z";
        let (_, _, view, _) = editor(text);
        let projection = project_viewport(text, &view, 1, 4);
        let line = &projection.lines[0];
        assert_eq!(line.text, "abce\u{301}");
        assert_eq!(line.source, 0.."abce\u{301}".len());
        assert_eq!(
            line.text.graphemes().collect::<Vec<_>>(),
            ["a", "b", "c", "e\u{301}"]
        );
    }

    #[test]
    fn viewport_projection_bounds_pathological_long_line_and_keeps_caret_visible() {
        let text = "α".repeat(600_000);
        let (_, _, mut view, _) = editor(&text);
        let caret = text.len() - "α".len() * 9;
        view.selections = vec![Selection::caret(caret)];

        let projection = project_viewport(&text, &view, usize::MAX, usize::MAX);
        assert_eq!(projection.lines.len(), 1);
        let line = &projection.lines[0];
        assert!(line.text.len() <= 32 * 1024);
        assert!(line.source.start <= caret && caret <= line.source.end);
        assert_eq!(line.carets, vec![(caret - line.source.start, true)]);
        assert!(text.is_char_boundary(line.source.start));
        assert!(text.is_char_boundary(line.source.end));
    }

    #[test]
    fn viewport_projection_drops_an_over_budget_grapheme_instead_of_splitting_it() {
        let text = format!("e{}", "\u{301}".repeat(20_000));
        assert!(text.len() > 32 * 1024);
        assert_eq!(text.graphemes().count(), 1);
        let (_, _, view, _) = editor(&text);

        let projection = project_viewport(&text, &view, 1, usize::MAX);
        let line = &projection.lines[0];
        assert_eq!(line.text, "");
        assert_eq!(line.source, 0..0);
        assert!(line.text.len() <= 32 * 1024);
    }

    #[test]
    fn viewport_projection_has_a_total_byte_budget_across_many_large_lines() {
        // Zero-width control bytes exercise the byte ceiling independently of
        // the (normally much tighter) visible-column ceiling.
        let line = format!("{}\n", "\u{0001}".repeat(40 * 1024));
        let text = line.repeat(40);
        let (_, _, view, _) = editor(&text);
        let projection = project_viewport(&text, &view, usize::MAX, usize::MAX);
        let total = projection
            .lines
            .iter()
            .map(|line| line.text.len())
            .sum::<usize>();
        assert!(total <= 512 * 1024);
        assert!(
            projection.lines.len() < 40,
            "the byte budget truncates rows"
        );
    }

    #[test]
    fn pointer_selection_collapses_extends_and_clamps_to_utf8() {
        let text = "aé🦀z\nsecond";
        let (_, _, mut view, _) = editor(text);

        assert!(view.pointer_select(text, 2, false, 4));
        assert_eq!(view.primary_selection(), &Selection::caret(1));

        assert!(view.pointer_select(text, "aé🦀".len(), true, 4));
        assert_eq!(view.primary_selection().range(), 1.."aé🦀".len());

        let first_line_end = text.find('\n').unwrap();
        assert!(view.pointer_select(text, first_line_end, false, 4));
        assert_eq!(view.primary_selection(), &Selection::caret(first_line_end));

        assert!(view.pointer_select(text, usize::MAX, false, 4));
        assert_eq!(view.primary_selection(), &Selection::caret(text.len()));
        assert!(!view.pointer_select(text, text.len(), false, 4));
    }

    #[test]
    fn pointer_selection_clamps_to_complete_user_visible_graphemes() {
        let text = "ae\u{301}👩‍💻z";
        let (_, _, mut view, _) = editor(text);

        assert!(view.pointer_select(text, "ae".len(), false, 4));
        assert_eq!(view.primary_selection(), &Selection::caret("a".len()));

        let emoji_start = "ae\u{301}".len();
        assert!(view.pointer_select(text, emoji_start + "👩".len(), false, 4));
        assert_eq!(view.primary_selection(), &Selection::caret(emoji_start));

        let crlf = "ab\r\ncd";
        let (_, _, mut view, _) = editor(crlf);
        assert!(view.pointer_select(crlf, 3, false, 4));
        assert_eq!(
            view.primary_selection(),
            &Selection::caret(2),
            "the interior of CRLF retreats to the start of the pair"
        );
        assert!(!view.pointer_select(crlf, 2, false, 4));
    }

    #[test]
    fn c_x_c_s_emits_typed_save_at_exact_sequence() {
        let (mut store, mut workspace, mut view, document) = editor("abc");
        workspace
            .handle_chord(&mut store, &mut view, KeyChord::parse("C-x").unwrap())
            .unwrap();
        let effects = workspace
            .handle_chord(&mut store, &mut view, KeyChord::parse("C-s").unwrap())
            .unwrap();
        assert_eq!(
            effects,
            vec![EditorEffect::SaveDocument {
                document,
                seq: store.snapshot(document).unwrap().seq,
            }]
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

    #[test]
    fn set_mark_persists_across_motion_and_kill_region() {
        let (mut store, mut workspace, mut view, document) = editor("alpha beta");
        workspace
            .execute(&mut store, &mut view, EditorCommand::SetMark)
            .unwrap();
        for _ in 0..5 {
            workspace
                .execute(&mut store, &mut view, EditorCommand::MoveForward)
                .unwrap();
        }
        assert!(view.mark_active);
        assert_eq!(view.primary_selection(), &Selection { anchor: 0, head: 5 });

        workspace
            .execute(&mut store, &mut view, EditorCommand::KillRegion)
            .unwrap();
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), " beta");
        assert_eq!(
            workspace.kill_ring.front().map(String::as_str),
            Some("alpha")
        );
        assert!(!view.mark_active);
        assert!(view.primary_selection().is_caret());
    }

    #[test]
    fn modal_queries_search_and_execute_without_typing_into_document() {
        let (mut store, mut workspace, mut view, document) = editor("zero needle tail");
        let original = store.snapshot(document).unwrap();

        workspace
            .execute(&mut store, &mut view, EditorCommand::IncrementalSearch)
            .unwrap();
        workspace
            .insert_text(&mut store, &mut view, "needle")
            .unwrap();
        let after_search = store.snapshot(document).unwrap();
        assert_eq!(after_search.seq, original.seq);
        assert_eq!(after_search.text, original.text);
        assert_eq!(view.primary_selection().range(), 5..11);
        workspace
            .execute(&mut store, &mut view, EditorCommand::Abort)
            .unwrap();
        assert_eq!(view.primary_selection(), &Selection::caret(0));

        workspace
            .execute(&mut store, &mut view, EditorCommand::ExecuteCommand)
            .unwrap();
        let selection_before_query = view.primary_selection().clone();
        assert!(!view.pointer_select(original.text.as_ref(), original.text.len(), false, 12));
        assert_eq!(view.primary_selection(), &selection_before_query);
        workspace
            .insert_text(&mut store, &mut view, "forward-char")
            .unwrap();
        let after_query = store.snapshot(document).unwrap();
        assert_eq!(after_query.seq, original.seq);
        assert_eq!(after_query.text, original.text);
        workspace.submit_minibuffer(&mut store, &mut view).unwrap();
        assert_eq!(view.primary_selection(), &Selection::caret(1));
        let after_command = store.snapshot(document).unwrap();
        assert_eq!(after_command.seq, original.seq);
        assert_eq!(after_command.text, original.text);
    }

    #[test]
    fn unknown_command_and_buffer_query_are_safe_typed_outcomes() {
        let (mut store, mut workspace, mut view, document) = editor("unchanged");
        let original = store.snapshot(document).unwrap();
        workspace
            .execute(&mut store, &mut view, EditorCommand::ExecuteCommand)
            .unwrap();
        workspace
            .insert_text(&mut store, &mut view, "kill-everything-maybe")
            .unwrap();
        let effects = workspace.submit_minibuffer(&mut store, &mut view).unwrap();
        assert!(matches!(view.minibuffer, Minibuffer::Command { .. }));
        assert!(
            matches!(effects.as_slice(), [EditorEffect::Status(message)] if message.contains("No editor command"))
        );
        let after_unknown = store.snapshot(document).unwrap();
        assert_eq!(after_unknown.seq, original.seq);
        assert_eq!(after_unknown.text, original.text);

        view.cancel_transient();
        workspace
            .execute(&mut store, &mut view, EditorCommand::SwitchBuffer)
            .unwrap();
        workspace
            .insert_text(&mut store, &mut view, "other.md")
            .unwrap();
        let effects = workspace.submit_minibuffer(&mut store, &mut view).unwrap();
        assert_eq!(
            effects,
            vec![EditorEffect::SwitchBuffer {
                query: "other.md".to_string()
            }]
        );
        let after_buffer = store.snapshot(document).unwrap();
        assert_eq!(after_buffer.seq, original.seq);
        assert_eq!(after_buffer.text, original.text);
    }

    #[test]
    fn goto_line_word_motion_and_revert_are_real_emacs_commands() {
        let (mut store, mut workspace, mut view, document) = editor("zero\none two\nthree four\n");
        view.selections = vec![Selection::caret("zero\none two\nthree".len())];
        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveWordBackward)
            .unwrap();
        assert_eq!(
            &store.snapshot(document).unwrap().text[view.primary_selection().head..],
            "three four\n"
        );
        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveWordForward)
            .unwrap();
        assert_eq!(
            &store.snapshot(document).unwrap().text[view.primary_selection().head..],
            " four\n"
        );

        workspace
            .execute(&mut store, &mut view, EditorCommand::GotoLine)
            .unwrap();
        workspace
            .insert_minibuffer_text(&store, &mut view, "2")
            .unwrap();
        let effects = workspace.submit_minibuffer(&mut store, &mut view).unwrap();
        assert_eq!(
            view.primary_selection(),
            &Selection::caret(byte_of_line(&store.snapshot(document).unwrap().text, 1))
        );
        assert!(
            matches!(effects.as_slice(), [EditorEffect::Status(message)] if message == "Line 2")
        );

        let effects = workspace
            .execute(&mut store, &mut view, EditorCommand::RevertBuffer)
            .unwrap();
        assert_eq!(effects, [EditorEffect::RevertDocument { document }]);
        assert_eq!(
            workspace.keymap.resolve(&[KeyChord::parse("M-g").unwrap()]),
            KeyResolution::Prefix {
                display: "M-g".to_string(),
                reachable: vec!["g  goto-line".to_string()],
            }
        );
    }

    #[test]
    fn backward_word_scans_a_long_token_once_and_applies_counts_in_one_pass() {
        let token = "a".repeat(256 * 1024);
        let text = format!("{token} one two three");
        let one = token.len() + 1;
        let two = one + "one ".len();
        let three = two + "two ".len();

        assert_eq!(previous_word_boundary(&text, text.len(), 1), three);
        assert_eq!(previous_word_boundary(&text, text.len(), 2), two);
        assert_eq!(previous_word_boundary(&text, text.len(), 3), one);
        assert_eq!(previous_word_boundary(&text, three, 1), two);
        assert_eq!(previous_word_boundary(&text, text.len(), 4), 0);
    }

    #[test]
    fn invalid_goto_line_stays_modal_and_c_g_restores_origin() {
        let (mut store, mut workspace, mut view, _) = editor("one\ntwo\n");
        view.selections = vec![Selection::caret(4)];
        workspace
            .execute(&mut store, &mut view, EditorCommand::GotoLine)
            .unwrap();
        workspace
            .insert_minibuffer_text(&store, &mut view, "zero")
            .unwrap();
        let effects = workspace.submit_minibuffer(&mut store, &mut view).unwrap();
        assert!(matches!(view.minibuffer, Minibuffer::GotoLine { .. }));
        assert!(
            matches!(effects.as_slice(), [EditorEffect::Status(message)] if message.contains("positive integer"))
        );
        workspace
            .handle_chord(&mut store, &mut view, KeyChord::parse("C-g").unwrap())
            .unwrap();
        assert_eq!(view.primary_selection(), &Selection::caret(4));
        assert!(matches!(view.minibuffer, Minibuffer::Inactive));
    }

    #[test]
    fn minibuffer_input_cap_is_utf8_safe_and_does_not_edit_document() {
        let (mut store, mut workspace, mut view, document) = editor("safe");
        workspace
            .execute(&mut store, &mut view, EditorCommand::ExecuteCommand)
            .unwrap();
        let oversized = "🦀".repeat(MINIBUFFER_QUERY_LIMIT);
        workspace
            .insert_text(&mut store, &mut view, &oversized)
            .unwrap();
        let Minibuffer::Command { query, .. } = &view.minibuffer else {
            panic!("command minibuffer remains active");
        };
        assert!(query.len() <= MINIBUFFER_QUERY_LIMIT);
        assert!(query.is_char_boundary(query.len()));
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "safe");
    }

    #[test]
    fn command_completion_is_bounded_prefix_first_and_fuzzy_typed() {
        let broad = command_completions("");
        assert_eq!(broad.len(), COMMAND_COMPLETION_LIMIT);
        assert_eq!(broad[0], EditorCommand::Save);
        assert_eq!(command_completions("save")[0], EditorCommand::Save);
        assert_eq!(command_completions("svb")[0], EditorCommand::Save);
        let move_commands = command_completions("move-");
        assert_eq!(
            move_commands,
            [EditorCommand::MoveLineStart, EditorCommand::MoveLineEnd]
        );
        assert!(command_completions("definitely-not-a-command").is_empty());
    }

    #[test]
    fn command_query_change_resets_selection_and_submit_runs_exact_selected_candidate() {
        let (mut store, mut workspace, mut view, document) = editor("abc\nnext");
        workspace
            .execute(&mut store, &mut view, EditorCommand::ExecuteCommand)
            .unwrap();
        workspace
            .insert_minibuffer_text(&store, &mut view, "move-")
            .unwrap();
        workspace
            .command_completion_action(&mut store, &mut view, EditorCompletionAction::Next)
            .unwrap();
        assert!(matches!(
            view.minibuffer,
            Minibuffer::Command { selected: 1, .. }
        ));
        workspace
            .insert_minibuffer_text(&store, &mut view, "b")
            .unwrap();
        assert!(matches!(
            &view.minibuffer,
            Minibuffer::Command { query, selected: 0 } if query == "move-b"
        ));
        assert_eq!(
            command_completions("move-b"),
            [EditorCommand::MoveLineStart]
        );

        view.minibuffer = Minibuffer::Command {
            query: "move-".to_string(),
            selected: 0,
        };
        workspace
            .command_completion_action(&mut store, &mut view, EditorCompletionAction::Choose(1))
            .unwrap();
        assert!(matches!(view.minibuffer, Minibuffer::Inactive));
        assert_eq!(view.primary_selection().head, 3, "ran move-end-of-line");
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "abc\nnext");
    }

    #[test]
    fn tab_completes_selected_name_without_running_and_navigation_wraps() {
        let (mut store, mut workspace, mut view, _) = editor("abc");
        workspace
            .execute(&mut store, &mut view, EditorCommand::ExecuteCommand)
            .unwrap();
        workspace
            .insert_minibuffer_text(&store, &mut view, "move-")
            .unwrap();
        workspace
            .command_completion_action(&mut store, &mut view, EditorCompletionAction::Previous)
            .unwrap();
        assert!(matches!(
            view.minibuffer,
            Minibuffer::Command { selected: 1, .. }
        ));
        workspace
            .command_completion_action(&mut store, &mut view, EditorCompletionAction::Complete)
            .unwrap();
        assert!(matches!(
            &view.minibuffer,
            Minibuffer::Command { query, selected: 0 } if query == "move-end-of-line"
        ));
        assert_eq!(view.primary_selection().head, 0, "Tab never executes");
    }

    #[test]
    fn shipping_command_palette_conforms_to_selection_reset_and_exact_submit_model() {
        let model = native_editor_command_palette_model();
        let (mut store, mut workspace, mut view, _) = editor("abc");
        let initial = palette_projection(&model, &view, 0, 0, 0, 0);
        assert_eq!(initial, model.init_state());

        workspace
            .execute(&mut store, &mut view, EditorCommand::ExecuteCommand)
            .unwrap();
        let opened = palette_projection(&model, &view, 0, 0, 0, 0);
        assert_palette_transition(&model, &initial, &opened, "Open");

        workspace
            .insert_minibuffer_text(&store, &mut view, "move-")
            .unwrap();
        let broad = palette_projection(&model, &view, 1, 1, 0, 0);
        assert_palette_transition(&model, &opened, &broad, "TypeBroad");
        workspace
            .command_completion_action(&mut store, &mut view, EditorCompletionAction::Next)
            .unwrap();
        let moved = palette_projection(&model, &view, 1, 0, 0, 0);
        assert_palette_transition(&model, &broad, &moved, "MoveNext");

        workspace
            .insert_minibuffer_text(&store, &mut view, "b")
            .unwrap();
        let refined = palette_projection(&model, &view, 2, 1, 0, 0);
        assert_palette_transition(&model, &moved, &refined, "Refine");
        workspace.submit_minibuffer(&mut store, &mut view).unwrap();
        let completed = palette_projection(&model, &view, 0, 0, 1, 1);
        assert_palette_transition(&model, &refined, &completed, "Submit");
        assert_eq!(
            workspace.command_history.back().map(String::as_str),
            Some("move-beginning-of-line"),
            "Enter dispatches the exact selected typed command"
        );

        let mut stale_selection = refined.clone();
        stale_selection.insert("selected", 1);
        assert_eq!(admits(&model, &moved, &stale_selection), None);
        assert!(!model.check_invariant("SelectionWithinResults", &stale_selection));
        assert!(!model.check_invariant("QueryChangeResetsSelection", &stale_selection));

        let mut nearest_unknown_dispatch = completed.clone();
        nearest_unknown_dispatch.insert("exact_selected_dispatch", 0);
        assert_eq!(admits(&model, &refined, &nearest_unknown_dispatch), None);
        assert!(!model.check_invariant("SubmitIsExactSelected", &nearest_unknown_dispatch));
    }

    #[test]
    fn utf8_motion_stays_on_addressable_boundaries() {
        let (mut store, mut workspace, mut view, _) = editor("aé🦀z");
        view.selections = vec![Selection::caret("aé🦀z".len())];
        for expected in ["aé🦀".len(), "aé".len(), "a".len(), 0] {
            workspace
                .execute(&mut store, &mut view, EditorCommand::MoveBackward)
                .unwrap();
            assert_eq!(view.primary_selection().head, expected);
        }
    }

    #[test]
    fn motion_treats_combining_and_zwj_sequences_as_one_character() {
        let text = "e\u{301}👩‍💻z";
        let (mut store, mut workspace, mut view, _) = editor(text);
        view.selections = vec![Selection::caret(text.len())];
        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveBackward)
            .unwrap();
        assert_eq!(view.primary_selection().head, "e\u{301}👩‍💻".len());
        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveBackward)
            .unwrap();
        assert_eq!(view.primary_selection().head, "e\u{301}".len());
        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveBackward)
            .unwrap();
        assert_eq!(view.primary_selection().head, 0);
    }
}
