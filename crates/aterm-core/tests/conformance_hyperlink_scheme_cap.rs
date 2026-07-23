// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the host-minted OSC-8 hyperlink scheme capability
//! (orca deep-links §7, #4384).
//!
//! Drives the REAL shipping `Terminal` host API
//! (`authorize_hyperlink_scheme` / `revoke_hyperlink_scheme`) and the real
//! OSC-8 gate, projects the live authorization state onto
//! `hyperlink_scheme_cap_model`'s variables, and checks every transition
//! against the derived model — including the at-capacity refusal at the TRUE
//! `MAX_EXTRA_SCHEMES` bound (the model's `Cap` equals it) and the
//! never-allow refusal. The OSC-8 acceptance DECISION is bound via
//! `Model::action_enabled("Accept")` ⟺ the gate's observable behavior.
//!
//! Includes a NEGATIVE control: a fabricated never-allow admission is
//! rejected by the model, so a green run is never vacuous.

use std::collections::BTreeMap;

use aterm_core::terminal::Terminal;
use aterm_spec::derive::{Model, hyperlink_scheme_cap_model};
use aterm_spec::verify;

type State = BTreeMap<&'static str, i64>;

const NEVER_ALLOW: &[&str] = &["javascript", "data", "file", "vbscript", "about", "blob"];

/// Project the real terminal's live scheme-authorization state onto the model
/// variables: `orca` (the distinguished scheme), `others` (rest of the extra
/// set), `never` (1 iff any never-allow scheme is live — must stay 0).
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

/// Fire `action` on the model from `state` and require the real projection to
/// land exactly on the model's successor, then re-check via the tiered
/// transition validator.
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

/// Whether the real OSC-8 gate accepts an `orca:` URI right now — the
/// behavioral form of the model's `Accept` enabledness.
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

    // Accept is disabled before any mint — and the real gate refuses.
    assert!(!model.action_enabled("Accept", &state));
    assert!(!gate_accepts_orca(&mut term));

    // Authorize the distinguished scheme.
    assert!(term.authorize_hyperlink_scheme("orca"));
    bind_transition(&model, &mut state, "Authorize", project(&term));

    // The gate decision tracks the model's Accept enabledness (a self-loop).
    assert!(model.action_enabled("Accept", &state));
    assert!(gate_accepts_orca(&mut term));
    bind_transition(&model, &mut state, "Accept", project(&term));

    // Fill the remaining slots up to the REAL bound (model Cap == engine cap).
    for other in ["alpha", "beta", "gamma"] {
        assert!(term.authorize_hyperlink_scheme(other));
        bind_transition(&model, &mut state, "AuthorizeOther", project(&term));
    }

    // At capacity: the real mint refuses (returns false, state unchanged) —
    // exactly the model's RefuseAtCap self-loop.
    assert!(
        !term.authorize_hyperlink_scheme("delta"),
        "the real engine must refuse the mint past MAX_EXTRA_SCHEMES"
    );
    bind_transition(&model, &mut state, "RefuseAtCap", project(&term));

    // Never-allow refusal leaves the state untouched at Buggy=0.
    assert!(!term.authorize_hyperlink_scheme("javascript"));
    bind_transition(&model, &mut state, "RefuseNeverAllow", project(&term));

    // Revoke restores the default allowlist for the distinguished scheme...
    term.revoke_hyperlink_scheme("orca");
    bind_transition(&model, &mut state, "Revoke", project(&term));

    // ...and the gate decision flips off with the model.
    assert!(!model.action_enabled("Accept", &state));
    assert!(!gate_accepts_orca(&mut term));

    // NEGATIVE CONTROL (non-vacuity): a fabricated never-allow admission must
    // NOT be admitted as a RefuseNeverAllow transition of the Buggy=0 model.
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
