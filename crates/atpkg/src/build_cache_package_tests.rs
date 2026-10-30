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

    fn admit(&self, attempt: &str, budget: u64) -> Result<aterm_json::Value, String> {
        admit_on(
            &self.home,
            &Admission {
                cache: &self.cache,
                target: &self.target,
                manifest: &self.manifest,
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
        super::super::reclaim_selected_on(
            &self.home,
            std::time::SystemTime::now(),
            super::super::super::RECLAIM_GONE,
            None,
            &mut |_, _| true,
            fence,
            Some(Context {
                cache: Some(&self.cache),
                kernel: &self.kernel,
            }),
        )
    }
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
    assert_ne!(super::super::identity(&world.target).unwrap(), identity);
    assert_eq!(world.receipt().package.unwrap().budget(), first * 2);
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

#[test]
fn allocated_footprint_counts_directory_blocks_and_deduplicates_hardlinks() {
    let world = World::new();
    std::fs::create_dir(&world.target).unwrap();
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
        super::super::super::RECLAIM_GONE,
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
        Some(Context {
            cache: Some(&world.cache),
            kernel: &world.kernel,
        }),
    );
    assert!(called);
    assert!(removed.is_empty());
    assert_eq!(super::super::identity(&world.target), identity);
    assert!(world.target.join("debug/deps/unit").exists());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn an_unrelated_live_guardian_cannot_authorize_the_settlement_caller() {
    use aterm_uds::deadline::{cat_echoes, kill_within};
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
    let mut child = aterm_uds::spawnfd::command("/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input = cat_echoes(&mut child);
    let guardian = Running(child);
    // A separate real worker group has completely retired while the unrelated guardian
    // remains alive. Lock ownership and a known nonce alone must not settle its receipt.
    let mut worker = aterm_uds::spawnfd::command("/usr/bin/true");
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
            &home.join("source"),
        );
        if case == "allow" {
            assert!(observed.unwrap());
        } else {
            assert!(observed.is_err(), "the registrar accepted {case:?}");
        }
        return;
    }
    let world = World::new();
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
    let executable = std::env::current_exe().unwrap();
    for case in ["allow", "wrong-token", "wrong-group"] {
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
