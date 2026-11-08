// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// The host answers an expired restart prepare replay `unadmitted` only for a prepare it never
/// admitted and no replay of which it can admit. One id under one prepare and its one deadline
/// (a fresh deadline is a new admission), on one host epoch: the epoch check before this answer
/// is an unmodelled premise, since a new epoch's clock and a second live process share no
/// deadline.
/// `deadline` is too far (0), in its window (1) or expired (2) on the one monotonic clock. `op`
/// is the id in the reached table: absent (0), reserved (1), published (2), recorded (3), started
/// (4), completed (5) or reclaimed (6). `journal` is the durable issuer record admission writes
/// before any effect and nothing removes; a self-exec (`own`) plan writes none.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_unadmitted_prepare_model() -> Model {
    aterm_spec::ty_model! {
        NativeUnadmittedPrepare {
            const Buggy = 0;
            var deadline = 0; var op = 0; var journal = 0; var own = 0;
            var answered = 0; var admitted = 0; var stale = 0; var early = 0;
            action Approach when (deadline == 0) { deadline = 1; }
            action Expire when (deadline == 1) { deadline = 2; }
            action OwnImage when (op == 0 && own == 0) { own = 1; }
            // A slow request may reserve after expiry; only its final check publishes.
            action Reserve when (op == 0 && deadline > 0) { op = 1; }
            action Drop when (op == 1) { op = 0; }
            action Publish when (op == 1 && (deadline == 1 || Buggy == 1)) {
                op = if own == 1 {3} else {2};
            }
            // Storage recovery is permitted after expiry; admission is not.
            action Record when (op == 2) { op = 3; journal = 1; }
            action LostAck when (op == 2 && journal == 0) { journal = 1; }
            action Start when (op == 3 && deadline == 1) { op = 4; admitted = 1; }
            action Complete when (op == 3 || op == 4) { op = 5; }
            action Reclaim when (op == 5 && deadline == 2) { op = 6; }
            action Answer when ((deadline == 2 || (Buggy == 2 && deadline == 1)
                    || (Buggy == 3 && deadline == 0))
                && (op == 0 || op == 6 || (Buggy == 4 && op > 1 && op <= 5))
                && (journal == 0 || Buggy == 5)
                && (own == 0 || Buggy == 6)) {
                answered = 1;
                stale = if op == 0 || op == 6 {stale} else {1};
                early = if deadline == 2 {early} else {1};
            }
            invariant NeverAdmitted: answered == 0 || admitted == 0;
            invariant NeverRecorded: answered == 0 || journal == 0;
            invariant NothingInMemory: stale == 0;
            invariant OnlyExpired: early == 0;
            bound Finite: 0 <= deadline && deadline <= 2 && 0 <= op && op <= 6
                && 0 <= journal && journal <= 1 && 0 <= own && own <= 1
                && 0 <= answered && answered <= 1 && 0 <= admitted && admitted <= 1
                && 0 <= stale && stale <= 1 && 0 <= early && early <= 1;
        }
    }
}
