// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A health warning that crosses a self-update. The successor keeps each warning its
//! landing does not answer and hands it to the producer whose own proof heals it
//! (`App::take_up_carried_update_health`); kept with no healer, it would stand for the
//! whole launch.
//!
//! A writer/reader pair (the shape of `NativeUpdateFailedMarkSuppression`). The writers
//! are the producers that owe the heal: the updater's announced word
//! (`aterm_update::adopt_announced_health`) and the watchdog
//! (`CheckerWatchState::adopt_carried_stall`). The reader is the window's one door
//! (`App::note_update_health`, `App::heal_update_health_where`), which ends a warning, and
//! its "To look at" line, only on the proof that answers it.

use aterm_spec::derive::Model;

/// `NativeUpdateCarriedHealth`: a carried check warning and a carried stall warning,
/// through the successor's seed (`App::seed_carried_messages`), the ledger's episodes and
/// this process's own checks.
///
/// `phase`: 0 the parent said both warnings, 1 the successor is seeded. `ledger`: the
/// machine's check streak is failing (any process's good check heals it). `owed`: this
/// process's updater owes the check's recovery. `up`/`entry`: the check warning stands /
/// its "To look at" line is claimed. `checked`: a check of this process completed since
/// the stall warning. `armed`: the watchdog owes the stall's heal. `stall`/`sentry`: the
/// stall warning and its line.
///
/// Each invariant is broken alone by one `Buggy = 1` action:
/// * `WarningHasHealer` — by `SeedRemembersOnly`; `StallHasHealer` — by
///   `SeedStallRemembersOnly`.
/// * `StallStandsUntilACheckHere` — only a check of this process ends the stall, never the
///   ledger's healing; by `RecoverHealsStall`.
/// * `CheckLineFollowsWarning` — by `SeedForgetsEntry`; `StallLineFollowsWarning` — by
///   `StallHealKeepsEntry`.
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

            // The successor's seed: every carried kind kept, handed to its
            // healer here, and its digest line claimed again.
            action Seed when (phase == 0) {
                phase = 1;
                owed = 1;
                up = 1;
                entry = 1;
                armed = 1;
                stall = 1;
                sentry = 1;
            }
            // The environment: the shared ledger heals, or fails again.
            action LedgerHeals when (phase == 1 && ledger == 1) {
                ledger = 0;
            }
            action LedgerFails when (phase == 1 && ledger == 0) {
                ledger = 1;
            }
            // The updater's recovery, owed: the check warning ends, its line
            // with it, never the stall.
            action Recover when (phase == 1 && ledger == 0 && owed == 1) {
                owed = 0;
                up = 0;
                entry = 0;
            }
            // The updater announces a new episode it owes nothing for.
            action Announce when (phase == 1 && ledger == 1 && owed == 0) {
                owed = 1;
                up = 1;
                entry = 1;
            }
            // The environment: this process's checker completes a check.
            action CheckCompletes when (phase == 1 && checked == 0) {
                checked = 1;
            }
            // The watchdog's heal of the stall, on that check.
            action StallHeals when (phase == 1 && checked == 1 && armed == 1) {
                armed = 0;
                stall = 0;
                sentry = 0;
            }

            // Mutants, dead at Buggy = 0.
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
