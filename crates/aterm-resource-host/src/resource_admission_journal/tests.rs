// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::num::NonZeroU32;
use std::os::unix::fs::PermissionsExt as _;

struct World {
    journal: Journal,
    namespace: Namespace,
    debt: ExternalDebt,
    legacy: PathBuf,
    scratch: Option<aterm_tempfile::TempDir>,
}

impl World {
    fn new() -> Self {
        let scratch = aterm_tempfile::tempdir().unwrap();
        eprintln!(
            "durable journal retry fixture: {}",
            scratch.path().display()
        );
        let home = scratch.path().join("home");
        let legacy = scratch.path().join("legacy");
        for path in [&home, &legacy] {
            std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        let namespace = Namespace::create_private(scratch.path(), &home).unwrap();
        let pool = Pool::open_in(
            &scratch.path().join("capacity"),
            NonZeroU32::new(2).unwrap(),
        )
        .unwrap();
        let retired = LegacyRetirement::retire(&legacy, 1)
            .expect("scoped retirement requires a non-root test process");
        let debt = ExternalDebt::activate(&pool, &retired).unwrap();
        Journal::prepare(&namespace, &debt).unwrap();
        let journal = Journal::open(&namespace, &debt).unwrap();
        Self {
            journal,
            namespace,
            debt,
            legacy,
            scratch: Some(scratch),
        }
    }

    fn binding(&self, attempt: u8) -> Binding {
        Binding {
            session: "s-acde".into(),
            attempt: [attempt; 16],
        }
    }

    fn record(&self, binding: &Binding, phase: &str) -> Value {
        aterm_json::json!({"version":3,"session":binding.session,
            "attempt":aterm_codec::hex::encode_lower(&binding.attempt),"phase":phase})
    }

    fn done(&self, binding: &Binding) -> PathBuf {
        self.journal.done.join(name(binding).unwrap())
    }

    fn assert_retry_refuses(&self, binding: &Binding) {
        assert!(self.journal.settle(binding).is_err());
        assert!(self.journal.finish(binding).is_err());
        assert_eq!(self.debt.reserved().unwrap(), 0);
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.legacy, std::fs::Permissions::from_mode(0o700));
        if std::thread::panicking()
            && let Some(scratch) = self.scratch.take()
        {
            eprintln!(
                "preserved journal retry fixture: {}",
                scratch.keep().display()
            );
        }
    }
}

fn private_bytes(path: &std::path::Path, bytes: &[u8]) {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    aterm_durable::durable(&file).unwrap();
}

#[test]
fn durable_journal_settlement_retries_real_rename_without_cpu_authority() {
    let world = World::new();
    // No ARM or child exists in this fixture. Keep actual original CPU capabilities to
    // show that acknowledging a tombstone itself never settles or reacquires CPU debt.
    let cpu = world
        .debt
        .try_reserve(
            aterm_capacity::build_cpu::Owner::current(None),
            NonZeroU32::new(1).unwrap(),
            2,
        )
        .unwrap()
        .unwrap();
    assert_eq!(world.debt.reserved().unwrap(), 1);
    let binding = world.binding(1);
    world.journal.create(&binding).unwrap();
    world
        .journal
        .update(&binding, "settled", aterm_json::json!({}))
        .unwrap();
    let active = world.journal.active.join(name(&binding).unwrap());
    let done = world.done(&binding);
    // Real durable data write followed by the actual rename, deliberately without its
    // acknowledgment/parent coverage: the exact state left by the reported I/O failure.
    std::fs::rename(&active, &done).unwrap();
    let identity_before = identity(&private(&done, false).unwrap()).unwrap();
    let bytes_before = std::fs::read(&done).unwrap();
    let ready_before = std::fs::read(world.journal.root.join("ready")).unwrap();
    for _ in 0..2 {
        world.journal.settle(&binding).unwrap();
        world.journal.finish(&binding).unwrap();
        assert!(!active.exists());
        assert_eq!(
            identity(&private(&done, false).unwrap()).unwrap(),
            identity_before
        );
        assert_eq!(std::fs::read(&done).unwrap(), bytes_before);
        assert_eq!(world.debt.reserved().unwrap(), 1);
    }
    let reopened = Journal::open(&world.namespace, &world.debt).unwrap();
    reopened.settle(&binding).unwrap();
    assert!(!reopened.fresh(&binding).unwrap());
    assert!(reopened.outstanding().unwrap().is_empty());
    assert_eq!(
        std::fs::read(world.journal.root.join("ready")).unwrap(),
        ready_before
    );

    let active_binding = world.binding(2);
    world.journal.create(&active_binding).unwrap();
    world.journal.settle(&active_binding).unwrap();
    world.journal.settle(&active_binding).unwrap();
    assert_eq!(
        read(&world.done(&active_binding)).unwrap()["phase"].as_str(),
        Some("settled")
    );
    assert_eq!(world.debt.reserved().unwrap(), 1);
    for lease in cpu {
        lease.settle().unwrap();
    }
    assert_eq!(world.debt.reserved().unwrap(), 0);
}

#[test]
fn durable_journal_settlement_refuses_missing_wrong_or_untrusted_done() {
    for fault in [
        "missing",
        "malformed",
        "misbound",
        "version",
        "unsettled",
        "both",
        "symlink",
        "hardlink",
        "public",
        "directory",
        "oversized",
        "foreign-directory",
    ] {
        let world = World::new();
        let binding = world.binding(3);
        let done = world.done(&binding);
        let active = world.journal.active.join(name(&binding).unwrap());
        let mut record = world.record(&binding, "settled");
        match fault {
            "missing" => {}
            "malformed" => private_bytes(&done, b"{\"version\":"),
            "misbound" => {
                record = world.record(&world.binding(4), "settled");
                write_new(&done, &record).unwrap();
            }
            "version" => {
                record
                    .as_object_mut()
                    .unwrap()
                    .insert("version".into(), Value::from(4u64));
                write_new(&done, &record).unwrap();
            }
            "unsettled" => {
                record = world.record(&binding, "running");
                write_new(&done, &record).unwrap();
            }
            "both" => {
                world.journal.create(&binding).unwrap();
                write_new(&done, &record).unwrap();
            }
            "symlink" | "hardlink" => {
                let original = world.journal.root.join("unrelated-private-file");
                write_new(&original, &record).unwrap();
                if fault == "symlink" {
                    std::os::unix::fs::symlink(&original, &done).unwrap();
                } else {
                    std::fs::hard_link(&original, &done).unwrap();
                }
            }
            "public" => {
                write_new(&done, &record).unwrap();
                std::fs::set_permissions(&done, std::fs::Permissions::from_mode(0o644)).unwrap();
            }
            "directory" => std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&done)
                .unwrap(),
            "oversized" => {
                let mut bytes = record.to_string().into_bytes();
                bytes.resize(MAX_RECORD as usize + 1, b' ');
                private_bytes(&done, &bytes);
            }
            "foreign-directory" => {
                std::fs::rename(
                    &world.journal.done,
                    world.journal.root.join("original-done"),
                )
                .unwrap();
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&world.journal.done)
                    .unwrap();
                write_new(&done, &record).unwrap();
                assert!(Journal::open(&world.namespace, &world.debt).is_err());
            }
            _ => unreachable!(),
        }
        let active_before = std::fs::read(&active).ok();
        let done_before = std::fs::read(&done).ok();
        let link_before = std::fs::read_link(&done).ok();
        let entry = |path: &std::path::Path| {
            std::fs::symlink_metadata(path)
                .ok()
                .map(|m| (m.dev(), m.ino(), m.mode(), m.nlink(), m.len()))
        };
        let active_entry = entry(&active);
        let done_entry = entry(&done);
        world.assert_retry_refuses(&binding);
        assert_eq!(
            entry(&active),
            active_entry,
            "preserve active inode/type for {fault}"
        );
        assert_eq!(
            entry(&done),
            done_entry,
            "preserve done inode/type for {fault}"
        );
        assert_eq!(
            std::fs::read(&active).ok(),
            active_before,
            "preserve active for {fault}"
        );
        assert_eq!(
            std::fs::read(&done).ok(),
            done_before,
            "preserve done for {fault}"
        );
        assert_eq!(
            std::fs::read_link(&done).ok(),
            link_before,
            "preserve original links for {fault}"
        );
        if fault == "missing" {
            assert!(!active.exists() && !done.exists());
        }
    }
}
