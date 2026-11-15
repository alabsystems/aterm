// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A private inherited socket holds the tether before the requested program's exec.
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::time::Duration;

pub(crate) const VAR: &str = "ATERM_TETHER_ENTRY_BARRIER";
const READY: u8 = b'R';
const RUN: u8 = b'E';
const WAIT: Duration = Duration::from_secs(60);

pub(super) struct Parent(crate::CtlStream);

impl Parent {
    pub(super) fn new() -> io::Result<(Self, OwnedFd)> {
        let (parent, child) = crate::kept_out(|| -> io::Result<_> {
            let (parent, child) = crate::CtlStream::pair()?;
            Ok((above_stdio(parent)?, above_stdio(child)?))
        })?;
        parent.set_read_timeout(Some(WAIT))?;
        parent.set_write_timeout(Some(WAIT))?;
        Ok((Self(parent), child.into()))
    }

    pub(super) fn ready(&mut self) -> io::Result<()> {
        expect(&mut self.0, READY)
    }

    pub(super) fn release(mut self) -> io::Result<()> {
        self.0.write_all(&[RUN])
    }
}

fn above_stdio(stream: crate::CtlStream) -> io::Result<crate::CtlStream> {
    if stream.as_raw_fd() >= 3 {
        return Ok(stream);
    }
    // SAFETY: duplicate this owned socket above stdio while kept_out excludes
    // concurrent forks. The returned descriptor is new and exclusively owned.
    let copied = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if copied < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful fcntl returned this fresh owned descriptor.
    let copied = unsafe { OwnedFd::from_raw_fd(copied) };
    drop(stream);
    Ok(copied.into())
}

fn expect(stream: &mut crate::CtlStream, expected: u8) -> io::Result<()> {
    let mut byte = [0];
    stream.read_exact(&mut byte)?;
    if byte[0] != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid private entry barrier message",
        ));
    }
    Ok(())
}

fn await_release(mut stream: crate::CtlStream) -> io::Result<()> {
    // A live owner may hold a prepared tool across a pause or handoff. Its
    // socket withdrawal (or process exit), rather than a timer, ends that hold.
    stream.set_write_timeout(Some(WAIT))?;
    stream.write_all(&[READY])?;
    expect(&mut stream, RUN)
}

/// Called only by the single-threaded tether entrypoint, before target exec.
/// The command supplies and exclusively transfers this inherited socket. The
/// handshake releases execution only; it grants no SDK, Source or root role.
pub(crate) fn before_exec() -> io::Result<()> {
    let Some(value) = std::env::var_os(VAR) else {
        return Ok(());
    };
    let fd = value
        .to_str()
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|fd| *fd >= 3)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid entry barrier"))?;
    // SAFETY: the tether runs before threads or other descriptor adopters. Its
    // command exclusively handed this socket to this entrypoint. Adoption
    // validates the socket, duplicates it close-on-exec and closes the old slot.
    let socket = unsafe { super::adopt_inherited_socket(fd) }?;
    let stream = crate::CtlStream::from(socket);
    await_release(stream)
}

#[cfg(test)]
#[path = "spawnfd_entry_barrier_tests.rs"]
mod tests;
