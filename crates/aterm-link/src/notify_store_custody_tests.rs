// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native directory ownership and the check-to-write store replacement boundary.

use super::*;
use std::fs::File;

struct DirectoryLock(File);

impl DirectoryLock {
    fn take(file: File) -> Result<Self, std::fs::TryLockError> {
        #[expect(
            clippy::disallowed_methods,
            reason = "this native control owns the descriptor and releases LOCK_UN in Drop"
        )]
        file.try_lock()?;
        Ok(Self(file))
    }
}

impl Drop for DirectoryLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

const PROBE_ROOT: &str = "ATERM_NOTIFY_DIRECTORY_PROBE_ROOT";
const PROBE_ROLE: &str = "ATERM_NOTIFY_DIRECTORY_PROBE_ROLE";

#[test]
fn directory_probe() {
    let Some(root) = std::env::var_os(PROBE_ROOT) else {
        return;
    };
    let role = std::env::var(PROBE_ROLE).unwrap();
    let parts: Vec<_> = role.split(':').collect();
    assert_eq!(parts.len(), 3);
    let expected = (
        parts[1].parse::<u64>().unwrap(),
        parts[2].parse::<u64>().unwrap(),
    );
    let file = File::open(root).unwrap();
    let metadata = file.metadata().unwrap();
    assert!(metadata.is_dir());
    assert_eq!(file_id(&metadata), expected);
    match (parts[0], DirectoryLock::take(file)) {
        ("held", Err(std::fs::TryLockError::WouldBlock)) => {}
        ("free", Ok(held)) => assert_eq!(file_id(&held.0.metadata().unwrap()), expected),
        (role, _) => panic!("the independent directory lock did not answer {role}"),
    }
    println!("\ndirectory-lock-probe-complete");
}

fn probe(root: &Path, held: bool, identity: (u64, u64)) {
    let module = module_path!().split_once("::").unwrap().1;
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            &format!("{module}::directory_probe"),
            "--nocapture",
        ])
        .env(PROBE_ROOT, root)
        .env(
            PROBE_ROLE,
            format!(
                "{}:{}:{}",
                if held { "held" } else { "free" },
                identity.0,
                identity.1
            ),
        );
    let output =
        aterm_uds::deadline::output_within(&mut command, aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line == "directory-lock-probe-complete"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn a_directory_lock_excludes_an_independent_child_after_pid_file_replacement() {
    let world = aterm_tempfile::tempdir().unwrap();
    let root = world.path().join("store");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join(LOCK), b"first").unwrap();
    let held = DirectoryLock::take(File::open(&root).unwrap()).expect("native directory flock");
    let identity = file_id(&held.0.metadata().unwrap());
    probe(&root, true, identity);
    std::fs::remove_file(root.join(LOCK)).unwrap();
    std::fs::write(root.join(LOCK), b"replacement").unwrap();
    probe(&root, true, identity);
    drop(held);
    probe(&root, false, identity);
}

fn cursor_step(off: u64) -> [journal::Step; 1] {
    [journal::Step {
        cursor: vec![("k".to_string(), off)],
        ..journal::Step::default()
    }]
}

/// Every file under `root`, by its path there, with its bytes.
fn tree(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut files = std::collections::BTreeMap::new();
    let mut walk = vec![root.to_path_buf()];
    while let Some(directory) = walk.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                files.insert(path.strip_prefix(root).unwrap().to_path_buf(), bytes);
            }
        }
    }
    files
}

/// A store write that falls after [`Hold::check`] passed: a journal rewrite, an append to a
/// `journal.steps` opened again, and another log's store taken up (its `logs/<id>/`).
#[derive(Clone, Copy, Debug)]
enum Late {
    Rewrite,
    Append,
    LogStore,
}

/// `notify/` removed and taken by a second notifier between the first notifier's
/// [`Hold::check`] and each kind of write it makes next. The write reaches only the removed
/// directory, the second store keeps its bytes, and the first notifier's next check refuses.
/// The mutant that resolves each name by the store's path ([`directory::BY_PATH`]), as a build
/// before the held descriptor did, lands every one of these writes in the second store.
#[test]
fn a_write_after_the_store_is_removed_and_taken_again_reaches_only_the_removed_directory() {
    let log = |n: u8| astream_broker::LogIdentity {
        id: [n; 16],
        generation: 0,
    };
    for late in [Late::Rewrite, Late::Append, Late::LogStore] {
        for by_path in [false, true] {
            let world = aterm_tempfile::tempdir().unwrap();
            let state = world.path().to_str().unwrap();
            let first = Store::open(state).unwrap();
            let mut journal = first.load().unwrap();
            journal
                .write(&cursor_step(1), Flush::Ordered, None)
                .unwrap();
            first.for_log(Some(log(1)), None).unwrap();
            first.access.0.check().unwrap();
            let removed = world.path().join("removed");
            std::fs::rename(&first.root, &removed).unwrap();
            let second = Store::open(state).expect("the new notify/ is a store of its own");
            second
                .load()
                .unwrap()
                .write(&cursor_step(2), Flush::Ordered, None)
                .unwrap();
            let (second_before, removed_before) = (tree(&second.root), tree(&removed));
            directory::BY_PATH.with(|by| by.set(by_path));
            let wrote = match late {
                Late::Rewrite => {
                    journal.rewrite_always = true;
                    journal.write(&cursor_step(3), Flush::Ordered, None)
                }
                Late::Append => first
                    .load()
                    .and_then(|mut again| again.write(&cursor_step(3), Flush::Ordered, None)),
                Late::LogStore => first.for_log(Some(log(2)), None).map(drop),
            };
            directory::BY_PATH.with(|by| by.set(false));
            wrote.unwrap();
            let (second_after, removed_after) = (tree(&second.root), tree(&removed));
            if by_path {
                assert_ne!(
                    second_after, second_before,
                    "{late:?}: the mutant's write lands in the second store"
                );
                continue;
            }
            assert_eq!(
                second_after, second_before,
                "{late:?}: the second store keeps its bytes"
            );
            assert_ne!(
                removed_after, removed_before,
                "{late:?}: the write reaches the removed directory"
            );
            match late {
                Late::Rewrite | Late::Append => assert_eq!(
                    Store::<ReadOnly>::existing(removed.clone())
                        .unwrap()
                        .read_cursor("k"),
                    Some(3),
                    "{late:?}"
                ),
                Late::LogStore => assert!(
                    removed.join("logs").join(log(2).to_string()).is_dir(),
                    "{late:?}: the other log's store is made in the removed directory"
                ),
            }
            assert_eq!(
                first.access.0.check().unwrap_err().kind(),
                io::ErrorKind::Other,
                "{late:?}: the first notifier stops at its next check: notify/ names another directory"
            );
        }
    }
}

#[test]
fn replacing_the_pid_file_cannot_admit_another_owner_of_one_directory() {
    let world = aterm_tempfile::tempdir().unwrap();
    let state = world.path().to_str().unwrap();
    let first = Store::open(state).unwrap();
    std::fs::remove_file(first.root.join(LOCK)).unwrap();
    let refused = Store::open(state).expect_err("the original directory owner is still held");
    assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
}

#[test]
fn writer_clones_and_their_journal_keep_one_original_directory_owner() {
    let world = aterm_tempfile::tempdir().unwrap();
    let state = world.path().to_str().unwrap();
    let first = Store::open(state).unwrap();
    let read_only = first.read_only();
    let clone = first.clone();
    let journal = first.load().unwrap();
    let identity = file_id(&first.directory.file.metadata().unwrap());
    drop(first);
    assert_eq!(
        Store::open(state).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(clone);
    assert_eq!(
        Store::open(state).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(journal);
    let successor = Store::open(state).unwrap();
    assert_eq!(
        file_id(&successor.directory.file.metadata().unwrap()),
        identity
    );
    assert_eq!(
        file_id(&read_only.directory.file.metadata().unwrap()),
        identity
    );
}

fn replace_temporary(
    store: &Store,
    temporary: &directory::Temporary,
    directory: bool,
    tag: &str,
) -> PathBuf {
    let path = store.root.join(&temporary.name);
    let original = store.root.join(format!("original-{tag}"));
    std::fs::rename(&path, &original).unwrap();
    if directory {
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("foreign"), b"keep").unwrap();
    } else {
        std::fs::write(&path, b"keep").unwrap();
    }
    path
}

#[test]
fn publishing_a_temporary_never_publishes_its_observed_replacement() {
    for directory in [false, true] {
        let world = aterm_tempfile::tempdir().unwrap();
        let store = Store::open(world.path().to_str().unwrap()).unwrap();
        let temporary = store.directory.temporary("publication", directory).unwrap();
        let foreign = replace_temporary(&store, &temporary, directory, "publication");
        assert!(temporary
            .publish(&store.directory, std::ffi::OsStr::new("published"))
            .is_err());
        assert!(!store.root.join("published").exists());
        assert_eq!(
            std::fs::read(if directory {
                foreign.join("foreign")
            } else {
                foreign
            })
            .unwrap(),
            b"keep"
        );
    }
}

#[test]
fn dropping_a_temporary_preserves_its_observed_replacement() {
    for directory in [false, true] {
        let world = aterm_tempfile::tempdir().unwrap();
        let store = Store::open(world.path().to_str().unwrap()).unwrap();
        let temporary = store.directory.temporary("cleanup", directory).unwrap();
        let foreign = replace_temporary(&store, &temporary, directory, "cleanup");
        drop(temporary);
        assert_eq!(
            std::fs::read(if directory {
                foreign.join("foreign")
            } else {
                foreign
            })
            .unwrap(),
            b"keep"
        );
    }
}
