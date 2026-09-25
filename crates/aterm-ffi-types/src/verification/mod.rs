// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! FFI pointer lifecycle tracking.
//!
//! - `ffi_free_tracker`: Unified free-pointer tracking (Kani, debug, and release modes)
//! - `terminal_handle_tracker`: Terminal-specific handle tracking

/// Unified FFI pointer free-tracking. Active in Kani, debug, and release (no-op) modes.
pub mod ffi_free_tracker;

/// Terminal-handle-specific free-tracking. Always active in non-Kani builds.
/// Use this for `AtermTerminal*` lifecycle instead of `ffi_free_tracker` which
/// compiles to no-ops in release. See #5856.
pub mod terminal_handle_tracker;

/// Selects which free-tracker to use for a given handle type.
///
/// The free combinator in [`super::ffi_free_combinator`] accepts this enum
/// to route `mark_freed` / `assert_not_freed` calls to the correct tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiTracker {
    /// General-purpose tracker ([`ffi_free_tracker`]) — all non-terminal handles.
    General,
    /// Dedicated tracker for `AtermGrid` handles.
    Grid,
    /// Dedicated tracker for `AtermCheckpoint` handles.
    Checkpoint,
    /// Dedicated tracker for `AtermConfig` handles.
    Config,
    /// Dedicated tracker for `AtermConfigWatcher` handles.
    ConfigWatcher,
    /// Dedicated tracker for `AtermParser` handles.
    Parser,
    /// Dedicated tracker for `AtermPolicy` handles.
    Policy,
    /// Terminal-specific tracker ([`terminal_handle_tracker`]) — `AtermTerminal` only.
    Terminal,
}

impl FfiTracker {
    const fn general_bucket(self) -> usize {
        match self {
            Self::General => 0,
            Self::Grid => 1,
            Self::Checkpoint => 2,
            Self::Config => 3,
            Self::ConfigWatcher => 4,
            Self::Parser => 5,
            Self::Policy => 6,
            Self::Terminal => 0,
        }
    }

    /// Check if a pointer was previously recorded as freed.
    pub fn is_freed(self, ptr: *const core::ffi::c_void) -> bool {
        match self {
            Self::Terminal => terminal_handle_tracker::is_freed(ptr),
            _ => ffi_free_tracker::is_freed_in(self.general_bucket(), ptr),
        }
    }

    /// Check if a pointer is currently recorded as live.
    pub fn is_allocated(self, ptr: *const core::ffi::c_void) -> bool {
        match self {
            Self::Terminal => terminal_handle_tracker::is_allocated(ptr),
            _ => ffi_free_tracker::is_allocated_in(self.general_bucket(), ptr),
        }
    }

    /// Record a pointer as live and clear any stale freed bit.
    pub fn mark_allocated(self, ptr: *mut core::ffi::c_void) {
        match self {
            Self::Terminal => terminal_handle_tracker::mark_allocated(ptr),
            _ => ffi_free_tracker::mark_allocated_in(self.general_bucket(), ptr),
        }
    }

    /// Record a pointer as freed. Returns `true` if already freed (double-free).
    pub fn mark_freed(self, ptr: *mut core::ffi::c_void) -> bool {
        match self {
            Self::Terminal => terminal_handle_tracker::mark_freed(ptr),
            _ => ffi_free_tracker::mark_freed_in(self.general_bucket(), ptr),
        }
    }

    /// Assert that a pointer has not been freed. Panics (or Kani-asserts) on double-free.
    pub fn assert_not_freed(self, ptr: *mut core::ffi::c_void) {
        match self {
            Self::Terminal => terminal_handle_tracker::assert_not_freed(ptr),
            _ => ffi_free_tracker::assert_not_freed_in(self.general_bucket(), ptr),
        }
    }
}
