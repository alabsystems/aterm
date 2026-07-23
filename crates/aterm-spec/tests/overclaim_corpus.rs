// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The OVERCLAIM CORPUS — no-false-negative confirmation over the known set
//! (TRUST_VACUITY_GATE §3.2/§3.3).
//!
//! Each RED fixture is a deliberately-broken anchor/proof/spec that MUST produce its
//! flag; a GREEN control proves the same machinery passes when the defect is removed.
//! A red fixture that does NOT flag (a false negative) FAILS this meta-test — that is
//! precisely the "no false negatives in the known set" the design asks for.
//!
//! The fixtures map onto the four findings the armed Trust now bites on:
//!   * **L1** (finding 1) — a typo'd `proof_name` that resolves to no harness in the
//!     manifest → `trust-ir spec-link --require-manifest` exit 1, `[L1 proof-resolves]`.
//!   * **L2** (finding 2) — an anchor on an actively-anchored machine with `project=""`
//!     → exit 1, `[L2 projection-present]`.
//!   * **Ob.1 / L3** (finding 4) — an EXTERNAL anchor naming a non-`Next` def
//!     (`TypeOK`) → rejected by BOTH the in-Rust gate (`SpecModule::action_names()` is
//!     now Next-only) AND the lowered artifact (`[Ob.1 action-exists]`).
//!   * **window_routing made-to-fail** (finding 3) — a corrupted `CloseWindow`
//!     transition that the real `ty` validation (the conformance the gate now RUNS)
//!     MUST reject, proving the binding is non-vacuous.
//!
//! Every fixture shells the SAME armed binaries the gate uses (`trust-ir`, `ty`),
//! located by the canonical-path search. Verification is always required
//! (batteries-on, see [`aterm_spec::verify`]): an absent Trust `ty`/`trust-ir` FAILS
//! the test with a build hint; build the toolchain once (`cargo build --release -p
//! tla-cli` in $HOME/trust/first-party/ty).

use std::path::{Path, PathBuf};
use std::process::Command;

use aterm_spec::derive::ring_model;
use aterm_spec::ir::{lower_to_ir, spec_link_report_is_clean};
use aterm_spec::tla_check::TlaSpec;
use aterm_spec::verify::{trust_ir_escalation, ty_escalation};
use aterm_spec::xref::{ProofAnchor, ProofKind, RefinementAnchor, SpecModule};

// ---------------------------------------------------------------------------
// Tool discovery (the honesty ratchet — batteries-on: present or the test FAILS)
// ---------------------------------------------------------------------------

/// Run `trust-ir spec-link <module> [--harness-manifest <m> --require-manifest]`,
/// returning (exit-code, combined stdout+stderr) — or `None` when the Trust
/// toolchain is absent (TIERED, VERIFY-1: spec-link agreement is an
/// external-tool obligation; the caller returns early on `None`, and the
/// prominent notice names what did not run).
fn spec_link(module: &Path, manifest: Option<&Path>) -> Option<(i32, String)> {
    let trust_ir = trust_ir_escalation("overclaim corpus (spec-link)")?;
    let mut cmd = Command::new(&trust_ir);
    // aterm emits the canonical TEXT format (`lower_to_ir`); trust-ir 0.2.0
    // auto-detects the `.trust_ir` extension as BINARY, so pin the format.
    cmd.arg("spec-link").arg("--format").arg("text").arg(module);
    if let Some(m) = manifest {
        cmd.arg("--harness-manifest")
            .arg(m)
            .arg("--require-manifest");
    }
    let out = cmd.output().expect("run trust-ir spec-link");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Some((out.status.code().unwrap_or(-1), combined))
}

/// A structurally clean artifact emitted by `lower_to_ir` is deliberately
/// design-only: it must fail closed with the exact non-certifying exit while
/// carrying no structural/user failure. Exit 0 is reserved for a certifying
/// Trust-IR artifact, and exit 2 is a parse/I/O failure.
fn assert_clean_design_only(code: i32, report: &str, label: &str) {
    assert_eq!(
        code, 1,
        "{label} MUST exit 1 as explicitly non-certifying design-only IR. Report:\n{report}"
    );
    assert!(
        spec_link_report_is_clean(false, report),
        "{label} must be structurally clean with only design-only non-certification; \
         report:\n{report}"
    );
}

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aterm_overclaim_{tag}_{}", std::process::id()));
    std::fs::create_dir_all(&d).expect("mk tmpdir");
    d
}

fn anchor(machine: &'static str, action: &'static str, project: &'static str) -> RefinementAnchor {
    RefinementAnchor {
        machine,
        action,
        rust_method: "fixture::method",
        location: "fixture.rs:1:1",
        project,
    }
}

/// A minimal harness manifest JSON listing exactly the named harnesses.
fn write_manifest(dir: &Path, names: &[&str]) -> PathBuf {
    let entries: Vec<String> = names
        .iter()
        .map(|n| format!("{{ \"name\": \"{n}\", \"span\": \"fixture.rs:1:1\" }}"))
        .collect();
    let json = format!("{{ \"harnesses\": [{}] }}\n", entries.join(", "));
    let p = dir.join("manifest.json");
    std::fs::write(&p, json).expect("write manifest");
    p
}

// ===========================================================================
// RED fixture 1 — L1: a typo'd proof_name resolves to no harness in the manifest.
// ===========================================================================

#[test]
fn red_l1_typo_proof_name_flags() {
    let dir = tmpdir("l1");
    let modules = vec![SpecModule::Embedded(ring_model())];
    let a = anchor("ring", "Push", "aterm_buffer::Ring::project");
    // A proof whose name is NOT in the manifest (a typo / dead harness).
    let typo = ProofAnchor {
        machine: "ring",
        action: "Push",
        proof_name: "ring_push_DOES_NOT_EXIST",
        kind: ProofKind::Kani,
        location: "fixture.rs:1:1",
    };
    let txt = lower_to_ir("red_l1", &modules, &[&a], &[], &[&typo]);
    let path = dir.join("red_l1.trust_ir");
    std::fs::write(&path, &txt).expect("write");
    // Manifest contains a DIFFERENT, real-looking harness — so the typo is unresolved.
    let manifest = write_manifest(&dir, &["ring_push_refines"]);

    let Some((code, report)) = spec_link(&path, Some(&manifest)) else {
        return;
    };
    assert_eq!(
        code, 1,
        "RED L1 fixture (typo proof_name) MUST exit 1 — a false negative otherwise. Report:\n{report}"
    );
    assert!(
        report.contains("[L1 proof-resolves]") && report.contains("ring_push_DOES_NOT_EXIST"),
        "RED L1 must report [L1 proof-resolves] naming the typo'd harness; report:\n{report}"
    );

    // GREEN control: fix the proof_name to one the manifest contains. The
    // structure is clean, while the artifact remains explicitly non-certifying.
    let good = ProofAnchor {
        proof_name: "ring_push_refines",
        ..typo
    };
    let good_txt = lower_to_ir("ctl_l1", &modules, &[&a], &[], &[&good]);
    let good_path = dir.join("ctl_l1.trust_ir");
    std::fs::write(&good_path, &good_txt).expect("write");
    let Some((code, report)) = spec_link(&good_path, Some(&manifest)) else {
        return;
    };
    assert_clean_design_only(code, &report, "GREEN L1 control (resolved proof_name)");
    let _ = std::fs::remove_dir_all(&dir);
}

// ===========================================================================
// RED fixture 2 — L2: an anchor on an actively-anchored machine with project="".
// ===========================================================================

#[test]
fn red_l2_empty_projection_flags() {
    let dir = tmpdir("l2");
    let modules = vec![SpecModule::Embedded(ring_model())];
    // Actively-anchored machine (Ring) with an anchor carrying NO projection.
    let no_proj = anchor("ring", "Push", "");
    let txt = lower_to_ir("red_l2", &modules, &[&no_proj], &[], &[]);
    let path = dir.join("red_l2.trust_ir");
    std::fs::write(&path, &txt).expect("write");

    let Some((code, report)) = spec_link(&path, None) else {
        return;
    };
    assert_eq!(
        code, 1,
        "RED L2 fixture (project=\"\") MUST exit 1 — a false negative otherwise. Report:\n{report}"
    );
    assert!(
        report.contains("[L2 projection-present]") && report.contains("Push"),
        "RED L2 must report [L2 projection-present] for action Push; report:\n{report}"
    );

    // GREEN control: supply a non-empty projection name. The structure is
    // clean, while the artifact remains explicitly non-certifying.
    let with_proj = anchor("ring", "Push", "aterm_buffer::Ring::project");
    let good_txt = lower_to_ir("ctl_l2", &modules, &[&with_proj], &[], &[]);
    let good_path = dir.join("ctl_l2.trust_ir");
    std::fs::write(&good_path, &good_txt).expect("write");
    let Some((code, report)) = spec_link(&good_path, None) else {
        return;
    };
    assert_clean_design_only(code, &report, "GREEN L2 control (non-empty project)");
    let _ = std::fs::remove_dir_all(&dir);
}

// ===========================================================================
// RED fixture 3 — Ob.1 / L3: an EXTERNAL anchor naming a non-Next def (TypeOK).
// Flags in BOTH the in-Rust gate (action_names is now Next-only) AND the artifact.
// ===========================================================================

/// A minimal external `.tla` whose `Next` has ONE disjunct (`Apply`) but which also
/// declares a non-`Next` top-level def `TypeOK` — exactly the L3 shape.
const EXTERNAL_TLA: &str = r#"---- MODULE Fixture ----
VARIABLES x
Init == x = 0
Apply == x' = 1
Next == Apply
TypeOK == x \in {0, 1}
====
"#;

#[test]
fn red_l3_external_anchor_on_typeok_flags_in_rust_and_artifact() {
    let dir = tmpdir("l3");
    let spec = TlaSpec::parse_str(EXTERNAL_TLA, "Fixture.tla").expect("parse external .tla");
    // Sanity: TypeOK is a declared def but NOT a Next disjunct.
    assert!(spec.actions.contains("TypeOK"), "TypeOK is a top-level def");
    assert!(
        !spec.next_actions.contains("TypeOK"),
        "TypeOK is NOT a Next disjunct"
    );
    let module = SpecModule::External(spec);

    // ---- In-Rust gate (finding 4): action_names() is now Next-only, so TypeOK is NOT
    // a valid refinement/proof target — but invariant_names() (the wider def set) IS.
    assert!(
        module.action_names().contains("Apply"),
        "Apply is a Next disjunct — a valid action target"
    );
    assert!(
        !module.action_names().contains("TypeOK"),
        "in-Rust Ob.1: TypeOK must NOT be in the Next-only action_names() (finding 4 alignment)"
    );
    assert!(
        module.invariant_names().contains("TypeOK"),
        "TypeOK MUST still resolve as an INVARIANT id (the wider invariant_names() set)"
    );

    // ---- Lowered artifact (L3): an anchor naming TypeOK is rejected with [Ob.1].
    let modules = vec![module];
    let bad = anchor("Fixture", "TypeOK", "fixture::project");
    let txt = lower_to_ir("red_l3", &modules, &[&bad], &[], &[]);
    let path = dir.join("red_l3.trust_ir");
    std::fs::write(&path, &txt).expect("write");
    let Some((code, report)) = spec_link(&path, None) else {
        return;
    };
    assert_eq!(
        code, 1,
        "RED L3 fixture (external anchor on TypeOK) MUST exit 1 — false negative otherwise. Report:\n{report}"
    );
    assert!(
        report.contains("[Ob.1 action-exists]") && report.contains("TypeOK"),
        "RED L3 must report [Ob.1 action-exists] naming TypeOK; report:\n{report}"
    );

    // GREEN control: anchor the REAL Next action (Apply). The structure is
    // clean, while the artifact remains explicitly non-certifying.
    let good = anchor("Fixture", "Apply", "fixture::project");
    let good_txt = lower_to_ir("ctl_l3", &modules, &[&good], &[], &[]);
    let good_path = dir.join("ctl_l3.trust_ir");
    std::fs::write(&good_path, &good_txt).expect("write");
    let Some((code, report)) = spec_link(&good_path, None) else {
        return;
    };
    assert_clean_design_only(
        code,
        &report,
        "GREEN L3 control (anchor on the real Next action Apply)",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ===========================================================================
// RED fixture 4 — window_routing made-to-fail (finding 3): a corrupted CloseWindow
// transition the real `ty` validation (the conformance the gate now RUNS) rejects.
// ===========================================================================

#[test]
fn red_window_routing_corrupted_close_is_rejected_by_ty() {
    use aterm_spec::derive::window_routing_model;
    use std::collections::BTreeMap;

    // TIERED (VERIFY-1): a `ty trace` overclaim control — external-tool
    // obligation; runs only where the Trust toolchain is installed.
    let Some(ty) = ty_escalation("red_window_routing (overclaim corpus)") else {
        return;
    };
    let dir = tmpdir("winroute");
    let m = window_routing_model();
    let spec = dir.join("WindowRouting.tla");
    let cfg = dir.join("WindowRouting.cfg");
    let trace = dir.join("t.json");
    std::fs::write(&spec, m.transition_spec()).expect("write spec");

    // Pin Init to a 2-window state [win_count=2, frontmost=1, next_id=3, exited=0].
    let init: BTreeMap<&'static str, i64> = [
        ("win_count", 2),
        ("frontmost", 1),
        ("next_id", 3),
        ("exited", 0),
    ]
    .into_iter()
    .collect();
    std::fs::write(
        &cfg,
        m.transition_cfg(&init, &[("MaxWin", 1_000_000), ("MaxId", 1_000_000_000)]),
    )
    .expect("write cfg");

    let trace_json = |action: &str, prev: [i64; 4], next: [i64; 4]| -> String {
        let st = |s: [i64; 4]| {
            format!(
                "{{\"win_count\":{{\"type\":\"int\",\"value\":{}}},\
                 \"frontmost\":{{\"type\":\"int\",\"value\":{}}},\
                 \"next_id\":{{\"type\":\"int\",\"value\":{}}},\
                 \"exited\":{{\"type\":\"int\",\"value\":{}}}}}",
                s[0], s[1], s[2], s[3]
            )
        };
        format!(
            "{{\"version\":\"1\",\"module\":\"WindowRouting\",\
             \"variables\":[\"win_count\",\"frontmost\",\"next_id\",\"exited\"],\"steps\":[\
             {{\"index\":0,\"state\":{}}},\
             {{\"index\":1,\"state\":{},\"action\":{{\"name\":\"{}\"}}}}\
             ]}}",
            st(prev),
            st(next),
            action
        )
    };
    let validate = |action: &str, prev: [i64; 4], next: [i64; 4]| -> bool {
        std::fs::write(&trace, trace_json(action, prev, next)).expect("write trace");
        let out = Command::new(&ty)
            .arg("trace")
            .arg("validate")
            .arg(&trace)
            .arg("--spec")
            .arg(&spec)
            .arg("--config")
            .arg(&cfg)
            .output()
            .expect("run ty trace validate");
        out.status.success()
    };

    // GREEN: an HONEST CloseWindow (a non-last window: 2 -> 1, app stays, frontmost
    // re-points to the surviving allocated id 2) conforms.
    let prev = [2, 1, 3, 0];
    assert!(
        validate("CloseWindow", prev, [1, 2, 3, 0]),
        "honest CloseWindow (2->1, survivor remains) must conform — the binding is real"
    );

    // RED (made-to-fail): the SAME CloseWindow corrupted to the `Buggy` defect — the
    // LAST window closes (win_count 1->0) but the app does NOT exit (exited stays 0).
    // `ExitIffEmpty` forbids it; ty MUST reject. If this were accepted, the gate's
    // window_routing conformance would be vacuous (a corrupted close would sail through).
    let prev_last = [1, 1, 2, 0];
    let corrupted = [0, 0, 2, 0]; // emptied but exited=0 — the missed-exit defect
    assert!(
        !validate("CloseWindow", prev_last, corrupted),
        "RED window_routing fixture: a CloseWindow that empties the window set WITHOUT \
         exiting MUST be ty-REJECTED (the Buggy missed-exit). A pass here is a false \
         negative — the gate's window_routing conformance would prove nothing."
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ===========================================================================
// CLEAN control — the all-real lowering is structurally clean (Ob.1/4 + L1 + L2)
// and explicitly non-certifying. The in-process closure owns Ob.3 certification.
// Mirrors §3.3: a real anchor with a non-empty projection + a resolved proof.
// ===========================================================================

#[test]
fn clean_control_full_pass() {
    let dir = tmpdir("clean");
    let modules = vec![SpecModule::Embedded(ring_model())];
    let a = anchor("ring", "Push", "aterm_buffer::Ring::project");
    let p = ProofAnchor {
        machine: "ring",
        action: "Push",
        proof_name: "ring_push_refines",
        kind: ProofKind::Kani,
        location: "fixture.rs:1:1",
    };
    let txt = lower_to_ir("clean", &modules, &[&a], &[], &[&p]);
    let path = dir.join("clean.trust_ir");
    std::fs::write(&path, &txt).expect("write");
    let manifest = write_manifest(&dir, &["ring_push_refines"]);

    let Some((code, report)) = spec_link(&path, Some(&manifest)) else {
        return;
    };
    assert_clean_design_only(code, &report, "CLEAN control");
    assert!(
        report.contains("harness manifest") && report.contains("checked 1 proof binding"),
        "clean control should show the manifest and exact resolved proof count; \
         report:\n{report}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
