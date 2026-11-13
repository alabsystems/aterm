// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The store-wide single-writer lock: one process at a time mutates a store.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::store::Layout;

/// A held advisory lock on an open file, released by `LOCK_UN` on drop rather than by the close.
pub(crate) struct Flock(File);

impl Flock {
    /// Take `file`'s exclusive lock without blocking.
    pub(crate) fn try_lock(file: File) -> Result<Self, std::fs::TryLockError> {
        let file: std::fs::File = file;
        #[cfg(any(unix, windows))]
        let _ = aterm_uds::keep_out_of_children(&file);
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        file.try_lock()?;
        Ok(Self(file))
    }

    /// Take `file`'s shared lock without blocking.
    pub(crate) fn try_lock_shared(file: File) -> Result<Self, std::fs::TryLockError> {
        let file: std::fs::File = file;
        #[cfg(any(unix, windows))]
        let _ = aterm_uds::keep_out_of_children(&file);
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        file.try_lock_shared()?;
        Ok(Self(file))
    }

    /// Take `file`'s exclusive lock, re-trying every [`LOCK_POLL`] for up to `limit`.
    pub(crate) fn lock_within(file: File, limit: Duration, what: &Path) -> io::Result<Self> {
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        let take: TakeLock = File::try_lock;
        Self::take_within(file, limit, what, take)
    }

    /// Take `file`'s shared lock, re-trying every [`LOCK_POLL`] for up to `limit`.
    #[cfg(windows)]
    pub(crate) fn lock_shared_within(file: File, limit: Duration, what: &Path) -> io::Result<Self> {
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        let take: TakeLock = File::try_lock_shared;
        Self::take_within(file, limit, what, take)
    }

    /// `take` `file`'s lock, re-trying every [`LOCK_POLL`] for up to `limit`.
    fn take_within(file: File, limit: Duration, what: &Path, take: TakeLock) -> io::Result<Self> {
        let file: std::fs::File = file;
        #[cfg(any(unix, windows))]
        let _ = aterm_uds::keep_out_of_children(&file);
        let deadline = Instant::now().checked_add(limit);
        loop {
            match take(&file) {
                Ok(()) => return Ok(Self(file)),
                Err(std::fs::TryLockError::WouldBlock)
                    if deadline.is_none_or(|d| Instant::now() < d) =>
                {
                    std::thread::sleep(LOCK_POLL);
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "{} was held for more than {}s",
                            what.display(),
                            limit.as_secs()
                        ),
                    ));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
            }
        }
    }

    /// Take `file`'s exclusive lock, waiting for a holder to let go.
    pub(crate) fn lock(file: File) -> io::Result<Self> {
        let file: std::fs::File = file;
        #[cfg(any(unix, windows))]
        let _ = aterm_uds::keep_out_of_children(&file);
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        file.lock()?;
        Ok(Self(file))
    }
}

/// How often [`Flock::lock_within`] re-tries a held lock.
const LOCK_POLL: Duration = Duration::from_millis(50);

/// One non-blocking take of a file's lock, exclusive or shared ([`Flock::take_within`]).
type TakeLock = fn(&File) -> Result<(), std::fs::TryLockError>;

impl std::ops::Deref for Flock {
    type Target = File;
    fn deref(&self) -> &File {
        &self.0
    }
}

impl std::ops::DerefMut for Flock {
    fn deref_mut(&mut self) -> &mut File {
        &mut self.0
    }
}

impl Drop for Flock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Serialize metadata edits through a stable lock file that publication never replaces.
pub(crate) fn lock_metadata(path: &Path) -> io::Result<Flock> {
    Flock::lock(open_lock_file(path)?)
}

/// Exclusive lock on a file carrying a holder record ([`crate::vendor_direct::watch`]).
pub(crate) use aterm_toolchain::record_lock::RecordLock;

/// The store-wide writer lock, held while the value lives.
pub struct StoreLock {
    _file: Flock,
    /// The holder record this lock announced ([`StoreLock::announce_holder`]).
    holder: Option<PathBuf>,
    /// The store this lock serializes ([`held_by_this_process`]).
    prefix: PathBuf,
}

/// The prefixes whose store lock a live [`StoreLock`] in this process holds.
static HELD: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Whether a [`StoreLock`] on `layout`'s store is alive in this process.
#[must_use]
pub fn held_by_this_process(layout: &Layout) -> bool {
    HELD.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&layout.prefix)
}

/// `<prefix>/store.lock.holder`.
#[must_use]
pub fn holder_path(layout: &Layout) -> PathBuf {
    layout.prefix.join("store.lock.holder")
}

impl StoreLock {
    /// First writer of an already-created private prefix. All mutation addresses the
    /// actual original directory; no configured store or pathname-open retry is involved.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    pub(crate) fn create_in_original(layout: &Layout, prefix: &File) -> io::Result<Self> {
        match astream_sys::owner_private_dir(prefix)? {
            Ok(_) => {}
            Err(why) => return Err(io::Error::other(format!("fresh private prefix: {why}"))),
        }
        let file = aterm_uds::kept_out(|| {
            aterm_dirfd::openat(
                prefix,
                "store.lock",
                aterm_dirfd::OFlags::RDWR
                    | aterm_dirfd::OFlags::CREATE
                    | aterm_dirfd::OFlags::EXCL
                    | aterm_dirfd::OFlags::NOFOLLOW
                    | aterm_dirfd::OFlags::CLOEXEC,
                aterm_dirfd::Mode::from_bits(0o600),
            )
            .map(File::from)
            .map_err(io::Error::from)
        })?;
        let file = Flock::try_lock(file).map_err(|why| io::Error::other(why.to_string()))?;
        HELD.lock()
            .map_err(|_| io::Error::other("store lock registry poisoned"))?
            .push(layout.prefix.clone());
        Ok(Self {
            _file: file,
            holder: None,
            prefix: layout.prefix.clone(),
        })
    }
    /// The layout this guard actually locked, even if configuration changes meanwhile.
    pub(crate) fn layout(&self) -> Layout {
        Layout {
            prefix: self.prefix.clone(),
        }
    }

    /// A local installer must still address the very writer-lock inode this guard holds.
    #[cfg(unix)]
    pub(crate) fn checked_layout(&self) -> io::Result<Layout> {
        use std::os::unix::fs::MetadataExt as _;
        let layout = self.layout();
        let named = crate::local_harness::fs::owned_file(&crate::metadata_io::open_regular(
            &layout.store_lock(),
        )?)?;
        let held = crate::local_harness::fs::owned_file(&self._file)?;
        if (named.dev(), named.ino()) != (held.dev(), held.ino()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "store writer lock changed",
            ));
        }
        Ok(layout)
    }

    /// Bind the writer lock through an already held prefix, never another pathname lookup.
    #[cfg(unix)]
    pub(crate) fn checked_layout_at(&self, prefix: &impl std::os::fd::AsFd) -> io::Result<Layout> {
        use std::os::unix::fs::MetadataExt as _;
        let named = aterm_dirfd::openat(
            prefix,
            "store.lock",
            aterm_dirfd::OFlags::RDONLY
                | aterm_dirfd::OFlags::NOFOLLOW
                | aterm_dirfd::OFlags::CLOEXEC
                | aterm_dirfd::OFlags::NONBLOCK,
            aterm_dirfd::Mode::empty(),
        )
        .map_err(io::Error::from)?;
        let named = crate::local_harness::fs::owned_file(&std::fs::File::from(named))?;
        let held = crate::local_harness::fs::owned_file(&self._file)?;
        if (named.dev(), named.ino()) != (held.dev(), held.ino()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "store writer lock changed",
            ));
        }
        Ok(self.layout())
    }

    /// Say who holds the store ([`holder_path`]): this pid, and whether it is a person's
    /// typed verb ([`person_holder`]).
    pub fn announce_holder(&mut self, layout: &Layout, person: bool) {
        let path = holder_path(layout);
        let body = format!("pid={}\nperson={}\n", std::process::id(), u8::from(person));
        let tmp = layout
            .prefix
            .join(format!("store.lock.holder.tmp-{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let written = crate::platform::open_create_new_write(&tmp, 0o644)
            .and_then(|mut f| std::io::Write::write_all(&mut f, body.as_bytes()))
            .and_then(|()| std::fs::rename(&tmp, &path));
        if written.is_ok() {
            self.holder = Some(path);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        {
            let mut held = HELD
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(at) = held.iter().position(|p| *p == self.prefix) {
                held.swap_remove(at);
            }
        }
        if let Some(path) = &self.holder
            && read_holder(path).is_some_and(|(pid, _)| pid == std::process::id())
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn read_holder(path: &Path) -> Option<(u32, bool)> {
    let text = crate::metadata_io::read_bounded_regular_utf8(path, 256).ok()?;
    let mut pid = None;
    let mut person = None;
    for line in text.lines() {
        match line.split_once('=') {
            Some(("pid", v)) => {
                let s = v.trim();
                pid = (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| s.parse::<u32>().ok())
                    .flatten();
            }
            Some(("person", v)) => person = Some(v.trim() == "1"),
            _ => {}
        }
    }
    Some((pid?, person?))
}

/// Whether the holder record names a person's typed verb in a live process ([`person_holder`]).
#[must_use]
pub fn holder_is_person(layout: &Layout) -> bool {
    read_holder(&holder_path(layout))
        .is_some_and(|(pid, person)| person && crate::progress::pid_alive(pid))
}

/// The pid of a person's typed verb holding `layout`'s store right now, else `None`.
#[must_use]
pub fn person_holder(layout: &Layout) -> Option<u32> {
    let (pid, person) = read_holder(&holder_path(layout))?;
    if !(person && pid != std::process::id() && crate::progress::pid_alive(pid)) {
        return None;
    }
    match try_lock_store(layout) {
        Ok(free) => {
            drop(free);
            if read_holder(&holder_path(layout)).is_some_and(|(p, _)| p == pid) {
                let _ = std::fs::remove_file(holder_path(layout));
            }
            None
        }
        Err(StoreLockError::Contended(_)) => Some(pid),
        Err(StoreLockError::Io(_, _)) => None,
    }
}

/// Why the store lock could not be taken.
#[derive(Debug)]
pub enum StoreLockError {
    /// Another atpkg process holds the lock.
    Contended(PathBuf),
    /// The lock file could not be created, opened or locked for another reason (an unwritable prefix, an I/O error).
    Io(PathBuf, io::Error),
}

impl std::fmt::Display for StoreLockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreLockError::Contended(path) => write!(
                f,
                "another atpkg process holds the store lock at {} — refusing to mutate \
                 the store concurrently (retry when it exits)",
                path.display()
            ),
            StoreLockError::Io(path, e)
                if e.kind() == io::ErrorKind::PermissionDenied
                    && e.to_string().contains("is a symlink") =>
            {
                write!(
                    f,
                    "cannot take the store lock at {}: {e} — refusing to mutate the \
                     store without it. atpkg never writes through a link at its \
                     prefix: remove the link, or point `[packages].prefix` in \
                     ~/.config/aterm/aterm.toml at a real directory",
                    path.display()
                )
            }
            StoreLockError::Io(path, e) if e.kind() == io::ErrorKind::PermissionDenied => {
                write!(
                    f,
                    "cannot take the store lock at {}: {e} — refusing to mutate the \
                     store without it. The prefix must be yours alone. atpkg's default \
                     prefix is under $HOME and needs no privilege; when root owns it, a \
                     run under sudo made it so, and `sudo chown -R \"$USER\"` on {} gives \
                     it back. A `[packages].prefix` in ~/.config/aterm/aterm.toml names a \
                     shared multi-user store, changed only as its owner; removing that \
                     line returns to the default",
                    path.display(),
                    path.parent().unwrap_or(path).display()
                )
            }
            StoreLockError::Io(path, e) => write!(
                f,
                "cannot take the store lock at {}: {e} — refusing to mutate the store \
                 without it",
                path.display()
            ),
        }
    }
}

impl std::error::Error for StoreLockError {}

/// The exit code a CLI-edge contention refusal carries.
pub const CONTENDED_EXIT: u8 = 75;

/// How often a waiting acquisition re-tries the lock ([`repoll_store_lock`]).
const WAIT_POLL: Duration = Duration::from_millis(50);

/// How long a waiter stays silent before it announces the wait (`on_first_contention`).
pub const WAIT_ANNOUNCE_GRACE: Duration = Duration::from_secs(2);

/// A development seam (`aterm_types::dev_seam!`; a shipped binary does not read it).
pub const WAIT_ANNOUNCE_GRACE_SEAM: &str = "ATPKG_WAIT_ANNOUNCE_GRACE_MS";

const WAIT_MAX: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Acquire the store lock ([`try_lock_store`], [`repoll_store_lock`], [`WAIT_POLL`]).
pub fn lock_store_waiting(
    layout: &Layout,
    timeout: Duration,
    on_first_contention: impl FnOnce(&Path),
    still_wanted: impl Fn() -> bool,
) -> Result<StoreLock, StoreLockError> {
    let grace = aterm_types::dev_seam!(WAIT_ANNOUNCE_GRACE_SEAM)
        .and_then(|ms| ms.to_str().filter(|s| !s.starts_with('+'))?.parse().ok())
        .map_or(WAIT_ANNOUNCE_GRACE, Duration::from_millis);
    lock_store_waiting_on(
        &MonotonicClock,
        layout,
        timeout,
        grace,
        on_first_contention,
        still_wanted,
    )
}

/// How long a person's typed verb waits for the pass holding the store before it stops.
pub const TYPED_WAIT: Duration = Duration::from_secs(10 * 60);

/// [`lock_store_waiting`] for a person at a terminal, up to `bound`, with one line on stderr
/// from the first contended poll on, naming `who` waits ("aterm doctor"): what the pass holding
/// the store is doing, read from its own progress (the program it is on, its bytes and the time
/// left; between programs, how many of its programs are done and how long it has run), else
/// that it waits for the pass. The line is cleared when the wait ends; where no meter can be
/// drawn, the waiting line is said once instead.
pub fn lock_store_waiting_shown(
    layout: &Layout,
    bound: Duration,
    who: &str,
) -> Result<StoreLock, StoreLockError> {
    let meter = std::cell::RefCell::new(None);
    let waiting = format!("{who} waits for the install pass that holds the store (Ctrl-C stops)");
    let waited = lock_store_waiting_on(
        &MonotonicClock,
        layout,
        bound,
        Duration::ZERO,
        |_| {
            let (layout, who, busy) = (layout.clone(), who.to_owned(), waiting.clone());
            let shown = crate::meter::TtyMeter::start(move || {
                let now = crate::flow::now_unix().max(0).unsigned_abs();
                match crate::progress::read_progress(&layout)
                    .filter(|file| crate::progress::snapshot_running(file, now))
                {
                    Some(file) => {
                        let idle = between_programs(&who, &file, now);
                        crate::meter::Show::Pass {
                            file,
                            focus: None,
                            idle,
                        }
                    }
                    None => crate::meter::Show::Busy(busy.clone()),
                }
            });
            if shown.is_none() {
                eprintln!("{waiting}");
            }
            *meter.borrow_mut() = shown;
        },
        || true,
    );
    drop(meter.take());
    waited
}

/// [`lock_store_waiting_shown`]'s line while the pass is between programs: `who` waits, how many
/// of its programs are done, and how long it has run at `now` (Unix seconds).
fn between_programs(who: &str, file: &crate::progress::ProgressFile, now: u64) -> String {
    let overall = &file.overall;
    format!(
        "{who} waits: install pass {}/{} programs, {} s in (Ctrl-C stops)",
        overall.programs_done,
        overall.programs_total,
        now.saturating_sub(file.started_unix)
    )
}

/// The clock a waiting acquisition reads and sleeps on ([`lock_store_waiting_on`]).
pub(crate) trait WaitClock {
    fn now(&self) -> Instant;
    fn sleep(&self, pause: Duration);
}

/// The monotonic clock and a real sleep ([`lock_store_waiting`]).
struct MonotonicClock;

impl WaitClock for MonotonicClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, pause: Duration) {
        std::thread::sleep(pause);
    }
}

/// [`lock_store_waiting`] on `clock`.
pub(crate) fn lock_store_waiting_on(
    clock: &impl WaitClock,
    layout: &Layout,
    timeout: Duration,
    grace: Duration,
    on_first_contention: impl FnOnce(&Path),
    still_wanted: impl Fn() -> bool,
) -> Result<StoreLock, StoreLockError> {
    let started = clock.now();
    let deadline = started + timeout.min(WAIT_MAX);
    let mut announce = Some(on_first_contention);
    let mut attempt = try_lock_store(layout);
    loop {
        match attempt {
            Ok(guard) => return Ok(guard),
            Err(StoreLockError::Contended(path)) => {
                // A waiter whose caller has gone stands down silently, grace or not: the
                // caller's pipe is exactly what it must not print to.
                if !still_wanted() {
                    return Err(StoreLockError::Contended(path));
                }
                let now = clock.now();
                if now.duration_since(started) >= grace
                    && let Some(announce) = announce.take()
                {
                    announce(&path);
                }
                if now >= deadline {
                    return Err(StoreLockError::Contended(path));
                }
                clock.sleep(WAIT_POLL.min(deadline.saturating_duration_since(now)));
                attempt = repoll_store_lock(layout, &path);
            }
            Err(io) => return Err(io),
        }
    }
}

/// One poll of the wait ([`prefix_is_a_real_dir`]).
fn repoll_store_lock(layout: &Layout, path: &Path) -> Result<StoreLock, StoreLockError> {
    if !prefix_is_a_real_dir(&layout.prefix) {
        return try_lock_store(layout);
    }
    #[cfg(test)]
    tests::between_vet_and_open();
    match open_store_lock(path) {
        Err(StoreLockError::Io(_, e))
            if e.kind() == std::io::ErrorKind::NotFound
                || !prefix_is_a_real_dir(&layout.prefix) =>
        {
            try_lock_store(layout)
        }
        polled => polled,
    }
}

/// Try-acquire the store lock for `layout`'s store ([`StoreLockError::Contended`]).
pub fn try_lock_store(layout: &Layout) -> Result<StoreLock, StoreLockError> {
    let path = layout.store_lock();
    if let Err(e) = layout.ensure_dir(&layout.prefix) {
        return Err(StoreLockError::Io(path, e));
    }
    open_store_lock(&path)
}

fn prefix_is_a_real_dir(prefix: &Path) -> bool {
    std::fs::symlink_metadata(prefix)
        .is_ok_and(|md| md.file_type().is_dir() && !crate::platform::is_reparse(&md))
}

/// Whether the store writer lock is free, for a parked GUI lane whose wait already timed out.
#[must_use]
pub fn store_writer_released(layout: &Layout) -> bool {
    if !prefix_is_a_real_dir(&layout.prefix) {
        return false;
    }
    let Ok(file) = crate::metadata_io::open_regular(&layout.store_lock()) else {
        return false;
    };
    let Ok(probe) = Flock::try_lock_shared(file) else {
        return false;
    };
    drop(probe);
    prefix_is_a_real_dir(&layout.prefix)
}

/// The open-and-try-lock half of [`try_lock_store`] ([`lock_store_waiting`]).
fn open_store_lock(path: &Path) -> Result<StoreLock, StoreLockError> {
    let file = match open_lock_file(path) {
        Ok(f) => f,
        Err(e) => return Err(StoreLockError::Io(path.to_path_buf(), e)),
    };
    match Flock::try_lock(file) {
        Ok(file) => {
            let prefix = path.parent().map(Path::to_path_buf).unwrap_or_default();
            HELD.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(prefix.clone());
            Ok(StoreLock {
                _file: file,
                holder: None,
                prefix,
            })
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            Err(StoreLockError::Contended(path.to_path_buf()))
        }
        Err(std::fs::TryLockError::Error(e)) => Err(StoreLockError::Io(path.to_path_buf(), e)),
    }
}

/// The lock file at `path`, created `0600` if it is missing and never truncated (a lock
/// file is a rendezvous, not data: the FileLock discipline), opened IN the directory
/// that is at `path`'s parent now: that directory is opened without following a link,
/// then the file by name inside it (`openat`).
#[cfg(unix)]
fn open_lock_file(path: &Path) -> io::Result<File> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} names no file in a directory", path.display()),
        ));
    };
    let dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        })?;
    let name = std::ffi::CString::new(name.as_bytes())?;
    let file = aterm_uds::kept_out(|| {
        // SAFETY: `dir` is open for the call and `name` is NUL-terminated; the mode
        // is the variadic argument `O_CREAT` reads.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK,
                libc::c_uint::from(0o600_u16),
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `openat` just returned `fd`, which nothing else owns.
        Ok(File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
    })?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    Ok(file)
}

/// The lock file at `path`, created if it is missing and never truncated.
#[cfg(not(unix))]
fn open_lock_file(path: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
}

#[cfg(test)]
#[path = "lock_tests.rs"]
mod tests;
