// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::document_store::{DocumentStore, TextEdit};
use crate::native_document_io::{encode_journal, encode_journal_v1, journal_tail};
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt as _;

pub(super) fn test_root(name: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("aterm-draft-journal-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    root
}

pub(super) fn snapshots(text: &str) -> (DocumentStore, DocumentId, DocumentSnapshot) {
    let mut store = crate::test_document_store();
    let id = store.open("file:///tmp/draft.md".to_string(), text.to_string());
    let snapshot = store.snapshot(id).unwrap();
    (store, id, snapshot)
}

#[cfg(unix)]
fn make_fifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt as _;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("FIFO path");
    // SAFETY: `path` is a live NUL-terminated pathname and `mkfifo` retains no pointer.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
}

#[test]
fn journal_reader_accepts_exact_cap_and_rejects_sparse_over_cap() {
    let root = test_root("bounded-reader");
    fs::create_dir_all(&root).unwrap();
    let exact = root.join("exact.atdj");
    File::create(&exact)
        .unwrap()
        .set_len(MAX_JOURNAL_BYTES as u64)
        .unwrap();
    let bytes = read_journal_image(&exact)
        .expect("exact-cap regular journal is admitted")
        .expect("exact-cap journal exists");
    assert_eq!(bytes.len(), MAX_JOURNAL_BYTES);
    drop(bytes);

    let oversized = root.join("oversized.atdj");
    File::create(&oversized)
        .unwrap()
        .set_len(MAX_JOURNAL_BYTES as u64 + 1)
        .unwrap();
    let started = std::time::Instant::now();
    let error = read_journal_image(&oversized).expect_err("oversized journal is rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains(&MAX_JOURNAL_BYTES.to_string()));
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "the over-cap read never returned"
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn inspection_refuses_fifo_and_final_symlink_without_waiting() {
    let root = test_root("special-inspection");
    let host = DraftJournalHost::new(root.clone()).unwrap();
    let canonical = "file:///tmp/special-inspection.md";
    let path = host.path_for(host.key_for(canonical));
    make_fifo(&path);
    let started = std::time::Instant::now();
    let fifo_error = host.inspect_open(canonical, b"disk").unwrap_err();
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "inspecting the FIFO journal parked"
    );
    assert!(fifo_error.contains("regular non-link"), "{fifo_error}");

    fs::remove_file(&path).unwrap();
    let victim = root.join("victim.atdj");
    fs::write(&victim, b"do not follow").unwrap();
    std::os::unix::fs::symlink(&victim, &path).unwrap();
    let link_error = host.inspect_open(canonical, b"disk").unwrap_err();
    assert!(link_error.contains("recovery journal"), "{link_error}");
    assert_eq!(fs::read(&victim).unwrap(), b"do not follow");
    assert!(
        fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn fifo_lock_cannot_park_journal_initialization() {
    let root = test_root("fifo-lock");
    let host = DraftJournalHost::new(root.clone()).unwrap();
    let (_store, _id, disk) = snapshots("base");
    let canonical = "file:///tmp/fifo-lock.md";
    let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
    let lock = journal_lock_path(&decision.path).unwrap();
    make_fifo(&lock);

    let started = std::time::Instant::now();
    let error = host.initialize(decision, &disk, &disk).unwrap_err();
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "the FIFO journal lock parked initialization"
    );
    assert!(error.contains("journal lock"), "{error}");
    assert!(fs::symlink_metadata(&lock).unwrap().file_type().is_fifo());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn held_journal_lock_returns_busy_and_a_later_retry_succeeds() {
    let root = test_root("held-lock");
    let host = DraftJournalHost::new(root.clone()).unwrap();
    let (_store, _id, disk) = snapshots("base");
    let canonical = "file:///tmp/held-lock.md";
    let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
    let lock_path = journal_lock_path(&decision.path).unwrap();
    let held =
        crate::native_document_host::HeldAdvisoryLock::lock(open_journal_lock(&lock_path).unwrap())
            .unwrap();

    let started = std::time::Instant::now();
    let error = host.initialize(decision.clone(), &disk, &disk).unwrap_err();
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "the held journal lock parked initialization"
    );
    assert!(error.contains("busy"), "{error}");
    assert!(error.contains("retry"), "{error}");

    drop(held);
    let initialized = host.initialize(decision, &disk, &disk).unwrap();
    assert!(recover_journal_for(initialized.key, &fs::read(initialized.path).unwrap()).is_ok());
    let _ = fs::remove_dir_all(root);
}

/// A second descriptor onto the open file description behind the one descriptor this process holds on `path` — what a child forked at this instant would hold until it execs.
#[cfg(unix)]
fn copy_of_the_open_description_of(path: &Path) -> File {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    fn identity(fd: libc::c_int) -> Option<(libc::dev_t, libc::ino_t)> {
        // SAFETY: `fstat` only writes the out-parameter, which lives for the call; a closed slot fails with EBADF and is skipped.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        (unsafe { libc::fstat(fd, &raw mut stat) } == 0).then_some((stat.st_dev, stat.st_ino))
    }

    let wanted = {
        let probe = File::open(path).expect("open the lock to learn its identity");
        identity(probe.as_raw_fd()).expect("fstat the lock")
    };
    for fd in 0..(1 << 16) {
        if identity(fd) == Some(wanted) {
            // SAFETY: `fd` is the live lock descriptor its owner is still holding on this thread's stack; the copy is a fresh descriptor this call owns.
            let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
            assert!(
                copy >= 0,
                "dup the lock: {}",
                std::io::Error::last_os_error()
            );
            return unsafe { File::from_raw_fd(copy) };
        }
    }
    panic!("no open descriptor refers to {}", path.display());
}

/// The journal lock is free the instant the locked operation returns, even while a copy of its description lives, as in a shell another thread forked mid-append.
#[cfg(unix)]
#[test]
fn a_released_journal_lock_is_free_while_a_forked_copy_of_it_lives() {
    let root = test_root("forked-copy");
    let path = root.join("draft.atdj");
    let lock_path = journal_lock_path(&path).unwrap();

    let forked_copy = with_journal_lock(&path, JournalLockPatience::Worker, || {
        Ok(copy_of_the_open_description_of(&lock_path))
    })
    .unwrap();

    let next = crate::native_document_host::HeldAdvisoryLock::try_lock(
        open_journal_lock(&lock_path).unwrap(),
    );
    assert!(
        next.is_ok(),
        "the lock outlived its operation on the forked copy of its description"
    );
    drop(forked_copy);
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn final_symlink_lock_is_refused_without_touching_its_victim() {
    let root = test_root("symlink-lock");
    let host = DraftJournalHost::new(root.clone()).unwrap();
    let (_store, _id, disk) = snapshots("base");
    let canonical = "file:///tmp/symlink-lock.md";
    let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
    let lock_path = journal_lock_path(&decision.path).unwrap();
    let victim = root.join("lock-victim");
    fs::write(&victim, b"do not lock through me").unwrap();
    std::os::unix::fs::symlink(&victim, &lock_path).unwrap();

    let error = host.initialize(decision, &disk, &disk).unwrap_err();
    assert!(error.contains("journal lock"), "{error}");
    assert_eq!(fs::read(&victim).unwrap(), b"do not lock through me");
    assert!(
        fs::symlink_metadata(&lock_path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn journal_root_final_symlink_is_refused_without_chmod_or_writes() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let fixture = test_root("symlink-root");
    let victim = fixture.join("victim");
    let linked_root = fixture.join("drafts");
    fs::create_dir_all(&victim).unwrap();
    fs::set_permissions(&victim, fs::Permissions::from_mode(0o755)).unwrap();
    symlink(&victim, &linked_root).unwrap();

    assert!(DraftJournalHost::new(linked_root.clone()).is_err());
    assert_eq!(
        fs::metadata(&victim).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(fs::read_dir(&victim).unwrap().next().is_none());
    assert!(
        fs::symlink_metadata(&linked_root)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let _ = fs::remove_dir_all(fixture);
}

#[cfg(unix)]
#[test]
fn hostile_append_and_rewrite_targets_fail_without_mutation() {
    let root = test_root("hostile-persistence");
    let host = DraftJournalHost::new(root.clone()).unwrap();
    let (mut store, id, disk) = snapshots("base");
    let canonical = "file:///tmp/hostile-persistence.md";
    let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
    let initialized = host.initialize(decision, &disk, &disk).unwrap();
    let original = fs::read(&initialized.path).unwrap();
    let _ = store.transact(
        id,
        disk.seq,
        vec![TextEdit {
            range: disk.text.len()..disk.text.len(),
            insert: " changed".to_string(),
        }],
    );
    let current = store.snapshot(id).unwrap();
    let mut reducer = DraftJournalReducer::new_with_key(id, initialized.key, disk.seq);
    let mut append = reducer.plan_snapshot(&current).unwrap();
    append.expected_tail = Some(initialized.tail);
    let rewrite = checkpoint_plan(
        initialized.key,
        JournalRewriteGeneration(1),
        disk.seq,
        initialized.tail.fingerprint(),
        &disk,
        &current,
    )
    .unwrap();

    fs::remove_file(&initialized.path).unwrap();
    make_fifo(&initialized.path);
    let started = std::time::Instant::now();
    assert!(matches!(
        execute_journal_append(
            &initialized.path,
            initialized.key,
            &append,
            JournalLockPatience::Worker,
        ),
        JournalAppendResult::Failed { message, .. }
            if message.contains("regular non-link")
    ));
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "appending to the FIFO journal parked"
    );
    assert!(
        fs::symlink_metadata(&initialized.path)
            .unwrap()
            .file_type()
            .is_fifo()
    );

    fs::remove_file(&initialized.path).unwrap();
    let victim = root.join("rewrite-victim.atdj");
    fs::write(&victim, &original).unwrap();
    std::os::unix::fs::symlink(&victim, &initialized.path).unwrap();
    assert!(matches!(
        execute_journal_rewrite(&initialized.path, &rewrite, JournalLockPatience::Worker),
        JournalRewriteResult::Failed(message) if message.contains("preflight")
    ));
    assert_eq!(fs::read(&victim).unwrap(), original);
    assert!(
        fs::symlink_metadata(&initialized.path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn clean_recovery_replays_before_reinitializing_sequence_space() {
    let root = test_root("recover");
    let host = DraftJournalHost::new(root.clone()).unwrap();
    let (mut store, id, disk) = snapshots("base");
    let current = {
        let outcome = store.transact(
            id,
            disk.seq,
            vec![TextEdit {
                range: 4..4,
                insert: " draft".to_string(),
            }],
        );
        assert!(matches!(
            outcome,
            crate::document_store::DocumentTxnOutcome::Committed { .. }
        ));
        store.snapshot(id).unwrap()
    };
    let canonical = "file:///tmp/draft.md";
    let first = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
    host.initialize(first, &disk, &current).unwrap();

    let reopened = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
    assert_eq!(reopened.recovered_text.as_deref(), Some("base draft"));
    assert!(matches!(reopened.notice, Some(RecoveryNotice::Recovered)));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn conflict_and_corruption_preserve_material_fail_closed() {
    let root = test_root("preserve");
    let host = DraftJournalHost::new(root.clone()).unwrap();
    let (mut store, id, disk) = snapshots("base");
    let current = {
        let _ = store.transact(
            id,
            disk.seq,
            vec![TextEdit {
                range: 4..4,
                insert: " draft".to_string(),
            }],
        );
        store.snapshot(id).unwrap()
    };
    let canonical = "file:///tmp/draft.md";
    let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
    let initialized = host.initialize(decision, &disk, &current).unwrap();
    let original = fs::read(&initialized.path).unwrap();

    let conflict = host.inspect_open(canonical, b"external").unwrap();
    assert!(matches!(
        conflict.notice,
        Some(RecoveryNotice::DiskConflict)
    ));
    let (other_store, _, external) = snapshots("external");
    let preserved = host.initialize(conflict, &external, &external).unwrap();
    let preserved_copy = |n: u32| {
        let stem = preserved.path.file_stem().unwrap().to_str().unwrap();
        root.join(format!("{stem}.preserved-{n}.atdj"))
    };
    assert_eq!(fs::read(preserved_copy(1)).unwrap(), original);
    drop(other_store);

    fs::write(&preserved.path, b"torn").unwrap();
    let corrupt = host.inspect_open(canonical, b"external").unwrap();
    assert!(matches!(corrupt.notice, Some(RecoveryNotice::Corrupt)));
    let kept = host.initialize(corrupt, &external, &external).unwrap();
    assert_eq!(fs::read(preserved_copy(2)).unwrap(), b"torn");
    assert_eq!(kept.path, preserved.path);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_clean_journal_reopened_over_a_changed_file_holds_no_draft() {
    let root = test_root("clean-reopen");
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let canonical = "file:///tmp/draft.md";
    let (_store, id, v0) = snapshots("v0");
    let opened = journals
        .inspect_open(canonical, v0.text.as_bytes())
        .unwrap();
    journals.initialize(opened, &v0, &v0).unwrap();

    let host = DraftJournalHost::new(root.clone()).unwrap();
    let reopened = host.inspect_open(canonical, b"v1").unwrap();
    assert_eq!(reopened.notice, None);
    assert_eq!(reopened.recovered_text, None);
    assert!(!reopened.preserve_existing);
    let (_store, _id, v1) = snapshots("v1");
    let initialized = host.initialize(reopened.clone(), &v1, &v1).unwrap();
    assert_eq!(initialized.notice, None);
    assert!(
        preserved_images(&root).is_empty(),
        "a clean image is replaced, not kept aside"
    );
    let recovered =
        recover_journal_for(initialized.key, &fs::read(&initialized.path).unwrap()).unwrap();
    assert_eq!(recovered.text, "v1");

    let (result, _) = run_reseat(&mut journals, id);
    assert!(
        matches!(
            result,
            JournalReseatResult::Republished {
                preserved: false,
                ..
            }
        ),
        "{result:?}"
    );
    let recovered =
        recover_journal_for(initialized.key, &fs::read(&initialized.path).unwrap()).unwrap();
    assert_eq!((recovered.text.as_str(), recovered.record_count), ("v0", 1));
    let reopened = host.inspect_open(canonical, b"v1").unwrap();
    assert_eq!(reopened.notice, None);
    assert!(!reopened.preserve_existing);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn stale_open_inspection_cannot_replace_a_newer_initialized_journal() {
    let root = test_root("initialize-cas");
    let host = DraftJournalHost::new(root.clone()).unwrap();
    let (_store, _id, disk) = snapshots("base");
    let canonical = "file:///tmp/draft.md";
    let first = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
    let stale = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
    let initialized = host.initialize(first, &disk, &disk).unwrap();
    let before = fs::read(&initialized.path).unwrap();

    let error = host.initialize(stale, &disk, &disk).unwrap_err();
    assert!(
        error.contains("changed after recovery inspection"),
        "{error}"
    );
    assert_eq!(fs::read(&initialized.path).unwrap(), before);
    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn journal_directory_and_file_are_private_and_uri_free() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = test_root("permissions");
    let host = DraftJournalHost::new(root.clone()).unwrap();
    let (_store, _id, disk) = snapshots("private");
    let canonical = "file:///Users//example/Secret%20Draft.md";
    let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
    let initialized = host.initialize(decision, &disk, &disk).unwrap();
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&initialized.path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let name = initialized.path.file_name().unwrap().to_string_lossy();
    assert!(!name.contains("Secret"));
    assert!(!name.contains("Draft"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn latest_edit_survives_inflight_append_and_stale_completion() {
    let root = test_root("latest");
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let (mut store, id, disk) = snapshots("a");
    let decision = journals
        .inspect_open("file:///tmp/draft.md", disk.text.as_bytes())
        .unwrap();
    journals.initialize(decision, &disk, &disk).unwrap();

    let _ = store.transact(
        id,
        disk.seq,
        vec![TextEdit {
            range: 1..1,
            insert: "b".into(),
        }],
    );
    let second = store.snapshot(id).unwrap();
    journals.observe_commit(&second).unwrap();
    let JournalEffect::Append {
        path,
        key,
        plan: first,
    } = journals.next_effect(id).unwrap().unwrap()
    else {
        panic!("expected first append")
    };

    let _ = store.transact(
        id,
        second.seq,
        vec![TextEdit {
            range: 2..2,
            insert: "c".into(),
        }],
    );
    let third = store.snapshot(id).unwrap();
    journals.observe_commit(&third).unwrap();
    assert!(journals.next_effect(id).unwrap().is_none());
    assert_eq!(
        journals.complete_append(
            id,
            crate::native_document_io::JournalGeneration(first.generation.0 + 7),
            JournalAppendResult::Committed(JournalAppendProof {
                record_len: first.bytes.len(),
                encoded_fingerprint: first.encoded_fingerprint,
                published: first.expected_tail.unwrap(),
                file_synced: true,
                landing: JournalLanding::Appended,
            }),
        ),
        JournalCompletion::Stale
    );
    assert!(journals.next_effect(id).unwrap().is_none());

    let committed = execute_journal_append(&path, key, &first, JournalLockPatience::Worker);
    assert_eq!(
        journals.complete_append(id, first.generation, committed),
        JournalCompletion::Durable {
            document: id,
            seq: second.seq
        }
    );
    let JournalEffect::Append {
        path,
        key,
        plan: latest,
    } = journals.next_effect(id).unwrap().unwrap()
    else {
        panic!("latest desired head must be queued")
    };
    let result = execute_journal_append(&path, key, &latest, JournalLockPatience::Worker);
    let completion = journals.complete_append(id, latest.generation, result.clone());
    assert!(
        matches!(completion, JournalCompletion::Durable { seq, .. } if seq == third.seq),
        "latest append must reduce durable at {:?}; got {completion:?} from {result:?}",
        third.seq
    );
    assert_eq!(journals.durable_seq(id), Some(third.seq));
    assert_eq!(
        recover_journal_for(key, &fs::read(path).unwrap())
            .unwrap()
            .text,
        "abc"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn saturated_append_deferral_replans_the_latest_desired_head() {
    let root = test_root("append-deferral");
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let (mut store, id, disk) = snapshots("a");
    let decision = journals
        .inspect_open("file:///tmp/deferred-draft.md", disk.text.as_bytes())
        .unwrap();
    journals.initialize(decision, &disk, &disk).unwrap();

    let _ = store.transact(
        id,
        disk.seq,
        vec![TextEdit {
            range: 1..1,
            insert: "b".into(),
        }],
    );
    let first_desired = store.snapshot(id).unwrap();
    journals.observe_commit(&first_desired).unwrap();
    let JournalEffect::Append { plan: first, .. } = journals.next_effect(id).unwrap().unwrap()
    else {
        panic!("first append expected")
    };

    assert!(journals.defer_append(id, first.generation));
    let JournalEffect::Append {
        plan: same_head_retry,
        ..
    } = journals.next_effect(id).unwrap().unwrap()
    else {
        panic!("deferred append must be replanned without another edit")
    };
    assert_ne!(same_head_retry.generation, first.generation);
    assert_eq!(same_head_retry.target_seq, first_desired.seq);

    let _ = store.transact(
        id,
        first_desired.seq,
        vec![TextEdit {
            range: 2..2,
            insert: "c".into(),
        }],
    );
    let latest = store.snapshot(id).unwrap();
    journals.observe_commit(&latest).unwrap();
    assert!(journals.defer_append(id, same_head_retry.generation));
    let entry = journals.entries.get(&id).unwrap();
    assert!(entry.append_text.is_none());
    assert!(matches!(
        entry.reducer.phase(),
        crate::native_document_io::JournalPhase::Idle
    ));

    let JournalEffect::Append {
        plan: latest_retry, ..
    } = journals.next_effect(id).unwrap().unwrap()
    else {
        panic!("deferred append must be replanned")
    };
    assert_ne!(latest_retry.generation, same_head_retry.generation);
    assert_eq!(latest_retry.base_durable, first.base_durable);
    assert_eq!(latest_retry.target_seq, latest.seq);
    assert!(Arc::ptr_eq(
        journals
            .entries
            .get(&id)
            .unwrap()
            .append_text
            .as_ref()
            .unwrap(),
        &latest.text
    ));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn saturated_rewrite_deferral_restores_checkpoint_intent() {
    let root = test_root("rewrite-deferral");
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let (mut store, id, disk) = snapshots("base");
    let decision = journals
        .inspect_open("file:///tmp/deferred-checkpoint.md", disk.text.as_bytes())
        .unwrap();
    journals.initialize(decision, &disk, &disk).unwrap();
    let _ = store.transact(
        id,
        disk.seq,
        vec![TextEdit {
            range: disk.text.len()..disk.text.len(),
            insert: " saved".into(),
        }],
    );
    let saved = store.snapshot(id).unwrap();
    journals.observe_commit(&saved).unwrap();
    journals
        .request_checkpoint(
            DurableCheckpoint {
                document: id,
                seq: saved.seq,
                source: DurableSource::AtomicFile,
            },
            saved.text.clone(),
        )
        .unwrap();

    let JournalEffect::Rewrite { plan: first, .. } = journals.next_effect(id).unwrap().unwrap()
    else {
        panic!("checkpoint rewrite expected")
    };
    assert!(journals.defer_rewrite(id, first.generation));
    let entry = journals.entries.get(&id).unwrap();
    assert!(entry.rewrite_inflight.is_none());
    assert!(Arc::ptr_eq(
        &entry.pending_checkpoint.as_ref().unwrap().text,
        &saved.text
    ));

    let JournalEffect::Rewrite { plan: retry, .. } = journals.next_effect(id).unwrap().unwrap()
    else {
        panic!("deferred checkpoint must be replanned")
    };
    assert_ne!(retry.generation, first.generation);
    assert_eq!(retry.base_durable, first.base_durable);
    assert_eq!(retry.target_seq, first.target_seq);
    assert_eq!(retry.fingerprint, first.fingerprint);
    assert_eq!(retry.bytes, first.bytes);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn checkpoint_rewrite_is_proof_gated_and_keeps_newer_draft() {
    let root = test_root("checkpoint");
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let (mut store, id, disk) = snapshots("base");
    let decision = journals
        .inspect_open("file:///tmp/draft.md", disk.text.as_bytes())
        .unwrap();
    journals.initialize(decision, &disk, &disk).unwrap();
    let _ = store.transact(
        id,
        disk.seq,
        vec![TextEdit {
            range: 4..4,
            insert: " saved".into(),
        }],
    );
    let saved = store.snapshot(id).unwrap();
    journals.observe_commit(&saved).unwrap();
    let _ = store.transact(
        id,
        saved.seq,
        vec![TextEdit {
            range: saved.text.len()..saved.text.len(),
            insert: " newer".into(),
        }],
    );
    let newer = store.snapshot(id).unwrap();
    journals.observe_commit(&newer).unwrap();
    assert!(
        journals
            .request_checkpoint(
                DurableCheckpoint {
                    document: id,
                    seq: saved.seq,
                    source: DurableSource::DraftJournal,
                },
                saved.text.clone(),
            )
            .unwrap_err()
            .contains("verified file")
    );
    journals
        .request_checkpoint(
            DurableCheckpoint {
                document: id,
                seq: saved.seq,
                source: DurableSource::AtomicFile,
            },
            saved.text.clone(),
        )
        .unwrap();
    let JournalEffect::Rewrite { path, plan } = journals.next_effect(id).unwrap().unwrap() else {
        panic!("checkpoint rewrite expected")
    };
    let mut bad = match execute_journal_rewrite(&path, &plan, JournalLockPatience::Worker) {
        JournalRewriteResult::Committed(proof) => proof,
        other => panic!("rewrite failed: {other:?}"),
    };
    bad.directory_synced = false;
    assert!(matches!(
        journals.complete_rewrite(id, plan.generation, JournalRewriteResult::Committed(bad)),
        JournalCompletion::Failed { .. }
    ));

    journals
        .request_checkpoint(
            DurableCheckpoint {
                document: id,
                seq: saved.seq,
                source: DurableSource::AtomicFile,
            },
            saved.text.clone(),
        )
        .unwrap();
    let JournalEffect::Rewrite { path, plan } = journals.next_effect(id).unwrap().unwrap() else {
        panic!("checkpoint retry expected")
    };
    let result = execute_journal_rewrite(&path, &plan, JournalLockPatience::Worker);
    assert_eq!(
        journals.complete_rewrite(id, plan.generation, result),
        JournalCompletion::Durable {
            document: id,
            seq: newer.seq
        }
    );
    let recovered = recover_journal_for(
        journals.key_for("file:///tmp/draft.md"),
        &fs::read(path).unwrap(),
    )
    .unwrap();
    assert_eq!(recovered.text, "base saved newer");
    assert_eq!(
        recovered.base_content,
        ContentFingerprint::of(saved.text.as_bytes())
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn stale_checkpoint_rewrite_cannot_erase_a_concurrent_append() {
    let root = test_root("rewrite-cas");
    let host = DraftJournalHost::new(root.clone()).unwrap();
    let (mut store, id, disk) = snapshots("base");
    let canonical = "file:///tmp/draft.md";
    let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
    let initialized = host.initialize(decision, &disk, &disk).unwrap();
    let rewrite = checkpoint_plan(
        initialized.key,
        JournalRewriteGeneration(1),
        disk.seq,
        initialized.tail.fingerprint(),
        &disk,
        &disk,
    )
    .unwrap();

    let _ = store.transact(
        id,
        disk.seq,
        vec![TextEdit {
            range: 4..4,
            insert: " newer".to_string(),
        }],
    );
    let newer = store.snapshot(id).unwrap();
    let mut reducer = DraftJournalReducer::new_with_key(id, initialized.key, disk.seq);
    let mut append = reducer.plan_snapshot(&newer).unwrap();
    append.expected_tail = Some(initialized.tail);
    assert!(matches!(
        execute_journal_append(
            &initialized.path,
            initialized.key,
            &append,
            JournalLockPatience::Worker,
        ),
        JournalAppendResult::Committed(_)
    ));

    assert!(matches!(
        execute_journal_rewrite(&initialized.path, &rewrite, JournalLockPatience::Worker),
        JournalRewriteResult::Failed(message) if message.contains("image changed")
    ));
    let recovered =
        recover_journal_for(initialized.key, &fs::read(&initialized.path).unwrap()).unwrap();
    assert_eq!(recovered.durable_seq, newer.seq);
    assert_eq!(recovered.text, "base newer");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn same_sequence_different_journal_image_is_rejected() {
    let root = test_root("same-seq-aba");
    let host = DraftJournalHost::new(root.clone()).unwrap();
    let (mut store, id, disk) = snapshots("base");
    let canonical = "file:///tmp/draft.md";
    let decision = host.inspect_open(canonical, disk.text.as_bytes()).unwrap();
    let initialized = host.initialize(decision, &disk, &disk).unwrap();
    let _ = store.transact(
        id,
        disk.seq,
        vec![TextEdit {
            range: 4..4,
            insert: " ours".to_string(),
        }],
    );
    let mut reducer = DraftJournalReducer::new_with_key(id, initialized.key, disk.seq);
    let mut plan = reducer.plan_snapshot(&store.snapshot(id).unwrap()).unwrap();
    plan.expected_tail = Some(initialized.tail);

    let other = synthetic_snapshot(id, disk.seq, Arc::from("other"));
    let other_image =
        encode_journal(&[JournalRecord::snapshot_for(initialized.key, &other)]).unwrap();
    fs::write(&initialized.path, &other_image).unwrap();
    assert!(matches!(
        execute_journal_append(
            &initialized.path,
            initialized.key,
            &plan,
            JournalLockPatience::Worker,
        ),
        JournalAppendResult::Failed { message, .. } if message.contains("image changed")
    ));
    assert_eq!(fs::read(&initialized.path).unwrap(), other_image);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cross_process_same_journal_generation_has_one_winner() {
    const CHILD: &str = "ATERM_JOURNAL_COMMIT_TEST_CHILD";
    const HANG: std::time::Duration = std::time::Duration::from_secs(60);
    const URI: &str = "file:///tmp/cross-process-draft.md";
    if let Ok(role) = std::env::var(CHILD) {
        let path = PathBuf::from(std::env::var("ATERM_JOURNAL_COMMIT_TEST_PATH").unwrap());
        let root = path.parent().unwrap();
        let key = DraftJournalHost::new(root.to_path_buf())
            .unwrap()
            .key_for(URI);
        let (mut store, id, disk) = snapshots("base");
        let _ = store.transact(
            id,
            disk.seq,
            vec![TextEdit {
                range: 4..4,
                insert: format!(" {role}"),
            }],
        );
        let mut reducer = DraftJournalReducer::new_with_key(id, key, disk.seq);
        let mut plan = reducer.plan_snapshot(&store.snapshot(id).unwrap()).unwrap();
        plan.expected_tail = Some(journal_tail(&fs::read(&path).unwrap()).unwrap());
        fs::write(root.join(format!("ready-{role}")), b"ready").unwrap();
        let go = root.join("go");
        let deadline = std::time::Instant::now() + HANG;
        while !go.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "parent race timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let retry_deadline = std::time::Instant::now() + HANG;
        let verdict = loop {
            match execute_journal_append(&path, key, &plan, JournalLockPatience::Worker) {
                JournalAppendResult::Committed(_) => break "committed",
                JournalAppendResult::Failed { message, .. } if message.contains("busy") => {
                    assert!(
                        std::time::Instant::now() < retry_deadline,
                        "journal lock remained busy past the bounded retry window"
                    );
                    aterm_uds::deadline::poll_yield();
                }
                JournalAppendResult::Failed { message, .. } => {
                    assert!(message.contains("image changed"), "{message}");
                    break "conflict";
                }
                JournalAppendResult::Cancelled => panic!("journal child was cancelled"),
            }
        };
        fs::write(root.join(format!("result-{role}")), verdict).unwrap();
        return;
    }

    let root = test_root("process-race");
    let host = DraftJournalHost::new(root.clone()).unwrap();
    let (_store, _id, disk) = snapshots("base");
    let decision = host.inspect_open(URI, disk.text.as_bytes()).unwrap();
    let initialized = host.initialize(decision, &disk, &disk).unwrap();
    let mut children: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|role| {
            let mut child = crate::test_alone::StandIn::this_test();
            child
                .command()
                .env(CHILD, role)
                .env("ATERM_JOURNAL_COMMIT_TEST_PATH", &initialized.path);
            child.spawn()
        })
        .collect();
    crate::test_alone::until_or_ended("both racers reach the barrier", &mut children, || {
        ["one", "two"]
            .iter()
            .all(|role| root.join(format!("ready-{role}")).exists())
    });
    fs::write(root.join("go"), b"go").unwrap();
    for child in children {
        child.passed();
    }
    let results =
        ["one", "two"].map(|role| fs::read_to_string(root.join(format!("result-{role}"))).unwrap());
    assert_eq!(
        results
            .iter()
            .filter(|result| *result == "committed")
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| *result == "conflict")
            .count(),
        1
    );
    let recovered =
        recover_journal_for(initialized.key, &fs::read(&initialized.path).unwrap()).unwrap();
    assert!(recovered.text == "base one" || recovered.text == "base two");

    let model = aterm_models_native::native_draft_journal_model();
    let edited = model.successors("Edit", &model.init_state())[0].clone();
    let begun = model.successors("BeginJournal", &edited)[0].clone();
    let external = model.successors("ExternalJournalCommit", &begun)[0].clone();
    let rejected = model.successors("RejectJournalDiskConflict", &external)[0].clone();
    assert_eq!(rejected["disk_conflict_rejected"], 1);
    assert!(model.check_invariant("JournalImageCas", &rejected));
    assert!(model.successors("AcceptJournal", &external).is_empty());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn single_edit_matches_the_char_scan_definition_on_multi_byte_boundaries() {
    fn reference(before: &str, after: &str) -> JournalEdit {
        let mut prefix = 0;
        for (left, right) in before.chars().zip(after.chars()) {
            if left != right {
                break;
            }
            prefix += left.len_utf8();
        }
        let mut suffix = 0;
        let before_tail = &before[prefix..];
        let after_tail = &after[prefix..];
        for (left, right) in before_tail.chars().rev().zip(after_tail.chars().rev()) {
            if left != right {
                break;
            }
            suffix += left.len_utf8();
        }
        JournalEdit {
            range: prefix..before.len().saturating_sub(suffix),
            insert: after[prefix..after.len().saturating_sub(suffix)].to_string(),
        }
    }

    const ALPHABET: [&str; 8] = ["a", "b", "é", "è", "→", "←", "🙂", "🙁"];
    let mut cases: Vec<(String, String)> = vec![
        (String::new(), String::new()),
        (String::new(), "🙂".to_string()),
        ("🙂".to_string(), String::new()),
        ("🙂é→".to_string(), "🙁é→".to_string()),
        ("→é🙂".to_string(), "→é🙁".to_string()),
        ("aéb".to_string(), "aèb".to_string()),
        ("prefix🙂".to_string(), "prefix🙂suffix".to_string()),
        ("🙂tail".to_string(), "🙂more tail".to_string()),
    ];
    let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
    let mut draw = |modulus: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((seed >> 33) % modulus) as usize
    };
    for _ in 0..512 {
        let len = draw(7);
        let before: Vec<&str> = (0..len).map(|_| ALPHABET[draw(8)]).collect();
        let mut after = before.clone();
        match draw(3) {
            0 if !after.is_empty() => {
                let at = draw(after.len() as u64);
                after[at] = ALPHABET[draw(8)];
            }
            1 => {
                let at = draw(after.len() as u64 + 1);
                after.insert(at, ALPHABET[draw(8)]);
            }
            _ if !after.is_empty() => {
                let at = draw(after.len() as u64);
                after.remove(at);
            }
            _ => {}
        }
        cases.push((before.concat(), after.concat()));
    }

    for (before, after) in cases {
        assert_eq!(
            single_edit(&before, &after),
            reference(&before, &after),
            "single_edit diverged on {before:?} -> {after:?}"
        );
    }
}

pub(super) fn journaled_edit(
    name: &str,
) -> (
    PathBuf,
    DocumentJournalStore,
    DocumentStore,
    DocumentId,
    DocumentSnapshot,
    PathBuf,
    JournalDocumentKey,
) {
    let root = test_root(name);
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let (mut store, id, disk) = snapshots("disk");
    let decision = journals
        .inspect_open("file:///tmp/draft.md", disk.text.as_bytes())
        .unwrap();
    journals.initialize(decision, &disk, &disk).unwrap();
    let _ = store.transact(
        id,
        disk.seq,
        vec![TextEdit {
            range: 4..4,
            insert: "+".into(),
        }],
    );
    journals
        .observe_commit(&store.snapshot(id).unwrap())
        .unwrap();
    let JournalEffect::Append { path, key, plan } = journals.next_effect(id).unwrap().unwrap()
    else {
        panic!("the edit is appended")
    };
    let result = execute_journal_append(&path, key, &plan, JournalLockPatience::Worker);
    assert!(matches!(
        journals.complete_append(id, plan.generation, result),
        JournalCompletion::Durable { .. }
    ));
    (root, journals, store, id, disk, path, key)
}

fn run_reseat(
    journals: &mut DocumentJournalStore,
    id: DocumentId,
) -> (JournalReseatResult, JournalCompletion) {
    assert_eq!(journals.owe_reseat_all(), vec![id]);
    let JournalEffect::Reseat { path, plan } = journals.next_effect(id).unwrap().unwrap() else {
        panic!("an owed re-seat runs before anything else")
    };
    let result = execute_journal_reseat(&path, &plan, JournalLockPatience::Worker);
    let completion = journals.complete_reseat(id, plan.generation, result.clone());
    (result, completion)
}

pub(super) fn preserved_images(root: &Path) -> Vec<Vec<u8>> {
    let mut preserved = fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".preserved-"))
        .map(|entry| fs::read(entry.path()).unwrap())
        .collect::<Vec<_>>();
    preserved.sort();
    preserved
}

#[test]
fn a_reseat_leaves_this_stores_own_image_alone() {
    let (root, mut journals, _store, id, _disk, path, _key) = journaled_edit("reseat-own");
    let before = fs::read(&path).unwrap();
    let (result, completion) = run_reseat(&mut journals, id);
    assert_eq!(result, JournalReseatResult::Unchanged);
    assert_eq!(completion, JournalCompletion::Stale);
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!journals.reseat_owed(id));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_reseat_takes_back_a_successors_republication() {
    let (root, mut journals, mut store, id, disk, path, key) = journaled_edit("reseat-successor");
    let head = store.snapshot(id).unwrap();
    let mut successor = DocumentJournalStore::for_test(root.clone()).unwrap();
    let (mut theirs, their_id, their_disk) = snapshots("disk");
    let decision = successor
        .inspect_open("file:///tmp/draft.md", their_disk.text.as_bytes())
        .unwrap();
    assert_eq!(decision.recovered_text.as_deref(), Some("disk+"));
    let _ = theirs.transact(
        their_id,
        their_disk.seq,
        vec![TextEdit {
            range: 0..4,
            insert: "disk+".into(),
        }],
    );
    let their_current = theirs.snapshot(their_id).unwrap();
    successor
        .initialize(decision, &their_disk, &their_current)
        .unwrap();
    let republished = fs::read(&path).unwrap();

    let _ = store.transact(
        id,
        head.seq,
        vec![TextEdit {
            range: 5..5,
            insert: "!".into(),
        }],
    );
    journals
        .observe_commit(&store.snapshot(id).unwrap())
        .unwrap();
    let JournalEffect::Append { plan, .. } = journals.next_effect(id).unwrap().unwrap() else {
        panic!("an append")
    };
    let refused = execute_journal_append(&path, key, &plan, JournalLockPatience::Worker);
    assert!(matches!(
        journals.complete_append(id, plan.generation, refused),
        JournalCompletion::Failed { .. }
    ));
    assert_eq!(fs::read(&path).unwrap(), republished, "and changes nothing");

    let (result, completion) = run_reseat(&mut journals, id);
    let JournalReseatResult::Republished {
        baseline,
        preserved,
        ..
    } = result
    else {
        panic!("the successor's image is replaced: {result:?}")
    };
    assert!(!preserved, "the successor's image held nothing new");
    assert_eq!(baseline, ContentFingerprint::of(disk.text.as_bytes()));
    let latest = store.snapshot(id).unwrap();
    assert_eq!(
        completion,
        JournalCompletion::Durable {
            document: id,
            seq: latest.seq
        }
    );
    let recovered = recover_journal_for(key, &fs::read(&path).unwrap()).unwrap();
    assert_eq!(recovered.text, "disk+!", "the latest edit is durable");
    assert_eq!(
        recovered.base_content,
        ContentFingerprint::of(b"disk"),
        "over the file it was made against"
    );
    assert!(preserved_images(&root).is_empty());

    let _ = store.transact(
        id,
        latest.seq,
        vec![TextEdit {
            range: 6..6,
            insert: "?".into(),
        }],
    );
    journals
        .observe_commit(&store.snapshot(id).unwrap())
        .unwrap();
    let JournalEffect::Append { plan, .. } = journals.next_effect(id).unwrap().unwrap() else {
        panic!("an append")
    };
    let landed = execute_journal_append(&path, key, &plan, JournalLockPatience::Worker);
    assert!(matches!(
        journals.complete_append(id, plan.generation, landed),
        JournalCompletion::Durable { .. }
    ));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_reseat_preserves_what_it_never_had() {
    let (root, mut journals, store, id, _disk, path, key) = journaled_edit("reseat-foreign");
    let head = store.snapshot(id).unwrap();
    let foreign = encode_journal(&[
        JournalRecord::snapshot_for(key, &synthetic_snapshot(id, Seq(1), Arc::from("disk"))),
        JournalRecord::snapshot_for(
            key,
            &synthetic_snapshot(id, Seq(2), Arc::from("someone else's draft")),
        ),
    ])
    .unwrap();
    fs::write(&path, &foreign).unwrap();
    let recording = aterm_durable::seam::Recording::start();
    let (result, _) = run_reseat(&mut journals, id);
    #[cfg(unix)]
    let expected_reseat = [Flush::Ordered, Flush::Durable, Flush::Durable];
    #[cfg(not(unix))]
    let expected_reseat = [Flush::Durable];
    assert_eq!(
        recording.take(),
        expected_reseat,
        "the re-seat's file is durable; Unix also flushes the set-aside link and publication"
    );
    drop(recording);
    assert!(
        matches!(
            result,
            JournalReseatResult::Republished {
                preserved: true,
                ..
            }
        ),
        "{result:?}"
    );
    assert_eq!(preserved_images(&root), vec![foreign]);
    let recovered = recover_journal_for(key, &fs::read(&path).unwrap()).unwrap();
    assert_eq!(recovered.text, *head.text);
    assert_eq!(recovered.base_content, ContentFingerprint::of(b"disk"));

    let other_base = encode_journal(&[JournalRecord::snapshot_for(
        key,
        &synthetic_snapshot(id, Seq(1), Arc::from("changed on disk")),
    )])
    .unwrap();
    for round in 1..=2 {
        fs::write(&path, &other_base).unwrap();
        let (result, _) = run_reseat(&mut journals, id);
        let JournalReseatResult::Republished {
            baseline,
            preserved,
            ..
        } = result
        else {
            panic!("round {round}: {result:?}")
        };
        assert!(!preserved, "round {round}");
        assert_eq!(baseline, ContentFingerprint::of(head.text.as_bytes()));
        let reopened = journals
            .inspect_open("file:///tmp/draft.md", b"changed on disk")
            .unwrap();
        assert_eq!(
            reopened.notice,
            Some(RecoveryNotice::DiskConflict),
            "round {round}"
        );
        assert!(reopened.preserve_existing, "round {round}");
    }
    assert_eq!(preserved_images(&root).len(), 1);

    let reseated = JournalRecord::snapshot_for(
        key,
        &synthetic_snapshot(id, Seq(3), Arc::from("their re-seated draft")),
    );
    let theirs = encode_journal(&[reseated.clone(), reseated]).unwrap();
    fs::write(&path, &theirs).unwrap();
    let (result, _) = run_reseat(&mut journals, id);
    assert!(
        matches!(
            result,
            JournalReseatResult::Republished {
                preserved: true,
                ..
            }
        ),
        "{result:?}"
    );
    let preserved = preserved_images(&root);
    assert_eq!(preserved.len(), 2);
    assert!(preserved.contains(&theirs));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_save_after_a_reseat_makes_the_baseline_the_file_again() {
    let (root, mut journals, store, id, _disk, path, key) = journaled_edit("save-after-reseat");
    let head = store.snapshot(id).unwrap();
    let other_base = encode_journal(&[JournalRecord::snapshot_for(
        key,
        &synthetic_snapshot(id, Seq(1), Arc::from("changed on disk")),
    )])
    .unwrap();
    fs::write(&path, &other_base).unwrap();
    let _ = run_reseat(&mut journals, id);
    journals
        .request_checkpoint(
            DurableCheckpoint {
                document: id,
                seq: head.seq,
                source: DurableSource::AtomicFile,
            },
            head.text.clone(),
        )
        .unwrap();
    let JournalEffect::Rewrite { path: rpath, plan } = journals.next_effect(id).unwrap().unwrap()
    else {
        panic!("the save checkpoints the journal")
    };
    let result = execute_journal_rewrite(&rpath, &plan, JournalLockPatience::Worker);
    assert!(matches!(
        journals.complete_rewrite(id, plan.generation, result),
        JournalCompletion::Durable { .. }
    ));
    let theirs = encode_journal(&[JournalRecord::snapshot_for(
        key,
        &synthetic_snapshot(id, Seq(1), head.text.clone()),
    )])
    .unwrap();
    fs::write(&path, &theirs).unwrap();
    let _ = run_reseat(&mut journals, id);
    let recovered = recover_journal_for(key, &fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        recovered.record_count, 1,
        "a clean re-seat writes one snapshot"
    );
    let reopened = journals
        .inspect_open("file:///tmp/draft.md", b"pulled")
        .unwrap();
    assert_eq!(reopened.notice, None, "a saved document holds no draft");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_fenced_reseat_holds_every_effect() {
    let (root, mut journals, mut store, id, _disk, _path, _key) = journaled_edit("reseat-fenced");
    let head = store.snapshot(id).unwrap();
    journals.owe_reseat_all();
    journals.set_reseat_fenced(true);
    let _ = store.transact(
        id,
        head.seq,
        vec![TextEdit {
            range: 0..0,
            insert: ">".into(),
        }],
    );
    journals
        .observe_commit(&store.snapshot(id).unwrap())
        .unwrap();
    assert!(journals.next_effect(id).unwrap().is_none());
    assert_eq!(
        journals
            .carry_candidate(&store.snapshot(id).unwrap())
            .unwrap_err(),
        DraftCarryRefusal::Reseating
    );
    journals.set_reseat_fenced(false);
    assert!(matches!(
        journals.next_effect(id).unwrap(),
        Some(JournalEffect::Reseat { .. })
    ));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_failed_reseat_is_refused_by_name_until_a_retry_lands() {
    let (root, mut journals, store, id, _disk, path, _key) = journaled_edit("reseat-failed");
    let head = store.snapshot(id).unwrap();
    journals.owe_reseat_all();
    let JournalEffect::Reseat { plan, .. } = journals.next_effect(id).unwrap().unwrap() else {
        panic!("a re-seat")
    };
    let held = hold_journal_lock_for_test(&path);
    let refused = execute_journal_reseat(&path, &plan, JournalLockPatience::EventLoop);
    assert!(
        matches!(refused, JournalReseatResult::Failed(_)),
        "{refused:?}"
    );
    assert!(matches!(
        journals.complete_reseat(id, plan.generation, refused),
        JournalCompletion::Failed { .. }
    ));
    assert_eq!(
        journals.carry_candidate(&head).unwrap_err(),
        DraftCarryRefusal::ReseatFailed
    );
    let JournalEffect::Reseat { plan, .. } = journals.next_effect(id).unwrap().unwrap() else {
        panic!("the re-seat is owed again")
    };
    assert_eq!(
        journals.carry_candidate(&head).unwrap_err(),
        DraftCarryRefusal::ReseatFailed
    );
    drop(held);
    let landed = execute_journal_reseat(&path, &plan, JournalLockPatience::Worker);
    assert_eq!(landed, JournalReseatResult::Unchanged);
    journals.complete_reseat(id, plan.generation, landed);
    assert!(journals.carry_candidate(&head).is_ok());
    let _ = fs::remove_dir_all(root);
}

fn journal_a_draft(
    journals: &mut DocumentJournalStore,
    store: &mut DocumentStore,
    uri: &str,
    draft: &str,
) -> PathBuf {
    let id = store.open(uri.to_string(), String::new());
    let disk = store.snapshot(id).unwrap();
    let decision = journals.inspect_open(uri, b"").unwrap();
    assert_eq!(decision.recovered_text, None, "{uri} starts with no draft");
    journals.initialize(decision, &disk, &disk).unwrap();
    let _ = store.transact(
        id,
        disk.seq,
        vec![TextEdit {
            range: 0..0,
            insert: draft.to_string(),
        }],
    );
    journals
        .observe_commit(&store.snapshot(id).unwrap())
        .unwrap();
    let JournalEffect::Append { path, key, plan } = journals.next_effect(id).unwrap().unwrap()
    else {
        panic!("the draft is appended")
    };
    let result = execute_journal_append(&path, key, &plan, JournalLockPatience::Worker);
    assert!(matches!(
        journals.complete_append(id, plan.generation, result),
        JournalCompletion::Durable { .. }
    ));
    path
}

#[test]
fn colliding_legacy_names_never_share_a_journal() {
    const A: &str = "file:///Users//alice/Downloads/7ykhvtpwaxxug.txt";
    const B: &str = "file:///Users//alice/Downloads/vwelffjwxnsmb.txt";
    assert_eq!(
        JournalDocumentKey::legacy_unkeyed(A),
        JournalDocumentKey(0x0307_5386_33bd_1a46)
    );
    assert_eq!(
        JournalDocumentKey::legacy_unkeyed(B),
        JournalDocumentKey::legacy_unkeyed(A)
    );
    let root = test_root("legacy-collision");
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let mut store = crate::test_document_store();
    let a_path = journal_a_draft(&mut journals, &mut store, A, "A's unsaved draft");
    drop(journals);
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let opened_b = journals.inspect_open(B, b"").unwrap();
    assert_ne!(opened_b.path, a_path, "B must not name A's journal");
    assert_eq!(
        opened_b.recovered_text, None,
        "B must not recover A's draft"
    );
    assert_eq!(opened_b.notice, None);
    let mut store = crate::test_document_store();
    let b_path = journal_a_draft(&mut journals, &mut store, B, "B's unsaved draft");
    assert_ne!(b_path, a_path);
    drop(journals);
    let journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let opened_a = journals.inspect_open(A, b"").unwrap();
    assert_eq!(opened_a.path, a_path);
    assert_eq!(
        opened_a.recovered_text.as_deref(),
        Some("A's unsaved draft")
    );
    let opened_b = journals.inspect_open(B, b"").unwrap();
    assert_eq!(opened_b.path, b_path);
    assert_eq!(
        opened_b.recovered_text.as_deref(),
        Some("B's unsaved draft")
    );
    let _ = fs::remove_dir_all(root);
}

fn write_legacy_journal(root: &Path, uri: &str, disk: &str, draft: &str) -> PathBuf {
    let typed: &[&str] = if draft == disk { &[] } else { &[draft] };
    write_legacy_journal_typing(root, uri, disk, typed)
}

fn write_legacy_journal_typing(root: &Path, uri: &str, disk: &str, texts: &[&str]) -> PathBuf {
    let key = JournalDocumentKey::legacy_unkeyed(uri);
    let mut store = crate::test_document_store();
    let id = store.open(uri.to_string(), disk.to_string());
    let base = store.snapshot(id).unwrap();
    let mut records = vec![JournalRecord::snapshot_for(key, &base)];
    let (mut seq, mut text) = (base.seq, disk);
    for &next in texts {
        let after = Seq(seq.0 + 1);
        records.push(
            JournalRecord::delta_for(key, seq, after, vec![single_edit(text, next)]).unwrap(),
        );
        (seq, text) = (after, next);
    }
    let path = root.join(format!("{:016x}.atdj", key.0));
    fs::write(&path, encode_journal_v1(&records).unwrap()).unwrap();
    path
}

pub(super) fn open_as_the_app_does(
    journals: &mut DocumentJournalStore,
    store: &mut DocumentStore,
    uri: &str,
    disk: &str,
) -> (JournalOpenDecision, DocumentId, InitializedJournal) {
    let id = store.open(uri.to_string(), disk.to_string());
    let base = store.snapshot(id).unwrap();
    let decision = journals.inspect_open(uri, disk.as_bytes()).unwrap();
    if let Some(recovered) = decision.recovered_text.clone() {
        let _ = store.transact(
            id,
            base.seq,
            vec![TextEdit {
                range: 0..base.text.len(),
                insert: recovered,
            }],
        );
    }
    let current = store.snapshot(id).unwrap();
    let initialized = journals
        .initialize(decision.clone(), &base, &current)
        .unwrap();
    (decision, id, initialized)
}

pub(super) fn type_and_append(
    journals: &mut DocumentJournalStore,
    store: &mut DocumentStore,
    id: DocumentId,
    typed: &str,
) -> (JournalAppendPlan, JournalCompletion) {
    let head = store.snapshot(id).unwrap();
    let _ = store.transact(
        id,
        head.seq,
        vec![TextEdit {
            range: head.text.len()..head.text.len(),
            insert: typed.to_string(),
        }],
    );
    journals
        .observe_commit(&store.snapshot(id).unwrap())
        .unwrap();
    let JournalEffect::Append { path, key, plan } = journals.next_effect(id).unwrap().unwrap()
    else {
        panic!("the edit is appended")
    };
    let result = execute_journal_append(&path, key, &plan, JournalLockPatience::Worker);
    let completion = journals.complete_append(id, plan.generation, result);
    (plan, completion)
}

#[test]
fn a_legacy_journal_is_recovered_and_retired_by_the_first_write() {
    const URI: &str = "file:///tmp/legacy-migration.md";
    let root = test_root("legacy-migration");
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let legacy = write_legacy_journal(&root, URI, "disk", "disk and a draft");
    let mut store = crate::test_document_store();
    let (decision, id, initialized) = open_as_the_app_does(&mut journals, &mut store, URI, "disk");
    assert_eq!(decision.recovered_text.as_deref(), Some("disk and a draft"));
    assert_eq!(initialized.notice, Some(RecoveryNotice::Recovered));
    assert_ne!(initialized.path, legacy);
    let keyed =
        recover_journal_for(journals.key_for(URI), &fs::read(&initialized.path).unwrap()).unwrap();
    assert_eq!(keyed.text, "disk and a draft");
    assert_eq!(keyed.base_content, ContentFingerprint::of(b"disk"));
    verify_carried_draft(
        &DraftCarryCandidate {
            path: legacy.clone(),
            key: JournalDocumentKey::legacy_unkeyed(URI),
        },
        "disk and a draft",
        ContentFingerprint::of(b"disk"),
    )
    .expect("an outgoing older aterm still finds its carried draft");

    let (plan, completion) = type_and_append(&mut journals, &mut store, id, "!");
    assert!(plan.retire_legacy.is_some(), "the first write owes it");
    assert!(matches!(completion, JournalCompletion::Durable { .. }));
    assert!(
        !legacy.exists(),
        "retired before the keyed journal moved on"
    );
    let (plan, completion) = type_and_append(&mut journals, &mut store, id, "?");
    assert_eq!(plan.retire_legacy, None, "and owed no more");
    assert!(matches!(completion, JournalCompletion::Durable { .. }));

    drop(journals);
    let journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let reopened = journals.inspect_open(URI, b"disk").unwrap();
    assert_eq!(
        reopened.recovered_text.as_deref(),
        Some("disk and a draft!?")
    );
    assert_eq!(preserved_images(&root), Vec::<Vec<u8>>::new());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_legacy_journal_rewritten_after_a_migration_wins_and_the_keyed_one_is_kept() {
    const URI: &str = "file:///tmp/legacy-rollback.md";
    let root = test_root("legacy-rollback");
    let legacy = write_legacy_journal(&root_with_dir(&root), URI, "disk", "disk v1");
    let mut successor = DocumentJournalStore::for_test(root.clone()).unwrap();
    let mut store = crate::test_document_store();
    let (_, _, first) = open_as_the_app_does(&mut successor, &mut store, URI, "disk");
    let migrated = fs::read(&first.path).unwrap();
    drop(successor);
    write_legacy_journal(&root, URI, "disk", "disk v2");

    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let mut store = crate::test_document_store();
    let (decision, _, _) = open_as_the_app_does(&mut journals, &mut store, URI, "disk");
    assert_eq!(decision.recovered_text.as_deref(), Some("disk v2"));
    assert_eq!(preserved_images(&root), vec![migrated]);
    assert!(legacy.exists(), "a carried draft's legacy file stays");
    let _ = fs::remove_dir_all(root);
}

fn root_with_dir(root: &Path) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    root.to_path_buf()
}

#[test]
fn a_legacy_journal_changed_after_the_read_refuses_the_retiring_write() {
    const URI: &str = "file:///tmp/legacy-changed.md";
    let root = test_root("legacy-changed");
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let legacy = write_legacy_journal(&root, URI, "disk", "disk v1");
    let mut store = crate::test_document_store();
    let (_, id, initialized) = open_as_the_app_does(&mut journals, &mut store, URI, "disk");
    let keyed = fs::read(&initialized.path).unwrap();
    write_legacy_journal(&root, URI, "disk", "disk v2");
    let newer = fs::read(&legacy).unwrap();

    let (_, completion) = type_and_append(&mut journals, &mut store, id, "!");
    let JournalCompletion::Failed { message, .. } = completion else {
        panic!("the retiring write refuses: {completion:?}")
    };
    assert!(message.contains("changed after it was read"), "{message}");
    assert_eq!(fs::read(&legacy).unwrap(), newer);
    assert_eq!(fs::read(&initialized.path).unwrap(), keyed);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_legacy_journal_with_nothing_to_carry_is_retired_at_the_open() {
    const CLEAN: &str = "file:///tmp/legacy-clean.md";
    const CONFLICT: &str = "file:///tmp/legacy-conflict.md";
    let root = test_root("legacy-retired-at-open");
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let clean = write_legacy_journal(&root, CLEAN, "disk", "disk");
    let conflict = write_legacy_journal(&root, CONFLICT, "old disk", "old disk and a draft");
    let conflict_image = fs::read(&conflict).unwrap();
    let mut store = crate::test_document_store();

    let recording = aterm_durable::seam::Recording::start();
    let (decision, _, initialized) = open_as_the_app_does(&mut journals, &mut store, CLEAN, "disk");
    #[cfg(unix)]
    let expected_clean = [Flush::Durable, Flush::Durable, Flush::Ordered];
    #[cfg(not(unix))]
    let expected_clean = [Flush::Durable];
    assert_eq!(
        recording.take(),
        expected_clean,
        "the keyed image; Unix also flushes publication and legacy retirement"
    );
    assert_eq!(decision.recovered_text, None);
    assert_eq!(initialized.notice, None);
    assert!(!clean.exists());
    assert_eq!(preserved_images(&root), Vec::<Vec<u8>>::new());

    let (decision, _, initialized) =
        open_as_the_app_does(&mut journals, &mut store, CONFLICT, "new disk");
    #[cfg(unix)]
    let expected_conflict = [
        Flush::Durable,
        Flush::Durable,
        Flush::Ordered,
        Flush::Ordered,
    ];
    #[cfg(not(unix))]
    let expected_conflict = [Flush::Durable];
    assert_eq!(
        recording.take(),
        expected_conflict,
        "the keyed image; Unix also flushes publication, set-aside and retirement"
    );
    drop(recording);
    assert_eq!(decision.recovered_text, None);
    assert_eq!(initialized.notice, Some(RecoveryNotice::DiskConflict));
    assert!(!conflict.exists());
    assert_eq!(preserved_images(&root), vec![conflict_image]);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_clean_legacy_journal_never_hides_a_keyed_draft() {
    const URI: &str = "file:///tmp/legacy-clean-after-keyed.md";
    const TORN: &str = "file:///tmp/legacy-clean-after-torn.md";
    let root = test_root("legacy-clean-after-keyed");
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let mut store = crate::test_document_store();
    let keyed = journal_a_draft(&mut journals, &mut store, URI, "the newest draft");
    drop(journals);
    let legacy = write_legacy_journal(&root, URI, "", "");

    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let mut store = crate::test_document_store();
    let (decision, _, initialized) = open_as_the_app_does(&mut journals, &mut store, URI, "");
    assert_eq!(
        decision.recovered_text.as_deref(),
        Some("the newest draft"),
        "a clean legacy image hid the keyed draft (notice {:?})",
        initialized.notice
    );
    assert_eq!(initialized.notice, Some(RecoveryNotice::Recovered));
    assert_eq!(initialized.path, keyed);
    assert!(
        !legacy.exists(),
        "a clean legacy image is retired at the open"
    );
    assert_eq!(preserved_images(&root), Vec::<Vec<u8>>::new());

    let torn = journals.inspect_open(TORN, b"").unwrap().path;
    fs::write(&torn, b"torn").unwrap();
    let torn_legacy = write_legacy_journal(&root, TORN, "", "");
    let (decision, _, initialized) = open_as_the_app_does(&mut journals, &mut store, TORN, "");
    assert_eq!(decision.recovered_text, None);
    assert_eq!(initialized.notice, Some(RecoveryNotice::Corrupt));
    assert!(!torn_legacy.exists());
    assert_eq!(preserved_images(&root), vec![b"torn".to_vec()]);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_legacy_draft_that_is_the_file_never_sets_a_keyed_draft_aside_silently() {
    const URI: &str = "file:///tmp/legacy-undone-after-keyed.md";
    let root = test_root("legacy-undone-after-keyed");
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let mut store = crate::test_document_store();
    let keyed = journal_a_draft(&mut journals, &mut store, URI, "the keyed draft");
    let keyed_image = fs::read(&keyed).unwrap();
    drop(journals);
    let legacy = write_legacy_journal_typing(&root, URI, "", &["typed", ""]);

    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let mut store = crate::test_document_store();
    let (decision, _, initialized) = open_as_the_app_does(&mut journals, &mut store, URI, "");
    assert_eq!(decision.recovered_text, None);
    assert_eq!(
        initialized.notice,
        Some(RecoveryNotice::DiskConflict),
        "the keyed draft was set aside with no notice"
    );
    assert_eq!(preserved_images(&root), vec![keyed_image]);
    assert!(
        legacy.exists(),
        "its draft is carried: it stays until the first write"
    );
    verify_carried_draft(
        &DraftCarryCandidate {
            path: legacy,
            key: JournalDocumentKey::legacy_unkeyed(URI),
        },
        "",
        ContentFingerprint::of(b""),
    )
    .expect("an outgoing older aterm still finds its carried draft");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn the_journal_secret_is_minted_private_once_and_read_back() {
    const URI: &str = "file:///tmp/secret.md";
    let root = test_root("secret-minted");
    let recording = aterm_durable::seam::Recording::start();
    let first = DocumentJournalStore::for_test(root.clone()).unwrap();
    #[cfg(unix)]
    let expected_mint = [Flush::Ordered, Flush::Ordered];
    #[cfg(not(unix))]
    let expected_mint = [Flush::Ordered];
    assert_eq!(
        recording.take(),
        expected_mint,
        "the minted secret is ordered; Unix also flushes its entry"
    );
    let secret = root.join(JOURNAL_SECRET_FILE);
    let image = fs::read(&secret).unwrap();
    assert_eq!(image.len(), JOURNAL_SECRET_FILE_LEN);
    assert_eq!(image[..JOURNAL_SECRET_MAGIC.len()], JOURNAL_SECRET_MAGIC);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(&secret).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let second = DocumentJournalStore::for_test(root.clone()).unwrap();
    assert_eq!(recording.take(), [], "a secret read back writes nothing");
    drop(recording);
    assert_eq!(
        fs::read(&secret).unwrap(),
        image,
        "read back, not re-minted"
    );
    assert_eq!(second.key_for(URI), first.key_for(URI));
    assert_eq!(
        fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count(),
        0,
        "no temporary is left behind"
    );
    let elsewhere = test_root("secret-elsewhere");
    let other = DocumentJournalStore::for_test(elsewhere.clone()).unwrap();
    assert_ne!(other.key_for(URI), first.key_for(URI));
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(elsewhere);
}

/// A secret that cannot be used refuses the store, and nothing is minted over it: the secret, a keyed journal and a legacy journal stay exactly as they were.
#[cfg(unix)]
#[test]
fn an_unusable_journal_secret_refuses_the_store_and_is_never_replaced() {
    use std::os::unix::fs::PermissionsExt as _;

    const URI: &str = "file:///tmp/secret-unusable.md";
    let valid = |root: &Path| fs::read(root.join(JOURNAL_SECRET_FILE)).unwrap();
    let mut zeros = JOURNAL_SECRET_MAGIC.to_vec();
    zeros.extend_from_slice(&[0; JOURNAL_SECRET_LEN]);
    type Spoil = Box<dyn Fn(&Path, &[u8])>;
    let spoils: Vec<(&str, Spoil)> = vec![
        (
            "short",
            Box::new(|path: &Path, good: &[u8]| fs::write(path, &good[..20]).unwrap()),
        ),
        (
            "bad-magic",
            Box::new(|path: &Path, good: &[u8]| {
                let mut bad = good.to_vec();
                bad[0] ^= 0xff;
                fs::write(path, bad).unwrap();
            }),
        ),
        (
            "zeros",
            Box::new(move |path: &Path, _: &[u8]| fs::write(path, &zeros).unwrap()),
        ),
        (
            "open-mode",
            Box::new(|path: &Path, _: &[u8]| {
                fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
            }),
        ),
        (
            "symlink",
            Box::new(|path: &Path, good: &[u8]| {
                let target = path.with_file_name("elsewhere-key");
                fs::write(&target, good).unwrap();
                fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
                fs::remove_file(path).unwrap();
                std::os::unix::fs::symlink(&target, path).unwrap();
            }),
        ),
        (
            "fifo",
            Box::new(|path: &Path, _: &[u8]| {
                fs::remove_file(path).unwrap();
                make_fifo(path);
            }),
        ),
    ];
    for (name, spoil) in spoils {
        let root = test_root(&format!("secret-{name}"));
        let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
        let mut store = crate::test_document_store();
        let keyed = journal_a_draft(&mut journals, &mut store, URI, "a keyed draft");
        drop(journals);
        let legacy = write_legacy_journal(&root, "file:///tmp/other.md", "x", "x y");
        let secret = root.join(JOURNAL_SECRET_FILE);
        spoil(&secret, &valid(&root));
        let before = |path: &Path| {
            (
                fs::symlink_metadata(path).unwrap().file_type(),
                fs::symlink_metadata(path).unwrap().permissions().mode(),
                fs::read(&keyed).unwrap(),
                fs::read(&legacy).unwrap(),
            )
        };
        let spoiled = before(&secret);
        let started = std::time::Instant::now();
        let error = match DocumentJournalStore::for_test(root.clone()) {
            Ok(_) => panic!("{name}: an unusable secret must refuse the store"),
            Err(error) => error,
        };
        assert!(
            started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
            "{name}: the secret's read parked"
        );
        assert!(error.contains(JOURNAL_SECRET_FILE), "{name}: {error}");
        assert!(error.contains("never replaced"), "{name}: {error}");
        assert_eq!(before(&secret), spoiled, "{name}: nothing was touched");
        let _ = fs::remove_dir_all(root);
    }
}

#[test]
fn a_deleted_journal_secret_is_reminted_and_its_journals_stay() {
    const URI: &str = "file:///tmp/secret-deleted.md";
    let root = test_root("secret-deleted");
    let mut journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    let mut store = crate::test_document_store();
    let orphan = journal_a_draft(&mut journals, &mut store, URI, "an orphaned draft");
    let orphan_image = fs::read(&orphan).unwrap();
    let old_key = journals.key_for(URI);
    drop(journals);
    fs::remove_file(root.join(JOURNAL_SECRET_FILE)).unwrap();

    let journals = DocumentJournalStore::for_test(root.clone()).unwrap();
    assert_ne!(journals.key_for(URI), old_key);
    let decision = journals.inspect_open(URI, b"").unwrap();
    assert_eq!(decision.recovered_text, None);
    assert_ne!(decision.path, orphan);
    assert_eq!(fs::read(&orphan).unwrap(), orphan_image);
    let _ = fs::remove_dir_all(root);
}
