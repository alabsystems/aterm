// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Live process image-anchor tests across bundle rollback and removal.

#![cfg(target_os = "macos")]

use aterm_uds::deadline::{FIRST_EXEC_HANG, KeptUntilRan};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use aterm_uds::spawnfd::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

const CHILD_MARKER: &str = "ATERM_ANCHOR_PROBE_CHILD";
const TAG: &str = "ANCHOR=";
const READ_TIMEOUT: Duration = Duration::from_secs(60);

struct Probe {
    child: Child,
    readings: Receiver<String>,
    /// What it was started as, for a failure's words.
    exe: PathBuf,
}

impl Probe {
    fn spawn(exe: &Path) -> Self {
        let mut child = Command::new(exe)
            .args([
                "--exact",
                "the_anchor_follows_a_bundle_renamed_and_then_deleted_under_a_live_process",
                "--nocapture",
            ])
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
        Self {
            child,
            readings,
            exe: exe.to_path_buf(),
        }
    }

    fn first(&mut self, step: &str) -> String {
        self.within(step, FIRST_EXEC_HANG)
    }

    fn read(&mut self, step: &str) -> String {
        self.within(step, READ_TIMEOUT)
    }

    fn within(&mut self, step: &str, hang: Duration) -> String {
        self.readings.recv_timeout(hang).unwrap_or_else(|e| {
            panic!(
                "{step}: no `{TAG}` reading from the child running {}: {e}",
                self.exe.display()
            )
        })
    }

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

fn lay_bundle(root: &Path, name: &str) -> PathBuf {
    let macos = root.join(name).join("Contents/MacOS");
    std::fs::create_dir_all(&macos).expect("bundle layout");
    let exe = macos.join("probe");
    std::fs::copy(std::env::current_exe().expect("test exe"), &exe).expect("copy probe");
    exe
}

#[test]
fn the_anchor_follows_a_bundle_renamed_and_then_deleted_under_a_live_process() {
    if std::env::var_os(CHILD_MARKER).is_some() {
        anchor_probe_child();
        return;
    }
    let mut scratch = KeptUntilRan::new(aterm_tempfile::tempdir().expect("tempdir").keep());
    let root = &scratch.path().to_path_buf();
    let exe = lay_bundle(root, "A.app");
    let mut probe = Probe::spawn(&exe);

    let before = probe.first("before");
    scratch.ran();
    assert_eq!(before, "live", "the negative control");

    lay_bundle(root, "incoming.app");
    std::fs::rename(root.join("A.app"), root.join("A.app.rollback")).expect("displace");
    std::fs::rename(root.join("incoming.app"), root.join("A.app")).expect("install");
    probe.nudge();
    assert_eq!(probe.read("after swap"), "displaced");

    std::fs::remove_dir_all(root.join("A.app.rollback")).expect("collect");
    probe.nudge();
    assert_eq!(probe.read("after gc"), "deleted");
}

#[test]
fn a_retargeted_launch_symlink_is_not_a_displacement() {
    let mut scratch = KeptUntilRan::new(aterm_tempfile::tempdir().expect("tempdir").keep());
    let root = &scratch.path().to_path_buf();
    let exe = lay_bundle(root, "R.app");
    let other = lay_bundle(root, "S.app");
    let shim = root.join("shim");
    std::os::unix::fs::symlink(&exe, &shim).expect("symlink");
    let mut probe = Probe::spawn(&shim);

    let before = probe.first("before");
    scratch.ran();
    assert_eq!(before, "live");
    std::fs::remove_file(&shim).expect("unlink shim");
    std::os::unix::fs::symlink(&other, &shim).expect("retarget shim");
    probe.nudge();
    assert_eq!(probe.read("after retarget"), "live");
}

fn anchor_probe_child() {
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
