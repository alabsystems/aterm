// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The live upgrade's Codex records are collected.

use aterm_spec::derive::*;

/// One Codex record and its tab.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_state_collection_model() -> Model {
    aterm_spec::ty_model! {
        HarnessUpgradeStateCollection {
            const Buggy = 0;
            var tab = 1;
            var stage = 0;
            var old = 0;
            var kept = 1;
            var aged = 0;

            action Begin when (kept == 1 && tab == 1 && stage == 0) {
                stage = 1;
                old = 0;
            }
            action Settle when (kept == 1 && tab == 1 && stage <= 1) {
                stage = 2;
                old = 0;
            }
            action Rearm when (kept == 1 && tab == 1 && stage == 2) {
                stage = 0;
                old = 0;
            }
            action Expire when (kept == 1 && tab == 0 && stage == 1) {
                stage = 2;
                old = 0;
            }
            action TabCloses when (tab == 1) {
                tab = 0;
            }
            action Week when (kept == 1 && old == 0) {
                aged = if (tab == 0 && stage == 2) { 1 } else { aged };
                old = 1;
            }
            action Prune when (kept == 1 && tab == 0 && stage == 2 && old == 1) {
                kept = 0;
            }

            action PruneLiveTab when (
                Buggy == 1 && kept == 1 && tab == 1 && stage == 2 && old == 1
            ) {
                kept = 0;
            }
            action PruneInFlight when (
                Buggy == 1 && kept == 1 && tab == 0 && stage == 1 && old == 1
            ) {
                kept = 0;
            }
            action PruneFresh when (
                Buggy == 1 && kept == 1 && tab == 0 && stage == 2 && old == 0
            ) {
                kept = 0;
            }
            action Rewrite when (Buggy == 1 && kept == 1 && tab == 0 && old == 1) {
                old = 0;
            }

            invariant OnlyAGoneTabsRecordGoes: kept == 1 || tab == 0;
            invariant OnlyASettledRecordGoes: kept == 1 || stage == 2;
            invariant OnlyAWeekOldRecordGoes: kept == 1 || old == 1;
            invariant AClosedSettledRecordStaysAged: kept == 0 || aged == 0 || old == 1;
        }
    }
}

/// A closed tab's settled record goes: `[]<>(kept = 0 \/ tab = 1 \/ stage <= 1)`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(harness_upgrade_state_collection_model)]
pub fn harness_upgrade_state_collection_liveness() -> Liveness {
    Liveness {
        name: "AClosedTabsSettledRecordGoes",
        goal: or_(
            eq(var("kept"), int(0)),
            or_(eq(var("tab"), int(1)), le(var("stage"), int(1))),
        ),
        weak: vec!["Week", "Prune"],
        strong: vec![],
        mutants: vec!["Rewrite"],
    }
}
