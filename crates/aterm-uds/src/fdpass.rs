// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! File-descriptor passing (`SCM_RIGHTS`) and peer PID credentials over `CtlStream`.

use std::io;
use std::io::IoSlice;

use crate::CtlStream;

/// Maximum descriptors carried by one [`send_with_fds`] or [`recv_with_fds`] call.
pub const MAX_FDS: usize = 64;

/// Control-buffer descriptor capacity used by [`recv_with_fds`].
#[cfg(any(
    target_vendor = "apple",
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
))]
pub const CONTROL_FDS: usize = 2 * MAX_FDS;

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
///
/// Returns `InvalidInput` if `fds` is non-empty and `payload` is empty or `fds.len() > MAX_FDS`.
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
///
/// Returns `InvalidInput` if `max_fds > MAX_FDS` or `InvalidData` on control-message truncation.
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
///
/// Same as [`peer_pid`].
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
            // SAFETY: `pid` and `len` are valid locals for `getsockopt`.
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
            // SAFETY: `cred` and `len` are valid locals for `getsockopt`.
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
        // SAFETY: `msg` references `iov` and `control`, which remain live across `sendmsg`.
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
        // SAFETY: `msg` references `bufs`, which remains live across `sendmsg`.
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
        // SAFETY: `msg` references `iov` and `control`, which remain live across `recvmsg`.
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
            // SAFETY: `raw` is a fresh descriptor delivered by `SCM_RIGHTS`.
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
        /// here.
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
                        // SAFETY: `raw` is borrowed for a single `fstat` and not closed.
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
            // SAFETY: `msg` references live `iov` and `control` buffers.
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
        /// message), and Darwin fails the `recvmsg` (`EMFILE`).
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
            // SAFETY: child only runs async-signal-safe syscalls before `_exit`.
            let child = unsafe { libc::fork() };
            if child == 0 {
                let receive = || {
                    let fd = b.as_raw_fd();
                    // SAFETY: `dup2` and `close` on valid descriptors in the child process.
                    unsafe {
                        alarm(60);
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
                // SAFETY: child process exits immediately via `_exit`.
                unsafe { libc::_exit(receive()) }
            }
            assert!(child > 0, "fork: {}", std::io::Error::last_os_error());
            let mut status = 0;
            // SAFETY: reaps the child process spawned above.
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
        /// `EPIPE` and the sender lives.
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
            // SAFETY: restores `SIG_DFL` for `SIGPIPE`.
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
