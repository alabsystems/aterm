// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An original-directory constraint narrows fresh creation, never adopts input.
use super::*;
use std::os::fd::{AsRawFd as _, BorrowedFd};

impl Producer {
    /// Create a fresh namespace only beneath this actual retained directory.
    /// The readonly descriptor adds a restriction before mkdir; it supplies no
    /// existing input, source, SDK, completion or native admission authority.
    ///
    /// # Errors
    /// As [`Self::create`], or the named parent differs from `original`.
    pub fn create_original_parent(
        parent_path: &Path,
        name: &str,
        original: BorrowedFd<'_>,
    ) -> io::Result<Self> {
        Self::create_with_parent(parent_path, name, Some(original), |_| Ok(()))
    }
}

pub(super) fn open(path: &Path, original: Option<BorrowedFd<'_>>) -> io::Result<File> {
    let Some(original) = original else {
        return fd::openat(
            fd::CWD,
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(io::Error::from);
    };
    // SAFETY: observe the borrowed original without changing flags or cursor.
    let flags = unsafe { libc::fcntl(original.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if flags & libc::O_ACCMODE != libc::O_RDONLY {
        return Err(refused("original private input parent is not readonly"));
    }
    let file = aterm_uds::kept_out(|| original.try_clone_to_owned().map(File::from))?;
    let actual = file.metadata()?;
    let named = std::fs::symlink_metadata(path)?;
    if !actual.is_dir()
        || !named.is_dir()
        || (actual.dev(), actual.ino()) != (named.dev(), named.ino())
    {
        return Err(refused(
            "private input parent is not the original directory",
        ));
    }
    Ok(file)
}

#[cfg(test)]
mod tests;
