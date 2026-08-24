// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PAINT CONFORMANCE — the shape matrix that pixel-checks the shipped binary.
//!
//! Born from the 2026-08-24 blackout audit (docs/RELEASE-PROOF-DISCIPLINE.md):
//! v0.48.0 and v0.49.0 shipped the rainbow cursor trail dark past green gates
//! because every proof measured a different machine, screen or profile than the
//! owner runs, and nothing in CI or the cut ever pixel-checked a shipped
//! artifact. This suite closes the CI half: it launches the RELEASE-profile
//! `target/release/aterm` HEADLESS, drives real keystrokes through the control
//! socket, records the take with `ctl video … full pace`, and asserts on the
//! pixels — through `tools/paint-conformance/paint_probe.sh`, the same driver
//! and scanner the release cut's paint smoke uses, so the two gates cannot
//! drift apart.
//!
//! THE MATRIX (each row a test):
//!   1. main-screen /bin/sh prompt typing            → effect ink PRESENT
//!   2. alt-screen fake-Claude shape (DEC-2026 bracketed repaints + DECTCEM
//!      hide + 150ms spinner + per-key echo), typing → effect ink PRESENT
//!   3. alt-screen cold spinner, NOTHING typed       → ZERO ink in EVERY frame
//!   4. alt-screen ESC7/ESC8 token streamer, typing  → effect ink PRESENT
//!      (the "unowned batch" cadence that tripped the ownership fence; kept
//!      as a matrix row so that regression class stays dead)
//!
//! INK means dynamic saturated pixels: saturated AND changing against frame 0
//! (static syntax color is saturated but byte-identical; echoed monochrome
//! glyphs change but are unsaturated; only an effect is both). The thresholds
//! live in the probe (and are re-pinned here): a healthy take carries ≥150
//! dynamic saturated px across ≥4 of 12 hue buckets — measured 2026-08-24 at
//! ~1450-1650 px / 9-10 buckets on a healthy HEAD, and exactly 0 on the dark
//! control, so the floor has ~10x margin on one side and no noise on the other.
//! (Calibration note: the ≥150/≥4 floor gates the recorded TAKE; a single
//! 584x350 headless frame of a healthy trail peaks at ~22 saturated px, so a
//! per-frame floor of 150 would be vacuously red on healthy builds.)
//!
//! WIRING: the pre-push gate covers this matrix through the `guards` lane of
//! `xtask gate lint` (which `.githooks/pre-push` runs on every push) —
//! `tools/paint_guard.sh` re-runs `cargo test -p aterm-conformance --test
//! paint` whenever the paint-relevant trees (aterm-effects / aterm-render /
//! aterm-gui / this gate itself) differ from the last take it proved green.
//!
//! macOS-ONLY LANE, honestly: the scanner decodes frames by shelling to
//! `sips`, so elsewhere the suite compiles to one loud not-run notice instead
//! of a silent green (see `paint_matrix_is_a_macos_only_lane` below).
//!
//! The binary under test is `target/release/aterm` — the RELEASE profile, the
//! bits a cut would ship (audit rule 1: parts of the old proof ran debug
//! binaries). If it is stale this test rebuilds it (`cargo build --release -p
//! aterm`, once per run); `ATERM_PAINT_BIN=<path>` overrides for driving a
//! specific artifact.

/// The honest answer everywhere the lane cannot run. NOT a pass in disguise:
/// it prints exactly what was not proven, so a green run on Linux/Windows can
/// never be read as "the paint matrix held" — it says out loud that nothing
/// about paint was measured here.
#[cfg(not(target_os = "macos"))]
#[test]
fn paint_matrix_is_a_macos_only_lane() {
    eprintln!(
        "paint: NOT RUN — the paint-conformance matrix is a macOS-only lane (the scanner \
         decodes recorded frames via `sips`, and the artifact under judgment is the macOS \
         bundle's binary). NOTHING about effect paint was proven on this platform; the matrix \
         runs on the mac that cuts releases, via tools/paint_guard.sh and the release smoke."
    );
}

#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
#[cfg(target_os = "macos")]
use std::sync::{Mutex, OnceLock};

/// The workspace root, from this crate's own manifest dir — the probe script
/// and the release binary are both addressed from it.
#[cfg(target_os = "macos")]
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/aterm-conformance has a workspace root two levels up")
        .to_path_buf()
}

/// Resolve — and, once per test run, freshen — the RELEASE binary under test.
///
/// `cargo build --release -p aterm` is the staleness check: warm and unchanged
/// it is a no-op costing well under a second, and it is exactly what the
/// blackout audit demands (rule 1 — a release-claiming proof runs the RELEASE
/// profile, not whatever binary happens to be lying around). `ATERM_PAINT_BIN`
/// bypasses the build for callers that already hold the artifact to judge
/// (the release smoke drives the just-built bundle through the same probe).
#[cfg(target_os = "macos")]
fn release_bin() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Ok(p) = std::env::var("ATERM_PAINT_BIN") {
            let p = PathBuf::from(p);
            assert!(
                p.is_file(),
                "ATERM_PAINT_BIN={} does not exist",
                p.display()
            );
            return p;
        }
        let root = workspace_root();
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        // Under the branded driver a nested invocation must name its own lane
        // (`targo --unverified`): the outer authorization does not propagate,
        // and a bare `targo build` here would refuse and fail the whole matrix
        // for a reason that has nothing to do with paint.
        let mut cmd = Command::new(&cargo);
        if Path::new(&cargo)
            .file_stem()
            .is_some_and(|n| n.to_string_lossy().starts_with("targo"))
        {
            cmd.arg("--unverified");
        }
        let status = cmd
            .args(["build", "--release", "-p", "aterm"])
            .current_dir(&root)
            .status()
            .unwrap_or_else(|e| panic!("could not spawn `{cargo} build --release -p aterm`: {e}"));
        assert!(
            status.success(),
            "`{cargo} build --release -p aterm` failed ({status}) — the paint matrix judges the \
             RELEASE binary and refuses to run without one (set ATERM_PAINT_BIN to drive a \
             prebuilt artifact)"
        );
        let bin = match std::env::var("CARGO_TARGET_DIR") {
            Ok(t) => {
                let t = PathBuf::from(t);
                let t = if t.is_absolute() { t } else { root.join(t) };
                t.join("release/aterm")
            }
            Err(_) => root.join("target/release/aterm"),
        };
        assert!(
            bin.is_file(),
            "built the release profile but {} is missing",
            bin.display()
        );
        bin
    })
    .clone()
}

/// What one matrix row expects of its take.
#[cfg(target_os = "macos")]
enum Expect {
    /// Effect ink present: ≥150 dynamic saturated px across the take, ≥4 of
    /// 12 hue buckets, and a visible clump in some single frame.
    Ink,
    /// The dark control: ZERO dynamic saturated px in EVERY frame.
    Dark,
}

/// Drive one shape through `tools/paint-conformance/paint_probe.sh` and
/// assert its verdict.
///
/// Serialized: each case launches its own headless instance and records
/// video, and two concurrent recorders would contend for frames and CPU —
/// a paint gate must never fail because of its own harness load.
#[cfg(target_os = "macos")]
fn probe(shape: &str, keys: &str, expect: Expect) {
    static SERIAL: Mutex<()> = Mutex::new(());
    let _take_turns = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    let bin = release_bin();
    let root = workspace_root();
    let script = root.join("tools/paint-conformance/paint_probe.sh");
    assert!(script.is_file(), "{} is missing", script.display());

    let (expect_arg, what) = match expect {
        Expect::Ink => ("ink", "effect ink present"),
        Expect::Dark => ("dark", "zero effect ink in every frame"),
    };
    let mut cmd = Command::new(&script);
    cmd.arg(&bin)
        .args(["--shape", shape, "--record", "5", "--expect", expect_arg])
        .args(["--min-ink", "150", "--min-hues", "4", "--budget", "180"]);
    if !keys.is_empty() {
        cmd.args(["--keys", keys]);
    }
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("could not spawn {}: {e}", script.display()));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let report = stdout
        .lines()
        .rev()
        .find(|l| l.starts_with("PAINT"))
        .unwrap_or("<no PAINT report line>")
        .to_string();
    match out.status.code() {
        Some(0) => eprintln!("paint[{shape}]: {report}"),
        Some(1) => panic!(
            "PAINT CONFORMANCE FAILED [{shape}]: expected {what}, and the recorded take says \
             otherwise — the shipped binary does not paint its flagship effect \
             (docs/RELEASE-PROOF-DISCIPLINE.md).\n  {report}\n--- probe stderr ---\n{stderr}"
        ),
        Some(2) => panic!(
            "PAINT CONFORMANCE COULD NOT RUN [{shape}]: the probe decided nothing, which is not \
             a pass.\n  {report}\n--- probe stderr ---\n{stderr}"
        ),
        code => panic!(
            "paint probe [{shape}] died abnormally (exit {code:?}, the protocol is 0/1/2)\n\
             --- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
        ),
    }
}

/// Matrix row 1: a human typing at a main-screen /bin/sh prompt leaves trail ink.
#[cfg(target_os = "macos")]
#[test]
fn main_screen_prompt_typing_paints_trail_ink() {
    probe("prompt", "e,c,h,o,space,h,e,l,l,o", Expect::Ink);
}

/// Matrix row 2: typing into the fake-Claude alt-screen shape (DEC-2026
/// bracketed repaints + DECTCEM hide + 150ms spinner + per-key echo) leaves
/// trail ink — the exact shape both dark releases were never proven against.
#[cfg(target_os = "macos")]
#[test]
fn alt_screen_fake_claude_typing_paints_trail_ink() {
    probe("fake-claude", "r,a,i,n,b,o,w,space,o,n", Expect::Ink);
}

/// Matrix row 3, the dark control: an alt-screen spinner repainting on its own
/// with NOTHING typed earns zero effect ink in every frame. This row is what
/// makes the other rows falsifiable — a scanner that sees ink here is
/// measuring noise, not the effect.
#[cfg(target_os = "macos")]
#[test]
fn alt_screen_cold_spinner_paints_zero_ink() {
    probe("cold-spinner", "", Expect::Dark);
}

/// Matrix row 4: typing beside an ESC7/ESC8 token streamer (unowned batches
/// landing away from the caret several times a second) leaves trail ink.
/// Verified live 2026-08-24 against HEAD 7cbf4651 ("an unowned batch retires
/// what it invalidates"): the no-row-probe fence fix holds under this shape.
#[cfg(target_os = "macos")]
#[test]
fn alt_screen_esc7_esc8_streamer_typing_paints_trail_ink() {
    probe("streamer", "h,e,l,l,o,space,w,o,r,l,d", Expect::Ink);
}
