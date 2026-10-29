// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Routing and deduplication for unasked `atpkg` diagnostic notices.
//!
//! A notice has two forms. stderr (a verb the person typed) prints what a person reads; a host's
//! aterm.log keeps words, kinds and numbers, so a notice that quotes the person's aterm.toml (a
//! value, the line that did not parse) hands the log its own form ([`say_with_log`]: the value by
//! its length, the error by its position). A host's log hears a notice two ways: in process
//! ([`to_host_log`]), and through the stderr of a pass it runs and copies into its log
//! ([`STDERR_IS_HOST_LOG_ENV`]); both take the log form.

use std::collections::BTreeSet;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

static TO_HOST_LOG: AtomicBool = AtomicBool::new(false);
static STDERR_IS_HOST_LOG: AtomicBool = AtomicBool::new(false);
static SILENT: AtomicBool = AtomicBool::new(false);
static SPELLING_REPORTED: AtomicBool = AtomicBool::new(false);
static SAID: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Routes this process's unasked notices to [`aterm_update_core::log_pkg_note`] instead of stderr.
pub fn to_host_log() {
    TO_HOST_LOG.store(true, Ordering::Relaxed);
}

/// What a host sets on a pass it runs when it copies that pass's stderr into its own aterm.log
/// (the window's package lanes): the pass's notices then print their log form there.
pub const STDERR_IS_HOST_LOG_ENV: &str = "ATPKG_STDERR_IS_HOST_LOG";

/// This process's stderr is read into a host's aterm.log: its notices print their log form there.
pub fn stderr_is_host_log() {
    STDERR_IS_HOST_LOG.store(true, Ordering::Relaxed);
}

/// [`stderr_is_host_log`] when the host that spawned this process set
/// [`STDERR_IS_HOST_LOG_ENV`] to `1`.
pub fn stderr_form_from_env() {
    if std::env::var_os(STDERR_IS_HOST_LOG_ENV).is_some_and(|v| v == "1") {
        stderr_is_host_log();
    }
}

/// Where this process's notices go, and so which of a notice's two forms each takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sink {
    /// The host's own log, in process ([`to_host_log`]): the log form.
    HostLog,
    /// stderr a host copies into its log ([`stderr_is_host_log`]): the log form, on stderr.
    HostReadStderr,
    /// stderr a person reads: the said form.
    Stderr,
}

fn sink() -> Sink {
    if TO_HOST_LOG.load(Ordering::Relaxed) {
        Sink::HostLog
    } else if STDERR_IS_HOST_LOG.load(Ordering::Relaxed) {
        Sink::HostReadStderr
    } else {
        Sink::Stderr
    }
}

/// Silences all unasked notices in this process.
pub fn silence() {
    SILENT.store(true, Ordering::Relaxed);
}

/// Marks `[packages]` spelling notes as reported by the caller so [`say_spelling`] stays quiet.
pub fn spelling_reported_here() {
    SPELLING_REPORTED.store(true, Ordering::Relaxed);
}

/// Emits `msg` (prefixed with `atpkg: ` on stderr) to this process's configured notice sink: a
/// notice in aterm's own words, said and logged alike.
pub(crate) fn say(msg: &str) {
    say_with_log(msg, msg);
}

/// Emits a notice whose stderr form is `said` and whose host-log form is `log`.
pub(crate) fn say_with_log(said: &str, log: &str) {
    #[cfg(test)]
    if heard::capturing(said, log) {
        return;
    }
    if SILENT.load(Ordering::Relaxed) {
        return;
    }
    crate::meter::around(|| {
        route(
            said,
            log,
            sink(),
            &mut std::io::stderr().lock(),
            aterm_update_core::log_pkg_note,
        );
    });
}

/// Calls [`say_with_log`] at most once per process for each distinct `said`.
pub(crate) fn say_once_with_log(said: &str, log: &str) {
    if first_time(&SAID, said) {
        say_with_log(said, log);
    }
}

/// Emits a `[packages]` spelling note once ([`say_once_with_log`]) unless
/// [`spelling_reported_here`] was called.
pub(crate) fn say_spelling(msg: &str) {
    say_spelling_with_log(msg, msg);
}

/// [`say_spelling`] with a host-log form of its own ([`say_with_log`]).
pub(crate) fn say_spelling_with_log(said: &str, log: &str) {
    if !SPELLING_REPORTED.load(Ordering::Relaxed) {
        say_once_with_log(said, log);
    }
}

fn first_time(said: &Mutex<BTreeSet<String>>, msg: &str) -> bool {
    said.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(msg.to_owned())
}

/// The host's log hears `logged`, in process or on the stderr it copies; a person's stderr
/// prints `said`.
fn route(said: &str, logged: &str, sink: Sink, stderr: &mut dyn Write, log: fn(&str)) {
    match sink {
        Sink::HostLog => log(logged),
        Sink::HostReadStderr => {
            let _ = writeln!(stderr, "atpkg: {logged}");
        }
        Sink::Stderr => {
            let _ = writeln!(stderr, "atpkg: {said}");
        }
    }
}

/// A test's view of this thread's notices: what each sink would have been handed, whatever the
/// process's [`silence`], [`to_host_log`] and [`stderr_is_host_log`] say.
#[cfg(test)]
pub(crate) mod heard {
    use super::Sink;
    use std::cell::RefCell;

    /// The two sinks' records, one route at a time.
    #[derive(Debug, Default)]
    pub(crate) struct Heard {
        /// What the host's aterm.log was handed in process ([`Sink::HostLog`]).
        pub(crate) log: Vec<String>,
        /// What stderr printed ([`Sink::HostReadStderr`], [`Sink::Stderr`]).
        pub(crate) stderr: String,
    }

    thread_local! {
        static CAPTURE: RefCell<Option<(Sink, Heard)>> = const { RefCell::new(None) };
    }

    /// Every notice this thread says while `run` runs, routed to `sink`.
    pub(crate) fn while_running(sink: Sink, run: impl FnOnce()) -> Heard {
        CAPTURE.with(|c| *c.borrow_mut() = Some((sink, Heard::default())));
        run();
        CAPTURE
            .with(|c| c.borrow_mut().take())
            .map(|(_, heard)| heard)
            .unwrap_or_default()
    }

    fn logged(line: &str) {
        CAPTURE.with(|c| {
            if let Some((_, heard)) = c.borrow_mut().as_mut() {
                heard.log.push(line.to_owned());
            }
        });
    }

    /// Route `said`/`log` into the capture, when one is open on this thread.
    pub(crate) fn capturing(said: &str, log: &str) -> bool {
        let Some(sink) = CAPTURE.with(|c| c.borrow().as_ref().map(|(sink, _)| *sink)) else {
            return false;
        };
        let mut stderr = Vec::new();
        super::route(said, log, sink, &mut stderr, logged);
        CAPTURE.with(|c| {
            if let Some((_, heard)) = c.borrow_mut().as_mut() {
                heard.stderr.push_str(&String::from_utf8_lossy(&stderr));
            }
        });
        true
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
        route(
            "the table could not be read (at its value)",
            "the table could not be read (6B)",
            Sink::HostLog,
            &mut stderr,
            heard,
        );
        assert!(stderr.is_empty(), "a host's notice reached stderr");
        let host = HEARD.lock().unwrap().clone();
        assert!(
            host.iter().any(|m| m == "the table could not be read (6B)"),
            "the host's log heard its log form: {host:?}"
        );
        assert!(
            !host.iter().any(|m| m.contains("at its value")),
            "the host's log heard the said form: {host:?}"
        );

        let mut stderr = Vec::new();
        route(
            "typed (at its value)",
            "typed (6B)",
            Sink::Stderr,
            &mut stderr,
            heard,
        );
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "atpkg: typed (at its value)\n",
            "a verb's notice prints what a person reads"
        );
        assert!(
            !HEARD.lock().unwrap().iter().any(|m| m.starts_with("typed")),
            "a verb's notice is not logged twice"
        );

        // A pass whose stderr the host copies into its aterm.log prints the log form there.
        let mut stderr = Vec::new();
        route(
            "copied (at its value)",
            "copied (6B)",
            Sink::HostReadStderr,
            &mut stderr,
            heard,
        );
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "atpkg: copied (6B)\n",
            "a stderr the host's log reads hears the log form"
        );
        assert!(
            !HEARD
                .lock()
                .unwrap()
                .iter()
                .any(|m| m.starts_with("copied")),
            "a copied stderr's notice is not logged in process as well"
        );
    }
}
