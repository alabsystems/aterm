// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of the alternate-screen round trip and the sandbox apply step: each holds in
//! every configuration it declares, and each law is broken by its own mutant.

use aterm_models_core::{alt_screen_model, sandbox_model};
use aterm_spec::derive::Model;
use aterm_spec::{interp, verify};

/// `m` in every configuration it declares, the committed one first.
fn configurations(m: &Model, scenarios: &[&[(&str, i64)]]) -> Vec<Model> {
    std::iter::once(m.clone())
        .chain(scenarios.iter().map(|o| interp::with_consts(m, o)))
        .collect()
}

/// The laws the mutant at `dial` breaks in some configuration.
fn broken_at(configs: &[Model], dial: i64) -> Vec<&'static str> {
    let mut laws: Vec<&'static str> = configs
        .iter()
        .flat_map(|c| interp::violated_invariants(&interp::with_buggy(c, dial)))
        .collect();
    laws.sort_unstable();
    laws.dedup();
    laws
}

#[test]
fn the_alt_screen_round_trip_restores_main_and_the_cursor() {
    let m = alt_screen_model();
    let configs = configurations(&m, &[]);
    assert_eq!(verify::sweep_faults(&configs), Vec::<String>::new());
    verify::prove_and_catch_scalar(&m, "AltScreen: main untouched, cursor restored");
    assert_eq!(broken_at(&configs, 1), ["MainUntouched"]);
    assert_eq!(broken_at(&configs, 2), ["CursorRestored"]);
    // The owner's round trip: write, enter, scribble elsewhere, leave.
    let mut s = m.init_state();
    for action in ["WriteMain", "Enter", "Scribble", "Leave"] {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["active"], s["touched"]), (0, 0));
    assert_eq!(s["cursor"], s["saved"]);
}

#[test]
fn the_sandbox_applies_every_enforceable_restriction_and_nothing_else() {
    let m = sandbox_model();
    let configs = configurations(
        &m,
        &[
            &[("Req", 0), ("Sup", 1)],
            &[("Req", 1), ("Sup", 0)],
            &[("Req", 0), ("Sup", 0)],
        ],
    );
    assert_eq!(verify::sweep_faults(&configs), Vec::<String>::new());
    verify::prove_and_catch_scalar(&m, "Sandbox: requested and supported means applied");
    assert_eq!(broken_at(&configs, 1), ["AllSupportedApplied"]);
    assert_eq!(broken_at(&configs, 2), ["NoPhantomApply"]);
}
