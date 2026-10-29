// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Account sync keeps ambiguous local identities and admits unclaimed new rows.
/// Candidate cardinality abstracts admitted local alias rows, not provider authority.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_alias_admission_model() -> Model {
    aterm_spec::ty_model! {
        AccountAliasAdmission {
            const Buggy = 0;
            var phase = 0; var candidates = 0; var label_kind = 0;
            var same_vendor = 1; var companion = 0;
            var accepted = 0; var added = 0; var kept = 0;
            var extra = 0; var reports = 1; var preserved = 1; var repeat = 1;
            action AddCandidate when (phase == 0 && candidates <= 1) { candidates = candidates + 1; }
            action NameFirst when (phase == 0 && candidates > 0 && label_kind == 0) { label_kind = 1; }
            action NameSecond when (phase == 0 && candidates == 2 && label_kind == 0) { label_kind = 2; }
            action NameOther when (phase == 0 && label_kind == 0) { label_kind = 3; }
            action ChangeMethod when (phase == 0 && same_vendor == 1) { same_vendor = 0; }
            action AddCompanion when (phase == 0 && companion == 0) { companion = 1; }
            action Sync when (phase == 0) {
                phase = 1;
                accepted = if Buggy == 8 && candidates == 2 {0} else {1};
                added = if Buggy == 2 && candidates == 2 && label_kind == 0 {1}
                    else if candidates == 0 && label_kind == 0 {if Buggy == 4 {0} else {1}}
                    else {0};
                kept = if (Buggy == 1 && candidates == 2 && label_kind == 1)
                    || (Buggy == 2 && candidates == 2 && label_kind == 0) {0}
                    else if candidates == 1 && label_kind == 1 && same_vendor == 1 {
                        if Buggy == 3 {1} else {0}
                    } else if candidates == 0 && label_kind == 0 {0} else {1};
                extra = if Buggy == 9 && candidates == 2 {0} else {companion};
                reports = if Buggy == 7 && candidates == 2 {0} else {1};
                preserved = if Buggy == 5 && candidates == 2 {0} else {1};
                repeat = if Buggy == 6 && candidates == 1 {0} else {1};
            }
            invariant Admission: phase == 0 || accepted == 1;
            invariant ConflictReport: phase == 0 || kept == if (candidates == 0 && label_kind == 0) || (candidates == 1 && label_kind == 1 && same_vendor == 1) {0} else {1};
            invariant FreshAddition: phase == 0 || added == if candidates == 0 && label_kind == 0 {1} else {0};
            invariant CompanionAddition: phase == 0 || extra == companion;
            invariant ExactReports: reports == 1;
            invariant ExistingPreserved: preserved == 1;
            invariant RepeatStable: repeat == 1;
        }
    }
}
