// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RESERVE: a ballast file the disk watch keeps while space is plenty
//! and unlinks FIRST when the volume is full, so the pass that follows can
//! write its journal.
//!
//! Every removal of the disk watch is journalled BEFORE it starts
//! ([`super::Refusal::Unjournalled`]: nothing is removed without a record).
//! At a true `ENOSPC` that rule removed nothing at all, because the first
//! intent row could not be written — the deadlock of 2026-10-16, when the
//! disk filled twice. The reserve breaks it without weakening the rule: its
//! blocks were written while there was room ([`ensure`]), its release is one
//! `unlink` that needs no space ([`release`]), and the journal row saying so
//! is the first thing written into the room it made.
//!
//! The reserve is a regular file this uid owns at one path. A symlink at
//! that entry or its immediate parent is refused and left alone
//! ([`ReserveError`]); earlier ancestors use normal path resolution.

use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The reserve's size: 256 MiB, far above the few rows one pass journals,
/// and small next to the floor it is kept above.
pub const RESERVE_BYTES: u64 = 256 * 1024 * 1024;

/// The reserve's folder under aterm's cache folder (`.noindex`: Spotlight
/// skips it) and its file name.
const FOLDER: &str = "disk-reserve.noindex";
const FILE: &str = "reserve";

/// Where the reserve lives for `home`: `~/Library/Caches/aterm/
/// disk-reserve.noindex/reserve` on macOS, `~/.cache/aterm/
/// disk-reserve.noindex/reserve` elsewhere — on the home volume the watch
/// measures, beside aterm's build cache.
#[must_use]
pub fn path_for(home: &Path) -> PathBuf {
    let cache = if cfg!(target_os = "macos") {
        home.join("Library/Caches/aterm")
    } else {
        home.join(".cache/aterm")
    };
    cache.join(FOLDER).join(FILE)
}

/// Why the reserve at a path was not touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReserveError {
    /// Something other than a regular file of this uid is there (a link, a
    /// folder, another user's file): left alone.
    NotOurs(String),
    /// The file system refused.
    Io(String),
}

impl std::fmt::Display for ReserveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReserveError::NotOurs(why) => write!(f, "the reserve is left alone: {why}"),
            ReserveError::Io(why) => write!(f, "the reserve could not be kept: {why}"),
        }
    }
}

/// What is at the reserve's path, judged without following a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Nothing.
    Absent,
    /// The reserve: a regular file of this uid, with this many bytes.
    Present(u64),
}

/// What is at `path`: the reserve, nothing, or something that is not ours.
///
/// # Errors
///
/// A link, a non-regular file or another uid's file is [`ReserveError::NotOurs`].
/// A symlink or non-directory at the immediate parent is also refused;
/// earlier ancestors use normal path resolution.
pub fn state(path: &Path) -> Result<State, ReserveError> {
    if let Some(folder) = path.parent() {
        match std::fs::symlink_metadata(folder) {
            Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
                return Err(ReserveError::NotOurs(format!(
                    "{} is not a folder (a link is not followed)",
                    folder.display()
                )));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::Absent),
            Err(e) => return Err(ReserveError::Io(format!("{}: {e}", folder.display()))),
        }
    }
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::Absent),
        Err(e) => return Err(ReserveError::Io(format!("{}: {e}", path.display()))),
    };
    if !meta.file_type().is_file() {
        return Err(ReserveError::NotOurs(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != super::discover::own_uid() {
            return Err(ReserveError::NotOurs(format!(
                "{} belongs to uid {}",
                path.display(),
                meta.uid()
            )));
        }
    }
    Ok(State::Present(meta.len()))
}

/// Keep the reserve at `path`, `bytes` long, its blocks WRITTEN (a sparse
/// file would free nothing): `Ok(true)` when it was written now, `Ok(false)`
/// when it was already there whole. The caller asks only while free space is
/// well above the floor. Written under a temporary name and renamed into
/// place, so a reserve half-written by a full disk is never the one kept;
/// mode `0600`, its folder `0700`.
///
/// # Errors
///
/// Something not ours at the path, or the write failed (the temporary file
/// is removed then).
pub fn ensure(path: &Path, bytes: u64) -> Result<bool, ReserveError> {
    if let State::Present(len) = state(path)?
        && len >= bytes
    {
        return Ok(false);
    }
    let folder = path
        .parent()
        .ok_or_else(|| ReserveError::Io("the reserve has no folder".to_owned()))?;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder
        .create(folder)
        .map_err(|e| ReserveError::Io(format!("{}: {e}", folder.display())))?;
    // The folder made, judged again: a link planted there is not followed.
    state(path)?;
    let tmp = folder.join(format!(".{FILE}.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let written = write_blocks(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(ReserveError::Io(format!("{}: {e}", path.display())));
    }
    Ok(true)
}

/// Write `bytes` of a non-zero pattern to a NEW file at `path` (never
/// through a link: `create_new`), mode `0600`, and order it ahead of its rename.
fn write_blocks(path: &Path, bytes: u64) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    let chunk = vec![0xa5_u8; 1024 * 1024];
    let mut left = bytes;
    while left > 0 {
        let n = usize::try_from(left.min(chunk.len() as u64)).unwrap_or(chunk.len());
        f.write_all(&chunk[..n])?;
        left -= n as u64;
    }
    // Ordered: the blocks land before the rename publishes the reserve whole; after a
    // power loss `state` finds it absent or short and `ensure` writes it again.
    aterm_durable::ordered(&f)
}

/// Unlink the reserve at `path`: the bytes it held, `Ok(None)` when none was
/// there. Only the reserve goes — a regular file of this uid at that exact
/// path, judged without following a link; an unlink needs no free space.
///
/// # Errors
///
/// Something not ours at the path ([`ReserveError::NotOurs`], left alone),
/// or the unlink failed.
pub fn release(path: &Path) -> Result<Option<u64>, ReserveError> {
    match state(path)? {
        State::Absent => Ok(None),
        State::Present(len) => {
            std::fs::remove_file(path)
                .map_err(|e| ReserveError::Io(format!("{}: {e}", path.display())))?;
            Ok(Some(len))
        }
    }
}

/// The pass lock's name, beside the reserve ([`pass_lock_for`]).
const PASS_LOCK: &str = "pass.lock";

/// Where the machine-wide pass lock lives for `home`: beside the reserve, on
/// the home volume ([`path_for`]'s folder).
#[must_use]
pub fn pass_lock_for(home: &Path) -> PathBuf {
    path_for(home).with_file_name(PASS_LOCK)
}

/// ONE PASS AT A TIME ACROSS INSTANCES (design §4.8): every aterm instance has
/// its own watch, and cargo's locks already make two passes safe, but not
/// sensible — each would survey, size and race the other for the same
/// caches. Held while this process's pass runs, released by `LOCK_UN` on
/// drop (a child mid-spawn keeps a copy of the descriptor until its exec,
/// so close alone would leave it held there).
pub struct PassLock(std::fs::File);

impl Drop for PassLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Take the pass lock at `path` without waiting: `Ok(Some)` taken,
/// `Ok(None)` another instance's pass holds it (this look runs none). The
/// file is created `0600` in a folder made `0700`, never through a link.
///
/// # Errors
///
/// The folder or the file could not be made or opened (a full disk): the
/// caller decides whether its pass runs unlocked.
pub fn take_pass(path: &Path) -> std::io::Result<Option<PassLock>> {
    if let Some(folder) = path.parent() {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder.create(folder)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    // The ascription is the lock-order census's File evidence (aterm-census
    // `is_file_binding_rhs`), keeping this site a cross-process advisory lock.
    let file: std::fs::File = opts.open(path)?;
    #[expect(
        clippy::disallowed_methods,
        reason = "the pass lock's own take: its Drop releases by LOCK_UN"
    )]
    let taken = file.try_lock();
    match taken {
        Ok(()) => Ok(Some(PassLock(file))),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

#[path = "disk_reserve_tests.rs"]
#[cfg(all(test, unix))]
mod tests;
