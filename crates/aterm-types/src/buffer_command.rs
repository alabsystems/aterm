// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Canonical buffer navigation and editing commands.
//!
//! `BufferCommand` is the unified action vocabulary for aterm. Every keybinding
//! maps to a `BufferCommand`; AI agents issue them directly. This enum replaces
//! both the vi-mode `ViMotion` and the editor `EditorCommand` as the single
//! lingua franca for buffer interaction.

/// Canonical buffer navigation and editing commands.
///
/// Every keybinding maps to one of these. AI agents issue these directly
/// via the daemon command channel. The terminal grid, editor buffer, and
/// any future navigable surface all consume `BufferCommand`s.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BufferCommand {
    // -- Cursor movement --
    /// Move cursor forward one character (emacs: C-f, vim: l).
    ForwardChar,
    /// Move cursor backward one character (emacs: C-b, vim: h).
    BackwardChar,
    /// Move cursor to next line (emacs: C-n, vim: j).
    NextLine,
    /// Move cursor to previous line (emacs: C-p, vim: k).
    PreviousLine,
    /// Move cursor forward one semantic word (emacs: M-f, vim: w).
    ForwardWord,
    /// Move cursor backward one semantic word (emacs: M-b, vim: b).
    BackwardWord,
    /// Move cursor to end of current/next semantic word (vim: e).
    ForwardWordEnd,
    /// Move cursor to end of previous semantic word (vim: ge).
    BackwardWordEnd,
    /// Move cursor forward one WORD (whitespace-delimited) (vim: W).
    ForwardWordBig,
    /// Move cursor backward one WORD (whitespace-delimited) (vim: B).
    BackwardWordBig,
    /// Move cursor to end of current/next WORD (vim: E).
    ForwardWordEndBig,
    /// Move cursor to end of previous WORD (vim: gE).
    BackwardWordEndBig,
    /// Move cursor to beginning of line (emacs: C-a, vim: 0).
    BeginningOfLine,
    /// Move cursor to end of line (emacs: C-e, vim: $).
    EndOfLine,
    /// Move cursor to first non-blank character on line (vim: ^).
    FirstNonBlank,

    // -- Logical line navigation --
    /// Move to the beginning of the logical (unwrapped) line (emacs: M-a).
    BeginningOfLogicalLine,
    /// Move to the end of the logical (unwrapped) line (emacs: M-e).
    EndOfLogicalLine,

    // -- Screen movement --
    /// Scroll viewport up one page (emacs: M-v, vim: C-b).
    ScrollUp,
    /// Scroll viewport down one page (emacs: C-v, vim: C-f).
    ScrollDown,
    /// Move cursor to top of buffer (emacs: M-<, vim: gg).
    GotoTop,
    /// Move cursor to bottom of buffer (emacs: M->, vim: G).
    GotoBottom,
    /// Move cursor to top of visible screen (vim: H).
    ScreenTop,
    /// Move cursor to middle of visible screen (vim: M).
    ScreenMiddle,
    /// Move cursor to bottom of visible screen (vim: L).
    ScreenBottom,
    /// Recenter the screen around the cursor (emacs: C-l).
    RecenterScreen,
    /// Move cursor to a specific line number (emacs: M-g g, vim: `:<n>`).
    GotoLine(u32),

    // -- Search --
    /// Begin forward incremental search (emacs: C-s, vim: /).
    SearchForward(String),
    /// Begin backward incremental search (emacs: C-r, vim: ?).
    SearchBackward(String),
    /// Begin forward regex search (emacs: C-M-s).
    SearchRegexForward(String),
    /// Begin backward regex search (emacs: C-M-r).
    SearchRegexBackward(String),
    /// Repeat last search forward (emacs: C-s again, vim: n).
    SearchNext,
    /// Repeat last search backward (emacs: C-r again, vim: N).
    SearchPrevious,
    /// Find character forward on current line (vim: `f<char>`).
    InlineFind(char),
    /// Find character backward on current line (vim: `F<char>`).
    InlineFindBack(char),
    /// Move to just before character forward on current line (vim: `t<char>`).
    InlineTill(char),
    /// Move to just after character backward on current line (vim: `T<char>`).
    InlineTillBack(char),
    /// Repeat last inline search in same direction (vim: ;).
    RepeatInlineSearch,
    /// Repeat last inline search in reverse direction (vim: ,).
    ReverseInlineSearch,

    // -- Selection / mark --
    /// Set mark at current position (emacs: C-SPC, vim: v).
    SetMark,
    /// Swap point and mark (emacs: C-x C-x).
    Exchange,
    /// Enter line-wise selection mode (vim: V).
    SelectLine,
    /// Enter block/column selection mode (vim: C-v).
    SelectBlock,
    /// Select entire buffer (emacs: C-x h).
    SelectAll,

    // -- Kill/yank (clipboard) --
    /// Kill from cursor to end of line (emacs: C-k, vim: `d<motion>`).
    Kill,
    /// Kill forward one word (emacs: M-d, vim: dw).
    KillWord,
    /// Kill backward one word (emacs: M-DEL, vim: db).
    BackwardKillWord,
    /// Kill entire current line (vim: dd).
    KillLine,
    /// Kill the selected region (emacs: C-w).
    KillRegion,
    /// Copy the selected region without killing (emacs: M-w, vim: `y<motion>`).
    CopyRegion,
    /// Yank (paste) from kill ring (emacs: C-y, vim: p).
    Yank,
    /// Cycle through kill ring after yank (emacs: M-y).
    YankPop,

    // -- Editing --
    /// Insert text at cursor position.
    Insert(String),
    /// Delete character at cursor (emacs: C-d, vim: x).
    Delete,
    /// Delete character before cursor (emacs: C-h / DEL).
    Backspace,
    /// Insert a newline at cursor position.
    Newline,
    /// Transpose the two characters around the cursor (emacs: C-t).
    TransposeChars,
    /// Transpose the two words around the cursor (emacs: M-t).
    TransposeWords,
    /// Capitalize the word following the cursor (emacs: M-c).
    CapitalizeWord,
    /// Convert the word following the cursor to uppercase (emacs: M-u).
    UpcaseWord,
    /// Convert the word following the cursor to lowercase (emacs: M-l).
    DowncaseWord,
    /// Undo last change (emacs: C-/, vim: u).
    Undo,
    /// Redo last undone change (vim: C-r).
    Redo,
    /// Save the current buffer (emacs: C-x C-s, vim: :w).
    Save,

    // -- Named marks (bookmarks) --
    /// Set a named mark at current position (vim: `m<char>`).
    SetNamedMark(char),
    /// Jump to exact position of named mark (vim: `` `<char> ``).
    GotoMark(char),
    /// Jump to first non-blank on line of named mark (vim: `'<char>`).
    GotoMarkLine(char),

    // -- Macros --
    /// Begin recording a macro (emacs: C-x (, vim: `q<reg>`).
    StartMacro,
    /// Stop recording current macro (emacs: C-x ), vim: q).
    EndMacro,
    /// Play back the last recorded macro (emacs: C-x e, vim: `@<reg>`).
    PlayMacro,

    // -- Buffer management --
    /// Switch to a named buffer (emacs: C-x b).
    SwitchBuffer(String),
    /// List all available buffers (emacs: C-x C-b).
    ListBuffers,

    // -- Paragraph / structural navigation --
    /// Move cursor up one paragraph (emacs: M-{, vim: {).
    ParagraphUp,
    /// Move cursor down one paragraph (emacs: M-}, vim: }).
    ParagraphDown,
    /// Jump to matching bracket/delimiter (emacs: C-M-f, vim: %).
    MatchBracket,

    // -- Command navigation (shell integration) --
    /// Jump to the next command output boundary.
    /// Leverages OSC 133 shell integration marks.
    NextCommand,
    /// Jump to the previous command output boundary.
    PreviousCommand,
    /// Select the current command's output region (set mark at output start, point at output end).
    SelectCommandOutput,
    /// Copy the current command's output to the kill ring.
    CopyCommandOutput,
    /// Jump to the next shell prompt.
    NextPrompt,
    /// Jump to the previous shell prompt.
    PreviousPrompt,

    // -- Mode transitions (vim insert-entry) --
    /// Enter insert mode at cursor (vim: i).
    EnterInsert,
    /// Enter insert mode at first non-blank of line (vim: I).
    InsertAtLineStart,
    /// Enter insert mode after cursor (vim: a).
    Append,
    /// Enter insert mode at end of line (vim: A).
    AppendAtLineEnd,
    /// Open new line below and enter insert mode (vim: o).
    OpenLineBelow,
    /// Open new line above and enter insert mode (vim: O).
    OpenLineAbove,

    // -- Buffer mode control --
    /// Enter buffer mode (switch from terminal to buffer navigation).
    EnterBufferMode,
    /// Exit buffer mode (return to live terminal).
    ExitBufferMode,
    /// Re-snapshot at the live edge without exiting buffer mode.
    Resnap,

    // -- Export --
    /// Save the buffer content to a file.
    SaveBuffer,
    /// Save the current region to a file.
    SaveRegion,
    /// Pipe the current region through a shell command.
    PipeRegion(String),

    // -- ANSI export --
    /// Save buffer/region with ANSI escape codes preserving SGR attributes.
    SaveBufferAnsi,

    // -- Window commands --
    /// Split pane vertically (emacs: C-x 2).
    WindowSplitVertical,
    /// Split pane horizontally (emacs: C-x 3).
    WindowSplitHorizontal,
    /// Close other panes (emacs: C-x 1).
    WindowCloseOthers,
    /// Close current pane (emacs: C-x 0).
    WindowCloseCurrent,
    /// Focus next pane (emacs: C-x o).
    WindowFocusNext,

    // -- Mode / control --
    /// Cancel current operation (emacs: C-g, vim: Escape).
    Cancel,
}

impl BufferCommand {
    /// Returns `true` if this command modifies buffer content.
    #[must_use]
    pub fn is_mutating(&self) -> bool {
        matches!(
            self,
            Self::Kill
                | Self::KillWord
                | Self::BackwardKillWord
                | Self::KillLine
                | Self::KillRegion
                | Self::Yank
                | Self::YankPop
                | Self::Insert(_)
                | Self::Delete
                | Self::Backspace
                | Self::Newline
                | Self::TransposeChars
                | Self::TransposeWords
                | Self::CapitalizeWord
                | Self::UpcaseWord
                | Self::DowncaseWord
                | Self::Undo
                | Self::Redo
                | Self::OpenLineBelow
                | Self::OpenLineAbove
                | Self::PipeRegion(_)
        )
    }

    /// Returns `true` if this command moves the cursor.
    #[must_use]
    pub fn is_motion(&self) -> bool {
        matches!(
            self,
            Self::ForwardChar
                | Self::BackwardChar
                | Self::NextLine
                | Self::PreviousLine
                | Self::ForwardWord
                | Self::BackwardWord
                | Self::ForwardWordEnd
                | Self::BackwardWordEnd
                | Self::ForwardWordBig
                | Self::BackwardWordBig
                | Self::ForwardWordEndBig
                | Self::BackwardWordEndBig
                | Self::BeginningOfLine
                | Self::EndOfLine
                | Self::FirstNonBlank
                | Self::BeginningOfLogicalLine
                | Self::EndOfLogicalLine
                | Self::ScrollUp
                | Self::ScrollDown
                | Self::GotoTop
                | Self::GotoBottom
                | Self::ScreenTop
                | Self::ScreenMiddle
                | Self::ScreenBottom
                | Self::GotoLine(_)
                | Self::SearchForward(_)
                | Self::SearchBackward(_)
                | Self::SearchRegexForward(_)
                | Self::SearchRegexBackward(_)
                | Self::SearchNext
                | Self::SearchPrevious
                | Self::InlineFind(_)
                | Self::InlineFindBack(_)
                | Self::InlineTill(_)
                | Self::InlineTillBack(_)
                | Self::RepeatInlineSearch
                | Self::ReverseInlineSearch
                | Self::GotoMark(_)
                | Self::GotoMarkLine(_)
                | Self::ParagraphUp
                | Self::ParagraphDown
                | Self::MatchBracket
                | Self::NextCommand
                | Self::PreviousCommand
                | Self::NextPrompt
                | Self::PreviousPrompt
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_command_is_mutating() {
        assert!(BufferCommand::Kill.is_mutating());
        assert!(BufferCommand::Insert("x".into()).is_mutating());
        assert!(BufferCommand::Delete.is_mutating());
        assert!(BufferCommand::Undo.is_mutating());
        assert!(BufferCommand::PipeRegion("sort".into()).is_mutating());

        assert!(!BufferCommand::ForwardChar.is_mutating());
        assert!(!BufferCommand::SearchNext.is_mutating());
        assert!(!BufferCommand::SetMark.is_mutating());
        assert!(!BufferCommand::NextCommand.is_mutating());
        assert!(!BufferCommand::EnterBufferMode.is_mutating());
        assert!(!BufferCommand::SaveBuffer.is_mutating());
        assert!(!BufferCommand::SaveRegion.is_mutating());
        assert!(!BufferCommand::BeginningOfLogicalLine.is_mutating());
    }

    #[test]
    fn test_buffer_command_is_motion() {
        assert!(BufferCommand::ForwardChar.is_motion());
        assert!(BufferCommand::GotoTop.is_motion());
        assert!(BufferCommand::SearchNext.is_motion());
        assert!(BufferCommand::GotoMark('a').is_motion());
        assert!(BufferCommand::NextCommand.is_motion());
        assert!(BufferCommand::PreviousCommand.is_motion());
        assert!(BufferCommand::NextPrompt.is_motion());
        assert!(BufferCommand::PreviousPrompt.is_motion());
        assert!(BufferCommand::BeginningOfLogicalLine.is_motion());
        assert!(BufferCommand::EndOfLogicalLine.is_motion());

        assert!(!BufferCommand::Kill.is_motion());
        assert!(!BufferCommand::SetMark.is_motion());
        assert!(!BufferCommand::Save.is_motion());
        assert!(!BufferCommand::SelectCommandOutput.is_motion());
        assert!(!BufferCommand::CopyCommandOutput.is_motion());
        assert!(!BufferCommand::EnterBufferMode.is_motion());
        assert!(!BufferCommand::ExitBufferMode.is_motion());
        assert!(!BufferCommand::Resnap.is_motion());
        assert!(!BufferCommand::SaveBuffer.is_motion());
        assert!(!BufferCommand::PipeRegion("wc".into()).is_motion());
    }

    #[test]
    fn test_buffer_command_clone_eq() {
        let cmd = BufferCommand::SearchForward("hello".into());
        let cloned = cmd.clone();
        assert_eq!(cmd, cloned);
    }

    #[test]
    fn test_buffer_command_debug() {
        let cmd = BufferCommand::GotoLine(42);
        let debug = format!("{cmd:?}");
        assert!(debug.contains("GotoLine"));
        assert!(debug.contains("42"));
    }
}
