// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LIVE UPGRADE'S CODEX RECORDS ARE COLLECTED (gap #22, 2026-09-29).
//!
//! A Codex upgrade is filed per TAB (`<state>/upgrade/codex-<tab>.json`,
//! `aterm_agent::harness::upgrade_drive`'s `codex` module), and until this
//! slice only the visit forgot one — when a current Codex led that tab again.
//! A tab id is never minted twice, so the record of every tab that closed was
//! kept for ever: the Claude lane's week-old prune (`prune_states`) passes
//! Codex records by. `codex::prune` removes one once its tab is proven gone
//! (the rendezvous registry, `codex::registry_life`), it has sat unwritten for
//! the Claude lane's week, and its upgrade is settled (`upgrade_drive::
//! settled`: done, failed, or pending with no word of the owner's).
//!
//! As everywhere in this crate the model is hand-written Rust DESCRIBING that
//! code: the Tier-1 bind in aterm-agent's `upgrade_codex_drive` tests runs the
//! real `codex::prune` over records on disk, projected onto these variables,
//! and holds each of its decisions to `Prune`'s guard in both directions.

use super::*;

/// ONE CODEX RECORD AND ITS TAB. The world: the tab closes (`TabCloses`) and
/// never comes back; while it lives, the upgrade's round moves on the record —
/// begins a restart (`Begin`), settles (`Settle`: done, failed, or a
/// pending round the owner said nothing about), and starts a new round
/// (`Rearm`); a hand-run pass fails a restart left in flight in a closed tab
/// (`Expire`, the orphan pass's `expired`); and the week passes over a record
/// nothing writes (`Week`). Every write starts the week again (`old = 0`).
/// The lane: `Prune`, the one removal.
///
/// Variables: `tab` 1 while the tab lives; `stage` 0 open (announced, or an
/// owner's word on it), 1 a restart in flight (exiting, relaunched), 2
/// settled; `old` 1 once the file has sat unwritten a week; `kept` 1 while
/// the record is on disk; `aged` 1 once a week has passed over the record
/// AFTER its tab closed and its round settled — history, which the one
/// removal's guard never reads.
///
/// Three invariants, each caught by its own `Buggy` member alone:
/// `OnlyAGoneTabsRecordGoes` (`PruneLiveTab`: a prune reading the host's own
/// roster alone, so another window's live tab reads gone),
/// `OnlyASettledRecordGoes` (`PruneInFlight`: a restart in flight taken from
/// the orphan pass that would finish it) and `OnlyAWeekOldRecordGoes`
/// (`PruneFresh`: the prune without the Claude lane's week). The fourth,
/// `AClosedSettledRecordStaysAged` (`Rewrite`: a pass that writes a closed
/// tab's record again), is the safety half of the liveness property
/// ([`harness_upgrade_state_collection_liveness`]): nothing writes a closed
/// tab's settled record, so once its week has passed it stays prunable. The
/// real code writes a closed tab's record only to expire a restart left in
/// flight (`Expire`), which a settled record never has.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn harness_upgrade_state_collection_model() -> Model {
    crate::ty_model! {
        HarnessUpgradeStateCollection {
            const Buggy = 0;
            var tab = 1;
            var stage = 0;
            var old = 0;
            var kept = 1;
            var aged = 0;

            // The round, in a live tab only: a record moves when its tab is
            // visited.
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
            // A hand-run pass fails a restart left in flight in a closed tab
            // (the orphan pass's `expired`): it settles, written now.
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
            // `codex::prune`: a proven-gone tab's settled record, a week old.
            action Prune when (kept == 1 && tab == 0 && stage == 2 && old == 1) {
                kept = 0;
            }

            // The mutants (Buggy = 1).
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
            // A pass that writes a closed tab's record again at every look:
            // the week never passes over it.
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

/// "A CLOSED TAB'S SETTLED RECORD GOES": `[]<>(kept = 0 \/ tab = 1 \/ stage
/// <= 1)` — again and again, the record is removed, or its tab lives, or its
/// upgrade is not settled. It says nothing of a closed tab's OPEN
/// record (an announcement, an owner's word, a restart left in flight that no
/// hand-run pass expires): those are kept, as the Claude lane keeps them.
///
/// Fairness, and nothing more: `Week`, weakly fair — the clock moves; `Prune`,
/// weakly fair — every pass that reads the tab gone takes it (every sweep runs
/// the prune under the sweep lock: a window host's step, which a look takes
/// only when an upgrade is due — the look itself (`due`) reads and prunes
/// nothing — and a hand-run pass; so this assumes such passes keep coming).
/// The world — the tab closing, the round's steps, a hand-run pass — is not
/// fair.
///
/// The mutant that breaks it alone: `Rewrite`, a pass that writes a closed
/// tab's record at every look, so the week never passes. The historical
/// defect — no `Prune` at all for a Codex record — is the property's own
/// negative control in the Tier-0 test: with `Prune` removed, a closed tab's
/// settled record is kept for ever.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
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
