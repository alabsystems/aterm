// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The self-update successor's pre-Commit input queue: when it may be called incoherent, and
//! what the replay at Commit does about it.
//!
//! Input typed into a successor's revealed window waits in a bounded queue until Commit and is
//! replayed then, in order. A focus loss is exempt from that deferral and clears the window's
//! ambient modifier snapshot live, so it can run out of order against input already queued — a
//! ⌘ press queued, then ⌘-Tab away, replays ⌘ down with nothing to release it. A replay under
//! the queue's `incoherent` flag therefore disarms the ambient modifiers rather than trust them.
//! A focus loss sets the flag only with input already queued: winit's macOS backend queues a
//! synthetic `Focused(false)` as it creates each window, which a successor does for every
//! window it adopts, so marking every loss would repair every handoff and hide real drops.

use super::Model;

/// One window's pre-Commit queue in the successor (`aterm-gui/src/lib.rs`:
/// `App::queue_pre_commit_input`, the focus arm's `App::pre_commit_focus_loss_reorders_queue`,
/// and `App::settle_replayed_handoff_input` at Commit).
///
/// State: `queued` — input for the window is waiting. `reordered` — the window lost focus with
/// input already queued, so the live clear ran ahead of it. `dropped` — the queue refused an
/// event past its cap (a press/release pair may be broken). `incoherent` — the queue's flag.
/// `settled` — Commit replayed the queue. `repaired` — the replay disarmed the ambient
/// modifiers. `stranded` — the replay trusted a stream that was reordered or lost events, so a
/// modifier whose release the user sent may still be armed.
///
/// The environment's moves: `Type` (input reaches the window), `LoseFocus` (any focus loss: the
/// user's ⌘-Tab, winit's startup event for a new window, the flip Commit's activation causes)
/// and `Drop` (the queue is full). `Settle` is the Commit arm's.
///
/// * `ReorderedInputIsRepaired` — a reordered or lossy stream is never replayed on trust (a
///   stranded ⌘ turns the first replayed `w` into a closed tab).
/// * `RepairOnlyForACause` — a replay disarms the modifiers only when the stream really was
///   reordered or lost an event, so the repair, its log line and its `input_incoherent=` count
///   are a signal a lost keystroke can be read from.
///
/// `Buggy = 1` has two defects, each caught alone: `LoseFocus` marks the queue on every loss
/// (so every handoff is repaired for winit's startup event), caught by `RepairOnlyForACause`;
/// `Drop` marks nothing (the release half of a chord is lost and the press replayed on trust),
/// caught by `ReorderedInputIsRepaired`. The over-correction — a loss after queued input that
/// marks nothing — is the committed `LoseFocus` disagreeing with the code, which Tier-1 reports
/// at that step. Tier-1 (`aterm-gui/src/precommit_input_conformance.rs`) replays every schedule
/// up to four steps on the real `App` (the shipping queue, `on_focus` and settle), projects
/// every variable at every step, and replays `Buggy = 1` as the negative control.
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
