// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! File and directory flush primitives.
//!
//! Every flush this crate makes is astream-sys's, so `astream_sys::flush_tally` counts each
//! one, by the call it is: [`primitive::full`] and [`primitive::full_data`] are `full_sync` and
//! `full_sync_data` (a `full` each), [`primitive::barrier`] is `barrier_sync` on Apple targets
//! (a `barrier`, and a `full` more when the volume refuses it) and `full_sync` elsewhere, and
//! `primitive::fsync` (Unix) is `plain_fsync` (a `plain`), on a file or a bare descriptor: aterm's
//! one plain flush, since `clippy.toml` refuses `libc::fsync` and `libc::fdatasync`. The `seam`
//! records what each write asked for ([`Flush`]) and counts the plain flushes beside it; the
//! tally counts the calls that answered them.

use std::fs::File;
use std::io;
use std::path::Path;

/// Which of this crate's flushes `astream_sys::flush_tally` counts, as one word a reading of
/// that tally names beside its counts (aterm-link's wait tally does): `all`, on every target,
/// since every flush here goes through astream-sys (the crate's doc says which call each is).
/// A flush that called the OS here instead would escape the tally and change this word.
pub const ASTREAM_COUNTS: &str = "all";

/// Which flush finished a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flush {
    /// On stable media when the flush returns ([`durable`]).
    Durable,
    /// Flush this descriptor and order the flushed I/O ahead of later writes on its device.
    Ordered,
}

impl Flush {
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

    /// Finish a write to `file` with this flush.
    /// # Errors
    /// The flush's own error.
    #[inline]
    pub fn file(self, file: &File) -> io::Result<()> {
        match self {
            Self::Durable => durable(file),
            Self::Ordered => ordered(file),
        }
    }

    /// Finish a write, naming the failed system call while retaining its error as a source.
    /// # Errors
    /// The flush's error, with the operation that failed.
    pub fn file_diagnosed(self, file: &File) -> io::Result<()> {
        #[cfg(any(test, debug_assertions, feature = "test-support"))]
        seam::note(self);
        let full = |file: &File| {
            let operation = if cfg!(target_vendor = "apple") && !durable_flushes_are_plain() {
                "F_FULLFSYNC"
            } else if cfg!(windows) {
                "FlushFileBuffers"
            } else {
                "fsync"
            };
            primitive::full(file).map_err(|error| operation_error(operation, error))
        };
        #[cfg(target_vendor = "apple")]
        if self.is_ordered() {
            return match astream_sys::barrier_sync(file) {
                Ok(()) => Ok(()),
                Err(error) if astream_sys::barrier_refused(&error) => full(file),
                Err(error) => Err(operation_error("F_BARRIERFSYNC", error)),
            };
        }
        full(file)
    }

    /// Finish a directory-entry change in `dir` with this flush. Windows has no flush of a
    /// directory (one opens as no file to flush: `Access is denied`, os error 5) and NTFS
    /// commits a directory's entries through its own log, so there the flush is only noted.
    ///
    /// # Errors
    /// The directory could not be opened, or the flush's own error.
    #[inline]
    pub fn dir(self, dir: &Path) -> io::Result<()> {
        self.dir_using(dir, |file| self.file(file), |error| error)
    }

    /// [`Self::dir`] with the failed open or flush operation retained in its error.
    /// # Errors
    /// The directory open or flush failed.
    pub fn dir_diagnosed(self, dir: &Path) -> io::Result<()> {
        self.dir_using(
            dir,
            |file| self.file_diagnosed(file),
            |error| operation_error("open directory", error),
        )
    }

    fn dir_using(
        self,
        dir: &Path,
        flush: impl FnOnce(&File) -> io::Result<()>,
        open_error: impl FnOnce(io::Error) -> io::Error,
    ) -> io::Result<()> {
        #[cfg(any(test, debug_assertions, feature = "test-support"))]
        if let Some(failed) = seam::dir_failure() {
            seam::note(self);
            return Err(failed);
        }
        if cfg!(windows) {
            #[cfg(any(test, debug_assertions, feature = "test-support"))]
            seam::note(self);
            return Ok(());
        }
        #[cfg(unix)]
        let opened = {
            use std::os::unix::fs::OpenOptionsExt as _;
            #[cfg(target_vendor = "apple")]
            const NONBLOCK_FLAG: i32 = 0x0004;
            #[cfg(not(target_vendor = "apple"))]
            const NONBLOCK_FLAG: i32 = 0o4000;
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(NONBLOCK_FLAG)
                .open(dir)
                .map_err(open_error)?
        };
        #[cfg(not(unix))]
        let opened = File::open(dir).map_err(open_error)?;
        flush(&opened)
    }

    /// [`Flush::dir`] for a writer that goes on where the directory's entries cannot be
    /// flushed here ([`DirFlushed::Unavailable`], astream's `dir_flush_unavailable`). Any
    /// other failure, `EIO` among them, is still an error: the entry may not survive a power
    /// loss. A writer whose promise rests on the entry's flush takes [`Flush::dir`] instead.
    ///
    /// # Errors
    /// The directory's open or flush failed other than as unavailable.
    #[inline]
    pub fn dir_best_effort(self, dir: &Path) -> io::Result<DirFlushed> {
        DirFlushed::classify(self.dir(dir))
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

#[derive(Debug)]
struct OperationError {
    operation: &'static str,
    error: io::Error,
}

fn operation_error(operation: &'static str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), OperationError { operation, error })
}

impl std::fmt::Display for OperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.operation, self.error)
    }
}

impl std::error::Error for OperationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// What a best-effort flush of a directory did ([`Flush::dir_best_effort`]).
#[derive(Debug)]
#[must_use = "a directory whose entries were not flushed is the caller's to note or to accept"]
pub enum DirFlushed {
    /// The flush returned: the directory's entries are on stable media, or ordered, as the
    /// flush promises.
    Flushed,
    /// The entries cannot be flushed here (`astream_sys::dir_flush_unavailable`): a volume
    /// that refuses to flush a directory, a directory this process may write but not open,
    /// or one that is gone. The error says which.
    Unavailable(io::Error),
}

impl DirFlushed {
    /// Classify a directory flush's outcome by astream's one rule
    /// (`astream_sys::dir_flush_unavailable`), for a writer that opens and flushes the
    /// directory itself.
    ///
    /// # Errors
    /// `flushed`'s error when it is a failed flush rather than an unavailable one.
    pub fn classify(flushed: io::Result<()>) -> io::Result<Self> {
        match flushed {
            Ok(()) => Ok(Self::Flushed),
            Err(e) if astream_sys::dir_flush_unavailable(&e) => Ok(Self::Unavailable(e)),
            Err(e) => Err(e),
        }
    }

    /// Whether the flush returned.
    #[must_use]
    pub const fn is_flushed(&self) -> bool {
        matches!(self, Self::Flushed)
    }
}

/// Accept a refused file flush for a writer that can proceed without its durability promise.
/// Pass only the flush's result: opening or writing the file must still succeed. A refusal
/// is classified by `astream_sys::file_flush_refused`; accepting it does not establish that
/// the write will survive power loss. Writers that require a successful flush use
/// [`durable`], [`ordered`] or [`Flush::file`] without this opt-in.
///
/// # Errors
/// The original flush error unless it is a refusal. I/O failures, exhausted space, denied
/// permission and missing files remain errors.
pub fn refusal_accepted(flushed: io::Result<()>) -> io::Result<()> {
    match flushed {
        Err(error) if astream_sys::file_flush_refused(&error) => Ok(()),
        result => result,
    }
}

/// Make everything written to `file` durable.
/// # Errors
/// The flush's own error.
pub fn durable(file: &File) -> io::Result<()> {
    #[cfg(any(test, debug_assertions, feature = "test-support"))]
    seam::note(Flush::Durable);
    primitive::full(file)
}

/// [`durable`] for `file`'s data and size metadata, without its timestamps.
/// # Errors
/// The flush's own error.
pub fn durable_data(file: &File) -> io::Result<()> {
    #[cfg(any(test, debug_assertions, feature = "test-support"))]
    seam::note(Flush::Durable);
    primitive::full_data(file)
}

/// Whether this process's durable flushes are plain `fsync(2)`s, not the full flushes
/// (`F_FULLFSYNC`) aterm ships: a test binary that asked, and every process it starts, in a
/// debug build on Apple targets ([`plain_durable_flushes_in_tests!`]), where astream-sys's
/// `full_sync` and `full_sync_data` take one (`astream_sys::test_plain_fsync`, which decides it
/// for them and for this). Always `false` in a release build and off Apple targets.
#[must_use]
pub fn durable_flushes_are_plain() -> bool {
    astream_sys::test_plain_fsync()
}

/// What a line that counts or times durable flushes ends with where they are plain
/// `fsync(2)`s ([`durable_flushes_note`]).
pub const PLAIN_DURABLE_FLUSHES: &str =
    " (each full flush a plain fsync(2): a test's process tree)";

/// [`PLAIN_DURABLE_FLUSHES`] where [`durable_flushes_are_plain`], else nothing: the end of a
/// line that counts or times durable flushes, so a test's plain `fsync(2)`s never read as
/// the full flushes aterm ships.
#[must_use]
pub fn durable_flushes_note() -> &'static str {
    if durable_flushes_are_plain() {
        PLAIN_DURABLE_FLUSHES
    } else {
        ""
    }
}

/// Make the durable flushes of this test binary, and of every process it starts, plain
/// `fsync(2)`s in a debug build on Apple targets.
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

/// Make the durable flushes of this test binary, and of every process it starts, the full
/// flushes aterm ships, whatever mark it inherited from a marked test that started it
/// ([`plain_durable_flushes_in_tests!`] is the other way): for a test binary that judges
/// those flushes themselves, as aterm-durable's own do.
#[macro_export]
macro_rules! shipped_durable_flushes_in_tests {
    () => {
        #[cfg(all(test, target_vendor = "apple"))]
        const _: () = {
            extern "C" fn shipped_durable_flushes() {
                // SAFETY: a load-time hook runs before `main`, so before any other thread
                // and before astream-sys reads the mark, once, at the first flush.
                unsafe { $crate::seam::shipped_durable_flushes() }
            }
            #[used]
            #[unsafe(link_section = "__DATA,__mod_init_func,mod_init_funcs")]
            static SHIPPED_DURABLE_FLUSHES: extern "C" fn() = shipped_durable_flushes;
        };
    };
}

// This crate's own tests judge the flushes aterm ships: the real F_FULLFSYNC and its
// refusal on /dev/null, which a test mark's plain fsync(2) would hide.
shipped_durable_flushes_in_tests!();

/// Order everything written to `file`.
/// # Errors
/// The flush's own error (the full flush's, when the barrier was refused).
pub fn ordered(file: &File) -> io::Result<()> {
    #[cfg(any(test, debug_assertions, feature = "test-support"))]
    seam::note(Flush::Ordered);
    primitive::barrier(file)
}

/// [`ordered`], or a plain `fsync(2)` where the volume refuses its full flush too (a network
/// home whose server takes no `F_FULLFSYNC`), as SQLite does there. That `fsync` sends the bytes
/// to the volume but orders nothing beyond what it promises, so this finishes a write whose loss
/// costs a repeat (a page marker), never one a Strict acknowledgement rests on.
/// # Errors
/// The flush's own error: [`ordered`]'s when it failed rather than was refused, else `fsync`'s.
pub fn ordered_or_fsync(file: &File) -> io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        primitive::refused_or(ordered(file), || primitive::fsync(file))
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        ordered(file)
    }
}

/// [`durable`] for the directory `dir`.
/// # Errors
/// The directory could not be opened, or the flush's own error.
pub fn durable_dir(dir: &Path) -> io::Result<()> {
    Flush::Durable.dir(dir)
}

/// [`ordered`] for the directory `dir`.
/// # Errors
/// The directory could not be opened, or the flush's own error.
pub fn ordered_dir(dir: &Path) -> io::Result<()> {
    Flush::Ordered.dir(dir)
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
    false
}

/// The flushes themselves, unrecorded as a [`Flush`] (the seam counts only [`primitive::fsync`]'s,
/// apart from them).
pub mod primitive {
    use std::fs::File;
    use std::io;

    /// The full flush: `astream_sys::full_sync` ([`File::sync_all`], `F_FULLFSYNC` on Apple
    /// targets; a plain `fsync(2)` in a test's process tree there), counted `full` by
    /// `astream_sys::flush_tally`.
    /// # Errors
    /// The flush's own error.
    pub fn full(file: &File) -> io::Result<()> {
        astream_sys::full_sync(file)
    }

    /// The full flush of the data and the size: `astream_sys::full_sync_data`
    /// ([`File::sync_data`], as [`full`] says), counted `full`.
    /// # Errors
    /// The flush's own error.
    pub fn full_data(file: &File) -> io::Result<()> {
        astream_sys::full_sync_data(file)
    }

    /// The ordering flush: `astream_sys::barrier_sync` (`fcntl(F_BARRIERFSYNC)`) on Apple
    /// targets, counted `barrier`, and [`full`] after it when the volume refused it; [`full`]
    /// elsewhere.
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

    /// `flushed`, answered by `fallback` only when the volume refused the flush.
    #[cfg(target_vendor = "apple")]
    pub(crate) fn refused_or(
        flushed: io::Result<()>,
        fallback: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        match flushed {
            Err(refused) if astream_sys::barrier_refused(&refused) => fallback(),
            other => other,
        }
    }

    /// A plain `fsync(2)` of an open file or directory, a `&File` or a bare descriptor (an
    /// `OwnedFd`, a `BorrowedFd`: the handle-relative trees' folders): `astream_sys::plain_fsync`,
    /// retried on `EINTR` there, counted `plain` by `astream_sys::flush_tally` and timed by
    /// `flush_times`. aterm's one plain flush: `clippy.toml` refuses `libc::fsync` and
    /// `libc::fdatasync`, whose flushes no tally counts. The seam counts each call on its
    /// thread ([`super::seam::Recording::take_plain`]), the one flush of this module it sees,
    /// since a caller takes it from here by name.
    /// # Errors
    /// The flush's own error.
    #[cfg(unix)]
    pub fn fsync(fd: impl std::os::fd::AsFd) -> io::Result<()> {
        #[cfg(any(test, debug_assertions, feature = "test-support"))]
        super::seam::note_plain();
        astream_sys::plain_fsync(fd)
    }
}

/// The counting seam (test and debug builds, and the `test-support` feature).
#[cfg(any(test, debug_assertions, feature = "test-support"))]
pub mod seam {
    use super::Flush;
    use std::cell::RefCell;

    /// What one recording has seen so far on its thread.
    #[derive(Default)]
    struct Log {
        /// Each [`Flush`] asked for, oldest first.
        flushes: Vec<Flush>,
        /// The plain `fsync(2)`s ([`crate::primitive::fsync`]), which no [`Flush`] names.
        plain: u32,
    }

    thread_local! {
        static LOG: RefCell<Option<Log>> = const { RefCell::new(None) };
    }

    /// A recording of every flush this thread asks for through this crate.
    #[must_use = "the recording stops when it drops"]
    pub struct Recording {
        outer: Option<Log>,
        _thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }

    impl Recording {
        /// Start recording this thread's flushes.
        pub fn start() -> Self {
            let outer = LOG.with(|log| log.replace(Some(Log::default())));
            Self {
                outer,
                _thread: std::marker::PhantomData,
            }
        }

        /// The flushes recorded since the start or the last take, oldest first. A plain
        /// `fsync(2)` is none of them: [`Recording::take_plain`] counts those.
        #[must_use]
        pub fn take(&self) -> Vec<Flush> {
            LOG.with(|log| match log.borrow_mut().as_mut() {
                Some(log) => std::mem::take(&mut log.flushes),
                None => Vec::new(),
            })
        }

        /// How many plain `fsync(2)`s ([`crate::primitive::fsync`]) this thread issued since
        /// the start or the last call. Each is one `plain` in `astream_sys::flush_tally`,
        /// which counts the whole process's: this is the count a test reads its own sites'
        /// by.
        #[must_use]
        pub fn take_plain(&self) -> u32 {
            LOG.with(|log| match log.borrow_mut().as_mut() {
                Some(log) => std::mem::take(&mut log.plain),
                None => 0,
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
            if let Some(log) = log.borrow_mut().as_mut() {
                log.flushes.push(flush);
            }
        });
    }

    /// One plain `fsync(2)` ([`crate::primitive::fsync`], Unix: elsewhere none is made).
    #[cfg(unix)]
    pub(crate) fn note_plain() {
        LOG.with(|log| {
            if let Some(log) = log.borrow_mut().as_mut() {
                log.plain += 1;
            }
        });
    }

    /// The number this platform gives a volume's I/O error, a flush that failed, for
    /// [`FailingDirs::with`]: `EIO` (5) on Unix, and `ERROR_IO_DEVICE` (1117) on Windows,
    /// where 5 is `ERROR_ACCESS_DENIED`, which [`astream_sys::dir_flush_unavailable`] reads as
    /// a folder this process may not open rather than as a failed flush.
    pub const EIO: i32 = if cfg!(windows) { 1117 } else { 5 };

    /// The number this platform gives a volume's refusal to flush, which
    /// [`astream_sys::dir_flush_unavailable`] reads as unavailable, for
    /// [`FailingDirs::with`]: `ENOTSUP` on Unix (Darwin's 45, Linux's 95), and
    /// `ERROR_CALL_NOT_IMPLEMENTED` (120, std's `Unsupported`) on Windows.
    pub const ENOTSUP: i32 = if cfg!(target_vendor = "apple") {
        45
    } else if cfg!(windows) {
        120
    } else {
        95
    };

    #[derive(Clone, Copy)]
    enum DirFailure {
        Errno(i32),
        Kind(std::io::ErrorKind),
    }

    /// Directory flushes to let through before the injected error starts.
    type Failing = Option<(u32, DirFailure)>;

    thread_local! {
        static FAILING_DIRS: std::cell::Cell<Failing> = const { std::cell::Cell::new(None) };
    }

    /// Fail directory flushes on this thread before opening a directory, recording the
    /// requested strength. Nested guards restore the outer fault when they drop.
    #[must_use = "the failure stops when it drops"]
    pub struct FailingDirs {
        outer: Failing,
        _thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }

    impl FailingDirs {
        /// Fail this thread's directory flushes with `errno`, a number as this platform gives
        /// it ([`EIO`], [`ENOTSUP`]): a POSIX errno is another error on Windows.
        pub fn with(errno: i32) -> Self {
            Self::after(0, errno)
        }

        /// Let `real` directory flushes through, then fail each with `errno`.
        pub fn after(real: u32, errno: i32) -> Self {
            Self::install(real, DirFailure::Errno(errno))
        }

        /// Fail with a portable kind, without assuming another platform's errno numbers.
        pub fn with_kind(kind: std::io::ErrorKind) -> Self {
            Self::install(0, DirFailure::Kind(kind))
        }

        fn install(real: u32, failure: DirFailure) -> Self {
            Self {
                outer: FAILING_DIRS.with(|failing| failing.replace(Some((real, failure)))),
                _thread: std::marker::PhantomData,
            }
        }
    }

    impl Drop for FailingDirs {
        fn drop(&mut self) {
            FAILING_DIRS.with(|failing| failing.set(self.outer.take()));
        }
    }

    pub(crate) fn dir_failure() -> Option<std::io::Error> {
        FAILING_DIRS.with(|failing| match failing.get()? {
            (0, DirFailure::Errno(errno)) => Some(std::io::Error::from_raw_os_error(errno)),
            (0, DirFailure::Kind(kind)) => Some(std::io::Error::from(kind)),
            (real, failure) => {
                failing.set(Some((real - 1, failure)));
                None
            }
        })
    }

    /// The environment variable [`plain_durable_flushes`] sets and [`shipped_durable_flushes`]
    /// removes, on Apple targets, the only ones that read it. A process a test starts without
    /// it takes the durable flushes aterm ships ([`crate::durable_flushes_are_plain`] is
    /// `false` there).
    #[cfg(target_vendor = "apple")]
    pub use astream_sys::TEST_PLAIN_FSYNC_ENV;

    /// Mark this process, and every process it starts, as a test's
    /// (`TEST_PLAIN_FSYNC_ENV`, on Apple targets).
    /// # Safety
    /// No other thread may be running: it sets an environment variable.
    pub unsafe fn plain_durable_flushes() {
        // SAFETY: the caller runs before any other thread exists.
        #[cfg(target_vendor = "apple")]
        unsafe {
            std::env::set_var(astream_sys::TEST_PLAIN_FSYNC_ENV, "1");
        }
    }

    /// Unmark this process, and every process it starts: its durable flushes are the full
    /// flushes aterm ships whatever mark it inherited (`TEST_PLAIN_FSYNC_ENV` removed, on
    /// Apple targets; [`plain_durable_flushes`] is the other way).
    /// # Safety
    /// No other thread may be running: it removes an environment variable.
    pub unsafe fn shipped_durable_flushes() {
        // SAFETY: the caller runs before any other thread exists.
        #[cfg(target_vendor = "apple")]
        unsafe {
            std::env::remove_var(astream_sys::TEST_PLAIN_FSYNC_ENV);
        }
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
