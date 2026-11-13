// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `state.toml` is replaced when what it holds changes, not at every round of the vault's
//! subscriber: the moment the vault last answered has a one-minute grain ([`heard_now`]), a
//! state saved with the text its file holds leaves the file ([`save_state`]), and what watches
//! this Mac's standing watches the trusted log's own files ([`Paths::standing_files`]).

use super::*;
use crate::membership_log::encode_records;
use crate::membership_log::store::classified_tests::chain_founded_at;

fn scratch() -> (aterm_tempfile::TempDir, Paths) {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(&root.path().join("mac"));
    (root, paths)
}

/// A state saved again as it is leaves its file as it is (the same inode and times), and one
/// that names another moment replaces it. Each round of the subscriber saved the state it had
/// just read (a new 3.7 KB file about every 28 s on a lab Mac, 2026-11-11).
/// NEGATIVE CONTROL (2026-11-11): with `save_state` writing by `fsx::write_atomic`, the second
/// save leaves another file (`the same state: the same file`).
#[test]
fn a_state_saved_as_it_is_leaves_its_file() {
    let (_root, paths) = scratch();
    let _lock = StateLock::take(&paths).unwrap();
    let mut state = State {
        seq: 7,
        heard: 1_000,
        membership: Stored::In,
        ..State::default()
    };
    save_state(&paths, &state).unwrap();
    let saved = fsx::stamp(&paths.state_file()).unwrap();
    save_state(&paths, &state).unwrap();
    save_state(&paths, &load_state(&paths).unwrap()).unwrap();
    assert_eq!(
        fsx::stamp(&paths.state_file()),
        Some(saved),
        "the same state: the same file"
    );

    state.heard = 1_060;
    save_state(&paths, &state).unwrap();
    assert_ne!(fsx::stamp(&paths.state_file()), Some(saved));
    assert_eq!(load_state(&paths).unwrap().heard, 1_060);
}

/// The moment the vault last answered moves only once it is a minute old: an answer within
/// [`HEARD_GRAIN_SECS`] of the one the state names keeps that one, so the state it is saved in
/// is the state on file.
/// NEGATIVE CONTROL (2026-11-11): with `heard_at` answering `now` whatever it is given, an
/// answer 59 s after the last moves it (`within the grain`).
#[test]
fn the_vault_is_heard_again_only_a_minute_after_it_was_last_heard() {
    const NOW: u64 = 1_800_000_000;
    let within = NOW - (HEARD_GRAIN_SECS - 1);
    assert_eq!(heard_at(NOW, within), within, "within the grain");
    assert_eq!(heard_at(NOW, NOW), NOW, "heard this second");
    assert_eq!(
        heard_at(NOW, NOW - HEARD_GRAIN_SECS),
        NOW,
        "a minute old: heard now"
    );
    assert_eq!(heard_at(NOW, 0), NOW, "never heard: heard now");
    assert_eq!(
        heard_at(NOW, NOW + 1),
        NOW,
        "a moment after now (the clock was set back) is heard now"
    );
    assert_eq!(heard_at(0, 0), 0, "a clock that cannot be read");
    assert_eq!(heard_at(0, NOW), 0);
}

/// What watches this Mac's standing watches the trusted log's files with the source, the
/// state, the identity and the Allowed: a record that arrives is put on the end of the records
/// file, which changes that file alone (no entry of the vault folder, and not the state).
/// NEGATIVE CONTROL (2026-11-11): with `standing_files` naming the first four files alone, no
/// watched file moved at the append (`the append is seen at a watched file`).
#[test]
fn the_standing_files_show_a_record_put_on_the_end_of_the_records_file() {
    let (_root, paths) = scratch();
    // A chain of this test's own: its records file is no other test's in the records cache.
    let c = chain_founded_at(7_005);
    save_records(&paths, &encode_records([&c.genesis, &c.first_by_m1])).unwrap();
    let files = paths.standing_files();
    assert_eq!(files[0], paths.source_file(), "the source comes first");
    assert!(files.contains(&paths.state_file()));
    let stamps = || {
        files
            .iter()
            .map(|file| fsx::stamp(file))
            .collect::<Vec<_>>()
    };
    let folder = fsx::stamp(&paths.dir);
    let before = stamps();
    assert_eq!(save_records(&paths, c.admit.bytes()).unwrap(), 1);
    assert_ne!(stamps(), before, "the append is seen at a watched file");
    assert_eq!(
        fsx::stamp(&paths.dir).map(|stamp| stamp.mtime_ns),
        folder.map(|stamp| stamp.mtime_ns),
        "the append changed no entry of the vault folder"
    );
}
