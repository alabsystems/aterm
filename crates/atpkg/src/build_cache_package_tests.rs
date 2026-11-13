// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Disposable package policy fixtures, using only private directories and inert producers.

use super::super::super::fence::Fence;
use super::*;
use std::cell::Cell;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};

const FIRST: &str = "11111111111111111111111111111111";
const SECOND: &str = "22222222222222222222222222222222";
const THIRD: &str = "33333333333333333333333333333333";

struct Inert {
    group_gone: Cell<bool>,
    retired: Cell<bool>,
    authority: Cell<bool>,
    free: Cell<u64>,
}

impl Default for Inert {
    fn default() -> Self {
        Self {
            group_gone: Cell::new(true),
            retired: Cell::new(false),
            authority: Cell::new(true),
            free: Cell::new(100 << 30),
        }
    }
}

impl Kernel for Inert {
    fn capture(&self, pid: u32, group: u32) -> Option<Producer> {
        Some(Producer {
            pid,
            birth: 10,
            group,
            boot: "inert-boot".into(),
            namespace: None,
        })
    }
    fn group_gone(&self, _: &Producer) -> bool {
        self.group_gone.get()
    }
    fn retired(&self, _: &Producer) -> bool {
        self.retired.get()
    }
    fn guardian_live(&self, _: &Producer) -> bool {
        true
    }
    fn settlement_authority(&self, _: &Producer) -> bool {
        self.authority.get()
    }
    fn free(&self, _: &Path) -> Option<u64> {
        Some(self.free.get())
    }
}

struct World {
    _directory: Option<crate::testdir::Dir>,
    home: PathBuf,
    cache: PathBuf,
    target: PathBuf,
    manifest: PathBuf,
    preserve: PathBuf,
    held: Option<crate::lock::Flock>,
    kernel: Inert,
}

impl World {
    fn new() -> Self {
        let directory = crate::testdir::dir("package-policy.noindex-");
        let home = directory.canonicalize().unwrap().join("home");
        Self::in_home(home, Some(directory))
    }

    fn in_home(home: PathBuf, directory: Option<crate::testdir::Dir>) -> Self {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&home)
            .unwrap();
        let cache = home.join("cache.noindex");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&cache)
            .unwrap();
        let owner = home.join("source");
        std::fs::create_dir(&owner).unwrap();
        let retention = home.join("retention");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&retention)
            .unwrap();
        let manifest = owner.join("Cargo.toml");
        std::fs::write(&manifest, "[workspace]\nmembers = []\n").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(cache.join(COORDINATOR))
            .unwrap();
        let mut world = Self {
            _directory: directory,
            home,
            target: cache.join("release.noindex"),
            cache,
            manifest,
            preserve: retention.join("preserve-builds"),
            held: None,
            kernel: Inert::default(),
        };
        world.lock();
        world
    }

    fn lock(&mut self) {
        assert!(self.held.is_none());
        let file = crate::metadata_io::open_regular(&self.cache.join(COORDINATOR)).unwrap();
        self.held = Some(crate::lock::Flock::try_lock(file).unwrap());
    }

    fn workspace(&mut self, number: usize) {
        let workspace = self.home.join(format!("source-{number}"));
        std::fs::create_dir(&workspace).unwrap();
        self.manifest = workspace.join("Cargo.toml");
        std::fs::write(&self.manifest, "[workspace]\nmembers = []\n").unwrap();
    }

    fn admit(&self, attempt: &str, budget: u64) -> Result<aterm_json::Value, String> {
        admit_on(
            &self.home,
            &Admission {
                cache: &self.cache,
                target: &self.target,
                manifest: &self.manifest,
                preserve_if_present: &self.preserve,
                budget,
                attempt,
                producer_pid: 100,
                producer_group: 101,
            },
            true,
            &self.kernel,
            &Fence::default(),
        )
    }

    fn output(&self, bytes: usize) {
        std::fs::create_dir_all(self.target.join("debug/deps")).unwrap();
        std::fs::write(self.target.join("debug/.cargo-lock"), b"").unwrap();
        std::fs::write(self.target.join("debug/deps/unit"), vec![0x5a; bytes]).unwrap();
    }

    fn receipt(&self) -> Receipt {
        super::super::read(&self.path()).unwrap()
    }
    fn path(&self) -> PathBuf {
        receipt_path(&self.home, &self.target).unwrap()
    }
    fn settle(&self, attempt: &str, success: bool) -> Result<aterm_json::Value, String> {
        settled_on(
            &self.home,
            &self.cache,
            attempt,
            success,
            &self.kernel,
            &Fence::default(),
        )
    }

    fn reclaim(&self, fence: &Fence<'_>) -> Vec<(PathBuf, super::super::super::Unused)> {
        self.reclaim_at(fence, std::time::SystemTime::now())
    }

    fn reclaim_at(
        &self,
        fence: &Fence<'_>,
        now: std::time::SystemTime,
    ) -> Vec<(PathBuf, super::super::super::Unused)> {
        super::super::reclaim_selected_on(
            &self.home,
            now,
            super::super::super::ReclaimAfter::CALM,
            None,
            &mut |_, _| true,
            fence,
            super::super::Selection::Package(Context {
                cache: Some(&self.cache),
                kernel: &self.kernel,
            }),
        )
    }
}

#[test]
fn advisory_package_inventory_admits_a_producer_while_cleanup_excludes_it() {
    use std::cell::RefCell;
    #[derive(Clone)]
    struct Probe {
        home: PathBuf,
        cache: PathBuf,
        target: PathBuf,
        manifest: PathBuf,
        preserve: PathBuf,
    }
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Observation {
        Busy,
        Admitted,
    }
    thread_local! {
        static INPUT: RefCell<Option<Probe>> = const { RefCell::new(None) };
        static OUTPUT: Cell<Option<Observation>> = const { Cell::new(None) };
    }
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            INPUT.with(|input| input.borrow_mut().take());
            OUTPUT.set(None);
        }
    }
    fn producer() {
        let Some(probe) = INPUT.with(|input| input.borrow_mut().take()) else {
            return;
        };
        let file = crate::metadata_io::open_regular(&probe.cache.join(COORDINATOR)).unwrap();
        let observation = match crate::lock::Flock::try_lock(file) {
            Err(std::fs::TryLockError::WouldBlock) => Observation::Busy,
            Err(std::fs::TryLockError::Error(error)) => panic!("coordinator: {error}"),
            Ok(_held) => {
                let kernel = Inert::default();
                admit_on(
                    &probe.home,
                    &Admission {
                        cache: &probe.cache,
                        target: &probe.target,
                        manifest: &probe.manifest,
                        preserve_if_present: &probe.preserve,
                        budget: 1 << 30,
                        attempt: SECOND,
                        producer_pid: 100,
                        producer_group: 101,
                    },
                    true,
                    &kernel,
                    &Fence::default(),
                )
                .unwrap();
                settled_on(
                    &probe.home,
                    &probe.cache,
                    SECOND,
                    true,
                    &kernel,
                    &Fence::default(),
                )
                .unwrap();
                Observation::Admitted
            }
        };
        OUTPUT.set(Some(observation));
    }
    fn observe(probe: Probe, step: impl FnOnce()) -> Observation {
        INPUT.with(|input| *input.borrow_mut() = Some(probe));
        aterm_uds::progress::heard_by(producer, step);
        OUTPUT.take().expect("the inventory callback ran")
    }
    let mut world = World::new();
    world.admit(FIRST, 1 << 30).unwrap();
    world.output(64);
    world.settle(FIRST, true).unwrap();
    world.held.take();
    let probe = Probe {
        home: world.home.clone(),
        cache: world.cache.clone(),
        target: world.target.clone(),
        manifest: world.manifest.clone(),
        preserve: world.preserve.clone(),
    };
    let payload = std::fs::read(world.target.join("debug/deps/unit")).unwrap();
    let _clear = Clear;
    assert_eq!(
        observe(probe.clone(), || {
            let _ = super::super::plan(&world.home, std::time::SystemTime::now());
        }),
        Observation::Admitted
    );
    assert_eq!(
        world.receipt().package.unwrap().last_settled.as_deref(),
        Some(SECOND)
    );
    assert_eq!(
        observe(probe, || {
            assert!(world.reclaim(&Fence::default()).is_empty());
        }),
        Observation::Busy
    );
    assert_eq!(
        std::fs::read(world.target.join("debug/deps/unit")).unwrap(),
        payload
    );
}

#[test]
fn interrupted_creation_keeps_package_custody_and_its_fresh_attempt() {
    use super::super::creation::{Point, faults};
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            faults::clear();
        }
    }
    for point in [
        Point::Created,
        Point::Marker,
        Point::Intent,
        Point::Publishing,
        Point::Renamed,
        Point::Committed,
    ] {
        let world = World::new();
        faults::set(move |seen| if seen == point { None } else { Some(()) });
        let guard = Clear;
        assert!(world.admit(FIRST, 1).is_err(), "{point:?}");
        drop(guard);
        let before = super::super::read(&world.path());
        let admitted = world.admit(FIRST, 1).unwrap();
        assert_eq!(admitted["budgeted"].as_bool(), Some(true), "{point:?}");
        assert_eq!(
            admitted["created"].as_bool(),
            Some(true),
            "{point:?}: genuinely empty"
        );
        let after = world.receipt();
        assert_eq!(after.schema, 3);
        assert!(after.creating.is_none());
        let policy = after.package.as_ref().unwrap();
        assert!(policy.active.as_ref().unwrap().fresh);
        assert!(policy.declaration.preserve_if_present.is_some());
        if let Some(before) = before {
            assert_eq!(before.identity, after.identity);
        }
    }
}

#[test]
fn a_creation_rechecks_the_coordinator_and_a_late_preservation_marker_before_admission() {
    use super::super::creation::{Point, faults};
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            faults::clear();
        }
    }
    for phase in [Point::Publishing, Point::Renamed] {
        for veto in ["coordinator", "preserve"] {
            let mut world = World::new();
            let mut held = if veto == "coordinator" {
                world.held.take()
            } else {
                None
            };
            let preserve = world.preserve.clone();
            faults::set(move |point| {
                if point == phase {
                    if veto == "coordinator" {
                        drop(held.take());
                    } else {
                        std::fs::write(&preserve, b"keep").unwrap();
                    }
                }
                Some(())
            });
            let guard = Clear;
            assert!(world.admit(FIRST, 1).is_err(), "{veto}");
            drop(guard);
            assert_eq!(
                world.target.exists(),
                phase == Point::Renamed,
                "a late veto never commits the creation intent"
            );
            assert_eq!(world.receipt().schema, 5);
            if veto == "coordinator" {
                world.lock();
            } else {
                std::fs::remove_file(&world.preserve).unwrap();
            }
            assert_eq!(
                world.admit(FIRST, 1).unwrap()["budgeted"].as_bool(),
                Some(true)
            );
        }
    }
}

#[test]
fn ordinary_clearance_keeps_the_whole_package_coordinator_for_v2_and_legacy_custody() {
    let world = World::new();
    world.admit(FIRST, 1).unwrap();
    for legacy in [false, true] {
        if legacy {
            let mut receipt = world.receipt();
            receipt.schema = 2;
            receipt
                .package
                .as_mut()
                .unwrap()
                .declaration
                .preserve_if_present = None;
            super::super::write(&world.path(), &receipt).unwrap();
        }
        assert!(
            super::super::ordinary_clearance(&world.home, &world.cache.join("unregistered-output"))
                .is_err()
        );
        assert!(super::super::ordinary_clearance(&world.home, &world.cache).is_err());
        assert!(
            super::super::ordinary_clearance(&world.home, &world.home.join("unrelated")).is_ok()
        );
    }
}

#[test]
fn a_late_preserve_request_keeps_active_custody_without_learning_a_success() {
    let world = World::new();
    world.admit(FIRST, 1).unwrap();
    world.output(32 << 10);
    std::fs::write(&world.preserve, b"").unwrap();
    let before = world.receipt();
    assert!(held_coordinator(
        &world.home,
        &before.package.as_ref().unwrap().declaration
    ));
    assert!(world.settle(FIRST, true).is_err());
    assert_eq!(world.receipt(), before);
    assert!(world.receipt().package.unwrap().learned_budget.is_none());
    std::fs::remove_file(&world.preserve).unwrap();
    world.settle(FIRST, true).unwrap();
    assert!(world.receipt().package.unwrap().learned_budget.is_some());
}

#[test]
fn preservation_is_rechecked_after_the_settlement_or_recovery_process_fence() {
    for recover in [false, true] {
        let mut world = World::new();
        world.admit(FIRST, 1).unwrap();
        world.output(32 << 10);
        let before = world.receipt();
        let called = Cell::new(false);
        let request = |_: &[PathBuf], _: &[u32]| {
            called.set(true);
            std::fs::write(&world.preserve, b"").unwrap();
            Ok(aterm_uds::holders::PathScan::default())
        };
        let fence = Fence::new(&request);
        if recover {
            world.held.take();
            world.kernel.retired.set(true);
            assert!(world.reclaim(&fence).is_empty());
        } else {
            assert!(
                settled_on(
                    &world.home,
                    &world.cache,
                    FIRST,
                    true,
                    &world.kernel,
                    &fence
                )
                .is_err()
            );
        }
        assert!(called.get());
        assert_eq!(world.receipt(), before);
        assert!(world.target.join("debug/deps/unit").exists());
    }
}

#[test]
fn a_late_preserve_request_keeps_every_generation_and_resumed_payload() {
    for phase in 0..3 {
        let mut world = World::new();
        world.admit(FIRST, 1).unwrap();
        world.output(32 << 10);
        let mut older_policy = world.receipt().package.unwrap();
        older_policy.active.as_mut().unwrap().token = SECOND.into();
        world.settle(FIRST, false).unwrap();
        let older = world.cache.join("previous.noindex");
        let workspace = world.manifest.parent().unwrap();
        if phase == 0 {
            super::super::create_with_policy(
                &world.home,
                &older,
                workspace,
                Some(older_policy),
                false,
            )
            .unwrap();
            std::fs::create_dir_all(older.join("debug/deps")).unwrap();
            std::fs::write(older.join("debug/.cargo-lock"), b"").unwrap();
            std::fs::write(older.join("debug/deps/unit"), vec![0x5a; 32 << 10]).unwrap();
            world.settle(SECOND, false).unwrap();
        }
        let payload = if phase > 0 {
            let stage = world.cache.join("retiring.noindex");
            super::super::stage_for_process_test(&world.home, &world.target, &stage);
            let mut receipt = world.receipt();
            receipt.pending.as_mut().unwrap().authorized = phase == 2;
            super::super::write(&world.path(), &receipt).unwrap();
            let payload = stage.join("cache.noindex");
            if phase == 2 {
                // A previous partial removal can already have removed the embedded receipt.
                std::fs::remove_file(payload.join(super::super::MARKER)).unwrap();
            }
            payload
        } else {
            world.target.clone()
        };
        std::fs::write(&world.preserve, b"").unwrap();
        world.held.take();
        assert!(world.reclaim(&Fence::default()).is_empty());
        assert_eq!(
            compact(&world.home, &world.cache, &Fence::default()).unwrap()["status"].as_str(),
            Some("deferred")
        );
        assert!(payload.join("debug/deps/unit").exists());
        assert_eq!(older.join("debug/deps/unit").exists(), phase == 0);
        std::fs::remove_file(&world.preserve).unwrap();
        assert_eq!(
            world.reclaim(&Fence::default()).len(),
            if phase == 0 { 2 } else { 1 }
        );
        assert!(!payload.exists());
        assert!(!older.exists());
    }
}

#[test]
fn a_preserve_request_after_candidate_recording_prevents_the_rename() {
    let mut world = World::new();
    world.admit(FIRST, 1).unwrap();
    world.output(32 << 10);
    world.settle(FIRST, false).unwrap();
    world.held.take();
    let identity = super::super::identity(&world.target);
    let mut recorded = false;
    let removed = super::super::reclaim_selected_on(
        &world.home,
        std::time::SystemTime::now(),
        super::super::super::ReclaimAfter::CALM,
        None,
        &mut |_, _| {
            recorded = true;
            std::fs::write(&world.preserve, b"").unwrap();
            true
        },
        &Fence::default(),
        super::super::Selection::Package(Context {
            cache: Some(&world.cache),
            kernel: &world.kernel,
        }),
    );
    assert!(recorded);
    assert_eq!(
        removed,
        [(
            world.target.clone(),
            super::super::super::Unused::Refused {
                by: super::super::super::Refusal::Stopped
            }
        )],
        "its intent closed, nothing of it gone"
    );
    assert_eq!(super::super::identity(&world.target), identity);
    assert!(world.target.join("debug/deps/unit").exists());
    std::fs::remove_file(&world.preserve).unwrap();
    assert!(world.receipt().pending.is_some());
    // This journal never renamed its source; recover it before requesting fresh retirement.
    assert!(world.reclaim(&Fence::default()).is_empty());
    assert!(world.receipt().pending.is_none());
    assert_eq!(world.reclaim(&Fence::default()).len(), 1);
    assert!(!world.target.exists());
}

#[test]
fn preservation_during_a_fresh_or_resumed_walk_keeps_the_remaining_payload_and_journal() {
    use super::super::super::remove::seams;
    use std::rc::Rc;
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            seams::clear();
        }
    }
    for resumed in [false, true] {
        let mut world = World::new();
        world.admit(FIRST, 1).unwrap();
        world.output(32 << 10);
        std::fs::create_dir(world.target.join("remaining")).unwrap();
        std::fs::write(
            world.target.join("remaining/precious"),
            b"preserve these remaining bytes",
        )
        .unwrap();
        world.settle(FIRST, false).unwrap();
        let files = package_file_allocations(&world.target);
        if resumed {
            let stage = world.cache.join("retiring.noindex");
            super::super::stage_for_process_test(&world.home, &world.target, &stage);
        }
        world.held.take();
        let clear = Clear;
        let requested = Rc::new(Cell::new(false));
        let (seen, marker) = (Rc::clone(&requested), world.preserve.clone());
        seams::on(seams::At::Opening, move |name| {
            if name == "remaining" {
                std::fs::write(&marker, b"keep the remainder").unwrap();
                seen.set(true);
            }
        });
        let removed = world.reclaim(&Fence::default());
        assert!(requested.get());
        assert_eq!(removed.len(), 1);
        let super::super::super::Unused::Revoked { freed } = removed[0].1 else {
            panic!("revocation must not claim complete removal: {removed:?}");
        };
        let pending = world
            .receipt()
            .pending
            .expect("revoked cleanup remains journalled");
        let payload = pending.directory.join("cache.noindex");
        assert_eq!(
            std::fs::read(payload.join("remaining/precious")).unwrap(),
            b"preserve these remaining bytes"
        );
        let actually_freed: u64 = files
            .into_iter()
            .filter(|(name, _)| !payload.join(name).exists())
            .map(|(_, bytes)| bytes)
            .sum();
        assert_eq!(freed, actually_freed);
        drop(clear);
        std::fs::remove_file(&world.preserve).unwrap();
        assert_eq!(world.reclaim(&Fence::default()).len(), 1);
        assert!(!pending.directory.exists());
        assert!(world.receipt().pending.is_none());
    }
}

struct ClearRemovalSeams;

impl Drop for ClearRemovalSeams {
    fn drop(&mut self) {
        super::super::super::remove::seams::clear();
    }
}

fn released_retirement(resumed: bool) -> World {
    let mut world = World::new();
    world.admit(FIRST, 1).unwrap();
    world.output(32 << 10);
    world.settle(FIRST, false).unwrap();
    if resumed {
        let stage = world.cache.join("retiring.noindex");
        super::super::stage_for_process_test(&world.home, &world.target, &stage);
    }
    world.held.take();
    world
}

fn package_file_allocations(payload: &Path) -> Vec<(PathBuf, u64)> {
    let mut files = Vec::new();
    let mut directories = vec![payload.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                directories.push(path);
            } else {
                // These fixtures have one name per file. Measure every actual file,
                // including cache metadata, without depending on the removal walker.
                assert_eq!(metadata.nlink(), 1);
                files.push((
                    path.strip_prefix(payload).unwrap().to_path_buf(),
                    metadata.blocks() * 512,
                ));
            }
        }
    }
    files
}

fn partial_allocation(removed: &[(PathBuf, super::super::super::Unused)]) -> u64 {
    use super::super::super::Unused;
    removed
        .iter()
        .map(|(_, why)| match why {
            Unused::Stayed { freed, .. } | Unused::Revoked { freed } => *freed,
            _ => panic!("a retained retirement must not be reported complete: {removed:?}"),
        })
        .sum()
}

/// RETIRING A PACKAGE TARGET COSTS A COLD BUILD, SO A PASS LEAVES IT TO THE END: its first run does
/// not touch one, so what the shared cache frees reaches its low-disk reason first, and its second
/// retires it. The second also asks once, not for each run.
#[test]
fn a_pass_leaves_a_package_target_to_its_second_run() {
    let world = released_retirement(false);
    let now = std::time::SystemTime::now();
    let after = super::super::super::ReclaimAfter::CALM;
    let fence = Fence::default();

    let mut asked = 0;
    let first = super::super::reclaim_dead_on(
        &world.home,
        now,
        after.into(),
        None,
        &mut |_, _| {
            asked += 1;
            true
        },
        &fence,
    );
    assert_eq!((first, asked), (Vec::new(), 0));
    assert!(world.target.join("debug/deps/unit").exists());

    let second = super::super::reclaim_live_on(
        &world.home,
        now,
        after.into(),
        None,
        &mut |_, _| {
            asked += 1;
            true
        },
        &fence,
    );
    assert_eq!(asked, 1, "{second:?}");
    assert!(
        matches!(
            second.as_slice(),
            [(_, super::super::super::Unused::PackageBudget { .. })]
        ),
        "{second:?}"
    );
    assert!(!world.target.exists());
}

#[test]
fn fresh_and_resumed_walks_do_not_adopt_a_replaced_stage() {
    use super::super::super::remove::seams;
    use std::rc::Rc;
    for resumed in [false, true] {
        let world = released_retirement(resumed);
        let original = world.home.join("saved-stage.noindex");
        let requested = Rc::new(Cell::new(false));
        let (seen, path, saved) = (Rc::clone(&requested), world.path(), original.clone());
        let clear = ClearRemovalSeams;
        seams::on(seams::At::Starting, move |name| {
            if name != "cache.noindex" || seen.replace(true) {
                return;
            }
            let pending = super::super::read(&path).unwrap().pending.unwrap();
            std::fs::rename(&pending.directory, &saved).unwrap();
            std::fs::create_dir_all(pending.directory.join("cache.noindex")).unwrap();
            std::fs::write(
                pending.directory.join("cache.noindex/owner-note"),
                b"keep the replacement stage",
            )
            .unwrap();
        });
        let removed = world.reclaim(&Fence::default());
        assert!(
            requested.get(),
            "reach the stage handoff: resumed={resumed}"
        );
        assert_eq!(partial_allocation(&removed), 0);
        let pending = world
            .receipt()
            .pending
            .expect("retain the original identity");
        assert_eq!(
            super::super::identity(&original).as_ref(),
            Some(&pending.identity)
        );
        assert_ne!(
            super::super::identity(&pending.directory).as_ref(),
            Some(&pending.identity)
        );
        assert_eq!(
            std::fs::read(original.join("cache.noindex/debug/deps/unit")).unwrap(),
            vec![0x5a; 32 << 10]
        );
        let note = pending.directory.join("cache.noindex/owner-note");
        assert_eq!(std::fs::read(&note).unwrap(), b"keep the replacement stage");
        drop(clear);
        assert!(world.reclaim(&Fence::default()).is_empty());
        assert_eq!(world.receipt().pending.unwrap().identity, pending.identity);
        assert_eq!(std::fs::read(note).unwrap(), b"keep the replacement stage");
    }
}

#[test]
fn fresh_and_resumed_walks_do_not_retry_into_a_replaced_payload() {
    use super::super::super::remove::seams;
    use std::rc::Rc;
    for resumed in [false, true] {
        let world = released_retirement(resumed);
        let original = world.home.join("saved-payload.noindex");
        let requested = Rc::new(Cell::new(false));
        let (seen, path, saved) = (Rc::clone(&requested), world.path(), original.clone());
        let clear = ClearRemovalSeams;
        seams::on(seams::At::Opening, move |name| {
            if name != "cache.noindex" || seen.replace(true) {
                return;
            }
            let pending = super::super::read(&path).unwrap().pending.unwrap();
            let payload = pending.directory.join("cache.noindex");
            std::fs::rename(&payload, &saved).unwrap();
            std::fs::create_dir(&payload).unwrap();
            // No source marker: the old identity alone must prevent a retry adopting this.
            std::fs::write(payload.join("owner-note"), b"keep the replacement payload").unwrap();
        });
        let removed = world.reclaim(&Fence::default());
        assert!(requested.get(), "reach the payload open: resumed={resumed}");
        assert_eq!(partial_allocation(&removed), 0);
        let receipt = world.receipt();
        let pending = receipt
            .pending
            .expect("retain the original payload identity");
        assert_eq!(
            super::super::identity(&original).as_ref(),
            Some(&receipt.identity)
        );
        let payload = pending.directory.join("cache.noindex");
        assert_ne!(
            super::super::identity(&payload).as_ref(),
            Some(&receipt.identity)
        );
        assert_eq!(
            std::fs::read(original.join("debug/deps/unit")).unwrap(),
            vec![0x5a; 32 << 10]
        );
        assert_eq!(
            std::fs::read(payload.join("owner-note")).unwrap(),
            b"keep the replacement payload"
        );
        drop(clear);
        assert!(world.reclaim(&Fence::default()).is_empty());
        assert!(world.receipt().pending.is_some());
        assert_eq!(
            std::fs::read(payload.join("owner-note")).unwrap(),
            b"keep the replacement payload"
        );
    }
}

#[test]
fn source_markers_created_at_the_payload_open_revoke_fresh_and_resumed_walks() {
    use super::super::super::remove::seams;
    use std::rc::Rc;
    for resumed in [false, true] {
        for marker in [".git", "Cargo.toml", "Targo.toml"] {
            let world = released_retirement(resumed);
            let requested = Rc::new(Cell::new(false));
            let (seen, path) = (Rc::clone(&requested), world.path());
            let clear = ClearRemovalSeams;
            seams::on(seams::At::Opening, move |name| {
                if name != "cache.noindex" || seen.replace(true) {
                    return;
                }
                let pending = super::super::read(&path).unwrap().pending.unwrap();
                std::fs::write(
                    pending.directory.join("cache.noindex").join(marker),
                    b"source",
                )
                .unwrap();
            });
            let removed = world.reclaim(&Fence::default());
            assert!(requested.get(), "reach {marker}: resumed={resumed}");
            assert!(matches!(
                removed.as_slice(),
                [(_, super::super::super::Unused::Revoked { freed: 0 })]
            ));
            let pending = world
                .receipt()
                .pending
                .expect("keep source work journalled");
            let payload = pending.directory.join("cache.noindex");
            assert_eq!(std::fs::read(payload.join(marker)).unwrap(), b"source");
            assert_eq!(
                std::fs::read(payload.join("debug/deps/unit")).unwrap(),
                vec![0x5a; 32 << 10]
            );
            drop(clear);
            assert!(world.reclaim(&Fence::default()).is_empty());
            assert!(world.receipt().pending.is_some());
            assert_eq!(std::fs::read(payload.join(marker)).unwrap(), b"source");
        }
    }
}

#[test]
fn a_stage_sibling_created_during_payload_open_is_never_walked_or_counted_as_freed() {
    use super::super::super::remove::seams;
    use std::rc::Rc;
    for resumed in [false, true] {
        let world = released_retirement(resumed);
        let before_payload = if resumed {
            world
                .receipt()
                .pending
                .unwrap()
                .directory
                .join("cache.noindex")
        } else {
            world.target.clone()
        };
        let allocations = package_file_allocations(&before_payload);
        let requested = Rc::new(Cell::new(false));
        let (seen, path) = (Rc::clone(&requested), world.path());
        let clear = ClearRemovalSeams;
        seams::on(seams::At::Opening, move |name| {
            if name != "cache.noindex" || seen.replace(true) {
                return;
            }
            let pending = super::super::read(&path).unwrap().pending.unwrap();
            std::fs::write(
                pending.directory.join("owner-note"),
                b"keep this stage sibling",
            )
            .unwrap();
        });
        let removed = world.reclaim(&Fence::default());
        assert!(
            requested.get(),
            "reach the sibling boundary: resumed={resumed}"
        );
        let pending = world
            .receipt()
            .pending
            .expect("the sibling keeps its stage");
        let payload = pending.directory.join("cache.noindex");
        let actually_freed = allocations
            .iter()
            .filter(|(name, _)| !payload.join(name).exists())
            .map(|(_, bytes)| bytes)
            .sum::<u64>();
        assert_eq!(partial_allocation(&removed), actually_freed);
        let note = pending.directory.join("owner-note");
        assert_eq!(std::fs::read(&note).unwrap(), b"keep this stage sibling");
        drop(clear);
        assert!(world.reclaim(&Fence::default()).is_empty());
        assert!(world.receipt().pending.is_some());
        assert_eq!(std::fs::read(note).unwrap(), b"keep this stage sibling");
    }
}

#[test]
fn legacy_package_custody_remains_visible_and_settles_without_retirement_authority() {
    let mut world = World::new();
    world.admit(FIRST, 1).unwrap();
    world.output(32 << 10);
    let mut receipt = world.receipt();
    receipt.schema = 2;
    receipt
        .package
        .as_mut()
        .unwrap()
        .declaration
        .preserve_if_present = None;
    super::super::write(&world.path(), &receipt).unwrap();
    super::super::write(&world.target.join(super::super::MARKER), &receipt).unwrap();
    assert!(
        super::super::roots(&world.home)
            .iter()
            .any(|cache| cache.root == world.target)
    );
    world.settle(FIRST, true).unwrap();
    assert!(world.receipt().package.unwrap().active.is_none());
    assert!(world.admit(SECOND, 1).is_err());
    world.held.take();
    assert!(world.reclaim(&Fence::default()).is_empty());
    assert_eq!(
        compact(&world.home, &world.cache, &Fence::default()).unwrap()["status"].as_str(),
        Some("deferred")
    );
    assert!(world.target.join("debug/deps/unit").exists());
    assert!(
        super::super::roots(&world.home)
            .iter()
            .any(|cache| cache.root == world.target)
    );
}

#[test]
fn independent_preserve_families_share_the_lock_without_superseding_each_other() {
    let mut world = World::new();
    world.admit(FIRST, 1).unwrap();
    world.output(32 << 10);
    world.settle(FIRST, false).unwrap();
    let first_target = world.target.clone();
    let first_path = world.path();
    let first_receipt = world.receipt();
    let first_guard = world.preserve.clone();
    let stage = world.cache.join("retiring.noindex");
    super::super::stage_for_process_test(&world.home, &world.target, &stage);
    std::fs::write(&first_guard, b"").unwrap();
    let parent = world.home.join("other-clone");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&parent)
        .unwrap();
    world.preserve = parent.join("preserve-builds");
    world.target = world.cache.join("other-clone.noindex");
    // The other family's unfinished retirement and preservation request do not veto this
    // family's admission or settlement; a cache-wide compact result is advisory only.
    assert_eq!(
        world.admit(SECOND, 1).unwrap()["budgeted"].as_bool(),
        Some(true)
    );
    world.output(32 << 10);
    world.settle(SECOND, false).unwrap();
    assert!(world.admit(FIRST, 1).is_err());
    assert_eq!(
        super::super::read(&first_path).unwrap().package,
        first_receipt.package
    );
    world.held.take();
    assert_eq!(world.reclaim(&Fence::default()).len(), 1);
    assert!(stage.join("cache.noindex/debug/deps/unit").exists());
    assert!(!world.target.exists());
    world.lock();
    world.target = first_target;
    assert!(world.admit(THIRD, 1).is_err());
}

#[test]
fn unrelated_keep_receipts_allow_package_settlement_and_owned_attempt_recovery() {
    let world = World::new();
    let kept = world.home.join("unrelated-kept.noindex");
    assert!(
        super::super::create_checked(&world.home, &kept, world.manifest.parent().unwrap()).unwrap()
    );
    let precious = kept.join("precious");
    std::fs::write(&precious, b"retained output").unwrap();
    super::super::retain(&world.home, &kept).unwrap();
    let kept_path = receipt_path(&world.home, &kept).unwrap();
    let kept_receipt = std::fs::read(&kept_path).unwrap();
    let kept_marker = std::fs::read(kept.join(super::super::MARKER)).unwrap();
    let decoded: aterm_json::Value = aterm_json::from_slice(&kept_receipt).unwrap();
    assert_eq!(decoded["schema"].as_u64(), Some(4));
    assert_eq!(decoded["retained"].as_bool(), Some(true));
    assert!(decoded["package"].is_null());

    // Package discovery reads the global index before filtering this coordinator.
    // A valid keep for another root must neither block it nor lose any keep bytes.
    assert_eq!(
        world.admit(FIRST, 1 << 20).unwrap()["attempt"].as_str(),
        Some(FIRST)
    );
    world.output(32 << 10);
    assert_eq!(
        world.settle(FIRST, true).unwrap()["attempt"].as_str(),
        Some(FIRST)
    );
    let settled = world.receipt().package.unwrap();
    assert!(settled.active.is_none());
    assert_eq!(settled.last_settled.as_deref(), Some(FIRST));
    let learned = settled.learned_budget;
    assert!(learned.is_some());

    world.admit(SECOND, 1 << 20).unwrap();
    let active = std::fs::read(world.path()).unwrap();
    let admit_next = || {
        admit_on(
            &world.home,
            &Admission {
                cache: &world.cache,
                target: &world.target,
                manifest: &world.manifest,
                preserve_if_present: &world.preserve,
                budget: 1 << 20,
                attempt: THIRD,
                producer_pid: 200,
                producer_group: 201,
            },
            true,
            &world.kernel,
            &Fence::default(),
        )
    };
    // Another attempt cannot inherit either a still-owned producer or held Cargo
    // output. Both refusals retain the original active token and acknowledgement.
    assert!(admit_next().is_err());
    assert_eq!(std::fs::read(world.path()).unwrap(), active);
    world.kernel.retired.set(true);
    let cargo = std::fs::File::open(world.target.join("debug/.cargo-lock")).unwrap();
    let held = crate::lock::Flock::try_lock(cargo).unwrap();
    assert!(admit_next().is_err());
    assert_eq!(std::fs::read(world.path()).unwrap(), active);
    drop(held);

    // The new injected guardian holds the real coordinator lock. Its predecessor's
    // retirement witness recovers SECOND and admits THIRD without a removal.
    assert_eq!(admit_next().unwrap()["attempt"].as_str(), Some(THIRD));
    world.kernel.retired.set(false);
    let recovered = world.receipt().package.unwrap();
    assert_eq!(recovered.last_settled.as_deref(), Some(SECOND));
    assert_eq!(recovered.active.as_ref().unwrap().token, THIRD);
    assert_eq!(recovered.active.as_ref().unwrap().producer.pid, 200);
    assert_eq!(recovered.active.as_ref().unwrap().producer.group, 201);
    assert_eq!(recovered.learned_budget, learned);
    assert_eq!(
        world.settle(THIRD, false).unwrap()["attempt"].as_str(),
        Some(THIRD)
    );
    let finished = world.receipt().package.unwrap();
    assert!(finished.active.is_none());
    assert_eq!(finished.last_settled.as_deref(), Some(THIRD));
    assert_eq!(finished.learned_budget, learned);

    assert_eq!(std::fs::read(&kept_path).unwrap(), kept_receipt);
    assert_eq!(
        std::fs::read(kept.join(super::super::MARKER)).unwrap(),
        kept_marker
    );
    assert_eq!(std::fs::read(precious).unwrap(), b"retained output");
    assert_eq!(
        std::fs::read(world.target.join("debug/deps/unit")).unwrap(),
        vec![0x5a; 32 << 10]
    );
}

#[test]
fn explicit_keep_preserves_package_policy_and_refuses_further_disposable_admission() {
    let mut world = World::new();
    world.admit(FIRST, 1).unwrap();
    world.output(32 << 10);
    world.settle(FIRST, false).unwrap();
    let before = world.receipt();
    assert!(before.package.is_some());
    super::super::retain(&world.home, &world.target).unwrap();
    let kept = world.receipt();
    assert!(kept.retained && kept.schema == 4);
    assert_eq!(kept.package, before.package);
    assert_eq!(kept.owners, before.owners);
    assert!(world.admit(SECOND, 1).is_err());
    world.held.take();
    assert!(world.reclaim(&Fence::default()).is_empty());
    assert!(world.target.join("debug/deps/unit").is_file());
}

#[test]
fn preserve_markers_and_replaced_or_unowned_parents_never_authorize_retirement() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    for form in 0..5 {
        let mut world = World::new();
        world.admit(FIRST, 1).unwrap();
        world.output(32 << 10);
        world.settle(FIRST, false).unwrap();
        let parent = world.preserve.parent().unwrap();
        match form {
            0 => std::fs::write(&world.preserve, b"").unwrap(),
            1 => std::fs::create_dir(&world.preserve).unwrap(),
            2 => symlink("missing", &world.preserve).unwrap(),
            3 => {
                std::fs::rename(parent, world.home.join("old-retention")).unwrap();
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(parent)
                    .unwrap();
            }
            _ => std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o777)).unwrap(),
        }
        world.held.take();
        assert!(world.reclaim(&Fence::default()).is_empty());
        assert!(world.target.join("debug/deps/unit").exists());
    }
    let world = World::new();
    let parent = world.preserve.parent().unwrap();
    let uid = std::fs::symlink_metadata(parent).unwrap().uid();
    assert!(super::super::identity_for(parent, uid).is_some());
    assert!(super::super::identity_for(parent, uid.wrapping_add(1)).is_none());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_proven_running_image_keeps_current_and_pending_package_generations() {
    use aterm_uds::deadline::{KeptUntilRan, ad_hoc_copy, cat_echoes, kill_within};
    use aterm_uds::spawnfd::{Child, Stdio};
    struct Running(Child);
    impl Drop for Running {
        fn drop(&mut self) {
            kill_within(&mut self.0, std::time::Duration::from_secs(60));
        }
    }
    let directory = aterm_tempfile::tempdir().unwrap().keep();
    let mut custody = KeptUntilRan::new(directory);
    let mut world = World::in_home(custody.path().canonicalize().unwrap().join("home"), None);
    world.admit(FIRST, 1).unwrap();
    world.output(32 << 10);
    world.settle(FIRST, false).unwrap();
    let image = world.target.join("debug/deps/cat");
    ad_hoc_copy(Path::new("/bin/cat"), &image);
    let mut child = aterm_uds::spawnfd::command(&image)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input = cat_echoes(&mut child);
    custody.ran();
    let child = Running(child);
    let processes = |dirs: &[PathBuf], _: &[u32]| {
        aterm_uds::holders::scan_path_users_of(&[child.0.id()], dirs, &[])
    };
    world.held.take();
    assert!(world.reclaim(&Fence::new(&processes)).is_empty());
    assert!(image.exists());
    let stage = world.cache.join("retiring.noindex");
    // The child proved execution above, so this identity-preserving rename is now safe.
    super::super::stage_for_process_test(&world.home, &world.target, &stage);
    assert!(world.reclaim(&Fence::new(&processes)).is_empty());
    assert!(stage.join("cache.noindex/debug/deps/cat").exists());
    drop(input);
    drop(child);
    assert_eq!(world.reclaim(&Fence::default()).len(), 1);
    assert!(!stage.exists());
}

#[test]
fn oversized_first_success_learns_once_and_warm_work_reuses_then_retires_whole() {
    let mut world = World::new();
    let admitted = world.admit(FIRST, 16 << 10).unwrap();
    assert_eq!(admitted["budgeted"].as_bool(), Some(true));
    assert_eq!(admitted["created"].as_bool(), Some(true));
    world.output(128 << 10);
    let identity = super::super::identity(&world.target).unwrap();
    let first = footprint(&world.target).unwrap();
    assert_eq!(
        world.settle(FIRST, true).unwrap()["budget_bytes"].as_u64(),
        Some(first * 2)
    );
    world.held.take();
    assert!(world.reclaim(&Fence::default()).is_empty());
    assert_eq!(
        super::super::identity(&world.target),
        Some(identity.clone())
    );
    world.lock();
    assert_eq!(
        world.admit(SECOND, 16 << 10).unwrap()["created"].as_bool(),
        Some(false)
    );
    world.output(512 << 10);
    world.settle(SECOND, true).unwrap();
    assert_eq!(world.receipt().package.unwrap().budget(), first * 2);
    world.held.take();
    assert_eq!(world.reclaim(&Fence::default()).len(), 1);
    assert!(!world.target.exists());
    assert!(world.receipt().pending.is_none());
    world.lock();
    assert_eq!(
        world.admit(THIRD, 16 << 10).unwrap()["created"].as_bool(),
        Some(true)
    );
    // Filesystems may reuse the removed directory's inode. A fresh admission
    // must lose the retired build's output, regardless of its new inode number.
    assert!(!world.target.join("debug/deps/unit").exists());
    assert_eq!(world.receipt().package.unwrap().budget(), first * 2);
}

#[test]
fn a_full_workspace_roster_keeps_proven_package_custody_and_all_retirement_fences() {
    let mut world = World::new();
    for number in 0..super::super::MAX_OWNERS {
        world.workspace(number);
        let attempt = format!("{:032x}", number + 1);
        assert_eq!(
            world.admit(&attempt, 1 << 30).unwrap()["budgeted"].as_bool(),
            Some(true)
        );
        world.output(32 << 10);
        world.settle(&attempt, false).unwrap();
    }
    let identity = super::super::identity(&world.target).unwrap();
    let receipt = world.receipt();
    assert_eq!(receipt.owners.len(), super::super::MAX_OWNERS);
    assert!(!receipt.owners_unknown);
    world.workspace(super::super::MAX_OWNERS);
    let admitted = world.admit(FIRST, 1 << 30);
    assert!(
        admitted.is_ok(),
        "a diagnostic roster exhausted custody: {admitted:?}"
    );
    assert_eq!(admitted.unwrap()["created"].as_bool(), Some(false));
    assert_eq!(
        world.admit(FIRST, 1 << 30).unwrap()["budgeted"].as_bool(),
        Some(true)
    );
    let receipt = world.receipt();
    assert_eq!(receipt.owners.len(), super::super::MAX_OWNERS);
    assert!(!receipt.owners_unknown);
    assert!(
        !receipt
            .owners
            .contains(&world.manifest.parent().unwrap().to_path_buf())
    );
    assert_eq!(
        receipt.package.unwrap().active.unwrap().workspace,
        world.manifest.parent().unwrap()
    );
    assert_eq!(super::super::identity(&world.target), Some(identity));
    world.held.take();
    assert!(world.reclaim(&Fence::default()).is_empty());
    world.lock();
    world.kernel.group_gone.set(false);
    assert!(world.settle(FIRST, false).is_err());
    world.kernel.group_gone.set(true);
    world.settle(FIRST, false).unwrap();
    for owner in world.receipt().owners {
        std::fs::remove_dir_all(owner).unwrap();
    }
    assert!(world.manifest.exists());
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(2 * 24 * 60 * 60);
    assert!(world.reclaim(&Fence::default()).is_empty());
    world.held.take();
    assert!(world.reclaim_at(&Fence::default(), later).is_empty());
    assert!(world.target.join("debug/deps/unit").exists());
    world.kernel.free.set(DISK_FLOOR - 1);
    let held = crate::lock::Flock::try_lock(
        std::fs::File::open(world.target.join("debug/.cargo-lock")).unwrap(),
    )
    .unwrap();
    assert!(world.reclaim(&Fence::default()).is_empty());
    drop(held);
    let unknown = |_: &[PathBuf], _: &[u32]| Err(std::io::Error::other("reader unavailable"));
    assert!(world.reclaim(&Fence::new(&unknown)).is_empty());
    assert!(world.target.join("debug/deps/unit").exists());
    let removed = world.reclaim(&Fence::default());
    assert_eq!(removed.len(), 1);
    assert!(matches!(
        removed[0].1,
        super::super::super::Unused::PackageBudget { low_disk: true, .. }
    ));
    assert!(!world.target.exists());
    assert!(world.manifest.exists());
    assert!(world.receipt().pending.is_none());
}

#[test]
fn legacy_roster_overflow_and_unknown_package_producers_remain_retained() {
    let mut legacy = World::new();
    for number in 0..=super::super::MAX_OWNERS {
        legacy.workspace(number);
        assert!(
            super::super::create_checked(
                &legacy.home,
                &legacy.target,
                legacy.manifest.parent().unwrap(),
            )
            .unwrap()
        );
    }
    assert!(legacy.receipt().package.is_none());
    assert!(legacy.receipt().owners_unknown);
    assert_eq!(legacy.receipt().owners.len(), super::super::MAX_OWNERS);
    legacy.output(32 << 10);
    for number in 0..=super::super::MAX_OWNERS {
        std::fs::remove_dir_all(legacy.home.join(format!("source-{number}"))).unwrap();
    }
    legacy.held.take();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(2 * 24 * 60 * 60);
    assert!(legacy.reclaim_at(&Fence::default(), later).is_empty());
    assert!(legacy.target.join("debug/deps/unit").exists());

    for historical in [false, true] {
        let mut world = World::new();
        world.admit(FIRST, 1).unwrap();
        world.output(32 << 10);
        world.settle(FIRST, false).unwrap();
        if historical {
            let mut receipt = world.receipt();
            receipt.owners_unknown = true;
            super::super::write(&world.path(), &receipt).unwrap();
        } else {
            let env = |name: &str| {
                (name == "CARGO_TARGET_DIR").then(|| world.target.as_os_str().to_owned())
            };
            let machine = super::super::super::Machine {
                home: Some(&world.home),
                root: None,
                cwd: world.manifest.parent(),
                env: &env,
                read: &|_| Ok(None),
                read_manifest: &|_| Ok(None),
                writable: &|_| Some(true),
            };
            assert!(
                super::super::observe_on(
                    &["--unverified".into(), "an-unknown-alias".into()],
                    super::super::super::Tool::Targo,
                    &world.home,
                    &machine,
                )
                .unwrap()
            );
        }
        assert!(world.receipt().owners_unknown);
        let before = std::fs::read(world.path()).unwrap();
        assert!(world.admit(SECOND, 1).is_err());
        assert_eq!(std::fs::read(world.path()).unwrap(), before);
        world.held.take();
        assert!(world.reclaim(&Fence::default()).is_empty());
        assert!(world.target.join("debug/deps/unit").exists());
    }
}

#[test]
fn failed_fresh_cohort_cannot_learn_later_warm_accumulation() {
    let mut world = World::new();
    world.admit(FIRST, 1 << 20).unwrap();
    world.output(32 << 10);
    world.settle(FIRST, false).unwrap();
    world.admit(SECOND, 1 << 20).unwrap();
    world.output(128 << 10);
    assert_eq!(
        world.settle(SECOND, true).unwrap()["learned"].as_bool(),
        Some(false)
    );
    assert!(world.receipt().package.unwrap().learned_budget.is_none());
    world.held.take();
    assert!(world.reclaim(&Fence::default()).is_empty());
}

#[test]
fn coordinator_and_cargo_locks_and_reader_uncertainty_each_keep_an_eligible_root() {
    let mut world = World::new();
    world.admit(FIRST, 1).unwrap();
    world.output(64 << 10);
    world.settle(FIRST, false).unwrap();
    assert!(world.reclaim(&Fence::default()).is_empty());
    world.held.take();
    let cargo = std::fs::File::open(world.target.join("debug/.cargo-lock")).unwrap();
    let held_cargo = crate::lock::Flock::try_lock(cargo).unwrap();
    assert!(world.reclaim(&Fence::default()).is_empty());
    drop(held_cargo);
    let unavailable = |_: &[PathBuf], _: &[u32]| Err(std::io::Error::other("inert unavailable"));
    assert!(world.reclaim(&Fence::new(&unavailable)).is_empty());
    assert!(world.target.join("debug/deps/unit").exists());
    assert_eq!(world.reclaim(&Fence::default()).len(), 1);
}

#[test]
fn disk_floor_can_retire_an_under_budget_generation_but_active_producers_are_kept() {
    let mut world = World::new();
    world.admit(FIRST, 1 << 30).unwrap();
    world.output(16 << 10);
    world.kernel.free.set(DISK_FLOOR - 1);
    world.held.take();
    assert!(world.reclaim(&Fence::default()).is_empty());
    world.lock();
    world.settle(FIRST, false).unwrap();
    world.held.take();
    assert_eq!(world.reclaim(&Fence::default()).len(), 1);
}

#[test]
fn abrupt_guard_release_requires_exact_retirement_and_clear_cargo_before_recovery() {
    let mut world = World::new();
    world.admit(FIRST, 1).unwrap();
    world.output(32 << 10);
    world.held.take();
    assert!(world.reclaim(&Fence::default()).is_empty());
    assert!(world.receipt().package.unwrap().active.is_some());
    world.kernel.retired.set(true);
    let cargo = std::fs::File::open(world.target.join("debug/.cargo-lock")).unwrap();
    let held = crate::lock::Flock::try_lock(cargo).unwrap();
    assert!(world.reclaim(&Fence::default()).is_empty());
    assert!(world.receipt().package.unwrap().active.is_some());
    drop(held);
    assert_eq!(world.reclaim(&Fence::default()).len(), 1);
    let policy = world.receipt().package.unwrap();
    assert!(policy.active.is_none());
    assert!(policy.learned_budget.is_none());
    assert_eq!(policy.last_settled.as_deref(), Some(FIRST));
}

#[test]
fn settlement_needs_the_guardian_caller_and_complete_group_retirement() {
    let world = World::new();
    world.admit(FIRST, 1 << 20).unwrap();
    world.output(16 << 10);
    world.kernel.authority.set(false);
    assert!(world.settle(FIRST, true).is_err());
    world.kernel.authority.set(true);
    world.kernel.group_gone.set(false);
    assert!(world.settle(FIRST, true).is_err());
    assert!(world.receipt().package.unwrap().active.is_some());
    world.kernel.group_gone.set(true);
    world.settle(FIRST, true).unwrap();
    assert!(world.admit(FIRST, 1 << 20).is_err());
    world.admit(SECOND, 1 << 20).unwrap();
    assert!(world.settle(FIRST, true).is_err());
    assert_eq!(
        world.receipt().package.unwrap().active.unwrap().token,
        SECOND
    );
}

#[test]
fn a_replaced_coordinator_never_transfers_disposable_authority() {
    let mut world = World::new();
    world.admit(FIRST, 1).unwrap();
    world.output(32 << 10);
    world.settle(FIRST, false).unwrap();
    world.held.take();
    std::fs::rename(
        world.cache.join(COORDINATOR),
        world.cache.join("original-lock"),
    )
    .unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(world.cache.join(COORDINATOR))
        .unwrap();
    world.lock();
    assert!(world.admit(SECOND, 1).is_err());
    world.held.take();
    assert!(world.reclaim(&Fence::default()).is_empty());
    assert!(world.target.join("debug/deps/unit").exists());
}

#[test]
fn ordinary_existing_targets_are_reusable_and_never_retroactively_disposable() {
    for registered in [false, true] {
        let world = World::new();
        std::fs::create_dir(&world.target).unwrap();
        if registered {
            std::fs::remove_dir(&world.target).unwrap();
            super::super::create_checked(
                &world.home,
                &world.target,
                world.manifest.parent().unwrap(),
            )
            .unwrap();
        }
        world.output(32 << 10);
        assert_eq!(
            world.admit(FIRST, 1).unwrap()["budgeted"].as_bool(),
            Some(false)
        );
        if registered {
            assert!(world.receipt().package.is_none());
        }
        assert!(world.target.join("debug/deps/unit").exists());
    }
}

#[test]
fn pending_payloads_keep_the_coordinator_and_cargo_fences_and_block_a_second_generation() {
    let mut world = World::new();
    world.admit(FIRST, 1).unwrap();
    world.output(32 << 10);
    world.settle(FIRST, false).unwrap();
    let stage = world.cache.join("retiring.noindex");
    super::super::stage_for_process_test(&world.home, &world.target, &stage);
    assert!(world.admit(SECOND, 1).is_err());
    assert!(world.reclaim(&Fence::default()).is_empty());
    world.held.take();
    let held = crate::lock::Flock::try_lock(
        std::fs::File::open(stage.join("cache.noindex/debug/.cargo-lock")).unwrap(),
    )
    .unwrap();
    assert!(world.reclaim(&Fence::default()).is_empty());
    drop(held);
    assert_eq!(world.reclaim(&Fence::default()).len(), 1);
    assert!(!stage.exists());
    assert!(world.receipt().pending.is_none());
}

#[test]
fn authorized_partial_payload_cannot_hide_late_source_or_a_foreign_stage_sibling() {
    for source in [false, true] {
        let mut world = World::new();
        world.admit(FIRST, 1).unwrap();
        world.output(32 << 10);
        world.settle(FIRST, false).unwrap();
        let stage = world.cache.join("retiring.noindex");
        super::super::stage_for_process_test(&world.home, &world.target, &stage);
        let mut receipt = world.receipt();
        receipt.pending.as_mut().unwrap().authorized = true;
        super::super::write(&world.path(), &receipt).unwrap();
        std::fs::remove_file(stage.join("cache.noindex").join(super::super::MARKER)).unwrap();
        let work = if source {
            stage.join("cache.noindex/Cargo.toml")
        } else {
            stage.join("owner-notes")
        };
        std::fs::write(&work, b"unreproducible owner work").unwrap();
        world.held.take();
        assert!(world.reclaim(&Fence::default()).is_empty());
        assert_eq!(std::fs::read(work).unwrap(), b"unreproducible owner work");
    }
}

#[test]
fn exact_token_and_worker_group_bind_subsequent_compiler_publication() {
    let world = World::new();
    world.admit(FIRST, 1 << 20).unwrap();
    let active = world.receipt().package.unwrap().active.unwrap();
    let workspace = world.manifest.parent().unwrap();
    assert!(publication_matches(
        &active,
        workspace,
        Some(FIRST.as_ref()),
        Some(101)
    ));
    assert!(!publication_matches(
        &active,
        workspace,
        Some(SECOND.as_ref()),
        Some(101)
    ));
    assert!(!publication_matches(
        &active,
        workspace,
        Some(FIRST.as_ref()),
        Some(102)
    ));
    assert!(!publication_matches(&active, workspace, None, Some(101)));
}

/// A FOOTPRINT SAYS IT IS STILL GOING: it reads every entry of a target, however many.
#[test]
fn a_footprint_ticks_for_each_entry() {
    const ENTRIES: usize = 5;
    let world = World::new();
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&world.target)
        .unwrap();
    for n in 0..ENTRIES {
        std::fs::write(world.target.join(n.to_string()), b"x").unwrap();
    }
    let before = aterm_uds::progress::ticks_here();
    assert!(footprint(&world.target).is_some());
    assert!(aterm_uds::progress::ticks_here() - before > ENTRIES);
}

#[test]
fn allocated_footprint_counts_directory_blocks_and_deduplicates_hardlinks() {
    let world = World::new();
    // Private whatever the umask, as admission makes a target.
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&world.target)
        .unwrap();
    for n in 0..40 {
        std::fs::create_dir(world.target.join(format!("directory-{n}"))).unwrap();
    }
    std::fs::write(world.target.join("unit"), vec![0x33; 32 << 10]).unwrap();
    std::fs::hard_link(world.target.join("unit"), world.target.join("unit-link")).unwrap();
    let mut expected = std::fs::symlink_metadata(&world.target).unwrap().blocks() * 512;
    for entry in std::fs::read_dir(&world.target).unwrap().flatten() {
        if entry.file_name() != "unit-link" {
            expected += entry.metadata().unwrap().blocks() * 512;
        }
    }
    assert_eq!(footprint(&world.target), Some(expected));
}

#[test]
fn a_completed_stage_removal_recovers_its_receipt_after_a_crash_but_never_a_replacement() {
    for replacement in [false, true] {
        let mut world = World::new();
        world.admit(FIRST, 1).unwrap();
        world.output(32 << 10);
        world.settle(FIRST, false).unwrap();
        let stage = world.cache.join("retiring.noindex");
        super::super::stage_for_process_test(&world.home, &world.target, &stage);
        let mut receipt = world.receipt();
        receipt.pending.as_mut().unwrap().authorized = true;
        super::super::write(&world.path(), &receipt).unwrap();
        assert!(super::super::super::remove::tree(&stage).whole());
        if replacement {
            std::fs::create_dir(&stage).unwrap();
            std::fs::write(stage.join("owner-note"), b"keep").unwrap();
        }
        world.held.take();
        world.reclaim(&Fence::default());
        assert_eq!(world.receipt().pending.is_some(), replacement);
        world.lock();
        assert_eq!(world.admit(SECOND, 1).is_err(), replacement);
        if replacement {
            assert_eq!(std::fs::read(stage.join("owner-note")).unwrap(), b"keep");
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_real_closed_descriptor_mapping_keeps_the_whole_package_root_until_unmapped() {
    use std::os::fd::AsFd as _;
    let mut world = World::new();
    world.admit(FIRST, 1).unwrap();
    world.output(32 << 10);
    world.settle(FIRST, false).unwrap();
    let path = world.target.join("debug/deps/mapped");
    std::fs::write(&path, vec![0x71; aterm_uds::shm::PAGE]).unwrap();
    let file = std::fs::File::open(&path).unwrap();
    // SAFETY: this private read-only file has exactly PAGE bytes; no writer, resize or
    // unlink occurs until the region is dropped after both refusal assertions below.
    let region =
        unsafe { aterm_uds::shm::Region::map_readonly(file.as_fd(), aterm_uds::shm::PAGE) }
            .unwrap();
    drop(file);
    let mut byte = [0];
    region.read_into(0, &mut byte).unwrap();
    assert_eq!(byte, [0x71]);
    world.held.take();
    let processes = |dirs: &[PathBuf], _: &[u32]| {
        let mut scan = aterm_uds::holders::scan_path_users_of(&[std::process::id()], dirs, &[])?;
        scan.users.retain(|user| user.how != "fd");
        Ok(scan)
    };
    assert!(world.reclaim(&Fence::new(&processes)).is_empty());
    assert!(world.target.exists());
    assert!(world.receipt().pending.is_none());
    drop(region);
    assert_eq!(world.reclaim(&Fence::new(&processes)).len(), 1);
}

#[test]
fn unreadable_or_inconsistent_receipts_and_source_markers_preserve_the_cache() {
    for state in 0..3 {
        let mut world = World::new();
        world.admit(FIRST, 1).unwrap();
        world.output(32 << 10);
        world.settle(FIRST, false).unwrap();
        match state {
            0 => {
                std::fs::write(world.path(), b"{not-json").unwrap();
            }
            1 => {
                let mut receipt = world.receipt();
                receipt.schema = 1;
                super::super::write(&world.path(), &receipt).unwrap();
            }
            _ => {
                std::fs::write(world.target.join("Cargo.toml"), b"[workspace]\n").unwrap();
            }
        }
        assert!(world.admit(SECOND, 1).is_err());
        world.held.take();
        assert!(world.reclaim(&Fence::default()).is_empty());
        assert!(world.target.join("debug/deps/unit").exists());
    }
}

#[test]
fn a_late_coordinator_replacement_after_candidate_recording_retains_the_original_root() {
    let mut world = World::new();
    world.admit(FIRST, 1).unwrap();
    world.output(32 << 10);
    world.settle(FIRST, false).unwrap();
    world.held.take();
    let identity = super::super::identity(&world.target);
    let mut called = false;
    let removed = super::super::reclaim_selected_on(
        &world.home,
        std::time::SystemTime::now(),
        super::super::super::ReclaimAfter::CALM,
        None,
        &mut |_, _| {
            called = true;
            std::fs::rename(
                world.cache.join(COORDINATOR),
                world.cache.join("former-lock"),
            )
            .unwrap();
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(world.cache.join(COORDINATOR))
                .unwrap();
            true
        },
        &Fence::default(),
        super::super::Selection::Package(Context {
            cache: Some(&world.cache),
            kernel: &world.kernel,
        }),
    );
    assert!(called);
    assert_eq!(
        removed,
        [(
            world.target.clone(),
            super::super::super::Unused::Refused {
                by: super::super::super::Refusal::Stopped
            }
        )],
        "its intent closed, nothing of it gone"
    );
    assert_eq!(super::super::identity(&world.target), identity);
    assert!(world.target.join("debug/deps/unit").exists());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_unrelated_live_guardian_cannot_authorize_the_settlement_caller() {
    use aterm_uds::deadline::{cat_echoes, kill_within, stand_in};
    use aterm_uds::spawnfd::{Child, Stdio};
    struct Running(Child);
    impl Drop for Running {
        fn drop(&mut self) {
            kill_within(&mut self.0, std::time::Duration::from_secs(60));
        }
    }
    let world = World::new();
    world.admit(FIRST, 1 << 20).unwrap();
    world.output(32 << 10);
    let mut child = aterm_uds::spawnfd::command(stand_in(Path::new("/bin/cat")))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input = cat_echoes(&mut child);
    let guardian = Running(child);
    // A separate real worker group has completely retired while the unrelated guardian
    // remains alive. Lock ownership and a known nonce alone must not settle its receipt.
    let mut worker = aterm_uds::spawnfd::command(stand_in(Path::new("/usr/bin/true")));
    worker.setsid(true);
    let mut worker = worker.spawn().unwrap();
    let group = worker.id();
    assert!(worker.wait().unwrap().success());
    let identity =
        aterm_uds::process::read_identity(i32::try_from(guardian.0.id()).unwrap()).unwrap();
    let producer = Producer {
        pid: guardian.0.id(),
        birth: identity.birth.0,
        group,
        boot: aterm_uds::process::boot_id().unwrap(),
        namespace: aterm_uds::process::pid_namespace(),
    };
    assert!(Live.guardian_live(&producer));
    assert!(Live.group_gone(&producer));
    assert!(!Live.settlement_authority(&producer));
    let mut receipt = world.receipt();
    receipt
        .package
        .as_mut()
        .unwrap()
        .active
        .as_mut()
        .unwrap()
        .producer = producer;
    super::super::write(&world.path(), &receipt).unwrap();
    assert!(
        settled_on(
            &world.home,
            &world.cache,
            FIRST,
            true,
            &Live,
            &Fence::default()
        )
        .is_err()
    );
    assert!(world.receipt().package.unwrap().active.is_some());
    drop(input);
    drop(guardian);
}

#[test]
fn changing_compiler_keys_requests_whole_retirement_before_a_second_target_can_exist() {
    let mut world = World::new();
    world.admit(FIRST, 1 << 20).unwrap();
    world.output(32 << 10);
    world.settle(FIRST, true).unwrap();
    let old_root = world.target.clone();
    let old_path = world.path();
    let learned = world.receipt().package.unwrap().learned_budget;
    world.target = world.cache.join("new-compiler.noindex");
    assert!(world.admit(SECOND, 1 << 20).is_err());
    assert!(!world.target.exists());
    let old = super::super::read(&old_path).unwrap();
    assert!(old.package.as_ref().unwrap().superseded);
    assert!(old.pending.is_none());
    world.held.take();
    let held = crate::lock::Flock::try_lock(
        std::fs::File::open(old_root.join("debug/.cargo-lock")).unwrap(),
    )
    .unwrap();
    assert!(world.reclaim(&Fence::default()).is_empty());
    world.lock();
    assert!(world.admit(SECOND, 1 << 20).is_err());
    assert!(!world.target.exists());
    world.held.take();
    drop(held);
    let unavailable = |_: &[PathBuf], _: &[u32]| Err(std::io::Error::other("reader unknown"));
    assert!(world.reclaim(&Fence::new(&unavailable)).is_empty());
    assert!(old_root.exists());
    assert_eq!(world.reclaim(&Fence::default()).len(), 1);
    assert!(!old_root.exists());
    assert_eq!(
        compact(&world.home, &world.cache, &Fence::default()).unwrap()["status"].as_str(),
        Some("ready")
    );
    world.lock();
    assert_eq!(
        world.admit(SECOND, 1 << 20).unwrap()["created"].as_bool(),
        Some(true)
    );
    world.output(16 << 10);
    world.settle(SECOND, true).unwrap();
    assert_eq!(
        super::super::read(&old_path)
            .unwrap()
            .package
            .unwrap()
            .learned_budget,
        learned
    );
    assert!(world.target.exists());
}

#[test]
fn unstarted_and_partly_removed_retirements_resume_without_reauthorizing_a_replacement() {
    for phase in 0..5 {
        let mut world = World::new();
        world.admit(FIRST, 1).unwrap();
        world.output(32 << 10);
        world.settle(FIRST, false).unwrap();
        let stage = world.cache.join("retiring.noindex");
        super::super::stage_for_process_test(&world.home, &world.target, &stage);
        let payload = stage.join("cache.noindex");
        let mut receipt = world.receipt();
        if phase < 2 {
            std::fs::rename(&payload, &world.target).unwrap();
            if phase == 1 {
                std::fs::remove_dir(&stage).unwrap();
            }
        } else {
            receipt.pending.as_mut().unwrap().authorized = true;
            super::super::write(&world.path(), &receipt).unwrap();
            match phase {
                2 => std::fs::remove_file(payload.join(super::super::MARKER)).unwrap(),
                3 => assert!(super::super::super::remove::tree(&payload).whole()),
                _ => {
                    std::fs::rename(&payload, world.cache.join("preserved-original.noindex"))
                        .unwrap();
                    std::fs::create_dir(&payload).unwrap();
                    std::fs::write(payload.join("owner-note"), b"keep").unwrap();
                }
            }
        }
        world.held.take();
        world.reclaim(&Fence::default());
        assert_eq!(world.receipt().pending.is_some(), phase == 4);
        if phase < 2 {
            assert!(world.target.join("debug/deps/unit").exists());
            assert_eq!(world.reclaim(&Fence::default()).len(), 1);
        } else if phase == 4 {
            assert_eq!(std::fs::read(payload.join("owner-note")).unwrap(), b"keep");
        } else {
            assert!(!stage.exists());
        }
    }
}

#[test]
fn actual_registrar_publications_require_the_recorded_token_and_kernel_worker_group() {
    const CASE: &str = "ATERM_TEST_PACKAGE_REGISTRAR_CASE";
    const ROOT: &str = "ATERM_TEST_PACKAGE_REGISTRAR_HOME";
    if let Some(case) = std::env::var_os(CASE) {
        let home = PathBuf::from(std::env::var_os(ROOT).unwrap());
        let observed = super::super::create_checked(
            &home,
            &home.join("cache.noindex/release.noindex"),
            &home.join(if case == "wrong-workspace" {
                "source-0"
            } else {
                "source"
            }),
        );
        if case == "allow" {
            assert!(observed.unwrap());
        } else {
            assert!(observed.is_err(), "the registrar accepted {case:?}");
        }
        return;
    }
    let mut world = World::new();
    let manifest = world.manifest.clone();
    for number in 0..super::super::MAX_OWNERS {
        world.workspace(number);
        let attempt = format!("{:032x}", number + 1);
        world.admit(&attempt, 1 << 20).unwrap();
        world.settle(&attempt, false).unwrap();
    }
    world.manifest = manifest;
    let pid = std::process::id();
    let group = aterm_uds::process::read_lineage(i32::try_from(pid).unwrap())
        .unwrap()
        .pgid;
    admit_on(
        &world.home,
        &Admission {
            cache: &world.cache,
            target: &world.target,
            manifest: &world.manifest,
            preserve_if_present: &world.preserve,
            budget: 1 << 20,
            attempt: FIRST,
            producer_pid: pid,
            producer_group: group,
        },
        true,
        &Live,
        &Fence::default(),
    )
    .unwrap();
    assert_eq!(world.receipt().owners.len(), super::super::MAX_OWNERS);
    assert!(
        !world
            .receipt()
            .owners
            .contains(&world.manifest.parent().unwrap().to_path_buf())
    );
    assert!(!world.receipt().owners_unknown);
    let executable = std::env::current_exe().unwrap();
    for case in [
        "allow",
        "wrong-token",
        "wrong-group",
        "wrong-workspace",
        "unknown-owner",
    ] {
        let before = if case == "unknown-owner" {
            let mut receipt = world.receipt();
            receipt.owners_unknown = true;
            super::super::write(&world.path(), &receipt).unwrap();
            Some(std::fs::read(world.path()).unwrap())
        } else {
            None
        };
        let mut command = aterm_uds::spawnfd::command(&executable);
        command.args(["--exact", "build_cache::registered::package::tests::actual_registrar_publications_require_the_recorded_token_and_kernel_worker_group", "--nocapture"])
            .env(CASE, case).env(ROOT, &world.home)
            .env(ATTEMPT_ENV, if case == "wrong-token" { SECOND } else { FIRST });
        command.setsid(case == "wrong-group");
        // output_within deliberately starts a new process group. Preserve the normal
        // registrar's inherited group here and bound its wait with the kernel exit watch.
        let log = world.home.join(format!("{case}.log"));
        let output = std::fs::File::create(&log).unwrap();
        let mut child = command
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(output.try_clone().unwrap())
            .stderr(output)
            .spawn()
            .unwrap();
        let by = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut exit = aterm_uds::deadline::ExitWait::new(&child);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            let left = by.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                aterm_uds::deadline::kill_within(&mut child, std::time::Duration::from_secs(60));
                panic!("{case} registrar child did not finish");
            }
            exit.wait(left);
        };
        assert!(
            status.success(),
            "{case}: {}",
            std::fs::read_to_string(log).unwrap()
        );
        if let Some(before) = before {
            assert_eq!(std::fs::read(world.path()).unwrap(), before);
        }
    }
    assert!(world.receipt().package.unwrap().active.is_some());
}

#[test]
fn compact_reports_a_replaced_existing_root_as_deferred() {
    let mut world = World::new();
    world.admit(FIRST, 1 << 20).unwrap();
    world.output(16 << 10);
    world.settle(FIRST, false).unwrap();
    world.held.take();
    std::fs::rename(
        &world.target,
        world.cache.join("preserved-original.noindex"),
    )
    .unwrap();
    std::fs::create_dir(&world.target).unwrap();
    let work = world.target.join("owner-note");
    std::fs::write(&work, b"keep").unwrap();
    assert_eq!(
        compact(&world.home, &world.cache, &Fence::default()).unwrap()["status"].as_str(),
        Some("deferred")
    );
    assert_eq!(std::fs::read(work).unwrap(), b"keep");
}

/// A GENERATION THE PASS KEPT IS NOT RETIRED: `compact` over a settled disposable generation
/// whose retirement intent closes with nothing of it gone (its cache folder made unwritable after
/// settlement, so the pass cannot make its private stage: `Unused::Refused`) answers `deferred`
/// with `removed` 0, the generation whole; the same generation with its folder writable again
/// answers `retired` with `removed` 1, and goes. NEGATIVE CONTROL: `compact` counting every entry
/// the pass returned answers `retired` for the kept one.
#[test]
fn compact_counts_a_kept_generation_as_neither_retired_nor_removed() {
    use std::os::unix::fs::PermissionsExt as _;
    /// Makes the folder writable again when dropped, so a failed test leaves its scratch
    /// removable.
    struct Writable(PathBuf);
    impl Drop for Writable {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let mut world = World::new();
    world.admit(FIRST, 1).unwrap();
    world.output(32 << 10);
    world.settle(FIRST, false).unwrap();
    world.held.take();
    let answer = |world: &World| {
        let answer = compact(&world.home, &world.cache, &Fence::default()).unwrap();
        (
            answer["status"].as_str().map(str::to_owned),
            answer["removed"].as_u64(),
        )
    };
    if crate::platform::our_uid() != 0 {
        let writable = Writable(world.cache.clone());
        std::fs::set_permissions(&world.cache, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(
            answer(&world),
            (Some("deferred".to_owned()), Some(0)),
            "a kept generation is not retired"
        );
        drop(writable);
        assert!(
            world.target.join("debug/deps/unit").is_file(),
            "it stays whole"
        );
    }
    assert_eq!(
        answer(&world),
        (Some("retired".to_owned()), Some(1)),
        "the next pass retires it"
    );
    assert!(!world.target.exists());
}

/// A PACKAGE DECLARATION RECORDED IN AN EARLIER BOOT BINDS BY ITS VOLUME'S UUID, AND ANOTHER
/// VOLUME'S STAYS REFUSED: `cache_identity`, `lock_identity` and
/// `preserve_if_present.parent_identity` record the volume's UUID beside the device number; when a
/// restart renumbers the volume (the same UUIDs and inodes, another device number), a subsequent
/// attempt on the warm generation is admitted and settles, and `compact` coordinates and retires
/// it. NEGATIVE CONTROL: when any of the three declaration identities carries another volume's
/// UUID (though its device number and inode match this boot's), admission and coordination refuse
/// and the generation stays.
#[cfg(target_os = "macos")]
#[test]
fn a_package_declaration_from_an_earlier_boot_binds_by_volume_uuid_and_another_volumes_stays() {
    let rewrite = |world: &World, mutate: &dyn Fn(&mut Receipt)| {
        let mut receipt = world.receipt();
        mutate(&mut receipt);
        super::super::write(&world.path(), &receipt).unwrap();
        let held = super::super::creation::pin(&world.target).unwrap();
        let mut marker = super::super::retention::receipt_at(&held).unwrap();
        mutate(&mut marker);
        super::super::retention::write_marker(&held, &marker).unwrap();
    };
    let mut world = World::new();
    world.admit(FIRST, 1).unwrap();
    world.output(32 << 10);
    world.settle(FIRST, false).unwrap();
    let initial = world.receipt();
    let decl = &initial.package.as_ref().unwrap().declaration;
    assert!(decl.cache_identity.volume.is_some());
    assert!(decl.lock_identity.volume.is_some());
    assert!(
        decl.preserve_if_present
            .as_ref()
            .unwrap()
            .parent_identity
            .volume
            .is_some()
    );
    rewrite(&world, &|receipt| {
        receipt.identity.device = receipt.identity.device.wrapping_add(1);
        let decl = &mut receipt.package.as_mut().unwrap().declaration;
        decl.cache_identity.device = decl.cache_identity.device.wrapping_add(1);
        decl.lock_identity.device = decl.lock_identity.device.wrapping_add(1);
        let guard = decl.preserve_if_present.as_mut().unwrap();
        guard.parent_identity.device = guard.parent_identity.device.wrapping_add(1);
    });
    let re_admitted = world.admit(SECOND, 1).unwrap();
    assert_eq!(re_admitted["budgeted"].as_bool(), Some(true));
    assert_eq!(re_admitted["created"].as_bool(), Some(false));
    world.settle(SECOND, false).unwrap();
    world.held.take();
    assert!(coordinate(&world.home, &world.receipt()).is_ok());
    let compacted = compact(&world.home, &world.cache, &Fence::default()).unwrap();
    assert_eq!(compacted["status"].as_str(), Some("retired"));
    assert_eq!(compacted["removed"].as_u64(), Some(1));
    assert!(!world.target.exists());

    for which in ["cache", "lock", "preserve"] {
        let mut world = World::new();
        world.admit(FIRST, 1).unwrap();
        world.output(32 << 10);
        world.settle(FIRST, false).unwrap();
        rewrite(&world, &|receipt| {
            let decl = &mut receipt.package.as_mut().unwrap().declaration;
            let id = match which {
                "cache" => &mut decl.cache_identity,
                "lock" => &mut decl.lock_identity,
                "preserve" => &mut decl.preserve_if_present.as_mut().unwrap().parent_identity,
                _ => unreachable!(),
            };
            id.volume.as_mut().unwrap()[0] ^= 0xff;
        });
        assert!(world.admit(SECOND, 1).is_err(), "{which}");
        world.held.take();
        assert!(
            coordinate(&world.home, &world.receipt()).is_err(),
            "{which}"
        );
        assert!(world.target.join("debug/deps/unit").is_file(), "{which}");
    }
}
