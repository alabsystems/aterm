// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// Registers the `trust` tool namespace under verify builds only, so `#[trust::…]` attributes
// compile everywhere and mean something under `targo trust build`.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's models of the native updater and its handoff.
//!
//! Each `*_model()` is a bounded state machine written once with `aterm_spec::ty_model!` (or a
//! hand-built `aterm_spec::derive::Model` under `#[aterm_spec::model]`); writing it registers it.
//! The engine (the macro, the interpreter, `verify`, `xref`) is `aterm-spec`; `aterm-models`
//! links every domain crate for `spec-check` and the registry-wide sweeps. Only the crates whose
//! tests drive these models dev-depend on this one, so editing a model here rebuilds this crate
//! and those tests, never a library.

mod models_control_bind_retry;
mod models_control_preparation;
mod models_rendezvous_grant;
mod models_update;
mod models_update_activation_observation;
mod models_update_carried_health;
mod models_update_check_coordination;
mod models_update_editor_carry;
mod models_update_environment_repair;
mod models_update_history_carry;
mod models_update_launched_dial_wait;
mod models_update_parallel_attach;
mod models_update_parent_diet;
mod models_update_precommit_input;
mod models_update_retired_intent;
mod models_update_roster_freshness;
mod models_update_settings_draft_carry;
mod models_update_successor_warm;
mod models_update_trial_give_back;
mod models_update_web_cache;
mod models_update_window_show;

pub use models_control_bind_retry::native_update_handoff_bind_retry_model;
pub use models_control_preparation::native_update_control_preparation_model;
pub use models_rendezvous_grant::native_update_rendezvous_grant_model;
pub use models_update::*;
pub use models_update_activation_observation::native_update_activation_observation_model;
pub use models_update_carried_health::native_update_carried_health_model;
pub use models_update_check_coordination::{
    native_update_boot_health_lock_model, native_update_check_join_model,
    native_update_check_receipt_model, native_update_check_wait_model,
    native_update_two_process_apply_model,
};
pub use models_update_editor_carry::native_update_editor_carry_model;
pub use models_update_environment_repair::native_update_environment_repair_model;
pub use models_update_history_carry::native_update_history_carry_model;
pub use models_update_launched_dial_wait::{
    native_update_launched_dial_wait_liveness, native_update_launched_dial_wait_model,
};
pub use models_update_parallel_attach::{
    native_update_successor_attach_liveness, native_update_successor_attach_model,
};
pub use models_update_parent_diet::{
    native_update_speculative_sidecar_liveness, native_update_speculative_sidecar_model,
};
pub use models_update_precommit_input::native_update_precommit_input_model;
pub use models_update_retired_intent::{
    native_update_failure_target_model, native_update_retired_intent_model,
};
pub use models_update_roster_freshness::native_update_roster_freshness_model;
pub use models_update_settings_draft_carry::native_update_settings_draft_carry_model;
pub use models_update_successor_warm::{
    native_update_successor_warm_before_claim_liveness,
    native_update_successor_warm_before_claim_model,
};
pub use models_update_trial_give_back::native_update_trial_give_back_model;
pub use models_update_web_cache::native_update_web_cache_model;
pub use models_update_window_show::native_update_window_show_model;
