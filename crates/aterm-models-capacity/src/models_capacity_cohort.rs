// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Durable spawn custody and the transition from the old capacity ledger.

use aterm_spec::derive::*;

/// One admitted reservation and a second heavy request, each needing the entire abstract
/// budget. Closing every guard and losing both the holder and group leader cannot prove that
/// the group's remaining work ended. Only a matching actual no-child outcome can cancel an
/// armed birth; a bound group finishes on a conclusive same-scope drain. A new boot of the
/// same host can retire any readable armed or bound birth without calling it a cancellation.
/// A foreign host with a different boot is still unknown. An unreadable journal
/// cannot be treated as an ordinary description, even when its guard locks are free.
///
/// The production projection uses phase 0 Description, 1 Armed, 2 Bound, 3 Unknown;
/// lifetime 0 Unknown, 1 Present, 2 Drained; and scope 0 same, 1 foreign, 2 unknown,
/// 3 same-host-new-boot. `spawn_outcome` is 0 unresolved, 1 actual child, 2 matching actual
/// NoChild, 3 a foreign or mismatched NoChild. These integers are observations, not authority
/// constructors. `journal_valid` is independent of phase so a malformed-tail downgrade cannot
/// erase its own witness. The namespace fence and retained predecessor barrier govern every
/// new admission, including a forced request. Environment changes stop at that admission, so
/// its invariants judge the facts at the decision rather than later external damage.
///
/// Buggy 1 reaps on free guards; 2 releases on a dead leader; 3 checks boot before host;
/// 4 ignores a torn tail; 5 credits numeric process memory against a whole reservation and
/// admits overlapping work; 6 lets an old reader bypass the namespace fence under force;
/// 7 cancels with a different or nonexistent NoChild proof; 8 spawns before durable ARM.
/// Deliberate process-group escape requires separate confinement and is outside this model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn capacity_cohort_model() -> Model {
    aterm_spec::ty_model! {
        CapacityCohort {
            const Buggy = 0;

            var phase = 0;
            var lifetime = 2;
            var full_budget = 1;
            var original_holder_alive = 1;
            var guards_held = 1;
            var group_leader_alive = 0;
            var group_member_alive = 0;
            var scope = 0;
            var namespace_ready = 0;
            var unknown_predecessor = 1;
            var second_admitted = 0;
            var arm_durable = 0;
            var spawn_outcome = 0;
            var journal_valid = 1;
            var cancelled = 0;
            var forced = 0;

            action PublishNamespaceFence when (second_admitted == 0 && namespace_ready == 0) {
                namespace_ready = 1;
            }
            action ProvePredecessorDrain when (second_admitted == 0 && unknown_predecessor == 1) {
                unknown_predecessor = 0;
            }
            action LoseNamespaceReadiness when (second_admitted == 0 && namespace_ready == 1) {
                namespace_ready = 0;
            }
            action ObserveUnknownPredecessor when (second_admitted == 0 && unknown_predecessor == 0) {
                unknown_predecessor = 1;
            }
            action RequestForce when (second_admitted == 0 && forced == 0) {
                forced = 1;
            }
            action DurableArm when (second_admitted == 0 && phase == 0 && full_budget == 1
                && arm_durable == 0 && original_holder_alive == 1 && guards_held == 1
                && scope == 0 && journal_valid == 1 && namespace_ready == 1
                && unknown_predecessor == 0) {
                phase = 1;
                lifetime = 0;
                arm_durable = 1;
            }
            action ActualSpawn when (second_admitted == 0 && phase == 1 && arm_durable == 1
                && spawn_outcome == 0 && original_holder_alive == 1 && scope == 0) {
                spawn_outcome = 1;
                group_member_alive = 1;
                group_leader_alive = 1;
            }
            action ActualNoChild when (second_admitted == 0 && phase == 1
                && spawn_outcome == 0 && original_holder_alive == 1 && scope == 0) {
                spawn_outcome = 2;
            }
            action ForeignNoChild when (second_admitted == 0 && phase == 1 && spawn_outcome == 0) {
                spawn_outcome = 3;
            }
            action CancelMatchingNoChild when (second_admitted == 0 && phase == 1
                && spawn_outcome == 2 && original_holder_alive == 1 && scope == 0
                && journal_valid == 1) {
                phase = 0;
                lifetime = 2;
                cancelled = 1;
            }
            action BindActualChild when (second_admitted == 0 && phase == 1
                && spawn_outcome == 1 && original_holder_alive == 1 && scope == 0
                && journal_valid == 1) {
                phase = 2;
                lifetime = 1;
            }
            action DropAllGuards when (second_admitted == 0 && guards_held == 1) {
                guards_held = 0;
            }
            action HolderDies when (second_admitted == 0 && original_holder_alive == 1) {
                original_holder_alive = 0;
            }
            action GroupLeaderDies when (second_admitted == 0 && group_leader_alive == 1
                && group_member_alive == 1) {
                group_leader_alive = 0;
            }
            action GroupActuallyDrains when (second_admitted == 0 && phase == 2
                && scope == 0 && group_member_alive == 1) {
                group_member_alive = 0;
                group_leader_alive = 0;
                lifetime = 2;
            }
            action ScopeBecomesForeign when (second_admitted == 0 && phase > 0
                && scope == 0 && lifetime <= 1) {
                scope = 1;
                lifetime = 0;
            }
            action ScopeBecomesUnreadable when (second_admitted == 0 && phase > 0
                && scope == 0 && lifetime <= 1) {
                scope = 2;
                lifetime = 0;
            }
            action SameHostReboots when (second_admitted == 0 && phase > 0 && scope == 0) {
                scope = 3;
                lifetime = 2;
                group_member_alive = 0;
                group_leader_alive = 0;
                original_holder_alive = 0;
                guards_held = 0;
            }
            action TailBecomesUnreadable when (second_admitted == 0 && phase > 0
                && journal_valid == 1) {
                phase = 3;
                journal_valid = 0;
                lifetime = 0;
            }
            action FinishDrainedCohort when (second_admitted == 0
                && (phase == 1 || phase == 2) && journal_valid == 1 && lifetime == 2
                && ((phase == 2 && scope == 0) || scope == 3)) {
                phase = 0;
            }
            action RetireDescription when (second_admitted == 0 && phase == 0
                && journal_valid == 1 && full_budget == 1 && group_member_alive == 0
                && (scope == 0 || scope == 3)
                && (arm_durable == 0 || cancelled == 1 || lifetime == 2)) {
                full_budget = 0;
                guards_held = 0;
            }
            action AdmitSecond when (second_admitted == 0 && full_budget == 0
                && namespace_ready == 1 && unknown_predecessor == 0) {
                second_admitted = 1;
            }

            action ReapFreeGuards when (Buggy == 1 && second_admitted == 0 && phase > 0
                && guards_held == 0 && full_budget == 1) {
                full_budget = 0;
            }
            action ReleaseDeadLeader when (Buggy == 2 && second_admitted == 0 && phase == 2
                && group_leader_alive == 0 && group_member_alive == 1 && full_budget == 1) {
                full_budget = 0;
            }
            action ForeignBootMeansDrained when (Buggy == 3 && second_admitted == 0
                && phase == 2 && scope == 1 && lifetime == 0) {
                lifetime = 2;
            }
            action IgnoreTornTail when (Buggy == 4 && second_admitted == 0
                && journal_valid == 0 && full_budget == 1) {
                phase = 0;
                full_budget = 0;
            }
            action CreditNumericMemoryAndAdmit when (Buggy == 5 && second_admitted == 0
                && phase == 2 && group_member_alive == 1 && full_budget == 1
                && namespace_ready == 1 && unknown_predecessor == 0) {
                full_budget = 0;
                second_admitted = 1;
            }
            action OldReaderAdmitsBeforeFence when (Buggy == 6 && second_admitted == 0
                && full_budget == 0 && forced == 1
                && (namespace_ready == 0 || unknown_predecessor == 1)) {
                second_admitted = 1;
            }
            action CancelWithoutMatchingOutcome when (Buggy == 7 && second_admitted == 0
                && phase == 1 && (spawn_outcome <= 1 || spawn_outcome == 3)) {
                phase = 0;
                cancelled = 1;
            }
            action SpawnBeforeDurableArm when (Buggy == 8 && second_admitted == 0
                && phase == 0 && full_budget == 1 && arm_durable == 0
                && original_holder_alive == 1 && guards_held == 1 && scope == 0) {
                spawn_outcome = 1;
                group_member_alive = 1;
                group_leader_alive = 1;
                lifetime = 1;
            }

            invariant SpawnFollowsDurableArm: spawn_outcome <= 0 || spawn_outcome > 1 || arm_durable == 1;
            invariant ArmedBoundAndUnknownKeepWholeBudget: phase == 0 || full_budget == 1;
            invariant ARemainingGroupKeepsWholeBudget: group_member_alive == 0 || full_budget == 1;
            invariant DrainRequiresSameHostEvidence: lifetime <= 1
                || (scope == 0 && group_member_alive == 0) || scope == 3;
            invariant UnknownJournalKeepsWholeBudget: journal_valid == 1 || full_budget == 1;
            invariant CancellationNamesActualNoChild: cancelled == 0 || spawn_outcome == 2;
            invariant ASecondHeavyClaimHasRoom: second_admitted == 0
                || (full_budget == 0 && group_member_alive == 0);
            invariant AdmissionUsesReadyNamespaceAndDrainedPredecessors: second_admitted == 0
                || (namespace_ready == 1 && unknown_predecessor == 0);
            bound CustodyPhasesAreFinite: phase <= 3 && lifetime <= 2 && scope <= 3
                && spawn_outcome <= 3;
        }
    }
}
