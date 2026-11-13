// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! File-descriptor passing (`SCM_RIGHTS`) and peer PID credentials over `CtlStream`.

use std::io;
use std::io::IoSlice;

use crate::CtlStream;

/// Maximum descriptors carried by one [`send_with_fds`] or [`recv_with_fds`] call.
// A subgroup can carry stdio/event4, CPU1, GNU2 and the original127 fences.
// This stays below Linux's253-right single-message kernel limit.
pub const MAX_FDS: usize = 134;

/// Control-buffer descriptor capacity used by [`recv_with_fds`].
#[cfg(any(
    target_vendor = "apple",
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
))]
// XNU can install rights before truncating ancillary copyout. Keep room for
// its entire UIPC_MAX_CMSG_FD512 message, even when the caller accepts only4;
// otherwise undisclosed installed descriptors cannot be closed on refusal.
// Linux's253-right maximum also fits this storage. This is receive storage,
// never permission to accept more than the caller's exact descriptor count.
// XNU: bsd/kern/uipc_usrreq.c, bsd/kern/uipc_syscalls.c.
pub const CONTROL_FDS: usize = 512;

/// Borrowed descriptor type accepted by [`send_with_fds`].
#[cfg(unix)]
pub type BorrowedDescriptor<'a> = std::os::fd::BorrowedFd<'a>;
/// Owned descriptor type returned by [`recv_with_fds`].
#[cfg(unix)]
pub type OwnedDescriptor = std::os::fd::OwnedFd;

/// Windows counterpart of [`BorrowedDescriptor`].
#[cfg(windows)]
pub type BorrowedDescriptor<'a> = std::os::windows::io::BorrowedHandle<'a>;
/// Windows counterpart of [`OwnedDescriptor`].
#[cfg(windows)]
pub type OwnedDescriptor = std::os::windows::io::OwnedHandle;

/// Payload byte count and descriptors received by [`recv_with_fds`].
#[derive(Debug)]
pub struct Received {
    /// Payload bytes written into the caller's buffer.
    pub bytes: usize,
    /// Received descriptors with `FD_CLOEXEC` set.
    pub fds: Vec<OwnedDescriptor>,
}

/// Send `payload` and `fds` as one message on `stream`.
///
/// # Errors
/// * `InvalidInput`: an empty payload (a zero-length stream `sendmsg` drops
///   the control message on Linux), or more than [`MAX_FDS`] descriptors.
/// * Anything `sendmsg(2)` reports, or `Unsupported` where there is no `SCM_RIGHTS` transport.
pub fn send_with_fds(
    stream: &CtlStream,
    payload: &[u8],
    fds: &[BorrowedDescriptor<'_>],
) -> io::Result<usize> {
    scm::send_with_fds(stream, payload, fds)
}

/// Send `bufs` in order as one gather write on `stream`.
///
/// # Errors
/// Anything `sendmsg(2)` reports (`EPIPE` if the peer is gone).
///
/// Returns the underlying socket write error.
pub fn send_vectored(stream: &CtlStream, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
    scm::send_vectored(stream, bufs)
}

/// Receive one message and up to `max_fds` descriptors from `stream` into `buf`.
///
/// # Errors
/// * `InvalidInput`: an empty `buf` (indistinguishable from EOF), or
///   `max_fds` above [`MAX_FDS`].
/// * `InvalidData`: a truncated control message, or more than `max_fds` descriptors sent.
/// * Anything `recvmsg(2)` reports, or `Unsupported` where there is no `SCM_RIGHTS` transport.
pub fn recv_with_fds(stream: &CtlStream, buf: &mut [u8], max_fds: usize) -> io::Result<Received> {
    scm::recv_with_fds(stream, buf, max_fds)
}

/// Return the connected peer's PID as reported by the kernel.
///
/// # Errors
/// * Anything `getsockopt(2)` reports, notably `ENOTCONN`.
///
/// Returns an error if the platform credential query fails or is unsupported.
pub fn peer_pid(stream: &CtlStream) -> io::Result<u32> {
    scm::peer_pid(stream)
}

/// Return [`peer_pid`] for raw socket descriptor `fd` without taking ownership.
///
/// # Errors
/// As [`peer_pid`]'s, and `EBADF`/`ENOTSOCK` for a descriptor that names no socket.
#[cfg(unix)]
pub fn peer_pid_of_fd(fd: std::os::fd::RawFd) -> io::Result<u32> {
    scm::peer_pid_of_fd(fd)
}

/// The two ABIs this file mirrors field-for-field.
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

        /// Darwin `struct cmsghdr`.
        #[repr(C)]
        pub struct CmsgHdr {
            cmsg_len: u32,
            cmsg_level: i32,
            cmsg_type: i32,
        }

        pub const LEVEL_OFFSET: usize = 4;
        /// Byte offset of `cmsg_type`.
        pub const TYPE_OFFSET: usize = 8;

        /// Darwin `__DARWIN_ALIGN32` control-message alignment.
        pub const CMSG_ALIGNMENT: usize = size_of::<u32>();

        pub const SOL_SOCKET: i32 = 0xffff;
        pub const SCM_RIGHTS: i32 = 0x01;
        pub const MSG_CTRUNC: i32 = 0x20;
        /// `MSG_NOSIGNAL` send flag.
        pub const SEND_FLAGS: i32 = 0x80000;

        /// `sys/un.h`: level `SOL_LOCAL`, option `LOCAL_PEERPID`.
        const SOL_LOCAL: i32 = 0;
        const LOCAL_PEERPID: i32 = 0x002;

        pub const fn msghdr(
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

        pub const fn control_len(msg: &MsgHdr) -> usize {
            msg.msg_controllen as usize
        }

        pub const fn msg_flags(msg: &MsgHdr) -> i32 {
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

        /// Query peer PID via `LOCAL_PEERPID`.
        #[cfg_attr(trust_verify, trust::skip)]
        pub fn peer_pid(fd: RawFd) -> io::Result<u32> {
            let mut pid: i32 = 0;
            let mut len: u32 = 4;
            // SAFETY: `pid` and `len` are valid locals for `getsockopt`; `len` states the exact
            // size the kernel writes, checked again below.
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
    /// `msg_iovlen`/`msg_controllen` are `size_t`, `msg_namelen` is `socklen_t`.
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

        /// Linux `struct cmsghdr`.
        #[repr(C)]
        pub struct CmsgHdr {
            cmsg_len: usize,
            cmsg_level: i32,
            cmsg_type: i32,
        }

        pub const LEVEL_OFFSET: usize = 8;
        pub const TYPE_OFFSET: usize = 12;

        /// Linux `CMSG_ALIGN` control-message alignment.
        pub const CMSG_ALIGNMENT: usize = size_of::<usize>();

        pub const SOL_SOCKET: i32 = 1;
        pub const SCM_RIGHTS: i32 = 0x01;
        pub const MSG_CTRUNC: i32 = 0x08;
        /// `MSG_NOSIGNAL` send flag.
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

        /// Query peer PID via `SO_PEERCRED`.
        #[cfg_attr(trust_verify, trust::skip)]
        pub fn peer_pid(fd: RawFd) -> io::Result<u32> {
            let mut cred = [0i32; 3];
            let mut len: u32 = UCRED_LEN as u32;
            // SAFETY: `cred` and `len` are valid locals for `getsockopt`; `len` states the exact
            // size the kernel writes, checked again below.
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

    /// `fcntl(2)`'s descriptor-flag setter and the one flag there is.
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

    const _: () = {
        if align_of::<ControlBuf>() < align_of::<abi::CmsgHdr>() {
            panic!("the control buffer must be at least cmsghdr-aligned");
        }
    };

    /// Send `payload` and `fds` via `sendmsg`.
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
        // SAFETY: `msg` references `iov` and `control`, live across `sendmsg`; `iov` is exactly
        // `payload`'s bytes, and `control_len` fits `control` (`fds.len() <= MAX_FDS <=
        // CONTROL_FDS`).
        let sent = unsafe { sendmsg(stream.as_raw_fd(), &msg, abi::SEND_FLAGS) };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(sent as usize)
    }

    /// Send `bufs` via `sendmsg` with `MSG_NOSIGNAL`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn send_vectored(stream: &CtlStream, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let bufs = &bufs[..bufs.len().min(MAX_IOV)];
        let msg = abi::msghdr(
            bufs.as_ptr().cast::<IoVec>().cast_mut(),
            bufs.len(),
            core::ptr::null_mut(),
            0,
        );
        // SAFETY: `msg` names `bufs.len()` iovecs at `bufs`, live across `sendmsg` and laid out as
        // `struct iovec` (asserted in `scm`); it names no control buffer.
        let sent = unsafe { sendmsg(stream.as_raw_fd(), &msg, abi::SEND_FLAGS) };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(sent as usize)
    }

    /// Receive one message and its descriptors via `recvmsg`.
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
        // SAFETY: `msg` references `iov` and `control`, live across `recvmsg`; `iov` is exactly
        // `buf`'s bytes and the header claims exactly `CONTROL_LEN`, the control buffer's length.
        let got = unsafe { recvmsg(stream.as_raw_fd(), &mut msg, 0) };
        if got < 0 {
            return Err(io::Error::last_os_error());
        }

        let filled = abi::control_len(&msg).min(CONTROL_LEN);
        let scan = scan_control(&control.0[..filled]);

        let mut fds: Vec<OwnedFd> = Vec::with_capacity(scan.fds.len());
        let mut malformed = scan.truncated;
        for raw in scan.fds {
            if raw < 0 {
                malformed = true;
                continue;
            }
            // SAFETY: `raw` is a fresh descriptor `SCM_RIGHTS` just installed, owned by nothing
            // else; each number appears once in a message, so no two `OwnedFd`s alias.
            fds.push(unsafe { OwnedFd::from_raw_fd(raw) });
        }
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

    /// Query peer PID of `stream`.
    pub fn peer_pid(stream: &CtlStream) -> io::Result<u32> {
        abi::peer_pid(stream.as_raw_fd())
    }

    /// Query peer PID of `fd`.
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
    #[cfg_attr(trust_verify, trust::skip)]
    fn set_cloexec(fd: RawFd) -> io::Result<()> {
        // SAFETY: `fd` is an open descriptor owned by this process.
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
        /// header was malformed).
        truncated: bool,
    }

    /// Walk `control` (the bytes the kernel wrote) and collect every `SCM_RIGHTS` descriptor
    /// number in it.
    fn scan_control(control: &[u8]) -> ControlScan {
        let mut scan = ControlScan {
            fds: Vec::new(),
            truncated: false,
        };
        let mut off = 0usize;
        while off.saturating_add(CMSG_HDR_LEN) <= control.len() {
            let Some(len) = abi::read_cmsg_len(&control[off..]) else {
                break;
            };
            if len < CMSG_DATA_OFFSET {
                scan.truncated = true;
                break;
            }
            let level = read_i32(&control[off + abi::LEVEL_OFFSET..]);
            let kind = read_i32(&control[off + abi::TYPE_OFFSET..]);

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
                if !data.len().is_multiple_of(FD_LEN) {
                    scan.truncated = true;
                }
            }

            off = off.saturating_add(cmsg_align(len));
        }
        scan
    }

    /// Native-endian `i32` from the front of `bytes`, zero-padded if short.
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
    mod tests;

    /// A send to a hung-up peer with SIGPIPE at its default, in a helper
    /// process: the test process's own SIGPIPE is not ours to change.
    #[cfg(test)]
    mod sigpipe_tests;
}

/// Every other target: the same entry points, returning `Unsupported`, except
/// [`send_vectored`] and Windows' [`peer_pid`].
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

    /// Gather write via `WSASend`.
    #[cfg(windows)]
    pub fn send_vectored(stream: &CtlStream, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        io::Write::write_vectored(&mut &*stream, bufs)
    }

    /// Fallback write of the first non-empty slice.
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
