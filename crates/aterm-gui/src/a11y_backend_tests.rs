// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_backend_panic_is_recognised_by_crate_and_off_the_main_thread_only() {
    let semantics = "crates/aterm-a11y/src/atspi/semantics/node.rs";
    let cases: [(&str, &str, Option<&str>, bool); 7] = [
        (
            "the publisher's own file",
            "crates/aterm-a11y/src/atspi/publisher.rs",
            None,
            true,
        ),
        (
            "the semantics on the publisher thread",
            semantics,
            Some("aterm-atspi"),
            true,
        ),
        (
            "a Windows path",
            "C:\\b\\crates\\aterm-a11y\\src\\text.rs",
            None,
            true,
        ),
        ("main thread is a crash", semantics, Some("main"), false),
        (
            "aterm's own projection",
            "crates/aterm-gui/src/a11y_tree.rs",
            None,
            false,
        ),
        (
            "a hyphenated longer name",
            "crates/aterm-a11y-extra/src/lib.rs",
            None,
            false,
        ),
        (
            "an unrelated crate",
            "crates/aterm-dbus/src/conn.rs",
            None,
            false,
        ),
    ];
    for (name, file, thread, want) in cases {
        assert_eq!(is_backend_panic(file, thread), want, "{name}");
    }
}

#[test]
fn the_reason_drops_the_unwrap_plumbing_and_keeps_the_bus_error() {
    let payload = "called `Result::unwrap()` on an `Err` value: \
                   Handshake(\"Server GUID mismatch: expected a, got b\")";
    assert_eq!(
        reason_from_payload(payload),
        "Handshake(\"Server GUID mismatch: expected a, got b\")"
    );
    assert_eq!(reason_from_payload("bus is gone"), "bus is gone");
}

#[test]
fn the_notice_joins_the_rows_title_and_detail_then_the_reason() {
    let msg =
        crate::message_reporters::a11y_publisher_dead("Handshake(\"Server GUID mismatch\")", true);
    let lines = notice_lines(&msg);
    assert_eq!(lines.len(), 2);
    assert_eq!(
        lines[0],
        format!("{} \u{2014} {}", msg.title, msg.detail[0])
    );
    assert_eq!(lines[1], "Handshake(\"Server GUID mismatch\")");
}
