// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::{
    fs::OpenOptions,
    io::{Read as _, Write as _},
    os::unix::fs::OpenOptionsExt as _,
};
#[test]
fn actual_fifo_escrow_observation_never_consumes_a_credit_or_changes_shared_flags() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("original-gnu");
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    let mut reader = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&path)
        .unwrap();
    let mut writer = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&path)
        .unwrap();
    let read = reader.as_fd().try_clone_to_owned().unwrap();
    let write = writer.as_fd().try_clone_to_owned().unwrap();
    let backing = fifo(&read, libc::O_RDONLY).unwrap();
    assert_eq!(fifo(&write, libc::O_WRONLY).unwrap(), backing);
    assert!(fifo(&read, libc::O_WRONLY).is_err());
    let flags = unsafe { libc::fcntl(read.as_raw_fd(), libc::F_GETFL) };
    let original = OriginalGnu {
        descriptions: Arc::new([read, write]),
        backing,
    };
    no_supplemental(&original).unwrap();
    writer.write_all(b"+").unwrap();
    assert!(
        no_supplemental(&original).is_err(),
        "a still-available original credit must refuse Enter"
    );
    let mut byte = [0];
    reader.read_exact(&mut byte).unwrap();
    assert_eq!(
        byte, *b"+",
        "observation must not consume or replace an actual original token"
    );
    no_supplemental(&original).unwrap();
    let after = unsafe { libc::fcntl(original.descriptions[0].as_raw_fd(), libc::F_GETFL) };
    #[cfg(target_os = "macos")]
    let kernel_markers = 0x1000 | 0x2000; // XNU FMARK/FDEFER are not semantic OFD flags.
    #[cfg(not(target_os = "macos"))]
    let kernel_markers = 0;
    assert_eq!(flags & !kernel_markers, after & !kernel_markers);
}
