// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `spec_xref_closure`: the toolchain-free source↔spec closure over every anchored crate.
//!
//! The registry only collects anchors linked into the binary. Anchors expand in every test
//! and debug build of their crate (`#[cfg_attr(any(test, debug_assertions),
//! aterm_spec_anchor::refines(…))]`, the `macro_rules!` attributes on a Trust compiler), so
//! this test — which links every anchored crate as a dev-dependency and every model through
//! `aterm_models::link_all` — sees them from outside. It enforces:
//!   1. Action exists: every anchor names a real action of its machine.
//!   3. Coverage: every actively bound machine has every action bound or waived
//!      (`ratio == 1.0`); others are reported (`aterm_spec::xref::check_closure`).
//!   4. Machine exists: every named machine resolves to a registered model.
//!
//! Every machine closes here, aterm-gui's included: a waiver sits on a non-test item
//! (a debug-and-test carrier fn where no shipping fn performs the action), so the
//! registry this binary links is the whole of it. aterm-gui's `spec_xref_gate` only runs
//! the two custody machines' Tier-1 bodies with the per-step execution evidence, which
//! reads crate-private seams. Obligation 2 (symbols resolve
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

/// Each collected list as one line per record, sorted, digested (FNV-1a 64): the line SP.2
/// compares across hosts, whose load-time constructor sections differ (Mach-O
/// `__mod_init_func`, ELF `.init_array`, PE `.CRT$XCU`; `aterm_spec_anchor::registry`).
/// Printed, never pinned: every new anchor moves it.
fn collected_digests(refs: &[&xref::RefinementAnchor], waivers: &[&xref::Waiver]) -> String {
    fn digest(mut lines: Vec<String>) -> String {
        lines.sort_unstable();
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in lines.iter().flat_map(|l| l.bytes().chain(*b"\n")) {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        }
        format!("{}={hash:016x}", lines.len())
    }
    let anchors = refs.iter().map(|r| {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            r.machine, r.action, r.rust_method, r.location, r.project, r.entry_id
        )
    });
    let waived = waivers.iter().map(|w| {
        format!(
            "{}\t{}\t{}\t{}\t{}",
            w.machine, w.action, w.rust_method, w.location, w.reason
        )
    });
    let models = xref::model_registry()
        .into_iter()
        .map(|m| format!("{}\n{}\n{}", m.name, m.to_tla(), m.to_cfg()));
    let liveness = xref::liveness_registry()
        .into_iter()
        .map(|(m, l)| format!("{}\n{}", m.name, l.to_tla(&m)));
    format!(
        "collected digests (records=fnv1a64): refinements {} waivers {} models {} liveness {}",
        digest(anchors.collect()),
        digest(waived.collect()),
        digest(models.collect()),
        digest(liveness.collect()),
    )
}

#[test]
fn spec_xref_closure() {
    // LINK every anchored crate and every model. The registry sees only object code the
    // binary links, and a crate this test never names is not linked at all: one root per
    // crate pulls in its anchored code (the window's entry reaches the GUI and the libraries
    // it drives).
    let _domains = aterm_models::link_all();
    #[expect(
        clippy::disallowed_methods,
        reason = "a link root: the window's entry is named, never called"
    )]
    std::hint::black_box((
        aterm_gui::main_entry as fn(Vec<std::ffi::OsString>),
        aterm_core::terminal::Terminal::new as fn(u16, u16) -> aterm_core::terminal::Terminal,
        aterm_keeper::KeeperCore::new as fn(bool) -> aterm_keeper::KeeperCore,
        aterm_keeper::RelaunchBrake::new as fn() -> aterm_keeper::RelaunchBrake,
        aterm_keeper::core::classify_death
            as fn(aterm_keeper::core::DeathEvidence) -> aterm_keeper::core::Verdict,
        // aterm-restore's anchored modules hold generic fns only but for these two, which
        // keep `recover.rs` and `writer.rs` linked whether or not the window reaches them.
        aterm_restore::recover::Restored::history_admissible
            as fn(&aterm_restore::recover::Restored) -> bool,
        <aterm_restore::writer::WriteError as std::fmt::Display>::fmt
            as fn(
                &aterm_restore::writer::WriteError,
                &mut std::fmt::Formatter<'_>,
            ) -> std::fmt::Result,
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
    eprintln!("spec_xref_closure: {}", collected_digests(&refs, &waivers));

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
    let report = xref::check_closure(&modules);

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
         linked here."
    );
}
