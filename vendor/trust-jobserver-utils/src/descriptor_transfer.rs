//! Bounded transfer of existing descriptions over one private Unix socket.
//!
//! Transferring descriptors grants no resource, process, proof or peer authority.
//! The authenticated protocol owner validates their meaning and retains their
//! original backing. Any send failure after entry is potentially ambiguous;
//! callers must not replay an execution request or reconstruct a resource pool.

use std::io;
use std::mem::size_of;
use std::os::fd::{AsRawFd as _, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::time::Instant;

/// Native subgroup stdio/events, CPU, GNU and the bounded legacy fence set.
/// This is a transport bound, never a grant to use this many resources.
pub const MAX_DESCRIPTORS: usize = 134;

fn time(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline { Err(io::ErrorKind::TimedOut.into()) } else { Ok(()) }
}

/// Send one marker and exact live descriptor set. The receiver must use its
/// complete native ancillary buffer and close every received fd on refusal.
/// This changes neither parent inheritance flags nor process signal policy.
pub fn send(
    stream: &UnixStream,
    descriptors: &[BorrowedFd<'_>],
    marker: u8,
    deadline: Instant,
) -> io::Result<()> {
    if descriptors.is_empty() || descriptors.len() > MAX_DESCRIPTORS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "descriptor transfer population exceeds bound",
        ));
    }
    time(deadline)?;
    let payload = descriptors.len() * size_of::<libc::c_int>();
    // SAFETY: the bounded population fits the native CMSG length argument.
    let control_bytes = unsafe { libc::CMSG_SPACE(payload as _) as usize };
    let mut control = vec![0usize; control_bytes.div_ceil(size_of::<usize>())];
    let mut marker = [marker];
    let mut iovec = libc::iovec { iov_base: marker.as_mut_ptr().cast(), iov_len: 1 };
    // SAFETY: zero initializes the optional native msghdr fields.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iovec;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control_bytes as _;
    // SAFETY: aligned control has room for one complete rights header/payload;
    // every borrowed descriptor remains owned throughout this call.
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(payload as _) as _;
        let rights = libc::CMSG_DATA(header).cast::<libc::c_int>();
        for (index, descriptor) in descriptors.iter().enumerate() {
            rights.add(index).write_unaligned(descriptor.as_raw_fd());
        }
    }
    loop {
        time(deadline)?;
        // SAFETY: all message buffers are initialized and remain live. Per-call
        // nonblocking/SIGPIPE flags mutate neither the socket nor other threads.
        let sent = unsafe {
            libc::sendmsg(stream.as_raw_fd(), &message, libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL)
        };
        if sent == 1 {
            return time(deadline);
        }
        if sent >= 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        let error = io::Error::last_os_error();
        match error.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let mut ready =
                    libc::pollfd { fd: stream.as_raw_fd(), events: libc::POLLOUT, revents: 0 };
                // SAFETY: one initialized pollfd names this held socket.
                let result = unsafe {
                    libc::poll(
                        &mut ready,
                        1,
                        remaining.as_millis().clamp(1, i32::MAX as u128) as i32,
                    )
                };
                if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                    return Err(io::Error::last_os_error());
                }
            }
            _ => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::fd::AsFd as _;
    use std::time::Duration;

    use super::*;

    #[test]
    fn transfer_refuses_expired_empty_and_excessive_bundles_before_sending() {
        let (sender, mut receiver) = UnixStream::pair().unwrap();
        receiver.set_nonblocking(true).unwrap();
        let file = File::open("/dev/null").unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        assert_eq!(
            send(&sender, &[], 1, deadline).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            send(&sender, &vec![file.as_fd(); MAX_DESCRIPTORS + 1], 1, deadline)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            send(&sender, &[file.as_fd()], 1, Instant::now()).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        use std::io::Read as _;
        assert_eq!(receiver.read(&mut [0]).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    }

    #[test]
    fn closed_peer_actor() {
        if std::env::var_os("TRUST_TRANSFER_SIGPIPE_ACTOR").is_none() {
            return;
        }
        // Only this isolated test process restores the default disposition.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
        let (sender, receiver) = UnixStream::pair().unwrap();
        drop(receiver);
        let file = File::open("/dev/null").unwrap();
        let error = send(&sender, &[file.as_fd()], 1, Instant::now() + Duration::from_secs(60))
            .unwrap_err();
        assert!(matches!(error.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset));
    }

    #[test]
    fn descriptor_transfer_survives_closed_peer_with_default_sigpipe() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "descriptor_transfer::tests::closed_peer_actor", "--nocapture"])
            .env("TRUST_TRANSFER_SIGPIPE_ACTOR", "1")
            .status()
            .unwrap();
        assert!(status.success());
    }
}
