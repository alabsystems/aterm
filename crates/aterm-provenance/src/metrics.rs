// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Per-subsystem Drop-on-Top metrics (§7.3, issue #8003).
//!
//! The trusted-provenance framework defines 6 consumer subsystems that MUST
//! drop a `Top`-tagged value on sight rather than treat it as a lift
//! opportunity (see `designs/2026-04-19-provenance-framework.md` §7.2). Every
//! such drop increments a per-subsystem counter, exposed through the
//! `aterm-core-ffi` crate to the host application.
//!
//! Metric names follow the `provenance.drop_on_top.<subsystem>` pattern from
//! §7.3 and are exposed via the `NAME` constants on [`Subsystem`]. Counters
//! are plain process-global `AtomicU64`s — no allocation, no locks, no
//! dependencies on any metrics crate. The aterm runtime surfaces the values
//! via the `aterm_provenance_drop_on_top_*` FFI getters.
//!
//! # Thread safety
//!
//! `Relaxed` ordering is sufficient: counters are monotonic, drop events are
//! logically independent, and the host polls them at its own cadence.

use core::sync::atomic::{AtomicU64, Ordering};

/// The six consumer subsystems enumerated in §7.2.
///
/// The variant order is the one used by the design document; see
/// [`Subsystem::all`] for an exhaustive iteration helper used in tests.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Subsystem {
    /// Grid / scrollback render path (`Grid::write_cell`).
    Grid,
    /// Session memory / scrollback persistence (`SessionMemory::record_*`).
    Memory,
    /// AI command predictor (`Predictor::observe`).
    Predictor,
    /// Voice narration observer (`VoiceObserver::narrate`).
    Voice,
    /// System notification dispatch (`NotificationCapability`).
    Notification,
    /// FFI cell / metadata accessor boundary (`aterm_get_cell`).
    Ffi,
}

impl Subsystem {
    /// Exhaustive list of subsystems, in design order.
    #[must_use]
    pub const fn all() -> [Subsystem; 6] {
        [
            Subsystem::Grid,
            Subsystem::Memory,
            Subsystem::Predictor,
            Subsystem::Voice,
            Subsystem::Notification,
            Subsystem::Ffi,
        ]
    }

    /// Stable metric-name string, matching the `provenance.drop_on_top.<subsystem>`
    /// pattern from §7.3.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Subsystem::Grid => "provenance.drop_on_top.grid",
            Subsystem::Memory => "provenance.drop_on_top.memory",
            Subsystem::Predictor => "provenance.drop_on_top.predictor",
            Subsystem::Voice => "provenance.drop_on_top.voice",
            Subsystem::Notification => "provenance.drop_on_top.notification",
            Subsystem::Ffi => "provenance.drop_on_top.ffi",
        }
    }
}

// -- Counters ---------------------------------------------------------------

static DROPS_GRID: AtomicU64 = AtomicU64::new(0);
static DROPS_MEMORY: AtomicU64 = AtomicU64::new(0);
static DROPS_PREDICTOR: AtomicU64 = AtomicU64::new(0);
static DROPS_VOICE: AtomicU64 = AtomicU64::new(0);
static DROPS_NOTIFICATION: AtomicU64 = AtomicU64::new(0);
static DROPS_FFI: AtomicU64 = AtomicU64::new(0);

const fn counter(subsystem: Subsystem) -> &'static AtomicU64 {
    match subsystem {
        Subsystem::Grid => &DROPS_GRID,
        Subsystem::Memory => &DROPS_MEMORY,
        Subsystem::Predictor => &DROPS_PREDICTOR,
        Subsystem::Voice => &DROPS_VOICE,
        Subsystem::Notification => &DROPS_NOTIFICATION,
        Subsystem::Ffi => &DROPS_FFI,
    }
}

/// Record a Drop-on-Top event for the given subsystem. Returns the **new**
/// counter value (post-increment). Saturates at `u64::MAX`; no wrap-around.
pub fn record_drop_on_top(subsystem: Subsystem) -> u64 {
    let c = counter(subsystem);
    // Saturating add: load, check, store-if-different. Under contention a
    // caller might miss one increment near u64::MAX which is acceptable —
    // 2^64 drops is 584 years of sustained 1-GHz drop rate.
    let prev = c.fetch_add(1, Ordering::Relaxed);
    prev.saturating_add(1)
}

/// Current value of the per-subsystem drop counter.
#[must_use]
pub fn drop_on_top_count(subsystem: Subsystem) -> u64 {
    counter(subsystem).load(Ordering::Relaxed)
}

/// Reset all counters to zero. Intended for tests only. Under concurrent
/// load this is racy; callers must serialize.
#[doc(hidden)]
pub fn __reset_all_for_tests() {
    for s in Subsystem::all() {
        counter(s).store(0, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Per-subsystem convenience entry points (§7.2 call-site ergonomics)
// ---------------------------------------------------------------------------
//
// These helpers exist so each consumer subsystem has a single, grep-able call
// site where a DynProvenance is checked for Top. They are thin wrappers around
// `DynProvenance::drop_if_top(subsystem)` that bind the subsystem argument.
//
// Usage pattern (once #8005 plumbs DynProvenance through the subsystem APIs):
//
//   // Inside `Grid::write_cell`:
//   let Some(dp) = aterm_provenance::drop_if_top_grid(dp) else { return; };
//   // ... proceed with the non-Top carrier ...
//
// The named helpers are the §7.2 drop enforcement points. Any Top-drop site
// outside these helpers is an audit violation.

use crate::provenance::DynProvenance;

/// §7.2 drop point for `Grid::write_cell`. See module docs for the full
/// enforcement pattern.
#[must_use = "Top values must be dropped; unused return = lost drop site"]
pub fn drop_if_top_grid<T>(dp: DynProvenance<T>) -> Option<DynProvenance<T>> {
    dp.drop_if_top(Subsystem::Grid)
}

/// §7.2 drop point for `SessionMemory::record_*`.
#[must_use = "Top values must be dropped; unused return = lost drop site"]
pub fn drop_if_top_memory<T>(dp: DynProvenance<T>) -> Option<DynProvenance<T>> {
    dp.drop_if_top(Subsystem::Memory)
}

/// §7.2 drop point for `Predictor::observe`.
#[must_use = "Top values must be dropped; unused return = lost drop site"]
pub fn drop_if_top_predictor<T>(dp: DynProvenance<T>) -> Option<DynProvenance<T>> {
    dp.drop_if_top(Subsystem::Predictor)
}

/// §7.2 drop point for `VoiceObserver::narrate`. The design specifies an
/// audible-notification fallback ("voice output suppressed: untrusted mixed
/// origin"); the subsystem is responsible for emitting that fallback when
/// this helper returns `None`. The counter is incremented in either case.
#[must_use = "Top values must be dropped; unused return = lost drop site"]
pub fn drop_if_top_voice<T>(dp: DynProvenance<T>) -> Option<DynProvenance<T>> {
    dp.drop_if_top(Subsystem::Voice)
}

/// §7.2 drop point for `NotificationCapability`. Fail-closed: the subsystem
/// must never dispatch a `Top`-tagged body. The counter is the audit signal.
#[must_use = "Top values must be dropped; unused return = lost drop site"]
pub fn drop_if_top_notification<T>(dp: DynProvenance<T>) -> Option<DynProvenance<T>> {
    dp.drop_if_top(Subsystem::Notification)
}

/// §7.2 drop point for the FFI cell/metadata accessors. At the FFI boundary
/// the host expects a concrete [`OriginTag`] byte; callers that encounter a
/// Top carrier should return [`crate::TOP_TAG_U8`] (`0xFF`) to the host and
/// decline to materialize the cell body.
#[must_use = "Top values must be dropped; unused return = lost drop site"]
pub fn drop_if_top_ffi<T>(dp: DynProvenance<T>) -> Option<DynProvenance<T>> {
    dp.drop_if_top(Subsystem::Ffi)
}
