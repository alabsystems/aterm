// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Types for the VT parser transition table.

use crate::state::State;

/// Action to perform during a state transition.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum ActionType {
    /// No action
    #[default]
    None = 0,
    /// Print the character
    Print,
    /// Execute C0/C1 control
    Execute,
    /// Clear parameters and intermediates
    Clear,
    /// Collect intermediate byte
    Collect,
    /// Add digit to current parameter
    Param,
    /// Dispatch ESC sequence
    EscDispatch,
    /// Dispatch CSI sequence
    CsiDispatch,
    /// Hook DCS
    DcsHook,
    /// Put DCS byte
    DcsPut,
    /// Start OSC
    OscStart,
    /// Put OSC byte
    OscPut,
    /// End OSC
    OscEnd,
    /// Ignore this byte
    Ignore,
    /// Start APC
    ApcStart,
    /// Put APC byte
    ApcPut,
    /// End APC
    ApcEnd,
}

/// A state transition entry.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct Transition {
    /// Next state
    pub next_state: State,
    /// Action to perform
    pub action: ActionType,
}

impl Transition {
    /// Create a new transition.
    pub const fn new(next_state: State, action: ActionType) -> Self {
        Self { next_state, action }
    }
}
