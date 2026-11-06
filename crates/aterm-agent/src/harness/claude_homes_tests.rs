// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The launch notes of Claude homes ([`super`]) on a scratch harness state.

use super::{KEEP_S, NOTES_MAX, launched, note, read};
use std::path::{Path, PathBuf};

const NOW: i64 = 1_793_000_000;

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::TempDir::new().expect("a scratch dir")
}

/// `n` Claude homes that are there, under `dir`.
fn homes(dir: &Path, n: usize) -> Vec<PathBuf> {
    (0..n)
        .map(|i| {
            let home = dir.join("homes").join(i.to_string());
            std::fs::create_dir_all(&home).expect("a home");
            home
        })
        .collect()
}

/// A launch names the home its `CLAUDE_CONFIG_DIR` sets, a relative one under the launch's
/// working directory; its own default home, and a launch that names no home or has no `HOME`
/// to judge a home by, are not noted.
#[test]
fn a_launch_names_its_own_home_and_never_its_default() {
    let dir = scratch();
    let user = dir.path().join("user");
    let user_s = user.to_str().expect("utf-8");
    let work = user.join("work");
    let custom = user.join(".claude-work");
    let custom_s = custom.to_str().expect("utf-8");
    assert_eq!(
        launched((Some(custom_s), Some(user_s)), Some(&work), Some(&user)),
        Some(custom.clone())
    );
    assert_eq!(
        launched(
            (Some(".claude-here"), Some(user_s)),
            Some(&work),
            Some(&user)
        ),
        Some(work.join(".claude-here")),
        "a relative home is the launch's working directory's"
    );
    assert_eq!(
        launched((None, Some(user_s)), Some(&work), Some(&user)),
        None,
        "the default home"
    );
    assert_eq!(launched((None, None), None, Some(&user)), None);
    assert_eq!(
        launched((Some(custom_s), None), Some(&work), None),
        None,
        "no HOME to judge the home by"
    );
}

/// A home under a folder the census never looks into (macOS's Documents) is not noted.
#[cfg(target_os = "macos")]
#[test]
fn a_home_in_a_private_folder_is_never_noted() {
    let dir = scratch();
    let user = dir.path().join("user");
    let private = user.join("Documents").join(".claude");
    assert_eq!(
        launched(
            (
                Some(private.to_str().expect("utf-8")),
                Some(user.to_str().expect("utf-8"))
            ),
            None,
            Some(&user)
        ),
        None
    );
}

/// A note reads back with its time, a later launch of the same home moves it, the newest come
/// first, and a note older than [`KEEP_S`] or of a home whose folder is gone is removed by the
/// read; a file that is no note (another name, another home's name, no time) is skipped.
#[test]
fn a_note_reads_back_while_it_is_kept_and_its_home_is_there() {
    let dir = scratch();
    let state = dir.path().join("harness");
    let [a, b, old, gone] = <[PathBuf; 4]>::try_from(homes(dir.path(), 4)).expect("four");
    note(&state, &a, NOW - 100).expect("a note");
    note(&state, &b, NOW - 50).expect("a note");
    note(&state, &old, NOW - KEEP_S - 1).expect("a note");
    note(&state, &gone, NOW - 20).expect("a note");
    note(&state, &a, NOW - 10).expect("a later note of the same home");
    std::fs::remove_dir(&gone).expect("the home goes");
    let notes = state.join(super::DIR);
    let c = a.to_str().expect("utf-8");
    std::fs::write(notes.join("not-a-note"), format!("1\n{c}")).expect("a stray file");
    std::fs::write(notes.join(super::name_of("/elsewhere")), format!("1\n{c}"))
        .expect("a misnamed note");
    std::fs::write(notes.join(super::name_of("/no-time")), "/no-time").expect("no time");
    assert_eq!(read(&state, NOW), [(a, NOW - 10), (b, NOW - 50)]);
    for removed in [&old, &gone] {
        let name = super::name_of(removed.to_str().expect("utf-8"));
        assert!(!notes.join(name).exists(), "{removed:?}'s note was removed");
    }
}

/// One read takes at most [`NOTES_MAX`] notes, the newest.
#[test]
fn a_read_takes_the_newest_notes_up_to_its_bound() {
    let dir = scratch();
    let state = dir.path().join("harness");
    let all = homes(dir.path(), NOTES_MAX + 3);
    for (n, home) in all.iter().enumerate() {
        note(&state, home, NOW - i64::try_from(n).expect("few")).expect("a note");
    }
    let notes = read(&state, NOW);
    assert_eq!(notes.len(), NOTES_MAX);
    assert_eq!(notes[0], (all[0].clone(), NOW));
    assert!(
        notes
            .iter()
            .all(|(_, at)| *at > NOW - i64::try_from(NOTES_MAX).expect("few")),
        "only the newest"
    );
}
