// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A candidate must contain the retained causal frontier in the same network.
/// This model covers the selection predicate, not its durable publication order.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn retained_frontier_model() -> Model {
    aterm_spec::ty_model! {
        VaultRetainedFrontier {
            const Buggy = 0;
            var same = 1; var complete = 1; var phase = 0; var admitted = 0;
            action OtherNetwork when (phase == 0 && same == 1) { same = 0; }
            action LoseAncestor when (phase == 0 && complete == 1) { complete = 0; }
            action Admit when (phase == 0) {
                phase = 1;
                admitted = if Buggy == 3 {0}
                    else if (same == 1 || Buggy == 1) && (complete == 1 || Buggy == 2) {1}
                    else {0};
            }
            invariant SameNetwork: admitted == 0 || same == 1;
            invariant RetainsAncestors: admitted == 0 || complete == 1;
            invariant CompleteAvailable: phase == 0 || same == 0 || complete == 0 || admitted == 1;
            bound ReadBox: same <= 1 && complete <= 1 && phase <= 1 && admitted <= 1;
        }
    }
}

/// Two causally ordered versions, independently durable data and receipt, and loss of the
/// receipt file. A shared reader never acknowledges data or repairs the journal.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn retained_commit_model() -> Model {
    aterm_spec::ty_model! {
        VaultRetainedCommit {
            const Buggy = 0;
            var raw = 0; var floor = 0; var known = 0;
            var damaged = 0; var read = 0; var seen = 2;
            action Publish when (raw == 0) { raw = 1; read = 0; seen = 2; }
            action Acknowledge when (raw == 1 && floor == 0 && damaged == 0) {
                floor = 1; known = 1; read = 0; seen = 2;
            }
            action Rollback when (raw == 1) {
                raw = 0; read = 0; seen = 2;
                floor = if Buggy == 1 {0} else {floor};
            }
            action Damage when (damaged == 0) { damaged = 1; read = 0; seen = 2; }
            action Read when (read == 0) {
                read = 1;
                seen = if Buggy == 5 {2}
                    else if Buggy == 3 {if floor > raw {2} else {floor}}
                    else if damaged == 1 {2}
                    else if Buggy == 2 || Buggy == 4 {raw}
                    else if floor > raw {2}
                    else {floor};
            }
            invariant NoForgottenAcknowledgement: known <= floor;
            invariant OnlyAcknowledgedView: seen == 2 || seen == floor;
            invariant DamagedReceiptWithholds: seen == 2 || damaged == 0;
            invariant CompleteReadAvailable: read == 0 || damaged == 1 || floor > raw || seen == floor;
            bound CommitBox: raw <= 1 && floor <= 1 && known <= 1 && damaged <= 1 && read <= 1 && seen <= 2;
        }
    }
}

/// A decoded receipt snapshot is reusable only for the exact, usable writer checkpoint.
/// Opening and validating the physical journal remain environment obligations.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn retained_replay_model() -> Model {
    aterm_spec::ty_model! {
        VaultRetainedReplay {
            const Buggy = 0;
            var directory = 1; var prefix = 1; var usable = 1; var cached = 1;
            var phase = 0; var reused = 0;
            action OtherDirectory when (phase == 0 && directory == 1) { directory = 0; }
            action Advance when (phase == 0 && prefix == 1) { prefix = 0; }
            action LoseCustody when (phase == 0 && usable == 1) { usable = 0; }
            action Evict when (phase == 0 && cached == 1) { cached = 0; }
            action Replay when (phase == 0) {
                phase = 1;
                reused = if Buggy == 5 {0}
                    else if (directory == 1 || Buggy == 1)
                        && (prefix == 1 || Buggy == 2)
                        && (usable == 1 || Buggy == 3)
                        && (cached == 1 || Buggy == 4) {1}
                    else {0};
            }
            invariant SameDirectory: reused == 0 || directory == 1;
            invariant ExactPrefix: reused == 0 || prefix == 1;
            invariant CurrentCustody: reused == 0 || usable == 1;
            invariant CachedHistory: reused == 0 || cached == 1;
            invariant MatchingReuse: phase == 0 || directory == 0 || prefix == 0
                || usable == 0 || cached == 0 || reused == 1;
            bound ReplayBox: directory <= 1 && prefix <= 1 && usable <= 1
                && cached <= 1 && phase <= 1 && reused <= 1;
        }
    }
}

/// A Mac's first enrollment of the membership records it holds, before any receipt: what a
/// writer's pass accepts as the floor every later read is held to.
///
/// `raw` is what the records this Mac holds are to the log it last applied: 0 behind it (short
/// even of the records the applied envelope was written at, as an old snapshot put back is), 1
/// that log exactly, 2 ahead of it (that log whole, and records that landed since: the store only
/// grows, and intake moves no applied digest), 3 beside it (the records the applied envelope was
/// written at, with or without others, and not a later record this Mac applied). `floor` is what
/// an enrollment acknowledged: 1 a log that holds the applied one, 2 one that lacks part of it.
///
/// The laws: `FloorHoldsApplied` (an enrollment never acknowledges records that lack part of
/// the applied log, so no read after it rolls a value back), `AHeldLogIsNeverRefused` (a writer
/// never refuses records that hold the applied log for holding more), and the liveness
/// `AHeldLogEnrolls` ([`first_enrollment_liveness`]).
///
/// The mutants, each a dead action:
///
/// - `EnrollByStamp`: the stamp's records alone are taken for the applied log (`raw == 3`):
///   breaks `FloorHoldsApplied`;
/// - `EnrollAny`: any verified store is taken (`raw == 0`): breaks `FloorHoldsApplied`;
/// - `RefuseGrown`: the writer demands the applied digest itself, as it did before 2026-11-15:
///   breaks `AHeldLogIsNeverRefused`, and `AHeldLogEnrolls`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn first_enrollment_model() -> Model {
    aterm_spec::ty_model! {
        VaultFirstEnrollment {
            const Buggy = 0;
            var raw = 1; var enrolled = 0; var floor = 0; var refused = 0;
            action Intake when (raw == 1) { raw = 2; }
            action Rollback when (raw == 1 || raw == 2) { raw = 0; }
            action Truncate when (raw == 1 || raw == 2) { raw = 3; }
            action Restore when (raw == 0 || raw == 3) { raw = 2; }
            action Enroll when (enrolled == 0 && (raw == 1 || (raw == 2 && refused == 0))) {
                enrolled = 1; floor = 1;
            }
            action EnrollByStamp when (Buggy == 1 && enrolled == 0 && raw == 3) {
                enrolled = 1; floor = 2;
            }
            action EnrollAny when (Buggy == 1 && enrolled == 0 && raw == 0) {
                enrolled = 1; floor = 2;
            }
            action RefuseGrown when (Buggy == 1 && enrolled == 0 && raw == 2 && refused == 0) {
                refused = 1;
            }
            invariant FloorHoldsApplied: enrolled == 0 || floor == 1;
            invariant AHeldLogIsNeverRefused: refused == 0;
            bound EnrollmentBox: raw <= 3 && enrolled <= 1 && floor <= 2 && refused <= 1;
        }
    }
}

/// `AHeldLogEnrolls` over [`first_enrollment_model`]: while the records this Mac holds hold the
/// log it applied, it does not stay unenrolled, assuming only that writer passes keep running
/// (every subscriber pass and every `aterm secrets sync` takes the state exclusively). A writer
/// that demands the applied digest itself (`RefuseGrown`) leaves a store one record ahead
/// unenrolled for good.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(first_enrollment_model)]
pub fn first_enrollment_liveness() -> aterm_spec::derive::Liveness {
    use aterm_spec::derive::{eq, int, or_, var};
    aterm_spec::derive::Liveness {
        name: "AHeldLogEnrolls",
        goal: or_(
            eq(var("enrolled"), int(1)),
            or_(eq(var("raw"), int(0)), eq(var("raw"), int(3))),
        ),
        weak: vec!["Enroll"],
        strong: vec![],
        mutants: vec!["RefuseGrown"],
    }
}
