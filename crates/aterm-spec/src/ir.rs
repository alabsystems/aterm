// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Lowers the source↔spec cross-reference to a `trust-ir` `SpecModule` text artifact, so
//! `trust-ir spec-link` checks the same models, anchors and waivers as
//! [`crate::xref::check_closure`] independently (S0/S1, Ob.1/Ob.4, L2).
//!
//! The artifact has source labels but no compiler-owned `FuncId`s, so every block is `enforcement
//! design-only` and the report is never a certifying Linked one. `spec-check trust-ir`
//! ([`crate::check`]) runs `spec-link` over every registered module, fully anchored, and over
//! planted defects, and accepts only a violation-free design-only (or certifying) report.
//!
//! aterm-spec is outside the release closure and has no `trust_ir` dependency: it writes the text
//! format of `trust-ir/crates/trust-ir/docs/spec-module-format.md` §2 directly, in the v27 writer's
//! shape (`; TrustIr text format v1` and `module "<name>"`, one blank line before each
//! `spec_module "<name>" { … }` block, every free-form string quoted with Rust `{:?}` escaping).
//!
//! Machine names: anchors are lower_snake (`"terminal_modes"`), models CamelCase
//! (`"TerminalModes"`). [`crate::xref::machine_matches`] compares case-insensitively without `_`,
//! but `spec-link` matches exactly, so every anchor's and waiver's `machine` is rewritten here to
//! the module name it resolves to, and the two resolvers agree.

use crate::derive::Model;
use crate::xref::{RefinementAnchor, SpecModule, Waiver, machine_matches};

/// A free-form string as a trust-ir quoted token: Rust `{:?}` escaping, as the trust-ir text
/// writer does.
fn q(s: &str) -> String {
    format!("{s:?}")
}

/// One anchor per Trust S0 fallback identity `(action, rust_symbol)`, chosen deterministically.
///
/// Anchors carry no Trust `FuncId` and `rust_method` is a bare function name, so distinct sites
/// (CPU/GPU, impls, projections) can share one identity, and emitting them all would duplicate S0
/// identities. Records are sorted, a usable projection first, and the first per identity is kept:
/// `spec-link` sees this deduplicated view, while aterm's own closure checks every record.
fn canonical_fallback_anchors<'a>(
    anchors: impl Iterator<Item = &'a RefinementAnchor>,
) -> Vec<&'a RefinementAnchor> {
    let mut anchors: Vec<_> = anchors.collect();
    anchors.sort_by(|a, b| {
        a.action
            .cmp(b.action)
            .then_with(|| a.rust_method.cmp(b.rust_method))
            .then_with(|| a.project.is_empty().cmp(&b.project.is_empty()))
            .then_with(|| a.project.cmp(b.project))
            .then_with(|| a.location.cmp(b.location))
            .then_with(|| a.machine.cmp(b.machine))
    });
    anchors.dedup_by(|a, b| a.action == b.action && a.rust_method == b.rust_method);
    anchors
}

/// One [`SpecModule`] as its `spec_module "<name>" { … }` block (the assembler adds the blank line
/// before it), with `anchors`/`waivers` already filtered to this module.
///
/// * Embedded `Model`: `origin embedded`, a `var` per state variable, an `action` per action,
///   and an `invariant` per invariant rendered by [`crate::derive::Expr::to_tla`].
/// * External `.tla` (the ISOLATION specs): `origin external "<path>"` and an `action` per `Next`
///   disjunct; no vars or invariants, which the external parser does not extract.
// Skip: iterator and string building over std bodies the prover lacks; spec-IR machinery, not
// shipped code.
#[cfg_attr(trust_verify, trust::skip)]
fn lower_module_block(
    module: &SpecModule,
    anchors: &[&RefinementAnchor],
    waivers: &[&Waiver],
) -> String {
    let name = module.name();
    let mut s = String::new();
    s.push_str(&format!("spec_module {} {{\n", q(name)));

    match module {
        SpecModule::Embedded(m) => {
            s.push_str("  origin embedded\n");
            s.push_str("  enforcement design-only\n");
            emit_embedded_vars(&mut s, m);
            // In declared order.
            for a in &m.actions {
                s.push_str(&format!("  action {}\n", q(a.name)));
            }
            for inv in &m.invariants {
                s.push_str(&format!(
                    "  invariant {} : {}\n",
                    q(inv.name),
                    q(&inv.expr.to_tla())
                ));
            }
        }
        SpecModule::External(t) => {
            s.push_str(&format!("  origin external {}\n", q(&t.file_path)));
            s.push_str("  enforcement design-only\n");
            // The `Next` disjuncts (the coverage set), sorted.
            for a in module.coverage_actions() {
                s.push_str(&format!("  action {}\n", q(&a)));
            }
        }
    }

    // Each anchor's machine is written as this module's canonical name.
    for a in anchors {
        s.push_str(&format!(
            "  anchor machine {} action {} rust {} span {}",
            q(name),
            q(a.action),
            q(a.rust_method),
            q(a.location),
        ));
        if !a.project.is_empty() {
            s.push_str(&format!(" project {}", q(a.project)));
        }
        // aterm has no typed Trust projection target: say so rather than rely on the parser's
        // default.
        s.push_str(" target none\n");
    }
    for w in waivers {
        s.push_str(&format!(
            "  waiver machine {} action {} reason {}\n",
            q(name),
            q(w.action),
            q(w.reason),
        ));
    }
    s.push_str("}\n");
    s
}

/// The `var` lines of an embedded [`Model`]: scalars tagged `"Int"`, then function-valued vars
/// tagged `"[1..<range> -> BOOLEAN]"` (the TLA+ generator's init shape). The IR treats tags as
/// opaque.
fn emit_embedded_vars(s: &mut String, m: &Model) {
    for v in &m.vars {
        s.push_str(&format!("  var {} : {}\n", q(v.name), q("Int")));
    }
    for f in &m.fn_vars {
        let ty = format!("[1..{} -> BOOLEAN]", f.range);
        s.push_str(&format!("  var {} : {}\n", q(f.name), q(&ty)));
    }
}

/// One complete `.trust_ir` module: the v1 header and `module "<name>"`, then a `spec_module` block
/// per [`SpecModule`] holding the fallback-deduplicated `refinements` and the `waivers` whose
/// `machine` resolves to it, rewritten to its canonical name (see the module doc).
///
/// Every block is `enforcement design-only`, so `spec-link` checks structure, action/machine
/// resolution and nonblank projections but returns a non-certifying report; coverage stays
/// fail-closed in [`crate::xref::check_closure`].
///
/// Dropped: an anchor or waiver whose `machine` is empty (the bare `reason="…"` bypass-setter
/// waiver) or resolves to no module, and a waiver with an empty `action`. aterm's own closure
/// reports a dangling machine.
pub fn lower_to_ir(
    module_name: &str,
    modules: &[SpecModule],
    refinements: &[&RefinementAnchor],
    waivers: &[&Waiver],
) -> String {
    let mut out = String::new();
    out.push_str("; TrustIr text format v1\n");
    out.push_str(&format!("module {}\n", q(module_name)));

    for module in modules {
        let canon = module.name();
        let my_anchors = canonical_fallback_anchors(
            refinements
                .iter()
                .copied()
                .filter(|a| machine_matches(a.machine, canon)),
        );
        let my_waivers: Vec<&Waiver> = waivers
            .iter()
            .copied()
            .filter(|w| {
                !w.machine.is_empty() && !w.action.is_empty() && machine_matches(w.machine, canon)
            })
            .collect();
        out.push('\n');
        out.push_str(&lower_module_block(module, &my_anchors, &my_waivers));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derive::ring_model;

    /// A hand-built anchor record (mirrors what the `#[refines]` macro submits).
    fn anchor(
        machine: &'static str,
        action: &'static str,
        rust: &'static str,
        loc: &'static str,
        project: &'static str,
    ) -> RefinementAnchor {
        RefinementAnchor {
            machine,
            action,
            rust_method: rust,
            location: loc,
            project,
            // No macro ran, so no entry probe; the artifact lowers the string closure only.
            entry_id: "",
        }
    }

    fn waiver(machine: &'static str, action: &'static str, reason: &'static str) -> Waiver {
        Waiver {
            machine,
            action,
            reason,
            rust_method: "test",
            location: "test:1",
        }
    }

    #[test]
    fn header_is_v1_canonical() {
        let txt = lower_to_ir("aterm_spec_xref", &[], &[], &[]);
        assert!(
            txt.starts_with("; TrustIr text format v1\nmodule \"aterm_spec_xref\"\n"),
            "{txt}"
        );
    }

    #[test]
    fn embedded_block_emits_vars_actions_invariants_and_canonicalizes_machine() {
        let modules = vec![SpecModule::Embedded(ring_model())];
        // Anchor uses the lower convention "ring"; Model::name is "Ring".
        let a = anchor(
            "ring",
            "Push",
            "aterm_buffer::Ring::push",
            "ring.rs:1:1",
            "Ring::project",
        );
        let w = waiver("ring", "Push", "test waiver");
        let txt = lower_to_ir("m", &modules, &[&a], &[&w]);
        assert!(txt.contains("spec_module \"Ring\" {\n"), "{txt}");
        assert!(txt.contains("  origin embedded\n"), "{txt}");
        assert!(txt.contains("  enforcement design-only\n"), "{txt}");
        assert!(txt.contains("  var \"seq\" : \"Int\"\n"), "{txt}");
        assert!(txt.contains("  var \"lo\" : \"Int\"\n"), "{txt}");
        assert!(txt.contains("  action \"Push\"\n"), "{txt}");
        assert!(
            txt.contains("  invariant \"LenBounded\" : \"seq - lo + 1 =< Cap\"\n"),
            "{txt}"
        );
        // The anchor's and waiver's machine are rewritten to the canonical "Ring".
        assert!(
            txt.contains("  anchor machine \"Ring\" action \"Push\" rust \"aterm_buffer::Ring::push\" span \"ring.rs:1:1\" project \"Ring::project\" target none\n"),
            "{txt}"
        );
        assert!(
            txt.contains("  waiver machine \"Ring\" action \"Push\" reason \"test waiver\"\n"),
            "{txt}"
        );
    }

    #[test]
    fn anchor_without_project_omits_project_clause() {
        let modules = vec![SpecModule::Embedded(ring_model())];
        let a = anchor("ring", "Push", "r::push", "r.rs:1:1", ""); // empty project
        let txt = lower_to_ir("m", &modules, &[&a], &[]);
        assert!(
            txt.contains(
                "  anchor machine \"Ring\" action \"Push\" rust \"r::push\" span \"r.rs:1:1\" target none\n"
            ),
            "no project clause: {txt}"
        );
        assert!(!txt.contains("project"), "{txt}");
    }

    #[test]
    fn fallback_anchor_identity_is_deduplicated_deterministically() {
        let modules = vec![SpecModule::Embedded(ring_model())];
        let later = anchor(
            "ring",
            "Push",
            "aterm_buffer::Ring::push",
            "z.rs:9:9",
            "Z::project",
        );
        let chosen = anchor(
            "ring",
            "Push",
            "aterm_buffer::Ring::push",
            "a.rs:1:1",
            "A::project",
        );
        let no_projection = anchor("ring", "Push", "aterm_buffer::Ring::push", "0.rs:1:1", "");

        let forward = lower_to_ir("m", &modules, &[&later, &no_projection, &chosen], &[]);
        let reversed = lower_to_ir("m", &modules, &[&chosen, &no_projection, &later], &[]);

        assert_eq!(
            forward, reversed,
            "inventory order must not change Trust IR"
        );
        assert_eq!(
            forward
                .matches("action \"Push\" rust \"aterm_buffer::Ring::push\"")
                .count(),
            1,
            "Trust S0 sees one unresolved fallback identity:\n{forward}"
        );
        assert!(
            forward.contains("span \"a.rs:1:1\" project \"A::project\" target none"),
            "deterministic usable projection wins:\n{forward}"
        );
    }

    #[test]
    fn unresolved_or_bare_records_are_dropped() {
        let modules = vec![SpecModule::Embedded(ring_model())];
        let dangling = anchor("no_such_machine", "X", "r", "r:1", "");
        let bare = waiver("", "", "bypass-setter"); // empty machine
        let txt = lower_to_ir("m", &modules, &[&dangling], &[&bare]);
        assert!(
            !txt.contains("no_such_machine"),
            "dangling anchor dropped: {txt}"
        );
        assert!(!txt.contains("bypass-setter"), "bare waiver dropped: {txt}");
    }
}
