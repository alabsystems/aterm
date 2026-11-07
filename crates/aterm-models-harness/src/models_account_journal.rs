// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// One closed parent, two candidates and both old and typed claim paths.
/// `binding` abstracts exact parent/recipe identity; `phase` is the request's
/// phase relative to the immutable forward parent. There is no execution grant.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_journal_model() -> Model {
    aterm_spec::ty_model! {
        AccountJournal {
            const Buggy = 0;
            // 0 absent, 1 legacy claim, 2 account claim, 3 account fence.
            var a = 0; var b = 0;
            var known = 1; var phase = 0; var binding = 1;
            var a_phase = 2; var b_phase = 2; var a_auth = 2; var b_auth = 2;
            var ca = 0; var cb = 0; var fa = 0; var fb = 0; var old_a = 0;
            var winner = 0;
            // Child: Prepared, MayExecute, NoExec, Seal without/with May.
            var child = 0;
            var records = 2; var bytes = 968; var held = 0;
            var limit = 9; var byte_limit = 5000;
            var ack = 0; var observed = 0; var which = 0;

            action LegacyParent when (a == 0 && b == 0 && known == 1) {
                known = 0; ack = 0; observed = 0; which = 0;
            }
            action WrongPhase when (phase == 0) {
                phase = 1; ack = 0; observed = 0; which = 0;
            }
            action RightPhase when (phase == 1) {
                phase = 0; ack = 0; observed = 0; which = 0;
            }
            action WrongBinding when (binding == 1) {
                binding = 0; ack = 0; observed = 0; which = 0;
            }
            action RightBinding when (binding == 0) {
                binding = 1; ack = 0; observed = 0; which = 0;
            }
            action TightRecords when (a == 0 && b == 0 && limit == 9) {
                limit = 4; ack = 0; observed = 0; which = 0;
            }
            action TightBytes when (a == 0 && b == 0 && byte_limit == 5000) {
                byte_limit = 2455; ack = 0; observed = 0; which = 0;
            }
            action ClaimA when ((a == 0 || (Buggy == 2 && a == 3))
                && ((known == 1 && phase == 0) || Buggy == 1)
                && (binding == 1 || Buggy == 6) && (winner == 0 || Buggy == 4)
                && ((records + held + 3 <= limit && bytes + (if held == 0 {0} else {if held == 1 {484} else {if held == 2 {968} else {if held == 3 {1452} else {1936}}}}) + 1488 <= byte_limit) || Buggy == 11)) {
                a = 2; ca = 1; winner = 1; a_phase = phase; a_auth = binding;
                records = records + 1; bytes = bytes + (if Buggy == 8 {519} else {520});
                held = held + (if Buggy == 12 {0} else {2});
                ack = 2; observed = 0; which = 1;
            }
            action ClaimB when ((b == 0 || (Buggy == 2 && b == 3))
                && ((known == 1 && phase == 0) || Buggy == 1)
                && (binding == 1 || Buggy == 6) && (winner == 0 || Buggy == 4)
                && ((records + held + 3 <= limit && bytes + (if held == 0 {0} else {if held == 1 {484} else {if held == 2 {968} else {if held == 3 {1452} else {1936}}}}) + 1488 <= byte_limit) || Buggy == 11)) {
                b = 2; cb = 1; winner = 2; b_phase = phase; b_auth = binding;
                records = records + 1; bytes = bytes + (if Buggy == 8 {519} else {520});
                held = held + (if Buggy == 12 {0} else {2});
                ack = 2; observed = 0; which = 2;
            }
            action FenceA when ((a == 0 || (Buggy == 3 && (a == 1 || a == 2)))
                && ((known == 1 && phase == 0) || Buggy == 1) && (binding == 1 || Buggy == 6)
                && ((records + held + 1 <= limit && bytes + (if held == 0 {0} else {if held == 1 {484} else {if held == 2 {968} else {if held == 3 {1452} else {1936}}}}) + 524 <= byte_limit) || Buggy == 11)) {
                a = 3; fa = 1; a_phase = phase; a_auth = binding;
                records = records + 1; bytes = bytes + 524;
                ack = 3; observed = 0; which = 1;
            }
            action FenceB when ((b == 0 || (Buggy == 3 && b == 2))
                && ((known == 1 && phase == 0) || Buggy == 1) && (binding == 1 || Buggy == 6)
                && ((records + held + 1 <= limit && bytes + (if held == 0 {0} else {if held == 1 {484} else {if held == 2 {968} else {if held == 3 {1452} else {1936}}}}) + 524 <= byte_limit) || Buggy == 11)) {
                b = 3; fb = 1; b_phase = phase; b_auth = binding;
                records = records + 1; bytes = bytes + 524;
                ack = 3; observed = 0; which = 2;
            }
            action RefuseA when (a == 0 && (known == 0 || phase == 1 || binding == 0 || winner > 0
                || records + held + 3 > limit || bytes + (if held == 0 {0} else {if held == 1 {484} else {if held == 2 {968} else {if held == 3 {1452} else {1936}}}}) + 1488 > byte_limit)) {
                ack = if Buggy == 10 {2} else {0}; observed = 0; which = 1;
            }
            action RefuseB when (b == 0 && (known == 0 || phase == 1 || binding == 0 || winner > 0
                || records + held + 3 > limit || bytes + (if held == 0 {0} else {if held == 1 {484} else {if held == 2 {968} else {if held == 3 {1452} else {1936}}}}) + 1488 > byte_limit)) {
                ack = if Buggy == 10 {2} else {0}; observed = 0; which = 2;
            }
            action RefuseFenceA when (a == 0 && (known == 0 || phase == 1 || binding == 0
                || records + held + 1 > limit || bytes + (if held == 0 {0} else {if held == 1 {484} else {if held == 2 {968} else {if held == 3 {1452} else {1936}}}}) + 524 > byte_limit)) {
                ack = if Buggy == 10 {3} else {0}; observed = 0; which = 1;
            }
            action RefuseFenceB when (b == 0 && (known == 0 || phase == 1 || binding == 0
                || records + held + 1 > limit || bytes + (if held == 0 {0} else {if held == 1 {484} else {if held == 2 {968} else {if held == 3 {1452} else {1936}}}}) + 524 > byte_limit)) {
                ack = if Buggy == 10 {3} else {0}; observed = 0; which = 2;
            }
            action LegacyA when (a == 0 && winner == 0 && binding == 1
                && records + held + 3 <= limit && bytes + (if held == 0 {0} else {if held == 1 {484} else {if held == 2 {968} else {if held == 3 {1452} else {1936}}}}) + 1487 <= byte_limit) {
                a = 1; old_a = 1; ca = 1; winner = 1;
                records = records + 1; bytes = bytes + 519; held = held + 2;
                ack = 0; observed = 0; which = 0;
            }
            action ReplayA when (a > 0) {
                ack = if (a == 2 || (Buggy == 5 && a == 1)) && known == 1 && phase == 0
                    && (binding == 1 || Buggy == 6) {2} else {0};
                a = if Buggy == 5 && a == 1 {2} else {a};
                a_phase = if Buggy == 5 && a == 1 {0} else {a_phase};
                a_auth = if Buggy == 5 && a == 1 {1} else {a_auth};
                records = records + (if Buggy == 9 && a == 2 && 9 > records {1} else {0});
                bytes = bytes + (if Buggy == 9 && a == 2 && 9 > records {520} else {0});
                observed = 0; which = 1;
            }
            action ReplayB when (b > 0) {
                ack = if b == 2 && known == 1 && phase == 0 && (binding == 1 || Buggy == 6) {2} else {0};
                records = records + (if Buggy == 9 && b == 2 && 9 > records {1} else {0});
                bytes = bytes + (if Buggy == 9 && b == 2 && 9 > records {520} else {0});
                observed = 0; which = 2;
            }
            action ReplayFenceA when (a > 0) {
                ack = if a == 3 && known == 1 && phase == 0 && binding == 1 {3} else {0};
                observed = 0; which = 1;
            }
            action ReplayFenceB when (b > 0) {
                ack = if b == 3 && known == 1 && phase == 0 && binding == 1 {3} else {0};
                observed = 0; which = 2;
            }
            action ObserveA {
                observed = if known == 1 && phase == 0 && binding == 1 && 2 <= a {a}
                    else {if Buggy == 7 {2} else {0}};
                ack = 0; which = 1;
            }
            action ObserveB {
                observed = if known == 1 && phase == 0 && binding == 1 && 2 <= b {b}
                    else {if Buggy == 7 {2} else {0}};
                ack = 0; which = 2;
            }
            action May when (winner > 0 && child == 0) {
                child = 1; held = if held > 0 {held - 1} else {0}; records = records + 1; bytes = bytes + 484;
                ack = 0; observed = 0; which = 0;
            }
            action NoExec when (winner > 0 && child == 0) {
                child = 2; held = 0; records = records + 1; bytes = bytes + 484;
                ack = 0; observed = 0; which = 0;
            }
            action Seal when (winner > 0 && child <= 1) {
                child = if child == 1 {4} else {3}; held = 0;
                records = records + 1; bytes = bytes + 484;
                ack = 0; observed = 0; which = 0;
            }

            invariant ExactRelation: (2 > a || (known == 1 && a_phase == 0 && a_auth == 1))
                && (2 > b || (known == 1 && b_phase == 0 && b_auth == 1));
            invariant FencesPermanent: (fa == 0 || a == 3) && (fb == 0 || b == 3);
            invariant ClaimsPermanent: (ca == 0 || a == 1 || a == 2) && (cb == 0 || b == 2);
            invariant LegacyStaysLegacy: old_a == 0 || a == 1;
            invariant UniqueSuccessor: ca + cb <= 1;
            invariant AcknowledgedExact: ack == 0 || (known == 1 && phase == 0 && binding == 1
                && ((which == 1 && a == ack) || (which == 2 && b == ack)));
            invariant ObservationExact: observed == 0 || (known == 1 && phase == 0 && binding == 1
                && ((which == 1 && a == observed) || (which == 2 && b == observed)));
            invariant Capacity: records + held <= limit && bytes + (if held == 0 {0} else {if held == 1 {484} else {if held == 2 {968} else {if held == 3 {1452} else {1936}}}}) <= byte_limit;
            invariant CountedRecords: records == 2 + (if a > 0 {1} else {0}) + (if b > 0 {1} else {0})
                + (if child == 4 {2} else {if child > 0 {1} else {0}});
            invariant CountedBytes: bytes == 968 + (if a == 1 {519} else {if a == 2 {520} else {if a == 3 {524} else {0}}})
                + (if b == 2 {520} else {if b == 3 {524} else {0}})
                + (if child == 4 {968} else {if child > 0 {484} else {0}});
            invariant HeldClosures: held == (if winner == 0 || child > 1 {0} else {if child == 1 {1} else {2}});
            bound Finite: 0 <= a && a <= 3 && 0 <= b && b <= 3 && 0 <= known && known <= 1
                && 0 <= phase && phase <= 1 && 0 <= binding && binding <= 1
                && 0 <= a_phase && a_phase <= 2 && 0 <= b_phase && b_phase <= 2
                && 0 <= a_auth && a_auth <= 2 && 0 <= b_auth && b_auth <= 2
                && 0 <= ca && ca <= 1 && 0 <= cb && cb <= 1 && 0 <= fa && fa <= 1
                && 0 <= fb && fb <= 1 && 0 <= old_a && old_a <= 1
                && 0 <= winner && winner <= 2 && 0 <= child && child <= 4
                && 2 <= records && records <= 11 && 968 <= bytes && bytes <= 8000 && 0 <= held && held <= 4
                && 4 <= limit && limit <= 9 && 2455 <= byte_limit && byte_limit <= 5000
                && 0 <= ack && ack <= 3 && 0 <= observed && observed <= 3 && 0 <= which && which <= 2;
        }
    }
}
