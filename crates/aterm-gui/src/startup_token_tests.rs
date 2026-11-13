// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// An opener started after a press carries the press's time as its startup id.
#[test]
fn an_opener_after_a_press_carries_its_time() {
    aterm_window::platform::x11::note_user_time_for_test(5_678_901);
    let opener = for_a_press(aterm_uds::spawnfd::command("xdg-open"));
    let id = opener
        .get_envs()
        .find(|(key, _)| *key == "DESKTOP_STARTUP_ID")
        .and_then(|(_, value)| value);
    assert_eq!(id, Some(std::ffi::OsStr::new("_TIME5678901")));
}

/// A launch token is one non-empty text value; an empty or non-UTF-8 one is none.
#[test]
fn a_token_is_a_non_empty_text_value() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;
    assert_eq!(
        token_value(Some(OsString::from(
            "gnome-shell/aterm/4242-0-host_TIME5678"
        ))),
        Some("gnome-shell/aterm/4242-0-host_TIME5678".to_string())
    );
    assert_eq!(token_value(Some(OsString::from(""))), None);
    assert_eq!(token_value(None), None);
    assert_eq!(
        token_value(Some(OsString::from_vec(b"id\xff".to_vec()))),
        None
    );
}
