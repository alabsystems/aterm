// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use crate::build_cpu::{Owner, Pool};

use super::*;

const HANG: Duration = Duration::from_secs(60);

// A sibling test can fork while one of our descriptors is open. The
// inherited lock lasts until that child execs or closes it; observe the
// required state after that copy closes, rather than assuming one sample.
fn eventually<T>(what: &str, mut step: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + HANG;
    loop {
        if let Some(value) = step() {
            return value;
        }
        assert!(Instant::now() < deadline, "{what} within the hang detector");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn pool(dir: &Path, slots: u32) -> Pool {
    eventually(
        "the pool opener's inherited initialization lock to close",
        || match Pool::open_in(dir, NonZeroU32::new(slots).unwrap()) {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
            result => Some(result.unwrap()),
        },
    )
}

#[test]
fn an_absent_pool_is_reported_without_creating_files() {
    let dir = aterm_tempfile::Builder::new()
        .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
        .unwrap();
    let missing = dir.path().join("missing");
    assert_eq!(CpuObservation::read(Some(&missing)), CpuObservation::Absent);
    assert!(!missing.exists());
    assert_eq!(
        CpuObservation::read(Some(dir.path())),
        CpuObservation::Absent
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    assert!(matches!(
        CpuObservation::read(None),
        CpuObservation::Unreadable(_)
    ));
}

#[test]
fn held_slots_follow_kernel_locks_and_not_stale_owner_records() {
    let dir = aterm_tempfile::Builder::new()
        .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
        .unwrap();
    let pool = pool(dir.path(), 2);
    let owner = Owner::current(Some(7));
    let lease = eventually("a lease after the opener's last copy closes", || match pool
        .try_acquire(owner)
    {
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
        result => result.unwrap(),
    });
    let expected = CpuObservation::Ready {
        total: 2,
        external: None,
        holders: vec![CpuHolder {
            slot: lease.slot(),
            reported_pid: Some(owner.pid),
            reported_birth: Some(7),
        }],
    };
    let observed = eventually("only the deliberately held lease remains", || {
        let observed = CpuObservation::read(Some(dir.path()));
        (observed == expected).then_some(observed)
    });
    assert_eq!(observed, expected);
    drop(lease);
    let released = eventually("the released lease's last copy to close", || {
        let observed = CpuObservation::read(Some(dir.path()));
        matches!(&observed, CpuObservation::Ready { total: 2, holders, .. } if holders.is_empty())
            .then_some(observed)
    });
    assert_eq!(
        released,
        CpuObservation::Ready {
            total: 2,
            external: None,
            holders: vec![]
        }
    );
}

#[test]
fn an_initializer_is_reported_without_waiting_for_its_lock() {
    let dir = aterm_tempfile::Builder::new()
        .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
        .unwrap();
    let pool = pool(dir.path(), 1);
    let _initializer = eventually("the initializer's last inherited copy to close", || {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(pool.path().join("init.lock"))
            .unwrap();
        match aterm_toolchain::record_lock::RecordLock::try_lock(file, 0) {
            Err(std::fs::TryLockError::WouldBlock) => None,
            result => Some(result.unwrap()),
        }
    });
    let path = dir.path().to_owned();
    let (send, receive) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || send.send(CpuObservation::read(Some(&path))).unwrap());
    assert_eq!(
        receive
            .recv_timeout(HANG)
            .expect("pool observation must not wait on an initializer"),
        CpuObservation::Initializing,
    );
    reader.join().unwrap();
}

#[test]
fn a_corrupt_pool_is_unreadable_and_is_not_repaired() {
    let dir = aterm_tempfile::Builder::new()
        .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
        .unwrap();
    let pool = pool(dir.path(), 1);
    let manifest = pool.path().join("pool");
    std::fs::write(&manifest, "invalid").unwrap();
    let observed = eventually("the corrupt pool is no longer initializing", || {
        let observed = CpuObservation::read(Some(dir.path()));
        (!matches!(observed, CpuObservation::Initializing)).then_some(observed)
    });
    assert!(matches!(observed, CpuObservation::Unreadable(_)));
    assert_eq!(std::fs::read_to_string(manifest).unwrap(), "invalid");
}

struct RetiredDirectory(std::path::PathBuf);
impl Drop for RetiredDirectory {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
    }
}
fn retirement(dir: &Path) -> Option<(RetiredDirectory, crate::build_cpu::LegacyRetirement)> {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join("legacy");
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let guard = RetiredDirectory(path.clone());
    let result = crate::build_cpu::LegacyRetirement::retire(&path, 1);
    // SAFETY: geteuid reads only this process's effective identity.
    if unsafe { libc::geteuid() } == 0 {
        assert!(result.is_err());
        return None;
    }
    Some((guard, result.unwrap()))
}

#[test]
fn retained_external_units_are_not_reported_free_after_owner_close() {
    use crate::build_cpu::ExternalDebt;
    let dir = aterm_tempfile::Builder::new()
        .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
        .unwrap();
    let Some((_restore, retired)) = retirement(dir.path()) else {
        return;
    };
    let pool = pool(dir.path(), 4);
    let debt = ExternalDebt::activate(&pool, &retired).unwrap();
    let worker = debt
        .try_acquire_native(Owner::current(Some(7)), 4)
        .unwrap()
        .unwrap();
    let mut external = debt
        .try_reserve(Owner::current(Some(8)), NonZeroU32::new(2).unwrap(), 4)
        .unwrap()
        .unwrap();
    let read = || CpuObservation::read_pool(&pool, CpuDomain::Mixed, Some(&retired));
    let initial = read().json();
    assert_eq!(initial["held"], Value::from(3u64));
    assert_eq!(initial["native_held"], Value::from(1u64));
    assert_eq!(initial["external"]["held"], Value::from(2u64));
    assert_eq!(initial["available"], Value::from(1u64));
    external.pop().unwrap().settle().unwrap();
    drop(external);
    let retained = read().json();
    assert_eq!(retained["held"], Value::from(2u64));
    assert_eq!(retained["external"]["held"], Value::from(0u64));
    assert_eq!(retained["external"]["retained"], Value::from(1u64));
    assert_eq!(retained["available"], Value::from(2u64));
    drop(worker);
    assert_eq!(read().available(), Some(3));
}

#[test]
fn adopted_scope_refuses_absent_partial_or_unvalidated_debt() {
    use crate::build_cpu::ExternalDebt;
    let dir = aterm_tempfile::Builder::new()
        .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
        .unwrap();
    let Some((_restore, retired)) = retirement(dir.path()) else {
        return;
    };
    let pool = pool(dir.path(), 2);
    assert!(matches!(
        CpuObservation::read_pool(&pool, CpuDomain::Legacy, None),
        CpuObservation::Ready { external: None, .. }
    ));
    assert!(matches!(
        CpuObservation::read_pool(&pool, CpuDomain::Mixed, Some(&retired)),
        CpuObservation::Unreadable(_)
    ));
    let _debt = ExternalDebt::activate(&pool, &retired).unwrap();
    assert!(matches!(
        CpuObservation::read_pool(&pool, CpuDomain::Mixed, None),
        CpuObservation::Unreadable(_)
    ));
    // Tamper the existing upstream readiness file; do not parse its private format.
    let anchor = pool.path().join("external-debt-v1");
    assert!(anchor.exists());
    std::fs::write(&anchor, b"partial").unwrap();
    let failed = CpuObservation::read_pool(&pool, CpuDomain::Mixed, Some(&retired));
    assert!(matches!(failed, CpuObservation::Unreadable(_)));
    for name in ["total", "held", "available", "external"] {
        assert!(failed.json().get(name).is_none());
    }
    assert_eq!(std::fs::read(anchor).unwrap(), b"partial");
}

#[test]
fn admission_transaction_busy_has_no_capacity_counts() {
    let dir = aterm_tempfile::Builder::new()
        .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
        .unwrap();
    let pool = pool(dir.path(), 2);
    let _lock = aterm_uds::file_lock::LockFile::take_existing(
        &pool.path().join("pool"),
        std::time::Instant::now() + std::time::Duration::from_secs(60),
        "test fixture lock is busy",
    )
    .unwrap();
    let observed = CpuObservation::read_pool(&pool, CpuDomain::Legacy, None);
    assert_eq!(observed, CpuObservation::Busy);
    assert!(observed.json().get("available").is_none());
}

#[test]
fn activated_debt_refuses_raw_new_clients_and_remains_fully_reserved() {
    use crate::build_cpu::ExternalDebt;
    let dir = aterm_tempfile::Builder::new()
        .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
        .unwrap();
    let Some((_restore, retired)) = retirement(dir.path()) else {
        return;
    };
    let pool = pool(dir.path(), 2);
    let debt = ExternalDebt::activate(&pool, &retired).unwrap();
    let _external = debt
        .try_reserve(Owner::current(Some(9)), NonZeroU32::new(2).unwrap(), 2)
        .unwrap()
        .unwrap();
    // New clients cannot bypass the activated shared-domain transaction.
    // Old executables are excluded by the separate durable retirement barrier.
    assert!(pool.try_acquire(Owner::current(Some(10))).is_err());
    let observed = CpuObservation::read_pool(&pool, CpuDomain::Mixed, Some(&retired));
    assert_eq!(observed.occupied(), Some(2));
    assert_eq!(observed.available(), Some(0));
}
