// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `scripts/verify-kani-proofs.sh --self-test`, in the merge contract.
//!
//! The Kani floor itself runs only under `--full` and only where trust-mc and
//! ay are installed, but the one piece of judgement in that script — which
//! harnesses are config-requiring, and which of those the explicit lane runs by
//! name with their own `#[kani::unwind]` — needs no model checker. Until
//! 2026-09-25 nothing ran it, so a renamed SIMD harness would have dropped out of
//! the explicit lane with no test failing on any box.
#![cfg(unix)]

use std::path::Path;
use std::process::Command;

#[test]
fn the_kani_script_classifier_and_explicit_lane_hold_without_trust_mc() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/aterm-verify sits two levels under the root");
    let out = Command::new("/bin/bash")
        .arg(root.join("scripts/verify-kani-proofs.sh"))
        .arg("--self-test")
        .output()
        .expect("bash runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("SELF-TEST: PASS"),
        "verify-kani-proofs.sh --self-test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The explicit lane is non-empty and names the three SIMD offset/range proofs.
    for name in [
        "simd_avx2_offset_no_overflow",
        "simd_neon_offset_no_overflow",
        "simd_scalar_fallback_range_valid",
    ] {
        assert!(
            stdout.contains(name),
            "the explicit lane lost {name}:\n{stdout}"
        );
    }
}
