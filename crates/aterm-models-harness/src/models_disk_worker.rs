// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ask's dedicated disk worker and the owner's alert (`disk-free@v1`,
//! `docs/DESIGN-disk-free-ask-2026-10-24.md` §8.4, §8.5 and §8.7).

use aterm_spec::derive::Model;

/// THE WORKER: one volume's disk worker, claimed by one host's look under the lease's lock after
/// its step (`ask::worker_step`), started by that host's main thread, recorded at its next look,
/// and over at its task's end, its time or its host's end; and the owner's alert, which each look
/// of its host re-reads (`ask::owner_needed`). The ask's own episode is `DiskFreeAsk`'s, kept here
/// only as far as the worker and the alert read it.
/// - `pres`: the volume reads clear (0), short with no crossing projected (1), or a crossing
///   projected, short or not (2);
/// - `done`: aterm's own freeing is done;
/// - `ep`: the episode: none (0), a round (1), a line owed or out (2), out (3), settled (4);
///   `wt`: its round waits only on held sessions;
/// - `need`: the lease reads a worker due, whatever the pressure, aterm's freeing, the worker
///   cooldown and the record say (those are their own): the episode is out; its round waits only
///   on held sessions, or follows a pick that freed nothing or never took its line; it settled
///   while a crossing is projected; or none is open and the line cooldown stands. What a round
///   followed is not kept: `Wait`, `Unwait` and `Undue` move a round's;
/// - `can`: a host that can start a worker looks in time (none that can has let one stay due for
///   `ask::SPAWN_MS`);
/// - `lcool`: a line went within the line cooldown; `wcool`: a worker was claimed within the
///   worker cooldown;
/// - `wk`: the lease's worker record: none (0), claimed (1), running (2), over (3); `ws`, apart
///   from it, the worker's session: none started (0), working under Autopilot (1), stopped (2):
///   its task ended or stalled, its host ended with no successor, or its host turned its
///   Autopilot Off at `expired`. A host that ended and whose successor runs the session moves
///   the record to it, which is no step here;
/// - `nW`: the workers claimed this episode (with none open, since the last one closed);
/// - `late`: the fill reaches the emergency line before the step in hand (a line owed or out,
///   a worker claimed or running) ends;
/// - `alert`: the owner's alert stands.
///
/// `Claim` records from its own state what its guard is meant to hold (`sdue`, `sdone`,
/// `scool`), and `Raise` and `Keep` whether the look that raised or kept the alert read the
/// owner needed (`salert`); each of their mutants is the same step under a weaker guard, so a
/// guard weakened in the step itself breaks the law too. The alert is the last look's: between
/// two looks the world may change under it, and the next look keeps it only if the owner is
/// still needed, or withdraws it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn disk_free_worker_model() -> Model {
    aterm_spec::ty_model! {
        DiskFreeWorker {
            const Buggy = 0;
            var pres = 0;
            var done = 0;
            var ep = 0;
            var wt = 0;
            var need = 0;
            var can = 1;
            var lcool = 0;
            var wcool = 0;
            var wk = 0;
            var ws = 0;
            var nW = 0;
            var late = 0;
            var alert = 0;
            var sdue = 1;
            var sdone = 1;
            var scool = 0;
            var salert = 1;

            action Press when (pres == 0) { pres = 1; }
            action Cross when (pres <= 1) {
                pres = 2;
                need = if (ep == 4) { 1 } else { need };
            }
            action Uncross when (pres == 2) {
                pres = 1;
                need = if (ep == 4) { 0 } else { need };
            }
            action Relax when (pres > 0) {
                pres = 0;
                need = if (ep == 4) { 0 } else { need };
            }
            action Spend when (done == 0) { done = 1; }
            action Unspend when (done == 1) { done = 0; }
            action Able when (can == 0) { can = 1; }
            action Unable when (can == 1) { can = 0; }
            action Late when (late == 0) { late = 1; }
            action InTime when (late == 1) { late = 0; }
            action LineCools when (lcool == 1) {
                lcool = 0;
                need = if (ep == 0) { 0 } else { need };
            }
            action WorkerCools when (wcool == 1) { wcool = 0; }

            action Open when (ep == 0 && pres > 0 && done == 1 && lcool == 0) {
                ep = 1;
                wt = 0;
                need = 0;
                nW = 0;
                wk = if (wk == 3) { 0 } else { wk };
                ws = if (wk == 3) { 0 } else { ws };
            }
            action Elect when (ep == 1 && (wk == 0 || wk == 3)) {
                ep = 2;
                wt = 0;
                need = 0;
            }
            action Type when (ep == 2 && lcool == 0) { lcool = 1; }
            action HandOff when (ep == 2) { ep = 1; need = 0; }
            action HandOffMissed when (ep == 2) { ep = 1; need = 1; }
            action Wait when (ep == 1 && wt == 0) { wt = 1; need = 1; }
            action Unwait when (ep == 1 && wt == 1) { wt = 0; }
            action Undue when (ep == 1 && wt == 0 && need == 1) { need = 0; }
            action RunOut when ((ep == 1 && (wk == 0 || wk == 3)) || ep == 2) {
                ep = 3;
                wt = 0;
                need = 1;
            }
            action Settle when (ep == 2) {
                ep = 4;
                need = if (pres == 2) { 1 } else { 0 };
            }
            action End when (ep > 0 && pres == 0 && (wk == 0 || wk == 3)) {
                ep = 0;
                wt = 0;
                need = lcool;
                nW = 0;
                wk = 0;
                ws = 0;
            }
            action GiveWay when ((ep == 3 || ep == 4) && pres > 0 && done == 1 && lcool == 0) {
                ep = 1;
                wt = 0;
                need = 0;
                nW = 0;
                wk = if (wk == 3) { 0 } else { wk };
                ws = if (wk == 3) { 0 } else { ws };
            }

            action Claim when (
                pres > 0 && done == 1 && need == 1 && can == 1 && wk == 0 && nW == 0
                    && wcool == 0
            ) {
                wk = 1;
                ws = 0;
                nW = nW + 1;
                wcool = 1;
                sdue = if (need == 1 && pres > 0) { 1 } else { 0 };
                sdone = done;
                scool = wcool;
            }
            action Spawn when (wk == 1 && ws == 0) { ws = 1; }
            action Start when (wk == 1 && ws > 0) { wk = 2; }
            action Unstarted when (wk == 1 && (ws == 0 || ws == 2)) { wk = 3; }
            action Stops when (ws == 1) { ws = 2; }
            action Over when (wk == 2 && ws == 2) { wk = 3; }
            action Expire when (wk == 2 && ws == 1) { wk = 3; ws = 2; }

            action Raise when (
                alert == 0 && pres > 0 && done == 1
                    && (((ep == 2 || wk == 1 || wk == 2) && late == 1)
                        || ((ep <= 1 || ep > 2) && (wk == 0 || wk == 3)
                            && (wk > 0 || wcool == 1 || can == 0 || need == 0)
                            && (ep > 2 || (ep == 0 && lcool == 1) || (ep == 1 && wt == 1))))
            ) {
                alert = 1;
                salert = if (
                    pres > 0 && done == 1
                        && (((ep == 2 || wk == 1 || wk == 2) && late == 1)
                            || ((ep <= 1 || ep > 2) && (wk == 0 || wk == 3)
                                && (wk > 0 || wcool == 1 || can == 0 || need == 0)
                                && (ep > 2 || (ep == 0 && lcool == 1) || (ep == 1 && wt == 1))))
                ) { 1 } else { 0 };
            }
            action Keep when (
                alert == 1 && pres > 0 && done == 1
                    && (((ep == 2 || wk == 1 || wk == 2) && late == 1)
                        || ((ep <= 1 || ep > 2) && (wk == 0 || wk == 3)
                            && (wk > 0 || wcool == 1 || can == 0 || need == 0)
                            && (ep > 2 || (ep == 0 && lcool == 1) || (ep == 1 && wt == 1))))
            ) {
                salert = if (
                    pres > 0 && done == 1
                        && (((ep == 2 || wk == 1 || wk == 2) && late == 1)
                            || ((ep <= 1 || ep > 2) && (wk == 0 || wk == 3)
                                && (wk > 0 || wcool == 1 || can == 0 || need == 0)
                                && (ep > 2 || (ep == 0 && lcool == 1) || (ep == 1 && wt == 1))))
                ) { 1 } else { 0 };
            }
            action Withdraw when (
                alert == 1
                    && (pres == 0 || done == 0
                        || ((ep == 2 || wk == 1 || wk == 2) && late == 0)
                        || ((ep <= 1 || ep > 2) && (wk == 0 || wk == 3)
                            && ((wk == 0 && wcool == 0 && can == 1 && need == 1)
                                || (ep == 1 && wt == 0) || (ep == 0 && lcool == 0))))
            ) {
                alert = 0;
                salert = 1;
            }

            action ClaimTwice when (
                Buggy == 1 && pres > 0 && done == 1 && need == 1 && can == 1 && wk == 3
                    && nW == 1 && wcool == 0
            ) {
                wk = 1;
                ws = 0;
                nW = nW + 1;
                wcool = 1;
                sdue = if (need == 1 && pres > 0) { 1 } else { 0 };
                sdone = done;
                scool = wcool;
            }
            action ClaimUndue when (
                Buggy == 2 && pres > 0 && done == 1 && need == 0 && can == 1 && wk == 0
                    && nW == 0 && wcool == 0
            ) {
                wk = 1;
                ws = 0;
                nW = nW + 1;
                wcool = 1;
                sdue = if (need == 1 && pres > 0) { 1 } else { 0 };
                sdone = done;
                scool = wcool;
            }
            action ClaimBeforeDone when (
                Buggy == 3 && pres > 0 && done == 0 && need == 1 && can == 1 && wk == 0
                    && nW == 0 && wcool == 0
            ) {
                wk = 1;
                ws = 0;
                nW = nW + 1;
                wcool = 1;
                sdue = if (need == 1 && pres > 0) { 1 } else { 0 };
                sdone = done;
                scool = wcool;
            }
            action ClaimInCooldown when (
                Buggy == 4 && pres > 0 && done == 1 && need == 1 && can == 1 && wk == 0
                    && nW == 0 && wcool == 1
            ) {
                wk = 1;
                ws = 0;
                nW = nW + 1;
                wcool = 1;
                sdue = if (need == 1 && pres > 0) { 1 } else { 0 };
                sdone = done;
                scool = wcool;
            }
            action LineWhileWorker when (Buggy == 5 && ep == 1 && ws == 1) {
                ep = 2;
                wt = 0;
                need = 0;
            }
            action LineAfterOver when (Buggy == 6 && (wk == 1 || wk == 2) && ws == 1) {
                wk = 3;
            }
            action AlertWhileWorker when (
                Buggy == 7 && alert == 0 && pres > 0 && done == 1 && (wk == 1 || wk == 2)
                    && late == 0
            ) {
                alert = 1;
                salert = if (
                    pres > 0 && done == 1
                        && (((ep == 2 || wk == 1 || wk == 2) && late == 1)
                            || ((ep <= 1 || ep > 2) && (wk == 0 || wk == 3)
                                && (wk > 0 || wcool == 1 || can == 0 || need == 0)
                                && (ep > 2 || (ep == 0 && lcool == 1) || (ep == 1 && wt == 1))))
                ) { 1 } else { 0 };
            }
            action AlertStays when (
                Buggy == 8 && alert == 1
                    && (pres == 0 || done == 0
                        || ((ep == 2 || wk == 1 || wk == 2) && late == 0)
                        || ((ep <= 1 || ep > 2) && (wk == 0 || wk == 3)
                            && ((wk == 0 && wcool == 0 && can == 1 && need == 1)
                                || (ep == 1 && wt == 0) || (ep == 0 && lcool == 0))))
            ) {
                salert = if (
                    pres > 0 && done == 1
                        && (((ep == 2 || wk == 1 || wk == 2) && late == 1)
                            || ((ep <= 1 || ep > 2) && (wk == 0 || wk == 3)
                                && (wk > 0 || wcool == 1 || can == 0 || need == 0)
                                && (ep > 2 || (ep == 0 && lcool == 1) || (ep == 1 && wt == 1))))
                ) { 1 } else { 0 };
            }

            invariant OneWorker: nW <= 1;
            invariant WorkerOnlyDue: sdue == 1;
            invariant WorkerOnlyDone: sdone == 1;
            invariant NoWorkerInCooldown: scool == 0;
            invariant NoLineWhileWorker: ws == 0 || ws == 2 || ep <= 1 || ep > 2;
            invariant AlertOnlyNeeded: alert == 0 || salert == 1;
            bound PresBox: pres <= 2;
            bound EpBox: ep <= 4;
            bound WorkerBox: wk <= 3;
            bound SessionBox: ws <= 2;
            bound ClaimsBox: nW <= 2;
        }
    }
}
