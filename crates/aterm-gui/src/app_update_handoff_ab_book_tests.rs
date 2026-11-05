// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that a seamless update's outgoing process exits inside `commit_and_exit` before reaching the A/B book exit seam.

use std::ffi::CString;
use std::os::fd::{AsRawFd as _, OwnedFd};

/// The Commit pipe, both ends kept across the test's own `fork`: the forked child is the
/// outgoing process and writes the Commit itself. `make_cloexec_pipe` keeps its ends out of
/// children (`aterm_uds::kept_out`, `FD_CLOFORK`), so the fork would close them in the child and
/// its drop of the read end would close a descriptor already gone.
fn forked_commit_pipe() -> (OwnedFd, OwnedFd) {
    let (read, write) = std::io::pipe().expect("a commit pipe");
    (OwnedFd::from(read), OwnedFd::from(write))
}

struct Outgoing {
    status: i32,
    reached_the_seam: bool,
    committed: Vec<u8>,
}

fn outgoing(commit: OwnedFd, read: Option<OwnedFd>, marker: &std::path::Path) -> Outgoing {
    let proof = crate::seamless::adoption_proof(
        "ab-book-exit-seam",
        2,
        "abcdef0",
        &[0x11; 32],
        &[0x22; 32],
        &[],
    )
    .expect("bounded proof fixture");
    let path = CString::new(marker.as_os_str().as_encoded_bytes()).expect("a path");
    // SAFETY: fork from the process `test_alone::alone` runs this test in, where no other test's
    // thread can hold a lock across the fork. The child's own calls are `write`, `open`, `close`
    // and `_exit`, plus `commit_and_exit`'s exit path (`clean_exit_now`: the log flush and the
    // crash marker's release), which takes only locks of this process that nothing else holds.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed");
    if pid == 0 {
        drop(read);
        // SAFETY: async-signal-safe; a refused write is EPIPE, never a kill.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
        match crate::seamless::commit_and_exit(&commit, proof) {
            Ok(never) => match never {},
            Err(_) => {
                // SAFETY: async-signal-safe; a path built before the fork.
                unsafe {
                    let fd = libc::open(
                        path.as_ptr(),
                        libc::O_CREAT | libc::O_WRONLY | libc::O_CLOEXEC,
                        0o600,
                    );
                    if fd >= 0 {
                        libc::close(fd);
                    }
                    libc::_exit(3)
                }
            }
        }
    }
    drop(commit);
    let mut status = 0_i32;
    loop {
        // SAFETY: blocking wait for one exact child pid into a local slot.
        let waited = unsafe { libc::waitpid(pid, &mut status, 0) };
        if waited == pid {
            break;
        }
        assert!(
            waited < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted,
            "waitpid refused to answer for the forked child"
        );
    }
    let mut committed = Vec::new();
    if let Some(read) = read {
        let mut wire = [0_u8; crate::seamless::READY_WIRE_LEN];
        // SAFETY: a bounded read into a local buffer from the pipe's read end,
        // whose only writer has exited.
        let got = unsafe { libc::read(read.as_raw_fd(), wire.as_mut_ptr().cast(), wire.len()) };
        committed.extend_from_slice(&wire[..usize::try_from(got).unwrap_or(0)]);
    }
    assert!(libc::WIFEXITED(status), "the child exited, not signalled");
    Outgoing {
        status: libc::WEXITSTATUS(status),
        reached_the_seam: marker.exists(),
        committed,
    }
}

#[test]
fn a_seamless_updates_outgoing_process_never_reaches_its_exit_seam() {
    // The fork runs alone in its own process: beside the lib suite's other threads, a lock one of
    // them held at the fork could leave the child signalled (seen once under a full verify).
    if !crate::test_alone::alone() {
        return;
    }
    let scratch = aterm_tempfile::tempdir().expect("scratch folder");
    let marker = scratch.path().join("book-written-at-exit");
    let (read, write) = forked_commit_pipe();
    let committed = outgoing(write, Some(read), &marker);
    assert_eq!(committed.status, 0, "the Commit's own `_exit(0)`");
    assert_eq!(committed.committed.len(), crate::seamless::READY_WIRE_LEN);
    assert!(
        !committed.reached_the_seam,
        "nothing runs past a landed Commit, the exit seam's book write included"
    );

    let (read, write) = forked_commit_pipe();
    drop(read);
    let refused = outgoing(write, None, &marker);
    assert_eq!(refused.status, 3, "the Commit returned: the seam ran");
    assert!(
        refused.reached_the_seam,
        "a returned Commit reaches the seam"
    );
}
