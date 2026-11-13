// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Publication of performance baselines and retained reports.

use std::io::{self, Write};
use std::path::Path;

pub(super) type Publisher = fn(&Path, &str) -> io::Result<()>;

pub(super) fn publish(path: &Path, text: &str) -> io::Result<()> {
    publish_with(path, |writer| writer.write_all(text.as_bytes()))
}

pub(super) fn publish_with(
    path: &Path,
    produce: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    #[cfg(any(unix, windows))]
    {
        let target = match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(path)?,
            Ok(_) => path.to_path_buf(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
            Err(error) => return Err(error),
        };
        let parent = target
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if !std::fs::metadata(parent)?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "publication parent is not a directory",
            ));
        }
        let metadata = match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.is_file() => Some(meta),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "publication target is not a regular file",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt as _;
            metadata.map_or(0o644, |meta| meta.permissions().mode() & 0o777)
        };
        #[cfg(windows)]
        let mode = {
            let _ = metadata;
            0o644
        };
        aterm_uds::atomic_file::write_with(&target, mode, produce)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, produce);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic performance publication is unavailable",
        ))
    }
}

/// Publish a report to a sink the person named (`--json-out PATH`). A descriptor spelling
/// (`/dev/stdout`, `/dev/fd/N`) is appended to where it is, whatever it reaches. A FIFO or a character
/// device the path reaches with its final link followed (`/dev/stdout`, `/dev/fd/N`, a process
/// substitution, a FIFO they made) is written where it is, never unlinked or replaced; a FIFO
/// outside `/dev` that no reader holds open is refused at once rather than waited on after the
/// whole measurement. Anything else is [`publish`]ed: written beside the file the path reaches
/// and renamed over it, so an interrupted write never loses the report already there.
///
/// # Errors
///
/// The path's stat other than absence, a FIFO with no reader (`BrokenPipe`), the open or write
/// of a descriptor or a stream, and every error [`publish`] has.
#[cfg(unix)]
pub(super) fn publish_named(path: impl AsRef<Path>, text: impl AsRef<str>) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};
    let (path, text) = (path.as_ref(), text.as_ref());
    // A descriptor spelling names a description this process already holds, whatever it reaches
    // (a terminal, a pipe, or the file the shell redirected it to): it is written where it is,
    // appended after what the descriptor already holds, with no flag set on it and nothing
    // created beside it (`/dev/fd` holds no other file).
    if descriptor_spelling(path) {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(path)?;
        return file.write_all(text.as_bytes());
    }
    let stream = match std::fs::metadata(path) {
        Ok(meta) => {
            let kind = meta.file_type();
            (kind.is_fifo() || kind.is_char_device()).then_some(kind.is_fifo())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let Some(fifo) = stream else {
        return publish(path, text);
    };
    // A FIFO this process holds no descriptor of is opened without waiting for a reader.
    let probe = fifo;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_CLOEXEC | if probe { libc::O_NONBLOCK } else { 0 })
        .open(path)
        .map_err(|error| {
            if probe && error.raw_os_error() == Some(libc::ENXIO) {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    format!("no reader holds {} open", path.display()),
                )
            } else {
                error
            }
        })?;
    // Recheck what was opened: a path that became a regular file (or a link to one) since the
    // stat is published whole, never written over in place.
    let opened = file.metadata()?.file_type();
    if !(opened.is_fifo() || opened.is_char_device()) {
        drop(file);
        return publish(path, text);
    }
    if probe {
        let fd = file.as_raw_fd();
        // SAFETY: F_GETFL and F_SETFL read and set the status flags of `fd`, which `file` owns
        // and keeps open across both calls; neither touches memory.
        let cleared = unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            flags >= 0 && libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) == 0
        };
        if !cleared {
            return Err(io::Error::last_os_error());
        }
    }
    (&file).write_all(text.as_bytes())
}

/// Whether `path` is spelled as one of this process's own descriptors: `/dev/stdin`,
/// `/dev/stdout`, `/dev/stderr` or `/dev/fd/<n>`, compared as written (never resolved).
#[cfg(unix)]
fn descriptor_spelling(path: &Path) -> bool {
    match path.to_str() {
        Some("/dev/stdin" | "/dev/stdout" | "/dev/stderr") => true,
        Some(text) => text
            .strip_prefix("/dev/fd/")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())),
        None => false,
    }
}

#[cfg(test)]
pub(super) fn interrupted(path: &Path, _: &str) -> io::Result<()> {
    publish_with(path, |writer| {
        writer.write_all(b"partial")?;
        Err(io::Error::other("injected producer failure"))
    })
}

#[cfg(all(test, any(unix, windows)))]
#[path = "publication_tests.rs"]
mod tests;
