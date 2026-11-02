// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Atomic durability flush helpers for release journals, records, and artifacts.

use std::fs::File;
use std::path::Path;

/// Flushes an open file or directory to durable storage.
#[cfg(not(all(test, target_vendor = "apple")))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    aterm_durable::durable(file)
}

/// Flushes file contents without a drive-cache barrier under Apple tests.
#[cfg(all(test, target_vendor = "apple"))]
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    atpkg::platform::sync_file_contents(file)
}

/// Flushes directory `dir` to make a rename or new entry durable.
pub(crate) fn sync_dir(dir: &Path) -> std::io::Result<()> {
    sync_all(&File::open(dir)?)
}

/// Computes `Instant::now() + wait`, saturating at one year if `wait` would overflow.
pub(crate) fn deadline_after(wait: std::time::Duration) -> std::time::Instant {
    let now = std::time::Instant::now();
    now.checked_add(wait)
        .unwrap_or_else(|| now + std::time::Duration::from_secs(365 * 24 * 3600))
}

/// Reads up to `max` bytes from a regular file at `path`, refusing symlinks and FIFOs.
pub(crate) fn read_regular_bytes(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    #[cfg(not(unix))]
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing symlink",
        ));
    }
    let file = astream_sys::open_nonblocking(path, astream_sys::Links::Refuse)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let cap = usize::try_from(max.min(64 * 1024)).unwrap_or(64 * 1024);
    let mut buf = Vec::with_capacity(cap);
    file.take(max.saturating_add(1)).read_to_end(&mut buf)?;
    if u64::try_from(buf.len()).unwrap_or(u64::MAX) > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            "file exceeds bound",
        ));
    }
    Ok(buf)
}

/// Reads up to `max` bytes of UTF-8 text from a regular file at `path`, refusing symlinks and FIFOs.
pub(crate) fn read_regular_text(path: &Path, max: u64) -> std::io::Result<String> {
    String::from_utf8(read_regular_bytes(path, max)?)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Stages `bytes` in a private temporary directory beside `path`, flushes it, renames over `path`, and flushes the directory.
pub(crate) fn write_atomic_durable(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let stage = aterm_tempfile::Builder::new()
        .prefix(".stage-")
        .tempdir_in(dir)?;
    let staged = stage.path().join("data");
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let mut file = open.open(&staged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
    }
    std::io::Write::write_all(&mut file, bytes)?;
    sync_all(&file)?;
    drop(file);
    std::fs::rename(&staged, path)?;
    sync_dir(dir)
}
