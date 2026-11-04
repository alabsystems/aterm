// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Authenticated local attachment of one privately delegated read capability.
//! The descriptor is consumed on every result. A final acknowledgement means
//! queued only; a transport failure after delivery may still have queued it.

use aterm_types::allocation_monitor as wire;
use aterm_uds::{
    CtlStream,
    ready::{self, Interest},
};
use std::{
    io::{self, Read as _, Write as _},
    os::fd::{AsFd as _, OwnedFd},
    path::Path,
    time::Instant,
};

const LINE_LIMIT: usize = 4096;

fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "allocation monitor attachment refused",
    )
}
fn malformed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid allocation monitor acknowledgement",
    )
}
fn time(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(())
}

/// Attach a privately delegated monitor descriptor to one explicitly pinned
/// local control socket. The endpoint uses the existing control owner token and
/// AUTH/version handshake; tokenless legacy fallback and redial are refused.
/// All socket I/O shares the caller's absolute deadline. No descriptor is sent
/// before explicit attachment admission. The return value means queued only.
///
/// # Errors
/// Missing/invalid local target or owner token, peer/auth refusal, expiry, I/O,
/// invalid admission, descriptor delivery, or malformed/zero queued revision.
pub fn attach(
    path: &str,
    client: OwnedFd,
    generation: [u8; 16],
    deadline: Instant,
) -> io::Result<u64> {
    time(deadline)?;
    if !Path::new(path).is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "allocation monitor requires an absolute local socket path",
        ));
    }
    // Pin the alias once. Neither authentication nor a retry can switch the
    // selected instance after the descriptor's destination was chosen.
    let pinned = crate::pin_alias(path);
    let mut stream = aterm_uds::connect::until(Path::new(&pinned), deadline)?;
    stream.set_nonblocking(true)?;
    if aterm_uds::peer::peer_uid(&stream) != Some(aterm_uds::peer::our_uid()) {
        return Err(refused());
    }
    let (_, token) = crate::read_token_file_at(&pinned).map_err(|_| refused())?;
    let token = aterm_uds::secret::Secret::from_vec(token.into_bytes());
    let mut hello = Vec::with_capacity(token.len() + 16);
    hello.extend_from_slice(b"AUTH ");
    hello.extend_from_slice(token.expose());
    hello.extend_from_slice(b"\nversion\n");
    let hello = aterm_uds::secret::Secret::from_vec(hello);
    write_all(&mut stream, hello.expose(), deadline)?;
    let accepted = read_line(&mut stream, deadline)?;
    if !accepted.starts_with("OK ") {
        return Err(refused());
    }
    exchange(&mut stream, client, generation, deadline)
}

fn exchange(
    stream: &mut CtlStream,
    client: OwnedFd,
    generation: [u8; 16],
    deadline: Instant,
) -> io::Result<u64> {
    write_all(stream, wire::request(generation).as_bytes(), deadline)?;
    let admission = read_line(stream, deadline)?;
    if admission != wire::ADMISSION {
        return Err(if admission.starts_with("ERR ") {
            refused()
        } else {
            malformed()
        });
    }
    loop {
        time(deadline)?;
        match aterm_uds::fdpass::send_with_fds(stream, &[wire::FD_MARKER], &[client.as_fd()]) {
            Ok(1) => break,
            Ok(_) => return Err(io::ErrorKind::WriteZero.into()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                ready::until(stream.as_fd(), Interest::Write, deadline)?
            }
            Err(error) => return Err(error),
        }
    }
    let queued = read_line(stream, deadline)?;
    if queued.starts_with("ERR ") {
        return Err(refused());
    }
    wire::parse_queued(&queued).ok_or_else(malformed)
}

fn write_all(stream: &mut CtlStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        time(deadline)?;
        match stream.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                ready::until(stream.as_fd(), Interest::Write, deadline)?
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn read_line(stream: &mut CtlStream, deadline: Instant) -> io::Result<String> {
    let mut line = Vec::new();
    loop {
        time(deadline)?;
        let mut part = [0; 256];
        match stream.read(&mut part) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                if let Some(end) = part[..n].iter().position(|&b| b == b'\n') {
                    if end + 1 != n {
                        return Err(malformed());
                    }
                    line.extend_from_slice(&part[..end]);
                    if line.len() > LINE_LIMIT {
                        return Err(malformed());
                    }
                    return String::from_utf8(line).map_err(|_| malformed());
                }
                line.extend_from_slice(&part[..n]);
                if line.len() > LINE_LIMIT {
                    return Err(malformed());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                ready::until(stream.as_fd(), Interest::Read, deadline)?
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
#[path = "allocation_monitor_tests.rs"]
mod tests;
