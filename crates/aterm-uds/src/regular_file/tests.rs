// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[cfg(any(unix, windows))]
use std::io::Read as _;

#[test]
#[cfg(target_os = "macos")]
fn native_clone_uses_the_held_inode_and_keeps_copies_independent() {
    use std::io::{Seek as _, SeekFrom};
    use std::os::{
        fd::AsRawFd as _,
        unix::fs::{FileExt as _, MetadataExt as _},
    };
    let dir = aterm_tempfile::Builder::new()
        .suffix(".noindex")
        .tempdir()
        .unwrap();
    let source_path = dir.path().join("source");
    let bytes = vec![0xa5; 8 * 1024 * 1024];
    std::fs::write(&source_path, &bytes).unwrap();
    let (mut source, original) = open(&source_path).unwrap();
    source.seek(SeekFrom::Start(17)).unwrap();
    let flags = unsafe { libc::fcntl(source.as_raw_fd(), libc::F_GETFL) };
    let descriptor_flags = unsafe { libc::fcntl(source.as_raw_fd(), libc::F_GETFD) };
    std::fs::rename(&source_path, dir.path().join("held-original")).unwrap();
    std::fs::write(&source_path, b"replacement must not be cloned").unwrap();
    let destination = dir.path().join("clone");
    assert!(
        clone_held(&source, &destination).unwrap(),
        "native APFS test requires actual clone support"
    );
    let cloned = std::fs::metadata(&destination).unwrap();
    assert_ne!(cloned.ino(), original.ino());
    assert_eq!(cloned.nlink(), 1);
    assert_eq!(std::fs::read(&destination).unwrap(), bytes);
    assert_eq!(source.stream_position().unwrap(), 17);
    assert_eq!(
        unsafe { libc::fcntl(source.as_raw_fd(), libc::F_GETFL) },
        flags
    );
    assert_eq!(
        unsafe { libc::fcntl(source.as_raw_fd(), libc::F_GETFD) },
        descriptor_flags
    );
    let writer = std::fs::OpenOptions::new()
        .write(true)
        .open(&destination)
        .unwrap();
    writer.write_all_at(b"changed", 0).unwrap();
    let mut original_prefix = [0; 7];
    source.read_exact_at(&mut original_prefix, 0).unwrap();
    assert_eq!(original_prefix, [0xa5; 7]);
    assert_eq!(
        std::fs::read(&source_path).unwrap(),
        b"replacement must not be cloned"
    );
    assert_eq!(
        clone_held(&source, &destination).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(&std::fs::read(destination).unwrap()[..7], b"changed");
    assert!(
        clone_held(
            &File::open(dir.path()).unwrap(),
            &dir.path().join("directory")
        )
        .is_err()
    );
}

#[cfg(any(unix, windows))]
#[test]
fn a_regular_reader_returns_binary_bytes_and_its_own_metadata() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("binary");
    let bytes = [0, 0xff, b'\n', 0x80, 1];
    std::fs::write(&path, bytes).unwrap();
    let (mut file, metadata) = open(&path).unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.len(), bytes.len() as u64);
    let mut read = Vec::new();
    file.read_to_end(&mut read).unwrap();
    assert_eq!(read, bytes);
}

#[cfg(any(unix, windows))]
#[test]
fn a_regular_reader_refuses_a_directory() {
    let dir = aterm_tempfile::tempdir().unwrap();
    assert_eq!(
        open(dir.path()).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[cfg(not(any(unix, windows)))]
#[test]
fn an_unsupported_target_refuses_before_examining_the_path() {
    assert_eq!(
        open(Path::new("not-a-real-file")).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
}

#[cfg(unix)]
#[test]
fn a_regular_reader_refuses_a_leaf_link_and_a_socket() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    std::fs::write(&file, b"target").unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert_eq!(open(&link).unwrap_err().kind(), io::ErrorKind::InvalidData);
    let socket = dir.path().join("socket");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    assert_eq!(
        open(&socket).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[cfg(unix)]
#[test]
fn a_link_replacing_the_checked_file_is_not_followed() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("checked");
    let original = dir.path().join("original");
    let target = dir.path().join("target");
    std::fs::write(&path, b"original").unwrap();
    std::fs::write(&target, b"must not read this target").unwrap();
    let mut checked = false;
    let result = open_after_check(&path, || {
        checked = true;
        std::fs::rename(&path, &original).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
    });
    assert!(checked);
    assert!(
        result.is_err(),
        "the precheck does not authorize the replaced name"
    );
    assert_eq!(std::fs::read(&original).unwrap(), b"original");
    assert_eq!(
        std::fs::read(&target).unwrap(),
        b"must not read this target"
    );
}

#[cfg(unix)]
#[test]
fn a_fifo_replacing_the_checked_file_cannot_block_the_reader() {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::time::Duration;

    const HANG: Duration = Duration::from_secs(60);
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("checked");
    std::fs::write(&path, b"original").unwrap();
    let reader_path = path.clone();
    let (ready, replaced) = std::sync::mpsc::channel();
    let (done, finished) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let outcome = open_after_check(&reader_path, || {
            std::fs::remove_file(&reader_path).unwrap();
            let fifo = std::ffi::CString::new(reader_path.as_os_str().as_bytes()).unwrap();
            // SAFETY: `fifo` is a NUL-terminated path in this test's private directory.
            assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
            ready.send(()).unwrap();
        });
        done.send(outcome.map(|_| ()).map_err(|error| error.kind()))
            .unwrap();
    });
    replaced
        .recv_timeout(HANG)
        .expect("the reader replaced its checked file");
    let outcome = match finished.recv_timeout(HANG) {
        Ok(outcome) => outcome,
        Err(timeout) => {
            // A read-write handle supplies a writer even if the reader was descheduled just
            // after its ready message. A write-only nonblocking open could return ENXIO then.
            let release = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&path)
                .expect("a release endpoint for the private FIFO");
            let _ = finished
                .recv_timeout(HANG)
                .expect("the released reader must finish before it is joined");
            reader.join().unwrap();
            drop(release);
            panic!("the reader waited for a FIFO writer: {timeout}");
        }
    };
    reader.join().unwrap();
    assert_eq!(outcome, Err(io::ErrorKind::InvalidData));
}

#[cfg(unix)]
#[test]
fn a_parent_link_does_not_change_the_leaf_policy() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("file"), b"through the parent").unwrap();
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let (mut file, _) = open(&alias.join("file")).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"through the parent");
}
