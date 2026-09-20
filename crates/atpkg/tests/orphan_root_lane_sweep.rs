// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The trust EXEC ROOT's debris sweep against the one writer the store-wide lock cannot
//! speak for: a lane helper that outlived the process which submitted it.
//!
//! `compat::sweep` was the last sweep in this crate still justifying its unguarded
//! `remove_dir_all` with the lock alone — "callers hold the store lock … so no other
//! atpkg is mid-way through laying a temp this would take for debris". That argument had
//! already been disproved for this exact machinery, twice, by the commits that taught
//! `store::sweep_stage_scratch`, `gc`'s partial sweep and `seam::sweep_view_debris` to
//! stop orphaned lane jobs first: a provenance-tracked pass hands the root lay to a
//! LAUNCHD job (`compat::ensure_root_with` submits a `seam::ViewJob::Root`, and
//! `view-helper` is one of `stage_helper`'s `JOB_STEMS`), so the helper is launchd's
//! child rather than the submitter's and holds no store lock of its own.
//!
//! A `kill -9` of that pass therefore drops the lock while its helper keeps laying into
//! the very `.<n>.tmp-<pid>` / `.<n>.old-<pid>` names this sweep deletes as debris — and
//! the debris comes straight back, one abandoned root's worth of clones per crash,
//! keeping a reclaimed build's blocks allocated with nothing under `store/` to show for
//! them. Nothing ever names it again: only a re-lay of that same build number would meet
//! it.
//!
//! The orphan is reproduced exactly — a real launchd job carrying OUR label shape, under
//! a pid that does not exist, re-creating root debris in a loop — and the real public
//! sweep (`aterm pkg gc`'s own, `compat::sweep`) is driven over it. Measured two ways,
//! neither fakeable: launchd must no longer list the label, and the debris must STAY
//! gone.
//!
//! AND WHAT IT MUST NOT STOP: a bystander of our own shape under a LIVE pid — another
//! pass in flight, or a pid since reused — sits in the same `launchctl list` and must
//! survive. Stopping it would be widening a sweep, which is the one thing these sweeps
//! may never do.
//!
//! ITS OWN TEST BINARY, for `orphan_lane_sweep.rs`'s reason: the fixture is a launchd job
//! with a dead owner pid, and stopping exactly such jobs is what every sweep in this
//! crate does — run beside a thousand parallel unit tests, one of them would stop the
//! fixture before the sweep under test ever saw it. Cargo runs test binaries one at a
//! time.
//!
//! macOS only: there is no launchd, and so no lane job to outlive anyone, anywhere else.

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

/// A pid nothing runs under, measured rather than guessed (macOS pids stay below 99999)
/// and taken WITHOUT spawning: a fork here would hand the child a copy of every fd this
/// binary holds open.
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

    // THE ORPHAN: the temp tree of a root lay that is gone, with a launchd job still
    // laying into it. The label carries the root lane's own shape — `view-helper` is one
    // of `JOB_STEMS`, and `ViewJob::Root` is submitted under it — beneath a pid that does
    // not exist, which is exactly what a killed pass leaves registered.
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

    // THE BYSTANDER: our shape under a LIVE pid. Not orphaned, so not ours to stop.
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

    // THE SUCCESSOR: the sweep itself, the public one `aterm pkg gc` runs.
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
