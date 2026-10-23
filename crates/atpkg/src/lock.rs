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
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        file.try_lock_shared()?;
        Ok(Self(file))
    }

    /// Take `file`'s exclusive lock, re-trying every [`LOCK_POLL`] for up to `limit`.
    pub(crate) fn lock_within(file: File, limit: Duration, what: &Path) -> io::Result<Self> {
        let file: std::fs::File = file;
        let deadline = Instant::now() + limit;
        loop {
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard's own take: its Drop releases by LOCK_UN"
            )]
            let taken = file.try_lock();
            match taken {
                Ok(()) => return Ok(Self(file)),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
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
    /// The layout this guard actually locked, even if configuration changes meanwhile.
    pub(crate) fn layout(&self) -> Layout {
        Layout {
            prefix: self.prefix.clone(),
        }
    }

    /// Say who holds the store ([`holder_path`]): this pid, and whether it is a person's
    /// typed verb ([`person_holder`]).
    pub fn announce_holder(&mut self, layout: &Layout, person: bool) {
        let path = holder_path(layout);
        let body = format!("pid={}\nperson={}\n", std::process::id(), u8::from(person));
        let tmp = layout
            .prefix
            .join(format!("store.lock.holder.tmp-{}", std::process::id()));
        if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
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
            Some(("pid", v)) => pid = v.trim().parse::<u32>().ok(),
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
        Err(_) => Some(pid),
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
        .and_then(|ms| ms.to_str()?.parse().ok())
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
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        })?;
    let name = std::ffi::CString::new(name.as_bytes())?;
    // SAFETY: `dir` is open for the call and `name` is NUL-terminated; the mode
    // is the variadic argument `O_CREAT` reads.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC,
            libc::c_uint::from(0o600_u16),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` just returned `fd`, which nothing else owns.
    Ok(File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
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
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    thread_local! {
        static BETWEEN_VET_AND_OPEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// [`repoll_store_lock`]'s seam: run the act this thread armed, once.
    pub(super) fn between_vet_and_open() {
        if let Some(act) = BETWEEN_VET_AND_OPEN.with(|slot| slot.borrow_mut().take()) {
            act();
        }
    }

    fn temp_layout(label: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::absent_layout(&format!("atpkg-lock-{label}-"))
    }

    struct ManualClock<'a> {
        origin: Instant,
        elapsed: std::cell::Cell<Duration>,
        sleeps: std::cell::Cell<u32>,
        at_sleep: std::cell::RefCell<Box<dyn FnMut(u32) + 'a>>,
    }

    impl<'a> ManualClock<'a> {
        fn new() -> Self {
            Self::at_sleep(|_| {})
        }

        fn at_sleep(at_sleep: impl FnMut(u32) + 'a) -> Self {
            Self {
                origin: Instant::now(),
                elapsed: std::cell::Cell::new(Duration::ZERO),
                sleeps: std::cell::Cell::new(0),
                at_sleep: std::cell::RefCell::new(Box::new(at_sleep)),
            }
        }

        fn elapsed(&self) -> Duration {
            self.elapsed.get()
        }
    }

    impl WaitClock for ManualClock<'_> {
        fn now(&self) -> Instant {
            self.origin + self.elapsed.get()
        }

        fn sleep(&self, pause: Duration) {
            let n = self.sleeps.get() + 1;
            self.sleeps.set(n);
            (self.at_sleep.borrow_mut())(n);
            self.elapsed.set(self.elapsed.get() + pause);
        }
    }

    #[test]
    fn release_probe_is_read_only_and_never_claims_the_writer_lock() {
        let layout = temp_layout("release-probe");
        std::fs::create_dir_all(&layout.prefix).unwrap();
        let before_missing = std::fs::metadata(&layout.prefix)
            .unwrap()
            .modified()
            .unwrap();
        assert!(
            !store_writer_released(&layout),
            "missing is not a release signal"
        );
        assert!(
            !layout.store_lock().exists(),
            "probe did not create the lock"
        );
        assert_eq!(
            std::fs::metadata(&layout.prefix)
                .unwrap()
                .modified()
                .unwrap(),
            before_missing,
            "probe did not change the prefix"
        );

        let holder = try_lock_store(&layout).unwrap();
        let lock_modified = std::fs::metadata(layout.store_lock())
            .unwrap()
            .modified()
            .unwrap();
        let prefix_modified = std::fs::metadata(&layout.prefix)
            .unwrap()
            .modified()
            .unwrap();
        for _ in 0..3 {
            assert!(!store_writer_released(&layout), "the writer still holds it");
        }
        assert_eq!(
            std::fs::metadata(layout.store_lock())
                .unwrap()
                .modified()
                .unwrap(),
            lock_modified,
            "held polls did not write the lock"
        );
        assert_eq!(
            std::fs::metadata(&layout.prefix)
                .unwrap()
                .modified()
                .unwrap(),
            prefix_modified,
            "held polls did not write the prefix"
        );
        drop(holder);
        assert!(store_writer_released(&layout), "writer dropped");
        let next = try_lock_store(&layout).expect("the probe left no writer claim");
        drop(next);
        #[cfg(unix)]
        {
            let target = layout.prefix.join("decoy");
            std::fs::write(&target, b"do not follow").unwrap();
            std::fs::remove_file(layout.store_lock()).unwrap();
            std::os::unix::fs::symlink(&target, layout.store_lock()).unwrap();
            assert!(
                !store_writer_released(&layout),
                "a linked lock path is not evidence that the writer released"
            );
            assert_eq!(std::fs::read(&target).unwrap(), b"do not follow");
        }
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    /// A person's typed verb reads as the holder only while its process lives and the lock is
    /// held; a lane never does, a dead or stale record reads as nobody, and the record goes
    /// with the lock.
    #[cfg(unix)]
    #[test]
    fn the_holder_record_names_a_person_only_while_one_holds_the_store() {
        let layout = temp_layout("holder");
        {
            let mut guard = try_lock_store(&layout).unwrap();
            guard.announce_holder(&layout, false);
            assert_eq!(person_holder(&layout), None, "a lane is no person");
            assert!(holder_path(&layout).is_file());
        }
        assert!(!holder_path(&layout).exists(), "released with the lock");
        let parent = std::os::unix::process::parent_id();
        std::fs::write(holder_path(&layout), format!("pid={parent}\nperson=1\n")).unwrap();
        {
            let _held = lock_store_waiting(&layout, aterm_spec::wait::MUST_HAPPEN, |_| {}, || true)
                .expect("the released store lock becomes takeable");
            assert_eq!(person_holder(&layout), Some(parent));
        }
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while person_holder(&layout).is_some() {
            assert!(
                Instant::now() < deadline,
                "a released lock never made the parent record stale"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !holder_path(&layout).exists(),
            "the stale record is removed"
        );
        std::fs::write(
            holder_path(&layout),
            format!("pid={}\nperson=1\n", std::process::id()),
        )
        .unwrap();
        assert_eq!(person_holder(&layout), None);
        std::fs::write(holder_path(&layout), "pid=999999999\nperson=1\n").unwrap();
        assert_eq!(person_holder(&layout), None);
        std::fs::write(holder_path(&layout), "garbage").unwrap();
        assert_eq!(person_holder(&layout), None);
        let _ = std::fs::remove_dir_all(&layout.prefix);
    }

    #[test]
    fn contended_store_lock_refuses_loudly_and_releases_on_drop() {
        let a = temp_layout("contend");
        let b = Layout {
            prefix: a.prefix.clone(),
        };
        let guard = try_lock_store(&a).expect("first acquisition succeeds");
        let err = match try_lock_store(&b) {
            Ok(_) => panic!("a held store lock must refuse a second mutator"),
            Err(e) => e,
        };
        assert!(
            matches!(err, StoreLockError::Contended(ref p) if *p == a.store_lock()),
            "contention names the lock path: {err:?}"
        );
        drop(guard);
        let reacquired = try_lock_store(&b).expect("a released store lock is takeable at once");
        drop(reacquired);
        let _ = std::fs::remove_dir_all(&a.prefix);
    }

    #[test]
    fn a_record_lock_leaves_its_record_readable_and_refuses_a_whole_file_probe() {
        use std::io::{Read as _, Write as _};
        let a = temp_layout("record");
        std::fs::create_dir_all(&a.prefix).expect("prefix");
        let path = a.prefix.join("record.lock");
        let open = || {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .expect("open")
        };
        let mut held = RecordLock::try_lock(open(), 64).expect("free");
        held.write_all(b"4242\n")
            .expect("the holder writes its record");
        let mut record = String::new();
        open()
            .take(64)
            .read_to_string(&mut record)
            .expect("another handle reads the record while the lock stands");
        assert_eq!(record, "4242\n");
        assert!(matches!(
            Flock::try_lock(open()),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        assert!(matches!(
            Flock::try_lock_shared(open()),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        assert!(
            matches!(
                RecordLock::try_lock(open(), 64),
                Err(std::fs::TryLockError::WouldBlock)
            ),
            "a second holder is refused"
        );
        held.unlock();
        assert!(Flock::try_lock(open()).is_ok(), "free once let go");
        drop(held);
        let _ = std::fs::remove_dir_all(&a.prefix);
    }

    #[test]
    fn a_dropped_record_lock_is_free_while_a_copy_of_its_file_lives() {
        let a = temp_layout("record-drop-copy");
        std::fs::create_dir_all(&a.prefix).expect("prefix");
        let path = a.prefix.join("record.lock");
        let open = || {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .expect("open")
        };
        let held = RecordLock::try_lock(open(), 64).expect("first holder");
        let copy = held.try_clone().expect("a live copy of the open file");
        assert!(matches!(
            RecordLock::try_lock(open(), 64),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(held);
        let again = RecordLock::try_lock(open(), 64).expect("free while its file copy lives");
        drop((again, copy));
    }

    #[test]
    fn a_dropped_store_lock_is_free_while_a_copy_of_its_descriptor_lives() {
        let a = temp_layout("dup");
        let b = Layout {
            prefix: a.prefix.clone(),
        };
        let guard = try_lock_store(&a).expect("first acquisition succeeds");
        let childs_copy = guard._file.try_clone().expect("dup the lock's descriptor");
        drop(guard);
        let again = try_lock_store(&b).expect("free while a copy of its descriptor lives");
        drop(again);
        let path = a.prefix.join("other.lock");
        let open = || {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&path)
                .unwrap()
        };
        let held = Flock::try_lock(open()).expect("free");
        let copy = held.try_clone().unwrap();
        drop(held);
        assert!(Flock::try_lock(open()).is_ok(), "free while its copy lives");
        let queued = Flock::lock(open()).expect("free");
        let queued_copy = queued.try_clone().unwrap();
        drop(queued);
        assert!(
            Flock::try_lock(open()).is_ok(),
            "a waited-for lock is free while its copy lives"
        );
        drop((childs_copy, copy, queued_copy));
        let _ = std::fs::remove_dir_all(&a.prefix);
    }

    #[test]
    fn store_lock_creates_the_prefix_and_hardens_the_lock_file() {
        let l = temp_layout("fresh");
        assert!(!l.prefix.exists(), "fixture: no prefix yet");
        let guard = try_lock_store(&l).expect("lockable before the store exists");
        assert!(
            l.store_lock().is_file(),
            "store.lock created under the prefix"
        );
        #[cfg(unix)]
        {
            let dir_mode = std::fs::metadata(&l.prefix).unwrap().permissions().mode();
            assert_eq!(dir_mode & 0o777, 0o700, "prefix hardened 0700");
            let mode = std::fs::metadata(l.store_lock())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "store.lock is 0600");
        }
        drop(guard);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Re-taking the lock moves no stamp on the prefix, an ancestor of every store build and
    /// exec root whose ctime tippy snapshots (a same-value `chmod` or `setxattr` moves ctime
    /// on APFS).
    #[cfg(unix)]
    #[test]
    fn a_second_store_lock_moves_no_stamp_on_the_prefix() {
        use std::os::unix::fs::MetadataExt;
        let l = temp_layout("nochurn");
        drop(try_lock_store(&l).expect("first acquisition"));
        let stamp = || {
            let m = std::fs::symlink_metadata(&l.prefix).unwrap();
            (m.ctime(), m.ctime_nsec(), m.mtime(), m.mtime_nsec())
        };
        let before = stamp();
        std::thread::sleep(std::time::Duration::from_millis(20));
        drop(try_lock_store(&l).expect("second acquisition"));
        assert_eq!(
            stamp(),
            before,
            "re-taking the lock moved the prefix's stamps"
        );
        std::fs::set_permissions(&l.prefix, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_ne!(stamp(), before, "control: a same-mode chmod moves ctime");
        std::fs::set_permissions(&l.prefix, std::fs::Permissions::from_mode(0o750)).unwrap();
        drop(try_lock_store(&l).expect("third acquisition"));
        let mode = std::fs::symlink_metadata(&l.prefix).unwrap().mode();
        assert_eq!(mode & 0o7777, 0o700, "a drifted mode is hardened");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn unusable_prefix_refuses_instead_of_passing_lock_free() {
        let l = temp_layout("badprefix");
        std::fs::write(&l.prefix, b"not a directory").unwrap();
        let err = match try_lock_store(&l) {
            Ok(_) => panic!("a file-shaped prefix cannot yield a store lock"),
            Err(e) => e,
        };
        assert!(
            matches!(err, StoreLockError::Io(..)),
            "refusal is Io: {err:?}"
        );
        let _ = std::fs::remove_file(&l.prefix);
    }

    /// A prefix that is a SYMLINK is refused (F16) as `PermissionDenied`, and nothing is
    /// opened through the link.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_prefix_is_refused_and_never_followed() {
        let l = temp_layout("linkprefix");
        let target = temp_layout("linkprefix-target");
        std::fs::create_dir_all(&target.prefix).unwrap();
        std::os::unix::fs::symlink(&target.prefix, &l.prefix).unwrap();
        let err = match try_lock_store(&l) {
            Ok(_) => panic!("a symlinked prefix cannot yield a store lock"),
            Err(e) => e,
        };
        assert!(
            matches!(&err, StoreLockError::Io(_, e) if e.kind() == io::ErrorKind::PermissionDenied),
            "the F16 refusal keeps its kind: {err:?}"
        );
        assert!(
            !target.store_lock().exists(),
            "nothing was opened through the link"
        );
        let _ = std::fs::remove_file(&l.prefix);
        let _ = std::fs::remove_dir_all(&target.prefix);
    }

    #[test]
    fn lock_store_waiting_acquires_when_the_holder_drops_and_stays_silent_inside_the_grace() {
        let a = temp_layout("wait-drop");
        let b = Layout {
            prefix: a.prefix.clone(),
        };
        let guard = std::cell::RefCell::new(Some(
            try_lock_store(&a).expect("first acquisition succeeds"),
        ));
        let polls = std::cell::Cell::new(0u32);
        let announced = std::cell::Cell::new(0u32);
        let got = lock_store_waiting_on(
            &ManualClock::new(),
            &b,
            Duration::from_secs(120),
            WAIT_ANNOUNCE_GRACE,
            |_| announced.set(announced.get() + 1),
            || {
                polls.set(polls.get() + 1);
                drop(guard.borrow_mut().take());
                true
            },
        );
        assert!(
            got.is_ok(),
            "the waiter takes the lock once the holder drops"
        );
        assert_eq!(polls.get(), 1, "it actually waited, one poll");
        assert_eq!(
            announced.get(),
            0,
            "a wait shorter than the grace announces nothing"
        );
        drop(got);
        let _ = std::fs::remove_dir_all(&a.prefix);
    }

    #[test]
    fn lock_store_waiting_announces_once_after_the_grace_then_acquires() {
        let a = temp_layout("wait-announce");
        let b = Layout {
            prefix: a.prefix.clone(),
        };
        let guard = try_lock_store(&a).expect("first acquisition succeeds");
        let clock = ManualClock::new();
        let announced = std::cell::RefCell::new(Vec::<(PathBuf, Duration)>::new());
        let got = lock_store_waiting_on(
            &clock,
            &b,
            Duration::from_secs(120),
            WAIT_ANNOUNCE_GRACE,
            |p| {
                announced
                    .borrow_mut()
                    .push((p.to_path_buf(), clock.elapsed()));
                drop(guard);
            },
            || true,
        );
        assert!(got.is_ok(), "acquired after the holder dropped");
        assert_eq!(
            announced.borrow().as_slice(),
            &[(a.store_lock(), WAIT_ANNOUNCE_GRACE)],
            "announced once, with the lock path, at the poll the grace ran out on"
        );
        drop(got);
        let _ = std::fs::remove_dir_all(&a.prefix);
    }

    #[test]
    fn lock_store_waiting_times_out_with_contended() {
        let a = temp_layout("wait-timeout");
        let b = Layout {
            prefix: a.prefix.clone(),
        };
        let _guard = try_lock_store(&a).expect("first acquisition succeeds");
        let timeout = WAIT_ANNOUNCE_GRACE + Duration::from_secs(1);
        let announced = std::cell::Cell::new(0u32);
        let clock = ManualClock::new();
        let err = match lock_store_waiting_on(
            &clock,
            &b,
            timeout,
            WAIT_ANNOUNCE_GRACE,
            |_| announced.set(announced.get() + 1),
            || true,
        ) {
            Ok(_) => panic!("a lock held for the whole wait must refuse"),
            Err(e) => e,
        };
        assert!(
            matches!(err, StoreLockError::Contended(ref p) if *p == a.store_lock()),
            "the timeout is a contention refusal naming the path: {err:?}"
        );
        assert_eq!(announced.get(), 1, "announced exactly once per wait");
        assert_eq!(
            clock.elapsed(),
            timeout,
            "it waited out its bound, and no more"
        );
        let _ = std::fs::remove_dir_all(&a.prefix);
    }

    /// The one poll that DOES vet again: a prefix removed under the wait (the
    /// holder's `gc`, a hand `rm -rf`) makes the re-open fail `NotFound`, and that
    /// poll is the full acquisition — the prefix comes back hardened and the lock
    /// is taken on the real, new file rather than an orphan inode.
    #[cfg(unix)]
    #[test]
    fn lock_store_waiting_re_vets_a_prefix_that_vanished_under_it() {
        let a = temp_layout("wait-vanish");
        let b = Layout {
            prefix: a.prefix.clone(),
        };
        let guard = try_lock_store(&a).expect("first acquisition succeeds");
        let aside = PathBuf::from(format!("{}-gone", a.prefix.display()));
        let _ = std::fs::remove_dir_all(&aside);
        let clock = ManualClock::at_sleep(|n| {
            if n == 1 {
                std::fs::rename(&a.prefix, &aside).expect("the prefix is movable under a holder");
            }
        });
        let got = lock_store_waiting_on(
            &clock,
            &b,
            aterm_spec::wait::MUST_HAPPEN,
            WAIT_ANNOUNCE_GRACE,
            |_| {},
            || true,
        );
        assert!(
            got.is_ok(),
            "the re-created prefix's lock is free: {:?}",
            got.err()
        );
        assert!(
            b.store_lock().is_file(),
            "store.lock re-created under the prefix"
        );
        let dir_mode = std::fs::metadata(&b.prefix).unwrap().permissions().mode();
        assert_eq!(dir_mode & 0o777, 0o700, "the re-created prefix is hardened");
        drop(clock);
        drop(guard);
        drop(got);
        let _ = std::fs::remove_dir_all(&a.prefix);
        let _ = std::fs::remove_dir_all(&aside);
    }

    /// A WAITING POLL RE-VETS THE PREFIX'S SHAPE, NOT ONLY ITS PRESENCE (F16, the
    /// symlinked-prefix refusal).
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn lock_store_waiting_refuses_a_prefix_swapped_for_a_symlink_under_it() {
        fn exchange(a: &Path, b: &Path) -> io::Result<()> {
            use std::os::unix::ffi::OsStrExt as _;
            let a = std::ffi::CString::new(a.as_os_str().as_bytes())?;
            let b = std::ffi::CString::new(b.as_os_str().as_bytes())?;
            // SAFETY: both C strings are NUL-terminated and outlive the call.
            #[cfg(target_os = "macos")]
            let rc = unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) };
            // SAFETY: as above; `AT_FDCWD` resolves both paths from the cwd.
            #[cfg(target_os = "linux")]
            let rc = unsafe {
                libc::renameat2(
                    libc::AT_FDCWD,
                    a.as_ptr(),
                    libc::AT_FDCWD,
                    b.as_ptr(),
                    libc::RENAME_EXCHANGE,
                )
            };
            if rc == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
        for inside_a_poll in [false, true] {
            let a = temp_layout("wait-symlink");
            let b = Layout {
                prefix: a.prefix.clone(),
            };
            let guard = try_lock_store(&a).expect("first acquisition succeeds");
            let elsewhere = temp_layout("wait-symlink-target");
            std::fs::create_dir_all(&elsewhere.prefix).unwrap();
            let moved = PathBuf::from(format!("{}-moved", a.prefix.display()));
            let _ = std::fs::remove_dir_all(&moved);
            std::os::unix::fs::symlink(&elsewhere.prefix, &moved)
                .expect("the link, beside the prefix");
            let swap = {
                let (moved, prefix) = (moved.clone(), a.prefix.clone());
                move || exchange(&moved, &prefix).expect("the prefix and the link exchange names")
            };
            let polls = std::cell::Cell::new(0u32);
            let swap = std::cell::Cell::new(Some(swap));
            let still_wanted = || {
                if polls.replace(polls.get() + 1) == 0 {
                    let swap = swap.take().expect("one swap");
                    if inside_a_poll {
                        BETWEEN_VET_AND_OPEN.with(|slot| *slot.borrow_mut() = Some(Box::new(swap)));
                    } else {
                        swap();
                    }
                }
                true
            };
            let err = match lock_store_waiting_on(
                &ManualClock::new(),
                &b,
                Duration::from_secs(120),
                WAIT_ANNOUNCE_GRACE,
                |_| {},
                still_wanted,
            ) {
                Ok(_) => {
                    panic!("inside a poll {inside_a_poll}: the waiter locked THROUGH the link")
                }
                Err(e) => e,
            };
            let unrun = BETWEEN_VET_AND_OPEN.with(|slot| slot.borrow_mut().take().is_some());
            assert!(!unrun, "the poll after the first ran the swap");
            assert_eq!(
                polls.get(),
                1,
                "inside a poll {inside_a_poll}: refused at that poll"
            );
            assert!(
                matches!(
                    err,
                    StoreLockError::Io(_, ref e)
                        if e.kind() == io::ErrorKind::PermissionDenied
                            && e.to_string().contains("is a symlink; refusing")
                ),
                "inside a poll {inside_a_poll}: the F16 refusal at the poll that saw the link, \
                 never the bound's contention: {err:?}"
            );
            assert!(
                !elsewhere.store_lock().exists(),
                "inside a poll {inside_a_poll}: nothing was opened through the link"
            );
            drop(guard);
            let _ = std::fs::remove_file(&a.prefix);
            let _ = std::fs::remove_dir_all(&moved);
            let _ = std::fs::remove_dir_all(&elsewhere.prefix);
        }
    }

    #[test]
    fn lock_store_waiting_never_waits_on_io() {
        let l = temp_layout("wait-badprefix");
        std::fs::write(&l.prefix, b"not a directory").unwrap();
        let clock = ManualClock::new();
        let announced = std::cell::Cell::new(0u32);
        let polls = std::cell::Cell::new(0u32);
        let err = match lock_store_waiting_on(
            &clock,
            &l,
            Duration::from_secs(120),
            WAIT_ANNOUNCE_GRACE,
            |_| announced.set(announced.get() + 1),
            || {
                polls.set(polls.get() + 1);
                true
            },
        ) {
            Ok(_) => panic!("a file-shaped prefix cannot yield a store lock"),
            Err(e) => e,
        };
        assert!(matches!(err, StoreLockError::Io(..)), "{err:?}");
        assert_eq!(polls.get(), 0, "no wait on Io: the first attempt's refusal");
        assert_eq!(clock.elapsed(), Duration::ZERO, "no wait on Io: no pause");
        assert_eq!(announced.get(), 0);
        let _ = std::fs::remove_file(&l.prefix);
    }

    #[test]
    fn lock_store_waiting_stands_down_when_no_longer_wanted() {
        let a = temp_layout("wait-orphan");
        let b = Layout {
            prefix: a.prefix.clone(),
        };
        let _guard = try_lock_store(&a).expect("first acquisition succeeds");
        let polls = std::cell::Cell::new(0u32);
        let announced = std::cell::Cell::new(0u32);
        let clock = ManualClock::new();
        let err = match lock_store_waiting_on(
            &clock,
            &b,
            Duration::from_secs(120),
            WAIT_ANNOUNCE_GRACE,
            |_| announced.set(announced.get() + 1),
            || {
                polls.set(polls.get() + 1);
                polls.get() < 2
            },
        ) {
            Ok(_) => panic!("held lock"),
            Err(e) => e,
        };
        assert!(matches!(err, StoreLockError::Contended(..)), "{err:?}");
        assert_eq!(polls.get(), 2, "stood down at the poll that found it gone");
        assert_eq!(clock.elapsed(), WAIT_POLL, "one poll in, not at the bound");
        assert_eq!(
            announced.get(),
            0,
            "stood down inside the grace: nothing to announce"
        );
        let _ = std::fs::remove_dir_all(&a.prefix);
    }

    #[test]
    fn lock_store_waiting_never_announces_to_a_caller_that_has_gone() {
        let a = temp_layout("wait-orphan-late");
        let b = Layout {
            prefix: a.prefix.clone(),
        };
        let _guard = try_lock_store(&a).expect("first acquisition succeeds");
        let clock = ManualClock::new();
        let wanted = std::cell::Cell::new(0u32);
        let announced = std::cell::Cell::new(0u32);
        let err = match lock_store_waiting_on(
            &clock,
            &b,
            Duration::from_secs(30),
            WAIT_ANNOUNCE_GRACE,
            |_| announced.set(announced.get() + 1),
            || {
                let still = clock.elapsed() < WAIT_ANNOUNCE_GRACE;
                wanted.set(wanted.get() + u32::from(still));
                still
            },
        ) {
            Ok(_) => panic!("held lock"),
            Err(e) => e,
        };
        assert!(matches!(err, StoreLockError::Contended(..)), "{err:?}");
        assert!(wanted.get() > 0, "it waited while it was wanted");
        assert_eq!(
            clock.elapsed(),
            WAIT_ANNOUNCE_GRACE,
            "it stood down at the poll the grace ran out on"
        );
        assert_eq!(
            announced.get(),
            0,
            "a caller that has gone is never announced to"
        );
        let _ = std::fs::remove_dir_all(&a.prefix);
    }
}
