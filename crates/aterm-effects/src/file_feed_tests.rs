// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn fixture_dir(label: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("at-feed-{label}-"))
        .suffix(".noindex")
        .tempdir()
        .expect("create visual-feed fixture directory")
}

#[test]
fn regular_file_round_trips_and_fingerprints_content() {
    let dir = fixture_dir("regular");
    let path = dir.path().join("feed.toml");
    std::fs::write(&path, "snowcat 😺\n").expect("write regular feed");
    let admitted = read_bounded_regular_utf8(&path, 64).expect("regular feed reads");
    assert_eq!(admitted, "snowcat 😺\n");
    let before = fingerprint_bounded_regular_utf8(&path, 64).expect("fingerprint feed");
    assert_eq!(
        before,
        fingerprint_admitted_utf8(&admitted),
        "same-handle admitted bytes and streaming path fingerprint agree"
    );
    assert_eq!(
        before,
        fingerprint_bounded_regular_utf8(&path, 64).expect("fingerprint is stable")
    );
    std::fs::write(&path, "snowcat 🐱\n").expect("edit regular feed");
    assert_ne!(
        before,
        fingerprint_bounded_regular_utf8(&path, 64).expect("fingerprint edited feed")
    );
}

#[test]
fn oversize_directory_and_invalid_utf8_are_rejected_by_read_and_hash() {
    let dir = fixture_dir("invalid");
    let root = dir.path();
    assert_eq!(
        read_bounded_regular_utf8(root, 8)
            .expect_err("directory is not a feed")
            .kind(),
        io::ErrorKind::InvalidInput
    );

    let oversized = root.join("oversized.toml");
    std::fs::write(&oversized, b"123456789").expect("write oversized feed");
    for error in [
        read_bounded_regular_utf8(&oversized, 8).expect_err("oversized read rejected"),
        fingerprint_bounded_regular_utf8(&oversized, 8)
            .expect_err("oversized fingerprint rejected"),
    ] {
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    let invalid_utf8 = root.join("invalid-utf8.toml");
    std::fs::write(&invalid_utf8, [0xf0, 0x9f, 0x98]).expect("write truncated UTF-8");
    assert_eq!(
        read_bounded_regular_utf8(&invalid_utf8, 8)
            .expect_err("invalid UTF-8 read rejected")
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        fingerprint_bounded_regular_utf8(&invalid_utf8, 8)
            .expect_err("invalid UTF-8 fingerprint rejected")
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[cfg(unix)]
#[test]
fn writerless_fifo_and_final_symlink_are_rejected_at_open() {
    use std::os::unix::ffi::OsStrExt as _;

    let dir = fixture_dir("special");
    let root = dir.path();
    let fifo = root.join("feed.fifo");
    let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path");
    // SAFETY: `fifo_c` is a live NUL-terminated pathname and mkfifo retains
    // no pointer. The private test directory makes the name unique.
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    let fifo_error = read_bounded_regular_utf8(&fifo, 64)
        .expect_err("writerless FIFO must return without blocking");
    assert_eq!(fifo_error.kind(), io::ErrorKind::InvalidInput);

    let regular = root.join("regular.toml");
    let linked = root.join("linked.toml");
    std::fs::write(&regular, "ok\n").expect("write symlink target");
    std::os::unix::fs::symlink(&regular, &linked).expect("create final symlink");
    assert!(
        read_bounded_regular_utf8(&linked, 64).is_err(),
        "final-component links are not admitted"
    );
}
