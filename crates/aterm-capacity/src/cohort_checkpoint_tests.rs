// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

struct Fixture {
    scratch: TestWorld,
    backend: File,
    writer: File,
    anchor: Anchor,
    name: String,
}

impl Fixture {
    fn new() -> Self {
        let scratch = TestWorld::new("checkpoint");
        let root = scratch.path().join("ledger.noindex");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let parent = File::open(&root).unwrap();
        let name = "0000000000001-1-1.claim".to_owned();
        let (anchor, writer) = Anchor::create(&parent, &name, "namespace").unwrap();
        let backend = anchor.directory.try_clone().unwrap();
        writer.write_all_at(b"head\n", 0).unwrap();
        let mut transaction = anchor.lock(&backend).unwrap();
        let capture = transaction
            .capture(&backend, &writer, &name, "head\n", false)
            .unwrap();
        transaction.commit(&backend, &writer, &capture).unwrap();
        drop(transaction);
        Self {
            scratch,
            backend,
            writer,
            anchor,
            name,
        }
    }

    fn root(&self) -> std::path::PathBuf {
        self.scratch.path().join("ledger.noindex")
    }

    fn record_path(&self) -> std::path::PathBuf {
        self.root().join(&self.anchor.name).join(&self.name)
    }

    fn metadata_path(&self, name: &str) -> std::path::PathBuf {
        let root = self.root().join(&self.anchor.name);
        if matches!(name, HEADER | MUTATION) {
            return root.join(name);
        }
        root.join("epoch-0000000000000001.noindex")
            .join(if name == "0000000000000001.json" {
                "base.json"
            } else {
                name
            })
    }

    fn validate(&self) -> io::Result<()> {
        validate_metadata(&File::open(self.root())?, &self.anchor.name, "namespace")
    }

    fn change(&self, transaction: &mut Transaction, text: &str) -> Capture {
        self.writer.write_all_at(text.as_bytes(), 0).unwrap();
        self.writer.set_len(text.len() as u64).unwrap();
        transaction
            .capture(&self.backend, &self.writer, &self.name, text, false)
            .unwrap()
    }
}

fn replace_metadata(f: &Fixture, name: &str, copied: bool) {
    let path = f.metadata_path(name);
    let original = std::fs::read(&path).unwrap();
    let before = Stamp::read(&File::open(&path).unwrap()).unwrap();
    if copied {
        let copy = f.root().join("copied-checkpoint");
        std::fs::copy(&path, &copy).unwrap();
        std::fs::rename(copy, &path).unwrap();
        assert_ne!(
            Stamp::read(&File::open(&path).unwrap()).unwrap().0[..2],
            before.0[..2]
        );
    } else {
        past_the_clock_tick(&f.backend, &File::open(&path).unwrap());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        writer.write_all_at(b"!", 0).unwrap();
        writer.write_all_at(&original, 0).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        let after = Stamp::read(&writer).unwrap();
        assert_eq!(after.0[..2], before.0[..2]);
        assert_ne!(after, before);
    }
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original,
        "CONTROL: canonical bytes alone would accept the metadata"
    );
}

#[test]
fn checkpoint_cover_cannot_recapture_a_same_byte_restore_as_its_known_rename() {
    thread_local! { static RESTORE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
    fn after_cover(parent: &File, name: &str, file: &File, renamed: &Stamp) -> io::Result<()> {
        assert_eq!(Stamp::read(file)?, *renamed);
        assert_eq!(Stamp::read(&open(parent, name, false, false)?)?, *renamed);
        if RESTORE.with(std::cell::Cell::take) {
            let before = file.metadata()?;
            let text = bytes(file, MAX_ENTRY)?.0;
            past_the_clock_tick(parent, file);
            file.write_all_at(text.as_bytes(), 0)?;
            file.set_modified(before.modified()?)?;
            let restored = Stamp::read(file)?;
            assert_eq!(restored.0[..8], renamed.0[..8]);
            assert_ne!(restored, *renamed);
            assert_eq!(bytes(file, MAX_ENTRY)?.0, text);
        }
        Ok(())
    }
    for retired in [false, true] {
        for restore in [false, true] {
            let f = Fixture::new();
            let mut transaction = f.anchor.lock(&f.backend).unwrap();
            let capture = transaction
                .capture(&f.backend, &f.writer, &f.name, "head\n", retired)
                .unwrap();
            RESTORE.with(|flag| flag.set(restore));
            AFTER_CHECKPOINT_COVER.with(|hook| hook.set(Some(after_cover)));
            let committed = transaction.commit(&f.backend, &f.writer, &capture);
            assert!(AFTER_CHECKPOINT_COVER.with(std::cell::Cell::take).is_none());
            if restore {
                assert_eq!(
                    committed.unwrap_err().kind(),
                    io::ErrorKind::InvalidData,
                    "a restore during parent cover cannot become the rename's frozen stamp"
                );
            } else {
                committed.unwrap();
                drop(transaction);
                // A normal known rename remains usable through a fresh physical read.
                let fresh = f.anchor.lock(&f.backend).unwrap();
                let (observed, stamp) = fresh.latest.as_ref().unwrap();
                let tip = fresh.tip_file.as_ref().unwrap();
                assert_eq!(observed, &capture);
                assert_eq!(Stamp::read(tip).unwrap(), *stamp);
                assert_eq!(bytes(tip, MAX_ENTRY).unwrap().0, capture.encode().unwrap());
            }
        }
    }
}

#[test]
fn cold_tip_authority_covers_its_exact_file_including_retired_tombstones() {
    for copied in [false, true] {
        for retired in [false, true] {
            let f = Fixture::new();
            let ordinal = if retired {
                let mut transaction = f.anchor.lock(&f.backend).unwrap();
                let capture = transaction
                    .capture(&f.backend, &f.writer, &f.name, "head\n", true)
                    .unwrap();
                transaction.commit(&f.backend, &f.writer, &capture).unwrap();
                drop(transaction);
                std::fs::remove_file(f.record_path()).unwrap();
                2
            } else {
                1
            };
            replace_metadata(&f, &format!("{ordinal:016}.json"), copied);
            let cold = Anchor::open(&f.backend, &f.name, "namespace").unwrap();
            FAIL_NAMED_FILE_FLUSH.with(|hook| {
                hook.set(Some(if retired {
                    "0000000000000002.json"
                } else {
                    "base.json"
                }))
            });
            assert!(
                cold.lock(&f.backend).is_err(),
                "cold tip/tombstone cannot authorize anything after its FILE flush was refused"
            );
            let recovered = cold.lock(&f.backend).unwrap();
            if retired {
                assert!(recovered.missing_retired());
                drop(recovered);
                f.validate().unwrap();
            } else {
                assert_eq!(
                    recovered.observe(&f.backend, &f.writer, &f.name).unwrap(),
                    "head\n"
                );
            }
        }
    }
}

#[test]
fn cold_header_cover_refuses_failure_then_recovers_without_rebasing_the_record() {
    for copied in [false, true] {
        let f = Fixture::new();
        replace_metadata(&f, HEADER, copied);
        assert!(
            f.anchor.lock(&f.backend).is_err(),
            "the original live header proof was revoked"
        );
        FAIL_PUBLICATION_FLUSH.with(|hook| hook.set(true));
        assert!(Anchor::open(&f.backend, &f.name, "namespace").is_err());
        let recovered = Anchor::open(&f.backend, &f.name, "namespace").unwrap();
        assert_eq!(
            recovered
                .lock(&f.backend)
                .unwrap()
                .observe(&f.backend, &f.writer, &f.name)
                .unwrap(),
            "head\n"
        );

        past_the_clock_tick(&f.backend, &f.writer);
        f.writer.write_all_at(b"!", 0).unwrap();
        f.writer.write_all_at(b"head\n", 0).unwrap();
        let cold = Anchor::open(&f.backend, &f.name, "namespace").unwrap();
        assert!(
            cold.lock(&f.backend)
                .unwrap()
                .observe(&f.backend, &f.writer, &f.name)
                .is_err(),
            "fresh header/tip cover cannot refresh the working record's old origin"
        );
    }
}

#[test]
fn cold_cover_cannot_refresh_a_predecessor_stamp_named_by_its_successor() {
    for copied in [false, true] {
        let f = Fixture::new();
        let mut transaction = f.anchor.lock(&f.backend).unwrap();
        let capture = f.change(&mut transaction, "next\n");
        transaction.commit(&f.backend, &f.writer, &capture).unwrap();
        drop(transaction);
        replace_metadata(&f, "0000000000000001.json", copied);
        let cold = Anchor::open(&f.backend, &f.name, "namespace").unwrap();
        assert!(
            cold.lock(&f.backend).is_err(),
            "covering the tip never rebases a changed physical predecessor"
        );
    }
}

#[test]
fn equal_bytes_on_the_same_inode_do_not_restore_a_committed_origin() {
    let f = Fixture::new();
    let before = Stamp::read(&f.writer).unwrap();
    past_the_clock_tick(&f.backend, &f.writer);
    f.writer.write_all_at(b"!", 0).unwrap();
    f.writer.write_all_at(b"head\n", 0).unwrap();
    let (text, after) = bytes(&f.writer, MAX_ENTRY).unwrap();
    assert_eq!(
        text, "head\n",
        "CONTROL: byte equality would accept this restore"
    );
    assert_eq!(
        before.0[..2],
        after.0[..2],
        "the original inode was retained"
    );
    assert_ne!(
        before, after,
        "the real write/restore changed physical origin"
    );
    assert!(
        f.anchor
            .lock(&f.backend)
            .unwrap()
            .observe(&f.backend, &f.writer, &f.name)
            .is_err()
    );
    let cold = Anchor::open(&f.backend, &f.name, "namespace").unwrap();
    assert!(
        cold.lock(&f.backend)
            .unwrap()
            .observe(&f.backend, &f.writer, &f.name)
            .is_err()
    );
}

#[test]
fn copied_payload_and_copied_checkpoint_directory_cannot_replace_the_originals() {
    let f = Fixture::new();
    let old = f.root().join("old-record");
    std::fs::rename(f.record_path(), &old).unwrap();
    std::fs::copy(&old, f.record_path()).unwrap();
    let replacement = open(&f.backend, &f.name, false, false).unwrap();
    assert_eq!(
        bytes(&replacement, MAX_ENTRY).unwrap().0,
        bytes(&f.writer, MAX_ENTRY).unwrap().0,
        "CONTROL: the copy has equal record bytes"
    );
    assert_ne!(
        Stamp::read(&replacement).unwrap().0[..2],
        Stamp::read(&f.writer).unwrap().0[..2]
    );
    assert!(f.anchor.lock(&f.backend).is_err());
    assert!(Anchor::open(&f.backend, &f.name, "namespace").is_err());

    let g = Fixture::new();
    let old_metadata = g.root().join("old-metadata");
    std::fs::rename(g.root().join(&g.anchor.name), &old_metadata).unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(g.root().join(&g.anchor.name))
        .unwrap();
    for name in [HEADER, MUTATION, g.name.as_str()] {
        std::fs::copy(
            old_metadata.join(name),
            g.root().join(&g.anchor.name).join(name),
        )
        .unwrap();
    }
    let epoch = "epoch-0000000000000001.noindex";
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(g.root().join(&g.anchor.name).join(epoch))
        .unwrap();
    std::fs::copy(
        old_metadata.join(epoch).join("base.json"),
        g.root().join(&g.anchor.name).join(epoch).join("base.json"),
    )
    .unwrap();
    let copied = File::open(g.root().join(&g.anchor.name)).unwrap();
    assert!(Anchor::open(&copied, &g.name, "namespace").is_err());
    assert!(g.anchor.lock(&g.backend).is_err());
}

#[test]
fn restore_during_the_covering_flush_cannot_be_captured_late() {
    let f = Fixture::new();
    let mut transaction = f.anchor.lock(&f.backend).unwrap();
    let capture = f.change(&mut transaction, "next\n");
    AFTER_FLUSH.with(|hook| hook.set(true));
    assert!(transaction.commit(&f.backend, &f.writer, &capture).is_err());
    assert_eq!(bytes(&f.writer, MAX_ENTRY).unwrap().0, "next\n");
    assert!(capture.check(&f.backend, &f.writer).is_err());
    drop(transaction);
    let mut recovered = f.anchor.lock(&f.backend).unwrap();
    assert!(recovered.observe(&f.backend, &f.writer, &f.name).is_err());
    assert!(recovered.commit(&f.backend, &f.writer, &capture).is_err());
}

#[test]
fn interrupted_publication_retries_only_its_original_frozen_generation() {
    let f = Fixture::new();
    let mut transaction = f.anchor.lock(&f.backend).unwrap();
    let capture = f.change(&mut transaction, "next\n");
    FAIL_BEFORE_PUBLISH.with(|hook| hook.set(true));
    assert!(transaction.commit(&f.backend, &f.writer, &capture).is_err());
    drop(transaction);
    let mut recovered = f.anchor.lock(&f.backend).unwrap();
    assert!(recovered.observe(&f.backend, &f.writer, &f.name).is_err());
    assert!(
        recovered
            .capture(&f.backend, &f.writer, &f.name, "next\n", false)
            .is_err(),
        "a retry cannot mint a later origin"
    );
    recovered.commit(&f.backend, &f.writer, &capture).unwrap();
    assert_eq!(
        recovered.observe(&f.backend, &f.writer, &f.name).unwrap(),
        "next\n"
    );
}

#[test]
fn an_existing_pending_file_is_not_proof_of_its_own_covering_flush() {
    let f = Fixture::new();
    let mut transaction = f.anchor.lock(&f.backend).unwrap();
    let capture = f.change(&mut transaction, "next\n");
    FAIL_PUBLICATION_FLUSH.with(|hook| hook.set(true));
    assert!(transaction.commit(&f.backend, &f.writer, &capture).is_err());
    drop(transaction);
    let mut recovered = f.anchor.lock(&f.backend).unwrap();
    assert!(recovered.observe(&f.backend, &f.writer, &f.name).is_err());
    FAIL_PUBLICATION_FLUSH.with(|hook| hook.set(true));
    assert!(
        recovered.commit(&f.backend, &f.writer, &capture).is_err(),
        "retry must cover the exact metadata FILE too"
    );
    recovered.commit(&f.backend, &f.writer, &capture).unwrap();
    assert_eq!(
        recovered.observe(&f.backend, &f.writer, &f.name).unwrap(),
        "next\n"
    );
    FAIL_PUBLICATION_FLUSH.with(|hook| hook.set(true));
    assert!(
        recovered.commit(&f.backend, &f.writer, &capture).is_err(),
        "an existing final name also requires its file flush"
    );
    recovered.commit(&f.backend, &f.writer, &capture).unwrap();
}

#[test]
fn torn_pending_history_and_a_missing_predecessor_block_cold_readers() {
    let f = Fixture::new();
    let mut transaction = f.anchor.lock(&f.backend).unwrap();
    let capture = f.change(&mut transaction, "next\n");
    FAIL_BEFORE_PUBLISH.with(|hook| hook.set(true));
    assert!(transaction.commit(&f.backend, &f.writer, &capture).is_err());
    drop(transaction);
    let pending = f.metadata_path("0000000000000002.pending");
    let mut permissions = std::fs::metadata(&pending).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt as _;
    permissions.set_mode(0o600);
    std::fs::set_permissions(&pending, permissions).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&pending)
        .unwrap()
        .set_len(1)
        .unwrap();
    assert!(f.anchor.lock(&f.backend).is_err());

    let g = Fixture::new();
    let mut transaction = g.anchor.lock(&g.backend).unwrap();
    let capture = g.change(&mut transaction, "next\n");
    transaction.commit(&g.backend, &g.writer, &capture).unwrap();
    drop(transaction);
    std::fs::remove_file(g.metadata_path("0000000000000001.json")).unwrap();
    assert!(g.anchor.lock(&g.backend).is_err());
}

#[test]
fn writer_aliases_contend_on_a_fresh_description_and_stale_capture_cannot_publish() {
    let f = Fixture::new();
    let alias = f.writer.try_clone().unwrap();
    let mut transaction = f.anchor.lock(&f.backend).unwrap();
    assert_eq!(
        f.anchor.lock(&f.backend).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let capture = f.change(&mut transaction, "next\n");
    transaction.commit(&f.backend, &alias, &capture).unwrap();
    drop(transaction);
    let mut other = f.anchor.lock(&f.backend).unwrap();
    assert_eq!(
        other.observe(&f.backend, &alias, &f.name).unwrap(),
        "next\n"
    );
    let later = f.change(&mut other, "last\n");
    other.commit(&f.backend, &alias, &later).unwrap();
    assert!(other.commit(&f.backend, &alias, &capture).is_err());
}

#[test]
fn only_a_durable_retired_tombstone_allows_a_missing_working_record() {
    let f = Fixture::new();
    let mut transaction = f.anchor.lock(&f.backend).unwrap();
    let retired = transaction
        .capture(&f.backend, &f.writer, &f.name, "head\n", true)
        .unwrap();
    transaction.commit(&f.backend, &f.writer, &retired).unwrap();
    assert!(
        transaction
            .capture(&f.backend, &f.writer, &f.name, "head\n", false)
            .is_err()
    );
    drop(transaction);
    std::fs::remove_file(f.record_path()).unwrap();
    f.validate().unwrap();

    let g = Fixture::new();
    std::fs::remove_file(g.record_path()).unwrap();
    assert!(g.validate().is_err());
}

fn fill_epoch(f: &Fixture, transaction: &mut Transaction) {
    loop {
        let epoch = transaction.epoch.as_ref().unwrap();
        let ordinal = transaction.latest.as_ref().unwrap().0.ordinal;
        if ordinal - epoch.base.ordinal + 1 >= (epoch::WINDOW - 1) as u64 {
            break;
        }
        transaction.ensure_room(&f.backend, &f.writer).unwrap();
        let capture = f.change(transaction, &format!("record-{ordinal}\n"));
        transaction.commit(&f.backend, &f.writer, &capture).unwrap();
    }
}

#[test]
fn epoch_rotation_bounds_physical_history_without_changing_the_record() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "ledger::cohort_ledger::checkpoint::tests::epoch_rotation_bounds_physical_history_without_changing_the_record",
    ) {
        return;
    }
    let f = Fixture::new();
    for update in 0..256 {
        let mut transaction = f.anchor.lock(&f.backend).unwrap();
        transaction.ensure_room(&f.backend, &f.writer).unwrap();
        let text = format!("record-{update}\n");
        let capture = f.change(&mut transaction, &text);
        transaction.commit(&f.backend, &f.writer, &capture).unwrap();
        assert_eq!(
            transaction.observe(&f.backend, &f.writer, &f.name).unwrap(),
            text
        );
        let directories = std::fs::read_dir(f.root().join(f.anchor.key()))
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.unwrap();
                entry
                    .file_name()
                    .to_str()
                    .unwrap()
                    .starts_with("epoch-")
                    .then_some(entry.path())
            })
            .collect::<Vec<_>>();
        assert!(directories.len() <= 2);
        let entries = directories
            .iter()
            .map(|path| std::fs::read_dir(path).unwrap().count())
            .sum::<usize>();
        assert!(
            entries <= epoch::WINDOW * 2,
            "active metadata is bounded after every actual acknowledgement"
        );
    }
}

#[test]
fn a_held_old_epoch_cannot_write_after_a_same_record_rotation() {
    let f = Fixture::new();
    let mut transaction = f.anchor.lock(&f.backend).unwrap();
    fill_epoch(&f, &mut transaction);
    let old = transaction.epoch.take().unwrap();
    let (capture, stamp) = transaction.latest.take().unwrap();
    let file = transaction.tip_file.take().unwrap();
    drop(transaction);
    let mut successor = f.anchor.lock(&f.backend).unwrap();
    successor.ensure_room(&f.backend, &f.writer).unwrap();
    capture.check(&f.backend, &f.writer).unwrap();
    assert_eq!(
        Stamp::read(&file).unwrap(),
        stamp,
        "CONTROL: original metadata remains readable and unchanged"
    );
    assert!(old.recheck(&f.anchor.directory).is_err());
    assert!(
        old.rotate(
            &f.anchor.directory,
            &f.backend,
            &f.writer,
            &capture,
            &file,
            &stamp
        )
        .is_err()
    );
    assert_eq!(
        successor.observe(&f.backend, &f.writer, &f.name).unwrap(),
        capture.text
    );
}

#[test]
fn a_copied_base_cannot_adopt_a_replacement_epoch_directory() {
    let f = Fixture::new();
    let root = f.root().join(f.anchor.key());
    let epoch = root.join("epoch-0000000000000001.noindex");
    let saved = f.root().join("original-epoch");
    let original = std::fs::read(epoch.join("base.json")).unwrap();
    std::fs::rename(&epoch, &saved).unwrap();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&epoch)
        .unwrap();
    std::fs::copy(saved.join("base.json"), epoch.join("base.json")).unwrap();
    assert_eq!(
        std::fs::read(epoch.join("base.json")).unwrap(),
        original,
        "CONTROL: canonical equal metadata bytes do not bind the new directory"
    );
    assert_eq!(bytes(&f.writer, MAX_ENTRY).unwrap().0, "head\n");
    assert!(f.anchor.lock(&f.backend).is_err());
    assert!(f.anchor.lock_recover(&f.backend).is_err());
}

#[test]
fn complete_rotation_intents_recover_but_unknown_or_restored_origins_do_not() {
    for stage in ["base-covered", "epoch-published", "predecessor-unlinked"] {
        let f = Fixture::new();
        let mut transaction = f.anchor.lock(&f.backend).unwrap();
        fill_epoch(&f, &mut transaction);
        if stage == "predecessor-unlinked" {
            transaction.ensure_room(&f.backend, &f.writer).unwrap();
            fill_epoch(&f, &mut transaction);
        }
        let before = bytes(&f.writer, MAX_ENTRY).unwrap();
        epoch::interrupt(stage);
        assert!(transaction.ensure_room(&f.backend, &f.writer).is_err());
        drop(transaction);
        if stage == "base-covered" {
            assert!(
                f.anchor.lock(&f.backend).is_err(),
                "ordinary readers do not publish a prepared epoch"
            );
            let base = File::open(
                f.root()
                    .join(f.anchor.key())
                    .join("epoch-0000000000000002.preparing.noindex")
                    .join("base.json"),
            )
            .unwrap();
            let origin = Stamp::read(&base).unwrap();
            FAIL_EXACT_FILE_FLUSH.with(|hook| {
                hook.set(Some((
                    origin.0[0].as_u64().unwrap(),
                    origin.0[1].as_u64().unwrap(),
                )))
            });
            assert!(
                f.anchor.lock_recover(&f.backend).is_err(),
                "cold recovery must cover the exact prepared metadata FILE"
            );
        }
        let mut cold = f.anchor.lock_recover(&f.backend).unwrap();
        assert_eq!(
            cold.observe(&f.backend, &f.writer, &f.name).unwrap(),
            before.0
        );
        assert_eq!(Stamp::read(&f.writer).unwrap(), before.1);
        cold.ensure_room(&f.backend, &f.writer).unwrap();
    }
    for restored in [false, true] {
        let f = Fixture::new();
        let mut transaction = f.anchor.lock(&f.backend).unwrap();
        fill_epoch(&f, &mut transaction);
        epoch::interrupt("base-covered");
        assert!(transaction.ensure_room(&f.backend, &f.writer).is_err());
        drop(transaction);
        if restored {
            let before = bytes(&f.writer, MAX_ENTRY).unwrap();
            past_the_clock_tick(&f.backend, &f.writer);
            f.writer.write_all_at(b"!", 0).unwrap();
            f.writer.write_all_at(before.0.as_bytes(), 0).unwrap();
            assert_ne!(Stamp::read(&f.writer).unwrap(), before.1);
        } else {
            let prepared = f
                .root()
                .join(f.anchor.key())
                .join("epoch-0000000000000002.preparing.noindex");
            std::fs::write(
                prepared.join("unknown"),
                b"owned only by the negative control",
            )
            .unwrap();
        }
        assert!(
            f.anchor.lock(&f.backend).is_err(),
            "a newer uncertainty must not fall back to the old epoch"
        );
        assert!(
            f.anchor.lock_recover(&f.backend).is_err(),
            "recovery cannot bless a changed working origin or an unknown preparation member"
        );
    }
}
