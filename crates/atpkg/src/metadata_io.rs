// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded, same-handle reads for private package metadata files.

use std::io::{self, Read as _};
use std::path::Path;

fn too_large(limit: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("package metadata exceeds the {limit}-byte limit"),
    )
}

fn not_regular() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "package metadata is not a regular non-link file",
    )
}

/// Opens an existing regular metadata file without following its final link.
#[cfg(unix)]
pub(crate) fn open_regular(path: &Path) -> io::Result<std::fs::File> {
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

/// Windows form of the existing regular-file admission, refusing reparse points.
#[cfg(windows)]
pub(crate) fn open_regular(path: &Path) -> io::Result<std::fs::File> {
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

/// Portable fallback with before/after regular-file checks on other targets.
#[cfg(not(any(unix, windows)))]
pub(crate) fn open_regular(path: &Path) -> io::Result<std::fs::File> {
    let before = std::fs::symlink_metadata(path)?;
    if !before.file_type().is_file() {
        return Err(not_regular());
    }
    let file = std::fs::File::open(path)?;
    let opened = file.metadata()?;
    let after = std::fs::symlink_metadata(path)?;
    if !opened.file_type().is_file()
        || !after.file_type().is_file()
        || before.len() != opened.len()
        || after.len() != opened.len()
        || before.modified().ok() != opened.modified().ok()
        || after.modified().ok() != opened.modified().ok()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "package metadata changed while it was opened",
        ));
    }
    Ok(file)
}

/// Read one regular, non-link file through the same handle used for admission.
pub(crate) fn read_bounded_regular(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let mut file = open_regular(path)?;
    let before = file.metadata()?;
    let limit_u64 = u64::try_from(limit).unwrap_or(u64::MAX);
    if before.len() > limit_u64 {
        return Err(too_large(limit));
    }
    let sentinel = limit.checked_add(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "package metadata byte limit cannot be represented",
        )
    })?;
    let initial_capacity = usize::try_from(before.len()).unwrap_or(limit).min(limit);
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(initial_capacity).map_err(|error| {
        io::Error::new(
            io::ErrorKind::OutOfMemory,
            format!("package metadata allocation refused: {error}"),
        )
    })?;
    std::io::Read::by_ref(&mut file)
        .take(u64::try_from(sentinel).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(too_large(limit));
    }
    let after = file.metadata()?;
    if !after.file_type().is_file() || after.len() != bytes.len() as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "package metadata changed while it was read",
        ));
    }
    Ok(bytes)
}

/// Reads at most `limit` leading bytes of a regular, non-link file.
pub(crate) fn read_head(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let file = open_regular(path)?;
    let mut head = Vec::with_capacity(limit);
    file.take(u64::try_from(limit).unwrap_or(u64::MAX))
        .read_to_end(&mut head)?;
    Ok(head)
}

/// Read one admitted metadata file and require complete UTF-8.
pub(crate) fn read_bounded_regular_utf8(path: &Path, limit: usize) -> io::Result<String> {
    String::from_utf8(read_bounded_regular(path, limit)?).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("package metadata is not UTF-8: {error}"),
        )
    })
}

/// Resolves a bounded chain of final-component symlinks and reads the target as bounded UTF-8.
pub(crate) fn read_bounded_regular_utf8_follow_final_links(
    path: &Path,
    limit: usize,
) -> io::Result<String> {
    const MAX_LINKS: usize = 16;

    let mut resolved = path.to_path_buf();
    for _ in 0..MAX_LINKS {
        let metadata = match std::fs::symlink_metadata(&resolved) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return read_bounded_regular_utf8(&resolved, limit);
            }
            Err(error) => return Err(error),
        };
        if !metadata.file_type().is_symlink() {
            return read_bounded_regular_utf8(&resolved, limit);
        }
        let target = std::fs::read_link(&resolved)?;
        resolved = if target.is_absolute() {
            target
        } else {
            resolved
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(target)
        };
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "package metadata symlink chain exceeds 16 links",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(label: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-metadata-{label}-"))
    }

    #[test]
    fn exact_boundary_is_admitted_and_one_byte_over_is_rejected() {
        let root = fixture("boundary");
        let exact = root.join("exact");
        let over = root.join("over");
        std::fs::write(&exact, b"12345678").unwrap();
        std::fs::write(&over, b"123456789").unwrap();
        assert_eq!(read_bounded_regular(&exact, 8).unwrap(), b"12345678");
        let error = read_bounded_regular(&over, 8).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn writerless_fifo_and_final_symlink_return_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;

        let root = fixture("special");
        let fifo = root.join("fifo");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `fifo_c` is a live NUL-terminated path in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        assert_eq!(
            read_bounded_regular(&fifo, 64).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );

        let target = root.join("target");
        let link = root.join("link");
        std::fs::write(&target, b"status").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(read_bounded_regular(&link, 64).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn explicit_follow_reader_supports_and_safely_retargets_config_symlinks() {
        let root = fixture("follow-link");
        let first = root.join("first.toml");
        let second = root.join("second.toml");
        let logical = root.join("aterm.toml");
        std::fs::write(&first, "[packages]\nchannel = \"first\"\n").unwrap();
        std::fs::write(&second, "[packages]\nchannel = \"second\"\n").unwrap();
        std::os::unix::fs::symlink(&first, &logical).unwrap();
        assert!(
            read_bounded_regular_utf8_follow_final_links(&logical, 128)
                .unwrap()
                .contains("first")
        );
        std::fs::remove_file(&logical).unwrap();
        std::os::unix::fs::symlink(&second, &logical).unwrap();
        assert!(
            read_bounded_regular_utf8_follow_final_links(&logical, 128)
                .unwrap()
                .contains("second")
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
