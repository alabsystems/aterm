// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::os::unix::fs::{PermissionsExt as _, symlink};

use super::{
    PINNED_DIR_OPEN_COMPONENT_LIMIT, PINNED_DIR_OPERATION_DESCRIPTOR_UNITS, PinnedDir,
    REMOVE_LIVE_FIXED_UNITS, REMOVE_MAX_DEPTH, pinned_chain_open_count,
    reset_pinned_chain_open_count,
};

fn unique_dir(stem: &str) -> std::path::PathBuf {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!(
            "aterm-pinned-{stem}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
}

/// An artifact's bytes are only ordered ahead of what follows (aterm-durable's
/// counting seam), never fully flushed: the requester is local and reads the
/// page cache, and no recovery reads them. Each entry published at once, and the
/// folder's own flush, take one plain `fsync(2)` of the held descriptor, aterm-durable's
/// (`primitive::fsync`), which astream-sys counts `plain`: the seam counts exactly this
/// thread's, a deferred entry takes none, and the process-wide `flush_tally` moves by at
/// least the folder's (this binary's other tests flush meanwhile, so only a bound reads
/// there; aterm-durable's `tests/flush_tally.rs` holds the two counts equal). A folder
/// flush that called the OS itself, as this one did through `aterm_dirfd::fsync`, moves
/// neither.
#[test]
fn an_artifacts_bytes_are_ordered_and_a_durable_write_stays_durable() {
    use aterm_durable::{Flush, seam::Recording};
    use std::ffi::OsStr;
    let root = unique_dir("flush-tiers");
    std::fs::create_dir_all(&root).unwrap();
    let dir = PinnedDir::open_resolved(&root).unwrap();
    let recording = Recording::start();
    let shot = dir.write_private(OsStr::new("shot.png"), b"png").unwrap();
    assert_eq!(
        recording.take(),
        [Flush::Ordered],
        "a replaced artifact's bytes; its entry takes the plain fsync(2)"
    );
    assert_eq!(recording.take_plain(), 1, "the replaced artifact's entry");
    let done = dir
        .write_new_private(OsStr::new("shot.done"), b"done")
        .unwrap();
    assert_eq!(recording.take(), [Flush::Ordered], "a new artifact's bytes");
    assert_eq!(recording.take_plain(), 1, "the new artifact's entry");
    let frame = dir
        .write_new_private_deferred_dir_sync_authorized(OsStr::new("frame.png"), b"f", || true)
        .unwrap();
    assert_eq!(
        recording.take(),
        [Flush::Ordered],
        "a video batch member's bytes"
    );
    assert_eq!(
        recording.take_plain(),
        0,
        "a deferred entry waits for the batch"
    );
    let token = dir
        .write_new_private_with_hooks(OsStr::new("token"), b"secret", || {}, || {}, Flush::Durable)
        .unwrap();
    assert_eq!(recording.take(), [Flush::Durable], "a durable write");
    assert_eq!(recording.take_plain(), 1, "the durable write's entry");
    let before = astream_sys::flush_tally();
    dir.sync().unwrap();
    let after = astream_sys::flush_tally();
    assert_eq!(recording.take(), [], "the directory's plain fsync(2)");
    assert_eq!(recording.take_plain(), 1, "the folder's own flush");
    assert!(
        after.plain > before.plain,
        "astream-sys counted the folder's flush: {before:?} -> {after:?}"
    );
    drop((shot, done, frame, token));
    let _ = std::fs::remove_dir_all(&root);
}

/// `O_NOFOLLOW`, `unlinkat` and `renameat` govern only the final component, so `"link/"`,
/// `"link/."` and `"link//"` must be refused as names, never resolved through the symlink.
#[test]
fn a_trailing_separator_never_reaches_through_a_symlink() {
    use std::ffi::OsStr;
    let root = unique_dir("trailing-separator");
    let confined = root.join("confined");
    let outside = root.join("outside");
    std::fs::create_dir_all(&confined).unwrap();
    std::fs::create_dir_all(outside.join("victim")).unwrap();
    std::fs::create_dir_all(outside.join("tree")).unwrap();
    std::fs::write(outside.join("tree").join("keep"), b"outside").unwrap();
    symlink(&outside, confined.join("link")).unwrap();
    symlink(outside.join("victim"), confined.join("vlink")).unwrap();
    symlink(outside.join("tree"), confined.join("tlink")).unwrap();
    symlink(outside.join("absent"), confined.join("dangle")).unwrap();
    let pinned = PinnedDir::open(&confined).unwrap();

    let mut escapes = Vec::new();
    for name in ["link/", "link/.", "link//", "link/./."] {
        match pinned.child(OsStr::new(name)) {
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {}
            other => escapes.push(format!(
                "child({name:?}) was not refused: {:?}",
                other.map(|child| child.path().to_path_buf())
            )),
        }
    }
    let _ = pinned.remove_child_tree(OsStr::new("tlink/"));
    if !outside.join("tree").join("keep").exists() {
        escapes.push("remove_child_tree(\"tlink/\") emptied an outside directory".into());
    }
    let _ = pinned.rename(OsStr::new("vlink/"), OsStr::new("moved"));
    if !outside.join("victim").is_dir() {
        escapes.push("rename(\"vlink/\", ..) moved an outside directory in".into());
    }
    let _ = pinned.create_child(OsStr::new("dangle/"));
    if outside.join("absent").exists() {
        escapes.push("create_child(\"dangle/\") made a directory outside".into());
    }
    assert!(pinned.child(OsStr::new("link")).is_err());
    drop(pinned);
    let _ = std::fs::remove_dir_all(&root);
    assert!(escapes.is_empty(), "{escapes:#?}");
}

/// A same-uid actor can retarget a symlink between admission and confinement, so admission
/// runs against the exact resolved target, and the hard depth ceiling fires too, both before
/// any ancestor handle opens.
#[test]
fn deep_resolved_path_is_rejected_before_ancestor_chain_open() {
    let root = unique_dir("deep-resolved-budget");
    let mut deep_parent = root.join("target");
    while deep_parent.components().count() + 1 < PINNED_DIR_OPEN_COMPONENT_LIMIT {
        deep_parent.push("d");
    }
    let deep = deep_parent.join("leaf");
    std::fs::create_dir_all(&deep).unwrap();
    let alias = root.join("alias");
    symlink(&deep_parent, &alias).unwrap();
    let resolved_alias = alias.join("leaf");

    reset_pinned_chain_open_count();
    let mut admission_ran = false;
    let error = PinnedDir::open_resolved_with_admission(&resolved_alias, |canonical| {
        admission_ran = true;
        assert_eq!(
            canonical.components().count(),
            PINNED_DIR_OPEN_COMPONENT_LIMIT
        );
        assert_eq!(pinned_chain_open_count(), 0);
        false
    })
    .expect_err("aggregate admission must be able to refuse the exact resolved depth");
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    assert!(admission_ran);
    assert_eq!(
        pinned_chain_open_count(),
        0,
        "admission refusal must occur before any retained ancestor handle opens"
    );

    let too_deep = deep.join("d");
    std::fs::create_dir(&too_deep).unwrap();
    reset_pinned_chain_open_count();
    let error = PinnedDir::open_resolved(&resolved_alias.join("d"))
        .expect_err("a resolved path deeper than the descriptor budget must fail");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(
        pinned_chain_open_count(),
        0,
        "the depth check must run before any retained ancestor handle opens"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn recursive_cleanup_depth_fits_the_charged_suffix() {
    assert_eq!(
        REMOVE_LIVE_FIXED_UNITS + REMOVE_MAX_DEPTH + 1,
        PINNED_DIR_OPERATION_DESCRIPTOR_UNITS,
        "fixed publication handles plus the deepest opened child must fit admission"
    );

    let root = unique_dir("cleanup-depth-budget");
    let recording = root.join("instance/recording");
    let mut nested = recording.clone();
    for depth in 0..=REMOVE_MAX_DEPTH {
        nested.push(format!("d{depth}"));
    }
    std::fs::create_dir_all(&nested).unwrap();
    let instance = PinnedDir::open(&root.join("instance")).unwrap();
    let error = instance
        .remove_child_tree(std::ffi::OsStr::new("recording"))
        .expect_err("a deeper owner-namespace tree must fail before exceeding its charge");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        recording.is_dir(),
        "fail-closed cleanup leaves the interfered exact tree for a later bounded sweep"
    );

    drop(instance);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn clear_contents_reports_only_successful_mutations() {
    let root = unique_dir("clear-mutation-count");
    std::fs::create_dir_all(root.join("nested")).unwrap();
    let pinned = PinnedDir::open(&root).unwrap();

    let mut budget = 8;
    let empty = pinned.clear_contents_with_budget(&mut budget);
    empty.result.unwrap();
    assert_eq!(empty.removed, 1, "the empty nested directory was removed");

    std::fs::write(root.join("first"), b"one").unwrap();
    std::fs::create_dir(root.join("nested")).unwrap();
    std::fs::write(root.join("nested/second"), b"two").unwrap();
    let mut budget = 8;
    let cleared = pinned.clear_contents_with_budget(&mut budget);
    cleared.result.unwrap();
    assert_eq!(
        cleared.removed, 3,
        "two files plus their containing directory are real mutations"
    );
    assert_eq!(budget, 5, "the scan budget tracks the same three entries");

    let mut budget = 8;
    let no_op = pinned.clear_contents_with_budget(&mut budget);
    no_op.result.unwrap();
    assert_eq!(no_op.removed, 0, "an empty sweep must not request fsync");
    assert_eq!(budget, 8);

    drop(pinned);
    let _ = std::fs::remove_dir_all(root);
}

/// A directory lock is an open file description of its own: two locks through one pin
/// exclude each other, and a lease released by `LOCK_UN` ([`HeldAdvisoryLock`]) is free at
/// once, even while a child forked under it has yet to exec.
#[test]
fn directory_lock_has_an_independent_unlock_lifetime() {
    use crate::native_document_host::HeldAdvisoryLock;
    use crate::parked_fork::ParkedChild;
    use std::fs::TryLockError;

    let root = unique_dir("directory-lock");
    std::fs::create_dir_all(&root).unwrap();
    let pin = PinnedDir::open(&root).unwrap();
    let contender = pin.open_directory_lock().unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the contender's probe; the one take it makes is let go by `unlock` below"
    )]
    let try_contender = || contender.try_lock();
    let refused = || matches!(try_contender(), Err(TryLockError::WouldBlock));

    let lease = HeldAdvisoryLock::try_lock(pin.open_directory_lock().unwrap())
        .expect("the lease takes the directory lock");
    assert!(
        refused(),
        "a second lock through one pin is its own description"
    );
    let child = ParkedChild::fork();
    drop(lease);
    try_contender().expect("the dropped lease's lock is free at once, its pin still open");
    assert!(
        child.still_parked(),
        "the child forked under it had yet to exec"
    );
    child.release();
    contender.unlock().unwrap();

    let lease = pin.open_directory_lock().unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the negative control: a lock let go by the close alone"
    )]
    lease.try_lock().unwrap();
    // The lease's own descriptor is kept out of children (`aterm_uds::kept_out` marks it
    // close-on-fork), so the control hands the child a duplicate a fork does copy.
    let inherited = lease.try_clone().unwrap();
    let child = ParkedChild::fork();
    drop((lease, inherited));
    assert!(
        refused(),
        "control: closed, not unlocked, the child's copy holds it"
    );
    child.release();

    drop((contender, pin));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn retained_leaf_never_redirects_and_fails_closed_after_path_replacement() {
    let root = unique_dir("ancestor-swap");
    let outside = unique_dir("outside");
    std::fs::create_dir_all(root.join("images")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let pinned = PinnedDir::open(&root.join("images")).unwrap();

    let moved = root.join("images-moved");
    std::fs::rename(root.join("images"), &moved).unwrap();
    symlink(&outside, root.join("images")).unwrap();

    assert!(
        pinned
            .write_private(std::ffi::OsStr::new("shot.png"), b"safe")
            .is_err()
    );
    assert!(
        !moved.join("shot.png").exists(),
        "the exact temporary/published artifact is removed on final validation failure"
    );
    assert!(
        !outside.join("shot.png").exists(),
        "the replacement ancestor receives no artifact bytes"
    );
    assert!(pinned.validate_path_identity().is_err());

    let _ = std::fs::remove_file(root.join("images"));
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}

#[test]
fn deferred_batch_write_uses_only_the_retained_directory_capability() {
    let root = unique_dir("deferred-batch-swap");
    let outside = unique_dir("deferred-batch-outside");
    let original = root.join("recording");
    let moved = root.join("recording-moved");
    std::fs::create_dir_all(&original).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let pinned = PinnedDir::open(&original).unwrap();

    std::fs::rename(&original, &moved).unwrap();
    symlink(&outside, &original).unwrap();

    let frame = pinned
        .write_new_private_deferred_dir_sync_authorized(
            std::ffi::OsStr::new("frame.png"),
            b"retained",
            || true,
        )
        .expect("batch members use the already-authorized retained directory");
    frame.validate_entry_identity_at_retained().unwrap();
    assert!(
        frame.validate_path_identity().is_err(),
        "ordinary exact-path validation remains fail-closed after replacement"
    );
    assert_eq!(std::fs::read(moved.join("frame.png")).unwrap(), b"retained");
    assert!(
        !outside.join("frame.png").exists(),
        "the replacement namespace never receives batch bytes"
    );

    drop(frame);
    drop(pinned);
    let _ = std::fs::remove_file(original);
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}

#[test]
fn retained_parent_removes_only_its_original_child_after_path_replacement() {
    let root = unique_dir("cleanup-swap");
    let replacement = unique_dir("cleanup-replacement");
    std::fs::create_dir_all(root.join("instance/rec-1")).unwrap();
    std::fs::write(root.join("instance/rec-1/frame.png"), b"old").unwrap();
    std::fs::create_dir_all(replacement.join("rec-1")).unwrap();
    std::fs::write(replacement.join("rec-1/keep.txt"), b"keep").unwrap();
    let instance = PinnedDir::open(&root.join("instance")).unwrap();

    let moved = root.join("instance-moved");
    std::fs::rename(root.join("instance"), &moved).unwrap();
    symlink(&replacement, root.join("instance")).unwrap();
    instance
        .remove_child_tree(std::ffi::OsStr::new("rec-1"))
        .unwrap();

    assert!(!moved.join("rec-1").exists());
    assert_eq!(
        std::fs::read(replacement.join("rec-1/keep.txt")).unwrap(),
        b"keep"
    );
    assert!(instance.validate_path_identity().is_err());

    let _ = std::fs::remove_file(root.join("instance"));
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(replacement);
}

#[test]
fn retained_file_probes_stay_on_original_directory_after_path_replacement() {
    let root = unique_dir("retained-read-swap");
    let original = root.join("instance");
    let moved = root.join("instance-moved");
    std::fs::create_dir_all(&original).unwrap();
    std::fs::write(original.join("index.json"), b"original-index").unwrap();
    std::fs::write(original.join("frame.png"), b"original-frame").unwrap();
    let pinned = PinnedDir::open(&original).unwrap();

    std::fs::rename(&original, &moved).unwrap();
    std::fs::create_dir(&original).unwrap();
    std::fs::write(original.join("index.json"), b"replacement-index").unwrap();
    std::fs::write(original.join("frame.png"), b"replacement-frame").unwrap();

    assert!(
        pinned.validate_path_identity().is_err(),
        "the ordinary path authority must reject the replaced namespace"
    );
    let (index, index_guard) = pinned
        .read_private_at_retained(std::ffi::OsStr::new("index.json"), 64)
        .expect("capability-bound read stays on the retained directory");
    let frame_guard = pinned
        .pin_private_file_at_retained(std::ffi::OsStr::new("frame.png"))
        .expect("capability-bound pin stays on the retained directory");
    assert_eq!(index, b"original-index");
    drop(frame_guard);
    drop(index_guard);
    assert_eq!(
        std::fs::read(original.join("index.json")).unwrap(),
        b"replacement-index",
        "the lexical replacement is never read or modified"
    );
    assert_eq!(
        std::fs::read(original.join("frame.png")).unwrap(),
        b"replacement-frame"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn hardlink_target_is_rejected_before_bytes_or_mode_change() {
    let root = unique_dir("hardlink");
    std::fs::create_dir_all(&root).unwrap();
    let victim = root.join("victim");
    std::fs::write(&victim, b"keep-me").unwrap();
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o640)).unwrap();
    std::fs::hard_link(&victim, root.join("shot.png")).unwrap();
    let pinned = PinnedDir::open(&root).unwrap();

    assert!(
        pinned
            .write_private(std::ffi::OsStr::new("shot.png"), b"replace")
            .is_err()
    );
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep-me");
    assert_eq!(
        std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
        0o640
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn fifo_target_fails_without_blocking() {
    use std::os::unix::ffi::OsStrExt as _;

    let root = unique_dir("fifo");
    std::fs::create_dir_all(&root).unwrap();
    let fifo = root.join("shot.png");
    let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: `path` is NUL-terminated and points to an absent path.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let pinned = PinnedDir::open(&root).unwrap();
    let (answered, answer) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = answered.send(
            pinned
                .write_private(std::ffi::OsStr::new("shot.png"), b"replace")
                .is_err(),
        );
    });
    let refused = answer
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("O_NONBLOCK must make a writerless FIFO fail rather than block");
    assert!(refused, "a FIFO is not a regular file to replace");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn exact_file_guard_rejects_replacement_and_create_new_does_not_modify_it() {
    let root = unique_dir("file-swap");
    std::fs::create_dir_all(&root).unwrap();
    let pinned = PinnedDir::open(&root).unwrap();
    let file = pinned
        .write_new_private(std::ffi::OsStr::new("index.json.tmp"), b"ours")
        .unwrap();
    pinned
        .remove_file_if_exists(std::ffi::OsStr::new("index.json.tmp"))
        .unwrap();
    std::fs::write(root.join("index.json.tmp"), b"replacement").unwrap();

    assert!(file.validate_path_identity().is_err());
    std::fs::write(root.join("index.json"), b"planted-final").unwrap();
    assert!(
        pinned
            .write_new_private(std::ffi::OsStr::new("index.json"), b"ours")
            .is_err()
    );
    assert_eq!(
        std::fs::read(root.join("index.json.tmp")).unwrap(),
        b"replacement"
    );
    assert_eq!(
        std::fs::read(root.join("index.json")).unwrap(),
        b"planted-final"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn failed_atomic_publication_leaves_no_private_temporary_file() {
    let root = unique_dir("publication-cleanup");
    std::fs::create_dir_all(root.join("index.json")).unwrap();
    let pinned = PinnedDir::open(&root).unwrap();

    assert!(
        pinned
            .write_new_private(std::ffi::OsStr::new("index.json"), b"complete")
            .is_err()
    );
    assert!(root.join("index.json").is_dir());
    assert!(
        std::fs::read_dir(&root).unwrap().flatten().all(|entry| {
            !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".aterm-write-")
        }),
        "the exact temporary file is removed when no-replace publication fails"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn enumeration_stops_at_the_requested_bound() {
    let root = unique_dir("bounded-enumeration");
    std::fs::create_dir_all(&root).unwrap();
    for index in 0..5 {
        std::fs::write(root.join(format!("{index}.txt")), b"x").unwrap();
    }
    let pinned = PinnedDir::open(&root).unwrap();
    assert!(pinned.names(4).is_err());
    assert_eq!(pinned.names(5).unwrap().len(), 5);
    let _ = std::fs::remove_dir_all(root);
}

/// A completion marker's rename is the last call on its directory, so a
/// consumer that removes the directory the moment the marker appears fails
/// nothing; an ordinary publication still checks its path after the rename
/// and does fail (the negative control).
#[test]
fn a_marker_consumer_may_remove_the_directory_at_once() {
    let consume = |root: &std::path::Path| {
        let done = root.join("snapshot.png.done");
        let root = root.to_path_buf();
        move || {
            assert!(done.is_file(), "the marker is visible");
            std::fs::remove_dir_all(&root).unwrap();
        }
    };
    let name = std::ffi::OsStr::new("snapshot.png.done");

    let root = unique_dir("marker-last");
    std::fs::create_dir_all(&root).unwrap();
    let pinned = PinnedDir::open(&root).unwrap();
    pinned
        .write_new_private_marker(name, b"done", consume(&root))
        .expect("nothing follows the marker's rename");
    assert!(!root.exists());

    let root = unique_dir("marker-ordinary");
    std::fs::create_dir_all(&root).unwrap();
    let pinned = PinnedDir::open(&root).unwrap();
    let error = pinned
        .write_new_private_with_hooks(
            name,
            b"done",
            || {},
            consume(&root),
            crate::durable::Flush::Ordered,
        )
        .expect_err("an ordinary publication revalidates its path after the rename");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{error}");
}

/// A retained file's exact removal says why it could not check its name:
/// the errno, or that the file vanished. Only another object at its name,
/// a symlink included, reads as "identity changed".
#[test]
fn exact_file_removal_names_its_probe_error() {
    let root = unique_dir("file-probe-error");
    std::fs::create_dir_all(&root).unwrap();
    let pinned = PinnedDir::open(&root).unwrap();
    let name = std::ffi::OsStr::new("index.json");
    drop(pinned.write_new_private(name, b"{}").unwrap());

    let file = pinned.pin_private_file(name).unwrap();
    super::fail_next_identity_probe(libc::EINTR);
    let error = file
        .remove_exact()
        .expect_err("a probe that could not look removes nothing");
    assert_eq!(error.raw_os_error(), Some(libc::EINTR), "{error}");
    assert!(root.join(name).is_file(), "a failed probe unlinks nothing");

    let file = pinned.pin_private_file(name).unwrap();
    std::fs::rename(root.join(name), root.join("moved.json")).unwrap();
    let error = file
        .remove_exact()
        .expect_err("a vanished file is not removed");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(error.to_string().contains("vanished"), "{error}");
    assert!(root.join("moved.json").is_file());

    drop(pinned.write_new_private(name, b"{}").unwrap());
    let file = pinned.pin_private_file(name).unwrap();
    std::fs::remove_file(root.join(name)).unwrap();
    symlink(root.join("moved.json"), root.join(name)).unwrap();
    let error = file
        .remove_exact()
        .expect_err("a file replaced by a symlink is not removed");
    assert!(error.to_string().contains("identity changed"), "{error}");
    assert!(root.join("moved.json").is_file(), "the link's target stays");

    drop(pinned);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn unlink_postcheck_never_certifies_a_swapped_replacement() {
    let root = unique_dir("unlink-barrier");
    std::fs::create_dir_all(root.join("recording")).unwrap();
    let parent = PinnedDir::open(&root).unwrap();
    let child = parent.child(std::ffi::OsStr::new("recording")).unwrap();
    let moved = root.join("recording-moved");
    let root_for_hook = root.clone();
    let moved_for_hook = moved.clone();

    let error = parent
        .remove_empty_child_exact_with_hook(std::ffi::OsStr::new("recording"), &child, move || {
            std::fs::rename(root_for_hook.join("recording"), &moved_for_hook).unwrap();
            std::fs::create_dir(root_for_hook.join("recording")).unwrap();
        })
        .expect_err("unlinking a swapped direct child must not report exact cleanup");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(
        moved.is_dir(),
        "the retained inode remains and the post-check detects that fact"
    );
    assert!(
        !root.join("recording").exists(),
        "the only residual side effect is confined to the owned parent"
    );

    let _ = std::fs::remove_dir_all(root);
}
