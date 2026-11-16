// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Descriptor pressure is created ONLY in each maintained isolated test
//! subprocess. Signed structural Mach-O fixtures are never executed as Python.
use super::*;
use std::{
    fs::File,
    io,
    os::{fd::AsRawFd as _, unix::fs::MetadataExt as _},
    sync::mpsc,
};

const SLOTS: usize = 128;

struct Limit(libc::rlimit);
impl Limit {
    fn enter() -> Self {
        let mut original = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: initialized exclusively borrowed output; no host/app process
        // is touched, and both callers already entered their own subprocess.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut original) },
            0
        );
        assert!(original.rlim_cur >= SLOTS as libc::rlim_t);
        let next = libc::rlimit {
            rlim_cur: SLOTS as libc::rlim_t,
            rlim_max: original.rlim_max,
        };
        // SAFETY: only this isolated test process's soft limit is reduced.
        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const next) },
            0
        );
        Self(original)
    }
}
impl Drop for Limit {
    fn drop(&mut self) {
        // SAFETY: restore this isolated process's original limits. No hard
        // limit was lowered, so restoring its original soft limit is allowed.
        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const self.0) },
            0
        );
    }
}

fn fill() -> Vec<File> {
    let mut population = Vec::new();
    // One finite pass; no pressure/retry loop or unrelated descriptors closed.
    for _ in 0..SLOTS {
        match aterm_uds::kept_out(|| File::open("/dev/null")) {
            Ok(file) => population.push(file),
            Err(error) => {
                assert_eq!(error.raw_os_error(), Some(libc::EMFILE));
                return population;
            }
        }
    }
    panic!("bounded isolated descriptor population did not reach its soft limit");
}

fn descriptors() -> usize {
    (0..SLOTS)
        .filter(|number| {
            // SAFETY: metadata-only query of this process's descriptor number;
            // no saved descriptor is adopted, closed or has its flags changed.
            (unsafe { libc::fcntl(*number as i32, libc::F_GETFD) }) >= 0
        })
        .count()
}

fn paired(fixture: &Fixture) -> HeldTool {
    let native = acquire(fixture)
        .into_native(Instant::now() + Duration::from_secs(60))
        .unwrap();
    native.check_current().unwrap();
    drop(native);
    assert!(!kept(fixture));
    acquire(fixture)
}

#[test]
fn partial_native_capture_emfile_closes_only_new_descriptors_and_returns_original_owner() {
    if !crate::testdir::isolated_lifecycle(
        "prerequisite::tests::native_inputs::exhaustion::partial_native_capture_emfile_closes_only_new_descriptors_and_returns_original_owner",
    ) {
        return;
    }
    let fixture = fixture();
    let tool = paired(&fixture);
    let image = tool.image.as_raw_fd();
    let identity = tool.image.metadata().unwrap();
    let end = Instant::now() + Duration::from_secs(60);
    let _limit = Limit::enter();
    let mut pressure = fill();
    // Original metadata checks need one slot; the actual native traversal then
    // starts retaining original directories before this limited room runs out.
    assert!(pressure.len() > 8);
    pressure.truncate(pressure.len() - 8);
    let before = descriptors();
    let refused = tool
        .into_native(end)
        .err()
        .expect("actual capture must run out of descriptors");
    assert_eq!(refused.error().raw_os_error(), Some(libc::EMFILE));
    assert_eq!(
        descriptors(),
        before,
        "only new partial capture FDs must close"
    );
    let (_, original) = refused.into_parts();
    assert_eq!(original.image.as_raw_fd(), image);
    assert_eq!(original.native_attempt, Some(end));
    let after = original.image.metadata().unwrap();
    assert_eq!((identity.dev(), identity.ino()), (after.dev(), after.ino()));
    drop(pressure);
    original.validate().unwrap();
    assert!(kept(&fixture));
    let refused = (*original)
        .into_native(end + Duration::from_secs(60))
        .err()
        .unwrap();
    assert_eq!(refused.error().kind(), io::ErrorKind::PermissionDenied);
    let (_, original) = refused.into_parts();
    assert_eq!(original.native_attempt, Some(end));
    drop(original);
    assert!(!kept(&fixture));
}

#[test]
fn concurrent_open_pressure_after_original_hash_cannot_manufacture_native_success() {
    if !crate::testdir::isolated_lifecycle(
        "prerequisite::tests::native_inputs::exhaustion::concurrent_open_pressure_after_original_hash_cannot_manufacture_native_success",
    ) {
        return;
    }
    let fixture = fixture();
    let tool = paired(&fixture);
    let image = tool.image.as_raw_fd();
    let end = Instant::now() + Duration::from_secs(60);
    let (go, begin) = mpsc::sync_channel(1);
    let (filled, ready) = mpsc::sync_channel(1);
    let (release, stop) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        if begin
            .recv_timeout(end.saturating_duration_since(Instant::now()))
            .is_err()
        {
            return;
        }
        let pressure = fill();
        let _ = filled.send(());
        let _ = stop.recv_timeout(end.saturating_duration_since(Instant::now()));
        drop(pressure);
    });
    let _limit = Limit::enter();
    let refused = tool
        .into_native_with(end, || {
            go.send(()).map_err(io::Error::other)?;
            ready
                .recv_timeout(end.saturating_duration_since(Instant::now()))
                .map_err(io::Error::other)?;
            Ok(())
        })
        .err()
        .expect("concurrent real opens must refuse native capture");
    let count = descriptors();
    assert_eq!(refused.error().raw_os_error(), Some(libc::EMFILE));
    let (_, original) = refused.into_parts();
    assert_eq!(original.image.as_raw_fd(), image);
    assert_eq!(original.native_attempt, Some(end));
    assert_eq!(descriptors(), count);
    let _ = release.send(());
    drop(go);
    worker.join().unwrap();
    original.validate().unwrap();
    assert!(kept(&fixture));
    drop(original);
    assert!(!kept(&fixture));
}

#[test]
fn native_capture_unwind_retains_the_original_signed_lease() {
    if !crate::testdir::isolated_lifecycle(
        "prerequisite::tests::native_inputs::exhaustion::native_capture_unwind_retains_the_original_signed_lease",
    ) {
        return;
    }
    let fixture = fixture();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        acquire(&fixture).into_native_with(Instant::now() + Duration::from_secs(60), || {
            panic!("controlled native capture unwind after original hashing")
        })
    }));
    assert!(result.is_err());
    assert!(kept(&fixture));
}
