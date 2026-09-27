// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The self-update successor's PRE-COMMIT INPUT QUEUE (2026-09-26, gap #33):
//! when the queue may be called incoherent, and what the replay at Commit
//! does about it.
//!
//! Input typed into a successor's revealed window waits in a bounded queue
//! until Commit and is replayed then, in order. A focus loss is exempt from
//! that deferral and clears the window's ambient modifier snapshot LIVE, so it
//! can run out of order against input already queued for the window — a ⌘
//! press queued, then ⌘-Tab away, replays ⌘ down with nothing to release it.
//! The queue therefore carries an `incoherent` flag, and a replay under it
//! disarms the ambient modifiers rather than trust them.
//!
//! Until this date that flag was set by EVERY focus loss while a handoff was
//! pending — and winit's macOS backend queues a synthetic `Focused(false)` as
//! it creates each window, which a successor does for every window it adopts
//! before anything can be typed. So every handoff (17 of 17 in the owner's
//! log) was repaired and logged as incoherent, a real dropped keystroke could
//! not be told from the noise, and a modifier the user really held was
//! disarmed at every Commit.

use super::Model;

/// One window's pre-Commit queue in the successor (`aterm-gui/src/lib.rs`:
/// `App::queue_pre_commit_input`, the focus arm's
/// `App::pre_commit_focus_loss_reorders_queue`, and
/// `App::settle_replayed_handoff_input` at Commit).
///
/// State: `queued` — input for the window is waiting in the queue. `reordered`
/// — the window lost focus with input already queued for it, so the live
/// clear ran ahead of that input. `dropped` — the queue refused an event past
/// its cap (a press/release pair may be broken). `incoherent` — the queue's
/// flag. `settled` — Commit replayed the queue. `repaired` — the replay
/// disarmed the ambient modifiers. `stranded` — the replay trusted a stream
/// that was reordered or lost events, so a modifier whose release the user
/// sent may still be armed.
///
/// The environment's moves are the user's and the platform's: `Type` (input
/// reaches the window), `LoseFocus` (any focus loss: the user's ⌘-Tab, winit's
/// startup event for a new window, the flip Commit's activation causes) and
/// `Drop` (the queue is full). `Settle` is the Commit arm's.
///
/// Invariants: `ReorderedInputIsRepaired` — a stream that was reordered or
/// lost events is never replayed on trust (the Cmd-W misfire: a stranded ⌘
/// turns the first replayed `w` into a closed tab). `RepairOnlyForACause` —
/// a replay disarms the modifiers only when the stream was really reordered or
/// really lost an event, so the repair — and its line in the log and its
/// `input_incoherent=` count — is a signal a lost keystroke can be read from.
///
/// `Buggy = 1` is the queue's two historical defects, each caught alone: in
/// `LoseFocus`, the latch this replaced, verbatim (EVERY loss marks the queue,
/// so every handoff is repaired for winit's startup event), caught by
/// `RepairOnlyForACause`; and in `Drop`, the silent drop the round-5 audit
/// found in this queue (an event refused past the cap marks nothing, so the
/// release half of a chord can be lost and the press replayed on trust),
/// caught by `ReorderedInputIsRepaired`. The over-correction — a loss after
/// input was queued that marks nothing — is the committed `LoseFocus`
/// disagreeing with the code, which Tier-1 reports at that step.
///
/// Tier-1 (`aterm-gui/src/precommit_input_conformance.rs`) replays every
/// schedule of this machine up to four steps on the real `App` — the shipping
/// queue, `on_focus` and settle — projects every variable at every step, and
/// replays the `Buggy = 1` machine as the negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_precommit_input_model() -> Model {
    crate::ty_model! {
        NativeUpdatePreCommitInput {
            const Buggy = 0;
            var queued = 0;
            var reordered = 0;
            var dropped = 0;
            var incoherent = 0;
            var settled = 0;
            var repaired = 0;
            var stranded = 0;

            action Type when (settled == 0) {
                queued = 1;
            }
            action LoseFocus when (settled == 0) {
                reordered = if queued == 1 { 1 } else { reordered };
                incoherent = if queued == 1 || Buggy == 1 { 1 } else { incoherent };
            }
            action Drop when (settled == 0 && queued == 1) {
                dropped = 1;
                incoherent = if Buggy == 1 { incoherent } else { 1 };
            }
            action Settle when (settled == 0) {
                settled = 1;
                repaired = incoherent;
                stranded = if (reordered == 1 || dropped == 1) && incoherent == 0 { 1 } else { 0 };
            }

            invariant ReorderedInputIsRepaired: stranded == 0;
            invariant RepairOnlyForACause: repaired == 0 || reordered == 1 || dropped == 1;
        }
    }
}
