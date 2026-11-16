// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deadline-bounded file-lock contention waits on Unix and Windows: exclusive, and the shared
//! lease [`LockFile::share`] takes; on macOS, a wait for a lock to come free that takes nothing
//! (`wait_unlocked`); and a thread's rule that it takes no lock at all ([`takes_nothing`]).

use std::io;
use std::time::{Duration, Instant};

/// Which lock a contention wait takes: one holder alone, or any number of shared holders while
/// no exclusive one has it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hold {
    Exclusive,
    Shared,
}

std::thread_local! {
    /// Whether this thread runs inside [`takes_nothing`].
    static TAKES_NOTHING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, debug_assertions))]
std::thread_local! {
    /// How many takes this thread tried through this module ([`takes_tried_here`]).
    static TAKES_TRIED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Run `run` with every lock this module would take on this thread refused: a watcher's rule. A
/// thread that waits for a lock to come free must never hold it, even for a moment, since the
/// holder's own waiters would then wait behind it (`aterm-cli`'s repository lease watch, which
/// waits with `PrivateLock::wait_until_free` on macOS). A take refused here is an error,
/// `PermissionDenied`, made before the lock is tried. The rule this thread had before is
/// restored as `run` returns or unwinds.
pub fn takes_nothing<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            TAKES_NOTHING.set(self.0);
        }
    }
    let _restore = Restore(TAKES_NOTHING.replace(true));
    run()
}

/// Test support: how many takes this thread has tried through this module (a `PrivateLock`, a
/// [`LockFile`], [`lock_exclusive_until`]), those [`takes_nothing`] refused included, from its
/// start: the witness that a thread which must take nothing never tried to, whatever rule it
/// ran under.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn takes_tried_here() -> u64 {
    TAKES_TRIED.get()
}

/// Where every take this module makes starts: [`takes_nothing`]'s refusal, where this thread
/// runs inside it, and in a test build a count of the try ([`takes_tried_here`]).
fn refused_here() -> io::Result<()> {
    #[cfg(any(test, debug_assertions))]
    TAKES_TRIED.set(TAKES_TRIED.get() + 1);
    if TAKES_NOTHING.get() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "this thread takes no lock (aterm_uds::file_lock::takes_nothing)",
        ));
    }
    Ok(())
}

/// Acquire an exclusive lock on an already validated file, waiting only for
/// actual lock contention until `deadline`. A first attempt is always made,
/// including when the deadline has passed. Other I/O errors return immediately.
/// File admission and any protocol-specific identity recheck belong to the caller.
/// The caller must explicitly unlock before closing or transferring ownership.
///
/// # Errors
/// Returns `WouldBlock` with `busy` when the lock remains held at the deadline,
/// or the original error of attempting the lock; [`takes_nothing`]'s refusal on a thread
/// inside it.
pub fn lock_exclusive_until(file: &std::fs::File, deadline: Instant, busy: &str) -> io::Result<()> {
    lock_exclusive_until_with(file, deadline, busy, || {})
}

/// [`lock_exclusive_until`], calling `held` after each actual contention result
/// before waiting. A test may release its holder there without timing assumptions.
///
/// # Errors
/// Returns [`lock_exclusive_until`]'s errors.
pub fn lock_exclusive_until_with(
    file: &std::fs::File,
    deadline: Instant,
    busy: &str,
    held: impl FnMut(),
) -> io::Result<()> {
    lock_exclusive_with_deadline(file, Some(deadline), busy, held)
}

/// Acquire an exclusive lock on an already validated file with no deadline and no polling: a
/// blocking `flock` that sleeps in the kernel until the holder lets go (a signal's interruption
/// waits again). The caller must explicitly unlock before closing or transferring ownership.
///
/// # Errors
/// The lock's own error, or [`takes_nothing`]'s refusal on a thread inside it.
#[cfg(unix)]
pub(crate) fn lock_exclusive_blocking(file: &std::fs::File) -> io::Result<()> {
    refused_here()?;
    crate::keep_out_of_children(file)?;
    loop {
        #[expect(
            clippy::disallowed_methods,
            reason = "the caller retains the validated file and explicitly unlocks before close"
        )]
        let taken = file.lock();
        match taken {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// Wait, taking nothing, until no open file description holds a lock on `file`'s vnode. A
/// kqueue hears each `flock` release there (`NOTE_FUNLOCK`, which an unlock, the last close of
/// a description that held a lock, and so its holder's death, each post), and `fcntl(F_GETLK)`,
/// which on macOS reads `flock` holders (as no process's, `l_pid` -1) and takes nothing, says
/// after each one, and once before the first wait, whether the lock is still held: a waiter
/// that holds it by the time of a read keeps this wait going until it lets go in turn. The
/// kernel only wakes a waiter at the release, which takes the lock once it runs, so a read made
/// first finds the lock free and ends the wait with that waiter about to take it. The watch is
/// armed before the first read, so a release after any read is heard, and no clock wakes the
/// thread meanwhile. `held` runs once, where the lock was found held, before the first wait.
/// The vnode leaving its name (deleted, renamed or revoked) ends the wait as well: the next
/// holder opens the file at that name, which nothing then holds. `file` itself is never locked
/// here.
///
/// # Errors
/// `kqueue(2)`'s, `kevent(2)`'s (but for an interrupted wait, which is waited again) or
/// `fcntl(2)`'s.
#[cfg(target_os = "macos")]
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn wait_unlocked(file: &std::fs::File, held: impl FnOnce()) -> io::Result<()> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    // What ends the wait with the vnode gone from its name.
    const GONE: u32 = libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE;
    // SAFETY: `kqueue()` takes no arguments and returns a new descriptor or -1.
    let raw = unsafe { libc::kqueue() };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is a fresh descriptor this process exclusively owns; a kqueue is never
    // inherited by a fork's child.
    let queue = unsafe { OwnedFd::from_raw_fd(raw) };
    let ident = libc::uintptr_t::try_from(file.as_raw_fd())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let watch = libc::kevent {
        ident,
        filter: libc::EVFILT_VNODE,
        flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR,
        fflags: libc::NOTE_FUNLOCK | GONE,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: one initialized change entry, live for the call; no event list requested.
    let armed = unsafe {
        libc::kevent(
            queue.as_raw_fd(),
            &raw const watch,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if armed < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut held = Some(held);
    while locked(file)? {
        if let Some(held) = held.take() {
            held();
        }
        // SAFETY: `kevent` is plain data; all-zero is a valid value for the call to fill.
        let mut heard: libc::kevent = unsafe { std::mem::zeroed() };
        // SAFETY: one output slot, live for the call; a null timeout waits with no clock.
        let got = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                std::ptr::null(),
                0,
                &raw mut heard,
                1,
                std::ptr::null(),
            )
        };
        if got < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        } else if got > 0 && heard.fflags & GONE != 0 {
            return Ok(());
        }
    }
    Ok(())
}

/// Whether any open file description holds a lock on `file`'s vnode, read with
/// `fcntl(F_GETLK)` over the whole file for an exclusive lock: on macOS a `flock` holder
/// answers it, this process's own others included, and nothing is taken. `file` holds none.
///
/// # Errors
/// `fcntl(2)`'s.
#[cfg(target_os = "macos")]
#[cfg_attr(trust_verify, trust::skip)]
fn locked(file: &std::fs::File) -> io::Result<bool> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: `flock` is plain data; all-zero is a valid value (`l_start` and `l_len` 0 name
    // the whole file).
    let mut probe = unsafe { std::mem::zeroed::<libc::flock>() };
    probe.l_type = libc::F_WRLCK;
    probe.l_whence = libc::SEEK_SET as libc::c_short;
    // SAFETY: F_GETLK reads and fills the one `flock` it is given, live for the call.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &raw mut probe) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(probe.l_type != libc::F_UNLCK)
}

/// Shared contention wait; `None` retains an unrepresentable duration's unbounded wait.
pub(crate) fn lock_exclusive_with_deadline(
    file: &std::fs::File,
    deadline: Option<Instant>,
    busy: &str,
    held: impl FnMut(),
) -> io::Result<()> {
    lock_exclusive_with_clock(file, deadline, busy, held, Instant::now, std::thread::sleep)
}

/// The clock and wait boundary lets tests advance the deadline without scheduling assumptions.
pub(crate) fn lock_exclusive_with_clock(
    file: &std::fs::File,
    deadline: Option<Instant>,
    busy: &str,
    held: impl FnMut(),
    now: impl Fn() -> Instant,
    sleep: impl FnMut(Duration),
) -> io::Result<()> {
    lock_with_clock(file, Hold::Exclusive, deadline, busy, held, now, sleep)
}

/// [`lock_exclusive_with_clock`] for either [`Hold`].
fn lock_with_clock(
    file: &std::fs::File,
    hold: Hold,
    deadline: Option<Instant>,
    busy: &str,
    mut held: impl FnMut(),
    now: impl Fn() -> Instant,
    mut sleep: impl FnMut(Duration),
) -> io::Result<()> {
    refused_here()?;
    crate::keep_out_of_children(file)?;
    let remaining = || {
        deadline.map_or(Duration::MAX, |deadline| {
            deadline.saturating_duration_since(now())
        })
    };
    let expired = || io::Error::new(io::ErrorKind::WouldBlock, busy.to_owned());
    let mut backoff = Duration::from_millis(1);
    let mut first_attempt = true;
    loop {
        if !first_attempt && remaining().is_zero() {
            return Err(expired());
        }
        first_attempt = false;
        #[expect(
            clippy::disallowed_methods,
            reason = "the caller retains the validated file and explicitly unlocks before close"
        )]
        let taken = match hold {
            Hold::Exclusive => file.try_lock(),
            Hold::Shared => file.try_lock_shared(),
        };
        match taken {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
            Err(std::fs::TryLockError::WouldBlock) => {
                if remaining().is_zero() {
                    return Err(expired());
                }
                #[cfg(any(test, debug_assertions))]
                LOCK_WAIT_OBSERVER.with(|observer| {
                    if let Some(observer) = observer.borrow_mut().as_mut() {
                        observer(file);
                    }
                });
                held();
                let pause = backoff.min(remaining());
                if pause.is_zero() {
                    return Err(expired());
                }
                sleep(pause);
                backoff = (backoff * 2).min(Duration::from_millis(50));
            }
        }
    }
}

#[cfg(any(test, debug_assertions))]
type LockWaitObserver = Box<dyn FnMut(&std::fs::File)>;

#[cfg(any(test, debug_assertions))]
std::thread_local! {
    static LOCK_WAIT_OBSERVER: std::cell::RefCell<Option<LockWaitObserver>> =
        const { std::cell::RefCell::new(None) };
}

/// Test support: observe actual, unexpired lock contention on this thread while
/// `run` executes. The previous observer is restored even if `run` unwinds.
/// The observer must not recursively acquire a contended lock on this thread.
#[cfg(any(test, debug_assertions))]
pub fn with_lock_wait_observer<T>(
    observer: impl FnMut(&std::fs::File) + 'static,
    run: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<LockWaitObserver>);
    impl Drop for Restore {
        fn drop(&mut self) {
            LOCK_WAIT_OBSERVER.with(|observer| {
                *observer.borrow_mut() = self.0.take();
            });
        }
    }
    let previous = LOCK_WAIT_OBSERVER.with(|current| current.replace(Some(Box::new(observer))));
    let _restore = Restore(previous);
    run()
}

/// A lock on a file of its own that guards something beside it (a record that is replaced whole
/// by a rename, so its own inode is no place for a lock): exclusive, taken by [`LockFile::take`]
/// or [`LockFile::try_take`], or shared, taken by [`LockFile::share`]; let go by `LOCK_UN` as it
/// drops, before the close: clippy.toml's flock release rule. A lock belongs to the open file it
/// was taken on, so two of one process are two holders, as two processes' are.
#[derive(Debug)]
pub struct LockFile(std::fs::File);

impl LockFile {
    /// Take an exclusive lock on the regular file at `path`, waiting while another holder has it
    /// until `deadline` ([`lock_exclusive_until`]). A file there is opened to read, whatever its
    /// mode lets this account write; none there is made 0600, whatever the umask. On Unix the
    /// open never follows a link and never waits on a FIFO (`O_NOFOLLOW | O_NONBLOCK`). The
    /// folder must exist.
    ///
    /// # Errors
    /// `WouldBlock` with `busy` when another holder kept it past `deadline`; `InvalidData` when
    /// `path` holds anything but a regular file; the open's and the lock's own error, a link at
    /// `path` included; [`takes_nothing`]'s refusal on a thread inside it.
    pub fn take(path: &std::path::Path, deadline: Instant, busy: &str) -> io::Result<Self> {
        Self::lock(open_or_make(path)?, path, deadline, busy)
    }

    /// Take a shared lock on the regular file at `path`, opened or made as [`Self::take`] opens
    /// or makes it: any number of shared holders hold it at once, and none beside an exclusive
    /// holder, whose let-go it waits for until `deadline`, with [`Self::take`]'s bounded wait.
    ///
    /// # Errors
    /// [`Self::take`]'s, `WouldBlock` with `busy` where an exclusive holder kept it past
    /// `deadline`.
    pub fn share(path: &std::path::Path, deadline: Instant, busy: &str) -> io::Result<Self> {
        let file = open_or_make(path)?;
        plain(&file, path)?;
        lock_with_clock(
            &file,
            Hold::Shared,
            Some(deadline),
            busy,
            || {},
            Instant::now,
            std::thread::sleep,
        )?;
        Ok(Self(file))
    }

    /// Take an exclusive lock on the regular file at `path`, opened or made as [`Self::take`]
    /// opens or makes it, where no other holder, shared or exclusive, has it now: `None` where
    /// one does. It never waits.
    ///
    /// # Errors
    /// The open's and the lock's own error; `InvalidData` when `path` holds anything but a
    /// regular file; [`takes_nothing`]'s refusal on a thread inside it.
    pub fn try_take(path: &std::path::Path) -> io::Result<Option<Self>> {
        let file = open_or_make(path)?;
        plain(&file, path)?;
        refused_here()?;
        crate::keep_out_of_children(&file)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "the LockFile it becomes keeps the file and unlocks before the close"
        )]
        let taken = file.try_lock();
        match taken {
            Ok(()) => Ok(Some(Self(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }

    /// Take an exclusive lock on an existing plain regular file, without creating a missing file.
    /// On Unix the open never follows a link or waits on a FIFO. On Windows it opens the reparse
    /// point itself and refuses it rather than following or locking its target.
    ///
    /// # Errors
    /// As [`Self::take`], also returning `NotFound` when the file does not exist and `InvalidData`
    /// for a Windows reparse point.
    pub fn take_existing(
        path: &std::path::Path,
        deadline: Instant,
        busy: &str,
    ) -> io::Result<Self> {
        Self::lock(open_existing(path)?, path, deadline, busy)
    }

    fn lock(
        file: std::fs::File,
        path: &std::path::Path,
        deadline: Instant,
        busy: &str,
    ) -> io::Result<Self> {
        plain(&file, path)?;
        lock_exclusive_until(&file, deadline, busy)?;
        Ok(Self(file))
    }
}

/// `InvalidData` unless `file`, opened at `path`, is a regular file (on Windows, and no reparse
/// point).
fn plain(file: &std::fs::File, path: &std::path::Path) -> io::Result<()> {
    let metadata = file.metadata()?;
    let plain_file = metadata.is_file();
    #[cfg(windows)]
    let plain_file = {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        plain_file && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
    };
    if !plain_file {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is no regular file", path.display()),
        ));
    }
    Ok(())
}

impl Drop for LockFile {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// The lock file at `path`: made by this open (`O_EXCL`, which never follows a link) and its mode
/// said again on the descriptor, so a umask that clears an owner bit leaves it 0600; or the one
/// there, opened to read (a lock needs no more).
fn open_or_make(path: &std::path::Path) -> io::Result<std::fs::File> {
    let mut making = std::fs::OpenOptions::new();
    making.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        making
            .mode(0o600)
            .custom_flags(crate::atomic_file::LOCK_OPEN_FLAGS);
    }
    match making.open(path) {
        Ok(file) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(file)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => open_existing(path),
        Err(error) => Err(error),
    }
}

fn open_existing(path: &std::path::Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

#[cfg(all(test, unix))]
#[path = "file_lock_tests.rs"]
mod tests;
