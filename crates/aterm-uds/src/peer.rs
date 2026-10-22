// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who is on the other end of a connected Unix-domain socket, as the kernel
//! recorded it: the peer's effective uid ([`peer_uid`]) and, on Darwin, its
//! audit token ([`peer_audit_token`]), which Security.framework checks code
//! identity by without trusting a possibly recycled pid.
//!
//! Both answer `None` on any failure; every caller treats that as a refusal.

use crate::CtlStream;

/// The connected peer's effective uid, or `None` when the kernel will not say (the peer
/// vanished, or no peer-credential primitive is wired here). Darwin: `getpeereid(2)`.
#[cfg(target_vendor = "apple")]
#[must_use]
// Skip: bottoms out at the `getpeereid(2)` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
pub fn peer_uid(stream: &CtlStream) -> Option<u32> {
    use std::os::fd::AsRawFd as _;
    unsafe extern "C" {
        fn getpeereid(fd: i32, uid: *mut u32, gid: *mut u32) -> i32;
    }
    let mut uid: u32 = 0;
    let mut gid: u32 = 0;
    // SAFETY: both out-parameters are live locals for the call.
    let rc = unsafe { getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    (rc == 0).then_some(uid)
}

/// Linux: `SO_PEERCRED` (`struct ucred { pid, uid, gid }`, three 32-bit
/// fields) on the x86-64/aarch64 ABIs whose option numbers this crate vouches
/// for (see `fdpass`).
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[must_use]
// Skip: bottoms out at the `getsockopt(2)` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
pub fn peer_uid(stream: &CtlStream) -> Option<u32> {
    use std::os::fd::AsRawFd as _;
    unsafe extern "C" {
        fn getsockopt(
            fd: i32,
            level: i32,
            name: i32,
            value: *mut core::ffi::c_void,
            len: *mut u32,
        ) -> i32;
    }
    const SOL_SOCKET: i32 = 1;
    const SO_PEERCRED: i32 = 17;
    let mut cred = [0u8; 12];
    let mut len: u32 = 12;
    // SAFETY: `cred`/`len` are live locals; `len` states the buffer's size.
    let rc = unsafe {
        getsockopt(
            stream.as_raw_fd(),
            SOL_SOCKET,
            SO_PEERCRED,
            cred.as_mut_ptr().cast(),
            &mut len,
        )
    };
    if rc != 0 || len != 12 {
        return None;
    }
    Some(u32::from_ne_bytes(cred.get(4..8)?.try_into().ok()?))
}

/// Everywhere else: no primitive wired, so the caller cannot verify and refuses.
#[cfg(not(any(
    target_vendor = "apple",
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
)))]
#[must_use]
pub fn peer_uid(_stream: &CtlStream) -> Option<u32> {
    None
}

/// This process's effective uid, the only uid the peer checks admit.
#[cfg(unix)]
#[must_use]
// Skip: bottoms out at the `geteuid` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
pub fn our_uid() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: a side-effect-free getter.
    unsafe { geteuid() }
}

/// A Darwin `audit_token_t`: eight 32-bit words naming a process (its ids, its pid and its pid
/// version, which changes when the pid is reused or its image replaced). Its one use is
/// Security.framework's guest lookup, which names a process by it rather than by a pid that
/// may have been recycled.
///
/// [`peer_audit_token`] answers for the process that last used the connection, as it is when
/// asked, not the one that `connect(2)`ed (measured 2026-10-03); an `exec` at that pid moves
/// the pid version. A caller that must know whom it trusted reads it once, checks that one,
/// and compares any later read with it (as `aterm-gui`'s handoff rendezvous does); it still
/// cannot tell from it who else holds the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuditToken(pub [u32; 8]);

impl AuditToken {
    /// The token's bytes in memory order, as `SecCodeCopyGuestWithAttributes`
    /// takes them inside a `CFData`.
    #[inline]
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.0) {
            *chunk = word.to_ne_bytes();
        }
        out
    }

    /// The pid the token names (`audit_token_to_pid`: word 5).
    #[inline]
    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.0[5]
    }

    /// The pid version the token names (`audit_token_to_pidversion`: word 7):
    /// the generation of the process at [`Self::pid`].
    #[inline]
    #[must_use]
    pub const fn pid_version(&self) -> u32 {
        self.0[7]
    }
}

/// The connected peer's audit token (`LOCAL_PEERTOKEN`) as of this call (see [`AuditToken`]).
/// Darwin only; elsewhere `None`, which a code-identity check reads as "cannot verify".
#[cfg(target_vendor = "apple")]
#[must_use]
// Skip: bottoms out at the `getsockopt(2)` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
pub fn peer_audit_token(stream: &CtlStream) -> Option<AuditToken> {
    use std::os::fd::AsRawFd as _;
    unsafe extern "C" {
        fn getsockopt(
            fd: i32,
            level: i32,
            name: i32,
            value: *mut core::ffi::c_void,
            len: *mut u32,
        ) -> i32;
    }
    // sys/un.h: SOL_LOCAL 0, LOCAL_PEERTOKEN 6, sizeof(audit_token_t) 32.
    const SOL_LOCAL: i32 = 0;
    const LOCAL_PEERTOKEN: i32 = 6;
    let mut raw = [0u8; 32];
    let mut len: u32 = 32;
    // SAFETY: `raw`/`len` are live locals; `len` states the buffer's size and
    // is re-read below rather than assumed.
    let rc = unsafe {
        getsockopt(
            stream.as_raw_fd(),
            SOL_LOCAL,
            LOCAL_PEERTOKEN,
            raw.as_mut_ptr().cast(),
            &mut len,
        )
    };
    if rc != 0 || len != 32 {
        return None;
    }
    let mut words = [0u32; 8];
    for (word, chunk) in words.iter_mut().zip(raw.as_chunks::<4>().0) {
        *word = u32::from_ne_bytes(*chunk);
    }
    Some(AuditToken(words))
}

/// See the Darwin twin.
#[cfg(not(target_vendor = "apple"))]
#[must_use]
pub fn peer_audit_token(_stream: &CtlStream) -> Option<AuditToken> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    /// On a pair this process made, the uid is ours and (Darwin) the token names our pid.
    #[test]
    fn a_socketpair_names_this_process() {
        let (a, _b) = crate::CtlStream::pair().expect("socketpair");
        #[cfg(any(
            target_vendor = "apple",
            all(
                target_os = "linux",
                any(target_arch = "x86_64", target_arch = "aarch64")
            )
        ))]
        assert_eq!(super::peer_uid(&a), Some(super::our_uid()));
        #[cfg(target_vendor = "apple")]
        {
            let token = super::peer_audit_token(&a).expect("LOCAL_PEERTOKEN answers");
            assert_eq!(token.pid(), std::process::id());
            assert_eq!(&token.to_bytes()[20..24], &std::process::id().to_ne_bytes());
            assert_eq!(
                &token.to_bytes()[28..32],
                &token.pid_version().to_ne_bytes()
            );
            // Nothing used the connection between the reads: the same answer.
            assert_eq!(super::peer_audit_token(&a), Some(token));
        }
        let _ = a;
    }

    /// A socket with no peer: the kernel refuses both questions, so neither answers, never
    /// the untouched out-parameter (a uid of 0 is root's). Darwin: Linux answers an
    /// unconnected socket's `SO_PEERCRED` with the overflow uid.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_socket_with_no_peer_names_nobody() {
        let lone = std::os::unix::net::UnixDatagram::unbound().expect("socket");
        let lone = crate::CtlStream::from(std::os::fd::OwnedFd::from(lone));
        assert_eq!(super::peer_uid(&lone), None);
        assert_eq!(super::peer_audit_token(&lone), None);
    }
}
