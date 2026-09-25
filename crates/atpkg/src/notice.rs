// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Where atpkg's UNASKED notices go — the lines a library call says on its own (a
//! `[packages]` or `[machine]` table it could not read, a configured prefix it will not use,
//! a lay that could not stay provenance-clean), never what a verb prints as its answer.
//!
//! In atpkg's own CLI they are stderr lines: a verb the person typed shows them, and a pass
//! the window spawns has its stderr kept in `aterm.log`. The two HOSTS that call this
//! library beside a live shell — the terminal session and the window — call
//! [`to_host_log`] once, before their first atpkg call, and from then on the notices are
//! records in their log: nothing prints into a shell the user did not ask to update (Phase 2
//! of `docs/DESIGN-atpkg-vendor-direct-updates-2026-09-22.md`).

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

/// Route this process's unasked notices to the host's log (`aterm_log`, through
/// [`aterm_update_core::log_pkg_note`]) instead of stderr. For a host process only; there is
/// no way back, because a host never becomes a verb.
pub fn to_host_log() {
    TO_HOST_LOG.store(true, Ordering::Relaxed);
}

/// Whether this process is a host whose notices are log records ([`to_host_log`]).
pub(crate) fn hosted() -> bool {
    TO_HOST_LOG.load(Ordering::Relaxed)
}

/// Say no unasked notice in this process. For the `__reroute` hidden verb
/// ([`crate::reroute`]): it runs before every `cargo`, `rustc` and `rustfmt` typed in a
/// session, and a line about `aterm.toml` there is said on every build — the doctor and
/// Settings name the file's troubles, never the tool a person ran.
pub fn silence() {
    SILENT.store(true, Ordering::Relaxed);
}

/// This process prints the `[packages]` table's spelling notes as its own answer (the
/// doctor's `note` lines, [`crate::config::PackagesConfig::config_notes`]), so the
/// load-time copies of them ([`say_spelling`]) are not said beside it.
pub fn spelling_reported_here() {
    SPELLING_REPORTED.store(true, Ordering::Relaxed);
}

/// Say `msg` — without the `atpkg: ` prefix, which is added — wherever this process's
/// unasked notices go.
pub(crate) fn say(msg: &str) {
    if SILENT.load(Ordering::Relaxed) {
        return;
    }
    // Around the terminal meter's line ([`crate::meter::around`]), and taken in its
    // order — the meter's screen, then stderr — so the two never wait on each other.
    crate::meter::around(|| {
        route(
            msg,
            TO_HOST_LOG.load(Ordering::Relaxed),
            &mut std::io::stderr().lock(),
            aterm_update_core::log_pkg_note,
        );
    });
}

/// [`say`], ONCE per process per text. A notice about the config FILE is a fact about the
/// file, not about the read that found it: `aterm.toml` is read by every layout
/// resolution, and the window re-reads it for each status collection, so a notice said
/// per read was said three times by one `doctor` and once per pass by the window.
pub(crate) fn say_once(msg: &str) {
    if first_time(&SAID, msg) {
        say(msg);
    }
}

/// A `[packages]` SPELLING note — a retired key, a development setting this build drops, a
/// word it does not know — said once per process ([`say_once`]), and not at all in a
/// process that reports the spelling itself ([`spelling_reported_here`]).
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

    /// ONCE PER TEXT: the second and later sayings of one notice are dropped, a
    /// different text is still said, and each set is its own process's.
    #[test]
    fn a_notice_is_new_once_per_text() {
        let said = Mutex::new(BTreeSet::new());
        assert!(first_time(&said, "[packages] channel is retired"));
        assert!(!first_time(&said, "[packages] channel is retired"));
        assert!(!first_time(&said, "[packages] channel is retired"));
        assert!(first_time(&said, "[packages] include is retired"));
    }

    /// A HOST'S NOTICE NEVER REACHES STDERR, and a verb's never skips it: the route is the
    /// one switch, and the two sinks are exclusive. (Replaces a scan of `lay.rs`'s source
    /// for the word `eprintln`, which could not see where the sink itself wrote.)
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
