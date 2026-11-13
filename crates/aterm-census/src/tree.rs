// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The census's reads of the checkout it walks.

use std::io;
use std::path::{Path, PathBuf};

use crate::memo::Op;

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

/// Open `path` exclusively (`create_new`) with non-blocking and close-on-exec flags where available.
pub(crate) fn open_create_new(path: &Path) -> io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
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
    opts.open(path)
}

/// Open `path` without blocking on FIFOs and verify the descriptor is a regular file within
/// [`MAX_FILE_BYTES`], preserving `IsADirectory` on directories. What a caller reads from the
/// file answers no recorded question, so a memo unit that opens one keeps no record
/// ([`crate::memo::untraceable`]); the corpus's loader reads scripts by it.
pub(crate) fn open_regular(path: &Path) -> io::Result<(std::fs::File, usize)> {
    crate::memo::untraceable("a unit opened a file whose reads the memo cannot follow");
    open_regular_unrecorded(path)
}

/// [`open_regular`] for a read that answers no census question: the census reads above it, and
/// the input capture's own fingerprints (`inputs`).
pub(crate) fn open_regular_unrecorded(path: &Path) -> io::Result<(std::fs::File, usize)> {
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
    let read = read_bytes_now(path);
    crate::memo::note_tree(path, Op::Bytes, |_, _| crate::memo::bytes_answer(&read));
    read
}

/// The answer `op` at `path` gives now, as a memo unit's recorded question holds it: what the
/// call of [`crate::memo::Op`]'s name returns, digested as that call records it; a text read of
/// a file the run's snapshot read answers as the snapshot read it. It records nothing itself.
pub(crate) fn answer_now(
    op: Op,
    path: &Path,
    snapshot: &crate::memo::Snapshot,
    root: &Path,
) -> crate::memo::Digest {
    match op {
        Op::Text => crate::memo::snapshot_answer(snapshot, root, path)
            .unwrap_or_else(|| text_digest(&read_to_string_now(path), path, snapshot, root)),
        Op::Bytes => crate::memo::bytes_answer(&read_bytes_now(path)),
        Op::List => list_digest(&read_dir_now(path)),
        Op::ListTyped => typed_digest(&read_dir_typed_now(path)),
        Op::IsDir => crate::memo::bool_answer(is_dir_now(path)),
        Op::IsFile => crate::memo::bool_answer(is_file_now(path)),
        Op::Exists => crate::memo::bool_answer(exists_now(path)),
    }
}

fn text_digest(
    read: &io::Result<String>,
    path: &Path,
    snapshot: &crate::memo::Snapshot,
    root: &Path,
) -> crate::memo::Digest {
    match read {
        Ok(text) => crate::memo::snapshot_text(snapshot, root, path, text)
            .unwrap_or_else(|| crate::memo::text_answer(read)),
        Err(_) => crate::memo::text_answer(read),
    }
}

fn list_digest(listed: &io::Result<Vec<io::Result<PathBuf>>>) -> crate::memo::Digest {
    crate::memo::listing_answer(listed, |entry: &PathBuf| name_bytes(entry))
}

fn typed_digest(listed: &io::Result<Vec<io::Result<(PathBuf, bool)>>>) -> crate::memo::Digest {
    crate::memo::listing_answer(listed, |(entry, dir): &(PathBuf, bool)| {
        let mut name = name_bytes(entry);
        name.push(u8::from(*dir));
        name
    })
}

fn read_bytes_now(path: &Path) -> io::Result<Vec<u8>> {
    use std::io::Read as _;
    let (file, cap) = open_regular_unrecorded(path)?;
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
    let read = read_to_string_now(path);
    crate::memo::note_tree(path, Op::Text, |snapshot, root| {
        text_digest(&read, path, snapshot, root)
    });
    read
}

fn read_to_string_now(path: &Path) -> io::Result<String> {
    #[cfg(test)]
    if let Some(answer) = memory::answer(path, memory::Files::read) {
        return answer;
    }
    use std::io::Read as _;
    let (file, cap) = open_regular_unrecorded(path)?;
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
    let answer = is_dir_now(path);
    crate::memo::note_tree(path, Op::IsDir, |_, _| crate::memo::bool_answer(answer));
    answer
}

fn is_dir_now(path: &Path) -> bool {
    #[cfg(test)]
    if let Some(answer) = memory::answer(path, memory::Files::is_dir) {
        return answer;
    }
    path.is_dir()
}

/// Whether `path` is a file (`Path::is_file`).
pub(crate) fn is_file(path: &Path) -> bool {
    crate::inputs::before(path, "is-file");
    let answer = is_file_now(path);
    crate::memo::note_tree(path, Op::IsFile, |_, _| crate::memo::bool_answer(answer));
    answer
}

fn is_file_now(path: &Path) -> bool {
    #[cfg(test)]
    if let Some(answer) = memory::answer(path, memory::Files::is_file) {
        return answer;
    }
    path.is_file()
}

/// Whether anything is at `path` (`Path::exists`).
pub(crate) fn exists(path: &Path) -> bool {
    crate::inputs::before(path, "exists");
    let answer = exists_now(path);
    crate::memo::note_tree(path, Op::Exists, |_, _| crate::memo::bool_answer(answer));
    answer
}

fn exists_now(path: &Path) -> bool {
    #[cfg(test)]
    if let Some(answer) = memory::answer(path, |files, rel| files.is_file(rel) || files.is_dir(rel))
    {
        return answer;
    }
    path.exists()
}

/// List the entries of the directory at `path`.
pub(crate) fn read_dir(path: &Path) -> io::Result<Vec<io::Result<PathBuf>>> {
    crate::inputs::before(path, "dir");
    let listed = read_dir_now(path);
    crate::memo::note_tree(path, Op::List, |_, _| list_digest(&listed));
    listed
}

/// An entry's name as a listing's answer holds it.
fn name_bytes(entry: &Path) -> Vec<u8> {
    entry
        .file_name()
        .map_or_else(Vec::new, |n| n.as_encoded_bytes().to_vec())
}

fn read_dir_now(path: &Path) -> io::Result<Vec<io::Result<PathBuf>>> {
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
    let listed = read_dir_typed_now(path);
    crate::memo::note_tree(path, Op::ListTyped, |_, _| typed_digest(&listed));
    listed
}

fn read_dir_typed_now(path: &Path) -> io::Result<Vec<io::Result<(PathBuf, bool)>>> {
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
