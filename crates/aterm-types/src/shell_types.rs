// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Shell integration types for OSC 7, 133, 633, and 1337 protocol support.

/// Current Unix timestamp in milliseconds, or `None` if before the Unix epoch.
#[cfg_attr(trust_verify, trust::skip)]
pub fn current_time_ms() -> Option<u64> {
    aterm_time::SystemTime::now()
        .duration_since(aterm_time::UNIX_EPOCH)
        .ok()
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Elapsed milliseconds from `start` to `end` (`None` if either is missing or `end < start`).
#[inline]
#[must_use]
pub(crate) const fn elapsed_ms(start: Option<u64>, end: Option<u64>) -> Option<u64> {
    match (start, end) {
        (Some(start), Some(end)) if end >= start => Some(end.saturating_sub(start)),
        _ => None,
    }
}

/// Shell command region and timestamps recorded from OSC 7 / 133 / 633 sequences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandMark {
    /// Absolute row where the prompt started.
    pub prompt_start_row: u64,
    /// Column where the prompt started.
    pub prompt_start_col: u16,
    /// Absolute row where the command input started.
    pub command_start_row: Option<u64>,
    /// Column where the command input started.
    pub command_start_col: Option<u16>,
    /// Absolute row where command output started.
    pub output_start_row: Option<u64>,
    /// Absolute row where command output ended.
    pub output_end_row: Option<u64>,
    /// Command exit status from `OSC 133 ; D ; <code>`.
    pub exit_code: Option<i32>,
    /// Working directory reported by OSC 7.
    pub working_directory: Option<Box<str>>,
    /// Explicit command line reported by `OSC 633 ; E`.
    pub commandline: Option<Box<str>>,
    /// Unix timestamp (ms) when `OSC 133 ; A` was received.
    pub prompt_time_ms: Option<u64>,
    /// Unix timestamp (ms) when `OSC 133 ; B` was received.
    pub command_input_start_time_ms: Option<u64>,
    /// Unix timestamp (ms) when `OSC 133 ; C` was received.
    pub command_exec_start_time_ms: Option<u64>,
    /// Unix timestamp (ms) when `OSC 133 ; D` was received.
    pub command_end_time_ms: Option<u64>,
}

impl CommandMark {
    /// Create a new command mark at `(row, col)` stamped with the current time.
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

    /// Whether this mark represents a completed command.
    #[inline]
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.exit_code.is_some()
    }

    /// Whether the command completed with exit code `0`.
    #[inline]
    #[must_use]
    pub const fn succeeded(&self) -> bool {
        matches!(self.exit_code, Some(0))
    }

    /// Execution duration in milliseconds from `OSC 133 ; C` to `OSC 133 ; D`.
    #[inline]
    #[must_use]
    pub const fn exec_duration_ms(&self) -> Option<u64> {
        elapsed_ms(self.command_exec_start_time_ms, self.command_end_time_ms)
    }
}

/// Navigation mark created by `OSC 1337 ; SetMark`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalMark {
    /// Monotonically increasing mark ID.
    pub id: u64,
    /// Absolute row where the mark was placed.
    pub row: u64,
    /// Column where the mark was placed.
    pub col: u16,
    /// Optional label for the mark.
    pub name: Option<String>,
}

impl TerminalMark {
    /// Create a navigation mark with `id` at `(row, col)`.
    #[inline]
    #[must_use]
    pub const fn new(id: u64, row: u64, col: u16) -> Self {
        Self {
            id,
            row,
            col,
            name: None,
        }
    }
}

/// Region annotation created by `OSC 1337 ; AddAnnotation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    /// Unique annotation ID.
    pub id: u64,
    /// Absolute starting row.
    pub row: u64,
    /// Starting column.
    pub col: u16,
    /// Annotated span length in characters (`None` for a single point).
    pub length: Option<usize>,
    /// Annotation text.
    pub message: String,
    /// Whether the annotation is initially hidden.
    pub hidden: bool,
}

impl Annotation {
    /// Create a visible point annotation with `id` and `message` at `(row, col)`.
    #[inline]
    #[must_use]
    pub const fn new(id: u64, row: u64, col: u16, message: String) -> Self {
        Self {
            id,
            row,
            col,
            length: None,
            message,
            hidden: false,
        }
    }
}

#[cfg(test)]
#[path = "shell_types_tests.rs"]
mod tests;
