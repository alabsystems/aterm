// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Routing and deduplication for unasked `atpkg` diagnostic notices.

use std::collections::BTreeSet;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

static TO_HOST_LOG: AtomicBool = AtomicBool::new(false);
static SILENT: AtomicBool = AtomicBool::new(false);
static SPELLING_REPORTED: AtomicBool = AtomicBool::new(false);
static SAID: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Routes this process's unasked notices to [`aterm_update_core::log_pkg_note`] instead of stderr.
pub fn to_host_log() {
    TO_HOST_LOG.store(true, Ordering::Relaxed);
}

/// Silences all unasked notices in this process.
pub fn silence() {
    SILENT.store(true, Ordering::Relaxed);
}

/// Marks `[packages]` spelling notes as reported by the caller so [`say_spelling`] stays quiet.
pub fn spelling_reported_here() {
    SPELLING_REPORTED.store(true, Ordering::Relaxed);
}

/// Emits `msg` (prefixed with `atpkg: ` on stderr) to this process's configured notice sink.
pub(crate) fn say(msg: &str) {
    if SILENT.load(Ordering::Relaxed) {
        return;
    }
    crate::meter::around(|| {
        route(
            msg,
            TO_HOST_LOG.load(Ordering::Relaxed),
            &mut std::io::stderr().lock(),
            aterm_update_core::log_pkg_note,
        );
    });
}

/// Calls [`say`] at most once per process for each distinct `msg`.
pub(crate) fn say_once(msg: &str) {
    if first_time(&SAID, msg) {
        say(msg);
    }
}

/// Emits a `[packages]` spelling note once unless [`spelling_reported_here`] was called.
pub(crate) fn say_spelling(msg: &str) {
    if !SPELLING_REPORTED.load(Ordering::Relaxed) {
        say_once(msg);
    }
}

fn first_time(said: &Mutex<BTreeSet<String>>, msg: &str) -> bool {
    said.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(msg.to_owned())
}

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

    #[test]
    fn a_notice_is_new_once_per_text() {
        let said = Mutex::new(BTreeSet::new());
        assert!(first_time(&said, "[packages] channel is retired"));
        assert!(!first_time(&said, "[packages] channel is retired"));
        assert!(!first_time(&said, "[packages] channel is retired"));
        assert!(first_time(&said, "[packages] include is retired"));
    }

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
