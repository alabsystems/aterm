// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reports failures when the background accessibility publisher thread panics.

use std::sync::atomic::{AtomicBool, Ordering};

const BACKEND_CRATE: &str = "aterm-a11y";

const UNWRAP_PREFIX: &str = "called `Result::unwrap()` on an `Err` value: ";

/// Returns whether a panic occurred in `aterm-a11y` off the main thread.
pub(crate) fn is_backend_panic(location_file: &str, thread_name: Option<&str>) -> bool {
    thread_name != Some("main")
        && location_file
            .split(['/', '\\'])
            .any(|segment| segment == BACKEND_CRATE)
}

/// Strips the standard `Result::unwrap` prefix from a panic payload.
pub(crate) fn reason_from_payload(payload: &str) -> &str {
    payload
        .strip_prefix(UNWRAP_PREFIX)
        .unwrap_or(payload)
        .trim()
}

/// Formats a dead-publisher [`aterm_messages::Message`] for stderr output.
pub(crate) fn notice_lines(msg: &aterm_messages::Message) -> Vec<String> {
    let mut detail = msg.detail.iter();
    let head = match detail.next() {
        Some(first) => format!("{} \u{2014} {first}", msg.title),
        None => msg.title.clone(),
    };
    std::iter::once(head).chain(detail.cloned()).collect()
}

/// Reports a dead accessibility publisher to stderr, the message inbox, and the log.
pub(crate) fn report_failure(reason: &str, location: &str) {
    let msg = crate::message_reporters::a11y_publisher_dead(reason, at_client_seen());
    for line in notice_lines(&msg) {
        crate::logging::stderr_line!("aterm-gui: {line}");
    }
    crate::message_inbox::queue_message(msg);
    aterm_log::error!("accessibility publisher failed at {location}: {reason}");
}

static AT_CLIENT_SEEN: AtomicBool = AtomicBool::new(false);

/// Records that an assistive-technology client attached to this process.
pub(crate) fn note_at_client() {
    AT_CLIENT_SEEN.store(true, Ordering::Release);
}

/// Returns whether an assistive-technology client has attached to this process.
pub(crate) fn at_client_seen() -> bool {
    AT_CLIENT_SEEN.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
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
    fn the_notice_joins_the_bands_title_and_detail_then_the_reason() {
        let msg = crate::message_reporters::a11y_publisher_dead(
            "Handshake(\"Server GUID mismatch\")",
            true,
        );
        let lines = notice_lines(&msg);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            format!("{} \u{2014} {}", msg.title, msg.detail[0])
        );
        assert_eq!(lines[1], "Handshake(\"Server GUID mismatch\")");
    }
}
