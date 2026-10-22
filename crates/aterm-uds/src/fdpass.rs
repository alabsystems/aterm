// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Descriptor passing over a connected control socket: `SCM_RIGHTS` send and
//! receive, plus the kernel-attested peer pid, for handing live descriptors to
//! a process this one did not fork (a LaunchServices-launched successor
//! inherits no descriptor table).
//!
//! The module only moves descriptors and names the peer. Every error is a
//! refusal the caller must fall back from (fork lane or rollback): a lost PTY
//! master is a lost user session.
//!
//! Properties the code relies on:
//!
//! * Received descriptors arrive at different numbers but share the sender's
//!   open file descriptions, so identity must be `fstat`-derived, never a
//!   descriptor number.
//! * `FD_CLOEXEC` is clear on arrival and Darwin has no `MSG_CMSG_CLOEXEC`, so
//!   [`recv_with_fds`] sets it on each descriptor as soon as it owns it, before
//!   any other validation.
//! * `MSG_CTRUNC` leaks: the kernel installs every descriptor sent, then truncates the
//!   message naming them. Truncation is fatal, every parsed descriptor is closed on that path,
//!   and the parse clamps to the control buffer supplied rather than the carried `cmsg_len`.
//!   So the receive budgets control space for [`CONTROL_FDS`] descriptors (twice
//!   [`MAX_FDS`]) whatever the caller's `max_fds`: an over-send is a visible over-count to
//!   close and refuse.
//! * Every parsed descriptor becomes an [`OwnedDescriptor`] before the next fallible step,
//!   so every error return closes what it parsed by dropping it.
//! * Every send carries `MSG_NOSIGNAL`, so a hung-up peer is `EPIPE` even while SIGPIPE is
//!   at its default. [`send_vectored`], the same `sendmsg` with no control message, is the
//!   gather write std does not make signal-free (see its docs).
//!
//! Platforms: Darwin, and Linux on x86-64/aarch64 (the two ABIs mirrored
//! below). Other targets, including Windows (afunix carries no ancillary
//! data), get the same entry points returning [`io::ErrorKind::Unsupported`];
//! only [`peer_pid`] and [`send_vectored`] are wired there.

use std::io;
use std::io::IoSlice;

use crate::CtlStream;

/// The most descriptors one [`send_with_fds`] / [`recv_with_fds`] carries.
///
/// The ceiling keeps the control buffer a fixed stack array and refuses an
/// absurd count before the kernel sees it. Callers with more descriptors chunk
/// them across messages.
pub const MAX_FDS: usize = 64;

/// Descriptor slots the receive budgets control space for, whatever the caller's `max_fds`
/// (see the module docs). Gated like `scm`, its one reader.
#[cfg(any(
    target_vendor = "apple",
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
))]
pub const CONTROL_FDS: usize = 2 * MAX_FDS;

/// The borrowed-descriptor type [`send_with_fds`] takes: `BorrowedFd` on Unix,
/// `BorrowedHandle` on Windows (where the operation is unsupported).
#[cfg(unix)]
pub type BorrowedDescriptor<'a> = std::os::fd::BorrowedFd<'a>;
/// The owned-descriptor type [`recv_with_fds`] returns; dropping it on an
/// error path is what makes a refusal leak-free.
#[cfg(unix)]
pub type OwnedDescriptor = std::os::fd::OwnedFd;

/// Windows twin of the Unix alias, so the signatures compile there; afunix
/// cannot pass a handle.
#[cfg(windows)]
pub type BorrowedDescriptor<'a> = std::os::windows::io::BorrowedHandle<'a>;
/// Windows twin of the Unix alias (see the Unix definition).
#[cfg(windows)]
pub type OwnedDescriptor = std::os::windows::io::OwnedHandle;

/// What one [`recv_with_fds`] dequeued: the payload bytes written into the
/// caller's buffer, and the descriptors that rode with them.
///
/// `bytes == 0` with no descriptors is EOF (the peer hung up), exactly as a
/// zero-length `read` is.
#[derive(Debug)]
pub struct Received {
    /// Payload bytes written into the caller's buffer.
    pub bytes: usize,
    /// The received descriptors, in the order the sender listed them, already
    /// `FD_CLOEXEC`-marked and owned by the caller.
    pub fds: Vec<OwnedDescriptor>,
}

/// Send `payload` and `fds` as one message on the connected `stream`,
/// returning the payload bytes the kernel accepted.
///
/// The descriptors ride the first byte of `payload` and are delivered to
/// whichever `recvmsg` dequeues it, whatever this call returns. A short return
/// means the rest of the payload still needs an ordinary `write`, never that
/// the descriptors should be sent again; keep `payload` small (the handoff's
/// is a fixed-size header).
///
/// An empty `fds` sends no control message, exactly like `write`.
///
/// # Errors
/// * `InvalidInput`: an empty payload (a zero-length stream `sendmsg` drops
///   the control message on Linux), or more than [`MAX_FDS`] descriptors.
/// * `Unsupported`: this platform has no `SCM_RIGHTS` (see the module docs).
/// * Anything `sendmsg(2)` reports (`EPIPE` if the peer is gone: the send
///   carries `MSG_NOSIGNAL`, so this is an error, not a signal).
pub fn send_with_fds(
    stream: &CtlStream,
    payload: &[u8],
    fds: &[BorrowedDescriptor<'_>],
) -> io::Result<usize> {
    scm::send_with_fds(stream, payload, fds)
}

/// Send `bufs` in order as one gather write on the connected `stream` and
/// return the bytes the kernel accepted. A short return, as from
/// `write_vectored`, leaves the rest to send again.
///
/// The `write_vectored` std gives a [`CtlStream`] is a bare `writev`: its
/// `write` sends with `MSG_NOSIGNAL` on Linux, its gather does not. On Darwin
/// std marks `SO_NOSIGPIPE` only on sockets it makes with `socket(2)`, not on
/// a `socketpair(2)`'s ends. Either raises SIGPIPE at a hung-up peer
/// whenever SIGPIPE is not ignored, and `Command::exec` sets it back to its
/// default for the whole process before its `execve`. This is `sendmsg`
/// with `MSG_NOSIGNAL` and no control message: the gather is kept, the
/// signal is not.
///
/// One call takes at most the first 1024 slices (`IOV_MAX` on Darwin and
/// Linux). On Windows it is the stream's own `WSASend` (no SIGPIPE there);
/// on a Unix this file does not vouch for, std's `write` of the first
/// non-empty slice.
///
/// # Errors
/// Anything `sendmsg(2)` reports (`EPIPE` if the peer is gone).
pub fn send_vectored(stream: &CtlStream, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
    scm::send_vectored(stream, bufs)
}

/// Receive one message from `stream` into `buf`, accepting at most `max_fds` descriptors with
/// it (checked after the receive, whose budget is always [`CONTROL_FDS`]). Every returned
/// descriptor is already `FD_CLOEXEC`.
///
/// Size `buf` for the whole message. A short `buf` leaves the remaining bytes
/// queued, but all of the message's descriptors arrive on this call.
///
/// # Errors
/// * `InvalidInput`: an empty `buf` (indistinguishable from EOF), or
///   `max_fds` above [`MAX_FDS`].
/// * `InvalidData`: more descriptors than `max_fds`, or a truncated control
///   message. Every received descriptor is closed first; after a truncation,
///   unnamed descriptors remain in this process, so treat the connection as
///   unusable and fall back rather than retry.
/// * `Interrupted`: a signal arrived before anything was dequeued; retrying
///   is safe.
/// * `Unsupported`: this platform has no `SCM_RIGHTS` (see the module docs).
pub fn recv_with_fds(stream: &CtlStream, buf: &mut [u8], max_fds: usize) -> io::Result<Received> {
    scm::recv_with_fds(stream, buf, max_fds)
}

/// The connected peer's pid as the kernel names it: `LOCAL_PEERPID` on Darwin, `SO_PEERCRED`
/// on Linux, `SIO_AF_UNIX_GETPEERPID` on Windows.
///
/// A peer cannot assert this for itself. Paired with a published birth record (which shows
/// the pid was not recycled) it attests who dialed on Linux, where `SO_PEERCRED` is fixed at
/// `connect(2)`/`socketpair(2)`. On Darwin it is the process that last used the connection,
/// read live (measured 2026-10-03), so another holder of the connection moves it by writing
/// on it.
///
/// # Errors
/// * Anything `getsockopt(2)` reports, notably `ENOTCONN`.
/// * `InvalidData`: the kernel answered a pid of zero or below.
/// * On Windows, whatever the ioctl reports; the control channel then keeps its
///   directory-ACL and token posture.
/// * `Unsupported`: a Unix this file does not vouch for (`SO_PEERCRED` and `SOL_SOCKET` are
///   renumbered on some architectures).
pub fn peer_pid(stream: &CtlStream) -> io::Result<u32> {
    scm::peer_pid(stream)
}

/// [`peer_pid`] of the connected socket `fd` names, which stays the caller's: for a server
/// lane that holds the connection it serves as a descriptor rather than a stream (the
/// control server's `ServingFd`).
///
/// # Errors
/// As [`peer_pid`]'s, and `EBADF`/`ENOTSOCK` for a descriptor that names no socket.
#[cfg(unix)]
pub fn peer_pid_of_fd(fd: std::os::fd::RawFd) -> io::Result<u32> {
    scm::peer_pid_of_fd(fd)
}

/// The two ABIs this file mirrors field-for-field. Everything outside `abi` is
/// platform-independent: the cmsg walk, the ownership discipline, the
/// validation.
#[cfg(any(
    target_vendor = "apple",
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
))]
mod scm {
    use std::io;
    use std::io::IoSlice;
    use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};

    use super::{CONTROL_FDS, MAX_FDS, Received};
    use crate::CtlStream;

    /// `struct iovec` — identical on both supported ABIs, so it is declared
    /// once and referred to from each `abi::MsgHdr`.
    #[repr(C)]
    struct IoVec {
        iov_base: *mut core::ffi::c_void,
        iov_len: usize,
    }

    // std guarantees an `IoSlice` is ABI-compatible with `struct iovec`, which
    // is what lets [`send_vectored`] hand the caller's slices to the kernel.
    const _: () = {
        if size_of::<IoSlice<'static>>() != size_of::<IoVec>()
            || align_of::<IoSlice<'static>>() != align_of::<IoVec>()
        {
            panic!("an IoSlice must be laid out as a struct iovec");
        }
    };

    /// `IOV_MAX` on Darwin and Linux: a longer `msg_iov` is `EMSGSIZE`, so a
    /// gather takes this many slices and returns short.
    const MAX_IOV: usize = 1024;

    /// Darwin's `sys/socket.h`: `msg_iovlen`/`msg_flags` are `int` and
    /// `msg_namelen`/`msg_controllen` are `socklen_t` (`u32`), unlike Linux,
    /// so the structs are spelled out per platform.
    #[cfg(target_vendor = "apple")]
    mod abi {
        use std::io;
        use std::os::fd::RawFd;

        #[repr(C)]
        pub struct MsgHdr {
            msg_name: *mut core::ffi::c_void,
            msg_namelen: u32,
            msg_iov: *mut super::IoVec,
            msg_iovlen: i32,
            msg_control: *mut core::ffi::c_void,
            msg_controllen: u32,
            msg_flags: i32,
        }

        /// `struct cmsghdr`: `socklen_t cmsg_len; int cmsg_level; int
        /// cmsg_type;`, 12 bytes (Linux's is 16).
        #[repr(C)]
        pub struct CmsgHdr {
            cmsg_len: u32,
            cmsg_level: i32,
            cmsg_type: i32,
        }

        pub const LEVEL_OFFSET: usize = 4;
        /// See [`LEVEL_OFFSET`].
        pub const TYPE_OFFSET: usize = 8;

        /// Darwin's cmsg macros round with `__DARWIN_ALIGN32` (4 bytes), not
        /// `sizeof(size_t)` like Linux's `CMSG_ALIGN`, so the data area starts
        /// at 12, right after the header. A 16-byte offset makes the kernel
        /// read the 4 padding bytes as an extra descriptor (a dup of fd 0)
        /// that the receiver then leaks.
        pub const CMSG_ALIGNMENT: usize = size_of::<u32>();

        pub const SOL_SOCKET: i32 = 0xffff;
        pub const SCM_RIGHTS: i32 = 0x01;
        pub const MSG_CTRUNC: i32 = 0x20;
        /// `MSG_NOSIGNAL` (`sys/socket.h`): a send to a hung-up peer is
        /// `EPIPE`, not a `SIGPIPE`. std's `SO_NOSIGPIPE` does not cover a
        /// `socketpair(2)`'s ends; with SIGPIPE at its default, a send to one
        /// whose peer closed killed the sender, and this flag made it `EPIPE`
        /// (measured on macOS 27).
        pub const SEND_FLAGS: i32 = 0x80000;

        /// `sys/un.h`: level `SOL_LOCAL`, option `LOCAL_PEERPID`.
        const SOL_LOCAL: i32 = 0;
        const LOCAL_PEERPID: i32 = 0x002;

        pub fn msghdr(
            iov: *mut super::IoVec,
            iov_len: usize,
            control: *mut u8,
            control_len: usize,
        ) -> MsgHdr {
            MsgHdr {
                msg_name: core::ptr::null_mut(),
                msg_namelen: 0,
                msg_iov: iov,
                msg_iovlen: iov_len as i32,
                msg_control: if control_len == 0 {
                    core::ptr::null_mut()
                } else {
                    control.cast()
                },
                msg_controllen: control_len as u32,
                msg_flags: 0,
            }
        }

        pub fn control_len(msg: &MsgHdr) -> usize {
            msg.msg_controllen as usize
        }

        pub fn msg_flags(msg: &MsgHdr) -> i32 {
            msg.msg_flags
        }

        pub fn read_cmsg_len(bytes: &[u8]) -> Option<usize> {
            let raw: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
            Some(u32::from_ne_bytes(raw) as usize)
        }

        pub fn write_cmsg_len(bytes: &mut [u8], value: usize) {
            if let Some(slot) = bytes.get_mut(..4) {
                slot.copy_from_slice(&(value as u32).to_ne_bytes());
            }
        }

        /// See [`super::super::peer_pid`].
        // Skip: bottoms out at the `getsockopt(2)` FFI call (unverifiable C body).
        #[cfg_attr(trust_verify, trust::skip)]
        pub fn peer_pid(fd: RawFd) -> io::Result<u32> {
            let mut pid: i32 = 0;
            let mut len: u32 = 4;
            // SAFETY: `pid`/`len` are live locals for the call; `len` states the exact size
            // of the `pid_t` the kernel writes, and is re-read below.
            let rc = unsafe {
                super::getsockopt(
                    fd,
                    SOL_LOCAL,
                    LOCAL_PEERPID,
                    core::ptr::from_mut(&mut pid).cast(),
                    &mut len,
                )
            };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
            super::checked_pid(pid, len as usize, 4)
        }
    }

    /// Linux's `bits/socket.h` on x86-64/aarch64, item for item as the Darwin `abi`:
    /// `msg_iovlen`/`msg_controllen` are `size_t`, `msg_namelen` is `socklen_t`. musl declares
    /// the first two as `int` plus padding, which matches only on a 64-bit little-endian
    /// target, hence the narrow `cfg` on this module.
    #[cfg(not(target_vendor = "apple"))]
    mod abi {
        use std::io;
        use std::os::fd::RawFd;

        #[repr(C)]
        pub struct MsgHdr {
            msg_name: *mut core::ffi::c_void,
            msg_namelen: u32,
            msg_iov: *mut super::IoVec,
            msg_iovlen: usize,
            msg_control: *mut core::ffi::c_void,
            msg_controllen: usize,
            msg_flags: i32,
        }

        /// `struct cmsghdr`: `size_t cmsg_len; int cmsg_level; int
        /// cmsg_type;`, 16 bytes with no tail padding (Darwin's is 12).
        #[repr(C)]
        pub struct CmsgHdr {
            cmsg_len: usize,
            cmsg_level: i32,
            cmsg_type: i32,
        }

        pub const LEVEL_OFFSET: usize = 8;
        pub const TYPE_OFFSET: usize = 12;

        /// `CMSG_ALIGN` rounds to `sizeof(size_t)` here (8, against Darwin's
        /// 4). The 16-byte header puts the data area at 16 either way; the
        /// alignment still shows up in `CMSG_SPACE`.
        pub const CMSG_ALIGNMENT: usize = size_of::<usize>();

        pub const SOL_SOCKET: i32 = 1;
        pub const SCM_RIGHTS: i32 = 0x01;
        pub const MSG_CTRUNC: i32 = 0x08;
        /// `MSG_NOSIGNAL`: a send to a hung-up peer is `EPIPE`, not a
        /// `SIGPIPE` that kills the process.
        pub const SEND_FLAGS: i32 = 0x4000;

        /// `SO_PEERCRED` at `SOL_SOCKET`, answering a `struct ucred`
        /// (`pid_t pid; uid_t uid; gid_t gid;` — three 32-bit fields).
        const SO_PEERCRED: i32 = 17;
        const UCRED_LEN: usize = 12;

        pub fn msghdr(
            iov: *mut super::IoVec,
            iov_len: usize,
            control: *mut u8,
            control_len: usize,
        ) -> MsgHdr {
            MsgHdr {
                msg_name: core::ptr::null_mut(),
                msg_namelen: 0,
                msg_iov: iov,
                msg_iovlen: iov_len,
                msg_control: if control_len == 0 {
                    core::ptr::null_mut()
                } else {
                    control.cast()
                },
                msg_controllen: control_len,
                msg_flags: 0,
            }
        }

        pub fn control_len(msg: &MsgHdr) -> usize {
            msg.msg_controllen
        }

        pub fn msg_flags(msg: &MsgHdr) -> i32 {
            msg.msg_flags
        }

        pub fn read_cmsg_len(bytes: &[u8]) -> Option<usize> {
            let raw: [u8; 8] = bytes.get(..8)?.try_into().ok()?;
            Some(usize::from_ne_bytes(raw))
        }

        pub fn write_cmsg_len(bytes: &mut [u8], value: usize) {
            if let Some(slot) = bytes.get_mut(..8) {
                slot.copy_from_slice(&value.to_ne_bytes());
            }
        }

        /// See [`super::super::peer_pid`].
        // Skip: bottoms out at the `getsockopt(2)` FFI call (unverifiable C body).
        #[cfg_attr(trust_verify, trust::skip)]
        pub fn peer_pid(fd: RawFd) -> io::Result<u32> {
            let mut cred = [0i32; 3];
            let mut len: u32 = UCRED_LEN as u32;
            // SAFETY: `cred`/`len` are live locals for the call; `len` states the exact size
            // of the `struct ucred` the kernel writes, and is re-read below.
            let rc = unsafe {
                super::getsockopt(
                    fd,
                    SOL_SOCKET,
                    SO_PEERCRED,
                    cred.as_mut_ptr().cast(),
                    &mut len,
                )
            };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
            super::checked_pid(cred[0], len as usize, UCRED_LEN)
        }
    }

    // The whole FFI surface of this module, in one block. `fcntl` is variadic
    // by declaration; `F_SETFD` takes one `int`.
    unsafe extern "C" {
        fn sendmsg(fd: RawFd, msg: *const abi::MsgHdr, flags: i32) -> isize;
        fn recvmsg(fd: RawFd, msg: *mut abi::MsgHdr, flags: i32) -> isize;
        fn getsockopt(
            fd: RawFd,
            level: i32,
            name: i32,
            value: *mut core::ffi::c_void,
            len: *mut u32,
        ) -> i32;
        fn fcntl(fd: RawFd, cmd: i32, ...) -> i32;
    }

    /// `fcntl(2)`'s descriptor-flag setter and the one flag there is. Same
    /// numbers on every supported Unix, so they are not per-ABI.
    const F_SETFD: i32 = 2;
    const FD_CLOEXEC: i32 = 1;

    const CMSG_HDR_LEN: usize = size_of::<abi::CmsgHdr>();
    /// `CMSG_DATA`'s offset from the header, `CMSG_ALIGN(sizeof(cmsghdr))`:
    /// 12 on Darwin, 16 on Linux (see each `abi::CMSG_ALIGNMENT`).
    const CMSG_DATA_OFFSET: usize = cmsg_align(CMSG_HDR_LEN);
    /// One passed descriptor is one `int` on the wire.
    const FD_LEN: usize = size_of::<RawFd>();
    /// The fixed control-buffer size — see [`super::CONTROL_FDS`].
    const CONTROL_LEN: usize = cmsg_space(CONTROL_FDS * FD_LEN);

    const fn cmsg_align(len: usize) -> usize {
        (len + (abi::CMSG_ALIGNMENT - 1)) & !(abi::CMSG_ALIGNMENT - 1)
    }

    /// `CMSG_SPACE`: header + payload, each rounded up; the buffer size a
    /// message needs, which is what the receive offers the kernel.
    const fn cmsg_space(data_len: usize) -> usize {
        CMSG_DATA_OFFSET + cmsg_align(data_len)
    }

    /// `CMSG_LEN`: header + payload, payload not rounded up; what `cmsg_len`
    /// must say, and what the send gives as `msg_controllen`.
    const fn cmsg_len(data_len: usize) -> usize {
        CMSG_DATA_OFFSET + data_len
    }

    /// A control buffer with `cmsghdr` alignment: the kernel reads and writes a
    /// `struct cmsghdr` at offset 0, though this file's parse never assumes
    /// alignment.
    #[repr(C, align(8))]
    struct ControlBuf([u8; CONTROL_LEN]);

    impl ControlBuf {
        const ZEROED: Self = Self([0; CONTROL_LEN]);
    }

    // The `align(8)` above must dominate the header's alignment on every ABI
    // this file compiles for.
    const _: () = {
        if align_of::<ControlBuf>() < align_of::<abi::CmsgHdr>() {
            panic!("the control buffer must be at least cmsghdr-aligned");
        }
    };

    /// Send `payload` and `fds` as one message. See [`super::send_with_fds`].
    // Skip: bottoms out at the `sendmsg(2)` FFI call (unverifiable C body).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn send_with_fds(
        stream: &CtlStream,
        payload: &[u8],
        fds: &[BorrowedFd<'_>],
    ) -> io::Result<usize> {
        if payload.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SCM_RIGHTS needs at least one payload byte: a zero-length \
                 stream sendmsg transfers nothing and drops the descriptors",
            ));
        }
        if fds.len() > MAX_FDS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} descriptors is over the {MAX_FDS} cap", fds.len()),
            ));
        }

        let mut control = ControlBuf::ZEROED;
        // The buffer is sized by `CMSG_SPACE`, but the declared length is
        // `CMSG_LEN`: Darwin refuses with EINVAL unless the one `cmsg_len`
        // equals the control length it was handed; Linux accepts either.
        let control_len = if fds.is_empty() {
            0
        } else {
            let data_len = fds.len() * FD_LEN;
            abi::write_cmsg_len(&mut control.0, cmsg_len(data_len));
            write_i32(&mut control.0, abi::LEVEL_OFFSET, abi::SOL_SOCKET);
            write_i32(&mut control.0, abi::TYPE_OFFSET, abi::SCM_RIGHTS);
            let data = &mut control.0[CMSG_DATA_OFFSET..];
            for (slot, fd) in data.as_chunks_mut::<FD_LEN>().0.iter_mut().zip(fds) {
                slot.copy_from_slice(&fd.as_raw_fd().to_ne_bytes());
            }
            cmsg_len(data_len)
        };

        let mut iov = IoVec {
            iov_base: payload.as_ptr().cast_mut().cast(),
            iov_len: payload.len(),
        };
        let msg = abi::msghdr(&mut iov, 1, control.0.as_mut_ptr(), control_len);
        // SAFETY: `msg` points at `iov` and `control`, both live for the whole
        // call; `iov` describes exactly `payload`'s bytes (never written —
        // sendmsg only reads them) and `control_len` never exceeds
        // `control`'s length by construction (`fds.len() <= MAX_FDS`, and the
        // buffer is sized for `CONTROL_FDS >= MAX_FDS`).
        let sent = unsafe { sendmsg(stream.as_raw_fd(), &msg, abi::SEND_FLAGS) };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(sent as usize)
    }

    /// Send `bufs` as one message with no control data. See
    /// [`super::send_vectored`].
    // Skip: bottoms out at the `sendmsg(2)` FFI call (unverifiable C body).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn send_vectored(stream: &CtlStream, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let bufs = &bufs[..bufs.len().min(MAX_IOV)];
        let msg = abi::msghdr(
            bufs.as_ptr().cast::<IoVec>().cast_mut(),
            bufs.len(),
            core::ptr::null_mut(),
            0,
        );
        // SAFETY: `msg` names `bufs.len()` iovecs at `bufs`, which is live for
        // the whole call and laid out as `struct iovec` (asserted above);
        // sendmsg only reads them. It names no control buffer.
        let sent = unsafe { sendmsg(stream.as_raw_fd(), &msg, abi::SEND_FLAGS) };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(sent as usize)
    }

    /// Receive one message and its descriptors. See [`super::recv_with_fds`].
    // Skip: bottoms out at the `recvmsg(2)`/`fcntl(2)` FFI calls. The cmsg walk,
    // `scan_control`, is pure and stays in scope for the verifier.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn recv_with_fds(
        stream: &CtlStream,
        buf: &mut [u8],
        max_fds: usize,
    ) -> io::Result<Received> {
        if buf.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a zero-length receive cannot carry descriptors and cannot be \
                 told apart from EOF",
            ));
        }
        if max_fds > MAX_FDS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("max_fds {max_fds} is over the {MAX_FDS} cap"),
            ));
        }

        let mut control = ControlBuf::ZEROED;
        let mut iov = IoVec {
            iov_base: buf.as_mut_ptr().cast(),
            iov_len: buf.len(),
        };
        let mut msg = abi::msghdr(&mut iov, 1, control.0.as_mut_ptr(), CONTROL_LEN);
        // SAFETY: `msg` points at `iov` and `control`, both live for the whole
        // call; `iov` describes exactly `buf`'s bytes and the control buffer
        // is exactly `CONTROL_LEN` long, which is what the header claims.
        let got = unsafe { recvmsg(stream.as_raw_fd(), &mut msg, 0) };
        if got < 0 {
            return Err(io::Error::last_os_error());
        }

        // Parse only what the kernel wrote, clamped to what was supplied: a
        // truncated message still carries the untruncated `cmsg_len`.
        let filled = abi::control_len(&msg).min(CONTROL_LEN);
        let scan = scan_control(&control.0[..filled]);

        // Take ownership before any fallible step: from here on every return
        // path closes what was parsed simply by dropping `fds`.
        let mut fds: Vec<OwnedFd> = Vec::with_capacity(scan.fds.len());
        let mut malformed = scan.truncated;
        for raw in scan.fds {
            if raw < 0 {
                // The kernel never writes one; if it somehow did there is
                // nothing to close, so record it and refuse below.
                malformed = true;
                continue;
            }
            // SAFETY: `raw` came out of an SCM_RIGHTS control message, so the
            // kernel just installed it in this process's descriptor table and
            // nothing else owns it. Each number appears once in a message, so
            // no two `OwnedFd`s here alias.
            fds.push(unsafe { OwnedFd::from_raw_fd(raw) });
        }
        // Before any validation or error path: Darwin has no
        // MSG_CMSG_CLOEXEC, so an `exec` on another thread could otherwise
        // inherit the PTY masters.
        for fd in &fds {
            set_cloexec(fd.as_raw_fd())?;
        }

        if malformed || (abi::msg_flags(&msg) & abi::MSG_CTRUNC) != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated SCM_RIGHTS control message: descriptors the kernel \
                 installed were not described, so this connection is unusable",
            ));
        }
        if fds.len() > max_fds {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "peer sent {} descriptors, at most {max_fds} allowed",
                    fds.len()
                ),
            ));
        }

        Ok(Received {
            bytes: got as usize,
            fds,
        })
    }

    /// See [`super::peer_pid`].
    pub fn peer_pid(stream: &CtlStream) -> io::Result<u32> {
        abi::peer_pid(stream.as_raw_fd())
    }

    /// See [`super::peer_pid_of_fd`].
    pub fn peer_pid_of_fd(fd: RawFd) -> io::Result<u32> {
        abi::peer_pid(fd)
    }

    /// Validate what `getsockopt` answered: the option must have come back at
    /// its full size (a short answer means we read something that is not the
    /// structure we asked for), and a pid must be a pid.
    fn checked_pid(pid: i32, got_len: usize, want_len: usize) -> io::Result<u32> {
        if got_len != want_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("peer-pid option answered {got_len} bytes, expected {want_len}"),
            ));
        }
        u32::try_from(pid).ok().filter(|&p| p > 0).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("peer pid {pid} does not name a process"),
            )
        })
    }

    /// Mark a just-received descriptor close-on-exec.
    // Skip: bottoms out at the `fcntl(2)` FFI call (unverifiable C body).
    #[cfg_attr(trust_verify, trust::skip)]
    fn set_cloexec(fd: RawFd) -> io::Result<()> {
        // SAFETY: `fd` is a descriptor this process owns for the duration of
        // the call; `F_SETFD` consumes exactly one `int` from the varargs, and
        // `FD_CLOEXEC` is the only descriptor flag there is, so setting it
        // outright clears nothing else.
        if unsafe { fcntl(fd, F_SETFD, FD_CLOEXEC) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// What one control-buffer walk found.
    struct ControlScan {
        /// Raw descriptor numbers, in message order; not owned yet, so the
        /// caller must take ownership immediately.
        fds: Vec<RawFd>,
        /// The buffer did not contain everything its headers claimed (or a
        /// header was malformed). Fatal: see the module docs.
        truncated: bool,
    }

    /// Walk `control` (the bytes the kernel wrote) and collect every `SCM_RIGHTS` descriptor
    /// number in it.
    ///
    /// Pure and total, so the verifier and the unit tests reach it. `cmsg_len` describes the
    /// message the kernel built, not the bytes it copied out, so every read clamps to
    /// `control.len()` and an overrun is `truncated`. A `cmsg_len` shorter than the header is
    /// refused, where `CMSG_NXTHDR` stops quietly: none is legitimate in what the kernel wrote.
    fn scan_control(control: &[u8]) -> ControlScan {
        let mut scan = ControlScan {
            fds: Vec::new(),
            truncated: false,
        };
        let mut off = 0usize;
        // Every read below is inside `off + CMSG_HDR_LEN <= control.len()`.
        while off.saturating_add(CMSG_HDR_LEN) <= control.len() {
            let Some(len) = abi::read_cmsg_len(&control[off..]) else {
                break;
            };
            // A header that does not even reach its own data area is junk.
            if len < CMSG_DATA_OFFSET {
                scan.truncated = true;
                break;
            }
            let level = read_i32(&control[off + abi::LEVEL_OFFSET..]);
            let kind = read_i32(&control[off + abi::TYPE_OFFSET..]);

            // Never trust `cmsg_len` past the end of the buffer.
            let claimed_end = off.saturating_add(len);
            if claimed_end > control.len() {
                scan.truncated = true;
            }
            let end = claimed_end.min(control.len());
            let data_start = off + CMSG_DATA_OFFSET;
            if data_start > end {
                scan.truncated = true;
                break;
            }
            if level == abi::SOL_SOCKET && kind == abi::SCM_RIGHTS {
                let data = &control[data_start..end];
                for slot in data.as_chunks::<FD_LEN>().0.iter() {
                    scan.fds.push(read_i32(slot));
                }
                // A partial descriptor at the tail is a descriptor the kernel
                // installed and only half described: unclosable, hence fatal.
                if !data.len().is_multiple_of(FD_LEN) {
                    scan.truncated = true;
                }
            }

            // `CMSG_NXTHDR`. `len >= CMSG_DATA_OFFSET > 0`, so this always
            // advances and the loop always terminates.
            off = off.saturating_add(cmsg_align(len));
        }
        scan
    }

    /// Native-endian `i32` from the front of `bytes`, zero-padded if short.
    /// Total, since a panic inside a descriptor-owning function would leak.
    fn read_i32(bytes: &[u8]) -> i32 {
        let mut raw = [0u8; 4];
        let n = bytes.len().min(4);
        raw[..n].copy_from_slice(&bytes[..n]);
        i32::from_ne_bytes(raw)
    }

    /// Native-endian `i32` written at `at`, ignored if it would not fit.
    fn write_i32(bytes: &mut [u8], at: usize, value: i32) {
        if let Some(slot) = bytes.get_mut(at..at + 4) {
            slot.copy_from_slice(&value.to_ne_bytes());
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{
            CMSG_DATA_OFFSET, CMSG_HDR_LEN, CONTROL_LEN, FD_LEN, abi, checked_pid, cmsg_align,
            cmsg_len, cmsg_space, scan_control, write_i32,
        };

        /// One `SCM_RIGHTS` header whose `cmsg_len` claims `claimed` descriptors, then
        /// `present` descriptor numbers, cut to `keep` bytes (what `msg_controllen` would
        /// say): the shape a truncated receive leaves.
        fn control(claimed: usize, present: &[i32], keep: usize) -> Vec<u8> {
            let mut buf = vec![0u8; CONTROL_LEN];
            abi::write_cmsg_len(&mut buf, cmsg_len(claimed * FD_LEN));
            write_i32(&mut buf, abi::LEVEL_OFFSET, abi::SOL_SOCKET);
            write_i32(&mut buf, abi::TYPE_OFFSET, abi::SCM_RIGHTS);
            for (i, fd) in present.iter().enumerate() {
                write_i32(&mut buf, CMSG_DATA_OFFSET + i * FD_LEN, *fd);
            }
            buf.truncate(keep);
            buf
        }

        /// 12 on Darwin (4-byte alignment), 16 on Linux.
        #[test]
        fn the_data_offset_matches_the_platform_header() {
            let expected = if cfg!(target_vendor = "apple") {
                12
            } else {
                16
            };
            assert_eq!(CMSG_DATA_OFFSET, expected, "CMSG_DATA(cmsg) - cmsg");
            assert_eq!(cmsg_len(4), expected + 4, "CMSG_LEN(4)");
        }

        /// Every message is walked, each header at the aligned end of the one before (here a
        /// foreign one whose one data byte leaves padding), and only `SCM_RIGHTS` yields.
        #[test]
        fn a_complete_message_yields_every_descriptor_in_order() {
            let buf = control(3, &[7, 8, 9], cmsg_space(3 * FD_LEN));
            let scan = scan_control(&buf);
            assert_eq!(scan.fds, [7, 8, 9]);
            assert!(!scan.truncated, "a complete message is not truncated");

            let next = cmsg_align(cmsg_len(1));
            let mut two = vec![0u8; next + cmsg_space(2 * FD_LEN)];
            abi::write_cmsg_len(&mut two, cmsg_len(1));
            write_i32(&mut two, abi::LEVEL_OFFSET, abi::SOL_SOCKET);
            write_i32(&mut two, abi::TYPE_OFFSET, abi::SCM_RIGHTS + 1);
            two[CMSG_DATA_OFFSET] = 0xff;
            two[next..].copy_from_slice(&control(2, &[7, 8], cmsg_space(2 * FD_LEN)));
            let scan = scan_control(&two);
            assert_eq!(scan.fds, [7, 8], "the message after a foreign one");
            assert!(!scan.truncated);
        }

        /// What `getsockopt` answered for the peer pid is taken only whole and only as a pid.
        #[test]
        fn a_peer_pid_is_a_whole_answer_naming_a_process() {
            assert_eq!(checked_pid(7, 4, 4).ok(), Some(7));
            for (pid, got_len) in [(7, 3), (0, 4), (-1, 4)] {
                let refused = checked_pid(pid, got_len, 4).err().map(|e| e.kind());
                assert_eq!(
                    refused,
                    Some(std::io::ErrorKind::InvalidData),
                    "{pid} {got_len}"
                );
            }
        }

        /// `cmsg_len` claims three descriptors, the buffer holds one, or only the header.
        #[test]
        fn a_cmsg_len_past_the_buffer_is_clamped_and_flagged() {
            let buf = control(3, &[7], CMSG_DATA_OFFSET + FD_LEN);
            let scan = scan_control(&buf);
            assert_eq!(scan.fds, [7], "only the descriptor really present");
            assert!(scan.truncated, "an overrunning cmsg_len is truncation");

            let scan = scan_control(&control(1, &[7], CMSG_HDR_LEN));
            assert!(scan.fds.is_empty());
            assert!(scan.truncated, "a header whose descriptors were cut off");
        }

        /// The whole descriptors are reported (ours to close) and the half-copied one makes
        /// the message fatal.
        #[test]
        fn a_partial_descriptor_at_the_tail_is_truncation() {
            let buf = control(3, &[7, 8], CMSG_DATA_OFFSET + FD_LEN + 2);
            let scan = scan_control(&buf);
            assert_eq!(scan.fds, [7]);
            assert!(scan.truncated);

            // The buffer holds all `cmsg_len` claims, which ends mid-descriptor.
            let mut buf = control(2, &[7, 8], cmsg_space(2 * FD_LEN));
            abi::write_cmsg_len(&mut buf, cmsg_len(FD_LEN + 2));
            let scan = scan_control(&buf);
            assert_eq!(scan.fds, [7]);
            assert!(scan.truncated, "half a descriptor inside the buffer");
        }

        /// Neither a non-`SCM_RIGHTS` message (another type, or `SCM_RIGHTS`'s number at
        /// another level) nor a header shorter than its own data area yields a descriptor;
        /// the runt is refused, and a header with no data at all is no runt.
        #[test]
        fn foreign_and_malformed_headers_yield_no_descriptors() {
            for (level, kind) in [
                (abi::SOL_SOCKET, abi::SCM_RIGHTS + 1),
                (abi::SOL_SOCKET + 1, abi::SCM_RIGHTS),
            ] {
                let mut foreign = control(2, &[7, 8], cmsg_space(2 * FD_LEN));
                write_i32(&mut foreign, abi::LEVEL_OFFSET, level);
                write_i32(&mut foreign, abi::TYPE_OFFSET, kind);
                let scan = scan_control(&foreign);
                assert!(scan.fds.is_empty(), "not SCM_RIGHTS: no descriptors");
                assert!(!scan.truncated);
            }

            let scan = scan_control(&control(0, &[], cmsg_space(0)));
            assert!(scan.fds.is_empty());
            assert!(!scan.truncated, "a header exactly its own length is whole");

            let mut runt = control(2, &[7, 8], cmsg_space(2 * FD_LEN));
            abi::write_cmsg_len(&mut runt, CMSG_DATA_OFFSET - 1);
            let scan = scan_control(&runt);
            assert!(scan.fds.is_empty(), "malformed: no descriptors");
            assert!(scan.truncated, "malformed is refused, not ignored");
        }

        /// A peer that sends past `MAX_FDS` (`send_with_fds` refuses to; a raw `sendmsg` does
        /// not), up to the twice-`MAX_FDS` budget the module documents, is still a visible
        /// over-count: every descriptor it sent is named, closed and refused, and none stays
        /// here. The sender's buffer is its own, sized for what it sends: one sized by
        /// `CONTROL_FDS` would shrink with a smaller budget and drop the slots past it.
        #[test]
        fn an_over_send_past_the_cap_is_named_closed_and_refused() {
            use std::os::fd::{AsRawFd as _, FromRawFd as _};
            use std::os::unix::fs::MetadataExt as _;
            const SENT: usize = 2 * super::MAX_FDS;
            #[repr(C, align(8))]
            struct SenderControl([u8; cmsg_space(SENT * FD_LEN)]);
            let scratch = aterm_tempfile::Builder::new()
                .prefix("aterm-uds-oversend.")
                .tempdir()
                .expect("scratch");
            let file = std::fs::File::create(scratch.path().join("passed")).expect("file");
            let want = file.metadata().map(|m| (m.dev(), m.ino())).expect("fstat");
            let copies = || {
                (0..4096)
                    .filter(|&fd| {
                        // SAFETY: borrowed for one `fstat` and never closed (`ManuallyDrop`); a
                        // number nothing has open fails it.
                        let f =
                            std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
                        f.metadata().is_ok_and(|m| (m.dev(), m.ino()) == want)
                    })
                    .count()
            };
            let (a, b) = crate::CtlStream::pair().expect("pair");
            let mut control = SenderControl([0; cmsg_space(SENT * FD_LEN)]);
            abi::write_cmsg_len(&mut control.0, cmsg_len(SENT * FD_LEN));
            write_i32(&mut control.0, abi::LEVEL_OFFSET, abi::SOL_SOCKET);
            write_i32(&mut control.0, abi::TYPE_OFFSET, abi::SCM_RIGHTS);
            for slot in control.0[CMSG_DATA_OFFSET..cmsg_len(SENT * FD_LEN)]
                .as_chunks_mut::<FD_LEN>()
                .0
            {
                slot.copy_from_slice(&file.as_raw_fd().to_ne_bytes());
            }
            let payload = [1u8];
            let mut iov = super::IoVec {
                iov_base: payload.as_ptr().cast_mut().cast(),
                iov_len: payload.len(),
            };
            let msg = abi::msghdr(&mut iov, 1, control.0.as_mut_ptr(), cmsg_len(SENT * FD_LEN));
            // SAFETY: as in `send_with_fds`: `msg` points at the live `iov` and `control`, and
            // the control length it declares is inside `control`.
            let sent = unsafe { super::sendmsg(a.as_raw_fd(), &msg, abi::SEND_FLAGS) };
            assert_eq!(sent, 1, "{}", std::io::Error::last_os_error());
            let before = copies();
            let mut buf = [0u8; 4];
            let refused = super::recv_with_fds(&b, &mut buf, super::MAX_FDS).err();
            assert_eq!(
                refused.map(|e| e.kind()),
                Some(std::io::ErrorKind::InvalidData)
            );
            assert_eq!(copies(), before, "a descriptor the peer sent stayed open");
        }

        /// A receiver at its descriptor limit, where the kernel installs fewer descriptors than
        /// were sent, refuses the message and keeps none that did arrive: Linux installs what
        /// fits, names only those and flags `MSG_CTRUNC` (the flag alone tells it from a whole
        /// message), and Darwin fails the `recvmsg` (`EMFILE`). Run in a fork whose soft
        /// `RLIMIT_NOFILE` leaves room for one of the two sent; `alarm` is its hang detector.
        #[test]
        fn a_message_the_kernel_could_not_install_whole_is_refused() {
            use std::os::fd::{AsFd as _, AsRawFd as _};
            unsafe extern "C" {
                fn alarm(seconds: u32) -> u32;
            }
            const REFUSED: i32 = 0;
            const KEPT: i32 = 1;
            const ACCEPTED: i32 = 2;
            const NO_SETUP: i32 = 3;
            let (a, b) = crate::CtlStream::pair().expect("pair");
            super::send_with_fds(&a, b"x", &[a.as_fd(), a.as_fd()]).expect("sent");
            crate::spawnfd::pass_libnotify_gate();
            // SAFETY: the child runs the receive under test and plain syscalls, then `_exit`s;
            // it never returns into the harness.
            let child = unsafe { libc::fork() };
            if child == 0 {
                let receive = || {
                    let fd = b.as_raw_fd();
                    // SAFETY: plain syscalls on a descriptor this process holds and on a
                    // fully initialized `rlimit`.
                    unsafe {
                        alarm(60);
                        // The two lowest free numbers: a limit at the second leaves the first.
                        let (first, second) = (libc::dup(fd), libc::dup(fd));
                        let mut lim = libc::rlimit {
                            rlim_cur: 0,
                            rlim_max: 0,
                        };
                        if first < 0
                            || second < 0
                            || libc::close(first) != 0
                            || libc::close(second) != 0
                            || libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0
                        {
                            return NO_SETUP;
                        }
                        lim.rlim_cur = second as libc::rlim_t;
                        if libc::setrlimit(libc::RLIMIT_NOFILE, &lim) != 0 {
                            return NO_SETUP;
                        }
                        let mut buf = [0u8; 4];
                        match super::recv_with_fds(&b, &mut buf, super::MAX_FDS) {
                            Ok(_) => ACCEPTED,
                            Err(_) if libc::dup(fd) == first => REFUSED,
                            Err(_) => KEPT,
                        }
                    }
                };
                // SAFETY: the forked child ends here.
                unsafe { libc::_exit(receive()) }
            }
            assert!(child > 0, "fork: {}", std::io::Error::last_os_error());
            let mut status = 0;
            // SAFETY: our own child, reaped once.
            assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
            assert!(libc::WIFEXITED(status), "the child died: {status:#x}");
            assert_eq!(libc::WEXITSTATUS(status), REFUSED);
        }

        /// A buffer too short to hold a header is no message, not a truncated one.
        #[test]
        fn an_empty_control_area_is_empty_not_truncated() {
            for keep in [0, 1, CMSG_HDR_LEN - 1] {
                let buf = control(1, &[7], keep);
                let scan = scan_control(&buf);
                assert!(scan.fds.is_empty());
                assert!(!scan.truncated, "{keep} bytes is no message at all");
            }
        }
    }

    /// A send to a hung-up peer with SIGPIPE at its default, in a helper
    /// process: the test process's own SIGPIPE is not ours to change.
    #[cfg(test)]
    mod sigpipe_tests {
        use std::io::{IoSlice, Write as _};
        use std::os::unix::process::ExitStatusExt as _;
        use std::process::{Command, ExitStatus, Stdio};

        use crate::CtlStream;

        const HELPER_ENV: &str = "ATERM_UDS_SEND_VECTORED_HELPER";
        /// The same numbers on Darwin and Linux.
        const SIGPIPE: i32 = 13;
        const SIG_DFL: usize = 0;
        const EPIPE: i32 = 32;

        unsafe extern "C" {
            fn signal(sig: i32, handler: usize) -> usize;
        }

        /// How the helper ended in `mode`, and what it printed.
        fn helper(mode: &str) -> (ExitStatus, String) {
            let out = Command::new(std::env::current_exe().expect("the test binary"))
                .args([
                    "--exact",
                    "fdpass::scm::sigpipe_tests::zz_helper_entry",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(HELPER_ENV, mode)
                .stderr(Stdio::null())
                .output()
                .expect("the helper runs");
            (
                out.status,
                String::from_utf8_lossy(&out.stdout).into_owned(),
            )
        }

        /// A gather to a `socketpair` end whose peer closed, with SIGPIPE at
        /// its default (as `Command::exec` leaves it before `execve`), is
        /// `EPIPE` and the sender lives. NEGATIVE CONTROL: std's own
        /// `write_vectored` on the same pair kills the helper with SIGPIPE.
        #[test]
        fn a_gather_to_a_hung_up_peer_is_epipe_not_sigpipe() {
            let (status, text) = helper("send_vectored");
            assert!(status.success(), "{status:?}: {text}");
            assert!(
                text.contains(&format!("send_vectored errno=Some({EPIPE})")),
                "{text}"
            );
            let (status, text) = helper("write_vectored");
            assert_eq!(
                status.signal(),
                Some(SIGPIPE),
                "std's writev raises SIGPIPE: {status:?}: {text}"
            );
        }

        /// The helper. Ordinary runs return at once.
        #[test]
        fn zz_helper_entry() {
            let Some(mode) = std::env::var_os(HELPER_ENV) else {
                return;
            };
            // SAFETY: the default disposition is a valid one for SIGPIPE.
            unsafe { signal(SIGPIPE, SIG_DFL) };
            let (near, far) = CtlStream::pair().expect("a socketpair");
            drop(far);
            let bufs = [IoSlice::new(b"ab"), IoSlice::new(b"cd")];
            let sent = if mode == "write_vectored" {
                (&near).write_vectored(&bufs)
            } else {
                super::send_vectored(&near, &bufs)
            };
            println!();
            println!(
                "{} errno={:?}",
                mode.to_string_lossy(),
                sent.err().and_then(|e| e.raw_os_error())
            );
        }
    }
}

/// Every other target: the same entry points, returning `Unsupported`, except
/// [`send_vectored`] and Windows' [`peer_pid`]. Windows' afunix has no
/// ancillary data, and on an unvouched Unix a wrong `msghdr` layout or option
/// number would read the wrong memory instead of failing.
#[cfg(not(any(
    target_vendor = "apple",
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
)))]
mod scm {
    use std::io;
    use std::io::IoSlice;

    use super::{BorrowedDescriptor, Received};
    use crate::CtlStream;

    fn unsupported(what: &str) -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{what} is not available on this platform (no SCM_RIGHTS transport)"),
        )
    }

    pub fn send_with_fds(
        _stream: &CtlStream,
        _payload: &[u8],
        _fds: &[BorrowedDescriptor<'_>],
    ) -> io::Result<usize> {
        Err(unsupported("descriptor passing"))
    }

    pub fn recv_with_fds(
        _stream: &CtlStream,
        _buf: &mut [u8],
        _max_fds: usize,
    ) -> io::Result<Received> {
        Err(unsupported("descriptor passing"))
    }

    #[cfg(windows)]
    pub fn peer_pid(stream: &CtlStream) -> io::Result<u32> {
        stream.peer_pid()
    }

    /// The stream's own gather, `WSASend`: Windows raises no SIGPIPE.
    #[cfg(windows)]
    pub fn send_vectored(stream: &CtlStream, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        io::Write::write_vectored(&mut &*stream, bufs)
    }

    /// No `msghdr` layout to trust, so no gather: the first non-empty slice
    /// goes by std's `write`, the send std makes signal-free where it can.
    #[cfg(not(windows))]
    pub fn send_vectored(stream: &CtlStream, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let first = bufs.iter().find(|buf| !buf.is_empty());
        io::Write::write(&mut &*stream, first.map_or(&[][..], |buf| &buf[..]))
    }

    #[cfg(not(windows))]
    pub fn peer_pid(_stream: &CtlStream) -> io::Result<u32> {
        Err(unsupported("the peer pid"))
    }

    #[cfg(unix)]
    pub fn peer_pid_of_fd(_fd: std::os::fd::RawFd) -> io::Result<u32> {
        Err(unsupported("the peer pid"))
    }
}
