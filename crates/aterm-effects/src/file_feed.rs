// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded admission for config-referenced visual-effect files.
//!
//! Opens once with non-blocking and no-follow semantics where available, verifies the opened
//! handle is a regular file, and reads at most `max_bytes` plus one sentinel byte.

use std::io::{self, Read as _};
use std::path::Path;

use aterm_hash::Fnv1a64;

#[cfg(debug_assertions)]
#[derive(Clone)]
struct OpenProbeState {
    path: std::path::PathBuf,
    attempts: std::rc::Rc<std::cell::Cell<usize>>,
}

#[cfg(debug_assertions)]
thread_local! {
    static OPEN_PROBE: std::cell::RefCell<Option<OpenProbeState>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only RAII counter of open attempts for an exact absolute path.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub struct OpenProbeGuard {
    previous: Option<OpenProbeState>,
    attempts: std::rc::Rc<std::cell::Cell<usize>>,
}

#[cfg(debug_assertions)]
impl OpenProbeGuard {
    #[inline]
    #[must_use]
    pub fn attempts(&self) -> usize {
        self.attempts.get()
    }
}

#[cfg(debug_assertions)]
impl Drop for OpenProbeGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        OPEN_PROBE.with(|probe| {
            *probe.borrow_mut() = previous;
        });
    }
}

/// Install a test-only counter at the platform `open` seam for `path`.
#[cfg(debug_assertions)]
#[doc(hidden)]
#[inline]
#[must_use]
pub fn probe_open_attempts(path: &Path) -> OpenProbeGuard {
    assert!(
        path.is_absolute(),
        "visual-feed open probes require an exact absolute path"
    );
    let attempts = std::rc::Rc::new(std::cell::Cell::new(0));
    let state = OpenProbeState {
        path: path.to_owned(),
        attempts: std::rc::Rc::clone(&attempts),
    };
    let previous = OPEN_PROBE.with(|probe| probe.borrow_mut().replace(state));
    OpenProbeGuard { previous, attempts }
}

#[inline]
fn note_open_attempt(path: &Path) {
    #[cfg(not(debug_assertions))]
    let _ = path;
    #[cfg(debug_assertions)]
    OPEN_PROBE.with(|probe| {
        if let Some(probe) = probe.borrow().as_ref()
            && probe.path == path
        {
            probe.attempts.set(probe.attempts.get().saturating_add(1));
        }
    });
}

/// Maximum size of a user-supplied Sparkle Words lexicon override ([`crate::spec::MAX_TOY_PACK_BYTES`]).
pub const MAX_SPARKLE_LEXICON_BYTES: usize = 256 * 1024;

#[inline]
fn limit_plus_sentinel(max_bytes: usize) -> io::Result<usize> {
    let Some(limit) = max_bytes.checked_add(1) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "visual-feed byte limit cannot be represented",
        ));
    };
    Ok(limit)
}

#[inline]
fn too_large(max_bytes: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("visual-feed file exceeds the {max_bytes}-byte limit"),
    )
}

#[inline]
fn not_regular() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "visual-feed path is not a regular non-link file",
    )
}

/// Open `path` once and prove the handle is a regular file. `O_NONBLOCK` keeps a writerless
/// FIFO from parking `open`; `O_NOFOLLOW` refuses a final-component link.
#[cfg(unix)]
#[inline]
fn open_regular(path: &Path) -> io::Result<(std::fs::File, u64)> {
    use std::os::unix::fs::OpenOptionsExt as _;

    note_open_attempt(path);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(not_regular());
    }
    Ok((file, metadata.len()))
}

/// Windows' reparse-point flag stands in for `O_NOFOLLOW`; the attribute is checked on the
/// opened handle before any byte is read.
#[cfg(windows)]
#[inline]
fn open_regular(path: &Path) -> io::Result<(std::fs::File, u64)> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    note_open_attempt(path);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(not_regular());
    }
    Ok((file, metadata.len()))
}

/// Portable fallback. Targets without a non-blocking filesystem-open flag use
/// a no-link precheck and still verify the opened handle before reading.
#[cfg(not(any(unix, windows)))]
#[inline]
fn open_regular(path: &Path) -> io::Result<(std::fs::File, u64)> {
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(not_regular());
    }
    note_open_attempt(path);
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(not_regular());
    }
    Ok((file, metadata.len()))
}

#[inline]
fn check_observed_size(observed_len: u64, max_bytes: usize) -> io::Result<usize> {
    let max_u64 = if usize::BITS > u64::BITS && max_bytes > u64::MAX as usize {
        u64::MAX
    } else {
        max_bytes as u64
    };
    if observed_len > max_u64 {
        return Err(too_large(max_bytes));
    }
    Ok(observed_len as usize)
}

/// Read a same-handle regular file into a byte vector bounded by `max_bytes`.
#[inline]
pub fn read_bounded_regular_file(path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
    let (file, observed_len) = open_regular(path)?;
    let reserve_len = check_observed_size(observed_len, max_bytes)?;
    let read_limit = limit_plus_sentinel(max_bytes)?;
    let mut bytes = Vec::new();
    if let Err(error) = bytes.try_reserve_exact(reserve_len) {
        return Err(io::Error::new(
            io::ErrorKind::OutOfMemory,
            format!("visual-feed allocation refused: {error}"),
        ));
    }
    file.take(read_limit as u64).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(too_large(max_bytes));
    }
    Ok(bytes)
}

/// Read a bounded visual-feed file and require complete UTF-8.
#[inline]
pub fn read_bounded_regular_utf8(path: &Path, max_bytes: usize) -> io::Result<String> {
    match String::from_utf8(read_bounded_regular_file(path, max_bytes)?) {
        Ok(s) => Ok(s),
        Err(error) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("visual-feed file is not UTF-8: {error}"),
        )),
    }
}

/// Delimit and fold in the byte count, so the result fingerprints content, not hasher state.
#[inline]
const fn seal_fingerprint(mut hash: Fnv1a64, len: u64) -> u64 {
    hash.write_u8(0xff);
    hash.write(&len.to_le_bytes());
    hash.finish()
}

/// Fingerprint UTF-8 contents already admitted through [`read_bounded_regular_utf8`].
#[inline]
#[must_use]
pub const fn fingerprint_admitted_utf8(contents: &str) -> u64 {
    let mut hash = Fnv1a64::new();
    hash.write(contents.as_bytes());
    seal_fingerprint(hash, contents.len() as u64)
}

#[inline]
fn invalid_utf8() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "visual-feed file is not complete UTF-8",
    )
}

/// Hash an admitted UTF-8 feed without allocating its contents.
#[inline]
pub fn fingerprint_bounded_regular_utf8(path: &Path, max_bytes: usize) -> io::Result<u64> {
    let (mut file, observed_len) = open_regular(path)?;
    let _ = check_observed_size(observed_len, max_bytes)?;
    let read_limit = limit_plus_sentinel(max_bytes)?;
    let mut total = 0usize;
    let mut hash = Fnv1a64::new();
    let mut read_buffer = [0u8; 8 * 1024];
    let mut utf8_buffer = [0u8; 8 * 1024 + 3];
    let mut carry = [0u8; 3];
    let mut carry_len = 0usize;

    loop {
        let remaining = read_limit.saturating_sub(total);
        if remaining == 0 {
            return Err(too_large(max_bytes));
        }
        let request = remaining.min(read_buffer.len());
        let count = file.read(&mut read_buffer[..request])?;
        if count == 0 {
            break;
        }
        total += count;
        if total > max_bytes {
            return Err(too_large(max_bytes));
        }
        hash.write(&read_buffer[..count]);

        let candidate = if carry_len == 0 {
            &read_buffer[..count]
        } else {
            utf8_buffer[..carry_len].copy_from_slice(&carry[..carry_len]);
            utf8_buffer[carry_len..carry_len + count].copy_from_slice(&read_buffer[..count]);
            &utf8_buffer[..carry_len + count]
        };
        match std::str::from_utf8(candidate) {
            Ok(_) => carry_len = 0,
            Err(error) if error.error_len().is_none() => {
                let suffix = &candidate[error.valid_up_to()..];
                if suffix.len() > carry.len() {
                    return Err(invalid_utf8());
                }
                carry[..suffix.len()].copy_from_slice(suffix);
                carry_len = suffix.len();
            }
            Err(_) => return Err(invalid_utf8()),
        }
    }

    if carry_len != 0 {
        return Err(invalid_utf8());
    }
    Ok(seal_fingerprint(hash, total as u64))
}

#[cfg(test)]
#[path = "file_feed_tests.rs"]
mod tests;
