// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The spin matrix's rows — the suite's own docs are in `../spin.rs`.

use super::support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// The workspace root, from this crate's own manifest dir.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/aterm-conformance has a workspace root two levels up")
        .to_path_buf()
}

/// Drive one shape through `tools/spin-conformance/spin_probe.sh` and assert
/// its verdict.
///
/// Serialized: each row launches its own instance and then MEASURES ITS
/// IDLENESS, and two concurrent rows would contend for the CPU — a gate about
/// quiet must never be disturbed by its own harness.
fn probe(shape: &str) {
    static SERIAL: Mutex<()> = Mutex::new(());
    let _take_turns = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let root = workspace_root();
    let bin = support::release_bin(&root, &["ATERM_SPIN_BIN", "ATERM_PAINT_BIN"]);
    let script = root.join("tools/spin-conformance/spin_probe.sh");
    assert!(script.is_file(), "{} is missing", script.display());

    let out = Command::new("/bin/bash")
        .arg(&script)
        .arg(&bin)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .args(["--shape", shape, "--settle", "3", "--window", "6"])
        .args(["--max-arms", "100", "--budget", "120"])
        .output()
        .unwrap_or_else(|e| panic!("could not spawn {}: {e}", script.display()));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let report = stdout
        .lines()
        .rev()
        .find(|l| l.starts_with("SPIN"))
        .unwrap_or("<no SPIN report line>")
        .to_string();
    match out.status.code() {
        Some(0) => eprintln!("spin[{shape}]: {report}"),
        Some(1) => panic!(
            "SPIN CONFORMANCE FAILED [{shape}]: an IDLE instance banked past-deadline arms — \
             the event loop is arming wakes at instants its own observation gate has already \
             passed, which is the 200 kHz spin class that cost the owner 79% CPU and a 335 ms \
             input p99 (docs/RELEASE-PROOF-DISCIPLINE.md).\n  {report}\n--- probe stderr ---\n{stderr}"
        ),
        Some(2) => panic!(
            "SPIN CONFORMANCE COULD NOT RUN [{shape}]: the probe decided nothing, which is not \
             a pass.\n  {report}\n--- probe stderr ---\n{stderr}"
        ),
        code => panic!(
            "spin probe [{shape}] died abnormally (exit {code:?}, the protocol is 0/1/2)\n\
             --- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
        ),
    }
}

/// Matrix row 1 — THE REGRESSION'S OWN SHAPE, and the row that can go red.
/// The alt-screen steady repainter arrives on the instance inside the measured
/// window: DEC-2026 bracketed, DECTCEM hide, the caret parked, and the SAME
/// BYTES rewritten ~6x/s. Output without grid movement is exactly what split
/// `StatusFsm::owed_wake`'s movement clock from `classify`'s output clock and
/// left the loop re-arming a dead deadline.
///
/// MEASURED 2026-08-24, both arms, RELEASE profile, headless, 6 s window:
///   healthy HEAD                    past_deadline_arms=0     PASS
///   420e4164 reverted               past_deadline_arms=7929  FAIL  (1321/s)
#[test]
fn an_idle_claude_shaped_client_banks_no_past_deadline_arms() {
    probe("claude");
}

/// Matrix row 2, the floor: a bare `/bin/sh` prompt with nothing running. If
/// this one ever goes red the spin is not client-shaped at all, which is a
/// materially different (and worse) finding than row 1 — so the two rows are
/// kept apart rather than folded.
///
/// It reads 0 on the BROKEN build too (measured), so it is not a falsifiable
/// row for the 2026-08 defect and does not pretend to be. Row 1 carries that
/// burden.
#[test]
fn a_bare_idle_instance_banks_no_past_deadline_arms() {
    probe("idle");
}
