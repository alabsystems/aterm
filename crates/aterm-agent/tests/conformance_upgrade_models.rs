// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 binding for `harness::upgrade_models` against the derived specs
//! `HarnessModelPriority` (the list), `HarnessModelLadder` (the ladder) and
//! `HarnessModelSwitch` (the switch). Each submodule drives the real code over every
//! reachable model state and validates each observed transition on every installed
//! tier; forged successors the model must refuse keep a green bind from being vacuous.

use std::collections::BTreeMap;

use aterm_spec::derive::Model;
use aterm_spec::verify;

#[path = "conformance_upgrade_models/ladder.rs"]
mod ladder;
#[path = "conformance_upgrade_models/priority.rs"]
mod priority;
#[path = "conformance_upgrade_models/switch.rs"]
mod switch;

type Vars = BTreeMap<&'static str, i64>;

/// Check one observed transition against the model's named action, on every
/// tier that is installed.
fn assert_transition(model: &Model, before: &Vars, after: &Vars, action: &str, label: &str) {
    let (accepted, diagnostics) = verify::validate_transition_tiered(
        model,
        &[("Buggy", 0)],
        before,
        after,
        Some(action),
        label,
    );
    assert!(
        accepted,
        "the real {label} is not the model's `{action}`: {before:?} -> {after:?}\n{diagnostics}"
    );
}

/// A successor the real code did not produce must be refused as `action`:
/// `base` is what the real code did produce, the forgery is `base` with
/// `changes` applied, offered as a successor of `before`.
fn assert_forged_rejected(
    model: &Model,
    before: &Vars,
    base: &Vars,
    action: &str,
    changes: &[(&'static str, i64)],
    label: &str,
) {
    let mut forged = base.clone();
    for (name, value) in changes {
        forged.insert(name, *value);
    }
    assert_ne!(
        &forged, base,
        "{label}: the forgery must differ from the truth"
    );
    let (accepted, diagnostics) = verify::validate_transition_tiered(
        model,
        &[("Buggy", 0)],
        before,
        &forged,
        Some(action),
        label,
    );
    assert!(
        !accepted,
        "a forged `{action}` was admitted, so the bind is vacuous: {before:?} -> {forged:?}\n\
         {diagnostics}"
    );
}
