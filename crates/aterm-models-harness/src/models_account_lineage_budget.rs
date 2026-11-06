// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Capacity algebra for one lineage among unrelated reservations. Transfer is
/// the ledger step after the host independently verifies durable publication.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_lineage_budget_model() -> Model {
    aterm_spec::ty_model! {
        AccountLineageBudget {
            const Buggy = 0;
            var root = 0; var child = 0; var spare = 0; var others = 0;
            var attempted = 0; var accepted = 0; var hops = 0;
            action Root when (root == 0) {
                root = if others <= 126 || Buggy == 6 { 1 } else { 0 };
                spare = if others <= 126 || Buggy == 6 { if Buggy == 1 { 0 } else { 1 } } else { 0 };
            }
            action Other when (others <= 128 && (Buggy == 2 || root + child + spare + others <= 127)) {
                others = others + 1;
            }
            action Child when (root == 1 && child == 0) {
                attempted = 1;
                accepted = if Buggy == 3 && root + spare + others == 128 { 0 } else { 1 };
                child = if Buggy == 3 && root + spare + others == 128 { 0 } else { 1 };
                spare = if Buggy == 3 && root + spare + others == 128 { 1 } else { 0 };
            }
            action Cancel when (child == 1) {
                child = 0; spare = if Buggy == 4 { 0 } else { 1 };
                attempted = 0; accepted = 0;
            }
            action Transfer when (child == 1 && hops <= 2) {
                child = 0; spare = if Buggy == 5 { 0 } else { 1 };
                attempted = 0; accepted = 0; hops = hops + 1;
            }
            invariant Capacity: root + child + spare + others <= 128;
            invariant Reserved: root == 0 || child == 1 || spare == 1;
            invariant AdmittedAtCapacity: attempted == 0 || accepted == 1;
            bound Finite: 0 <= root && root <= 1 && 0 <= child && child <= 1
                && 0 <= spare && spare <= 1 && 0 <= others && others <= 129
                && 0 <= attempted && attempted <= 1 && 0 <= accepted && accepted <= 1
                && 0 <= hops && hops <= 3;
        }
    }
}
