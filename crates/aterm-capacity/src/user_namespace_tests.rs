// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
struct World {
    fixture: crate::ledger::CohortFixture,
    legacy: PathBuf,
}
impl World {
    fn new() -> Self {
        let fixture = crate::ledger::CohortFixture::new();
        let legacy = fixture
            .namespace
            .fence()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("account-memory");
        std::fs::create_dir(&legacy).unwrap();
        std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { fixture, legacy }
    }
    fn adopt(&self) -> io::Result<()> {
        // Only this private fixture simulates a previously retired boot.
        // Production callers have no boot-identity injection surface.
        if !paths(&self.legacy)?.1.exists() && !paths(&self.legacy)?.0.exists() {
            let error = adopt_in_boot(&self.legacy, &self.fixture.namespace, "fixture-prior-boot")
                .unwrap_err();
            if error.kind() != io::ErrorKind::WouldBlock {
                return Err(error);
            }
        }
        adopt(&self.legacy, &self.fixture.namespace)
    }
}
impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.legacy, std::fs::Permissions::from_mode(0o700));
    }
}
#[test]
fn adoption_preserves_cpu_child_and_requires_original_selector_and_denial() {
    let world = World::new();
    let pool =
        crate::build_cpu::Pool::open_in(&world.legacy, std::num::NonZeroU32::new(2).unwrap())
            .unwrap();
    assert_eq!(resolve(&world.legacy).unwrap(), None);
    world.adopt().unwrap();
    assert_eq!(
        resolve(&world.legacy).unwrap(),
        Some(world.fixture.namespace.clone())
    );
    assert!(
        pool.try_acquire(crate::build_cpu::Owner::current(None))
            .unwrap()
            .is_some()
    );
    world.adopt().unwrap();
    std::fs::remove_file(paths(&world.legacy).unwrap().0).unwrap();
    assert!(resolve(&world.legacy).is_err());
}
/// THE ADMISSION ROW'S LEDGER (Continuum TM-1): once this account's ledger was adopted into a
/// memory namespace, its claims are decided in the namespace's backend, so an admission row is
/// held to the account ledger's own folder ([`crate::Ledger::user`]'s `dir`) and written.
/// NEGATIVE CONTROL: held to the capacity folder it was adopted from, as TM-0 held it, none is.
#[test]
fn an_adopted_accounts_admissions_are_held_to_its_namespaced_ledger() {
    let world = World::new();
    world.adopt().unwrap();
    let ledger = crate::Ledger::account_at(world.legacy.clone());
    assert_eq!(ledger.dir(), world.fixture.namespace.backend());
    let asked = crate::telemetry::Asked::of(&crate::Need::new("targo test".to_owned()));
    let refused: Result<crate::Claim, crate::Error> = Err(crate::Error::Never("too big".into()));
    let mut n = 0;
    crate::telemetry::decided_from(
        &ledger,
        world.legacy.clone(),
        "m101",
        aterm_telemetry::Digest::dir("/facts"),
        &asked,
        &refused,
        |_| {
            n += 1;
            true
        },
    );
    assert_eq!(
        n, 1,
        "the account ledger's own folder, as `decided` reads it"
    );
    let mut held_to_source = 0;
    crate::telemetry::decided_in(
        &ledger,
        &world.legacy,
        "m101",
        aterm_telemetry::Digest::dir("/facts"),
        &asked,
        &refused,
        |_| {
            held_to_source += 1;
            true
        },
    );
    assert_eq!(held_to_source, 0, "the folder it was adopted from");
}

#[test]
fn account_reader_reopens_retained_backend_and_rechecks_source_every_time() {
    let world = World::new();
    let stale = crate::Ledger::account_at(world.legacy.clone());
    assert!(!stale.cohort_aware().unwrap());
    world.adopt().unwrap();
    assert!(stale.list().is_err());
    let current = crate::Ledger::account_at(world.legacy.clone());
    assert!(current.cohort_aware().unwrap());
    assert_eq!(current.dir(), world.fixture.namespace.backend());
    std::fs::remove_file(paths(&world.legacy).unwrap().0).unwrap();
    assert!(current.list().is_err());
    assert!(
        crate::Ledger::account_at(world.legacy.clone())
            .list()
            .is_err()
    );
}

#[test]
fn legacy_claim_is_not_recovered_from_its_closed_descriptor() {
    let world = World::new();
    let claim = world.legacy.join("1-1-1.claim");
    std::fs::write(&claim, b"old final-close reservation").unwrap();
    std::fs::set_permissions(&claim, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        adopt(&world.legacy, &world.fixture.namespace)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(resolve(&world.legacy).is_err());
    assert_eq!(
        adopt(&world.legacy, &world.fixture.namespace)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(
        std::fs::read(&claim).unwrap(),
        b"old final-close reservation"
    );
    assert!(paths(&world.legacy).unwrap().1.exists());
}
#[test]
fn active_turn_blocks_commit_and_exact_prepared_intent_can_resume() {
    let world = World::new();
    let lock = world.legacy.join("ledger.lock");
    std::fs::write(&lock, b"").unwrap();
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o600)).unwrap();
    let held = aterm_uds::file_lock::LockFile::take_existing(
        &lock,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
        "test fixture lock is busy",
    )
    .unwrap();
    assert!(world.adopt().is_err());
    assert!(resolve(&world.legacy).is_err());
    drop(held);
    world.adopt().unwrap();
    assert!(resolve(&world.legacy).unwrap().is_some());
    std::fs::set_permissions(&world.legacy, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(resolve(&world.legacy).is_err());
}
#[cfg(target_os = "macos")]
#[test]
fn an_explicit_creation_acl_prevents_readiness_even_with_mode_0500() {
    let world = World::new();
    // SAFETY: geteuid reads only this fixture process's effective identity.
    let uid = unsafe { libc::geteuid() };
    let user = aterm_uds::account::user_name(uid).unwrap();
    let status = aterm_uds::spawnfd::command("/bin/chmod")
        .args(["+a", &format!("user:{user} allow add_file")])
        .arg(&world.legacy)
        .status()
        .unwrap();
    assert!(status.success());
    let result = world.adopt();
    let status = aterm_uds::spawnfd::command("/bin/chmod")
        .arg("-N")
        .arg(&world.legacy)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(result.is_err());
    assert!(paths(&world.legacy).unwrap().1.exists());
    assert!(!paths(&world.legacy).unwrap().0.exists());
}
#[test]
fn empty_legacy_records_do_not_prove_same_boot_execution_drain() {
    let world = World::new();
    assert_eq!(
        adopt(&world.legacy, &world.fixture.namespace)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(resolve(&world.legacy).is_err());
    assert!(!paths(&world.legacy).unwrap().0.exists());
}

#[test]
fn a_prebarrier_intent_from_an_earlier_boot_cannot_authorize_readiness() {
    let world = World::new();
    let parent = directory(&world.legacy).unwrap();
    prepare_intent(
        &world.legacy,
        &world.fixture.namespace,
        "fixture-prior-boot",
        private(&parent, true).unwrap(),
    )
    .unwrap();
    assert_eq!(
        adopt(&world.legacy, &world.fixture.namespace)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    let (_, pending) = paths(&world.legacy).unwrap();
    let (_, _, barrier_boot, fenced) = decode(&record(&pending).unwrap()).unwrap();
    assert!(fenced);
    assert_eq!(barrier_boot, aterm_uds::process::boot_id().unwrap());
    assert!(!paths(&world.legacy).unwrap().0.exists());
}

/// A selector parent of this user's that its group or others may write (Linux's
/// user-private-group umask, 002, leaves aterm's cache folder 0775) is narrowed through the
/// descriptor checked, dropping exactly those write bits, not refused: an account never adopted
/// reads as having no selector, and an adopted one as its namespace. (Here the parent is also the
/// cohort's private home, which adoption holds to 0700, so the adopted case narrows 0720.)
/// NEGATIVE CONTROL (by hand): `outer` refusing such a parent again, the first `resolve` here
/// fails, as every capacity claim of the account did (`verify`'s on Linux, 2026-11-06).
#[test]
fn a_selector_parent_its_group_may_write_is_narrowed_not_refused() {
    let world = World::new();
    let parent = world.legacy.parent().unwrap().to_path_buf();
    let mode = |path: &Path| std::fs::metadata(path).unwrap().mode() & 0o7777;
    let set = |bits: u32| {
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(bits)).unwrap();
    };
    set(0o775);
    assert_eq!(resolve(&world.legacy).unwrap(), None);
    assert_eq!(mode(&parent), 0o755);
    set(0o700);
    let pool =
        crate::build_cpu::Pool::open_in(&world.legacy, std::num::NonZeroU32::new(2).unwrap())
            .unwrap();
    world.adopt().unwrap();
    set(0o720);
    assert_eq!(
        resolve(&world.legacy).unwrap(),
        Some(world.fixture.namespace.clone())
    );
    assert_eq!(mode(&parent), 0o700);
    drop(pool);
}
