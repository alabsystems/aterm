// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The image anchor, bound to a REAL process whose bundle moves under it.
//!
//! The table test proves the classification; this binds its INPUT — the
//! kernel's path for the image, see [`aterm_containment::ImageAnchor`] — to a
//! child running out of a real `.app`, through the updater's two moves: the
//! atomic swap that leaves the running bundle at `A.app.rollback`, then the
//! boot-health delete. Step 1 reads `live` before anything moves, so a detector
//! that always answered `displaced` fails; a second child launched through a
//! symlink that is retargeted mid-run must stay `live`.
//!
//! The child is this same test binary, re-entered through the `#[ignore]`d
//! `anchor_probe_child`.

#![cfg(target_os = "macos")]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

const CHILD_MARKER: &str = "ATERM_ANCHOR_PROBE_CHILD";
/// Readings are tagged: `--nocapture` interleaves the harness's own output.
const TAG: &str = "ANCHOR=";
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// The child, killed and reaped on every exit path, including a failed assert.
struct Probe {
    child: Child,
    readings: Receiver<String>,
}

impl Probe {
    fn spawn(exe: &Path) -> Self {
        let mut child = Command::new(exe)
            .args(["--exact", "anchor_probe_child", "--nocapture", "--ignored"])
            .env(CHILD_MARKER, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn the probe child");
        let stdout = child.stdout.take().expect("child stdout");
        let (tx, readings) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(anchor) = line.trim().strip_prefix(TAG)
                    && tx.send(anchor.to_string()).is_err()
                {
                    return;
                }
            }
        });
        Self { child, readings }
    }

    /// The child's next reading; fails rather than hanging on a silent child.
    fn read(&mut self, step: &str) -> String {
        self.readings
            .recv_timeout(READ_TIMEOUT)
            .unwrap_or_else(|e| panic!("{step}: no `{TAG}` reading from the child: {e}"))
    }

    /// Let the child take its next reading.
    fn nudge(&mut self) {
        let stdin = self.child.stdin.as_mut().expect("child stdin");
        stdin.write_all(b"\n").expect("nudging the child");
        stdin.flush().expect("flushing the child");
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Lay `<root>/<name>/Contents/MacOS/probe`, a copy of this test binary.
fn lay_bundle(root: &Path, name: &str) -> std::path::PathBuf {
    let macos = root.join(name).join("Contents/MacOS");
    std::fs::create_dir_all(&macos).expect("bundle layout");
    let exe = macos.join("probe");
    std::fs::copy(std::env::current_exe().expect("test exe"), &exe).expect("copy probe");
    exe
}

#[test]
fn the_anchor_follows_a_bundle_renamed_and_then_deleted_under_a_live_process() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let root = scratch.path();
    let exe = lay_bundle(root, "A.app");
    let mut probe = Probe::spawn(&exe);

    assert_eq!(probe.read("before"), "live", "the negative control");

    // The apply's swap: the incoming bundle takes the name, the running one
    // lands at `A.app.rollback`.
    lay_bundle(root, "incoming.app");
    std::fs::rename(root.join("A.app"), root.join("A.app.rollback")).expect("displace");
    std::fs::rename(root.join("incoming.app"), root.join("A.app")).expect("install");
    probe.nudge();
    assert_eq!(probe.read("after swap"), "displaced");

    // Boot-health confirmation collects the rollback.
    std::fs::remove_dir_all(root.join("A.app.rollback")).expect("collect");
    probe.nudge();
    assert_eq!(probe.read("after gc"), "deleted");
}

/// A launch symlink retargeted mid-run moves nothing the kernel resolves, so
/// the anchor must stay `live`: the bundle this process runs from is untouched.
#[test]
fn a_retargeted_launch_symlink_is_not_a_displacement() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let root = scratch.path();
    let exe = lay_bundle(root, "R.app");
    let other = lay_bundle(root, "S.app");
    let shim = root.join("shim");
    std::os::unix::fs::symlink(&exe, &shim).expect("symlink");
    let mut probe = Probe::spawn(&shim);

    assert_eq!(probe.read("before"), "live");
    std::fs::remove_file(&shim).expect("unlink shim");
    std::os::unix::fs::symlink(&other, &shim).expect("retarget shim");
    probe.nudge();
    assert_eq!(probe.read("after retarget"), "live");
}

/// The child half: one tagged reading per nudge. `#[ignore]`d and gated on the
/// marker so an `--ignored` sweep cannot hang on a stdin nobody writes to.
#[test]
#[ignore = "re-entered as the child of the live-anchor tests"]
fn anchor_probe_child() {
    if std::env::var_os(CHILD_MARKER).is_none() {
        return;
    }
    let stdin = std::io::stdin();
    loop {
        println!("{TAG}{}", aterm_containment::image_anchor().as_str());
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
    }
}
