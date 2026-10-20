// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The workspace: the state every editor view shares (kill ring, marks,
//! command history, keymap, undo history, the keyboard macro), and the
//! reducer's entry points, which turn typed text, a key chord or a command
//! into document transactions and host effects.

use std::collections::{BTreeMap, VecDeque};

use aterm_buffer::Seq;

use super::chord::KeyChord;
use super::command::{EditorCommand, PrefixArgument, TAB_STOP};
use super::history::DocumentHistory;
use super::keymap::{KeyResolution, Keymap, Platform, Profile};
use super::kill::YankRecord;
use super::macros::MacroStep;
use super::minibuffer::Minibuffer;
use super::projection::{EditorLines, editor_display_column, line_end, line_start};
use super::view::{
    EditorBufferView, Selection, next_boundary, next_line_position, next_word_boundary,
    previous_boundary, previous_line_position, previous_word_boundary, repeat_line_motion,
};
use crate::store::{
    DocumentId, DocumentStore, DocumentViewId, EditDelta, TextEdit, rebase_position,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlobalMark {
    pub document: DocumentId,
    pub offset: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorEffect {
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
pub enum EditorError {
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

/// State shared across every editor document and view in one workspace/window group.
pub struct EditorWorkspace {
    pub buffers: Vec<DocumentId>,
    pub kill_ring: VecDeque<String>,
    pub global_mark_ring: VecDeque<GlobalMark>,
    pub command_history: VecDeque<String>,
    pub keymap: Keymap,
    pub(super) histories: BTreeMap<DocumentId, DocumentHistory>,
    pub(super) recording: Option<Vec<MacroStep>>,
    pub(super) last_macro: Vec<MacroStep>,
    pub(super) playing_macro: bool,
    pub(super) last_yank: Option<YankRecord>,
    pub(super) last_commit: Option<(DocumentId, Seq, Vec<EditDelta>)>,
}

/// The workspace aterm's native Editor view uses: the GUI keymap of the host
/// platform. The terminal face builds its own with
/// [`EditorWorkspace::with_keymap`] and [`Profile::Terminal`].
impl Default for EditorWorkspace {
    fn default() -> Self {
        Self::with_keymap(Keymap::emacs(Profile::Gui(Platform::HOST)))
    }
}

impl EditorWorkspace {
    /// A workspace whose keys resolve through `keymap`.
    pub fn with_keymap(keymap: Keymap) -> Self {
        Self {
            buffers: Vec::new(),
            kill_ring: VecDeque::new(),
            global_mark_ring: VecDeque::new(),
            command_history: VecDeque::new(),
            keymap,
            histories: BTreeMap::new(),
            recording: None,
            last_macro: Vec::new(),
            playing_macro: false,
            last_yank: None,
            last_commit: None,
        }
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Drop everything this workspace keeps for `document`, which the store
    /// has released: its buffer entry, undo and redo history, the global marks
    /// into it, and a yank or commit record naming it. Reopening the file is a
    /// new document with a history of its own.
    pub fn forget(&mut self, document: DocumentId) {
        self.buffers.retain(|buffer| *buffer != document);
        self.histories.remove(&document);
        self.global_mark_ring
            .retain(|mark| mark.document != document);
        if self
            .last_yank
            .as_ref()
            .is_some_and(|yank| yank.document() == document)
        {
            self.last_yank = None;
        }
        if self
            .last_commit
            .as_ref()
            .is_some_and(|(committed, ..)| *committed == document)
        {
            self.last_commit = None;
        }
    }

    pub fn attach(
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

    pub fn insert_text(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        text: &str,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        if view.minibuffer_active() {
            return self.insert_minibuffer_text(store, view, text);
        }
        // After C-u a digit is part of the argument, never text. The host
        // routes a plain key as text once the C-u chord has resolved. A macro
        // records the argument with the command that takes it, not the digit.
        if view.accept_argument_digit(text) {
            return Ok(Vec::new());
        }
        if let Some(steps) = &mut self.recording
            && !self.playing_macro
        {
            steps.push(MacroStep::Text(text.to_owned()));
        }
        // Text insertion takes no count: a pending C-u stops here.
        view.take_prefix_argument();
        let edits = edits_for_selections(&view.selections, text)?;
        self.apply_recorded(store, view, edits)?;
        self.last_yank = None;
        Ok(Vec::new())
    }

    /// Insert a paste: one transaction, never read as keys. Even a one-digit
    /// paste after `C-u` is text, never part of the argument, which (as for
    /// any insertion) ends with it. A paste into an open minibuffer goes to
    /// its query, and the argument waits on for the command it runs.
    pub(crate) fn insert_paste(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        text: &str,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        if !view.minibuffer_active() {
            view.take_prefix_argument();
        }
        self.insert_text(store, view, text)
    }

    pub fn handle_chord(
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
        // ESC is Meta for the key after it, as in Emacs (`ESC x` is `M-x`), so
        // every M- binding has a spelling on a keyboard or terminal whose Alt
        // key types characters instead. ESC ESC quits, as one Escape did.
        let mut chord = chord;
        if view.escape_pending() {
            view.chord_prefix.clear();
            if chord.is_escape() {
                return self.execute(store, view, EditorCommand::Abort);
            }
            chord.meta = true;
        } else if view.chord_prefix.is_empty() && chord.is_escape() {
            view.chord_prefix.push(chord);
            view.prefix_hud = Some(format!(
                "{}ESC — the next key is Meta (C-g cancels)",
                view.prefix_argument
                    .map(|argument| format!("{} ", argument.label()))
                    .unwrap_or_default()
            ));
            return Ok(Vec::new());
        }
        if view.chord_prefix.is_empty()
            && !(chord.control || chord.meta || chord.shift)
            && view.accept_argument_digit(&chord.key)
        {
            return Ok(Vec::new());
        }
        view.chord_prefix.push(chord);
        match self.keymap.resolve(&view.chord_prefix) {
            KeyResolution::Command(command) => {
                view.chord_prefix.clear();
                view.prefix_hud = None;
                self.execute(store, view, command)
            }
            KeyResolution::Prefix { display, reachable } => {
                // A pending C-u stays on screen while the sequence it waits
                // for is typed (`C-u 4 C-x — …`).
                let argument = view
                    .prefix_argument
                    .map(|argument| format!("{} ", argument.label()))
                    .unwrap_or_default();
                view.prefix_hud = Some(format!("{argument}{display} — {}", reachable.join("  ")));
                Ok(Vec::new())
            }
            KeyResolution::Unbound => {
                let keys = view
                    .chord_prefix
                    .iter()
                    .map(KeyChord::display)
                    .collect::<Vec<_>>()
                    .join(" ");
                view.chord_prefix.clear();
                view.prefix_hud = None;
                // An undefined key ends a pending C-u too, as in Emacs: no
                // command took it, and its hint is gone with the HUD.
                view.prefix_argument = None;
                Ok(vec![EditorEffect::Status(format!("{keys} is not bound"))])
            }
        }
    }

    pub fn execute(
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
        // C-u builds the argument and M-x forwards it to the command chosen
        // in the minibuffer. Every other command takes it here, once: a command
        // with no use for it drops it rather than leaving it for a later one.
        let argument = if matches!(
            command,
            EditorCommand::UniversalArgument | EditorCommand::ExecuteCommand
        ) {
            None
        } else {
            view.take_prefix_argument()
        };
        let count = argument.map_or(1, PrefixArgument::count);
        if let Some(steps) = &mut self.recording
            && !self.playing_macro
            && !matches!(
                command,
                EditorCommand::StartMacro
                    | EditorCommand::EndMacro
                    | EditorCommand::PlayMacro
                    | EditorCommand::UniversalArgument
            )
        {
            steps.push(MacroStep::Command(command, argument));
        }
        self.remember_command(command.name());

        match command {
            EditorCommand::MoveBackward => {
                let snapshot = snapshot_for_view(store, view)?;
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
                    let position = repeat_line_motion(selection.head, count, |at| {
                        previous_line_position(&snapshot.text, at, desired)
                    });
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
                    let position = repeat_line_motion(selection.head, count, |at| {
                        next_line_position(&snapshot.text, at, desired)
                    });
                    selection.move_head(position, preserve_anchor);
                }
                self.last_yank = None;
            }
            EditorCommand::ScrollPageDown | EditorCommand::ScrollPageUp => {
                // A page is the view's height less two lines of context. The
                // view and the carets move by the same lines, so the caret
                // keeps its row on screen; at either end the view stops and
                // the carets go on to the first or last line, so no press is
                // a no-op before the caret reaches the end of the document.
                let snapshot = snapshot_for_view(store, view)?;
                let lines = EditorLines::scanning(&snapshot.text);
                let page = view.viewport_lines().saturating_sub(2).max(1);
                let down = command == EditorCommand::ScrollPageDown;
                let delta = i32::try_from(page).unwrap_or(i32::MAX);
                view.scroll_lines_with(&lines, if down { delta } else { -delta });
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
                    let mut position = selection.head;
                    for _ in 0..page {
                        let next = if down {
                            next_line_position(&snapshot.text, position, desired)
                        } else {
                            previous_line_position(&snapshot.text, position, desired)
                        };
                        if next == position {
                            break;
                        }
                        position = next;
                    }
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
                let preserve_anchor = view.mark_active;
                for selection in &mut view.selections {
                    let at = if command == EditorCommand::MoveWordBackward {
                        previous_word_boundary(&snapshot.text, selection.head, count)
                    } else {
                        next_word_boundary(&snapshot.text, selection.head, count)
                    };
                    selection.move_head(at, preserve_anchor);
                }
                view.desired_column = None;
                self.last_yank = None;
            }
            EditorCommand::DeleteBackward | EditorCommand::DeleteForward => {
                let snapshot = snapshot_for_view(store, view)?;
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
            EditorCommand::IndentForTab => {
                let snapshot = snapshot_for_view(store, view)?;
                let heads = view
                    .selections
                    .iter()
                    .map(|selection| selection.head)
                    .collect::<Vec<_>>();
                // Each caret's column is read off its line as the indents
                // before it on that line leave it, so a second caret on one
                // line lands on a tab stop too: the line up to each caret is
                // rebuilt in document order, indents included.
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
                    let column = editor_display_column(&indented, indented.len(), 0);
                    let insert = " ".repeat(TAB_STOP - column % TAB_STOP);
                    indented.push_str(&insert);
                    edits.push(TextEdit {
                        range: head..head,
                        insert,
                    });
                }
                let (_, deltas) = self.apply_recorded(store, view, normalize_edits(edits)?)?;
                // The indent goes in at each caret and never over a region:
                // each selection becomes a caret after its own indent.
                for (selection, head) in view.selections.iter_mut().zip(heads) {
                    selection.collapse(rebase_position(head, &deltas));
                }
                view.desired_column = None;
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
            EditorCommand::KillRegion => return self.kill_region(store, view),
            EditorCommand::KillLine => self.kill_line(store, view, argument, count)?,
            EditorCommand::Yank => self.yank(store, view, argument, count)?,
            EditorCommand::YankPop => return self.yank_pop(store, view),
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
                let argument = PrefixArgument::after_universal(view.prefix_argument);
                view.prefix_argument = Some(argument);
                view.prefix_hud = Some(argument.hud());
            }
            EditorCommand::ExecuteCommand => {
                view.open_minibuffer(Minibuffer::Command {
                    query: String::new(),
                    selected: 0,
                });
                return Ok(vec![EditorEffect::ShowCommands]);
            }
            EditorCommand::IncrementalSearch => {
                view.mark_active = false;
                let origin = view.primary_selection().head;
                view.open_minibuffer(Minibuffer::Search {
                    query: String::new(),
                    origin,
                });
            }
            EditorCommand::GotoLine => {
                view.mark_active = false;
                let origin = view.primary_selection().head;
                view.open_minibuffer(Minibuffer::GotoLine {
                    query: String::new(),
                    origin,
                });
            }
            EditorCommand::SwitchBuffer => {
                view.open_minibuffer(Minibuffer::Buffer {
                    query: String::new(),
                });
                return Ok(Vec::new());
            }
            EditorCommand::RevertBuffer => {
                return Ok(vec![EditorEffect::RevertDocument {
                    document: view.document,
                }]);
            }
            EditorCommand::StartMacro => return Ok(self.start_kbd_macro()),
            EditorCommand::EndMacro => return Ok(self.end_kbd_macro()),
            EditorCommand::PlayMacro => self.play_macro(store, view)?,
        }
        Ok(Vec::new())
    }
}

pub(super) fn snapshot_for_view(
    store: &DocumentStore,
    view: &EditorBufferView,
) -> Result<crate::store::DocumentSnapshot, EditorError> {
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

pub(super) fn edits_for_selections(
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

pub(super) fn normalize_edits(mut edits: Vec<TextEdit>) -> Result<Vec<TextEdit>, EditorError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::editor;
    use crate::editor::kill::YANK_POP_NEEDS_YANK;
    use crate::store::DocumentTxnOutcome;

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
    fn unbound_keys_and_idle_bound_commands_say_their_own_reason() {
        let (mut store, mut workspace, mut view, _) = editor("abc");
        workspace
            .handle_chord(&mut store, &mut view, KeyChord::parse("C-x").unwrap())
            .unwrap();
        let effects = workspace
            .handle_chord(&mut store, &mut view, KeyChord::parse("z").unwrap())
            .unwrap();
        assert!(
            matches!(effects.as_slice(), [EditorEffect::Status(message)] if message.contains("not bound")),
            "{effects:?}"
        );
        // C-w IS bound: with no region it must not claim the key is unbound.
        let effects = workspace
            .handle_chord(&mut store, &mut view, KeyChord::parse("C-w").unwrap())
            .unwrap();
        assert!(
            matches!(effects.as_slice(), [EditorEffect::Status(message)] if !message.contains("not bound")),
            "{effects:?}"
        );
        let effects = workspace
            .handle_chord(&mut store, &mut view, KeyChord::parse("M-y").unwrap())
            .unwrap();
        assert_eq!(
            effects,
            [EditorEffect::Status(YANK_POP_NEEDS_YANK.to_string())]
        );
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
}
