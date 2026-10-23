// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THRU-2's INTERACTIVE-INPUT-PENDING hint.

use std::sync::atomic::{AtomicU64, Ordering};

/// `crate::metrics::now_ns` until which input counts as pending; 0 = never
/// armed.
static TYPING_HOT_UNTIL_NS: AtomicU64 = AtomicU64::new(0);
/// How long after a keystroke the reader keeps its term-lock holds fine.
pub(crate) const TYPING_HOT_TAIL_NS: u64 = 250_000_000;

/// Arm the window.
pub(crate) fn arm() {
    TYPING_HOT_UNTIL_NS.store(
        crate::metrics::now_ns().saturating_add(TYPING_HOT_TAIL_NS),
        Ordering::Relaxed,
    );
}

/// Whether a human is actively typing.
#[must_use]
pub(crate) fn pending() -> bool {
    let until = TYPING_HOT_UNTIL_NS.load(Ordering::Relaxed);
    until != 0 && crate::metrics::now_ns() < until
}
