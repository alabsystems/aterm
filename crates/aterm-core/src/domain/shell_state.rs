// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shell integration protocol state.
//!
//! [`ShellState`] represents the OSC 133 shell integration state machine.
//! It lives in `domain` (a leaf module) so that both `terminal` and `semantic`
//! can import it without creating a module dependency cycle.

/// Shell integration state machine states.
///
/// Based on FinalTerm/Terminal's OSC 133 protocol:
/// - A: Prompt is starting
/// - B: Command input is starting (prompt finished)
/// - C: Command execution is starting (user pressed enter)
/// - D: Command execution finished (with exit code)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ShellState {
    /// Ground state - waiting for prompt.
    #[default]
    Ground,
    /// Receiving prompt text (after OSC 133 ; A).
    ReceivingPrompt,
    /// User is entering command (after OSC 133 ; B).
    EnteringCommand,
    /// Command is executing (after OSC 133 ; C).
    Executing,
}
