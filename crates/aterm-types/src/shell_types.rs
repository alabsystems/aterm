// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shell integration types for OSC 7 / 133 / 633 / 1337 protocol support.
//!
//! Provides data types for command marks, terminal marks, annotations,
//! and shell events. Extracted from `aterm-core/src/terminal/shell.rs`
//! to break circular dependencies (Part of #5663, #2341).

/// Get current time as milliseconds since Unix epoch.
///
/// Returns `Some(ms)` on success, `None` if system time is before Unix epoch.
// Skip: aterm_time clock accessors (third-party absent bodies).
#[cfg_attr(trust_verify, trust::skip)]
pub fn current_time_ms() -> Option<u64> {
    // aterm_time::SystemTime is std::time on native (byte-identical) and the JS
    // Date clock on wasm32, where std::time::SystemTime::now() panics.
    aterm_time::SystemTime::now()
        .duration_since(aterm_time::UNIX_EPOCH)
        .ok()
        // as_millis() returns u128; saturate to u64 (won't overflow for centuries)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

// ============================================================================
// Shell Integration (OSC 133)
// ============================================================================

/// A mark representing a shell command and its output.
///
/// Command marks are created by OSC 133 shell integration sequences.
/// They track the boundaries of prompts, commands, and output in the terminal.
///
/// Row coordinates use absolute row numbers (u64) that survive scrollback eviction.
/// Use `Grid::visible_to_absolute()` to convert screen-relative coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandMark {
    /// Row where the prompt started (absolute line number).
    pub prompt_start_row: u64,
    /// Column where the prompt started.
    pub prompt_start_col: u16,
    /// Row where the prompt ended / command started (absolute line number).
    pub command_start_row: Option<u64>,
    /// Column where the command started.
    pub command_start_col: Option<u16>,
    /// Row where command output started (absolute line number).
    pub output_start_row: Option<u64>,
    /// Row where command output ended (absolute line number).
    pub output_end_row: Option<u64>,
    /// Command exit code (from `OSC 133 ; D ; <code>`).
    pub exit_code: Option<i32>,
    /// Working directory at time of command (from OSC 7).
    ///
    /// Uses `Box<str>` instead of `String` to save 8 bytes.
    pub working_directory: Option<Box<str>>,
    /// Explicit commandline text (from OSC 633 ; E).
    ///
    /// Set by VS Code shell integration when the shell reports the command text
    /// explicitly, rather than relying on screen scraping.
    pub commandline: Option<Box<str>>,
    /// Timestamp when prompt was received (milliseconds since Unix epoch).
    ///
    /// Captured when OSC 133;A is processed. Used by AI agents to understand
    /// how long ago the prompt appeared (e.g., for timeout handling).
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

impl CommandMark {
    /// Create a new command mark at the given position.
    ///
    /// Captures the current timestamp when the prompt is received.
    ///
    /// # Arguments
    ///
    /// * `row` - Absolute row number (use `Grid::visible_to_absolute()` to convert)
    /// * `col` - Column position
    pub fn new(row: u64, col: u16) -> Self {
        Self {
            prompt_start_row: row,
            prompt_start_col: col,
            command_start_row: None,
            command_start_col: None,
            output_start_row: None,
            output_end_row: None,
            exit_code: None,
            working_directory: None,
            commandline: None,
            prompt_time_ms: current_time_ms(),
            command_input_start_time_ms: None,
            command_exec_start_time_ms: None,
            command_end_time_ms: None,
        }
    }

    /// Check if this mark represents a completed command.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.exit_code.is_some()
    }

    /// Check if the command succeeded (exit code 0).
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// Calculate the command input duration in milliseconds.
    ///
    /// Returns the time between prompt display (A) and command input start (B).
    /// Returns `None` if timestamps are incomplete or inconsistent.
    #[must_use]
    pub fn prompt_duration_ms(&self) -> Option<u64> {
        match (self.prompt_time_ms, self.command_input_start_time_ms) {
            // saturating_sub: exact under the `end >= start` guard (which the
            // verifier cannot chain into the arm's arithmetic).
            (Some(start), Some(end)) if end >= start => Some(end.saturating_sub(start)),
            _ => None,
        }
    }

    /// Calculate the command typing duration in milliseconds.
    ///
    /// Returns the time between command input start (B) and execution start (C).
    /// Returns `None` if timestamps are incomplete or inconsistent.
    #[must_use]
    pub fn input_duration_ms(&self) -> Option<u64> {
        match (
            self.command_input_start_time_ms,
            self.command_exec_start_time_ms,
        ) {
            // saturating_sub: exact under the `end >= start` guard (which the
            // verifier cannot chain into the arm's arithmetic).
            (Some(start), Some(end)) if end >= start => Some(end.saturating_sub(start)),
            _ => None,
        }
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
}

// ============================================================================
// Terminal OSC 1337 Protocol (Marks, Annotations)
// ============================================================================

/// A user-created mark for navigation (OSC 1337 SetMark).
///
/// Marks are created by applications to allow users to jump back to
/// important locations in the terminal output. Unlike command marks
/// (OSC 133), these are explicitly set by the user or application.
///
/// Row coordinates use absolute row numbers (u64) that survive scrollback eviction.
///
/// Format: `OSC 1337 ; SetMark ST`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalMark {
    /// Unique ID for this mark (monotonically increasing).
    pub id: u64,
    /// Row where the mark was set (absolute line number).
    pub row: u64,
    /// Column where the mark was set.
    pub col: u16,
    /// Optional name/label for the mark.
    pub name: Option<String>,
}

impl TerminalMark {
    /// Create a new mark at the given position.
    ///
    /// # Arguments
    ///
    /// * `id` - Unique identifier for this mark
    /// * `row` - Absolute row number (use `Grid::visible_to_absolute()` to convert)
    /// * `col` - Column position
    pub fn new(id: u64, row: u64, col: u16) -> Self {
        Self {
            id,
            row,
            col,
            name: None,
        }
    }
}

/// An annotation attached to terminal content (OSC 1337 AddAnnotation).
///
/// Annotations allow applications to attach metadata or notes to specific
/// regions of terminal output. They can be visible or hidden.
///
/// Row coordinates use absolute row numbers (u64) that survive scrollback eviction.
///
/// Formats:
/// - `OSC 1337 ; AddAnnotation=message ST` - Add annotation at cursor
/// - `OSC 1337 ; AddAnnotation=length|message ST` - Add with length
/// - `OSC 1337 ; AddHiddenAnnotation=message ST` - Add hidden annotation
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    /// Unique ID for this annotation.
    pub id: u64,
    /// Row where the annotation starts (absolute line number).
    pub row: u64,
    /// Column where the annotation starts.
    pub col: u16,
    /// Length of the annotated region (in characters).
    /// If None, annotation applies to a single point.
    pub length: Option<usize>,
    /// The annotation message/content.
    pub message: String,
    /// Whether this annotation is hidden from normal display.
    pub hidden: bool,
}

impl Annotation {
    /// Create a new visible annotation.
    ///
    /// # Arguments
    ///
    /// * `id` - Unique identifier for this annotation
    /// * `row` - Absolute row number (use `Grid::visible_to_absolute()` to convert)
    /// * `col` - Column position
    /// * `message` - The annotation content
    pub fn new(id: u64, row: u64, col: u16, message: String) -> Self {
        Self {
            id,
            row,
            col,
            length: None,
            message,
            hidden: false,
        }
    }

    /// Create a new hidden annotation.
    ///
    /// # Arguments
    ///
    /// * `id` - Unique identifier for this annotation
    /// * `row` - Absolute row number (use `Grid::visible_to_absolute()` to convert)
    /// * `col` - Column position
    /// * `message` - The annotation content
    pub fn new_hidden(id: u64, row: u64, col: u16, message: String) -> Self {
        Self {
            id,
            row,
            col,
            length: None,
            message,
            hidden: true,
        }
    }
}

/// Shell integration event sent to callbacks.
///
/// Row coordinates use absolute row numbers (u64) that survive scrollback eviction.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShellEvent {
    /// Prompt started (OSC 133 ; A).
    PromptStart {
        /// Row where prompt started (absolute line number).
        row: u64,
        /// Column where prompt started.
        col: u16,
    },
    /// Command input started (OSC 133 ; B).
    CommandStart {
        /// Row where command input started (absolute line number).
        row: u64,
        /// Column where command input started.
        col: u16,
    },
    /// Command execution started (OSC 133 ; C).
    OutputStart {
        /// Row where output started (absolute line number).
        row: u64,
    },
    /// Command finished (`OSC 133 ; D ; <code>`).
    CommandFinished {
        /// Exit code of the command.
        exit_code: i32,
    },
    /// Current working directory changed (OSC 7 or OSC 633 ; P ; Cwd=...).
    DirectoryChanged {
        /// New working directory, or `None` when the shell clears it.
        path: Option<Box<str>>,
    },
    /// Explicit shell text payload (OSC 633 ; E).
    SemanticText {
        /// Unescaped text payload reported by the shell integration layer.
        text: Box<str>,
    },
    /// Progress tracking started (OSC 633 ; F).
    ProgressStart {
        /// Raw progress payload, if one was supplied by the shell.
        payload: Option<Box<str>>,
    },
    /// Progress tracking updated (OSC 633 ; G).
    ProgressUpdate {
        /// Raw progress payload, if one was supplied by the shell.
        payload: Option<Box<str>>,
    },
    /// Progress tracking ended (OSC 633 ; H).
    ProgressEnd {
        /// Raw progress payload, if one was supplied by the shell.
        payload: Option<Box<str>>,
    },
}

#[cfg(test)]
#[path = "shell_types_tests.rs"]
mod tests;
