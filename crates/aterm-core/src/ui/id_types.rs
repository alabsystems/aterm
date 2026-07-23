// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! UI identifier newtypes.
//!
//! See `docs/ID_TYPES.md` for the workspace ID-type guideline.

/// Terminal identifier.
///
/// Newtype over `u32` to prevent accidental confusion with other integer
/// identifiers (e.g., `CallbackId`). Use [`TerminalId::from_raw`] at FFI
/// boundaries and [`TerminalId::raw`] for bounds checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct TerminalId(u32);

impl TerminalId {
    /// Create from a raw `u32` value (FFI and test boundaries).
    pub const fn from_raw(val: u32) -> Self {
        Self(val)
    }

    /// Extract the raw `u32` value (FFI boundaries and bounds checks).
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for TerminalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Event identifier (unique per event).
///
/// Newtype over `u64` to prevent confusion with other counters. Generated
/// by the atomic `NEXT_EVENT_ID` counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(transparent)]
pub struct EventId(pub(super) u64);

/// Callback identifier.
///
/// Newtype over `u32` to prevent accidental confusion with `TerminalId`
/// or other integer identifiers. Use [`CallbackId::from_raw`] at FFI
/// boundaries and [`CallbackId::raw`] for bounds checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct CallbackId(u32);

impl CallbackId {
    /// Create from a raw `u32` value (FFI and test boundaries).
    pub const fn from_raw(val: u32) -> Self {
        Self(val)
    }

    /// Extract the raw `u32` value (FFI boundaries and bounds checks).
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for CallbackId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
