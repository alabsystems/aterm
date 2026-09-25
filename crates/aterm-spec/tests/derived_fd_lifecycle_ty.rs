// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tier-0 for the A7 fd-lifecycle DERIVED model.
//!
//! `fd_lifecycle_model()` (machine `FdLifecycle`) is the drift-free, code-bound twin
//! of the `SinkWriter` PTY-master ownership discipline in `aterm-session/src/sink.rs`
//! (initiative A7, WS-G). It SUPERSEDES the hand-written `FdLifecycle.tla`, which is
//! now quarantined to `aterm-spec-models/specs/legacy/` — exactly as the kernel
//! family was when its derived twins took over; `aterm-spec-models`'
//! `model_check.rs` refuses it back into the active checked set.
//!
//! **Tier-0 prove-AND-catch** (tiered, VERIFY-1): the in-process interpreter BFSes
//! the whole bounded `MaxClones = 3` state space at the committed `Buggy = 0` and
//! requires both invariants (`NoUseAfterClose`, `ClosedImpliesNoClones`) on every
//! reachable state; at `Buggy = 1` the defect rides the always-live `DropClone`
//! action (it closes the fd on a NON-last drop — the pre-fix bare-`i32`
//! out-of-band close), so a subsequent `UseFd` latches a use-after-close and a
//! counterexample MUST be found — the proof is non-vacuous. Wherever the Trust
//! toolchain is installed, `ty` re-proves both arms of the generated TLA+ and must
//! explore the same number of states as the interpreter; a disagreement panics.
//!
//! No separate action-set pin: every action is load-bearing for this proof.
//! Deleting `Clone` or `DropClone` leaves `Buggy = 1` with no counterexample, so
//! the catch arm fails here; deleting `UseFd` leaves `NoUseAfterClose`
//! unfalsifiable, which `non_vacuity_ratchet.rs` refuses as a ghost invariant.

use aterm_spec::derive::fd_lifecycle_model;
use aterm_spec::verify;

#[test]
fn derived_fd_lifecycle_proves_and_catches_use_after_close() {
    verify::prove_and_catch_scalar(
        &fd_lifecycle_model(),
        "derived FdLifecycle spec (out-of-band close use-after-close)",
    );
}
