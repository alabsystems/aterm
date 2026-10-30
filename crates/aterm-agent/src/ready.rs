// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Launcher half of `aterm --headless --ready-fd <n>`.

use std::fs::File;
use std::io::{self, Read as _};
use std::os::fd::{AsRawFd as _, RawFd};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// The instance's flag.
pub const FLAG: &str = "--ready-fd";

/// What [`Ready::wait`] read.
#[derive(Debug)]
pub enum Signal {
    /// The byte: the instance's control socket listens.
    Ready,
    /// End-of-file with nothing written: the instance exited, or it has no socket to offer.
    Closed,
    /// Neither within the wait's bound: a hang.
    TimedOut,
    /// The read end could not be read (`poll`/`read` failed).
    Failed(io::Error),
}

/// One instance's ready signal.
#[derive(Debug)]
pub struct Ready {
    read: Option<File>,
    write: Option<File>,
    number: RawFd,
    /// The reading thread's one answer: the bytes its read returned (1 or 0).
    answer: Option<mpsc::Receiver<io::Result<usize>>>,
    /// The signal was read ([`Signal::Ready`] or [`Signal::Closed`]).
    done: bool,
}

impl Ready {
    /// A fresh signal in `dir` — a directory only this process writes (its scratch root).
    /// # Errors
    /// `mkfifo(3)` or either `open(2)` failed; nothing is left behind.
    pub fn open(dir: &Path) -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt as _;
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let path = dir.join(format!(
            ".aterm-ready-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL in the FIFO path"))?;
        // SAFETY: `c_path` is a NUL-terminated string that outlives the call, which only reads it.
        if unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(&path)
            .and_then(|read| {
                use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
                let rmeta = read.metadata()?;
                if !rmeta.file_type().is_fifo() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "ready path is not a FIFO",
                    ));
                }
                let write = std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&path)?;
                let wmeta = write.metadata()?;
                if wmeta.dev() != rmeta.dev() || wmeta.ino() != rmeta.ino() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "ready FIFO changed between opens",
                    ));
                }
                Ok((read, write))
            });
        let unlinked = std::fs::remove_file(&path);
        let (read, write) = opened?;
        unlinked?;
        // SAFETY: `F_GETFL`/`F_SETFL` read and write one descriptor's status flags; `read` is open
        // and owned here.
        let blocking = unsafe {
            let flags = libc::fcntl(read.as_raw_fd(), libc::F_GETFL);
            flags >= 0
                && libc::fcntl(read.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK) >= 0
        };
        if !blocking {
            return Err(io::Error::last_os_error());
        }
        let number = write.as_raw_fd();
        Ok(Self {
            read: Some(read),
            write: Some(write),
            number,
            answer: None,
            done: false,
        })
    }

    /// The flag and the number the instance is to be told.
    #[must_use]
    pub fn args(&self) -> [String; 2] {
        [FLAG.to_string(), self.number.to_string()]
    }

    /// The write end's number, which the child finds it at.
    #[must_use]
    pub const fn number(&self) -> RawFd {
        self.number
    }

    /// Hand the write end to `cmd`'s child at its own number and add [`FLAG`] to its arguments.
    pub fn hand(&self, cmd: &mut aterm_uds::spawnfd::StdCommand) {
        use std::os::unix::process::CommandExt as _;
        let fd = self.number;
        cmd.args(self.args());
        // SAFETY: the hook runs in the forked child before its exec and makes only
        // async-signal-safe calls (`fcntl`) on one integer copied into it; `last_os_error` reads
        // `errno` and allocates nothing.
        unsafe {
            cmd.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    /// Let go of this process's write end.
    pub fn spawned(&mut self) {
        self.write = None;
    }

    /// Block until the instance signals or closes its end, for at most `limit`.
    pub fn wait(&mut self, limit: Duration) -> Signal {
        self.spawned();
        if self.done {
            return Signal::Closed;
        }
        if self.answer.is_none() {
            let Some(mut read) = self.read.take() else {
                return Signal::Closed;
            };
            let (tx, rx) = mpsc::sync_channel(1);
            let started = std::thread::Builder::new()
                .name("aterm-ready-wait".into())
                .spawn(move || {
                    let mut byte = [0u8; 1];
                    let got = loop {
                        match read.read(&mut byte) {
                            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                            other => break other,
                        }
                    };
                    let _ = tx.send(got);
                });
            if let Err(e) = started {
                return Signal::Failed(e);
            }
            self.answer = Some(rx);
        }
        let Some(answer) = &self.answer else {
            return Signal::Closed;
        };
        match answer.recv_timeout(limit) {
            Ok(Ok(n)) => {
                self.done = true;
                if n == 0 {
                    Signal::Closed
                } else {
                    Signal::Ready
                }
            }
            Ok(Err(e)) => {
                self.done = true;
                Signal::Failed(e)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Signal::TimedOut,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.done = true;
                Signal::Failed(io::Error::other("the ready signal's reader ended"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Ready, Signal};
    use std::io::Write as _;
    use std::time::Duration;

    #[test]
    fn the_byte_is_ready_a_bare_close_is_closed_and_a_held_end_is_a_hang() {
        let dir = crate::supervise::test_scratch_path("ready", "signal");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the test's dir");

        let mut ready = Ready::open(&dir).expect("a ready signal");
        let mut child = ready
            .write
            .as_ref()
            .expect("held")
            .try_clone()
            .expect("dup");
        child.write_all(b"R").expect("the byte");
        drop(child);
        assert!(matches!(ready.wait(Duration::from_secs(60)), Signal::Ready));

        let mut closed = Ready::open(&dir).expect("a ready signal");
        let child = closed
            .write
            .as_ref()
            .expect("held")
            .try_clone()
            .expect("dup");
        drop(child);
        assert!(matches!(
            closed.wait(Duration::from_secs(60)),
            Signal::Closed
        ));

        let mut held = Ready::open(&dir).expect("a ready signal");
        let _child = held.write.as_ref().expect("held").try_clone().expect("dup");
        assert!(matches!(
            held.wait(Duration::from_millis(50)),
            Signal::TimedOut
        ));
        assert_eq!(held.args()[0], super::FLAG);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
