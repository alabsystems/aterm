// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deadline-bounded local socket connections.

use std::io::{self, ErrorKind};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Instant;

use crate::CtlStream;

#[cfg(target_vendor = "apple")]
const SO_NOSIGPIPE: libc::c_int = 0x1022;

/// Connect to a filesystem socket by `deadline`, returning a blocking stream.
/// The descriptor is excluded from spawned children before connecting. A full
/// Linux listen backlog refuses immediately with its original `EAGAIN` error.
///
/// # Errors
/// Invalid paths, an expired deadline, or the original socket, descriptor,
/// connection, polling, or peer-address error.
#[cfg_attr(trust_verify, trust::skip)]
pub fn until(path: &Path, deadline: Instant) -> io::Result<CtlStream> {
    check_deadline(deadline)?;
    // SAFETY: sockaddr_un is a plain C address; zero also terminates sun_path.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() || bytes.contains(&0) || bytes.len() >= address.sun_path.len() {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "invalid Unix socket path",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = libc::c_char::from_ne_bytes([*byte]);
    }
    let length =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    #[cfg(target_vendor = "apple")]
    {
        address.sun_len = length as u8;
    }
    // SAFETY: socket takes only integer arguments and returns an owned descriptor.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: this fresh descriptor has no other owner, including on error paths.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: F_GETFD reads flags from our live descriptor.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: F_SETFD changes only our descriptor's inheritance flags.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    crate::keep_out_of_children(&fd)?;
    #[cfg(target_vendor = "apple")]
    {
        // Adopting an OwnedFd bypasses std's Apple socket constructor.
        let enabled: libc::c_int = 1;
        // SAFETY: enabled is a live socket option value of the supplied size.
        if unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                SO_NOSIGPIPE,
                std::ptr::from_ref(&enabled).cast(),
                std::mem::size_of_val(&enabled) as libc::socklen_t,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    let stream = CtlStream::from(fd);
    stream.set_nonblocking(true)?;
    check_deadline(deadline)?;
    // SAFETY: address is initialized, and length covers its family and terminated path.
    let connected = unsafe {
        libc::connect(
            stream.as_raw_fd(),
            std::ptr::from_ref(&address).cast(),
            length,
        )
    };
    if connected < 0 {
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINPROGRESS | libc::EINTR) => wait_connected(&stream, deadline)?,
            // AF_UNIX EAGAIN (full backlog) has no pending connection to poll.
            _ => return Err(error),
        }
    }
    stream.peer_addr()?;
    check_deadline(deadline)?;
    stream.set_nonblocking(false)?;
    Ok(stream)
}

fn check_deadline(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        Err(io::Error::new(
            ErrorKind::TimedOut,
            "Unix socket connect deadline expired",
        ))
    } else {
        Ok(())
    }
}

fn wait_connected(stream: &CtlStream, deadline: Instant) -> io::Result<()> {
    loop {
        check_deadline(deadline)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        // Round up so sub-millisecond remaining time does not become a busy poll.
        let millis = remaining.as_millis().saturating_add(u128::from(
            !remaining.subsec_nanos().is_multiple_of(1_000_000),
        ));
        let millis = libc::c_int::try_from(millis).unwrap_or(libc::c_int::MAX);
        let mut poll = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: poll points to exactly one initialized descriptor record.
        let ready = unsafe { libc::poll(&raw mut poll, 1, millis) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            continue;
        }
        if let Some(error) = stream.take_error()? {
            return Err(error);
        }
        // Writability alone can describe an unconnected AF_UNIX socket.
        stream.peer_addr()?;
        return Ok(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::ffi::OsStringExt;
    use std::time::Duration;

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    #[test]
    fn connected_stream_has_peer_identity_and_is_blocking_and_cloexec() {
        let root = aterm_tempfile::tempdir().unwrap();
        let path = root.path().join("connect.sock");
        let listener = crate::CtlListener::bind(&path).unwrap();
        crate::keep_out_of_children(&listener).unwrap();
        let mut client = until(&path, deadline()).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        crate::keep_out_of_children(&server).unwrap();
        assert_eq!(crate::peer::peer_uid(&client), Some(crate::peer::our_uid()));
        // SAFETY: both fcntl calls read flags from a live test descriptor.
        let status = unsafe { libc::fcntl(client.as_raw_fd(), libc::F_GETFL) };
        let descriptor = unsafe { libc::fcntl(client.as_raw_fd(), libc::F_GETFD) };
        assert!(status >= 0 && status & libc::O_NONBLOCK == 0);
        assert!(descriptor >= 0 && descriptor & libc::FD_CLOEXEC != 0);
        #[cfg(target_vendor = "apple")]
        {
            let mut enabled: libc::c_int = 0;
            let mut length = std::mem::size_of_val(&enabled) as libc::socklen_t;
            // SAFETY: enabled and length are writable and describe the option buffer.
            assert_eq!(
                unsafe {
                    libc::getsockopt(
                        client.as_raw_fd(),
                        libc::SOL_SOCKET,
                        SO_NOSIGPIPE,
                        std::ptr::from_mut(&mut enabled).cast(),
                        &raw mut length,
                    )
                },
                0
            );
            assert_eq!(enabled, 1);
        }
        server
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        client.write_all(b"x").unwrap();
        let mut byte = [0];
        server.read_exact(&mut byte).unwrap();
        assert_eq!(byte, *b"x");
    }

    #[test]
    fn expired_missing_and_invalid_paths_refuse() {
        let root = aterm_tempfile::tempdir().unwrap();
        let missing = root.path().join("missing.sock");
        assert_eq!(
            until(&missing, Instant::now()).unwrap_err().kind(),
            ErrorKind::TimedOut
        );
        assert_eq!(
            until(&missing, deadline()).unwrap_err().raw_os_error(),
            Some(libc::ENOENT)
        );
        for path in [
            Path::new("").to_owned(),
            std::ffi::OsString::from_vec(b"bad\0path".to_vec()).into(),
            Path::new(&"x".repeat(256)).to_owned(),
        ] {
            assert_eq!(
                until(&path, deadline()).unwrap_err().kind(),
                ErrorKind::InvalidInput
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn full_linux_backlog_refuses_without_returning_an_unconnected_stream() {
        let root = aterm_tempfile::tempdir().unwrap();
        let path = root.path().join("backlog.sock");
        let listener = crate::CtlListener::bind(&path).unwrap();
        crate::keep_out_of_children(&listener).unwrap();
        // SAFETY: listen changes the queue size of this live listening socket.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
        let first = until(&path, deadline()).unwrap();
        let refused = until(&path, deadline()).unwrap_err();
        assert_eq!(refused.raw_os_error(), Some(libc::EAGAIN));
        assert!(first.peer_addr().is_ok());
    }
}
