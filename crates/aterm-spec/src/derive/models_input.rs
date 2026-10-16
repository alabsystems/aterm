// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The unread-input gate on socket-driven input: a driver's key is never queued behind unread
//! input of a program established as not reading, and nothing else is refused.
//!
//! A screen fence cannot see a program that stopped reading: it never redraws, so every fence
//! passes, and when it next reads one `read()` returns the old key and the driver's together.
//! The kernel's input queue can: aterm-session's `SinkWriter::input_backlog` reads it and dates
//! its oldest byte.

use super::Model;

/// A raw-mode program's input queue (at most `Cap` unread writes, each with an age), its two
/// writers (the window keyboard, never gated; a socket driver, gated), the reader's states
/// (reading or frozen, stopped or running, raw or canonical), and the watch that publishes a
/// stall.
///
/// Ages are bands, not ticks: `a1` is the oldest unread write's band (`0` younger than
/// `WARN_AFTER`, `1` up to `STALL_AFTER`, `2` past it); `Tick` advances it. `a2` is the second
/// write's band; the queue is FIFO, so consuming one promotes the second's.
///
/// The watch publishes (`Publish`) only a program that is not reading (`reading == 0`, standing
/// for `classify`'s evidence) whose raw input has aged into the stall band, and withdraws it
/// once the queue is read, turns canonical, or holds only younger input.
///
/// The gate restates `input_backlog::gate`: a driver key is refused only when the queue is not
/// empty and the job is stopped, or a stall is published and the raw oldest write is in the
/// stall band. Every other key is written; one behind raw input in the warn band or older
/// carries the warning (`warned`).
///
/// One latch per way the gate could be wrong:
///
/// * `behind_stalled`: a driver key landed behind input of a stopped job or of a program
///   published as not reading (`NoDriverKeyBehindStalledInput`);
/// * `refused_live`: a driver key was refused with neither a stop nor a published stall, as a
///   static age cap would (`NoRefusalWithoutAnEstablishedStall`);
/// * `healthy_published`: the watch published a program that was reading, merely slow
///   (`NoReadingProgramPublished`).
///
/// `Buggy = 1` is all three defects (a gate that admits every key, a refusal on the warn age, a
/// watch that publishes on age alone); each invariant is falsified by its own.
///
/// Tier-1: `aterm-session/tests/conformance_input_unread.rs` drives a real pty whose raw slave
/// never reads and checks the shipped `gate` and `classify` against the model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn input_unread_gate_model() -> Model {
    crate::ty_model! {
        InputUnreadGate {
            const Buggy = 0;
            const Cap = 2;
            const Warn = 1;
            const Stall = 2;
            const AgeMax = 2;
            var raw = 1;
            var reading = 1;
            var stopped = 0;
            var q = 0;
            var a1 = 0;
            var a2 = 0;
            var published = 0;
            var warned = 0;
            var behind_stalled = 0;
            var refused_live = 0;
            var healthy_published = 0;

            action HumanKey when (q <= Cap - 1) {
                a1 = if q == 0 { 0 } else { a1 };
                a2 = if q == 1 { 0 } else { a2 };
                q = q + 1;
            }
            action DriverKey when (q <= Cap - 1
                && (Buggy == 1
                    || q == 0
                    || (stopped == 0 && (published == 0 || raw == 0 || a1 <= Stall - 1))))
            {
                behind_stalled = if q > 0
                    && (stopped == 1 || (published == 1 && raw == 1 && Stall <= a1))
                {
                    1
                } else {
                    behind_stalled
                };
                warned = if q > 0 && raw == 1 && Warn <= a1 { 1 } else { 0 };
                a1 = if q == 0 { 0 } else { a1 };
                a2 = if q == 1 { 0 } else { a2 };
                q = q + 1;
            }
            action DriverRefused when (q > 0
                && (stopped == 1
                    || (published == 1 && raw == 1 && Stall <= a1)
                    || (Buggy == 1 && raw == 1 && Warn <= a1)))
            {
                refused_live = if stopped == 0
                    && (published == 0 || raw == 0 || a1 <= Stall - 1)
                {
                    1
                } else {
                    refused_live
                };
            }
            action Publish when (published == 0 && stopped == 0 && raw == 1 && q > 0
                && Stall <= a1 && (reading == 0 || Buggy == 1))
            {
                healthy_published = if reading == 1 { 1 } else { healthy_published };
                published = 1;
            }
            action Withdraw when (published == 1 && (q == 0 || raw == 0 || a1 <= Stall - 1)) {
                published = 0;
            }
            action Consume when (reading == 1 && stopped == 0 && q > 0) {
                a1 = if q == 2 { a2 } else { 0 };
                a2 = 0;
                q = q - 1;
            }
            action Tick when (q > 0 && a1 <= AgeMax - 1) {
                a1 = a1 + 1;
                a2 = if q == 2 { a2 + 1 } else { a2 };
            }
            action Freeze when (reading == 1) {
                reading = 0;
            }
            action Thaw when (reading == 0) {
                reading = 1;
            }
            action StopJob when (stopped == 0) {
                stopped = 1;
            }
            action ContJob when (stopped == 1) {
                stopped = 0;
            }
            action ModeFlip when (raw <= 1) {
                raw = if raw == 1 { 0 } else { 1 };
            }

            invariant NoDriverKeyBehindStalledInput: behind_stalled == 0;
            invariant NoRefusalWithoutAnEstablishedStall: refused_live == 0;
            invariant NoReadingProgramPublished: healthy_published == 0;
        }
    }
}
