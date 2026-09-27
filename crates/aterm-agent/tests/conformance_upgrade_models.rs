// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 binding for the live upgrade's model priority list
//! (`harness::upgrade_models`, spec `HarnessModelPriority`).
//!
//! A `ty`-green model on its own is a statement about the DESCRIPTION of the
//! code. These tests make it a statement about the code that compiled: they
//! drive the real `Priority::admit`, `render`/`parse` and
//! `upgrade_models::target_allowed` over every reachable state of the derived
//! model, project what those produce onto the model's variables, and check
//! every observed transition against the same model checked at Tier 0 — by
//! the in-process interpreter always, and additionally by `ty trace validate`
//! wherever that binary is installed. FORGED successors the model must refuse
//! keep a green bind from being vacuous.

use std::collections::BTreeMap;

use aterm_spec::derive::Model;
use aterm_spec::verify;

#[path = "conformance_upgrade_models/priority.rs"]
mod priority;

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

/// A successor the real code did NOT produce must be refused as `action`:
/// `base` is what the real code DID produce, the forgery is `base` with
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
