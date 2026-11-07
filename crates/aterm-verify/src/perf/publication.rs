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
mod tests {
    use super::*;

    /// `--json-out /dev/stdout > report.json`: a descriptor spelling that reaches a regular file
    /// (the shell's redirect) is appended to through that descriptor, after what it already
    /// holds. NEGATIVE CONTROL: published as a file, the write was refused on macOS (its temp file
    /// had no place in `/dev/fd`), and on Linux it replaced the file and lost what was there.
    #[cfg(unix)]
    #[test]
    fn a_descriptor_spelling_reaching_a_file_is_appended_to_through_it() {
        use std::os::fd::AsRawFd as _;
        let dir = aterm_tempfile::tempdir().unwrap();
        let report = dir.path().join("report.json");
        let mut held = std::fs::File::create(&report).unwrap();
        held.write_all(b"head\n").unwrap();
        let spelled = format!("/dev/fd/{}", held.as_raw_fd());
        publish_named(Path::new(&spelled), "{}\n").unwrap();
        assert_eq!(std::fs::read_to_string(&report).unwrap(), "head\n{}\n");
    }

    /// A person-named sink is written where it points. A FIFO stays a FIFO and its reader gets
    /// the bytes; a FIFO with no reader is refused at once rather than waited on; a link stays a
    /// link and the file it reaches is replaced whole, keeping its mode. NEGATIVE CONTROL: with
    /// `publish_named` falling to `publish` for a FIFO, the FIFO was refused as no regular file;
    /// with a plain blocking open, the readerless FIFO's write never returned.
    #[cfg(unix)]
    #[test]
    fn a_named_sink_is_written_through_a_fifo_and_a_link() {
        use std::io::Read as _;
        use std::os::unix::fs::{
            FileTypeExt as _, OpenOptionsExt as _, PermissionsExt as _, symlink,
        };
        let dir = aterm_tempfile::tempdir().unwrap();
        let fifo = dir.path().join("fifo");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `mkfifo` reads the NUL-terminated path `c` owns, alive for the call.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "the FIFO");
        let refused = publish_named(&fifo, "{}\n").unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::BrokenPipe, "{refused}");
        // The read end opened first, without waiting for a writer, is the FIFO's reader.
        let mut reader = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&fifo)
            .unwrap();
        publish_named(&fifo, "{\"lane\":1}\n").unwrap();
        assert!(
            std::fs::symlink_metadata(&fifo)
                .unwrap()
                .file_type()
                .is_fifo()
        );
        // The writer has closed, so the read ends at the bytes it wrote.
        let mut got = String::new();
        reader.read_to_string(&mut got).unwrap();
        assert_eq!(got, "{\"lane\":1}\n");

        let target = dir.path().join("reports.json");
        std::fs::write(&target, "an older, longer report\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        let link = dir.path().join("link.json");
        symlink(&target, &link).unwrap();
        publish_named(&link, "{}\n").unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{}\n");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );

        for spelled in ["/dev/stdout", "/dev/stderr", "/dev/fd/1", "/dev/fd/12"] {
            assert!(descriptor_spelling(Path::new(spelled)), "{spelled}");
        }
        for other in [
            "/dev/../tmp/report.fifo",
            "/dev/fd/",
            "/dev/fd/1x",
            "/dev/null",
            "fifo",
        ] {
            assert!(!descriptor_spelling(Path::new(other)), "{other}");
        }
    }

    #[test]
    fn publication_retains_existing_paths_and_modes() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let target = dir.path().join("report");
        publish(&target, "first").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o644
            );
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        publish_with(&target, |writer| writer.write_all(b"replacement")).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "replacement");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o640
            );
            let link = dir.path().join("linked");
            std::os::unix::fs::symlink("report", &link).unwrap();
            publish(&link, "linked update").unwrap();
            assert!(
                std::fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "linked update");
            assert!(interrupted(&link, "unused").is_err());
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "linked update");
        }
        let missing = dir.path().join("absent").join("report");
        assert_eq!(
            publish(&missing, "no").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(!missing.parent().unwrap().exists());
    }
}
