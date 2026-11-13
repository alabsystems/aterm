// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn reveal_records_metadata_once_and_fails_closed() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let journal = Journal::at(dir.path());
    let cell = Cell {
        table: "alab/registry",
        collection: "mailbox",
        row: "mb-fixture",
        column: "password",
    };
    let value = "fixture-password-not-for-logs";
    let token = secret::Secret::new(value.into())
        .seal(&[3; 32], &[4; 32], cell)
        .unwrap();
    assert!(
        reveal(
            &journal,
            &token,
            &[3; 32],
            cell,
            Surface::SignIn,
            123,
            |plain| plain == value
        )
        .unwrap()
    );
    let log = std::fs::read_to_string(&journal.path).unwrap();
    assert_eq!(log.lines().count(), 1);
    assert!(!log.contains(value));
    assert!(!log.contains(&token));
    assert!(log.contains("SignIn"));
    assert!(
        reveal(
            &journal,
            &token,
            &[8; 32],
            cell,
            Surface::Grid,
            124,
            |_| panic!("must not reveal")
        )
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(&journal.path).unwrap(), log);
    let blocked = Journal {
        path: dir.path().to_owned(),
    };
    assert!(
        reveal(
            &blocked,
            &token,
            &[3; 32],
            cell,
            Surface::Grid,
            124,
            |_| panic!("must not reveal")
        )
        .is_err()
    );
}
