// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that an `aterm-lab band-launch` job runs its program as an app in its own coalition and
//! is removed once that program ends.

#![cfg(target_os = "macos")]

use std::fs::OpenOptions;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Stdio;

/// A hang detector, never a latency budget.
const HANG: Duration = Duration::from_secs(60);

/// `PRIO_DARWIN_ROLE_UI_FOCAL`, the role the job's program must start at.
const UI_FOCAL: i32 = 1;

/// The base priority of a user-interactive thread at role `UI` (46).
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

/// The job `label` in this user's GUI domain, where `band-launch` loads it.
fn target(label: &str) -> String {
    // SAFETY: `getuid` takes no arguments and cannot fail.
    format!("gui/{}/{label}", unsafe { libc::getuid() })
}

/// Removes the job by its label however the test ends.
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

/// The job's world: its environment file and the lifeline FIFO, whose held end is returned.
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
