// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// Registers the `trust` tool namespace under verify builds only, so `#[trust::…]` attributes
// compile everywhere and mean something under `targo trust build`.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! aterm's models of the agent harness and the supervisor.
//!
//! Each `*_model()` is a bounded state machine written once with `aterm_spec::ty_model!` (or a
//! hand-built `aterm_spec::derive::Model` under `#[aterm_spec::model]`); writing it registers it.
//! The engine (the macro, the interpreter, `verify`, `xref`) is `aterm-spec`; `aterm-models`
//! links every domain crate for `spec-check` and the registry-wide sweeps. Only the crates whose
//! tests drive these models dev-depend on this one, so editing a model here rebuilds this crate
//! and those tests, never a library.

mod models_agent_verdict;
mod models_disk;
mod models_harness;
mod models_harness_account_key;
mod models_harness_host;
mod models_harness_policy_writer;
mod models_harness_progress;
mod models_harness_state_collection;
mod models_occupancy;
mod models_program_queue;
mod models_program_resolution;
mod models_relaunch_prompt;
mod models_supervise;
mod models_supervise_accounts;
mod models_supervise_tape;
mod models_wakes;

pub use models_agent_verdict::claude_idle_at_composer_model;
pub use models_disk::{disk_await_liveness, disk_await_model, disk_reclaim_pass_model};
pub use models_harness::*;
pub use models_harness_account_key::harness_account_key_model;
pub use models_harness_host::{
    harness_agy_exit_witness_model, harness_agy_quiet_upgrade_model, harness_commit_owed_model,
    harness_exit_record_model, harness_gemini_exit_witness_model, harness_handoff_claim_model,
    harness_leave_model, harness_muse_exit_witness_model, harness_muse_quiet_upgrade_model,
    harness_relaunch_on_exit_model, harness_restart_stamp_liveness, harness_restart_stamp_model,
    harness_restored_carry_model, harness_restored_first_attempt_model, harness_upgrade_age_model,
    harness_upgrade_look_model, harness_worker_lifecycle_model,
};
pub use models_harness_policy_writer::{
    harness_policy_one_writer_liveness, harness_policy_one_writer_model,
};
pub use models_harness_progress::*;
pub use models_harness_state_collection::{
    harness_upgrade_state_collection_liveness, harness_upgrade_state_collection_model,
};
pub use models_occupancy::{occupancy_door_liveness, occupancy_door_model};
pub use models_program_queue::{
    claude_footer_model_model, claude_footer_watch_model, program_resolver_queue_model,
};
pub use models_program_resolution::program_resolution_retry_model;
pub use models_relaunch_prompt::{relaunch_prompt_read_liveness, relaunch_prompt_read_model};
pub use models_supervise::{
    supervisor_box_ask_model, supervisor_box_hold_model, supervisor_break_offer_model,
    supervisor_claim_model, supervisor_credits_hold_liveness, supervisor_credits_hold_model,
    supervisor_cruise_model, supervisor_decline_keys_model, supervisor_focus_choice_model,
    supervisor_host_turn_end_model, supervisor_land_liveness, supervisor_land_model,
    supervisor_model_switch_liveness, supervisor_model_switch_model, supervisor_network_wall_model,
    supervisor_question_answer_model, supervisor_question_text_model,
    supervisor_stall_remedy_model, supervisor_survey_press_model, supervisor_turn_end_model,
    supervisor_usage_wall_liveness, supervisor_usage_wall_model, upgrade_attended_turn_end_model,
    upgrade_refused_turn_end_model, upgrade_settle_turn_end_model,
};
pub use models_supervise_accounts::{
    supervisor_account_rotation_liveness, supervisor_account_rotation_model,
    supervisor_limit_reset_claim_liveness, supervisor_limit_reset_confirmation_liveness,
    supervisor_limit_reset_model, supervisor_usage_probe_liveness, supervisor_usage_probe_model,
};
pub use models_supervise_tape::{
    supervise_replay_cursor_model, supervise_tape_recovery_model, supervisor_raised_latch_model,
};
pub use models_wakes::{wake_book_liveness, wake_book_model, wake_room_liveness, wake_room_model};
