// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Admission and retention of a sync report after publication. Native fixtures
/// exercise the record reader and journal; this model bounds the reporting policy.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_sync_report_model() -> Model {
    aterm_spec::ty_model! {
        AccountSyncReport {
            const Buggy = 0;
            var phase = 0; var fault = 0; var published = 0; var edit = 0;
            var available = 0; var changed = 0; var retained = 1;
            action AnotherFault when (phase == 0 && 6 > fault) { fault = fault + 1; }
            action Published when (phase == 0 && published == 0) { published = 1; }
            action LaterEdit when (phase == 0 && edit == 0) { edit = 1; }
            action Read when (phase == 0) {
                phase = 1;
                available = if Buggy == 1 {1} else {
                    if Buggy == 2 {0} else { if 2 > fault {1} else {0} }
                };
                changed = if published == 1 {
                    if Buggy == 5 { if fault == 1 {1} else {0} } else {1}
                } else {
                    if fault == 1 { if Buggy == 3 {0} else {1} } else {0}
                };
                retained = if Buggy == 4 && edit == 1 {0} else {1};
            }
            invariant UnknownRefused: phase == 0 || 2 > fault || available == 0;
            invariant KnownAvailable: phase == 0 || fault > 1 || available == 1;
            invariant RecoveryCounted: phase == 0 || available == 0 || fault == 0 || fault > 1 || changed == 1;
            invariant PublishedCounted: phase == 0 || available == 0 || published == 0 || changed == 1;
            invariant SnapshotRetained: retained == 1;
        }
    }
}
