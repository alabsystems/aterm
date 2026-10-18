// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Block-based output model for agent-friendly terminal interaction.
//!
//! Provides [`OutputBlock`] and [`BlockState`] for representing command-output
//! pairs as discrete, addressable units.
//!
//! Terminal query/manipulation APIs that consume these types live in
//! `aterm-core`'s terminal shell/block accessors.

use crate::shell_types::current_time_ms;

/// The state of an output block.
///
/// Blocks represent atomic units of command-output pairs in the terminal.
/// This is the foundation for agent-friendly terminal interaction where
/// commands and their outputs can be treated as discrete, addressable units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BlockState {
    /// Block contains only prompt (user hasn't typed command yet).
    PromptOnly,
    /// User is typing a command.
    EnteringCommand,
    /// Command is executing (output may be streaming).
    Executing,
    /// Command has completed with exit code.
    Complete,
}

/// An output block representing a command and its output as an atomic unit.
///
/// Blocks are the fundamental abstraction for agent workflows:
/// - Each block contains prompt + command + output
/// - Blocks are independently addressable
/// - Navigation can jump between blocks
/// - Copy operations can target specific parts
///
/// Row coordinates use absolute row numbers (u64) that survive scrollback eviction.
/// Use `Grid::visible_to_absolute()` to convert screen-relative coordinates.
///
/// # Example
///
/// ```text
/// ┌─────────────────────────────────────────┐
/// │ Block 0 (complete, exit_code=0)         │
/// │ $ git status                            │  ← prompt + command
/// │ On branch main                          │  ← output
/// │ nothing to commit                       │
/// ├─────────────────────────────────────────┤
/// │ Block 1 (complete, exit_code=1)         │
/// │ $ cargo build                           │
/// │ error[E0382]: use of moved value        │
/// └─────────────────────────────────────────┘
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputBlock {
    /// Unique identifier for this block within the session.
    pub id: u64,
    /// Current state of this block.
    pub state: BlockState,
    /// Row where the prompt started (absolute line number).
    pub prompt_start_row: u64,
    /// Column where the prompt started.
    pub prompt_start_col: u16,
    /// Row where the command text started (absolute line number).
    pub command_start_row: Option<u64>,
    /// Column where the command text started.
    pub command_start_col: Option<u16>,
    /// Row where the command output started (absolute line number).
    pub output_start_row: Option<u64>,
    /// Row where the block ends (exclusive, absolute line number).
    pub end_row: Option<u64>,
    /// Exit code of the command (only if Complete).
    pub exit_code: Option<i32>,
    /// Working directory at time of command.
    ///
    /// Uses `Box<str>` instead of `String` to save 8 bytes.
    pub working_directory: Option<Box<str>>,
    /// Explicit commandline text (from OSC 633 ; E).
    ///
    /// Set by VS Code shell integration when the shell reports the command text
    /// explicitly, rather than relying on screen scraping.
    pub commandline: Option<Box<str>>,
    /// Whether the output portion of this block is collapsed.
    ///
    /// When collapsed, only the prompt+command is visible; output is hidden.
    /// This is purely metadata - the UI layer uses this to control rendering.
    pub collapsed: bool,
    /// A full clear of the main screen (ED 2, or ED 0 from home: `clear`,
    /// Ctrl-L) blanked rows this block reads its text from after its command
    /// finished: a prompt had archived it, or its D had come and the next
    /// prompt not yet.
    ///
    /// Whatever was written on those rows since is not its own (the next
    /// command's, or the shell's before its next prompt), so its row text is
    /// gone as surely as evicted history: `aterm-core` reads it as
    /// `BlockText::Evicted`. The block itself (its id, state and exit code)
    /// stays. An archived block keeps its rows; one whose next prompt has not
    /// come yet moves the rows the clear blanked to the cleared screen's top
    /// row, so the next prompt's row (its end) is never above its start.
    pub text_cleared: bool,
    /// This block's command rows were lost while it was being built, and they
    /// were a non-empty range then: a full clear of the main screen blanked
    /// some of them while its command ran (`clear; make`, a program that
    /// clears the screen it writes to), or a scrollback erase (ED 3, Cmd-K)
    /// took some of them before its command finished.
    ///
    /// `aterm-core` then moves the rows it lost onto what is left (the cleared
    /// screen's top row, or the oldest row the erase kept), where its command
    /// goes on writing; its output reads from there. Its command line is not
    /// among its rows any more, so `aterm-core` reads its command as
    /// `BlockText::Evicted` unless OSC 633;E named it.
    pub command_text_cleared: bool,
    /// Timestamp when prompt was received (milliseconds since Unix epoch).
    ///
    /// Captured when the block is created (OSC 133;A). Used by AI agents
    /// to understand how long ago the prompt appeared.
    pub prompt_time_ms: Option<u64>,
    /// Timestamp when command input started (milliseconds since Unix epoch).
    ///
    /// Captured when OSC 133;B is processed (prompt ended, user typing command).
    pub command_input_start_time_ms: Option<u64>,
    /// Timestamp when command execution started (milliseconds since Unix epoch).
    ///
    /// Captured when OSC 133;C is processed (user pressed enter, command running).
    pub command_exec_start_time_ms: Option<u64>,
    /// Timestamp when command execution ended (milliseconds since Unix epoch).
    ///
    /// Captured when OSC 133;D is processed (command finished, exit code available).
    pub command_end_time_ms: Option<u64>,
}

impl OutputBlock {
    /// Create a new block starting at the given position.
    ///
    /// # Arguments
    ///
    /// * `id` - Unique identifier for this block
    /// * `row` - Absolute row number (use `Grid::visible_to_absolute()` to convert)
    /// * `col` - Column position
    #[must_use]
    pub fn new(id: u64, row: u64, col: u16) -> Self {
        Self {
            id,
            state: BlockState::PromptOnly,
            prompt_start_row: row,
            prompt_start_col: col,
            command_start_row: None,
            command_start_col: None,
            output_start_row: None,
            end_row: None,
            exit_code: None,
            working_directory: None,
            commandline: None,
            collapsed: false,
            text_cleared: false,
            command_text_cleared: false,
            prompt_time_ms: current_time_ms(),
            command_input_start_time_ms: None,
            command_exec_start_time_ms: None,
            command_end_time_ms: None,
        }
    }

    /// Check if this block is complete (has finished executing).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.state == BlockState::Complete
    }

    /// Check if the command in this block succeeded (exit code 0).
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// Check if the command in this block failed (exit code != 0).
    #[must_use]
    pub fn failed(&self) -> bool {
        matches!(self.exit_code, Some(code) if code != 0)
    }

    /// Absolute `(start, end_exclusive)` rows of the command portion of this
    /// block.
    #[must_use]
    pub fn command_rows(&self) -> Option<(u64, u64)> {
        let start = self.command_start_row?;
        let end = self
            .output_start_row
            .unwrap_or(self.end_row.unwrap_or(start.saturating_add(1)));
        Some((start, end))
    }

    /// Absolute `(start, end_exclusive)` rows of the output portion of this
    /// block.
    #[must_use]
    pub fn output_rows(&self) -> Option<(u64, u64)> {
        let start = self.output_start_row?;
        let end = self.end_row.unwrap_or(start.saturating_add(1));
        Some((start, end))
    }
}

#[cfg(test)]
#[path = "shell_blocks_tests.rs"]
mod tests;
