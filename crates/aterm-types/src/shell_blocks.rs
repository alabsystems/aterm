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

/// Named absolute row span with exclusive end semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowSpan {
    /// Absolute start row, inclusive.
    pub start_row: u64,
    /// Absolute end row, exclusive.
    pub end_row_exclusive: u64,
}

impl RowSpan {
    /// Create a new absolute row span with an exclusive end row.
    #[must_use]
    pub const fn new(start_row: u64, end_row_exclusive: u64) -> Self {
        Self {
            start_row,
            end_row_exclusive,
        }
    }

    /// Return the number of rows in the span.
    #[must_use]
    pub const fn row_count(self) -> u64 {
        self.end_row_exclusive.saturating_sub(self.start_row)
    }

    /// Convert back to the legacy tuple representation.
    #[must_use]
    pub const fn as_tuple(self) -> (u64, u64) {
        (self.start_row, self.end_row_exclusive)
    }
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

    /// Calculate the command execution duration in milliseconds.
    ///
    /// Returns the time between command execution start (C) and completion (D).
    /// Returns `None` if timestamps are incomplete or inconsistent.
    #[must_use]
    pub fn exec_duration_ms(&self) -> Option<u64> {
        match (self.command_exec_start_time_ms, self.command_end_time_ms) {
            // saturating_sub: exact under the `end >= start` guard (which the
            // verifier cannot chain into the arm's arithmetic).
            (Some(start), Some(end)) if end >= start => Some(end.saturating_sub(start)),
            _ => None,
        }
    }

    /// Calculate the total command duration in milliseconds.
    ///
    /// Returns the wall-clock time from prompt display (A) to completion (D),
    /// spanning all phases: prompt, input, and execution.
    /// Returns `None` if either timestamp is missing or inconsistent.
    #[must_use]
    pub fn command_duration_ms(&self) -> Option<u64> {
        match (self.prompt_time_ms, self.command_end_time_ms) {
            // saturating_sub: exact under the `end >= start` guard (which the
            // verifier cannot chain into the arm's arithmetic).
            (Some(start), Some(end)) if end >= start => Some(end.saturating_sub(start)),
            _ => None,
        }
    }

    /// Get the typed row span for the prompt portion of this block.
    #[must_use]
    pub fn prompt_row_span(&self) -> RowSpan {
        let end = self
            .command_start_row
            .or(self.output_start_row)
            .or(self.end_row)
            .unwrap_or(self.prompt_start_row.saturating_add(1));
        RowSpan::new(self.prompt_start_row, end)
    }

    /// Get the typed row span for the command portion of this block.
    #[must_use]
    pub fn command_row_span(&self) -> Option<RowSpan> {
        let start = self.command_start_row?;
        let end = self
            .output_start_row
            .unwrap_or(self.end_row.unwrap_or(start.saturating_add(1)));
        Some(RowSpan::new(start, end))
    }

    /// Get the typed row span for the output portion of this block.
    #[must_use]
    pub fn output_row_span(&self) -> Option<RowSpan> {
        let start = self.output_start_row?;
        let end = self.end_row.unwrap_or(start.saturating_add(1));
        Some(RowSpan::new(start, end))
    }

    /// Compatibility shim for callers still using tuple row ranges.
    #[must_use]
    pub fn prompt_rows(&self) -> (u64, u64) {
        self.prompt_row_span().as_tuple()
    }

    /// Compatibility shim for callers still using tuple row ranges.
    #[must_use]
    pub fn command_rows(&self) -> Option<(u64, u64)> {
        self.command_row_span().map(RowSpan::as_tuple)
    }

    /// Compatibility shim for callers still using tuple row ranges.
    #[must_use]
    pub fn output_rows(&self) -> Option<(u64, u64)> {
        self.output_row_span().map(RowSpan::as_tuple)
    }

    /// Check if a given row falls within this block.
    ///
    /// # Arguments
    ///
    /// * `row` - Absolute row number to check
    #[must_use]
    pub fn contains_row(&self, row: u64) -> bool {
        if row < self.prompt_start_row {
            return false;
        }
        match self.end_row {
            Some(end) => row < end,
            None => true, // Block is still in progress
        }
    }

    /// Check if a given row is visible (not part of collapsed output).
    ///
    /// When a block is collapsed, only the prompt and command portions are
    /// visible; the output portion is hidden.
    ///
    /// # Arguments
    ///
    /// * `row` - Absolute row number to check
    #[must_use]
    pub fn is_row_visible(&self, row: u64) -> bool {
        if !self.contains_row(row) {
            return true; // Not our row, doesn't matter
        }
        if !self.collapsed {
            return true; // Not collapsed, everything visible
        }
        // Collapsed: only prompt and command visible
        match self.output_start_row {
            Some(output_start) => row < output_start,
            None => true, // No output yet, everything visible
        }
    }

    /// Get the number of visible rows in this block.
    ///
    /// When collapsed, this excludes the output portion.
    /// Returns count as `usize` since row counts fit in memory.
    #[must_use]
    pub fn visible_row_count(&self) -> usize {
        let end = self.end_row.unwrap_or(
            self.output_start_row.unwrap_or(
                self.command_start_row
                    .unwrap_or(self.prompt_start_row.saturating_add(1)),
            ),
        );
        // Row-count differences are small — saturate on 32-bit for safety
        let total =
            usize::try_from(end.saturating_sub(self.prompt_start_row)).unwrap_or(usize::MAX);

        if self.collapsed {
            // Only count rows before output starts
            if let Some(output_start) = self.output_start_row {
                usize::try_from(output_start.saturating_sub(self.prompt_start_row))
                    .unwrap_or(usize::MAX)
            } else {
                total
            }
        } else {
            total
        }
    }

    /// Get the number of hidden rows (collapsed output rows).
    ///
    /// Returns count as `usize` since row counts fit in memory.
    #[must_use]
    pub fn hidden_row_count(&self) -> usize {
        if !self.collapsed {
            return 0;
        }
        if let Some(rows) = self.output_row_span() {
            usize::try_from(rows.row_count()).unwrap_or(usize::MAX)
        } else {
            0
        }
    }
}

#[cfg(test)]
#[path = "shell_blocks_tests.rs"]
mod tests;
