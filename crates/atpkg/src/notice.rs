// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Where atpkg's unasked notices go: lines a library call says on its own (a `[packages]` or
//! `[machine]` table it could not read, a configured prefix it will not use), never a verb's
//! answer.
//!
//! In the CLI they are stderr lines. The two hosts that call this library beside a live shell
//! (the terminal session and the window) call [`to_host_log`] before their first atpkg call,
//! so their notices become log records and nothing prints into a shell the user did not ask
//! to update (Phase 2 of `docs/DESIGN-atpkg-vendor-direct-updates-2026-09-22.md`).

use std::collections::BTreeSet;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

static TO_HOST_LOG: AtomicBool = AtomicBool::new(false);
/// This process says no unasked notice at all ([`silence`]).
static SILENT: AtomicBool = AtomicBool::new(false);
/// This process reports the config's spelling itself ([`spelling_reported_here`]).
static SPELLING_REPORTED: AtomicBool = AtomicBool::new(false);
/// Every [`say_once`] text this process has already said.
static SAID: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Route this process's unasked notices to the host's log
/// ([`aterm_update_core::log_pkg_note`]) instead of stderr. One-way: for a host process only.
pub fn to_host_log() {
    TO_HOST_LOG.store(true, Ordering::Relaxed);
}

/// Say no unasked notice in this process. For the `__reroute` hidden verb
/// ([`crate::reroute`]), which runs before every `cargo`, `rustc` and `rustfmt` in a session;
/// the doctor and Settings report `aterm.toml`'s troubles instead.
pub fn silence() {
    SILENT.store(true, Ordering::Relaxed);
}

/// This process prints the `[packages]` spelling notes as its own answer (the doctor's
/// `note` lines, [`crate::config::PackagesConfig::config_notes`]), so [`say_spelling`] stays
/// quiet.
pub fn spelling_reported_here() {
    SPELLING_REPORTED.store(true, Ordering::Relaxed);
}

/// Say `msg` — without the `atpkg: ` prefix, which is added — wherever this process's
/// unasked notices go.
pub(crate) fn say(msg: &str) {
    if SILENT.load(Ordering::Relaxed) {
        return;
    }
    // The meter's line cleared first, and its lock order kept (meter screen, then stderr),
    // so the two never wait on each other.
    crate::meter::around(|| {
        route(
            msg,
            TO_HOST_LOG.load(Ordering::Relaxed),
            &mut std::io::stderr().lock(),
            aterm_update_core::log_pkg_note,
        );
    });
}

/// [`say`], once per process per text. `aterm.toml` is read by every layout resolution and
/// each window status collection; a notice about the file is not repeated per read.
pub(crate) fn say_once(msg: &str) {
    if first_time(&SAID, msg) {
        say(msg);
    }
}

/// A `[packages]` spelling note (a retired key, a dropped development setting, an unknown
/// word): said once per process, and not at all where [`spelling_reported_here`] was called.
pub(crate) fn say_spelling(msg: &str) {
    if !SPELLING_REPORTED.load(Ordering::Relaxed) {
        say_once(msg);
    }
}

/// Whether `msg` is new to `said`, recording it. A poisoned set is still a set.
fn first_time(said: &Mutex<BTreeSet<String>>, msg: &str) -> bool {
    said.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(msg.to_owned())
}

/// [`say`] with its three inputs explicit: exactly one of `stderr` and `log` hears `msg`.
/// A stderr that cannot be written is not a reason to fail the caller.
fn route(msg: &str, to_log: bool, stderr: &mut dyn Write, log: fn(&str)) {
    if to_log {
        log(msg);
    } else {
        let _ = writeln!(stderr, "atpkg: {msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static HEARD: Mutex<Vec<String>> = Mutex::new(Vec::new());

    fn heard(msg: &str) {
        HEARD.lock().unwrap().push(msg.to_owned());
    }

    /// Repeats of one text are dropped; a different text is still new.
    #[test]
    fn a_notice_is_new_once_per_text() {
        let said = Mutex::new(BTreeSet::new());
        assert!(first_time(&said, "[packages] channel is retired"));
        assert!(!first_time(&said, "[packages] channel is retired"));
        assert!(!first_time(&said, "[packages] channel is retired"));
        assert!(first_time(&said, "[packages] include is retired"));
    }

    /// A host's notice goes only to the log, a verb's only to stderr.
    #[test]
    fn a_hosted_notice_is_a_log_record_and_a_verbs_is_a_stderr_line() {
        let mut stderr = Vec::new();
        route("the table could not be read", true, &mut stderr, heard);
        assert!(stderr.is_empty(), "a host's notice reached stderr");
        assert!(
            HEARD
                .lock()
                .unwrap()
                .iter()
                .any(|m| m == "the table could not be read"),
            "the host's log heard it"
        );

        let mut stderr = Vec::new();
        route("typed", false, &mut stderr, heard);
        assert_eq!(String::from_utf8(stderr).unwrap(), "atpkg: typed\n");
        assert!(
            !HEARD.lock().unwrap().iter().any(|m| m == "typed"),
            "a verb's notice is not logged twice"
        );
    }
}
