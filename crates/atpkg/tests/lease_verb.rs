// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE HOLDER VERB, run as a PROCESS (review, 2026-09-26).
//!
//! `aterm pkg lease <dir> -- <command…>` holds the build `<dir>` belongs to for exactly as
//! long as the command runs. The packers `exec` into it, so the pid their caller holds —
//! the one `kill`, `timeout` or a closed tab signals — is the HOLDER's. These drive the dev
//! `atpkg` binary with a temp `HOME` (its default prefix lives under it, so nothing of the
//! machine's store is read or written) and signal the holder alone, as such a caller does.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use atpkg::lease::{Holders, Subject};

/// A temp `HOME`, and the store build `store/trust/9192/bin` under its default prefix.
struct Fixture {
    root: PathBuf,
    home: PathBuf,
    prefix: PathBuf,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("atpkg-lease-verb-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let prefix = atpkg::store::default_prefix(&home);
        std::fs::create_dir_all(prefix.join("store/trust/9192/bin")).expect("fixture build");
        Self { root, home, prefix }
    }

    fn bin(&self) -> PathBuf {
        self.prefix.join("store/trust/9192/bin")
    }

    fn holders(&self) -> Holders {
        atpkg::lease::holders(&self.prefix, &Subject::build("trust", 9192).unwrap())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Wait for `path` to hold a pid — the command's own word that it runs (a backstop bound
/// only: the file is the event).
fn pid_in(path: &Path) -> libc::pid_t {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(pid) = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
        {
            return pid;
        }
        assert!(Instant::now() < deadline, "the command never started");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn alive(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only asks whether `pid` exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// A SIGTERM TO THE HOLDER ENDS THE COMMAND, NOT JUST THE HOLDER: it is passed on, the
/// holder waits for the command to end, lets the lease go, and leaves by the same signal.
/// Before, the holder died of it at once: the lease was gone and the command ran on
/// unleased, orphaned, while its caller read the run as over.
#[test]
fn a_sigterm_to_the_holder_ends_the_command_it_holds_for() {
    use std::os::unix::process::ExitStatusExt as _;
    let fx = Fixture::new("term");
    let pidfile = fx.root.join("child.pid");
    let mut holder = Command::new(env!("CARGO_BIN_EXE_atpkg"))
        .arg("lease")
        .arg(fx.bin())
        .args(["--who", "sigterm test", "--", "/bin/sh", "-c"])
        .arg("echo $$ >\"$1\"; while :; do sleep 0.1; done")
        .arg("sh")
        .arg(&pidfile)
        .env("HOME", &fx.home)
        .env("XDG_CONFIG_HOME", fx.root.join("config"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn dev atpkg");
    let child = pid_in(&pidfile);
    let held = fx.holders();
    assert!(
        matches!(&held, Holders::Held(who) if who.len() == 1 && who[0].starts_with("sigterm test")),
        "held while the command runs: {held:?}"
    );
    let holder_pid = libc::pid_t::try_from(holder.id()).unwrap();
    // SAFETY: signals the holder this test spawned, and only it.
    unsafe { libc::kill(holder_pid, libc::SIGTERM) };
    let status = holder.wait().expect("the holder's status");
    let still_running = alive(child);
    if still_running {
        // SAFETY: the orphan this test's holder left; never another process.
        unsafe { libc::kill(child, libc::SIGKILL) };
    }
    assert!(
        !still_running,
        "the command the holder held for was left running, unleased"
    );
    assert_eq!(
        status.signal(),
        Some(libc::SIGTERM),
        "the holder leaves by the signal its command died of: {status:?}"
    );
    assert_eq!(fx.holders(), Holders::Free, "the lease is let go");
}
