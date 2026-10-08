// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The crate's durability flushes, in one place. [`sync_all`] is the barrier an atomic
//! publish takes: the restore manifest, the config markers, a document save and its
//! draft journal, and a pinned directory's private files. [`fsync`] is the lighter flush
//! the crash journal takes.

use std::fs::File;

/// Make one open file or directory DURABLE: the barrier an atomic publish takes on its
/// temporary before the rename, and on the directory after it. It is
/// [`File::sync_all`], which on Apple targets is `fcntl(F_FULLFSYNC)`, a drive-cache
/// flush.
#[cfg(not(all(test, target_vendor = "apple")))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    file.sync_all()
}

/// [`sync_all`] under this crate's own unit tests on Apple targets: a plain `fsync(2)`,
/// retried on `EINTR` as std retries its `F_FULLFSYNC`. It is still a real flush, made at
/// the same point and in the same order, so a test sees the same files, renames and
/// errors. What it drops is the drive-cache barrier. No test can observe that barrier,
/// because it only orders the disk against a power loss. It also stalls the whole
/// VOLUME while it runs, so every test binary the gate runs beside this one waits behind
/// it for its own temp-dir metadata. Measured 2026-10-05: one `aterm-gui --lib` run made
/// 4,763 `F_FULLFSYNC`s, 2,477 of them the restore manifest's. Run alone,
/// `crash_journal_conformance::the_real_journal_conforms_to_the_model` went from
/// 13.6-17.0 s to 3.7 s when they became `fsync(2)`, and a no-op flush gained nothing
/// more (4.3 s). With this arm the whole run made 1,230, all of them in other crates. It
/// took 49.7-51.9 s instead of 91.7-141.7 s beside `atpkg --lib` (load 15-37). Off Apple
/// targets `sync_all` already is `fsync(2)`, so the arm above stays. Integration tests
/// and every shipped build compile it too.
#[cfg(all(test, target_vendor = "apple"))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    loop {
        match fsync(file) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            flushed => return flushed,
        }
    }
}

/// A plain `fsync(2)`, not `F_FULLFSYNC`: the crash journal guards against process
/// death, which the page cache survives; this orders bytes before the rename without
/// flushing the drive cache on every write. A torn image after power loss reads as
/// unreadable.
///
/// The unit tests keep it, and [`sync_all`]'s arm above, a real flush (re-measured
/// 2026-10-06 beside eight spinners and three `F_FULLFSYNC` writers): with every
/// `fsync(2)` interposed as a descriptor check,
/// `crash_journal_conformance::the_real_journal_conforms_to_the_model` took
/// 40.3/38.7 s against 42.1/45.0 s and a whole `aterm-gui --lib` run 102.7/102.1 s
/// against 96.4/123.6 s, inside the noise. Alone, that test's 17,864 directory
/// flushes cost 0.7 µs each and its 6,516 file flushes 23 µs; beside other binaries
/// its replays wait on their creates and renames.
#[cfg(unix)]
pub(crate) fn fsync(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: `fsync` on a descriptor this `File` owns for the call.
    if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
