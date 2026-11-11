// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::{fs::OpenOptions, os::unix::fs::PermissionsExt as _};

fn world() -> aterm_tempfile::TempDir {
    let world = aterm_tempfile::tempdir().unwrap();
    std::fs::set_permissions(world.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    world
}

fn blocked(path: &Path) -> bool {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.join(LOCK))
        .unwrap();
    matches!(
        aterm_toolchain::record_lock::RecordLock::try_lock(file, 0),
        Err(std::fs::TryLockError::WouldBlock)
    )
}

#[test]
fn fresh_materialization_transfers_actual_owner_and_retirement_waits_for_users() {
    let world = world();
    let producer = Producer::create(world.path(), "input").unwrap();
    std::fs::write(
        producer.path().join("manifest"),
        b"caller validation remains separate",
    )
    .unwrap();
    let owner = producer.finish().unwrap();
    let input = owner.lend().unwrap();
    assert_eq!(
        std::fs::read(input.path().join("manifest")).unwrap(),
        b"caller validation remains separate"
    );
    assert!(blocked(&world.path().join("input")));
    let failure = owner
        .retire()
        .err()
        .expect("actual use prevents retirement");
    assert_eq!(failure.error().kind(), io::ErrorKind::WouldBlock);
    let (_, owner) = failure.into_parts();
    owner.check_current().unwrap();
    drop(input);
    let retired = owner.retire().unwrap();
    retired.check_current().unwrap();
    assert!(blocked(retired.container()));
    assert_eq!(
        std::fs::read(retired.container().join(STATE)).unwrap(),
        RETIRED
    );
    drop(retired);
    assert!(!blocked(&world.path().join("input")));
    assert_eq!(
        Producer::create(world.path(), "input")
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
}

#[test]
fn existing_inputs_and_invalid_names_are_never_adopted_or_rewritten() {
    let world = world();
    let old = world.path().join("existing");
    std::fs::create_dir(&old).unwrap();
    std::fs::write(old.join("sentinel"), b"original input").unwrap();
    assert!(Producer::create(world.path(), "existing").is_err());
    assert_eq!(
        std::fs::read(old.join("sentinel")).unwrap(),
        b"original input"
    );
    assert!(!old.join(STATE).exists());
    for name in ["", ".", "..", "a/b", "a/", "/absolute", "nul\0name"] {
        assert!(Producer::create(world.path(), name).is_err(), "{name:?}");
    }
    assert!(Producer::create(Path::new("relative"), "input").is_err());
    assert!(Producer::create(world.path(), &"a".repeat(129)).is_err());
}

#[test]
fn unsafe_or_symlinked_parent_refuses_before_creating_input() {
    let world = world();
    let alias = world.path().join("alias");
    std::os::unix::fs::symlink(world.path(), &alias).unwrap();
    assert!(Producer::create(&alias, "input").is_err());
    std::fs::set_permissions(world.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Producer::create(world.path(), "input").is_err());
    std::fs::set_permissions(world.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!world.path().join("input").exists());
}

#[test]
fn creation_path_swap_cannot_redirect_lifecycle_publication() {
    let world = world();
    let moved = world.path().join("original");
    let result = Producer::create_with(world.path(), "input", |path| {
        std::fs::rename(path, &moved)?;
        std::fs::create_dir(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        std::fs::write(path.join(STATE), b"replacement must remain unchanged")
    });
    assert!(result.is_err());
    assert_eq!(
        std::fs::read(world.path().join("input").join(STATE)).unwrap(),
        b"replacement must remain unchanged"
    );
    assert_eq!(std::fs::read(moved.join(STATE)).unwrap(), QUARANTINED);
    assert!(moved.join(CONTENTS).is_dir());
    assert!(!world.path().join("input").join(CONTENTS).exists());
}

#[test]
fn owner_drop_quarantines_but_retained_use_keeps_original_lock() {
    let world = world();
    let producer = Producer::create(world.path(), "input").unwrap();
    let owner = producer.finish().unwrap();
    let input = owner.lend().unwrap();
    let container = world.path().join("input");
    drop(owner);
    assert_eq!(std::fs::read(container.join(STATE)).unwrap(), QUARANTINED);
    assert!(input.check_current().is_err());
    assert!(blocked(&container));
    drop(input);
    assert!(!blocked(&container));
    assert!(Producer::create(world.path(), "input").is_err());
}

#[test]
fn incomplete_materialization_is_quarantined_without_removal() {
    let world = world();
    let producer = Producer::create(world.path(), "input").unwrap();
    let output = producer.path().join("partial");
    std::fs::write(&output, b"partial bytes").unwrap();
    drop(producer);
    assert_eq!(std::fs::read(output).unwrap(), b"partial bytes");
    assert_eq!(
        std::fs::read(world.path().join("input").join(STATE)).unwrap(),
        QUARANTINED
    );
}

#[test]
fn namespace_drift_is_sticky_and_only_quarantines_original_directory() {
    let world = world();
    let owner = Producer::create(world.path(), "input")
        .unwrap()
        .finish()
        .unwrap();
    let moved = world.path().join("moved");
    let container = world.path().join("input");
    std::fs::rename(&container, &moved).unwrap();
    std::fs::create_dir(&container).unwrap();
    std::fs::set_permissions(&container, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(container.join(STATE), b"replacement").unwrap();
    assert!(owner.check_current().is_err());
    assert_eq!(
        std::fs::read(container.join(STATE)).unwrap(),
        b"replacement"
    );
    assert_eq!(std::fs::read(moved.join(STATE)).unwrap(), QUARANTINED);
    std::fs::remove_file(container.join(STATE)).unwrap();
    std::fs::remove_dir(&container).unwrap();
    std::fs::rename(moved, container).unwrap();
    assert!(owner.check_current().is_err());
}

#[test]
fn contents_lock_and_state_substitution_refuse_original_bookend() {
    for which in [CONTENTS, LOCK, STATE] {
        let world = world();
        let owner = Producer::create(world.path(), "input")
            .unwrap()
            .finish()
            .unwrap();
        let root = world.path().join("input");
        let original = root.join(which);
        std::fs::rename(&original, root.join("moved")).unwrap();
        if which == CONTENTS {
            std::fs::create_dir(&original).unwrap();
        } else {
            std::fs::write(&original, if which == STATE { READY } else { b"" }).unwrap();
            std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        // Equal replacement state bytes are data, not replacement lock/dir custody.
        if which == STATE {
            // State replacement by the same private writer is allowed by the atomic
            // record protocol. Corrupt content rather than pretending inode pinning.
            std::fs::write(&original, b"unknown lifecycle").unwrap();
        }
        assert!(owner.check_current().is_err(), "{which}");
        assert!(owner.lend().is_err(), "{which}");
    }
}

#[test]
fn foreign_process_check_and_drop_cannot_quarantine_parent_state() {
    let world = world();
    let mut owner = Producer::create(world.path(), "input")
        .unwrap()
        .finish()
        .unwrap();
    // Inject the creator-process mismatch without changing this test process's
    // credentials or forking a multithreaded Rust runtime. This exercises the
    // real check and Owner::Drop path; it is not native fork qualification.
    let inner = Arc::get_mut(owner.inner.as_mut().unwrap()).unwrap();
    inner.process = std::process::id().wrapping_add(1);
    let input = InputUse {
        inner: Arc::clone(owner.inner.as_ref().unwrap()),
    };
    assert!(owner.check_current().is_err());
    assert!(input.check_current().is_err());
    drop(owner);
    assert_eq!(
        std::fs::read(world.path().join("input").join(STATE)).unwrap(),
        READY
    );
    assert!(blocked(&world.path().join("input")));
    // Restore only the injected creator field while holding the sole actual
    // test owner, so ordinary test cleanup does not leak its kernel lock.
    let mut inner = input.inner;
    Arc::get_mut(&mut inner).unwrap().process = std::process::id();
    inner.quarantine();
}

#[test]
fn final_foreign_drop_neither_unlocks_original_description_nor_writes_state() {
    let mut actor = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    actor
        .args([
            "--exact",
            "private_input::tests::final_foreign_drop_actor",
            "--test-threads=1",
            "--nocapture",
        ])
        .env("ATPKG_PRIVATE_INPUT_FOREIGN_DROP", "1");
    let output =
        aterm_uds::deadline::output_within(&mut actor, aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn final_foreign_drop_actor() {
    if std::env::var_os("ATPKG_PRIVATE_INPUT_FOREIGN_DROP").is_none() {
        return;
    }
    let world = world();
    let mut owner = Producer::create(world.path(), "input")
        .unwrap()
        .finish()
        .unwrap();
    let mut inner = Arc::try_unwrap(owner.inner.take().unwrap())
        .ok()
        .expect("sole original owner");
    inner.process = std::process::id().wrapping_add(1);
    drop(inner); // Execute the actual last-drop branch, not a retained Arc.
    let state = std::fs::read(world.path().join("input").join(STATE)).unwrap();
    let still_held = blocked(&world.path().join("input"));
    assert_eq!(state, READY);
    assert!(still_held, "foreign Drop released the original kernel lock");
    // The isolated maintained test actor exits after this control. The kernel
    // closes its deliberately forgotten foreign-copy descriptor. Never recover
    // a numeric fd whose original owner may already have closed it.
}
