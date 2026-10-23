// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance tests for [`hyperlink_scheme_cap_model`].

use std::collections::BTreeMap;

use aterm_core::terminal::Terminal;
use aterm_models_misc::hyperlink_scheme_cap_model;
use aterm_spec::derive::Model;
use aterm_spec::verify;

type State = BTreeMap<&'static str, i64>;

const NEVER_ALLOW: &[&str] = &["javascript", "data", "file", "vbscript", "about", "blob"];

fn project(term: &Terminal) -> State {
    let orca = i64::from(term.is_hyperlink_scheme_authorized("orca"));
    let total = i64::try_from(term.hyperlink_extra_scheme_count()).expect("bounded set fits i64");
    let never = i64::from(
        NEVER_ALLOW
            .iter()
            .any(|s| term.is_hyperlink_scheme_authorized(s)),
    );
    [("orca", orca), ("others", total - orca), ("never", never)]
        .into_iter()
        .collect()
}

fn bind_transition(model: &Model, state: &mut State, action: &str, observed: State) {
    let prev = state.clone();
    let mut expected = prev.clone();
    assert!(
        model.fire(action, &mut expected),
        "model action {action} disabled at {prev:?}"
    );
    assert_eq!(
        observed, expected,
        "real scheme-capability projection diverged after {action}"
    );
    let (conforms, diagnostics) = verify::validate_transition_tiered(
        model,
        &[],
        &prev,
        &observed,
        Some(action),
        "HyperlinkSchemeCap Tier-1",
    );
    assert!(
        conforms,
        "real transition {action} is not admitted by the derived model: {diagnostics}"
    );
    *state = observed;
}

fn gate_accepts_orca(term: &mut Terminal) -> bool {
    term.process(b"\x1b]8;;orca://probe\x07");
    let accepted = term.current_hyperlink().is_some();
    term.process(b"\x1b]8;;\x07"); // close/clear so probes stay independent
    accepted
}

#[test]
fn real_terminal_scheme_mints_refusals_and_gate_refine_the_model() {
    let model = hyperlink_scheme_cap_model();
    let mut state = model.init_state();
    let mut term = Terminal::new(4, 40);
    assert_eq!(
        project(&term),
        state,
        "a fresh terminal projects onto the model's init state"
    );

    assert!(!model.action_enabled("Accept", &state));
    assert!(!gate_accepts_orca(&mut term));

    assert!(term.authorize_hyperlink_scheme("orca"));
    bind_transition(&model, &mut state, "Authorize", project(&term));

    assert!(model.action_enabled("Accept", &state));
    assert!(gate_accepts_orca(&mut term));
    bind_transition(&model, &mut state, "Accept", project(&term));

    for other in ["alpha", "beta", "gamma"] {
        assert!(term.authorize_hyperlink_scheme(other));
        bind_transition(&model, &mut state, "AuthorizeOther", project(&term));
    }

    assert!(
        !term.authorize_hyperlink_scheme("delta"),
        "the real engine must refuse the mint past MAX_EXTRA_SCHEMES"
    );
    bind_transition(&model, &mut state, "RefuseAtCap", project(&term));

    assert!(!term.authorize_hyperlink_scheme("javascript"));
    bind_transition(&model, &mut state, "RefuseNeverAllow", project(&term));

    term.revoke_hyperlink_scheme("orca");
    bind_transition(&model, &mut state, "Revoke", project(&term));

    assert!(!model.action_enabled("Accept", &state));
    assert!(!gate_accepts_orca(&mut term));

    let mut forged = state.clone();
    forged.insert("never", 1);
    let (conforms, _) = verify::validate_transition_tiered(
        &model,
        &[],
        &state,
        &forged,
        Some("RefuseNeverAllow"),
        "HyperlinkSchemeCap Tier-1 negative control",
    );
    assert!(
        !conforms,
        "the model must reject a never-allow admission — the pass would be vacuous otherwise"
    );
    assert!(
        !model.check_invariant("NeverAllowRefused", &forged),
        "the forged state must violate NeverAllowRefused"
    );
}
