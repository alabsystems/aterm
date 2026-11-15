// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The search a Linux spawn makes for its program: glibc 2.39's `execvp`'s (`__execvpe_common`),
//! the rule `std`'s forked child followed, so a spawn finds what `std` found. The candidates are
//! made here, in the parent; the child walks them with `execve` alone, after its `chdir`, so a
//! relative entry (and an empty one, the program's own name) is searched where the child runs.
//! glibc sets no limit on the name: its `NAME_MAX` check reads a length it capped at `NAME_MAX`,
//! so it never refuses, and each exec answers a long name (`ENAMETOOLONG` where the entry's
//! directory exists, which ends the walk).

use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt as _;

/// glibc's `PATH_MAX`: its buffer holds `PATH` up to `PATH_MAX - 1` bytes and a NUL, so an entry
/// of `PATH_MAX` bytes or more is passed over.
const PATH_MAX: usize = 4096;
/// glibc's `_CS_PATH`, searched when the child has no `PATH`.
const CS_PATH: &[u8] = b"/bin:/usr/bin";

/// The paths a child tries for its program, in order, and whether a file the kernel will not run
/// (`ENOEXEC`) goes to the system shell, as `execvp` hands it (not for a native image,
/// [`super::NativeImage`]).
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Search {
    pub(super) candidates: Vec<CString>,
    pub(super) script: bool,
}

impl Search {
    /// `program` exactly as named, its one candidate, with no shell over it: a plain `execve`.
    pub(super) fn exact(program: &CStr) -> Self {
        Self {
            candidates: vec![program.to_owned()],
            script: false,
        }
    }

    /// The search for `program` on `path`, the child's `PATH` (`None`: it has none).
    ///
    /// # Errors
    /// `ENOENT` for an empty name, `execvp`'s answer before its first exec. A NUL cannot occur
    /// (`program` is a C string, and an entry is a part of an `OsStr` with none).
    pub(super) fn new(program: &CStr, path: Option<&OsStr>, native: bool) -> io::Result<Self> {
        let name = program.to_bytes();
        if name.is_empty() {
            return Err(io::Error::from_raw_os_error(libc::ENOENT));
        }
        let script = !native;
        if name.contains(&b'/') {
            return Ok(Self {
                candidates: vec![program.to_owned()],
                script,
            });
        }
        let path = path.map_or(CS_PATH, OsStr::as_bytes);
        let mut candidates = Vec::new();
        let mut entries = path.split(|&b| b == b':').peekable();
        while let Some(entry) = entries.next() {
            if entry.len() >= PATH_MAX {
                // glibc's walk steps past such an entry onto its `:`, where it reads an empty
                // entry, the name itself; after the last entry it ends.
                if entries.peek().is_some() {
                    candidates.push(program.to_owned());
                }
                continue;
            }
            let mut joined = Vec::with_capacity(entry.len() + 1 + name.len());
            if !entry.is_empty() {
                joined.extend_from_slice(entry);
                joined.push(b'/');
            }
            joined.extend_from_slice(name);
            candidates.push(super::c_string(&joined)?);
        }
        Ok(Self { candidates, script })
    }
}

#[cfg(test)]
#[path = "spawnfd_search_tests.rs"]
mod tests;
