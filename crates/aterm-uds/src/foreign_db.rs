// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ANOTHER PROGRAM'S LIVE SQLITE DATABASE, READ WITH NO LOCK ON IT.
//!
//! A reader that opens a live database through SQLite joins its locking, and a
//! lock a writer meets fails that writer AT ONCE unless the writer set a busy
//! timeout: its write is then lost. Measured on Darwin 27.0.0 with Apple's
//! `libsqlite3` 3.54.0 (`docs/measured/real-claude-lab-2026-10-15.md`, the
//! 2026-10-20 section), in both journal modes:
//!
//! * A ROLLBACK JOURNAL (the system `TCC.db`, whose writer, `tccd`, sets no busy
//!   timeout and writes `BEGIN`/`COMMIT`): every reader holds `SHARED` while it
//!   reads, and a writer's `COMMIT` needs `EXCLUSIVE`. A reader's lock held as a
//!   writer shaped as `tccd` commits failed that `COMMIT` `database is locked`
//!   every time, and a loop of the Developer Tools read (`sqlite3 -readonly`)
//!   lost 323 of 5 000 such writes.
//! * WAL (agy's `conversation_summaries.db`): a reader that finds the WAL index
//!   unbuilt (its `-shm` just made by an opening writer, or left by one that
//!   died) takes the index's write lock to build it, which a writer with no busy
//!   timeout meets as `database is locked`. agy sets `_busy_timeout=5000` and
//!   waits; past 5 s it logs `summary store: sync upsert failed: ... database is
//!   locked` and drops that write for good.
//!
//! So aterm never opens another program's live database through SQLite. [`read`]
//! hands the reader bytes taken with plain reads: with no `-wal` beside the file,
//! the file itself as an IMMUTABLE URI ([`immutable_uri`]: SQLite reads it with no
//! lock and opens no `-wal`, `-shm` or journal); with one, a PRIVATE COPY of the
//! file and its `-wal` (never the `-shm`) in a directory of this process's own,
//! where the reader builds an index of its own from the copied `-wal` — SQLite's
//! own recovery, which keeps only committed frames. The answer is kept only when
//! every one of those files is the same after the read as before (device, inode,
//! length, modified and changed times) and no rollback journal with anything in it
//! stood beside them at either look: a write that landed meanwhile could have
//! mixed two states into what was read, and a journal is a write under way (or one
//! that died mid-way and is not rolled back yet). Otherwise the read is no answer
//! ([`Unread`]) and the caller reads again at its next look; nothing is ever
//! written beside the live file.

use std::path::{Path, PathBuf};

/// Why [`read`] has no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unread {
    /// No database there: nothing, or no regular file (a link is never followed).
    Missing,
    /// A rollback journal with something in it stands beside the database: a
    /// write is under way, or one died mid-way and is not rolled back yet, so its
    /// bytes are no committed state.
    Writing,
    /// One of its files changed while it was read: what was read could mix two
    /// states.
    Changed,
    /// The copy could not be made, a file could not be looked at, or the reader
    /// gave no answer.
    Failed,
}

/// What identifies one file's content: its device, inode, length, modified and
/// changed times (seconds and nanoseconds), and whether it is a regular file.
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

/// The identity of whatever is at `p` itself (a link's own, never its target's):
/// `None` when nothing is there.
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

/// One look at `db`, its `-wal` and its `-journal` (the `-shm` is never looked
/// at: no reader of ours opens it).
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

/// `db` as an IMMUTABLE file URI (`file:<path>?immutable=1`), every byte but the
/// path's plain ones percent-encoded: SQLite reads it with no lock, opens no
/// `-wal`, `-shm` or journal, and writes nothing.
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

/// Copy `from` to the new file `to` with plain reads (no lock of any kind),
/// refusing a file that is not the one looked at (`was`).
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

/// A directory of this process's own for one copy: mode 0700 on Unix, an
/// unpredictable name under the temporary directory, named with no symbolic link
/// in its path (a reader may open with `-nofollow`, which refuses any: macOS's
/// temporary directory is under the link `/var`), removed with everything in it
/// when dropped.
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

/// READ `db`, ANOTHER PROGRAM'S LIVE SQLITE DATABASE, WITH NO LOCK ON IT (the
/// module's rule): `reader` gets the SQLite filename to open — the file itself as
/// an immutable URI while no `-wal` stands beside it, else the path of a private
/// copy of the file and its `-wal`, opened by `reader` as it likes (it may build
/// its own index there; the directory is gone once `reader` returns). Its answer
/// is kept only when the files are the same after it as before and no rollback
/// journal stood beside them.
///
/// # Errors
/// [`Unread`]: no database file, a write under way, a change during the read, or
/// a copy or reader that failed.
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

/// THE LOCK WITNESS, a test seam (test and debug builds, Unix): whether a read
/// held a lock a writer would meet, and what a writer met then.
#[cfg(all(unix, any(test, debug_assertions)))]
pub mod witness {
    use std::path::Path;

    /// The pid of a process other than this one holding a lock on `file`'s bytes
    /// `[start, start + len)` that a writer's exclusive lock there would meet
    /// (`F_GETLK`, which takes nothing), if one does now.
    fn holder(file: &std::fs::File, start: u64, len: u64) -> Option<i32> {
        use std::os::fd::AsRawFd as _;
        // SAFETY: a zeroed `flock` is a valid value of the plain C struct.
        let mut fl: libc::flock = unsafe { std::mem::zeroed() };
        fl.l_type = libc::F_WRLCK as _;
        fl.l_whence = libc::SEEK_SET as _;
        fl.l_start = i64::try_from(start).ok()?;
        fl.l_len = i64::try_from(len).ok()?;
        // SAFETY: `fl` is a valid, writable `flock` for the call's duration, and
        // the descriptor is open.
        let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &raw mut fl) };
        (rc == 0 && fl.l_type != libc::F_UNLCK as _).then_some(fl.l_pid)
    }

    /// Sends `SIGCONT` to the stopped process when dropped, so a panicking
    /// `write` never leaves it frozen.
    struct Thaw(i32);

    impl Drop for Thaw {
        fn drop(&mut self) {
            // SAFETY: `kill` takes plain integers.
            unsafe { libc::kill(self.0, libc::SIGCONT) };
        }
    }

    /// RUN `read` ON ITS OWN THREAD AND WATCH ITS LOCKS: while it runs, ask the
    /// kernel again and again whether a process other than this one holds a lock
    /// on `file`'s bytes `[start, start + len)` that a writer's exclusive lock
    /// there would meet. The first time one does, that process is stopped
    /// (`SIGSTOP`); if the lock is still its own, `write` runs while it stands
    /// frozen, holding it; then it runs on (`SIGCONT`). Returns `read`'s answer
    /// and, when a holder was caught, its pid and `write`'s answer. `file` is a
    /// scratch file of the test's own, so only a process the test started can
    /// hold a lock on it (a lock this process takes is never reported to it).
    /// `file` is opened read-write once it exists, and closed at the end: this
    /// process holds no lock on it to lose.
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
