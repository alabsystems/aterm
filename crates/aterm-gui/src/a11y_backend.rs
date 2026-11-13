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

/// Reports a dead accessibility publisher (its panic's `payload` text) to stderr and the message
/// inbox, which name its reason, and to the log, which names the panic's kind and length: the
/// reason can quote the accessibility text, a session's screen (DESIGN §3.6).
pub(crate) fn report_failure(payload: &str, location: &str) {
    let msg = crate::message_reporters::a11y_publisher_dead(
        reason_from_payload(payload),
        at_client_seen(),
    );
    for line in notice_lines(&msg) {
        crate::logging::stderr_line!("aterm-gui: {line}");
    }
    crate::message_inbox::queue_message(msg);
    aterm_log::error!(
        "accessibility publisher failed at {location}: {}",
        aterm_log::words::Panic::of_text(payload)
    );
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
#[path = "a11y_backend_tests.rs"]
mod tests;
