// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reporting when the OS accessibility publisher stops serving its tree.
//!
//! On Linux the AT-SPI publisher (`aterm_a11y::atspi`) is the only route to a
//! screen reader. It runs on one process-global thread. A bus that fails is
//! not this module's business: the publisher logs it, deactivates the
//! windows and rejoins when at-spi2-core comes back (accesskit_unix, which it
//! replaced, unwrapped any bus error into a panic). What is left is a panic
//! on that thread, in the publisher or in the AT-SPI semantics it answers
//! from, all of it aterm-a11y's since A11Y.5: the thread is gone for the life
//! of the process, and only a restart brings it back.
//!
//! That failure is otherwise invisible: a spawned thread's panic does not end
//! the process, and the windows' channel outlives the thread, so every later
//! tree update is discarded while aterm looks like it is publishing. This
//! module makes the failure visible in the window.

use std::sync::atomic::{AtomicBool, Ordering};

/// The crate whose code runs on the publisher thread: aterm's publisher and
/// the tree, text and AT-SPI semantics it answers from (crates/aterm-a11y). A
/// panic inside it on that thread is a publisher failure, not an aterm crash.
/// Matched by crate rather than file, so any of its files matches.
const BACKEND_CRATE: &str = "aterm-a11y";

/// The `Result::unwrap` prefix Rust puts on a panic payload, stripped so the
/// shown text starts at the real fault (`Handshake("Server GUID mismatch: …")`).
const UNWRAP_PREFIX: &str = "called `Result::unwrap()` on an `Err` value: ";

/// Whether a panic belongs to the accessibility publisher's own background thread.
///
/// Both checks are needed. The location separates the publisher from every
/// other panic, including aterm's own `a11y_tree` projection, whose bugs
/// must stay crashes. The thread matters because the same backend code on the
/// main thread takes the terminal down and must still file a crash report.
pub(crate) fn is_backend_panic(location_file: &str, thread_name: Option<&str>) -> bool {
    // A path segment equal to the crate's directory (`crates/aterm-a11y`):
    // a longer name (`aterm-a11y-extra`) is a different crate.
    thread_name != Some("main")
        && location_file
            .split(['/', '\\'])
            .any(|segment| segment == BACKEND_CRATE)
}

/// The part of a panic payload worth showing a person: the bus error itself, with
/// the `unwrap` plumbing in front of it removed.
pub(crate) fn reason_from_payload(payload: &str) -> &str {
    payload
        .strip_prefix(UNWRAP_PREFIX)
        .unwrap_or(payload)
        .trim()
}

/// The lines stderr prints: the band row's title and detail
/// ([`a11y_publisher_dead`]), so both surfaces read the same: the loss, the
/// only retry (a process gets one backend), then the reason.
///
/// [`a11y_publisher_dead`]: crate::message_reporters::a11y_publisher_dead
pub(crate) fn notice_lines(msg: &aterm_messages::Message) -> Vec<String> {
    let mut detail = msg.detail.iter();
    let head = match detail.next() {
        Some(first) => format!("{} \u{2014} {first}", msg.title),
        None => msg.title.clone(),
    };
    std::iter::once(head).chain(detail.cloned()).collect()
}

/// Report a dead accessibility publisher on the message band (one Standing
/// `a11y.publisher` row), the log and stderr. Called from the panic hook, so it
/// must not unwind or take a lock the panicking thread could hold; the pre-App
/// inbox and the file logger are never used by the publisher thread, and both
/// degrade to a no-op when poisoned.
///
/// The caller still chains to the default hook, so the panic message and
/// backtrace reach stderr unchanged.
pub(crate) fn report_failure(reason: &str, location: &str) {
    let msg = crate::message_reporters::a11y_publisher_dead(reason, at_client_seen());
    for line in notice_lines(&msg) {
        crate::logging::stderr_line!("aterm-gui: {line}");
    }
    crate::message_inbox::queue_message(msg);
    aterm_log::error!("accessibility publisher failed at {location}: {reason}");
}

/// Raised the first time an assistive-technology client asks any window of
/// this process for its tree (`InitialTreeRequested`, see
/// `App::on_accessibility_event`), and never lowered. An atomic, so the panic
/// hook reads it without a lock.
static AT_CLIENT_SEEN: AtomicBool = AtomicBool::new(false);

/// An AT client attached to this process's tree.
pub(crate) fn note_at_client() {
    AT_CLIENT_SEEN.store(true, Ordering::Release);
}

/// Whether an AT client has attached in this process's life: the dead
/// publisher's row stands on the glass only then, and is a record otherwise
/// (`message_reporters::a11y_publisher_dead`).
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
        // stderr says what the band says: title and first detail on one line,
        // the reason after it.
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            format!("{} \u{2014} {}", msg.title, msg.detail[0])
        );
        assert_eq!(lines[1], "Handshake(\"Server GUID mismatch\")");
    }
}
