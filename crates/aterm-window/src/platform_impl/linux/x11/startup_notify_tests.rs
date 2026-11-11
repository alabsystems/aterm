// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/activation.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::*;

#[test]
fn the_remove_message_quotes_the_id_and_ends_in_nul() {
    assert_eq!(remove_message(""), b"remove: ID=\"\"\0");
    assert_eq!(
        remove_message("gnome-shell/aterm/4242-0-host_TIME5678"),
        b"remove: ID=\"gnome-shell/aterm/4242-0-host_TIME5678\"\0"
    );
    assert_eq!(remove_message("a\"b\\c d"), b"remove: ID=\"a\\\"b\\\\c d\"\0");
}

#[test]
fn a_startup_id_s_time_is_the_stamp_after_its_last_time() {
    assert_eq!(startup_time("gnome-shell/aterm/4242-0-host_TIME5678"), Some(5678));
    assert_eq!(startup_time("a_TIME1-b_TIME42"), Some(42));
    assert_eq!(startup_time("x_TIME77+junk"), Some(77));
    assert_eq!(startup_time("x_TIME4294967295"), Some(u32::MAX));
    for none in ["", "gnome-shell/aterm/4242-0-host", "x_TIME", "x_TIMEz9", "x_TIME4294967296"] {
        assert_eq!(startup_time(none), None, "{none:?}");
    }
}
