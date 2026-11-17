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

/// What a value-free look asks of the journal follows what an append requires of it, with
/// nothing written: a journal not made yet is ready and stays unmade, the file a reveal made
/// is ready, and one that is not this user's private regular file (other users may read it;
/// it has a second name; it is a link) is refused by both.
/// NEGATIVE CONTROL: the same journal is ready again once its mode is private and its second
/// name is gone, so the refusals are of those two states.
#[cfg(unix)]
#[test]
fn a_look_holds_the_journal_to_what_an_append_requires_and_writes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let dir = aterm_tempfile::tempdir().unwrap();
    let folder = dir.path().join("secret-reveals");
    let journal = Journal::at(&folder);
    let file = folder.join("reveals.jsonl");
    let cell = Cell {
        table: "alab/registry.3",
        collection: "mailbox",
        row: "mb-fixture",
        column: "totp",
    };
    let record = |journal: &Journal| record_reveal(journal, cell, Surface::SignIn, 1);

    journal.ready().expect("a journal not made yet");
    assert!(!folder.exists(), "the look made nothing");
    record(&journal).expect("the first reveal makes it");
    journal.ready().expect("the file a reveal made");
    let lines = || std::fs::read_to_string(&file).unwrap().lines().count();
    assert_eq!(lines(), 1, "the look appended nothing");

    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(journal.ready().is_err(), "readable by others");
    assert!(record(&journal).is_err());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    journal.ready().expect("CONTROL: private again");

    let second = folder.join("second-name");
    std::fs::hard_link(&file, &second).unwrap();
    assert!(journal.ready().is_err(), "a second name");
    assert!(record(&journal).is_err());
    std::fs::remove_file(&second).unwrap();
    journal.ready().expect("CONTROL: one name again");
    assert_eq!(lines(), 1, "no refused reveal and no look appended");

    let linked = Journal::at(&dir.path().join("linked"));
    std::fs::create_dir(dir.path().join("linked")).unwrap();
    std::os::unix::fs::symlink(&file, dir.path().join("linked").join("reveals.jsonl")).unwrap();
    assert!(linked.ready().is_err(), "a link");
    assert!(record(&linked).is_err());
    assert_eq!(lines(), 1);
}
