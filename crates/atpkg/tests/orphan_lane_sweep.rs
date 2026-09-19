// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The store's stage-scratch sweep against the ONE writer the store-wide lock cannot
//! speak for: a staging helper that outlived the process which submitted it.
//!
//! The untracked staging lane hands extraction to a LAUNCHD job
//! (`atpkg::stage_helper`), so the helper is launchd's child, not the installer's. A
//! `kill -9` of the installer therefore releases the store lock while its helper keeps
//! extracting into `store/<program>/<build>.incoming-<the dead installer's pid>`. The
//! next install took that lock, swept the scratch as debris — deleting it out from under
//! the live helper — and stopped the helper only later, when its own lane submitted a
//! job. In between, a writer this installer could not see was laying files inside the
//! store, and the scratch it had just reclaimed came straight back as debris nothing
//! reclaims (scratch is invisible to `list_installed`, so only a re-stage of that same
//! build number would ever meet it again).
//!
//! The orphan is reproduced exactly — a real launchd job carrying OUR label shape under
//! a pid that does not exist, writing into that scratch in a loop — and a real install
//! is driven over it through the public `verify_and_stage`, whose first act is the
//! sweep. Whether the sweep stopped the helper first is measured the one way that cannot
//! be faked: the scratch must STAY deleted.
//!
//! AND WHAT IT MUST NOT STOP. The sweep issues `launchctl remove`, so the selection rule
//! is as load-bearing as the stopping: three BYSTANDER jobs are registered beside the
//! orphan and must all survive the same pass — one of OUR shape under a LIVE pid (an
//! install in flight, or a pid since reused), one carrying a dead-pid tail under another
//! vendor's prefix, and one under our own prefix with a stem that is not a lane of ours.
//! A sweep that took any of those would be stopping a stranger's job.
//!
//! ITS OWN TEST BINARY, on purpose. The fixture is a launchd job with a dead owner pid,
//! and stopping exactly such jobs is what every sweep in this crate now does — so run
//! inside the unit suite, a thousand parallel tests would stop the fixture before the
//! sweep under test ever saw it (measured: the precondition failed, and the fixture's
//! own process churn took an unrelated timing test down with it). Cargo runs test
//! binaries one at a time, so here the fixture is only ever met by the sweep under test,
//! and for the same reason all of it is ONE test function rather than four.
//!
//! macOS only: there is no launchd, and so no lane job to outlive anyone, anywhere else.

#![cfg(target_os = "macos")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use atpkg::install::verify_and_stage;
use atpkg::manifest::{Artifact, Cost};

/// A scratch root of this test's own.
fn tmp(label: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("atpkg-orphan-lane-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The digest the signed row carries, taken the way an operator would check it.
fn sha256_hex(path: &Path) -> String {
    let out = Command::new("/usr/bin/shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .expect("/usr/bin/shasum runs");
    assert!(
        out.status.success(),
        "shasum: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .expect("shasum prints the digest first")
        .to_string()
}

/// A `raw-binary` row whose `sha256` HONESTLY describes `archive`: the smallest payload
/// that reaches the swap, so what is under test is the sweep and not an extractor.
/// `tree_root` is empty (the pre-`tree_root` manifest shape), which skips the apply-time
/// re-verify without weakening anything this test asserts.
fn artifact(archive: &Path) -> Artifact {
    Artifact {
        target: "aarch64-apple-darwin".into(),
        kind: "binary".into(),
        asset: "ay".into(),
        sha256: sha256_hex(archive),
        tree_root: String::new(),
        size: 0,
        reloc: "self-contained".into(),
        cost: Cost {
            disk_installed: 1 << 20,
            ..Cost::default()
        },
        url: String::new(),
        payload: "raw-binary".into(),
        entry: "ay".into(),
        strip_components: 0,
        links: BTreeMap::new(),
        vendor: String::new(),
        protocol: "github-release".into(),
        signer_team: String::new(),
        elevated: false,
        provides: vec![],
        manager: String::new(),
        package: String::new(),
        label_prefix: String::new(),
    }
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

/// Whether launchd still lists `label` — `launchctl list <label>` fails for a label it
/// does not have.
fn listed(label: &str) -> bool {
    Command::new("/bin/launchctl")
        .args(["list", label])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Register `label` on a job that idles until it is stopped: a BYSTANDER, there to be
/// found by the sweep's `launchctl list` and left alone.
fn submit_idle(label: &str) -> bool {
    Command::new("/bin/launchctl")
        .args(["submit", "-l", label, "--", "/bin/sh", "-c"])
        .arg("while :; do /bin/sleep 5; done")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn a_launchd_helper_outliving_a_killed_stager_is_stopped_before_its_scratch_is_swept() {
    let dir = tmp("sweep");
    let archive = dir.join("ay");
    std::fs::write(&archive, b"#!/bin/true\nthe ay binary\n").unwrap();
    let art = artifact(&archive);
    let build = dir.join("store").join("ay").join("18");
    std::fs::create_dir_all(build.parent().unwrap()).unwrap();

    // THE ORPHAN: the scratch of a stager that is gone, with a launchd job still writing
    // into it. The label carries the lane's own shape — `<stem>-<pid>-<seq>-<nonce>`
    // under `systems.alab.atpkg.` — because that shape, with a dead owner, is exactly
    // what a killed stager leaves registered.
    let dead = a_dead_pid();
    let me = std::process::id();
    let scratch = build.with_file_name(format!("18.incoming-{dead}"));
    let label = format!("systems.alab.atpkg.stage-helper-{dead}-0-{me:x}");
    let submit = Command::new("/bin/launchctl")
        .args(["submit", "-l", &label, "--", "/bin/sh", "-c"])
        .arg("while :; do /bin/mkdir -p \"$1\" && : > \"$1/bin.part\"; /bin/sleep 0.2; done")
        .arg("atpkg-orphan-helper")
        .arg(&scratch)
        .output()
        .expect("/bin/launchctl runs");
    assert!(
        submit.status.success(),
        "launchctl submit: {}",
        String::from_utf8_lossy(&submit.stderr)
    );

    // THE BYSTANDERS, each excluded by a DIFFERENT clause of the selection rule: a live
    // owner pid, a prefix that is not ours, and a stem that is not a lane of ours. All
    // three sit in the very `launchctl list` the sweep reads.
    let live = format!("systems.alab.atpkg.stage-helper-{me}-9-{me:x}");
    let foreign_prefix = format!("com.example.atpkg-audit.stage-helper-{dead}-0-{me:x}");
    let foreign_stem = format!("systems.alab.atpkg.mytool-{dead}-0-{me:x}");
    let bystanders = [&live, &foreign_prefix, &foreign_stem];
    let all_registered = bystanders.iter().all(|l| submit_idle(l));

    // The fixture is the real thing only once the helper is really writing in there.
    let started = Instant::now();
    while !scratch.join("bin.part").exists() && started.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let was_writing = scratch.join("bin.part").exists();

    // THE SUCCESSOR: a real install of build 18, whose first act is the scratch sweep.
    let staged = verify_and_stage(&art, &archive, &build);

    let job_left_running = listed(&label);
    let bystanders_left: Vec<&String> = bystanders.iter().copied().filter(|l| !listed(l)).collect();
    // A helper that is still alive re-creates the directory within its next tick; a
    // stopped one cannot. Watch for several ticks, so "gone" means gone.
    let watch = Instant::now();
    let mut came_back = false;
    while watch.elapsed() < Duration::from_millis(1_500) {
        if scratch.exists() {
            came_back = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Never leave a launchd job behind, whatever the assertions below decide.
    for l in std::iter::once(&label).chain(bystanders) {
        let _ = Command::new("/bin/launchctl").args(["remove", l]).output();
    }

    assert!(
        was_writing,
        "PRECONDITION: the orphaned helper never started writing into {}",
        scratch.display()
    );
    assert!(
        all_registered,
        "PRECONDITION: a bystander job would not register"
    );
    staged.expect("the successor's own install still goes through");
    assert!(
        build.join("bin").join("ay").is_file(),
        "PRECONDITION: the successor really staged build 18"
    );
    assert!(
        !job_left_running,
        "the sweep deleted the scratch and left the helper running: launchd still lists \
         {label}"
    );
    assert!(
        !came_back,
        "{} came back after the sweep — a writer this process cannot see was still laying \
         files inside the store",
        scratch.display()
    );
    assert!(
        bystanders_left.is_empty(),
        "the sweep stopped a job that is not its business to stop: {bystanders_left:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
