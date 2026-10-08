// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A child's output, read until the child has EXITED and its pipes are empty —
//! never until end-of-file.
//!
//! A pipe reads end-of-file only once every copy of its write end is closed,
//! and a sibling thread's spawn can take a copy: on macOS std marks a new pipe
//! close-on-exec in a second step (there is no `pipe2`), and a child forked
//! between the two keeps the write end for its whole life. In the merge gate of
//! 2026-09-29 a 20 ms `aterm ctl who` in `fences_live_headless` read for 150 s
//! that way: the `await` spawned beside it held the copy until its own timeout.
//! Once the child has exited, everything it wrote is already in the pipe, so
//! this reads what is there and stops.

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::os::fd::{AsRawFd, OwnedFd};
use std::process::{Child, Output};
use std::time::{Duration, Instant};

/// Wait for `child` and collect what it wrote to its piped stdout and stderr
/// (an unpiped stream reads empty). `None` when it has not exited by
/// `deadline`: it is then killed and reaped, and the caller says what hung.
#[allow(dead_code)]
pub fn wait_and_read(mut child: Child, deadline: Instant) -> Option<Output> {
    let mut pipes = [
        child.stdout.take().map(|p| nonblocking(OwnedFd::from(p))),
        child.stderr.take().map(|p| nonblocking(OwnedFd::from(p))),
    ];
    let mut read = [Vec::new(), Vec::new()];
    loop {
        // Exit first, then the reads: what the child wrote before it exited is
        // in the pipe by the time its exit can be seen.
        let status = child.try_wait().expect("poll the child");
        for (pipe, buf) in pipes.iter_mut().zip(read.iter_mut()) {
            read_available(pipe, buf);
        }
        if let Some(status) = status {
            let [stdout, stderr] = read;
            return Some(Output {
                status,
                stdout,
                stderr,
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn nonblocking(fd: OwnedFd) -> File {
    let raw = fd.as_raw_fd();
    // SAFETY: `raw` is the open descriptor `fd` owns for this whole call.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    // SAFETY: as above; only O_NONBLOCK is added to the flags just read.
    let set = unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    assert!(
        flags >= 0 && set >= 0,
        "make the child's pipe non-blocking: {}",
        std::io::Error::last_os_error()
    );
    File::from(fd)
}

/// Read what `pipe` holds now; at end-of-file it is dropped.
fn read_available(pipe: &mut Option<File>, buf: &mut Vec<u8>) {
    let Some(file) = pipe else { return };
    let mut chunk = [0u8; 8192];
    loop {
        match file.read(&mut chunk) {
            Ok(0) => {
                *pipe = None;
                return;
            }
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => return,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => panic!("read the child's output: {e}"),
        }
    }
}
