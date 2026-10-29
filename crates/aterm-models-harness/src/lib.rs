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

mod models_account_adopt_identity;
mod models_account_adopt_source;
mod models_account_advice_queue;
mod models_account_alias_admission;
mod models_account_alias_relabel;
mod models_account_answer_observation;
mod models_account_application;
mod models_account_attempt_generation;
mod models_account_attempt_policy;
mod models_account_binding_retention;
mod models_account_claim_activity;
mod models_account_completion_identity;
mod models_account_controls;
mod models_account_create_label;
mod models_account_dashboard;
mod models_account_directory_admission;
mod models_account_gui_read;
mod models_account_host_identity;
mod models_account_identity;
mod models_account_identity_snapshot;
mod models_account_immediate;
mod models_account_landing;
mod models_account_landing_identity;
mod models_account_landing_snapshot;
mod models_account_launch_read;
mod models_account_lifecycle_commit;
mod models_account_local_commit;
mod models_account_local_observation;
mod models_account_login_attribution;
mod models_account_login_observation;
mod models_account_mailbox_precedence;
mod models_account_mailbox_scope;
mod models_account_mailbox_selection;
mod models_account_origin;
mod models_account_plain_login_commit;
mod models_account_policy;
mod models_account_pool_canary;
mod models_account_pool_commit;
mod models_account_pool_identity;
mod models_account_pool_launch;
mod models_account_pool_launch_policy;
mod models_account_pool_move_policy;
mod models_account_pool_observation_identity;
mod models_account_pool_planning;
mod models_account_pool_preparation;
mod models_account_pool_recovery;
mod models_account_pool_usage_policy;
mod models_account_process_context;
mod models_account_provider_realm;
mod models_account_queued_selection;
mod models_account_recovery_identity;
mod models_account_recovery_lock;
mod models_account_recovery_policy;
mod models_account_rename;
mod models_account_renewal_commit;
mod models_account_renewal_generation;
mod models_account_renewal_identity;
mod models_account_renewal_policy;
mod models_account_renewal_read;
mod models_account_report_read;
mod models_account_response;
mod models_account_restart_source;
mod models_account_retry_inputs;
mod models_account_return;
mod models_account_sample_context;
mod models_account_selection_commit;
mod models_account_served_identity;
mod models_account_session_attribution;
mod models_account_shared_selection;
mod models_account_sheet_columns;
mod models_account_signin;
mod models_account_signin_epoch;
mod models_account_signin_lock;
mod models_account_signin_observation_commit;
mod models_account_signin_policy;
mod models_account_signin_publication;
mod models_account_source_path;
mod models_account_source_presence;
mod models_account_source_refresh;
mod models_account_state_commit;
mod models_account_state_read;
mod models_account_state_retention;
mod models_account_sync_commit;
mod models_account_sync_new_labels;
mod models_account_sync_report;
mod models_account_tap_read;
mod models_account_usage_observation;
mod models_account_worker_environment;
mod models_agent_verdict;
mod models_codex_login_publication;
mod models_codex_refresh_authority;
mod models_codex_refresh_identity;
mod models_codex_refresh_publication;
mod models_codex_refresh_result;
mod models_codex_row_identity;
mod models_codex_stored_identity;
mod models_codex_usage_walls;
mod models_cold_resume_capability;
mod models_cold_resume_environment;
mod models_disk;
mod models_disk_projected;
mod models_disk_worker;
mod models_fleet_machine_assignment;
mod models_harness;
mod models_harness_account_key;
mod models_harness_host;
mod models_harness_owed_relaunch;
mod models_harness_policy_writer;
mod models_harness_progress;
mod models_harness_snapshot_read;
mod models_harness_state_collection;
mod models_left;
mod models_occupancy;
mod models_program_queue;
mod models_program_resolution;
mod models_relaunch_prompt;
mod models_shared_disposition;
mod models_shared_renewal;
mod models_supervise;
mod models_supervise_accounts;
mod models_supervise_tape;
mod models_transcript_process;
mod models_usage_source;
mod models_vault_refresh_context;
mod models_wakes;

pub use models_account_adopt_identity::account_adopt_identity_model;
pub use models_account_adopt_source::account_adopt_source_model;
pub use models_account_advice_queue::account_advice_queue_model;
pub use models_account_alias_admission::account_alias_admission_model;
pub use models_account_alias_relabel::account_alias_relabel_model;
pub use models_account_answer_observation::account_answer_observation_model;
pub use models_account_application::account_application_model;
pub use models_account_attempt_generation::account_attempt_generation_model;
pub use models_account_attempt_policy::account_attempt_policy_model;
pub use models_account_binding_retention::account_binding_retention_model;
pub use models_account_claim_activity::{
    account_claim_activity_model, account_claim_observation_model,
};
pub use models_account_completion_identity::account_completion_identity_model;
pub use models_account_controls::account_controls_model;
pub use models_account_create_label::account_create_label_model;
pub use models_account_dashboard::account_dashboard_model;
pub use models_account_directory_admission::account_directory_admission_model;
pub use models_account_gui_read::account_gui_read_model;
pub use models_account_host_identity::account_host_identity_model;
pub use models_account_identity::account_identity_model;
pub use models_account_identity_snapshot::account_identity_snapshot_model;
pub use models_account_immediate::account_immediate_model;
pub use models_account_landing::account_landing_model;
pub use models_account_landing_identity::account_landing_identity_model;
pub use models_account_landing_snapshot::account_landing_snapshot_model;
pub use models_account_launch_read::account_launch_read_model;
pub use models_account_lifecycle_commit::account_lifecycle_commit_model;
pub use models_account_local_commit::account_local_commit_model;
pub use models_account_local_observation::account_local_observation_model;
pub use models_account_login_attribution::account_login_attribution_model;
pub use models_account_login_observation::account_login_observation_model;
pub use models_account_mailbox_precedence::account_mailbox_precedence_model;
pub use models_account_mailbox_scope::account_mailbox_scope_model;
pub use models_account_mailbox_selection::account_mailbox_selection_model;
pub use models_account_origin::account_origin_model;
pub use models_account_plain_login_commit::plain_login_commit_model;
pub use models_account_policy::account_policy_model;
pub use models_account_pool_canary::account_pool_canary_model;
pub use models_account_pool_commit::account_pool_commit_model;
pub use models_account_pool_identity::account_pool_identity_model;
pub use models_account_pool_launch::account_pool_launch_model;
pub use models_account_pool_launch_policy::account_pool_launch_policy_model;
pub use models_account_pool_move_policy::account_pool_move_policy_model;
pub use models_account_pool_observation_identity::account_pool_observation_identity_model;
pub use models_account_pool_planning::account_pool_planning_model;
pub use models_account_pool_preparation::account_pool_preparation_model;
pub use models_account_pool_recovery::account_pool_recovery_model;
pub use models_account_pool_usage_policy::account_pool_usage_policy_model;
pub use models_account_process_context::account_process_context_model;
pub use models_account_provider_realm::account_provider_realm_model;
pub use models_account_queued_selection::account_queued_selection_model;
pub use models_account_recovery_identity::account_recovery_identity_model;
pub use models_account_recovery_lock::account_recovery_lock_model;
pub use models_account_recovery_policy::account_recovery_policy_model;
pub use models_account_rename::account_rename_model;
pub use models_account_renewal_commit::account_renewal_commit_model;
pub use models_account_renewal_generation::account_renewal_generation_model;
pub use models_account_renewal_identity::renewal_identity_model;
pub use models_account_renewal_policy::account_renewal_policy_model;
pub use models_account_renewal_read::account_renewal_read_model;
pub use models_account_report_read::{account_report_attribution_model, account_report_read_model};
pub use models_account_response::account_response_model;
pub use models_account_restart_source::account_restart_source_model;
pub use models_account_retry_inputs::account_retry_inputs_model;
pub use models_account_return::account_return_model;
pub use models_account_sample_context::account_sample_context_model;
pub use models_account_selection_commit::account_selection_commit_model;
pub use models_account_served_identity::account_served_identity_model;
pub use models_account_session_attribution::account_session_attribution_model;
pub use models_account_shared_selection::account_shared_selection_model;
pub use models_account_sheet_columns::account_sheet_columns_model;
pub use models_account_signin::account_signin_admission_model;
pub use models_account_signin_epoch::account_signin_epoch_model;
pub use models_account_signin_lock::account_signin_lock_model;
pub use models_account_signin_observation_commit::account_signin_observation_commit_model;
pub use models_account_signin_policy::account_signin_policy_model;
pub use models_account_signin_publication::signin_publication_model;
pub use models_account_source_path::account_source_path_model;
pub use models_account_source_presence::account_source_presence_model;
pub use models_account_source_refresh::account_source_refresh_model;
pub use models_account_state_commit::account_state_commit_model;
pub use models_account_state_read::account_state_read_model;
pub use models_account_state_retention::account_state_retention_model;
pub use models_account_sync_commit::account_sync_commit_model;
pub use models_account_sync_new_labels::account_sync_new_labels_model;
pub use models_account_sync_report::account_sync_report_model;
pub use models_account_tap_read::account_tap_read_model;
pub use models_account_usage_observation::account_usage_observation_model;
pub use models_account_worker_environment::account_worker_environment_model;
pub use models_agent_verdict::claude_idle_at_composer_model;
pub use models_codex_login_publication::codex_login_publication_model;
pub use models_codex_refresh_authority::codex_refresh_authority_model;
pub use models_codex_refresh_identity::codex_refresh_identity_model;
pub use models_codex_refresh_publication::codex_refresh_publication_model;
pub use models_codex_refresh_result::codex_refresh_result_model;
pub use models_codex_row_identity::codex_row_identity_model;
pub use models_codex_stored_identity::codex_stored_identity_model;
pub use models_codex_usage_walls::codex_usage_walls_model;
pub use models_cold_resume_capability::cold_resume_capability_model;
pub use models_cold_resume_environment::cold_resume_environment_model;
pub use models_disk::{
    disk_await_liveness, disk_await_model, disk_free_ask_model, disk_reclaim_pass_model,
};
pub use models_disk_projected::disk_projected_hold_model;
pub use models_disk_worker::disk_free_worker_model;
pub use models_fleet_machine_assignment::fleet_machine_assignment_model;
pub use models_harness::*;
pub use models_harness_account_key::harness_account_key_model;
pub use models_harness_host::{
    harness_agy_exit_witness_model, harness_agy_quiet_upgrade_model, harness_commit_owed_model,
    harness_exit_record_model, harness_handoff_claim_model, harness_leave_model,
    harness_muse_exit_witness_model, harness_muse_quiet_upgrade_model,
    harness_relaunch_on_exit_model, harness_restart_stamp_liveness, harness_restart_stamp_model,
    harness_restored_carry_model, harness_restored_first_attempt_model, harness_upgrade_age_model,
    harness_upgrade_look_model, harness_worker_lifecycle_model,
};
pub use models_harness_owed_relaunch::harness_owed_relaunch_model;
pub use models_harness_policy_writer::{
    harness_policy_one_writer_liveness, harness_policy_one_writer_model,
};
pub use models_harness_progress::*;
pub use models_harness_snapshot_read::harness_snapshot_read_model;
pub use models_harness_state_collection::{
    harness_upgrade_state_collection_liveness, harness_upgrade_state_collection_model,
};
pub use models_left::{left_write_liveness, left_write_model};
pub use models_occupancy::{occupancy_door_liveness, occupancy_door_model};
pub use models_program_queue::{
    claude_footer_model_model, claude_footer_watch_model, program_resolver_queue_model,
};
pub use models_program_resolution::program_resolution_retry_model;
pub use models_relaunch_prompt::{relaunch_prompt_read_liveness, relaunch_prompt_read_model};
pub use models_shared_disposition::shared_disposition_model;
pub use models_shared_renewal::{
    renewal_snapshot_model, shared_begin_model, shared_browser_model, shared_checkpoint_model,
    shared_renewal_model,
};
pub use models_supervise::{
    supervisor_box_ask_model, supervisor_box_hold_model, supervisor_break_offer_model,
    supervisor_claim_model, supervisor_condition_resume_model, supervisor_credits_hold_liveness,
    supervisor_credits_hold_model, supervisor_cruise_model, supervisor_decline_keys_model,
    supervisor_focus_choice_model, supervisor_host_turn_end_model, supervisor_land_liveness,
    supervisor_land_model, supervisor_model_switch_liveness, supervisor_model_switch_model,
    supervisor_outage_wait_model, supervisor_question_answer_model, supervisor_question_text_model,
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
pub use models_transcript_process::transcript_process_model;
pub use models_usage_source::usage_source_model;
pub use models_vault_refresh_context::vault_refresh_context_model;
pub use models_wakes::{
    wake_book_left_liveness, wake_book_liveness, wake_book_model, wake_room_liveness,
    wake_room_model,
};
