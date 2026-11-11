// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;
use std::{collections::BTreeMap, io::Write, os::unix::fs::PermissionsExt};

thread_local! {
    pub(super) static MUTANT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    static REPLACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(super) fn after_lock(guard: &ReadGuard) {
    if REPLACE.replace(false) {
        replace(guard);
    }
}

fn replace(guard: &ReadGuard) {
    std::fs::rename(guard.dir.join(&guard.name), guard.dir.join("old-lock")).unwrap();
    write_private_durable(&guard.dir, &guard.name, b"").unwrap();
}

fn snapshot(dir: &Path) -> BTreeMap<String, (u32, Vec<u8>)> {
    if !dir.exists() {
        return BTreeMap::new();
    }
    let mut files: BTreeMap<_, _> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_str().unwrap().to_owned(),
                (
                    std::fs::symlink_metadata(&path)
                        .unwrap()
                        .permissions()
                        .mode(),
                    std::fs::read(&path).unwrap(),
                ),
            )
        })
        .collect();
    files.insert(
        ".".into(),
        (
            std::fs::metadata(dir).unwrap().permissions().mode(),
            Vec::new(),
        ),
    );
    files
}

fn locked(dir: &Path) -> bool {
    let Ok((file, _)) = private_file::open_private(dir, "test.lock") else {
        return false;
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the test probe is explicitly unlocked by WriterLock before close"
    )]
    match file.try_lock() {
        Ok(()) => {
            drop(WriterLock(file));
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(error) => panic!("lock probe failed: {error}"),
    }
}

fn check(scenario: i64, mutant: u8) -> State {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path().join("journal");
    let mut owner = if scenario == 4 {
        None
    } else {
        Some(super::super::tests::open(&dir))
    };
    if scenario != 2 {
        drop(owner.take());
    }
    match scenario {
        1 => std::fs::remove_file(dir.join("test.lock")).unwrap(),
        5 => {
            std::fs::rename(dir.join("test.lock"), dir.join("link-target")).unwrap();
            std::os::unix::fs::symlink("link-target", dir.join("test.lock")).unwrap();
        }
        6 => std::fs::set_permissions(
            dir.join("test.lock"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap(),
        9 => {
            let mut file = OpenOptions::new()
                .append(true)
                .open(dir.join("test.astream"))
                .unwrap();
            file.write_all(b"unacknowledged tail").unwrap();
        }
        10 => std::fs::hard_link(dir.join("test.lock"), dir.join("another-link")).unwrap(),
        11 => std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap(),
        _ => {}
    }
    let mut before = snapshot(&dir);
    MUTANT.set(mutant);
    REPLACE.set(scenario == 3);
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            MUTANT.set(0);
            REPLACE.set(false);
        }
    }
    let _reset = Reset;
    let guard = ReadGuard::take(&dir, "test", Instant::now());
    if mutant == 0 {
        match scenario {
            1 | 4 => assert_eq!(
                guard.as_ref().err().unwrap().kind(),
                io::ErrorKind::NotFound
            ),
            5 | 6 | 10 | 11 => assert_eq!(
                guard.as_ref().err().unwrap().kind(),
                io::ErrorKind::PermissionDenied
            ),
            2 => assert_eq!(
                guard.as_ref().err().unwrap().kind(),
                io::ErrorKind::WouldBlock
            ),
            _ => {}
        }
    }
    // Only the test environment changed these two names during acquisition.
    if scenario == 3 {
        before.insert("old-lock".into(), before["test.lock"].clone());
    }
    let admitted = guard.is_ok();
    let held = !admitted || locked(&dir);
    if scenario == 8 {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = guard.unwrap();
            panic!("bounded callback unwind");
        }));
        assert!(result.is_err());
    } else if scenario == 7 {
        let callback = || -> io::Result<()> {
            let _guard = guard?;
            Err(io::Error::other("bounded callback refused"))
        };
        assert!(callback().is_err());
    } else {
        drop(guard);
    }
    let unchanged = before == snapshot(&dir);
    drop(owner);
    State::from([
        ("scenario", scenario),
        ("done", 1),
        ("admitted", i64::from(admitted)),
        ("held", i64::from(held)),
        ("unchanged", i64::from(unchanged)),
        ("released", i64::from(!locked(&dir))),
    ])
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    match action {
        "Case" => {
            let mut next = state.clone();
            next.insert("scenario", state["scenario"] + 1);
            next
        }
        "Take" => check(state["scenario"], 0),
        _ => panic!("unknown read custody action: {action}"),
    }
}

#[test]
fn retained_read_guard_refines_writer_exclusion_without_source_mutation() {
    let model = aterm_models_swarm::journal_read_guard_model();
    aterm_spec::interp::prove_and_catch(&model);
    let mut cases = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
                cases += usize::from(action.name == "Take");
            }
        }
    }
    assert_eq!(cases, 12);
    for (mutant, scenario, law) in [
        (1, 0, "WriterExcluded"),
        (1, 2, "ExistingPrivateCustody"),
        (2, 3, "ExistingPrivateCustody"),
    ] {
        assert!(
            !model.check_invariant(law, &check(scenario, mutant)),
            "{law}"
        );
    }
}

#[test]
fn mutable_open_is_not_a_non_mutating_read_guard() {
    let dir = aterm_tempfile::tempdir().unwrap();
    drop(super::super::tests::open(dir.path()));
    let mut file = OpenOptions::new()
        .append(true)
        .open(dir.path().join("test.astream"))
        .unwrap();
    file.write_all(b"unacknowledged tail").unwrap();
    drop(file);
    let before = snapshot(dir.path());
    let guard = ReadGuard::take(dir.path(), "test", Instant::now()).unwrap();
    assert_eq!(snapshot(dir.path()), before);
    drop(guard);
    drop(super::super::tests::open(dir.path()));
    let mut actual = check(9, 0);
    actual.insert("unchanged", i64::from(snapshot(dir.path()) == before));
    assert!(
        !aterm_models_swarm::journal_read_guard_model()
            .check_invariant("NoSourceMutation", &actual)
    );
}

#[test]
fn replaced_guard_is_refused_without_reacquisition() {
    let dir = aterm_tempfile::tempdir().unwrap();
    drop(super::super::tests::open(dir.path()));
    let guard = ReadGuard::take(dir.path(), "test", Instant::now()).unwrap();
    replace(&guard);
    assert!(guard.check().is_err());
    assert!(!locked(dir.path()), "checking never locks the replacement");
    drop(guard);
}

#[test]
fn real_journal_writer_waits_until_the_read_guard_drops() {
    let dir = aterm_tempfile::tempdir().unwrap();
    drop(super::super::tests::open(dir.path()));
    let guard = ReadGuard::take(dir.path(), "test", Instant::now()).unwrap();
    let blocked = Log::open(
        dir.path(),
        "test",
        b"pinned",
        true,
        super::super::tests::limits(),
    )
    .err()
    .unwrap();
    assert_eq!(blocked.kind(), io::ErrorKind::WouldBlock);
    guard.check().unwrap();
    drop(guard);
    drop(super::super::tests::open(dir.path()));
}

#[test]
fn invalid_names_do_not_touch_the_directory() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let before = snapshot(dir.path());
    for name in ["", ".", "..", "a/b", "name.lock", "🦀"] {
        assert!(ReadGuard::take(dir.path(), name, Instant::now()).is_err());
    }
    assert_eq!(snapshot(dir.path()), before);
}
