// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{Ledger, Machine, Need, Polled, Procs, Reading, Waiter};
use aterm_uds::spawnfd;
use std::{
    io::Read as _,
    num::NonZeroU32,
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _},
};

struct World {
    directory: Option<aterm_tempfile::TempDir>,
    root: PathBuf,
    layout: Layout,
}
impl World {
    fn new() -> Self {
        let directory = aterm_tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        for name in ["memory", "legacy", "memory/capacity", "legacy/build-slot"] {
            std::fs::DirBuilder::new()
                .mode(0o755)
                .create(root.join(name))
                .unwrap();
        }
        let layout = Layout::new(
            &root.join("memory/capacity"),
            &root.join("legacy/build-slot"),
        )
        .unwrap();
        Self {
            directory: Some(directory),
            root,
            layout,
        }
    }
    fn root(&self) -> &Path {
        &self.root
    }
    fn record(&self, kind: Ingress) -> PathBuf {
        match kind {
            Ingress::Capacity => self.layout.capacity(),
            Ingress::BuildSlot => self.layout.build_slot(),
        }
        .with_file_name(kind.record())
    }
}
impl Drop for World {
    fn drop(&mut self) {
        if std::thread::panicking()
            && let Some(directory) = self.directory.take()
        {
            eprintln!("kept failed ingress world: {}", directory.keep().display());
        }
    }
}

struct Quiet;
impl Machine for Quiet {
    fn read(&self, _: Option<&Path>) -> Reading {
        Reading {
            cores: 4,
            available: Some(8 << 30),
            physical: Some(16 << 30),
            load1: Some(0.0),
            load5: Some(0.0),
            ..Reading::default()
        }
    }
    fn procs(&self) -> Option<Procs> {
        None
    }
}
fn old_claim(ledger: &Ledger) -> crate::Claim {
    match Waiter::enter(
        ledger,
        &Quiet,
        Need::new("original legacy memory").mem(1024),
    )
    .unwrap()
    .poll(&Quiet)
    {
        Polled::Admitted(claim) => claim,
        other => panic!("private legacy claim refused: {other:?}"),
    }
}
fn late_write(directory: &File, name: &str, bytes: &[u8]) {
    let mut file = create(directory, name).unwrap();
    file.write_all(bytes).unwrap();
    aterm_durable::durable(&file).unwrap();
}
fn old_clients_refused(layout: &Layout) {
    let width = NonZeroU32::new(1).unwrap();
    assert!(crate::build_cpu::Pool::open_in(layout.capacity(), width).is_err());
    assert!(crate::build_cpu::Pool::open_existing(layout.capacity()).is_err());
    assert!(Ledger::at(layout.capacity()).list().is_err());
    // This is the actual fresh path operation performed before legacy
    // build-slot claims. Nested covered work deliberately bypasses it.
    assert!(std::fs::create_dir_all(layout.build_slot()).is_err());
    assert!(
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(layout.build_slot().join("slot-0"))
            .is_err()
    );
}

#[test]
fn both_path_fences_preserve_actual_legacy_work_and_late_writers() {
    let world = World::new();
    let pool =
        crate::build_cpu::Pool::open_in(world.layout.capacity(), NonZeroU32::new(1).unwrap())
            .unwrap();
    let _lease = pool
        .try_acquire(crate::build_cpu::Owner::current(None))
        .unwrap()
        .unwrap();
    let ledger = Ledger::at(world.layout.capacity());
    let claim = old_claim(&ledger);
    let claim_name = claim.path().file_name().unwrap().to_owned();
    let claim_bytes = std::fs::read(claim.path()).unwrap();
    let (old_memory, _) = directory(world.layout.capacity()).unwrap();
    let (old_slots, _) = directory(world.layout.build_slot()).unwrap();
    std::fs::create_dir(world.layout.build_slot().join("adopted")).unwrap();
    std::fs::write(
        world.layout.build_slot().join("adopted/original.json"),
        b"original adopted work: not a free-slot proof",
    )
    .unwrap();
    std::fs::write(world.layout.build_slot().join("queue"), b"original queue").unwrap();
    let memory_id = Identity::of(&old_memory).unwrap();
    let slots_id = Identity::of(&old_slots).unwrap();
    let mut preparation = Preparation::begin(&world.layout).unwrap();
    assert_eq!(
        preparation.observe().unwrap(),
        PendingStatus {
            capacity: NameState::LegacyDirectory,
            build_slot: NameState::LegacyDirectory,
        }
    );
    assert_eq!(
        preparation.fence_next().unwrap(),
        PendingStatus {
            capacity: NameState::FileFence,
            build_slot: NameState::LegacyDirectory,
        }
    );
    let all = preparation.fence_next().unwrap();
    assert_eq!(
        all,
        PendingStatus {
            capacity: NameState::FileFence,
            build_slot: NameState::FileFence
        }
    );
    old_clients_refused(&world.layout);
    assert!(
        pool.try_acquire(crate::build_cpu::Owner::current(None))
            .is_err()
    );
    assert!(ledger.list().is_err());
    // Existing descriptors can still write after BOTH names are fenced.
    // This is why no empty/lock observation in this API grants readiness.
    late_write(&old_memory, "late.claim", b"late memory work");
    late_write(&old_slots, "late-slot", b"late slot work");
    let memory = preparation.predecessor(Ingress::Capacity).unwrap();
    let slots = preparation.predecessor(Ingress::BuildSlot).unwrap();
    assert_eq!((memory.device, memory.inode), (memory_id.0, memory_id.1));
    assert_eq!((slots.device, slots.inode), (slots_id.0, slots_id.1));
    assert_eq!(
        std::fs::read(memory.path.join(claim_name)).unwrap(),
        claim_bytes
    );
    assert_eq!(
        std::fs::read(memory.path.join("late.claim")).unwrap(),
        b"late memory work"
    );
    assert_eq!(
        std::fs::read(slots.path.join("late-slot")).unwrap(),
        b"late slot work"
    );
    assert_eq!(
        std::fs::read(slots.path.join("adopted/original.json")).unwrap(),
        b"original adopted work: not a free-slot proof"
    );
    assert_eq!(memory.contents, Contents::EntriesPresent);
    assert_eq!(slots.contents, Contents::EntriesPresent);
    for _ in 0..3 {
        assert_eq!(preparation.fence_next().unwrap(), all);
    }
    assert!(
        Preparation::reopen(&world.layout).is_err(),
        "the original locks exclude another handle"
    );
    drop(preparation);
    let reopened = Preparation::reopen(&world.layout).unwrap();
    assert_eq!(reopened.observe().unwrap(), all);
    old_clients_refused(&world.layout);
}

#[test]
fn a_shared_target_cannot_acquire_a_different_paired_journal() {
    let world = World::new();
    let mut first = Preparation::begin(&world.layout).unwrap();
    let other_parent = world.root().join("other");
    std::fs::create_dir(&other_parent).unwrap();
    std::fs::create_dir(other_parent.join("capacity")).unwrap();
    let other = Layout::new(&other_parent.join("capacity"), world.layout.build_slot()).unwrap();
    assert!(Preparation::begin(&other).is_err());
    first.fence_next().unwrap();
    first.fence_next().unwrap();
    drop(first);
    assert!(Preparation::reopen(&other).is_err());
    assert!(
        other.capacity().is_dir(),
        "foreign intent must not mutate the other capacity"
    );
    let mut original = Preparation::reopen(&world.layout).unwrap();
    assert_eq!(
        original.fence_next().unwrap().build_slot,
        NameState::FileFence
    );
}

#[test]
fn missing_replaced_or_partial_objects_never_recreate_a_namespace() {
    for mode in ["missing-prior", "foreign-record", "blank-record"] {
        let world = World::new();
        let mut preparation = Preparation::begin(&world.layout).unwrap();
        preparation.fence_next().unwrap();
        let before = std::fs::read(world.layout.capacity()).unwrap();
        match mode {
            "missing-prior" => {
                let prior = preparation.predecessor(Ingress::Capacity).unwrap().path;
                std::fs::rename(&prior, prior.with_extension("preserved")).unwrap();
            }
            "foreign-record" => {
                let record = world.record(Ingress::Capacity);
                let bytes = std::fs::read(&record).unwrap();
                std::fs::rename(&record, record.with_extension("preserved")).unwrap();
                std::fs::write(&record, bytes).unwrap();
                std::fs::set_permissions(record, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            "blank-record" => {
                std::fs::write(world.record(Ingress::Capacity), b"").unwrap();
            }
            _ => unreachable!(),
        }
        assert!(preparation.fence_next().is_err());
        assert!(
            world.layout.build_slot().is_dir(),
            "a refusal must not advance the other ingress"
        );
        drop(preparation);
        assert!(Preparation::reopen(&world.layout).is_err());
        assert_eq!(std::fs::read(world.layout.capacity()).unwrap(), before);
        assert!(Preparation::begin(&world.layout).is_err());
    }
}

#[test]
fn foreign_host_or_account_cannot_resume_an_otherwise_identical_pending_pair() {
    for field in ["host", "uid"] {
        let world = World::new();
        let mut preparation = Preparation::begin(&world.layout).unwrap();
        preparation.fence_next().unwrap();
        preparation.fence_next().unwrap();
        let mut value: Value = aterm_json::from_slice(&preparation.bytes).unwrap();
        if field == "host" {
            value
                .as_object_mut()
                .unwrap()
                .insert("host".into(), aterm_json::json!("foreign-host"));
        } else {
            value.as_object_mut().unwrap().insert(
                "uid".into(),
                aterm_json::json!(u64::from(preparation.scope.uid) + 1),
            );
        }
        let bytes = aterm_json::to_vec(&value).unwrap();
        let paths = [
            world.record(Ingress::Capacity),
            world.record(Ingress::BuildSlot),
            world.layout.capacity().to_owned(),
            world.layout.build_slot().to_owned(),
        ];
        drop(preparation);
        // All physical inode pairs and canonical record bytes still match;
        // only serialized native host/account scope is foreign.
        for path in paths {
            std::fs::write(path, &bytes).unwrap();
        }
        assert!(Preparation::reopen(&world.layout).is_err());
        assert!(world.layout.capacity().is_file() && world.layout.build_slot().is_file());
    }
}

#[test]
fn unsafe_ancestry_and_overlapping_layouts_refuse_before_record_creation() {
    let world = World::new();
    let nested = world.layout.capacity().join("build-slot");
    std::fs::create_dir(&nested).unwrap();
    assert!(Layout::new(world.layout.capacity(), &nested).is_err());
    let alias = world.root().join("alias");
    std::os::unix::fs::symlink(world.layout.capacity().parent().unwrap(), &alias).unwrap();
    assert!(Layout::new(&alias.join("capacity"), world.layout.build_slot()).is_err());
    assert!(
        Layout::new(
            &world.root().join("memory/./capacity"),
            world.layout.build_slot()
        )
        .is_err()
    );
    std::fs::set_permissions(
        world.layout.build_slot().parent().unwrap(),
        std::fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    assert!(Preparation::begin(&world.layout).is_err());
    assert!(
        !world.record(Ingress::Capacity).exists() && !world.record(Ingress::BuildSlot).exists()
    );
    assert!(world.layout.capacity().is_dir() && world.layout.build_slot().is_dir());
}

const ACTOR: &str = "ATERM_TEST_CANONICAL_INGRESS_ROOT";
const CUT: &str = "ATERM_TEST_CANONICAL_INGRESS_CUT";
#[test]
fn preparer_actor_if_requested() {
    let Some(root) = std::env::var_os(ACTOR).map(PathBuf::from) else {
        return;
    };
    let layout = Layout::new(
        &root.join("memory/capacity"),
        &root.join("legacy/build-slot"),
    )
    .unwrap();
    let mut preparation = Preparation::begin(&layout).unwrap();
    let cut: usize = std::env::var(CUT).unwrap().parse().unwrap();
    assert!((1..=2).contains(&cut));
    for _ in 0..cut {
        preparation.fence_next().unwrap();
    }
    println!("\nINGRESS_PREPARED {cut}");
    std::io::stdout().flush().unwrap();
    let mut byte = [0];
    std::io::stdin().read_exact(&mut byte).unwrap();
    assert_eq!(byte, [b'k']);
    aterm_uds::process::kill_self_now();
}

#[test]
fn actual_preparer_sigkill_reopens_partial_and_complete_fences_as_pending_only() {
    use std::os::unix::process::ExitStatusExt as _;
    for cut in 1..=2 {
        let world = World::new();
        let (old_memory, _) = directory(world.layout.capacity()).unwrap();
        let (old_slots, _) = directory(world.layout.build_slot()).unwrap();
        let mut command = spawnfd::command(std::env::current_exe().unwrap());
        let errors = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(world.root().join(format!("preparer-{cut}.stderr")))
            .unwrap();
        command
            .args([
                "--exact",
                "canonical_ingress::tests::preparer_actor_if_requested",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(ACTOR, world.root())
            .env(CUT, cut.to_string())
            .stdin(spawnfd::Stdio::piped())
            .stdout(spawnfd::Stdio::piped())
            .stderr(errors);
        struct Actor(spawnfd::Child);
        impl Drop for Actor {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut actor = Actor(command.spawn().unwrap());
        aterm_uds::deadline::prints(
            &mut actor.0,
            &format!("INGRESS_PREPARED {cut}"),
            aterm_spec::wait::MUST_HAPPEN,
        );
        assert!(
            Preparation::reopen(&world.layout).is_err(),
            "actual child holds original preparation locks"
        );
        actor.0.stdin.as_mut().unwrap().write_all(b"k").unwrap();
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "actual ingress preparer SIGKILL",
            || actor.0.try_wait().unwrap(),
        );
        assert_eq!(actor.0.wait().unwrap().signal(), Some(libc::SIGKILL));
        late_write(
            &old_memory,
            "post-crash.claim",
            b"still unknown memory work",
        );
        late_write(&old_slots, "post-crash-slot", b"still unknown slot work");
        let mut reopened = Preparation::reopen(&world.layout).unwrap();
        let status = reopened.observe().unwrap();
        assert_eq!(status.capacity, NameState::FileFence);
        assert_eq!(
            status.build_slot,
            if cut == 1 {
                NameState::LegacyDirectory
            } else {
                NameState::FileFence
            }
        );
        assert_eq!(
            reopened.predecessor(Ingress::Capacity).unwrap().contents,
            Contents::EntriesPresent
        );
        assert_eq!(
            reopened.predecessor(Ingress::BuildSlot).unwrap().contents,
            Contents::EntriesPresent
        );
        let all = reopened.fence_next().unwrap();
        assert_eq!(
            all,
            PendingStatus {
                capacity: NameState::FileFence,
                build_slot: NameState::FileFence
            }
        );
        old_clients_refused(&world.layout);
        let memory = reopened.predecessor(Ingress::Capacity).unwrap();
        let slots = reopened.predecessor(Ingress::BuildSlot).unwrap();
        assert_eq!(
            std::fs::read(memory.path.join("post-crash.claim")).unwrap(),
            b"still unknown memory work"
        );
        assert_eq!(
            std::fs::read(slots.path.join("post-crash-slot")).unwrap(),
            b"still unknown slot work"
        );
        assert!(!memory.path.join(".ready").exists() && !slots.path.join(".ready").exists());
        assert_eq!(reopened.fence_next().unwrap(), all);
    }
}
