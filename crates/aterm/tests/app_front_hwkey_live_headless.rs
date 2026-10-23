// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `hwkey` is answered by the App lane whatever the front tab shows (xa-20).

#![cfg(unix)]

use crate::headless_boot;

/// What a refused post says once the request reached `cmd_hwkey`.
fn reached_hwkey(reply: &str) -> bool {
    if cfg!(target_os = "macos") {
        reply.contains("ERR hwkey: no frontmost AppKit window to post into")
    } else {
        reply.contains("ERR hwkey: hardware-path injection is implemented on macOS only")
    }
}

fn said(inst: &headless_boot::Instance, args: &[&str]) -> String {
    headless_boot::said(&headless_boot::ctl(inst, args))
}

#[test]
fn hwkey_reaches_its_handler_with_a_native_view_in_front() {
    let inst = headless_boot::boot("athw", &[]);
    headless_boot::ctl_ok(&inst, &["open", "app", "settings", "/appearance"]);
    let tabs = headless_boot::ctl_ok(&inst, &["inspect", "app/v1", "tabs"]);
    assert!(
        tabs.lines()
            .any(|line| line.contains("kind=app") && line.contains("active=true")),
        "Settings is the front tab: {tabs}"
    );
    let down = said(&inst, &["hwkey", "down"]);
    assert!(
        !down.contains("no active terminal"),
        "an App verb is never refused for want of a terminal: {down}"
    );
    assert!(reached_hwkey(&down), "the post reached hwkey: {down}");
    let bad = said(&inst, &["hwkey", "notakey"]);
    assert!(
        bad.contains("hwkey") && !bad.contains("no active terminal"),
        "a malformed key is the hwkey parser's to refuse: {bad}"
    );
    assert!(!reached_hwkey(&bad), "a malformed key posts nothing: {bad}");

    headless_boot::ctl_ok(&inst, &["tab", "prev"]);
    let tabs = headless_boot::ctl_ok(&inst, &["inspect", "app/v1", "tabs"]);
    assert!(
        tabs.lines()
            .any(|line| line.contains("kind=terminal") && line.contains("active=true")),
        "the terminal is the front tab: {tabs}"
    );
    let unread = said(&inst, &["hwkey", "unread=ok", "down"]);
    assert!(
        reached_hwkey(&unread),
        "`unread=ok` lifts the gate, not the verb: {unread}"
    );
}
