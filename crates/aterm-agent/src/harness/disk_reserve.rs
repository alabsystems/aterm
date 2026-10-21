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
//!
//! ON WINDOWS a link is ANY reparse point ([`atpkg::platform::is_reparse`]:
//! a junction as much as a symlink, and a cloud placeholder), and the file
//! is opened as itself (`FILE_FLAG_OPEN_REPARSE_POINT`) wherever it is
//! judged or deleted. A reserve must HOLD its blocks: one compressed, sparse
//! or offline, or allocated short of its length (a folder that compresses
//! what is written into it — 0xa5 compresses to almost nothing), would free
//! nothing at the emergency it is kept for, so it is not counted as the
//! reserve ([`ReserveError::Thin`]). It is still aterm's own regular file at
//! aterm's own path, so it is not left for good either: [`ensure`] writes a
//! whole one over it, and [`release`] deletes it at an emergency for what it
//! does hold. It is written `NOT_CONTENT_INDEXED`, and released by a delete
//! through the handle it was judged by. There is no owner check: the folder
//! is in the profile's own `%LOCALAPPDATA%`, private by its ACL (as atpkg's
//! store is).

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
/// disk-reserve.noindex/reserve` on other Unix, and
/// `~\AppData\Local\aterm\disk-reserve\reserve` on Windows (`%LOCALAPPDATA%`
/// in the profile's own layout, where the harness state already is: outside
/// Windows Search's default scope and OneDrive's folders, and derived from
/// `home`, never the environment, so a scratch profile keeps its own) — each
/// on the home volume the watch measures.
#[must_use]
pub fn path_for(home: &Path) -> PathBuf {
    if cfg!(windows) {
        return home
            .join("AppData")
            .join("Local")
            .join("aterm")
            .join("disk-reserve")
            .join(FILE);
    }
    let cache = if cfg!(target_os = "macos") {
        home.join("Library/Caches/aterm")
    } else {
        home.join(".cache/aterm")
    };
    cache.join(FOLDER).join(FILE)
}

/// Whether `meta` (read without following) is a link: a symlink, and on
/// Windows any reparse point.
fn is_link(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        atpkg::platform::is_reparse(meta)
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

/// Why the reserve at a path was not touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReserveError {
    /// Something other than a regular file of this uid is there (a link, a
    /// folder, another user's file): left alone.
    NotOurs(String),
    /// (Windows) The reserve's own regular file is there but holds fewer
    /// blocks than its length — compressed, sparse, offline, or allocated
    /// short — so it is not counted as the reserve; [`ensure`] writes a whole
    /// one over it and [`release`] deletes it.
    Thin(String),
    /// The file system refused.
    Io(String),
    /// Writing the reserve failed (its blocks or its rename): what was
    /// written of it is removed again. A retry writes it from the start, so
    /// a failure that holds (a quota, a failing disk) costs up to
    /// [`RESERVE_BYTES`] of writes at every try.
    Unwritten(String),
}

impl std::fmt::Display for ReserveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReserveError::NotOurs(why) => write!(f, "the reserve is left alone: {why}"),
            ReserveError::Thin(why) => {
                write!(f, "the reserve does not hold its blocks: {why}")
            }
            ReserveError::Io(why) => write!(f, "the reserve could not be kept: {why}"),
            ReserveError::Unwritten(why) => write!(f, "the reserve could not be written: {why}"),
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
/// earlier ancestors use normal path resolution. On Windows a file holding
/// fewer blocks than its length is not the reserve either
/// ([`ReserveError::Thin`], [`holds_its_blocks`]).
pub fn state(path: &Path) -> Result<State, ReserveError> {
    if let Some(folder) = path.parent() {
        match std::fs::symlink_metadata(folder) {
            Ok(m) if is_link(&m) || !m.is_dir() => {
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
    if is_link(&meta) || !meta.file_type().is_file() {
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
    #[cfg(windows)]
    holds_its_blocks(path)?;
    Ok(State::Present(meta.len()))
}

/// Open the regular file at `path` AS ITSELF (never through a reparse point)
/// with `access`, sharing everything, and judge it again through that handle:
/// a reparse point or anything not a regular file is [`ReserveError::NotOurs`].
#[cfg(windows)]
fn open_judged(path: &Path, access: u32) -> Result<std::fs::File, ReserveError> {
    use std::os::windows::fs::OpenOptionsExt as _;
    /// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`.
    const SHARE_ALL: u32 = 0x7;
    let file = std::fs::OpenOptions::new()
        .access_mode(access)
        .share_mode(SHARE_ALL)
        .custom_flags(super::win::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|e| ReserveError::Io(format!("{}: {e}", path.display())))?;
    let meta = file
        .metadata()
        .map_err(|e| ReserveError::Io(format!("{}: {e}", path.display())))?;
    if is_link(&meta) || !meta.file_type().is_file() {
        return Err(ReserveError::NotOurs(format!(
            "{} is not a regular file (a link is not followed)",
            path.display()
        )));
    }
    Ok(file)
}

/// Whether the file at `path` HOLDS ITS BLOCKS — not compressed, sparse,
/// offline or a placeholder, and allocated at least its length — read
/// through its own handle ([`super::win::allocated`]): releasing one that
/// does not frees next to nothing at the emergency it is kept for, so it is
/// not the reserve.
///
/// # Errors
///
/// [`ReserveError::Thin`] for a thin file; [`ReserveError::NotOurs`] for a
/// reparse point or a non-file; the open or the query failing.
#[cfg(windows)]
fn holds_its_blocks(path: &Path) -> Result<(), ReserveError> {
    use std::os::windows::fs::MetadataExt as _;
    let file = open_judged(path, super::win::FILE_READ_ATTRIBUTES)?;
    let io = |e: std::io::Error| ReserveError::Io(format!("{}: {e}", path.display()));
    let meta = file.metadata().map_err(io)?;
    if meta.file_attributes() & super::win::FILE_ATTRIBUTES_THIN != 0 {
        return Err(ReserveError::Thin(format!(
            "{} is compressed, sparse or offline, so it holds fewer blocks than its length",
            path.display()
        )));
    }
    let held = super::win::allocated(&file).map_err(io)?;
    if held < meta.len() {
        return Err(ReserveError::Thin(format!(
            "{} holds {held} bytes of disk for its {}",
            path.display(),
            meta.len()
        )));
    }
    Ok(())
}

/// Keep the reserve at `path`, `bytes` long, its blocks WRITTEN (a sparse
/// file would free nothing): `Ok(true)` when it was written now, `Ok(false)`
/// when it was already there whole. The caller asks only while free space is
/// well above the floor. Written under a temporary name and renamed into
/// place, so a reserve half-written by a full disk is never the one kept;
/// mode `0600`, its folder `0700`. On Windows a folder that compresses what
/// is written into it is refused before a byte is written, the written file
/// must hold its blocks ([`holds_its_blocks`]) before it is renamed in, and a
/// reserve that no longer holds its own ([`ReserveError::Thin`]) is written
/// over like a short one.
///
/// # Errors
///
/// Something not ours at the path ([`ReserveError::NotOurs`]), its folder
/// not made ([`ReserveError::Io`]), or the write failed
/// ([`ReserveError::Unwritten`]; the temporary file is removed then).
pub fn ensure(path: &Path, bytes: u64) -> Result<bool, ReserveError> {
    match state(path) {
        Ok(State::Present(len)) if len >= bytes => return Ok(false),
        Ok(_) | Err(ReserveError::Thin(_)) => {}
        Err(e) => return Err(e),
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
    match state(path) {
        Ok(_) | Err(ReserveError::Thin(_)) => {}
        Err(e) => return Err(e),
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        let compresses = std::fs::symlink_metadata(folder)
            .map_err(|e| ReserveError::Io(format!("{}: {e}", folder.display())))?
            .file_attributes()
            & super::win::FILE_ATTRIBUTE_COMPRESSED
            != 0;
        if compresses {
            return Err(ReserveError::NotOurs(format!(
                "{} compresses what is written into it, so no reserve there would hold its blocks",
                folder.display()
            )));
        }
    }
    // Own the staging directory through publication and cleanup. A preexisting
    // temporary name is never evidence that this attempt may remove it.
    let stage = aterm_tempfile::TempDir::new_in(folder)
        .map_err(|e| ReserveError::Unwritten(format!("{}: {e}", path.display())))?;
    let tmp = stage.path().join(FILE);
    write_blocks(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| ReserveError::Unwritten(format!("{}: {e}", path.display())))?;
    Ok(true)
}

/// Write `bytes` of a non-zero pattern to a NEW file at `path` (never
/// through a link: `create_new`), mode `0600`, and order it ahead of its
/// rename. On Windows it is marked `NOT_CONTENT_INDEXED` (Windows Search
/// leaves it alone), and once ordered it must hold its blocks, read through
/// the handle that wrote them, or it is no reserve.
fn write_blocks(path: &Path, bytes: u64) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    #[cfg(windows)]
    super::win::add_attributes(&f, super::win::FILE_ATTRIBUTE_NOT_CONTENT_INDEXED)?;
    let chunk = vec![0xa5_u8; 1024 * 1024];
    let mut left = bytes;
    while left > 0 {
        let n = usize::try_from(left.min(chunk.len() as u64)).unwrap_or(chunk.len());
        f.write_all(&chunk[..n])?;
        left -= n as u64;
    }
    // Ordered: the blocks land before the rename publishes the reserve whole; after a
    // power loss `state` finds it absent or short and `ensure` writes it again.
    aterm_durable::ordered(&f)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        let meta = f.metadata()?;
        let held = super::win::allocated(&f)?;
        if meta.file_attributes() & super::win::FILE_ATTRIBUTES_THIN != 0 || held < bytes {
            return Err(std::io::Error::other(format!(
                "written, it holds {held} bytes of disk for its {bytes} (attributes {:#x})",
                meta.file_attributes()
            )));
        }
    }
    Ok(())
}

/// Unlink the reserve at `path`: the bytes it held, `Ok(None)` when none was
/// there. Only the reserve goes — a regular file of this uid at that exact
/// path, judged without following a link; an unlink needs no free space. On
/// Windows the file is opened as itself, judged again through that handle,
/// and deleted THROUGH IT ([`super::win::delete_open`]): what goes is the
/// file that was judged, whatever a name resolves to by then. A reserve that
/// no longer holds its blocks ([`ReserveError::Thin`]) goes too, for the
/// clusters it does hold (its allocation, what is said to have been freed).
///
/// # Errors
///
/// Something not ours at the path ([`ReserveError::NotOurs`], left alone),
/// or the unlink failed.
pub fn release(path: &Path) -> Result<Option<u64>, ReserveError> {
    match state(path) {
        Ok(State::Absent) => Ok(None),
        #[cfg(windows)]
        Ok(State::Present(_)) | Err(ReserveError::Thin(_)) => {
            let file = open_judged(path, super::win::DELETE | super::win::FILE_READ_ATTRIBUTES)?;
            let io = |e: std::io::Error| ReserveError::Io(format!("{}: {e}", path.display()));
            let len = file.metadata().map_err(io)?.len();
            let held = super::win::allocated(&file).map_err(io)?;
            super::win::delete_open(&file).map_err(io)?;
            Ok(Some(held.min(len)))
        }
        #[cfg(not(windows))]
        Ok(State::Present(len)) => {
            std::fs::remove_file(path)
                .map_err(|e| ReserveError::Io(format!("{}: {e}", path.display())))?;
            Ok(Some(len))
        }
        Err(e) => Err(e),
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
/// file is created `0600` in a folder made `0700`, never through a link (on
/// Windows opened as itself, `FILE_FLAG_OPEN_REPARSE_POINT`, and refused
/// when that is a reparse point or not a regular file).
///
/// # Errors
///
/// The folder or the file could not be made or opened (a full disk), or a
/// link is in its place: the caller decides whether its pass runs unlocked.
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
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        opts.custom_flags(super::win::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    // The ascription is the lock-order census's File evidence (aterm-census
    // `is_file_binding_rhs`), keeping this site a cross-process advisory lock.
    let file: std::fs::File = opts.open(path)?;
    #[cfg(windows)]
    {
        let meta = file.metadata()?;
        if is_link(&meta) || !meta.file_type().is_file() {
            return Err(std::io::Error::other(format!(
                "{} is not a regular file (a link is not followed)",
                path.display()
            )));
        }
    }
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

#[path = "disk_reserve_windows_tests.rs"]
#[cfg(all(test, windows))]
mod windows_tests;
