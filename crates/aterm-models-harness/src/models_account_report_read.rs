// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::derive::Model;

/// Bounded report reads of four provider stores, three roster shapes, and ten
/// publication/failure fixtures. External file editors are not serialized.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_report_read_model() -> Model {
    aterm_spec::ty_model! {
        AccountReportRead {
            const Buggy = 0;
            var phase = 0; var provider = 1; var shape = 0; var fault = 0;
            var available = 0; var coherent = 1; var repeatable = 1; var preserved = 1;
            action AnotherProvider when (phase == 0 && 4 > provider) { provider = provider + 1; }
            action AnotherShape when (phase == 0 && 2 > shape) { shape = shape + 1; }
            action AnotherFault when (phase == 0 && 9 > fault) { fault = fault + 1; }
            action Read when (phase == 0) {
                phase = 1;
                available = if 2 > fault || fault == 2 && shape > 0 || fault == 9 && shape == 0 {
                    if Buggy == 1 {0} else {1}
                } else { if Buggy == 2 {1} else {0} };
                coherent = if Buggy == 3 {0} else {1};
                repeatable = if Buggy == 4 {0} else {1};
                preserved = if Buggy == 5 {0} else {1};
            }
            invariant RecoverableReadAvailable:
                phase == 0 || (fault > 1 && (fault > 2 || shape == 0) && (9 > fault || shape > 0)) || available == 1;
            invariant UnresolvedOrSpanningReadRefused:
                phase == 0 || 2 > fault || (fault == 2 && shape > 0) || (fault == 9 && shape == 0) || available == 0;
            invariant WholeSnapshotOrRefusal: coherent == 1;
            invariant StableAfterPublication: repeatable == 1;
            invariant CredentialsAndRefusedRecordsPreserved: preserved == 1;
        }
    }
}

/// The default-home identity filter follows the captured pool cardinality,
/// independently of a later roster on disk.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn account_report_attribution_model() -> Model {
    aterm_spec::ty_model! {
        AccountReportAttribution {
            const Buggy = 0;
            var phase = 0; var snapshot_many = 0; var disk_many = 0; var mismatch = 0;
            var kept = 1;
            action SnapshotMany when (phase == 0 && snapshot_many == 0) { snapshot_many = 1; }
            action DiskMany when (phase == 0 && disk_many == 0) { disk_many = 1; }
            action OtherProfile when (phase == 0 && mismatch == 0) { mismatch = 1; }
            action Read when (phase == 0) {
                phase = 1;
                kept = if Buggy == 1 {
                    if disk_many == 1 || mismatch == 0 {1} else {0}
                } else {
                    if Buggy == 2 {1} else {
                        if Buggy == 3 {
                            if mismatch == 0 {1} else {0}
                        } else { if snapshot_many == 1 || mismatch == 0 {1} else {0} }
                    }
                };
            }
            invariant CapturedPoolOrSameIdentityKeeps:
                phase == 0 || (snapshot_many == 0 && mismatch == 1) || kept == 1;
            invariant SinglePoolOtherIdentityDrops:
                phase == 0 || snapshot_many == 1 || mismatch == 0 || kept == 0;
        }
    }
}
