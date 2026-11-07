// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::Read as _;

#[test]
fn host_lifeline_is_live_empty_before_sharing_and_eof_after_sole_writer_death() {
    let (mut reader, writer) = host_pipe().unwrap();
    // SAFETY: inspect this actual owned reader without changing its flags.
    let original = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
    assert_eq!(original & libc::O_ACCMODE, libc::O_RDONLY);
    assert_ne!(original & libc::O_NONBLOCK, 0);
    let duplicate = aterm_uds::kept_out(|| reader.as_fd().try_clone_to_owned()).unwrap();
    let mut duplicate = std::fs::File::from(duplicate);
    assert_eq!(
        reader.read(&mut [0]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(
        duplicate.read(&mut [0]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(writer);
    assert_eq!(reader.read(&mut [0]).unwrap(), 0);
    assert_eq!(duplicate.read(&mut [0]).unwrap(), 0);
    // SAFETY: the same owned reader remains open; no descriptor was replaced.
    assert_eq!(
        unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) },
        original
    );
}
