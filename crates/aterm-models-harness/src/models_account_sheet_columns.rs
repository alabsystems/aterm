// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A selected account sheet refuses ambiguous columns while preserving provider discovery and
/// equivalent duplicates; rows that give one address conflicting assignments (case 8, and case
/// 10's member beside a row that disqualifies it) leave that address alone out and named, and
/// the rest of the sheet is read (`exact` there: no row takes the address, and its rows are
/// reported), so source order chooses no machine and one bad row stops no sync.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_sheet_columns_model() -> Model {
    aterm_spec::ty_model! {
        AccountSheetColumns {
            const Buggy = 0;
            var case = 0; var accepted = 0; var present = 0; var exact = 1;
            action Unique when (case == 0) {
                case = 1; accepted = if Buggy == 1 {0} else {1}; present = 1;
                exact = if Buggy == 8 {0} else {1};
            }
            action DuplicateEmail when (case == 0) {
                case = 2; accepted = if Buggy == 2 {1} else {0}; present = 1;
            }
            action DuplicateMember when (case == 0) {
                case = 3; accepted = if Buggy == 3 {1} else {0};
                present = if Buggy == 6 {0} else {1};
            }
            action MissingEmail when (case == 0) {
                case = 4; accepted = if Buggy == 4 {1} else {0}; present = 1;
            }
            action MissingMember when (case == 0) {
                case = 5; accepted = if Buggy == 5 {1} else {0};
                present = if Buggy == 9 {1} else {0};
            }
            action DuplicateAnchor when (case == 0) {
                case = 6; accepted = if Buggy == 7 {1} else {0}; present = 1;
            }
            action MultiMachine when (case == 0) {
                case = 7; accepted = 1; present = 1;
                exact = if Buggy == 10 {0} else {1};
            }
            action ConflictingRows when (case == 0) {
                case = 8; accepted = if Buggy == 11 {0} else {1}; present = 1;
                exact = if Buggy == 14 {0} else {1};
            }
            action EquivalentRows when (case == 0) {
                case = 9; accepted = if Buggy == 12 {0} else {1}; present = 1;
            }
            action ExcludedDuplicate when (case == 0) {
                case = 10; accepted = if Buggy == 13 {0} else {1}; present = 1;
                exact = if Buggy == 15 {0} else {1};
            }
            invariant NoAmbiguousSource: case <= 1 || case == 4 || case == 5 || case > 6 || accepted == 0;
            invariant RequiredColumns: 4 > case || case > 5 || accepted == 0;
            invariant ValidSource: case == 0 || (case > 1 && 7 > case) || accepted == 1;
            invariant HeaderPresence: case == 0 || (case == 5 && present == 0) || ((5 > case || case > 5) && present == 1);
            invariant ExactMapping: accepted == 0 || exact == 1;
        }
    }
}
