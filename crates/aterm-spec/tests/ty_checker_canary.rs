// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Does the DISCOVERED `ty` still find a counterexample with its OWN default
//! reductions on?
//!
//! Deliberately UNARMED — every other `ty check` in this workspace routes through
//! `verify::arm_whole_space_check`, and this one must not, because it is checking
//! the CHECKER rather than a model.
//!
//! ## Why this exists
//!
//! On 2026-08-06 `spec_xref_closure` went red with a dead-action tier
//! disagreement. The root cause was a driver that had not armed reduction off —
//! fixed — but underneath it was something the fix only routes around: the `ty`
//! that `find_trust_bin` selects on this machine has an **unsound partial-order
//! reduction**. Given `RainbowJumpBurstLifecycle` at `Buggy = 1`, where
//! `NoLostFadePayload` is violated three steps from `Init`, it collapses the
//! 128-state space to one, prints `Model checking complete: No errors found
//! (exhaustive).` under a `Soundness mode: Sound` banner, and exits 0.
//!
//! Two fix commits have now routed around that binary without ever naming it.
//! This names it.
//!
//! ## Why a NOTICE and not a failure
//!
//! The house idiom for a toolchain-state problem is the one AGENTS.md sets for
//! the escalation tier: *"print a prominent notice and return early where the
//! tool is absent — never a silent false `ok`, and never claimed as discharged."*
//! A red suite would also make every unrelated change look broken, and the
//! remedy here is one rebuild command on a toolchain this repo does not own.
//!
//! The soundness of aterm's gates does NOT rest on this test: every `ty check`
//! arms reduction off (`ty_drivers_are_armed` enforces that structurally), so a
//! broken reduction cannot reach a verdict. This is the smoke detector for the
//! day someone reaches for `ty` without the arming.

use std::process::Command;

use aterm_spec::derive::rainbow_jump_burst_lifecycle_model;
use aterm_spec::{interp, verify};

#[test]
fn discovered_ty_finds_a_counterexample_under_its_own_reductions() {
    let Some(ty) = verify::ty_escalation("ty partial-order-reduction soundness canary") else {
        return; // tool absent — the escalation tier's documented early return
    };
    // A model with a REAL violation close to Init: `Buggy = 1` makes `BeginFade`
    // drop the fade payload, violating `NoLostFadePayload` at depth 3.
    let m = interp::with_buggy(&rainbow_jump_burst_lifecycle_model(), 1);
    assert!(
        interp::bmc(&m).is_err(),
        "canary fixture must actually be violated at Buggy = 1 — if this fires, the model \
         changed and the canary is measuring nothing"
    );

    let dir = std::env::temp_dir().join(format!("aterm_ty_canary_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk tmp dir");
    let tla = dir.join(format!("{}.tla", m.name));
    let cfg = dir.join(format!("{}.cfg", m.name));
    std::fs::write(&tla, m.to_tla()).expect("write tla");
    std::fs::write(&cfg, m.to_cfg()).expect("write cfg");

    // ty-driver-unarmed: this driver's SUBJECT is the checker's own default
    // reduction behaviour, so arming it would test nothing. The sole exemption
    // `ty_drivers_are_armed` permits, and that gate pins it to this file.
    let out = Command::new(&ty)
        .arg("check")
        .arg(&tla)
        .arg("--config")
        .arg(&cfg)
        .output()
        .unwrap_or_else(|e| panic!("failed to run {ty:?}: {e}"));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);

    let found_it = !out.status.success() || combined.contains("is violated");
    if found_it {
        return;
    }
    eprintln!(
        "\n=====================================================================\n\
         UNSOUND MODEL CHECKER ON THIS MACHINE — action required\n\
         =====================================================================\n\
         {}\n\
         reports a CLEAN, \"exhaustive\" verdict on `{}` at Buggy = 1, whose invariant\n\
         `NoLostFadePayload` the in-process interpreter violates three steps from Init.\n\
         Its partial-order reduction collapses the reachable space and it still prints\n\
         `Soundness mode: Sound`.\n\n\
         aterm's gates are NOT relying on it: every `ty check` in this workspace arms\n\
         `--no-auto-por` (enforced by `ty_drivers_are_armed`), so no verdict here rests\n\
         on the broken reduction. But a checker that answers \"proved\" about spaces it\n\
         never entered should not be the one this machine discovers.\n\n\
         REMEDY:  cargo build --release -p tla-cli      (in $HOME/trust/first-party/ty)\n\
         A correct build already exists on this disk:\n\
         $HOME/trust/build/<triple>/stage2/bin/ty   (measured: POR 0/127 reduced, 128 states)\n\
         `find_trust_bin` probes the first-party path first, so rebuilding there is what\n\
         changes which binary is selected.\n\
         =====================================================================\n\n{combined}",
        verify::ty_evidence_header(&ty).trim_end(),
        m.name,
    );
}
