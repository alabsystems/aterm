// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The crate's flushes, in one place, for it and for aterm-gui. [`sync_all`] is
//! aterm-durable's DURABLE arm, the barrier an atomic publish takes where something
//! beyond the device's write order depends on it (a document save, the restore
//! manifest a quit writes); [`finish`] picks between it and [`aterm_durable::ordered`],
//! which every write that needs only its order kept takes (the config markers, the
//! crash journal's images, a pinned directory's artifacts, the session log).
//! [`fsync`] is the lighter flush the crash journal's retirement, adoption and boot
//! claim take.

pub use aterm_durable::Flush;
use std::fs::File;

/// Make one open file or directory DURABLE: the barrier an atomic publish takes on its
/// temporary before the rename, and on the directory after it. It is
/// [`aterm_durable::durable`], which on Apple targets is `fcntl(F_FULLFSYNC)`, a
/// drive-cache flush.
#[cfg(not(all(any(test, debug_assertions), target_vendor = "apple")))]
pub fn sync_all(file: &File) -> std::io::Result<()> {
    aterm_durable::durable(file)
}

/// [`sync_all`] in a debug build on Apple targets — every unit test of aterm-gui,
/// which reaches this crate as a dependency, so `cfg(test)` alone would leave its
/// tests on `F_FULLFSYNC` — a plain `fsync(2)`, retried on `EINTR` as std retries
/// its `F_FULLFSYNC` ([`aterm_durable::seam::durable_fsync`], which the counting
/// seam records as durable). It is still a real flush, made at
/// the same point and in the same order, so a test sees the same files, renames and
/// errors. What it drops is the drive-cache barrier. No test can observe that barrier,
/// because it only orders the disk against a power loss. It also holds up most
/// flushes, growing writes, creates and renames on the volume while it runs, so a
/// test binary running beside this one waits behind it for its own temp-dir metadata
/// (`docs/measured/flush-wait-2026-10-21.md`). Measured: one `aterm-gui --lib` run made
/// 4,763 `F_FULLFSYNC`s, 2,477 of them the restore manifest's. Run alone,
/// `crash_journal_conformance::the_real_journal_conforms_to_the_model` went from
/// 13.6-17.0 s to 3.7 s when they became `fsync(2)`, and a no-op flush gained nothing
/// more (4.3 s). With this arm the whole run made 1,230, all of them in other crates. It
/// took 49.7-51.9 s instead of 91.7-141.7 s beside `atpkg --lib` (load 15-37). Off Apple
/// targets `sync_all` already is `fsync(2)`, so the arm above stays. Every release
/// build — every shipped one — compiles the `F_FULLFSYNC` arm.
#[cfg(all(any(test, debug_assertions), target_vendor = "apple"))]
pub fn sync_all(file: &File) -> std::io::Result<()> {
    aterm_durable::seam::durable_fsync(file)
}

/// Finish a write to `file` with `flush`: [`Flush::Durable`] through [`sync_all`] (and
/// its debug arm), [`Flush::Ordered`] through [`aterm_durable::ordered`].
///
/// # Errors
/// The flush's own error.
pub fn finish(file: &File, flush: Flush) -> std::io::Result<()> {
    match flush {
        Flush::Durable => sync_all(file),
        Flush::Ordered => aterm_durable::ordered(file),
    }
}

/// A plain `fsync(2)`, not `F_FULLFSYNC`: the crash journal's retirement, adoption and
/// boot claim guard against process death, which the page cache survives. By its man
/// page it promises neither durability nor ORDER against a power loss, so the
/// journal's image publish, which needs its order kept, takes
/// [`aterm_durable::ordered`] instead ([`finish`]).
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
pub fn fsync(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: `fsync` on a descriptor this `File` owns for the call.
    if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
