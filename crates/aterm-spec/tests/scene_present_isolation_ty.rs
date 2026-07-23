// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Proof-Carrying Performance: the terminal is ISOLATED from the decorative Scenes layer.
//!
//! The v0.5.9 typing lag was a decorative subsystem starving the terminal: the animated Scenes
//! HUD (a) leaked its build buffer (~150 sprites/present, never cleared → re-sliced cost climbed
//! 26ms→130ms) and (b) ran that growing build SYNCHRONOUSLY on the terminal's output→present
//! path, so every keystroke echo paid it. The fix builds each frame from a cleared buffer AND
//! moves the whole build onto a WORKER thread, so the decorative cost ON the echo's present path
//! is ZERO (`crates/aterm-gui/src/scene_panel.rs`).
//!
//! `crates/aterm-spec-models/specs/ScenePresentIsolation.tla` models this with two invariants —
//! `SceneFrameBounded` (the buffer never grows past its ceiling) and `DecorativeLatencyBounded`
//! (a pending echo's present latency is the terminal's own cost, with the decorative layer
//! contributing nothing) — under the Buggy-constant prove-and-catch convention
//! (`tests/present_coalescing_ty.rs`):
//!
//!   * `Buggy = 0` (the fix, worker-isolated + bounded): `ty` PROVES both, EXHAUSTIVELY.
//!   * `Buggy = 1` (the old code, on-path + leaking): `ty` finds a COUNTEREXAMPLE — a pending
//!     echo whose latency exceeds the bound because the growing scene buffer is on its present
//!     path — a genuine, non-vacuous catch of the exact regression class.
//!
//! Batteries-on: an absent `ty` FAILS this test (like every Trust gate in the repo), so a green
//! run proves `ty` actually model-checked both configs. The runnable, ty-free half of this
//! certificate — driving the REAL default scene stack — is
//! `aterm_gui::scene_panel::tests::scene_build_is_bounded_and_leak_free`.

use aterm_spec::verify::ty_escalation;
use std::path::{Path, PathBuf};
use std::process::Command;

/// `crates/aterm-spec/tests/` → `crates/aterm-spec-models/specs/ScenePresentIsolation.tla`.
fn spec_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../aterm-spec-models/specs/ScenePresentIsolation.tla")
}

/// Run `ty check <spec> --config <cfg> [extra...]`; return (exit-0, combined output).
fn run(ty: &PathBuf, spec: &Path, cfg: &Path, extra: &[&str]) -> (bool, String) {
    let out = Command::new(ty)
        .arg("check")
        .arg(spec)
        .arg("--config")
        .arg(cfg)
        .args(extra)
        .output()
        .expect("run ty check");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}

#[test]
fn scene_present_isolation_proves_terminal_untouched_and_catches_the_leak() {
    // TIERED (VERIFY-1): a hand-written `.tla` — external-tool obligation;
    // runs only where the Trust toolchain is installed (loud notice otherwise).
    let Some(ty) = ty_escalation("ScenePresentIsolation (terminal-vs-decorative isolation) spec")
    else {
        return;
    };
    let spec = spec_path();
    assert!(
        spec.exists(),
        "missing committed spec {} — the scene-isolation PCP certificate",
        spec.display()
    );
    let okcfg = spec.with_extension("cfg"); // ScenePresentIsolation.cfg (Buggy = 0)

    // Buggy = 0 (the fix): both invariants hold under an EXHAUSTIVE (uncapped) BFS — a
    // decorative subsystem, however heavy, never inflates the terminal's echo latency.
    let (ok, out) = run(&ty, &spec, &okcfg, &["--require-exhaustive"]);
    assert!(
        ok,
        "ScenePresentIsolation (Buggy=0, the fix) must model-check clean \
         (SceneFrameBounded + DecorativeLatencyBounded)\n{out}"
    );
    assert!(
        out.contains("exhaustive"),
        "the prove run must be an exhaustive (uncapped) BFS, not a bounded sample\n{out}"
    );

    // Buggy = 1 (the OLD code: on-path, leaking build): MUST yield a counterexample. The cfg
    // flips only the Buggy constant, so the catch is attributable to that single behaviour.
    let dir = std::env::temp_dir().join(format!("aterm-pcp-scene-iso-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk tempdir");
    let bugcfg = dir.join("ScenePresentIsolation.bug.cfg");
    let oktext = std::fs::read_to_string(&okcfg).expect("read committed cfg");
    let bugtext = oktext.replace("Buggy = 0", "Buggy = 1");
    assert_ne!(oktext, bugtext, "cfg must contain `Buggy = 0` to flip");
    std::fs::write(&bugcfg, bugtext).expect("write bug cfg");
    let (bug_ok, bug_out) = run(&ty, &spec, &bugcfg, &[]);
    assert!(
        !bug_ok,
        "ScenePresentIsolation (Buggy=1, old code) MUST yield a counterexample \
         (decorative cost on the terminal's present path)\n{bug_out}"
    );
    // The catch must be the isolation theorem failing — a decorative-cost-inflated echo latency —
    // not an unrelated invariant, so a future regression can't masquerade as a genuine catch.
    assert!(
        bug_out.contains("DecorativeLatencyBounded") || bug_out.contains("SceneFrameBounded"),
        "Buggy=1 must violate the isolation/bound invariants specifically\n{bug_out}"
    );
    let _ = std::fs::remove_dir_all(&dir);

    eprintln!(
        "PCP ScenePresentIsolation: terminal isolation proven (Buggy=0, exhaustive) and \
         decorative-cost-on-present-path caught (Buggy=1 → counterexample)."
    );
}
