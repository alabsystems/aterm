// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The rustup VIEW's debris sweep against the one writer the store-wide lock cannot speak
//! for: a lane helper that outlived the process which submitted it.
//!
//! The view lane hands its work to a launchd job (`stage_helper::Job::prepare(&scratch,
//! "view-helper")`), so the helper is launchd's child, not the submitter's, and a killed
//! submitter releases the lock while its helper keeps writing — `seam::sweep_view_debris`
//! cannot justify its unguarded `remove_dir_all` with the lock alone and has to stop the
//! orphan first, as `store::sweep_stage_scratch` and `gc`'s pass do. The refresh driven over
//! the orphan is the one that lays nothing, so the sweep is the only thing under test.
//! Measured two ways, neither fakeable: launchd must no longer list the label, and the debris
//! must stay gone. A bystander of our shape under a live pid must survive the same pass —
//! stopping it would widen the sweep.
//!
//! Its own test binary, for `orphan_lane_sweep.rs`'s reason: the fixture is a launchd job
//! with a dead owner pid, and every sweep here stops exactly those. macOS only.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use atpkg::seam::{refresh_view_in_process, view_dir};
use atpkg::store::Layout;

/// A scratch root of this test's own.
fn tmp(label: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("atpkg-orphan-view-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A store with one trust build — the Trust-named tools in `bin/`, a `lib/` — and
/// `store/trust/current` pointing at it: the shape `refresh_view_in_process` reads.
fn fake_store(prefix: &Path, build: u64) -> PathBuf {
    let dir = prefix.join("store").join("trust").join(build.to_string());
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("lib").join("libtrust.dylib"), "driver").unwrap();
    for tool in ["trustc", "targo"] {
        std::fs::write(
            dir.join("bin").join(tool),
            format!("#!/bin/sh\necho {tool}\n"),
        )
        .unwrap();
    }
    std::os::unix::fs::symlink(&dir, prefix.join("store").join("trust").join("current")).unwrap();
    dir
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
fn a_launchd_helper_outliving_a_killed_view_pass_is_stopped_before_its_debris_is_swept() {
    let dir = tmp("sweep");
    let layout = Layout {
        prefix: dir.join("prefix"),
    };
    std::fs::create_dir_all(&layout.prefix).unwrap();
    fake_store(&layout.prefix, 8595);

    // A laid view, so the refresh under test lays nothing and the sweep is all it does.
    refresh_view_in_process(&layout, "trust")
        .unwrap_or_else(|e| panic!("the view must lay on this macOS: {e}"));
    let view = view_dir(&layout, "trust");

    // The orphan: the debris of a view pass that is gone, with a launchd job still writing
    // into it. The label carries the view lane's own shape (`view-helper` is one of
    // `JOB_STEMS`) under a pid that does not exist — what a killed pass leaves registered.
    let dead = a_dead_pid();
    let me = std::process::id();
    let debris = view.join(format!(".bin.old-{dead}"));
    let label = format!("systems.alab.atpkg.view-helper-{dead}-0-{me:x}");
    let submit = Command::new("/bin/launchctl")
        .args(["submit", "-l", &label, "--", "/bin/sh", "-c"])
        .arg("while :; do /bin/mkdir -p \"$1/bin\" && : > \"$1/bin/trustc\"; /bin/sleep 0.2; done")
        .arg("atpkg-orphan-view-helper")
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

    // The fixture is the real thing only once the helper is really writing in there.
    let started = Instant::now();
    while !debris.join("bin").join("trustc").exists() && started.elapsed() < Duration::from_secs(30)
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    let was_writing = debris.join("bin").join("trustc").exists();

    // The successor: a refresh whose view already matches, so `sweep_view_debris` is the
    // whole of what it does.
    let refreshed = refresh_view_in_process(&layout, "trust");

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
        was_writing,
        "PRECONDITION: the orphaned helper never started writing into {}",
        debris.display()
    );
    assert!(
        bystander_registered,
        "PRECONDITION: the bystander job would not register"
    );
    let refreshed = refreshed.expect("the successor's own refresh still goes through");
    assert!(
        !refreshed.changed,
        "PRECONDITION: the view already matched, so the sweep is all this pass did"
    );
    assert!(
        !job_left_running,
        "the sweep deleted the view debris and left the helper running: launchd still \
         lists {label}"
    );
    assert!(
        !came_back,
        "{} came back after the sweep — a writer this process cannot see was still laying \
         files inside the view",
        debris.display()
    );
    assert!(
        bystander_survived,
        "the sweep stopped a lane job whose owner is ALIVE: {live}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
