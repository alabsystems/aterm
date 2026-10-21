// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An app-band instance's launchd job (`aterm-lab band-launch`, the load
//! `instance --band app` makes, whose first process is `aterm-lab band`) runs
//! its program as launchd runs an app: it heads a coalition of its own, where
//! a child of this test shares the test's coalition; it runs at darwin role
//! `UI_FOCAL`; it is an APP-type task, so it may set its own darwin role as
//! aterm-pty sets each shell's (a DAEMON-type task gets `ENOTSUP`); and it
//! carries no QoS clamp, so a user-interactive thread lands at the UI band
//! (46), where a `launchctl submit` job's `ProcessType` `Standard` lands at 20.
//! Once its program ends (the lifeline cut), launchd's KeepAlive restart
//! removes the job by its label instead of starting the program again.
//!
//! The program is `aterm-lab band-standin` in aterm's place: it records one
//! line per start (its role, whether its own role change took, and the
//! priority a user-interactive request landed at), then reads the lifeline to
//! its end of file.

#![cfg(target_os = "macos")]

use std::fs::OpenOptions;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Stdio;

/// A hang detector, never a latency budget: launchd restarts a KeepAlive job
/// at most every 10 s.
const HANG: Duration = Duration::from_secs(60);

/// `PRIO_DARWIN_ROLE_UI_FOCAL`, the role the job's program must start at.
const UI_FOCAL: i32 = 1;

/// The base priority of a user-interactive thread at role `UI` (aterm-pty's
/// role for a shell, which the stand-in takes): 46. Under launchd's utility
/// clamp it is 20.
const UI_BAND: i32 = 46;

/// `pid`'s resource and jetsam coalition ids (`PROC_PIDCOALITIONINFO`, 20).
fn coalition(pid: i32) -> [u64; 2] {
    let mut buf = [0u64; 5];
    // SAFETY: `buf` is a writable 40-byte buffer, the size passed.
    let got = unsafe { libc::proc_pidinfo(pid, 20, 0, buf.as_mut_ptr().cast(), 40) };
    assert_eq!(got, 40, "PROC_PIDCOALITIONINFO of {pid}");
    [buf[0], buf[1]]
}

fn launchctl(args: &[&str]) -> bool {
    aterm_uds::spawnfd::command("/bin/launchctl")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The job `label` in this user's GUI domain, where `band-launch` loads it:
/// asked and removed by that domain, never through the legacy `list` and
/// `remove`, which read the caller's (another one over SSH, where a check of
/// the job's removal would pass with the job still loaded).
fn target(label: &str) -> String {
    // SAFETY: `getuid` takes no arguments and cannot fail.
    format!("gui/{}/{label}", unsafe { libc::getuid() })
}

/// Removes the job by its label however the test ends, so a failing run
/// leaves no KeepAlive job behind for launchd to restart.
struct Loaded<'a>(&'a str);

impl Drop for Loaded<'_> {
    fn drop(&mut self) {
        let _ = launchctl(&["bootout", &target(self.0)]);
    }
}

fn wait_for<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let end = Instant::now() + HANG;
    loop {
        if let Some(v) = probe() {
            return v;
        }
        assert!(Instant::now() < end, "{what} within {} s", HANG.as_secs());
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The job's world: its environment file and the lifeline FIFO, whose held
/// end is returned.
fn world(dir: &Path) -> OwnedFd {
    let fifo = dir.join("band.lifeline");
    let c = std::ffi::CString::new(fifo.to_str().expect("utf-8 path")).expect("path");
    // SAFETY: `c` is a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo");
    std::fs::write(
        dir.join("band.env"),
        format!("PATH=/usr/bin:/bin\0HOME={}\0", dir.display()),
    )
    .expect("the job's environment");
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo)
        .expect("the lifeline's held end")
        .into()
}

#[test]
fn an_app_band_job_runs_its_program_as_an_app_in_its_own_coalition_and_is_removed_once_it_ends() {
    // First declared, so the job-removal guard and FIFO end drop before its world.
    let owned_world = aterm_tempfile::TempDir::new_in(env!("CARGO_TARGET_TMPDIR"))
        .expect("exclusive scratch world");
    let dir = owned_world.path().to_path_buf();
    let held = world(&dir);
    let label = format!(
        "aterm-lab.test.band.{}",
        dir.file_name()
            .expect("exclusive world basename")
            .to_string_lossy()
    );
    let lab = env!("CARGO_BIN_EXE_aterm-lab");
    let runs = dir.join("runs");
    let (d, log) = (dir.to_str().expect("utf-8"), dir.join("log"));
    let launched = aterm_uds::spawnfd::command(lab)
        .args([
            "band-launch",
            d,
            &label,
            "--cwd",
            d,
            "--log",
            log.to_str().expect("utf-8"),
            "--",
            lab,
            "band-standin",
            runs.to_str().expect("utf-8"),
        ])
        .stdin(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    assert!(launched, "aterm-lab band-launch");
    let _loaded = Loaded(&label);
    let pid: i32 = wait_for("the job's pid file", || {
        std::fs::read_to_string(dir.join("band.pid"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    });
    wait_for("the program's start", || {
        std::fs::read_to_string(&runs)
            .ok()
            .filter(|s| s.ends_with('\n'))
    });
    let ours = coalition(i32::try_from(std::process::id()).expect("pid"));
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("60")
        .stdin(Stdio::null())
        .spawn()
        .expect("a child of this test");
    let childs = coalition(i32::try_from(child.id()).expect("pid"));
    let _ = child.kill();
    let _ = child.wait();
    let job = coalition(pid);
    let loaded = launchctl(&["print", &target(&label)]);
    // The job's end: its lifeline cut, the program reads end of file and exits;
    // launchd restarts the job, which removes itself by its label.
    drop(held);
    wait_for("the job's removal", || {
        (!launchctl(&["print", &target(&label)])).then_some(())
    });
    let starts = std::fs::read_to_string(&runs).unwrap_or_default();
    let job_log = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(loaded, "the job runs in this user's GUI domain");
    assert_eq!(
        childs, ours,
        "the control: a child of this test shares its coalition"
    );
    assert_ne!(
        job[0], ours[0],
        "the job heads a resource coalition of its own"
    );
    assert_ne!(job[1], ours[1], "and a jetsam coalition of its own");
    assert_eq!(
        starts,
        format!("{UI_FOCAL} ok {UI_BAND}\n"),
        "one start, at role UI_FOCAL, an APP-type task whose own role change takes, with no QoS \
         clamp: a user-interactive request lands at the UI band (log: {job_log:?})"
    );
}
