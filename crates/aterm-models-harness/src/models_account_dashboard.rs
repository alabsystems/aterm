// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// A dashboard keeps its report's projection across a later publication or read
/// failure. The record reader and identity filter have their own models.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_dashboard_model() -> Model {
    aterm_spec::ty_model! {
        AccountDashboard {
            const Buggy = 0;
            var phase = 0; var fault = 0; var later = 0;
            var visible = 0; var coherent = 1;
            action AnotherFault when (phase == 0 && 6 > fault) { fault = fault + 1; }
            action LaterChange when (phase == 0 && 2 > later) { later = later + 1; }
            action Draw when (phase == 0) {
                phase = 1;
                visible = if 2 > fault {
                    if Buggy == 1 {0} else {1}
                } else { if Buggy == 2 {1} else {0} };
                coherent = if Buggy == 3 && later > 0 {0} else {1};
            }
            invariant KnownReportVisible: phase == 0 || fault > 1 || visible == 1;
            invariant UnknownReportHidden: phase == 0 || 2 > fault || visible == 0;
            invariant SameCapturedProjection: coherent == 1;
        }
    }
}
