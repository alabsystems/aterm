// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
