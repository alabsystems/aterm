// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::{Path, PathBuf};

use super::write_open_files;

const NO_SUCH_PID: u32 = 2_147_483_647;

fn named(got: &[PathBuf], p: &Path) -> bool {
    got.iter().any(|g| {
        g == p
            || g.strip_prefix("/System/Volumes/Data")
                .is_ok_and(|rel| Path::new("/").join(rel) == p)
    })
}

/// A child killed and reaped when it drops, so a failed assertion or a hang detector leaves
/// nothing running (`top -l 0` samples forever).
#[cfg(target_vendor = "apple")]
struct KilledOnDrop(crate::spawnfd::Child);

#[cfg(target_vendor = "apple")]
impl Drop for KilledOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A process of this user's that became a set-user-ID program after the listing (`/bin/ps`
/// run from a shell, measured refusing the disk sweep's scan under a `ps` loop) is outside
/// the scan, not unread; `top` is one that stays to be read, handed to the scan as the
/// listing would have before its exec. Negative control: without the guard it is unread.
#[cfg(target_vendor = "apple")]
#[test]
fn a_process_that_became_a_set_user_id_program_is_outside_the_path_scan() {
    // SAFETY: `geteuid` is a side-effect-free getter.
    let me = unsafe { super::super::ffi::geteuid() };
    assert_ne!(me, 0, "a scan as root reads every process");
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path().canonicalize().expect("canonical scratch dir");
    let top = KilledOnDrop(
        crate::spawnfd::command("/usr/bin/top")
            .args(["-l", "0", "-s", "60"])
            .stdin(crate::spawnfd::Stdio::null())
            .stdout(crate::spawnfd::Stdio::null())
            .stderr(crate::spawnfd::Stdio::null())
            .spawn()
            .expect("spawn top"),
    );
    let pid = top.0.id();
    let started = std::time::Instant::now();
    while crate::process::read_short_info(i32::try_from(pid).expect("a pid"))
        .is_none_or(|info| info.pbsi_uid == me)
    {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "top never ran as its owner"
        );
        crate::deadline::poll_yield();
    }
    let scan = super::scan_path_users_of(&[pid], std::slice::from_ref(&dir), &[]);
    drop(top);
    let scan = scan.expect("scan");
    assert_eq!(scan.scanned, 1, "{scan:?}");
    assert!(
        scan.unreadable.is_empty(),
        "outside this user's scan: {scan:?}"
    );
    assert!(scan.users.is_empty(), "{scan:?}");
}

#[test]
fn the_writes_reader_names_the_regular_files_open_for_writing_and_nothing_else() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path().canonicalize().expect("canonical scratch dir");
    let wo = dir.join("write-only.log");
    let rw = dir.join("read-write.db");
    let ro = dir.join("read-only.txt");
    std::fs::write(&ro, b"x").expect("seed the read-only file");
    let _w = std::fs::File::create(&wo).expect("open write-only");
    let _rw = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&rw)
        .expect("open read-write");
    let _r = std::fs::File::open(&ro).expect("open read-only");
    let _null = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .expect("open /dev/null for writing");
    let got = write_open_files(std::process::id()).expect("own table");
    assert!(named(&got, &wo), "{wo:?} not in {got:?}");
    assert!(named(&got, &rw), "{rw:?} not in {got:?}");
    assert!(!named(&got, &ro), "a read-only open is no write: {got:?}");
    assert!(
        !got.iter().any(|g| g.starts_with("/dev")),
        "a device is no regular file: {got:?}"
    );
}

#[test]
fn a_process_that_does_not_exist_holds_no_file() {
    assert_eq!(write_open_files(NO_SUCH_PID).ok(), Some(Vec::new()));
}

#[cfg(target_vendor = "apple")]
#[test]
fn the_open_files_reader_names_a_read_only_file_and_refuses_an_exited_pid() {
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path().canonicalize().expect("canonical scratch dir");
    let lock = dir.join("held.lock");
    std::fs::write(&lock, b"").expect("seed the lock");
    let _held = std::fs::File::open(&lock).expect("open read-only");
    let got = super::open_files(std::process::id()).expect("own table");
    assert!(named(&got, &lock), "{lock:?} not in {got:?}");
    let gone = super::open_files(NO_SUCH_PID).map_err(|e| e.kind());
    assert_eq!(gone, Err(std::io::ErrorKind::NotFound));
}
