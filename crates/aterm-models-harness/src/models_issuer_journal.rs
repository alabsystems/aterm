// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Two live host epochs share five physical records. Preparation reserves two
/// future records; terminal closure releases only its unused append capacity.
/// Once a read finds an operation's record, every later read finds it: the host's
/// unadmitted answer reads absence as never recorded.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn issuer_journal_model() -> Model {
    aterm_spec::ty_model! {
        IssuerJournal {
            const Buggy = 0;
            // 0 absent, 1 prepared, 2 may execute, 3 NoExec,
            // 4 sealed without MayExecute, 5 sealed with MayExecute.
            var a = 0; var b = 0; var records = 0;
            var a_may = 0; var b_may = 0;
            var a_closed = 0; var b_closed = 0;
            var observed = 0; var changed = 0; var seen_a = 0;
            action PrepareA when (a == 0) {
                a = if records + (if b == 1 {2} else {if b == 2 {1} else {0}}) + 3 <= 5 {1} else {0};
                records = if records + (if b == 1 {2} else {if b == 2 {1} else {0}}) + 3 <= 5 {records + 1} else {records};
            }
            action PrepareB when (b == 0) {
                b = if records + (if Buggy == 1 {0} else {if a == 1 {2} else {if a == 2 {1} else {0}}}) + 3 <= 5 {1} else {0};
                records = if records + (if Buggy == 1 {0} else {if a == 1 {2} else {if a == 2 {1} else {0}}}) + 3 <= 5 {records + 1} else {records};
            }
            action MayA when (a == 1) { a = 2; a_may = 1; records = records + 1; }
            action MayB when (b == 1) { b = 2; b_may = 1; records = records + 1; }
            action NoExecA when (a == 1 || (a == 2 && Buggy == 2)) {
                a = 3; a_closed = 1; records = records + 1;
            }
            action NoExecB when (b == 1 || (b == 2 && Buggy == 2)) {
                b = 3; b_closed = 1; records = records + 1;
            }
            action SealA when (a == 1 || a == 2) {
                a = if a == 2 && Buggy <= 2 || a == 2 && Buggy > 3 {5} else {4};
                a_closed = 1; records = records + 1;
            }
            action SealB when (b == 1 || b == 2) {
                b = if b == 2 && Buggy <= 2 || b == 2 && Buggy > 3 {5} else {4};
                b_closed = 1; records = records + 1;
            }
            action ObserveA when (observed == 0) {
                observed = if a == 3 || Buggy == 4 {1} else {0};
            }
            action RetryA when (a > 0) { records = if Buggy == 5 {records + 1} else {records}; }
            action ConflictA when (a > 0) { changed = if Buggy == 6 {1} else {0}; }
            action ContainsA when (seen_a == 0) { seen_a = if a > 0 {1} else {0}; }
            action ReopenA when (a > 2) { a = if Buggy == 7 {1} else {if Buggy == 8 {0} else {a}}; }
            invariant Capacity:
                records + (if a == 1 {2} else {if a == 2 {1} else {0}})
                    + (if b == 1 {2} else {if b == 2 {1} else {0}}) <= 5;
            invariant MayNeverNoExec: (a_may == 0 || a == 2 || a == 5) && (b_may == 0 || b == 2 || b == 5);
            invariant ObserveOnlyNoExec: observed == 0 || a == 3;
            invariant Counted:
                records == (if a == 0 {0} else {if a == 1 {1} else {if a == 5 {3} else {2}}})
                    + (if b == 0 {0} else {if b == 1 {1} else {if b == 5 {3} else {2}}});
            invariant Immutable: changed == 0;
            invariant Terminal: (a_closed == 0 || a > 2) && (b_closed == 0 || b > 2);
            invariant PresencePermanent: seen_a == 0 || a > 0;
            invariant Finite: 0 <= a && a <= 5 && 0 <= b && b <= 5 && 0 <= records && records <= 6
                && 0 <= a_may && a_may <= 1 && 0 <= b_may && b_may <= 1
                && 0 <= a_closed && a_closed <= 1 && 0 <= b_closed && b_closed <= 1
                && 0 <= observed && observed <= 1 && 0 <= changed && changed <= 1
                && 0 <= seen_a && seen_a <= 1;
        }
    }
}
