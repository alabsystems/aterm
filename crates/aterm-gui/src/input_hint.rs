// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THRU-2's INTERACTIVE-INPUT-PENDING hint: whether a human is waiting on the
//! UI thread, so the PTY reader keeps its term-lock holds fine.
//!
//! Split from the `metrics` input→present stamp (touch-to-glass audit round
//! 2). That stamp is consumed by the next CONTENT present, which under
//! concurrent streaming output is a log-line frame, not the keystroke's echo —
//! so the reader's fine 8 KiB slicing disarmed within one frame of each key
//! and spent ~85% of a typing burst back on whole-64-KiB term-lock holds,
//! exactly the starvation the slicing exists to prevent. This deadline is
//! armed at HARDWARE key arrival (before any press-path term_lock) and decays
//! [`TYPING_HOT_TAIL_NS`] after the LAST key, so it spans a whole typing burst
//! and is immune to what the present path does. `metrics reset` never touches
//! it: it is scheduling state, not an observation.

use std::sync::atomic::{AtomicU64, Ordering};

/// `crate::metrics::now_ns` until which input counts as pending; 0 = never
/// armed.
static TYPING_HOT_UNTIL_NS: AtomicU64 = AtomicU64::new(0);
/// How long after a keystroke the reader keeps its term-lock holds fine. Longer
/// than a fast typist's inter-key gap (~125 ms at 100 wpm) so a burst never
/// disarms mid-word, short enough that a pure output flood pays the 8x lock
/// round-trip cost only while a human is actually at the keyboard.
pub(crate) const TYPING_HOT_TAIL_NS: u64 = 250_000_000;

/// Arm the window: a human is waiting on the UI thread — a key press, a
/// wheel/trackpad gesture, a mouse button press, input a control client or
/// controller host writes — so the PTY reader must keep its term-lock holds
/// FINE (and the gather its short batch budget) until [`TYPING_HOT_TAIL_NS`]
/// after the LAST such input. Hover motion is deliberately NOT an arming
/// event: it is not a wait, and it would keep the reader on twice the lock
/// round-trips through any mouse movement.
///
/// Armed at the TRUE hardware arrival (the `WindowEvent::KeyboardInput` arm),
/// BEFORE any press-path `term_lock` — that ordering is the whole point. An
/// earlier design armed the signal deep inside `input_to_session`, i.e. AFTER
/// three-to-four terminal-mutex acquisitions had already queued behind
/// whole-64-KiB reader holds; only the final `seam_egress` acquisition
/// benefited from the signal the keystroke itself had set.
pub(crate) fn arm() {
    TYPING_HOT_UNTIL_NS.store(
        crate::metrics::now_ns().saturating_add(TYPING_HOT_TAIL_NS),
        Ordering::Relaxed,
    );
}

/// Whether a human is actively typing (armed by [`arm`], decaying
/// [`TYPING_HOT_TAIL_NS`] after the last input). THRU-2's ingest signal: the
/// PTY reader keeps its term-lock holds FINE (8 KiB slices) inside this window
/// and takes wider holds otherwise — so the interactivity bound the chunking
/// exists for is enforced exactly when a human is waiting on it, and a pure
/// output flood pays far fewer lock round-trips. Read-only, lock-free, Relaxed
/// (a stale read for one hold is harmless: the next hold re-reads, and a
/// mid-hold arrival waits at most one capped process — sub-frame).
#[must_use]
pub(crate) fn pending() -> bool {
    let until = TYPING_HOT_UNTIL_NS.load(Ordering::Relaxed);
    until != 0 && crate::metrics::now_ns() < until
}
