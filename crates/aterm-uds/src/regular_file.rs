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
    #[cfg(unix)]
    {
        open_unix(path)
    }
    #[cfg(windows)]
    {
        open_after_check(path, || {})
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

/// Make a distinct copy-on-write file from the actual held regular description.
/// This never reopens a source pathname, changes its cursor/flags or replaces an
/// existing destination. The caller owns destination permissions and durability.
/// `false` means this OS/filesystem cannot clone; callers retain their bounded
/// copy policy. Resource exhaustion and other errors are returned unchanged.
///
/// # Errors
/// The source is not regular, the destination exists, or the native clone fails.
pub fn clone_held(source: &File, destination: &Path) -> io::Result<bool> {
    if !source.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular clone source",
        ));
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::{fd::AsRawFd as _, unix::ffi::OsStrExt as _};
        let destination = std::ffi::CString::new(destination.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        unsafe extern "C" {
            fn fclonefileat(
                source: i32,
                directory: i32,
                path: *const std::ffi::c_char,
                flags: u32,
            ) -> i32;
        }
        // CLONE_NOFOLLOW | CLONE_NOOWNERCOPY. Native cloning creates a distinct
        // inode owned by this caller, sharing immutable blocks until a write.
        // SAFETY: the source File and destination CString remain live; no
        // borrowed source path or mutable descriptor flags enter the operation.
        if unsafe {
            fclonefileat(
                source.as_raw_fd(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                0x1 | 0x2,
            )
        } == 0
        {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if matches!(
            error.raw_os_error(),
            Some(libc::EXDEV | libc::ENOTSUP | libc::ENOSYS)
        ) {
            return Ok(false);
        }
        Err(error)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = destination;
        Ok(false)
    }
}

#[cfg(unix)]
fn open_unix(path: &Path) -> io::Result<(File, Metadata)> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "not a regular file");
    let mut options = OpenOptions::new();
    options.read(true);
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound
                && std::fs::symlink_metadata(path).is_ok_and(|m| !m.file_type().is_file())
            {
                return Err(invalid());
            }
            return Err(err);
        }
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid());
    }
    Ok((file, metadata))
}

#[cfg(any(test, windows))]
fn open_after_check(path: &Path, checked: impl FnOnce()) -> io::Result<(File, Metadata)> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "not a regular file");
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(invalid());
    }
    checked();
    #[cfg(unix)]
    {
        open_unix(path)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        let mut options = OpenOptions::new();
        options.read(true);
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let file = options.open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(invalid());
        }
        Ok((file, metadata))
    }
}

#[cfg(test)]
mod tests;
