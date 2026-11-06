// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::{PermissionsExt as _, symlink};

fn deadline() -> login_drive::Deadline<'static> {
    login_drive::Deadline::after(Duration::from_secs(60))
}

#[test]
fn the_actual_native_http_guard_expires_on_sleep_in_the_original_clock_domain() {
    assert!(aterm_uds::process::continuous_ns().is_some());
    for lease_fresh in [true, false] {
        let home = aterm_tempfile::tempdir().unwrap();
        let budget = login_drive::Deadline::after(Duration::from_secs(3600));
        let lock = NativeLock::acquire(
            home.path(),
            Region::Global,
            budget,
            Arc::new(aterm_http::AlwaysAuthorized),
        )
        .unwrap();
        let guard = lock.guard();
        assert!(guard.is_authorized());
        {
            let mut lease = lock.lease.lock().unwrap();
            // Same live continuous clock as the original budget, advanced only for this lease.
            lease
                .slept_ns
                .store(7200 * 1_000_000_000, std::sync::atomic::Ordering::Release);
            if lease_fresh {
                lease.updated_continuous = lease
                    .updated_continuous
                    .map(|now| now + 7200 * 1_000_000_000);
            }
        }
        let refused = !guard.is_authorized();
        let current = lock.check_current();
        assert_eq!(guard.receipt_current(), lease_fresh);
        let before = std::fs::metadata(path(home.path(), Region::Global))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(lock.lease.lock().unwrap().heartbeat(), lease_fresh);
        if !lease_fresh {
            assert_eq!(
                std::fs::metadata(path(home.path(), Region::Global))
                    .unwrap()
                    .modified()
                    .unwrap(),
                before,
                "a stale lease is not revived by its former heartbeat"
            );
        }
        drop(lock);
        assert!(refused);
        assert!(current.is_err());
        assert!(!path(home.path(), Region::Global).exists());
    }
}

fn path(home: &Path, region: Region) -> PathBuf {
    let credential = credential_path(home, region);
    let name = credential.file_stem().unwrap().to_str().unwrap();
    home.join("oauth").join(format!("{name}.lock"))
}

fn stamp(path: &Path, time: SystemTime) {
    astream_sys::open_nonblocking(path, astream_sys::Links::Refuse)
        .unwrap()
        .set_times(FileTimes::new().set_modified(time))
        .unwrap();
}

#[test]
fn native_lock_uses_the_region_storage_slot_and_releases_its_guard() {
    for region in [Region::Global, Region::MainlandCn] {
        let home = aterm_tempfile::tempdir().unwrap();
        let lock = NativeLock::acquire(
            home.path(),
            region,
            deadline(),
            Arc::new(aterm_http::AlwaysAuthorized),
        )
        .unwrap();
        let path = path(home.path(), region);
        assert!(path.is_dir());
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
        assert!(
            std::fs::create_dir(&path).is_err(),
            "native peer cannot acquire"
        );
        assert!(
            NativeLock::acquire(
                home.path(),
                region,
                deadline(),
                Arc::new(aterm_http::AlwaysAuthorized)
            )
            .is_err()
        );
        let guard = lock.guard();
        assert!(guard.is_authorized());
        drop(lock);
        assert!(!path.exists());
        assert!(
            !guard.is_authorized(),
            "a retained HTTP guard cannot outlive its lock"
        );
        drop(
            NativeLock::acquire(
                home.path(),
                region,
                deadline(),
                Arc::new(aterm_http::AlwaysAuthorized),
            )
            .unwrap(),
        );
    }
}

#[test]
fn stale_native_lock_recovery_refuses_fresh_future_or_unowned_shapes() {
    for case in 0..7 {
        let home = aterm_tempfile::tempdir().unwrap();
        aterm_secrets::fsx::ensure_private_dir(&home.path().join("oauth")).unwrap();
        let path = path(home.path(), Region::Global);
        match case {
            4 => std::fs::write(&path, b"not a lock directory").unwrap(),
            5 => symlink(home.path(), &path).unwrap(),
            _ => std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap(),
        }
        if case == 3 {
            std::fs::write(path.join("keep"), b"not an empty native lock").unwrap();
        }
        if case == 6 {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
        }
        if matches!(case, 0 | 2 | 3 | 6) {
            stamp(
                &path,
                if case == 2 {
                    SystemTime::now() + Duration::from_secs(60)
                } else {
                    SystemTime::now() - Duration::from_secs(60)
                },
            );
        }
        let before = std::fs::symlink_metadata(&path).unwrap();
        let result = NativeLock::acquire(
            home.path(),
            Region::Global,
            deadline(),
            Arc::new(aterm_http::AlwaysAuthorized),
        );
        if case == 0 {
            let lock = result.unwrap();
            assert!(lock.guard().is_authorized());
            drop(lock);
            assert!(!path.exists());
        } else {
            assert!(result.is_err(), "case {case}");
            assert_eq!(
                identity(&std::fs::symlink_metadata(&path).unwrap()),
                identity(&before)
            );
            if case == 3 {
                assert_eq!(
                    std::fs::read(path.join("keep")).unwrap(),
                    b"not an empty native lock"
                );
            }
        }
    }
}

#[test]
fn lost_native_lock_never_touches_or_removes_a_replacement() {
    for replace_inode in [false, true] {
        let home = aterm_tempfile::tempdir().unwrap();
        let lock = NativeLock::acquire(
            home.path(),
            Region::Global,
            deadline(),
            Arc::new(aterm_http::AlwaysAuthorized),
        )
        .unwrap();
        let path = path(home.path(), Region::Global);
        let guard = lock.guard();
        if replace_inode {
            std::fs::rename(&path, path.with_extension("retained")).unwrap();
            std::fs::create_dir(&path).unwrap();
        } else {
            stamp(&path, SystemTime::now() + Duration::from_secs(60));
        }
        let before = std::fs::symlink_metadata(&path).unwrap();
        assert!(!guard.is_authorized());
        assert!(!lock.lease.lock().unwrap().heartbeat());
        assert_eq!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .modified()
                .unwrap(),
            before.modified().unwrap()
        );
        drop(lock);
        assert_eq!(
            identity(&std::fs::symlink_metadata(&path).unwrap()),
            identity(&before)
        );
    }
}

#[test]
fn native_lease_guard_rejects_a_missed_heartbeat_or_deadline() {
    for expired_deadline in [false, true] {
        let home = aterm_tempfile::tempdir().unwrap();
        let lock = NativeLock::acquire(
            home.path(),
            Region::Global,
            deadline(),
            Arc::new(aterm_http::AlwaysAuthorized),
        )
        .unwrap();
        let guard = lock.guard();
        {
            let mut lease = lock.lease.lock().unwrap();
            if expired_deadline {
                lease.deadline = login_drive::Deadline::after(Duration::ZERO).bounds();
            } else {
                lease.updated = Instant::now() - STALE;
            }
            // Work expiry denies HTTP, while the held owner can retain the exact response's
            // lease until checkpoint/retirement. A missed heartbeat still loses authority.
            assert_eq!(lease.heartbeat(), expired_deadline);
        }
        assert!(!guard.is_authorized());
        drop(lock);
        assert!(!path(home.path(), Region::Global).exists());
    }
}

#[test]
fn native_heartbeat_updates_mtime_and_keeps_a_peer_excluded() {
    let home = aterm_tempfile::tempdir().unwrap();
    let lock = NativeLock::acquire(
        home.path(),
        Region::Global,
        deadline(),
        Arc::new(aterm_http::AlwaysAuthorized),
    )
    .unwrap();
    let path = path(home.path(), Region::Global);
    let first = std::fs::metadata(&path).unwrap().modified().unwrap();
    let hang = deadline();
    while std::fs::metadata(&path).unwrap().modified().unwrap() == first {
        assert!(!hang.expired(), "heartbeat worker stopped making progress");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(lock.guard().is_authorized());
    assert!(std::fs::create_dir(&path).is_err());
}
