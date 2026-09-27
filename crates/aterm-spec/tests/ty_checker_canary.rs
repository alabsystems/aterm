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

use aterm_spec::derive::Model;
use aterm_spec::{interp, verify};

/// THE FIXTURE: the retired v1 rainbow jump-burst ring, `RainbowJumpBurstLifecycle`.
///
/// It no longer describes any code — the burst ring it modelled was deleted with
/// the v1 rainbow kitty (`48ea44608`), and the model left the registry with it —
/// so it lives here and only here, as a CHECKER fixture: its shape (a stutter
/// action, `SlowJump`, beside a violation three steps from `Init`) is the one
/// that exposed the unsound reduction, and the incident histories in
/// `aterm_spec::verify` name it by this module name.
fn unsound_reduction_fixture() -> Model {
    aterm_spec::ty_model! {
        RainbowJumpBurstLifecycle {
            const BurstCap = 3;
            const TotalCap = 6;
            const MaxIssued = 6;
            const Buggy = 0;
            var resident = 0;
            var newest = 0;
            var ghost = 0;
            var ghost_newest = 0;
            var issued = 0;
            var wake = 0;
            var lost = 0;

            action FastJump when (issued <= MaxIssued - 1) {
                resident = if resident + 1 > BurstCap { BurstCap } else { resident + 1 };
                newest = if Buggy == 1 && resident + 1 > BurstCap {
                    newest
                } else {
                    issued + 1
                };
                issued = issued + 1;
                wake = 1;
            }
            action SlowJump { resident = resident; }
            action ExpireOne when (resident > 0) {
                resident = resident - 1;
                newest = if resident > 1 { newest } else { 0 };
                wake = if resident - 1 + ghost > 0 { 1 } else { 0 };
            }
            action BeginFade when (resident > 0) {
                ghost = if Buggy == 1 { 0 } else { resident };
                ghost_newest = if Buggy == 1 { 0 } else { newest };
                resident = 0;
                newest = 0;
                wake = if Buggy == 1 { 0 } else { 1 };
                lost = if Buggy == 1 { 1 } else { 0 };
            }
            action FinishFade when (ghost > 0) {
                ghost = 0;
                ghost_newest = 0;
                wake = if resident > 0 { 1 } else { 0 };
            }
            action Reset when (resident + ghost > 0) {
                resident = 0;
                newest = 0;
                ghost = 0;
                ghost_newest = 0;
                wake = 0;
            }

            invariant ResidentBounded: resident <= BurstCap;
            invariant GhostBounded: ghost <= BurstCap;
            invariant TotalBounded: resident + ghost <= TotalCap;
            invariant NewestRetained:
                if resident > 0 { newest == issued } else { newest == 0 };
            invariant GhostIdentityBounded: ghost_newest <= issued;
            invariant NoLostFadePayload: lost == 0;
            invariant WakeMatchesResidents:
                wake == if resident + ghost > 0 { 1 } else { 0 };
            invariant IssuedBounded: issued <= MaxIssued;
            invariant WakeBounded: wake <= 1;
        }
    }
}

#[test]
fn discovered_ty_finds_a_counterexample_under_its_own_reductions() {
    let Some(ty) = verify::ty_escalation("ty partial-order-reduction soundness canary") else {
        return; // tool absent — the escalation tier's documented early return
    };
    // A model with a REAL violation close to Init: `Buggy = 1` makes `BeginFade`
    // drop the fade payload, violating `NoLostFadePayload` at depth 3.
    let m = interp::with_buggy(&unsound_reduction_fixture(), 1);
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
         REMEDY:  aterm pkg install ty   (discovery takes the store's ty first; a build\n\
         of your own is reached by `aterm pkg link ty <checkout>`)\n\
         =====================================================================\n\n{combined}",
        verify::ty_evidence_header(&ty).trim_end(),
        m.name,
    );
}
