// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Closed table of editor commands, authority classes, prompts, and command history.

use super::workspace::EditorWorkspace;

const COMMAND_HISTORY_LIMIT: usize = 200;
const COMMAND_COMPLETION_LIMIT: usize = 8;

/// Columns between tab stops.
pub const TAB_STOP: usize = 4;

/// A pending `C-u` argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PrefixArgument {
    /// `C-u` alone, pressed n times: 4ⁿ.
    Raw(i64),
    /// Decimal digits typed after `C-u` (`C-u 2`).
    Numeric { value: i64, open: bool },
}

impl PrefixArgument {
    /// The number the argument stands for: 4ⁿ for `C-u` alone, else the digits.
    pub(super) const fn value(self) -> i64 {
        let (Self::Raw(value) | Self::Numeric { value, .. }) = self;
        value
    }

    /// How many times a counting command repeats ([`repeat_count`]).
    pub(super) fn count(self) -> usize {
        repeat_count(self.value()) as usize
    }

    /// The argument after one more `C-u`, as in Emacs.
    pub(super) const fn after_universal(argument: Option<Self>) -> Self {
        match argument {
            None | Some(Self::Numeric { open: false, .. }) => Self::Raw(4),
            Some(Self::Raw(value)) => Self::Raw(value.saturating_mul(4)),
            Some(Self::Numeric { value, open: true }) => Self::Numeric { value, open: false },
        }
    }

    /// The argument after one more decimal digit, or `None` once the number is closed.
    pub(super) fn after_digit(self, digit: u32) -> Option<Self> {
        let digit = i64::from(digit);
        match self {
            Self::Raw(_) => Some(Self::Numeric {
                value: digit,
                open: true,
            }),
            Self::Numeric { value, open: true } => Some(Self::Numeric {
                value: value.saturating_mul(10).saturating_add(digit),
                open: true,
            }),
            Self::Numeric { open: false, .. } => None,
        }
    }

    /// The argument after `M-<digit>` (`digit-argument`).
    pub(super) fn after_meta_digit(argument: Option<Self>, digit: u32) -> Self {
        match argument {
            Some(open @ Self::Numeric { open: true, .. }) => {
                open.after_digit(digit).unwrap_or(Self::Numeric {
                    value: i64::from(digit),
                    open: true,
                })
            }
            None | Some(Self::Raw(_) | Self::Numeric { open: false, .. }) => Self::Numeric {
                value: i64::from(digit),
                open: true,
            },
        }
    }

    /// `C-u 4`: how the HUD names the argument, alone or before a half-typed key sequence.
    pub(super) fn label(self) -> String {
        format!("C-u {}", self.value())
    }

    pub(super) fn hud(self) -> String {
        format!("{} — awaiting command (C-g cancels)", self.label())
    }
}

trust_contract_fn! {
    /// How many times a counting command repeats for a prefix argument of
    /// `value`: zero (and below) counts as one, and no count exceeds 10,000,
    /// the clamp every counting command has always had.
    #[expect(
        clippy::manual_clamp,
        reason = "Trust kernel-imports a select, not a call to `Ord::clamp` (E6 refuses calls)"
    )]
    fn repeat_count(value: i64) -> i64
    clauses [ensures result >= 1 && result <= 10000 by repeat_count_within_bounds
             ensures value < 1 || value > 10000 || result == value by repeat_count_keeps_an_in_range_value]
    {
        if value < 1 {
            1
        } else if value > 10_000 {
            10_000
        } else {
            value
        }
    }
}

verify_island! {
    clean {
        def i64_order_key (v : BitVec 64) : Nat := Nat.xor (BitVec.toNat v) 9223372036854775808

        theorem repeat_count_one_le_cap :
            Nat.le (i64_order_key (BitVec.ofNat 64 1)) (i64_order_key (BitVec.ofNat 64 10000)) := by
          decide

        theorem repeat_count_within_bounds (value : BitVec 64) :
            And (Nat.le (i64_order_key (BitVec.ofNat 64 1)) (i64_order_key (trust_import_amacs__editor__command__repeat_count value)))
                (Nat.le (i64_order_key (trust_import_amacs__editor__command__repeat_count value)) (i64_order_key (BitVec.ofNat 64 10000))) :=
          @Decidable.casesOn (Nat.lt (i64_order_key value) (i64_order_key (BitVec.ofNat 64 1)))
            (fun (d : Decidable (Nat.lt (i64_order_key value) (i64_order_key (BitVec.ofNat 64 1)))) =>
              And (Nat.le (i64_order_key (BitVec.ofNat 64 1))
                    (i64_order_key (@ite (BitVec 64) (Nat.lt (i64_order_key value) (i64_order_key (BitVec.ofNat 64 1))) d (BitVec.ofNat 64 1)
                      (@ite (BitVec 64) (Nat.lt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value))
                        (Nat.decLt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value)) (BitVec.ofNat 64 10000) value))))
                  (Nat.le (i64_order_key (@ite (BitVec 64) (Nat.lt (i64_order_key value) (i64_order_key (BitVec.ofNat 64 1))) d (BitVec.ofNat 64 1)
                      (@ite (BitVec 64) (Nat.lt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value))
                        (Nat.decLt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value)) (BitVec.ofNat 64 10000) value)))
                    (i64_order_key (BitVec.ofNat 64 10000))))
            (Nat.decLt (i64_order_key value) (i64_order_key (BitVec.ofNat 64 1)))
            (fun (at_least_one : Not (Nat.lt (i64_order_key value) (i64_order_key (BitVec.ofNat 64 1)))) =>
              @Decidable.casesOn (Nat.lt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value))
                (fun (e : Decidable (Nat.lt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value))) =>
                  And (Nat.le (i64_order_key (BitVec.ofNat 64 1))
                        (i64_order_key (@ite (BitVec 64) (Nat.lt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value)) e (BitVec.ofNat 64 10000) value)))
                      (Nat.le (i64_order_key (@ite (BitVec 64) (Nat.lt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value)) e (BitVec.ofNat 64 10000) value))
                        (i64_order_key (BitVec.ofNat 64 10000))))
                (Nat.decLt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value))
                (fun (at_most_cap : Not (Nat.lt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value))) =>
                  And.intro (Nat.ge_of_not_lt at_least_one) (Nat.ge_of_not_lt at_most_cap))
                (fun (_ : Nat.lt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value)) =>
                  And.intro repeat_count_one_le_cap (Nat.le_refl (i64_order_key (BitVec.ofNat 64 10000)))))
            (fun (_ : Nat.lt (i64_order_key value) (i64_order_key (BitVec.ofNat 64 1))) =>
              And.intro (Nat.le_refl (i64_order_key (BitVec.ofNat 64 1))) repeat_count_one_le_cap)

        theorem repeat_count_keeps_an_in_range_value (value : BitVec 64) :
            Or (Or (Nat.lt (i64_order_key value) (i64_order_key (BitVec.ofNat 64 1))) (Nat.lt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value)))
               (Eq (trust_import_amacs__editor__command__repeat_count value) value) :=
          @Decidable.casesOn (Nat.lt (i64_order_key value) (i64_order_key (BitVec.ofNat 64 1)))
            (fun (d : Decidable (Nat.lt (i64_order_key value) (i64_order_key (BitVec.ofNat 64 1)))) =>
              Or (Or (Nat.lt (i64_order_key value) (i64_order_key (BitVec.ofNat 64 1))) (Nat.lt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value)))
                 (Eq (@ite (BitVec 64) (Nat.lt (i64_order_key value) (i64_order_key (BitVec.ofNat 64 1))) d (BitVec.ofNat 64 1)
                       (@ite (BitVec 64) (Nat.lt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value))
                         (Nat.decLt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value)) (BitVec.ofNat 64 10000) value))
                     value))
            (Nat.decLt (i64_order_key value) (i64_order_key (BitVec.ofNat 64 1)))
            (fun (_ : Not (Nat.lt (i64_order_key value) (i64_order_key (BitVec.ofNat 64 1)))) =>
              @Decidable.casesOn (Nat.lt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value))
                (fun (e : Decidable (Nat.lt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value))) =>
                  Or (Or (Nat.lt (i64_order_key value) (i64_order_key (BitVec.ofNat 64 1))) (Nat.lt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value)))
                     (Eq (@ite (BitVec 64) (Nat.lt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value)) e (BitVec.ofNat 64 10000) value) value))
                (Nat.decLt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value))
                (fun (_ : Not (Nat.lt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value))) => Or.inr (Eq.refl value))
                (fun (above_cap : Nat.lt (i64_order_key (BitVec.ofNat 64 10000)) (i64_order_key value)) => Or.inl (Or.inr above_cap)))
            (fun (below_one : Nat.lt (i64_order_key value) (i64_order_key (BitVec.ofNat 64 1))) => Or.inl (Or.inl below_one))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorCommand {
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
    /// TAB: indent to the next tab stop.
    IndentForTab,
    SetMark,
    KillRegion,
    KillLine,
    Yank,
    YankPop,
    Undo,
    Redo,
    /// `C-c u`: undo the newest agent edit, wherever it sits in the history.
    UndoAgentEdit,
    Save,
    Abort,
    UniversalArgument,
    ExecuteCommand,
    IncrementalSearch,
    SearchNext,
    SearchPrevious,
    GotoLine,
    SwitchBuffer,
    RevertBuffer,
    /// Keep the buffer's text over the file that changed on disk under it
    /// (the GUI header row's Keep Mine): Save then writes over the file.
    KeepBufferOverDisk,
    StartMacro,
    EndMacro,
    PlayMacro,
    /// Page Down (and `C-v` off Windows, `PAGE_CHORDS`).
    ScrollPageDown,
    /// Page Up (and `M-v` off Windows): the view and every caret a screen up.
    ScrollPageUp,
    /// `C-x k`: close a buffer's view (this one by default), saving its draft first.
    KillBuffer,
    /// `C-x C-c`: in the GUI, close this view (a terminal face exits).
    SaveBuffersKillTerminal,
    /// `C-c C-c` and `C-x #`: the edit a waiting client asked for is done.
    FinishEdit,
    /// `C-c C-k`: the edit a waiting client asked for is abandoned.
    AbortEdit,
    /// `M-<`: point to the start of the buffer, leaving the mark behind.
    BeginningOfBuffer,
    /// `M->`: point to the end of the buffer, leaving the mark behind.
    EndOfBuffer,
    /// `C-l`: scroll so the caret's line cycles through middle, top, and bottom.
    RecenterTopBottom,
    /// `M-d`: kill to the end of the word.
    KillWord,
    /// `M-DEL`: kill to the start of the word.
    BackwardKillWord,
    /// `M-w`: copy the region to the kill ring, leaving the text.
    KillRingSave,
    /// `C-x C-x`: swap point and mark, activating the region.
    ExchangePointAndMark,
    /// `C-x h`: the region is the whole buffer, point at its start.
    MarkWholeBuffer,
    /// `C-u C-SPC`: jump back to the last mark set in this buffer, cycling through earlier marks.
    PopMark,
    /// `C-o`: a newline after the caret, which stays put.
    OpenLine,
    /// `C-t`: swap the characters around the caret.
    TransposeChars,
    /// Enter: a newline at every caret.
    Newline,
    /// `M-q`: refill the paragraph at the caret to the fill column.
    FillParagraph,
    /// `M-0` … `M-9`: begin or extend a numeric argument.
    DigitArgument,
    /// `C-c C-t` (and ⌘E in a GUI): flip a table between its text and its grid.
    ToggleGridView,
    /// `C-o` in a grid: an empty row below the current one, the cursor on it.
    GridInsertRow,
    /// `C-S-backspace` in a grid: delete the current row.
    GridDeleteRow,
}

/// Authority class required to execute an editor command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommandAuthority {
    /// Moves the caret, the view or the mark, or opens a minibuffer.
    Read,
    /// Changes the document's text in memory, as one undo unit.
    Edit,
    /// Writes the document to its file, or closes its view (which saves a changed draft first).
    Save,
    /// Opens or writes a file the command names (S9's find-file).
    Files,
    /// Puts document text on the system clipboard, and reaches the kill ring that mirrors it:
    /// input without it kills to and yanks from a ring of its own.
    Clipboard,
    /// Runs a program (P2's filter-region); never reachable from the wire.
    Spawn,
}

impl CommandAuthority {
    /// All authority classes, granted to interactive window input.
    pub const ALL: [Self; 6] = [
        Self::Read,
        Self::Edit,
        Self::Save,
        Self::Files,
        Self::Clipboard,
        Self::Spawn,
    ];

    /// Returns the authority class name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Edit => "edit",
            Self::Save => "save",
            Self::Files => "files",
            Self::Clipboard => "clipboard",
            Self::Spawn => "spawn",
        }
    }
}

/// Minibuffer argument prompt requested by an editor command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prompt {
    /// `switch-to-buffer`: an open buffer's name or file URI.
    BufferName,
    /// `goto-line`: a line number, from 1.
    LineNumber,
    /// `kill-buffer`: an open buffer's name or file URI; empty is this one.
    BufferToKill,
}

impl Prompt {
    /// The minibuffer's label while it asks.
    pub const fn label(self) -> &'static str {
        match self {
            Self::BufferName => "Switch buffer: ",
            Self::LineNumber => "Goto line: ",
            Self::BufferToKill => "Kill buffer (default this buffer): ",
        }
    }
}

/// An editor command invocation with optional repeat count and prompt arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub command: EditorCommand,
    pub count: Option<i64>,
    pub args: Vec<String>,
}

impl Invocation {
    /// `command` with no argument and no answers yet.
    pub const fn new(command: EditorCommand) -> Self {
        Self {
            command,
            count: None,
            args: Vec::new(),
        }
    }

    /// The prompt still to be answered, if any.
    pub fn next_prompt(&self) -> Option<Prompt> {
        self.command.prompts().get(self.args.len()).copied()
    }
}

impl EditorCommand {
    /// Every command, in the order a blank M-x lists them (Save first).
    pub const ALL: [Self; 56] = [
        Self::Save,
        Self::Undo,
        Self::Redo,
        Self::UndoAgentEdit,
        Self::IncrementalSearch,
        Self::SearchNext,
        Self::SearchPrevious,
        Self::GotoLine,
        Self::SwitchBuffer,
        Self::RevertBuffer,
        Self::KeepBufferOverDisk,
        Self::ToggleGridView,
        Self::GridInsertRow,
        Self::GridDeleteRow,
        Self::SetMark,
        Self::KillRegion,
        Self::KillLine,
        Self::KillWord,
        Self::BackwardKillWord,
        Self::KillRingSave,
        Self::Yank,
        Self::YankPop,
        Self::FinishEdit,
        Self::AbortEdit,
        Self::KillBuffer,
        Self::SaveBuffersKillTerminal,
        Self::MoveBackward,
        Self::MoveForward,
        Self::MoveLineUp,
        Self::MoveLineDown,
        Self::ScrollPageDown,
        Self::ScrollPageUp,
        Self::RecenterTopBottom,
        Self::MoveLineStart,
        Self::MoveLineEnd,
        Self::MoveWordBackward,
        Self::MoveWordForward,
        Self::BeginningOfBuffer,
        Self::EndOfBuffer,
        Self::ExchangePointAndMark,
        Self::MarkWholeBuffer,
        Self::PopMark,
        Self::DeleteBackward,
        Self::DeleteForward,
        Self::Newline,
        Self::OpenLine,
        Self::TransposeChars,
        Self::IndentForTab,
        Self::FillParagraph,
        Self::Abort,
        Self::UniversalArgument,
        Self::DigitArgument,
        Self::ExecuteCommand,
        Self::StartMacro,
        Self::EndMacro,
        Self::PlayMacro,
    ];

    /// Looks up a command by its exact Emacs name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|command| command.name() == name)
    }

    /// Returns the authority class required by this command.
    pub const fn authority(self) -> CommandAuthority {
        use CommandAuthority as A;
        match self {
            Self::MoveBackward
            | Self::MoveForward
            | Self::MoveLineUp
            | Self::MoveLineDown
            | Self::MoveLineStart
            | Self::MoveLineEnd
            | Self::MoveWordBackward
            | Self::MoveWordForward
            | Self::ScrollPageDown
            | Self::ScrollPageUp
            | Self::RecenterTopBottom
            | Self::BeginningOfBuffer
            | Self::EndOfBuffer
            | Self::SetMark
            | Self::ExchangePointAndMark
            | Self::MarkWholeBuffer
            | Self::PopMark
            | Self::Abort
            | Self::UniversalArgument
            | Self::DigitArgument
            | Self::ExecuteCommand
            | Self::IncrementalSearch
            | Self::SearchNext
            | Self::SearchPrevious
            | Self::GotoLine
            | Self::SwitchBuffer
            | Self::StartMacro
            | Self::EndMacro
            | Self::ToggleGridView => A::Read,
            Self::DeleteBackward
            | Self::DeleteForward
            | Self::IndentForTab
            | Self::KillRegion
            | Self::KillLine
            | Self::KillWord
            | Self::BackwardKillWord
            | Self::Yank
            | Self::YankPop
            | Self::Undo
            | Self::Redo
            | Self::UndoAgentEdit
            | Self::Newline
            | Self::OpenLine
            | Self::TransposeChars
            | Self::FillParagraph
            | Self::RevertBuffer
            | Self::KeepBufferOverDisk
            | Self::GridInsertRow
            | Self::GridDeleteRow
            | Self::PlayMacro => A::Edit,
            Self::Save
            | Self::FinishEdit
            | Self::AbortEdit
            | Self::KillBuffer
            | Self::SaveBuffersKillTerminal => A::Save,
            Self::KillRingSave => A::Clipboard,
        }
    }

    /// Whether this command takes text out of the document onto a kill ring (and so, for the
    /// person's input, the clipboard).
    #[must_use]
    pub const fn kills(self) -> bool {
        matches!(
            self,
            Self::KillRegion
                | Self::KillLine
                | Self::KillWord
                | Self::BackwardKillWord
                | Self::KillRingSave
        )
    }

    /// Returns the sequence of prompts required by this command.
    pub const fn prompts(self) -> &'static [Prompt] {
        match self {
            Self::SwitchBuffer => &[Prompt::BufferName],
            Self::GotoLine => &[Prompt::LineNumber],
            Self::KillBuffer => &[Prompt::BufferToKill],
            _ => &[],
        }
    }

    pub const fn name(&self) -> &'static str {
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
            Self::IndentForTab => "indent-for-tab-command",
            Self::SetMark => "set-mark-command",
            Self::KillRegion => "kill-region",
            Self::KillLine => "kill-line",
            Self::Yank => "yank",
            Self::YankPop => "yank-pop",
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::UndoAgentEdit => "undo-agent-edit",
            Self::Save => "save-buffer",
            Self::Abort => "keyboard-quit",
            Self::UniversalArgument => "universal-argument",
            Self::ExecuteCommand => "execute-extended-command",
            Self::IncrementalSearch => "isearch-forward",
            Self::SearchNext => "search-next",
            Self::SearchPrevious => "search-previous",
            Self::GotoLine => "goto-line",
            Self::SwitchBuffer => "switch-to-buffer",
            Self::RevertBuffer => "revert-buffer",
            Self::KeepBufferOverDisk => "keep-buffer-over-disk",
            Self::StartMacro => "start-kbd-macro",
            Self::EndMacro => "end-kbd-macro",
            Self::PlayMacro => "call-last-kbd-macro",
            Self::ScrollPageDown => "scroll-up-command",
            Self::ScrollPageUp => "scroll-down-command",
            Self::KillBuffer => "kill-buffer",
            Self::SaveBuffersKillTerminal => "save-buffers-kill-terminal",
            Self::FinishEdit => "finish-edit",
            Self::AbortEdit => "abort-edit",
            Self::BeginningOfBuffer => "beginning-of-buffer",
            Self::EndOfBuffer => "end-of-buffer",
            Self::RecenterTopBottom => "recenter-top-bottom",
            Self::KillWord => "kill-word",
            Self::BackwardKillWord => "backward-kill-word",
            Self::KillRingSave => "kill-ring-save",
            Self::ExchangePointAndMark => "exchange-point-and-mark",
            Self::MarkWholeBuffer => "mark-whole-buffer",
            Self::PopMark => "pop-to-mark-command",
            Self::OpenLine => "open-line",
            Self::TransposeChars => "transpose-chars",
            Self::Newline => "newline",
            Self::FillParagraph => "fill-paragraph",
            Self::DigitArgument => "digit-argument",
            Self::ToggleGridView => "toggle-grid-view",
            Self::GridInsertRow => "grid-insert-row",
            Self::GridDeleteRow => "grid-delete-row",
        }
    }

    /// Whether the command runs in a grid view: motion (which moves by cells), saving and
    /// closing, undo and redo (an agent's edit too), the prompts, revert and Keep Mine, the flip,
    /// the kills, copy and yank (which take the cell's value), and the row commands (`C-o` adds a
    /// row). A command that edits raw text answers "not in the grid view" there and changes
    /// nothing; DEL and C-d clear the cell.
    pub const fn allowed_in_grid(self) -> bool {
        match self {
            Self::MoveBackward
            | Self::MoveForward
            | Self::MoveLineUp
            | Self::MoveLineDown
            | Self::MoveLineStart
            | Self::MoveLineEnd
            | Self::MoveWordBackward
            | Self::MoveWordForward
            | Self::ScrollPageDown
            | Self::ScrollPageUp
            | Self::RecenterTopBottom
            | Self::BeginningOfBuffer
            | Self::EndOfBuffer
            | Self::DeleteBackward
            | Self::DeleteForward
            | Self::Undo
            | Self::Redo
            | Self::UndoAgentEdit
            | Self::Save
            | Self::FinishEdit
            | Self::AbortEdit
            | Self::KillBuffer
            | Self::SaveBuffersKillTerminal
            | Self::Abort
            | Self::UniversalArgument
            | Self::DigitArgument
            | Self::ExecuteCommand
            | Self::IncrementalSearch
            | Self::SearchNext
            | Self::SearchPrevious
            | Self::GotoLine
            | Self::SwitchBuffer
            | Self::RevertBuffer
            | Self::KeepBufferOverDisk
            | Self::ToggleGridView
            | Self::KillRegion
            | Self::KillLine
            | Self::KillRingSave
            | Self::Yank
            | Self::OpenLine
            | Self::GridInsertRow
            | Self::GridDeleteRow => true,
            Self::IndentForTab
            | Self::SetMark
            | Self::YankPop
            | Self::StartMacro
            | Self::EndMacro
            | Self::PlayMacro
            | Self::KillWord
            | Self::BackwardKillWord
            | Self::ExchangePointAndMark
            | Self::MarkWholeBuffer
            | Self::PopMark
            | Self::TransposeChars
            | Self::Newline
            | Self::FillParagraph => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorCompletionAction {
    Previous,
    Next,
    Complete,
    Choose(usize),
}

/// Returns commands matching `query` by prefix or fuzzy ranking.
pub fn command_completions(query: &str) -> Vec<EditorCommand> {
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
                let first = first.unwrap_or(0);
                return Some((
                    3,
                    index.saturating_sub(first.saturating_add(needle.len()).saturating_sub(1)),
                    name.len(),
                ));
            }
        }
    }
    None
}

impl EditorWorkspace {
    pub(super) fn remember_command(&mut self, command: &str) {
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

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
