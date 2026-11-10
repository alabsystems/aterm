// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Local precedence with independent source presence and request scope. This proves selection,
/// not source authenticity, currency, parsing, durable retirement or account authorization.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_source_selection_model() -> Model {
    aterm_spec::ty_model! {
        AccountSourceSelection {
            const Buggy = 0;
            var init = 0; var key = 0; var named = 0; var beside = 0;
            var pool = 0; var vault = 0; var sheet = 0; var retired = 0;
            var selected = 0; var phase = 0;
            action Automatic when (phase == 0 && init == 0) { init = 1; }
            action Key when (phase == 0 && key == 0) { key = 1; }
            action Named when (phase == 0 && named == 0) { named = 1; }
            action Beside when (phase == 0 && beside == 0) { beside = 1; }
            action Pool when (phase == 0 && pool == 0) { pool = 1; }
            action Vault when (phase == 0 && vault == 0) { vault = 1; }
            action Sheet when (phase == 0 && sheet == 0) { sheet = 1; }
            action Retire when (phase == 0 && retired == 0) { retired = 1; }
            action Select when (phase == 0) {
                selected = if Buggy == 1 && init == 1 && named == 1 && pool == 1 {2}
                    else if key == 1 && named == 0 {1}
                    else if pool == 1 && key == 0 {2}
                    else if (init == 1 || Buggy == 2) && vault == 1 && (retired == 0 || Buggy == 3) {3}
                    else if init == 1 && sheet == 1 {4}
                    else if named == 1 || (init == 1 && beside == 1) {5}
                    else {0};
                phase = 1;
            }
            invariant SelectionMatchesPolicy: phase == 0 || selected ==
                (if key == 1 && named == 0 {1}
                 else if pool == 1 && key == 0 {2}
                 else if init == 1 && vault == 1 && retired == 0 {3}
                 else if init == 1 && sheet == 1 {4}
                 else if named == 1 || (init == 1 && beside == 1) {5}
                 else {0});
            bound SelectionBox: init <= 1 && key <= 1 && named <= 1 && beside <= 1
                && pool <= 1 && vault <= 1 && sheet <= 1 && retired <= 1 && selected <= 5 && phase <= 1;
        }
    }
}
