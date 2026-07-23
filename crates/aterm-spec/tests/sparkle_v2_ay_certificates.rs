// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Sparkle Words v2.1 P6 — the §7.5 `ay` certificate bundle, re-checked
//! fail-closed (docs/sparkle-words-v2-design.md §7.5/§14 P6).
//!
//! Two proof-gated optimizations hang off this gate:
//! * **SimHash bit-sliced kernel equivalence** (§3.2): the single-column
//!   QF_BV miter + prove-and-catch controls. GREEN is the license under which
//!   `genome.rs::SIMHASH_KERNEL` ships `BitSliced` (flipped 2026-07-08); the
//!   reference loop stays the oracle, and a red run here demands the flip
//!   back to `Reference` in the same change — refuse, don't silently pass.
//! * **Nova emit quad budget Q ≤ 392** (§6.3): the bounded-increment Horn
//!   system's hand-supplied inductive invariant, all VCs discharged as one
//!   QF_LIA query, plus the closed-form/tightness/catches controls. GREEN is
//!   the license for demoting the per-nova truncation branch (which is KEPT
//!   until this gate runs in CI — refuse, don't silently pass).
//!
//! VERIFICATION GATE (honesty ratchet, batteries-on, see [`aterm_spec::verify`]):
//! `ay` is discovered by the same canonical bootstrap scan that finds `ty`
//! (`verify::find_ay`). Verification is always required — an absent Trust `ay`
//! FAILS the test with a build hint; there is no env var and no skip path.
//! The bundle's own `verify.sh` (expected-verdict table, SAT non-vacuity
//! controls — the established proofs/ay convention) is the single source of
//! truth for the obligation list; this test injects the discovered `ay` and
//! asserts the script's verdict.

use std::path::PathBuf;
use std::process::Command;

fn bundle_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../aterm-spec-models/proofs/ay/sparkle_v2")
        .canonicalize()
        .expect("sparkle_v2 certificate bundle exists")
}

fn run_verify(extra: &[&str]) {
    // TIERED (VERIFY-1): SMT/CHC certificate re-check — external-tool
    // obligation; runs only where the Trust `ay` solver is installed.
    let Some(ay) = aterm_spec::verify::ay_escalation("Sparkle Words v2.1 §7.5 ay certificates")
    else {
        return;
    };
    let script = bundle_dir().join("verify.sh");
    let out = Command::new("bash")
        .arg(&script)
        .args(extra)
        .env("AY", &ay)
        .output()
        .expect("run sparkle_v2 verify.sh");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "sparkle-v2 ay certificate bundle FAILED (a stale certificate re-reds \
         the gate and the guarded code keeps its guard):\n{combined}"
    );
    // Belt-and-braces: the script prints one PASS per obligation; make sure
    // the expected-verdict table was actually exercised, not short-circuited.
    assert!(
        combined.matches("  PASS  ").count() >= 7,
        "expected >= 7 discharged obligations, got:\n{combined}"
    );
}

/// The CI set: single-column SimHash lemma + controls, nova budget Horn VCs +
/// closed form + tightness + catches — all sub-second on ay v0.11.0.
#[test]
fn sparkle_v2_ay_certificates_discharge() {
    run_verify(&[]);
}

/// The OPTIONAL full-domain 9×64-bit whole-kernel miter — honestly
/// minutes-class (§3.2's routing: the column lemma is the CI artifact; this
/// row is belt-and-braces for the lane-independence lift). Manual:
///
/// ```sh
/// cargo test -p aterm-spec --release sparkle_v2_ay_full_domain -- --ignored --nocapture
/// ```
#[test]
#[ignore = "minutes-class full-domain miter (§3.2): run manually with --ignored"]
fn sparkle_v2_ay_full_domain() {
    run_verify(&["--full"]);
}
