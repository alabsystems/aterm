// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! File-I/O helpers for `atpkg-keys`.

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-process counter that makes each staged temporary's name unique (see [`stage_sibling_temp`]).
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Maximum bytes read by [`read_bytes`].
pub const READ_CAP: u64 = 1024 * 1024;

/// Maximum bytes of the file `sign` signs and `verify-signed` checks (a release's
/// `SHA256SUMS`): one bound, so every file one verb takes the other takes too.
pub const SIGNED_FILE_READ_CAP: u64 = 64 * 1024 * 1024;

/// Build a message by concatenation — deliberately not `format!`.
#[must_use]
pub fn concat(parts: &[&str]) -> String {
    let mut s = String::with_capacity(parts.iter().map(|p| p.len()).sum());
    for p in parts {
        s.push_str(p);
    }
    s
}

/// Name a non-regular file's actual type for the [`read_bytes`] refusal message.
fn file_type_name(t: std::fs::FileType) -> &'static str {
    use std::os::unix::fs::FileTypeExt as _;
    if t.is_dir() {
        "a directory"
    } else if t.is_char_device() {
        "a character device"
    } else if t.is_block_device() {
        "a block device"
    } else if t.is_fifo() {
        "a FIFO"
    } else if t.is_socket() {
        "a socket"
    } else {
        "not a regular file"
    }
}

/// Read a file's exact bytes, the one shared file-read site.
pub fn read_bytes(path: &str) -> std::io::Result<Vec<u8>> {
    read_bytes_up_to(path, READ_CAP)
}

/// [`read_bytes`] with a caller's bound, for a release's `SHA256SUMS`.
pub fn read_bytes_up_to(path: &str, cap: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let over_cap = || {
        let mib = 1024 * 1024;
        let limit = if cap >= mib && cap.is_multiple_of(mib) {
            concat(&[&(cap / mib).to_string(), " MiB"])
        } else {
            concat(&[&cap.to_string(), "-byte"])
        };
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            concat(&[path, " exceeds the ", &limit, " read cap"]),
        )
    };
    let mut open = std::fs::OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    open.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    let f = open.open(path)?;
    let meta = f.metadata()?;
    let ft = meta.file_type();
    if !ft.is_file() {
        return Err(std::io::Error::other(concat(&[
            path,
            " is ",
            file_type_name(ft),
            ", not a regular file",
        ])));
    }
    if meta.len() > cap {
        return Err(over_cap());
    }
    let mut bytes =
        Vec::with_capacity(usize::try_from(meta.len().min(cap).saturating_add(1)).unwrap_or(0));
    f.take(cap.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(over_cap());
    }
    Ok(bytes)
}

/// Write `bytes` to `path` atomically and durably.
pub fn write_bytes(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    write_bytes_atomic(path, bytes)
}

/// Replace `path` atomically via a synced sibling temporary ([`stage_sibling_temp_with_mode`]).
pub fn write_bytes_atomic(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    write_bytes_atomic_with_sync(path, bytes, sync_parent).map_err(|e| e.error)
}

/// An atomic replacement's error and whether its rename already succeeded.
#[derive(Debug)]
pub(crate) struct AtomicWriteError {
    pub error: std::io::Error,
    pub renamed: bool,
}

/// The shared replacement with its parent-directory sync operation supplied by the caller.
pub(crate) fn write_bytes_atomic_with_sync(
    path: &str,
    bytes: &[u8],
    sync: impl FnOnce(&str) -> std::io::Result<()>,
) -> Result<(), AtomicWriteError> {
    let before_rename = |error| AtomicWriteError {
        error,
        renamed: false,
    };
    let tmp = stage_sibling_temp(path, bytes).map_err(before_rename)?;
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(before_rename(error));
    }
    sync(path).map_err(|error| AtomicWriteError {
        error,
        renamed: true,
    })
}

/// The staging half of [`write_bytes_atomic`].
fn stage_sibling_temp(path: &str, bytes: &[u8]) -> std::io::Result<String> {
    stage_sibling_temp_with_mode(path, bytes, 0o666)
}

/// [`stage_sibling_temp`] with an explicit creation mode ([`write_owner_file_create_new`]).
fn stage_sibling_temp_with_mode(path: &str, bytes: &[u8], mode: u32) -> std::io::Result<String> {
    use std::io::Write as _;
    let (tmp, mut f) = loop {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let tmp = concat(&[
            path,
            ".atpkg-keys.",
            &std::process::id().to_string(),
            ".",
            &sequence.to_string(),
            ".tmp",
        ]);
        #[cfg(test)]
        window::reached("temp name drawn", &tmp);
        match std::fs::OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(mode)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&tmp)
        {
            Ok(file) => break (tmp, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    };
    let written = f.write_all(bytes).and_then(|()| aterm_durable::durable(&f));
    drop(f);
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(tmp)
}

/// Install a new owner-only (`0600`) file atomically, never replacing an existing path.
pub fn write_owner_file_create_new(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = stage_sibling_temp_with_mode(path, bytes, 0o600)?;
    if let Err(e) = std::fs::hard_link(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = sync_parent(path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::remove_file(&tmp)?;
    sync_parent(path)
}

/// Flush the directory entry containing `path`.
pub fn sync_parent(path: &str) -> std::io::Result<()> {
    let parent = std::path::Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    aterm_durable::Flush::Durable.dir(parent)
}

/// [`sync_parent`], only ORDERED ([`aterm_durable::ordered`]).
///
/// # Errors
/// The parent could not be opened, or the flush's own error.
pub fn sync_parent_ordered(path: &str) -> std::io::Result<()> {
    let parent = std::path::Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    aterm_durable::Flush::Ordered.dir(parent)
}

/// Create every missing directory above `path`.
pub fn ensure_parent_dir(path: &str) -> std::io::Result<()> {
    let Some(parent) = std::path::Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    let mut missing = Vec::new();
    let mut cursor = parent;
    while !cursor.as_os_str().is_empty() && std::fs::symlink_metadata(cursor).is_err() {
        missing.push(cursor.to_path_buf());
        let Some(next) = cursor.parent() else {
            break;
        };
        cursor = next;
    }
    std::fs::create_dir_all(parent)?;
    for directory in missing.iter().rev() {
        let directory = directory.to_str().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "new directory path is not UTF-8",
            )
        })?;
        sync_parent(directory)?;
    }
    Ok(())
}

/// The test seam for what another process does inside a window between a check and its
/// use: litter at the temporary name just drawn, a lock file swapped after it was opened, a
/// write that lands between ours and its read-back. A test arms one act for one named
/// window on its own thread ([`with`](window::with)); the code under test marks the window
/// with [`reached`](window::reached). Test builds only: a release build has no window.
#[cfg(test)]
#[path = "fsio_window_tests.rs"]
pub(crate) mod window;

#[cfg(test)]
#[path = "fsio_tests.rs"]
mod tests;
