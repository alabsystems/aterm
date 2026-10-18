// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `spec_xref_closure`: the toolchain-free source↔spec closure over every anchored crate.
//!
//! The registry only collects anchors linked into the binary. Anchors expand in every test
//! and debug build of their crate (`cfg(any(test, debug_assertions))`), so this test — which
//! links every anchored crate as a dev-dependency and every model through
//! `aterm_models::link_all` — sees them from outside. It enforces:
//!   1. Action exists: every anchor names a real action of its machine.
//!   3. Coverage: every actively bound machine has every action bound or waived
//!      (`ratio == 1.0`); others are reported (`aterm_spec::xref::check_closure`).
//!   4. Machine exists: every named machine resolves to a registered model.
//!
//! The machines aterm-gui binds are the exception for obligation 3: some of their seams and
//! waivers are test-only there, so aterm-gui's own `spec_xref_gate` closes them, and runs
//! their Tier-1 bodies with the per-step execution evidence. Obligation 2 (symbols resolve
//! to live DefIds) needs typed Trust `FuncId`s, which aterm does not have, so it is not
//! checked. `ty --strict-vacuity` over the registry is `spec-check vacuity`.

use std::collections::BTreeSet;

use aterm_spec::xref::{self, SpecModule};

/// The registered `SpecModule` set: every model linked into this binary.
fn registered_modules() -> Vec<SpecModule> {
    xref::model_registry()
        .into_iter()
        .map(SpecModule::Embedded)
        .collect()
}

/// The registered machines (as the closure names them) that some anchor or waiver linked
/// here from `source` binds.
fn machines_bound_from(report: &xref::ClosureReport, source: &str) -> BTreeSet<String> {
    let anchored: BTreeSet<&str> = xref::refinements()
        .filter(|r| r.location.starts_with(source))
        .map(|r| r.machine)
        .chain(
            xref::waivers()
                .filter(|w| w.location.starts_with(source) && !w.machine.is_empty())
                .map(|w| w.machine),
        )
        .collect();
    report
        .coverage
        .iter()
        .filter(|c| {
            anchored
                .iter()
                .any(|a| xref::machine_matches(a, &c.machine))
        })
        .map(|c| c.machine.clone())
        .collect()
}

#[test]
fn spec_xref_closure() {
    // LINK every anchored crate and every model. The registry sees only object code the
    // binary links, and a crate this test never names is not linked at all: one root per
    // crate pulls in its anchored code (the window's entry reaches the GUI and the libraries
    // it drives).
    let _domains = aterm_models::link_all();
    std::hint::black_box((
        aterm_gui::main_entry as fn(Vec<std::ffi::OsString>),
        aterm_core::terminal::Terminal::new as fn(u16, u16) -> aterm_core::terminal::Terminal,
        aterm_keeper::KeeperCore::new as fn(bool) -> aterm_keeper::KeeperCore,
        aterm_keeper::RelaunchBrake::new as fn() -> aterm_keeper::RelaunchBrake,
        aterm_keeper::core::classify_death
            as fn(aterm_keeper::core::DeathEvidence) -> aterm_keeper::core::Verdict,
    ));
    // ---- Proof #1: the registry ACTUALLY collected the anchors, cross-crate. ----
    let refs: Vec<_> = xref::refinements().collect();
    let waivers: Vec<_> = xref::waivers().collect();

    // Per-machine collected counts (the cross-crate collection evidence).
    let machines: BTreeSet<&str> = refs
        .iter()
        .map(|r| r.machine)
        .chain(
            waivers
                .iter()
                .filter(|w| !w.machine.is_empty())
                .map(|w| w.machine),
        )
        .collect();
    eprintln!(
        "spec_xref_closure: collected {} refinement anchor(s) + \
         {} waiver(s) across {} machine(s):",
        refs.len(),
        waivers.len(),
        machines.len()
    );
    for m in &machines {
        let r = refs.iter().filter(|x| x.machine == *m).count();
        let w = waivers.iter().filter(|x| x.machine == *m).count();
        eprintln!("    {m:<16} refinements={r:<3} waivers={w}");
    }

    // Phase 0 not regressed: the terminal_modes anchors (from aterm-core, expanded in
    // its debug build) must still be collected here.
    let tm = refs
        .iter()
        .filter(|r| r.machine == "terminal_modes")
        .count();
    assert!(
        tm >= 26,
        "expected >= 26 `terminal_modes` refinement anchors collected cross-crate from \
         aterm-core (Phase 0 must not regress), found {tm}"
    );

    // ---- Build the registered SpecModule set ----
    let modules = registered_modules();
    eprintln!("spec_xref_closure: {} registered model(s)", modules.len());

    // ---- Run the obligations (1, 3, 4) ----
    let mut report = xref::check_closure(&modules);
    // A machine aterm-gui binds is closed by aterm-gui's own `spec_xref_gate`: some of its
    // seams and waivers are test-only there, so its coverage is whole only in that binary.
    // Its anchors' obligations 1 and 4 hold here as everywhere.
    let gui_bound = machines_bound_from(&report, "crates/aterm-gui/");
    report.violations.retain(|v| {
        v.obligation != 3
            || !gui_bound
                .iter()
                .any(|m| v.message.starts_with(&format!("machine `{m}` ")))
    });

    eprintln!("spec_xref_closure: per-machine coverage ledger:");
    for c in &report.coverage {
        eprintln!(
            "  {:<16} ratio={:.3} bound={:<3} waived={:<3} actions={:<3} {}{}",
            c.machine,
            c.ratio(),
            c.bound.len(),
            c.waived.len(),
            c.total_actions,
            if c.active {
                "[ACTIVE]"
            } else {
                "[report-only]"
            },
            if c.active && !c.uncovered.is_empty() {
                format!(" UNCOVERED={:?}", c.uncovered)
            } else {
                String::new()
            },
        );
    }

    // ---- Proof #2: GREEN (no obligation violations) ----
    assert!(
        report.is_ok(),
        "spec_xref_closure FAILED — source↔spec obligations violated:\n{}",
        report
            .violations
            .iter()
            .map(|v| format!("  [obligation {}] {}", v.obligation, v.message))
            .collect::<Vec<_>>()
            .join("\n")
    );

    eprintln!(
        "spec_xref_closure: GREEN — obligations 1/3/4 hold over every anchor, waiver and model \
         linked here ({} machine(s) aterm-gui binds are closed by its own gate).",
        gui_bound.len()
    );
}
