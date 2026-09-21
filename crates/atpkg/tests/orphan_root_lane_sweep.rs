// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The trust EXEC ROOT's debris sweep against the one writer the store-wide lock cannot
//! speak for: a lane helper that outlived the process which submitted it.
//!
//! A provenance-tracked pass hands the root lay to a launchd job (`compat::ensure_root_with`
//! submits a `seam::ViewJob::Root`, and `view-helper` is one of `stage_helper`'s
//! `JOB_STEMS`), so the helper is launchd's child and holds no store lock — the lock alone
//! cannot justify this sweep's unguarded `remove_dir_all`. A `kill -9` of the pass drops the
//! lock while its helper keeps laying into the very `.<n>.tmp-<pid>` / `.<n>.old-<pid>` names
//! the sweep deletes, so the debris returns and keeps a reclaimed build's blocks allocated
//! with nothing under `store/` left to name it. Measured two ways, neither fakeable: launchd
//! must no longer list the label, and the debris must stay gone. A bystander of our shape
//! under a live pid must survive the same pass — stopping it would widen the sweep.
//!
//! Its own test binary, for `orphan_lane_sweep.rs`'s reason: the fixture is a launchd job
//! with a dead owner pid, and every sweep here stops exactly those. macOS only.

#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use atpkg::compat::{roots_dir, sweep};
use atpkg::store::Layout;

/// A scratch root of this test's own.
fn tmp(label: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("atpkg-orphan-root-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A pid nothing runs under (macOS pids stay below 99999), found without spawning: a
/// fork here would hand the child a copy of every fd this binary holds open.
fn a_dead_pid() -> u32 {
    (90_000..99_999u32)
        .rev()
        .find(|pid| {
            let rc = unsafe { libc::kill(*pid as libc::pid_t, 0) };
            rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        })
        .expect("some pid below 99999 is unused")
}

/// Whether launchd still lists `label`.
fn listed(label: &str) -> bool {
    Command::new("/bin/launchctl")
        .args(["list", label])
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn a_launchd_helper_outliving_a_killed_root_lay_is_stopped_before_its_debris_is_swept() {
    let dir = tmp("sweep");
    let layout = Layout {
        prefix: dir.join("prefix"),
    };
    // `<prefix>/compat/trust`, the one directory this sweep walks.
    let roots = roots_dir(&layout);
    std::fs::create_dir_all(&roots).unwrap();

    // The orphan: the temp tree of a root lay that is gone, with a launchd job still
    // laying into it. The label carries the root lane's own shape (`view-helper` is one of
    // `JOB_STEMS`, and `ViewJob::Root` is submitted under it) beneath a pid that does not
    // exist — what a killed pass leaves registered.
    let dead = a_dead_pid();
    let me = std::process::id();
    let debris = roots.join(format!(".8595.tmp-{dead}"));
    let label = format!("systems.alab.atpkg.view-helper-{dead}-0-{me:x}");
    let submit = Command::new("/bin/launchctl")
        .args(["submit", "-l", &label, "--", "/bin/sh", "-c"])
        .arg("while :; do /bin/mkdir -p \"$1/bin\" && : > \"$1/bin/trustc\"; /bin/sleep 0.2; done")
        .arg("atpkg-orphan-root-helper")
        .arg(&debris)
        .output()
        .expect("/bin/launchctl runs");
    assert!(
        submit.status.success(),
        "launchctl submit: {}",
        String::from_utf8_lossy(&submit.stderr)
    );

    // The bystander: our shape under a live pid. Not orphaned, so not ours to stop.
    let live = format!("systems.alab.atpkg.view-helper-{me}-9-{me:x}");
    let bystander_registered = Command::new("/bin/launchctl")
        .args(["submit", "-l", &live, "--", "/bin/sh", "-c"])
        .arg("while :; do /bin/sleep 5; done")
        .output()
        .is_ok_and(|o| o.status.success());

    // The fixture is the real thing only once the helper is really laying in there.
    let started = Instant::now();
    while !debris.join("bin").join("trustc").exists() && started.elapsed() < Duration::from_secs(30)
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    let was_laying = debris.join("bin").join("trustc").exists();

    // The successor: the sweep itself, the public one `aterm pkg gc` runs.
    let report = sweep(&layout);

    let job_left_running = listed(&label);
    let bystander_survived = listed(&live);
    // A helper that is still alive re-creates the directory within its next tick; a
    // stopped one cannot. Watch for several ticks, so "gone" means gone.
    let watch = Instant::now();
    let mut came_back = false;
    while watch.elapsed() < Duration::from_millis(1_500) {
        if debris.exists() {
            came_back = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Never leave a launchd job behind, whatever the assertions below decide.
    for l in [&label, &live] {
        let _ = Command::new("/bin/launchctl").args(["remove", l]).output();
    }

    assert!(
        was_laying,
        "PRECONDITION: the orphaned helper never started laying into {}",
        debris.display()
    );
    assert!(
        bystander_registered,
        "PRECONDITION: the bystander job would not register"
    );
    assert!(
        report.swept.contains(&debris),
        "PRECONDITION: the sweep did not remove the debris it exists for: swept \
         {:?}, errors {:?}",
        report.swept,
        report.errors
    );
    assert!(
        !job_left_running,
        "the sweep deleted the root debris and left the helper running: launchd still \
         lists {label}"
    );
    assert!(
        !came_back,
        "{} came back after the sweep — a writer this process cannot see was still laying \
         files inside the exec roots",
        debris.display()
    );
    assert!(
        bystander_survived,
        "the sweep stopped a lane job whose owner is ALIVE: {live}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
