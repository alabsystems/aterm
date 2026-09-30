// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless conformance for an ISOLATED instance's footprint (the
//! 2026-09-24 live probe's D9 and D10): a headless instance given its own
//! state root writes nothing into the person's log directory, and a `kill`
//! takes its control socket with it.
//!
//! * THE LOG FOLLOWS THE STATE ROOT: with `$ATERM_STATE_HOME` set, `aterm.log`
//!   is `<state>/logs/aterm.log`; the platform log directory under the
//!   instance's `$HOME` is never created. NEGATIVE CONTROL: the same launch
//!   without the override logs under `$HOME`, so the first assertion is about
//!   the override, not about a logger that wrote nothing.
//! * SIGTERM REMOVES THE ENDPOINT: the socket and its token are gone once the
//!   process has died of the signal (it still dies of it — the exit status
//!   names SIGTERM).
//!
//! ISOLATION: scratch HOME/XDG roots, a private socket, every automatic lane
//! off (`support/launch_isolation.rs`), `--no-reroute`, `SHELL=/bin/sh`. An
//! instance that cannot start FAILS the test (`support/headless_boot.rs`
//! `await_ready`); SKIP only on a scratch, log or spawn refusal.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

#[path = "support/launch_isolation.rs"]
mod launch_isolation;

#[path = "support/headless_boot.rs"]
mod headless_boot;

use headless_boot::{POLL_GAP, SOCKET_POLLS};

struct Instance {
    child: Child,
    tmp: PathBuf,
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

impl Instance {
    fn sock(&self) -> PathBuf {
        self.tmp.join("run/aterm/aterm.sock")
    }
}

/// A scratch world of its own for the launch tagged `tag`.
fn scratch_root(tag: &str) -> Option<PathBuf> {
    headless_boot::scratch_root(&format!("atht{tag}"))
}

/// One isolated headless launch, its log at `info`, not yet spawned.
fn headless(tmp: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, tmp);
    cmd.args(["--headless", launch_isolation::NO_REROUTE])
        .args(launch_isolation::control_sock(tmp))
        // (A development seam: this is a debug build.)
        .env("ATERM_LOG", "info")
        .args(["--lines", "24", "--columns", "80"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

/// Boot one headless instance with `state` as its `$ATERM_STATE_HOME` when
/// given. `None` = an environmental refusal (scratch, log, spawn), a SKIP; an
/// instance that exits or never listens PANICS (`headless_boot::await_ready`).
fn boot(tag: &str, state: Option<&str>) -> Option<Instance> {
    let Some(tmp) = scratch_root(tag) else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return None;
    };
    let log = tmp.join("gui.log");
    let (out, err) = match std::fs::File::create(&log).and_then(|f| Ok((f.try_clone()?, f))) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("SKIP: cannot open the instance log ({e})");
            let _ = std::fs::remove_dir_all(&tmp);
            return None;
        }
    };
    let mut cmd = headless(&tmp);
    cmd.stdout(out).stderr(err);
    if let Some(state) = state {
        cmd.env("ATERM_STATE_HOME", tmp.join(state));
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: cannot launch aterm --headless ({e})");
            let _ = std::fs::remove_dir_all(&tmp);
            return None;
        }
    };
    let mut inst = Instance { child, tmp };
    let sock = inst.sock();
    headless_boot::await_ready(&mut inst, |i| &mut i.child, &sock, &log, |_| true).then_some(inst)
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

/// The logger opens its file at startup, before the socket binds; allow it a
/// moment anyway so a slow machine does not read a race as a defect.
fn appears(path: &Path) -> bool {
    for _ in 0..50 {
        if path.exists() {
            return true;
        }
        std::thread::sleep(POLL_GAP);
    }
    false
}

/// Whether `log` says the control socket is listening — the line the server
/// logs only AFTER it recorded what it owns (`owned_endpoint::publish`), so a
/// signal sent after it finds the handler armed. The socket FILE appears
/// earlier, at bind.
fn listening(log: &Path) -> bool {
    for _ in 0..SOCKET_POLLS {
        if std::fs::read_to_string(log).is_ok_and(|l| l.contains("control socket listening at")) {
            return true;
        }
        std::thread::sleep(POLL_GAP);
    }
    false
}

#[test]
fn an_isolated_state_root_keeps_the_log_and_the_default_does_not() {
    let Some(isolated) = boot("s", Some("own-state")) else {
        return;
    };
    assert!(
        appears(&isolated.tmp.join("own-state/logs/aterm.log")),
        "the instance's log is under its own state root"
    );
    assert!(
        !home_log(&isolated.tmp).exists() && !home_log(&isolated.tmp).parent().unwrap().exists(),
        "nothing is written to the person's log directory"
    );
    drop(isolated);
    // NEGATIVE CONTROL: no override, and the log is the platform's.
    let Some(plain) = boot("p", None) else {
        return;
    };
    assert!(appears(&home_log(&plain.tmp)), "the default log directory");
}

#[test]
fn sigterm_removes_the_socket_and_its_token() {
    use std::os::unix::process::ExitStatusExt;
    let Some(mut inst) = boot("t", Some("own-state")) else {
        return;
    };
    let sock = inst.sock();
    let token = PathBuf::from(format!("{}.token", sock.display()));
    assert!(token.exists(), "the token sits beside the socket");
    assert!(
        listening(&inst.tmp.join("own-state/logs/aterm.log")),
        "the server says it is listening"
    );
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

/// A state root that is not an absolute path is refused — no log directory
/// resolves — and the instance SAYS so on stderr rather than silently writing
/// no log. NEGATIVE CONTROL: the words name the variable, which the message
/// before 2026-09-24 ("set HOME") did not.
#[test]
fn a_refused_state_root_is_said_not_silent() {
    use std::io::Read as _;
    let Some(tmp) = scratch_root("r") else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return;
    };
    let mut cmd = headless(&tmp);
    cmd.env("ATERM_STATE_HOME", "relative-state")
        .stderr(Stdio::piped());
    let Ok(child) = cmd.spawn() else {
        eprintln!("SKIP: cannot launch aterm --headless");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let mut inst = Instance { child, tmp };
    // The words come first thing in startup; the bound socket is well after.
    for _ in 0..SOCKET_POLLS {
        if headless_boot::is_socket_or_symlink(&inst.sock())
            || !matches!(inst.child.try_wait(), Ok(None))
        {
            break;
        }
        std::thread::sleep(POLL_GAP);
    }
    let _ = inst.child.kill();
    let _ = inst.child.wait();
    let mut stderr = String::new();
    let _ = inst
        .child
        .stderr
        .take()
        .expect("piped")
        .read_to_string(&mut stderr);
    // The refusal is SAID, naming the variable and that what depends on the
    // state root is off; the list of what is off grows with the product
    // (d034733c6 added session restore, the crash journal and the cell-metrics
    // cache), so the test pins the sentence's head and its verdict, not the list.
    assert!(
        stderr.contains("ATERM_STATE_HOME is not an absolute path; crash reports")
            && stderr.contains("aterm.log")
            && stderr.contains("are off"),
        "{stderr}"
    );
    assert!(
        !home_log(&inst.tmp).exists(),
        "and no log went to HOME instead"
    );
}
