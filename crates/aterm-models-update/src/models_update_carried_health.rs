// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of health warnings carried across a self-update.

use aterm_spec::derive::Model;

/// `NativeUpdateCarriedHealth`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_carried_health_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateCarriedHealth {
            const Buggy = 0;
            var phase = 0;
            var ledger = 1;
            var owed = 0;
            var up = 0;
            var entry = 0;
            var checked = 0;
            var armed = 0;
            var stall = 0;
            var sentry = 0;

            action Seed when (phase == 0) {
                phase = 1;
                owed = 1;
                up = 1;
                entry = 1;
                armed = 1;
                stall = 1;
                sentry = 1;
            }
            action LedgerHeals when (phase == 1 && ledger == 1) {
                ledger = 0;
            }
            action LedgerFails when (phase == 1 && ledger == 0) {
                ledger = 1;
            }
            action Recover when (phase == 1 && ledger == 0 && owed == 1) {
                owed = 0;
                up = 0;
                entry = 0;
            }
            action Announce when (phase == 1 && ledger == 1 && owed == 0) {
                owed = 1;
                up = 1;
                entry = 1;
            }
            action CheckCompletes when (phase == 1 && checked == 0) {
                checked = 1;
            }
            action StallHeals when (phase == 1 && checked == 1 && armed == 1) {
                armed = 0;
                stall = 0;
                sentry = 0;
            }

            action SeedRemembersOnly when (Buggy == 1 && phase == 0) {
                phase = 1;
                up = 1;
                entry = 1;
                armed = 1;
                stall = 1;
                sentry = 1;
            }
            action SeedStallRemembersOnly when (Buggy == 1 && phase == 0) {
                phase = 1;
                owed = 1;
                up = 1;
                entry = 1;
                stall = 1;
                sentry = 1;
            }
            action RecoverHealsStall when (
                Buggy == 1 && phase == 1 && ledger == 0 && owed == 1
            ) {
                owed = 0;
                up = 0;
                entry = 0;
                stall = 0;
                sentry = 0;
            }
            action SeedForgetsEntry when (Buggy == 1 && phase == 0) {
                phase = 1;
                owed = 1;
                up = 1;
                armed = 1;
                stall = 1;
                sentry = 1;
            }
            action StallHealKeepsEntry when (
                Buggy == 1 && phase == 1 && checked == 1 && armed == 1
            ) {
                armed = 0;
                stall = 0;
            }

            invariant WarningHasHealer: up == 0 || owed == 1;
            invariant StallHasHealer: stall == 0 || armed == 1;
            invariant StallStandsUntilACheckHere: phase == 0 || checked == 1 || stall == 1;
            invariant CheckLineFollowsWarning: up == entry;
            invariant StallLineFollowsWarning: stall == sentry;
        }
    }
}
