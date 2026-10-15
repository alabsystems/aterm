// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-spec's integration suite: ONE test binary for every file listed
//! below, instead of one linked and started per file.
//!
//! Each file keeps its path (every `tests/<file>.rs` citation still resolves)
//! and its own `#[test]`s; a test is named `<file>::<test>` here. The three
//! shared helpers under `tests/common/` are mounted ONCE, here — a file-backed
//! module mounted twice is compiled twice (`clippy::duplicate_mod`) — and each
//! file reaches them as `crate::conjunct_audit` / `crate::harness` /
//! `crate::walk`.
//!
//! Sharing a process was checked, not assumed: no file below sets or removes an
//! environment variable, installs a hook or an allocator, re-executes itself or
//! submits `inventory` records. Every `ty` spawn goes through
//! `aterm_spec::verify::ty_output`, which takes no lock, and the checker
//! drivers give each call a scratch directory of its own
//! (`verify::scratch_dir`), so two files checking one model at once cannot
//! tear each other's spec or cfg. `non_vacuity_ratchet` installs a panic hook
//! and is its own `[[test]]`; so are the three `ay` certificate files the
//! public export withholds.
//!
//! `autotests = false`: a `tests/*.rs` file that is neither a `mod` line here
//! nor a `[[test]]` of its own NEVER COMPILES. Run one file's tests with
//! `targo --unverified test -p aterm-spec --test spec_suite -- <file>::`.

#[path = "common/conjunct_audit.rs"]
mod conjunct_audit;
#[path = "common/harness.rs"]
mod harness;
#[path = "common/walk.rs"]
mod walk;

mod conformance_fabric_enable;
mod derived_atpkg_contention_release;
mod derived_atpkg_flip_quiet;
mod derived_atpkg_full_pass;
mod derived_atpkg_head_watch_hosts;
mod derived_atpkg_index_pending;
mod derived_atpkg_index_probe;
mod derived_atpkg_index_publish;
mod derived_atpkg_pass_stamps;
mod derived_atpkg_pending_wait;
mod derived_atpkg_published_spacing;
mod derived_atpkg_session_index;
mod derived_atpkg_session_index_retry;
mod derived_atpkg_vendor_pending;
mod derived_broadcast_checkpoint;
mod derived_broadcast_head;
mod derived_budgeted_search_resume_ty;
mod derived_chrome_regrow;
mod derived_console_pet_z_order;
mod derived_control_bind_retry;
mod derived_control_preparation;
mod derived_control_socket_handoff;
mod derived_crash_journal;
mod derived_debugger_access;
mod derived_desktop_alert_debt;
mod derived_driver_geometry;
mod derived_editor_carry;
mod derived_exceptions_digest;
mod derived_exceptions_once;
mod derived_fabric_outbox_wake;
mod derived_fabric_reconnect_backoff;
mod derived_fabric_refill_retry;
mod derived_fd_lifecycle_ty;
mod derived_foreground_handback;
mod derived_handoff_address_owner;
mod derived_harness_capture_worker;
mod derived_harness_codex_daemon_update;
mod derived_harness_host;
mod derived_harness_login_wall;
mod derived_harness_policy_one_writer;
mod derived_harness_relaunch;
mod derived_harness_restart;
mod derived_harness_restore_queue;
mod derived_harness_state_collection;
mod derived_harness_upgrade_age;
mod derived_harness_upgrade_drain_bound;
mod derived_harness_upgrade_goal_pause;
mod derived_harness_upgrade_ladder;
mod derived_harness_upgrade_limit_queue;
mod derived_harness_upgrade_never_strands;
mod derived_harness_upgrade_notice_owner;
mod derived_harness_upgrade_progress;
mod derived_input_unread;
mod derived_keeper;
mod derived_launched_dial_wait;
mod derived_main_stall_refusal;
mod derived_metal_drawable_acquire_ty;
mod derived_native_leaf_cache;
mod derived_net_slot_displacement;
mod derived_output_echo_ty;
mod derived_output_retention;
mod derived_paste_order_ty;
mod derived_pet_observation_admission;
mod derived_pet_surface_seam;
mod derived_precommit_input;
mod derived_relaunch_prompt_read;
mod derived_release_publish_once;
mod derived_render;
mod derived_rendezvous_admission;
mod derived_rendezvous_claim;
mod derived_rendezvous_grant;
mod derived_repaint_nudge;
mod derived_resize_row_reuse;
mod derived_ribbon_follow;
mod derived_ribbon_release_restoration;
mod derived_ribbon_row_hold;
mod derived_ring_ty;
mod derived_run_loop_waker;
mod derived_say_replay_budget;
mod derived_screen_generation;
mod derived_settings_draft_carry;
mod derived_streaming_search_ty;
mod derived_subscribe_announcement;
mod derived_successor_warm_before_claim;
mod derived_supervise;
mod derived_text_selection_lifecycle;
mod derived_trail_sound_ty;
mod derived_trust_facts;
mod derived_typed_rekey;
mod derived_update_carried_health;
mod derived_update_check_coordination;
mod derived_update_history_carry;
mod derived_update_parallel_attach;
mod derived_update_parent_diet;
mod derived_update_roster_freshness;
mod derived_update_structural_latch;
mod derived_update_web_cache;
mod derived_upgrade_attended;
mod derived_upgrade_settle;
mod derived_window_show;
mod foreign_facts_are_observed;
mod macro_ring;
mod macro_ty_model;
mod overclaim_corpus;
mod pet_stroke_ty;
mod present_coalescing_ty;
mod rainbow_composer_newline_ty;
mod rainbow_short_wrap_ty;
mod resize_throttle_ty;
mod rfc_worked_examples_ty;
mod ty_checker_canary;
mod ty_drivers_are_armed;
mod update_activation_observation;
mod window_routing_twin;
