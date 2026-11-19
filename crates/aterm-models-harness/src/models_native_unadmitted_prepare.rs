// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// The host answers an expired restart prepare replay `unadmitted` only for a prepare it never
/// admitted and no replay of which it can admit. One id under one prepare and its one deadline
/// (a fresh deadline is a new admission), on one host epoch: the epoch check before this answer
/// is an unmodelled premise, since a new epoch's table is not the one that admitted it. What a
/// later reader can answer from the issuer journal alone, `lapsed`, is the
/// `NativeHistoricalClosure` model.
/// `deadline` is too far (0), in its window (1) or expired (2) on the one monotonic clock. `op`
/// is the id in the reached table: absent (0), reserved (1), published (2), recorded (3), started
/// (4), completed (5) or reclaimed (6). `journal` is the durable issuer record admission writes
/// before any effect and nothing removes; a self-exec (`own`) plan writes none.
/// `asked` is plain (0), a supported typed asked Restart (1), or an unsupported
/// shape (2). Asking does not grant admission or alter the retained predecessor: a supported
/// asked prepare publishes only once qualified (`qual`), as an upgrade (1) naming the predecessor
/// this host settled (`settled`), or as a re-entry (2) into a recreated host. `Recreate` is a new
/// host process: its table binds nothing (`bound`), it settled nothing, and a registration the
/// agent saved before it is an earlier epoch's (`reg` 1; 2 is this host's own, 0 none).
/// `Unbind` is the table's binding resolving to none: an operation of the epoch (another, whose
/// publication replaced any settlement, or this one once completed) ending owing nothing and
/// leaving nothing a later asked move succeeds (an effect-free abort with nothing settled before
/// it, an abort after an effect, a retirement, a close, a commit no asked move succeeds), or a
/// settlement nothing that runs can inherit (its resulting vendor or shell ended or replaced,
/// another conversation current, the built-in relaunch continuity it was settled under broken).
/// The table binds nothing again, which a re-entry may qualify from, and settles nothing it could
/// revive. A re-entry qualifies only from this host's own registration with nothing of its epoch
/// bound, and no host revives a predecessor it did not settle (`lapse` books the first breach: 1
/// a re-entry from another epoch's registration, 2 one over a bound table, 3 a revival).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_unadmitted_prepare_model() -> Model {
    aterm_spec::ty_model! {
        NativeUnadmittedPrepare {
            const Buggy = 0;
            var deadline = 0; var op = 0; var journal = 0; var own = 0; var asked = 0;
            var answered = 0; var admitted = 0; var stale = 0; var early = 0;
            var settled = 0; var bound = 0; var reg = 0; var qual = 0; var lapse = 0;
            action Approach when (deadline == 0) { deadline = 1; }
            action Expire when (deadline == 1) { deadline = 2; }
            action OwnImage when (op == 0 && own == 0) { own = 1; }
            action Ask when (op == 0 && asked == 0 && answered == 0) { asked = 1; }
            action UnsupportedAsk when (op == 0 && asked == 0 && answered == 0) { asked = 2; }
            action Settle when (settled == 0 && op == 0 && qual == 0) { settled = 1; bound = 1; }
            action Bind when (bound == 0 && op == 0 && qual == 0) { bound = 1; }
            action Unbind when (bound == 1 && ((op == 0 && qual == 0) || op == 5 || op == 6)) {
                settled = 0; bound = 0;
            }
            action Recreate when (op == 0 && journal == 0 && answered == 0 && qual == 0
                && (settled == 1 || bound == 1 || reg == 2)) {
                settled = 0; bound = 0; reg = if reg == 2 {1} else {reg};
            }
            action Register when (reg <= 1) { reg = 2; }
            action QualifyUpgrade when (asked == 1 && qual == 0 && op <= 1
                && (settled == 1 || Buggy == 10)) {
                qual = 1;
                lapse = if lapse == 0 && settled == 0 {3} else {lapse};
            }
            action Reenter when (asked == 1 && qual == 0 && op <= 1
                && (reg == 2 || Buggy == 8) && (bound == 0 || Buggy == 9)) {
                qual = 2;
                lapse = if lapse > 0 {lapse} else if reg <= 1 {1} else if bound == 1 {2} else {0};
            }
            // A slow request may reserve after expiry; only its final check publishes.
            action Reserve when (op == 0 && deadline > 0) { op = 1; }
            action Drop when (op == 1) { op = 0; }
            action Publish when (op == 1 && (deadline == 1 || Buggy == 1) && (asked == 0 || asked == 2 || qual > 0)) {
                op = if own == 1 {3} else {2};
                bound = 1;
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
                && (own == 0 || Buggy == 6) && (asked <= 1 || Buggy == 7)) {
                answered = 1;
                stale = if op == 0 || op == 6 {stale} else {1};
                early = if deadline == 2 {early} else {1};
            }
            invariant NeverAdmitted: answered == 0 || admitted == 0;
            invariant NeverRecorded: answered == 0 || journal == 0;
            invariant NothingInMemory: stale == 0;
            invariant OnlyExpired: early == 0;
            invariant SupportedShape: answered == 0 || asked <= 1;
            invariant ReentryOwnRegistration: lapse == 0 || lapse > 1;
            invariant ReentryUnbound: lapse <= 1 || lapse == 3;
            invariant NoRevival: lapse <= 2;
            bound Finite: 0 <= deadline && deadline <= 2 && 0 <= op && op <= 6
                && 0 <= journal && journal <= 1 && 0 <= own && own <= 1
                && 0 <= asked && asked <= 2
                && 0 <= answered && answered <= 1 && 0 <= admitted && admitted <= 1
                && 0 <= stale && stale <= 1 && 0 <= early && early <= 1
                && 0 <= settled && settled <= 1 && 0 <= bound && bound <= 1
                && 0 <= reg && reg <= 2 && 0 <= qual && qual <= 2 && 0 <= lapse && lapse <= 3;
        }
    }
}
