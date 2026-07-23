// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// SHARED model of the scrollback-offload DETACH WINDOW, `include!`d by BOTH the
// compile-time gate (`build.rs`) and the conformance/lockstep tests — ONE source of
// truth, so the gate and the code-binding cannot check different specs. Requires
// `Model` + `ty_model` in scope at the include site.
//
// The offload detaches the tiered store, rewraps it OFF the lock on a worker, then
// re-attaches. While detached, concurrent steps interleave in ALL orderings:
//   Produce  — the PTY reader scrolls a line off (environment)
//   Erase    — an ED3 / `clear` lands (environment)
//   Reattach — the worker finishes and re-attaches (clean completion)
//   Abort    — the worker DIED; `abort_reflow_offload` recovers to a bounded state
//              (window output discarded BY DESIGN — loss is accepted on abort, the
//              alternative is a permanently wedged grid; see audit #5)
//
// Invariants (found by the adversarial audit; L1 history-integrity class):
//   NoLoss            (bug B): on a CLEAN completion, every line produced during the
//                     window is SAFE — still staged (`retained`) or relocated into the
//                     re-attached tiered store (`relocated`, the shadow counter for the
//                     opaque store lane the real `drain_lazy_buffer` moves lines into).
//                     `slack` = W while the window is open or after an abort, 0 after a
//                     clean Reattach — so the invariant is exact at clean completion
//                     and honestly waived mid-window/abort.
//   ErasedStaysErased (bug C): an erase during the window is never resurrected.
//
// `Buggy=0` is the shipped fix; `Buggy=1` reintroduces bugs B + C (must be CAUGHT —
// the gate also requires the Buggy=1 counterexample, so the invariants can't go
// vacuous).

/// The detach window as a bounded state machine (see file comment above).
fn offload_window_model() -> Model {
    ty_model! {
        ScrollbackOffloadWindow {
            const W = 3;         // bounded lines that scroll off during the window
            const Buggy = 0;     // 0 = shipped fix; 1 = pre-fix bugs B + C
            var detached = 1;    // store detached for the off-thread reflow (window open)
            var produced = 0;    // lines that scrolled off during the window
            var retained = 0;    // ... of those, kept (staged to lazy_buffer)
            var relocated = 0;   // ... of those, moved lazy_buffer -> re-attached store
                                 // (the drain_lazy_buffer RELOCATE at re-attach: still
                                 // SAFE, not loss — the shadow lane for the opaque
                                 // tiered store the projection cannot see into)
            var cleared = 0;     // a `clear` / ED3 landed during the window
            var done = 0;        // window closed (Reattach or Abort)
            var aborted = 0;     // closed via Abort (worker death) — loss accepted
            var resurrected = 0; // erased history came back after the window (bug C)
            var slack = 3;       // = W while loss is excusable; 0 after clean Reattach

            // PTY output scrolls a line off during the window. Fixed: stage to the
            // lazy buffer (retained++). Buggy B: drop it once the ring is full.
            action Produce when (detached > 0 && done <= 0 && produced <= W - 1) {
                produced = produced + 1;
                retained = if Buggy > 0 { retained } else { retained + 1 };
            }

            // A `clear` / ED3 lands during the window (bumps the clear generation).
            // The erase also destroys the ALREADY-STAGED window lines (the real
            // `erase_scrollback` clears the lazy buffer) — that is the user's clear,
            // not loss — so the produced/retained "debt" resets: NoLoss thereafter
            // covers only post-erase output.
            action Erase when (detached > 0 && done <= 0 && cleared <= 0) {
                cleared = 1;
                produced = 0;
                retained = 0;
            }

            // Worker finishes; re-attach applies the fix logic. Fixed: an erase during
            // the window drops the stale pre-erase store — nothing resurrects. Buggy C:
            // re-attaches the pre-erase store even though it was cleared. Clean
            // completion drops `slack` to 0, arming NoLoss exactly here.
            action Reattach when (detached > 0 && done <= 0) {
                detached = 0;
                done = 1;
                resurrected = if Buggy > 0 { cleared } else { 0 };
                slack = 0;
            }

            // Worker died; `abort_reflow_offload` recovers to a bounded state. Window
            // output is discarded BY DESIGN (retained = 0) and nothing re-attaches
            // (no resurrection possible). `slack` stays W: loss is excused on abort.
            action Abort when (detached > 0 && done <= 0) {
                detached = 0;
                done = 1;
                aborted = 1;
                retained = 0;
                resurrected = 0;
                slack = 3;
            }

            // A produced line is accounted for while staged (`retained`) OR after the
            // re-attach drain relocated it into the tiered store (`relocated`). The
            // hand actions never write `relocated` (it stays 0, so this reads exactly
            // as the historical `produced <= retained + slack`); the DERIVED Reattach
            // of the full-verbatim real body performs the conserving transfer
            // `relocated' = relocated + retained, retained' = 0` — the discard bug
            // (clearing the lazy buffer instead of draining it) breaks the sum.
            invariant NoLoss: produced <= retained + relocated + slack;
            invariant ErasedStaysErased: resurrected <= 0;
            // The window CLOSES: detached and done are never both set. This is the
            // invariant the abort-wedge violates at the MODEL level (an Abort that
            // fails to clear `detached` yields the state (detached=1, done=1)) — the
            // extraction RFC's Tier-A target: a wedged `abort_reflow_offload` derives
            // an Abort action with no `detached' = 0` update, and ty fails the BUILD.
            invariant WindowCloses: detached + done <= 1;
        }
    }
}
