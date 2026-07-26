// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded admission for config-referenced visual-effect files.
//!
//! A manifest path is not proof that its target is a normal file: a FIFO can
//! park `open`, a device can produce an unbounded stream, and a pathname can be
//! swapped between a metadata check and a second open.  This module opens once,
//! makes that open non-blocking where the platform exposes the flag, checks the
//! opened handle itself, and reads at most the caller's limit plus one sentinel
//! byte.  Unix and Windows also refuse a final-component link/reparse point;
//! config authors should name the actual feed file.

use std::io::{self, Read as _};
use std::path::Path;

/// Maximum accepted size of a user-supplied Sparkle Words lexicon override.
///
/// The override is parsed and merged on config admission, so matching the
/// 256-KiB Toy Pack ceiling leaves generous room for authored entries while
/// placing a hard bound on both file I/O and parser input.
pub const MAX_SPARKLE_LEXICON_BYTES: usize = 256 * 1024;

fn limit_plus_sentinel(max_bytes: usize) -> io::Result<usize> {
    max_bytes.checked_add(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "visual-feed byte limit cannot be represented",
        )
    })
}

fn too_large(max_bytes: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("visual-feed file exceeds the {max_bytes}-byte limit"),
    )
}

fn not_regular() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "visual-feed path is not a regular non-link file",
    )
}

/// Open `path` once and prove that the handle which will be read is a regular
/// file. `O_NONBLOCK` prevents a planted writerless FIFO from parking `open`;
/// `O_NOFOLLOW` closes the final-component replacement/link ambiguity.
#[cfg(unix)]
fn open_regular(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(not_regular());
    }
    Ok(file)
}

/// Windows' reparse-point flag is the closest equivalent of `O_NOFOLLOW`.
/// The attribute is checked on the opened handle before any byte is consumed.
#[cfg(windows)]
fn open_regular(path: &Path) -> io::Result<std::fs::File> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(not_regular());
    }
    Ok(file)
}

/// Portable fallback. Targets without a non-blocking filesystem-open flag use
/// a no-link precheck and still verify the opened handle before reading.
#[cfg(not(any(unix, windows)))]
fn open_regular(path: &Path) -> io::Result<std::fs::File> {
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(not_regular());
    }
    let file = std::fs::File::open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(not_regular());
    }
    Ok(file)
}

fn check_observed_size(file: &std::fs::File, max_bytes: usize) -> io::Result<()> {
    if file.metadata()?.len() > u64::try_from(max_bytes).unwrap_or(u64::MAX) {
        return Err(too_large(max_bytes));
    }
    Ok(())
}

/// Read a same-handle regular file into a bounded byte vector.
///
/// Metadata provides an early rejection only. The `max + 1` sentinel read is
/// still authoritative, so a regular file which grows after `fstat` cannot
/// exceed the allocation or enter a parser truncated at the limit.
pub fn read_bounded_regular_file(path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
    let file = open_regular(path)?;
    check_observed_size(&file, max_bytes)?;
    let read_limit = limit_plus_sentinel(max_bytes)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(
            usize::try_from(file.metadata()?.len())
                .unwrap_or(max_bytes)
                .min(max_bytes),
        )
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::OutOfMemory,
                format!("visual-feed allocation refused: {error}"),
            )
        })?;
    file.take(read_limit as u64).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(too_large(max_bytes));
    }
    Ok(bytes)
}

/// Read a bounded visual-feed file and require complete UTF-8.
pub fn read_bounded_regular_utf8(path: &Path, max_bytes: usize) -> io::Result<String> {
    String::from_utf8(read_bounded_regular_file(path, max_bytes)?).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("visual-feed file is not UTF-8: {error}"),
        )
    })
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fingerprint_fold(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn invalid_utf8() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "visual-feed file is not complete UTF-8",
    )
}

/// Hash an admitted UTF-8 feed without allocating its full contents.
///
/// This uses the exact same open/regular-file/size/UTF-8 admission rules as
/// [`read_bounded_regular_utf8`]. Reads stop at `max + 1`, and UTF-8 split
/// across two read buffers is carried explicitly, so fingerprints cannot wait
/// for a FIFO writer or allocate in proportion to an oversized path.
pub fn fingerprint_bounded_regular_utf8(path: &Path, max_bytes: usize) -> io::Result<u64> {
    let mut file = open_regular(path)?;
    check_observed_size(&file, max_bytes)?;
    let read_limit = limit_plus_sentinel(max_bytes)?;
    let mut total = 0usize;
    let mut hash = FNV_OFFSET;
    let mut read_buffer = [0u8; 8 * 1024];
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
        hash = fingerprint_fold(hash, &read_buffer[..count]);

        let mut utf8_buffer = [0u8; 8 * 1024 + 3];
        utf8_buffer[..carry_len].copy_from_slice(&carry[..carry_len]);
        utf8_buffer[carry_len..carry_len + count].copy_from_slice(&read_buffer[..count]);
        let candidate = &utf8_buffer[..carry_len + count];
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
    // Delimit and fold the byte count so this remains a content fingerprint,
    // not merely the raw streaming hasher state.
    hash = fingerprint_fold(hash, &[0xff]);
    Ok(fingerprint_fold(hash, &(total as u64).to_le_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fixture_dir(label: &str) -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "aterm-visual-feed-{label}-{}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create visual-feed fixture directory");
        path
    }

    #[test]
    fn regular_file_round_trips_and_fingerprints_content() {
        let root = fixture_dir("regular");
        let path = root.join("feed.toml");
        std::fs::write(&path, "snowcat 😺\n").expect("write regular feed");
        assert_eq!(
            read_bounded_regular_utf8(&path, 64).expect("regular feed reads"),
            "snowcat 😺\n"
        );
        let before = fingerprint_bounded_regular_utf8(&path, 64).expect("fingerprint feed");
        assert_eq!(
            before,
            fingerprint_bounded_regular_utf8(&path, 64).expect("fingerprint is stable")
        );
        std::fs::write(&path, "snowcat 🐱\n").expect("edit regular feed");
        assert_ne!(
            before,
            fingerprint_bounded_regular_utf8(&path, 64).expect("fingerprint edited feed")
        );
        std::fs::remove_dir_all(root).expect("remove regular fixtures");
    }

    #[test]
    fn oversize_directory_and_invalid_utf8_are_rejected_by_read_and_hash() {
        let root = fixture_dir("invalid");
        assert_eq!(
            read_bounded_regular_utf8(&root, 8)
                .expect_err("directory is not a feed")
                .kind(),
            io::ErrorKind::InvalidInput
        );

        let oversized = root.join("oversized.toml");
        std::fs::write(&oversized, b"123456789").expect("write oversized feed");
        for error in [
            read_bounded_regular_utf8(&oversized, 8).expect_err("oversized read rejected"),
            fingerprint_bounded_regular_utf8(&oversized, 8)
                .expect_err("oversized fingerprint rejected"),
        ] {
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().contains("8-byte"), "{error}");
        }

        let invalid_utf8 = root.join("invalid-utf8.toml");
        std::fs::write(&invalid_utf8, [0xf0, 0x9f, 0x98]).expect("write truncated UTF-8");
        assert_eq!(
            read_bounded_regular_utf8(&invalid_utf8, 8)
                .expect_err("invalid UTF-8 read rejected")
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            fingerprint_bounded_regular_utf8(&invalid_utf8, 8)
                .expect_err("invalid UTF-8 fingerprint rejected")
                .kind(),
            io::ErrorKind::InvalidData
        );
        std::fs::remove_dir_all(root).expect("remove invalid fixtures");
    }

    #[cfg(unix)]
    #[test]
    fn writerless_fifo_and_final_symlink_are_rejected_at_open() {
        use std::os::unix::ffi::OsStrExt as _;

        let root = fixture_dir("special");
        let fifo = root.join("feed.fifo");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path");
        // SAFETY: `fifo_c` is a live NUL-terminated pathname and mkfifo retains
        // no pointer. The private test directory makes the name unique.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let fifo_error = read_bounded_regular_utf8(&fifo, 64)
            .expect_err("writerless FIFO must return without blocking");
        assert_eq!(fifo_error.kind(), io::ErrorKind::InvalidInput);

        let regular = root.join("regular.toml");
        let linked = root.join("linked.toml");
        std::fs::write(&regular, "ok\n").expect("write symlink target");
        std::os::unix::fs::symlink(&regular, &linked).expect("create final symlink");
        assert!(
            read_bounded_regular_utf8(&linked, 64).is_err(),
            "final-component links are not admitted"
        );
        std::fs::remove_dir_all(root).expect("remove special fixtures");
    }
}
