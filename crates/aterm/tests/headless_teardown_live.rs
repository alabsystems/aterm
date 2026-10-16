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
//! (`launch_isolation::lifeline`): a runner killed mid-test takes its instance with it, which
//! the `Drop` below cannot do.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::{headless_boot, launch_isolation};

use headless_boot::{POLL_GAP, SOCKET_POLLS};

struct Instance {
    child: Child,
    /// Cut after `child` is killed (fields drop after `Drop::drop`), and closed
    /// by the kernel if this test process dies first.
    _lifeline: aterm_uds::lifeline::Lifeline,
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

/// One isolated headless launch, its log at `info`, not yet spawned, and the
/// lifeline it is armed with — held as long as the instance should run.
fn headless(tmp: &Path) -> (Command, aterm_uds::lifeline::Lifeline) {
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
    let lifeline = launch_isolation::lifeline(&mut cmd, tmp);
    (cmd, lifeline)
}

/// Boot one headless instance, with `state` as its `$ATERM_STATE_HOME` when given. `None` is an
/// environment refusal (scratch, log, spawn), a skip; an instance that exits or never listens
/// panics (`headless_boot::await_ready`).
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
    let (mut cmd, lifeline) = headless(&tmp);
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
    let mut inst = Instance {
        child,
        _lifeline: lifeline,
        tmp,
    };
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

/// The logger opens its file at startup, before the socket binds; it gets the boot's hang
/// detector anyway.
fn appears(path: &Path) -> bool {
    for _ in 0..SOCKET_POLLS {
        if path.exists() {
            return true;
        }
        std::thread::sleep(POLL_GAP);
    }
    false
}

/// Whether `log` says the control socket is listening, a line the server logs only after it
/// recorded what it owns (`owned_endpoint::publish`), so a signal sent after it finds the
/// handler armed. The socket file appears earlier, at bind.
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
    // Negative control: no override, and the log is the platform's.
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

/// A state root that is not an absolute path is refused (no log directory resolves), and the
/// instance says so on stderr, naming the variable, rather than silently writing no log.
#[test]
fn a_refused_state_root_is_said_not_silent() {
    use std::io::Read as _;
    let Some(tmp) = scratch_root("r") else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return;
    };
    let (mut cmd, lifeline) = headless(&tmp);
    cmd.env("ATERM_STATE_HOME", "relative-state")
        .stderr(Stdio::piped());
    let Ok(child) = cmd.spawn() else {
        eprintln!("SKIP: cannot launch aterm --headless");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let mut inst = Instance {
        child,
        _lifeline: lifeline,
        tmp,
    };
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
    // The refusal names the variable and says what depends on the state root is off; that
    // list grows with the product, so the test pins the sentence's head and its verdict.
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
