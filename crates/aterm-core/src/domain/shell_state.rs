// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shell integration protocol state ([`ShellState`], OSC 133).

/// OSC 133 shell integration state (FinalTerm protocol). `D` (command finished) returns to
/// [`Ground`](Self::Ground).
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
