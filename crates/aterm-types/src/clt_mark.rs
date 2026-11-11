// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The account's durable record of an administrator-password attempt. A valid record holds
//! every automatic asker, across launches and software updates. Explicit person retries remain
//! available. `opened <build>` retains the exact claim owner for a proven preprompt release;
//! historical multi-line and bare `opened` records also retain the hold.
//!
//! Window and terminal callers share one record and one lock. A claim succeeds only when the
//! record is absent, so concurrent askers from different builds cannot each open a prompt.
//! Cancellation, refused passwords, failed installs and unknown child outcomes keep the record.
//! Only the exact owner whose child provably never reached the prompt may release its line.
//! Unreadable or malformed records hold automatic asks and are never rewritten.

use std::io;
use std::path::{Path, PathBuf};

/// The record's file name, in this account's aterm state ([`crate::dirs::account_state_dir`]).
pub const FILE: &str = "command-line-tools-tab";

/// The maximum historical record length; new claims write one owner line.
pub const LINES: usize = 8;

/// The longest build id a line names; aterm's are far shorter (a version, `+g` and a commit).
pub const BUILD_BYTES: usize = 96;

/// The longest record read or written: [`LINES`] `opened <build>` lines.
pub const BYTES: usize = LINES * ("opened ".len() + BUILD_BYTES + 1);

/// Hang detector while a change of the record waits for another asker's durable write
/// ([`Live`]'s lock). Scheduling delay alone must not spend the automatic ask.
#[cfg(not(target_family = "wasm"))]
pub const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// This account's record: [`FILE`] in its aterm state root ([`crate::dirs::account_state_dir`]:
/// `~/Library/Application Support/aterm` on macOS, which `ATERM_STATE_HOME` isolates for a lab
/// instance). It reads no `XDG_CONFIG_HOME`, which a shell may export while the window, opened by
/// launchd, has none, so the window and the follow-up `aterm init` of a person's own package
/// install or update, each in its own environment, claim in one file under one lock. `None` where
/// the state root has no place (no home).
#[must_use]
pub fn path() -> Option<PathBuf> {
    crate::dirs::account_state_dir().map(|dir| dir.join(FILE))
}

/// An exclusive hold on the lock beside a record ([`Files::lock`]), let go when it drops.
pub struct Held {
    _guard: Box<dyn Send>,
}

impl Held {
    /// A hold that `guard` keeps until it drops.
    #[must_use]
    pub fn new(guard: impl Send + 'static) -> Self {
        Self {
            _guard: Box::new(guard),
        }
    }
}

/// The record's file I/O: this Mac's ([`Live`]), or a test double's own files.
pub trait Files {
    /// The text of the regular file at `path`, at most `max` bytes, read without following a link
    /// or waiting on a pipe; `None` where there is none.
    ///
    /// # Errors
    /// Anything else at `path` (a link, a folder, a FIFO), a file longer than `max` bytes or not
    /// UTF-8, and the read's own error.
    fn read(&self, path: &Path, max: usize) -> io::Result<Option<String>>;

    /// Replace the file at `path` whole with `text`, durably, its directory entry included, making
    /// its folder where there is none.
    ///
    /// # Errors
    /// A path held by anything but a regular file, which is left as it is, and the write's own
    /// error, which leaves the file as it was.
    fn write(&self, path: &Path, text: &str) -> io::Result<()>;

    /// Remove the file at `path`; none there is no error.
    ///
    /// # Errors
    /// The removal's own.
    fn remove(&self, path: &Path) -> io::Result<()>;

    /// Hold the lock that every change of the record at `path` is made under ([`claim`],
    /// [`forget`]), waiting while another asker, in this process or another, holds it, until the
    /// hold drops. Its folder is made where there is none.
    ///
    /// # Errors
    /// No hold within the wait, and the lock's own error.
    fn lock(&self, path: &Path) -> io::Result<Held>;
}

/// What the account's record says about automatic password requests.
#[derive(Debug)]
pub enum Asked {
    /// A previous attempt consumed the automatic ask, including on another build.
    Spent,
    /// No record exists.
    Available,
    /// The record cannot be read safely; automatic requests remain held.
    Unread(io::Error),
}

/// Read the durable episode, independently of the running build.
#[must_use]
pub fn reading(files: &dyn Files, path: &Path) -> Asked {
    match files.read(path, BYTES) {
        Ok(None) => Asked::Available,
        Ok(Some(text)) => match builds(&text) {
            Some(_) => Asked::Spent,
            None => Asked::Unread(malformed()),
        },
        Err(error) => Asked::Unread(error),
    }
}

/// Whether a previous attempt, or an unreadable record, holds automatic requests.
#[must_use]
pub fn spent(files: &dyn Files, path: &Path) -> bool {
    !matches!(reading(files, path), Asked::Available)
}

/// Claim the absent automatic-ask episode under the shared lock. The durable owner is `build`.
/// Returns false without rewriting any existing valid record, regardless of its owner.
///
/// # Errors
/// An unsafe or malformed record, an invalid owner, or a lock/write error.
pub fn claim(files: &dyn Files, path: &Path, build: &str) -> io::Result<bool> {
    change(files, path, build, Change::Claim)
}

/// Record an explicit retry before it can ask. An absent episode receives an exact owner and
/// returns true. An existing episode becomes an anonymous sticky hold and returns false: no
/// older in-flight asker's preprompt release may erase this person's attempt.
///
/// # Errors
/// As [`claim`]. Explicit callers keep their person-granted authority on recording failure.
pub fn explicit(files: &dyn Files, path: &Path, build: &str) -> io::Result<bool> {
    change(files, path, build, Change::Explicit)
}

/// Release exactly `build`'s line after proving its attempt never reached a password prompt.
/// Other historical owners and anonymous holds are retained.
///
/// # Errors
/// An unsafe or malformed record, an invalid owner, or a lock/write/removal error.
pub fn forget(files: &dyn Files, path: &Path, build: &str) -> io::Result<()> {
    change(files, path, build, Change::Forget).map(|_| ())
}

/// A change of the record ([`change`]).
#[derive(Clone, Copy)]
enum Change {
    /// Add the build's line where there is none ([`claim`]).
    Claim,
    /// Revoke older release capabilities before an explicit retry.
    Explicit,
    /// Take the build's line out ([`forget`]).
    Forget,
}

/// Make `how` to `build`'s line in the record at `path` under the record's lock: `true` when the
/// record changed.
fn change(files: &dyn Files, path: &Path, build: &str, how: Change) -> io::Result<bool> {
    use std::fmt::Write as _;
    if !nameable(build) {
        return Err(unnameable());
    }
    let _held = files.lock(path)?;
    let text = files.read(path, BYTES)?;
    let mut builds = match text.as_deref() {
        None => Vec::new(),
        Some(text) => builds(text).ok_or_else(malformed)?,
    };
    let named = builds.contains(&Some(build));
    match how {
        Change::Claim if !builds.is_empty() => return Ok(false),
        Change::Claim => builds.push(Some(build)),
        Change::Explicit if !builds.is_empty() => {
            if !builds.contains(&None) {
                // A sticky hold supersedes historical owner lines without growing the bounded
                // record or granting another release capability.
                files.write(path, "opened\n")?;
            }
            return Ok(false);
        }
        Change::Explicit => builds.push(Some(build)),
        Change::Forget if !named => return Ok(false),
        Change::Forget => builds.retain(|&kept| kept != Some(build)),
    }
    if builds.is_empty() {
        files.remove(path)?;
        return Ok(true);
    }
    let mut lines = String::new();
    for named in builds {
        match named {
            Some(named) => {
                let _ = writeln!(lines, "opened {named}");
            }
            None => lines.push_str("opened\n"),
        }
    }
    files.write(path, &lines)?;
    Ok(true)
}

/// Why a build no line could name has no reading and no change.
fn unnameable() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "no line of the record can name this build",
    )
}

/// Why a malformed record has no reading and is left as found.
fn malformed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "the record is malformed and is left as found",
    )
}

/// The builds a record's `text` names, oldest first: one `opened <build>` line each, and a bare
/// `opened` naming none. `None` for any other text, an empty one included, and for more lines
/// than a record keeps.
fn builds(text: &str) -> Option<Vec<Option<&str>>> {
    let mut lines = 0;
    let mut builds = Vec::new();
    for line in text.lines() {
        lines += 1;
        match line.split_once(' ') {
            None if line == "opened" => builds.push(None),
            Some(("opened", build)) if nameable(build) => builds.push(Some(build)),
            _ => return None,
        }
    }
    (1..=LINES).contains(&lines).then_some(builds)
}

/// Whether a line can name `build`: one word, no longer than [`BUILD_BYTES`].
fn nameable(build: &str) -> bool {
    (1..=BUILD_BYTES).contains(&build.len()) && !build.contains(char::is_whitespace)
}

/// The lock file beside the record at `path`: its name and `.lock`.
#[cfg(not(target_family = "wasm"))]
fn lock_path(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the record has no name"))?;
    Ok(path.with_file_name(format!("{name}.lock")))
}

/// The folder the record at `path` sits in (`.` for a bare name).
#[cfg(not(target_family = "wasm"))]
fn folder(path: &Path) -> &Path {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    }
}

/// This Mac's files: a read bounded to a regular file, never through a link and never waiting on
/// a pipe ([`aterm_uds::runs::read_bounded_regular_text`]), a write through a file of this
/// process's own beside the record, flushed durably and renamed over it, then its folder flushed,
/// and a lock that is an exclusive `flock` of a file of its own beside the record (its name and
/// `.lock`, [`aterm_uds::file_lock::LockFile`]: the record itself is replaced by each write, so
/// its own inode holds no lock), waited for up to [`LOCK_WAIT`].
#[cfg(not(target_family = "wasm"))]
#[derive(Debug, Clone, Copy, Default)]
pub struct Live;

#[cfg(not(target_family = "wasm"))]
fn lock_started(path: &Path, started: std::time::Instant) -> io::Result<Held> {
    let deadline = started + LOCK_WAIT;
    if std::time::Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Command Line Tools record lock did not finish",
        ));
    }
    let lock = lock_path(path)?;
    std::fs::create_dir_all(folder(path))?;
    aterm_uds::file_lock::LockFile::take(
        &lock,
        deadline,
        "another aterm is changing the record of its Command Line Tools ask",
    )
    .map(Held::new)
}

#[cfg(not(target_family = "wasm"))]
impl Files for Live {
    fn read(&self, path: &Path, max: usize) -> io::Result<Option<String>> {
        match aterm_uds::runs::read_bounded_regular_text(path, max as u64) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn write(&self, path: &Path, text: &str) -> io::Result<()> {
        use std::io::Write as _;
        let dir = folder(path);
        std::fs::create_dir_all(dir)?;
        if std::fs::symlink_metadata(path).is_ok_and(|meta| !meta.file_type().is_file()) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the record's path is held by something that is not a file",
            ));
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the record has no name"))?;
        let staged = path.with_file_name(format!("{name}.tmp-{}", std::process::id()));
        // One an earlier process of this pid left as it died mid-write (before its rename) would
        // hold every later change: the name is this live process's own, under the record's lock
        // ([`Files::lock`]), and a link there goes, not where it points.
        let _ = std::fs::remove_file(&staged);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
        }
        let mut file = options.open(&staged)?;
        let written = file
            .write_all(text.as_bytes())
            .and_then(|()| aterm_durable::durable(&file));
        drop(file);
        let written = written.and_then(|()| std::fs::rename(&staged, path));
        if written.is_err() {
            let _ = std::fs::remove_file(&staged);
        }
        written?;
        // The record is in place; a folder that cannot be flushed here (a network home that takes
        // no full flush) keeps it, as the window's other marks are kept.
        let _ = aterm_durable::durable_dir(dir);
        Ok(())
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        match std::fs::remove_file(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            done => done,
        }
    }

    fn lock(&self, path: &Path) -> io::Result<Held> {
        lock_started(path, std::time::Instant::now())
    }
}

#[cfg(all(test, unix))]
#[path = "clt_mark_tests.rs"]
mod tests;
