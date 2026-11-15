// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::client::{MaterializationBlocker as Blocker, MaterializationObservation};

// Negative control, run: matching LockMissing instead of WriterBusy fails both tests.
#[test]
fn a_busy_vault_writer_withholds_the_row_without_reading_a_false_verdict() {
    let observed = MaterializationObservation::<()>::Blocked(Blocker::WriterBusy);
    let row = member_observation(observed, |_| panic!("a writer is no readiness verdict"))
        .expect("withheld reading");
    assert_eq!(row.subject, Subject::Setting(Setting::Secrets));
    assert!(row.is_withheld());
}

#[test]
fn an_actual_materialization_blocker_still_reads_not_ready() {
    for blocker in [
        Blocker::LockMissing,
        Blocker::Unconfigured,
        Blocker::ConfigChanged,
        Blocker::UntrustedPin,
        Blocker::NotAdmitted,
        Blocker::Revoked,
        Blocker::Dropped,
        Blocker::SourceChanged,
        Blocker::ApplyIncomplete,
    ] {
        let observed = MaterializationObservation::<()>::Blocked(blocker);
        let row = member_observation(observed, |ready| {
            assert!(!ready);
            Ok(Observation::unknown(Subject::Setting(Setting::Secrets)))
        })
        .expect("actual reading");
        assert!(!row.is_withheld());
    }
}
