// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Check completion provenance, joins, and scheduler/boot lock release.
use super::Model;

#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_check_receipt_model() -> Model {
    crate::ty_model! {
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
    crate::ty_model! {
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
    crate::ty_model! {
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
    crate::ty_model! {
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
