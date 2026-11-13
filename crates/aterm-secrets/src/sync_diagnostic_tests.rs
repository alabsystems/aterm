// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn outcomes_round_trip_without_free_form_messages() {
    for outcome in [
        SyncDiagnostic::Applied {
            seq: u64::MAX,
            failed: u64::MAX,
        },
        SyncDiagnostic::Unchanged(2),
        SyncDiagnostic::PutBack(3),
        SyncDiagnostic::NotIn,
        SyncDiagnostic::Failed(ErrorKind::JournalBusy),
        SyncDiagnostic::Failed(ErrorKind::CustodyBusy),
        SyncDiagnostic::Failed(ErrorKind::VaultBusy),
        SyncDiagnostic::Failed(ErrorKind::VaultChanged),
        SyncDiagnostic::Failed(ErrorKind::MembershipCapacity),
        SyncDiagnostic::Failed(ErrorKind::MembershipHistoryUnavailable),
        SyncDiagnostic::Failed(ErrorKind::TimedOut),
        SyncDiagnostic::Failed(ErrorKind::JoinQueueIncomplete),
        SyncDiagnostic::Failed(ErrorKind::NotStored),
        SyncDiagnostic::Failed(ErrorKind::Refused),
        SyncDiagnostic::Failed(ErrorKind::MemberBusy),
        SyncDiagnostic::Failed(ErrorKind::LabIdentity),
        SyncDiagnostic::Failed(ErrorKind::Dropped),
        SyncDiagnostic::Failed(ErrorKind::Other),
    ] {
        for ending in ["", "\n", "\r\n"] {
            assert_eq!(
                SyncDiagnostic::parse(&format!("{}{ending}", outcome.line())),
                Some(outcome)
            );
        }
    }
}

#[test]
fn unknown_or_extra_data_is_not_a_diagnostic() {
    for text in [
        "aterm-secrets-sync-v2 unchanged 4",
        "aterm-secrets-sync-v1 failed password=private",
        "aterm-secrets-sync-v1 unchanged -1",
        "aterm-secrets-sync-v1 unchanged +1",
        "aterm-secrets-sync-v1 applied +2 0",
        "aterm-secrets-sync-v1 applied 2",
        "aterm-secrets-sync-v1 unchanged 3\napplied seq 99",
        "aterm-secrets-sync-v1\nunchanged 3",
        "aterm-secrets-sync-v1 failed local-network-pending",
    ] {
        assert_eq!(SyncDiagnostic::parse(text), None);
    }
}
