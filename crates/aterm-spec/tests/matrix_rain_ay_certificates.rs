// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! PHOSPHOR (Matrix digital rain) — the §4/§10 `ay` emission-cap certificate
//! bundle, re-checked fail-closed (docs/matrix-rain-design.md §4/§10).
//!
//! One proof-gated optimization hangs off this gate: **demoting the `emit()`
//! whole-column truncation** (the rollback against `quad_cap` and the
//! `MAX_RAIN_ADD` break) from a correctness-load branch to a cold backstop.
//! GREEN is the license; per design §10 the branch is KEPT in the shipped
//! renderer until this gate runs in CI — refuse, don't silently pass.
//!
//! The bundle proves, over ALL clamped configs/geometries: the effective quad
//! cap lands in `[256, 2048]` (floor + GPU ceiling); the truncated emission is
//! `<= quad_cap <= MAX_RAIN_QUADS`; the halo stream is `<= MAX_RAIN_ADD`;
//! emitted texels never exceed `max(256*cell, 409600)`. SAT controls witness
//! that 2048 and 256 are reachable (tight) and that removing the truncation,
//! the 256 floor, or the halo cap each breaks its bound (load-bearing).
//!
//! VERIFICATION GATE (honesty ratchet, batteries-on, see [`aterm_spec::verify`]):
//! `ay` is discovered by the same canonical bootstrap scan that finds `ty`
//! (`verify::find_ay`). Verification is always required — an absent Trust `ay`
//! FAILS the test with a build hint; there is no env var and no skip path.
//! The bundle's own `verify.sh` (expected-verdict table, SAT non-vacuity
//! controls) is the single source of truth for the obligation list; this test
//! injects the discovered `ay` and asserts the script's verdict.

use std::path::PathBuf;
use std::process::Command;

fn bundle_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../aterm-spec-models/proofs/ay/matrix_rain")
        .canonicalize()
        .expect("matrix_rain certificate bundle exists")
}

/// The full obligation set is sub-second on ay v0.11.0 (linear-only, per the
/// bundle README's frontier note), so there is a single always-on set.
#[test]
fn matrix_rain_ay_certificates_discharge() {
    let ay = aterm_spec::verify::ay("PHOSPHOR §4/§10 ay emission-cap certificates");
    let script = bundle_dir().join("verify.sh");
    let out = Command::new("bash")
        .arg(&script)
        .env("AY", &ay)
        .output()
        .expect("run matrix_rain verify.sh");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "matrix_rain ay certificate bundle FAILED (a stale certificate re-reds \
         the gate and the guarded emit() truncation keeps its guard):\n{combined}"
    );
    // Belt-and-braces: the script prints one PASS per obligation; make sure the
    // expected-verdict table (9 rows) was actually exercised, not short-circuited.
    assert!(
        combined.matches("  PASS  ").count() >= 9,
        "expected >= 9 discharged obligations, got:\n{combined}"
    );
}
