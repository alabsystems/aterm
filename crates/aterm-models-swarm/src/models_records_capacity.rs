// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the membership log's record capacity: the two disjoint record budgets and the
//! reserve proof admissions leave (`crates/aterm-secrets/src/membership_log/budgets.rs`,
//! `crates/aterm-secrets/src/client/readmit.rs`), and the crossed mark and the fences that keep a
//! 0.146-0.152 build from ever holding part of a crossed log or a log past its bound: a vault
//! server's records fence (`crates/aterm-secrets/src/crossing.rs`, `server.rs`) and a Mac's
//! legacy database fence (`crates/aterm-db/src/control.rs`).

use aterm_spec::derive::*;

/// `SwarmRecordsCapacity`: one membership log's budgets (scenario `Budgets`), one vault server at
/// the legacy line (scenario `Crossing`) and one Mac at it (scenario `Mac`), each picked by its
/// first action.
///
/// `Budgets` compares the legacy held limit and production budgets divided by 1,024: `W`
/// weighed records (`Genesis`, `Admit`, `Remove`; 4,096), `K` keyed ones (every other kind;
/// 12,288) and `H` in all (4,096 for the legacy scenario, 16,384 in production), with the proof
/// reserves `WR` and `KR` at three quarters of `W` and `K`. It starts as a genesis, a `Node`
/// record and an `Admit` (`w` 2, `k` 1). `AddWeighed` is a person's `Admit`, `AddKeyed` a keyed
/// record (a `Name`, as a `Wrap` or `Put` counts), each held when its budget and the held total
/// have room; `Prove` is a Mac on the home LAN proving its node key, let in only while both
/// budgets are below their reserves and the admission's whole batch fits: its `Admit` and the
/// `PW` keyed `Wrap`s that hand the entrant each newest epoch, all signed or none (at the legacy
/// held total the start leaves room for no such batch). Each
/// action keeps what it did in `last` (1 a weighed record held, 2 one refused, 3 a keyed record
/// held, 4 one refused, 5 a proof let in, 6 one refused) and the counts it judged in `lw` and
/// `lk`.
///
/// `Crossing` is a server holding a base log three records short of `LEGACY_RECORDS` and a
/// removal batch of `R` records past it (`Remove`, an `[everyone]` `Epoch`, three `Wrap`s, the
/// last to A), so the whole log holds two more than a 0.146-0.152 build takes: counted past the
/// base, a legacy build holds `LX` of the batch and the server's held total `CX` (all `LX` until
/// the activation, then all `R`). `nr` is how much of the batch the server holds, `nx` 1 while
/// those records came from a source that crossed, `mark` 1 once its crossed mark is stored, and
/// for the last answer to a legacy build's records request (one that states no bound), `fenced` 1
/// where it was the fence's `409` and `gs` how much of the batch it carried. `CrossedPost` is a
/// crossed source's POST of the batch, of which the server takes what its held total does, the
/// mark first; `Grow` a POST of it from a source that states it has not crossed, of which the
/// server takes as much: at the legacy held total the batch's first `LX` records, cut mid-`Wrap`s,
/// with no mark, for the server then holds no more than a legacy build takes; at the activation's
/// the whole batch, past the bound, the mark first. Each `…Dies` is the same POST with the server
/// dying between its mark and its records, read again from disk. `LegacyGet` is a records GET
/// that states no bound and `LegacyPost` a POST of the batch that states none: a server that
/// crossed (its mark, or more than `LX` past the base) answers either `409`, serving no record
/// and taking none; any other serves a GET its whole log, and takes from a POST what `Grow` takes
/// where it holds none of the batch, the answer carrying no record.
///
/// `Mac` is a Mac whose records file holds the same base log, with the same variables for its
/// file and its mark. `MacPulls` is a pull of the batch from a crossed log's server (the mark,
/// then the file); `MacPullDies` the Mac dying between the two; `MacTakes` the batch from a
/// source that has not crossed (the mark first where the file passes the legacy bound);
/// `MacTakeFails` the same write where the mark cannot be stored; and `LegacyDbHello` a
/// database peer of `atdb/2` or `atdb/3`, whose exchange a crossed Mac (its mark, or more than
/// `LX` past the base) refuses before its hello, and any other carries its whole log.
///
/// The laws: the budgets are disjoint (a record is refused only by its own budget or the held
/// total: no flood of `Wrap`s blocks a `Remove`, no flood of `Admit`s a rotation), a proof is let
/// in exactly while both budgets are below their reserves and its whole batch fits, no node holds
/// a crossed log's record or a log past the legacy bound without its mark, and what a legacy
/// build is served is the node's whole log, at most `LX` past the base, and nothing a crossed
/// source taught: never a prefix of a crossed log, never a log past its bound. Until the
/// activation a batch the legacy held total itself cuts (`Grow`, `LegacyPost`, `MacTakes`) is
/// served whole as the node holds it: every legacy build holds that same cut log, and the
/// activation's room is what ends it.
///
/// Mutants: 1 serves a legacy build a crossed log capped at its bound (the first records, the
/// batch cut mid-`Wrap`s); 2 takes the records before it stores the mark; 3 counts the reserve
/// over every kind against one bound, three quarters of the held total (`HR`), as before the
/// split; 4 counts keyed kinds as weighed (the reserve then judges every record by `WR`, none
/// by `KR`); 5 judges a proof's fit by its `Admit` alone, so its `Wrap`s may not fit; 6 the
/// legacy database fence reads no durable mark, only the count the Mac holds.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn records_capacity_model() -> Model {
    aterm_spec::ty_model! {
        SwarmRecordsCapacity {
            const Buggy = 0;
            const W = 4;
            const K = 12;
            const H = 4;
            const WR = 3;
            const KR = 9;
            const HR = 3;
            const PW = 1;
            const R = 5;
            const LX = 3;
            const CX = 3;
            // The activation (S10): the held total is the two budgets together, and a node takes
            // the whole batch past the legacy bound.
            scenario H = 16, HR = 12, CX = 5;

            var sc = 0;
            var w = 0;
            var k = 0;
            var last = 0;
            var lw = 0;
            var lk = 0;
            var nr = 0;
            var nx = 0;
            var mark = 0;
            var gs = 0;
            var fenced = 0;

            action Budgets when (sc == 0) {
                sc = 1;
                w = 2;
                k = 1;
            }
            action Crossing when (sc == 0) {
                sc = 2;
            }
            action Mac when (sc == 0) {
                sc = 3;
            }

            // Mutant 4 judges every record by the weighed budget and counts keyed ones there.
            action AddWeighed when (sc == 1) {
                w = if (if Buggy == 4 { w + k } else { w }) + 1 <= W && w + k + 1 <= H {
                    w + 1
                } else {
                    w
                };
                last = if (if Buggy == 4 { w + k } else { w }) + 1 <= W && w + k + 1 <= H {
                    1
                } else {
                    2
                };
                lw = w;
                lk = k;
            }
            action AddKeyed when (sc == 1) {
                k = if (if Buggy == 4 { w + k } else { k }) + 1 <= (if Buggy == 4 { W } else { K }) &&
                    w + k + 1 <= H {
                    k + 1
                } else {
                    k
                };
                last = if (if Buggy == 4 { w + k } else { k }) + 1 <= (if Buggy == 4 { W } else { K }) &&
                    w + k + 1 <= H {
                    3
                } else {
                    4
                };
                lw = w;
                lk = k;
            }
            // Mutant 3 keeps the reserve over every kind against `HR`, not each budget apart;
            // mutant 4's counts put every record in the weighed budget; mutant 5 judges a batch
            // with no `Wrap`, and its `Wrap`s are then held only while they fit.
            action Prove when (sc == 1) {
                w = if (if Buggy == 3 || Buggy == 4 { w + k } else { w }) + 1 <=
                        (if Buggy == 3 { HR } else { WR }) &&
                    (if Buggy == 3 || Buggy == 4 { 0 } else { k }) + 1 <= KR &&
                    w + 1 <= W && k + (if Buggy == 5 { 0 } else { PW }) <= K &&
                    w + k + 1 + (if Buggy == 5 { 0 } else { PW }) <= H {
                    w + 1
                } else {
                    w
                };
                k = if (if Buggy == 3 || Buggy == 4 { w + k } else { w }) + 1 <=
                        (if Buggy == 3 { HR } else { WR }) &&
                    (if Buggy == 3 || Buggy == 4 { 0 } else { k }) + 1 <= KR &&
                    w + 1 <= W && k + (if Buggy == 5 { 0 } else { PW }) <= K &&
                    w + k + 1 + (if Buggy == 5 { 0 } else { PW }) <= H {
                    if w + k + 1 + PW <= H { k + PW } else { H - w - 1 }
                } else {
                    k
                };
                last = if (if Buggy == 3 || Buggy == 4 { w + k } else { w }) + 1 <=
                        (if Buggy == 3 { HR } else { WR }) &&
                    (if Buggy == 3 || Buggy == 4 { 0 } else { k }) + 1 <= KR &&
                    w + 1 <= W && k + (if Buggy == 5 { 0 } else { PW }) <= K &&
                    w + k + 1 + (if Buggy == 5 { 0 } else { PW }) <= H {
                    5
                } else {
                    6
                };
                lw = w;
                lk = k;
            }

            action CrossedPost when (sc == 2 && nr + 1 <= CX) {
                mark = 1;
                nr = CX;
                nx = 1;
            }
            // Mutant 2 holds the records first, so the death leaves them with no mark.
            action CrossedPostDies when (sc == 2 && mark == 0 && nr + 1 <= CX) {
                mark = if Buggy == 2 { 0 } else { 1 };
                nr = if Buggy == 2 { CX } else { nr };
                nx = if Buggy == 2 { 1 } else { nx };
            }
            action Grow when (sc == 2 && nr == 0) {
                mark = if LX + 1 <= CX { 1 } else { mark };
                nr = CX;
                nx = 0;
            }
            action GrowDies when (sc == 2 && LX + 1 <= CX && nr == 0 && mark == 0) {
                mark = if Buggy == 2 { 0 } else { 1 };
                nr = if Buggy == 2 { CX } else { nr };
            }
            // Not where a server that has not crossed holds the batch's first records: it can
            // take no more of them, as `Grow` posts none there.
            action LegacyPost when (sc == 2 && (nr == 0 || mark == 1 || LX + 1 <= nr)) {
                mark = if mark == 1 || LX + 1 <= nr {
                    mark
                } else {
                    if LX + 1 <= CX { 1 } else { mark }
                };
                nr = if mark == 1 || LX + 1 <= nr { nr } else { CX };
                fenced = if mark == 1 || LX + 1 <= nr { 1 } else { 0 };
                gs = 0;
            }
            // Mutant 1 serves a crossed log's first records, up to the legacy bound.
            action LegacyGet when (sc == 2) {
                fenced = if mark == 1 || LX + 1 <= nr {
                    if Buggy == 1 { 0 } else { 1 }
                } else {
                    0
                };
                gs = if mark == 1 || LX + 1 <= nr {
                    if Buggy == 1 {
                        if nr <= LX { nr } else { LX }
                    } else {
                        0
                    }
                } else {
                    nr
                };
            }

            action MacPulls when (sc == 3 && nr + 1 <= CX) {
                mark = 1;
                nr = CX;
                nx = 1;
            }
            // Mutant 2 writes the file first, so the death leaves it with no mark.
            action MacPullDies when (sc == 3 && mark == 0 && nr + 1 <= CX) {
                mark = if Buggy == 2 { 0 } else { 1 };
                nr = if Buggy == 2 { CX } else { nr };
                nx = if Buggy == 2 { 1 } else { nx };
            }
            action MacTakes when (sc == 3 && nr == 0) {
                mark = if LX + 1 <= CX { 1 } else { mark };
                nr = CX;
                nx = 0;
            }
            // Mutant 2 writes the file before the mark it then cannot store.
            action MacTakeFails when (sc == 3 && LX + 1 <= CX && nr == 0 && mark == 0) {
                nr = if Buggy == 2 { CX } else { nr };
            }
            // Mutant 6 reads only the count the Mac holds, not its durable mark.
            action LegacyDbHello when (sc == 3) {
                fenced = if (if Buggy == 6 { 0 } else { mark }) == 1 || LX + 1 <= nr {
                    1
                } else {
                    0
                };
                gs = if (if Buggy == 6 { 0 } else { mark }) == 1 || LX + 1 <= nr {
                    0
                } else {
                    nr
                };
            }

            invariant WeighedBound:
                w <= W && (last <= 1 || 3 <= last || W <= lw || H <= lw + lk);
            invariant KeyedBound:
                k <= K && (last <= 3 || 5 <= last || K <= lk || H <= lw + lk);
            invariant ProofReserve:
                (last <= 4 || 6 <= last ||
                    (lw + 1 <= WR && lk + 1 <= KR && lw + 1 <= W && lk + PW <= K &&
                        lw + lk + 1 + PW <= H)) &&
                (last <= 5 || WR <= lw || KR <= lk || W <= lw || K + 1 <= lk + PW ||
                    H <= lw + lk + PW);
            invariant CrossedBeforeHeld:
                (nr <= LX || mark == 1) && (nx == 0 || mark == 1);
            invariant LegacyNeverPartial: gs == 0 || (gs == nr && nr <= LX && nx == 0);
            bound RecordsCapacityBox:
                sc <= 3 && w <= W && k <= K && last <= 6 && lw <= W && lk <= K && nr <= R &&
                nx <= 1 && mark <= 1 && gs <= R && fenced <= 1;
        }
    }
}
