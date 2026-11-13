// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::{PermissionsExt as _, symlink};

fn fixture() -> (aterm_tempfile::TempDir, PathBuf) {
    let root = aterm_tempfile::tempdir().unwrap();
    let entry = root.path().join("records");
    fsx::write_atomic(&entry, b"original history", 0o600).unwrap();
    (root, entry)
}

fn stop_at(context: &Context, cut: Step) -> Result<()> {
    context.prepare(|step| {
        if step == cut {
            Err(refused("interrupted test transition"))
        } else {
            Ok(())
        }
    })
}

fn prepare_path(entry: &Path) -> PathBuf {
    write_path(entry, true).unwrap().path.clone()
}

fn budget(held: usize) -> crate::membership_log::RecordBudgets {
    crate::membership_log::RecordBudgets {
        weighed: 16,
        keyed: 128,
        held,
    }
}

/// A reader resolves a pinned layout once while every file `data_path` judges stands as it
/// did ([`Judgements`]): another selection reads its header no more. Each change `data_path`
/// refuses that the selection's own witnesses do not see (the parent opened to its group, the
/// header rewritten in place, the data file opened to others or grown past its bound, a
/// smaller capacity) is refused again.
/// NEGATIVE CONTROLS (2026-11-08): a judgement without the parent's mode takes the layout
/// after the parent's change, without the header's `lstat` after the header's, without the
/// data file's `lstat` after either of the data file's, without the data file's stamp alone
/// after its growth, and without the capacity after the smaller one. APFS moves a file's
/// inode-change time at each `chmod`, so no change here shows a mode the stamp kept, which
/// [`fsx::Seen`] holds for a coarser clock.
#[test]
fn a_reader_resolves_an_unchanged_layout_once_and_a_changed_one_again() {
    /// A change to a judged layout, the capacity its reader then holds, and what makes it.
    struct Change {
        what: &'static str,
        held: usize,
        make: fn(&Path),
    }
    fn mode(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    let changes = [
        Change {
            what: "the parent opened to its group",
            held: 32,
            make: |entry| mode(entry.parent().unwrap(), 0o770),
        },
        Change {
            what: "the header rewritten in place",
            held: 32,
            make: |entry| std::fs::write(entry.join(HEADER), b"not a layout").unwrap(),
        },
        Change {
            what: "the data file opened to others",
            held: 32,
            make: |entry| mode(&entry.join(DATA), 0o644),
        },
        Change {
            what: "the data file grown past its bound",
            held: 32,
            make: |entry| {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(entry.join(DATA))
                    .unwrap()
                    .set_len(crate::membership_log::MAX_RECORDS_STREAM as u64 + 1)
                    .unwrap();
            },
        },
        Change {
            what: "a smaller capacity",
            held: 16,
            make: |_| {},
        },
    ];
    for Change {
        what: change,
        held,
        make,
    } in changes
    {
        let (_root, entry) = fixture();
        let data = budget(32).within(|| prepare_path(&entry));
        let judgements = Judgements::new();
        budget(32).within(|| {
            assert_eq!(judgements.selected(&entry).unwrap().path(), data.as_path());
            let read = fsx::reads_of(&entry.join(HEADER));
            assert_eq!(judgements.selected(&entry).unwrap().path(), data.as_path());
            assert_eq!(fsx::reads_of(&entry.join(HEADER)), read, "read again");
        });
        make(&entry);
        budget(held).within(|| {
            assert!(data_path(&entry).is_err(), "{change}: data_path takes it");
            assert!(
                judgements.selected(&entry).is_err(),
                "{change}: a reader takes it"
            );
        });
    }
}

#[test]
fn a_stale_snapshot_never_prepares_or_raises_the_layout() {
    for fenced in [false, true] {
        let (_root, entry) = fixture();
        let path = if fenced {
            budget(32).within(|| prepare_path(&entry))
        } else {
            entry.clone()
        };
        let expected = fsx::stamp(&path);
        fsx::write_atomic(&path, b"another writer's newer history", 0o600).unwrap();
        let before = std::fs::read(&path).unwrap();
        budget(64).within(|| {
            assert!(write_path_if_unchanged(&entry, true, expected).is_err());
        });
        budget(32).within(|| {
            assert_eq!(data_path(&entry).unwrap(), path);
            assert_eq!(std::fs::read(&path).unwrap(), before);
        });
        assert_eq!(pin_path(&entry).unwrap().exists(), fenced);
    }
}

#[test]
fn an_absent_snapshot_refuses_a_new_file_before_preparing_a_layout() {
    let root = aterm_tempfile::tempdir().unwrap();
    let entry = root.path().join("records");
    fsx::write_atomic(&entry, b"new history", 0o600).unwrap();
    assert!(write_path_if_unchanged(&entry, true, None).is_err());
    assert_eq!(std::fs::read(&entry).unwrap(), b"new history");
    assert!(!pin_path(&entry).unwrap().exists());
    assert!(!intent_path(&entry).unwrap().exists());
}

#[test]
fn a_larger_writer_fences_format_aware_smaller_readers_before_changing_data() {
    let (_root, entry) = fixture();
    let data = budget(32).within(|| prepare_path(&entry));
    budget(64).within(|| {
        let target = write_path(&entry, false).unwrap();
        assert_eq!(std::fs::read(&*target).unwrap(), b"original history");
        assert!(budget(32).within(|| data_path(&entry)).is_err());
        fsx::write_atomic(&target, b"new history", 0o600).unwrap();
    });
    budget(32).within(|| {
        assert!(data_path(&entry).is_err());
        assert!(write_path(&entry, false).is_err());
    });
    budget(64).within(|| {
        assert_eq!(data_path(&entry).unwrap(), data);
        assert_eq!(std::fs::read(&data).unwrap(), b"new history");
    });
}

#[test]
fn a_smaller_writer_cannot_finish_a_larger_pending_layout() {
    for cut in [Step::Intent, Step::Exchanged] {
        let (_root, entry) = fixture();
        budget(64).within(|| {
            assert!(stop_at(&Context::new(&entry).unwrap(), cut).is_err());
        });
        budget(32).within(|| {
            assert!(recover_pending(&entry).is_err());
            assert!(!pin_path(&entry).unwrap().exists());
        });
        budget(64).within(|| {
            recover_pending(&entry).unwrap();
            assert_eq!(
                std::fs::read(data_path(&entry).unwrap()).unwrap(),
                b"original history"
            );
        });
    }
}

#[test]
fn the_write_target_keeps_a_process_shared_lock_until_the_write_finishes() {
    let (_root, entry) = fixture();
    let target = write_path(&entry, true).unwrap();
    // A sibling fork retains this open file description until it execs. Keep a copy
    // here so releasing the logical writer cannot depend on that scheduling window.
    let inherited = target._guard.0.try_clone().unwrap();
    let other = fsx::open_lock(&entry.with_file_name("records.writing")).unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "probe the held writer lock without waiting"
    )]
    let during = other.try_lock();
    assert!(matches!(during, Err(std::fs::TryLockError::WouldBlock)));
    fsx::write_atomic(&target, b"written under lock", 0o600).unwrap();
    drop(target);
    #[expect(
        clippy::disallowed_methods,
        reason = "prove dropping the write target released the writer lock"
    )]
    let after = other.try_lock();
    after.unwrap();
    other.unlock().unwrap();
    drop(inherited);
}

#[test]
fn layout_preserves_the_original_and_selects_later_writes() {
    let (root, entry) = fixture();
    let data = prepare_path(&entry);
    assert_eq!(std::fs::read(&data).unwrap(), b"original history");
    let context = Context::new(&entry).unwrap();
    let layout = context.header(&context.directory().unwrap()).unwrap();
    let original = root.path().join(format!(".aterm-records-{}", layout.nonce));
    assert_eq!(std::fs::read(&original).unwrap(), b"original history");
    fsx::write_atomic(&data, b"expanded history", 0o600).unwrap();
    assert_eq!(prepare_path(&entry), data);
    recover_pending(&entry).unwrap();
    assert_eq!(
        std::fs::read(data_path(&entry).unwrap()).unwrap(),
        b"expanded history"
    );
    assert_eq!(std::fs::read(original).unwrap(), b"original history");
    assert!(fsx::read_owned(&entry, 1024).is_err());
    assert!(fsx::write_atomic(&entry, b"old writer", 0o600).is_err());
}

#[test]
fn interruption_keeps_custody_and_only_a_writer_finishes_a_pending_transition() {
    for cut in [
        Step::Intent,
        Step::Copied,
        Step::Prepared,
        Step::Exchanged,
        Step::Pinned,
    ] {
        let (_root, entry) = fixture();
        let context = Context::new(&entry).unwrap();
        assert!(stop_at(&context, cut).is_err());
        let before = fsx::kind(&pin_path(&entry).unwrap()).unwrap();
        let read = data_path(&entry);
        assert_eq!(read.is_err(), cut == Step::Exchanged);
        assert_eq!(fsx::kind(&pin_path(&entry).unwrap()).unwrap(), before);
        recover_pending(&entry).unwrap();
        let selected = data_path(&entry).unwrap();
        assert_eq!(std::fs::read(&selected).unwrap(), b"original history");
        assert_ne!(selected, entry);
        let selected = prepare_path(&entry);
        assert_eq!(std::fs::read(&selected).unwrap(), b"original history");
    }
}

#[test]
fn preparation_retries_reuse_one_declared_generation() {
    let (root, entry) = fixture();
    let context = Context::new(&entry).unwrap();
    let mut generation = None;
    for _ in 0..3 {
        assert!(stop_at(&context, Step::Prepared).is_err());
        let intent = context.intent().unwrap().unwrap();
        if let Some(before) = &generation {
            assert_eq!(before, &intent.nonce);
        }
        generation = Some(intent.nonce);
        let copies = std::fs::read_dir(root.path())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".aterm-records-")
            })
            .count();
        assert_eq!(copies, 1);
        assert_eq!(std::fs::read(&entry).unwrap(), b"original history");
    }
    recover_pending(&entry).unwrap();
    assert_eq!(
        std::fs::read(data_path(&entry).unwrap()).unwrap(),
        b"original history"
    );
}

#[test]
fn an_interrupted_derived_copy_resumes_from_its_existing_intent() {
    let (_root, entry) = fixture();
    let context = Context::new(&entry).unwrap();
    assert!(stop_at(&context, Step::Intent).is_err());
    let intent = context.intent().unwrap().unwrap();
    let staged = entry.with_file_name(format!(".aterm-records-{}", intent.nonce));
    fsx::ensure_private_dir(&staged).unwrap();
    fsx::write_atomic(&staged.join("current.building"), b"partial", 0o600).unwrap();
    assert_eq!(data_path(&entry).unwrap(), entry);
    recover_pending(&entry).unwrap();
    assert_eq!(
        std::fs::read(data_path(&entry).unwrap()).unwrap(),
        b"original history"
    );
}

#[test]
fn an_interrupted_pin_write_resumes_without_moving_the_prepared_files() {
    let (_root, entry) = fixture();
    let context = Context::new(&entry).unwrap();
    assert!(stop_at(&context, Step::Exchanged).is_err());
    let current = fsx::stamp(&entry.join(DATA)).unwrap();
    let header = fsx::stamp(&entry.join(HEADER)).unwrap();
    fsx::write_atomic(
        &entry.with_file_name("records.layout.building"),
        b"partial pin",
        0o600,
    )
    .unwrap();
    assert!(data_path(&entry).is_err());
    recover_pending(&entry).unwrap();
    assert_eq!(fsx::stamp(&entry.join(DATA)), Some(current));
    assert_eq!(fsx::stamp(&entry.join(HEADER)), Some(header));
    assert_eq!(
        std::fs::read(data_path(&entry).unwrap()).unwrap(),
        b"original history"
    );
}

#[test]
fn an_intent_does_not_turn_a_missing_original_into_empty_custody() {
    let (root, entry) = fixture();
    let context = Context::new(&entry).unwrap();
    assert!(stop_at(&context, Step::Intent).is_err());
    let saved = root.path().join("saved-original");
    std::fs::rename(&entry, &saved).unwrap();
    assert!(data_path(&entry).is_err());
    assert!(recover_pending(&entry).is_err());
    assert_eq!(std::fs::read(saved).unwrap(), b"original history");
}

#[test]
fn an_active_pin_never_falls_back_after_current_or_layout_is_lost() {
    let (root, entry) = fixture();
    let data = prepare_path(&entry);
    std::fs::rename(&data, entry.join("saved-current")).unwrap();
    assert!(data_path(&entry).is_err());
    assert!(recover_pending(&entry).is_err());
    let saved = root.path().join("saved-layout");
    std::fs::rename(&entry, &saved).unwrap();
    assert!(data_path(&entry).is_err());
    fsx::write_atomic(&entry, b"an old flat snapshot", 0o600).unwrap();
    assert!(data_path(&entry).is_err());
    assert!(write_path(&entry, false).is_err());
    assert_eq!(
        std::fs::read(saved.join("saved-current")).unwrap(),
        b"original history"
    );
}

#[test]
fn a_changed_predecessor_refuses_pending_recovery_without_losing_either_copy() {
    let (root, entry) = fixture();
    let context = Context::new(&entry).unwrap();
    assert!(stop_at(&context, Step::Exchanged).is_err());
    let layout = context.header(&context.directory().unwrap()).unwrap();
    let original = root.path().join(format!(".aterm-records-{}", layout.nonce));
    fsx::write_atomic(&original, b"a concurrent old write", 0o600).unwrap();
    assert!(recover_pending(&entry).is_err());
    assert!(data_path(&entry).is_err());
    assert_eq!(
        std::fs::read(entry.join(DATA)).unwrap(),
        b"original history"
    );
    assert_eq!(std::fs::read(original).unwrap(), b"a concurrent old write");
    assert!(!pin_path(&entry).unwrap().exists());
}

/// NEGATIVE CONTROLS (2026-11-05): with the entry's `lstat` replaced by `None` in
/// `selection_witnesses`, the entry's arm confirms the replaced entry
/// (`the selection still stood`); with the intent's stamp replaced the same way, the intent's
/// arm does.
#[test]
fn a_selection_resolves_again_once_its_entry_or_its_intent_moved() {
    let refusal = |held: &Selected| {
        held.confirm()
            .map(|()| "the selection still stood".to_owned())
            .unwrap_or_else(|error| error.to_string())
    };

    // What an interrupted transition leaves, in place of the file the reader selected.
    let (root, entry) = fixture();
    let held = selected(&entry).unwrap();
    held.confirm().unwrap();
    std::fs::rename(&entry, root.path().join("aside")).unwrap();
    fsx::ensure_private_dir(&entry).unwrap();
    let refused = refusal(&held);
    assert!(
        refused.contains("an interrupted transition needs its writer lock"),
        "{refused}"
    );

    // A preparation that names the original, appearing over the entry the reader found empty.
    let (root, entry) = fixture();
    assert!(stop_at(&Context::new(&entry).unwrap(), Step::Intent).is_err());
    let intent = intent_path(&entry).unwrap();
    let saved = root.path().join("saved-intent");
    std::fs::rename(&intent, &saved).unwrap();
    std::fs::rename(&entry, root.path().join("saved-original")).unwrap();
    let held = selected(&entry).unwrap();
    held.confirm().unwrap();
    std::fs::rename(&saved, &intent).unwrap();
    let refused = refusal(&held);
    assert!(
        refused.contains("the intended original records file is missing"),
        "{refused}"
    );
}

/// NEGATIVE CONTROLS (2026-11-05), `selected`'s witness arm loosened each way: with any kind
/// at the entry, the symlink selects the entry (`a symlink`, `left: Ok(entry), right: Err(..)`);
/// with `Dir` or `Other` admitted alone, the directory or the FIFO does; with the intent's slot
/// `_`, the missing original does; with the pin's slot `_`, the pin beside a file does.
#[test]
fn a_selection_from_its_witnesses_answers_as_data_path_does() {
    let answer = |path: Result<PathBuf>| path.map_err(|error| error.to_string());
    let shaped = |shape: &str, make: &dyn Fn(&Path, &Path), refused: bool| {
        let (root, entry) = fixture();
        make(root.path(), &entry);
        let resolved = answer(data_path(&entry));
        assert_eq!(resolved.is_err(), refused, "{shape}: {resolved:?}");
        assert_eq!(
            answer(selected(&entry).map(|held| held.path().to_path_buf())),
            resolved,
            "{shape}"
        );
    };
    shaped("a regular file", &|_, _| {}, false);
    shaped(
        "nothing",
        &|_, entry| std::fs::remove_file(entry).unwrap(),
        false,
    );
    shaped(
        "a symlink",
        &|root, entry| {
            let kept = root.join("kept");
            std::fs::rename(entry, &kept).unwrap();
            symlink(&kept, entry).unwrap();
        },
        true,
    );
    shaped(
        "a directory",
        &|_, entry| {
            std::fs::remove_file(entry).unwrap();
            fsx::ensure_private_dir(entry).unwrap();
        },
        true,
    );
    shaped(
        "a FIFO",
        &|_, entry| {
            std::fs::remove_file(entry).unwrap();
            let made = aterm_uds::spawnfd::command("/usr/bin/mkfifo")
                .arg(entry)
                .status()
                .expect("mkfifo");
            assert!(made.success());
        },
        true,
    );
    shaped(
        "an intent whose original is gone",
        &|root, entry| {
            assert!(stop_at(&Context::new(entry).unwrap(), Step::Intent).is_err());
            std::fs::rename(entry, root.join("saved-original")).unwrap();
        },
        true,
    );
    shaped(
        "a pinned layout",
        &|_, entry| drop(write_path(entry, true).unwrap()),
        false,
    );
    shaped(
        "a pin beside a regular file",
        &|_, entry| {
            fsx::write_atomic(&pin_path(entry).unwrap(), b"no pin", 0o600).unwrap();
        },
        true,
    );
}

#[test]
fn symlinks_hard_links_and_public_layout_files_are_refused() {
    for slot in [DATA, HEADER, "pin"] {
        let (_root, entry) = fixture();
        write_path(&entry, true).unwrap();
        let path = if slot == "pin" {
            pin_path(&entry).unwrap()
        } else {
            entry.join(slot)
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(data_path(&entry).is_err(), "public {slot}");
    }
    // NEGATIVE CONTROL (2026-11-05): with `context.pin_is_private()?` gone from `data_path`,
    // both arms resolve, since `fsx::read_private` admits either pin.
    for read_only in [false, true] {
        let (root, entry) = fixture();
        write_path(&entry, true).unwrap();
        let pin = pin_path(&entry).unwrap();
        if read_only {
            std::fs::set_permissions(&pin, std::fs::Permissions::from_mode(0o400)).unwrap();
        } else {
            std::fs::hard_link(&pin, root.path().join("kept-pin")).unwrap();
        }
        assert!(data_path(&entry).is_err(), "read only: {read_only}");
        assert!(selected(&entry).is_err(), "read only: {read_only}");
    }
    for link in [false, true] {
        let (_root, entry) = fixture();
        let data = prepare_path(&entry);
        let kept = entry.join("kept-current");
        std::fs::rename(&data, &kept).unwrap();
        if link {
            symlink(&kept, &data).unwrap();
        } else {
            std::fs::hard_link(&kept, &data).unwrap();
        }
        assert!(data_path(&entry).is_err());
        assert_eq!(std::fs::read(kept).unwrap(), b"original history");
    }
}

#[test]
fn a_fresh_layout_has_no_invented_predecessor() {
    let root = aterm_tempfile::tempdir().unwrap();
    let entry = root.path().join("records");
    let data = prepare_path(&entry);
    assert!(std::fs::read(data).unwrap().is_empty());
    let context = Context::new(&entry).unwrap();
    let layout = context.header(&context.directory().unwrap()).unwrap();
    assert!(layout.original.is_none());
    assert!(
        !root
            .path()
            .join(format!(".aterm-records-{}", layout.nonce))
            .exists()
    );
}

#[test]
fn an_old_open_descriptor_can_only_change_the_preserved_predecessor() {
    let (_root, entry) = fixture();
    let mut old = std::fs::OpenOptions::new()
        .write(true)
        .open(&entry)
        .unwrap();
    let data = prepare_path(&entry);
    old.write_all(b"OLD").unwrap();
    assert_eq!(
        std::fs::read(data_path(&entry).unwrap()).unwrap(),
        b"original history"
    );
    assert_eq!(std::fs::read(data).unwrap(), b"original history");
}
