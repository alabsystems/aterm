// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A member's records file takes the records a pass brings on its end ([`persist_records`]),
//! and is replaced whole wherever its bytes are not exactly the complete records the store
//! held before them: a record waiting on a parent, a torn tail, a file another writer left.

use super::*;
use crate::membership_log::encode_records;
use crate::membership_log::store::classified_tests::chain_founded_at;
use std::os::unix::fs::MetadataExt as _;

/// A scratch vault folder. Each test here founds its own chain
/// ([`chain_founded_at`] of another moment): the process's records cache knows an entry by the
/// stamps of the last few files that held its bytes, so two tests whose files held the same
/// bytes would take each other's place in it, and a store whose stamp it let go is written
/// whole.
fn scratch() -> (aterm_tempfile::TempDir, Paths) {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(&root.path().join("mac"));
    (root, paths)
}

fn inode(paths: &Paths) -> u64 {
    std::fs::symlink_metadata(paths.records_data_file().unwrap())
        .unwrap()
        .ino()
}

fn on_file(paths: &Paths) -> Vec<u8> {
    std::fs::read(paths.records_data_file().unwrap()).unwrap()
}

/// A save that brings a complete record to a file of complete records appends that record
/// alone: the file is the same inode, its bytes the ones it held with the record after them,
/// and a read takes every record. Before, each such save wrote the whole store to a temporary
/// file and renamed it over (4.8 MB a record on a lab Mac, 2026-11-11). A save of records the
/// file holds touches nothing.
/// NEGATIVE CONTROL (2026-11-11): with `persist_records` replacing the file whole whatever it
/// holds, the file after the second save is another inode (`appended in place`).
#[test]
fn a_complete_record_goes_on_the_end_of_the_records_file() {
    let (_root, paths) = scratch();
    let c = chain_founded_at(7_001);
    assert_eq!(
        save_records(&paths, &encode_records([&c.genesis, &c.first_by_m1])).unwrap(),
        2
    );
    let (first, held) = (inode(&paths), on_file(&paths));
    assert_eq!(held, encode_records([&c.genesis, &c.first_by_m1]));

    assert_eq!(save_records(&paths, c.admit.bytes()).unwrap(), 1);
    assert_eq!(inode(&paths), first, "appended in place");
    assert_eq!(on_file(&paths), [held, c.admit.bytes().to_vec()].concat());
    assert_eq!(save_records(&paths, c.by_m2.bytes()).unwrap(), 1);
    assert_eq!(inode(&paths), first, "and again onto what the append left");
    let all = encode_records([&c.genesis, &c.first_by_m1, &c.admit, &c.by_m2]);
    assert_eq!(on_file(&paths), all);
    let read = load_records(&paths).unwrap();
    assert_eq!(read.complete_inventory().len(), 4);
    assert_eq!(read.encode(), all);

    let stamp = fsx::stamp(&paths.records_data_file().unwrap());
    assert_eq!(save_records(&paths, &all).unwrap(), 0);
    assert_eq!(fsx::stamp(&paths.records_data_file().unwrap()), stamp);
}

/// A file that holds a record waiting on a parent is never appended to, nor is a store that
/// keeps one waiting appended: complete records come before waiting ones on file, so each such
/// write replaces the file whole, and the file is the store's own encoding after each. Once
/// the file holds complete records alone, the next one is appended.
/// NEGATIVE CONTROL (2026-11-11): with [`crate::membership_log::Store::extends`] not asking
/// whether either store holds a waiting record, the save of the waiting `admit` writes
/// nothing (the complete records are the file's own) and leaves the same inode (`a record
/// that waits on a parent`).
#[test]
fn a_records_file_with_a_waiting_record_is_replaced_whole() {
    let (_root, paths) = scratch();
    let c = chain_founded_at(7_002);
    save_records(&paths, c.genesis.bytes()).unwrap();
    let mut last = inode(&paths);
    let mut replaced = |why: &str| {
        let now = inode(&paths);
        assert_ne!(now, last, "{why}");
        last = now;
        assert_eq!(
            on_file(&paths),
            load_records(&paths).unwrap().encode(),
            "{why}"
        );
    };

    assert_eq!(save_records(&paths, c.admit.bytes()).unwrap(), 1);
    replaced("a record that waits on a parent");
    assert_eq!(
        load_records(&paths).unwrap().complete_inventory().len(),
        1,
        "the admit waits for the record before it"
    );
    assert_eq!(save_records(&paths, c.first_by_m1.bytes()).unwrap(), 1);
    replaced("a waiting record on file");
    assert_eq!(load_records(&paths).unwrap().complete_inventory().len(), 3);

    assert_eq!(save_records(&paths, c.by_m2.bytes()).unwrap(), 1);
    assert_eq!(inode(&paths), last, "complete records alone: appended");
    assert_eq!(on_file(&paths), load_records(&paths).unwrap().encode());
}

/// An append the disk cuts short is followed at once by a whole replacement, so the save
/// succeeds and the file holds the store and no part of a record. A file left with such a part
/// by a writer that died mid-append reads as the records before it, and the next save
/// replaces it whole rather than append after the part.
/// NEGATIVE CONTROL (2026-11-11): with [`records_cache::held_on_file`] not comparing the bytes
/// its entry holds with the file's length, the save after the torn file appends `by_m2` behind
/// the part and the file is not the store's encoding (`the torn file is replaced`).
#[test]
fn a_torn_append_is_replaced_whole_and_a_torn_file_is_never_appended_to() {
    use std::io::Write as _;

    let (_root, paths) = scratch();
    let c = chain_founded_at(7_003);
    save_records(&paths, &encode_records([&c.genesis, &c.first_by_m1])).unwrap();
    let first = inode(&paths);

    RECORD_APPENDS.with(|appends| appends.set(RecordAppends::Tear));
    let saved = save_records(&paths, c.admit.bytes());
    RECORD_APPENDS.with(|appends| appends.set(RecordAppends::Land));
    assert_eq!(saved.unwrap(), 1);
    let whole = inode(&paths);
    assert_ne!(whole, first, "the cut append was replaced whole");
    let three = encode_records([&c.genesis, &c.first_by_m1, &c.admit]);
    assert_eq!(on_file(&paths), three);

    // A writer that died mid-append: all but the last byte of a record on the end.
    let cut = &c.by_m2.bytes()[..c.by_m2.bytes().len() - 1];
    std::fs::OpenOptions::new()
        .append(true)
        .open(paths.records_data_file().unwrap())
        .unwrap()
        .write_all(cut)
        .unwrap();
    let read = load_records(&paths).unwrap();
    assert_eq!(read.encode(), three, "the records before the cut");
    assert_eq!(save_records(&paths, c.by_m2.bytes()).unwrap(), 1);
    assert_eq!(
        on_file(&paths),
        encode_records([&c.genesis, &c.first_by_m1, &c.admit, &c.by_m2]),
        "the torn file is replaced"
    );
    assert_ne!(inode(&paths), whole);
}

/// A store is appended only to the file its records were read from: one another writer
/// renamed over the path since, which no read of this process stamped, is replaced whole,
/// even where it is as long as the file that was read.
/// NEGATIVE CONTROL (2026-11-11): with [`records_cache::held_on_file`] taking an entry read
/// under any stamp, the write puts `admit` on the end of the other writer's bytes (`the file
/// is the store`).
#[test]
fn a_file_another_writer_left_is_replaced_whole() {
    let (_root, paths) = scratch();
    let c = chain_founded_at(7_004);
    save_records(&paths, &encode_records([&c.genesis, &c.first_by_m1])).unwrap();
    let _lock = StateLock::take(&paths).unwrap();
    let mut store = load_records(&paths).unwrap();
    let theirs = vec![0; on_file(&paths).len()];
    fsx::write_atomic(&paths.records_data_file().unwrap(), &theirs, 0o600).unwrap();
    let left = inode(&paths);

    assert_eq!(store.ingest(c.admit.bytes()).unwrap(), 1);
    write_records(&paths, &store).unwrap();
    assert_eq!(
        on_file(&paths),
        encode_records([&c.genesis, &c.first_by_m1, &c.admit]),
        "the file is the store"
    );
    assert_ne!(inode(&paths), left, "replaced whole");
}
