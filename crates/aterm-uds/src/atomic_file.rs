// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! FILES, WRITTEN THE CAREFUL WAY AND READ WITHIN A BOUND.
//!
//! The bounded regular-file readers ([`read_regular`], [`read_regular_text`]) run on every
//! platform. The whole-file writer and aterm.toml's (`ConfigFile`, under the config service's
//! write lock) are unix-only, since their modes and `flock` have no counterpart elsewhere.

use std::io;
use std::path::Path;

#[cfg(unix)]
#[path = "atomic_file_unix.rs"]
mod unix;
#[cfg(unix)]
pub use unix::*;

/// Maximum size in bytes of a configuration or state file read by `ConfigFile` (16 MiB).
pub const MAX_CONFIG_BYTES: u64 = 16 * 1024 * 1024;

/// Read a regular file non-blockingly up to `max_bytes`, returning its bytes and open-handle
/// metadata.
/// Symbolic links are followed. Use [`crate::regular_file::open`] when the final component
/// must itself be a regular file.
///
/// # Errors
/// Returns an error if `path` cannot be opened, is not a regular file, or exceeds `max_bytes`.
pub fn read_regular(path: &Path, max_bytes: u64) -> io::Result<(Vec<u8>, std::fs::Metadata)> {
    use std::io::Read as _;
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?
    };
    #[cfg(not(unix))]
    let file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    if meta.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file exceeds {max_bytes} bytes"),
        ));
    }
    let mut buf = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut buf)?;
    if u64::try_from(buf.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file exceeds {max_bytes} bytes"),
        ));
    }
    Ok((buf, meta))
}

/// Read a regular UTF-8 file non-blockingly up to `max_bytes`.
///
/// # Errors
/// Returns an error if `path` cannot be opened, is not a regular file, exceeds `max_bytes`, or is
/// not valid UTF-8.
pub fn read_regular_text(path: &Path, max_bytes: u64) -> io::Result<String> {
    let (bytes, _) = read_regular(path, max_bytes)?;
    String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
