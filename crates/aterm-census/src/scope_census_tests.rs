// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

use crate::scan_set::test_fixtures::{Tree, synth_tree};

const SYNTH_CLAIMS: &[ScopeClaim] = &[
    ScopeClaim {
        id: "flash-limiter",
        machine: Some("SynthLimiter"),
        unmodelled_because: "",
        scope: Scope::Window,
        root: "WindowState",
        chain: &[
            Link {
                file: "crates/host/src/lib.rs",
                owner: "WindowState",
                decl: "fx: EffectsPipeline,",
            },
            Link {
                file: "crates/engine/src/pipeline.rs",
                owner: "EffectsPipeline",
                decl: "decos: WordDecorations,",
            },
            Link {
                file: "crates/engine/src/enforcer.rs",
                owner: "WordDecorations",
                decl: "ignitions: Vec<u32>,",
            },
        ],
        closed_tokens: &["WordDecorations", "EffectsPipeline"],
        replicas: &[],
        aggregator: None,
        rationale: "WCAG 2.3.1 charges 2 flash pairs per ignition on one window retina.",
    },
    ScopeClaim {
        id: "supernova-burst-mutex",
        machine: Some("SynthMutex"),
        unmodelled_because: "",
        scope: Scope::Window,
        root: "WindowState",
        chain: &[
            Link {
                file: "crates/host/src/lib.rs",
                owner: "WindowState",
                decl: "fx: EffectsPipeline,",
            },
            Link {
                file: "crates/engine/src/pipeline.rs",
                owner: "EffectsPipeline",
                decl: "decos: WordDecorations,",
            },
            Link {
                file: "crates/engine/src/enforcer.rs",
                owner: "WordDecorations",
                decl: "persist: Vec<u32>,",
            },
        ],
        closed_tokens: &[],
        replicas: &[Replica {
            file: "crates/engine/src/enforcer.rs",
            owner: "ParkedPane",
            decl: "persist: Vec<u32>,",
            kind: ReplicaKind::Shard,
            justification: "Parked pane shards share the window-wide burst mutex.",
        }],
        aggregator: Some(Aggregator {
            symbol: "super_prepass",
            file: "crates/engine/src/enforcer.rs",
            reads: &["self.persist", "self.parked"],
        }),
        rationale: "MAX_ACTIVE_SUPERNOVAE = 1 spans active and parked shards.",
    },
];

fn synth_claim(id: &str) -> &'static ScopeClaim {
    SYNTH_CLAIMS
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no synthetic claim `{id}`"))
}

fn base_root(name: &str) -> Tree {
    synth_tree(
        "aterm-scope-census",
        name,
        &[
            (
                "crates/aterm-models-synth/src/lib.rs".to_string(),
                "SynthLimiter {\n}\nSynthMutex {\n}\n".to_string(),
            ),
            (
                "crates/host/src/lib.rs".to_string(),
                "pub struct WindowState {\n    fx: EffectsPipeline,\n}\n".to_string(),
            ),
            (
                "crates/engine/src/pipeline.rs".to_string(),
                "pub struct EffectsPipeline {\n\
                 \x20   decos: WordDecorations,\n\
                 }\n\
                 impl EffectsPipeline {\n\
                 \x20   pub fn for_window() -> Self {\n\
                 \x20       Self {\n\
                 \x20           decos: WordDecorations::default(),\n\
                 \x20       }\n\
                 \x20   }\n\
                 }\n"
                .to_string(),
            ),
            (
                "crates/engine/src/enforcer.rs".to_string(),
                "pub struct WordDecorations {\n\
                 \x20   ignitions: Vec<u32>,\n\
                 \x20   persist: Vec<u32>,\n\
                 \x20   parked: Vec<ParkedPane>,\n\
                 }\n\
                 impl WordDecorations {\n\
                 \x20   pub fn default() -> Self {\n\
                 \x20       Self {\n\
                 \x20           ignitions: Vec::new(),\n\
                 \x20           persist: Vec::new(),\n\
                 \x20           parked: Vec::new(),\n\
                 \x20       }\n\
                 \x20   }\n\
                 \x20   fn super_prepass(&self) {\n\
                 \x20       let _ = &self.persist;\n\
                 \x20       for p in &self.parked {\n\
                 \x20           let _ = &p.persist;\n\
                 \x20       }\n\
                 \x20   }\n\
                 }\n\
                 pub struct ParkedPane {\n\
                 \x20   persist: Vec<u32>,\n\
                 }\n"
                .to_string(),
            ),
        ],
    )
}

fn write_file(root: &Tree, rel: &str, contents: &str) {
    root.write(rel, contents);
}

fn mutate(root: &Tree, rel: &str, from: &str, to: &str) {
    let before = crate::tree::read_to_string(&root.join(rel)).expect("read for mutation");
    let after = before.replace(from, to);
    assert_ne!(
        before, after,
        "the mutation did not match `{from}` in {rel}"
    );
    root.write(rel, &after);
}

fn run_one(root: &Path, claim: &'static ScopeClaim) -> CensusOutcome {
    run_scope_census_over(root, std::slice::from_ref(claim))
}

#[test]
fn a_per_pane_word_decorations_map_fails_the_flash_limiter_chain() {
    let claim = synth_claim("flash-limiter");
    let root = base_root("per-pane-map");

    let before = run_one(&root, claim);
    assert!(
        before.ok,
        "the unmutated synthetic claim tree must be GREEN:\n{}",
        before.log
    );

    mutate(
        &root,
        "crates/host/src/lib.rs",
        "    fx: EffectsPipeline,\n",
        "    fx: std::collections::HashMap<u64, EffectsPipeline>,\n",
    );
    let after = run_one(&root, claim);
    assert!(
        !after.ok,
        "a per-pane pipeline map MUST fail the flash-limiter chain:\n{}",
        after.log
    );
    assert!(
        after.log.contains("[OB-13] claim `flash-limiter`"),
        "the failure must be attributed to the pinned chain link:\n{}",
        after.log
    );
    assert!(
        after.log.contains("WCAG 2.3.1"),
        "the diagnostic must carry the safety argument, not just a diff:\n{}",
        after.log
    );
}

#[test]
fn a_new_per_pane_owner_fails_the_closure() {
    let claim = synth_claim("flash-limiter");
    let root = base_root("per-pane-owner");
    assert!(run_one(&root, claim).ok);

    write_file(
        &root,
        "crates/host/src/pane_state.rs",
        "pub(crate) struct PaneState {\n\
         \x20   pub(crate) decos: WordDecorations,\n\
         }\n",
    );
    let out = run_one(&root, claim);
    assert!(
        !out.ok,
        "a new per-pane owner of the enforcing state must open the closure:\n{}",
        out.log
    );
    assert!(
        out.log.contains("[OB-14]") && out.log.contains("PaneState"),
        "OB-14 must name the unaccounted owner:\n{}",
        out.log
    );

    let root = base_root("per-pane-pipeline");
    write_file(
        &root,
        "crates/host/src/pane_fx.rs",
        "pub(crate) struct PaneFx {\n\
         \x20   pub(crate) fx: EffectsPipeline,\n\
         }\n",
    );
    let out = run_one(&root, claim);
    assert!(
        !out.ok && out.log.contains("[OB-14]") && out.log.contains("PaneFx"),
        "a per-pane pipeline is a second limiter and must open the closure:\n{}",
        out.log
    );
}

#[test]
fn a_per_pane_constructor_fails_the_closure() {
    let claim = synth_claim("flash-limiter");
    let root = base_root("per-pane-ctor");
    assert!(run_one(&root, claim).ok);

    write_file(
        &root,
        "crates/host/src/pane_engines.rs",
        "pub(crate) fn build_pane_engines(panes: usize) -> Vec<WordDecorations> {\n\
         \x20   let mut out = Vec::new();\n\
         \x20   for _pane in 0..panes {\n\
         \x20       out.push(WordDecorations::default());\n\
         \x20   }\n\
         \x20   out\n\
         }\n",
    );
    let out = run_one(&root, claim);
    assert!(
        !out.ok,
        "a per-pane constructor must open the closure even with no field:\n{}",
        out.log
    );
    assert!(
        out.log.contains("[OB-14]") && out.log.contains("build_pane_engines"),
        "OB-14 must name the constructing fn:\n{}",
        out.log
    );
}

#[test]
fn named_constructors_are_discovered_from_impl_blocks() {
    let src = "pub struct EffectsPipeline;\n\
               impl EffectsPipeline {\n\
               \x20   pub fn new() -> Self {\n        Self\n    }\n\
               \x20   pub fn for_window() -> Self {\n        Self::new()\n    }\n\
               \x20   pub fn word_panes(&self) -> &WordPanes {\n        &self.panes\n    }\n\
               }\n\
               impl Default for EffectsPipeline {\n\
               \x20   fn default() -> Self {\n        Self::new()\n    }\n\
               }\n\
               #[cfg(test)]\n\
               impl EffectsPipeline {\n\
               \x20   fn for_test() -> Self {\n        Self\n    }\n\
               }\n";
    let named = [(
        std::path::PathBuf::from("crates/engine/src/pipeline.rs"),
        src.to_string(),
    )];
    let ctors = closed_token_ctors(&named, &closed_tokens(SYNTH_CLAIMS));
    let fx: Vec<&str> = ctors["EffectsPipeline"]
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(fx, ["default", "for_window", "new"]);
    let body = "fn a() -> Self {} fn b(&self) -> &Self {} fn c() -> Option<Self> {}\n\
                fn d(&self) -> u32 { 0 } fn e() -> T where X: Y {}";
    assert_eq!(self_returning_fns(body, "T"), ["a", "c", "e"]);
    let src = "impl<'a> T<'a> { fn a() {} }\nimpl Default for crate::m::T { fn b() {} }\n\
               impl U { fn c() {} }\nimpl X for Ts { fn d() {} }";
    assert_eq!(impl_bodies(src, "T").len(), 2);
}

#[test]
fn a_per_pane_named_constructor_fails_the_closure() {
    let claim = synth_claim("flash-limiter");
    let root = base_root("per-pane-named-ctor");
    assert!(run_one(&root, claim).ok);

    write_file(
        &root,
        "crates/host/src/pane_fx_probe.rs",
        "pub(crate) fn pane_pipelines(n: usize) -> Vec<EffectsPipeline> {\n\
         \x20   let mut v = Vec::new();\n\
         \x20   for _ in 0..n {\n\
         \x20       v.push(EffectsPipeline::for_window());\n\
         \x20   }\n\
         \x20   v\n\
         }\n",
    );
    let out = run_one(&root, claim);
    assert!(
        !out.ok
            && out.log.contains("[OB-14]")
            && out.log.contains("construction of `EffectsPipeline`")
            && out.log.contains("pane_pipelines"),
        "a per-pane `for_window()` is a second limiter and must open the closure, \
         naming the fn:\n{}",
        out.log
    );
}

#[test]
fn a_renamed_field_or_owner_fails_stale_rather_than_silently_passing() {
    let claim = synth_claim("flash-limiter");

    let root = base_root("renamed-field");
    mutate(
        &root,
        "crates/host/src/lib.rs",
        "    fx: EffectsPipeline,\n",
        "    effects: EffectsPipeline,\n",
    );
    let out = run_one(&root, claim);
    assert!(!out.ok, "a renamed claimed field must go RED:\n{}", out.log);
    assert!(
        out.log.contains("occurs 0 time(s)"),
        "the diagnostic must say the pinned declaration vanished:\n{}",
        out.log
    );

    let root = base_root("renamed-owner");
    mutate(
        &root,
        "crates/host/src/lib.rs",
        "struct WindowState {",
        "struct WindowShell {",
    );
    let out = run_one(&root, claim);
    assert!(!out.ok, "a renamed claimed owner must go RED:\n{}", out.log);
    assert!(
        out.log.contains("[OB-16]") && out.log.contains("STALE"),
        "the diagnostic must say the claim went stale:\n{}",
        out.log
    );
}

#[test]
fn a_return_type_is_not_a_construction() {
    for before in [
        "pub fn decos(&self) -> ",
        "pub fn decos(&self) -> &",
        "pub fn decos_mut(&mut self) -> &mut ",
        "fn lend<'a>(x: &'a Y) -> &'a mut ",
        "fn lend<'a>(x: &'a Y) -> &'a ",
    ] {
        assert!(returns_token(before), "{before:?} is a return type");
    }
    for before in ["let a = &", "let a = ", "    decos: ", "out.push("] {
        assert!(!returns_token(before), "{before:?} is not a return type");
    }
}

#[test]
fn enum_variants_and_fn_parameters_are_not_owners() {
    let claim = synth_claim("flash-limiter");
    let root = base_root("false-positives");
    write_file(
        &root,
        "crates/engine/src/calibration.rs",
        "pub enum Owner {\n    WordDecorations = 21,\n}\n\
         pub enum SeriousEffect {\n    WordDecorations,\n}\n\
         fn which() -> SeriousEffect {\n    SeriousEffect::WordDecorations\n}\n\
         pub fn drain(decos: &mut crate::enforcer::WordDecorations, n: u32) {}\n\
         impl WordDecorations {\n    fn len(&self) -> usize {\n        0\n    }\n}\n",
    );
    let out = run_one(&root, claim);
    assert!(
        out.ok,
        "variants, qualified variants, impl blocks and fn parameters must NOT count as \
         owners of the enforcing state:\n{}",
        out.log
    );
}

#[test]
fn the_supernova_aggregator_regression_goes_red() {
    let root = base_root("supernova");

    let out = run_scope_census_over(&root, SYNTH_CLAIMS);
    assert!(out.ok, "the aggregator must pass as written:\n{}", out.log);

    mutate(
        &root,
        "crates/engine/src/enforcer.rs",
        "        for p in &self.parked {",
        "        for p in std::iter::empty::<&ParkedPane>() {",
    );
    let out = run_scope_census_over(&root, SYNTH_CLAIMS);
    assert!(
        !out.ok,
        "a per-bound-pane scan must fail OB-15:\n{}",
        out.log
    );
    assert!(
        out.log.contains("[OB-15]") && out.log.contains("self.parked"),
        "the failure must name the missing shard read:\n{}",
        out.log
    );
}
