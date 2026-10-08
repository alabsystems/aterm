// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The unread-input gate on socket-driven input — the rule that a driver's key
//! is never queued behind input a program the watch has established is not
//! reading has left unread, and that nothing else is refused.
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
/// never gated, and a socket driver, gated — the reader's states (reading or
/// frozen, stopped or running, raw or canonical), and the input WATCH that
/// publishes a stall.
///
/// AGES ARE BANDS, not ticks: `a1` is the oldest unread write's band — `0`
/// younger than `WARN_AFTER` (1 s), `1` from there to `STALL_AFTER` (10 s),
/// `2` past it — and a `Tick` moves it to the next band. `a2` is the second
/// write's band; the queue is FIFO, so consuming one promotes the second's.
///
/// THE WATCH publishes (`Publish`) only a program that is not reading, whose
/// raw input has aged into the stall band — its evidence, in aterm-session's
/// `classify`: spinning and silent, or asleep, past thresholds the machine's
/// load stretches. `reading == 0` is that evidence. It withdraws the stall
/// (`Withdraw`) once the queue is read, turns canonical, or holds only input
/// younger than the stall.
///
/// THE GATE is aterm-session's `input_backlog::gate`, restated (owner
/// direction, 2026-09-29: caps are warnings, and nothing blocks that does not
/// have to). A driver key is REFUSED only when the queue is not empty and
/// the job is stopped, or a stall is published and the raw oldest write is
/// still in the stall band. Every other key is written; one written behind
/// raw input in the warn band or older carries the warning (`warned`).
///
/// Three latches, one per way the gate has been, or could be, wrong:
///
/// * `behind_stalled` — a driver key landed behind input of a stopped job or
///   of a program published as not reading: the 2026-09-24 incident, where a
///   screen-fenced key queued behind a frozen Claude Code's Enter and was
///   read hours later (`NoDriverKeyBehindStalledInput`);
/// * `refused_live` — a driver key was refused with neither a stop nor a
///   published stall: the 1 s static cap the owner overruled on 2026-09-29,
///   which blocked every driver of a healthy program on a loaded machine
///   (`NoRefusalWithoutAnEstablishedStall`);
/// * `healthy_published` — the watch published a program that was reading
///   (merely slow): a stall judged on age alone
///   (`NoReadingProgramPublished`).
///
/// `Buggy = 1` is all three defects at once — the screen-only fence admits
/// every key, the refusal fires on the static warn age, and the watch
/// publishes on age alone — and each invariant, isolated, is falsified by
/// its own.
///
/// Tier-1 (`aterm-session/tests/conformance_input_unread.rs`) drives a real
/// pty whose raw slave never reads, projects the sink's reading onto
/// `raw`/`q`/`a1` at every step and the model's `published` onto the gate's
/// argument, checks the shipped `gate` against `DriverKey`/`DriverRefused`
/// and the warning, and `classify`'s entry against `Publish`, and replays
/// the incident and the static cap as negative controls.
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
