// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// The Commons dial scheduler's cadence (`aterm_db::runtime::discovery`): when this side dials
/// one established endpoint of a peer, and how far a walk offers a table that moved here.
///
/// Time is in units of two minutes. `wait` is how many whole units remain before the endpoint's
/// own wait ends: 0 within the ordinary interval (a follow-up, a retry), 1 for a failed dial's
/// backoff (at most two minutes), `Rest` (3) for an audit's rest (six to eight minutes). `age`
/// is how many whole units ago this side's last dial of it began; `Reverify` (4) is the most an
/// audit's rest lasts. `Tick` passes one unit while the endpoint waits: a due endpoint is dialled
/// before time passes, the caps' place wait being a test's bound and not this model's.
///
/// A dial (`Dial`) is due by its own wait, or is the one probe of a silence (`silent`: a peer
/// that beaconed went unheard for 30 s, under a unit). While it runs, its peer's answer is
/// chosen: the round leaves table work named (`owes`), finds the table scan not yet round every
/// table both list or its audit not yet complete (`sweeps`), moves records (`moves`), breaks off
/// in its membership or application exchange after authenticating (`breaks`), or fails
/// (`fails`). `Finish` sets the next wait: a backoff after a failure, at once after a round that
/// moved or owes or sweeps or broke off, else a rest until the audit. (A run of broken-off
/// rounds backs off after the first few; each modelled round starts with none behind it.) A
/// round the seat dials in (`Vouch`) pulls in only an endpoint whose dial failed, once per
/// failure streak (`pulled`). A beacon (`Beacon`) ends a silence and moves no wait. `last` names
/// the last action that could move the wait (1 a beacon, 2 a vouch, 0 a tick, a dial or a
/// finish) and `before` the wait just before it, where that was a beacon or a vouch (else 0).
///
/// The walk: a table that moved here (`Move`) is offered to `Seats` seats one at a time. A round
/// over it that sent records (`WalkSends`) starts the count of cold answers again, one that found
/// the seat already held it (`WalkCold`) counts one, and one whose seat lacks the table and took
/// another (`WalkLacks`) settles the seat all the same. The walk ends at `Cold` cold answers in a
/// row or once every seat was tried. `walked` counts its dials since the move.
///
/// Laws: `BeaconGrantsNothing` (a beacon leaves the wait as it found it); `HealthyVouchInert` (a
/// round dialled in leaves a healthy endpoint's wait as it found it); `PendingNeverRests` (no
/// endpoint rests after a round that owes, sweeps or broke off); `AuditWithin` (this side dials
/// each endpoint within `Reverify` of its last dial, whatever rounds dialled in and beacons do);
/// `SilenceProbesOnce`; `WalkEnds` (with no new move, a walk dials at most `Seats` times).
///
/// Mutants: 1, a beacon makes the endpoint due; 2, a silence is probed again after each round;
/// 3, a round that owes, sweeps or broke off rests; 4, a round dialled in pulls in a healthy
/// endpoint; 5, an audit's rest is drawn past `Reverify`; 6, a seat whose round took another
/// table is not settled for the moved one.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn dial_cadence_model() -> Model {
    aterm_spec::ty_model! {
        DbDialCadence {
            const Buggy = 0;
            const Reverify = 4;
            const Rest = 3;
            const Seats = 3;
            const Cold = 2;
            var busy = 0; var wait = 0; var age = 0;
            var owes = 0; var sweeps = 0; var moves = 0; var fails = 0; var breaks = 0;
            var failing = 0; var rest = 0; var pulled = 0;
            var beaconed = 0; var silent = 0; var probed = 0; var probes = 0;
            var last = 0; var before = 0;
            var hot = 0; var tried = 0; var cold = 0; var walked = 0;
            action Tick when (busy == 0 && wait > 0 && age <= Reverify) {
                wait = wait - 1; age = age + 1; last = 0; before = 0;
            }
            action Beacon {
                beaconed = 1; silent = 0; probed = 0; probes = 0;
                wait = if Buggy == 1 {0} else {wait};
                last = 1; before = wait;
            }
            action Silence when (beaconed == 1 && silent == 0 && busy == 0) { silent = 1; }
            action Dial when (busy == 0
                && (wait == 0 || (silent == 1 && failing == 0 && (probed == 0 || Buggy == 2)))) {
                busy = 1; wait = 0; age = 0; rest = 0; last = 0; before = 0;
                owes = 0; sweeps = 0; moves = 0; fails = 0; breaks = 0;
                probes = if silent == 1 && failing == 0 && (probed == 0 || Buggy == 2) {
                    if probes == 2 {2} else {probes + 1}
                } else {probes};
                probed = if silent == 1 && failing == 0 {1} else {probed};
            }
            action Owes when (busy == 1 && owes == 0) { owes = 1; }
            action Sweeps when (busy == 1 && sweeps == 0) { sweeps = 1; }
            action Moves when (busy == 1 && moves == 0) { moves = 1; }
            action Fails when (busy == 1 && fails == 0) { fails = 1; }
            action Breaks when (busy == 1 && breaks == 0) { breaks = 1; }
            action Finish when (busy == 1) {
                busy = 0; last = 0; before = 0;
                moves = 0; fails = 0;
                failing = fails;
                pulled = if fails == 1 {pulled} else {0};
                rest = if fails == 0 && moves == 0
                    && ((owes == 0 && sweeps == 0 && breaks == 0) || Buggy == 3)
                    {1} else {0};
                wait = if fails == 1 {1}
                    else if moves == 1 {0}
                    else if Buggy == 5 && owes == 0 && sweeps == 0 && breaks == 0 {Reverify + 1}
                    else if (owes == 0 && sweeps == 0 && breaks == 0) || Buggy == 3 {Rest}
                    else {0};
                silent = if fails == 1 {silent} else {0};
            }
            action Vouch when (busy == 0) {
                wait = if failing == 1 && pulled == 0 {0}
                    else if failing == 0 && Buggy == 4 {0}
                    else {wait};
                pulled = if failing == 1 {1} else {pulled};
                last = 2; before = wait;
            }
            action Move { hot = 1; tried = 0; cold = 0; walked = 0; }
            action WalkSends when (hot == 1) {
                walked = if walked == Seats + 1 {walked} else {walked + 1};
                hot = if Seats <= tried + 1 {0} else {1};
                tried = if Seats <= tried + 1 {0} else {tried + 1};
                cold = 0;
            }
            action WalkCold when (hot == 1) {
                walked = if walked == Seats + 1 {walked} else {walked + 1};
                hot = if Seats <= tried + 1 || Cold <= cold + 1 {0} else {1};
                tried = if Seats <= tried + 1 || Cold <= cold + 1 {0} else {tried + 1};
                cold = if Seats <= tried + 1 || Cold <= cold + 1 {0} else {cold + 1};
            }
            action WalkLacks when (hot == 1) {
                walked = if walked == Seats + 1 {walked} else {walked + 1};
                hot = if Buggy == 6 {1} else if Seats <= tried + 1 {0} else {1};
                tried = if Buggy == 6 {tried} else if Seats <= tried + 1 {0} else {tried + 1};
                cold = if Buggy == 6 {cold} else if Seats <= tried + 1 {0} else {cold};
            }
            invariant BeaconGrantsNothing: last == 0 || last == 2 || before == wait;
            invariant HealthyVouchInert: last <= 1 || failing == 1 || before == wait;
            invariant PendingNeverRests: busy == 1 || rest == 0
                || (owes == 0 && sweeps == 0 && breaks == 0);
            invariant AuditWithin: age <= Reverify;
            invariant SilenceProbesOnce: probes <= 1;
            invariant WalkEnds: walked <= Seats;
        }
    }
}

#[cfg(test)]
#[path = "dial_cadence_tests.rs"]
mod tests;
