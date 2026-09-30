// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The successor's boot around its claim: what it may do before the outgoing
//! process has handed anything over (docs/DESIGN-warm-successor-2026-09-29.md,
//! "The derived model and invariants").

use super::*;

/// `NativeUpdateSuccessorWarmBeforeClaim`: a successor may WARM before its
/// claim — spawn threads, present into hidden windows — but may not mutate the
/// process environment once a thread exists, own anything the outgoing process
/// still owns, show anything, or let a warm present prove.
///
/// `phase`: 0 Launched, 1 Warming (the prologue after the handoff snapshot and
/// before the dial), 2 Dialled, 3 Claimed, 4 Adopted, 5 Proved, 6 Committed,
/// 7 Exited.
///
/// THE P1 HALF (`HandoffEnv`, aterm-gui's `handoff_env.rs`) is what makes the
/// first invariant hold by construction: `Capture` is the one environment
/// mutation, it happens in `Launched` before any `SpawnWarm`, and the claimed
/// descriptors land in the snapshot rather than in `environ`.
/// `PublishToEnv` (dead at `Buggy = 0`) is `ClaimedHandoff::publish`, which
/// wrote them into the process environment after the claim: harmless while no
/// thread could exist before the claim, a data race the moment one does (P2).
///
/// THE P2 ORDER (aterm-gui's `warm_prologue`, placed by `warm_order`): on the
/// launched lane the whole claim-independent prologue — config, fonts, the
/// backend worker — runs BEFORE the dial (`SpawnWarm`, then `WarmDone` when its
/// main-thread section returns), and the dial, which the outgoing process
/// parks on, comes only after it. Three facts bound that order:
///
/// * `relative_socket`: an explicit RELATIVE control socket's intake restores
///   the parent's binding directory with a `chdir` (`PrepareSocketDir`), whose
///   contract is a single-threaded process. That launch keeps the late order —
///   `SpawnLate`, after the intake — and `NoCwdChangeAfterSpawn` is what the
///   ordering decision buys.
/// * `build_size_ok`: before the dial the backend is built at the advisory
///   launch hint's font size; a hint the authenticated carry contradicts
///   (`StaleHint`) is re-selected at the join (`Reshape`) before any carried
///   screen is presented (`CarriedPresentAtCarriedSize`). A stale hint costs a
///   warm, never a frame at the wrong size.
/// * `warm_done`: the prologue waits on NO thread it spawned — the dial never
///   joins the backend worker (`PrologueJoinsNoThread`). `WarmDone` is weakly
///   fair because it is straight-line main-thread work; the liveness claim
///   ([`native_update_successor_warm_before_claim_liveness`]) is that every
///   successor proves or exits, and `JoinWorkerBeforeDial` (a prologue that
///   waits on a worker the environment may never finish) breaks both.
///
/// Each other `Buggy = 1` mutant is its own action, so none masks another:
/// `BindWhileWarming` takes an owned resource (the control socket) before the
/// claim; `RevealPlaceholder` shows a warm window over the frozen parent;
/// `WarmProves` lets a warm present stand for the carried screens; `FoldHint`
/// folds the advisory launch hint into the digests;
/// `WarmBeforeDialOnRelativeSocket` warms before the dial on the one shape
/// whose intake changes directory; `PresentAtHintedSize` presents the carried
/// screens at a stale hint's size. Tier-1 (aterm-gui's
/// `seamless::handoff_env_conformance`) drives the real snapshot, dial and
/// intake, the real order decision, the real miss decision and the hint's
/// exclusion from the adoption proof; the rest is waived there by name.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_successor_warm_before_claim_model() -> Model {
    crate::ty_model! {
        NativeUpdateSuccessorWarmBeforeClaim {
            const Buggy = 0;
            var phase = 0;
            var captured = 0;
            var threads_spawned = 0;
            var env_mut_after_spawn = 0;
            var owned_taken = 0;
            var window_visible = 0;
            var warm_presented = 0;
            var carried_presented = 0;
            var claim_ok = 0;
            var hint_in_digest = 0;
            var relative_socket = 0;
            var warm_done = 0;
            var socket_dir_ready = 0;
            var cwd_mut_after_spawn = 0;
            var build_size_ok = 1;

            // The environment: this launch's control socket is an explicit
            // relative path (decided by its flags, before anything runs).
            action RelativeSocket when (phase == 0 && relative_socket == 0) {
                relative_socket = 1;
            }
            // The one environment mutation: read and clear every handoff name,
            // while no thread exists.
            action Capture when (phase == 0) {
                captured = 1;
                env_mut_after_spawn = threads_spawned;
                phase = 1;
            }
            // The warm prologue before the dial: never on the relative shape.
            action SpawnWarm when (phase == 1 && threads_spawned == 0 && relative_socket == 0) {
                threads_spawned = 1;
            }
            // Its main-thread section returns, waiting on none of its threads.
            action WarmDone when (phase == 1 && threads_spawned == 1 && warm_done == 0) {
                warm_done = 1;
            }
            action WarmPresent when (phase == 1 && warm_presented == 0) {
                warm_presented = 1;
            }
            // The environment: the launch hint the backend was built at is not
            // what the carry will say (a zoom during the hold, a hostile hint).
            action StaleHint when (phase == 1 && relative_socket == 0 && build_size_ok == 1) {
                build_size_ok = 0;
            }
            action Dial when (
                phase == 1 && captured == 1 && (warm_done == 1 || relative_socket == 1)
            ) {
                phase = 2;
            }
            action ClaimOk when (phase == 2) {
                phase = 3;
                claim_ok = 1;
            }
            action ClaimFail when (phase == 2) {
                phase = 7;
            }
            action TakeOwned when (phase == 3 && claim_ok == 1 && owned_taken == 0) {
                owned_taken = 1;
            }
            action Adopt when (phase == 3 && owned_taken == 1) {
                phase = 4;
            }
            // The relative shape's intake: `chdir` to the parent's directory.
            action PrepareSocketDir when (
                phase == 4 && relative_socket == 1 && socket_dir_ready == 0
            ) {
                socket_dir_ready = 1;
                cwd_mut_after_spawn = threads_spawned;
            }
            // The late order: the prologue after the intake.
            action SpawnLate when (
                phase == 4 && relative_socket == 1 && socket_dir_ready == 1 && threads_spawned == 0
            ) {
                threads_spawned = 1;
                warm_done = 1;
            }
            // A warm miss, corrected at the backend join.
            action Reshape when (phase == 4 && build_size_ok == 0) {
                build_size_ok = 1;
            }
            action RealPresent when (
                phase == 4 && carried_presented == 0 && warm_done == 1 && build_size_ok == 1
            ) {
                carried_presented = 1;
            }
            action Reveal when (claim_ok == 1 && carried_presented == 1 && window_visible == 0) {
                window_visible = 1;
            }
            action Prove when (phase == 4 && carried_presented == 1) {
                phase = 5;
            }
            action Commit when (phase == 5) {
                phase = 6;
            }

            // Mutants, dead at Buggy = 0.
            action PublishToEnv when (Buggy == 1 && phase == 3 && env_mut_after_spawn == 0) {
                env_mut_after_spawn = threads_spawned;
            }
            action BindWhileWarming when (Buggy == 1 && phase == 1 && owned_taken == 0) {
                owned_taken = 1;
            }
            action RevealPlaceholder when (Buggy == 1 && phase == 1 && window_visible == 0) {
                window_visible = 1;
            }
            action WarmProves when (
                Buggy == 1 && phase == 4 && warm_presented == 1 && carried_presented == 0
            ) {
                phase = 5;
            }
            action FoldHint when (Buggy == 1 && phase == 4 && hint_in_digest == 0) {
                hint_in_digest = 1;
            }
            action WarmBeforeDialOnRelativeSocket when (
                Buggy == 1 && phase == 1 && relative_socket == 1 && threads_spawned == 0
            ) {
                threads_spawned = 1;
                warm_done = 1;
            }
            action PresentAtHintedSize when (
                Buggy == 1 && phase == 4 && carried_presented == 0 && warm_done == 1
                    && build_size_ok == 0
            ) {
                carried_presented = 1;
            }
            // (liveness) The prologue joins the backend worker before it dials,
            // and the worker never finishes (a GPU driver that does not answer).
            action JoinWorkerBeforeDial when (
                Buggy == 1 && phase == 1 && threads_spawned == 1 && warm_done == 0
            ) {
                warm_done = 2;
            }

            invariant NoEnvMutationAfterSpawn: env_mut_after_spawn == 0;
            invariant NothingOwnedBeforeClaim: owned_taken == 0 || claim_ok == 1;
            invariant NeverRevealedBeforeClaim:
                (window_visible == 0 && carried_presented == 0) || claim_ok == 1;
            invariant WarmPresentNeverProves: phase <= 4 || phase == 7 || carried_presented == 1;
            invariant HintNotInProof: hint_in_digest == 0;
            invariant FailedClaimLeavesNothing:
                phase <= 6 || claim_ok == 1 || (owned_taken == 0 && window_visible == 0);
            invariant NoCwdChangeAfterSpawn: cwd_mut_after_spawn == 0;
            invariant CarriedPresentAtCarriedSize: carried_presented == 0 || build_size_ok == 1;
            // `warm_done = 2` is a prologue parked on a thread it spawned.
            invariant PrologueJoinsNoThread: warm_done <= 1;
        }
    }
}

/// [`native_update_successor_warm_before_claim_model`]'s progress claim (warm
/// successor P2): every successor PROVES or EXITS — the warm before the dial
/// can delay the dial, never withhold it.
///
/// Fairness, each an assumption someone owns:
///
/// * `Capture`, `SpawnWarm`, `WarmDone`, `Dial` — the prologue is straight-line
///   main-thread work that joins none of its threads, and the dial follows it;
/// * `ClaimFail`, weakly fair — the claim's own deadlines (the dial budget and
///   the grant hold, `CLAIM_DIAL_BUDGET`/`GRANT_HOLD_BUDGET`) end every claim
///   that is not granted; the grant itself (`ClaimOk`) is the outgoing
///   process's and is assumed nothing;
/// * `TakeOwned`, `Adopt`, `PrepareSocketDir`, `SpawnLate`, `Reshape`,
///   `RealPresent`, `Prove` — the successor's own boot after the claim.
///
/// The mutant that breaks it alone: `JoinWorkerBeforeDial` — a prologue that
/// waits on its backend worker before dialling hangs with the worker, and the
/// outgoing process's dial budget is then the only thing that ends it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_successor_warm_before_claim_liveness() -> Liveness {
    Liveness {
        name: "TheWarmSuccessorProvesOrExits",
        goal: gt(var("phase"), int(4)),
        weak: vec![
            "Capture",
            "SpawnWarm",
            "WarmDone",
            "Dial",
            "ClaimFail",
            "TakeOwned",
            "Adopt",
            "PrepareSocketDir",
            "SpawnLate",
            "Reshape",
            "RealPresent",
            "Prove",
        ],
        strong: vec![],
        mutants: vec!["JoinWorkerBeforeDial"],
    }
}
