// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE A/B BOOK ACROSS A SEAMLESS UPDATE (`docs/DESIGN-harness-autopilot-
//! 2026-10-15.md` §10.5.4): does the outgoing process write its book at its
//! exit seam? It does not. Its Commit is `seamless::commit_and_exit`, whose
//! success is uninhabited — one write and `_exit(0)` — and it runs inside
//! the event loop, so `run_app` never returns and the exit seam after it
//! (`App::write_harness_ab_at_exit`) never runs. The outgoing book holds what
//! its last page reading wrote; the successor, a new start under the same
//! pid, names a file of its own and reads that one as another's.

use std::ffi::CString;
use std::os::fd::{AsRawFd as _, OwnedFd};

use super::make_cloexec_pipe;

/// What a forked outgoing process did: its exit status, whether it reached
/// the exit seam (its stand-in wrote `marker`), and the bytes its Commit put
/// on the pipe.
struct Outgoing {
    status: i32,
    reached_the_seam: bool,
    committed: Vec<u8>,
}

/// Fork a child that Commits over `commit` and then does what the exit seam
/// does after a Commit that returns: writes its book, here a marker file
/// (`open` and `close`, async-signal-safe: libtest is multi-threaded at the
/// fork). `read` is the pipe's other end, `None` where it is already closed.
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
    // SAFETY: fork from a multi-threaded harness; the child makes only
    // async-signal-safe calls (`write`, `open`, `close`, `_exit`) — see above.
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

/// A SEAMLESS UPDATE'S OUTGOING PROCESS NEVER REACHES ITS EXIT SEAM: its
/// Commit lands (the 40-byte wire on the pipe) and the process is gone
/// (`_exit(0)`), with nothing after the Commit run — so the A/B book's exit
/// write does not happen there, and what it measured since its last page
/// reading is lost, as in a crash (§10.5.4). NEGATIVE CONTROL: a Commit that
/// cannot be written (the successor's end closed: EPIPE) returns, and the
/// same process goes on to the seam and writes.
#[test]
fn a_seamless_updates_outgoing_process_never_reaches_its_exit_seam() {
    let scratch = aterm_tempfile::tempdir().expect("scratch folder");
    let marker = scratch.path().join("book-written-at-exit");
    let (read, write) = make_cloexec_pipe().expect("commit pipe");
    let committed = outgoing(write, Some(read), &marker);
    assert_eq!(committed.status, 0, "the Commit's own `_exit(0)`");
    assert_eq!(committed.committed.len(), crate::seamless::READY_WIRE_LEN);
    assert!(
        !committed.reached_the_seam,
        "nothing runs past a landed Commit, the exit seam's book write included"
    );

    let (read, write) = make_cloexec_pipe().expect("commit pipe");
    drop(read);
    let refused = outgoing(write, None, &marker);
    assert_eq!(refused.status, 3, "the Commit returned: the seam ran");
    assert!(
        refused.reached_the_seam,
        "a returned Commit reaches the seam"
    );
}
