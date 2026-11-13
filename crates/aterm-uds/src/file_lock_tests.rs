// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn existing_only_lock_never_recreates_a_removed_slot() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("slot");
    assert_eq!(
        LockFile::take_existing(&path, Instant::now(), "busy")
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert!(!path.exists());
    std::fs::write(&path, b"retained").unwrap();
    let held = LockFile::take_existing(&path, Instant::now(), "busy").unwrap();
    assert_eq!(
        LockFile::take_existing(&path, Instant::now(), "busy")
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"retained");
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        LockFile::take_existing(&path, Instant::now(), "busy")
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert!(!path.exists());
    drop(held);
}

/// One holder at a time: a second take while the first holds it waits until its deadline and
/// says `busy`, and takes it once the first let go; a missing file is made 0600, one there is
/// taken whatever its mode, and a folder is no lock file.
#[test]
fn a_lock_file_has_one_holder() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("record.lock");
    let first = LockFile::take(&path, Instant::now(), "busy").unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let refused = LockFile::take(&path, Instant::now(), "busy").unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(refused.to_string(), "busy");
    drop(first);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
    drop(LockFile::take(&path, Instant::now(), "busy").unwrap());
    assert!(
        LockFile::take(dir.path(), Instant::now(), "busy").is_err(),
        "a folder is no lock file"
    );
}

/// A link at the path is never followed (`O_NOFOLLOW`): a take through a link to a regular
/// file there fails with `ELOOP`, and that file is never locked through it, so a take of the
/// file itself, while the take through the link is held, gets it at once.
#[test]
fn a_link_at_the_path_is_never_followed_to_the_file_it_names() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    std::fs::write(&target, b"").unwrap();
    let linked = dir.path().join("linked.lock");
    std::os::unix::fs::symlink(&target, &linked).unwrap();
    let through = LockFile::take(&linked, Instant::now(), "busy");
    let direct = LockFile::take(&target, Instant::now(), "busy");
    assert!(
        direct.is_ok(),
        "the target was locked through the link: {direct:?}"
    );
    assert_eq!(
        through.as_ref().map_err(io::Error::raw_os_error).err(),
        Some(Some(libc::ELOOP)),
        "{through:?}"
    );
}

/// A FIFO at the path is never waited on (`O_NONBLOCK`): the take returns at once, refused as
/// no regular file, where a read-only open without that flag waits for a writer that never
/// comes. The take is a one-minute hang detector away from the test.
#[test]
fn a_fifo_at_the_path_is_refused_at_once() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let fifo = dir.path().join("fifo.lock");
    let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `c` is a valid NUL-terminated path in the test's own scratch folder.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo");
    let (tx, rx) = std::sync::mpsc::channel();
    let at = fifo.clone();
    std::thread::spawn(move || {
        let _ = tx.send(LockFile::take(&at, Instant::now(), "busy").map(drop));
    });
    let taken = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the take of a FIFO returned (a hang detector)");
    assert_eq!(taken.map_err(|e| e.kind()), Err(io::ErrorKind::InvalidData));
}
