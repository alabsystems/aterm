// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::{
    derive::{
        native_update_activation_observation_model, native_update_environment_repair_model,
        native_update_failure_target_model, native_update_retired_intent_model,
    },
    verify,
};

#[test]
fn activation_observation_proves_and_catches_unknown_retirement() {
    verify::prove_and_catch_scalar(
        &native_update_activation_observation_model(),
        "activation observation preserves unknown identity",
    );
}

#[test]
fn retired_intent_proves_and_catches_obsolete_latch() {
    verify::prove_and_catch_scalar(
        &native_update_retired_intent_model(),
        "retired stage releases its exact retry latch",
    );
}

#[test]
fn failure_target_proves_and_catches_replacement_misattribution() {
    verify::prove_and_catch_scalar(
        &native_update_failure_target_model(),
        "returned apply failure retains its original target",
    );
}

#[test]
fn environment_repair_proves_and_catches_permanent_refusal() {
    verify::prove_and_catch_scalar(
        &native_update_environment_repair_model(),
        "verified installed-source repair releases its environmental block",
    );
}
