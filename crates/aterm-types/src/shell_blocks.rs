// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Block-based output model for agent-friendly terminal interaction.

use crate::shell_types::current_time_ms;

/// The state of an output block.
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
    pub working_directory: Option<Box<str>>,
    /// Explicit commandline text (from OSC 633 ; E).
    pub commandline: Option<Box<str>>,
    /// Whether the output portion of this block is collapsed.
    pub collapsed: bool,
    /// Whether a full-screen clear (`ED 2` or `ED 0` from home) blanked rows after completion.
    pub text_cleared: bool,
    /// Whether a full-screen clear or scrollback erase (`ED 3`) lost command rows mid-run.
    pub command_text_cleared: bool,
    /// Timestamp when prompt was received (milliseconds since Unix epoch).
    pub prompt_time_ms: Option<u64>,
    /// Timestamp when command input started (milliseconds since Unix epoch).
    pub command_input_start_time_ms: Option<u64>,
    /// Timestamp when command execution started (milliseconds since Unix epoch).
    pub command_exec_start_time_ms: Option<u64>,
    /// Timestamp when command execution ended (milliseconds since Unix epoch).
    pub command_end_time_ms: Option<u64>,
}

impl OutputBlock {
    /// Create a new block starting at the given position.
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
    #[inline]
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self.state, BlockState::Complete)
    }

    /// Check if the command in this block succeeded (exit code 0).
    #[inline]
    #[must_use]
    pub const fn succeeded(&self) -> bool {
        matches!(self.exit_code, Some(0))
    }

    /// Check if the command in this block failed (exit code != 0).
    #[inline]
    #[must_use]
    pub const fn failed(&self) -> bool {
        matches!(self.exit_code, Some(code) if code != 0)
    }

    /// Absolute `(start, end_exclusive)` rows of the command portion of this block.
    #[inline]
    #[must_use]
    pub const fn command_rows(&self) -> Option<(u64, u64)> {
        let Some(start) = self.command_start_row else {
            return None;
        };
        let end = match self.output_start_row {
            Some(row) => row,
            None => match self.end_row {
                Some(row) => row,
                None => start.saturating_add(1),
            },
        };
        Some((start, end))
    }

    /// Absolute `(start, end_exclusive)` rows of the output portion of this block.
    #[inline]
    #[must_use]
    pub const fn output_rows(&self) -> Option<(u64, u64)> {
        let Some(start) = self.output_start_row else {
            return None;
        };
        let end = match self.end_row {
            Some(row) => row,
            None => start.saturating_add(1),
        };
        Some((start, end))
    }
}

#[cfg(test)]
#[path = "shell_blocks_tests.rs"]
mod tests;
