// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Atomic durability flush helpers for release journals, records, and artifacts.

use std::fs::File;
use std::path::Path;

/// Flushes an open file or directory to durable storage: a full flush, which the test binary's
/// process tree takes as a plain `fsync(2)` on Apple (`plain_durable_flushes_in_tests!` in
/// `tests/it/main.rs`), counted `full` either way.
pub(crate) fn sync_all(file: &File) -> std::io::Result<()> {
    aterm_durable::durable(file)
}

/// Flushes directory `dir` to make a rename or new entry durable.
pub(crate) fn sync_dir(dir: &Path) -> std::io::Result<()> {
    sync_all(&atpkg::platform::open_for_sync(dir)?)
}

/// Computes `Instant::now() + wait`, saturating at one year if `wait` would overflow.
pub(crate) fn deadline_after(wait: std::time::Duration) -> std::time::Instant {
    let now = std::time::Instant::now();
    now.checked_add(wait)
        .unwrap_or_else(|| now + std::time::Duration::from_secs(365 * 24 * 3600))
}

/// Opens a regular file at `path` in non-blocking mode, refusing symlinks and special files.
pub(crate) fn open_regular_file_with_meta(
    path: &Path,
) -> std::io::Result<(File, std::fs::Metadata)> {
    #[cfg(not(unix))]
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing symlink",
        ));
    }
    let file = astream_sys::open_nonblocking(path, astream_sys::Links::Refuse)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok((file, meta))
}

/// Opens a regular file at `path` in non-blocking mode, refusing symlinks and special files.
pub(crate) fn open_regular_file(path: &Path) -> std::io::Result<File> {
    open_regular_file_with_meta(path).map(|(file, _)| file)
}

/// Default upper bound (256 MiB) for release file and log reads.
pub(crate) const MAX_RELEASE_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// Reads up to `max` bytes from a regular file at `path`, refusing symlinks and FIFOs, and returns its metadata.
pub(crate) fn read_regular_bytes_with_meta(
    path: &Path,
    max: u64,
) -> std::io::Result<(Vec<u8>, std::fs::Metadata)> {
    use std::io::Read as _;
    let (file, meta) = open_regular_file_with_meta(path)?;
    if meta.len() > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            "file exceeds bound",
        ));
    }
    let cap = usize::try_from(meta.len().min(max).saturating_add(1)).unwrap_or(0);
    let mut buf = Vec::with_capacity(cap);
    file.take(max.saturating_add(1)).read_to_end(&mut buf)?;
    if u64::try_from(buf.len()).unwrap_or(u64::MAX) > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            "file exceeds bound",
        ));
    }
    Ok((buf, meta))
}

/// Reads up to `max` bytes from a regular file at `path`, refusing symlinks and FIFOs.
pub(crate) fn read_regular_bytes(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    read_regular_bytes_with_meta(path, max).map(|(buf, _)| buf)
}

/// Reads up to `max` bytes of UTF-8 text from a regular file at `path`, refusing symlinks and FIFOs.
pub(crate) fn read_regular_text(path: &Path, max: u64) -> std::io::Result<String> {
    String::from_utf8(read_regular_bytes(path, max)?)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Reads a regular file at `path` up to [`MAX_RELEASE_FILE_BYTES`], refusing symlinks and FIFOs, and returns its metadata.
pub(crate) fn read_file_with_meta(path: &Path) -> std::io::Result<(Vec<u8>, std::fs::Metadata)> {
    read_regular_bytes_with_meta(path, MAX_RELEASE_FILE_BYTES)
}

/// Reads a regular file at `path` up to [`MAX_RELEASE_FILE_BYTES`], refusing symlinks and FIFOs.
pub(crate) fn read_file(path: &Path) -> std::io::Result<Vec<u8>> {
    read_regular_bytes(path, MAX_RELEASE_FILE_BYTES)
}

/// Reads UTF-8 text from a regular file at `path` up to [`MAX_RELEASE_FILE_BYTES`], refusing symlinks and FIFOs.
pub(crate) fn read_text(path: &Path) -> std::io::Result<String> {
    read_regular_text(path, MAX_RELEASE_FILE_BYTES)
}

/// Opens a brand-new regular file at `path` with `O_CREAT | O_EXCL` (`LOCK_OPEN_FLAGS` on Unix).
pub(crate) fn open_create_new(path: &Path) -> std::io::Result<std::fs::File> {
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
    }
    open.open(path)
}

/// Unlinks any existing entry at `path` and opens a fresh regular file with `O_CREAT | O_EXCL`.
pub(crate) fn create_fresh(path: &Path) -> std::io::Result<std::fs::File> {
    let _ = std::fs::remove_file(path);
    open_create_new(path)
}

/// Copies a regular file at `from` (`O_NOFOLLOW | O_NONBLOCK`) to a fresh regular file at `to`
/// (`create_new(true)` + `LOCK_OPEN_FLAGS`), preserving permissions.
pub(crate) fn copy_regular_fresh(
    from: impl AsRef<Path>,
    to: impl AsRef<Path>,
) -> std::io::Result<u64> {
    let (mut reader, meta) = open_regular_file_with_meta(from.as_ref())?;
    let mut writer = create_fresh(to.as_ref())?;
    let copied = std::io::copy(&mut reader, &mut writer)?;
    writer.set_permissions(meta.permissions())?;
    Ok(copied)
}

/// Opens a regular file at `path` for appending (creating if `create` is true), refusing symlinks and FIFOs.
pub(crate) fn open_append(path: &Path, create: bool) -> std::io::Result<std::fs::File> {
    let mut open = std::fs::OpenOptions::new();
    open.create(create).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
    }
    let file = open.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}

/// Unlinks any existing entry at `path` and creates a fresh regular file with `O_CREAT | O_EXCL`.
pub(crate) fn write_fresh(path: &Path, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    use std::io::Write as _;
    create_fresh(path)?.write_all(bytes.as_ref())
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
        open.mode(mode)
            .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_file_and_read_text_roundtrip_and_refuse_oversized_or_symlinks() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.txt");
        write_atomic_durable(&path, b"hello release", 0o600).expect("write");

        assert_eq!(read_file(&path).expect("read_file"), b"hello release");
        assert_eq!(read_text(&path).expect("read_text"), "hello release");
        assert_eq!(
            read_regular_bytes(&path, 5).expect_err("over cap").kind(),
            std::io::ErrorKind::FileTooLarge
        );

        #[cfg(unix)]
        {
            let link = dir.path().join("link.txt");
            std::os::unix::fs::symlink(&path, &link).expect("symlink");
            assert!(read_file(&link).is_err());
            assert!(read_text(&link).is_err());
            assert!(open_append(&link, true).is_err());
            assert!(open_create_new(&link).is_err());
        }

        let log = dir.path().join("append.log");
        assert!(open_append(&log, false).is_err());
        std::io::Write::write_all(&mut open_append(&log, true).expect("create"), b"a\n")
            .expect("write a");
        std::io::Write::write_all(&mut open_append(&log, false).expect("append"), b"b\n")
            .expect("write b");
        assert_eq!(read_text(&log).expect("read log"), "a\nb\n");
    }
}
