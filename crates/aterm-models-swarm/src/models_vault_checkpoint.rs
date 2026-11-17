// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal model of the vault checkpoint's finality and share rule
//! (`crates/aterm-secrets/src/membership_log/checkpoint.rs`, `docs/DESIGN-vault-log-2026-11-14.md`
//! step 4, tracker FL-148b): a checkpoint is final only with more than half of its own cut's
//! recent seats' shares, a seat's shares never shrink, the newest final checkpoint contains every
//! final one (there is none where two final ones do not nest), and a prune keeps every share of a
//! checkpoint the newest does not contain.

use aterm_spec::derive::*;

/// `VaultCheckpoint`: three cuts of one log and three seats. Cut 1 is a day's cut; cuts 2 and 3
/// each contain it and not each other, a month later and a day apart (the two sides of a
/// split). Each cut
/// counts its own recent seats, those that signed a record of its past within 30 days before it:
/// A and B for cut 1; A alone for cut 2, whose past holds B's record of cut 1 and none since; B
/// alone for cut 3, the other way. Seat C signed nothing, so its share counts for nothing. So the
/// two sides' cuts can each be final, and not nest: no rule a side can apply alone tells a split
/// from dead seats.
///
/// `a1`..`c3` say which seat shared which cut's body, and `la`, `lb`, `lc` the cut each seat
/// shared last. `ShareXn` is seat X sharing cut n, allowed where every cut it shared before is
/// contained in cut n (`checkpoint::may_share`). `Judge` judges each body at once
/// (`checkpoint::judge`): `f1`..`f3` say which are final. `Prune` is a prune at a later cut that
/// holds every share (`checkpoint::plan_at`): `nw` is the newest final checkpoint the later body
/// names as its predecessor (0 for none), the final one whose cut contains every final one's,
/// and `kept` is how many share records the prune keeps: every share of the newest and of each
/// body it does not contain, and with no newest, every share. The share rule bounds `kept`: a
/// seat shares at most two of the three cuts, so a prune keeps at most six shares, and a seat
/// that shares with no rule breaks that.
///
/// The mutants: `Buggy == 1` shares with no rule, `Buggy == 2` finalizes at half the recent
/// seats, `Buggy == 3` counts every seat as recent (C's share counts), `Buggy == 4` lets a seat
/// share any cut later by its day than every cut it shared, nested or not, and `Buggy == 5`
/// takes the latest final checkpoint by its day as the newest, nested or not, and drops every
/// other final one's shares.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn vault_checkpoint_model() -> Model {
    aterm_spec::ty_model! {
        VaultCheckpoint {
            const Buggy = 0;

            var a1 = 0;
            var a2 = 0;
            var a3 = 0;
            var b1 = 0;
            var b2 = 0;
            var b3 = 0;
            var c1 = 0;
            var c2 = 0;
            var c3 = 0;
            var la = 0;
            var lb = 0;
            var lc = 0;
            var judged = 0;
            var f1 = 0;
            var f2 = 0;
            var f3 = 0;
            var nw = 0;
            var kept = 0;

            action ShareA1 when (judged == 0 && a1 == 0 && (Buggy == 1 || a2 + a3 == 0)) {
                a1 = 1;
                la = 1;
            }
            action ShareA2 when (judged == 0 && a2 == 0 && (Buggy == 1 || a3 == 0)) {
                a2 = 1;
                la = 2;
            }
            action ShareA3 when (judged == 0 && a3 == 0 && (Buggy == 1 || Buggy == 4 || a2 == 0)) {
                a3 = 1;
                la = 3;
            }
            action ShareB1 when (judged == 0 && b1 == 0 && (Buggy == 1 || b2 + b3 == 0)) {
                b1 = 1;
                lb = 1;
            }
            action ShareB2 when (judged == 0 && b2 == 0 && (Buggy == 1 || b3 == 0)) {
                b2 = 1;
                lb = 2;
            }
            action ShareB3 when (judged == 0 && b3 == 0 && (Buggy == 1 || Buggy == 4 || b2 == 0)) {
                b3 = 1;
                lb = 3;
            }
            action ShareC1 when (judged == 0 && c1 == 0 && (Buggy == 1 || c2 + c3 == 0)) {
                c1 = 1;
                lc = 1;
            }
            action ShareC2 when (judged == 0 && c2 == 0 && (Buggy == 1 || c3 == 0)) {
                c2 = 1;
                lc = 2;
            }
            action ShareC3 when (judged == 0 && c3 == 0 && (Buggy == 1 || Buggy == 4 || c2 == 0)) {
                c3 = 1;
                lc = 3;
            }
            action Judge when (judged == 0) {
                judged = 1;
                f1 = if Buggy == 2 {
                    if a1 + b1 > 0 { 1 } else { 0 }
                } else if Buggy == 3 {
                    if a1 + b1 + c1 > 1 { 1 } else { 0 }
                } else {
                    if a1 + b1 > 1 { 1 } else { 0 }
                };
                f2 = if Buggy == 3 {
                    if a2 + b2 + c2 > 1 { 1 } else { 0 }
                } else {
                    a2
                };
                f3 = if Buggy == 3 {
                    if a3 + b3 + c3 > 1 { 1 } else { 0 }
                } else {
                    b3
                };
            }
            action Prune when (judged == 1) {
                judged = 2;
                nw = if Buggy == 5 {
                    if f3 == 1 { 3 } else if f2 == 1 { 2 } else if f1 == 1 { 1 } else { 0 }
                } else if f2 + f3 == 2 {
                    0
                } else if f3 == 1 {
                    3
                } else if f2 == 1 {
                    2
                } else if f1 == 1 {
                    1
                } else {
                    0
                };
                kept = if Buggy == 5 {
                    if f3 == 1 {
                        a3 + b3 + c3 + (if f2 == 1 { 0 } else { a2 + b2 + c2 })
                    } else {
                        a1 + b1 + c1 + a2 + b2 + c2 + a3 + b3 + c3
                            - (if f2 == 1 { a1 + b1 + c1 } else { 0 })
                    }
                } else if f2 + f3 == 1 {
                    a2 + b2 + c2 + a3 + b3 + c3
                } else {
                    a1 + b1 + c1 + a2 + b2 + c2 + a3 + b3 + c3
                };
            }

            invariant FinalOnlyWithMajority:
                (f1 == 0 || a1 + b1 > 1) && (f2 == 0 || a2 == 1) && (f3 == 0 || b3 == 1);
            invariant NewestNestsEveryFinal:
                (nw == 0 || nw > 1 || f2 + f3 == 0)
                && (nw <= 1 || nw > 2 || f3 == 0)
                && (nw <= 2 || f2 == 0);
            invariant PruneKeepsWhatTheNewestOmits:
                judged <= 1
                || (nw > 1 && kept == a2 + b2 + c2 + a3 + b3 + c3)
                || (nw <= 1 && kept == a1 + b1 + c1 + a2 + b2 + c2 + a3 + b3 + c3);
            invariant SharesNeverShrink:
                (a1 == 0 || la > 0) && (a2 == 0 || la == 2) && (a3 == 0 || la == 3)
                && (b1 == 0 || lb > 0) && (b2 == 0 || lb == 2) && (b3 == 0 || lb == 3)
                && (c1 == 0 || lc > 0) && (c2 == 0 || lc == 2) && (c3 == 0 || lc == 3);
            invariant ChainAndSharesBounded: kept <= 6 && la <= 3 && lb <= 3 && lc <= 3;
        }
    }
}
