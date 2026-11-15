// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::typed::secret::{Memory, Secret, Store, reference};

#[test]
fn reveal_reads_the_store_records_metadata_once_and_fails_closed() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let journal = Journal::at(dir.path());
    let cell = Cell {
        table: "alab/registry.3",
        collection: "mailbox",
        row: "mb-fixture",
        column: "totp",
    };
    let value = "fixture-password-not-for-logs";
    let store = Memory::default();
    let entry = "alab-mailbox-totp-mb-fixture";
    store.put(entry, &Secret::new(value.into())).unwrap();
    let named = reference(Memory::VAULT, entry);
    assert_eq!(
        reveal(
            &journal,
            &store,
            &named,
            cell,
            Surface::SignIn,
            123,
            |plain| plain == value
        )
        .unwrap(),
        Some(true)
    );
    let log = std::fs::read_to_string(&journal.path).unwrap();
    assert_eq!(log.lines().count(), 1);
    assert!(!log.contains(value));
    assert!(log.contains("SignIn"));
    // An entry of the cell the store does not hold reveals nothing and journals nothing.
    let absent = reference(
        Memory::VAULT,
        &crate::typed::secret::test_entry(("mailbox", "totp", "mb-fixture"), 2),
    );
    assert_eq!(
        reveal(
            &journal,
            &store,
            &absent,
            cell,
            Surface::SignIn,
            124,
            |_| -> bool { panic!("must not reveal") }
        )
        .unwrap(),
        None
    );
    assert_eq!(std::fs::read_to_string(&journal.path).unwrap(), log);
    // Another row's entry, held by the store, is refused before the store is asked.
    store
        .put(
            "alab-mailbox-totp-mb-other",
            &Secret::new("other-row-value".into()),
        )
        .unwrap();
    for other in [
        reference(Memory::VAULT, "alab-mailbox-totp-mb-other"),
        reference("other-vault", entry),
    ] {
        assert!(
            reveal(
                &journal,
                &store,
                &other,
                cell,
                Surface::SignIn,
                126,
                |_| -> bool { panic!("must not reveal") }
            )
            .is_err()
        );
    }
    assert_eq!(std::fs::read_to_string(&journal.path).unwrap(), log);
    // A journal that cannot be written reveals nothing.
    let blocked = Journal {
        path: dir.path().to_owned(),
    };
    assert!(
        reveal(
            &blocked,
            &store,
            &named,
            cell,
            Surface::SignIn,
            125,
            |_| -> bool { panic!("must not reveal") }
        )
        .is_err()
    );
}
