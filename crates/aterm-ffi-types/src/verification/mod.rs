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
/// The free combinators in [`super::ffi_free_combinator`] accept this enum
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
    /// Stable `u8` tag for carrying a tracker selector across closure
    /// boundaries.
    ///
    /// Trust L0: a closure upvar of type `&FfiTracker` (Copy upvars are
    /// captured by shared borrow) trips an unsupported-MIR
    /// `AggregateKind::Closure` type mismatch in the Trust full verifier,
    /// while an integer upvar lowers cleanly. `from_tag(t.tag()) == t` for
    /// every variant (see `tracker_tag_roundtrips_for_all_variants`), so
    /// smuggling the tag is behavior-identical.
    pub(crate) const fn tag(self) -> u8 {
        match self {
            Self::General => 0,
            Self::Grid => 1,
            Self::Checkpoint => 2,
            Self::Config => 3,
            Self::ConfigWatcher => 4,
            Self::Parser => 5,
            Self::Policy => 6,
            Self::Terminal => 7,
        }
    }

    /// Inverse of [`Self::tag`]. Total: tags outside `0..=7` map to
    /// `General`, which is unreachable in practice because every call site
    /// passes `tag()` output.
    #[inline(never)]
    pub(crate) const fn from_tag(tag: u8) -> Self {
        match tag {
            1 => Self::Grid,
            2 => Self::Checkpoint,
            3 => Self::Config,
            4 => Self::ConfigWatcher,
            5 => Self::Parser,
            6 => Self::Policy,
            7 => Self::Terminal,
            _ => Self::General,
        }
    }

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

#[cfg(test)]
mod tests {
    use super::FfiTracker;

    /// `from_tag` must be the exact inverse of `tag` for every variant —
    /// this is the identity the free combinators rely on when smuggling the
    /// tracker across the unwind-closure boundary as a `u8`.
    #[test]
    fn tracker_tag_roundtrips_for_all_variants() {
        for tracker in [
            FfiTracker::General,
            FfiTracker::Grid,
            FfiTracker::Checkpoint,
            FfiTracker::Config,
            FfiTracker::ConfigWatcher,
            FfiTracker::Parser,
            FfiTracker::Policy,
            FfiTracker::Terminal,
        ] {
            assert_eq!(FfiTracker::from_tag(tracker.tag()), tracker);
        }
    }

    /// Out-of-range tags fall back to `General` (total function, no panic).
    #[test]
    fn tracker_from_tag_is_total() {
        assert_eq!(FfiTracker::from_tag(8), FfiTracker::General);
        assert_eq!(FfiTracker::from_tag(u8::MAX), FfiTracker::General);
    }
}
