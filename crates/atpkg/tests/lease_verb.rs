// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm pkg lease <dir> -- <command…>` holds the store build for the command's lifetime.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::{Command, Stdio};
use std::time::{Duration, Instant};

use atpkg::lease::{Holders, Subject};

struct Fixture {
    root: aterm_tempfile::TempDir,
    home: PathBuf,
    prefix: PathBuf,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-lease-verb-{case}-"))
            .tempdir()
            .unwrap();
        let home = root.path().join("home");
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

#[test]
fn a_sigterm_to_the_holder_ends_the_command_it_holds_for() {
    use std::os::unix::process::ExitStatusExt as _;
    let fx = Fixture::new("term");
    let pidfile = fx.root.path().join("child.pid");
    let mut holder = Command::new(env!("CARGO_BIN_EXE_atpkg"))
        .arg("lease")
        .arg(fx.bin())
        .args(["--who", "sigterm test", "--", "/bin/sh", "-c"])
        .arg("echo $$ >\"$1\"; while :; do sleep 0.1; done")
        .arg("sh")
        .arg(&pidfile)
        .env("HOME", &fx.home)
        .env("XDG_CONFIG_HOME", fx.root.path().join("config"))
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
