// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the membership log's evaluator: set-union merges, `Remove` against `Admit`,
//! mutual removes, cutoffs, the quorum `Remove`, and labels
//! (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.2, F11).

use aterm_spec::derive::*;

/// `Membership`: two islands of one network, each holding some of a scenario's three records
/// (`r1` signed on island 1, `r2` and `r3` on island 2) and evaluating what it holds.
///
/// A scenario action picks the records and the genesis; `Sign<k>` puts record `k` on its home
/// island, `Deliver<k>` copies it to the other one (a rejoin is these copies), and `Evaluate<i>`
/// is island `i`'s evaluator. Island `i` holds `r1..r3` when `a1..a3` (island 1) or `b1..b3`
/// (island 2) are 1, and its evaluation is `u`, `v`, `w`, `x` with the suffix `1` or `2`.
///
/// | `sc` | Genesis | `r1` (island 1) | `r2` (island 2) | `r3` (island 2) | `u`, `v`, `w`, `x` |
/// |---|---|---|---|---|---|
/// | 1 | B, C, D | B admits X as m5 | C admits Y as m5 | later, D admits Z as m5 | labels of X, Y, Z |
/// | 2 | B, C, D | C admits X | B admits X, then D removes it | | X in |
/// | 3 | A, B, C | A removes B | B removes A | | A in, B in |
/// | 4 | A, B | A removes B | B admits Z, unaware | | B in, Z in |
/// | 5 | A, B, C, D | A removes B, C and D | B's quorum remove of A | C's quorum remove of A | A, B, C, D in |
/// | 6 | B, S | B removes S | S's lone quorum remove of B | | B in, S in |
///
/// A label is 0 for none, 1 for a bare m5 and 2 for m5 with the Mac's own suffix; the mutants
/// add 3 (m6) and 4 (m7).
///
/// `c<who><i>` counts the times island `i`'s label for X, Y or Z changed from one label to
/// another. `d1` and `d2` mark an island that took a record and has not evaluated since.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn membership_model() -> Model {
    aterm_spec::ty_model! {
        Membership {
            const Buggy = 0;

            var sc = 0;
            var a1 = 0; var a2 = 0; var a3 = 0;
            var b1 = 0; var b2 = 0; var b3 = 0;
            var d1 = 0; var d2 = 0;
            var u1 = 0; var v1 = 0; var w1 = 0; var x1 = 0;
            var u2 = 0; var v2 = 0; var w2 = 0; var x2 = 0;
            var cu1 = 0; var cv1 = 0; var cw1 = 0;
            var cu2 = 0; var cv2 = 0; var cw2 = 0;

            action Labels when (sc == 0) {
                sc = 1;
            }
            action RemoveVsAdmit when (sc == 0) {
                sc = 2;
            }
            action Mutual when (sc == 0) {
                sc = 3;
                u1 = 1; v1 = 1; u2 = 1; v2 = 1;
            }
            action Cutoff when (sc == 0) {
                sc = 4;
                u1 = 1; u2 = 1;
            }
            action MassRemove when (sc == 0) {
                sc = 5;
                u1 = 1; v1 = 1; w1 = 1; x1 = 1;
                u2 = 1; v2 = 1; w2 = 1; x2 = 1;
            }
            action LoneShare when (sc == 0) {
                sc = 6;
                u1 = 1; v1 = 1; u2 = 1; v2 = 1;
            }

            action Sign1 when (sc > 0 && d1 == 0 && d2 == 0 && a1 == 0) {
                a1 = 1; d1 = 1;
            }
            // The quorum shares (sc 5, 6) follow the removal they answer.
            action Sign2 when (sc > 0 && d1 == 0 && d2 == 0 && b2 == 0 &&
                (sc <= 4 || b1 == 1)) {
                b2 = 1; d2 = 1;
            }
            // Z's admission comes after X and Y are settled everywhere; C's share follows the
            // removal.
            action Sign3 when (d1 == 0 && d2 == 0 && b3 == 0 &&
                ((sc == 1 && a1 == 1 && a2 == 1 && b1 == 1 && b2 == 1) || (sc == 5 && b1 == 1))) {
                b3 = 1; d2 = 1;
            }
            action Deliver1 when (d1 == 0 && d2 == 0 && a1 == 1 && b1 == 0) {
                b1 = 1; d2 = 1;
            }
            action Deliver2 when (d1 == 0 && d2 == 0 && b2 == 1 && a2 == 0) {
                a2 = 1; d1 = 1;
            }
            action Deliver3 when (d1 == 0 && d2 == 0 && b3 == 1 && a3 == 0) {
                a3 = 1; d1 = 1;
            }

            action Evaluate1 when (d1 == 1) {
                d1 = 0;
                u1 = if sc == 1 {
                    if Buggy == 1 {
                        if u1 > 0 { u1 } else { if a1 == 0 { 0 } else { if v1 + w1 > 0 { 2 } else { 1 } } }
                    } else { if Buggy == 2 {
                        if a1 == 0 { 0 } else { if a3 == 1 { 3 } else { 1 } }
                    } else {
                        if a1 == 0 { 0 } else { if a2 + a3 > 0 { 2 } else { 1 } }
                    } }
                } else { if sc == 2 {
                    if Buggy == 3 { a1 } else { if a1 == 1 && a2 == 0 { 1 } else { 0 } }
                } else { if sc == 3 {
                    if Buggy == 4 { if a2 == 1 && a1 == 0 { 0 } else { 1 } } else { 1 - a2 }
                } else { if sc == 4 {
                    1 - a1
                } else { if sc == 5 {
                    if Buggy == 6 || a2 + a3 <= 1 { 1 } else { 0 }
                } else {
                    if Buggy == 7 { 1 - a2 } else { 1 }
                } } } } };
                v1 = if sc == 1 {
                    if Buggy == 1 {
                        if v1 > 0 { v1 } else { if a2 == 0 { 0 } else { if u1 + w1 > 0 { 2 } else { 1 } } }
                    } else { if Buggy == 2 {
                        if a2 == 0 { 0 } else { if a1 + a3 == 2 { 4 } else { if a1 + a3 == 1 { 3 } else { 1 } } }
                    } else {
                        if a2 == 0 { 0 } else { if a1 + a3 > 0 { 2 } else { 1 } }
                    } }
                } else { if sc == 2 {
                    0
                } else { if sc == 3 {
                    1 - a1
                } else { if sc == 4 {
                    if Buggy == 5 { a2 } else { if a2 == 1 && a1 == 0 { 1 } else { 0 } }
                } else { if sc == 5 {
                    if Buggy == 6 || a2 + a3 <= 1 { 1 - a1 } else { 1 }
                } else {
                    if Buggy == 7 && a2 == 1 { 1 } else { 1 - a1 }
                } } } } };
                w1 = if sc == 1 {
                    if Buggy == 1 {
                        if w1 > 0 { w1 } else { if a3 == 0 { 0 } else { if u1 + v1 > 0 { 2 } else { 1 } } }
                    } else { if Buggy == 2 {
                        if a3 == 0 { 0 } else { 1 }
                    } else {
                        if a3 == 0 { 0 } else { if a1 + a2 > 0 { 2 } else { 1 } }
                    } }
                } else { if sc == 5 {
                    if Buggy == 6 || a2 + a3 <= 1 { 1 - a1 } else { 1 }
                } else {
                    0
                } };
                x1 = if sc == 5 {
                    if Buggy == 6 || a2 + a3 <= 1 { 1 - a1 } else { 1 }
                } else {
                    0
                };
                cu1 = cu1 + (if u1 > 0 && sc == 1 { if (if Buggy == 1 {
                        if u1 > 0 { u1 } else { if a1 == 0 { 0 } else { if v1 + w1 > 0 { 2 } else { 1 } } }
                    } else { if Buggy == 2 {
                        if a1 == 0 { 0 } else { if a3 == 1 { 3 } else { 1 } }
                    } else {
                        if a1 == 0 { 0 } else { if a2 + a3 > 0 { 2 } else { 1 } }
                    } }) == u1 { 0 } else { 1 } } else { 0 });
                cv1 = cv1 + (if v1 > 0 && sc == 1 { if (if Buggy == 1 {
                        if v1 > 0 { v1 } else { if a2 == 0 { 0 } else { if u1 + w1 > 0 { 2 } else { 1 } } }
                    } else { if Buggy == 2 {
                        if a2 == 0 { 0 } else { if a1 + a3 == 2 { 4 } else { if a1 + a3 == 1 { 3 } else { 1 } } }
                    } else {
                        if a2 == 0 { 0 } else { if a1 + a3 > 0 { 2 } else { 1 } }
                    } }) == v1 { 0 } else { 1 } } else { 0 });
                cw1 = cw1 + (if w1 > 0 && sc == 1 { if (if Buggy == 1 {
                        if w1 > 0 { w1 } else { if a3 == 0 { 0 } else { if u1 + v1 > 0 { 2 } else { 1 } } }
                    } else { if Buggy == 2 {
                        if a3 == 0 { 0 } else { 1 }
                    } else {
                        if a3 == 0 { 0 } else { if a1 + a2 > 0 { 2 } else { 1 } }
                    } }) == w1 { 0 } else { 1 } } else { 0 });
            }

            action Evaluate2 when (d2 == 1) {
                d2 = 0;
                u2 = if sc == 1 {
                    if Buggy == 1 {
                        if u2 > 0 { u2 } else { if b1 == 0 { 0 } else { if v2 + w2 > 0 { 2 } else { 1 } } }
                    } else { if Buggy == 2 {
                        if b1 == 0 { 0 } else { if b3 == 1 { 3 } else { 1 } }
                    } else {
                        if b1 == 0 { 0 } else { if b2 + b3 > 0 { 2 } else { 1 } }
                    } }
                } else { if sc == 2 {
                    if Buggy == 3 { b1 } else { if b1 == 1 && b2 == 0 { 1 } else { 0 } }
                } else { if sc == 3 {
                    if Buggy == 4 { if b2 == 1 && b1 == 0 { 0 } else { 1 } } else { 1 - b2 }
                } else { if sc == 4 {
                    1 - b1
                } else { if sc == 5 {
                    if Buggy == 6 || b2 + b3 <= 1 { 1 } else { 0 }
                } else {
                    if Buggy == 7 { 1 - b2 } else { 1 }
                } } } } };
                v2 = if sc == 1 {
                    if Buggy == 1 {
                        if v2 > 0 { v2 } else { if b2 == 0 { 0 } else { if u2 + w2 > 0 { 2 } else { 1 } } }
                    } else { if Buggy == 2 {
                        if b2 == 0 { 0 } else { if b1 + b3 == 2 { 4 } else { if b1 + b3 == 1 { 3 } else { 1 } } }
                    } else {
                        if b2 == 0 { 0 } else { if b1 + b3 > 0 { 2 } else { 1 } }
                    } }
                } else { if sc == 2 {
                    0
                } else { if sc == 3 {
                    1 - b1
                } else { if sc == 4 {
                    if Buggy == 5 { b2 } else { if b2 == 1 && b1 == 0 { 1 } else { 0 } }
                } else { if sc == 5 {
                    if Buggy == 6 || b2 + b3 <= 1 { 1 - b1 } else { 1 }
                } else {
                    if Buggy == 7 && b2 == 1 { 1 } else { 1 - b1 }
                } } } } };
                w2 = if sc == 1 {
                    if Buggy == 1 {
                        if w2 > 0 { w2 } else { if b3 == 0 { 0 } else { if u2 + v2 > 0 { 2 } else { 1 } } }
                    } else { if Buggy == 2 {
                        if b3 == 0 { 0 } else { 1 }
                    } else {
                        if b3 == 0 { 0 } else { if b1 + b2 > 0 { 2 } else { 1 } }
                    } }
                } else { if sc == 5 {
                    if Buggy == 6 || b2 + b3 <= 1 { 1 - b1 } else { 1 }
                } else {
                    0
                } };
                x2 = if sc == 5 {
                    if Buggy == 6 || b2 + b3 <= 1 { 1 - b1 } else { 1 }
                } else {
                    0
                };
                cu2 = cu2 + (if u2 > 0 && sc == 1 { if (if Buggy == 1 {
                        if u2 > 0 { u2 } else { if b1 == 0 { 0 } else { if v2 + w2 > 0 { 2 } else { 1 } } }
                    } else { if Buggy == 2 {
                        if b1 == 0 { 0 } else { if b3 == 1 { 3 } else { 1 } }
                    } else {
                        if b1 == 0 { 0 } else { if b2 + b3 > 0 { 2 } else { 1 } }
                    } }) == u2 { 0 } else { 1 } } else { 0 });
                cv2 = cv2 + (if v2 > 0 && sc == 1 { if (if Buggy == 1 {
                        if v2 > 0 { v2 } else { if b2 == 0 { 0 } else { if u2 + w2 > 0 { 2 } else { 1 } } }
                    } else { if Buggy == 2 {
                        if b2 == 0 { 0 } else { if b1 + b3 == 2 { 4 } else { if b1 + b3 == 1 { 3 } else { 1 } } }
                    } else {
                        if b2 == 0 { 0 } else { if b1 + b3 > 0 { 2 } else { 1 } }
                    } }) == v2 { 0 } else { 1 } } else { 0 });
                cw2 = cw2 + (if w2 > 0 && sc == 1 { if (if Buggy == 1 {
                        if w2 > 0 { w2 } else { if b3 == 0 { 0 } else { if u2 + v2 > 0 { 2 } else { 1 } } }
                    } else { if Buggy == 2 {
                        if b3 == 0 { 0 } else { 1 }
                    } else {
                        if b3 == 0 { 0 } else { if b1 + b2 > 0 { 2 } else { 1 } }
                    } }) == w2 { 0 } else { 1 } } else { 0 });
            }

            // Two islands that hold the same records evaluate them alike (Buggy 1: arrival order
            // decides the label).
            invariant MergeCommutes: d1 == 1 || d2 == 1 || a1 + b1 == 1 || a2 + b2 == 1 || a3 + b3 == 1 ||
                (u1 == u2 && v1 == v2 && w1 == w2 && x1 == x2);
            // Buggy 2: the next label above every label, re-dealt at each clash.
            invariant LabelChangesAtMostOnce: cu1 <= 1 && cv1 <= 1 && cw1 <= 1 &&
                cu2 <= 1 && cv2 <= 1 && cw2 <= 1;
            // Buggy 3: the admission wins.
            invariant RemoveBeatsConcurrentAdmit: sc <= 1 || sc > 2 ||
                ((d1 == 1 || a1 + a2 <= 1 || u1 == 0) && (d2 == 1 || b1 + b2 <= 1 || u2 == 0));
            // Buggy 4: the first removal voids the other.
            invariant MutualRemovesRemoveBoth: sc <= 2 || sc > 3 ||
                ((d1 == 1 || a1 + a2 <= 1 || u1 + v1 == 0) && (d2 == 1 || b1 + b2 <= 1 || u2 + v2 == 0));
            // Buggy 5: a cutoff reaches only records that follow the removal.
            invariant RemovedSignerInertAfterCutoff: sc <= 3 || sc > 4 ||
                ((d1 == 1 || a1 + a2 <= 1 || v1 == 0) && (d2 == 1 || b1 + b2 <= 1 || v2 == 0));
            // Buggy 6: no quorum rule.
            invariant QuorumVoidsItsActor: sc <= 4 || sc > 5 ||
                ((d1 == 1 || a1 + a2 + a3 <= 2 || (u1 == 0 && v1 + w1 + x1 == 3)) &&
                 (d2 == 1 || b1 + b2 + b3 <= 2 || (u2 == 0 && v2 + w2 + x2 == 3)));
            // Buggy 7: one signer is a quorum.
            invariant OneMemberIsNeverAQuorum: sc <= 5 || sc > 6 ||
                ((d1 == 1 || a1 + a2 <= 1 || (u1 == 1 && v1 == 0)) &&
                 (d2 == 1 || b1 + b2 <= 1 || (u2 == 1 && v2 == 0)));
            bound EvaluationBox: sc <= 6 && u1 <= 4 && v1 <= 4 && w1 <= 4 && x1 <= 1 &&
                u2 <= 4 && v2 <= 4 && w2 <= 4 && x2 <= 1 &&
                cu1 <= 2 && cv1 <= 2 && cw1 <= 2 && cu2 <= 2 && cv2 <= 2 && cw2 <= 2;
        }
    }
}
