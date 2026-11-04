// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deadline-bounded exclusive file-lock contention waits on Unix and Windows.

use std::io;
use std::time::{Duration, Instant};

/// Acquire an exclusive lock on an already validated file, waiting only for
/// actual lock contention until `deadline`. A first attempt is always made,
/// including when the deadline has passed. Other I/O errors return immediately.
/// File admission and any protocol-specific identity recheck belong to the caller.
/// The caller must explicitly unlock before closing or transferring ownership.
///
/// # Errors
/// Returns `WouldBlock` with `busy` when the lock remains held at the deadline,
/// or the original error of attempting the lock.
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
    mut held: impl FnMut(),
    now: impl Fn() -> Instant,
    mut sleep: impl FnMut(Duration),
) -> io::Result<()> {
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
        let taken = file.try_lock();
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

/// An exclusive lock on a file of its own that guards something beside it (a record that is
/// replaced whole by a rename, so its own inode is no place for a lock), taken by
/// [`LockFile::take`] and let go by `LOCK_UN` as it drops, before the close: clippy.toml's flock
/// release rule.
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
    /// `path` included.
    pub fn take(path: &std::path::Path, deadline: Instant, busy: &str) -> io::Result<Self> {
        Self::lock(open_or_make(path)?, path, deadline, busy)
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
        lock_exclusive_until(&file, deadline, busy)?;
        Ok(Self(file))
    }
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
        making.mode(0o600);
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
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
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
mod tests {
    use super::*;

    #[test]
    fn existing_only_lock_never_recreates_a_removed_slot() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("slot");
        assert_eq!(
            LockFile::take_existing(&path, Instant::now(), "busy")
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert!(!path.exists());
        std::fs::write(&path, b"retained").unwrap();
        let held = LockFile::take_existing(&path, Instant::now(), "busy").unwrap();
        assert_eq!(
            LockFile::take_existing(&path, Instant::now(), "busy")
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"retained");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            LockFile::take_existing(&path, Instant::now(), "busy")
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert!(!path.exists());
        drop(held);
    }

    /// One holder at a time: a second take while the first holds it waits until its deadline and
    /// says `busy`, and takes it once the first let go; a missing file is made 0600, one there is
    /// taken whatever its mode, and a folder is no lock file.
    #[test]
    fn a_lock_file_has_one_holder() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("record.lock");
        let first = LockFile::take(&path, Instant::now(), "busy").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let refused = LockFile::take(&path, Instant::now(), "busy").unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(refused.to_string(), "busy");
        drop(first);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        drop(LockFile::take(&path, Instant::now(), "busy").unwrap());
        assert!(
            LockFile::take(dir.path(), Instant::now(), "busy").is_err(),
            "a folder is no lock file"
        );
    }

    /// A link at the path is never followed (`O_NOFOLLOW`): a take through a link to a regular
    /// file there fails with `ELOOP`, and that file is never locked through it, so a take of the
    /// file itself, while the take through the link is held, gets it at once.
    #[test]
    fn a_link_at_the_path_is_never_followed_to_the_file_it_names() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, b"").unwrap();
        let linked = dir.path().join("linked.lock");
        std::os::unix::fs::symlink(&target, &linked).unwrap();
        let through = LockFile::take(&linked, Instant::now(), "busy");
        let direct = LockFile::take(&target, Instant::now(), "busy");
        assert!(
            direct.is_ok(),
            "the target was locked through the link: {direct:?}"
        );
        assert_eq!(
            through.as_ref().map_err(io::Error::raw_os_error).err(),
            Some(Some(libc::ELOOP)),
            "{through:?}"
        );
    }

    /// A FIFO at the path is never waited on (`O_NONBLOCK`): the take returns at once, refused as
    /// no regular file, where a read-only open without that flag waits for a writer that never
    /// comes. The take is a one-minute hang detector away from the test.
    #[test]
    fn a_fifo_at_the_path_is_refused_at_once() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let fifo = dir.path().join("fifo.lock");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `c` is a valid NUL-terminated path in the test's own scratch folder.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo");
        let (tx, rx) = std::sync::mpsc::channel();
        let at = fifo.clone();
        std::thread::spawn(move || {
            let _ = tx.send(LockFile::take(&at, Instant::now(), "busy").map(drop));
        });
        let taken = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the take of a FIFO returned (a hang detector)");
        assert_eq!(taken.map_err(|e| e.kind()), Err(io::ErrorKind::InvalidData));
    }
}
