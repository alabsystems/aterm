// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/event_loop/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::*;

/// The display's errno is the exit code, else 1; only a dead transport is
/// the display's loss, and a protocol error stays one even when the
/// compositor hung up after sending it.
#[test]
fn only_a_dead_transport_is_the_display_s_loss() {
    use aterm_wayland::proxy::Error;
    let lost = |error, hung_up| {
        let (failure, code) = wire_failure(error, hung_up);
        (matches!(failure, EventLoopError::DisplayLost(_)), code)
    };
    for hung_up in [false, true] {
        assert_eq!(lost(Error::Display(libc::EPIPE), hung_up), (true, libc::EPIPE));
        assert_eq!(lost(Error::Display(libc::ECONNRESET), hung_up), (true, libc::ECONNRESET));
        assert_eq!(lost(Error::Display(libc::EPROTO), hung_up), (false, libc::EPROTO));
    }
    assert_eq!(lost(Error::Display(libc::ENOMEM), false), (false, libc::ENOMEM));
    assert_eq!(lost(Error::Display(-1), false), (false, 1));
    assert_eq!(lost(Error::NoObject, false), (false, 1));
    assert_eq!(lost(Error::Display(libc::ENOMEM), true), (true, libc::ENOMEM));
    assert_eq!(lost(Error::Display(-1), true), (true, 1));
    let (EventLoopError::DisplayLost(why), _) = wire_failure(Error::Display(libc::EPIPE), false)
    else {
        panic!("EPIPE is the display's loss");
    };
    assert_eq!(why.raw_os_error(), Some(libc::EPIPE));
}
