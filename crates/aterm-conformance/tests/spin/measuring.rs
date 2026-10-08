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
/// its verdict. Serialized: each row measures idleness, which concurrent rows
/// would disturb.
fn probe(shape: &str, window: &str, extra: &[&str]) {
    static SERIAL: Mutex<()> = Mutex::new(());
    let _take_turns = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let Some(bin) = support::release_bin(&["ATERM_SPIN_BIN", "ATERM_PAINT_BIN"]) else {
        return;
    };
    let root = workspace_root();
    let script = root.join("tools/spin-conformance/spin_probe.sh");
    assert!(script.is_file(), "{} is missing", script.display());

    let out = Command::new("/bin/bash")
        .arg(&script)
        .arg(&bin)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .args(["--shape", shape, "--settle", "3", "--window", window])
        .args(["--max-arms", "100", "--budget", "120"])
        .args(extra)
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
        // The probe decided nothing: the message opens with the gate's COULD
        // NOT RUN sentinel, never a finding and never green (the MEASURE
        // receipt says `measured no`).
        Some(2) => panic!(
            "aterm-gate: COULD NOT RUN — SPIN CONFORMANCE COULD NOT RUN [{shape}]: the probe \
             decided nothing, which is not a pass.\n  {report}\n--- probe stderr ---\n{stderr}"
        ),
        code => panic!(
            "spin probe [{shape}] died abnormally (exit {code:?}, the protocol is 0/1/2)\n\
             --- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
        ),
    }
}

/// Matrix row 1, the regression's shape: the alt-screen steady repainter
/// arrives inside the measured window (one second of real writes, then bytes
/// marking no cell ~6x/s). Output without grid movement is what splits
/// `StatusFsm::owed_wake`'s movement clock from `classify`'s output clock. The
/// window is 10 s because the split bites `quiet_after` (5 s) after the last
/// real write.
#[test]
fn an_idle_claude_shaped_client_banks_no_past_deadline_arms() {
    probe("claude", "10", &["--max-timer-wakes", "12"]);
}

/// Matrix row 2, the floor: a bare `/bin/sh` prompt with nothing running. Red
/// here would mean a spin that is not client-shaped, a different finding from
/// row 1, so the rows stay separate.
#[test]
fn a_bare_idle_instance_banks_no_past_deadline_arms() {
    probe("idle", "6", &[]);
}
