// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lock-free reads of another program's live SQLite database ([`read`], [`immutable_uri`],
//! [`Unread`]).

use std::path::{Path, PathBuf};

/// Why [`read`] has no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unread {
    /// No database there: nothing, or no regular file (a link is never followed).
    Missing,
    /// A rollback journal with something in it stands beside the database.
    Writing,
    /// One of its files changed while it was read: what was read could mix two states.
    Changed,
    /// The copy could not be made, a file could not be looked at, or the reader gave no answer.
    Failed,
}

/// What identifies one file's content.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    regular: bool,
}

#[cfg(unix)]
fn identity_of(m: &std::fs::Metadata) -> Identity {
    use std::os::unix::fs::MetadataExt as _;
    Identity {
        dev: m.dev(),
        ino: m.ino(),
        len: m.len(),
        modified: (m.mtime(), m.mtime_nsec()),
        changed: (m.ctime(), m.ctime_nsec()),
        regular: m.file_type().is_file(),
    }
}

#[cfg(not(unix))]
fn identity_of(m: &std::fs::Metadata) -> Identity {
    let nanos = |t: std::io::Result<std::time::SystemTime>| {
        t.ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or((0, 0), |d| {
                (
                    i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
                    i64::from(d.subsec_nanos()),
                )
            })
    };
    Identity {
        dev: 0,
        ino: 0,
        len: m.len(),
        modified: nanos(m.modified()),
        changed: nanos(m.created()),
        regular: m.file_type().is_file(),
    }
}

/// The identity of whatever is at `p` itself: `None` when nothing is there.
fn identity(p: &Path) -> Result<Option<Identity>, Unread> {
    match std::fs::symlink_metadata(p) {
        Ok(m) => Ok(Some(identity_of(&m))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Unread::Failed),
    }
}

/// `db` with `suffix` after its whole name (`x.db` → `x.db-wal`).
fn beside(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// The database's files at one look.
#[derive(Debug, PartialEq, Eq)]
struct Files {
    db: Identity,
    wal: Option<Identity>,
    journal: Option<Identity>,
}

/// One look at `db`, its `-wal` and its `-journal`.
fn look(db: &Path) -> Result<Files, Unread> {
    let Some(main) = identity(db)?.filter(|id| id.regular) else {
        return Err(Unread::Missing);
    };
    let wal = identity(&beside(db, "-wal"))?;
    let journal = identity(&beside(db, "-journal"))?;
    if journal.as_ref().is_some_and(|j| j.len > 0 || !j.regular) {
        return Err(Unread::Writing);
    }
    Ok(Files {
        db: main,
        wal,
        journal,
    })
}

/// `db` as an IMMUTABLE file URI (`file:<path>?immutable=1`).
#[must_use]
pub fn immutable_uri(db: &Path) -> String {
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt as _;
        db.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let bytes = db.to_string_lossy().into_owned().into_bytes();
    let mut uri = String::from("file:");
    for b in bytes {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            uri.push(char::from(b));
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri.push_str("?immutable=1");
    uri
}

/// Open `p` to read, never through a link.
fn open_no_link(p: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(p)
}

/// Copy `from` to the new file `to` with plain reads (no lock of any kind).
fn copy_unlocked(from: &Path, to: &Path, was: &Identity) -> Result<(), Unread> {
    let mut src = open_no_link(from).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Unread::Changed
        } else {
            Unread::Failed
        }
    })?;
    let opened = src.metadata().map_err(|_| Unread::Failed)?;
    if identity_of(&opened) != *was {
        return Err(Unread::Changed);
    }
    let mut dst = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|_| Unread::Failed)?;
    std::io::copy(&mut src, &mut dst).map_err(|_| Unread::Failed)?;
    Ok(())
}

/// A directory of this process's own for one copy.
struct Private(PathBuf);

impl Private {
    fn new() -> Result<Self, Unread> {
        let token = crate::rand::hex_token::<16>().map_err(|_| Unread::Failed)?;
        let path = std::env::temp_dir().join(format!("aterm-foreign-db-{token}"));
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt as _;
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = std::fs::DirBuilder::new();
        builder.create(&path).map_err(|_| Unread::Failed)?;
        match std::fs::canonicalize(&path) {
            Ok(real) => Ok(Self(real)),
            Err(_) => {
                let _ = std::fs::remove_dir(&path);
                Err(Unread::Failed)
            }
        }
    }
}

impl Drop for Private {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// READ `db`, ANOTHER PROGRAM'S LIVE SQLITE DATABASE, WITH NO LOCK ON IT (the module's rule).
///
/// # Errors
/// [`Unread`]: missing database, active write, concurrent change, or reader failure.
pub fn read<T>(db: &Path, reader: impl FnOnce(&str) -> Option<T>) -> Result<T, Unread> {
    let before = look(db)?;
    let answer = if let Some(wal) = &before.wal {
        let dir = Private::new()?;
        let copy = dir.0.join("db");
        copy_unlocked(db, &copy, &before.db)?;
        copy_unlocked(&beside(db, "-wal"), &beside(&copy, "-wal"), wal)?;
        let Some(name) = copy.to_str() else {
            return Err(Unread::Failed);
        };
        reader(name)
    } else {
        reader(&immutable_uri(db))
    };
    if look(db)? != before {
        return Err(Unread::Changed);
    }
    answer.ok_or(Unread::Failed)
}

/// THE LOCK WITNESS, a test seam (test and debug builds, Unix).
#[cfg(all(unix, any(test, debug_assertions)))]
pub mod witness {
    use std::path::Path;

    /// The pid of a process other than this one holding a lock on `file`'s bytes `[start.
    fn holder(file: &std::fs::File, start: u64, len: u64) -> Option<i32> {
        use std::os::fd::AsRawFd as _;
        // SAFETY: A zeroed `flock` is a valid value of the plain C struct.
        let mut fl: libc::flock = unsafe { std::mem::zeroed() };
        fl.l_type = libc::F_WRLCK as _;
        fl.l_whence = libc::SEEK_SET as _;
        fl.l_start = i64::try_from(start).ok()?;
        fl.l_len = i64::try_from(len).ok()?;
        // SAFETY: `fl` is a valid, writable `flock` for the call's duration.
        let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &raw mut fl) };
        (rc == 0 && fl.l_type != libc::F_UNLCK as _).then_some(fl.l_pid)
    }

    /// Sends `SIGCONT` to the stopped process when dropped.
    struct Thaw(i32);

    impl Drop for Thaw {
        fn drop(&mut self) {
            // SAFETY: `kill` takes plain integers.
            unsafe { libc::kill(self.0, libc::SIGCONT) };
        }
    }

    /// RUN `read` ON ITS OWN THREAD AND WATCH ITS LOCKS.
    ///
    /// # Panics
    /// When `read` panicked.
    pub fn while_read<R: Send, W>(
        file: &Path,
        start: u64,
        len: u64,
        read: impl FnOnce() -> R + Send,
        write: impl FnOnce() -> W,
    ) -> (R, Option<(i32, W)>) {
        let me = i32::try_from(std::process::id()).unwrap_or(i32::MAX);
        let mut write = Some(write);
        let mut caught = None;
        let answer = std::thread::scope(|s| {
            let reading = s.spawn(read);
            let mut open = None;
            while !reading.is_finished() && caught.is_none() {
                if open.is_none() {
                    open = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(file)
                        .ok();
                }
                let Some(pid) = open
                    .as_ref()
                    .and_then(|f| holder(f, start, len))
                    .filter(|pid| *pid > 1 && *pid != me)
                else {
                    std::hint::spin_loop();
                    continue;
                };
                // SAFETY: `kill` takes plain integers.
                unsafe { libc::kill(pid, libc::SIGSTOP) };
                let thaw = Thaw(pid);
                if open.as_ref().and_then(|f| holder(f, start, len)) == Some(pid)
                    && let Some(write) = write.take()
                {
                    caught = Some((pid, write()));
                }
                drop(thaw);
            }
            reading.join().expect("the read")
        });
        (answer, caught)
    }
}

#[cfg(test)]
#[path = "foreign_db_tests.rs"]
mod tests;
