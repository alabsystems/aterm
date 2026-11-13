// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Clear only an empty directory placeholder; never unlink an appearing user file.

use std::io;
use std::path::Path;

pub(super) fn remove(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        // rmdir decides the final entry's kind and emptiness atomically. In particular,
        // a symlink or file that appeared since a probe remains exactly where it is.
        match std::fs::remove_dir(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(all(test, unix))]
#[path = "remove_empty_dir_tests.rs"]
mod tests;
