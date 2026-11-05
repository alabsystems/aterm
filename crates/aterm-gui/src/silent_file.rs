// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A file no read is ever answered from, for tests: a FIFO with no writer, whose `open(2)` for
//! reading blocks as a read on a network home whose server stopped answering does
//! (`docs/DESIGN-hung-network-volumes-2026-10-20.md`).

use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A FIFO at a path in the test's own scratch folder.
pub(crate) struct SilentFile(PathBuf);

impl SilentFile {
    pub(crate) fn at(path: &Path) -> Self {
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("no NUL");
        // SAFETY: `c` is a live NUL-terminated pathname in the test's own scratch folder, and
        // mkfifo retains no pointer.
        assert_eq!(
            unsafe { libc::mkfifo(c.as_ptr(), 0o600) },
            0,
            "a FIFO at {}",
            path.display()
        );
        Self(path.to_path_buf())
    }

    /// Lets the reader blocked on it go, waiting for one within the hang detector's minute: its
    /// write end opened and closed, so that read ends empty. Whether a reader came.
    pub(crate) fn release(&self) -> bool {
        aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            Duration::from_millis(5),
            || {
                std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                    .open(&self.0)
                    .ok()
            },
        )
        .is_some()
    }
}
