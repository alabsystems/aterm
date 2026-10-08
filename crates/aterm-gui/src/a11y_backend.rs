// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reporting when the OS accessibility publisher stops serving its tree.
//!
//! On Linux the AccessKit tree is the only route to AT-SPI and so to a screen
//! reader. AccessKit's Unix backend runs that route on one process-global
//! thread whose body is `run_event_loop(..).await.unwrap()`, so any error on the
//! way to the accessibility bus (bus absent, a stale `org.a11y.Bus.GetAddress`
//! socket, a refused handshake) unwinds the thread for the life of the process.
//!
//! That failure is otherwise invisible: a spawned thread's panic does not end
//! the process, and the channel's send half outlives the thread, so every later
//! tree update is discarded while aterm looks like it is publishing.
//! `accesskit_unix` keeps its channel and app context in `OnceLock`s, so only a
//! restart can retry; this module makes the failure visible in the window.

use std::sync::atomic::{AtomicBool, Ordering};

/// Crate names of the AccessKit backend that owns the AT-SPI route. A panic
/// inside one of these is a publisher failure, not an aterm crash. Matched by
/// crate rather than file so version bumps and vendored copies still match;
/// `accesskit_atspi_common` is the other half of the same publisher thread.
const BACKEND_CRATES: [&str; 2] = ["accesskit_unix", "accesskit_atspi_common"];

/// The `Result::unwrap` prefix Rust puts on a panic payload, stripped so the
/// shown text starts at the real fault (`Handshake("Server GUID mismatch: …")`).
const UNWRAP_PREFIX: &str = "called `Result::unwrap()` on an `Err` value: ";

/// Whether a panic belongs to the accessibility publisher's own background thread.
///
/// Both checks are needed. The location separates the publisher from every
/// other panic, including aterm's own `accesskit_tree` projection, whose bugs
/// must stay crashes. The thread matters because the same backend code on the
/// main thread takes the terminal down and must still file a crash report.
pub(crate) fn is_backend_panic(location_file: &str, thread_name: Option<&str>) -> bool {
    if thread_name == Some("main") {
        return false;
    }
    // Registry checkouts carry the version in the directory name
    // (`accesskit_unix-0.22.0`); a vendored or path copy does not. Segment
    // equality plus the `-` suffix accepts both and rejects longer names.
    location_file.split(['/', '\\']).any(|segment| {
        BACKEND_CRATES.iter().any(|krate| {
            segment == *krate
                || segment
                    .strip_prefix(krate)
                    .is_some_and(|rest| rest.starts_with('-'))
        })
    })
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
/// inbox and the file logger are never used by AccessKit threads, and both
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
    fn a_panic_inside_the_accesskit_unix_backend_is_recognised_by_crate_not_by_file() {
        let registry = "/home/u/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/\
                        accesskit_unix-0.22.0/src/context.rs";
        assert!(is_backend_panic(registry, None));
        assert!(
            is_backend_panic("vendor/accesskit_unix/src/atspi/bus.rs", None),
            "a vendored copy carries no version suffix and must still be recognised"
        );
        assert!(
            is_backend_panic(
                "/r/accesskit_atspi_common-0.19.0/src/adapter.rs",
                Some("worker")
            ),
            "the other half of the same publisher fails the same way"
        );
    }

    #[test]
    fn aterms_own_accesskit_projection_is_not_the_backend() {
        assert!(
            !is_backend_panic("crates/aterm-gui/src/accesskit_tree.rs", None),
            "aterm's own mapping bugs are aterm's crashes"
        );
        assert!(
            !is_backend_panic("/r/accesskit_unix_helper-1.0.0/src/lib.rs", None),
            "a crate whose name merely starts with the backend's is a different crate"
        );
        assert!(!is_backend_panic("/r/zbus-5.0.0/src/connection.rs", None));
    }

    #[test]
    fn a_backend_panic_on_the_main_thread_is_still_a_crash() {
        let file = "/r/accesskit_unix-0.22.0/src/context.rs";
        assert!(
            !is_backend_panic(file, Some("main")),
            "the main thread unwinding takes the process with it; that is a crash"
        );
        assert!(is_backend_panic(file, None));
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
    fn the_notice_names_the_loss_before_the_jargon_and_offers_the_only_retry() {
        let msg = crate::message_reporters::a11y_publisher_dead(
            "Handshake(\"Server GUID mismatch\")",
            true,
        );
        let lines = notice_lines(&msg);
        // stderr says what the band says: the loss in words that need no
        // AT-SPI knowledge, then the only retry, then the reason.
        assert_eq!(
            lines,
            [
                "Screen reader access lost \u{2014} restart aterm to retry".to_string(),
                "Handshake(\"Server GUID mismatch\")".to_string(),
            ]
        );
        assert_eq!(
            lines[0],
            format!("{} \u{2014} {}", msg.title, msg.detail[0])
        );
    }
}
