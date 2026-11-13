// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const INPUTS: [&str; 2] = ["credential", "inventory"];

fn fixture() -> (aterm_tempfile::TempDir, Paths) {
    let root = aterm_tempfile::tempdir().unwrap();
    let source = root.path().join("team.vault");
    let paths = Paths::for_home(&root.path().join("reader"));
    create(&paths, source.to_str().unwrap(), None, Some("reader")).unwrap();
    publish(&paths, &|doc| {
        for name in INPUTS.into_iter().chain(["unrelated"]) {
            doc.set_entry(&Audience::Everyone, name, b"fixture-first")
                .unwrap();
        }
    });
    (root, paths)
}

fn publish(paths: &Paths, change: &dyn Fn(&mut Document)) {
    let opened = admin_open(paths).unwrap();
    let mut document = opened.doc.clone();
    change(&mut document);
    admin_publish(paths, &opened, &document).unwrap();
    sync(paths, None).unwrap();
}

fn capture(paths: &Paths) -> MaterializationSnapshot {
    let configured = config(paths).unwrap().unwrap();
    with_materialization_ready_for_entries(paths, &configured, &INPUTS, || Ok(()))
        .unwrap()
        .unwrap()
        .snapshot
}

#[test]
fn scoped_materialization_allows_unrelated_updates_but_not_changed_or_missing_inputs() {
    let (_root, paths) = fixture();
    let configured = config(&paths).unwrap().unwrap();
    let broad = with_materialization_ready(&paths, &configured, || Ok(()))
        .unwrap()
        .unwrap();
    let scoped = capture(&paths);
    assert_ne!(
        scoped, broad.snapshot,
        "entry scope is part of snapshot identity"
    );
    publish(&paths, &|doc| {
        doc.set_entry(&Audience::Everyone, "unrelated", b"fixture-next")
            .unwrap();
    });
    assert!(scoped.is_current(&paths).unwrap());
    assert!(scoped.source_continues(&paths).unwrap());
    assert!(
        !broad.snapshot.is_current(&paths).unwrap(),
        "broad callers retain their existing contract"
    );
    for name in INPUTS {
        let then = capture(&paths);
        publish(&paths, &|doc| {
            doc.set_entry(&Audience::Everyone, name, b"fixture-next")
                .unwrap();
        });
        assert!(
            !then.is_current(&paths).unwrap(),
            "changed scoped input {name}"
        );
        assert!(!then.source_continues(&paths).unwrap(), "{name}");
    }
    let then = capture(&paths);
    publish(&paths, &|doc| {
        doc.unset_entry(&Audience::Everyone, "inventory").unwrap();
    });
    assert!(!then.is_current(&paths).unwrap());
    assert!(!then.source_continues(&paths).unwrap());
    assert!(
        with_materialization_ready_for_entries::<()>(&paths, &configured, &INPUTS, || {
            panic!("a missing registered input must not reach the reader")
        })
        .unwrap()
        .is_none()
    );
    assert!(with_materialization_ready_for_entries(&paths, &configured, &[], || Ok(())).is_err());
}

#[test]
fn scoped_materialization_still_requires_complete_ready_source_and_membership() {
    for failure in ["incomplete", "source", "revoked"] {
        let (_root, paths) = fixture();
        let captured = capture(&paths);
        match failure {
            "incomplete" => {
                std::fs::remove_file(paths.home.join(".secrets/unrelated")).unwrap();
            }
            "source" => {
                let mut changed = config(&paths).unwrap().unwrap();
                changed.source = Source::Path(paths.home.join("different-vault"));
                save_config(&paths, &changed).unwrap();
            }
            "revoked" => {
                let mut state = load_state(&paths).unwrap();
                state.membership = Stored::Revoked;
                save_state(&paths, &state).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(!captured.is_current(&paths).unwrap(), "{failure}");
        assert_eq!(
            captured.source_continues(&paths).unwrap(),
            failure == "incomplete",
            "file integrity is checked at full boundaries, not by continuation"
        );
    }
}

/// A vault pass holding the state lock says nothing of whether the vault changed (the subscriber
/// takes it for its pass over every long-poll answer, an unchanged one included): a capture and
/// a currency check wait it out and read the vault as it is, and continuation, which must not
/// wait, goes on. NEGATIVE CONTROLS (2026-11-11): with `with_ready_state_reusing`'s waiting arm
/// observing the lock once and answering `WriterBusy` (the old non-waiting `is_current_now`),
/// the capture is `None` and the snapshot reads as changed; with `source_continues` answering
/// `false` on a busy lock, continuation stops the work.
#[test]
fn a_writer_released_within_the_wait_leaves_the_capture_current() {
    let (_root, paths) = fixture();
    let configured = config(&paths).unwrap().unwrap();
    let mut writer = Some(StateLock::take(&paths).unwrap());
    let (captured, tries) = test_while_state_lock_held(
        move || drop(writer.take()),
        || with_materialization_ready_for_entries(&paths, &configured, &INPUTS, || Ok(())),
    );
    let captured = captured
        .unwrap()
        .expect("a writer that changed nothing leaves the vault ready")
        .snapshot;
    assert!(tries >= 1, "the capture met the held writer and waited");

    let mut writer = Some(StateLock::take(&paths).unwrap());
    assert!(
        captured.source_continues(&paths).unwrap(),
        "continuation does not wait, and a held writer is no change"
    );
    let (current, tries) =
        test_while_state_lock_held(move || drop(writer.take()), || captured.is_current(&paths));
    assert!(
        current.unwrap(),
        "a writer that changed nothing leaves the capture current"
    );
    assert!(
        tries >= 1,
        "the currency check met the held writer and waited"
    );
}

/// A writer that applies a real change while the reader waits on it reads as that change.
#[test]
fn a_change_applied_while_the_reader_waits_reads_changed() {
    let (_root, paths) = fixture();
    let captured = capture(&paths);
    let mut writer = Some(StateLock::take(&paths).unwrap());
    let applying = paths.clone();
    let (current, tries) = test_while_state_lock_held(
        move || {
            if let Some(held) = writer.take() {
                drop(held);
                publish(&applying, &|doc| {
                    doc.set_entry(&Audience::Everyone, "credential", b"fixture-next")
                        .unwrap();
                });
            }
        },
        || captured.is_current(&paths),
    );
    assert!(
        !current.unwrap(),
        "the change the writer applied reads as one"
    );
    assert!(tries >= 1, "the currency check met the held writer");
}

/// A writer that still holds the lock at the end of the wait is said as what it is, an error
/// of its own kind, never as a changed or unready vault; continuation still goes on, and stops
/// on an apply that did not finish.
#[test]
fn a_writer_that_outlasts_the_wait_is_busy_and_never_a_change() {
    let (_root, paths) = fixture();
    let captured = capture(&paths);
    let configured = config(&paths).unwrap().unwrap();
    let writer = StateLock::take(&paths).unwrap();
    with_current_wait(Duration::from_millis(50), || {
        let busy = captured.is_current(&paths).unwrap_err();
        assert_eq!(busy.kind(), crate::ErrorKind::VaultBusy, "{busy}");
        assert!(
            busy.to_string()
                .contains("a vault pass on this machine has held the vault's state lock"),
            "{busy}"
        );
        let busy =
            with_materialization_ready_for_entries::<()>(&paths, &configured, &INPUTS, || {
                panic!("nothing is read under a writer")
            })
            .err()
            .expect("a writer past the wait");
        assert_eq!(busy.kind(), crate::ErrorKind::VaultBusy, "{busy}");
    });
    assert!(captured.source_continues(&paths).unwrap());
    drop(writer);
    assert!(captured.is_current(&paths).unwrap());
    for failure in ["failed", "pending"] {
        let mut state = load_state(&paths).unwrap();
        match failure {
            "failed" => state.failed = true,
            "pending" => {
                state
                    .pending
                    .insert("inventory".into(), "unfinished".into());
            }
            _ => unreachable!(),
        }
        save_state(&paths, &state).unwrap();
        assert!(!captured.source_continues(&paths).unwrap(), "{failure}");
        state.failed = false;
        state.pending.clear();
        save_state(&paths, &state).unwrap();
    }
}

/// The readiness checks a reader acts on (`materialization_ready`, `_for`, `_for_setup`: a
/// service repair, `init.secrets`' probe and apply) wait out a vault pass holding the state lock
/// and read the vault as it is, and one that outlasts the wait is busy, never a vault not ready.
/// The status read (`observe_materialization`) answers at once. NEGATIVE CONTROL (2026-11-12):
/// with `materialization_ready_with` answering a held lock at once, each check reads `false`
/// under a writer released within the wait.
#[test]
fn a_readiness_check_waits_out_a_writer_and_a_status_read_does_not() {
    let (_root, paths) = fixture();
    let configured = config(&paths).unwrap().unwrap();
    let checks: [(&str, &dyn Fn() -> crate::Result<bool>); 3] = [
        ("materialization_ready", &|| materialization_ready(&paths)),
        ("materialization_ready_for", &|| {
            materialization_ready_for(&paths, &configured)
        }),
        ("materialization_ready_for_setup", &|| {
            materialization_ready_for_setup(&paths)
        }),
    ];
    for (check, ready) in checks {
        let mut writer = Some(StateLock::take(&paths).unwrap());
        let (read, tries) = test_while_state_lock_held(move || drop(writer.take()), ready);
        assert_eq!(read.ok(), Some(true), "{check}");
        assert!(tries >= 1, "{check} met the held writer and waited");
    }

    let writer = StateLock::take(&paths).unwrap();
    let (status, tries) = test_while_state_lock_held(
        || {},
        || observe_materialization(&paths, &configured, || Ok(())),
    );
    assert!(
        matches!(
            status.unwrap(),
            MaterializationObservation::Blocked(MaterializationBlocker::WriterBusy)
        ),
        "a status read answers a held writer at once"
    );
    assert_eq!(tries, 0, "a status read does not wait");
    with_current_wait(Duration::from_millis(50), || {
        for (check, ready) in checks {
            assert_eq!(
                ready().err().map(|error| error.kind()),
                Some(crate::ErrorKind::VaultBusy),
                "{check}"
            );
        }
    });
    drop(writer);
    for (check, ready) in checks {
        assert_eq!(ready().ok(), Some(true), "{check}");
    }
}
