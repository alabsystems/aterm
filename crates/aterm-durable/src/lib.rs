// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-durable` — the two ways aterm finishes a write, in one place.
//!
//! A write that must outlive more than a process death ends with a flush, and there
//! are two. They are named by what a power loss leaves; a kernel panic, a forced
//! power-off and a watchdog reset count as one here, since no Apple document says the
//! drive's volatile cache survives them.
//!
//! * [`durable`]: the data is on stable media when the call returns. Apple:
//!   `fcntl(F_FULLFSYNC)` (std's [`File::sync_all`]), which drains the device's whole
//!   queue. Measured on one Mac (Apple SSD, APFS, under other agents' builds): p50
//!   4.3 ms in steps of about 4.2 ms, and while it runs a neighbour's 64-byte write
//!   with no flush went to p95 4-5 ms. A later probe there
//!   (`docs/measured/flush-wait-2026-10-21.md`) saw every barrier it caught, and
//!   most `fsync(2)`s, growing appends, creates and renames, wait out a full flush
//!   in flight from another thread; a read and an in-place page-cache write did not.
//!   Elsewhere the platform full flush. Stable-on-return is the supported
//!   filesystem/device contract, not an independent check of the hardware. A site
//!   needs it when something outside this device's write order depends on the data
//!   having reached the media: a reply or ack that promises it is saved, a network
//!   send or other irreversible effect that assumes it, a promise made to the person,
//!   or a write whose loss the next boot's recovery would neither notice nor repair.
//! * [`ordered`]: flush the named descriptor, then order that I/O and any other I/O
//!   already flushed with `fsync` on the same device before later I/O. Nothing is
//!   promised durable on return, and unflushed other descriptors are not covered.
//!   Apple: `fcntl(F_BARRIERFSYNC)` (astream-sys's `barrier_sync`), measured p50
//!   0.13-0.6 ms beside the same load, with a neighbour tax an order of magnitude
//!   smaller. A site may use this for "A persists before B" on one device only
//!   after flushing every descriptor that phase A depends on, including directory
//!   publications. The barrier does not order writes within a phase, make a batch
//!   atomic, or prove that a reader accepts only a valid prefix. Each caller must
//!   establish those invariants and tolerate older state; its comment names that
//!   recovery. A returned flush error or an intentionally ignored directory error
//!   supplies no such ordering guarantee.
//!
//! A directory flush after a create, rename or unlink is the same choice, because
//! the entry is metadata on the same device ([`durable_dir`], [`ordered_dir`]).
//!
//! THE BARRIER IS DEVICE-WIDE AND ONLY DEVICE-WIDE. fcntl(2): "The barrier applies to
//! I/O that have been flushed with fsync(2) on the same device before. These
//! operations are guaranteed to be persisted before any other I/O that would follow
//! the barrier." It can therefore cover other descriptors on the volume only when
//! their I/O was flushed before the barrier. It promises nothing about another
//! volume: where the writes that must come
//! after live on a different device, [`Flush::ordered_if_same_device`] answers
//! [`Flush::Durable`]. Under that same storage contract, a successful full flush
//! also persists the earlier, already-flushed I/O on its device.
//!
//! Off Apple targets there is no ordering-only flush (Linux's `fdatasync` flushes the
//! drive cache too), so [`ordered`] is the same `fsync(2)` as [`durable`]: never
//! weaker than asked, and byte-for-byte the flush those platforms took before.
//!
//! ## Test and debug builds
//!
//! (And any build with the `test-support` feature, which a `--release` test run of a
//! dependent names on its command line, `--features aterm-durable/test-support`.)
//! [`seam::Recording`] records which of the two each operation on this thread asked
//! for, so a test asserts the tier a site chose with no timing. It records and changes
//! nothing else: [`ordered`] issues the real `F_BARRIERFSYNC` in every build, test,
//! debug, `opt-test` and release alike, so a debug instance (`lab instance`) or the
//! `opt-test` perf lane measures the flush that ships. That costs a test run little:
//! measured on the crash journal's conformance test, the heaviest user, 3.8-7.5 s
//! against 2.2-4.2 s with a plain `fsync(2)` in its place (load 27-47). What a crate's
//! own unit tests may swap is the DURABLE flush (`seam::durable_fsync`, Unix only),
//! whose `F_FULLFSYNC` costs a step of about 4.2 ms and, while it runs, holds up
//! most flushes, growing writes, creates and renames made beside it: a probe saw
//! every barrier it caught and 72-99% of the rest wait, from threads of one process
//! on files in one directory tree (`docs/measured/flush-wait-2026-10-21.md`).

use std::fs::File;
use std::io;
use std::path::Path;

/// Which flush finished a write. See the [crate docs](crate) for what each leaves
/// after a power loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flush {
    /// On stable media when the flush returns ([`durable`]).
    Durable,
    /// Flush this descriptor and order the flushed I/O ahead of later writes on
    /// its device, with no general stable-on-return promise ([`ordered`]).
    Ordered,
}

impl Flush {
    /// Canonical 1-byte wire tag (`1` = `Durable`, `2` = `Ordered`).
    #[inline]
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Durable => 1,
            Self::Ordered => 2,
        }
    }

    /// Decode a 1-byte wire tag (`1` = `Durable`, `2` = `Ordered`).
    #[inline]
    #[must_use]
    pub const fn from_u8(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Durable),
            2 => Some(Self::Ordered),
            _ => None,
        }
    }

    /// Whether this flush is [`Flush::Durable`].
    #[inline]
    #[must_use]
    pub const fn is_durable(self) -> bool {
        matches!(self, Self::Durable)
    }

    /// Whether this flush is [`Flush::Ordered`].
    #[inline]
    #[must_use]
    pub const fn is_ordered(self) -> bool {
        matches!(self, Self::Ordered)
    }

    /// [`Flush::Ordered`] when `written_dev == later_dev`, else [`Flush::Durable`].
    #[inline]
    #[must_use]
    pub const fn for_devices(written_dev: u64, later_dev: u64) -> Self {
        if written_dev == later_dev {
            Self::Ordered
        } else {
            Self::Durable
        }
    }

    /// Encode a 17-byte device-pair decision record `(flush_tag: u8, written_dev: u64, later_dev: u64)`
    /// in little-endian byte order using bitwise shifts.
    #[inline]
    #[must_use]
    pub const fn encode_device_pair(written_dev: u64, later_dev: u64) -> [u8; 17] {
        let tag = Self::for_devices(written_dev, later_dev).as_u8();
        [
            tag,
            (written_dev & 0xFF) as u8,
            ((written_dev >> 8) & 0xFF) as u8,
            ((written_dev >> 16) & 0xFF) as u8,
            ((written_dev >> 24) & 0xFF) as u8,
            ((written_dev >> 32) & 0xFF) as u8,
            ((written_dev >> 40) & 0xFF) as u8,
            ((written_dev >> 48) & 0xFF) as u8,
            ((written_dev >> 56) & 0xFF) as u8,
            (later_dev & 0xFF) as u8,
            ((later_dev >> 8) & 0xFF) as u8,
            ((later_dev >> 16) & 0xFF) as u8,
            ((later_dev >> 24) & 0xFF) as u8,
            ((later_dev >> 32) & 0xFF) as u8,
            ((later_dev >> 40) & 0xFF) as u8,
            ((later_dev >> 48) & 0xFF) as u8,
            ((later_dev >> 56) & 0xFF) as u8,
        ]
    }

    /// Decode a 17-byte device-pair decision record `(Flush, written_dev, later_dev)`,
    /// returning `None` if the tag is invalid or inconsistent with `for_devices(written_dev, later_dev)`.
    #[inline]
    #[must_use]
    pub const fn decode_device_pair(bytes: &[u8; 17]) -> Option<(Self, u64, u64)> {
        let Some(flush) = Self::from_u8(bytes[0]) else {
            return None;
        };
        let written_dev = (bytes[1] as u64)
            | ((bytes[2] as u64) << 8)
            | ((bytes[3] as u64) << 16)
            | ((bytes[4] as u64) << 24)
            | ((bytes[5] as u64) << 32)
            | ((bytes[6] as u64) << 40)
            | ((bytes[7] as u64) << 48)
            | ((bytes[8] as u64) << 56);
        let later_dev = (bytes[9] as u64)
            | ((bytes[10] as u64) << 8)
            | ((bytes[11] as u64) << 16)
            | ((bytes[12] as u64) << 24)
            | ((bytes[13] as u64) << 32)
            | ((bytes[14] as u64) << 40)
            | ((bytes[15] as u64) << 48)
            | ((bytes[16] as u64) << 56);
        let expected = Self::for_devices(written_dev, later_dev);
        if flush.as_u8() != expected.as_u8() {
            return None;
        }
        Some((flush, written_dev, later_dev))
    }

    /// Finish a write to `file` with this flush.
    ///
    /// # Errors
    /// The flush's own error.
    #[inline]
    pub fn file(self, file: &File) -> io::Result<()> {
        match self {
            Self::Durable => durable(file),
            Self::Ordered => ordered(file),
        }
    }

    /// Finish a directory-entry change in `dir` with this flush.
    ///
    /// # Errors
    /// The directory could not be opened, or the flush's own error.
    #[inline]
    pub fn dir(self, dir: &Path) -> io::Result<()> {
        match File::open(dir) {
            Ok(f) => self.file(&f),
            Err(e) => Err(e),
        }
    }

    /// [`Flush::Ordered`] when `written` and `later` are on one device, else
    /// [`Flush::Durable`]. `written` is what this flush finishes; `later` is where the
    /// writes that must not persist before it land. A barrier orders only its own
    /// device's I/O, so a later write on another volume could reach its media first;
    /// a path that cannot be read answers [`Flush::Durable`], the safe direction.
    #[must_use]
    pub fn ordered_if_same_device(written: &Path, later: &Path) -> Self {
        if same_device(written, later) {
            Self::Ordered
        } else {
            Self::Durable
        }
    }
}

/// Make everything written to `file` DURABLE: on stable media when this returns. Apple:
/// `fcntl(F_FULLFSYNC)`; elsewhere `fsync(2)`.
///
/// # Errors
/// The flush's own error.
pub fn durable(file: &File) -> io::Result<()> {
    #[cfg(any(test, debug_assertions, feature = "test-support"))]
    seam::note(Flush::Durable);
    primitive::full(file)
}

/// [`durable`] for `file`'s data and the metadata a read of it needs (its size), not
/// its timestamps: std's [`File::sync_data`]. Apple: the same `fcntl(F_FULLFSYNC)`;
/// Linux: `fdatasync(2)`, which a log append that changes no other metadata pays less
/// for.
///
/// # Errors
/// The flush's own error.
pub fn durable_data(file: &File) -> io::Result<()> {
    #[cfg(any(test, debug_assertions, feature = "test-support"))]
    seam::note(Flush::Durable);
    primitive::full_data(file)
}

/// ORDER everything written to `file`, and everything else this process or another
/// flushed on its device, ahead of every write issued after this call. Nothing is
/// promised durable on return. Apple: `fcntl(F_BARRIERFSYNC)`; a filesystem or device
/// that refuses the barrier gets [`durable`]'s full flush instead, which orders by
/// persisting. Measured on macOS 27.0.1: devfs and autofs refuse it, while APFS, HFS+,
/// and FAT and exFAT (both through FSKit) take it, so an ordered write to a FAT or
/// exFAT stick is ordered on return, not persisted. Any other barrier error is
/// returned, never covered by a full flush that happens to succeed after it
/// ([`primitive::barrier`]). Elsewhere `fsync(2)`, as [`durable`].
///
/// # Errors
/// The flush's own error (the full flush's, when the barrier was refused).
pub fn ordered(file: &File) -> io::Result<()> {
    #[cfg(any(test, debug_assertions, feature = "test-support"))]
    seam::note(Flush::Ordered);
    primitive::barrier(file)
}

/// [`durable`] for the directory `dir`: what makes a create, rename or unlink in it
/// durable.
///
/// # Errors
/// The directory could not be opened, or the flush's own error.
pub fn durable_dir(dir: &Path) -> io::Result<()> {
    match File::open(dir) {
        Ok(f) => durable(&f),
        Err(e) => Err(e),
    }
}

/// [`ordered`] for the directory `dir`: a create, rename or unlink in it reaches the
/// media before any write issued after this call.
///
/// # Errors
/// The directory could not be opened, or the flush's own error.
pub fn ordered_dir(dir: &Path) -> io::Result<()> {
    match File::open(dir) {
        Ok(f) => ordered(&f),
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn same_device(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_device(_a: &Path, _b: &Path) -> bool {
    // Off unix `ordered` is the full flush anyway, so the answer changes nothing.
    false
}

/// The flushes themselves, unrecorded. Public so this crate's tests, and a reviewer,
/// can reach each one by name; product code finishes a write with [`durable`] or
/// [`ordered`].
pub mod primitive {
    use std::fs::File;
    use std::io;

    /// The full flush: [`File::sync_all`], which is `fcntl(F_FULLFSYNC)` on Apple
    /// targets and `fsync(2)` elsewhere.
    ///
    /// # Errors
    /// The flush's own error.
    pub fn full(file: &File) -> io::Result<()> {
        #[expect(
            clippy::disallowed_methods,
            reason = "the workspace's one full flush: aterm_durable::durable"
        )]
        file.sync_all()
    }

    /// The full flush of the data and the size: [`File::sync_data`], the same
    /// `fcntl(F_FULLFSYNC)` on Apple targets and `fdatasync(2)` on Linux.
    ///
    /// # Errors
    /// The flush's own error.
    pub fn full_data(file: &File) -> io::Result<()> {
        #[expect(
            clippy::disallowed_methods,
            reason = "the workspace's one full data flush: aterm_durable::durable_data"
        )]
        file.sync_data()
    }

    /// The ordering flush: `fcntl(F_BARRIERFSYNC)` on Apple targets, the full flush
    /// where the volume refuses the barrier or the platform has none.
    ///
    /// # Errors
    /// The barrier's own error when it failed, and the full flush's when the volume
    /// refused the barrier (astream-sys's `barrier_refused`, an Apple-only dependency,
    /// so a plain name here: this doc is built on every target). Off Apple targets,
    /// the full flush's.
    pub fn barrier(file: &File) -> io::Result<()> {
        #[cfg(target_vendor = "apple")]
        {
            barrier_or_full(file, astream_sys::barrier_sync, full)
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            full(file)
        }
    }

    /// `barrier`, answered by the full flush only when the volume REFUSED it. This is
    /// the one body [`barrier`] runs on Apple targets. Tests hand it a barrier and a
    /// full flush of their own to see which errors reach which; another runs
    /// [`barrier`] itself on `/dev/null`, whose barrier and full flush devfs both
    /// refuse, to pin the pair production passes in (a full flush that skipped the
    /// flush would answer `Ok` there).
    ///
    /// WHY ONLY A REFUSAL. astream-sys's contract ([`astream_sys::barrier_refused`]):
    /// `F_BARRIERFSYNC` answers `ENOTSUP`, `ENODEV`, `EINVAL` or `ENOTTY` when the
    /// filesystem or device cannot take a barrier at all. Nothing was ordered, and
    /// the full flush orders by persisting. Every other error (`EIO`, `ENOSPC`, ...) is
    /// a FAILED flush, and is returned as a failed `fsync` is. A full flush that then
    /// succeeds says nothing about what the failed one was to cover: the device has
    /// nothing dirty left to rewrite for those bytes, so it returns 0 whether or not
    /// they are on media. astream's own log refuses that mask for the same reason:
    /// its `issue_flush` (`astream-broker` `store.rs`) sends only a refused barrier
    /// to the full flush and returns every other barrier error, and claim
    /// `broker.ordered-tier` says why ("a later flush that succeeds says nothing about
    /// what the failed one was to cover"). What the log does next is its own: a failed
    /// flush inside a commit rolls the batch back, and poisons the open too only when
    /// earlier acks rode barriers that no full flush has covered. The rule this
    /// replaced, "any error gets the full flush, which reports a real I/O error just
    /// as well", was written against astream c6ee512, before astream 87140e5 added
    /// `barrier_refused`.
    ///
    /// WHY NO LATCH. astream's log serves Strict, full flushes only, for the rest of
    /// an open after one refusal (`BrokerLog::settle_issued` latches it through
    /// `latch_strict`; `issue_flush` itself only falls back for the one call), because
    /// a log is one file on one device and reports the tier it serves. This crate
    /// keeps no state: [`barrier`] takes whatever descriptor a site holds, on whatever
    /// volume. A process-wide latch would turn every later barrier on the APFS home
    /// into a full flush after one refusal on another volume (devfs, autofs). A latch
    /// per device would add an `fstat` and a
    /// lock to every barrier on the common path, to skip one `fcntl` on the volumes
    /// that refuse. So every call asks the barrier first.
    #[cfg(target_vendor = "apple")]
    pub(crate) fn barrier_or_full(
        file: &File,
        barrier: impl FnOnce(&File) -> io::Result<()>,
        full: impl FnOnce(&File) -> io::Result<()>,
    ) -> io::Result<()> {
        match barrier(file) {
            Ok(()) => Ok(()),
            Err(refused) if astream_sys::barrier_refused(&refused) => full(file),
            Err(failed) => Err(failed),
        }
    }

    /// A plain `fsync(2)`, retried on `EINTR` as std retries its `F_FULLFSYNC`: the
    /// data handed to the drive, which by its own man page promises neither
    /// durability nor order against a power loss. A crate's own unit tests take it in
    /// place of the full flush ([`seam::durable_fsync`](crate::seam::durable_fsync));
    /// nothing takes it in place of the barrier (see the [crate docs](crate)).
    ///
    /// # Errors
    /// The flush's own error.
    #[cfg(unix)]
    pub fn fsync(file: &File) -> io::Result<()> {
        use std::os::fd::AsRawFd as _;
        // SAFETY: `fsync(2)` takes a descriptor by value and dereferences no pointer;
        // for a descriptor that is not open it fails with `EBADF`.
        unsafe extern "C" {
            safe fn fsync(fd: std::ffi::c_int) -> std::ffi::c_int;
        }
        loop {
            if fsync(file.as_raw_fd()) == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

/// THE COUNTING SEAM (test and debug builds, and the `test-support` feature): which
/// flush each operation asked for, recorded per thread while a
/// [`Recording`](crate::seam::Recording) lives, so a test asserts the tier a site
/// chose without timing anything. A thread with no recording pays one thread-local
/// read per flush; a release build without `test-support` compiles none of this.
#[cfg(any(test, debug_assertions, feature = "test-support"))]
pub mod seam {
    use super::Flush;
    use std::cell::RefCell;
    #[cfg(unix)]
    use std::fs::File;
    #[cfg(unix)]
    use std::io;

    thread_local! {
        static LOG: RefCell<Option<Vec<Flush>>> = const { RefCell::new(None) };
    }

    /// A recording of every flush this thread asks for through this crate, from
    /// [`Recording::start`] until it drops. A recording started inside another hides
    /// the outer one until it drops, and the outer one then resumes without the
    /// inner's flushes. The guard must stay on its creating thread, so it is neither
    /// `Send` nor `Sync`.
    #[must_use = "the recording stops when it drops"]
    pub struct Recording {
        outer: Option<Vec<Flush>>,
        // The LOG slot belongs to the creating thread. The marker allocates nothing.
        _thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }

    impl Recording {
        /// Start recording this thread's flushes.
        pub fn start() -> Self {
            let outer = LOG.with(|log| log.replace(Some(Vec::new())));
            Self {
                outer,
                _thread: std::marker::PhantomData,
            }
        }

        /// The flushes recorded since the start or the last take, oldest first, and
        /// an empty log behind them.
        #[must_use]
        pub fn take(&self) -> Vec<Flush> {
            LOG.with(|log| match log.borrow_mut().as_mut() {
                Some(v) => std::mem::take(v),
                None => Vec::new(),
            })
        }
    }

    impl Drop for Recording {
        fn drop(&mut self) {
            let outer = self.outer.take();
            LOG.with(|log| *log.borrow_mut() = outer);
        }
    }

    pub(crate) fn note(flush: Flush) {
        LOG.with(|log| {
            if let Some(flushes) = log.borrow_mut().as_mut() {
                flushes.push(flush);
            }
        });
    }

    /// [`durable`](super::durable) as a crate's own unit tests take it on Apple
    /// targets: recorded as [`Flush::Durable`], then a plain `fsync(2)` instead of
    /// `F_FULLFSYNC`. The crates whose `durable` modules keep that test arm (each
    /// measured what the drive-cache flush cost its suite and its neighbours) call
    /// this, so the seam sees their durable sites too.
    ///
    /// # Errors
    /// The flush's own error.
    #[cfg(unix)]
    pub fn durable_fsync(file: &File) -> io::Result<()> {
        note(Flush::Durable);
        super::primitive::fsync(file)
    }
}

#[cfg(test)]
mod tests {
    use super::{Flush, durable, durable_data, ordered, primitive, seam};
    #[cfg(unix)]
    use super::{durable_dir, ordered_dir};
    use std::io::Write as _;

    fn scratch() -> aterm_tempfile::TempDir {
        aterm_tempfile::Builder::new()
            .prefix("aterm-durable-")
            .tempdir()
            .expect("a scratch directory")
    }

    #[test]
    fn flush_device_pair_codec_round_trips_and_rejects_mismatched_tags() {
        let same = Flush::encode_device_pair(42, 42);
        assert_eq!(
            Flush::decode_device_pair(&same),
            Some((Flush::Ordered, 42, 42))
        );
        assert!(Flush::Ordered.is_ordered() && !Flush::Ordered.is_durable());
        let diff = Flush::encode_device_pair(42, 99);
        assert_eq!(
            Flush::decode_device_pair(&diff),
            Some((Flush::Durable, 42, 99))
        );
        assert!(Flush::Durable.is_durable() && !Flush::Durable.is_ordered());
        let mut bad = same;
        bad[0] = Flush::Durable.as_u8();
        assert_eq!(Flush::decode_device_pair(&bad), None);
        bad[0] = 0;
        assert_eq!(Flush::decode_device_pair(&bad), None);
    }

    #[test]
    fn the_seam_names_the_flush_each_operation_asked_for() {
        let dir = scratch();
        let mut file = std::fs::File::create(dir.path().join("f")).unwrap();
        file.write_all(b"bytes").unwrap();
        let recording = seam::Recording::start();
        ordered(&file).unwrap();
        durable(&file).unwrap();
        #[cfg(unix)]
        {
            ordered_dir(dir.path()).unwrap();
            durable_dir(dir.path()).unwrap();
        }
        Flush::Ordered.file(&file).unwrap();
        #[cfg(unix)]
        Flush::Durable.dir(dir.path()).unwrap();
        durable_data(&file).unwrap();
        #[cfg(unix)]
        let expected = [
            Flush::Ordered,
            Flush::Durable,
            Flush::Ordered,
            Flush::Durable,
            Flush::Ordered,
            Flush::Durable,
            Flush::Durable,
        ];
        #[cfg(not(unix))]
        let expected = [
            Flush::Ordered,
            Flush::Durable,
            Flush::Ordered,
            Flush::Durable,
        ];
        assert_eq!(recording.take(), expected);
        assert_eq!(recording.take(), [], "a take empties the log");
    }

    #[test]
    fn nothing_is_recorded_without_a_recording_and_an_inner_one_hides_the_outer() {
        let dir = scratch();
        let file = std::fs::File::create(dir.path().join("f")).unwrap();
        ordered(&file).unwrap();
        let outer = seam::Recording::start();
        assert_eq!(
            outer.take(),
            [],
            "the flush before the start is not recorded"
        );
        durable(&file).unwrap();
        {
            let inner = seam::Recording::start();
            ordered(&file).unwrap();
            assert_eq!(inner.take(), [Flush::Ordered]);
        }
        ordered(&file).unwrap();
        assert_eq!(outer.take(), [Flush::Durable, Flush::Ordered]);
    }

    #[test]
    fn another_threads_flushes_are_not_this_threads() {
        let dir = scratch();
        let path = dir.path().join("f");
        std::fs::write(&path, b"x").unwrap();
        let recording = seam::Recording::start();
        std::thread::spawn(move || {
            // Open for writing, as a writer flushes: Windows's `FlushFileBuffers`
            // refuses a read-only handle (access denied), where `fsync(2)` takes one.
            let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            durable(&file).unwrap();
        })
        .join()
        .unwrap();
        assert_eq!(recording.take(), []);
    }

    /// The real primitives on a regular file, and on Unix also on a directory of
    /// the test volume: the barrier and the full flush.
    #[test]
    fn the_real_barrier_and_full_flush_take_a_file_and_a_directory() {
        let dir = scratch();
        let mut file = std::fs::File::create(dir.path().join("f")).unwrap();
        file.write_all(b"ordered").unwrap();
        primitive::barrier(&file).expect("the barrier on a regular file");
        primitive::full(&file).expect("the full flush on a regular file");
        #[cfg(unix)]
        {
            let handle = std::fs::File::open(dir.path()).unwrap();
            primitive::barrier(&handle).expect("the barrier on a directory");
            primitive::full(&handle).expect("the full flush on a directory");
        }
    }

    /// The barrier itself, with no fallback behind it, on a regular file and on a directory
    /// of the test volume (Apple). [`primitive::barrier`] answers a refused barrier with the
    /// full flush, so its own test passes either way; this one says whether the volume
    /// takes `F_BARRIERFSYNC` at all, for the directory entries most ordered sites flush.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn this_volume_takes_the_barrier_on_a_file_and_a_directory() {
        let dir = scratch();
        let mut file = std::fs::File::create(dir.path().join("f")).unwrap();
        file.write_all(b"ordered").unwrap();
        if let Err(error) = astream_sys::barrier_sync(&file) {
            panic!("this volume refused F_BARRIERFSYNC on a regular file: {error}");
        }
        let handle = std::fs::File::open(dir.path()).unwrap();
        if let Err(error) = astream_sys::barrier_sync(&handle) {
            panic!(
                "this volume refused F_BARRIERFSYNC on a directory, so every ordered \
                 directory flush here is a full one: {error}"
            );
        }
    }

    /// What [`primitive::barrier_or_full`] did with one injected barrier answer: its
    /// result, and how many times it asked the barrier and the full flush. The full
    /// flush it is handed is the real one ([`primitive::full`]) unless `full_fails`,
    /// which stands in a full flush that answers `EIO`.
    #[cfg(target_vendor = "apple")]
    fn barrier_answering(
        answer: std::io::Error,
        full_fails: bool,
    ) -> (std::io::Result<()>, usize, usize) {
        let dir = scratch();
        let mut file = std::fs::File::create(dir.path().join("f")).unwrap();
        file.write_all(b"ordered").unwrap();
        let (mut barriers, mut fulls) = (0, 0);
        let result = primitive::barrier_or_full(
            &file,
            |_| {
                barriers += 1;
                Err(answer)
            },
            |f| {
                fulls += 1;
                if full_fails {
                    Err(std::io::Error::from_raw_os_error(5))
                } else {
                    primitive::full(f)
                }
            },
        );
        (result, barriers, fulls)
    }

    /// The four errnos astream-sys reads as a REFUSED barrier (`ENODEV`, `EINVAL`,
    /// `ENOTTY`, `ENOTSUP`) promise no ordering, so each gets the full flush, whose
    /// own answer is the result. A barrier that succeeds gets no full flush.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_refused_barrier_is_answered_by_the_full_flush() {
        for errno in [19, 22, 25, 45] {
            let (result, barriers, fulls) =
                barrier_answering(std::io::Error::from_raw_os_error(errno), false);
            result.unwrap_or_else(|e| panic!("errno {errno}: the full flush failed: {e}"));
            assert_eq!((barriers, fulls), (1, 1), "errno {errno}");
        }
        let (result, _, fulls) = barrier_answering(std::io::Error::from_raw_os_error(45), true);
        assert_eq!(
            result.map_err(|e| e.raw_os_error()),
            Err(Some(5)),
            "a refused barrier answers with the full flush's own error"
        );
        assert_eq!(fulls, 1);

        let dir = scratch();
        let file = std::fs::File::create(dir.path().join("f")).unwrap();
        let mut fulls = 0;
        primitive::barrier_or_full(
            &file,
            |_| Ok(()),
            |_| {
                fulls += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(fulls, 0, "a barrier that succeeds owes no full flush");
    }

    /// THE NEGATIVE CONTROL of the test above: the same harness, with a barrier that
    /// FAILED rather than was refused. Its error is returned, and the full flush is
    /// never called, since one that succeeds afterwards would answer `Ok` for bytes
    /// the failed flush may have lost. An error that carries no errno is no refusal
    /// either: astream-sys reads a refusal only from the OS's own number.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_failed_barrier_is_returned_and_never_covered_by_a_full_flush() {
        for errno in [5, 28, 9, 1] {
            // EIO, ENOSPC, EBADF, EPERM.
            let (result, barriers, fulls) =
                barrier_answering(std::io::Error::from_raw_os_error(errno), false);
            assert_eq!(
                result.map_err(|e| e.raw_os_error()),
                Err(Some(errno)),
                "errno {errno} is the barrier's own failure"
            );
            assert_eq!((barriers, fulls), (1, 0), "errno {errno}");
        }
        for kind in [std::io::ErrorKind::Unsupported, std::io::ErrorKind::Other] {
            let (result, _, fulls) = barrier_answering(std::io::Error::from(kind), false);
            assert_eq!(result.map_err(|e| e.kind()), Err(kind));
            assert_eq!(fulls, 0, "{kind:?} carries no errno, so it is no refusal");
        }
    }

    /// The pair [`primitive::barrier`] itself hands [`primitive::barrier_or_full`] on
    /// Apple: astream-sys's barrier and this crate's real full flush. The two tests
    /// above inject both, so they pin the rule but not what production passes it: a
    /// full-flush argument that answered `Ok` without flushing would pass them, and the
    /// shipped barrier would then answer `Ok` for a refused barrier with nothing on
    /// media. `/dev/null` is a descriptor whose answers the OS fixes: devfs refuses
    /// `F_BARRIERFSYNC` on it and fails `F_FULLFSYNC` too, both with `ENODEV` (measured
    /// on macOS 27.0.1), so the shipped barrier must answer with the full flush's own
    /// error. Both answers are asserted first, so a macOS that changed either fails
    /// this test by name instead of letting it pass with nothing checked.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn the_shipped_barrier_answers_a_refusal_with_the_real_full_flush() {
        let null = std::fs::File::open("/dev/null").expect("/dev/null opens");
        let barrier = astream_sys::barrier_sync(&null);
        assert!(
            barrier.as_ref().is_err_and(astream_sys::barrier_refused),
            "devfs no longer refuses F_BARRIERFSYNC on /dev/null ({barrier:?}): this test \
             needs another descriptor whose barrier is refused"
        );
        let full = primitive::full(&null).map_err(|e| e.raw_os_error());
        assert!(
            full.is_err(),
            "devfs's full flush on /dev/null succeeded, so a shipped barrier that skipped \
             it would pass here: this test needs a descriptor whose full flush fails"
        );
        assert_eq!(
            primitive::barrier(&null).map_err(|e| e.raw_os_error()),
            full,
            "a refused barrier answers with the real full flush's own result"
        );
    }

    #[test]
    fn a_later_write_on_the_same_device_is_ordered_and_an_unreadable_one_is_durable() {
        let dir = scratch();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"a").unwrap();
        std::fs::create_dir(&b).unwrap();
        #[cfg(unix)]
        let same_device = Flush::Ordered;
        #[cfg(not(unix))]
        let same_device = Flush::Durable;
        assert_eq!(Flush::ordered_if_same_device(&a, &b), same_device);
        #[cfg(unix)]
        assert_eq!(
            Flush::ordered_if_same_device(&a, dir.path()),
            Flush::Ordered
        );
        assert_eq!(
            Flush::ordered_if_same_device(&a, &dir.path().join("absent")),
            Flush::Durable,
            "a path that cannot be read is another device for all this knows"
        );
    }

    /// Two volumes, where this host has them: `/dev` is its own filesystem on Apple
    /// and Linux, so a write there is never ordered by a barrier on the temp volume.
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    #[test]
    fn a_later_write_on_another_device_is_durable() {
        let dir = scratch();
        assert_eq!(
            Flush::ordered_if_same_device(dir.path(), std::path::Path::new("/dev")),
            Flush::Durable
        );
    }
}
