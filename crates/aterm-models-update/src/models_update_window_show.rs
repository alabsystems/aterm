// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The self-update handoff's window show state: each window's full screen, minimized state,
//! place in the stack and the key window cross the update, and the successor puts them back in
//! an order that never costs the update, never moves the user to another Space while they are
//! in another app, and never takes the keyboard from a window the user typed into before
//! Commit. A click alone before Commit is not seen (the successor defers keys, not clicks), so
//! a window chosen only by a click gives the keyboard back to the carried key window at Commit:
//! `Steal` without `Type`.

use aterm_spec::derive::Model;

/// One restore's carried show state in the process that restored it
/// (`aterm-gui/src/window_show.rs`, `App::carried_window_show`).
///
/// State: `lane` — 0 not yet restored, 1 an update's successor, 2 a cold launch restoring a
/// quit's manifest. `revealed` — every carried window has had its first present. `proof` — the
/// update lane's adoption proof is written (every window painted, the restore drained, the
/// control service ready). `stacked` — the carried stack is applied (minimize, then raise back
/// to front, the key window last). `keyed` — the carried key window holds the keyboard. `typed`
/// — the user typed into another carried window before Commit. `committed` — this process owns
/// every window (at once on a cold launch; at Commit on the update lane). `front` — aterm is the
/// active app, as its focus records say. `fs` — a carried full screen was re-entered, and
/// `fsfront` whether aterm was in front then.
///
/// The environment's moves are the OS's and the user's: `Reveal` (a window's first present),
/// `Activate`/`Deactivate`, `Steal` (the keyboard moves to another carried window before Commit
/// — a click, or AppKit's own pick on activation) and `Type` (typing reaches that window). The
/// App's are `Prove` (timed by the boot: the presents, the control service), `Stack`, `Commit`
/// (the outgoing process's answer to the proof) and `EnterFullScreen`.
///
/// `Type` is enabled from the reveal, not from the stack: a revealed window takes keys once on
/// glass (a click on it activates the successor), and the proof the stack waits for can be
/// seconds later. The keyboard is then on a window the reveal order left key (`keyed == 0`); a
/// stack that raised the carried key window over it would split the typing across two
/// terminals, so the stack keys the window typed into (`keyed` stays 0 with `typed == 1`) and
/// Commit leaves it there.
///
/// * `StackOnlyOnGlass` — the stack never raises a window before its first present (a raise is
///   a reveal, and the reveal-at-first-present rule forbids a frame before the carried pixels).
/// * `StackOnlyAtProof` — on the update lane the stack goes on only in the proof path's call:
///   the proof waits for every window's frame, and a window the stack minimizes presents nothing
///   again, so a stack in between could leave the proof unwritten until the parent rolls back.
/// * `FullScreenOnlyAfterCommit` — entering full screen resizes the window, and before Commit
///   the successor shares every PTY with a parked process that may yet resume.
/// * `FullScreenOnlyInFront` — a full-screen re-entry never pulls a user in another app into
///   aterm's new Space.
/// * `KeyWindowAtCommit` — once committed and stacked, the carried key window holds the keyboard
///   unless the user typed elsewhere.
/// * `TypingKeepsItsWindow` — a window typed into before Commit keeps the keyboard.
///
/// `Buggy = 1` admits each failure alone: a stack that raises before the reveal or the proof, a
/// full screen entered at the stack (before Commit, in front or not), a stack that leaves the
/// key where the reveal order left it, and a Commit that re-keys the carried window over the
/// one typed into.
///
/// Tier-1 (`aterm-gui/src/window_show_conformance.rs`) replays schedules on the real `App` carry
/// — the settle the event loop runs after every event, the real proof path
/// (`maybe_signal_handoff_ready`, writing a real proof), the Commit arm's call and the focus
/// arm's — projects every variable at every step, requires the real code to act the moment the
/// model lets it, and replays `Buggy = 1` as the negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_window_show_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateWindowShow {
            const Buggy = 0;
            var lane = 0;
            var revealed = 0;
            var proof = 0;
            var stacked = 0;
            var keyed = 0;
            var typed = 0;
            var committed = 0;
            var front = 0;
            var fs = 0;
            var fsfront = 0;

            action Launch when (lane == 0) {
                lane = 1;
            }
            action ColdLaunch when (lane == 0) {
                lane = 2;
                committed = 1;
            }
            action Reveal when (lane > 0 && revealed == 0) {
                revealed = 1;
            }
            action Prove when (lane == 1 && revealed == 1 && proof == 0) {
                proof = 1;
            }
            action Stack when (
                lane > 0
                    && stacked == 0
                    && ((revealed == 1 && (lane == 2 || proof == 1)) || Buggy == 1)
            ) {
                stacked = 1;
                keyed = if typed == 1 || Buggy == 1 { 0 } else { 1 };
            }
            action Activate when (front == 0) {
                front = 1;
            }
            action Deactivate when (front == 1) {
                front = 0;
            }
            action Steal when (lane == 1 && committed == 0 && stacked == 1 && keyed == 1) {
                keyed = 0;
            }
            action Type when (lane == 1 && committed == 0 && revealed == 1 && keyed == 0) {
                typed = 1;
            }
            action Commit when (lane == 1 && committed == 0 && proof == 1) {
                committed = 1;
                keyed = if stacked == 1 && (typed == 0 || Buggy == 1) { 1 } else { keyed };
            }
            action EnterFullScreen when (
                stacked == 1
                    && fs == 0
                    && (committed == 1 || Buggy == 1)
                    && (front == 1 || Buggy == 1)
            ) {
                fs = 1;
                fsfront = front;
            }

            invariant StackOnlyOnGlass: stacked == 0 || revealed == 1;
            invariant StackOnlyAtProof: lane == 2 || stacked == 0 || proof == 1;
            invariant FullScreenOnlyAfterCommit: fs == 0 || committed == 1;
            invariant FullScreenOnlyInFront: fs == 0 || fsfront == 1;
            invariant KeyWindowAtCommit:
                committed == 0 || stacked == 0 || typed == 1 || keyed == 1;
            invariant TypingKeepsItsWindow: committed == 0 || typed == 0 || keyed == 0;
        }
    }
}
