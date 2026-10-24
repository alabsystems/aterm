// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The formal model `AosWorkerLease` of Aos M5b's worker-lease ledger
//! (`docs/DESIGN-aos-hosted-execution-2026-10-21.md` §11.7), over the rules of §11.5 as
//! `aterm-effects`' `aos_momentum::lease::Ledger` implements them; the ledger wins where the design
//! text differs.

use aterm_spec::derive::*;

/// One job and two workers, A honest and B possibly lying, after both said hello and the search
/// opened (design §11.7, "Formal model `AosWorkerLease`").
///
/// Variables: `job` is 0 `Pending`, 1 `Leased`, 2 `Claimed`, 3 `Cancelled`, 4 `Exhausted`.
/// `attempts` counts the leases issued; `holder` is the worker (1 A, 2 B) of the live lease or the
/// accepted claim. `wa`/`wb` are the workers' states: 0 `Idle`, 1 `Busy`, 2 `Draining { Expired }`,
/// 3 `Draining { Cancelled }`, then `Gone` as 4 `Eof`, 5 `DrainTimeout`, 6 `Retired`, 7 `Refused`.
/// `ea`/`eb` are the epochs (attempts) of the lease each holds or drains, `ra`/`rb` the epochs of
/// each worker's result in flight (0: none), and `lb` whether B's differs from the truth (A never
/// lies). `acc` is the accepted claim and `late` a late claim awaiting the job's claim (0 none, 1
/// the truth, 2 the lie). `resent` records the claim holder's one resend. `out` is 0 open, 1
/// complete, 2 void. Ghosts: `twice` (a second accept), `stale` (an accept on an expired lease),
/// `post` (an accept after a cancel) and `ignored` (a claim differing from the accepted one arrived
/// and `out` is not void).
///
/// Actions: worker sends `SendX` (the result of the lease it holds or drains), `LieB` and `ResendX`
/// (a copy of its accepted claim; B's differing resend is a `LieB`). Arrivals, each compared in the
/// same step: `AcceptX` (the holder, the current epoch), `CompareX` (a resend), `LateX` (a result
/// on an expired lease, which retires its sender) and `DropX` (a result that crossed a cancel).
/// The coordinator's: `LeaseX`, `ExpireX` (a job back to `Pending`, or `Exhausted` at the limit,
/// which voids), `DrainTimeoutX` (drains end in the order they began), `Cancel` (voids, and
/// cancels the job and its holder in the same step, as `Ledger::void` does) and `Finish`
/// (`Ledger::finish`: the job claimed and no worker draining). `LoseX` is a worker's EOF, read
/// after its frames; `AckX` its `Cancelled`. A loss or expiry that leaves an open job with no
/// `Idle` or `Busy` worker voids (`NoWorkers`), as the ledger's settle does; a voiding resend kills
/// its sender (`Refused`).
///
/// Dials, each of which alone breaks its own law: 1 (the action `ExpireKeepsLease`) the holder
/// drains and the job stays leased, breaking `LeasedByBusy` and [`aos_worker_lease_liveness`]. 2:
/// an equal resend is accepted again. 3: a late result for a `Pending` job is accepted as its claim
/// (the fence checks the job, not the worker and epoch). 4: `Cancel` leaves the holder holding. 5:
/// a differing resend or late result is dropped, not compared. 6: expiry at the limit re-leases to
/// the expiring holder in the same step, capped at `MaxAttempts + 1`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn aos_worker_lease_model() -> Model {
    aterm_spec::ty_model! {
        AosWorkerLease {
            const Buggy = 0;
            const MaxAttempts = 2;
            scenario MaxAttempts = 1;
            var job = 0;
            var attempts = 0;
            var holder = 0;
            var wa = 0;
            var wb = 0;
            var ea = 0;
            var eb = 0;
            var ra = 0;
            var rb = 0;
            var lb = 0;
            var acc = 0;
            var late = 0;
            var resent = 0;
            var out = 0;
            var twice = 0;
            var stale = 0;
            var post = 0;
            var ignored = 0;

            action LeaseA when (out == 0 && job == 0 && wa == 0) {
                job = 1;
                holder = 1;
                attempts = attempts + 1;
                wa = 1;
                ea = attempts + 1;
            }
            action LeaseB when (out == 0 && job == 0 && wb == 0) {
                job = 1;
                holder = 2;
                attempts = attempts + 1;
                wb = 1;
                eb = attempts + 1;
            }
            action SendA when ((wa == 1 || wa == 2) && ra == 0) {
                ra = ea;
            }
            action SendB when ((wb == 1 || wb == 2) && rb == 0) {
                rb = eb;
                lb = 0;
            }
            action LieB when (rb == 0
                && (wb == 1 || wb == 2 || (wb == 0 && job == 2 && holder == 2 && resent == 0)))
            {
                rb = if wb == 0 { attempts } else { eb };
                lb = 1;
                resent = if wb == 0 { 1 } else { resent };
            }
            action ResendA when (wa == 0 && job == 2 && holder == 1 && resent == 0 && ra == 0) {
                ra = attempts;
                resent = 1;
            }
            action ResendB when (wb == 0 && job == 2 && holder == 2 && resent == 0 && rb == 0) {
                rb = attempts;
                lb = 0;
                resent = 1;
            }
            action AcceptA when (wa == 1 && ra == ea && ra > 0 && job == 1 && holder == 1) {
                job = 2;
                acc = 1;
                wa = 0;
                ea = 0;
                ra = 0;
                late = 0;
                out = if late == 2 { if Buggy == 5 { out } else { 2 } } else { out };
                ignored = if late == 2 && Buggy == 5 && out <= 1 { 1 } else { ignored };
                post = if out == 2 { 1 } else { post };
            }
            action AcceptB when (wb == 1 && rb == eb && rb > 0 && job == 1 && holder == 2) {
                job = 2;
                acc = lb + 1;
                wb = 0;
                eb = 0;
                rb = 0;
                lb = 0;
                late = 0;
                out = if (late == 1 && lb == 1) || (late == 2 && lb == 0) {
                    if Buggy == 5 { out } else { 2 }
                } else {
                    out
                };
                ignored = if ((late == 1 && lb == 1) || (late == 2 && lb == 0))
                    && Buggy == 5 && out <= 1
                {
                    1
                } else {
                    ignored
                };
                post = if out == 2 { 1 } else { post };
            }
            action CompareA when (wa == 0 && ra > 0) {
                ra = 0;
                twice = if Buggy == 2 { 1 } else { twice };
            }
            action CompareB when (wb == 0 && rb > 0) {
                rb = 0;
                lb = 0;
                wb = if (acc == 1 && lb == 1) || (acc == 2 && lb == 0) {
                    if Buggy == 5 { wb } else { 7 }
                } else {
                    wb
                };
                out = if (acc == 1 && lb == 1) || (acc == 2 && lb == 0) {
                    if Buggy == 5 { out } else { 2 }
                } else {
                    out
                };
                twice = if (acc == 1 && lb == 0) || (acc == 2 && lb == 1) {
                    if Buggy == 2 { 1 } else { twice }
                } else {
                    twice
                };
                ignored = if ((acc == 1 && lb == 1) || (acc == 2 && lb == 0))
                    && Buggy == 5 && out <= 1
                {
                    1
                } else {
                    ignored
                };
            }
            action LateA when (wa == 2 && ra > 0) {
                wa = 6;
                ea = 0;
                ra = 0;
                late = if job <= 1 { if Buggy == 3 && job == 0 { late } else { 1 } } else { late };
                job = if Buggy == 3 && job == 0 { 2 } else { job };
                holder = if Buggy == 3 && job == 0 { 1 } else { holder };
                acc = if Buggy == 3 && job == 0 { 1 } else { acc };
                stale = if Buggy == 3 && job == 0 { 1 } else { stale };
                out = if job == 2 && acc == 2 { if Buggy == 5 { out } else { 2 } } else { out };
                ignored = if job == 2 && acc == 2 && Buggy == 5 && out <= 1 { 1 } else { ignored };
            }
            action LateB when (wb == 2 && rb > 0) {
                wb = 6;
                eb = 0;
                rb = 0;
                lb = 0;
                late = if job <= 1 {
                    if Buggy == 3 && job == 0 { late } else { lb + 1 }
                } else {
                    late
                };
                job = if Buggy == 3 && job == 0 { 2 } else { job };
                holder = if Buggy == 3 && job == 0 { 2 } else { holder };
                acc = if Buggy == 3 && job == 0 { lb + 1 } else { acc };
                stale = if Buggy == 3 && job == 0 { 1 } else { stale };
                out = if job == 2 && ((acc == 1 && lb == 1) || (acc == 2 && lb == 0)) {
                    if Buggy == 5 { out } else { 2 }
                } else {
                    out
                };
                ignored = if job == 2 && ((acc == 1 && lb == 1) || (acc == 2 && lb == 0))
                    && Buggy == 5 && out <= 1
                {
                    1
                } else {
                    ignored
                };
            }
            action DropA when (wa == 3 && ra > 0) {
                ra = 0;
            }
            action DropB when (wb == 3 && rb > 0) {
                rb = 0;
                lb = 0;
            }
            action ExpireA when (wa == 1) {
                wa = if Buggy == 6 && attempts == MaxAttempts { 1 } else { 2 };
                ea = if Buggy == 6 && attempts == MaxAttempts { attempts + 1 } else { ea };
                attempts = if Buggy == 6 && attempts == MaxAttempts { attempts + 1 } else { attempts };
                job = if Buggy == 6 && attempts == MaxAttempts {
                    1
                } else if MaxAttempts <= attempts {
                    4
                } else if out == 0 && wb > 1 {
                    3
                } else {
                    0
                };
                holder = if Buggy == 6 && attempts == MaxAttempts { 1 } else { 0 };
                out = if Buggy == 6 && attempts == MaxAttempts {
                    out
                } else if out == 0 && (MaxAttempts <= attempts || wb > 1) {
                    2
                } else {
                    out
                };
            }
            action ExpireB when (wb == 1) {
                wb = if Buggy == 6 && attempts == MaxAttempts { 1 } else { 2 };
                eb = if Buggy == 6 && attempts == MaxAttempts { attempts + 1 } else { eb };
                attempts = if Buggy == 6 && attempts == MaxAttempts { attempts + 1 } else { attempts };
                job = if Buggy == 6 && attempts == MaxAttempts {
                    1
                } else if MaxAttempts <= attempts {
                    4
                } else if out == 0 && wa > 1 {
                    3
                } else {
                    0
                };
                holder = if Buggy == 6 && attempts == MaxAttempts { 2 } else { 0 };
                out = if Buggy == 6 && attempts == MaxAttempts {
                    out
                } else if out == 0 && (MaxAttempts <= attempts || wa > 1) {
                    2
                } else {
                    out
                };
            }
            action ExpireKeepsLease when (Buggy == 1 && (wa == 1 || wb == 1)) {
                wa = if wa == 1 { 2 } else { wa };
                wb = if wb == 1 { 2 } else { wb };
            }
            action LoseA when (wa <= 3 && ra == 0) {
                wa = 4;
                ea = 0;
                job = if wa == 1 {
                    if MaxAttempts <= attempts { 4 } else if out == 0 && wb > 1 { 3 } else { 0 }
                } else if job <= 1 && out == 0 && wb > 1 {
                    3
                } else {
                    job
                };
                holder = if wa == 1 || (job <= 1 && out == 0 && wb > 1) { 0 } else { holder };
                out = if out == 0
                    && ((wa == 1 && MaxAttempts <= attempts) || ((wa == 1 || job <= 1) && wb > 1))
                {
                    2
                } else {
                    out
                };
            }
            action LoseB when (wb <= 3 && rb == 0) {
                wb = 4;
                eb = 0;
                job = if wb == 1 {
                    if MaxAttempts <= attempts { 4 } else if out == 0 && wa > 1 { 3 } else { 0 }
                } else if job <= 1 && out == 0 && wa > 1 {
                    3
                } else {
                    job
                };
                holder = if wb == 1 || (job <= 1 && out == 0 && wa > 1) { 0 } else { holder };
                out = if out == 0
                    && ((wb == 1 && MaxAttempts <= attempts) || ((wb == 1 || job <= 1) && wa > 1))
                {
                    2
                } else {
                    out
                };
            }
            action DrainTimeoutA when ((wa == 2 || wa == 3) && (wb <= 1 || wb > 3 || ea <= eb)) {
                wa = 5;
                ea = 0;
                ra = 0;
            }
            action DrainTimeoutB when ((wb == 2 || wb == 3) && (wa <= 1 || wa > 3 || eb <= ea)) {
                wb = 5;
                eb = 0;
                rb = 0;
                lb = 0;
            }
            action AckA when ((wa == 2 || wa == 3) && ra == 0) {
                wa = 6;
                ea = 0;
            }
            action AckB when ((wb == 2 || wb == 3) && rb == 0) {
                wb = 6;
                eb = 0;
            }
            action Cancel when (out == 0) {
                out = 2;
                job = if job == 0 { 3 } else if job == 1 { if Buggy == 4 { 1 } else { 3 } } else { job };
                holder = if job == 1 { if Buggy == 4 { holder } else { 0 } } else { holder };
                wa = if wa == 1 { if Buggy == 4 { 1 } else { 3 } } else { wa };
                wb = if wb == 1 { if Buggy == 4 { 1 } else { 3 } } else { wb };
            }
            action Finish when (out == 0 && job == 2 && (wa <= 1 || wa > 3) && (wb <= 1 || wb > 3)) {
                out = 1;
            }

            invariant LeasedByBusy: (job == 1 && holder == 1 && wa == 1 && (wb == 0 || wb > 1))
                || (job == 1 && holder == 2 && wb == 1 && (wa == 0 || wa > 1))
                || ((job == 0 || job > 1) && (wa == 0 || wa > 1) && (wb == 0 || wb > 1));
            invariant NoDuplicateAccept: twice == 0;
            invariant NoStaleAccept: stale == 0;
            invariant NoAcceptAfterCancel: post == 0;
            invariant NoContradictionIgnored: ignored == 0;
            invariant AttemptsBounded: attempts <= MaxAttempts;
            bound AttemptsBox: attempts <= MaxAttempts + 1;
        }
    }
}

/// `Settles`: every fair run of [`aos_worker_lease_model`] ends complete or void,
/// `[]<>(out == 1 || out == 2)`. Its fairness is the coordinator's transitions alone (a lease, an
/// expiry, a drain's grace, the finish), the minimal set `verify::audit_liveness` accepts: it rests
/// on no send of B's, no acknowledgement and no late result. Dial 1 (`ExpireKeepsLease`) breaks it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[aterm_spec::liveness(aos_worker_lease_model)]
pub fn aos_worker_lease_liveness() -> Liveness {
    Liveness {
        name: "Settles",
        goal: or_(eq(var("out"), int(1)), eq(var("out"), int(2))),
        weak: vec![
            "LeaseA",
            "LeaseB",
            "ExpireA",
            "ExpireB",
            "DrainTimeoutA",
            "DrainTimeoutB",
            "Finish",
        ],
        strong: vec![],
        mutants: vec!["ExpireKeepsLease"],
    }
}
