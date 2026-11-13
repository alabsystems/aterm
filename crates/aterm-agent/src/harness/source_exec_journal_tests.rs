// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn snapshot() -> Snapshot {
    Snapshot {
        digest: [7; 32],
        bytes: 513,
    }
}

#[test]
fn exact_retries_preserve_one_snapshot_and_terminal_history() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut journal = Journal::create_at(root.path(), &[9; 32], b"exact live intent").unwrap();
    assert!(!journal.possible_execution());
    assert!(journal.activated().is_err());
    assert!(journal.settled().is_err());
    let first = journal.may_execute(snapshot()).unwrap();
    assert!(journal.possible_execution());
    assert!(journal.settled().is_err());
    assert_eq!(journal.may_execute(snapshot()).unwrap(), first);
    assert!(
        journal
            .may_execute(Snapshot {
                bytes: 514,
                ..snapshot()
            })
            .is_err()
    );
    let activated = journal.activated().unwrap();
    assert!(activated > first);
    assert_eq!(journal.activated().unwrap(), activated);
    assert_eq!(journal.may_execute(snapshot()).unwrap(), first);
    let settled = journal.settled().unwrap();
    assert!(settled > activated);
    assert_eq!(journal.settled().unwrap(), settled);
    assert_eq!(journal.activated().unwrap(), activated);
    assert_eq!(journal.may_execute(snapshot()).unwrap(), first);
    let records = journal.log.read_range(astream_wire::Offset(0), 10).unwrap();
    assert_eq!(records.len(), 4);
    for (index, record) in records.iter().enumerate() {
        assert_eq!(record.subject, SUBJECT);
        assert_eq!(record.body.len(), BODY);
        assert_eq!(record.body[4], u8::try_from(index).unwrap());
        hmac::verify(
            &journal.key,
            &record.body[..BODY - 32],
            &record.body[BODY - 32..],
        )
        .unwrap();
    }
}

#[test]
fn damaged_custody_is_unknown_and_never_becomes_no_execution() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut journal = Journal::create_at(root.path(), &[9; 32], b"exact live intent").unwrap();
    let dir = std::fs::read_dir(root.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::remove_file(dir.join("source.checkpoint")).unwrap();
    assert!(journal.may_execute(snapshot()).is_err());
    assert!(journal.possible_execution());
    assert!(journal.activated().is_err());
    assert!(journal.settled().is_err());
    assert!(journal.may_execute(snapshot()).is_err());
    assert!(journal.possible_execution());
}

#[test]
fn concurrent_live_owners_do_not_share_a_writer_or_authority() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut a = Journal::create_at(root.path(), &[9; 32], b"same caller words").unwrap();
    let mut b = Journal::create_at(root.path(), &[9; 32], b"same caller words").unwrap();
    a.may_execute(snapshot()).unwrap();
    assert!(!b.possible_execution());
    b.may_execute(Snapshot {
        bytes: 514,
        ..snapshot()
    })
    .unwrap();
    assert_ne!(a.intent, b.intent);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
}
