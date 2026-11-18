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
        cap: CAP,
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

/// The cell every bound test records.
#[cfg(unix)]
const BOUND_CELL: Cell<'static> = Cell {
    table: "alab/registry.3",
    collection: "mailbox",
    row: "mb-fixture",
    column: "totp",
};

/// The `when_ms` of each record in `file`, oldest first; none where there is no file.
#[cfg(unix)]
fn whens(file: &std::path::Path) -> Vec<u64> {
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .map(|line| {
            aterm_json::from_str::<aterm_json::Value>(line).unwrap()["when_ms"]
                .as_u64()
                .unwrap()
        })
        .collect()
}

/// The journal keeps the newest records in two files of its bound: a reveal that would take the
/// file past the bound retires it to the one earlier file, replacing the one retired before, and
/// starts the journal anew. Across many times the bound the two files stay within twice the
/// bound and one record, and what they hold is the newest records in order with none missing.
/// NEGATIVE CONTROL: the same reveals under a bound they never meet leave one file past twice the
/// small bound and no earlier file, so the bound is what retires.
#[cfg(unix)]
#[test]
fn the_journal_keeps_the_newest_records_in_two_files_of_its_bound() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let cap = 1024;
    let journal = Journal {
        cap,
        ..Journal::at(dir.path())
    };
    let (file, earlier) = (dir.path().join("reveals.jsonl"), dir.path().join(RETIRED));
    let len = |path: &std::path::Path| std::fs::metadata(path).map_or(0, |meta| meta.len());
    let mut widest = 0;
    for when in 0..200 {
        record_reveal(&journal, BOUND_CELL, Surface::SignIn, when).unwrap();
        widest = widest.max(len(&file) + len(&earlier));
        assert!(
            len(&file) <= cap,
            "the journal's own file stays in its bound"
        );
    }
    let one = len(&file) / whens(&file).len() as u64;
    assert!(widest <= 2 * cap, "two files of the bound: {widest}");
    assert!(widest > cap, "and the earlier file is kept: {widest}");
    let kept = [whens(&earlier), whens(&file)].concat();
    let first = 200 - kept.len() as u64;
    assert_eq!(
        kept,
        (first..200).collect::<Vec<_>>(),
        "the newest, in order"
    );
    assert!(
        kept.len() as u64 >= cap / one,
        "at least one bound of records"
    );
    use std::os::unix::fs::PermissionsExt;
    for path in [&file, &earlier] {
        let mode = std::fs::metadata(path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{}", path.display());
    }

    let control = aterm_tempfile::tempdir().unwrap();
    let unbounded = Journal::at(control.path());
    for when in 0..200 {
        record_reveal(&unbounded, BOUND_CELL, Surface::SignIn, when).unwrap();
    }
    assert!(len(&control.path().join("reveals.jsonl")) > 2 * cap);
    assert!(!control.path().join(RETIRED).exists());
}

/// Only the file a reveal found at its bound is retired. Where another process retired that file
/// and started the journal anew between this reveal's open and its turn at the lock, the path
/// names the new file: it stays where it is, and the file retired before is not replaced by it.
/// A lock held by another reveal past the wait retires nothing, and the record still goes in.
/// A retirement that fails fails its reveal.
/// NEGATIVE CONTROL: with the path still naming the held file, the same call retires it.
#[cfg(unix)]
#[test]
fn a_reveal_retires_only_the_file_it_found_at_its_bound() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let journal = Journal {
        cap: 1,
        ..Journal::at(dir.path())
    };
    let (file, earlier) = (dir.path().join("reveals.jsonl"), dir.path().join(RETIRED));
    std::fs::write(&file, "full\n").unwrap();
    let held = std::fs::File::open(&file).unwrap();

    // Another process's retirement, then its new journal.
    std::fs::rename(&file, &earlier).unwrap();
    std::fs::write(&file, "new\n").unwrap();
    journal.retire(dir.path(), &held).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\n");
    assert_eq!(std::fs::read_to_string(&earlier).unwrap(), "full\n");

    // Its retirement with no new journal yet: nothing is there to retire.
    std::fs::remove_file(&file).unwrap();
    journal.retire(dir.path(), &held).unwrap();
    assert_eq!(std::fs::read_to_string(&earlier).unwrap(), "full\n");

    // CONTROL: the path names the held file.
    std::fs::write(&file, "mine\n").unwrap();
    let mine = std::fs::File::open(&file).unwrap();
    journal.retire(dir.path(), &mine).unwrap();
    assert!(!file.exists());
    assert_eq!(std::fs::read_to_string(&earlier).unwrap(), "mine\n");

    // A lock another reveal holds: nothing retired, the record appended over the bound.
    let lock = aterm_uds::line_log::lock(&file).unwrap();
    record_reveal(&journal, BOUND_CELL, Surface::SignIn, 1).unwrap();
    record_reveal(&journal, BOUND_CELL, Surface::SignIn, 2).unwrap();
    assert_eq!(whens(&file), [1, 2], "both in the one file, over its bound");
    assert_eq!(std::fs::read_to_string(&earlier).unwrap(), "mine\n");
    drop(lock);
    record_reveal(&journal, BOUND_CELL, Surface::SignIn, 3).unwrap();
    assert_eq!((whens(&earlier), whens(&file)), (vec![1, 2], vec![3]));

    // A retirement that cannot be made fails the reveal, and its record is not appended.
    std::fs::remove_file(&earlier).unwrap();
    std::fs::create_dir(&earlier).unwrap();
    std::fs::write(earlier.join("kept"), "").unwrap();
    assert!(record_reveal(&journal, BOUND_CELL, Surface::SignIn, 4).is_err());
    assert_eq!(
        whens(&file),
        [3],
        "no record over a bound nothing could keep"
    );
}
