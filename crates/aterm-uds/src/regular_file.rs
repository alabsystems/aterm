// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only regular files, opened without following a link at the final component.

#[cfg(any(unix, windows))]
use std::fs::OpenOptions;
use std::fs::{File, Metadata};
use std::io;
use std::path::Path;

/// Open `path` for reading and return the opened file's metadata. A final symlink or Windows
/// reparse point is refused. Ancestor directories may be links: callers that need to confine
/// the entire path must establish that separately. Unix opens are nonblocking, so a file that
/// becomes a FIFO after the path check cannot wait for a writer.
///
/// This acquires no file lock and imposes no byte limit. A caller that streams or parses the
/// file retains its own size, freshness and locking policy.
///
/// # Errors
/// The path, open or metadata error, or [`io::ErrorKind::InvalidData`] when the final component
/// is a link, reparse point or anything other than a regular file. Other than Unix and Windows,
/// this returns [`io::ErrorKind::Unsupported`] before examining `path`.
pub fn open(path: &Path) -> io::Result<(File, Metadata)> {
    #[cfg(any(unix, windows))]
    {
        open_after_check(path, || {})
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(any(unix, windows))]
fn open_after_check(path: &Path, checked: impl FnOnce()) -> io::Result<(File, Metadata)> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "not a regular file");
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(invalid());
    }
    checked();
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(invalid());
        }
    }
    Ok((file, metadata))
}

#[cfg(test)]
mod tests;
