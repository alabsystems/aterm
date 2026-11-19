// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Non-blocking child output collection bounded by child exit rather than pipe EOF, for as long
//! as what this test binary started makes progress ([`crate::patience`]).

use std::fs::File;
use std::io::{ErrorKind, Read};
use std::os::fd::{AsRawFd, OwnedFd};
use std::process::{Child, Output};
use std::time::Duration;

use crate::patience::{Patience, Spent};

/// Wait for `child` to exit and collect its piped stdout and stderr. A hang detector, not a
/// latency budget: it waits for as long as what this test binary started (`child`, the instance
/// it talks to) keeps working, and gives up once a reading of their tree shows no CPU or I/O
/// progress for one whole `window`, or once the guard passed in all ([`Patience`]).
///
/// # Errors
/// Why the patience was spent, with `child` killed and reaped.
#[allow(dead_code)]
pub fn wait_and_read(mut child: Child, window: Duration) -> Result<Output, Spent> {
    let mut patience = Patience::new(window);
    let mut pipes = [
        child.stdout.take().map(|p| nonblocking(OwnedFd::from(p))),
        child.stderr.take().map(|p| nonblocking(OwnedFd::from(p))),
    ];
    let mut read = [Vec::new(), Vec::new()];
    loop {
        let status = child.try_wait().expect("poll the child");
        for (pipe, buf) in pipes.iter_mut().zip(read.iter_mut()) {
            read_available(pipe, buf);
        }
        if let Some(status) = status {
            let [stdout, stderr] = read;
            return Ok(Output {
                status,
                stdout,
                stderr,
            });
        }
        if let Some(why) = patience.spent() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(why);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The maximum raw bytes retained from either stream before an acknowledgement.
pub const ACK_OUTPUT_LIMIT: usize = 64 * 1024;

/// Wait until `child`'s piped stderr emits a line accepted by `wanted`, for as long as
/// [`wait_and_read`] waits: until a reading of what this test binary started shows no CPU or
/// I/O progress for one whole `window`, or the guard passed ([`Patience`]).
///
/// # Errors
/// [`NoLine`]: `child` exited first, the patience was spent first (`child` then reaped, killed
/// first where it still ran), or its output before the line reached [`ACK_OUTPUT_LIMIT`].
#[allow(dead_code)]
pub fn await_stderr_line(
    child: &mut Child,
    wanted: impl Fn(&str) -> bool,
    window: Duration,
) -> Result<(), NoLine> {
    let mut patience = Patience::new(window);
    let mut stderr = child.stderr.take().map(|p| nonblocking(OwnedFd::from(p)));
    let mut said = Vec::new();
    let mut line_from = 0;
    loop {
        let status = child.try_wait().expect("poll the child");
        let mut byte = [0u8; 1];
        while let Some(file) = stderr.as_mut() {
            if let Some(why) = patience.spent() {
                if status.is_none() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                return Err(NoLine::Hung {
                    why,
                    stderr: String::from_utf8_lossy(&said).into_owned(),
                });
            }
            if said.len() == ACK_OUTPUT_LIMIT {
                if status.is_none() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                return Err(NoLine::OutputLimit {
                    stream: "stderr",
                    captured: String::from_utf8_lossy(&said).into_owned(),
                });
            }
            match file.read(&mut byte) {
                Ok(0) => stderr = None,
                Ok(_) => {
                    said.push(byte[0]);
                    if byte[0] == b'\n' {
                        let line = String::from_utf8_lossy(&said[line_from..said.len() - 1]);
                        line_from = said.len();
                        if wanted(line.trim_end_matches('\r')) {
                            child.stderr = stderr.map(|f| OwnedFd::from(f).into());
                            return Ok(());
                        }
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => panic!("read the child's stderr: {e}"),
            }
        }
        if let Some(status) = status {
            let mut stdout = child.stdout.take().map(|p| nonblocking(OwnedFd::from(p)));
            let mut out = Vec::new();
            let mut chunk = [0u8; 8192];
            while let Some(file) = stdout.as_mut() {
                if let Some(why) = patience.spent() {
                    return Err(NoLine::Hung {
                        why,
                        stderr: String::from_utf8_lossy(&said).into_owned(),
                    });
                }
                if out.len() == ACK_OUTPUT_LIMIT {
                    return Err(NoLine::OutputLimit {
                        stream: "stdout",
                        captured: String::from_utf8_lossy(&out).into_owned(),
                    });
                }
                let room = (ACK_OUTPUT_LIMIT - out.len()).min(chunk.len());
                match file.read(&mut chunk[..room]) {
                    Ok(0) => break,
                    Ok(n) => out.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(e) => panic!("read the child's stdout: {e}"),
                }
            }
            return Err(NoLine::Exited(Output {
                status,
                stdout: out,
                stderr: said,
            }));
        }
        if let Some(why) = patience.spent() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(NoLine::Hung {
                why,
                stderr: String::from_utf8_lossy(&said).into_owned(),
            });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Why [`await_stderr_line`] saw no line it wanted.
#[allow(dead_code)]
pub enum NoLine {
    /// The child exited first: reaped, with bounded output available now.
    Exited(Output),
    /// The patience was spent first, for `why`, with the child reaped (killed first where it
    /// still ran); `stderr` is what it had written.
    Hung { why: Spent, stderr: String },
    /// Pre-acknowledgement output reached [`ACK_OUTPUT_LIMIT`].
    OutputLimit {
        stream: &'static str,
        captured: String,
    },
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
