// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Descriptor passing over a connected control socket — `SCM_RIGHTS` send /
//! receive, plus the kernel-attested peer pid — the out-of-band transport for
//! handing live descriptors to a process this one did NOT fork.
//!
//! The seamless handoff has always moved PTY masters by INHERITANCE: `fork`
//! copies the descriptor table verbatim and `execve` keeps it, so the child
//! finds each master at the number its parent named in `ATERM_SEAMLESS_FDS`.
//! That works only while the successor is our own child. A LaunchServices
//! launch — the point of which is to give the new instance its OWN launchd
//! application job, i.e. `ppid == 1` — inherits no descriptor table at all, so
//! the masters have to travel over the socket instead. This module is that
//! transport and nothing more: it moves descriptors and it names the peer. It
//! decides nothing about the handoff, and every error it returns is a REFUSAL
//! the caller must fall back from (fork lane, or rollback) rather than an
//! error it can paper over — a lost PTY master is a lost user session.
//!
//! Three properties were measured on this machine (macOS, Apple Silicon) with
//! a socketpair and three real `openpty` masters, and all three are load-
//! bearing here:
//!
//! * **The numbers change; the open file descriptions do not.** Received
//!   descriptors come back at DIFFERENT numbers with the SAME PTY device
//!   minors — the same open file descriptions. That is exactly why an
//!   adoption proof may not name a descriptor NUMBER once the masters move
//!   here (a number is a slot in one process's table, and `fork` was the only
//!   reason two processes ever agreed on one), and why `fstat`-derived
//!   identity survives.
//! * **`FD_CLOEXEC` is FALSE on arrival**, and Darwin has no
//!   `MSG_CMSG_CLOEXEC` to ask for it atomically. [`recv_with_fds`] therefore
//!   sets it on every descriptor the moment it owns one, before it validates
//!   anything else: an `exec` racing this call from another thread would
//!   otherwise leak the user's PTYs into an unrelated program.
//! * **`MSG_CTRUNC` is a descriptor LEAK, not a clean refusal.** A receiver
//!   that budgets control space for fewer descriptors than were sent still
//!   ends up with the extra descriptors installed in its table — the kernel
//!   installs them, then truncates the message that would have told us their
//!   numbers. So truncation is fatal here (never "take what fits"), every
//!   descriptor parsed out of the message is closed on that path, and the
//!   parse clamps to the control buffer this process actually supplied
//!   instead of trusting the `cmsg_len` the message carries. (Re-measured
//!   here while writing this: a 2-descriptor budget against a 3-descriptor
//!   send returns `MSG_CTRUNC` with two numbers visible, and closing those two
//!   still leaves the process one descriptor heavier. The kernel half of that
//!   is measured rather than asserted — reaching it needs a message wider than
//!   [`CONTROL_FDS`], which this surface will not send — but the parse half is
//!   pinned by `scan_control`'s unit tests.)
//!
//! The last point is also why the receive budgets control space for
//! [`CONTROL_FDS`] descriptors — double the [`MAX_FDS`] this surface will
//! send — regardless of the `max_fds` the caller declares. An over-send is
//! then a VISIBLE over-count that we close and refuse, instead of an
//! invisible leak we cannot even name. The declared `max_fds` is enforced in
//! user space, after the descriptors are safely owned.
//!
//! Ownership is the whole safety argument on the receive path: every parsed
//! descriptor becomes an [`OwnedDescriptor`] before the next fallible step, so
//! EVERY error return — truncation, over-count, a failed `fcntl` — closes
//! everything it parsed simply by dropping the vector. There is no path that
//! returns an error while a received descriptor is still open.
//!
//! Platforms: implemented for Darwin and for Linux on x86-64/aarch64 — the
//! two ABIs this file mirrors field-for-field below. Every other target
//! (Windows, and any Unix whose `msghdr`/socket-option numbering this file
//! does not vouch for) gets the same entry points returning
//! [`io::ErrorKind::Unsupported`]. Honest refusal, never a silent success:
//! there is no emulation of `SCM_RIGHTS`, and afunix on Windows has none.

use std::io;

use crate::CtlStream;

/// The most descriptors one [`send_with_fds`] / [`recv_with_fds`] carries.
///
/// A ceiling exists so the control buffer is a fixed stack array and an absurd
/// count is refused before the kernel is asked (Linux's own `SCM_MAX_FD` is
/// 253; Darwin bounds the control mbuf instead). Callers with more descriptors
/// than this chunk them across messages — the descriptors of one message all
/// arrive with its first byte, so chunking is a protocol detail, not a
/// correctness risk.
pub const MAX_FDS: usize = 64;

/// Descriptor slots the RECEIVE budgets control space for, whatever `max_fds`
/// the caller declares: double [`MAX_FDS`], because the alternative to
/// over-budgeting is the measured `MSG_CTRUNC` leak (see the module docs), and
/// half a kilobyte of stack is a cheap way to make a peer's over-send visible
/// enough to close. Beyond this the kernel truncates and the leak is
/// unavoidable — which is why truncation is fatal.
pub const CONTROL_FDS: usize = 2 * MAX_FDS;

/// The borrowed-descriptor type [`send_with_fds`] takes: `BorrowedFd` on Unix,
/// `BorrowedHandle` on Windows (where the operation is unsupported, but the
/// signature must still name something real).
#[cfg(unix)]
pub type BorrowedDescriptor<'a> = std::os::fd::BorrowedFd<'a>;
/// The owned-descriptor type [`recv_with_fds`] hands back — owned because the
/// receiver is the only thing that can close it, and dropping it on an error
/// path is what makes a refusal leak-free.
#[cfg(unix)]
pub type OwnedDescriptor = std::os::fd::OwnedFd;

/// Windows twin of the Unix alias (see the Unix definition); present so call
/// sites and this module's signatures compile there, never because afunix can
/// pass a handle over a socket.
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

/// Send `payload` and `fds` as ONE message on the connected `stream`,
/// returning the payload bytes the kernel accepted.
///
/// The descriptors ride the first byte of `payload`: they are delivered to
/// whichever `recvmsg` dequeues that byte, and they are gone from this call's
/// point of view whatever the return value says. A short return therefore
/// means "the rest of the payload still has to be written" (with an ordinary
/// `write`, carrying no descriptors) — never "send the descriptors again".
/// Keep `payload` small enough that a short send is not a live concern; the
/// handoff's is a fixed-size header.
///
/// An empty `fds` sends the payload with no control message at all, which is
/// exactly an ordinary `write`.
///
/// # Errors
/// * `InvalidInput` — an EMPTY payload (a zero-length stream `sendmsg` sends
///   nothing on Linux, so the control message, and with it the descriptors,
///   would be dropped on the floor), or more than [`MAX_FDS`] descriptors.
/// * `Unsupported` — this platform has no `SCM_RIGHTS` (see the module docs).
/// * Anything `sendmsg(2)` reports (`EPIPE` if the peer is gone — the sockets
///   std hands out are `SO_NOSIGPIPE`/`MSG_NOSIGNAL`, so this is an error, not
///   a signal).
pub fn send_with_fds(
    stream: &CtlStream,
    payload: &[u8],
    fds: &[BorrowedDescriptor<'_>],
) -> io::Result<usize> {
    scm::send_with_fds(stream, payload, fds)
}

/// Receive one message from `stream` into `buf`, accepting at most `max_fds`
/// descriptors with it.
///
/// `max_fds` is the caller's DECLARED maximum, not the receive budget: the
/// budget is always [`CONTROL_FDS`] so that a peer sending too many is a
/// closable over-count instead of a kernel-side leak (module docs). Every
/// returned descriptor is already `FD_CLOEXEC`.
///
/// Size `buf` for the whole message the peer sends. A short `buf` leaves the
/// remaining bytes queued (they are just stream bytes), but ALL of that
/// message's descriptors arrive on this call regardless.
///
/// # Errors
/// * `InvalidInput` — an empty `buf` (a zero-length receive cannot be
///   distinguished from EOF and cannot carry descriptors), or `max_fds` above
///   [`MAX_FDS`].
/// * `InvalidData` — the peer misbehaved: more descriptors than `max_fds`, or
///   a truncated control message. Both close every descriptor received before
///   returning; a truncation additionally means descriptors we were never told
///   the numbers of are stranded in this process, so the caller must treat the
///   connection as unusable and fall back rather than retry.
/// * `Interrupted` — a signal arrived before anything was dequeued; retrying
///   is safe precisely because nothing was.
/// * `Unsupported` — this platform has no `SCM_RIGHTS` (see the module docs).
pub fn recv_with_fds(stream: &CtlStream, buf: &mut [u8], max_fds: usize) -> io::Result<Received> {
    scm::recv_with_fds(stream, buf, max_fds)
}

/// The connected peer's pid as the KERNEL recorded it at `connect(2)` /
/// `socketpair(2)` — `LOCAL_PEERPID` on Darwin, `SO_PEERCRED` on Linux.
///
/// This is identity a peer cannot assert for itself, which is what makes it
/// worth having: the handoff's parent attestation otherwise rests on a
/// published birth record that a same-uid process could copy. Pair the two —
/// the peer pid says WHO dialed, the birth record says the pid was not
/// recycled between `connect` and this call — and neither gap is left open.
///
/// # Errors
/// * Anything `getsockopt(2)` reports, notably `ENOTCONN` on a socket that is
///   not connected (a listener, or a peer that vanished before the option was
///   read).
/// * `InvalidData` — the kernel answered with a pid that cannot name a process
///   (zero or negative); refused rather than returned, because a caller
///   comparing pids must never be handed a plausible-looking zero.
/// * `Unsupported` — this platform has no peer-pid primitive wired here. On
///   Windows that is the same reduced posture the control channel already
///   discloses at startup (afunix has no `SO_PEERCRED` analog); on a Unix this
///   file does not vouch for, it is a refusal to guess a socket-option number
///   (`SO_PEERCRED` is not 17 everywhere — sparc and mips renumber it, and
///   sparc renumbers `SOL_SOCKET` too, so a wrong guess would read some
///   unrelated option and answer confidently).
pub fn peer_pid(stream: &CtlStream) -> io::Result<u32> {
    scm::peer_pid(stream)
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

    /// Darwin's `sys/socket.h`. `msg_iovlen`/`msg_flags` are `int` and
    /// `msg_namelen`/`msg_controllen` are `socklen_t` (`u32`) — the ONE place
    /// the two supported ABIs disagree, which is why the structs are spelled
    /// out per platform instead of parameterized.
    #[cfg(target_vendor = "apple")]
    mod abi {
        use std::io;
        use std::os::fd::RawFd;

        /// `struct msghdr`.
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

        /// `struct cmsghdr` — `socklen_t cmsg_len; int cmsg_level; int
        /// cmsg_type;`, i.e. 12 bytes, which `CMSG_DATA` then rounds up to 16.
        /// (Linux's is 16 with no padding; the DATA OFFSET agrees at 16 on
        /// both, but only because Darwin pads — never fold the two.)
        #[repr(C)]
        pub struct CmsgHdr {
            cmsg_len: u32,
            cmsg_level: i32,
            cmsg_type: i32,
        }

        /// Byte offsets of `cmsg_level` / `cmsg_type` inside [`CmsgHdr`].
        pub const LEVEL_OFFSET: usize = 4;
        /// See [`LEVEL_OFFSET`].
        pub const TYPE_OFFSET: usize = 8;

        /// Darwin's cmsg macros round with `__DARWIN_ALIGN32` — `sizeof(u32)`,
        /// **not** `__DARWIN_ALIGN`/`sizeof(size_t)` like Linux's `CMSG_ALIGN`
        /// (`sys/socket.h`, `arm/_param.h`). So the data area starts at 12,
        /// immediately after the header, with NO padding — and assuming
        /// otherwise is not a near miss. Measured: with a 16-byte offset the
        /// kernel reads the 4 padding bytes as a descriptor number of their
        /// own, so a send of three PTY masters silently transfers FOUR
        /// descriptors, the extra one a dup of fd 0, which the receiver then
        /// leaks because it skipped those same 4 bytes.
        pub const CMSG_ALIGNMENT: usize = size_of::<u32>();

        pub const SOL_SOCKET: i32 = 0xffff;
        pub const SCM_RIGHTS: i32 = 0x01;
        pub const MSG_CTRUNC: i32 = 0x20;
        /// Darwin has no `MSG_NOSIGNAL`; std sets `SO_NOSIGPIPE` on every
        /// socket it creates instead, so a send to a dead peer is `EPIPE`.
        pub const SEND_FLAGS: i32 = 0;

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
                // A NULL control pointer is the documented "no ancillary
                // data" spelling; a non-null pointer with length 0 is not.
                msg_control: if control_len == 0 {
                    core::ptr::null_mut()
                } else {
                    control.cast()
                },
                msg_controllen: control_len as u32,
                msg_flags: 0,
            }
        }

        /// Control bytes the kernel actually wrote (NOT what we offered).
        pub fn control_len(msg: &MsgHdr) -> usize {
            msg.msg_controllen as usize
        }

        /// Flags the kernel set on the received message (`MSG_CTRUNC`).
        pub fn msg_flags(msg: &MsgHdr) -> i32 {
            msg.msg_flags
        }

        /// Read a `cmsg_len` out of the front of `bytes`, or `None` when the
        /// buffer cannot even hold the field.
        pub fn read_cmsg_len(bytes: &[u8]) -> Option<usize> {
            let raw: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
            Some(u32::from_ne_bytes(raw) as usize)
        }

        /// Write a `cmsg_len` at the front of `bytes` (a no-op on a buffer too
        /// short to hold one — callers always pass the whole control buffer).
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
            // SAFETY: `pid`/`len` are live locals for the duration of the
            // call; `len` states the exact size of the `pid_t` the kernel is
            // asked to write, and is re-read below rather than assumed.
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

    /// Linux's `bits/socket.h` on x86-64/aarch64: `msg_iovlen`/
    /// `msg_controllen` are `size_t`, `msg_namelen` is `socklen_t`. musl
    /// declares those two as `int` plus explicit padding, which is the SAME
    /// bytes on a 64-bit little-endian target and a different order on a
    /// big-endian one — hence the narrow `cfg` on this module rather than a
    /// blanket `target_os = "linux"`.
    #[cfg(not(target_vendor = "apple"))]
    mod abi {
        use std::io;
        use std::os::fd::RawFd;

        /// `struct msghdr`.
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

        /// `struct cmsghdr` — `size_t cmsg_len; int cmsg_level; int
        /// cmsg_type;`, i.e. 16 bytes with no tail padding (Darwin's is 12
        /// plus 4 of padding; see the Darwin twin).
        #[repr(C)]
        pub struct CmsgHdr {
            cmsg_len: usize,
            cmsg_level: i32,
            cmsg_type: i32,
        }

        /// Byte offsets of `cmsg_level` / `cmsg_type` inside [`CmsgHdr`].
        pub const LEVEL_OFFSET: usize = 8;
        /// See [`LEVEL_OFFSET`].
        pub const TYPE_OFFSET: usize = 12;

        /// `CMSG_ALIGN` rounds to `sizeof(size_t)` here — 8, against Darwin's
        /// 4 (see the Darwin twin for what a wrong offset costs). The header
        /// is already 16 bytes, so the data area starts flush at 16 either
        /// way; the alignment still shows up in `CMSG_SPACE`.
        pub const CMSG_ALIGNMENT: usize = size_of::<usize>();

        pub const SOL_SOCKET: i32 = 1;
        pub const SCM_RIGHTS: i32 = 0x01;
        pub const MSG_CTRUNC: i32 = 0x08;
        /// `MSG_NOSIGNAL` — a send to a hung-up peer must be `EPIPE`, not a
        /// `SIGPIPE` that would kill the process mid-handoff. (std passes the
        /// same flag on its own Linux writes.)
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
                // A NULL control pointer is the documented "no ancillary
                // data" spelling; a non-null pointer with length 0 is not.
                msg_control: if control_len == 0 {
                    core::ptr::null_mut()
                } else {
                    control.cast()
                },
                msg_controllen: control_len,
                msg_flags: 0,
            }
        }

        /// Control bytes the kernel actually wrote (NOT what we offered).
        pub fn control_len(msg: &MsgHdr) -> usize {
            msg.msg_controllen
        }

        /// Flags the kernel set on the received message (`MSG_CTRUNC`).
        pub fn msg_flags(msg: &MsgHdr) -> i32 {
            msg.msg_flags
        }

        /// Read a `cmsg_len` out of the front of `bytes`, or `None` when the
        /// buffer cannot even hold the field.
        pub fn read_cmsg_len(bytes: &[u8]) -> Option<usize> {
            let raw: [u8; 8] = bytes.get(..8)?.try_into().ok()?;
            Some(usize::from_ne_bytes(raw))
        }

        /// Write a `cmsg_len` at the front of `bytes` (a no-op on a buffer too
        /// short to hold one — callers always pass the whole control buffer).
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
            // SAFETY: `cred`/`len` are live locals for the duration of the
            // call; `len` states the exact size of the `struct ucred` the
            // kernel is asked to write, and is re-read below rather than
            // assumed.
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
    /// `CMSG_DATA`'s offset from the header: `CMSG_ALIGN(sizeof(cmsghdr))` —
    /// 12 on Darwin, 16 on Linux, for two independent reasons (see each
    /// `abi::CMSG_ALIGNMENT`).
    const CMSG_DATA_OFFSET: usize = cmsg_align(CMSG_HDR_LEN);
    /// One passed descriptor is one `int` on the wire.
    const FD_LEN: usize = size_of::<RawFd>();
    /// The fixed control-buffer size — see [`super::CONTROL_FDS`].
    const CONTROL_LEN: usize = cmsg_space(CONTROL_FDS * FD_LEN);

    const fn cmsg_align(len: usize) -> usize {
        (len + (abi::CMSG_ALIGNMENT - 1)) & !(abi::CMSG_ALIGNMENT - 1)
    }

    /// `CMSG_SPACE`: header + payload, each rounded up — how much BUFFER a
    /// message needs, which is what the receive offers the kernel.
    const fn cmsg_space(data_len: usize) -> usize {
        CMSG_DATA_OFFSET + cmsg_align(data_len)
    }

    /// `CMSG_LEN`: header + payload, payload NOT rounded up — what `cmsg_len`
    /// must say, and (see [`send_with_fds`]) what the SEND must also give as
    /// `msg_controllen`.
    const fn cmsg_len(data_len: usize) -> usize {
        CMSG_DATA_OFFSET + data_len
    }

    /// A control buffer with `cmsghdr` alignment. The kernel writes a
    /// `struct cmsghdr` at offset 0 and reads one back from there, so a
    /// byte array with weaker alignment is not a legal control buffer even
    /// though this file's own parse never assumes alignment.
    #[repr(C, align(8))]
    struct ControlBuf([u8; CONTROL_LEN]);

    impl ControlBuf {
        const ZEROED: Self = Self([0; CONTROL_LEN]);
    }

    // Not a preference: a control buffer the kernel cannot read a `cmsghdr`
    // out of is not a control buffer, so the `align(8)` above must dominate
    // the header's alignment on every ABI this file compiles for.
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
        // The buffer is sized by `CMSG_SPACE`, but the length we DECLARE is
        // `CMSG_LEN`: Darwin's `unp_internalize` refuses outright — EINVAL, no
        // descriptors sent, no hint as to why — unless the message's one
        // `cmsg_len` equals the control length it was handed, and only
        // `CMSG_LEN` is that length by definition. Linux accepts either (its
        // `CMSG_SPACE` is 4 bytes longer for an odd descriptor count; Darwin's
        // coincides, since it aligns cmsgs to 4 and a descriptor array is
        // already 4-aligned). Measured, in that order, by `tests/fdpass.rs`.
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

    /// Receive one message and its descriptors. See [`super::recv_with_fds`].
    // Skip: bottoms out at the `recvmsg(2)`/`fcntl(2)` FFI calls (unverifiable
    // C bodies). The cmsg walk it depends on is `scan_control`, which is pure
    // and stays in scope for the verifier and the unit tests below.
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

        // Parse only what the kernel says it wrote, and never more than we
        // supplied — the second clamp is the one that matters, because a
        // truncated message still carries the UNtruncated `cmsg_len`.
        let filled = abi::control_len(&msg).min(CONTROL_LEN);
        let scan = scan_control(&control.0[..filled]);

        // Take ownership BEFORE any fallible step: from here on every return
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
        // Immediately, before the validation below and before any error path:
        // Darwin has no MSG_CMSG_CLOEXEC, so this window is the only thing
        // between an inherited PTY master and an unrelated `exec` on another
        // thread.
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
        /// Raw descriptor numbers, in message order. They are NOT owned yet —
        /// the caller must take ownership immediately.
        fds: Vec<RawFd>,
        /// The buffer did not contain everything its headers claimed (or a
        /// header was malformed). Fatal: see the module docs.
        truncated: bool,
    }

    /// Walk `control` — the bytes the kernel actually wrote — and collect
    /// every `SCM_RIGHTS` descriptor number in it.
    ///
    /// Deliberately pure and total: no unsafe, no syscalls, every index
    /// clamped to `control`, so the one piece of this transport with real
    /// parsing logic is unit-testable against hand-built buffers (below) and
    /// visible to the verifier.
    ///
    /// The rule that earns its place: `cmsg_len` describes the message the
    /// KERNEL BUILT, not the bytes it managed to copy out. On a truncated
    /// receive it points past the end of our own buffer, so every read here
    /// clamps to `control.len()` and the overrun is reported as `truncated`
    /// instead of being trusted, sliced, or (in C) read straight off the end
    /// of the stack.
    ///
    /// One deliberate difference from `CMSG_NXTHDR`, which stops QUIETLY at a
    /// header shorter than a `cmsghdr` because a caller-supplied buffer may be
    /// padded with anything: `control` here is only ever the region the kernel
    /// said it wrote, where no such header is legitimate, so a short one is
    /// refused instead of read as end-of-list.
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

            // THE clamp.
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
    /// Total by construction: the callers above have already clamped, and a
    /// panicking reader inside a descriptor-owning function would leak.
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
            CMSG_DATA_OFFSET, CMSG_HDR_LEN, CONTROL_LEN, FD_LEN, abi, cmsg_len, cmsg_space,
            scan_control, write_i32,
        };

        /// Build one control buffer holding a single `SCM_RIGHTS` header whose
        /// `cmsg_len` claims `claimed` descriptors, followed by `present`
        /// descriptor numbers, cut to `keep` bytes — the shape a truncated
        /// receive leaves behind. `keep` is what `msg_controllen` would say,
        /// so it is the exact message size in the untruncated cases.
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

        /// The ABI constant this file got WRONG first: Darwin rounds cmsg
        /// offsets with `__DARWIN_ALIGN32` (4 bytes), so its data area starts
        /// flush against a 12-byte header, while Linux's `CMSG_ALIGN` rounds
        /// to 8 against a 16-byte header. Both land where the header says, and
        /// nothing about the failure is subtle-but-harmless: with 16 on Darwin
        /// the kernel reads the 4 bytes of padding as a descriptor of its own,
        /// so a send of three PTY masters transfers four descriptors — the
        /// extra one a dup of fd 0 — and the receiver, skipping the same 4
        /// bytes, never sees it to close it. That is precisely the leak
        /// `an_over_budget_receive_is_refused_without_leaking_descriptors`
        /// caught, and this is the assertion that names the cause.
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

        /// The ordinary case: three descriptors in, three numbers out, in
        /// order, nothing flagged.
        #[test]
        fn a_complete_message_yields_every_descriptor_in_order() {
            let buf = control(3, &[7, 8, 9], cmsg_space(3 * FD_LEN));
            let scan = scan_control(&buf);
            assert_eq!(scan.fds, [7, 8, 9]);
            assert!(!scan.truncated, "a complete message is not truncated");
        }

        /// THE parse rule. `cmsg_len` says three descriptors; the buffer the
        /// kernel copied out holds one. Trusting the length would read two
        /// descriptors' worth of whatever follows our buffer (in C, off the
        /// end of the stack; here, a panic) and report junk numbers as
        /// descriptors to close. The clamp reports exactly the one that is
        /// really there, and flags the message.
        #[test]
        fn a_cmsg_len_past_the_buffer_is_clamped_and_flagged() {
            let buf = control(3, &[7], CMSG_DATA_OFFSET + FD_LEN);
            let scan = scan_control(&buf);
            assert_eq!(scan.fds, [7], "only the descriptor really present");
            assert!(scan.truncated, "an overrunning cmsg_len is truncation");
        }

        /// A descriptor cut in half by the copy-out: the whole ones are
        /// reported (they are ours to close), and the message is still fatal
        /// because the half-copied one is installed and unnameable.
        #[test]
        fn a_partial_descriptor_at_the_tail_is_truncation() {
            let buf = control(3, &[7, 8], CMSG_DATA_OFFSET + FD_LEN + 2);
            let scan = scan_control(&buf);
            assert_eq!(scan.fds, [7]);
            assert!(scan.truncated);
        }

        /// A control message that is not `SCM_RIGHTS` carries no descriptors,
        /// and a header shorter than its own data area is junk, not a
        /// descriptor source. Neither may invent one.
        #[test]
        fn foreign_and_malformed_headers_yield_no_descriptors() {
            let mut foreign = control(2, &[7, 8], cmsg_space(2 * FD_LEN));
            write_i32(&mut foreign, abi::TYPE_OFFSET, abi::SCM_RIGHTS + 1);
            let scan = scan_control(&foreign);
            assert!(scan.fds.is_empty(), "not SCM_RIGHTS: no descriptors");
            assert!(!scan.truncated);

            let mut runt = control(2, &[7, 8], cmsg_space(2 * FD_LEN));
            abi::write_cmsg_len(&mut runt, CMSG_DATA_OFFSET - 1);
            let scan = scan_control(&runt);
            assert!(scan.fds.is_empty(), "malformed: no descriptors");
            assert!(scan.truncated, "malformed is refused, not ignored");
        }

        /// An empty control area (the common case — a message with no
        /// descriptors) is empty, not truncated: a buffer too short to hold a
        /// header at all is no message, not a broken one.
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
}

/// Every other target: the same entry points, refusing honestly.
///
/// There is no emulation to offer. Windows' afunix has no ancillary data at
/// all (handles move with `DuplicateHandle`, an entirely different model with
/// its own pid-and-permission story), and on a Unix this file does not vouch
/// for, guessing an ABI would be worse than refusing — a wrong `msghdr` layout
/// or option number does not fail loudly, it reads the wrong memory.
///
/// A refusal is safe by construction for the caller this exists for: the
/// seamless handoff falls back to the fork lane, which is what every platform
/// without this transport uses anyway.
#[cfg(not(any(
    target_vendor = "apple",
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
)))]
mod scm {
    use std::io;

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

    pub fn peer_pid(_stream: &CtlStream) -> io::Result<u32> {
        Err(unsupported("the peer pid"))
    }
}
