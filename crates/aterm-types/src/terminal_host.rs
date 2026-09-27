// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal orchestration boundary traits and shared DTOs.
//!
//! These contracts let orchestrator-style consumers interact with terminal state
//! without importing concrete `aterm-core` internals.

use crate::TerminalSize;

/// Stable block lifecycle state for orchestration consumers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TerminalBlockState {
    /// Block contains only prompt text.
    PromptOnly,
    /// User is entering a command.
    EnteringCommand,
    /// Command is executing and may stream output.
    Executing,
    /// Command completed and has final status.
    Complete,
}

/// Shared block summary for command/output navigation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalBlockSnapshot {
    /// Session-unique block identifier.
    pub id: u64,
    /// Block lifecycle state.
    pub state: TerminalBlockState,
    /// Absolute prompt start row.
    pub prompt_start_row: u64,
    /// Absolute command start row (if command text began).
    pub command_start_row: Option<u64>,
    /// Absolute output start row (if output started).
    pub output_start_row: Option<u64>,
    /// Absolute block end row, exclusive.
    pub end_row: Option<u64>,
    /// Process exit code if command completed.
    pub exit_code: Option<i32>,
    /// Working directory captured for this block, if available.
    pub working_directory: Option<String>,
    /// Explicit command line text if provided by shell integration.
    pub commandline: Option<String>,
    /// Whether output is currently collapsed.
    pub collapsed: bool,
}

/// Read/write terminal host surface needed by orchestrator paths.
pub trait TerminalHost {
    /// Feed parser input bytes into the terminal.
    fn process(&mut self, input: &[u8]);

    /// Resize viewport dimensions.
    fn resize(&mut self, rows: u16, cols: u16);

    /// Get current viewport dimensions.
    fn size(&self) -> TerminalSize;

    /// Get visible viewport content.
    fn visible_content(&self) -> String;

    /// Get current working directory.
    fn current_working_directory(&self) -> Option<String>;

    /// Get total scrollback line count.
    fn scrollback_line_count(&self) -> usize;

    /// Get one scrollback line by index (0 = oldest).
    fn scrollback_line(&self, index: usize) -> Option<String>;

    /// Get one scrollback line by reverse index (0 = newest).
    fn scrollback_line_from_end(&self, reverse_index: usize) -> Option<String>;
}

/// Block inspection surface for command/output navigation.
pub trait TerminalBlockAccess {
    /// List completed and in-progress blocks.
    fn blocks(&self) -> Vec<TerminalBlockSnapshot>;

    /// Get command text for a block ID.
    fn block_command(&self, block_id: u64) -> Option<String>;

    /// Get output text for a block ID.
    fn block_output(&self, block_id: u64) -> Option<String>;
}
