// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _, PermissionsExt as _, symlink};
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
