// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test helpers for hanging up control sockets, reading to EOF under a hang detector, and leaving
//! a socket file nothing listens on ([`never_listening_socket`], [`leave_no_listener`]).

use std::thread::JoinHandle;

use aterm_uds::CtlStream;

/// Hangs up `client` and closes its descriptor.
pub(crate) fn hang_up(client: CtlStream) {
    aterm_uds::hangup::hang_up(&client);
}

/// Joins `join` under [`aterm_spec::wait::MUST_HAPPEN`] after a client hang-up.
#[track_caller]
pub(crate) fn join_after_hang_up<T>(join: JoinHandle<T>, what: &str) -> T {
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, what, || {
        join.is_finished().then_some(())
    });
    match join.join() {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// Reads `stream` to EOF in polled slices under [`aterm_spec::wait::MUST_HAPPEN`].
#[cfg(unix)]
#[track_caller]
pub(crate) fn read_to_end_of_file(
    stream: &CtlStream,
    report: impl FnOnce(&[u8]) -> String,
) -> Vec<u8> {
    use std::io::Read as _;
    use std::os::fd::AsRawFd as _;
    let until = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let (mut read, mut chunk) = (Vec::new(), [0u8; 4096]);
    loop {
        let readable =
            crate::control_listener::poll_readable(stream.as_raw_fd(), crate::control::HANGUP_POLL);
        match readable {
            Ok(true) => match (&*stream).read(&mut chunk) {
                Ok(0) => return read,
                Ok(n) => read.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => panic!("read to end-of-file: {e}: {}", report(&read)),
            },
            Ok(false) => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => panic!("poll for end-of-file: {e}: {}", report(&read)),
        }
        if std::time::Instant::now() >= until {
            panic!(
                "hang detector: no end-of-file within {:?}: {}",
                aterm_spec::wait::MUST_HAPPEN,
                report(&read)
            );
        }
    }
}

/// Reads `stream` to EOF using read timeouts under [`aterm_spec::wait::MUST_HAPPEN`].
#[cfg(not(unix))]
#[track_caller]
pub(crate) fn read_to_end_of_file(
    stream: &CtlStream,
    report: impl FnOnce(&[u8]) -> String,
) -> Vec<u8> {
    use std::io::Read as _;
    stream
        .set_read_timeout(Some(crate::control::HANGUP_POLL))
        .expect("a read slice");
    let until = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let (mut read, mut chunk) = (Vec::new(), [0u8; 4096]);
    loop {
        match (&*stream).read(&mut chunk) {
            Ok(0) => return read,
            Ok(n) => read.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                if std::time::Instant::now() >= until {
                    panic!(
                        "hang detector: no end-of-file within {:?}: {}",
                        aterm_spec::wait::MUST_HAPPEN,
                        report(&read)
                    );
                }
            }
            Err(e) => panic!("read to end-of-file: {e}: {}", report(&read)),
        }
    }
}

/// Bind a Unix stream socket at `path` (whatever was there removed first) that never listens, and
/// answer its descriptor: a socket file nothing listens on, as a crashed server leaves. It refuses
/// every connect (`ECONNREFUSED`) for as long as its descriptor, or any copy of it, lives. A
/// listener bound and closed at once does not: a child another test's thread spawned while it was
/// open keeps a copy until its `exec`, or for its whole life when std's spawn (which takes no
/// spawn lock) copied the descriptor between its `socket(2)` and its close-on-exec mark, and a
/// connect to the file is queued on that copy's backlog and reads as answered. The descriptor is
/// made through [`aterm_uds::kept_out`] and closes on exec.
/// # Panics
/// When the socket cannot be made or bound, or `path` does not fit `sun_path`.
#[cfg(unix)]
#[track_caller]
pub(crate) fn never_listening_socket(path: &std::path::Path) -> std::os::fd::OwnedFd {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::os::unix::ffi::OsStrExt as _;
    let _ = std::fs::remove_file(path);
    let socket = aterm_uds::kept_out(|| {
        // SAFETY: `socket(2)` takes three integers and touches no memory.
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `socket(2)` just made this descriptor, and nothing else owns it.
        let socket = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: `F_SETFD` sets one descriptor's flags and touches no memory.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(socket)
    })
    .expect("a Unix stream socket");
    // SAFETY: `sockaddr_un` is a plain C address; zero also terminates `sun_path`.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_bytes();
    assert!(
        bytes.len() < address.sun_path.len(),
        "{} fits sun_path",
        path.display()
    );
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = libc::c_char::from_ne_bytes([*byte]);
    }
    // SAFETY: `address` is initialized and its whole size is passed beside it.
    let bound = unsafe {
        libc::bind(
            socket.as_raw_fd(),
            std::ptr::from_ref(&address).cast::<libc::sockaddr>(),
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    assert_eq!(
        bound,
        0,
        "bind {}: {}",
        path.display(),
        std::io::Error::last_os_error()
    );
    socket
}

/// Leave `path` as a stand-in host that is done serving must leave it: a socket file nothing
/// listens on ([`never_listening_socket`], bound and closed; the file refuses every connect
/// whether that descriptor is held, copied into a child or closed). The stand-in calls this
/// before it ends the connection it served, since that end starts the relay's look at `path`.
/// Dropping its listener is not the going: a child another test forked while the listener was
/// open holds a copy of it, the look's connect is queued on that copy's backlog, and a client
/// the relay cannot see gone (the same child holds a copy of its end too) is owed the look's
/// whole wait, which is as long as a test's hang detector. Off Unix no child copies a listener,
/// and its drop is the going.
pub(crate) fn leave_no_listener(path: &std::path::Path) {
    #[cfg(unix)]
    drop(never_listening_socket(path));
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(unix)]
mod tests {
    use super::read_to_end_of_file;
    use aterm_uds::CtlStream;
    use std::io::Write as _;

    /// A reader may start after its peer is gone: a forward that found no
    /// token closed before the sibling's thread accepted it. Darwin then
    /// refuses any socket option on the reading end (`EINVAL`), yet what the
    /// peer wrote and its end are there to be read.
    #[test]
    fn an_end_whose_peer_already_closed_reads_to_its_end_of_file() {
        let (near, far) = CtlStream::pair().expect("pair");
        (&far).write_all(b"said\n").expect("the peer writes");
        drop(far);
        aterm_spec::wait::until_peer_closed(&near);
        let read = read_to_end_of_file(&near, |read| {
            format!("after {:?}", String::from_utf8_lossy(read))
        });
        assert_eq!(read, b"said\n");
    }
}
