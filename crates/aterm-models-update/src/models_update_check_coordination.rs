// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Models for update-check provenance, joins, lock release, and two-process staged-build apply.
use aterm_spec::derive::{Liveness, Model, cst, eq, int, le, or_, var};

/// Immutable bundle files retain their identity when the installed path is swapped.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_installed_cache_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateInstalledCache {
            const Buggy = 0;
            var phase = 0;
            var path_file = 1;
            var opened_file = 0;
            var cached_file = 0;
            var cached_build = 0;
            var observed_file = 0;
            var observed_build = 0;

            action Open when (phase == 0) {
                opened_file = path_file;
                phase = 1;
            }
            action Replace when (path_file == 1) { path_file = 2; }
            action Restore when (path_file == 2) { path_file = 1; }
            action Read when (phase == 1) {
                cached_file = opened_file;
                cached_build = if Buggy == 1 { path_file } else { opened_file };
                phase = 2;
            }
            action Observe when (phase == 2) {
                observed_file = path_file;
                observed_build = if cached_file == path_file { cached_build } else { path_file };
                phase = 3;
            }

            invariant ObservedBuildMatchesFile: phase <= 2 || observed_build == observed_file;
        }
    }
}

/// Queued progress keeps its origin across checker replacement and host delivery.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_progress_delivery_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateProgressDelivery {
            const Buggy = 0;
            var generation = 1;
            var queued = 0;
            var origin = 0;
            var delivered = 0;
            var delivered_generation = 0;
            var accepted = 0;

            action QueueCurrent when (queued == 0 && delivered == 0) {
                queued = 1;
                origin = generation;
            }
            action QueueManual when (queued == 0 && delivered == 0) {
                queued = 1;
                origin = 0;
            }
            action Replace when (generation == 1) {
                generation = 2;
            }
            action Deliver when (queued == 1) {
                accepted = if origin == 0 {
                    if Buggy == 2 { 0 } else { 1 }
                } else {
                    if origin == generation {
                        if Buggy == 3 { 0 } else { 1 }
                    } else {
                        if Buggy == 1 { 1 } else { 0 }
                    }
                };
                delivered_generation = generation;
                delivered = 1;
                queued = 0;
            }

            invariant RetiredProgressIgnored:
                delivered == 0 || origin == 0 || origin == delivered_generation || accepted == 0;
            invariant ManualProgressDelivered:
                delivered == 0 || origin > 0 || accepted == 1;
            invariant CurrentProgressDelivered:
                if delivered == 1 && origin == delivered_generation { accepted == 1 } else { accepted <= 1 };
        }
    }
}

/// An answer that opens the gate, or the updater's own named hold, comes before this boot
/// chains another update; asks nothing answers open the gate after `UnansweredTries`.
///
/// `state` is 0 awaiting, 1 in flight, 2 open (confirmed, or settled at its bound). `Failure`
/// is an answer that the trial is still pending: the updater holds, names why (`held`) and is
/// asked again. `Unanswered` is an ask the updater could not answer, `Unavailable` one that
/// was never queued, `Overdue` one whose answer never came; each counts toward
/// `UnansweredTries`, after which the apply lane's own admission decides. So a miss leaves the
/// gate awaiting either held by the updater's own answer or counted (`AMissIsHeldOrCounted`).
///
/// Dial 1 is `UnansweredForgotten` alone, because the liveness audit isolates its mutant on the
/// `Buggy = 1` baseline and the dead-action audit credits it only where that baseline holds:
/// the forgotten count lands lawful after a named hold (`held == 1`), so it is no violation
/// step by itself. The mutants written inside committed actions are dials 2 (`Unavailable`),
/// 3 (`Queue`) and 4 (`Apply`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_boot_health_confirmation_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateBootHealthConfirmation {
            const Buggy = 0;
            const UnansweredTries = 5;
            var state = 0;
            var outstanding = 0;
            var missed = 0;
            var retry = 0;
            var applied = 0;
            var health_at_apply = 0;
            var unanswered = 0;
            var held = 0;
            var released_at_apply = 0;

            action Queue when ((state == 0 || (Buggy == 3 && state == 1)) && outstanding <= 1) {
                state = 1;
                outstanding = outstanding + 1;
                missed = 0;
                retry = 0;
                held = 0;
            }
            action Unavailable when (state == 0) {
                missed = 1;
                retry = if Buggy == 2 { 0 } else { 1 };
                unanswered =
                    if unanswered + 1 <= UnansweredTries { unanswered + 1 } else { unanswered };
            }
            action Failure when (state == 1) {
                state = 0;
                outstanding = 0;
                missed = 1;
                retry = 1;
                unanswered = 0;
                held = 1;
            }
            action Unanswered when (state == 1) {
                state = 0;
                outstanding = 0;
                missed = 1;
                retry = 1;
                unanswered =
                    if unanswered + 1 <= UnansweredTries { unanswered + 1 } else { unanswered };
            }
            action Overdue when (state == 1) {
                state = 0;
                outstanding = 0;
                missed = 1;
                retry = 1;
                unanswered =
                    if unanswered + 1 <= UnansweredTries { unanswered + 1 } else { unanswered };
            }
            action Confirm when (state == 1) {
                state = 2;
                outstanding = 0;
                missed = 0;
                retry = 0;
                unanswered = 0;
            }
            action Apply when (
                applied == 0
                    && (state == 2
                        || UnansweredTries <= unanswered
                        || (Buggy == 4 && state == 1))
            ) {
                applied = 1;
                health_at_apply = state;
                released_at_apply = if UnansweredTries <= unanswered { 1 } else { 0 };
            }

            action UnansweredForgotten when (Buggy == 1 && state == 0 && unanswered > 0) {
                unanswered = 0;
            }

            invariant ConfirmedBeforeApply:
                applied == 0 || health_at_apply == 2 || released_at_apply == 1;
            invariant RetryAfterMiss: missed == 0 || state > 0 || retry == 1;
            invariant OneConfirmationInFlight: outstanding <= 1;
            invariant AMissIsHeldOrCounted:
                state > 0 || missed == 0 || held == 1 || unanswered > 0;
            bound UnansweredIsFinite: unanswered <= UnansweredTries;
        }
    }
}

/// [`native_update_boot_health_confirmation_model`]'s progress claim: every ask ends with the
/// gate open or with the updater holding by name. The mutant drops the unanswered count.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(native_update_boot_health_confirmation_model)]
pub fn native_update_boot_health_confirmation_liveness() -> Liveness {
    Liveness {
        name: "TheGateOpensOrTheUpdaterHolds",
        goal: or_(
            eq(var("state"), int(2)),
            or_(
                le(cst("UnansweredTries"), var("unanswered")),
                eq(var("held"), int(1)),
            ),
        ),
        weak: vec!["Queue", "Overdue"],
        strong: vec![],
        mutants: vec!["UnansweredForgotten"],
    }
}

#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_check_receipt_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateCheckReceipt {
            const Buggy = 0;
            var actual_age = 0;
            var stamped_age = 0;
            var same_source = 1;
            var same_build = 1;
            var written = 0;
            var skipped = 0;
            action Tick when (actual_age <= 1) {
                actual_age = actual_age + 1;
                stamped_age = stamped_age + 1;
                skipped = 0;
            }
            action StatusWrite when (written == 0) {
                stamped_age = if Buggy == 1 { 0 } else { stamped_age };
                written = 1;
                skipped = 0;
            }
            action ChangeSource when (same_source == 1) { same_source = 0; skipped = 0; }
            action ChangeBuild when (same_build == 1) { same_build = 0; skipped = 0; }
            action Skip when (skipped == 0 && stamped_age <= 1 && same_source == 1 && same_build == 1) {
                skipped = 1;
            }
            invariant OnlyCompletedCheckDefers:
                skipped == 0 || (actual_age <= 1 && same_source == 1 && same_build == 1);
        }
    }
}

#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_check_join_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateCheckJoin {
            const Buggy = 0;
            var fresh = 0;
            var same_source = 1;
            var same_build = 1;
            var joined = 0;
            var poisoned = 0;
            var recovered = 0;
            action Complete when (fresh == 0 && joined == 0 && poisoned == 0) { fresh = 1; }
            action ChangeSource when (same_source == 1 && joined == 0) { same_source = 0; }
            action ChangeBuild when (same_build == 1 && joined == 0) { same_build = 0; }
            action Panic when (poisoned == 0 && joined == 0) {
                poisoned = 1;
                recovered = 0;
                fresh = 0;
            }
            action Recover when (poisoned == 1 && joined == 0) {
                poisoned = if Buggy == 1 { 1 } else { 0 };
                recovered = 1;
                fresh = 0;
            }
            action Join when (joined == 0 && ((fresh == 1 && same_source == 1 && same_build == 1 && poisoned == 0) || Buggy == 1)) {
                joined = 1;
            }
            invariant ExactCompletedRequest:
                joined == 0 || (fresh == 1 && same_source == 1 && same_build == 1 && poisoned == 0);
            invariant RecoveryClearsPoison: recovered == 0 || poisoned == 0;
        }
    }
}

#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_check_wait_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateCheckWait {
            const Buggy = 0;
            var lane_held = 1;
            var file_held = 1;
            var sleeping = 0;
            action Wait when (sleeping == 0) {
                file_held = 0;
                lane_held = if Buggy == 1 { 1 } else { 0 };
                sleeping = 1;
            }
            invariant NoSleepingCheckOwner:
                sleeping == 0 || (lane_held == 0 && file_held == 0);
        }
    }
}

#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_boot_health_lock_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateBootHealthLock {
            const Buggy = 0;
            var held = 1;
            var expired = 0;
            var returned = 0;
            var counted = 0;
            action Timeout when (held == 1 && expired == 0) {
                expired = 1;
                returned = if Buggy == 1 { 0 } else { 1 };
            }
            invariant LaunchDoesNotAwaitHeldLock:
                expired == 0 || (returned == 1 && counted == 0);
        }
    }
}

/// Two processes arm one staged build and it is swapped in once.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_two_process_apply_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateTwoProcessApply {
            const Buggy = 0;
            var ready = 1;
            var a_done = 0;
            var b_peeked = 0;
            var b_done = 0;
            var swaps = 0;
            action BPeek when (b_peeked == 0 && ready == 1) { b_peeked = 1; }
            action ASwap when (a_done == 0 && ready == 1) {
                a_done = 1;
                ready = 0;
                swaps = swaps + 1;
            }
            action BApply when (b_peeked == 1 && b_done == 0) {
                b_done = 1;
                swaps = if Buggy == 1 || ready == 1 { swaps + 1 } else { swaps };
                ready = 0;
            }
            invariant OneSwapPerBuild: swaps <= 1;
        }
    }
}
