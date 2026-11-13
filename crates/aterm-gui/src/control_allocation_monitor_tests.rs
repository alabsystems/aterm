// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn only_exact_local_unselected_owner_can_receive_a_descriptor() {
    assert!(allowed(Scope::Owner, false, false));
    assert!(!allowed(Scope::Owner, true, false));
    assert!(!allowed(Scope::Owner, false, true));
    assert!(!allowed(Scope::Bridge, false, false));
    for content in [false, true] {
        for drive in [false, true] {
            assert!(!allowed(Scope::Fleet { content, drive }, false, false));
        }
    }
}
#[test]
fn handoff_requires_exact_marker_and_one_descriptor() {
    use std::os::fd::AsFd as _;
    for mode in 0..4 {
        let (send, recv) = aterm_uds::kept_out(CtlStream::pair).unwrap();
        recv.set_nonblocking(true).unwrap();
        let (capability, _server) = aterm_uds::kept_out(CtlStream::pair).unwrap();
        let data = if mode == 0 {
            vec![FD_MARKER]
        } else if mode == 1 {
            vec![b'?']
        } else {
            vec![FD_MARKER, 1]
        };
        let fds = if mode == 3 {
            Vec::new()
        } else {
            vec![capability.as_fd()]
        };
        aterm_uds::fdpass::send_with_fds(&send, &data, &fds).unwrap();
        assert_eq!(receive(&recv, Instant::now() + HANDOFF).is_ok(), mode == 0);
    }
}
