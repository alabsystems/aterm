// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SPIN CONFORMANCE — an idle instance must stay idle.
//!
//! WHY THIS SUITE EXISTS (2026-08 responsiveness audit, item A3). The
//! freeze-safety gate (`tools/freeze-safety-gate`) watches for loops that
//! BLOCK. Nothing in this repo watched for a loop that RUNS TOO MUCH, and so a
//! 200 kHz event-loop spin read to every existing gate as vigor.
//!
//! MEASURED LIVE on the owner's session before the fix: 31,913 stale deadline
//! re-arms per second, 913 MILLION banked, the process pinned at 79% CPU while
//! IDLE, and input latency p50 7.9 ms but p95 100 ms / p99 335 ms. The tail was
//! the lag they felt. The trigger shape is their daily one: a Claude-shaped
//! client emitting OUTPUT WITHOUT GRID MOVEMENT — a spinner repainting in
//! place — which armed a deadline already behind its own observation gate, on
//! every pass, forever.
//!
//! THE WITNESS is `past_deadline_arms` from `ctl metrics`. It counts exactly
//! the contradiction the spin is made of: a wake armed at an instant the gate
//! that judges it has already passed. Healthy idle banks EXACTLY ZERO of them
//! across this suite's window; the same window on a build with the fix reverted
//! banks 7929. That is why this counter, and not CPU% or a frame rate, is what
//! the gate asserts — it is the contradiction itself, counted.
//!
//! THE MATRIX (each row a test):
//!   1. a steady repainter arriving on the instance, 6 s → ~0 past_deadline_arms
//!   2. a bare /bin/sh prompt, nothing running, 6 s   → ~0 past_deadline_arms
//!
//! THE FIXTURE IS THE GATE. Row 1 does NOT reuse the paint matrix's
//! `fake_claude.py`: that client's spinner glyph and counter change every
//! frame, so every repaint is REAL grid movement and the movement clock never
//! goes stale. Measured against a build with the fix reverted, `fake_claude.py`
//! banked exactly 0 past-deadline arms — a green gate over a broken build.
//! `tools/spin-conformance/idle_repainter.py` rewrites BYTE-IDENTICAL content
//! ~6x/s forever: the bytes keep arriving (so `classify` sustains
//! phase=Running) while honest damage accounting sees nothing change (so
//! `owed_wake`'s movement clock goes stale), which is the two-clock split the
//! spin was made of.
//!
//! AND SO IS THE WINDOW'S PLACEMENT. The counter is zeroed BEFORE the client
//! starts, and the measured window opens with its arrival. Headless, the whole
//! episode is a burst in the ~2 s after the client reaches the alt screen,
//! after which the status FSM's Running phase ages out and the loop goes quiet
//! again; a probe that settles the client first and resets afterwards measures
//! the calm on the far side of the fire and reports 0. (On the owner's live
//! WINDOWED session fresh turns kept arriving, so the same defect never ran out
//! of fuel and sustained 31,913 arms/sec indefinitely.)
//!
//! PROVEN RED, 2026-08-24. Commit 420e4164 ("the event loop stops promising
//! itself work it cannot do") was reverted in a scratch worktree and the
//! RELEASE binary rebuilt; the healthy arm is the same worktree restored.
//!
//!   row 1 (steady repainter)   broken: 7929 arms / 1321 per sec  FAIL
//!                             healthy:    0 arms /    0 per sec  PASS
//!   row 2 (bare prompt)        broken:    0 arms                 PASS
//!                             healthy:    0 arms                 PASS
//!
//! Row 2 does not go red on the broken build, and that is correct and stated
//! rather than hidden: it is the FLOOR, not the regression's shape. Row 1 is
//! the falsifiable row.
//!
//! WHAT THIS INSTRUMENT PERTURBS (docs/RELEASE-PROOF-DISCIPLINE.md, the
//! OBSERVER RULE): during the measurement window the probe issues NO control
//! verbs — the two `ctl metrics` calls bracket the window from outside it.
//! Deliberately NOT `ctl video`: a recording drives a present loop, which is
//! the very idleness under test (and its motion pin is what blinded the paint
//! matrix for three releases). The counter is zeroed with `ctl metrics reset`
//! before the window, so what is asserted is a RATE over a known interval and
//! startup's own arming is never charged to the steady state.
//!
//! THE CEILING, calibrated 2026-08-24 (RELEASE profile, headless): BOTH rows
//! bank exactly 0 arms over a 6 s window on a healthy build, and row 1 banks
//! 7929 on the broken one. The gate's ceiling is 100 per window (≈16/s) —
//! infinitely above a healthy reading of zero, 79x below the measured
//! regression, and far enough off both to survive a loaded machine.
//!
//! WIRING: `tools/spin_guard.sh`, in the `guards` lane of `xtask gate lint` —
//! beside `paint_guard`, and fingerprinted the same way, so a run that does not
//! touch the event loop costs one content hash. NOTHING RUNS THAT LANE
//! AUTOMATICALLY: `.githooks/pre-push` was demoted to advisory on 2026-08-24
//! (its body is one printf and `exit 0`), and `tools/verify.sh --fast` — the
//! merge contract — runs `grep_guard.sh` and the license sweep as stages of its
//! own but never `run_repo_guards`, so it never reaches this script or
//! `paint_guard`. So this guard runs when a human runs
//! `cargo run -p xtask -- gate lint`, and at no other time. The fingerprint is
//! still worth having, because that is the run it makes cheap.
//!
//! The binary under test is RELEASE profile. Without an override, the shared
//! conformance helper freshens `target/conformance-release/release/aterm`, a
//! dedicated target reused by paint so nested builds do not feature-thrash the
//! outer test target. `ATERM_SPIN_BIN` (or `ATERM_PAINT_BIN`) drives an existing
//! artifact.

#[cfg(unix)]
mod support;

/// The honest answer on Windows. NOT a pass in disguise: the probe drives a
/// POSIX shell sandbox (`mktemp -d`, `/bin/sh`, a unix control socket path),
/// so it says out loud that nothing about spin was measured here.
#[cfg(windows)]
#[test]
fn spin_matrix_is_a_unix_lane() {
    eprintln!(
        "spin: NOT RUN — the spin-conformance matrix drives a POSIX sandbox (mktemp -d, \
         /bin/sh, a unix-socket control path). NOTHING about event-loop spin was proven on \
         this platform; the matrix runs on the unix machines, via tools/spin_guard.sh."
    );
}

#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
#[cfg(unix)]
use std::sync::Mutex;

/// The workspace root, from this crate's own manifest dir.
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
#[test]
fn a_bare_idle_instance_banks_no_past_deadline_arms() {
    probe("idle");
}
