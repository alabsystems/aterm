// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Controlling pseudo-terminal helper for `provision_cli`.
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Stdio};

/// Master and slave descriptors of a pseudo-terminal pair.
pub struct Pty {
    master: std::fs::File,
    slave: OwnedFd,
}

impl Pty {
    /// Open a pty pair, the master non-blocking.
    pub fn open() -> Self {
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: two fresh descriptors this function alone owns.
        let (master, slave) = unsafe {
            (
                std::fs::File::from_raw_fd(master),
                OwnedFd::from_raw_fd(slave),
            )
        };
        aterm_pty::set_nonblocking(master.as_raw_fd(), true).expect("a non-blocking master");
        Self { master, slave }
    }

    /// Spawn `cmd` with a null stdin and this pty as its controlling terminal.
    pub fn spawn(&self, mut cmd: Command) -> Child {
        use aterm_uds::spawnfd::InheritNothing as _;
        let slave = self.slave.as_raw_fd();
        cmd.inherit_nothing().stdin(Stdio::null());
        // SAFETY: everything in the hook is async-signal-safe (`setsid`, `ioctl`, `dup`) and
        // touches no state shared with the parent. `slave` is open in the child from the
        // fork until its exec closes it.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setsid() < 0
                    || libc::ioctl(slave, libc::c_ulong::from(libc::TIOCSCTTY), 0) < 0
                    || libc::dup(slave) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd.spawn().expect("the atpkg-keys binary runs")
    }

    /// Read whatever the child has written to the terminal so far, appending to `into`.
    pub fn drain(&mut self, into: &mut String) {
        let mut buf = [0u8; 4096];
        loop {
            match self.master.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => into.push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(_) => return,
            }
        }
    }

    /// Type a line at the terminal, the way a human would.
    pub fn type_line(&mut self, line: &str) {
        self.master
            .write_all(line.as_bytes())
            .and_then(|()| self.master.write_all(b"\n"))
            .and_then(|()| self.master.flush())
            .expect("typing at the terminal");
    }
}
