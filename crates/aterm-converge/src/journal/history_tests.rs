// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{FileJournal, Journal};

#[test]
fn unrelated_changes_keep_the_stamp_and_unindexed_bytes_break_it() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut journal = FileJournal::under(scratch.path());
    journal.before_change("setting", "a", "b", None).unwrap();
    let path = journal.path();
    let stamp = || FileJournal::history_stamp_at(&path, |id| id == "setting").unwrap();
    let before = stamp();
    journal
        .before_change("sync", "private value", "other value", None)
        .unwrap();
    journal.failed("setting", "no effect").unwrap();
    assert_eq!(stamp(), before);
    let bytes = std::fs::read(journal.path().with_file_name(FILE)).unwrap();
    assert!(!String::from_utf8(bytes).unwrap().contains("private value"));
    journal.before_change("setting", "b", "a", None).unwrap();
    assert_ne!(
        stamp(),
        before,
        "a change back to the same value is still a change"
    );

    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .append(true)
        .open(journal.path())
        .unwrap()
        .write_all(b"legacy bytes\n")
        .unwrap();
    assert!(
        History::read(&journal.path()).is_none(),
        "an older writer cannot leave a valid witness"
    );
    journal.before_change("sync", "a", "b", None).unwrap();
    assert!(
        !stamp().same_history(before),
        "untracked bytes start a new incarnation"
    );
}

#[test]
fn damaged_and_oversized_history_is_never_published_as_valid() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut journal = FileJournal::under(scratch.path());
    journal.before_change("setting", "a", "b", None).unwrap();
    let path = journal.path().with_file_name(FILE);
    let before = std::fs::read(&path).unwrap();
    let mut history = History::read(&journal.path()).unwrap();
    history.note(&"x".repeat(BOUND as usize)).unwrap();
    assert!(history.publish(&journal.path()).is_err());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "the bounded writer leaves the old evidence whole"
    );
    std::fs::write(&path, b"{broken").unwrap();
    assert!(History::read(&journal.path()).is_none());
    journal.failed("setting", "no change").unwrap();
    assert!(
        History::read(&journal.path()).is_some(),
        "a fresh incarnation repairs unreadable history"
    );
}

/// Identity and length alone are insufficient after inode reuse. A different final nonce
/// invalidates a witness even when an index is forged to match the new file's metadata.
#[test]
fn a_replacement_journal_cannot_reuse_an_old_index() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let mut journal = FileJournal::under(scratch.path());
    journal.before_change("setting", "a", "b", None).unwrap();
    let mut old = History::read(&journal.path()).unwrap();
    let held = std::fs::File::open(journal.path()).unwrap();
    std::fs::rename(journal.path(), journal.path().with_extension("old")).unwrap();
    journal.before_change("setting", "a", "b", None).unwrap();
    let meta = std::fs::metadata(journal.path()).unwrap();
    old.device = meta.dev();
    old.inode = meta.ino();
    old.len = meta.len();
    std::fs::write(
        journal.path().with_file_name(FILE),
        aterm_json::to_vec(&old).unwrap(),
    )
    .unwrap();
    assert!(
        History::read(&journal.path()).is_none(),
        "a new nonce cannot satisfy the old witness"
    );
    drop(held);
}

#[test]
fn a_checkpoint_cannot_hide_change_fields_or_malformed_metadata() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let journal = FileJournal::under(scratch.path());
    journal.prepare_history().unwrap();
    let bytes = std::fs::read(journal.path()).unwrap();
    assert!(FileJournal::is_history_checkpoint(&bytes));
    let value: aterm_json::Value = aterm_json::from_slice(&bytes).unwrap();
    for (key, bad) in [
        ("id", aterm_json::json!("setting")),
        ("from", aterm_json::json!("a")),
        ("to", aterm_json::json!("b")),
        ("nonce", aterm_json::json!("invalid")),
        ("time", aterm_json::json!("not a timestamp")),
        ("checkpoint", aterm_json::json!(false)),
    ] {
        let mut changed = value.clone();
        changed.as_object_mut().unwrap().insert(key.to_owned(), bad);
        assert!(
            !FileJournal::is_history_checkpoint(&aterm_json::to_vec(&changed).unwrap()),
            "{key}"
        );
    }
}
