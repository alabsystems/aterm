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
/// drive-cache flush (a plain `fsync(2)` in a test or debug build there, as every durable
/// flush: aterm-durable's crate docs say why and what it measured here).
#[inline]
pub fn sync_all(file: &File) -> std::io::Result<()> {
    aterm_durable::durable(file)
}

/// Finish a write to `file` with `flush`: [`Flush::Durable`] through [`sync_all`],
/// [`Flush::Ordered`] through [`aterm_durable::ordered`].
///
/// # Errors
/// The flush's own error.
#[inline]
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
/// The unit tests keep it, and [`sync_all`], a real flush (re-measured
/// 2026-10-06 beside eight spinners and three `F_FULLFSYNC` writers): with every
/// `fsync(2)` interposed as a descriptor check,
/// `crash_journal_conformance::the_real_journal_conforms_to_the_model` took
/// 40.3/38.7 s against 42.1/45.0 s and a whole `aterm-gui --lib` run 102.7/102.1 s
/// against 96.4/123.6 s, inside the noise. Alone, that test's 17,864 directory
/// flushes cost 0.7 µs each and its 6,516 file flushes 23 µs; beside other binaries
/// its replays wait on their creates and renames.
#[cfg(unix)]
#[inline]
pub fn fsync(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: `fsync` on a descriptor this `File` owns for the call.
    if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
