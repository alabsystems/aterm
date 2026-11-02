// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Bounded comparison reports preserve their limits and incomplete-history
/// evidence. Equality of source metadata cannot authorize a replacement.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn import_comparison_model() -> Model {
    aterm_spec::ty_model! {
        DbImportComparison {
            const Buggy = 0;
            var scenario = 0; var source_bounded = 1; var row_bounded = 1;
            var source_omitted = 0; var source_truncated = 0;
            var row_omitted = 0; var row_truncated = 0;
            var history = 1; var history_shown = 1;
            var conflict = 0; var conflict_shown = 0; var authority = 0;
            action Complete when (scenario == 0) {
                scenario = 1; history_shown = if Buggy == 5 { 0 } else { 1 };
                authority = if Buggy == 7 { 1 } else { 0 };
            }
            action SourcesCapped when (scenario == 0) {
                scenario = 2; source_omitted = 1;
                source_bounded = if Buggy == 1 { 0 } else { 1 };
                source_truncated = if Buggy == 2 { 0 } else { 1 };
            }
            action Withheld when (scenario == 0) {
                scenario = 3; history = 0;
                history_shown = if Buggy == 3 { 1 } else { 0 };
            }
            action Opaque when (scenario == 0) {
                scenario = 4; history = 0;
                history_shown = if Buggy == 3 { 1 } else { 0 };
            }
            action Rejected when (scenario == 0) {
                scenario = 5; history = 0;
                history_shown = if Buggy == 3 { 1 } else { 0 };
            }
            action RowsCapped when (scenario == 0) {
                scenario = 6; row_omitted = 1;
                row_truncated = if Buggy == 4 { 0 } else { 1 };
                row_bounded = if Buggy == 8 { 0 } else { 1 };
            }
            action Conflict when (scenario == 0) {
                scenario = 7; conflict = 1;
                conflict_shown = if Buggy == 6 { 0 } else { 1 };
            }
            action Duplicate when (scenario == 0) {
                scenario = 8; history_shown = if Buggy == 5 { 0 } else { 1 };
            }
            action ConflictCapped when (scenario == 0) {
                scenario = 9; source_omitted = 1; conflict = 1;
                source_bounded = if Buggy == 1 { 0 } else { 1 };
                source_truncated = if Buggy == 2 { 0 } else { 1 };
                conflict_shown = if Buggy == 6 { 0 } else { 1 };
            }
            invariant SourceBound: source_bounded == 1;
            invariant RowBound: row_bounded == 1;
            invariant SourceOmissionsVisible: source_omitted == 0 || source_truncated == 1;
            invariant RowOmissionsVisible: row_omitted == 0 || row_truncated == 1;
            invariant HistoryTruth: history_shown <= history;
            invariant HistoryAvailability: history <= history_shown;
            invariant ConflictsVisible: conflict <= conflict_shown;
            invariant NoAuthority: authority == 0;
        }
    }
}
