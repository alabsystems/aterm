// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Closed table of editor commands, authority classes, prompts, and command history.

use super::workspace::EditorWorkspace;

const COMMAND_HISTORY_LIMIT: usize = 200;
const COMMAND_COMPLETION_LIMIT: usize = 8;

/// Columns between tab stops.
pub const TAB_STOP: usize = 4;

/// A pending `C-u` argument: Emacs's raw prefix argument, waiting for the one
/// command that takes it.
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

    /// The argument after one more `C-u`, as in Emacs: it multiplies a bare
    /// `C-u` by four, closes a number being typed, and starts over at four
    /// after a closed one.
    pub(super) const fn after_universal(argument: Option<Self>) -> Self {
        match argument {
            None | Some(Self::Numeric { open: false, .. }) => Self::Raw(4),
            Some(Self::Raw(value)) => Self::Raw(value.saturating_mul(4)),
            Some(Self::Numeric { value, open: true }) => Self::Numeric { value, open: false },
        }
    }

    /// The argument after one more decimal digit, or `None` once the number
    /// is closed.
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

    /// The argument after `M-<digit>` (`digit-argument`): a number being
    /// typed takes the digit, and anything else starts a new number with it,
    /// which more digits, plain or `M-`, then extend (`M-1 2` is twelve).
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

    /// `C-u 4`: how the HUD names the argument, alone or before a half-typed
    /// key sequence.
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
    StartMacro,
    EndMacro,
    PlayMacro,
    /// Page Down (and `C-v` off Windows, `PAGE_CHORDS`): the view and every
    /// caret a screen down, less two lines of context (Emacs'
    /// `next-screen-context-lines`).
    ScrollPageDown,
    /// Page Up (and `M-v` off Windows): the same, a screen up.
    ScrollPageUp,
    /// `C-x k`: close a buffer's view (this one by default), saving its
    /// draft first as closing any view does.
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
    /// `C-l`: scroll so the caret's line is in the middle, then at the top,
    /// then at the bottom, as the key repeats.
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
    /// `C-u C-SPC`: point back to the last mark set in this buffer, cycling
    /// through the earlier ones as it repeats.
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
}

/// Authority class required to execute an editor command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommandAuthority {
    /// Moves the caret, the view or the mark, or opens a minibuffer; the
    /// document's bytes stay as they are.
    Read,
    /// Changes the document's text in memory, as one undo unit.
    Edit,
    /// Writes the document to its file, or closes its view (which saves a
    /// changed draft first).
    Save,
    /// Opens or writes a file the command names (S9's find-file).
    Files,
    /// Puts document text on the system clipboard: `kill-ring-save`, and
    /// the clipboard copy of every kill
    /// ([`super::EditorEffect::KillToClipboard`]), which an input without
    /// this class never gets.
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
    pub const ALL: [Self; 51] = [
        Self::Save,
        Self::Undo,
        Self::Redo,
        Self::IncrementalSearch,
        Self::SearchNext,
        Self::SearchPrevious,
        Self::GotoLine,
        Self::SwitchBuffer,
        Self::RevertBuffer,
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
            | Self::EndMacro => A::Read,
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
            | Self::Newline
            | Self::OpenLine
            | Self::TransposeChars
            | Self::FillParagraph
            | Self::RevertBuffer
            | Self::PlayMacro => A::Edit,
            Self::Save
            | Self::FinishEdit
            | Self::AbortEdit
            | Self::KillBuffer
            | Self::SaveBuffersKillTerminal => A::Save,
            Self::KillRingSave => A::Clipboard,
        }
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
        // Keep a constant score so blank queries stay in curated table order.
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
        // Prefix matches tie as a class and fall back to curated table order.
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
        // Punctuation-only queries fall back to the curated blank-query order.
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
mod tests {
    use super::*;
    use crate::editor::projection::byte_of_line;
    use crate::editor::{
        CloseWhy, EditorBufferView, EditorEffect, EditorLines, KeyChord, KeyResolution, Keymap,
        Minibuffer, Platform, Profile, Selection, edit, editor, fill,
    };
    use crate::store::DocumentStore;

    #[test]
    fn repeat_count_is_the_clamp_it_replaced() {
        for value in [i64::MIN, -1, 0, 1, 2, 9_999, 10_000, 10_001, i64::MAX] {
            assert_eq!(repeat_count(value), value.clamp(1, 10_000));
        }
    }

    /// One C-u argument goes to exactly one command.
    #[test]
    fn universal_argument_is_consumed_once() {
        let text = (0..10)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let (mut store, mut workspace, mut view, document) = editor(&text);
        let press = |store: &mut DocumentStore,
                     workspace: &mut EditorWorkspace,
                     view: &mut EditorBufferView,
                     key: &str| {
            workspace
                .handle_chord(
                    store,
                    view,
                    KeyChord::parse(key).expect("test chord parses"),
                )
                .unwrap()
        };

        press(&mut store, &mut workspace, &mut view, "C-u");
        workspace.insert_text(&mut store, &mut view, "4").unwrap();
        assert_eq!(
            view.prefix_hud.as_deref(),
            Some("C-u 4 — awaiting command (C-g cancels)")
        );
        press(&mut store, &mut workspace, &mut view, "C-n");
        assert_eq!(view.primary_selection().head, byte_of_line(&text, 4));
        assert_eq!(view.prefix_hud, None);
        press(&mut store, &mut workspace, &mut view, "C-f");
        assert_eq!(view.primary_selection().head, byte_of_line(&text, 4) + 1);
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), text);

        view.selections = vec![Selection::caret(0)];
        press(&mut store, &mut workspace, &mut view, "C-u");
        press(&mut store, &mut workspace, &mut view, "C-n");
        assert_eq!(view.primary_selection().head, byte_of_line(&text, 4));
        press(&mut store, &mut workspace, &mut view, "C-f");
        assert_eq!(view.primary_selection().head, byte_of_line(&text, 4) + 1);

        view.selections = vec![Selection::caret(0)];
        press(&mut store, &mut workspace, &mut view, "C-u");
        press(&mut store, &mut workspace, &mut view, "C-x");
        let hud = view.prefix_hud.clone().unwrap_or_default();
        assert!(hud.starts_with("C-u 4 C-x — "), "{hud}");
        let effects = press(&mut store, &mut workspace, &mut view, "C-s");
        assert!(matches!(
            effects.as_slice(),
            [EditorEffect::SaveDocument { .. }]
        ));
        press(&mut store, &mut workspace, &mut view, "C-f");
        assert_eq!(view.primary_selection().head, 1);
        assert_eq!(view.prefix_hud, None);

        for (digits, count) in [(None, 4), (Some("2"), 2)] {
            view.selections = vec![Selection::caret(0)];
            press(&mut store, &mut workspace, &mut view, "C-u");
            if let Some(digits) = digits {
                workspace
                    .insert_text(&mut store, &mut view, digits)
                    .unwrap();
            }
            let effects = press(&mut store, &mut workspace, &mut view, "M-x");
            assert_eq!(effects, [EditorEffect::ShowCommands]);
            workspace
                .insert_text(&mut store, &mut view, "forward-char")
                .unwrap();
            workspace.submit_minibuffer(&mut store, &mut view).unwrap();
            assert_eq!(view.primary_selection().head, count);
            assert_eq!(view.prefix_hud, None);
            press(&mut store, &mut workspace, &mut view, "C-f");
            assert_eq!(view.primary_selection().head, count + 1);
            assert_eq!(store.snapshot(document).unwrap().text.as_ref(), text);
        }

        view.selections = vec![Selection::caret(0)];
        press(&mut store, &mut workspace, &mut view, "C-u");
        press(&mut store, &mut workspace, &mut view, "M-x");
        workspace
            .insert_text(&mut store, &mut view, "save-buffer")
            .unwrap();
        let effects = workspace.submit_minibuffer(&mut store, &mut view).unwrap();
        assert!(matches!(
            effects.as_slice(),
            [EditorEffect::SaveDocument { .. }]
        ));
        press(&mut store, &mut workspace, &mut view, "C-f");
        assert_eq!(view.primary_selection().head, 1);
        assert_eq!(view.prefix_hud, None);

        view.selections = vec![Selection::caret(0)];
        press(&mut store, &mut workspace, &mut view, "C-u");
        press(&mut store, &mut workspace, &mut view, "M-x");
        press(&mut store, &mut workspace, &mut view, "C-g");
        press(&mut store, &mut workspace, &mut view, "C-f");
        assert_eq!(view.primary_selection().head, 1);
        assert_eq!(view.prefix_hud, None);
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), text);

        for keys in [&["M-z"][..], &["C-x", "z"][..]] {
            view.selections = vec![Selection::caret(0)];
            press(&mut store, &mut workspace, &mut view, "C-u");
            let mut effects = Vec::new();
            for &key in keys {
                effects = press(&mut store, &mut workspace, &mut view, key);
            }
            assert!(
                matches!(
                    effects.as_slice(),
                    [EditorEffect::Status(status)] if status.ends_with("is not bound")
                ),
                "C-u {keys:?}: {effects:?}"
            );
            assert_eq!(view.prefix_hud, None);
            press(&mut store, &mut workspace, &mut view, "C-f");
            assert_eq!(view.primary_selection().head, 1, "after C-u {keys:?}");
        }

        view.selections = vec![Selection::caret(0)];
        press(&mut store, &mut workspace, &mut view, "C-u");
        press(&mut store, &mut workspace, &mut view, "2");
        press(&mut store, &mut workspace, &mut view, "C-k");
        let after = store.snapshot(document).unwrap().text;
        assert_eq!(after.as_ref(), &text[byte_of_line(&text, 2)..]);
        assert_eq!(
            workspace.kill_ring.front().map(String::as_str),
            Some("line 0\nline 1\n")
        );

        workspace
            .execute(&mut store, &mut view, EditorCommand::UniversalArgument)
            .unwrap();
        workspace.insert_text(&mut store, &mut view, "x").unwrap();
        workspace
            .execute(&mut store, &mut view, EditorCommand::MoveForward)
            .unwrap();
        assert_eq!(view.primary_selection().head, 2);
        assert!(
            store
                .snapshot(document)
                .unwrap()
                .text
                .starts_with("xline 2\n")
        );
    }

    /// A second C-u closes a typed number, as in Emacs: C-u 1 C-u 3 types a 3
    /// rather than making the argument 13, and a C-u after a closed number
    /// starts over at four.
    #[test]
    fn a_second_universal_argument_closes_the_number() {
        let (mut store, mut workspace, mut view, document) = editor("ab");
        view.selections = vec![Selection::caret(1)];
        let universal = |store: &mut DocumentStore,
                         workspace: &mut EditorWorkspace,
                         view: &mut EditorBufferView| {
            workspace
                .execute(store, view, EditorCommand::UniversalArgument)
                .unwrap();
        };
        universal(&mut store, &mut workspace, &mut view);
        workspace.insert_text(&mut store, &mut view, "1").unwrap();
        universal(&mut store, &mut workspace, &mut view);
        assert_eq!(
            view.prefix_hud.as_deref(),
            Some("C-u 1 — awaiting command (C-g cancels)")
        );
        workspace.insert_text(&mut store, &mut view, "3").unwrap();
        assert_eq!(store.snapshot(document).unwrap().text.as_ref(), "a3b");
        assert_eq!(view.prefix_hud, None);

        universal(&mut store, &mut workspace, &mut view);
        workspace.insert_text(&mut store, &mut view, "2").unwrap();
        universal(&mut store, &mut workspace, &mut view);
        universal(&mut store, &mut workspace, &mut view);
        assert_eq!(
            view.prefix_hud.as_deref(),
            Some("C-u 4 — awaiting command (C-g cancels)")
        );
    }

    /// Press `keys`, space-separated chords, and answer the last one's effects.
    fn keys(
        store: &mut DocumentStore,
        workspace: &mut EditorWorkspace,
        view: &mut EditorBufferView,
        keys: &str,
    ) -> Vec<EditorEffect> {
        let mut effects = Vec::new();
        for key in keys.split_whitespace() {
            effects = workspace
                .handle_chord(
                    store,
                    view,
                    KeyChord::parse(key).expect("test chord parses"),
                )
                .unwrap();
        }
        effects
    }

    fn text(store: &DocumentStore, document: crate::store::DocumentId) -> String {
        store.snapshot(document).unwrap().text.to_string()
    }

    /// Every command has exactly the authority this table pins: a change to
    /// one is a decision, made here on purpose.
    #[test]
    fn authority_table_is_total_and_pinned() {
        use CommandAuthority::{Clipboard, Edit, Read, Save};
        let pinned: &[(&str, CommandAuthority)] = &[
            ("save-buffer", Save),
            ("undo", Edit),
            ("redo", Edit),
            ("isearch-forward", Read),
            ("search-next", Read),
            ("search-previous", Read),
            ("goto-line", Read),
            ("switch-to-buffer", Read),
            ("revert-buffer", Edit),
            ("set-mark-command", Read),
            ("kill-region", Edit),
            ("kill-line", Edit),
            ("kill-word", Edit),
            ("backward-kill-word", Edit),
            ("kill-ring-save", Clipboard),
            ("yank", Edit),
            ("yank-pop", Edit),
            ("finish-edit", Save),
            ("abort-edit", Save),
            ("kill-buffer", Save),
            ("save-buffers-kill-terminal", Save),
            ("backward-char", Read),
            ("forward-char", Read),
            ("previous-line", Read),
            ("next-line", Read),
            ("scroll-up-command", Read),
            ("scroll-down-command", Read),
            ("recenter-top-bottom", Read),
            ("move-beginning-of-line", Read),
            ("move-end-of-line", Read),
            ("backward-word", Read),
            ("forward-word", Read),
            ("beginning-of-buffer", Read),
            ("end-of-buffer", Read),
            ("exchange-point-and-mark", Read),
            ("mark-whole-buffer", Read),
            ("pop-to-mark-command", Read),
            ("backward-delete-char", Edit),
            ("delete-char", Edit),
            ("newline", Edit),
            ("open-line", Edit),
            ("transpose-chars", Edit),
            ("indent-for-tab-command", Edit),
            ("fill-paragraph", Edit),
            ("keyboard-quit", Read),
            ("universal-argument", Read),
            ("digit-argument", Read),
            ("execute-extended-command", Read),
            ("start-kbd-macro", Read),
            ("end-kbd-macro", Read),
            ("call-last-kbd-macro", Edit),
        ];
        let table = EditorCommand::ALL
            .iter()
            .map(|command| (command.name(), command.authority()))
            .collect::<Vec<_>>();
        assert_eq!(table, pinned, "the table lists every command, in M-x order");
        assert!(EditorCommand::ALL.iter().all(|command| !matches!(
            command.authority(),
            CommandAuthority::Files | CommandAuthority::Spawn
        )));
        let mut moved = table.clone();
        let copied = moved
            .iter_mut()
            .find(|(name, _)| *name == "kill-ring-save")
            .unwrap();
        copied.1 = Edit;
        assert_ne!(moved, pinned);
    }

    /// `from_name` finds every command by its exact name and nothing else:
    /// not a prefix, not another case, not a name with spaces around it.
    #[test]
    fn from_name_round_trips_every_command() {
        let mut names = std::collections::BTreeSet::new();
        for command in EditorCommand::ALL {
            assert!(names.insert(command.name()), "{} twice", command.name());
            assert_eq!(EditorCommand::from_name(command.name()), Some(command));
        }
        assert_eq!(names.len(), EditorCommand::ALL.len());
        for near in [
            "kill",
            "Kill-Buffer",
            " kill-buffer",
            "kill-buffer ",
            "",
            "kill buffer",
        ] {
            assert_eq!(EditorCommand::from_name(near), None, "{near:?}");
        }
    }

    /// M-x lists every command: its exact name puts it first, and the new
    /// P0 commands are found by the words people type for them.
    #[test]
    fn m_x_lists_every_command() {
        for command in EditorCommand::ALL {
            assert_eq!(
                command_completions(command.name()).first(),
                Some(&command),
                "{}",
                command.name()
            );
        }
        assert_eq!(
            command_completions("kill"),
            [
                EditorCommand::KillRegion,
                EditorCommand::KillLine,
                EditorCommand::KillWord,
                EditorCommand::KillRingSave,
                EditorCommand::KillBuffer,
                EditorCommand::BackwardKillWord,
                EditorCommand::SaveBuffersKillTerminal,
            ]
        );
    }

    /// An Invocation runs a command with its count and its prompts'
    /// answers in one call; with no answers a prompting command asks for
    /// them, and with the wrong number it runs nothing.
    #[test]
    fn an_invocation_carries_its_count_and_answers() {
        let text = "one\ntwo\nthree\nfour\n";
        let (mut store, mut workspace, mut view, document) = editor(text);
        let invocation = |command, count, args: &[&str]| Invocation {
            command,
            count,
            args: args.iter().map(|arg| arg.to_string()).collect(),
        };
        workspace
            .invoke(
                &mut store,
                &mut view,
                invocation(EditorCommand::MoveLineDown, Some(2), &[]),
            )
            .unwrap();
        assert_eq!(view.primary_selection().head, byte_of_line(text, 2));
        let effects = workspace
            .invoke(
                &mut store,
                &mut view,
                invocation(EditorCommand::GotoLine, None, &["4"]),
            )
            .unwrap();
        assert_eq!(effects, [EditorEffect::Status("Line 4".to_string())]);
        assert_eq!(view.primary_selection().head, byte_of_line(text, 3));
        assert!(!view.minibuffer_active());
        let effects = workspace
            .invoke(
                &mut store,
                &mut view,
                invocation(EditorCommand::KillBuffer, None, &[""]),
            )
            .unwrap();
        assert_eq!(
            effects,
            [EditorEffect::CloseView {
                why: CloseWhy::Plain
            }]
        );
        let effects = workspace
            .invoke(
                &mut store,
                &mut view,
                invocation(EditorCommand::KillBuffer, None, &["other.md"]),
            )
            .unwrap();
        assert_eq!(
            effects,
            [EditorEffect::KillBuffer {
                query: "other.md".to_string()
            }]
        );
        let effects = workspace
            .invoke(
                &mut store,
                &mut view,
                invocation(EditorCommand::Save, None, &["x"]),
            )
            .unwrap();
        assert_eq!(
            effects,
            [EditorEffect::Status(
                "save-buffer takes 0 arguments, not 1".to_string()
            )]
        );
        workspace
            .invoke(
                &mut store,
                &mut view,
                invocation(EditorCommand::KillBuffer, Some(3), &[]),
            )
            .unwrap();
        assert_eq!(
            view.minibuffer,
            Minibuffer::Prompt {
                invocation: invocation(EditorCommand::KillBuffer, Some(3), &[]),
                query: String::new(),
            }
        );
        assert_eq!(
            view.minibuffer.prompt_label(),
            Some("Kill buffer (default this buffer): ")
        );
        assert_eq!(text_of(&store, document), text);
    }

    fn text_of(store: &DocumentStore, document: crate::store::DocumentId) -> String {
        text(store, document)
    }

    /// `kill-buffer` (C-x k) asks which buffer, this one by default: Enter
    /// alone closes this view, and a name asks the host for that buffer.
    #[test]
    fn kill_buffer_asks_and_defaults_to_this_buffer() {
        let (mut store, mut workspace, mut view, document) = editor("keep");
        keys(&mut store, &mut workspace, &mut view, "C-x k");
        assert!(matches!(view.minibuffer, Minibuffer::Prompt { .. }));
        let effects = workspace.submit_minibuffer(&mut store, &mut view).unwrap();
        assert_eq!(
            effects,
            [EditorEffect::CloseView {
                why: CloseWhy::Plain
            }]
        );
        keys(&mut store, &mut workspace, &mut view, "C-x k");
        workspace
            .insert_text(&mut store, &mut view, "notes.md")
            .unwrap();
        workspace.minibuffer_backspace(&store, &mut view).unwrap();
        workspace.insert_text(&mut store, &mut view, "d").unwrap();
        let effects = workspace.submit_minibuffer(&mut store, &mut view).unwrap();
        assert_eq!(
            effects,
            [EditorEffect::KillBuffer {
                query: "notes.md".to_string()
            }]
        );
        assert!(!view.minibuffer_active());
        keys(&mut store, &mut workspace, &mut view, "C-x k C-g");
        assert!(!view.minibuffer_active());
        assert_eq!(text(&store, document), "keep");
        assert_eq!(workspace.kill_ring.len(), 0);
    }

    /// `save-buffers-kill-terminal` (C-x C-c) closes the view; `finish-edit`
    /// (C-c C-c, C-x #) and `abort-edit` (C-c C-k) ask the host to answer
    /// the client waiting on the edit.
    #[test]
    fn closing_keys_ask_the_host_to_close_the_view() {
        let (mut store, mut workspace, mut view, document) = editor("text");
        for (sequence, why) in [
            ("C-x C-c", CloseWhy::Plain),
            ("C-c C-c", CloseWhy::Finish),
            ("C-x #", CloseWhy::Finish),
            ("C-c C-k", CloseWhy::Abort),
        ] {
            assert_eq!(
                keys(&mut store, &mut workspace, &mut view, sequence),
                [EditorEffect::CloseView { why }],
                "{sequence}"
            );
        }
        assert_eq!(text(&store, document), "text");
    }

    /// `beginning-of-buffer` (M-<) and `end-of-buffer` (M->) go to the ends
    /// and leave the mark where the caret was, so C-u C-SPC comes back; an
    /// active region stretches instead.
    #[test]
    fn buffer_ends_leave_the_mark_behind() {
        let (mut store, mut workspace, mut view, _) = editor("alpha\nbeta\ngamma");
        view.selections = vec![Selection::caret(7)];
        let effects = keys(&mut store, &mut workspace, &mut view, "M->");
        assert_eq!(effects, [EditorEffect::Status("Mark set".to_string())]);
        assert_eq!(view.selections, [Selection::caret(16)]);
        assert!(!view.mark_active);
        keys(&mut store, &mut workspace, &mut view, "C-u C-Space");
        assert_eq!(view.selections, [Selection::caret(7)]);
        keys(&mut store, &mut workspace, &mut view, "M-S-,");
        assert_eq!(view.selections, [Selection::caret(0)]);
        view.selections = vec![Selection::caret(7)];
        keys(&mut store, &mut workspace, &mut view, "C-Space M-<");
        assert_eq!(view.selections, [Selection { anchor: 7, head: 0 }]);
        assert!(view.mark_active);
        keys(&mut store, &mut workspace, &mut view, "M-S-.");
        assert_eq!(
            view.selections,
            [Selection {
                anchor: 7,
                head: 16
            }]
        );
    }

    /// `recenter-top-bottom` (C-l) puts the caret's line in the middle, then
    /// at the top, then at the bottom, and starts over after another key;
    /// `C-u 0 C-l` is the top row and `C-u -1`… counts from the bottom.
    #[test]
    fn recenter_puts_the_line_middle_top_then_bottom() {
        let text = (0..100)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let (mut store, mut workspace, mut view, _) = editor(&text);
        view.reconcile_viewport(&text, 10);
        view.selections = vec![Selection::caret(byte_of_line(&text, 50))];
        let lines = EditorLines::scanning(&text);
        let top = |view: &EditorBufferView| lines.number_at(view.viewport_anchor);
        for expected in [45, 50, 41, 45] {
            keys(&mut store, &mut workspace, &mut view, "C-l");
            assert_eq!(top(&view), expected);
        }
        keys(&mut store, &mut workspace, &mut view, "C-l C-f C-b C-l");
        assert_eq!(top(&view), 45);
        keys(&mut store, &mut workspace, &mut view, "C-u 0 C-l");
        assert_eq!(top(&view), 50);
        keys(&mut store, &mut workspace, &mut view, "C-u 3 C-l");
        assert_eq!(top(&view), 47);
        view.selections = vec![Selection::caret(text.len())];
        keys(&mut store, &mut workspace, &mut view, "C-l C-l");
        assert_eq!(top(&view), 91);
        assert_eq!(
            view.primary_selection().head,
            text.len(),
            "C-l never moves the caret"
        );
    }

    /// `kill-word` (M-d) and `backward-kill-word` (M-DEL, and C-backspace off
    /// macOS) kill by the words M-f and M-b move over, and take a count.
    #[test]
    fn kill_word_kills_by_words() {
        let (mut store, mut workspace, mut view, document) = editor("alpha beta gamma delta");
        view.selections = vec![Selection::caret(6)];
        keys(&mut store, &mut workspace, &mut view, "M-d");
        assert_eq!(text(&store, document), "alpha  gamma delta");
        assert_eq!(view.selections, [Selection::caret(6)]);
        assert_eq!(
            workspace.kill_ring.front().map(String::as_str),
            Some("beta")
        );
        keys(&mut store, &mut workspace, &mut view, "C-e M-backspace");
        assert_eq!(text(&store, document), "alpha  gamma ");
        assert_eq!(
            workspace.kill_ring.front().map(String::as_str),
            Some("delta")
        );
        keys(&mut store, &mut workspace, &mut view, "C-a C-u 2 M-d");
        assert_eq!(text(&store, document), " ");
        assert_eq!(
            workspace.kill_ring.front().map(String::as_str),
            Some("alpha  gamma")
        );
        assert_eq!(
            keys(&mut store, &mut workspace, &mut view, "C-e M-d"),
            [EditorEffect::Status("End of buffer".to_string())]
        );
        let windows = Keymap::emacs(Profile::Gui(Platform::Windows));
        assert_eq!(
            windows.resolve(&[KeyChord::parse("C-backspace").unwrap()]),
            KeyResolution::Command(EditorCommand::BackwardKillWord)
        );
    }

    /// `kill-ring-save` (M-w) copies the region to the kill ring and leaves
    /// the text; the region is done with, and with none it says so.
    #[test]
    fn kill_ring_save_copies_the_region() {
        let (mut store, mut workspace, mut view, document) = editor("copy this text");
        view.selections = vec![Selection::caret(5)];
        keys(&mut store, &mut workspace, &mut view, "C-Space M-f M-w");
        assert_eq!(text(&store, document), "copy this text");
        assert_eq!(
            workspace.kill_ring.front().map(String::as_str),
            Some("this")
        );
        assert_eq!(view.selections, [Selection::caret(9)]);
        assert!(!view.mark_active);
        assert_eq!(
            keys(&mut store, &mut workspace, &mut view, "M-w"),
            [EditorEffect::Status("No region selected".to_string())]
        );
        keys(&mut store, &mut workspace, &mut view, "C-a C-y");
        assert_eq!(text(&store, document), "thiscopy this text");
    }

    /// Every kill, and `kill-ring-save`, hands the kill ring's newest entry
    /// to the face for the system clipboard: the joined entry after a kill
    /// that appends, and a macro's once, when it is done.
    #[test]
    fn kills_reach_the_clipboard_only_with_clipboard_authority() {
        let clipboard = |effects: Vec<EditorEffect>| {
            effects
                .into_iter()
                .filter_map(|effect| match effect {
                    EditorEffect::KillToClipboard { text } => Some(text),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let (mut store, mut workspace, mut view, document) = editor("one\ntwo\n");
        assert_eq!(
            clipboard(keys(&mut store, &mut workspace, &mut view, "C-k")),
            ["one"]
        );
        assert_eq!(
            clipboard(keys(&mut store, &mut workspace, &mut view, "C-k")),
            ["one\n"]
        );
        assert!(clipboard(keys(&mut store, &mut workspace, &mut view, "C-f")).is_empty());
        view.selections = vec![Selection { anchor: 0, head: 3 }];
        assert_eq!(
            clipboard(keys(&mut store, &mut workspace, &mut view, "M-w")),
            ["two"]
        );

        workspace.granted = vec![
            CommandAuthority::Read,
            CommandAuthority::Edit,
            CommandAuthority::Save,
        ];
        assert!(clipboard(keys(&mut store, &mut workspace, &mut view, "C-k")).is_empty());
        assert_eq!(text(&store, document), "two");
        assert_eq!(workspace.kill_ring.front().map(String::as_str), Some("\n"));
        workspace.granted = CommandAuthority::ALL.to_vec();
        assert!(clipboard(keys(&mut store, &mut workspace, &mut view, "C-a")).is_empty());

        let (mut store, mut workspace, mut view, _) = editor("a b c d");
        keys(&mut store, &mut workspace, &mut view, "C-x ( M-d M-d C-x )");
        assert_eq!(
            clipboard(keys(&mut store, &mut workspace, &mut view, "C-x e")),
            [" c d"]
        );
        assert_eq!(workspace.kill_ring, [" c d", "a b"]);
    }

    /// A command runs only when the input carries every authority it needs,
    /// whichever path runs it: its key, M-x, an [`Invocation`] or a macro
    /// that replays it.
    #[test]
    fn a_command_runs_only_with_the_authority_it_needs() {
        let refusal = || {
            vec![EditorEffect::Status(
                "kill-ring-save needs clipboard authority, which this input does not carry"
                    .to_string(),
            )]
        };
        let (mut store, mut workspace, mut view, document) = editor("copy this text");
        keys(
            &mut store,
            &mut workspace,
            &mut view,
            "C-x ( C-Space M-f M-w C-x )",
        );
        assert_eq!(workspace.kill_ring.len(), 1);
        workspace.kill_ring.clear();
        workspace.granted = vec![
            CommandAuthority::Read,
            CommandAuthority::Edit,
            CommandAuthority::Save,
        ];
        view.selections = vec![Selection::caret(5)];
        assert_eq!(
            keys(&mut store, &mut workspace, &mut view, "C-Space M-f C-u M-w"),
            refusal()
        );
        assert_eq!(view.prefix_argument, None);
        assert!(view.mark_active, "the region survives the refusal");
        keys(&mut store, &mut workspace, &mut view, "M-x");
        workspace
            .insert_text(&mut store, &mut view, "kill-ring-save")
            .unwrap();
        assert_eq!(
            workspace.submit_minibuffer(&mut store, &mut view).unwrap(),
            refusal()
        );
        assert_eq!(
            workspace
                .invoke(
                    &mut store,
                    &mut view,
                    Invocation::new(EditorCommand::KillRingSave)
                )
                .unwrap(),
            refusal()
        );
        assert_eq!(
            workspace.authorities(EditorCommand::PlayMacro),
            [
                CommandAuthority::Read,
                CommandAuthority::Edit,
                CommandAuthority::Clipboard
            ]
        );
        assert_eq!(
            keys(&mut store, &mut workspace, &mut view, "C-x e"),
            vec![EditorEffect::Status(
                "call-last-kbd-macro needs clipboard authority, which this input does not carry"
                    .to_string(),
            )]
        );
        assert!(workspace.kill_ring.is_empty());
        keys(&mut store, &mut workspace, &mut view, "C-w");
        assert_eq!(text(&store, document), "copy  text");
        workspace.granted = CommandAuthority::ALL.to_vec();
        keys(&mut store, &mut workspace, &mut view, "C-a C-Space M-f M-w");
        assert_eq!(
            workspace.kill_ring.front().map(String::as_str),
            Some("copy")
        );
    }

    /// `exchange-point-and-mark` (C-x C-x) swaps the caret and the mark and
    /// activates the region; without a region it uses the last mark set.
    #[test]
    fn exchange_point_and_mark_swaps_them() {
        let (mut store, mut workspace, mut view, document) = editor("one two three");
        assert_eq!(
            keys(&mut store, &mut workspace, &mut view, "C-x C-x"),
            [EditorEffect::Status(
                "No mark set in this buffer".to_string()
            )]
        );
        view.selections = vec![Selection::caret(4)];
        keys(&mut store, &mut workspace, &mut view, "C-Space M-f M-f");
        assert_eq!(
            view.selections,
            [Selection {
                anchor: 4,
                head: 13
            }]
        );
        keys(&mut store, &mut workspace, &mut view, "C-x C-x");
        assert_eq!(
            view.selections,
            [Selection {
                anchor: 13,
                head: 4
            }]
        );
        assert!(view.mark_active);
        keys(&mut store, &mut workspace, &mut view, "C-g C-e C-x C-x");
        assert_eq!(
            view.selections,
            [Selection {
                anchor: 13,
                head: 4
            }]
        );
        assert!(view.mark_active);
        assert_eq!(text(&store, document), "one two three");
    }

    /// `mark-whole-buffer` (C-x h) makes the whole buffer the region, caret
    /// at the start, and C-w then kills it all.
    #[test]
    fn mark_whole_buffer_selects_everything() {
        let (mut store, mut workspace, mut view, document) = editor("all\nof it");
        view.selections = vec![Selection::caret(5)];
        let effects = keys(&mut store, &mut workspace, &mut view, "C-x h");
        assert_eq!(effects, [EditorEffect::Status("Mark set".to_string())]);
        assert_eq!(view.selections, [Selection { anchor: 9, head: 0 }]);
        assert!(view.mark_active);
        keys(&mut store, &mut workspace, &mut view, "C-w");
        assert_eq!(text(&store, document), "");
        assert_eq!(
            workspace.kill_ring.front().map(String::as_str),
            Some("all\nof it")
        );
        assert_eq!(
            workspace.global_mark_ring.front().map(|mark| mark.offset),
            Some(0)
        );
    }

    /// `pop-to-mark-command` (C-u C-SPC) returns to the marks set in this
    /// buffer, most recent first, cycling; the marks move with edits made
    /// before them.
    #[test]
    fn pop_mark_cycles_through_this_buffers_marks() {
        let (mut store, mut workspace, mut view, document) = editor("a b c d e");
        assert_eq!(
            keys(&mut store, &mut workspace, &mut view, "C-u C-Space"),
            [EditorEffect::Status(
                "No mark set in this buffer".to_string()
            )]
        );
        for at in [2, 6] {
            view.selections = vec![Selection::caret(at)];
            keys(&mut store, &mut workspace, &mut view, "C-Space C-g");
        }
        view.selections = vec![Selection::caret(0)];
        workspace.insert_text(&mut store, &mut view, "__").unwrap();
        assert_eq!(text(&store, document), "__a b c d e");
        for expected in [8, 4, 8] {
            keys(&mut store, &mut workspace, &mut view, "C-u C-Space");
            assert_eq!(view.selections, [Selection::caret(expected)]);
            assert!(!view.mark_active);
        }
        workspace
            .execute(&mut store, &mut view, EditorCommand::PopMark)
            .unwrap();
        assert_eq!(view.selections, [Selection::caret(4)]);
    }

    /// `open-line` (C-o) breaks the line after the caret, which stays put;
    /// `newline` (Enter) breaks it before, and both take a count.
    #[test]
    fn open_line_and_newline_break_the_line() {
        let (mut store, mut workspace, mut view, document) = editor("ab");
        view.selections = vec![Selection::caret(1)];
        keys(&mut store, &mut workspace, &mut view, "C-o");
        assert_eq!(text(&store, document), "a\nb");
        assert_eq!(view.selections, [Selection::caret(1)]);
        keys(&mut store, &mut workspace, &mut view, "C-u 2 C-o");
        assert_eq!(text(&store, document), "a\n\n\nb");
        assert_eq!(view.selections, [Selection::caret(1)]);
        workspace
            .execute(&mut store, &mut view, EditorCommand::Newline)
            .unwrap();
        assert_eq!(text(&store, document), "a\n\n\n\nb");
        assert_eq!(view.selections, [Selection::caret(2)]);
        keys(&mut store, &mut workspace, &mut view, "C-u 2");
        workspace
            .execute(&mut store, &mut view, EditorCommand::Newline)
            .unwrap();
        assert_eq!(text(&store, document), "a\n\n\n\n\n\nb");
        assert_eq!(view.selections, [Selection::caret(4)]);
        keys(&mut store, &mut workspace, &mut view, "C-/");
        assert_eq!(text(&store, document), "a\n\n\n\nb");

        let (mut store, mut workspace, mut view, document) = editor("ab\r\ncd\r\n");
        view.selections = vec![Selection::caret(1)];
        keys(&mut store, &mut workspace, &mut view, "C-o");
        assert_eq!(text(&store, document), "a\r\nb\r\ncd\r\n");
        assert_eq!(view.selections, [Selection::caret(1)]);
        workspace
            .execute(&mut store, &mut view, EditorCommand::Newline)
            .unwrap();
        assert_eq!(text(&store, document), "a\r\n\r\nb\r\ncd\r\n");
        assert_eq!(view.selections, [Selection::caret(3)]);
    }

    /// A CRLF line's text ends before its `\r` (design §2.2 keeps the
    /// ending byte-exact): C-e, C-n and C-p stop there, C-k kills the text
    /// and then the whole CRLF, and C-t at the end swaps the two characters
    /// before it.
    #[test]
    fn crlf_line_ends_stop_before_the_carriage_return() {
        let (mut store, mut workspace, mut view, document) = editor("abc\r\nd\r\nefg");
        keys(&mut store, &mut workspace, &mut view, "C-e");
        assert_eq!(view.selections, [Selection::caret(3)]);
        keys(&mut store, &mut workspace, &mut view, "C-n");
        assert_eq!(view.selections, [Selection::caret(6)]);
        keys(&mut store, &mut workspace, &mut view, "C-n");
        assert_eq!(view.selections, [Selection::caret(11)]);
        keys(&mut store, &mut workspace, &mut view, "C-p C-p");
        assert_eq!(view.selections, [Selection::caret(3)]);
        workspace.insert_text(&mut store, &mut view, "x").unwrap();
        assert_eq!(text(&store, document), "abcx\r\nd\r\nefg");

        let (mut store, mut workspace, mut view, document) = editor("ab\r\ncd\r\n");
        keys(&mut store, &mut workspace, &mut view, "C-k");
        assert_eq!(text(&store, document), "\r\ncd\r\n");
        keys(&mut store, &mut workspace, &mut view, "C-k");
        assert_eq!(text(&store, document), "cd\r\n");
        assert_eq!(
            workspace.kill_ring.front().map(String::as_str),
            Some("ab\r\n")
        );

        let (mut store, mut workspace, mut view, document) = editor("ab\r\ncd");
        keys(&mut store, &mut workspace, &mut view, "C-e C-t");
        assert_eq!(text(&store, document), "ba\r\ncd");
        assert_eq!(view.selections, [Selection::caret(2)]);
    }

    /// `transpose-chars` (C-t) drags the character before the caret over
    /// the one after it; at a line's end it swaps the two before; at the
    /// start of the buffer it refuses.
    #[test]
    fn transpose_chars_swaps_around_the_caret() {
        let (mut store, mut workspace, mut view, document) = editor("abcd\nxy");
        view.selections = vec![Selection::caret(1)];
        keys(&mut store, &mut workspace, &mut view, "C-t");
        assert_eq!(text(&store, document), "bacd\nxy");
        assert_eq!(view.selections, [Selection::caret(2)]);
        keys(&mut store, &mut workspace, &mut view, "C-e C-t");
        assert_eq!(text(&store, document), "badc\nxy");
        assert_eq!(view.selections, [Selection::caret(4)]);
        view.selections = vec![Selection::caret(1)];
        keys(&mut store, &mut workspace, &mut view, "C-u 2 C-t");
        assert_eq!(text(&store, document), "adbc\nxy");
        assert_eq!(view.selections, [Selection::caret(3)]);
        view.selections = vec![Selection::caret(0)];
        assert_eq!(
            keys(&mut store, &mut workspace, &mut view, "C-t"),
            [EditorEffect::Status("Beginning of buffer".to_string())]
        );
        let (mut store, mut workspace, mut view, document) = editor("e\u{301}x");
        view.selections = vec![Selection::caret(3)];
        keys(&mut store, &mut workspace, &mut view, "C-t");
        assert_eq!(text(&store, document), "xe\u{301}");
    }

    /// `indent-for-tab-command` (TAB) reads the workspace's options: the tab
    /// width, and a tab character when they indent with tabs.
    #[test]
    fn tab_reads_the_indent_options() {
        let (mut store, mut workspace, mut view, document) = editor("ab");
        workspace.options.tab_width = 8;
        view.selections = vec![Selection::caret(2)];
        keys(&mut store, &mut workspace, &mut view, "Tab");
        assert_eq!(text(&store, document), "ab      ");
        workspace.options.indent_tabs = true;
        keys(&mut store, &mut workspace, &mut view, "Tab");
        assert_eq!(text(&store, document), "ab      \t");
        assert_eq!(edit::tab_stop_columns(5, 4), 3);
        assert_eq!(edit::tab_stop_columns(8, 4), 4);
        assert_eq!(
            edit::tab_stop_columns(3, 0),
            1,
            "a zero width is one column"
        );
    }

    /// `fill-paragraph` (M-q) refills the paragraph at the caret to the fill
    /// column: a comment as a comment, a list item hanging under its text,
    /// and the next paragraph left alone.
    #[test]
    fn fill_paragraph_fills_to_the_fill_column() {
        let text = "Subject\n\nThe body of a commit message that someone typed as one very long line without any breaks.\n\nNext paragraph.\n";
        let (mut store, mut workspace, mut view, document) = editor(text);
        workspace.options.fill_column = 30;
        let caret = text.find("someone").unwrap() + 2;
        view.selections = vec![Selection::caret(caret)];
        keys(&mut store, &mut workspace, &mut view, "M-q");
        let filled = text_of(&store, document);
        assert_eq!(
            filled,
            "Subject\n\nThe body of a commit message\nthat someone typed as one very\nlong line without any breaks.\n\nNext paragraph.\n"
        );
        assert_eq!(&filled[view.primary_selection().head..][..5], "meone");
        let seq = store.snapshot(document).unwrap().seq;
        keys(&mut store, &mut workspace, &mut view, "M-q");
        assert_eq!(store.snapshot(document).unwrap().seq, seq);
        keys(&mut store, &mut workspace, &mut view, "C-/");
        assert_eq!(text_of(&store, document), text);

        let comment = "    // A comment that runs on and on past the column\n    // and then some more.\nfn main() {}\n";
        let (mut store, mut workspace, mut view, document) = editor(comment);
        workspace.options.fill_column = 32;
        view.selections = vec![Selection::caret(10)];
        keys(&mut store, &mut workspace, &mut view, "M-q");
        assert_eq!(
            text_of(&store, document),
            "    // A comment that runs on\n    // and on past the column\n    // and then some more.\nfn main() {}\n"
        );

        let list = "- an item whose text is long enough to wrap\n- a second item\n";
        let (mut store, mut workspace, mut view, document) = editor(list);
        workspace.options.fill_column = 20;
        keys(&mut store, &mut workspace, &mut view, "M-q");
        assert_eq!(
            text_of(&store, document),
            "- an item whose text\n  is long enough to\n  wrap\n- a second item\n"
        );

        let crlf = "aaa bbb\r\nccc ddd\r\n\r\nnext\r\n";
        let (mut store, mut workspace, mut view, document) = editor(crlf);
        keys(&mut store, &mut workspace, &mut view, "M-q");
        assert_eq!(text_of(&store, document), "aaa bbb ccc ddd\r\n\r\nnext\r\n");
        workspace.options.fill_column = 8;
        keys(&mut store, &mut workspace, &mut view, "M-q");
        assert_eq!(text_of(&store, document), crlf);
        let (mut store, mut workspace, mut view, document) = editor("x\r\n\r\nthree four\r");
        workspace.options.fill_column = 6;
        view.selections = vec![Selection::caret(6)];
        keys(&mut store, &mut workspace, &mut view, "M-q");
        assert_eq!(text_of(&store, document), "x\r\n\r\nthree\r\nfour\r");

        let (mut store, mut workspace, mut view, _) = editor("a\n\nb");
        view.selections = vec![Selection::caret(2)];
        assert_eq!(
            keys(&mut store, &mut workspace, &mut view, "M-q"),
            [EditorEffect::Status(
                "No paragraph to fill here".to_string()
            )]
        );
    }

    /// The fill's one arithmetic decision, exhaustively over small widths:
    /// a word fits in the room a line leaves exactly when the line, a space
    /// and the word stay within the fill column, and nothing overflows at
    /// the top of the range.
    #[test]
    fn word_fits_is_room_for_a_space_and_the_word() {
        let fits =
            |line: u64, word: u64, column: u64| fill::word_fits(column.saturating_sub(line), word);
        for line in 0..12_u64 {
            for word in 0..12_u64 {
                for column in 0..12_u64 {
                    assert_eq!(
                        fits(line, word, column),
                        u128::from(line) + 1 + u128::from(word) <= u128::from(column),
                        "{line} {word} {column}"
                    );
                }
            }
        }
        assert!(!fits(u64::MAX, u64::MAX, u64::MAX));
        assert!(fits(0, u64::MAX - 1, u64::MAX));
        assert!(!fits(1, u64::MAX - 1, u64::MAX));
    }

    /// Redo has Emacs's `C-?` (and the GUI's Ctrl+Shift+/), and `M-<digit>`
    /// types a numeric argument as `C-u <digits>` does, which plain digits
    /// then extend.
    #[test]
    fn redo_key_and_digit_argument() {
        let (mut store, mut workspace, mut view, document) = editor("");
        workspace.insert_text(&mut store, &mut view, "x").unwrap();
        keys(&mut store, &mut workspace, &mut view, "C-/");
        assert_eq!(text(&store, document), "");
        keys(&mut store, &mut workspace, &mut view, "C-?");
        assert_eq!(text(&store, document), "x");
        keys(&mut store, &mut workspace, &mut view, "C-/ C-S-/");
        assert_eq!(text(&store, document), "x");

        let text = "abcdefghijklmnopqrstuvwxyz";
        let (mut store, mut workspace, mut view, _) = editor(text);
        keys(&mut store, &mut workspace, &mut view, "M-3");
        assert_eq!(
            view.prefix_hud.as_deref(),
            Some("C-u 3 — awaiting command (C-g cancels)")
        );
        keys(&mut store, &mut workspace, &mut view, "C-f");
        assert_eq!(view.primary_selection().head, 3);
        keys(&mut store, &mut workspace, &mut view, "M-1 2 C-f");
        assert_eq!(view.primary_selection().head, 15);
        keys(&mut store, &mut workspace, &mut view, "M-1 M-0 C-b");
        assert_eq!(view.primary_selection().head, 5);
        assert_eq!(view.prefix_hud, None);
        assert_eq!(
            workspace
                .execute(&mut store, &mut view, EditorCommand::DigitArgument)
                .unwrap(),
            [EditorEffect::Status(
                "digit-argument is typed as M-0 … M-9".to_string()
            )]
        );
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
}
