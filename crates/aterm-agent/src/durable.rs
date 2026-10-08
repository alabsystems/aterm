// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The crate's durability flushes, in one place: the fleet operator's WAL frames,
//! checkpoints, fault marker, private files and their directories
//! ([`crate::operator`]), the harness's atomic writes
//! ([`crate::harness::upgrade_models::write_atomic_mode`]) and its ledger cut
//! ([`crate::harness::cli::bound_ledger`]).

use std::fs::File;

/// Make one open file or directory DURABLE: the barrier an atomic publish takes on its
/// temporary before the rename, and on the directory after it. It is
/// [`File::sync_all`], which on Apple targets is `fcntl(F_FULLFSYNC)`, a drive-cache
/// flush, and `fsync(2)` elsewhere.
#[cfg(not(all(test, target_vendor = "apple")))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    file.sync_all()
}

/// Make one open file's DATA durable: the barrier a WAL append takes before its frame
/// counts. It is [`File::sync_data`], which on Apple targets is the same
/// `fcntl(F_FULLFSYNC)` as [`sync_all`], and `fdatasync(2)` on Linux.
#[cfg(not(all(test, target_vendor = "apple")))]
pub(crate) fn sync_data(file: &File) -> std::io::Result<()> {
    file.sync_data()
}

/// [`sync_all`] under this crate's own unit tests on Apple targets: a plain `fsync(2)`,
/// retried on `EINTR` as std retries its `F_FULLFSYNC`. It is still a real flush, made at
/// the same point and in the same order, so a test sees the same files, renames and
/// errors. What it drops is the drive-cache barrier. No test can observe that barrier,
/// because it only orders the disk against a power loss. It also stalls the whole VOLUME
/// while it runs, so every test binary the gate runs beside this one waits behind it for
/// its own temp-dir metadata. Measured 2026-10-06: one `aterm-agent --lib` run made 2,561
/// `F_FULLFSYNC`s, 17.7 s of flushing across its threads: about 1,790 WAL appends
/// (`commit_record`), 450 of the operator's directory, private-file, fault-marker and
/// checkpoint flushes, 290 `write_atomic_mode`s and 3 ledger cuts. With these arms it
/// makes 2, both in two tests' own code. The run took 45.8-46.3 s instead of 49.8-50.9 s
/// alone, and 52.9-54.1 s instead of 57.7-57.9 s beside the aterm-primer,
/// aterm-update-core and aterm-release unit-test runs; beside it, those first two run
/// back to back took at most 1.0-1.1 s instead of 2.5 s. Off Apple targets `sync_all`
/// already is `fsync(2)`, so the arm above stays. Integration tests, every shipped build
/// and every crate that depends on this one (an `aterm-gui --lib` run made about 835 of
/// these through the operator and `write_atomic_mode`, counted 2026-10-06) compile it
/// too.
#[cfg(all(test, target_vendor = "apple"))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;
    loop {
        // SAFETY: `fsync` on a descriptor this `File` owns for the call.
        if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// [`sync_data`] under this crate's own unit tests on Apple targets: the same plain
/// `fsync(2)` as [`sync_all`]'s test arm, since std's `sync_data` there is the same
/// `F_FULLFSYNC` as its `sync_all`.
#[cfg(all(test, target_vendor = "apple"))]
pub(crate) fn sync_data(file: &File) -> std::io::Result<()> {
    sync_all(file)
}
