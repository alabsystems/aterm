// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The live upgrade's Codex records are collected.
//!
//! A Codex record is filed per tab (`<state>/upgrade/codex-<tab>.json`: the Codex lane's
//! upgrade, a codex-alab's crash relaunch), and a tab id is never minted twice, so a closed
//! tab's record would be kept for ever: the Claude lane's week-old prune (`prune_states`)
//! passes Codex records by. `codex_alab_drive::prune` (`upgrade_drive`'s) removes one once its
//! tab is proven gone (`codex_alab_drive::registry_life`), it has sat unwritten a week, and its
//! round is `upgrade_drive::settled`. The Tier-1 bind in aterm-agent's `codex_alab_drive` tests
//! runs the real `prune` over records on disk and holds each decision to `Prune`'s guard in
//! both directions.

use aterm_spec::derive::*;

/// One Codex record and its tab. The world: the tab closes for good (`TabCloses`); while it
/// lives the round begins a restart (`Begin`), settles (`Settle`: done, failed, or pending with
/// no word from the owner) and rearms (`Rearm`); a hand-run pass fails a restart left in flight
/// in a closed tab (`Expire`); a week passes over a record nothing writes (`Week`), and every
/// write restarts it (`old = 0`). The lane's one removal is `Prune`.
///
/// `tab` 1 while the tab lives; `stage` 0 open, 1 a restart in flight, 2 settled; `old` 1 once
/// the file has sat unwritten a week; `kept` 1 while on disk; `aged` 1 once a week has passed
/// after the tab closed and the round settled (history `Prune`'s guard never reads).
///
/// Each invariant is caught by its own mutant: `OnlyAGoneTabsRecordGoes` (`PruneLiveTab`: a
/// prune reading only the host's own roster, so another window's live tab reads gone),
/// `OnlyASettledRecordGoes` (`PruneInFlight`), `OnlyAWeekOldRecordGoes` (`PruneFresh`), and
/// `AClosedSettledRecordStaysAged` (`Rewrite`), the safety half of
/// [`harness_upgrade_state_collection_liveness`]: the real code writes a closed tab's record
/// only to expire a restart in flight, which a settled record never has.
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

            // The round moves only when its live tab is visited.
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
            // A hand-run pass fails a closed tab's restart left in flight (`expired`).
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
            // `codex_alab_drive::prune`: a proven-gone tab's settled record, a week old.
            action Prune when (kept == 1 && tab == 0 && stage == 2 && old == 1) {
                kept = 0;
            }

            // Mutants (Buggy = 1).
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
            // A pass that rewrites a closed tab's record at every look.
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

/// A closed tab's settled record goes: `[]<>(kept = 0 \/ tab = 1 \/ stage <= 1)`. A closed
/// tab's open record (an announcement, an owner's word, a restart no hand-run pass expires) is
/// kept, as the Claude lane keeps it.
///
/// Fairness: `Week` (the clock moves) and `Prune` (every pass that reads the tab gone takes it:
/// the prune runs under the sweep lock in a window host's step, taken only when an upgrade is
/// due, and in a hand-run pass, so this assumes such passes keep coming). The world is not
/// fair. `Rewrite` alone breaks it; removing `Prune` is the Tier-0 test's negative control.
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
