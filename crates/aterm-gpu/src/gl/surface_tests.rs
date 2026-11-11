// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_nvidia_s_composited_glx_posts_by_readback() {
    let little = cfg!(target_endian = "little");
    assert_eq!(readback_wanted("NVIDIA Corporation", true, None), little);
    assert!(
        !readback_wanted("NVIDIA Corporation", false, None),
        "no compositing manager"
    );
    assert!(!readback_wanted("SGI", true, None), "Mesa's GLX");
    assert!(readback_wanted("SGI", false, Some("readback")));
    assert!(!readback_wanted("NVIDIA Corporation", true, Some("window")));
    assert_eq!(
        readback_wanted("NVIDIA Corporation", true, Some("other")),
        little,
        "an unknown seam value is the default"
    );
}

#[test]
fn a_glx_window_s_refused_bind_latches_once_it_persists() {
    assert_eq!(BIND_FAILURES_LOST, 3);
    assert!(!bind_failure_is_loss(1, true, true), "a window in teardown");
    assert!(!bind_failure_is_loss(2, true, true));
    assert!(bind_failure_is_loss(3, true, true), "the failure persists");
    assert!(bind_failure_is_loss(4, true, true));
    assert!(
        bind_failure_is_loss(1, true, false),
        "the context binds nowhere"
    );
    assert!(
        bind_failure_is_loss(1, false, true),
        "the window never bound"
    );
    assert!(bind_failure_is_loss(1, false, false));
}
