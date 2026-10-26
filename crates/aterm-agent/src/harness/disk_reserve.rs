// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Disk-watch ballast reserve file and cross-instance pass lock.

use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Default reserve file size in bytes (256 MiB).
pub const RESERVE_BYTES: u64 = 256 * 1024 * 1024;

/// The reserve's folder under aterm's cache folder (`.noindex`: Spotlight
/// skips it) and its file name.
const FOLDER: &str = "disk-reserve.noindex";
const FILE: &str = "reserve";

/// Returns the reserve file path under `home`'s cache directory.
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
pub(super) fn is_link(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        atpkg::platform::is_reparse(meta)
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

/// Error returned when inspecting, writing, or releasing the reserve file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReserveError {
    /// Target path is not a regular file owned by the current user.
    NotOurs(String),
    /// Reserve file is sparse, compressed, offline, or under-allocated on Windows.
    Thin(String),
    /// Underlying filesystem error.
    Io(String),
    /// Writing or renaming the reserve failed and any partial file was removed.
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

/// Current state of the reserve path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// No file exists at the reserve path.
    Absent,
    /// Regular file owned by the current user with the given byte length.
    Present(u64),
}

/// Inspects `path` without following symlinks and returns its [`State`].
/// # Errors
/// Returns [`ReserveError`] if `path` is not ours, is under-allocated, or fails an I/O check.
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

/// Ensures a fully allocated reserve of `bytes` exists at `path`, returning `Ok(true)` if newly written.
/// # Errors
/// Returns [`ReserveError`] if `path` is not ours or writing/allocating fails.
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
/// rename.
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

/// Unlinks the reserve at `path` and returns its byte size, or `Ok(None)` if absent.
/// # Errors
/// Returns [`ReserveError`] if `path` is not ours or unlinking fails.
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

/// Returns the cross-instance pass lock path beside [`path_for`].
#[must_use]
pub fn pass_lock_for(home: &Path) -> PathBuf {
    path_for(home).with_file_name(PASS_LOCK)
}

/// Cross-instance non-blocking flock guard for a disk-watch pass.
#[derive(Debug)]
pub struct PassLock(std::fs::File);

impl Drop for PassLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Acquires the pass lock at `path` without blocking, returning `Ok(None)` if held elsewhere.
/// # Errors
/// Returns an I/O error if creating or locking `path` fails.
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
