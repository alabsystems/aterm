// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::client::release::probe::model::{Repository, Subject};
use std::os::unix::fs::PermissionsExt as _;

fn seed() -> Journal {
    Journal {
        nonce: "12".repeat(16),
        subject: Subject {
            uid: fsx::euid(),
            node: "34".repeat(16),
            member: "56".repeat(16),
            writer: "bc".repeat(32),
            incarnation: "78".repeat(16),
            network: "9a".repeat(16),
            authority: "cd".repeat(32),
            genesis: "de".repeat(32),
            vault: "ef".repeat(32),
        },
        repositories: vec![Repository::new("fixture-owner".into())],
        created_ms: 1,
        updated_ms: 1,
        artifact: None,
        submission: None,
        apple_accepted: false,
        pending: None,
        cleanup_only: false,
    }
}

fn fixture() -> (aterm_tempfile::TempDir, PathBuf, Journal) {
    let dir = aterm_tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let seed = seed();
    let target = dir
        .path()
        .join(format!("aterm-release-probe-{}.noindex", seed.nonce));
    (dir, target, seed)
}

#[test]
fn a_reserved_probe_recovers_after_mkdir_or_before_first_journal() {
    let (_dir, target, seed) = fixture();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&target)
        .unwrap();
    let store = Store::open(&target, &seed.nonce, Some(&seed)).unwrap();
    assert_eq!(store.load().unwrap(), Some(seed.clone()));
    drop(store);
    // Simulate a crash after the durable reservation, before the first journal publication.
    std::fs::remove_file(target.join(FILE)).unwrap();
    let resumed = Store::open(&target, &seed.nonce, None).unwrap();
    assert_eq!(resumed.load().unwrap(), Some(seed));
}

#[test]
fn unrelated_nonempty_directories_are_not_adopted_or_changed() {
    let (_dir, target, seed) = fixture();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&target)
        .unwrap();
    let unrelated = target.join("valuable");
    std::fs::write(&unrelated, b"keep exactly").unwrap();
    assert!(Store::open(&target, &seed.nonce, Some(&seed)).is_err());
    assert_eq!(std::fs::read(&unrelated).unwrap(), b"keep exactly");
    assert!(!target.join(MARKER).exists());
    assert!(!target.join(LOCK).exists());
}

#[test]
fn every_save_preserves_the_reserved_subject_targets_and_creation_time() {
    let (_dir, target, seed) = fixture();
    let mut store = Store::open(&target, &seed.nonce, Some(&seed)).unwrap();
    for field in ["node", "vault", "owner", "created"] {
        let mut changed = seed.clone();
        match field {
            "node" => changed.subject.node = "ab".repeat(16),
            "vault" => changed.subject.vault = "ab".repeat(32),
            "owner" => changed.repositories[0].owner = "different-owner".into(),
            "created" => {
                changed.created_ms = 2;
                changed.updated_ms = 2;
            }
            _ => unreachable!(),
        }
        assert!(store.save(&changed).is_err(), "{field}");
        assert_eq!(store.load().unwrap(), Some(seed.clone()));
    }
}

#[test]
fn a_second_store_is_busy_until_the_original_store_unlocks() {
    let (_dir, target, seed) = fixture();
    let store = Store::open(&target, &seed.nonce, Some(&seed)).unwrap();
    let error = Store::open(&target, &seed.nonce, None).err().unwrap();
    assert!(error.to_string().contains("busy"));
    drop(store);
    assert!(Store::open(&target, &seed.nonce, None).is_ok());
}

#[test]
fn another_process_observes_the_live_transaction_lock() {
    let (_dir, target, seed) = fixture();
    let held = Store::open(&target, &seed.nonce, Some(&seed)).unwrap();
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "client::release::probe::journal::tests::process_lock_peer",
            "--nocapture",
        ])
        .env("ATERM_RELEASE_PROBE_LOCK_TEST", &target)
        .env("ATERM_RELEASE_PROBE_LOCK_NONCE", &seed.nonce);
    let output = aterm_uds::deadline::output_until_complete_stdout(
        &mut child,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    )
    .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("observed probe transaction busy"));
    drop(held);
    assert!(Store::open(&target, &seed.nonce, None).is_ok());
}

#[test]
fn process_lock_peer() {
    let Some(path) = std::env::var_os("ATERM_RELEASE_PROBE_LOCK_TEST") else {
        return;
    };
    let nonce = std::env::var("ATERM_RELEASE_PROBE_LOCK_NONCE").unwrap();
    let error = Store::open(Path::new(&path), &nonce, None)
        .err()
        .expect("parent holds lock");
    assert!(error.to_string().contains("busy"));
    println!("observed probe transaction busy");
}

#[test]
fn a_well_formed_but_changed_journal_is_not_a_new_plan() {
    let (_dir, target, seed) = fixture();
    drop(Store::open(&target, &seed.nonce, Some(&seed)).unwrap());
    let mut changed = seed.clone();
    changed.repositories[0].owner = "foreign-owner".into();
    fsx::write_atomic(
        &target.join(FILE),
        &encoding::encode(&changed).unwrap(),
        0o600,
    )
    .unwrap();
    assert!(Store::open(&target, &seed.nonce, None).is_err());
}

#[test]
fn linked_private_files_and_replaced_directories_are_refused() {
    let (_dir, target, seed) = fixture();
    let mut store = Store::open(&target, &seed.nonce, Some(&seed)).unwrap();
    let copy = target.with_file_name("marker-copy");
    std::fs::hard_link(target.join(MARKER), &copy).unwrap();
    assert!(store.load().is_err());
    std::fs::remove_file(copy).unwrap();
    let old = target.with_file_name("previous-directory");
    std::fs::rename(&target, &old).unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&target)
        .unwrap();
    assert!(store.save(&seed).is_err());
}
