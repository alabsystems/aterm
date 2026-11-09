// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm pkg lease <dir> -- <command…>` holds the store build for the command's lifetime.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::{Command, Stdio};

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

/// The pid the command wrote at `path`, once it did, which it must: within the one-minute hang
/// detector.
fn pid_in(path: &Path) -> libc::pid_t {
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the command's start", || {
        std::fs::read_to_string(path).ok()?.trim().parse().ok()
    })
}

fn alive(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only asks whether `pid` exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// A holder, SIGKILLed when this drops while it still runs (a test that failed before its end, a
/// hang detector's included), and then the command it holds for, by the pid the command wrote at
/// `pidfile`: an orphan would run on unleased for good. A holder that ended was judged by its test,
/// which ends the command it left.
struct Holding {
    holder: aterm_uds::spawnfd::Child,
    pidfile: PathBuf,
}

impl Drop for Holding {
    fn drop(&mut self) {
        if !matches!(self.holder.try_wait(), Ok(None)) {
            return;
        }
        let _ = self.holder.kill();
        let _ = self.holder.wait();
        let command = std::fs::read_to_string(&self.pidfile)
            .ok()
            .and_then(|s| s.trim().parse::<libc::pid_t>().ok());
        if let Some(pid) = command.filter(|&pid| pid > 1 && alive(pid)) {
            // SAFETY: SIGKILL to the command this test's holder started, by the pid it wrote, which
            // its loop keeps alive; never another process.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

#[test]
fn a_sigterm_to_the_holder_ends_the_command_it_holds_for() {
    use std::os::unix::process::ExitStatusExt as _;
    let fx = Fixture::new("term");
    let pidfile = fx.root.path().join("child.pid");
    // The holder keeps an ignored SIGTERM ignored, so it starts with SIGTERM at its default, set
    // here, not this test binary's.
    let start = atpkg::platform::Starting::at_defaults(&[libc::SIGTERM]);
    let holder = start.spawning(|| {
        Command::new(aterm_tempfile::bin_exe("atpkg"))
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
            .expect("spawn dev atpkg")
    });
    drop(start);
    let mut holding = Holding {
        holder,
        pidfile: pidfile.clone(),
    };
    let child = pid_in(&pidfile);
    let held = fx.holders();
    assert!(
        matches!(&held, Holders::Held(who) if who.len() == 1 && who[0].starts_with("sigterm test")),
        "held while the command runs: {held:?}"
    );
    let holder_pid = libc::pid_t::try_from(holding.holder.id()).unwrap();
    // SAFETY: signals the holder this test spawned, and only it.
    unsafe { libc::kill(holder_pid, libc::SIGTERM) };
    let status =
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the holder's end", || {
            holding.holder.try_wait().ok().flatten()
        });
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
