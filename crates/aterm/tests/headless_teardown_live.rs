// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance for an isolated instance's footprint: a headless instance given
//! its own state root writes nothing into the person's log directory, and a `kill` takes its
//! control socket with it.
//!
//! * The log follows the state root: with `$ATERM_STATE_HOME` set, `aterm.log` is
//!   `<state>/logs/aterm.log`, and the platform log directory under the instance's `$HOME` is
//!   never created. Negative control: without the override it logs under `$HOME`.
//! * SIGTERM removes the endpoint: the socket and its token are gone once the process has died
//!   of the signal (the exit status still names SIGTERM).
//!
//! Isolation is `support/launch_isolation.rs`'s, with a lifeline
//! (`launch_isolation::lifeline`): a runner killed mid-test takes its instance with it. The boot
//! is the shared one (`support/headless_boot.rs`): an instance that cannot start fails the test,
//! an environment refusal is COULD NOT RUN, and readiness is the instance's own word
//! (`--ready-fd`).

#![cfg(unix)]

use std::path::{Path, PathBuf};

use crate::headless_boot::{self, Instance};

/// Boot one headless instance, its log at `info`, with `state` (a path under
/// its scratch world) as its `$ATERM_STATE_HOME` when given.
fn boot(tag: &str, state: Option<&str>) -> Instance {
    headless_boot::boot_with(
        &format!("atht{tag}"),
        |tmp, cmd| {
            // (A development seam: this is a debug build.)
            cmd.env("ATERM_LOG", "info")
                .args(["--lines", "24", "--columns", "80"]);
            if let Some(state) = state {
                cmd.env("ATERM_STATE_HOME", tmp.join(state));
            }
        },
        |_| true,
    )
}

fn sock(inst: &Instance) -> PathBuf {
    PathBuf::from(&inst.sock)
}

/// Where `aterm.log` lands for this instance's `$HOME` with no override —
/// the platform rule `aterm_types::dirs::logs_dir` states.
fn home_log(tmp: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        tmp.join("home/Library/Logs/aterm/aterm.log")
    } else {
        tmp.join("state/aterm/logs/aterm.log")
    }
}

#[test]
fn an_isolated_state_root_keeps_the_log_and_the_default_does_not() {
    let isolated = boot("s", Some("own-state"));
    // The logger opens its file at startup, before the socket binds: once the
    // instance has said it listens, the file is there or it never will be.
    assert!(
        isolated.tmp.join("own-state/logs/aterm.log").exists(),
        "the instance's log is under its own state root"
    );
    assert!(
        !home_log(&isolated.tmp).exists() && !home_log(&isolated.tmp).parent().unwrap().exists(),
        "nothing is written to the person's log directory"
    );
    drop(isolated);
    // Negative control: no override, and the log is the platform's.
    let plain = boot("p", None);
    assert!(home_log(&plain.tmp).exists(), "the default log directory");
}

#[test]
fn sigterm_removes_the_socket_and_its_token() {
    use std::os::unix::process::ExitStatusExt;
    // Ready means the server recorded what it owns (`owned_endpoint::publish`
    // comes before its ready word), so the signal below finds the handler armed.
    let mut inst = boot("t", Some("own-state"));
    let sock = sock(&inst);
    let token = PathBuf::from(format!("{}.token", sock.display()));
    assert!(token.exists(), "the token sits beside the socket");
    let pid = libc::pid_t::try_from(inst.child.id()).unwrap();
    // SAFETY: a plain signal to the child this test spawned and still owns.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let status = inst.child.wait().expect("wait for the instance");
    assert_eq!(
        status.signal(),
        Some(libc::SIGTERM),
        "it still dies of the signal"
    );
    assert!(!sock.exists(), "the socket went with it");
    assert!(!token.exists(), "the token went with it");
}

/// A state root that is not an absolute path is refused (no log directory resolves), and the
/// instance says so on stderr, naming the variable, rather than silently writing no log.
#[test]
fn a_refused_state_root_is_said_not_silent() {
    // The words come first thing in startup, long before the ready word. The state root stays
    // the bare relative word (`boot` would anchor it).
    let inst = headless_boot::boot_with(
        "athtr",
        |_, cmd| {
            cmd.env("ATERM_STATE_HOME", "relative-state").args([
                "--lines",
                "24",
                "--columns",
                "80",
            ]);
        },
        |_| true,
    );
    let stderr = std::fs::read_to_string(&inst.log).unwrap_or_default();
    // The refusal names the variable.
    assert!(stderr.contains("ATERM_STATE_HOME"), "{stderr}");
    assert!(
        !home_log(&inst.tmp).exists(),
        "and no log went to HOME instead"
    );
}
