// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The census's reads of the checkout it walks.

use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

/// Upper bound on any single file read by the census (64 MiB).
pub(crate) const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    any(
        target_arch = "x86",
        target_arch = "x86_64",
        target_arch = "riscv64",
        target_arch = "aarch64",
        target_arch = "arm",
    ),
))]
const NONBLOCK_OPEN_FLAG: i32 = 0x0000_0800 | 0x0008_0000;

#[cfg(any(target_os = "macos", target_os = "ios"))]
const NONBLOCK_OPEN_FLAG: i32 = 0x0000_0004 | 0x0100_0000;

#[cfg(any(
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly",
))]
const NONBLOCK_OPEN_FLAG: i32 = 0x0000_0004;

/// Open `path` without blocking on FIFOs and verify the descriptor is a regular file within
/// [`MAX_FILE_BYTES`], preserving `IsADirectory` on directories. It records no input
/// ([`crate::inputs::before`]): the corpus's loader reads scripts by it, and the input capture
/// its own fingerprints.
pub(crate) fn open_regular(path: &Path) -> io::Result<(std::fs::File, usize)> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(any(
        all(
            any(target_os = "linux", target_os = "android"),
            any(
                target_arch = "x86",
                target_arch = "x86_64",
                target_arch = "riscv64",
                target_arch = "aarch64",
                target_arch = "arm",
            ),
        ),
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly",
    ))]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(NONBLOCK_OPEN_FLAG);
    }
    let file = opts.open(path)?;
    let meta = file.metadata()?;
    if meta.is_dir() {
        return Err(io::ErrorKind::IsADirectory.into());
    }
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} exceeds {} byte census cap ({} bytes)",
                path.display(),
                MAX_FILE_BYTES,
                meta.len()
            ),
        ));
    }
    Ok((file, usize::try_from(meta.len()).unwrap_or(0)))
}

/// The regular file at `path`, bounded by [`MAX_FILE_BYTES`].
pub(crate) fn read_bytes(path: &Path) -> io::Result<Vec<u8>> {
    crate::inputs::before(path, "file");
    let (file, cap) = open_regular(path)?;
    let mut out = Vec::with_capacity(cap.saturating_add(1));
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut out)?;
    if out.len() as u64 > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} grew past {} byte census cap",
                path.display(),
                MAX_FILE_BYTES
            ),
        ));
    }
    Ok(out)
}

/// The file at `path`, as UTF-8 (`std::fs::read_to_string`).
pub(crate) fn read_to_string(path: &Path) -> io::Result<String> {
    crate::inputs::before(path, "file");
    #[cfg(test)]
    if let Some(answer) = memory::answer(path, memory::Files::read) {
        return answer;
    }
    let (file, cap) = open_regular(path)?;
    let mut out = String::with_capacity(cap.saturating_add(1));
    file.take(MAX_FILE_BYTES + 1).read_to_string(&mut out)?;
    if out.len() as u64 > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} grew past {} byte census cap",
                path.display(),
                MAX_FILE_BYTES
            ),
        ));
    }
    Ok(out)
}

/// Whether `path` is a directory (`Path::is_dir`).
pub(crate) fn is_dir(path: &Path) -> bool {
    crate::inputs::before(path, "is-dir");
    #[cfg(test)]
    if let Some(answer) = memory::answer(path, memory::Files::is_dir) {
        return answer;
    }
    path.is_dir()
}

/// Whether `path` is a file (`Path::is_file`).
pub(crate) fn is_file(path: &Path) -> bool {
    crate::inputs::before(path, "is-file");
    #[cfg(test)]
    if let Some(answer) = memory::answer(path, memory::Files::is_file) {
        return answer;
    }
    path.is_file()
}

/// List the entries of the directory at `path`.
pub(crate) fn read_dir(path: &Path) -> io::Result<Vec<io::Result<PathBuf>>> {
    crate::inputs::before(path, "dir");
    #[cfg(test)]
    if let Some(answer) = memory::answer(path, |files, rel| files.read_dir(path, rel)) {
        return answer;
    }
    Ok(std::fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect())
}

/// List the entries of the directory at `path` along with whether each entry is a directory.
pub(crate) fn read_dir_typed(path: &Path) -> io::Result<Vec<io::Result<(PathBuf, bool)>>> {
    crate::inputs::before(path, "dir");
    #[cfg(test)]
    if let Some(answer) = memory::answer(path, |files, rel| {
        Ok(files
            .read_dir(path, rel)?
            .into_iter()
            .map(|res| {
                res.map(|entry_path| {
                    let is_dir = entry_path
                        .file_name()
                        .is_some_and(|name| files.is_dir(&rel.join(name)));
                    (entry_path, is_dir)
                })
            })
            .collect())
    }) {
        return answer;
    }
    Ok(std::fs::read_dir(path)?
        .map(|entry| {
            entry.map(|entry| {
                let entry_path = entry.path();
                let is_dir = match entry.file_type() {
                    Ok(ft) if ft.is_dir() => true,
                    Ok(ft) if ft.is_file() => false,
                    _ => entry_path.is_dir(),
                };
                (entry_path, is_dir)
            })
        })
        .collect())
}

#[cfg(test)]
#[path = "tree_memory_tests.rs"]
pub(crate) mod memory;
