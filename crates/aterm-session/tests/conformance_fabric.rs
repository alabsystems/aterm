// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Derived-spec models for the routing fabric's two bounded state machines, per
//! `AGENTS.md`: the per-edge FAIL-CLOSED gate (`decide_edge`) and the SinkWriter
//! NO-SILENT-LOSS write loop. Each is authored with `ty_model!` (one source → both
//! the `ty`-checkable spec and the executable transition semantics), exercised by
//! the in-process interpreter for a proves-side (invariant holds) and a catches-side
//! (a `Buggy` variant reaches a violation), and — for the gate — bound to the REAL
//! shipping `decide_edge` with a non-vacuous negative control (Tier-1 conformance).
//!
//! ## Verification status (honest, per AGENTS.md)
//!
//! The EXHAUSTIVE TLA+ Tier-0 model-check (`assert_model_checks`/
//! `assert_proves_and_catches`) is **authored-pending-build**: the `ty` binary is not
//! on PATH in this environment, so the all-states check cannot be claimed to have
//! run. What DOES run here, with no external tool, is (a) `to_tla()` rendering of the
//! derived spec and (b) the interpreter-driven proves/catches below — the model's
//! transition semantics executed in Rust — plus the Tier-1 binding to real code.

use aterm_session::{EdgeDecision, EdgeTable, EdgeToken, LaunchNonce, Op, SessionId, decide_edge};
use aterm_spec::derive::Model;
use aterm_spec::ty_model;

// ---------------------------------------------------------------------------
// Model 1 — the per-edge fail-closed gate.
//
// Abstraction: `granted` is 1 iff a matching (token, dst, op, nonce) grant exists;
// `decision` is the gate's output (1 = Permit, 0 = Deny), RECOMPUTED from `granted`
// in the same step that changes it (the real gate is stateless / per-call, so the
// decision is never stale). Fail-closed is `decision <= granted`: a Permit requires
// a grant. The `Buggy` variant permits even with no grant (the missing default-deny
// arm) and must be caught.
// ---------------------------------------------------------------------------

fn edge_gate_model() -> Model {
    ty_model! {
        EdgeGate {
            const Buggy = 0;
            var granted = 0;
            var decision = 0;
            action Grant when (granted <= 0) {
                granted = 1;
                decision = if 1 + Buggy > 0 { 1 } else { 0 };
            }
            action Revoke when (granted > 0) {
                granted = 0;
                decision = if 0 + Buggy > 0 { 1 } else { 0 };
            }
            invariant FailClosed: decision <= granted;
        }
    }
}

fn edge_gate_model_buggy() -> Model {
    ty_model! {
        EdgeGateBuggy {
            const Buggy = 1;
            var granted = 0;
            var decision = 0;
            action Grant when (granted <= 0) {
                granted = 1;
                decision = if 1 + Buggy > 0 { 1 } else { 0 };
            }
            action Revoke when (granted > 0) {
                granted = 0;
                decision = if 0 + Buggy > 0 { 1 } else { 0 };
            }
            invariant FailClosed: decision <= granted;
        }
    }
}

#[test]
fn edge_gate_renders_tla_and_proves_and_catches() {
    let m = edge_gate_model();
    // The derived spec renders (the artifact `ty` would check when available).
    assert!(
        m.to_tla().contains("FailClosed"),
        "derived TLA+ must carry the invariant"
    );

    // PROVES: across a Grant/Revoke cycle the recomputed decision never permits
    // without a grant.
    let mut st = m.init_state();
    assert!(
        m.check_invariant("FailClosed", &st),
        "init is fail-closed (Deny)"
    );
    assert!(m.fire("Grant", &mut st));
    assert!(
        m.check_invariant("FailClosed", &st),
        "Grant -> Permit, with a grant"
    );
    assert!(m.fire("Revoke", &mut st));
    assert!(
        m.check_invariant("FailClosed", &st),
        "Revoke recomputes the decision to Deny"
    );
    assert!(m.fire("Grant", &mut st));
    assert!(
        m.check_invariant("FailClosed", &st),
        "re-Grant -> Permit again"
    );

    // CATCHES: the Buggy variant (permit even with no grant) reaches a VIOLATING
    // state — Grant then Revoke leaves decision = Permit while granted = 0.
    let b = edge_gate_model_buggy();
    let mut bs = b.init_state();
    assert!(b.fire("Grant", &mut bs));
    assert!(b.fire("Revoke", &mut bs));
    assert!(
        !b.check_invariant("FailClosed", &bs),
        "a permit-on-no-grant gate MUST be catchable (decision=Permit, granted=0)"
    );
}

// ---------------------------------------------------------------------------
// Tier-1 conformance — bind the abstract gate to the SHIPPING `decide_edge`.
// ---------------------------------------------------------------------------

#[test]
fn decide_edge_conforms_to_fail_closed_gate_with_negative_control() {
    let src = SessionId::new("s-src");
    let dst = SessionId::new("s-dst");
    let other = SessionId::new("s-other");
    let nonce = LaunchNonce::from_bytes([3u8; 16]);
    let stale = LaunchNonce::from_bytes([4u8; 16]);

    let mut tbl = EdgeTable::new();
    let read_tok = tbl.grant(src.clone(), dst.clone(), Op::ReadScreen, nonce);
    let write_tok = tbl.grant(src.clone(), dst.clone(), Op::WriteInput, nonce);

    // Project a real query onto the abstract (granted, decision) and assert the
    // binding-level FailClosed: a Permit implies a matching grant exists, and an
    // exact matching grant permits.
    let check = |tok: &EdgeToken, d: &SessionId, op: Op, n: &LaunchNonce, matching: bool| {
        let permit = decide_edge(&tbl, tok, d, op, n) == EdgeDecision::Permit;
        assert!(
            !permit || matching,
            "decide_edge PERMITTED without a matching grant (op={op:?}, matching={matching})"
        );
        if matching {
            assert!(permit, "an exact matching grant must permit (op={op:?})");
        }
    };

    // Matching grants permit.
    check(&read_tok, &dst, Op::ReadScreen, &nonce, true);
    check(&write_tok, &dst, Op::WriteInput, &nonce, true);

    // NEGATIVE CONTROL (non-vacuous): the op-scope split (§7.2) — a READ token on a
    // WRITE op (and vice-versa) must DENY, never silently widen.
    check(&read_tok, &dst, Op::WriteInput, &nonce, false);
    check(&read_tok, &dst, Op::Signal, &nonce, false);
    check(&write_tok, &dst, Op::ReadScreen, &nonce, false);
    // Wrong dst (a token minted for B cannot drive A), stale nonce (target restarted),
    // and an unknown token all DENY.
    check(&write_tok, &other, Op::WriteInput, &nonce, false);
    check(&write_tok, &dst, Op::WriteInput, &stale, false);
    check(&EdgeToken::generate(), &dst, Op::WriteInput, &nonce, false);
}

// ---------------------------------------------------------------------------
// Model 2 — the SinkWriter no-silent-loss write loop.
//
// Abstraction: a frame of `Frame` bytes is written one accepted chunk at a time;
// `written` accumulates accepted bytes, `lost` accumulates dropped tail. The fixed
// `write_some` loop never drops (`lost` stays 0). The `Buggy` variant models the OLD
// `write_all`, which `break`-dropped the unwritten tail on a short write — caught by
// NoLoss.
// ---------------------------------------------------------------------------

fn sink_no_loss_model() -> Model {
    ty_model! {
        SinkNoLoss {
            const Frame = 4;
            const Buggy = 0;
            var written = 0;
            var lost = 0;
            action Step when (written + lost <= Frame - 1) {
                written = written + 1;
                lost = if Buggy > 0 { Frame - written } else { lost };
            }
            invariant NoLoss: lost <= 0;
            invariant Accounted: written + lost <= Frame;
        }
    }
}

fn sink_no_loss_model_buggy() -> Model {
    ty_model! {
        SinkNoLossBuggy {
            const Frame = 4;
            const Buggy = 1;
            var written = 0;
            var lost = 0;
            action Step when (written + lost <= Frame - 1) {
                written = written + 1;
                lost = if Buggy > 0 { Frame - written } else { lost };
            }
            invariant NoLoss: lost <= 0;
            invariant Accounted: written + lost <= Frame;
        }
    }
}

#[test]
fn sink_no_loss_proves_and_buggy_tail_drop_is_caught() {
    // PROVES: writing a whole frame one chunk at a time never loses a byte.
    let m = sink_no_loss_model();
    let mut st = m.init_state();
    let mut steps = 0;
    while m.fire("Step", &mut st) {
        assert!(m.check_invariant("NoLoss", &st), "no byte is ever dropped");
        assert!(
            m.check_invariant("Accounted", &st),
            "bytes are accounted, never exceed the frame"
        );
        steps += 1;
        assert!(steps <= 8, "loop must terminate at the frame boundary");
    }
    assert_eq!(steps, 4, "all Frame=4 bytes are written, none lost");

    // CATCHES: the old silent-tail-drop (write_all break-on-short-write) violates NoLoss.
    let b = sink_no_loss_model_buggy();
    let mut bs = b.init_state();
    assert!(b.fire("Step", &mut bs));
    assert!(
        !b.check_invariant("NoLoss", &bs),
        "a tail-dropping sink MUST be catchable (lost > 0 after a short write)"
    );
}

// ===========================================================================
// Tier-0 IN TRUST — the SAME `ty_model!`-derived specs, now exhaustively
// model-checked by the real Trust-bundled `ty` ($HOME/trust/first-party/ty). This is
// the "TLA+ spec auto-generated from code and linked to it, model-checked by
// Trust" guarantee: the model is Rust, `to_tla()` emits the spec, `ty check`
// proves the invariant over the WHOLE bounded state space, and the Buggy=1 cfg
// MUST yield a counterexample — so the invariant is non-trivial AND catches the
// real defect. VERIFICATION GATE (honesty ratchet, batteries-on, see
// `aterm_spec::verify`): verification is always required — an absent Trust `ty` FAILS
// the test with a build hint (`cargo build --release -p tla-cli` in
// $HOME/trust/first-party/ty).
// ===========================================================================

use std::path::PathBuf;
use std::process::Command;

use aterm_spec::verify::ty_escalation;

fn run_ty_check(ty: &PathBuf, spec: &std::path::Path, cfg: &std::path::Path) -> (bool, String) {
    let mut cmd = Command::new(ty);
    cmd.arg("check").arg(spec).arg("--config").arg(cfg);
    // ARMED: partial-order reduction is a speed feature that does not
    // preserve coverage, and the `ty` this machine discovers has an
    // UNSOUND one (it reduced a 128-state model to 1 and still printed
    // "No errors found (exhaustive)"). Every `ty check` in this workspace
    // arms the whole space through the one shared place.
    aterm_spec::verify::arm_whole_space_check(&mut cmd);
    let out = cmd.output().expect("run ty check");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}

/// Emit the derived spec, then assert `ty` PROVES the invariant at the committed
/// `Buggy=0` and finds a COUNTEREXAMPLE at `Buggy=1` (via `to_cfg_with`) — both
/// exhaustively, in Trust.
fn assert_proves_and_catches_in_trust(ty: &PathBuf, m: &Model) {
    let dir = std::env::temp_dir().join(format!("aterm-fabric-{}-{}", m.name, std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk tempdir");
    let spec = dir.join(format!("{}.tla", m.name));
    std::fs::write(&spec, m.to_tla()).expect("write derived spec");

    let ok_cfg = dir.join("ok.cfg");
    std::fs::write(&ok_cfg, m.to_cfg()).expect("write ok cfg");
    let (ok, out) = run_ty_check(ty, &spec, &ok_cfg);
    assert!(
        ok,
        "derived {} (Buggy=0) must model-check clean in ty\n--- {}.tla ---\n{}\n--- ty ---\n{out}",
        m.name,
        m.name,
        m.to_tla()
    );

    let bug_cfg = dir.join("bug.cfg");
    std::fs::write(&bug_cfg, m.to_cfg_with(&[("Buggy", 1)])).expect("write bug cfg");
    let (bug_ok, bug_out) = run_ty_check(ty, &spec, &bug_cfg);
    assert!(
        !bug_ok,
        "derived {} (Buggy=1) MUST yield a counterexample in ty\n{bug_out}",
        m.name
    );

    let _ = std::fs::remove_dir_all(&dir);
    eprintln!(
        "TRUST: derived {} proven (Buggy=0) and caught (Buggy=1) by ty.",
        m.name
    );
}

/// The per-edge FAIL-CLOSED gate, model-checked by Trust: a Permit requires a
/// grant, exhaustively over the whole state space; the permit-on-no-grant bug is
/// caught.
#[test]
fn edge_gate_spec_model_checked_in_trust() {
    // TIERED (VERIFY-1): external-tool obligation; runs only with the toolchain.
    let Some(ty) = ty_escalation("edge_gate fail-closed spec") else {
        return;
    };
    assert_proves_and_catches_in_trust(&ty, &edge_gate_model());
}

/// The SinkWriter NO-SILENT-LOSS loop, model-checked by Trust: no byte is dropped;
/// the silent-tail-drop bug is caught.
#[test]
fn sink_no_loss_spec_model_checked_in_trust() {
    // TIERED (VERIFY-1): external-tool obligation; runs only with the toolchain.
    let Some(ty) = ty_escalation("sink no-loss spec") else {
        return;
    };
    assert_proves_and_catches_in_trust(&ty, &sink_no_loss_model());
}
