// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What aterm says when the OS accessibility publisher stops serving its tree.
//!
//! On Linux the AccessKit tree is the ONLY route to AT-SPI, and therefore the only
//! route to a screen reader (this crate's `Cargo.toml` states that as the reason the
//! stack ships unconditionally there). AccessKit's Unix backend runs the whole of
//! that route on ONE process-global thread — spawned the first time an adapter is
//! created — whose body is `run_event_loop(..).await.unwrap()`. Every error on the
//! way to the accessibility bus reaches that `unwrap`: the bus absent, the address
//! `org.a11y.Bus.GetAddress` advertises naming a socket a different daemon has since
//! bound, the handshake refused. The thread unwinds and is gone for the life of the
//! process.
//!
//! TWO PROPERTIES MAKE THAT INVISIBLE without this module, which is why it exists.
//! A panic on a spawned thread does not end the process, so aterm keeps running. And
//! the SEND half of that thread's channel outlives it, so every later tree update is
//! posted with a discarded `Err` — the publisher looks, from inside aterm, exactly
//! like one that is publishing while the AT-SPI registry lists no application at all.
//! A screen-reader user gets no terminal and no sign that anything went wrong.
//!
//! Nothing here can revive that thread: `accesskit_unix` holds its channel and its
//! app context in `OnceLock`s, so restarting the process is the only retry that
//! exists. So this module owns the other obligation — an accessibility surface that
//! has failed must SAY it has failed, in the window, where a person can act on it.

/// Crate names of the AccessKit backend that owns the AT-SPI route. A panic located
/// inside one of these is a failure OF the accessibility publisher, not a crash of
/// aterm, and it is matched by CRATE rather than by file so a version bump — or a
/// vendored copy under `vendor/` — keeps being recognised.
///
/// `accesskit_atspi_common` sits beside `accesskit_unix` because they are two halves
/// of one publisher running on that one thread; a panic in either loses the tree the
/// same way.
const BACKEND_CRATES: [&str; 2] = ["accesskit_unix", "accesskit_atspi_common"];

/// The `Result::unwrap` plumbing Rust prefixes to a panic payload. Stripped from what
/// the window shows, so the sentence starts at the part that names the real fault
/// (`Handshake("Server GUID mismatch: …")`) instead of at a combinator's name.
const UNWRAP_PREFIX: &str = "called `Result::unwrap()` on an `Err` value: ";

/// Whether a panic belongs to the accessibility publisher's own background thread.
///
/// Both halves are load-bearing. The LOCATION distinguishes the publisher from every
/// other panic in the process — including aterm's own `accesskit_tree` projection,
/// whose bugs are aterm's and must stay crashes. The THREAD is what makes the
/// distinction safe: the same backend code called on the main thread unwinds the
/// event loop and really does take the terminal down, and a process that is going
/// away must still file a crash report.
pub(crate) fn is_backend_panic(location_file: &str, thread_name: Option<&str>) -> bool {
    if thread_name == Some("main") {
        return false;
    }
    // Registry checkouts carry the version in the directory name
    // (`accesskit_unix-0.22.0`); a vendored or path copy does not. Segment equality
    // plus the `-` version suffix accepts both and refuses a longer name that merely
    // starts with the same letters.
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

/// The sentences the window shows, in reading order.
///
/// The LOSS comes first and in plain words, because it is the part that matters to
/// someone who does not know what AT-SPI is: this window is invisible to a screen
/// reader. The RETRY comes ahead of the reason because restarting really is the only
/// one — the publisher's `OnceLock`s mean a process gets one backend — and because
/// the notice band ellipsizes to the window width from the right, so whatever must
/// survive a narrow window has to be written early.
pub(crate) fn notice_lines(reason: &str) -> [String; 2] {
    [
        "accessibility OFF \u{2014} no screen reader can see this window".to_string(),
        format!("accessibility: restart aterm to retry \u{2014} {reason}"),
    ]
}

/// Report a dead accessibility publisher on every surface aterm has for it: the
/// window notice band a person can read, the log a support request can carry, and
/// the stderr a console launch keeps. Called FROM THE PANIC HOOK, so it must not
/// unwind and must not take a lock the panicking thread could already hold — the
/// notice lane and the file logger are both reached only from threads that have
/// nothing to do with AccessKit, and both degrade to a no-op when poisoned.
///
/// Deliberately NOT a substitute for the default hook: the caller still chains to it,
/// so the panic's own message and backtrace reach stderr exactly as before. This adds
/// the surface a windowed launch has; it takes none away.
pub(crate) fn report_failure(reason: &str, location: &str) {
    for line in notice_lines(reason) {
        eprintln!("aterm-gui: {line}");
        crate::config_notice::queue_deferred(line);
    }
    aterm_log::error!("accessibility publisher failed at {location}: {reason}");
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
        let lines = notice_lines("Handshake(\"Server GUID mismatch\")");
        assert!(
            lines[0].contains("no screen reader can see this window"),
            "the first line must state the loss in words that need no AT-SPI knowledge: {}",
            lines[0]
        );
        assert!(
            lines[1].starts_with("accessibility: restart aterm to retry"),
            "the only retry there is must precede the reason, which the band ellipsizes: {}",
            lines[1]
        );
        assert!(lines[1].contains("Server GUID mismatch"));
    }
}
