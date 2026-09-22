// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The image anchor, bound to a REAL process whose bundle moves under it.
//!
//! The table test beside [`aterm_containment::classify_image_anchor`] proves
//! the classification. It cannot prove the INPUT, and the input is where this
//! detector was broken: until 2026-09-21 `image_anchor()` classified
//! `std::env::current_exe()`, which on macOS is the `execve`-time string and
//! never moves — so `Displaced` and `Deleted` were unreachable for the one
//! case the enum exists for, and the owner's `aterm ctl privacy` printed
//! `anchor=live` while `tccd` logged
//! `responsible_path=/Applications/aterm.app.rollback/…` for that same pid.
//!
//! So this test does the thing itself: it lays a real `.app`, runs a real
//! child out of it, and performs the updater's exact two moves — an atomic
//! swap (`renamex_np(RENAME_SWAP)` in `aterm_update::install`, reproduced here
//! with two renames, which is indistinguishable from the running process's
//! point of view) and then the boot-health delete — asserting the anchor the
//! child reports at each step.
//!
//! **Negative control.** Step 1 asserts `Live` BEFORE anything moves. Without
//! it a detector that answered `Displaced` unconditionally would pass, and the
//! old detector's failure was precisely a constant answer.
//!
//! The child is this same test binary, re-entered through the `#[ignore]`d
//! `anchor_probe_child` below — no fixture crate, no build script.

#![cfg(target_os = "macos")]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// The env var that tells a re-entered copy it is the child.
const CHILD_MARKER: &str = "ATERM_ANCHOR_PROBE_CHILD";

/// One temp directory, removed on drop even when an assert unwinds.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("aterm-anchor-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

/// The child's readings are TAGGED, because `--nocapture` interleaves the test
/// harness's own preamble ("running 2 tests", blank lines) on the same stdout.
/// Scanning for the tag rather than taking the first line is what keeps this
/// test from reading `""` and calling it an anchor.
const TAG: &str = "ANCHOR=";

/// Read the child's next tagged anchor, failing rather than hanging if the
/// child died or stopped tagging.
fn next_anchor(reader: &mut BufReader<std::process::ChildStdout>, step: &str) -> String {
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .unwrap_or_else(|e| panic!("{step}: reading the child's anchor: {e}"));
        assert!(read > 0, "{step}: the child produced no `{TAG}` line");
        if let Some(anchor) = line.trim().strip_prefix(TAG) {
            return anchor.to_string();
        }
    }
}

/// Let the child take its next reading.
fn step(child: &mut Child) {
    let stdin = child.stdin.as_mut().expect("child stdin");
    stdin.write_all(b"\n").expect("nudging the child");
    stdin.flush().expect("flushing the child");
}

#[test]
fn the_anchor_follows_a_bundle_renamed_and_then_deleted_under_a_live_process() {
    let scratch = Scratch::new("live");
    let app = scratch.path().join("A.app");
    let macos = app.join("Contents/MacOS");
    std::fs::create_dir_all(&macos).expect("bundle layout");
    let exe = macos.join("probe");
    std::fs::copy(std::env::current_exe().expect("test exe"), &exe).expect("copy probe");

    let mut child = Command::new(&exe)
        .args(["--exact", "anchor_probe_child", "--nocapture", "--ignored"])
        .env(CHILD_MARKER, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the probe child");
    let mut out = BufReader::new(child.stdout.take().expect("child stdout"));

    // 1. THE NEGATIVE CONTROL. Nothing has moved; a detector that always cries
    //    displacement fails right here.
    assert_eq!(
        next_anchor(&mut out, "before"),
        "live",
        "an untouched bundle must read live, or the other two arms prove nothing"
    );

    // 2. The apply's atomic swap: the incoming bundle takes the name, the one
    //    this child is executing out of lands at `A.app.rollback`.
    let rollback = scratch.path().join("A.app.rollback");
    let incoming = scratch.path().join("incoming.app");
    std::fs::create_dir_all(incoming.join("Contents/MacOS")).expect("incoming layout");
    std::fs::copy(&exe, incoming.join("Contents/MacOS/probe")).expect("copy incoming");
    std::fs::rename(&app, &rollback).expect("displace the running bundle");
    std::fs::rename(&incoming, &app).expect("install the incoming bundle");
    step(&mut child);
    assert_eq!(
        next_anchor(&mut out, "after swap"),
        "displaced",
        "the frozen exec path still names a live `.app` here — only the kernel's \
         path shows the displacement, which is the whole point of this test"
    );

    // 3. Boot-health confirmation collects the rollback. The child's code now
    //    has no path at all, which is what macOS cannot build an identity from.
    std::fs::remove_dir_all(&rollback).expect("collect the rollback");
    step(&mut child);
    assert_eq!(
        next_anchor(&mut out, "after gc"),
        "deleted",
        "a collected rollback must outrank displacement: it is the less \
         recoverable state"
    );

    step(&mut child);
    let status = child.wait().expect("the child exits");
    assert!(status.success(), "child exited {status:?}");
}

/// The child half. Prints one anchor per nudge, three times.
///
/// `#[ignore]` keeps it out of an ordinary `targo test` run: it is reachable
/// only by the explicit `--exact … --ignored` invocation above, and it
/// early-returns when the marker is absent so a `--ignored` sweep cannot hang
/// waiting on a stdin nobody is writing to.
#[test]
#[ignore = "re-entered as the child of the live-anchor test"]
fn anchor_probe_child() {
    if std::env::var_os(CHILD_MARKER).is_none() {
        return;
    }
    let stdin = std::io::stdin();
    for _ in 0..3 {
        println!("ANCHOR={}", aterm_containment::image_anchor().as_str());
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
    }
}
