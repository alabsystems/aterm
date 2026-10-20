// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `hwkey` is answered by the App lane whatever the front tab shows (xa-20).
//!
//! The verb posts a real NSEvent to the front window, so it needs no terminal
//! session; it is an App-target row of the verb table. The App lane had no arm
//! for it, so with a native view such as Settings in front the request fell
//! through to the session resolver and was refused as "no active terminal", a
//! refusal naming the wrong cause, before the hwkey parser ever ran; and with a
//! terminal in front, the session path handed it its tail with `unread=ok`
//! still on, so the documented override was refused as a usage error.
//!
//! A headless instance has no NSApplication, so a real post is refused there
//! ("no frontmost AppKit window"); that refusal is the proof the request
//! reached `cmd_hwkey` (off macOS, its "implemented on macOS only").
//!
//! ISOLATION: `support/launch_isolation.rs`; a headless instance opens no
//! window. An environment refusal is COULD NOT RUN, never a pass.

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

    // A terminal in front: the leading `unread=ok` is taken by the App lane's
    // gate, so the parser never sees it.
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
