// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The workspace: the state every editor view shares (kill ring, marks,
//! command history, keymap, undo history, the keyboard macro), and the
//! reducer's entry points, which turn typed text, a key chord or a command
//! into document transactions and host effects.

use std::collections::{BTreeMap, VecDeque};

use aterm_buffer::Seq;

use super::chord::KeyChord;
use super::command::{CommandAuthority, EditorCommand, Invocation, PrefixArgument, TAB_STOP};
use super::history::DocumentHistory;
use super::keymap::{KeyResolution, Keymap, Platform, Profile};
use super::kill::YankRecord;
use super::macros::MacroStep;
use super::minibuffer::Minibuffer;
use super::projection::{EditorLines, editor_display_column, line_end, line_start, line_text_end};
use super::view::{
    EditorBufferView, Selection, next_boundary, next_line_position, next_word_boundary,
    previous_boundary, previous_line_position, previous_word_boundary, repeat_line_motion,
};
use crate::store::{DocumentId, DocumentStore, DocumentViewId, EditDelta, TextEdit};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlobalMark {
    pub document: DocumentId,
    pub offset: usize,
}

/// Why a command asks the host to close its view ([`EditorEffect::CloseView`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseWhy {
    /// The person is done with the view: `kill-buffer` of this buffer,
    /// `save-buffers-kill-terminal`.
    Plain,
    /// `finish-edit`: the edit a waiting client asked for is done.
    Finish,
    /// `abort-edit`: the edit a waiting client asked for is abandoned.
    Abort,
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
    /// Close the view the command ran in, as the host closes any view (a
    /// changed draft is saved first), or answer the client waiting on its
    /// edit (`why`).
    CloseView {
        why: CloseWhy,
    },
    /// `kill-buffer` of another open buffer, named as `switch-to-buffer`
    /// names one; the host resolves the name and closes that buffer's view.
    KillBuffer {
        query: String,
    },
    /// The kill ring's newest entry, which a kill or `kill-ring-save` just
    /// made (or joined), for the face to put on the system clipboard: the
    /// platform clipboard in the GUI, an OSC 52 write in a terminal (design
    /// §2.1). Emitted only for an input granted
    /// [`CommandAuthority::Clipboard`], so a control client's kill stays in
    /// the ring and never reaches the person's clipboard.
    KillToClipboard {
        text: String,
    },
    Status(String),
    Bell,
}

/// Whether [`EditorWorkspace::execute_with`] runs a command whose macro
/// step is already recorded: one a prompt's answers complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Recorded {
    No,
    AsItsKey,
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

/// The editing options the workspace's commands read. Config slice C1 fills
/// them from `[amacs]` (design §9); until then they are Emacs's defaults,
/// with aterm's four-column tab stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditorOptions {
    /// The display column `fill-paragraph` (M-q) fills lines to.
    pub fill_column: usize,
    /// The columns between the tab stops TAB indents to.
    pub tab_width: usize,
    /// Whether TAB indents with a tab character rather than spaces.
    pub indent_tabs: bool,
}

impl Default for EditorOptions {
    fn default() -> Self {
        Self {
            fill_column: 70,
            tab_width: TAB_STOP,
            indent_tabs: false,
        }
    }
}

/// What a command left behind it: the view it ran in, the document's
/// sequence and that view's selections. The command before is Emacs's
/// `last-command` only while all three still hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LeftBy {
    view: DocumentViewId,
    seq: Option<Seq>,
    selections: Vec<Selection>,
}

impl LeftBy {
    fn now(store: &DocumentStore, view: &EditorBufferView) -> Self {
        Self {
            view: view.document_view,
            seq: store.snapshot(view.document).map(|snapshot| snapshot.seq),
            selections: view.selections.clone(),
        }
    }
}

/// State shared across every editor document and view in one workspace/window group.
pub struct EditorWorkspace {
    pub buffers: Vec<DocumentId>,
    pub kill_ring: VecDeque<String>,
    pub global_mark_ring: VecDeque<GlobalMark>,
    pub command_history: VecDeque<String>,
    pub keymap: Keymap,
    pub options: EditorOptions,
    /// The authority classes the input being reduced carries. A command
    /// that needs one it lacks ([`Self::authorities`]) runs nothing and says
    /// why. Every class unless the host narrows it around a control
    /// client's input, and restores it after.
    pub granted: Vec<CommandAuthority>,
    pub(super) histories: BTreeMap<DocumentId, DocumentHistory>,
    pub(super) recording: Option<Vec<MacroStep>>,
    pub(super) last_macro: Vec<MacroStep>,
    pub(super) playing_macro: bool,
    pub(super) last_yank: Option<YankRecord>,
    pub(super) last_commit: Option<(DocumentId, Seq, Vec<EditDelta>)>,
    /// What the last command left ([`LeftBy`]). A view's `last_command`
    /// counts only while it does, so a kill joins the kill ring's newest
    /// entry only straight after a kill that made it: never after a kill in
    /// another view, an edit from elsewhere, or a selection the host set (⌘A,
    /// accessibility), all of which the kill ring, shared by every view,
    /// would otherwise run together.
    pub(super) last_left: Option<LeftBy>,
    /// Whether the kill ring's newest entry changed since the face was last
    /// handed it ([`EditorEffect::KillToClipboard`]).
    pub(super) kill_ring_changed: bool,
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
            options: EditorOptions::default(),
            granted: CommandAuthority::ALL.to_vec(),
            histories: BTreeMap::new(),
            recording: None,
            last_macro: Vec::new(),
            playing_macro: false,
            last_yank: None,
            last_commit: None,
            last_left: None,
            kill_ring_changed: false,
        }
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Every authority running `command` here needs: its own
    /// ([`EditorCommand::authority`]) and, for `call-last-kbd-macro`, those
    /// of every command the macro replays, so a macro can never carry a
    /// command past a check its own name would fail.
    pub fn authorities(&self, command: EditorCommand) -> Vec<CommandAuthority> {
        let mut needed = vec![command.authority()];
        if command == EditorCommand::PlayMacro {
            needed.extend(self.last_macro.iter().filter_map(|step| match step {
                MacroStep::Command(command, ..) => Some(command.authority()),
                MacroStep::Text(_) => None,
            }));
        }
        needed.sort_unstable();
        needed.dedup();
        needed
    }

    /// Why `command` may not run under `granted`: the first class it needs
    /// ([`Self::authorities`]) that `granted` lacks, as the status that says
    /// so. `None` when it may run.
    #[must_use]
    pub fn refusal(&self, command: EditorCommand, granted: &[CommandAuthority]) -> Option<String> {
        self.authorities(command)
            .into_iter()
            .find(|needed| !granted.contains(needed))
            .map(|missing| {
                format!(
                    "{} needs {} authority, which this input does not carry",
                    command.name(),
                    missing.name()
                )
            })
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
        // Text insertion takes no count: a pending C-u stops here. Typing
        // also ends a run of kills, so the next kill starts a new entry.
        view.take_prefix_argument();
        view.last_command = None;
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
            if matches!(view.minibuffer, Minibuffer::Search { .. }) {
                if chord == KeyChord::parse("C-s").expect("static chord") {
                    return self.navigate_search(store, view, true);
                }
                if chord == KeyChord::parse("C-r").expect("static chord") {
                    return self.navigate_search(store, view, false);
                }
            }
            return Ok(vec![EditorEffect::Status(
                "Minibuffer active — Enter accepts; C-g cancels".to_string(),
            )]);
        }
        // In the terminal face ESC is Meta for the key after it, as in Emacs
        // (`ESC x` is `M-x`), so every M- binding has a spelling on a
        // terminal whose Alt key types characters instead; ESC ESC quits. A
        // GUI's Escape is its own key, which quits (`Profile::escape_is_meta`).
        let mut chord = chord;
        if view.escape_pending() {
            view.chord_prefix.clear();
            if chord.is_escape() {
                return self.execute(store, view, EditorCommand::Abort);
            }
            chord.meta = true;
        } else if view.chord_prefix.is_empty()
            && chord.is_escape()
            && self.keymap.profile().escape_is_meta()
        {
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
        let resolution = self.keymap.resolve(&view.chord_prefix);
        // `M-<digit>` names its digit in the key itself, which the command
        // reads here; run any other way it has no digit to read.
        if resolution == KeyResolution::Command(EditorCommand::DigitArgument)
            && let Some(digit) = view
                .chord_prefix
                .last()
                .and_then(|chord| chord.key.parse::<u32>().ok())
                .filter(|digit| *digit <= 9)
        {
            view.chord_prefix.clear();
            let argument = PrefixArgument::after_meta_digit(view.prefix_argument, digit);
            view.prefix_argument = Some(argument);
            view.prefix_hud = Some(argument.hud());
            return Ok(Vec::new());
        }
        match resolution {
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

    /// Run `command` as a key, a button or M-x runs it: with the pending
    /// `C-u` argument, and asking for its arguments in the minibuffer.
    pub fn execute(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        command: EditorCommand,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        self.execute_with(store, view, command, Vec::new(), Recorded::No)
    }

    /// Run a whole [`Invocation`], as a machine does: its count replaces any
    /// pending argument, and its `args` answer the command's prompts in one
    /// call. With no `args` a prompting command asks for them in the
    /// minibuffer, as from a key; with the wrong number it runs nothing.
    pub fn invoke(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        invocation: Invocation,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let Invocation {
            command,
            count,
            args,
        } = invocation;
        let wanted = command.prompts().len();
        if !args.is_empty() && args.len() != wanted {
            return Ok(vec![EditorEffect::Status(format!(
                "{} takes {wanted} argument{}, not {}",
                command.name(),
                if wanted == 1 { "" } else { "s" },
                args.len()
            ))]);
        }
        if !view.minibuffer_active() {
            view.prefix_argument =
                count.map(|value| PrefixArgument::Numeric { value, open: false });
            view.prefix_hud = None;
        }
        self.execute_with(store, view, command, args, Recorded::No)
    }

    /// Run the command a prompt has collected every answer for. Its key
    /// recorded the command, before the prompt opened, so a macro being
    /// defined takes the answers into that step, to replay without asking.
    pub(super) fn answer_prompt(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        invocation: Invocation,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        self.record_answers(invocation.command, &invocation.args);
        let Invocation {
            command,
            count,
            args,
        } = invocation;
        view.prefix_argument = count.map(|value| PrefixArgument::Numeric { value, open: false });
        view.prefix_hud = None;
        self.execute_with(store, view, command, args, Recorded::AsItsKey)
    }

    /// Give the macro step that opened `command`'s minibuffer the answers it
    /// collected. The step is the newest one: while a minibuffer is open no
    /// other command runs, and its text is the query's, not the macro's.
    pub(super) fn record_answers(&mut self, command: EditorCommand, answers: &[String]) {
        if self.playing_macro {
            return;
        }
        if let Some(steps) = &mut self.recording
            && let Some(MacroStep::Command(recorded, _, args)) = steps.last_mut()
            && *recorded == command
            && args.is_empty()
        {
            *args = answers.to_vec();
        }
    }

    /// Run one command: its authority checked, its argument taken, recorded
    /// into a macro being defined (unless `recorded` says its key already
    /// was), and the kill ring's newest entry handed to the face when it
    /// changed.
    pub(super) fn execute_with(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        command: EditorCommand,
        args: Vec<String>,
        recorded: Recorded,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        if view.minibuffer_active() {
            if command == EditorCommand::Abort {
                view.cancel_transient();
                return Ok(vec![EditorEffect::Status("Quit".into())]);
            }
            if matches!(&view.minibuffer, Minibuffer::Search { .. }) {
                match command {
                    EditorCommand::IncrementalSearch | EditorCommand::SearchNext => {
                        return self.navigate_search(store, view, true);
                    }
                    EditorCommand::SearchPrevious => {
                        return self.navigate_search(store, view, false);
                    }
                    _ => {}
                }
            }
            return Ok(vec![EditorEffect::Status(
                "Minibuffer active — Enter accepts; C-g cancels".to_string(),
            )]);
        }
        // A command the input may not run runs nothing, and the argument
        // meant for it goes with it, as an unbound key's does.
        if let Some(refusal) = self.refusal(command, &self.granted) {
            view.prefix_argument = None;
            view.prefix_hud = None;
            return Ok(vec![EditorEffect::Status(refusal)]);
        }
        // C-u and M-<digit> build the argument and M-x forwards it to the
        // command chosen in the minibuffer. Every other command takes it
        // here, once: a command with no use for it drops it rather than
        // leaving it for a later one.
        let argument_command = matches!(
            command,
            EditorCommand::UniversalArgument
                | EditorCommand::DigitArgument
                | EditorCommand::ExecuteCommand
        );
        let argument = if argument_command {
            None
        } else {
            view.take_prefix_argument()
        };
        // A step carries the command's argument and its answers. M-x is no
        // step: the command it runs records itself, with the argument M-x
        // forwarded to it.
        if let Some(steps) = &mut self.recording
            && !self.playing_macro
            && recorded == Recorded::No
            && !matches!(
                command,
                EditorCommand::StartMacro
                    | EditorCommand::EndMacro
                    | EditorCommand::PlayMacro
                    | EditorCommand::UniversalArgument
                    | EditorCommand::DigitArgument
                    | EditorCommand::ExecuteCommand
            )
        {
            steps.push(MacroStep::Command(command, argument, args.clone()));
        }
        self.remember_command(command.name());
        // What ran before this command decides whether a kill joins the last
        // one and where C-l puts the line, while the view is as that command
        // left it ([`LeftBy`]). The argument commands are not "before"
        // anything: `C-k C-u C-k` still appends.
        let previous = view
            .last_command
            .filter(|_| self.last_left.as_ref() == Some(&LeftBy::now(store, view)));
        let mut result = self.run(store, view, command, argument, previous, args);
        view.last_command = if argument_command {
            previous
        } else {
            Some(command)
        };
        self.last_left = Some(LeftBy::now(store, view));
        // A macro's kills reach the face once, when the macro is done.
        if !self.playing_macro
            && let Ok(effects) = &mut result
            && std::mem::take(&mut self.kill_ring_changed)
            && self.granted.contains(&CommandAuthority::Clipboard)
            && let Some(newest) = self.kill_ring.front()
        {
            effects.push(EditorEffect::KillToClipboard {
                text: newest.clone(),
            });
        }
        result
    }

    /// The body of one command, with its argument taken.
    fn run(
        &mut self,
        store: &mut DocumentStore,
        view: &mut EditorBufferView,
        command: EditorCommand,
        argument: Option<PrefixArgument>,
        previous: Option<EditorCommand>,
        args: Vec<String>,
    ) -> Result<Vec<EditorEffect>, EditorError> {
        let count = argument.map_or(1, PrefixArgument::count);
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
            EditorCommand::RecenterTopBottom => {
                let snapshot = snapshot_for_view(store, view)?;
                let lines = EditorLines::scanning(&snapshot.text);
                view.recenter_top_bottom(&lines, argument, previous == Some(command));
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
                    let position = line_text_end(&snapshot.text, selection.head);
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
            EditorCommand::BeginningOfBuffer | EditorCommand::EndOfBuffer => {
                return self.buffer_end(store, view, command == EditorCommand::EndOfBuffer);
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
            EditorCommand::IndentForTab => self.indent_for_tab(store, view)?,
            EditorCommand::Newline => self.newline(store, view, count)?,
            EditorCommand::OpenLine => self.open_line(store, view, count)?,
            EditorCommand::TransposeChars => {
                return self.transpose_chars(store, view, argument, count);
            }
            EditorCommand::FillParagraph => return self.fill_paragraph(store, view),
            // `C-u C-SPC` pops the mark instead of setting it, as in Emacs.
            EditorCommand::SetMark if argument.is_some() => return self.pop_mark(store, view),
            EditorCommand::SetMark => return Ok(self.set_mark(view)),
            EditorCommand::PopMark => return self.pop_mark(store, view),
            EditorCommand::ExchangePointAndMark => {
                return self.exchange_point_and_mark(store, view);
            }
            EditorCommand::MarkWholeBuffer => return self.mark_whole_buffer(store, view),
            EditorCommand::KillRegion => return self.kill_region(store, view, previous),
            EditorCommand::KillLine => self.kill_line(store, view, argument, count, previous)?,
            EditorCommand::KillWord | EditorCommand::BackwardKillWord => {
                return self.kill_word(
                    store,
                    view,
                    count,
                    previous,
                    command == EditorCommand::BackwardKillWord,
                );
            }
            EditorCommand::KillRingSave => return self.kill_ring_save(store, view, previous),
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
            EditorCommand::KillBuffer => {
                let Some(query) = args.into_iter().next() else {
                    view.open_minibuffer(Minibuffer::Prompt {
                        invocation: Invocation {
                            command,
                            count: argument.map(PrefixArgument::value),
                            args: Vec::new(),
                        },
                        query: String::new(),
                    });
                    return Ok(Vec::new());
                };
                // An empty answer is this buffer, as `C-x k RET` is in Emacs.
                return Ok(vec![if query.trim().is_empty() {
                    EditorEffect::CloseView {
                        why: CloseWhy::Plain,
                    }
                } else {
                    EditorEffect::KillBuffer { query }
                }]);
            }
            EditorCommand::SaveBuffersKillTerminal => {
                return Ok(vec![EditorEffect::CloseView {
                    why: CloseWhy::Plain,
                }]);
            }
            EditorCommand::FinishEdit => {
                return Ok(vec![EditorEffect::CloseView {
                    why: CloseWhy::Finish,
                }]);
            }
            EditorCommand::AbortEdit => {
                return Ok(vec![EditorEffect::CloseView {
                    why: CloseWhy::Abort,
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
            EditorCommand::DigitArgument => {
                // Reached only without a digit key (M-x, a button): there is
                // no digit to take.
                return Ok(vec![EditorEffect::Status(
                    "digit-argument is typed as M-0 … M-9".to_string(),
                )]);
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
            EditorCommand::SearchNext | EditorCommand::SearchPrevious => {
                return self.navigate_search(store, view, command == EditorCommand::SearchNext);
            }
            EditorCommand::GotoLine => {
                view.mark_active = false;
                let origin = view.primary_selection().head;
                let Some(query) = args.into_iter().next() else {
                    view.open_minibuffer(Minibuffer::GotoLine {
                        query: String::new(),
                        origin,
                    });
                    return Ok(Vec::new());
                };
                return Ok(match self.goto_line(store, view, &query)? {
                    Ok(effects) => effects,
                    Err(refusal) => vec![EditorEffect::Status(refusal)],
                });
            }
            EditorCommand::SwitchBuffer => {
                let Some(query) = args.into_iter().next() else {
                    view.open_minibuffer(Minibuffer::Buffer {
                        query: String::new(),
                    });
                    return Ok(Vec::new());
                };
                return Ok(vec![EditorEffect::SwitchBuffer { query }]);
            }
            EditorCommand::RevertBuffer => {
                return Ok(vec![EditorEffect::RevertDocument {
                    document: view.document,
                }]);
            }
            EditorCommand::StartMacro => return Ok(self.start_kbd_macro()),
            EditorCommand::EndMacro => return Ok(self.end_kbd_macro()),
            EditorCommand::PlayMacro => return self.play_macro(store, view),
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
