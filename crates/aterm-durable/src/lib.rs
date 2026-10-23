// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! File and directory flush primitives: [`durable`] ([`File::sync_all`], [`durable_data`],
//! [`durable_dir`], [`Flush::Durable`]) and [`ordered`] ([`ordered_dir`],
//! [`Flush::ordered_if_same_device`]), with [`seam::Recording`] for test/debug inspection.
//!
//! Every application flushes in full, debug builds included (astream 46e7c1b's ruling):
//! only a test binary that asks ([`plain_durable_flushes_in_tests!`]), and every process
//! it starts, astream's broker included, takes a plain `fsync(2)` for its durable flushes
//! in a debug build on Apple targets (`astream_sys::TEST_PLAIN_FSYNC_ENV`).

use std::fs::File;
use std::io;
use std::path::Path;

/// Which flush finished a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flush {
    /// On stable media when the flush returns ([`durable`]).
    Durable,
    /// Flush this descriptor and order the flushed I/O ahead of later writes on its device.
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

    /// Encode a 17-byte device-pair decision record.
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

    /// Decode a 17-byte device-pair decision record `(Flush, written_dev, later_dev)`.
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

    /// [`Flush::Ordered`] when `written` and `later` are on one device, else [`Flush::Durable`].
    #[must_use]
    pub fn ordered_if_same_device(written: &Path, later: &Path) -> Self {
        if same_device(written, later) {
            Self::Ordered
        } else {
            Self::Durable
        }
    }
}

/// Make everything written to `file` durable.
///
/// # Errors
/// The flush's own error.
pub fn durable(file: &File) -> io::Result<()> {
    #[cfg(any(test, debug_assertions, feature = "test-support"))]
    seam::note(Flush::Durable);
    full_or_fsync(file, primitive::full)
}

/// [`durable`] for `file`'s data and size metadata, without its timestamps.
///
/// # Errors
/// The flush's own error.
pub fn durable_data(file: &File) -> io::Result<()> {
    #[cfg(any(test, debug_assertions, feature = "test-support"))]
    seam::note(Flush::Durable);
    full_or_fsync(file, primitive::full_data)
}

/// The durable flush `full` ([`primitive::full`] or [`primitive::full_data`]), or a plain
/// `fsync(2)` in a test binary on Apple targets that asked ([`plain_durable_flushes_in_tests!`]).
#[inline]
fn full_or_fsync(file: &File, full: fn(&File) -> io::Result<()>) -> io::Result<()> {
    #[cfg(all(any(test, debug_assertions), target_vendor = "apple"))]
    if cfg!(test) || astream_sys::test_plain_fsync() {
        return primitive::fsync(file);
    }
    full(file)
}

/// Make the durable flushes of this test binary, and of every process it starts, plain
/// `fsync(2)`s in a debug build on Apple targets: a crate's root says
/// `#[cfg(test)] aterm_durable::plain_durable_flushes_in_tests!();`, which expands only
/// under `cfg(test)` to a load-time hook that marks the process tree
/// (`astream_sys::TEST_PLAIN_FSYNC_ENV`). Applications keep the full flush.
#[macro_export]
macro_rules! plain_durable_flushes_in_tests {
    () => {
        #[cfg(all(test, target_vendor = "apple"))]
        const _: () = {
            extern "C" fn plain_durable_flushes() {
                // SAFETY: a load-time hook runs before `main`, so before any other thread.
                unsafe { $crate::seam::plain_durable_flushes() }
            }
            #[used]
            #[unsafe(link_section = "__DATA,__mod_init_func,mod_init_funcs")]
            static PLAIN_DURABLE_FLUSHES: extern "C" fn() = plain_durable_flushes;
        };
    };
}

/// Order everything written to `file`.
///
/// # Errors
/// The flush's own error (the full flush's, when the barrier was refused).
pub fn ordered(file: &File) -> io::Result<()> {
    #[cfg(any(test, debug_assertions, feature = "test-support"))]
    seam::note(Flush::Ordered);
    primitive::barrier(file)
}

/// [`durable`] for the directory `dir`.
///
/// # Errors
/// The directory could not be opened, or the flush's own error.
pub fn durable_dir(dir: &Path) -> io::Result<()> {
    match File::open(dir) {
        Ok(f) => durable(&f),
        Err(e) => Err(e),
    }
}

/// [`ordered`] for the directory `dir`.
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

/// The flushes themselves, unrecorded.
pub mod primitive {
    use std::fs::File;
    use std::io;

    /// The full flush: [`File::sync_all`].
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

    /// The full flush of the data and the size.
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

    /// The ordering flush: `fcntl(F_BARRIERFSYNC)` on Apple targets.
    ///
    /// # Errors
    /// The barrier's own error when it failed.
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

    /// `barrier`, answered by the full flush only when the volume refused it.
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

    /// A plain `fsync(2)`, retried on `EINTR` as std retries its `F_FULLFSYNC`.
    ///
    /// # Errors
    /// The flush's own error.
    #[cfg(unix)]
    pub fn fsync(file: &File) -> io::Result<()> {
        use std::os::fd::AsRawFd as _;
        // SAFETY: `fsync(2)` takes a descriptor by value and dereferences no pointer.
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

/// The counting seam (test and debug builds, and the `test-support` feature).
#[cfg(any(test, debug_assertions, feature = "test-support"))]
pub mod seam {
    use super::Flush;
    use std::cell::RefCell;

    thread_local! {
        static LOG: RefCell<Option<Vec<Flush>>> = const { RefCell::new(None) };
    }

    /// A recording of every flush this thread asks for through this crate.
    #[must_use = "the recording stops when it drops"]
    pub struct Recording {
        outer: Option<Vec<Flush>>,
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

        /// The flushes recorded since the start or the last take, oldest first.
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

    /// Mark this process, and every process it starts, as a test's
    /// (`astream_sys::TEST_PLAIN_FSYNC_ENV`): in a debug build on Apple targets their
    /// durable flushes, and the broker's, are plain `fsync(2)`s. Only
    /// [`plain_durable_flushes_in_tests!`](crate::plain_durable_flushes_in_tests)'s
    /// load-time hook calls it.
    ///
    /// # Safety
    /// No other thread may be running: it sets an environment variable.
    pub unsafe fn plain_durable_flushes() {
        // SAFETY: the caller runs before any other thread exists.
        #[cfg(target_vendor = "apple")]
        unsafe {
            std::env::set_var(astream_sys::TEST_PLAIN_FSYNC_ENV, "1");
        }
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
    /// of the test volume (Apple).
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
    /// result, and how many times it asked the barrier and the full flush.
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
    /// own answer is the result.
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
    /// FAILED rather than was refused.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_failed_barrier_is_returned_and_never_covered_by_a_full_flush() {
        for errno in [5, 28, 9, 1] {
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
    /// Apple: astream-sys's barrier and this crate's real full flush.
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
