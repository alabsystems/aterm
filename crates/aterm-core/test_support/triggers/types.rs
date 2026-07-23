// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Trigger system data types.
//!
//! Extracted from `mod.rs` for module size reduction (#4613).
//! Contains: `TriggerAction`, `TriggerError`, `TriggerMatch`, `EvaluatedTrigger`.

/// Actions that can be triggered on pattern matches.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TriggerAction {
    /// Highlight the matched text with specified colors.
    /// Colors are RGB values [r, g, b].
    Highlight {
        /// Foreground color (if any)
        foreground: Option<[u8; 3]>,
        /// Background color (if any)
        background: Option<[u8; 3]>,
    },

    /// Show a system notification/alert.
    Alert {
        /// Alert title
        title: String,
        /// Alert message (can use $0 for full match, $1-$9 for groups)
        message: String,
    },

    /// Play a bell sound.
    Bell,
}

/// Errors that can occur in the trigger system.
#[derive(Debug, Clone, PartialEq, Eq, aterm_error::Error)]
#[non_exhaustive]
pub enum TriggerError {
    /// Invalid regex pattern
    #[error("invalid trigger pattern '{pattern}': {reason}")]
    InvalidPattern {
        /// The problematic pattern
        pattern: String,
        /// Description of the error
        reason: String,
    },
    /// Pattern not specified (builder error)
    #[error("trigger pattern not specified")]
    MissingPattern,
    /// Action not specified (builder error)
    #[error("trigger action not specified")]
    MissingAction,
}

/// A match found by a trigger.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerMatch {
    /// Start byte offset in the matched text
    pub start: usize,
    /// End byte offset in the matched text
    pub end: usize,
    /// The matched text
    pub text: String,
}

/// Output of evaluating a trigger on a line.
///
/// Contains information about which trigger matched, the match details,
/// and the action to execute. Renamed from `TriggerResult` to avoid
/// confusion with `Result<T, E>` type alias convention.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct EvaluatedTrigger {
    /// Index of the trigger that matched
    pub trigger_index: usize,
    /// The match information
    pub match_info: TriggerMatch,
    /// The action to execute
    pub action: TriggerAction,
}
