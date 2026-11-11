// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded regular-file reading and atomic writing for config-directory consent markers.

use crate::durable::Flush;
use std::io::Write as _;
use std::path::Path;

/// The marker's text, when a regular, non-symlink file of at most `max_bytes` sits at `path`
/// inside a real (non-symlink) parent directory.
#[inline]
pub fn read_marker(path: &Path, max_bytes: usize) -> Option<String> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty())
        && !std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_dir())
    {
        return None;
    }
    aterm_effects::file_feed::read_bounded_regular_utf8(path, max_bytes).ok()
}

fn ensure_marker_target(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_file() => Ok(()),
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "marker path is occupied by something that is not a marker",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn ensure_real_parent(path: &Path) -> std::io::Result<()> {
    let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) else {
        return Ok(());
    };
    if !std::fs::symlink_metadata(dir)?.file_type().is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "marker parent path is not a directory",
        ));
    }
    Ok(())
}

/// Atomically writes `text` to `path` using `flush` ([`Flush::Ordered`], [`Flush::Durable`]),
/// as [`finish`] does it.
pub fn write_marker(
    path: &Path,
    text: &str,
    max_bytes: usize,
    flush: Flush,
) -> std::io::Result<()> {
    if text.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "record is larger than a marker may hold",
        ));
    }
    let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "marker has no name")
    })?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if std::fs::symlink_metadata(dir).is_ok_and(|m| !m.file_type().is_dir()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "marker parent path is not a directory",
            ));
        }
        std::fs::create_dir_all(dir)?;
        ensure_real_parent(path)?;
    }
    ensure_marker_target(path)?;
    let tmp = path.with_file_name(format!("{name}.tmp-{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        const FLAGS: i32 = 0x0100 | 0x0100_0000;
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        const FLAGS: i32 = 0o2_000_000;
        options.mode(0o600).custom_flags(FLAGS);
    }
    let mut file = options.open(&tmp)?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| finish(&file, flush));
    drop(file);
    let written = written
        .and_then(|()| ensure_real_parent(path))
        .and_then(|()| ensure_marker_target(path))
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written?;
    #[cfg(unix)]
    if flush == Flush::Durable
        && let Some(dir) = path.parent()
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        if let Ok(dir) = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dir)
        {
            let _ = crate::durable::sync_all(&dir);
        }
    }
    Ok(())
}

/// A marker's flush. [`Flush::Durable`] is kept as asked. [`Flush::Ordered`] is
/// [`aterm_durable::ordered_or_fsync`]: an ordered marker records that something was asked,
/// answered or checked, and its loss costs only a repeat. So a volume that refuses both ordered
/// flushes (a network home whose server takes no `F_FULLFSYNC`) takes a plain `fsync` rather
/// than no marker at all, which would ask again at every launch.
fn finish(file: &std::fs::File, flush: Flush) -> std::io::Result<()> {
    match flush {
        Flush::Durable => crate::durable::sync_all(file),
        Flush::Ordered => aterm_durable::ordered_or_fsync(file),
    }
}

/// Remove the marker at `path`, if a regular file is there ([`write_marker`]).
pub fn remove_marker(path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty())
        && std::fs::symlink_metadata(dir).is_ok_and(|m| !m.file_type().is_dir())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "marker parent path is not a directory",
        ));
    }
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_file() => std::fs::remove_file(path),
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "marker path is occupied by something that is not a marker",
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Return `true` when `len` fits within `max_bytes`.
#[must_use]
#[inline]
pub const fn is_within_marker_cap(len: usize, max_bytes: usize) -> bool {
    len <= max_bytes
}

/// Encodes the four FDA record fields into 16 little-endian bytes.
#[must_use]
#[inline]
pub const fn encode_fda_timestamps_le(
    asked_at: u32,
    opened_at: u32,
    verified_at: u32,
    verified_build: u32,
) -> [u8; 16] {
    [
        asked_at as u8,
        (asked_at >> 8) as u8,
        (asked_at >> 16) as u8,
        (asked_at >> 24) as u8,
        opened_at as u8,
        (opened_at >> 8) as u8,
        (opened_at >> 16) as u8,
        (opened_at >> 24) as u8,
        verified_at as u8,
        (verified_at >> 8) as u8,
        (verified_at >> 16) as u8,
        (verified_at >> 24) as u8,
        verified_build as u8,
        (verified_build >> 8) as u8,
        (verified_build >> 16) as u8,
        (verified_build >> 24) as u8,
    ]
}

/// Decode 16 little-endian bytes into the four FDA record fields.
#[must_use]
#[inline]
pub const fn decode_fda_timestamps_le(bytes: [u8; 16]) -> (u32, u32, u32, u32) {
    let asked_at = (bytes[0] as u32)
        | ((bytes[1] as u32) << 8)
        | ((bytes[2] as u32) << 16)
        | ((bytes[3] as u32) << 24);
    let opened_at = (bytes[4] as u32)
        | ((bytes[5] as u32) << 8)
        | ((bytes[6] as u32) << 16)
        | ((bytes[7] as u32) << 24);
    let verified_at = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let verified_build = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (asked_at, opened_at, verified_at, verified_build)
}

#[cfg(test)]
#[path = "config_marker_tests.rs"]
mod tests;
