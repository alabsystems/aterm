// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The unread-input gate on socket-driven input — the rule that a driver's key
//! is never queued behind input the program has left unread.
//!
//! 2026-09-24: a Claude Code grown past 38 GiB stopped reading its terminal.
//! The owner's Enter sat unread in the slave's input queue for hours, and a
//! supervisor, fencing its keys on the SCREEN (which a frozen program never
//! redraws, so every fence passed), wrote a `down` straight into the same
//! queue. When the program next read, one `read()` returned both keystrokes
//! and it acted on a key aimed at a screen it had never drawn. The screen
//! cannot say this; the kernel's input queue can, and aterm-session's
//! `SinkWriter::input_backlog` reads it and dates its oldest byte.

use super::Model;

/// A raw-mode program's input queue (at most `Cap` unread writes, each with
/// an age), the two writers that feed it — the human's window keyboard,
/// never gated, and a socket driver, gated — and the reader's states:
/// reading or frozen, stopped or running, raw or canonical.
///
/// The gate is aterm-session's `input_backlog::refuses`, restated: a driver
/// key is admitted when the queue is empty, or when the job is running and
/// either the slave is canonical (only complete lines are counted there, and
/// a complete line a shell has not asked for is ordinary type-ahead) or the
/// oldest unread write is younger than `Refuse` ticks. A stopped job refuses
/// at any age. `behind_stale` latches when a driver key lands behind input
/// that is stale by that same rule, and `NoDriverKeyBehindStaleInput` says it
/// never does.
///
/// One `Tick` is 500 ms of the oldest byte's wait, so `Refuse = 2` is
/// `REFUSE_AFTER` (1 s) and `a1` projects `min(wait_ms / 500, AgeMax)`.
/// `a1` and `a2` are the ages of the first and second unread writes; the
/// queue is FIFO, so consuming one promotes the second's age. `Buggy = 1`
/// admits every driver key the queue has room for — the screen-only fence
/// the incident ran under, which could not see the queue at all.
///
/// Tier-1 (`aterm-session/tests/conformance_input_unread.rs`) drives a real
/// pty whose raw slave never reads, projects the sink's reading onto
/// `raw`/`q`/`a1` at every step, checks the shipped `refuses` against
/// `DriverKey`'s guard, and replays the incident as the negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn input_unread_gate_model() -> Model {
    crate::ty_model! {
        InputUnreadGate {
            const Buggy = 0;
            const Cap = 2;
            const Refuse = 2;
            const AgeMax = 3;
            var raw = 1;
            var reading = 1;
            var stopped = 0;
            var q = 0;
            var a1 = 0;
            var a2 = 0;
            var behind_stale = 0;

            action HumanKey when (q <= Cap - 1) {
                a1 = if q == 0 { 0 } else { a1 };
                a2 = if q == 1 { 0 } else { a2 };
                q = q + 1;
            }
            action DriverKey when (q <= Cap - 1
                && (Buggy == 1 || q == 0 || (stopped == 0 && (raw == 0 || a1 <= Refuse - 1))))
            {
                behind_stale = if q > 0 && (stopped == 1 || (raw == 1 && Refuse <= a1)) {
                    1
                } else {
                    behind_stale
                };
                a1 = if q == 0 { 0 } else { a1 };
                a2 = if q == 1 { 0 } else { a2 };
                q = q + 1;
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

            invariant NoDriverKeyBehindStaleInput: behind_stale == 0;
        }
    }
}
