// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tier-0 for DERIVED specs: the TLA+ generated from a Rust `Model` (one source)
//! is exhaustively model-checked by the real `ty` binary.
//!
//! This is the derivation half of `docs/RFC-ty-embed-derived-tla.md`: no
//! hand-written `.tla` — `Model::to_tla()` produces the module and `.to_cfg()` the
//! config, and `ty check` proves the invariants hold over the whole bounded state
//! space. Change the model, the spec changes, and this re-checks the new spec.
//! Drift is impossible by construction. Both the single-action ring AND the
//! two-action cursor (which exercises `UNCHANGED` + a disjunctive `Next`) are
//! checked, so the derivation is shown to generalize.
//!
//! VERIFICATION GATE (two-tier, see [`aterm_spec::verify`]): every obligation is
//! discharged by the IN-PROCESS interpreter (exhaustive BFS of the same bounded
//! model — a real check, toolchain-free), and ADDITIONALLY by the external `ty`
//! binary wherever it is installed. The tiers must agree; disagreement panics.

// The property models are iterated via `harness::instances()`, not named here.
use aterm_spec::derive::{
    Model, aa_edge_hardening_model, active_handle_model, alt_archive_pool_model,
    alt_selection_park_model, anchored_artifact_transaction_model, artifact_handoff_capacity_model,
    artifact_reader_lease_model, artifact_reply_publication_model, asymmetric_pad_layout_model,
    capture_after_present_model, channel_bind_model, chrome_face_gate_model,
    claude_footer_watch_model, claude_idle_at_composer_model, clipboard_mailbox_model,
    closed_recovery_ledgers_model, coalesce_model, companion_tenure_flicker_model,
    composed_sync_hold_model, composite_accessibility_route_model, config_catalog_snapshot_model,
    config_file_commit_cas_model, contrast_floor_model, control_connection_admission_model,
    control_lane_tenure_model, ct_frac_bearing_model, cursor_cat_curse_wince_model,
    cursor_cat_earn_floor_model, cursor_cat_fold_model, cursor_cat_model,
    cursor_cat_motion_pulse_routing_model, cursor_companion_owner_lifecycle_model,
    cursor_cutout_clip_model, cursor_effect_scroll_model, cursor_hint_license_model, cursor_model,
    cursor_scroll_signal_model, cursor_viewport_lifecycle_model, damage_to_present_model,
    deco_band_containment_model, deco_phase_model, done_mark_lru_model, dsu_quiescence_model,
    echo_ledger_bridge_model, effect_phase_lock_model, effect_present_rebase_model,
    effect_presentability_settle_model, emacs_search_navigation_model, evict_full_model,
    exact_instance_retention_model, exact_profanity_completion_model, fallback_band_clip_model,
    fallback_precedence_model, fallback_scale_clamp_model, fd_handoff_no_leak_model,
    flash_limiter_model, flash_limiter_window_model, focus_modifier_cache_model,
    gpu_loss_recovery_model, gpu_loss_route_model, grid_translate_model, handoff_roundtrip_model,
    harness_model_ladder_model, harness_model_priority_model, harness_model_switch_model,
    hdr_present_gate_model, hdr_reconfigure_retag_model, hyperlink_scheme_cap_model,
    idle_deadline_model, ignition_reservation_lifecycle_model, ignition_reservation_rekey_model,
    inject_floor_model, input_release_pairing_model, kernel_model, key_injectivity_model,
    kitty_collectibles_model, kitty_flush_worker_model, kitty_pin_merge_model,
    kitty_sidecar_durability_model, kitty_sing_detector_model, layout_coordinate_reset_model,
    ligature_gate_model, manual_config_completion_model, manual_config_diagnostics_lane_model,
    manual_config_handoff_model, manual_config_problem_navigation_model, mint_reachability_model,
    motion_policy_model, native_async_delivery_model, native_capture_source_model,
    native_close_plan_model, native_config_observation_handoff_model,
    native_config_transaction_model, native_control_routing_model,
    native_document_publication_model, native_document_queue_model, native_draft_journal_model,
    native_editor_command_palette_model, native_editor_modal_model, native_editor_viewport_model,
    native_file_watch_model, native_markdown_history_model, native_markdown_viewport_model,
    native_packages_worker_model, native_recovery_interaction_model, native_reopen_ledger_model,
    native_save_intent_latch_model, native_settings_draft_close_model,
    native_settings_singleton_model, native_tab_identity_model, native_update_admission_model,
    native_update_apply_ladder_model, native_update_attempt_identity_model,
    native_update_auto_intent_model, native_update_disk_transaction_model,
    native_update_failed_mark_suppression_model, native_update_hidden_output_quiet_model,
    native_update_menu_activation_model, native_update_overlap_handoff_model,
    native_update_seamless_handoff_ownership_model, native_update_status_reconciliation_model,
    native_update_worker_queue_model, native_updater_model, net_capability_grant_model,
    net_dial_after_grant_model, notify_follow_checkpoint_model, nova_phase_model,
    one_shot_peek_model, operator_event_delivery_model, operator_fleet_fault_model,
    operator_leadership_model, operator_resync_cursor_model, operator_wal_actuator_model,
    output_streak_attribution_model, output_streak_episode_delivery_model, pad_absorption_model,
    pane_tree_model, path_feed_snapshot_model, per_window_metrics_model,
    predictive_echo_visibility_model, present_retry_model, presentation_gate_model,
    presented_frame_tap_model, press_custody_model, program_resolution_retry_model,
    program_resolver_queue_model, proxy_forward_model, rain_band_containment_model,
    rain_ignition_model, rain_lifecycle_model, rainbow_exit_sampling_model,
    rainbow_idle_twinkle_model, rainbow_landing_pool_model, rainbow_typed_continuity_model,
    read_image_seq_model, recording_model, recovery_redraw_model,
    reduced_motion_companion_handoff_model, release_channel_floor_model,
    release_claim_landing_model, release_durable_post_intent_model,
    release_historical_recovery_model, release_journal_prefix_model,
    release_published_identity_model, release_publisher_fence_model,
    release_yank_successor_first_model, restore_manifest_single_use_model, ring_model,
    roster_pair_redo_model, same_caret_typed_echo_model, scroll_glide_model,
    scrollback_maintenance_lane_model, seamless_nonce_model, selection_custody_model,
    self_governor_model, semantic_prewarm_generation_model, semantic_prewarm_handshake_model,
    semantic_prewarm_request_swap_model, serious_mode_intent_queue_model, serious_mode_model,
    session_chrome_expiry_model, session_id_claim_model, session_pool_model,
    settings_page_scroll_model, shade_phase_model, shared_budget_model,
    snapshot_generation_commit_model, snapshot_model, sparkle_identity_model,
    sparkle_persist_capacity_model, sparkle_reflow_cardinality_model, sparkle_retype_rearm_model,
    spawn_locale_model, startup_phase_publication_model, stream_fade_gate_model,
    strike_selection_model, styled_run_face_model, subscribe_model, supernova_burst_mutex_model,
    surface_coverage_model, sync_reopen_visibility_model, tab_nav_model, tab_stop_handoff_model,
    tab_strip_model, text_blend_gate_model, tier_residency_model,
    title_summary_managed_endpoint_model, title_summary_model,
    title_summary_observation_scheduler_model, title_summary_runtime_model,
    title_summary_socket_owner_retry_model, top_anchored_scroll_history_model,
    trail_audio_lifecycle_model, trail_audio_start_latency_model, transact_model,
    unknown_insert_orphan_key_model, vf_axis_clamp_model, vf_nudge_gate_model,
    vibrancy_contrast_model, video_batch_publication_durability_model,
    video_recording_lifecycle_model, video_tap_slot_model, visible_pad_crop_model,
    watcher_failure_recovery_model, watcher_latch_model, wide_center_model, window_routing_model,
};
use aterm_spec::verify;
use std::process::Command;

/// The POLICY this file states for the THREE function-valued models it drives
/// (EvictFull, TierResidency, Recording): report the miss and keep going. For
/// every other model here — all scalar — the interpreter tier discharges the
/// obligation unconditionally and this is a no-op, since [`verify::NotRun`] is
/// only reachable when a function-valued model meets a machine with no Trust
/// `ty`.
///
/// Skipping rather than failing is deliberate. A hard require would make
/// `cargo test -p aterm-spec --test derived_ring_ty` — the file you iterate on
/// while editing a model — unrunnable without the Trust toolchain, and it would
/// buy no coverage: each of the three has a toolchain-free Tier-1 conformance
/// twin binding it to shipping code (`aterm-buffer`'s `conformance_evict_full`
/// and `conformance_temporal`, `aterm-core`'s `conformance_recording`). What
/// must never happen is the miss passing SILENTLY, which is exactly what
/// dropping the old `Discharge::NotRun` with a bare statement did: now the
/// `Result` makes stating a policy unskippable, and this line makes the chosen
/// one visible in the test output.
fn tier0_or_skip(discharge: Result<verify::Covered, verify::NotRun>) {
    if let Err(verify::NotRun { model }) = discharge {
        eprintln!(
            "TIER-0 SKIPPED (this test is NOT a pass for it): `{model}` is function-valued and \
             Trust `ty` is not installed — see the escalation notice above. Its Tier-1 \
             conformance twin is unaffected."
        );
    }
}

/// TIERED Tier-0 check: the interpreter proves every invariant over the whole
/// bounded reachable space (always), and `ty check` additionally proves the
/// generated TLA+ wherever the binary is installed (see
/// [`verify::check_model_tiered`]). Function-valued models (EvictFull) run on
/// the `ty` tier only — the interpreter cannot evaluate them, so with no `ty`
/// they take [`tier0_or_skip`]'s skip-loudly path.
fn assert_model_checks(m: &Model) {
    tier0_or_skip(verify::check_model_tiered(m, m.name));
}

/// The ring's live window never exceeds `Cap`, and the late-eviction mutant (one
/// row over budget, the alt-archive undercharge) is `LenBounded`'s counterexample
/// on the push that should have evicted.
#[test]
fn derived_ring_spec_proves_and_catches_late_eviction() {
    let model = ring_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut state = buggy.init_state();
    for _ in 0..4 {
        assert!(buggy.fire("Push", &mut state));
    }
    assert_eq!(
        (state["seq"], state["lo"]),
        (4, 1),
        "the fourth row is kept"
    );
    assert!(!buggy.check_invariant("LenBounded", &state));
}

/// NEW-1 of the live e2e (2026-09-26): the server's `idle` for a Claude
/// Code is the handshake an orchestrator types its first prompt on. Idle is
/// published only for the NEW REPL drawn whole, the terminal's cursor in its
/// prompt box, so a prompt typed on it is never lost, on every path — a new
/// folder's trust dialog, pressed, or a folder trusted before; in a new tab,
/// or the inline renderer relaunched in the same tab with the previous run's
/// prompt box still on the screen. The old reader (`Buggy=1`: idle for any
/// screen with no box and no spinner) publishes idle on the shell's rows the
/// dialog's press leaves; the reader of 2026-09-26 (`Buggy=2`: idle at the
/// last caret's whole box, the cursor not asked) publishes idle on the
/// previous run's box while nothing of the new launch is drawn under it (the
/// review of 2026-09-26: a draft lost 3 of 3). The prompt typed there is
/// lost — each catches both invariants.
#[test]
fn derived_claude_is_idle_only_at_its_composer() {
    let model = claude_idle_at_composer_model();
    assert_proves_and_catches(&model);

    type State = std::collections::BTreeMap<&'static str, i64>;
    fn step(m: &Model, state: &State, action: &str) -> State {
        m.successors(action, state)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("{action} is enabled"))
    }
    for relaunched in [false, true] {
        let start = if relaunched {
            step(&model, &model.init_state(), "Relaunched")
        } else {
            model.init_state()
        };
        let asked = step(&model, &start, "AskTrust");
        let looked = step(&model, &asked, "Look");
        assert_eq!(looked["published"], 2, "the dialog is a prompt");
        let pressed = step(&model, &looked, "PressTrust");
        let erased = step(&model, &pressed, "Look");
        assert_eq!(erased["published"], 0, "the erased dialog is not idle");
        assert!(!model.action_enabled("Type", &erased));
        let half = step(&model, &step(&model, &erased, "DrawHalf"), "Look");
        assert_eq!(half["published"], 0, "the half-drawn REPL is not idle");
        let whole = step(&model, &step(&model, &half, "DrawWhole"), "Look");
        assert_eq!(whole["published"], 1, "the REPL is idle");
        let typed = step(&model, &whole, "Type");
        assert_eq!(typed["lost"], 0);
        // A folder trusted before: no dialog, the same rule.
        let trusted = step(&model, &start, "AlreadyTrusted");
        assert_eq!(step(&model, &trusted, "Look")["published"], 0);

        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let old = step(&buggy, &pressed, "Look");
        assert_eq!(
            old["published"], 1,
            "the old reader: the erased dialog read idle"
        );
        assert!(!buggy.check_invariant("IdleIsTheComposer", &old));
        let lost = step(&buggy, &old, "Type");
        assert!(!buggy.check_invariant("NoFirstPromptLost", &lost));

        // The reader of 2026-09-26: any whole box on the screen.
        let framed = aterm_spec::interp::with_buggy(&model, 2);
        let seen = step(&framed, &pressed, "Look");
        assert_eq!(
            seen["published"],
            i64::from(relaunched),
            "the previous run's box read idle only in the same tab"
        );
        let early = step(&framed, &step(&framed, &start, "AlreadyTrusted"), "Look");
        assert_eq!(early["published"], i64::from(relaunched));
        if relaunched {
            assert!(!framed.check_invariant("IdleIsTheComposer", &seen));
            let lost = step(&framed, &seen, "Type");
            assert!(!framed.check_invariant("NoFirstPromptLost", &lost));
        }
    }
    // Both invariants hold over every state the fix reaches, and each buggy
    // reader reaches a state that breaks them.
    for bug in [1, 2] {
        let buggy = aterm_spec::interp::with_buggy(&model, bug);
        let mut seen = vec![buggy.init_state()];
        let mut next = 0;
        let mut broke = [false, false];
        while next < seen.len() {
            let state = seen[next].clone();
            next += 1;
            broke[0] |= !buggy.check_invariant("IdleIsTheComposer", &state);
            broke[1] |= !buggy.check_invariant("NoFirstPromptLost", &state);
            for action in &buggy.actions {
                for s in buggy.successors(action.name, &state) {
                    if !seen.contains(&s) {
                        seen.push(s);
                    }
                }
            }
        }
        assert_eq!(broke, [true, true], "Buggy={bug}");
    }
}

#[test]
fn derived_program_resolution_retries_a_static_miss_and_stops_after_a_name() {
    let model = program_resolution_retry_model();
    assert_proves_and_catches(&model);

    let started = model.successors("Start", &model.init_state())[0].clone();
    assert_eq!(started["attempts"], 1);
    assert_eq!(started["armed"], 1);
    let elapsed = model.successors("Elapse", &started)[0].clone();
    let decided = model.successors("Decide", &elapsed)[0].clone();
    assert_eq!(decided["retry_due"], 1, "no screen movement required");
    let retried = model.successors("Retry", &decided)[0].clone();
    assert_eq!(retried["attempts"], 2);
    let resolved = model.successors("Resolve", &retried)[0].clone();
    assert_eq!(resolved["known"], 1);
    assert_eq!(resolved["armed"], 0);
    assert!(!model.action_enabled("Elapse", &resolved));

    let shell = model.successors("ResolveShell", &started)[0].clone();
    assert_eq!(shell["confirm_armed"], 1);
    let shell_elapsed = model.successors("Elapse", &shell)[0].clone();
    let confirmed = model.successors("ConfirmShell", &shell_elapsed)[0].clone();
    assert_eq!(confirmed["confirm_armed"], 0, "stable sh is quiescent");
    let moved = model.successors("MoveNamed", &confirmed)[0].clone();
    assert_eq!(moved["confirm_armed"], 1, "a later frame owns a recheck");
    let claude = model.successors("NameBecomesClaude", &moved)[0].clone();
    assert_eq!(claude["agent"], 1);
    assert_eq!(claude["confirm_armed"], 0);
    let shell_again = model.successors("AgentLeaves", &claude)[0].clone();
    assert_eq!(shell_again["confirm_armed"], 1);
    let departed = model.successors("GroupLeaves", &shell_again)[0].clone();
    assert_eq!(departed["confirm_armed"], 0);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let old_shell = buggy.successors("ResolveShell", &started)[0].clone();
    assert!(!buggy.check_invariant("NamedShellKeepsNeededDeadline", &old_shell));
    let old_move = buggy.successors("MoveNamed", &confirmed)[0].clone();
    assert!(!buggy.check_invariant("NamedShellKeepsNeededDeadline", &old_move));
}

#[test]
fn derived_program_resolver_coalesces_replacements_and_recovers_a_crash() {
    let model = program_resolver_queue_model();
    assert_proves_and_catches(&model);

    let mut state = model.init_state();
    for action in ["AskFirst", "Take", "AskNewGroup", "AskAgain", "Finish"] {
        assert!(model.fire(action, &mut state), "{action}");
    }
    assert_eq!(state["queued"], 1, "the latest group keeps one token");
    assert_eq!(state["completed"], 1, "the old lookup cannot consume it");
    assert!(model.fire("Crash", &mut state));
    assert!(model.fire("Restart", &mut state));
    assert_eq!(state["queued"], 1, "restart restores the retained token");
    assert!(model.fire("Take", &mut state));
    assert!(model.fire("Finish", &mut state));
    assert_eq!(state["completed"], 2);
    assert_eq!(state["queued"], 0);
}

#[test]
fn derived_claude_footer_watch_retires_and_keeps_a_replacement() {
    let model = claude_footer_watch_model();
    assert_proves_and_catches(&model);

    let mut state = model.init_state();
    for action in ["AskOld", "AskNew", "StopOld"] {
        assert!(model.fire(action, &mut state), "{action}");
    }
    assert_eq!(state["watch"], 2);
    assert!(model.action_enabled("IdleRead", &state));
    assert!(model.fire("StopSession", &mut state));
    assert_eq!(state["watch"], 0);
    assert!(!model.action_enabled("IdleRead", &state));
}

/// TERMINAL MODES: `ty` proves that either reset (DECSTR / RIS) leaves the
/// terminal usable — cursor shown in the host shape, autowrap on, no mouse
/// capture, no synchronized-output hold (`ResetRestoresDefaults`) — and catches
/// the DECSTR that forgets the synchronized-output hold (Buggy=1 ->
/// counterexample: the frozen-screen class). Tier-1: aterm-core's
/// `terminal_modes_conformance` drives every anchored action through real bytes.
#[test]
fn derived_terminal_modes_proves_and_catches_stuck_sync_hold() {
    assert_proves_and_catches(&aterm_spec::derive::terminal_modes_model());
}

#[test]
fn derived_cursor_proves_and_catches_overshoot() {
    // Exercises the multi-action / UNCHANGED generation path through `ty`, and
    // catches the delivery that parks the cursor one past the head (Buggy=1 ->
    // counterexample on CursorBounded). Tier-1: aterm-buffer's
    // `tests/conformance_cursor.rs` binds it to the real `Surface::poll`.
    assert_proves_and_catches(&cursor_model());
}

#[test]
fn derived_momentum_wait_read_proves_and_catches_expired_reparking() {
    assert_proves_and_catches(&aterm_spec::derive::momentum_wait_read_model());
}

#[test]
fn derived_clipboard_mailbox_proves_one_latest_pending_write() {
    let model = clipboard_mailbox_model();
    assert_proves_and_catches(&model);

    let mut state = model.init_state();
    for action in [
        "PublishClipboard",
        "PublishPrimary",
        "PublishClipboard",
        "Consume",
    ] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
        assert!(model.check_invariant("OneLatestPendingWrite", &state));
    }

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut unbounded = buggy.init_state();
    assert!(buggy.fire("PublishClipboard", &mut unbounded));
    assert!(buggy.check_invariant("OneLatestPendingWrite", &unbounded));
    assert!(buggy.fire("PublishClipboard", &mut unbounded));
    assert!(
        !buggy.check_invariant("OneLatestPendingWrite", &unbounded),
        "the retired queue must fail the bounded/latest invariant"
    );
}

#[test]
fn derived_native_document_queue_proves_bounded_nonblocking_admission() {
    let model = native_document_queue_model();
    assert_proves_and_catches(&model);

    let mut state = model.init_state();
    for action in [
        "SubmitAccepted",
        "WorkerStarts",
        "ConsumeDrainEdge",
        "SubmitAccepted",
        "SubmitAccepted",
        "SubmitAccepted",
        "SubmitAccepted",
        "SubmitDeferred",
        "WorkerCompletes",
        "WorkerStarts",
        "RetryAccepted",
    ] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
        for invariant in [
            "BoundedFullImageJobs",
            "DeferredIntentRetained",
            "OpenSlotHasRetryEdge",
        ] {
            assert!(
                model.check_invariant(invariant, &state),
                "{action}: {state:?}"
            );
        }
    }
    assert_eq!(state["queued"], 4);
    assert_eq!(state["executing"], 1);
    assert_eq!(state["retry_pending"], 0);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut unbounded = buggy.init_state();
    assert!(buggy.fire("SubmitAccepted", &mut unbounded));
    assert!(buggy.fire("WorkerStarts", &mut unbounded));
    for _ in 0..5 {
        assert!(buggy.fire("SubmitAccepted", &mut unbounded));
    }
    assert!(
        !buggy.check_invariant("BoundedFullImageJobs", &unbounded),
        "one executing plus five queued images must exceed the retained cap"
    );

    let mut lost = buggy.init_state();
    for action in [
        "SubmitAccepted",
        "WorkerStarts",
        "SubmitAccepted",
        "SubmitAccepted",
        "SubmitAccepted",
        "SubmitAccepted",
        "SubmitDeferred",
    ] {
        assert!(buggy.fire(action, &mut lost), "{action}: {lost:?}");
    }
    assert!(
        !buggy.check_invariant("DeferredIntentRetained", &lost),
        "the retired Full path must expose its dropped retry intent"
    );

    let mut missing_wake = model.init_state();
    for action in [
        "SubmitAccepted",
        "WorkerStarts",
        "ConsumeDrainEdge",
        "SubmitAccepted",
        "SubmitAccepted",
        "SubmitAccepted",
        "SubmitAccepted",
        "SubmitDeferred",
        "WorkerCompletes",
    ] {
        assert!(model.fire(action, &mut missing_wake));
    }
    assert!(buggy.fire("WorkerStarts", &mut missing_wake));
    assert!(
        !buggy.check_invariant("OpenSlotHasRetryEdge", &missing_wake),
        "capacity release without a wake must strand the retained intent"
    );
}

#[test]
fn derived_evict_full_spec_model_checks() {
    // The FUNCTION-VALUED faithful ring: proves EvictOldestContiguous over a
    // live: [1..MaxSeq -> BOOLEAN] set — the property the scalar ring can't express.
    assert_model_checks(&evict_full_model());
}

/// A model using the `Buggy` convention: the invariant must be PROVEN at the
/// committed `Buggy=0`, and a COUNTEREXAMPLE found at `Buggy=1` — so the
/// invariant is non-trivial AND genuinely catches the bug. TIERED: the
/// interpreter always runs the whole protocol; `ty` additionally re-proves it
/// wherever installed (see [`verify::prove_and_catch_tiered`]). The two
/// function-valued models routed here (TierResidency, Recording) take
/// [`tier0_or_skip`]'s skip-loudly path when `ty` is absent.
fn assert_proves_and_catches(m: &Model) {
    tier0_or_skip(verify::prove_and_catch_tiered(m, m.name));
}

/// The stronger form of [`assert_every_invariant_carries_a_mutant`]: every
/// design invariant must be the ONLY law some `Buggy = 1` step breaks — a
/// transition out of a reachable state where every invariant still holds, into
/// one where that law, and no other, fails.
///
/// Isolation alone accepts an invariant that is only ever reached through a
/// state another law already refused: an inline `Buggy` arm that makes one
/// slip unavoidable before the next can fire, or a mutant whose harm is a
/// consequence of an earlier one. Being among the laws a first step breaks is
/// not enough either: a law that repeats clauses of others breaks on exactly
/// their steps, never alone, and its catch is theirs. Such a law carries no
/// catch of its own, and the per-invariant ratchet cannot see that. This walks
/// the `Buggy = 1` space from every all-good state and demands that each design
/// law be the sole law some single step breaks.
fn assert_every_invariant_breaks_first(m: &Model, bounds_guards: &[&str]) {
    assert_every_invariant_carries_a_mutant(m, bounds_guards);
    let buggy = aterm_spec::interp::with_buggy(m, 1);
    let all_hold = |st: &aterm_spec::interp::State| {
        m.invariants
            .iter()
            .all(|inv| buggy.check_invariant(inv.name, st))
    };
    let key = |st: &aterm_spec::interp::State| -> Vec<(&'static str, i64)> {
        st.iter().map(|(k, v)| (*k, *v)).collect()
    };
    let mut alone: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = std::collections::VecDeque::new();
    let init = buggy.init_state();
    seen.insert(key(&init));
    queue.push_back(init);
    while let Some(st) = queue.pop_front() {
        assert!(
            seen.len() < 100_000,
            "{}: Buggy=1 good space unbounded",
            m.name
        );
        for action in &buggy.actions {
            for next in buggy.successors(action.name, &st) {
                if all_hold(&next) {
                    if seen.insert(key(&next)) {
                        queue.push_back(next);
                    }
                } else {
                    let broken: Vec<&str> = m
                        .invariants
                        .iter()
                        .filter(|inv| !buggy.check_invariant(inv.name, &next))
                        .map(|inv| inv.name)
                        .collect();
                    if let [only] = broken.as_slice() {
                        alone.insert(only);
                    }
                }
            }
        }
    }
    let shared: Vec<&str> = m
        .invariants
        .iter()
        .map(|inv| inv.name)
        .filter(|name| !bounds_guards.contains(name) && !alone.contains(name))
        .collect();
    assert!(
        shared.is_empty(),
        "{}: {shared:?} never break alone — every Buggy=1 step from an all-good state \
         that breaks them breaks another law too (or they break only after another law \
         already broke), so their catch is some other law's",
        m.name
    );
}

/// PER-INVARIANT non-vacuity: every invariant a model states as a DESIGN CLAIM
/// must be falsified by the `Buggy = 1` family when it is checked ALONE.
///
/// `prove_and_catch` stops at the FIRST violated invariant, so a model can pass it
/// with one live property carrying the whole catch and every other invariant a
/// ghost — true by construction, unfalsifiable by any mutant, stating nothing about
/// the code. That is not hypothetical: `press_custody_model` shipped it once over a
/// self-reported flag, and `selection_custody_model` shipped it again with five of
/// eight invariants unfalsifiable. Isolating each invariant is what turns "the
/// model catches the defect" into "each named law catches its own defect".
///
/// `bounds_guards` names the invariants that state the SPACE rather than the design
/// (`StateBounds` and friends). They are expected NOT to be caught, and the
/// assertion is two-sided on purpose: adding a real invariant to that list to
/// silence a failure fails the test instead.
fn assert_every_invariant_carries_a_mutant(m: &Model, bounds_guards: &[&str]) {
    for guard in bounds_guards {
        assert!(
            m.invariants.iter().any(|inv| inv.name == *guard),
            "{}: `{guard}` is named as a bounds guard but the model has no such invariant",
            m.name
        );
    }
    // ONE implementation of the isolation sweep, shared with the workspace-wide
    // ratchet (`non_vacuity_ratchet.rs`) so the two can never drift apart.
    let uncaught = aterm_spec::verify::uncaught_invariants(m);
    for inv in &m.invariants {
        let caught = !uncaught.contains(&inv.name);
        if bounds_guards.contains(&inv.name) {
            assert!(
                !caught,
                "{}: `{}` is named as a bounds guard but a Buggy=1 member falsifies it — it is \
                 a design claim, so move it out of the guard list",
                m.name, inv.name
            );
        } else {
            assert!(
                caught,
                "{}: NO Buggy=1 member falsifies `{}` — it is a ghost invariant that cannot \
                 fail whatever the code does, exactly the failure this suite exists to prevent",
                m.name, inv.name
            );
        }
    }
}

/// The committed-dead actions of `m` are exactly `expected`, and each is an
/// independently caught mutant (`verify::audit_dead_negative_controls`: the
/// all-live `Buggy=1` baseline is safe, and each dead action, added back ALONE,
/// fires and is caught). This is the interpreter half of the `aterm-gui` gate's
/// `ty --strict-vacuity` audit, pinned where the mutants are written, so a mutant
/// folded back into a live action — which masks its neighbours and removes a
/// healthy path from the `Buggy=1` world — fails here by name.
fn assert_committed_dead_are_caught_mutants(m: &Model, expected: &[&str]) {
    let healthy_fired = aterm_spec::interp::fired_actions(&aterm_spec::interp::with_buggy(m, 0));
    let dead: Vec<_> = m
        .actions
        .iter()
        .map(|action| action.name)
        .filter(|name| !healthy_fired.contains(name))
        .collect();
    assert_eq!(dead, expected, "{}: committed-dead actions", m.name);
    assert_eq!(
        verify::audit_dead_negative_controls(m, expected),
        Ok(expected.len()),
        "{}: every committed-dead action must remain an independently caught mutant",
        m.name
    );
}

/// Operator models are registered model-checking inputs, not one-off examples:
/// every committed action must be reachable, both constant settings must stay
/// exhaustively bounded, and a work-in-progress state must always have a next
/// step. `is_final` excludes only the model's explicit work-complete terminal.
fn assert_operator_model_shape(
    model: &Model,
    is_final: impl Fn(&aterm_spec::interp::State) -> bool,
) {
    let registered = aterm_spec::xref::model_registry()
        .into_iter()
        .any(|candidate| candidate.name == model.name);
    assert!(
        registered,
        "{} must resolve through the global spec↔source registry",
        model.name
    );

    verify::audit_dead_negative_controls(model, &[]).unwrap_or_else(|reason| {
        panic!(
            "{} must have full committed-config action coverage: {reason}",
            model.name
        )
    });

    let buggy = aterm_spec::interp::with_buggy(model, 1);
    let declared: std::collections::BTreeSet<_> =
        model.actions.iter().map(|action| action.name).collect();
    let buggy_fired = aterm_spec::interp::fired_actions(&buggy);
    assert_eq!(
        buggy_fired, declared,
        "{} must remain bounded and exercise every action at Buggy=1",
        model.name
    );

    let deadlock =
        aterm_spec::interp::find_deadlock(&aterm_spec::interp::with_buggy(model, 0), is_final);
    assert!(
        deadlock.is_none(),
        "{} must not wedge before its legitimate terminal: {deadlock:?}",
        model.name
    );
}

#[test]
fn derived_subscribe_proves_and_catches_silent_loss() {
    assert_proves_and_catches(&subscribe_model());
}

#[test]
fn derived_notify_follow_checkpoint_proves_and_catches_premature_cursor() {
    assert_proves_and_catches(&notify_follow_checkpoint_model());
}

#[test]
fn derived_operator_event_delivery_proves_and_catches_stale_claim() {
    let model = operator_event_delivery_model();
    assert_operator_model_shape(&model, |state| state[&"phase"] == 2 || state[&"phase"] == 5);
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &["Bounds"]);

    // The cap slip escalates on the FIRST expiry, and the in-doubt slip drops
    // the token the human reconciliation must name. Each has its own trace, so
    // neither can hide behind the stale-ack counterexample ty reports first.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let claimed = buggy.successors("Claim", &buggy.init_state())[0].clone();
    let expired = buggy.successors("Expire", &claimed)[0].clone();
    let early = buggy.successors("ReclaimAsEscalation", &expired)[0].clone();
    assert_eq!(early["redeliveries"], 1);
    assert!(!buggy.check_invariant("EscalationOccursAtCap", &early));
    let final_claim = buggy.successors("ClaimEscalation", &early)[0].clone();
    let final_expired = buggy.successors("Expire", &final_claim)[0].clone();
    let tokenless = buggy.successors("ExpiredEscalationInDoubt", &final_expired)[0].clone();
    assert_eq!((tokenless["phase"], tokenless["token"]), (5, 0));
    assert!(!buggy.check_invariant("ClaimStateOwnsToken", &tokenless));
}

#[test]
fn derived_operator_wal_actuator_proves_and_catches_interjection_or_replay() {
    let model = operator_wal_actuator_model();
    assert_operator_model_shape(&model, |state| state[&"phase"] == 4);
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &[]);

    // One ordinary-action trace per WAL-law slip.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let idle = buggy.init_state();
    let paste_first = buggy.successors("MutateOnce", &idle)[0].clone();
    assert!(!buggy.check_invariant("MutationRequiresDurableIntent", &paste_first));

    let intent = buggy.successors("PersistIntent", &idle)[0].clone();
    let pasted = buggy.successors("MutateOnce", &intent)[0].clone();
    let unsubmitted = buggy.successors("PersistResult", &pasted)[0].clone();
    assert_eq!(unsubmitted["submit_writes"], 0);
    assert!(!buggy.check_invariant("ResultFollowsOneSubmittedMutation", &unsubmitted));

    // The re-grant: a revoked permit validated again, after which the guarded
    // submit is enabled once more.
    let revoked = buggy.successors("InvalidateAuthority", &pasted)[0].clone();
    assert_eq!(revoked["authority_valid"], 0);
    assert!(model.check_invariant("RevocationIsFinal", &revoked));
    let regranted = buggy.successors("InvalidateAuthority", &revoked)[0].clone();
    assert_eq!(
        (
            regranted["authority_valid"],
            regranted["authority_invalidated"]
        ),
        (1, 1)
    );
    assert!(!buggy.check_invariant("RevocationIsFinal", &regranted));
    assert!(!buggy.successors("GuardedSubmit", &regranted).is_empty());
    assert!(model.successors("InvalidateAuthority", &revoked).is_empty());

    let submitted = buggy.successors("GuardedSubmit", &pasted)[0].clone();
    let finished = buggy.successors("PersistResult", &submitted)[0].clone();
    assert!(model.check_invariant("DurableOutcomesAreExclusive", &finished));
    let doubted = buggy.successors("CrashAfterMutation", &finished)[0].clone();
    assert!(!buggy.check_invariant("DurableOutcomesAreExclusive", &doubted));

    let acked_in_flight = buggy.successors("ResolveInDoubt", &intent)[0].clone();
    assert!(!buggy.check_invariant("ResolutionHasDurableOutcome", &acked_in_flight));
}

#[test]
fn derived_operator_resync_cursor_proves_and_catches_silent_loss() {
    let model = operator_resync_cursor_model();
    assert_operator_model_shape(&model, |_| false);
    assert_proves_and_catches(&model);
}

#[test]
fn derived_operator_leadership_proves_and_catches_split_brain() {
    let model = operator_leadership_model();
    assert_operator_model_shape(&model, |state| {
        state[&"a_live"] == 0 && state[&"b_live"] == 0 && state[&"epoch"] == 3
    });
    assert_proves_and_catches(&model);
}

#[test]
fn derived_operator_fleet_fault_proves_and_catches_blocked_egress() {
    let model = operator_fleet_fault_model();
    assert_operator_model_shape(&model, |_| false);
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &["Bounds"]);

    // A latch that swallowed the marker write, and a clear that skipped its
    // in-doubt scan, each on its own trace.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let unmarked = buggy.successors("LatchFault", &buggy.init_state())[0].clone();
    assert_eq!((unmarked["phase"], unmarked["marker"]), (2, 0));
    assert!(!buggy.check_invariant("MarkerOwnsEveryBlockedPhase", &unmarked));

    let prepared = buggy.successors("PrepareFault", &buggy.init_state())[0].clone();
    let faulted = buggy.successors("CommitFault", &prepared)[0].clone();
    // The same failed write during Rebaseline leaves the old marker in place: a
    // swallowed error cannot delete a marker, only fail to create one.
    let rebaseline = buggy.successors("BeginClear", &faulted)[0].clone();
    let relatched = buggy.successors("LatchFault", &rebaseline)[0].clone();
    assert_eq!((relatched["phase"], relatched["marker"]), (2, 1));
    assert!(buggy.check_invariant("MarkerOwnsEveryBlockedPhase", &relatched));

    let mut ambiguous = buggy.successors("BeginClearWithInDoubt", &faulted)[0].clone();
    for _ in 0..2 {
        ambiguous = buggy.successors("BaselineOne", &ambiguous)[0].clone();
    }
    assert_eq!((ambiguous["pending"], ambiguous["in_doubt"]), (0, 1));
    let committed = buggy.successors("CommitClear", &ambiguous)[0].clone();
    assert!(!buggy.check_invariant("ClearCommitHasNoAmbiguity", &committed));
}

#[test]
fn derived_native_settings_draft_close_proves_and_catches_loss() {
    let model = native_settings_draft_close_model();
    let initial = model.init_state();
    let dirty = model
        .successors("Edit", &initial)
        .into_iter()
        .next()
        .expect("Edit creates one retained draft state");

    assert_every_invariant_carries_a_mutant(
        &model,
        &["FlagsBounded", "ResultBounded", "PreservationBounded"],
    );

    let mut unsafe_close = dirty.clone();
    unsafe_close.insert("close_result", 2);
    unsafe_close.insert("recovery_visible", 0);
    assert!(
        !model.check_invariant("DirtyNeverReady", &unsafe_close),
        "negative control: a dirty Ready verdict must be rejected"
    );
    assert!(
        !model.check_invariant("DirtyRecoveryVisible", &unsafe_close),
        "negative control: blocked recovery cannot disappear"
    );

    let mut one_click_loss = dirty;
    one_click_loss.insert("draft", 0);
    one_click_loss.insert("discard_armed", 1);
    one_click_loss.insert("recovery_visible", 0);
    assert!(
        !model.check_invariant("ConfirmationOwnsDraft", &one_click_loss),
        "negative control: the first destructive gesture cannot drop the draft"
    );
    assert_proves_and_catches(&model);
}

/// Host-minted OSC-8 hyperlink scheme capability (orca deep-links §7): PROVES
/// the extra-scheme set stays bounded and never-allow schemes are refused at
/// `Buggy=0`; CATCHES the over-cap grow and the never-allow admission at
/// `Buggy=1`. Tier-1 binds the real `HyperlinkAuth` in aterm-core
/// (`conformance_hyperlink_scheme_cap.rs`).
#[test]
fn derived_hyperlink_scheme_cap_proves_and_catches_never_allow_admission() {
    assert_proves_and_catches(&hyperlink_scheme_cap_model());
}

/// Proof-carrying dynamic software update (RFC "Proof-Carrying DSU", Rung 0): a
/// dynamic update may be applied to a RUNNING process only at a QUIESCENCE point —
/// applying while a computation is in flight tears state (old-layout value resumed
/// under new code). PROVES `NoTear` at Buggy=0 (the quiescence-gated apply is safe),
/// CATCHES the mid-flight apply at Buggy=1. This is the safety PRECONDITION the DSU
/// mechanism must honor; pinning it here means the "only at quiescence" rule is a
/// checked theorem, not a code comment.
#[test]
fn derived_dsu_quiescence_proves_and_catches_midflight_tear() {
    assert_proves_and_catches(&dsu_quiescence_model());
}

/// Proof-carrying DSU (RFC Rung 1a): the seamless re-exec hands the session set to
/// the new binary as a manifest that must round-trip EXACTLY — no session lost, none
/// fabricated. PROVES `NoLossNoFabricate` at Buggy=0, CATCHES a dropped session at
/// Buggy=1. Concretely bound to `SessionHandoff`'s real serde round-trip
/// (`session_store.rs`).
#[test]
fn derived_handoff_roundtrip_proves_and_catches_dropped_session() {
    assert_proves_and_catches(&handoff_roundtrip_model());
}

/// Proof-carrying DSU (RFC Rung 1b): the seamless re-exec clears FD_CLOEXEC on each PTY
/// master so it survives the exec; every such master must then be RE-ADOPTED or CLOSED,
/// never left dangling (a leaked, ungated PTY channel). PROVES `NoLeak` at Buggy=0,
/// CATCHES the dropped-without-closing fd at Buggy=1. The CLOEXEC survival itself is
/// proven with real syscalls in `aterm-pty` (`cloexec_controls_master_survival_across_exec`).
#[test]
fn derived_fd_handoff_no_leak_proves_and_catches_dangling_fd() {
    assert_proves_and_catches(&fd_handoff_no_leak_model());
}

/// Proof-carrying DSU (RFC Rung 1b, live wiring): the seamless update-apply authenticates
/// the inherited fd map with a SINGLE-USE nonce stamp — minted into the `0700` dir, then
/// consumed (read-then-unlink) before any fd is trusted. A presented nonce must authorize
/// AT MOST ONCE: a replayed `ATERM_SEAMLESS_FDS` after one adoption finds no stamp and
/// fails closed. PROVES `NoReplay` (`accepted <= minted /\ replayed = 0`) at Buggy=0, and
/// CATCHES the replayable (not-unlinked) stamp at Buggy=1. Concretely bound to the real
/// read-then-unlink consume (`control_auth::consume_seamless_stamp`) by aterm-gui's
/// `seamless_stamp_is_single_use_and_fails_closed` conformance test.
#[test]
fn derived_seamless_nonce_proves_and_catches_replay() {
    assert_proves_and_catches(&seamless_nonce_model());
}

/// Observation Kernel (RFC "The Reactive Surface", L0): the no-silent-loss latch
/// — a transiently-true surface predicate must be caught at the `post_process`
/// seam, never lost to a coalescing consumer wake. PROVES at Buggy=0, CATCHES the
/// deferred-to-wake coalescing bug at Buggy=1. Bound to the real engine by
/// `aterm-core/tests/conformance_observe.rs`.
#[test]
fn derived_watcher_latch_proves_and_catches_silent_loss() {
    assert_proves_and_catches(&watcher_latch_model());
}

/// Damage→present bounded response (the 2026-07-05 five-fps incident): pending
/// PTY damage must reach a present within `Expiry + 1` ticks of the wake-latch
/// protocol — a lost `Wake::Output` may cost one bounded heal window, never
/// process-lifetime present starvation. PROVES the bound with the self-expiring
/// latch (Buggy=0, `spawn::gated_output_wake`'s `WAKE_LATCH_EXPIRY_NS`),
/// CATCHES the shipped one-shot latch (Buggy=1 → Damage, Lose, Tick* — the
/// exact incident trace) as a counterexample.
#[test]
fn derived_damage_to_present_proves_and_catches_starvation() {
    assert_proves_and_catches(&damage_to_present_model());
}

/// Observation Kernel (RFC L0): the single armed idle deadline must equal the
/// minimum of all pending `IdleFor` deadlines, so an earlier wake is never
/// missed. PROVES `armed = min` at Buggy=0, CATCHES the keep-first bug at
/// Buggy=1. Bound to the real engine by `WatcherSet::next_deadline`.
#[test]
fn derived_idle_deadline_proves_and_catches_missed_earliest() {
    assert_proves_and_catches(&idle_deadline_model());
}

/// A valid first-present phase ledger is published only after all eight
/// intervals exist. PROVES the abstract completeness gate and CATCHES an
/// early-valid publication. Timestamp order and exact sums are host-level pure
/// derivation obligations, not claims of this count model.
#[test]
fn derived_startup_phase_publication_requires_all_eight_intervals() {
    let model = startup_phase_publication_model();
    let mut state = model.init_state();
    for completed in 1..=8 {
        assert!(model.fire("Step", &mut state));
        assert_eq!(state["phase"], completed);
        if completed < 8 {
            assert!(
                model.successors("Publish", &state).is_empty(),
                "a {completed}/8 timeline must not publish as valid"
            );
        }
    }
    assert!(model.fire("Publish", &mut state));
    assert_eq!(state["published"], 1);
    assert!(model.check_invariant("CompleteBeforeValidPublication", &state));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut early = buggy.init_state();
    assert!(buggy.fire("Publish", &mut early));
    assert!(
        !buggy.check_invariant("CompleteBeforeValidPublication", &early),
        "negative control: an empty timeline cannot publish as valid"
    );
    assert_proves_and_catches(&model);
}

/// A dropped surface may autonomously retry only on strictly-future deadlines
/// and only while finite episode fuel remains. One pristine pre-first-present
/// `GpuOccluded` drop owns exactly one such bootstrap retry. PROVES the
/// delayed/bounded train and CATCHES the old immediate, non-consuming redraw
/// loop.
#[test]
fn derived_present_retry_proves_future_bounded_recovery_and_catches_unbounded_train() {
    let model = present_retry_model();
    let idle = model.init_state();
    assert!(
        model.successors("Stimulus", &idle).is_empty(),
        "an ordinary external input at idle is a production no-op"
    );
    let forced = model.successors("ForcedStimulus", &idle);
    assert_eq!(forced.len(), 1);
    assert_eq!(forced[0]["outstanding"], 1);

    let bootstrap = model.successors("DropBootstrapOccluded", &idle);
    assert_eq!(
        bootstrap.len(),
        1,
        "the pristine initial retry state admits one GpuOccluded bootstrap retry"
    );
    let mut after_bootstrap_drop = bootstrap[0].clone();
    assert_eq!(after_bootstrap_drop["remaining"], 5);
    assert_eq!(after_bootstrap_drop["train"], 0);
    assert_eq!(after_bootstrap_drop["retry"], 1);
    assert_eq!(after_bootstrap_drop["ready"], 0);
    assert_eq!(after_bootstrap_drop["parked"], 0);
    assert_eq!(after_bootstrap_drop["outstanding"], 0);
    assert_eq!(after_bootstrap_drop["bootstrap_available"], 0);
    assert!(
        model.check_invariant("RetryDeadlineIsStrictlyFuture", &after_bootstrap_drop),
        "the bootstrap retry must use the same strictly-future deadline encoding"
    );
    assert!(model.fire("Wake", &mut after_bootstrap_drop));
    assert_eq!(after_bootstrap_drop["remaining"], 4);
    assert_eq!(after_bootstrap_drop["train"], 1);
    assert_eq!(after_bootstrap_drop["retry"], 0);
    assert_eq!(after_bootstrap_drop["ready"], 1);
    assert_eq!(after_bootstrap_drop["outstanding"], 1);
    assert!(
        model
            .successors("DropBootstrapOccluded", &after_bootstrap_drop)
            .is_empty(),
        "Tier-0 negative control: the consumed bootstrap permit cannot retry a second occlusion"
    );
    assert_eq!(
        model
            .successors("DropPersistent", &after_bootstrap_drop)
            .len(),
        1,
        "the negative control remains a live drop state; only the bootstrap classification is disabled"
    );

    let mut presented_without_bootstrap = idle.clone();
    assert!(model.fire("Present", &mut presented_without_bootstrap));
    assert_eq!(presented_without_bootstrap["bootstrap_available"], 0);
    assert!(
        model
            .successors("DropBootstrapOccluded", &presented_without_bootstrap)
            .is_empty(),
        "a successful first present permanently closes bootstrap admission"
    );

    // Synchronous capture/internal stimuli redraw inside the same main-thread
    // operation, so they do not create a new OS request. They do preserve one
    // that was already outstanding, matching the shipping reducer.
    let mut synchronous = idle.clone();
    assert!(model.fire("DropPersistent", &mut synchronous));
    let mut coupled = synchronous.clone();
    assert!(model.fire("Stimulus", &mut coupled));
    assert_eq!(coupled["outstanding"], 1);
    assert!(
        model
            .successors("DropBootstrapOccluded", &coupled)
            .is_empty(),
        "negative control: a coupled outstanding redraw is not pristine bootstrap state"
    );

    assert!(model.fire("SynchronousStimulus", &mut synchronous));
    assert_eq!(synchronous["remaining"], 5);
    assert_eq!(synchronous["train"], 0);
    assert_eq!(synchronous["retry"], 0);
    assert_eq!(synchronous["ready"], 1);
    assert_eq!(synchronous["parked"], 0);
    assert_eq!(synchronous["outstanding"], 0);
    assert_eq!(synchronous["bootstrap_available"], 1);
    assert_eq!(
        model
            .successors("DropBootstrapOccluded", &synchronous)
            .len(),
        1,
        "a synchronous pre-first-present attempt preserves the one bootstrap permit"
    );
    assert!(model.fire("DropBootstrapOccluded", &mut synchronous));
    assert!(model.fire("Wake", &mut synchronous));
    assert!(model.fire("SynchronousStimulus", &mut synchronous));
    assert_eq!(synchronous["bootstrap_available"], 0);
    assert_eq!(synchronous["outstanding"], 1);
    assert!(
        model
            .successors("DropBootstrapOccluded", &synchronous)
            .is_empty(),
        "Tier-0 negative control: a later synchronous reset cannot replenish the consumed permit"
    );
    assert_eq!(
        model.successors("DropPersistent", &synchronous).len(),
        1,
        "the consumed-permit control remains a live present/drop state"
    );

    let mut past_deadline = model.init_state();
    past_deadline.insert("ready", 0);
    past_deadline.insert("retry", 2);
    assert!(
        !model.check_invariant("RetryDeadlineIsStrictlyFuture", &past_deadline),
        "Tier-0 negative control: the deleted immediate/past retry must be rejected"
    );

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let Err((state, invariant)) = aterm_spec::interp::bmc(&buggy) else {
        panic!("Buggy=1 must exceed the finite autonomous retry train");
    };
    assert_eq!(invariant, "AutonomousTrainBound");
    assert_eq!(state["train"], 6);
    assert_proves_and_catches(&model);
}

/// A failed acquire from a latched dead GPU must route straight to CPU fallback,
/// never into the transient surface-retry train. PROVES the route and CATCHES
/// the historical retry-on-loss branch that could exhaust and park forever.
#[test]
fn derived_gpu_loss_routes_to_fallback_and_catches_retry_on_dead_device() {
    let model = gpu_loss_route_model();
    let mut retry_mutant = model.init_state();
    retry_mutant.insert("lost", 1);
    retry_mutant.insert("route", 1);
    assert!(
        !model.check_invariant("LostUsesFallback", &retry_mutant),
        "Tier-0 negative control: retrying a latched dead GPU must be rejected"
    );
    assert_proves_and_catches(&model);
}

/// The complete lost-device transaction must abort GPU recording and, from the
/// fresh post-present state, arm a bounded typed retry without counting one
/// frame twice. PROVES its safety/conditional continuation and CATCHES all three
/// deleted effects; it does not assume an unavailable CPU builder eventually works.
#[test]
fn derived_gpu_loss_recovery_schedules_once_and_stops_recording() {
    let model = gpu_loss_recovery_model();

    let mut no_retry = model.init_state();
    no_retry.insert("path", 1);
    no_retry.insert("fallback_failed", 1);
    no_retry.insert("retry", 0);
    no_retry.insert("drops", 1);
    no_retry.insert("reason", 2);
    no_retry.insert("recording", 0);
    assert!(!model.check_invariant("UnexhaustedFailureOwnsRetryOrDeliveredAttempt", &no_retry));

    let mut double_count = no_retry.clone();
    double_count.insert("retry", 1);
    double_count.insert("drops", 2);
    assert!(!model.check_invariant("OneDropCountPerFrame", &double_count));

    let mut recording_wedge = no_retry;
    recording_wedge.insert("retry", 1);
    recording_wedge.insert("recording", 1);
    assert!(!model.check_invariant("LossStopsGpuRecording", &recording_wedge));

    // Conditional continuation: once the future deadline is delivered, a
    // successful external CPU build first reaches ready+redraw-outstanding;
    // only the separate present transition reaches glass. This demonstrates
    // the protocol without asserting environmental fairness or conflating
    // `request_redraw` with a completed present.
    let mut recovery = model.init_state();
    assert!(model.fire("FailFallbackAfterPresent", &mut recovery));
    assert!(model.fire("Wake", &mut recovery));
    assert!(model.fire("BuildCpuAfterWake", &mut recovery));
    assert_eq!(recovery["cpu_ready"], 1);
    assert_eq!(recovery["requested"], 1);
    assert_eq!(recovery["cpu_presented"], 0);
    assert!(model.fire("PresentCpu", &mut recovery));
    assert_eq!(recovery["cpu_presented"], 1);
    assert_eq!(recovery["fallback_failed"], 0);

    // Exhausted prior fuel is an intentional bounded park, not a claimed
    // autonomous retry. A genuine external stimulus is proven separately by
    // PresentRetry + RecoveryRedraw.
    let mut exhausted = model.init_state();
    assert!(model.fire("FailFallbackAfterDropExhausted", &mut exhausted));
    assert_eq!(exhausted["retry"], 0);
    assert_eq!(exhausted["parked"], 1);

    assert_proves_and_catches(&model);

    let mutants = [
        "BuggyLossKeepsRecording",
        "BuggyFailedPresentOmitsRetry",
        "BuggyDropCountedTwice",
        "BuggyFuelledDropKeepsSourceReason",
        "BuggyExhaustedArmsRetry",
        "BuggyWakeKeepsDeadline",
        "BuggyReadyWithoutRedraw",
        "BuggyPresentOnWake",
    ];
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &mutants),
        Ok(mutants.len()),
        "each slip of the recovery transaction must fire and be caught on its own"
    );

    // Every trace below is a trace of the Buggy=1 model alone: its live
    // actions are the correct transaction, and only the named mutant slips.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let init = buggy.init_state();
    let step = |from: &aterm_spec::interp::State, action: &str| {
        let next = buggy.successors(action, from);
        assert_eq!(next.len(), 1, "{action} is deterministic at {from:?}");
        next[0].clone()
    };

    let alive = step(&init, "BuggyLossKeepsRecording");
    assert!(!buggy.check_invariant("LossStopsGpuRecording", &alive));

    let unarmed = step(&init, "BuggyFailedPresentOmitsRetry");
    assert_eq!((unarmed["retry"], unarmed["parked"]), (0, 1));
    assert!(!buggy.check_invariant("UnexhaustedFailureOwnsRetryOrDeliveredAttempt", &unarmed));

    let doubled = step(&init, "BuggyDropCountedTwice");
    assert!(!buggy.check_invariant("OneDropCountPerFrame", &doubled));

    // The fuelled failed-present path refines the drop but keeps the source
    // surface's reason, so the failed CPU fallback is never named.
    let undiagnosed = step(&init, "BuggyFuelledDropKeepsSourceReason");
    assert_eq!(
        (undiagnosed["fallback_failed"], undiagnosed["reason"]),
        (1, 1)
    );
    assert!(!buggy.check_invariant("FailedFallbackIsDiagnosed", &undiagnosed));

    // Exhausted fuel treated as a fresh drop arms a retry past the cap.
    let unparked = step(&init, "BuggyExhaustedArmsRetry");
    assert_eq!((unparked["retry"], unparked["parked"]), (1, 0));
    assert!(!buggy.check_invariant("ExhaustedFailureIsParked", &unparked));

    // `take_due` that hands the wake over but keeps the consumed deadline,
    // after a correct fresh retry the Buggy=1 model itself armed.
    let armed = step(&init, "FailFallbackAfterPresent");
    assert_eq!(armed["retry"], 1);
    let rewoken = step(&armed, "BuggyWakeKeepsDeadline");
    assert_eq!((rewoken["delivered"], rewoken["retry"]), (1, 1));
    assert!(!buggy.check_invariant("DeliveredRetryHasNoDeadline", &rewoken));

    // Source ready classified without its `request_redraw`: nothing presents.
    let frozen = step(&init, "BuggyReadyWithoutRedraw");
    assert_eq!((frozen["cpu_ready"], frozen["requested"]), (1, 0));
    assert!(!buggy.check_invariant("ReadyCpuOwnsRedrawUntilPresent", &frozen));

    // The delivered wake taken as the acknowledging present, before any CPU
    // target exists — from the correct wake of that same armed retry.
    let woken = step(&armed, "Wake");
    for invariant in &model.invariants {
        assert!(
            buggy.check_invariant(invariant.name, &woken),
            "{}",
            invariant.name
        );
    }
    assert!(!buggy.action_enabled("PresentCpu", &woken));
    let acknowledged = step(&woken, "BuggyPresentOnWake");
    assert_eq!(
        (acknowledged["cpu_presented"], acknowledged["cpu_ready"]),
        (1, 0)
    );
    assert!(!buggy.check_invariant("CpuPresentWasReady", &acknowledged));

    assert_every_invariant_breaks_first(&model, &["Bounds"]);
}

/// Resetting unresolved recovery state must deliver a redraw in the same host
/// action. PROVES the coupled edge and CATCHES the gate-open/no-redraw mutant.
#[test]
fn derived_recovery_stimulus_requests_redraw_and_catches_silent_reset() {
    let model = recovery_redraw_model();
    let mut silent_reset = model.init_state();
    silent_reset.insert("unresolved", 0);
    silent_reset.insert("stimulated", 1);
    assert!(
        !model.check_invariant("RecoveryStimulusRequestsRedraw", &silent_reset),
        "Tier-0 negative control: a silent recovery reset must be rejected"
    );

    let mut repeated = model.init_state();
    assert!(model.fire("Stimulus", &mut repeated));
    assert!(model.fire("Suppress", &mut repeated));
    assert_eq!(repeated["unresolved"], 1);
    assert!(model.fire("Stimulus", &mut repeated));
    assert_eq!(repeated["requested"], 1);
    assert!(model.fire("Present", &mut repeated));
    assert_eq!(repeated["unresolved"], 0);
    assert_eq!(repeated["presented"], 1);
    assert_proves_and_catches(&model);

    let mutants = ["BuggyStimulusAcknowledges", "BuggyStimulusWithoutRedraw"];
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &mutants),
        Ok(mutants.len()),
        "each half of the frozen-window defect must fire and fail on its own"
    );
    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    // The gate reopened with no redraw requested — the silent reset above, now
    // reached by a transition rather than hand-built.
    let silent = buggy.successors("BuggyStimulusWithoutRedraw", &buggy.init_state())[0].clone();
    assert_eq!((silent["stimulated"], silent["requested"]), (1, 0));
    assert!(!buggy.check_invariant("RecoveryStimulusRequestsRedraw", &silent));

    // Acknowledged on the first stimulus: the edge winit then suppresses is
    // never replaced.
    let early = buggy.successors("BuggyStimulusAcknowledges", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("OnlyPresentAcknowledgesRecovery", &early));
    let suppressed = buggy.successors("Suppress", &early)[0].clone();
    assert!(!buggy.action_enabled("Stimulus", &suppressed));

    assert_every_invariant_breaks_first(&model, &["Bounds"]);
}

/// Font zoom may leave an odd raw-surface remainder. PROVES that the present
/// covers the whole surface with live-background bands and CATCHES the deleted
/// frame-sized viewport/scissor that left the trailing pixels stale.
#[test]
fn derived_surface_coverage_proves_full_live_bands_and_catches_frame_viewport() {
    let model = surface_coverage_model();
    let mut old_frame_viewport = model.init_state();
    old_frame_viewport.insert("presented", 1);
    old_frame_viewport.insert("covered", old_frame_viewport["frame"]);
    old_frame_viewport.insert("band_live", 0);
    assert!(
        !model.check_invariant("PresentCoversSurface", &old_frame_viewport),
        "Tier-0 negative control: a frame-sized viewport must not cover the raw surface"
    );
    assert!(
        !model.check_invariant("RemainderUsesLiveBackground", &old_frame_viewport),
        "Tier-0 negative control: an uncleared remainder must not pass as a live band"
    );
    assert_proves_and_catches(&model);
}

/// Adaptive predictions on a fast link may be tracked but are never pixels;
/// Codex's application-owned composer may neither arm nor paint a prediction.
/// PROVES both visibility laws and CATCHES the deleted confirmation-only gate.
#[test]
fn derived_predictive_echo_proves_no_flash_and_catches_immediate_display() {
    let model = predictive_echo_visibility_model();

    let mut old_fast_expiry = model.init_state();
    old_fast_expiry.insert("confirmed", 1);
    old_fast_expiry.insert("pending", 0);
    old_fast_expiry.insert("visible", 0);
    old_fast_expiry.insert("erased", 1);
    assert!(
        !model.check_invariant("InvisibleExpiryCannotErase", &old_fast_expiry),
        "Tier-0 negative control: a fast-link visible erase must be rejected"
    );

    let mut old_codex_ghost = model.init_state();
    old_codex_ghost.insert("app_owned", 1);
    old_codex_ghost.insert("confirmed", 1);
    old_codex_ghost.insert("pending", 1);
    old_codex_ghost.insert("visible", 1);
    assert!(
        !model.check_invariant("AppOwnedHasNoPrediction", &old_codex_ghost),
        "Tier-0 negative control: an app-owned ghost must be rejected"
    );

    let mut inherited_remote_rtt = model.init_state();
    inherited_remote_rtt.insert("slow", 1);
    assert!(
        !model.check_invariant("FreshSessionHasNoInheritedRtt", &inherited_remote_rtt),
        "Tier-0 negative control: a fresh pane must reject an inherited slow-link RTT"
    );

    assert_proves_and_catches(&model);
}

/// The incident models must participate in the repository-wide spec-link and
/// strict-vacuity closure, not only their direct Tier-0/Tier-1 tests. This is a
/// regression lock for the registry seam consumed by aterm-gui's closure gate.
#[test]
fn zoom_and_typing_incident_models_are_registered_for_global_verification() {
    let registered: std::collections::BTreeSet<_> = aterm_spec::xref::model_registry()
        .into_iter()
        .map(|model| model.name)
        .collect();
    for expected in [
        "SurfaceCoverage",
        "StartupPhasePublication",
        "PresentRetry",
        "GpuLossRoute",
        "GpuLossRecovery",
        "RecoveryRedraw",
        "PredictiveEchoVisibility",
    ] {
        assert!(
            registered.contains(expected),
            "{expected} must resolve through the global spec↔source registry"
        );
    }
}

/// Self-reflection feedback governor (RFC R4 / L2): once the breaker trips, no
/// self-write survives — the storm backstop. PROVES FailClosed at Buggy=0,
/// CATCHES the breaker-bypass at Buggy=1. Bound to `aterm-agent::SelfGovernor`
/// (whose `allow_self_write` returns false once `tripped`).
#[test]
fn derived_self_governor_proves_and_catches_breaker_bypass() {
    assert_proves_and_catches(&self_governor_model());
}

/// Self-feed floor (RFC D3): the un-bypassable control-layer backstop never
/// admits a self-injection past an empty token bucket. PROVES NoOverdraft at
/// Buggy=0, CATCHES the overdraft at Buggy=1. Bound to `aterm-gui::inject_floor`.
#[test]
fn derived_inject_floor_proves_and_catches_overdraft() {
    assert_proves_and_catches(&inject_floor_model());
}

/// No-mint-reachability (ATERM_DESIGN §5.4): an untrusted actor never reaches `Top`
/// (the capability MINT) — the mint is launcher-only. PROVES NoUntrustedTop at
/// Buggy=0, CATCHES the untrusted-reachable mint at Buggy=1. Bound to real code by
/// `aterm-cap/tests/mint_reachability.rs` (the sealed `aterm_cap::Authority` constructor is
/// named in exactly one product location, unreachable from any engine crate).
#[test]
fn derived_mint_reachability_proves_and_catches_untrusted_mint() {
    assert_proves_and_catches(&mint_reachability_model());
}

/// Network capability (RFC D4 / L3): an edge token captured on one connection must
/// not authorize on another. PROVES NoReplay at Buggy=0, CATCHES the
/// channel-unbound bug at Buggy=1. Bound to `aterm-net::channel_bind`/`verify_presented`.
#[test]
fn derived_channel_bind_proves_and_catches_replay() {
    assert_proves_and_catches(&channel_bind_model());
}

/// L3 network drive: the listener's `verify_capability` grants ONLY when the
/// (src, op) is a minted capability AND the channel-binding HMAC verifies. PROVES
/// GrantImpliesKnownAndBound at Buggy=0, CATCHES the dropped-binding (forgery/
/// replay) bug at Buggy=1. Bound to `aterm-net::verify_capability`.
#[test]
fn derived_net_capability_grant_proves_and_catches_dropped_binding() {
    assert_proves_and_catches(&net_capability_grant_model());
}

/// L3 network drive: `accept_and_relay` dials the LOCAL control socket only AFTER
/// the capability is granted, so a denied dialer never reaches it. PROVES
/// DialImpliesGranted at Buggy=0, CATCHES the premature-dial bug at Buggy=1. Bound
/// to `aterm-net::drive::accept_and_relay`.
#[test]
fn derived_net_dial_after_grant_proves_and_catches_premature_dial() {
    assert_proves_and_catches(&net_dial_after_grant_model());
}

/// W2 (linear-corrected weight compensation): the texel-level gate of the
/// perceptual alpha remap — corrected mode only, interior coverage only,
/// non-degenerate luminance gap only. `ty` PROVES `CorrectionGated` over the
/// whole bounded state space (Buggy=0) and CATCHES the unguarded
/// div-by-near-zero variant (Buggy=1 → counterexample). Bound to the shipping
/// `aterm_render::correction_applies`/`blend_text` by
/// `aterm-render/tests/text_blending.rs` (Tier-1, exhaustive domain).
#[test]
fn derived_text_blend_gate_proves_and_catches_degenerate_divide() {
    assert_proves_and_catches(&text_blend_gate_model());
}

/// W6 (per-style fonts): a styled ligature run with a REAL bold face available
/// is never drawn as primary + synthetic dilation. `ty` PROVES
/// `RealBoldNeverDilated` over the whole input square (Buggy=0) and CATCHES
/// the old hard-coded-Primary route (Buggy=1 → counterexample). Bound to the
/// shipping `aterm_render::resolve_styled_face` / `run_face_pick` by
/// `aterm-render/tests/styled_faces.rs` (Tier-1, exhaustive 2^6 + rendered-ink
/// run-routing gates).
#[test]
fn derived_styled_run_face_proves_and_catches_dilated_bold_run() {
    assert_proves_and_catches(&styled_run_face_model());
}

/// W7 (font-metric decorations): the underline pattern phase is a pure
/// function of ABSOLUTE x — a cell seam never resets it. `ty` PROVES
/// `PhasePure` over the whole bounded state space (Buggy=0) and CATCHES the
/// historical per-cell phase restart (Buggy=1 → the seam-reset
/// counterexample). Bound to the shipping `aterm_render::deco` pattern
/// predicates + `underline_rects_into` emission by
/// `aterm-render/tests/deco_lines.rs::pattern_rects_are_partition_invariant`
/// (Tier-1: every partition of a run over a size lattice covers identical
/// pixels, with the old dash law as a failing negative control).
#[test]
fn derived_deco_phase_proves_and_catches_seam_reset() {
    assert_proves_and_catches(&deco_phase_model());
}

/// CROSS-CUTTING THEOREM (c) — decoration band containment (W7). The clamp
/// ORDER (thickness into `[1, cell_h]` first, then top into `[0, cell_h − t]`)
/// keeps every decoration band inside its cell: `ty` PROVES `Contained`
/// (`y + t <= cell_h`) and `ThicknessInCell` at Buggy=0 and CATCHES each at
/// Buggy=1 — the pre-fix order spilling a low thick band past the bottom, and
/// the dropped floor settling a zero-thickness line. Bound to the shipping
/// resolver over the whole lattice by
/// `aterm-render/tests/deco_lines.rs::resolved_bands_conform_to_the_deco_band_containment_model`.
#[test]
fn derived_deco_band_containment_proves_and_catches_spill() {
    let model = deco_band_containment_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &[]);
}

/// W6 (TOML fallback chain): an explicit config font entry strictly outranks
/// built-in discovery. `ty` PROVES `ConfigOutranksDiscovery` (Buggy=0) and
/// CATCHES the inverted precedence (Buggy=1 → counterexample). Bound to the
/// shipping `aterm_render::fallback_chain_order` by
/// `aterm-render/tests/styled_faces.rs` (Tier-1, presence-lattice first-element
/// classes).
#[test]
fn derived_fallback_precedence_proves_and_catches_discovery_over_config() {
    assert_proves_and_catches(&fallback_precedence_model());
}

#[test]
fn derived_presentation_gate_proves_and_catches_text_colored_as_emoji() {
    // The ⏺ (U+23FA) fix, model-checked by the real `ty` over the whole bounded
    // state space: a default-TEXT code point is never resolved to the colour face
    // (Buggy=0 PROVES NoColorForText), and the old coverage-only gate is genuinely
    // caught (Buggy=1 -> counterexample).
    assert_proves_and_catches(&presentation_gate_model());
}

/// M3 phase B (EDR "HDR glow" present gate): over every (config × surface-caps ×
/// aurora-presence) combination and every Attach→Present sequence, `hdr_glow`
/// OFF means NOTHING HDR ever happens — no Rgba16Float swapchain, no linear
/// blit decode, no >1.0 aurora pass (SdrInvariance); a boost only ever lands on
/// a linear-decoded f16 swapchain (BoostNeedsLinearF16); the EDR format is
/// never picked without surface support (F16NeedsSupport). Buggy=1 (Attach
/// picks f16 from capability alone, ignoring the config — HDR-by-default on
/// every capable Mac) is genuinely caught. Bound to the shipping
/// `aterm_gpu::{hdr_swapchain_wants_f16, hdr_present_plan}` by aterm-gpu's
/// `tests/hdr_gate.rs` exhaustive Attach→Present enumeration (Tier-1); the
/// float clamp laws the gate feeds are proven in `aterm_render::hdr`.
#[test]
fn derived_hdr_present_gate_proves_and_catches_hdr_without_optin() {
    assert_proves_and_catches(&hdr_present_gate_model());
}

/// M3 reconfigure lifecycle: an f16 swapchain recreated by resize, a live alpha
/// change, or Outdated/Lost recovery — or checked after a same-size Windows HDR
/// toggle — stays f16 only if its scRGB re-tag/validation succeeds. Failure
/// atomically falls back to SDR and updates capture metadata to match. Buggy=1
/// preserves the old untagged f16 surface/linear-capture claim, so the model
/// must produce a counterexample.
#[test]
fn derived_hdr_reconfigure_retag_proves_and_catches_ignored_failure() {
    let model = hdr_reconfigure_retag_model();
    assert_proves_and_catches(&model);

    let mutants = [
        "BuggyRetagFailureIgnored",
        "BuggyUpgradeFailureKeepsF16",
        "BuggyEscapeKeepsLinearCapture",
    ];
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &mutants),
        Ok(mutants.len()),
        "each retag slip must fire and be caught on its own"
    );
    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    // A failed live re-tag ignored: the surface stays f16 and capture stays
    // linear. Encoding and capture still agree, so only the re-tag law sees it.
    let ignored = buggy.successors("BuggyRetagFailureIgnored", &buggy.init_state())[0].clone();
    assert_eq!(
        (
            ignored["retagged"],
            ignored["is_f16"],
            ignored["capture_linear"]
        ),
        (0, 1, 1)
    );
    assert!(buggy.check_invariant("CaptureMatchesSurfaceEncoding", &ignored));
    assert!(!buggy.check_invariant("ResolvedF16RequiresSuccessfulRetag", &ignored));

    // The HDR-on upgrade whose tag failed, left f16 while capture reads SDR —
    // from the correct SDR escape, so the first law it breaks is its own.
    let sdr = buggy.successors("EnterSdrFallback", &buggy.init_state())[0].clone();
    for invariant in &model.invariants {
        assert!(
            buggy.check_invariant(invariant.name, &sdr),
            "{}",
            invariant.name
        );
    }
    let failed_upgrade = buggy.successors("BuggyUpgradeFailureKeepsF16", &sdr)[0].clone();
    assert_eq!(
        (failed_upgrade["is_f16"], failed_upgrade["capture_linear"]),
        (1, 0)
    );
    assert!(!buggy.check_invariant("CaptureMatchesSurfaceEncoding", &failed_upgrade));

    // The SDR escape whose apply lost its metadata half: SDR format, linear
    // capture. The re-tag law has nothing to say about stage 1.
    let unapplied =
        buggy.successors("BuggyEscapeKeepsLinearCapture", &buggy.init_state())[0].clone();
    assert_eq!(
        (
            unapplied["stage"],
            unapplied["is_f16"],
            unapplied["capture_linear"]
        ),
        (1, 0, 1)
    );
    assert!(buggy.check_invariant("ResolvedF16RequiresSuccessfulRetag", &unapplied));
    assert!(!buggy.check_invariant("CaptureMatchesSurfaceEncoding", &unapplied));

    assert_every_invariant_breaks_first(&model, &["ValuesBounded"]);
}

/// W11 (MotionPolicy — reduced-motion totality): over the whole
/// (mode × system-flag × focus) domain, a Reduced policy has EXACTLY zero
/// animation amplitude, an unfocused window always demotes, and a Full policy
/// animates at unit amplitude (the non-vacuity twin). `ty` PROVES all three
/// (Buggy=0) and CATCHES the pre-W11 defect — the OS Reduce Motion flag was
/// never queried, so auto mode kept animating (Buggy=1 → counterexample).
/// Bound to the shipping resolver by aterm-gui's exhaustive
/// `motion::tests::reduced_motion_totality` (Tier-1, complete over the finite
/// domain × the enumerated `MotionEffect::ALL` set).
#[test]
fn derived_motion_policy_proves_and_catches_ignored_reduce_flag() {
    assert_proves_and_catches(&motion_policy_model());
}

/// Serious mode is an effective-policy overlay: every audible/decorative effect
/// is suppressed while it is active, while requested settings remain mutable
/// underneath and are restored exactly when the overlay is removed.  The buggy
/// twin leaves the cursor trail alive, proving the silence invariant is not
/// vacuous.  Tier-1 tests in aterm-gui bind the same requested/effective
/// projection to the shipping application policy.
#[test]
fn derived_serious_mode_proves_and_catches_effect_leak() {
    assert_proves_and_catches(&serious_mode_model());
}

/// Emacs-style search navigation is a host-owned state machine: Cmd-S/Cmd-R
/// never reach the PTY, each repeat advances exactly one precomputed ordinal
/// with wraparound, streaming output deselects the stale hit, cancel restores
/// the captured viewport, and accept retains the selected match. Each of those
/// laws is caught by its own `Buggy = 1` member. (The model states no work
/// bound for a repeat: nothing in the shipping code counts that work, so no
/// bind could fail on it — see the model's doc.)
#[test]
fn derived_emacs_search_navigation_proves_and_catches_each_regression() {
    let model = emacs_search_navigation_model();
    assert_proves_and_catches(&model);
    // Each design law catches a member of its own; only the space guards do not.
    assert_every_invariant_carries_a_mutant(
        &model,
        &["HitCountBounded", "DirectionBounded", "DirtyBounded"],
    );
}

/// M1/W11 (smooth-scroll convergence + accessibility settlement): a Full-policy
/// wheel glide makes strict bounded progress and disarms exactly at its target;
/// a Full→Reduced edge lands there and disarms AT ONCE, so Reduced owns no glide
/// deadline. `ty` proves `BoundedWakes`, `DisarmedAtTarget`, and
/// `ReducedSettled` (Buggy=0), and catches the audited mutant that keeps the
/// intermediate row + armed deadline across `SetReduced` (Buggy=1).
/// Bound to the shipping `scroll_motion::Glide` and App settle reducer by
/// aterm-gui's convergence lattice tests plus
/// `reduced_motion_settle_conforms_to_scroll_glide_model` (Tier-1).
#[test]
fn derived_scroll_glide_proves_and_catches_unsettled_reduced_edge() {
    assert_proves_and_catches(&scroll_glide_model());
}

/// M1b (sub-row scroll translate chrome exemption): the render-side translate
/// shifts a frame row by the fractional-pixel residual IFF the row is in the
/// terminal-content grid band `[GridTop, GridBot)` — chrome (tab strip, edge bars,
/// split dividers) stays pinned. `ty` PROVES `ShiftOnlyInBand` (Buggy=0) and
/// CATCHES the band-leak mutant that shifts the first bottom-chrome row
/// (`row == GridBot`; Buggy=1 → counterexample). Bound to the shipping
/// `scroll_translate::translate_grid_band_in_place` by aterm-render's
/// exhaustive `chrome_pixels_are_invariant` lattice test (Tier-1) and the
/// real-renderer `scroll_frac_translate.rs` chrome-invariance test.
#[test]
fn derived_grid_translate_proves_and_catches_chrome_band_leak() {
    assert_proves_and_catches(&grid_translate_model());
}

/// M2 ("ink that dries" bypass soundness): the stream-fade gate permits fading
/// ONLY with the config on and every bypass clear — a keystroke echo in flight
/// (`input_hot`), the alternate screen, a scrolled-back viewport, and a W11
/// Reduced motion policy each force the INSTANT path (exact bytes), and the
/// non-vacuity twin pins that an all-clear frame genuinely fades. `ty` PROVES
/// all six invariants (Buggy=0) and CATCHES the fading-keystroke-echo mutant
/// (the gate ignoring `input_hot`; Buggy=1 → counterexample). Bound to the
/// shipping `stream_fade::fade_permitted` by aterm-gui's exhaustive 2^5
/// `fade_gate_exhaustive` (Tier-1, complete over the finite boolean domain)
/// plus the byte-identity pipeline test `bypass_is_byte_identical`.
#[test]
fn derived_stream_fade_gate_proves_and_catches_fading_keystroke_echo() {
    assert_proves_and_catches(&stream_fade_gate_model());
}

/// W5b (minimum-contrast floor): the floor's delivery bound —
/// `contrast(result, bg) >= min(requested, max_achievable(bg))` — over the
/// abstract contrast lattice. `ty` PROVES `FloorDelivers` (Buggy=0) and
/// CATCHES the old luminance-midpoint fallback-pole rule, which chases the
/// WEAKER pole on mid-luminance backgrounds (Buggy=1 → counterexample).
/// Bound to the shipping `aterm_render::floor_fg_contrast` by
/// `aterm-render/tests/contrast_floor.rs` (Tier-1, grayscale-exhaustive +
/// RGB-lattice against an independent WCAG oracle).
#[test]
fn derived_contrast_floor_proves_and_catches_weak_pole() {
    assert_proves_and_catches(&contrast_floor_model());
}

/// M5 (true vibrancy legibility guarantee): engaging translucent glass
/// (`background_opacity < 1.0`) auto-raises the effective per-cell contrast
/// floor to WCAG AA — `translucent ⇒ effective >= Floor`. `ty` PROVES
/// `NeverIllegible` over the whole opacity × configured-contrast lattice
/// (Buggy=0) and CATCHES the dropped auto-floor that would let text sink into
/// the desktop through the glass (Buggy=1 → counterexample). Bound to the
/// shipping `Config::effective_minimum_contrast` by `aterm-gui`'s exhaustive
/// `vibrancy_contrast_guarantee` Tier-1 lattice test.
#[test]
fn derived_vibrancy_contrast_proves_and_catches_dropped_floor() {
    assert_proves_and_catches(&vibrancy_contrast_model());
}

/// W1 (kill the compositor stretch): the window-fit + padding-absorption law —
/// `pad_lo + cols*cell + pad_hi == w` EXACTLY (so the surface is the raw window
/// and the compositor never rescales), pads keep the configured floor, the grid
/// is maximal, and an odd remainder splits near-evenly. `ty` PROVES all four
/// invariants over the whole bounded lattice (Buggy=0) and CATCHES the lopsided
/// all-remainder-on-one-edge split (Buggy=1 → NearEvenSplit counterexample).
/// Bound to the shipping `aterm_render::pad_split` by
/// `aterm-render/tests/pad_absorption.rs` (Tier-1 model↔code conformance).
#[test]
fn derived_pad_absorption_proves_and_catches_lopsided_split() {
    assert_proves_and_catches(&pad_absorption_model());
}

/// In the RAW renderer transport, a top-only inset redistributes padding and a
/// layout-origin change invalidates dimension-identical CPU/GPU cache entries.
/// (`VisiblePadCrop` below proves the separately exposed GUI frame.) The traces
/// pin raw exact cover, bounds, and the grid-top cache key.
#[test]
fn derived_asymmetric_pad_layout_proves_cover_bounds_and_cache_invalidation() {
    let model = asymmetric_pad_layout_model();
    assert_proves_and_catches(&model);

    let picked = model
        .successors("PickLayout", &model.init_state())
        .into_iter()
        .find(|state| {
            state["pad"] == 2
                && state["head"] == 1
                && state["initial_request"] == 2
                && state["changed_request"] == 0
        })
        .expect("bounded layout fixture");
    let initial = model.successors("ApplyInitialTop", &picked)[0].clone();
    let cached = model.successors("PrimeLayoutCache", &initial)[0].clone();
    let changed = model.successors("ApplyChangedTop", &cached)[0].clone();
    assert_eq!(changed["pad_top"], 0);
    assert_eq!(changed["pad_bottom"], 4);
    assert_eq!(changed["grid_top"], 1);
    assert!(model.check_invariant("ExactVerticalPadCover", &changed));
    assert!(model.check_invariant("TopPadIsBounded", &changed));
    assert!(model.check_invariant("GridOriginTracksTopAndHead", &changed));
    let repainted = model.successors("RenderWithLayoutCache", &changed)[0].clone();
    assert_eq!(repainted["cache_hit"], 0);
    assert_eq!(repainted["full_repaint"], 1);
    assert!(model.check_invariant("LayoutChangeForcesFullRepaint", &repainted));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let buggy_picked = buggy
        .successors("PickLayout", &buggy.init_state())
        .into_iter()
        .find(|state| {
            state["pad"] == 2
                && state["head"] == 1
                && state["initial_request"] == 2
                && state["changed_request"] == 0
                && state["gate_fault"] == 0
                && state["top_fault"] == 0
        })
        .expect("bounded buggy cache fixture");
    let buggy_initial = buggy.successors("ApplyInitialTop", &buggy_picked)[0].clone();
    let buggy_cached = buggy.successors("PrimeLayoutCache", &buggy_initial)[0].clone();
    let buggy_changed = buggy.successors("ApplyChangedTop", &buggy_cached)[0].clone();
    let stale = buggy.successors("RenderWithLayoutCache", &buggy_changed)[0].clone();
    assert_eq!(stale["cache_hit"], 1);
    assert_eq!(stale["full_repaint"], 0);
    assert!(!buggy.check_invariant("LayoutChangeForcesFullRepaint", &stale));

    let pick = |pad: i64, initial: i64, changed: i64, gate_fault: i64, top_fault: i64| {
        buggy
            .successors("PickLayout", &buggy.init_state())
            .into_iter()
            .find(|state| {
                state["pad"] == pad
                    && state["head"] == 1
                    && state["initial_request"] == initial
                    && state["changed_request"] == changed
                    && state["gate_fault"] == gate_fault
                    && state["top_fault"] == top_fault
            })
            .expect("bounded buggy layout fixture")
    };

    // The unclamped initial top overshoots its pad and breaks the cover.
    let unbounded = buggy.successors("ApplyInitialTop", &pick(2, 4, 4, 0, 0))[0].clone();
    assert!(!buggy.check_invariant("TopPadIsBounded", &unbounded));
    assert!(!buggy.check_invariant("ExactVerticalPadCover", &unbounded));

    // The getter-only clamp: `pad_top()` reports 2, the origin sits at 1 + 4.
    let mut getter_clamp = pick(2, 2, 4, 0, 0);
    for action in ["ApplyInitialTop", "PrimeLayoutCache", "ApplyChangedTop"] {
        assert!(buggy.fire(action, &mut getter_clamp), "{action}");
    }
    assert_eq!((getter_clamp["pad_top"], getter_clamp["grid_top"]), (2, 5));
    assert!(!buggy.check_invariant("GridOriginTracksTopAndHead", &getter_clamp));

    // The runtime path's own non-absorbing slip: a tightened changed top
    // leaves the bottom at the old symmetric pad, so the frame is short of
    // its cover, and an over-pad one is taken unclamped.
    let mut non_absorbing = pick(2, 2, 0, 0, 1);
    for action in ["ApplyInitialTop", "PrimeLayoutCache", "ApplyChangedTop"] {
        assert!(buggy.fire(action, &mut non_absorbing), "{action}");
    }
    assert_eq!(
        (non_absorbing["pad_top"], non_absorbing["pad_bottom"]),
        (0, 2)
    );
    assert!(!buggy.check_invariant("ExactVerticalPadCover", &non_absorbing));
    assert!(buggy.check_invariant("GridOriginTracksTopAndHead", &non_absorbing));
    let mut over_pad = pick(2, 2, 4, 0, 1);
    for action in ["ApplyInitialTop", "PrimeLayoutCache", "ApplyChangedTop"] {
        assert!(buggy.fire(action, &mut over_pad), "{action}");
    }
    assert!(!buggy.check_invariant("TopPadIsBounded", &over_pad));

    // The always-false `d8a744d24` gate: a re-request landing on the same
    // origin still repaints in full.
    assert!(
        model
            .successors("PickLayout", &model.init_state())
            .iter()
            .all(|state| state["gate_fault"] == 0 && state["top_fault"] == 0),
        "no gate or top fault exists to choose at Buggy=0"
    );
    let mut unmoved = pick(2, 1, 1, 1, 0);
    for action in [
        "ApplyInitialTop",
        "PrimeLayoutCache",
        "ApplyChangedTop",
        "RenderWithLayoutCache",
    ] {
        assert!(buggy.fire(action, &mut unmoved), "{action}");
    }
    assert_eq!(unmoved["cached_grid_top"], unmoved["grid_top"]);
    assert_eq!((unmoved["cache_hit"], unmoved["full_repaint"]), (0, 1));
    assert!(!buggy.check_invariant("IdenticalLayoutMayReuseCache", &unmoved));
}

/// The GUI crops the raw renderer transport by exactly the removed top delta:
/// visible top stays requested/clamped, visible bottom stays the BASE pad, and
/// visible height uses those independent edges. The buggy regime exposes the
/// old raw bottom/height and is therefore caught non-vacuously.
#[test]
fn derived_visible_pad_crop_proves_base_bottom_and_catches_raw_exposure() {
    let model = visible_pad_crop_model();
    assert_proves_and_catches(&model);

    let picked = model
        .successors("ChooseGeometry", &model.init_state())
        .into_iter()
        .find(|state| {
            state["pad"] == 3 && state["request"] == 1 && state["grid"] == 4 && state["head"] == 2
        })
        .expect("bounded visible-crop fixture");
    let cropped = model.successors("Crop", &picked)[0].clone();
    assert_eq!(cropped["pad_top"], 1);
    assert_eq!(cropped["raw_pad_bottom"], 5);
    assert_eq!(cropped["visible_pad_bottom"], 3);
    assert_eq!(cropped["raw_height"], 12);
    assert_eq!(cropped["visible_height"], 10);
    assert_eq!(cropped["crop_total"], 2);
    for invariant in [
        "VisibleTopMatchesRendererTop",
        "VisibleBottomIsBasePad",
        "VisibleHeightUsesIndependentEdges",
        "CropDeletesOnlyRemovedTop",
    ] {
        assert!(model.check_invariant(invariant, &cropped), "{invariant}");
    }

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let buggy_picked = buggy
        .successors("ChooseGeometry", &buggy.init_state())
        .into_iter()
        .find(|state| {
            state["pad"] == 3 && state["request"] == 1 && state["grid"] == 4 && state["head"] == 2
        })
        .expect("bounded buggy visible-crop fixture");
    let exposed_raw = buggy.successors("Crop", &buggy_picked)[0].clone();
    assert_eq!(exposed_raw["visible_pad_bottom"], 5);
    assert_eq!(exposed_raw["visible_height"], 12);
    assert!(!buggy.check_invariant("VisibleBottomIsBasePad", &exposed_raw));
    assert!(!buggy.check_invariant("VisibleHeightUsesIndependentEdges", &exposed_raw));

    // An over-pad request: the renderer clamps its top, the unclamped visible
    // authority keeps the request, and the two edges disagree.
    let over = buggy
        .successors("ChooseGeometry", &buggy.init_state())
        .into_iter()
        .find(|state| state["pad"] == 2 && state["request"] == 4)
        .expect("bounded over-pad request fixture");
    let split = buggy.successors("Crop", &over)[0].clone();
    assert_eq!((split["pad_top"], split["visible_pad_top"]), (2, 4));
    assert!(!buggy.check_invariant("VisibleTopMatchesRendererTop", &split));
}

/// W12 (mixed-DPI, glyph-key injectivity): px is part of every `GlyphKey` by
/// construction, so a glyph rasterized at one size never collides in the shared
/// cache with the SAME glyph at another size. `ty` PROVES `NoCollision` (Buggy=0:
/// two keys differing only in px map to distinct addresses) and CATCHES the defect
/// (Buggy=1: a key that drops px aliases the two sizes → counterexample). Bound to
/// the shipping `aterm_render::GlyphKey` `Eq`/`Hash` by
/// `aterm-render/tests/glyph_key_injectivity.rs` (Tier-1: real keys at two sizes).
#[test]
fn derived_key_injectivity_proves_and_catches_size_collision() {
    assert_proves_and_catches(&key_injectivity_model());
}

/// W12 (mixed-DPI, per-window metric consistency): every draw of a window uses
/// metrics derived from THAT window's own scale factor, never a shared
/// most-recently-scaled global. `ty` PROVES `PerWindowConsistent` (Buggy=0: a drawn
/// window always rendered at its own scale, over every interleaving of scale-changes
/// and draws) and CATCHES the SHARED-BACKEND defect (Buggy=1: scaling window 2 then
/// redrawing window 1 renders it at the other window's DPI → counterexample). Bound
/// to the shipping per-window `MetricsView` derivation by
/// `aterm-gui`'s `metrics_view` unit tests (`font_px_for_scale` / `pad_for_scale`).
#[test]
fn derived_per_window_metrics_proves_and_catches_shared_backend_clobber() {
    assert_proves_and_catches(&per_window_metrics_model());
}

/// W8 (fallback harmony, normalization clamp): the fallback-face raster scale
/// never leaves the clamp interval AND passes an in-interval ratio through
/// exactly. `ty` PROVES `ScaleInBounds` + `ScaleExactInRange` (Buggy=0) and
/// CATCHES the unclamped raw ratio (Buggy=1 → counterexample). Bound to the
/// shipping `aterm_render::fallback_cjk_scale` / `fallback_xheight_scale` by
/// `aterm-render/tests/fallback_harmony.rs` (Tier-1, dense f32 lattice +
/// degenerate inputs).
#[test]
fn derived_fallback_scale_clamp_proves_and_catches_unclamped_ratio() {
    assert_proves_and_catches(&fallback_scale_clamp_model());
}

/// W8 (fallback harmony, wide centring): any floor-characterized offset
/// (`2*off <= gap <= 2*off + 1`) balances the two margins to within 1px.
/// `ty` PROVES `MarginsBalance` (Buggy=0) and CATCHES the pre-W8 left-bias
/// (Buggy=1: `off = 0` ships for any gap → counterexample). Bound to the
/// shipping `aterm_render::wide_center_offset` by
/// `aterm-render/tests/fallback_harmony.rs` (Tier-1: the real fn satisfies
/// the characterization exhaustively).
#[test]
fn derived_wide_center_proves_and_catches_left_bias() {
    assert_proves_and_catches(&wide_center_model());
}

/// W8 (fallback harmony, row-band clip): the raster-time trim only ever drops
/// rows and every kept row lies inside the cell row band. `ty` PROVES
/// `KeptRowsInBand` (Buggy=0) and CATCHES the pre-W8 unclipped fallback blit
/// (Buggy=1 → an ascender-overshoot counterexample). Bound to the shipping
/// `aterm_render::clamp_to_row_band` by
/// `aterm-render/tests/fallback_harmony.rs` (Tier-1, exhaustive lattice).
#[test]
fn derived_fallback_band_clip_proves_and_catches_unclipped_blit() {
    assert_proves_and_catches(&fallback_band_clip_model());
}

/// W9 (variable-font instantiation, axis clamp): every resolved variation
/// coordinate stays inside its `fvar` axis bounds AND an in-bounds request
/// resolves exactly. `ty` PROVES `CoordInBounds` + `CoordExactInRange`
/// (Buggy=0) and CATCHES the pre-W9 no-instantiation pass-through (Buggy=1:
/// an off-axis request escapes → counterexample). Bound to the shipping
/// `aterm_render::variation::clamp_axis` by
/// `aterm-render/tests/variation_instantiation.rs` (Tier-1: exhaustive
/// bounds lattice incl. NaN/±∞ totality, plus the SF Mono acceptance and
/// the live-renderer coord-consistency bindings).
#[test]
fn derived_vf_axis_clamp_proves_and_catches_unclamped_coord() {
    assert_proves_and_catches(&vf_axis_clamp_model());
}

/// W9 (dark-theme weight nudge, safety gate): the nudge applies ONLY under
/// advance invariance — `|adv_nudged − adv_default| <= 0.25px` (1 quarter-px
/// in the model). `ty` PROVES `NudgeOnlyWhenInvariant` (Buggy=0) and CATCHES
/// the unconditional nudge (Buggy=1 → a 1.5px-drift counterexample). Bound
/// to the shipping `aterm_render::variation::dark_nudge_permitted` by
/// `aterm-render/tests/variation_instantiation.rs` (Tier-1: exhaustive
/// advance lattice incl. NaN/∞ failed measurements, plus the SF Mono
/// end-to-end geometry-stability binding).
#[test]
fn derived_vf_nudge_gate_proves_and_catches_ungated_nudge() {
    assert_proves_and_catches(&vf_nudge_gate_model());
}

/// W10 (emoji strike selection): the chosen bitmap strike is the SMALLEST
/// adequate (`>= target`) strike among those carrying the glyph when one
/// exists, else the largest carrying strike. `ty` PROVES the three invariants
/// (`ChosenFromAvailable`, `ChosenAdequateMinimal`, `ChosenMaxWhenNoneAdequate`)
/// at Buggy=0 and CATCHES the pre-W10 always-largest (`u16::MAX`) request
/// (Buggy=1: strikes {1,2}, target 1 → old picks 2, the law demands 1 →
/// counterexample). Bound to the shipping `aterm_render::select_strike_ppem` /
/// `pick_glyph_raster` by `aterm-render/tests/emoji_resample.rs` (Tier-1:
/// exhaustive strike lattice) and the real-face per-glyph dead-zone sweep in
/// aterm-render's in-module tests.
#[test]
fn derived_strike_selection_proves_and_catches_largest_strike_bias() {
    assert_proves_and_catches(&strike_selection_model());
}

/// W3 (fractional-bearing CoreText rasters): the sub-pixel placement law —
/// integer bearing (floor) + RETAINED in-bitmap phase reconstructs the designed
/// glyph position EXACTLY (`Decompose`), and the reported phase stays in
/// `[0, 1)` (`PhaseInUnit`). `ty` PROVES both over the whole bounded
/// eighth-px lattice (Buggy=0) and CATCHES the pre-fix round-and-pin placement
/// — bearing rounded to nearest, phase discarded, every glyph up to 0.5px off —
/// at Buggy=1 (counterexample on Decompose). Bound to the shipping
/// `aterm_render::ct_pen_and_bearing` / `CtFont::rasterize` by
/// `aterm-render/tests/ct_fractional_bearing.rs` (Tier-1 conformance).
#[test]
fn derived_ct_frac_bearing_proves_and_catches_rounded_pin() {
    assert_proves_and_catches(&ct_frac_bearing_model());
}

/// W4 (cursor ink integrity): the block-cursor cut-out clip law — the visible
/// cut-out slice is the glyph∩window intersection, ordered inside the glyph
/// (`SlicesOrdered`, so the fg remainders tile around it) and NEVER exiting the
/// cursor rect (`CutoutInsideWindow` — partition/no-bleed). `ty` PROVES both
/// over every extent × window on the bounded lattice (Buggy=0) and CATCHES the
/// pre-W4 unclipped cut-out — the whole glyph repainted in bg, bleeding over a
/// ligature's lead cells / a wide glyph's right half — at Buggy=1
/// (counterexample). Bound to the shipping `aterm_render::clip_span` /
/// `glyph_quad` x-clip / `draw_cursor` by `aterm-render/tests/cursor_ink.rs`
/// (Tier-1: exhaustive lattice + the pixel-level complement sweep).
#[test]
fn derived_cursor_cutout_clip_proves_and_catches_unclipped_bleed() {
    assert_proves_and_catches(&cursor_cutout_clip_model());
}

#[test]
fn derived_aa_edge_hardening_proves_and_catches_soft_seam() {
    // The procedural-AA seam-tiling law, model-checked by the real `ty`: an
    // anti-aliased glyph's CELL-EDGE texel is always hard 0/MAX after the
    // border-hardening pass (Buggy=0 PROVES EdgeTexelsHard over every raw
    // supersample value), and skipping the pass — a fractional half-covered
    // seam line — is genuinely caught (Buggy=1 -> counterexample). Tier-1 is
    // aterm-render's procedural_aa_edges exhaustive size-lattice test.
    assert_proves_and_catches(&aa_edge_hardening_model());
}

#[test]
fn derived_shade_phase_proves_and_catches_doubled_seam_line() {
    // The shade-dither uniform-period law, model-checked by the real `ty`:
    // the ░ pattern is the ABSOLUTE-column-parity function across cells of
    // width 9 (Buggy=0 PROVES UniformPeriod, so a doubled line at a seam is
    // impossible), and cell-LOCAL parity — the audited odd-width banding —
    // is genuinely caught at the first seam (Buggy=1 -> counterexample).
    // Tier-1 is aterm-render's shade_phase composed-cell + rendered-frame
    // tests.
    assert_proves_and_catches(&shade_phase_model());
}

#[test]
fn derived_chrome_face_gate_proves_and_catches_dejavu_hardcode() {
    // The chrome-typography fix, model-checked by the real `ty` over the whole
    // bounded state space: the embedded DejaVu is chosen ONLY as a coverage
    // fallback and a covered bold run keeps its weight (Buggy=0 PROVES
    // EmbeddedOnlyAsCoverageFallback + BoldHonoredWhenCovered), and the old
    // hardcoded-DejaVu chrome is genuinely caught (Buggy=1 -> counterexample).
    // Tier-1 binding: aterm-gui tray_raster's exhaustive 2^3 enumeration of the
    // shipping `select_chrome_face`.
    assert_proves_and_catches(&chrome_face_gate_model());
}

#[test]
fn derived_transact_proves_and_catches_lost_update() {
    assert_proves_and_catches(&transact_model());
}

#[test]
fn derived_kernel_proves_and_catches_gap() {
    assert_proves_and_catches(&kernel_model());
}

#[test]
fn derived_snapshot_proves_and_catches_leak() {
    assert_proves_and_catches(&snapshot_model());
}

/// SPAWN LOCALE: `ty` proves the child always ends up with a UTF-8 `LC_CTYPE`
/// (`ChildHasUtf8Ctype`, Buggy=0) and catches the shipped all-unset guard that left a
/// present-but-non-UTF-8 inherited locale unfixed (Buggy=1 → counterexample) — the
/// formal twin of the emacs box-drawing-`?` fix in `aterm_pty::resolve_spawn_locale`.
#[test]
fn derived_spawn_locale_proves_and_catches_non_utf8_child() {
    assert_proves_and_catches(&spawn_locale_model());
}

/// COALESCE: `ty` proves the bulk and single-char write lanes never diverge over
/// the same event stream (the screen is a pure function of the byte log), and
/// catches the bulk-lane skipped-fixup regression (the wide-char-wrap-tail and
/// ZWJ-join class fixed in aterm-grid/aterm-core). This is the model the engine
/// lacked when those two bugs shipped.
#[test]
fn derived_coalesce_proves_and_catches_lane_divergence() {
    assert_proves_and_catches(&coalesce_model());
}

// --- Property-combinator suite (the introspection control-plane models) ---
//
// The introspection models (M1 dispatch, M2 relay, S1 registry, the forward-handshake
// liveness twin, and the F1 info-flow / ordering class models) are `derive::props`
// combinator INSTANCES; the reply-fidelity class model is a hand-written `ty_model!`
// since it gained `DialFail`. All of them are driven by ONE umbrella test over the
// shared instance table. Adding a verified property is a generator instance (~3
// lines) + one row in `harness::instances()` — no new test fn.
#[path = "common/harness.rs"]
mod harness;

/// LIVENESS / deadlock-freedom: deadlock-free at `Buggy = 0` (the served
/// terminal stutters via the `Done` self-loop) and a DEADLOCK — not an
/// invariant violation — at `Buggy = 1` (the all-parties-parked wedge). The
/// liveness twin of [`assert_proves_and_catches`]. TIERED: the interpreter's
/// no-successor wedge search always runs; `ty`'s `CHECK_DEADLOCK TRUE` (via
/// `to_cfg_deadlock_with`) additionally re-proves it wherever installed. This
/// is the mechanism that closes the documented gap: it catches the
/// blocking-call class (the `drain_buffered` `fill_buf` hang) that no
/// reachable-bad-STATE safety invariant can see.
fn assert_deadlock_free_and_catches_wedge(
    m: &Model,
    is_final: fn(&aterm_spec::interp::State) -> bool,
) {
    verify::deadlock_free_and_catches_tiered(m, is_final, m.name);
}

/// THE UMBRELLA: every property-combinator instance PROVES (Buggy=0) + CATCHES
/// (Buggy=1) — a `Safety` invariant via [`assert_proves_and_catches`], a
/// `Liveness` instance via [`assert_deadlock_free_and_catches_wedge`] — on both
/// tiers. The introspection/control-plane models are iterated from the ONE shared
/// table; a new property adds a row there, not a test fn here.
#[test]
fn property_classes_prove_and_catch_under_ty() {
    for inst in harness::instances() {
        match inst.class {
            harness::Class::Safety => assert_proves_and_catches(&inst.model),
            harness::Class::Liveness { is_final } => {
                assert_deadlock_free_and_catches_wedge(&inst.model, is_final);
            }
        }
    }
}

#[test]
fn derived_tier_residency_proves_and_catches_silent_loss() {
    // HIERARCHICAL_SESSIONS.md Addendum B, B.8.2 (GREEN-ORDER step 3): the
    // spill-not-forget property of the hydratable temporal buffer. `ty` PROVES
    // NoSilentLoss at Buggy=0 (every evicted seq stays resident in warm/cold over
    // the whole bounded state space) and CATCHES the silent loss at Buggy=1 (Push
    // drops on evict without spilling) -> counterexample. The proof must hold
    // BEFORE the spill hook ships.
    assert_proves_and_catches(&tier_residency_model());
}

#[test]
fn derived_recording_proves_and_catches_dropped_event() {
    // HIERARCHICAL_SESSIONS.md Addendum B, B.8.3 (GREEN-ORDER step 5): the
    // hydration-faithfulness centerpiece — replaying from a keyframe reproduces
    // the live engine state, P(replay@t) = P(live@t), as a parallel-fold
    // refinement (NOT a counter tautology). `ty` PROVES ReplayFaithful at Buggy=0
    // (keyframe-seed + forward replay = the live parity fold over the whole
    // bounded space) and CATCHES the silent drop at Buggy=1 (a ReplayStep skips a
    // payload, so the replay parity diverges from live) -> counterexample. Only
    // authorable after the B.4.2 Clock seam made time an explicit recorded input.
    assert_proves_and_catches(&recording_model());
}

#[test]
fn derived_read_image_seq_proves_and_catches_torn_and_run_ahead_stamps() {
    // REARCH A-3: the read_image snapshot-seq protocol — monotone seq,
    // snapshot internal-consistency (no torn read), staleness-detectable.
    // `ty` PROVES NoTornRead + SeqIsStaleOrCurrent at Buggy=0 and reports ONE
    // Buggy=1 counterexample — the shallowest, which is the run-ahead stamp. The
    // interpreter isolates each law: every invariant, checked alone, has its own
    // mutant, and each mutant is pinned by its own trace below.
    let model = read_image_seq_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &[]);

    // The torn read: a Write after the capture leaks into the active snapshot.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let snapped = buggy.successors("ReadImage", &buggy.init_state())[0].clone();
    let torn = buggy.successors("Write", &snapped)[0].clone();
    assert_eq!(torn["torn"], 1);
    assert!(!buggy.check_invariant("NoTornRead", &torn));
    let held = model.successors("ReadImage", &model.init_state())[0].clone();
    let clean = model.successors("Write", &held)[0].clone();
    assert!(model.check_invariant("NoTornRead", &clean));

    // The run-ahead stamp: one ahead of the live epoch, it hides staleness
    // before any write happens at all.
    let ahead = snapped;
    assert_eq!((ahead["epoch"], ahead["snap_seq"]), (0, 1));
    assert!(!buggy.check_invariant("SeqIsStaleOrCurrent", &ahead));
    assert!(buggy.check_invariant("NoTornRead", &ahead));
}

#[test]
fn derived_window_routing_proves_and_catches_each_seam_slip() {
    // In-process multi-window routing (GUI multi-window work): `ty` PROVES
    // ExitIffEmpty + FrontmostLive + FrontmostAllocated at Buggy=0 (closing the
    // last window exits the app; the frontmost is null iff there are no windows
    // and is never a future/reused id) and reports ONE Buggy=1 counterexample,
    // the shallowest. The interpreter isolates each law: every invariant,
    // checked alone, has its own mutant, and each is pinned by a trace below.
    let model = window_routing_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &[]);

    // The last close of the mutant misses the exit AND the frontmost re-point:
    // win_count=0 with exited=0 (ExitIffEmpty), and the frontmost still naming
    // the dead window (FrontmostLive). The committed close does neither.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let dangling = buggy.successors("CloseWindow", &buggy.init_state());
    assert_eq!(dangling.len(), 1);
    assert_eq!(
        (
            dangling[0]["win_count"],
            dangling[0]["exited"],
            dangling[0]["frontmost"]
        ),
        (0, 0, 1)
    );
    assert!(!buggy.check_invariant("ExitIffEmpty", &dangling[0]));
    assert!(!buggy.check_invariant("FrontmostLive", &dangling[0]));
    let closed = model.successors("CloseWindow", &model.init_state());
    assert_eq!(closed.len(), 1);
    assert_eq!((closed[0]["exited"], closed[0]["frontmost"]), (1, 0));
    // A create that reads the allocator after bumping it hands the frontmost
    // the id the next create will mint again.
    let reused = buggy.successors("CreateWindow", &buggy.init_state())[0].clone();
    assert_eq!((reused["frontmost"], reused["next_id"]), (3, 3));
    assert!(!buggy.check_invariant("FrontmostAllocated", &reused));
}

#[test]
fn derived_tab_nav_proves_and_catches_out_of_range_active() {
    // The GUI per-window tab-strip index machine (`TabIndex` in aterm-gui): `ty`
    // PROVES CountPositive + ActiveInRange at Buggy=0 — a window always keeps >= 1
    // tab and the active index never leaves the renderer's range under ANY
    // interleaving of NewTab / SelectTab / Cycle / Close over the whole bounded
    // (Cap=4) space — and CATCHES the out-of-range active at Buggy=1 (a Close that
    // forgets to re-clamp `active` after the count shrinks, so closing the last
    // active tab leaves `active = count` past the new end) -> counterexample on
    // ActiveInRange. This holds the new tab feature to the same Trust bar as the
    // engine: the renderer never indexes a tab that no longer exists.
    assert_proves_and_catches(&tab_nav_model());
}

#[test]
fn derived_pane_tree_proves_and_catches_dangling_focus() {
    // The GUI in-tab split-pane tree (`PaneTree` in aterm-gui): `ty` PROVES
    // FocusInRange at Buggy=0 — the focused leaf index never leaves the renderer's
    // `0..leaf_count-1` range under ANY interleaving of Split (Cmd-D/Cmd-Shift-D) /
    // Close (Cmd-W) over the whole bounded (Cap=4) space — and CATCHES the dangling
    // focus at Buggy=1 (a Close that forgets to re-point `focused` to a surviving
    // sibling after the leaf count shrinks, so closing the focused last leaf leaves
    // `focused = leaf_count` past the new end) -> counterexample on FocusInRange. This
    // holds the split-pane feature to the same Trust bar as tabs: input + the solid
    // cursor never route to a pane that no longer exists.
    let model = pane_tree_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &[]);

    // The dangling focus, pinned: a two-leaf close that keeps `focused = 1` past the
    // shrunk end.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let split = buggy.successors("Split", &buggy.init_state())[0].clone();
    let dangling = buggy
        .successors("Close", &split)
        .into_iter()
        .find(|s| s["focused"] == 1)
        .expect("the mutant admits the forgotten re-point");
    assert!(!buggy.check_invariant("FocusInRange", &dangling));
    // The sole leaf is `LastPane`, the tab machine's: Close never sees it, whatever
    // Buggy says.
    assert!(!model.action_enabled("Close", &model.init_state()));
    assert!(!buggy.action_enabled("Close", &buggy.init_state()));
}

#[test]
fn derived_session_pool_proves_and_catches_premature_close() {
    // The GUI session pool refcount accounting (`SessionPool` in aterm-gui): `ty`
    // PROVES ClosedIffEmpty at Buggy=0 — a pooled session's entry is retired exactly
    // when (and only when) its last window viewer detaches, so the Cmd-Shift-O
    // two-windows-one-session path (refcount 2) never retires early and a fully
    // detached session never leaks an entry — and CATCHES the premature retire at
    // Buggy=1 (a Release that retires on EVERY detach, closing while a co-viewer
    // remains) -> counterexample on ClosedIffEmpty.
    assert_proves_and_catches(&session_pool_model());
}

#[test]
fn derived_tab_strip_proves_and_catches_strip_desync() {
    // The native macOS titlebar tab strip (the NSSegmentedControl in aterm-gui's
    // toolbar.rs): `ty` PROVES StripMirrorsTruth at Buggy=0 — the strip's segment
    // count always equals the tab count, its selection always equals the active tab,
    // and the selection stays a valid (in-range) segment index, under ANY interleaving
    // of NewTab / SelectTab / Close over the whole bounded (Cap=4) space — and CATCHES
    // the desync at Buggy=1 (a Close that forgets to re-sync the strip — a missed
    // refresh_window_tabs on a non-front-window close — leaving BOTH seg_count and
    // selected stale, so the strip shows an extra segment with an out-of-range
    // selection) -> counterexample on StripMirrorsTruth. This is the two-lane parity
    // discipline the GUI tab-strip sync must preserve so the native chrome never shows
    // a phantom tab or highlights a segment past the end.
    assert_proves_and_catches(&tab_strip_model());
}

/// Stable tab and view IDs survive reorder as identities, are never reused,
/// and every focus/leaf reference remains live after close. The mutant leaves
/// focus dangling on the retired tab or explicitly reuses its burned IDs.
#[test]
fn derived_native_tab_identity_proves_and_catches_reuse_or_dangling_focus() {
    let model = native_tab_identity_model();
    assert_proves_and_catches(&model);

    // Keep retired-ID reuse independently non-vacuous: close an INACTIVE tab so
    // the dangling-focus mutant does not mask the later allocation defect.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let opened = buggy.successors("OpenTab", &buggy.init_state())[0].clone();
    let closed_inactive = buggy.successors("CloseFirst", &opened)[0].clone();
    let reused = buggy.successors("OpenTab", &closed_inactive)[0].clone();
    assert!(!buggy.check_invariant("TabIdsNeverReused", &reused));
    assert!(!buggy.check_invariant("ViewIdsNeverReused", &reused));
}

/// The bounded native undo-close ledger retains failed document reopens and consumes one
/// descriptor only after a fresh identity is minted. Mutants lose the record or reuse the
/// retired identity.
#[test]
fn derived_native_reopen_ledger_proves_and_catches_loss_or_identity_reuse() {
    let model = native_reopen_ledger_model();
    assert_proves_and_catches(&model);

    // Capacity is reachable rather than a decorative upper bound: four live native
    // tabs closed without reopening saturate the three-entry abstract ledger.
    let mut state = model.init_state();
    for _ in 0..3 {
        state = model.successors("OpenAnother", &state)[0].clone();
    }
    for _ in 0..4 {
        state = model.successors("Close", &state)[0].clone();
    }
    assert_eq!(state["native_live"], 0);
    assert_eq!(state["ledger"], 3);

    // Prove both mutant classes have their own ordinary-action trace.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let closed = buggy.successors("Close", &buggy.init_state())[0].clone();
    let reused = buggy.successors("Reopen", &closed)[0].clone();
    assert!(!buggy.check_invariant("FreshReopenIdentity", &reused));
    let lost = buggy.successors("FailReopen", &closed)[0].clone();
    assert!(!buggy.check_invariant("FailedReopenRetainsDescriptor", &lost));

    // The same four closes over-fill the ledger when a full push evicts one
    // too few.
    let mut overfull = buggy.init_state();
    for _ in 0..3 {
        overfull = buggy.successors("OpenAnother", &overfull)[0].clone();
    }
    for _ in 0..4 {
        overfull = buggy.successors("Close", &overfull)[0].clone();
    }
    assert_eq!(overfull["ledger"], 4);
    assert!(!buggy.check_invariant("LedgerBounded", &overfull));
    assert_every_invariant_carries_a_mutant(
        &model,
        &[
            "NativeLiveBounded",
            "NextIdentityBounded",
            "FailureCountBounded",
        ],
    );
}

/// Closed-view and closed-tab recovery are separately bounded, never double-record one
/// gesture, and retain both kinds of record across failed reconstruction.
#[test]
fn derived_closed_recovery_ledgers_prove_and_catch_double_record_or_loss() {
    let model = closed_recovery_ledgers_model();
    assert_proves_and_catches(&model);

    let initial = model.init_state();
    let after_view = model.successors("CloseView", &initial)[0].clone();
    assert_eq!(after_view["view_ledger"], 1);
    assert_eq!(after_view["tab_ledger"], 0);
    let after_tab = model.successors("CloseTab", &after_view)[0].clone();
    assert_eq!(after_tab["view_ledger"], 1);
    assert_eq!(after_tab["tab_ledger"], 1);

    // A push that evicts one too few over-fills each ledger in turn.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut state = buggy.init_state();
    for action in ["CloseView", "CloseTab", "OpenTab", "CloseView"] {
        state = buggy.successors(action, &state)[0].clone();
    }
    assert_eq!(state["view_ledger"], 3);
    assert!(!buggy.check_invariant("ViewLedgerBounded", &state));
    state = buggy.successors("CloseTab", &state)[0].clone();
    assert_eq!(state["tab_ledger"], 4);
    assert!(!buggy.check_invariant("TabLedgerBounded", &state));
    assert_every_invariant_carries_a_mutant(&model, &["LiveLeavesBounded", "FailureCountBounded"]);
}

/// Markdown reading history is per-view, capacity-bounded, and a new visit from
/// the middle discards the abandoned forward branch before appending.
#[test]
fn derived_native_markdown_history_proves_and_catches_unbounded_or_untrimmed_visit() {
    let model = native_markdown_history_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut uncapped = buggy.init_state();
    for _ in 0..4 {
        uncapped = buggy.successors("Visit", &uncapped)[0].clone();
    }
    assert!(!buggy.check_invariant("HistoryBounded", &uncapped));

    let first = buggy.successors("Visit", &buggy.init_state())[0].clone();
    let second = buggy.successors("Visit", &first)[0].clone();
    let backed = buggy.successors("Back", &second)[0].clone();
    let branched = buggy.successors("Visit", &backed)[0].clone();
    assert!(!buggy.check_invariant("ForwardBranchTruncated", &branched));

    // The cursor's two off-by-one edges, from a one-entry history.
    let past_newest = buggy.successors("Forward", &first)[0].clone();
    assert_eq!((past_newest["len"], past_newest["cursor"]), (1, 2));
    assert!(!buggy.check_invariant("CursorWithinHistory", &past_newest));
    let off_oldest = buggy.successors("Back", &first)[0].clone();
    assert_eq!((off_oldest["len"], off_oldest["cursor"]), (1, 0));
    assert!(!buggy.check_invariant("EmptyIffNoCursor", &off_oldest));
    assert_every_invariant_carries_a_mutant(&model, &["VisitsBounded"]);
}

/// A row request must retain progress inside a tall Markdown block. The
/// negative configuration is the retired block-only reducer, which jumps four
/// visual rows for one input row and is caught immediately.
#[test]
fn derived_native_markdown_viewport_proves_and_catches_block_only_scroll() {
    let model = native_markdown_viewport_model();
    assert_proves_and_catches(&model);

    let good = model.successors("Step", &model.init_state())[0].clone();
    assert_eq!(good["actual_row"], 1);
    assert_eq!(good["expected_row"], 1);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let skipped = buggy.successors("Step", &buggy.init_state())[0].clone();
    assert_eq!(skipped["actual_row"], 4);
    assert!(!buggy.check_invariant("ExactIntraBlockProgress", &skipped));
}

#[test]
fn derived_native_editor_viewport_proves_and_catches_fixed_desktop_capacity() {
    let model = native_editor_viewport_model();
    assert_proves_and_catches(&model);

    let compact = model.successors("Resize", &model.init_state())[0].clone();
    assert_eq!(compact["visible_lines"], 8);
    assert_eq!(compact["anchor_line"], 15);
    assert_eq!(compact["short_visible_lines"], 40);
    assert_eq!(compact["short_anchor_line"], 0);
    assert!(model.check_invariant("CaretVisibleAfterResize", &compact));
    assert!(model.check_invariant("ShortDocumentFullyVisible", &compact));

    let bottom = model.successors("Overscroll", &model.init_state())[0].clone();
    assert_eq!(bottom["scroll_anchor_line"], 9);
    assert!(model.check_invariant("StoredScrollAnchorPresentable", &bottom));
    let reversed = model.successors("ReverseScroll", &bottom)[0].clone();
    assert_eq!(reversed["scroll_anchor_line"], 8);
    assert!(model.check_invariant("FirstReverseStepMoves", &reversed));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let hidden = buggy.successors("Resize", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("CaretVisibleAfterResize", &hidden));
    assert!(!buggy.check_invariant("ShortDocumentFullyVisible", &hidden));
    let indebted = buggy.successors("Overscroll", &buggy.init_state())[0].clone();
    assert_eq!(indebted["scroll_anchor_line"], 12);
    assert!(!buggy.check_invariant("StoredScrollAnchorPresentable", &indebted));
    let inert = buggy.successors("ReverseScroll", &indebted)[0].clone();
    assert_eq!(inert["scroll_anchor_line"], 11);
    assert!(!buggy.check_invariant("FirstReverseStepMoves", &inert));
}

#[test]
fn derived_native_editor_command_palette_proves_selection_and_exact_submit() {
    let model = native_editor_command_palette_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let open = buggy.successors("Open", &buggy.init_state())[0].clone();
    let broad = buggy.successors("TypeBroad", &open)[0].clone();
    let moved = buggy.successors("MoveNext", &broad)[0].clone();
    let stale = buggy.successors("Refine", &moved)[0].clone();
    assert!(!buggy.check_invariant("SelectionWithinResults", &stale));
    assert!(!buggy.check_invariant("QueryChangeResetsSelection", &stale));

    let exact_query = buggy.successors("TabComplete", &open)[0].clone();
    let wrong_dispatch = buggy.successors("Submit", &exact_query)[0].clone();
    assert!(!buggy.check_invariant("SubmitIsExactSelected", &wrong_dispatch));
    assert_every_invariant_carries_a_mutant(&model, &["ResultsBounded", "PhaseBounded"]);
}

#[test]
fn derived_manual_config_completion_proves_keyboard_window_and_context_lifecycle() {
    let model = manual_config_completion_model();
    assert_proves_and_catches(&model);

    let mut page_two = model.init_state();
    for action in ["EnterSelection", "MoveNext", "MoveNext", "MoveNext"] {
        assert!(model.fire(action, &mut page_two), "{action}: {page_two:?}");
    }
    assert_eq!(page_two["selected"], 3);
    assert_eq!(page_two["window_start"], 3);
    assert!(model.check_invariant("SelectedCandidateVisible", &page_two));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut hidden = buggy.init_state();
    for action in ["EnterSelection", "MoveNext", "MoveNext", "MoveNext"] {
        assert!(buggy.fire(action, &mut hidden), "{action}: {hidden:?}");
    }
    assert_eq!(hidden["selected"], 3);
    assert_eq!(hidden["window_start"], 0);
    assert!(!buggy.check_invariant("SelectedCandidateVisible", &hidden));
}

#[test]
fn derived_manual_config_handoff_proves_path_reuse_and_exact_target_handling() {
    let model = manual_config_handoff_model();
    assert_proves_and_catches(&model);

    let selected = model.successors("RevealAuthoredKey", &model.init_state())[0].clone();
    assert_eq!(selected["selected_exact"], 1);
    assert_eq!(selected["canonical_path_authority"], 1);
    assert_eq!(selected["editor_instances"], 1);

    let fallback = model.successors("SeedAbsentKey", &selected)[0].clone();
    assert_eq!(fallback["search_exact"], 1);
    assert_eq!(fallback["completion_ready"], 1);
    assert_eq!(
        fallback["editor_instances"], 1,
        "the Manual editor is reused"
    );

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let redirected = buggy.successors("RevealAuthoredKey", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("HostOwnsCanonicalPath", &redirected));
    assert!(!buggy.check_invariant("AuthoredTargetSelected", &redirected));
}

#[test]
fn derived_native_packages_worker_proves_matching_completion_and_result_truth() {
    let model = native_packages_worker_model();
    assert_proves_and_catches(&model);

    let mut state = model.init_state();
    assert!(model.fire("BeginRefresh", &mut state));
    assert!(model.fire("FinishRefresh", &mut state));
    assert_eq!(state["observed"], 1);

    assert!(model.fire("BeginCheck", &mut state));
    assert_eq!(state["operation"], 2);
    assert!(model.fire("FinishCheckFailure", &mut state));
    assert_eq!(state["last_result"], 2);
    assert_eq!(state["presented_result"], 2);
    assert!(model.check_invariant("FinalResultIsPresented", &state));

    // A silent refresh preserves the user's last process result.
    assert!(model.fire("BeginRefresh", &mut state));
    assert!(model.fire("FinishRefresh", &mut state));
    assert_eq!(state["last_result"], 2);
    assert_eq!(state["presented_result"], 2);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut stale_success = buggy.init_state();
    assert!(buggy.fire("BeginCheck", &mut stale_success));
    assert!(buggy.fire("FinishCheckFailure", &mut stale_success));
    assert_eq!(stale_success["last_result"], 2);
    assert_eq!(stale_success["presented_result"], 1);
    assert!(!buggy.check_invariant("FinalResultIsPresented", &stale_success));

    // An abort that clears `inflight` but leaves `busy` names a verb nothing runs.
    let mut stuck_busy = buggy.init_state();
    assert!(buggy.fire("BeginCheck", &mut stuck_busy));
    assert!(buggy.fire("Abort", &mut stuck_busy));
    assert_eq!((stuck_busy["inflight"], stuck_busy["operation"]), (0, 2));
    assert!(!buggy.check_invariant("SingleFlightHasOneKind", &stuck_busy));

    // A refresh completion that assigns its absent command erases the verb's result.
    let mut erased = buggy.init_state();
    for action in [
        "BeginInstall",
        "FinishInstallSuccess",
        "BeginRefresh",
        "FinishRefresh",
    ] {
        assert!(buggy.fire(action, &mut erased), "{action}");
    }
    assert_eq!((erased["last_result"], erased["expected_result"]), (0, 1));
    assert!(!buggy.check_invariant("RefreshKeepsVerbResult", &erased));
    assert_every_invariant_carries_a_mutant(&model, &["StateIsBounded"]);
}

#[test]
fn derived_manual_problem_navigation_proves_exact_reveal_and_full_semantics() {
    let model = manual_config_problem_navigation_model();
    assert_proves_and_catches(&model);

    let mut one = model.successors("LoadOne", &model.init_state())[0].clone();
    assert!(model.fire("JumpNext", &mut one));
    assert_eq!(one["selected"], 0);
    assert_eq!(one["caret_target"], 1);
    assert_eq!(one["revealed"], 1);

    let mut wrapped = model.successors("LoadThree", &model.init_state())[0].clone();
    assert!(model.fire("JumpPrevious", &mut wrapped));
    assert_eq!(wrapped["selected"], 2);
    assert_eq!(wrapped["caret_target"], 3);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut paint_only = buggy.successors("LoadOne", &buggy.init_state())[0].clone();
    assert!(buggy.fire("JumpNext", &mut paint_only));
    assert!(!buggy.check_invariant("JumpMovesToExactProblem", &paint_only));
    assert!(!buggy.check_invariant("JumpRevealsProblem", &paint_only));
    assert!(!buggy.check_invariant("FullProblemIsSemantic", &paint_only));
}

#[test]
fn derived_native_recovery_interaction_proves_and_catches_unsafe_lifecycle() {
    let model = native_recovery_interaction_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut page = buggy.init_state();
    page = buggy.successors("NextPage", &page)[0].clone();
    page = buggy.successors("NextPage", &page)[0].clone();
    assert!(!buggy.check_invariant("PageBounded", &page));

    let pending = buggy.successors("BeginRetry", &buggy.init_state())[0].clone();
    let duplicate = buggy.successors("BeginCopy", &pending)[0].clone();
    assert!(!buggy.check_invariant("SingleCapabilityFlight", &duplicate));

    let cleared = buggy.successors("StaleComplete", &pending)[0].clone();
    assert!(!buggy.check_invariant("StaleCannotClear", &cleared));
    assert_every_invariant_carries_a_mutant(&model, &["StartsBounded", "CompletionsBounded"]);
}

/// Mark anchors survive ordinary motion, modal query input cannot become a
/// document edit, and cancelling search restores its captured origin.
#[test]
fn derived_native_editor_modal_proves_and_catches_anchor_or_input_leak() {
    let model = native_editor_modal_model();
    assert_proves_and_catches(&model);

    // The generic Buggy counterexample reaches MarkPinned first. Independently
    // drive the other defect class so modal typing's negative control can never
    // become vacuous behind that shorter trace.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let opened = buggy.successors("OpenCommand", &buggy.init_state())[0].clone();
    let leaked = buggy.successors("MinibufferType", &opened)[0].clone();
    assert!(!buggy.check_invariant("MinibufferCannotEditDocument", &leaked));

    // Goto is a first-class modal lifecycle rather than an unmodelled M-x side
    // effect: query input remains non-mutating and accepted/cancelled exits are
    // distinct reachable transitions.
    let goto = model.successors("OpenGoto", &model.init_state())[0].clone();
    let typed = model.successors("MinibufferType", &goto)[0].clone();
    let submitted = model.successors("SubmitGoto", &typed)[0].clone();
    assert_eq!(submitted["mode"], 0);
    assert_eq!(submitted["caret"], 1);
    let aborted = model.successors("AbortGoto", &goto)[0].clone();
    assert_eq!(aborted["last_exit"], 1);

    // The off-by-one search clamp: a search opened at the document end parks
    // the caret one past it, and a mark set there pins an anchor past it too.
    let mut at_end = model.init_state();
    for _ in 0..3 {
        at_end = model.successors("Move", &at_end)[0].clone();
    }
    assert_eq!(at_end["caret"], 3);
    let search = buggy.successors("OpenSearch", &at_end)[0].clone();
    let clamped = model.successors("MinibufferType", &search)[0].clone();
    assert_eq!(clamped["caret"], 3);
    let overrun = buggy.successors("MinibufferType", &search)[0].clone();
    assert_eq!(overrun["caret"], 4);
    assert!(!buggy.check_invariant("CaretBounded", &overrun));
    let accepted = buggy.successors("Submit", &overrun)[0].clone();
    let marked = buggy.successors("SetMark", &accepted)[0].clone();
    assert!(!buggy.check_invariant("AnchorBounded", &marked));

    assert_every_invariant_carries_a_mutant(
        &model,
        &[
            "ModeBounded",
            "QueryBounded",
            "DocumentEditsBounded",
            "ExitKindBounded",
        ],
    );
}

/// Native front content cannot inherit a hidden PTY target, while Owner App and
/// explicitly addressed live sessions remain independent of front focus.
#[test]
fn derived_native_control_routing_proves_and_catches_hidden_terminal_fallback() {
    let model = native_control_routing_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let native = buggy.successors("FocusNative", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("OwnerAppAlwaysAllowed", &native));
    assert!(!buggy.check_invariant("NoHiddenTerminalFallback", &native));
    let terminal = buggy.successors("FocusTerminal", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("EdgeAppDenied", &terminal));

    // Swapped liveness inputs: retiring the explicit session with a terminal in
    // front closes the bare route and leaves the explicit one open.
    let retired = buggy.successors("RetireExplicitSession", &buggy.init_state())[0].clone();
    assert_eq!(
        (
            retired["bare_session_allowed"],
            retired["explicit_session_allowed"]
        ),
        (0, 1)
    );
    assert!(!buggy.check_invariant("BareSessionIffFrontTerminal", &retired));
    assert!(!buggy.check_invariant("ExplicitSessionIffLive", &retired));
    assert_every_invariant_carries_a_mutant(&model, &["FrontKindBounded"]);
}

/// Socket admission never exceeds its queued-plus-running worker lanes, and a
/// completing worker releases exactly the lane it held. The mutants over-admit
/// while every worker is already owned, and complete without releasing.
#[test]
fn derived_control_connection_admission_proves_and_catches_overflow() {
    let model = control_connection_admission_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let first = buggy.successors("Admit", &buggy.init_state())[0].clone();
    let full = buggy.successors("Admit", &first)[0].clone();
    let overflow = buggy.successors("Admit", &full)[0].clone();
    assert!(!buggy.check_invariant("LaneBounded", &overflow));

    let leaked = buggy.successors("Complete", &first)[0].clone();
    assert_eq!((leaked["outstanding"], leaked["completed"]), (1, 1));
    assert!(!buggy.check_invariant("AcceptedWorkAccounted", &leaked));
    assert_every_invariant_carries_a_mutant(&model, &["ArrivalsBounded"]);
}

/// A control connection is on a request lane only while it has a request: idle,
/// it parks; waiting, it moves to a wait lane; so persistent drivers never use
/// the request lanes up and a fresh client is refused only for work or the
/// open-connection bound. The mutant is the 2026-09-26 design (and four more
/// defects), each caught alone; its headline trace is replayed here: two
/// drivers go idle ON their lanes and the third client is refused for it.
#[test]
fn derived_control_lane_tenure_proves_and_catches_the_held_lane() {
    let model = control_lane_tenure_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &[]);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let step = |state, action| buggy.successors(action, &state)[0].clone();
    let first = step(buggy.init_state(), "Admit");
    let idle_on_lane = step(first, "Finish");
    assert!(!buggy.check_invariant("NoIdleHold", &idle_on_lane));
    let second = step(idle_on_lane, "Admit");
    let both_idle = step(second, "Finish");
    let refused = step(both_idle, "Refuse");
    assert!(!buggy.check_invariant("NoRefusalByIdleDriver", &refused));

    // The same two drivers under the shipping design park: both request lanes
    // are free, yet a third client is still refused — for the open-connection
    // bound (Cap=2, two open), not for a lane an idle driver holds, so the
    // refusal leaves `NoRefusalByIdleDriver` intact.
    let step = |state, action| model.successors(action, &state)[0].clone();
    let first = step(model.init_state(), "Admit");
    let parked = step(first, "Finish");
    let second = step(parked, "Admit");
    let both_parked = step(second, "Finish");
    assert_eq!(both_parked["parked"], 2);
    assert_eq!(
        both_parked["working"] + both_parked["held"] + both_parked["queued"],
        0,
        "no request lane is taken"
    );
    assert_eq!(both_parked["open"], 2, "open == Cap");
    assert!(
        model.successors("Admit", &both_parked).is_empty(),
        "Cap=2 bounds the open connections"
    );
    let refused = model.successors("Refuse", &both_parked);
    assert_eq!(refused.len(), 1, "the third client is refused");
    assert!(
        model.check_invariant("NoRefusalByIdleDriver", &refused[0]),
        "a refusal for the open-connection bound is not one an idle driver caused"
    );
}

/// Native Settings has one process instance and at most one ordinary implicit
/// view per window, and every activation focuses the window that asked. The
/// mutants allocate again on repeated activation, and raise the singleton's
/// first window instead of the requester.
#[test]
fn derived_native_settings_singleton_proves_and_catches_duplicate_activation() {
    let model = native_settings_singleton_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let one = buggy.successors("OpenOne", &buggy.init_state())[0].clone();
    let stolen = buggy.successors("OpenTwo", &one)[0].clone();
    assert_eq!(
        (stolen["requesting_window"], stolen["focused_window"]),
        (2, 1)
    );
    assert!(!buggy.check_invariant("RequestingWindowFocused", &stolen));
    assert_every_invariant_carries_a_mutant(&model, &["OpensBounded"]);
}

/// Previous, Next, absolute positioning, and signed line scrolling share one
/// clamped Settings virtual cursor. The mutant omits the upper clamp.
#[test]
fn derived_settings_page_scroll_proves_and_catches_overscroll() {
    let model = settings_page_scroll_model();
    assert_proves_and_catches(&model);

    let mut at_end = model.init_state();
    for _ in 0..3 {
        at_end = model.successors("GrowLimit", &at_end)[0].clone();
        at_end = model.successors("NextPage", &at_end)[0].clone();
    }
    assert_eq!(at_end["limit"], 3);
    assert_eq!(at_end["cursor"], 3);
    let clamped = model.successors("NextPage", &at_end)[0].clone();
    assert_eq!(clamped["cursor"], 3);

    let mut out_of_range = at_end;
    for _ in 0..4 {
        out_of_range = model.successors("ChooseTarget", &out_of_range)[0].clone();
    }
    let absolute = model.successors("Absolute", &out_of_range)[0].clone();
    assert_eq!(absolute["target"], 4);
    assert_eq!(absolute["cursor"], 3);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let overscrolled = buggy.successors("NextPage", &buggy.init_state())[0].clone();
    assert_eq!(overscrolled["limit"], 0);
    assert_eq!(overscrolled["cursor"], 1);
    assert!(!buggy.check_invariant("CursorBounded", &overscrolled));
}

/// Screenshot ordering is a present barrier: a staged native frame may be
/// captured only after its present succeeds. Drops retry to a fixed bound and
/// then fail closed. The mutants capture the old compositor pixels on a drop, and
/// ask for a retry at the bound instead of failing closed.
#[test]
fn derived_capture_after_present_proves_and_catches_stale_pixels_and_late_retry() {
    let model = capture_after_present_model();
    assert_proves_and_catches(&model);

    let mut state = model.successors("Mutate", &model.init_state())[0].clone();
    for expected_attempt in 1..=2 {
        let retry = model.successors("Decide", &state)[0].clone();
        assert_eq!(retry["decision"], 2);
        assert_eq!(retry["captured"], 0);
        assert_eq!(retry["attempts"], expected_attempt);
        state = model.successors("Retry", &retry)[0].clone();
    }
    let failed = model.successors("Decide", &state)[0].clone();
    assert_eq!(failed["decision"], 3);
    assert_eq!(failed["failed"], 1);
    assert_eq!(failed["captured"], 0);
    assert_eq!(failed["attempts"], 3);
    assert!(
        model.successors("Retry", &failed).is_empty(),
        "three failed presents exhaust the production attempt bound"
    );
    assert!(
        model.successors("Decide", &failed).is_empty(),
        "the model must not admit a fourth decision/present attempt"
    );

    let mutated = model.successors("Mutate", &model.init_state())[0].clone();
    let presented = model.successors("MarkPresentSucceeded", &mutated)[0].clone();
    let captured = model.successors("Decide", &presented)[0].clone();
    assert_eq!(captured["decision"], 1);
    assert_eq!(captured["captured"], 1);
    assert_eq!(captured["staged"], 0);

    // Both mutants are dead at the committed config, and each is independently
    // fired and caught (not one global counterexample shared between them).
    let mutants = ["BuggyRetryAtLimit", "BuggyStaleCapture"];
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &mutants),
        Ok(mutants.len())
    );

    // Each mutant, pinned by its own trace. The stale capture: a dropped present
    // authorizes the old pixels.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mutated = buggy.successors("Mutate", &buggy.init_state())[0].clone();
    let stale = buggy.successors("BuggyStaleCapture", &mutated)[0].clone();
    assert_eq!(stale["captured"], 1);
    assert_eq!(stale["staged"], 1);
    assert!(!buggy.check_invariant("NoStaleCapture", &stale));
    // The off-by-one retry arm: drop every present, and the third decision asks
    // for a fourth attempt instead of failing closed.
    let mut state = mutated;
    for _ in 1..=2 {
        assert!(buggy.successors("BuggyRetryAtLimit", &state).is_empty());
        let retry = buggy.successors("Decide", &state)[0].clone();
        assert_eq!(retry["decision"], 2);
        state = buggy.successors("Retry", &retry)[0].clone();
    }
    let at_limit = buggy.successors("BuggyRetryAtLimit", &state)[0].clone();
    assert_eq!(
        (
            at_limit["attempts"],
            at_limit["decision"],
            at_limit["failed"]
        ),
        (3, 2, 0),
        "the mutant retries at the limit"
    );
    assert!(!buggy.check_invariant("DecisionMatchesOutcome", &at_limit));
    assert!(buggy.check_invariant("NoStaleCapture", &at_limit));
    assert!(buggy.check_invariant("CaptureRequiresPresent", &at_limit));
    assert!(
        buggy.successors("Retry", &at_limit).is_empty(),
        "the loop bound still refuses the fourth present"
    );
    assert_every_invariant_carries_a_mutant(&model, &["ValuesBounded"]);
}

/// A pre-created video directory remains privately owned through the pending,
/// recording, and exporting phases. Every non-success terminal path removes it;
/// success alone transfers it to a published artifact. Recording and exporting
/// use disjoint slots, so an exporter blocks a second start.
#[test]
fn derived_video_recording_lifecycle_proves_cleanup_and_serialization() {
    let model = video_recording_lifecycle_model();
    assert_proves_and_catches(&model);

    let reserved = model.successors("Reserve", &model.init_state())[0].clone();
    assert_eq!(reserved["phase"], 1);
    assert_eq!(reserved["recording_slot"], 1);
    assert_eq!(reserved["private_dirs"], 1);
    assert_eq!(reserved["published"], 0);

    let recording = model.successors("BeginHeadless", &reserved)[0].clone();
    assert_eq!(recording["phase"], 2);
    assert_eq!(recording["mode"], 2);
    assert_eq!(recording["timer"], 1);
    assert!(model.check_invariant("OffscreenOnlyWithoutGlass", &recording));
    assert!(model.check_invariant("RecordingOwnsItsPacingTimer", &recording));
    assert_eq!(
        model.successors("Reserve", &recording).len(),
        0,
        "a live recording refuses a second reservation"
    );

    let ticked = model.successors("Tick", &recording)[0].clone();
    let exporting = model.successors("BeginExport", &ticked)[0].clone();
    assert_eq!(exporting["phase"], 3);
    assert_eq!(exporting["mode"], 0);
    assert_eq!(exporting["timer"], 0);
    assert_eq!(exporting["recording_slot"], 0);
    assert_eq!(exporting["export_permit"], 1);
    assert_eq!(exporting["private_dirs"], 1);
    assert_eq!(
        model.successors("Reserve", &exporting).len(),
        0,
        "the process-wide export permit blocks a second capture"
    );
    assert_eq!(
        model.successors("PublishSuccess", &exporting).len(),
        0,
        "publication must first win the live-to-authorized CAS"
    );

    let authorized = model.successors("AuthorizeCommit", &exporting)[0].clone();
    assert_eq!(authorized["cancel_state"], 2);
    assert_eq!(
        model.successors("CancelLive", &authorized).len(),
        0,
        "late cancellation cannot change an authorized commit to cancelled"
    );
    assert_eq!(
        model.successors("CancelAfterAuthorization", &authorized),
        vec![authorized.clone()],
        "a cancellation that loses the CAS changes nothing"
    );

    let published = model.successors("PublishSuccess", &authorized)[0].clone();
    assert_eq!(published["phase"], 0);
    assert_eq!(
        (published["recording_slot"], published["export_permit"]),
        (0, 0)
    );
    assert_eq!(published["private_dirs"], 0);
    assert_eq!(published["published"], 1);
    assert_eq!(published["cancel_state"], 2);
    assert!(model.check_invariant("CommitAuthorizationScope", &published));

    // A live transition to translucent glass aborts the raw tap and cleans its
    // unpublished directory before another frame can be accepted.
    let glass = model.successors("AttachGlass", &model.init_state())[0].clone();
    let reserved = model.successors("Reserve", &glass)[0].clone();
    let tap = model.successors("BeginOnGlass", &reserved)[0].clone();
    assert_eq!(tap["mode"], 1);
    assert_eq!(tap["timer"], 0, "an unpaced tap is driven by its presents");
    // A paced tap arms the same WaitUntil owner the offscreen loop uses.
    let paced = model.successors("RequestPaced", &glass)[0].clone();
    let reserved_paced = model.successors("Reserve", &paced)[0].clone();
    let paced_tap = model.successors("BeginOnGlass", &reserved_paced)[0].clone();
    assert_eq!((paced_tap["mode"], paced_tap["timer"]), (1, 1));
    assert!(model.action_enabled("Tick", &paced_tap));
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, &paced_tap),
            "{}",
            invariant.name
        );
    }
    let aborted = model.successors("MakeTapTranslucent", &tap)[0].clone();
    assert_eq!(aborted["phase"], 0);
    assert_eq!(aborted["private_dirs"], 0);
    assert_eq!(aborted["translucent"], 1);
    assert!(model.check_invariant("NoTranslucentTap", &aborted));

    // Each failure boundary is explicit and owns the same cleanup obligation.
    let reserved = model.successors("Reserve", &model.init_state())[0].clone();
    let rejected = model.successors("RejectBegin", &reserved)[0].clone();
    assert_eq!((rejected["phase"], rejected["private_dirs"]), (0, 0));

    let reserved = model.successors("Reserve", &model.init_state())[0].clone();
    let recording = model.successors("BeginHeadless", &reserved)[0].clone();
    let cancelled = model.successors("CancelLive", &recording)[0].clone();
    assert_eq!(
        (cancelled["recording_slot"], cancelled["private_dirs"]),
        (0, 0)
    );
    assert_eq!(cancelled["cancel_state"], 1);
    assert_eq!(
        model.successors("AuthorizeCommit", &cancelled).len(),
        0,
        "a cancelled lifecycle cannot later authorize publication"
    );

    let reserved = model.successors("Reserve", &model.init_state())[0].clone();
    let recording = model.successors("BeginHeadless", &reserved)[0].clone();
    let exporting = model.successors("BeginExport", &recording)[0].clone();
    let failed = model.successors("Fail", &exporting)[0].clone();
    assert_eq!((failed["export_permit"], failed["private_dirs"]), (0, 0));

    let reserved = model.successors("Reserve", &model.init_state())[0].clone();
    let recording = model.successors("BeginHeadless", &reserved)[0].clone();
    let owner_lost = model.successors("OwnerLost", &recording)[0].clone();
    assert_eq!(
        (owner_lost["recording_slot"], owner_lost["private_dirs"]),
        (0, 0)
    );

    // Buggy cleanup strands the pre-created directory after no lifecycle owns
    // it. This is a transition-derived negative control, not a fabricated state.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let reserved = buggy.successors("Reserve", &buggy.init_state())[0].clone();
    let leaked = buggy.successors("BuggyStrandPrivateDirOnCleanup", &reserved)[0].clone();
    assert_eq!((leaked["phase"], leaked["private_dirs"]), (0, 1));
    assert!(!buggy.check_invariant("PrivateDirectoryOwnedByLifecycle", &leaked));

    // A windowed offscreen fallback lies about the capture source.
    let glass = buggy.successors("AttachGlass", &buggy.init_state())[0].clone();
    let reserved = buggy.successors("Reserve", &glass)[0].clone();
    let dishonest = buggy.successors("BuggyBeginOnGlassOffscreen", &reserved)[0].clone();
    assert!(!buggy.check_invariant("OffscreenOnlyWithoutGlass", &dishonest));

    // The opacity mutant retains a live swapchain tap after its admission
    // assumption has become false.
    let glass = buggy.successors("AttachGlass", &buggy.init_state())[0].clone();
    let reserved = buggy.successors("Reserve", &glass)[0].clone();
    let tap = buggy.successors("BeginOnGlass", &reserved)[0].clone();
    let translucent_tap = buggy.successors("BuggyRetainTapWhenTranslucent", &tap)[0].clone();
    assert_eq!(translucent_tap["phase"], 2);
    assert_eq!(translucent_tap["mode"], 1);
    assert_eq!(translucent_tap["translucent"], 1);
    assert!(!buggy.check_invariant("NoTranslucentTap", &translucent_tap));

    // Publication without the CAS authorization creates a completion artifact
    // that cancellation was still entitled to revoke.
    let reserved = buggy.successors("Reserve", &buggy.init_state())[0].clone();
    let recording = buggy.successors("BeginHeadless", &reserved)[0].clone();
    let exporting = buggy.successors("BeginExport", &recording)[0].clone();
    let unauthorized = buggy.successors("BuggyPublishWithoutAuthorization", &exporting)[0].clone();
    assert_eq!(unauthorized["published"], 1);
    assert_eq!(unauthorized["cancel_state"], 0);
    assert!(!buggy.check_invariant("CommitAuthorizationScope", &unauthorized));

    // Starting again during export overlaps both concrete ownership slots and
    // produces two privately owned lifecycle directories.
    let reserved = buggy.successors("Reserve", &buggy.init_state())[0].clone();
    let recording = buggy.successors("BeginHeadless", &reserved)[0].clone();
    let exporting = buggy.successors("BeginExport", &recording)[0].clone();
    let overlapped = buggy.successors("BuggyStartSecondWhileExporting", &exporting)[0].clone();
    assert_eq!(overlapped["recording_slot"], 1);
    assert_eq!(overlapped["export_permit"], 1);
    assert_eq!(overlapped["private_dirs"], 2);
    assert!(!buggy.check_invariant("SlotMatchesPhase", &overlapped));
    assert!(!buggy.check_invariant("PrivateDirectoryOwnedByLifecycle", &overlapped));

    // A finalize that reads `video_rec` instead of taking it exports with the
    // recording slot still occupied.
    let reserved = buggy.successors("Reserve", &buggy.init_state())[0].clone();
    let recording = buggy.successors("BeginHeadless", &reserved)[0].clone();
    let held = buggy.successors("BuggyFinalizeKeepsRecordingSlot", &recording)[0].clone();
    assert_eq!(
        (held["phase"], held["recording_slot"], held["export_permit"]),
        (3, 1, 1)
    );
    assert!(!buggy.check_invariant("SlotMatchesPhase", &held));
    assert!(buggy.check_invariant("PrivateDirectoryOwnedByLifecycle", &held));

    // Finalize that forgets `ws.present = None` exports while the headless
    // window still carries its recording-only Virtual target.
    let reserved = buggy.successors("Reserve", &buggy.init_state())[0].clone();
    let recording = buggy.successors("BeginHeadless", &reserved)[0].clone();
    let retained = buggy.successors("BuggyExportRetainsVirtualTarget", &recording)[0].clone();
    assert_eq!((retained["phase"], retained["mode"]), (3, 2));
    assert!(!buggy.check_invariant("ModeMatchesRecordingPhase", &retained));

    // A window close that skips `video_abort_window_close` leaves the tap
    // copying a surface that no longer exists.
    let glass = buggy.successors("AttachGlass", &buggy.init_state())[0].clone();
    let reserved = buggy.successors("Reserve", &glass)[0].clone();
    let tap = buggy.successors("BeginOnGlass", &reserved)[0].clone();
    let orphaned = buggy.successors("BuggyDetachGlassDuringTap", &tap)[0].clone();
    assert_eq!((orphaned["mode"], orphaned["glass"]), (1, 0));
    assert!(!buggy.check_invariant("TapOnlyOnGlass", &orphaned));

    // `pace.then_some(now)` begins an unpaced offscreen loop with no redraw
    // driver. Paced, the same slip happens to arm the timer and is harmless.
    let reserved = buggy.successors("Reserve", &buggy.init_state())[0].clone();
    let untimed = buggy.successors("BuggyBeginHeadlessUntimed", &reserved)[0].clone();
    assert_eq!((untimed["mode"], untimed["timer"]), (2, 0));
    assert!(!buggy.check_invariant("RecordingOwnsItsPacingTimer", &untimed));
    let paced = buggy.successors("RequestPaced", &buggy.init_state())[0].clone();
    let reserved = buggy.successors("Reserve", &paced)[0].clone();
    let lucky = buggy.successors("BuggyBeginHeadlessUntimed", &reserved)[0].clone();
    assert!(buggy.check_invariant("RecordingOwnsItsPacingTimer", &lucky));
    assert!(
        model.check_invariant("OffscreenOnlyWithoutGlass", &untimed),
        "the untimed begin is otherwise an honest headless recording"
    );

    // A paced tap whose begin arms no deadline starves on its first skipped
    // unchanged frame.
    let glass = buggy.successors("AttachGlass", &buggy.init_state())[0].clone();
    let paced = buggy.successors("RequestPaced", &glass)[0].clone();
    let reserved = buggy.successors("Reserve", &paced)[0].clone();
    let starved = buggy.successors("BuggyPacedTapUntimed", &reserved)[0].clone();
    assert_eq!((starved["mode"], starved["timer"]), (1, 0));
    assert!(!buggy.check_invariant("RecordingOwnsItsPacingTimer", &starved));

    // A blind store of VIDEO_CANCELLED revokes an export past its CAS: the
    // take reads cancelled while it is still exporting.
    let reserved = buggy.successors("Reserve", &buggy.init_state())[0].clone();
    let recording = buggy.successors("BeginHeadless", &reserved)[0].clone();
    let exporting = buggy.successors("BeginExport", &recording)[0].clone();
    let authorized = buggy.successors("AuthorizeCommit", &exporting)[0].clone();
    let revoked = buggy.successors("BuggyLateCancelRevokes", &authorized)[0].clone();
    assert_eq!((revoked["cancel_state"], revoked["phase"]), (1, 3));
    assert!(!buggy.check_invariant("CancelledOwnsNothing", &revoked));

    let mutants = [
        "BuggyStrandPrivateDirOnCleanup",
        "BuggyBeginOnGlassOffscreen",
        "BuggyRetainTapWhenTranslucent",
        "BuggyPublishWithoutAuthorization",
        "BuggyStartSecondWhileExporting",
        "BuggyExportRetainsVirtualTarget",
        "BuggyFinalizeKeepsRecordingSlot",
        "BuggyDetachGlassDuringTap",
        "BuggyBeginHeadlessUntimed",
        "BuggyPacedTapUntimed",
        "BuggyLateCancelRevokes",
    ];
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &mutants),
        Ok(mutants.len()),
        "each lifecycle slip must fire and be caught on its own"
    );

    assert_every_invariant_breaks_first(&model, &["Bounds"]);
}

/// Retention is an exact lease decision. PID liveness is consulted only for
/// lease-less legacy namespaces; it cannot override a held, free, or malformed
/// lease observation.
#[test]
fn derived_exact_instance_retention_covers_the_lease_pid_matrix() {
    let model = exact_instance_retention_model();
    assert_proves_and_catches(&model);

    // (selector from MissingLegacy, lease code, pid_alive, decision)
    // decision: 1 Keep, 2 Remove.
    for (selector, expected_lease, pid_alive, expected_decision) in [
        (None, 0, 0, 2),
        (None, 0, 1, 1),
        (Some("SelectHeld"), 1, 0, 1),
        (Some("SelectHeld"), 1, 1, 1),
        (Some("SelectFree"), 2, 0, 2),
        (Some("SelectFree"), 2, 1, 2),
        (Some("SelectMalformed"), 3, 0, 1),
        (Some("SelectMalformed"), 3, 1, 1),
    ] {
        let mut state = model.init_state();
        if let Some(action) = selector {
            state = model.successors(action, &state)[0].clone();
        }
        if pid_alive == 1 {
            state = model.successors("ObservePidAlive", &state)[0].clone();
        }
        let decided = model.successors("Decide", &state)[0].clone();
        assert_eq!(decided["lease"], expected_lease);
        assert_eq!(decided["pid_alive"], pid_alive);
        assert_eq!(
            decided["decision"], expected_decision,
            "lease={expected_lease} pid_alive={pid_alive}"
        );
        for invariant in [
            "HeldNeverRemoved",
            "MalformedNeverRemoved",
            "FreeAlwaysRemoved",
            "MissingAloneUsesPidFallback",
        ] {
            assert!(
                model.check_invariant(invariant, &decided),
                "{invariant}: lease={expected_lease} pid_alive={pid_alive}"
            );
        }
    }

    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    // A held exact-instance lease is live authority even if the PID probe says
    // dead; removing it is the destructive false-stale decision.
    let held = buggy.successors("SelectHeld", &buggy.init_state())[0].clone();
    let removed_held = buggy.successors("Decide", &held)[0].clone();
    assert_eq!(removed_held["decision"], 2);
    assert!(!buggy.check_invariant("HeldNeverRemoved", &removed_held));

    // Malformed lease metadata fails closed. It cannot be reclassified as free.
    let malformed = buggy.successors("SelectMalformed", &buggy.init_state())[0].clone();
    let removed_malformed = buggy.successors("Decide", &malformed)[0].clone();
    assert_eq!(removed_malformed["decision"], 2);
    assert!(!buggy.check_invariant("MalformedNeverRemoved", &removed_malformed));

    // A freely lockable exact lease proves the old instance is gone. A reused,
    // live numeric PID must not retain that stale namespace.
    let free = buggy.successors("SelectFree", &buggy.init_state())[0].clone();
    let reused_pid = buggy.successors("ObservePidAlive", &free)[0].clone();
    let wrongly_kept = buggy.successors("Decide", &reused_pid)[0].clone();
    assert_eq!(wrongly_kept["decision"], 1);
    assert!(!buggy.check_invariant("FreeAlwaysRemoved", &wrongly_kept));

    // A lease-less legacy namespace whose PID is dead is the one case the PID
    // probe decides. Losing that arm into fail-closed Keep leaks it forever.
    let legacy_dead = buggy.successors("Decide", &buggy.init_state())[0].clone();
    assert_eq!((legacy_dead["lease"], legacy_dead["pid_alive"]), (0, 0));
    assert_eq!(legacy_dead["decision"], 1);
    assert!(!buggy.check_invariant("MissingAloneUsesPidFallback", &legacy_dead));

    assert_every_invariant_breaks_first(&model, &["Bounds"]);
}

/// Confined artifact operations retain the original inside object. An ancestor
/// swap can make reply-time validation fail, but cannot redirect the operation
/// or certify the replacement object.
#[test]
fn derived_anchored_artifact_transaction_proves_handle_and_reply_identity() {
    let model = anchored_artifact_transaction_model();
    assert_proves_and_catches(&model);

    let pinned = model.successors("ConfinePin", &model.init_state())[0].clone();
    assert_eq!(pinned["phase"], 1);
    assert_eq!(pinned["path_identity"], 1);

    let read = model.successors("ReadPinned", &pinned)[0].clone();
    assert_eq!((read["operation"], read["effect_target"]), (1, 1));
    let read_reply = model.successors("ValidateReply", &read)[0].clone();
    assert_eq!(read_reply["reply"], 1);
    assert_eq!(read_reply["certified_identity"], 1);
    assert!(model.check_invariant("SuccessfulReplyCertifiesOriginal", &read_reply));

    let pinned = model.successors("ConfinePin", &model.init_state())[0].clone();
    let write = model.successors("WritePinned", &pinned)[0].clone();
    assert_eq!((write["operation"], write["effect_target"]), (2, 1));
    let write_reply = model.successors("ValidateReply", &write)[0].clone();
    assert_eq!(write_reply["reply"], 1);
    assert_eq!(write_reply["certified_identity"], 1);

    // If the ancestor is replaced before I/O, a retained handle still targets
    // the original object, while reply-time identity validation fails closed.
    let pinned = model.successors("ConfinePin", &model.init_state())[0].clone();
    let swapped = model.successors("SwapAncestor", &pinned)[0].clone();
    let anchored_read = model.successors("ReadPinned", &swapped)[0].clone();
    assert_eq!(anchored_read["effect_target"], 1);
    let rejected = model.successors("ValidateReply", &anchored_read)[0].clone();
    assert_eq!(rejected["reply"], 2);
    assert_eq!(rejected["certified_identity"], 0);
    assert!(model.check_invariant("AnchoredAccessNeverOutside", &rejected));
    assert!(model.check_invariant("SuccessfulReplyCertifiesOriginal", &rejected));

    // The same validation catches a swap in the operation-to-reply interval.
    let pinned = model.successors("ConfinePin", &model.init_state())[0].clone();
    let anchored_write = model.successors("WritePinned", &pinned)[0].clone();
    let swapped = model.successors("SwapAncestor", &anchored_write)[0].clone();
    let rejected = model.successors("ValidateReply", &swapped)[0].clone();
    assert_eq!(rejected["effect_target"], 1);
    assert_eq!(rejected["reply"], 2);

    // A swap may also be rejected before any read/write is attempted.
    let pinned = model.successors("ConfinePin", &model.init_state())[0].clone();
    let swapped = model.successors("SwapAncestor", &pinned)[0].clone();
    let rejected = model.successors("ValidateReply", &swapped)[0].clone();
    assert_eq!(rejected["operation"], 0);
    assert_eq!(rejected["effect_target"], 0);
    assert_eq!(rejected["reply"], 2);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    // Path-only confinement (the design before 6aeae4606): every operation
    // resolves the path again, so a swapped ancestor redirects it outside.
    let pinned = buggy.successors("ConfinePin", &buggy.init_state())[0].clone();
    let swapped = buggy.successors("SwapAncestor", &pinned)[0].clone();
    let outside_read = buggy.successors("BuggyReresolveRead", &swapped)[0].clone();
    assert_eq!(
        (outside_read["operation"], outside_read["effect_target"]),
        (1, 2)
    );
    assert!(!buggy.check_invariant("AnchoredAccessNeverOutside", &outside_read));

    let pinned = buggy.successors("ConfinePin", &buggy.init_state())[0].clone();
    let swapped = buggy.successors("SwapAncestor", &pinned)[0].clone();
    let outside_write = buggy.successors("BuggyReresolveWrite", &swapped)[0].clone();
    assert_eq!(
        (outside_write["operation"], outside_write["effect_target"]),
        (2, 2)
    );
    assert!(!buggy.check_invariant("AnchoredAccessNeverOutside", &outside_write));

    // Even an operation that safely used the original pin cannot issue a
    // success reply certifying the path's swapped replacement identity.
    let pinned = buggy.successors("ConfinePin", &buggy.init_state())[0].clone();
    let read = buggy.successors("ReadPinned", &pinned)[0].clone();
    let swapped = buggy.successors("SwapAncestor", &read)[0].clone();
    let false_reply = buggy.successors("BuggyCertifySwapped", &swapped)[0].clone();
    assert_eq!(false_reply["reply"], 1);
    assert_eq!(false_reply["certified_identity"], 2);
    assert!(!buggy.check_invariant("SuccessfulReplyCertifiesOriginal", &false_reply));
    assert!(buggy.check_invariant("AnchoredAccessNeverOutside", &false_reply));

    assert_every_invariant_breaks_first(&model, &["Bounds"]);
}

#[test]
fn derived_artifact_reply_publication_requires_ack_or_quarantine_expiry() {
    let model = artifact_reply_publication_model();
    assert_proves_and_catches(&model);
    let negative_controls = [
        "BuggyAbortRetainsArtifact",
        "BuggyAcceptPreChallengeAck",
        "BuggyChallengeBeforeBody",
        "BuggyDropBeforeWrite",
        "BuggyOkBeforeRevalidation",
        "BuggyPruneLeased",
        "BuggyPublishAfterCancel",
        "BuggyReleaseQuarantineEarly",
        "BuggyReleaseWithoutAck",
        "BuggyTrailerErrorIgnored",
    ];
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &negative_controls),
        Ok(negative_controls.len()),
        "every artifact publication mutant must fire and fail independently"
    );

    let mut acknowledged = model.init_state();
    for action in [
        "AuthorizeCommit",
        "QueueGuard",
        "PrepareWire",
        "WriteWire",
        "AcknowledgePeer",
        "ReleaseGuard",
    ] {
        assert!(
            model.fire(action, &mut acknowledged),
            "{action} must be reachable"
        );
    }
    assert_eq!(acknowledged["phase"], 10);
    assert_eq!(
        (
            acknowledged["reply"],
            acknowledged["challenge"],
            acknowledged["ack"],
            acknowledged["ack_failed"],
            acknowledged["guard"],
        ),
        (1, 1, 1, 0, 0)
    );
    assert!(model.check_invariant("ImmediateReleaseRequiresValidAck", &acknowledged));

    let mut rejected_ack = model.init_state();
    for action in [
        "AuthorizeCommit",
        "QueueGuard",
        "PrepareWire",
        "WriteWire",
        "AcknowledgeFailed",
    ] {
        assert!(
            model.fire(action, &mut rejected_ack),
            "{action} must be reachable"
        );
    }
    assert_eq!(
        (
            rejected_ack["phase"],
            rejected_ack["ack"],
            rejected_ack["ack_failed"],
            rejected_ack["quarantine_age"],
            rejected_ack["guard"],
        ),
        (7, 0, 1, 0, 1)
    );
    assert!(
        !model.action_enabled("ReleaseGuard", &rejected_ack),
        "ACK failure cannot release before quarantine expires"
    );
    assert!(model.fire("AdvanceQuarantine", &mut rejected_ack));
    assert!(model.fire("ExpireQuarantine", &mut rejected_ack));
    assert_eq!(
        (
            rejected_ack["phase"],
            rejected_ack["quarantine_age"],
            rejected_ack["guard"],
        ),
        (8, 1, 1)
    );
    assert!(model.fire("ReleaseGuard", &mut rejected_ack));
    assert_eq!(rejected_ack["phase"], 11);
    assert!(model.check_invariant("QuarantineReleaseRequiresExpiry", &rejected_ack));

    let mut aborted = model.init_state();
    for action in [
        "AuthorizeCommit",
        "QueueGuard",
        "PrepareFailed",
        "ReleaseGuard",
    ] {
        assert!(
            model.fire(action, &mut aborted),
            "{action} must be reachable"
        );
    }
    assert_eq!(
        (
            aborted["phase"],
            aborted["artifact"],
            aborted["committed"],
            aborted["guard"],
        ),
        (12, 0, 0, 0)
    );
    assert!(model.check_invariant("AbortReleaseRemovesUncommittedArtifact", &aborted));

    let mut write_failed = model.init_state();
    for action in [
        "AuthorizeCommit",
        "QueueGuard",
        "PrepareWire",
        "WriteFailed",
    ] {
        assert!(
            model.fire(action, &mut write_failed),
            "{action} must be reachable"
        );
    }
    assert_eq!(
        (
            write_failed["phase"],
            write_failed["artifact"],
            write_failed["committed"],
            write_failed["reply"],
            write_failed["write_error"],
            write_failed["guard"],
        ),
        (7, 1, 1, 0, 1, 1)
    );
    assert!(!model.action_enabled("ReleaseGuard", &write_failed));
    assert!(model.fire("AdvanceQuarantine", &mut write_failed));
    assert!(model.fire("ExpireQuarantine", &mut write_failed));
    assert!(model.fire("ReleaseGuard", &mut write_failed));
    assert_eq!(write_failed["phase"], 11);
    assert!(model.check_invariant("QuarantineReleaseRequiresExpiry", &write_failed));

    let authorized = model.successors("AuthorizeCommit", &model.init_state())[0].clone();
    let queued = model.successors("QueueGuard", &authorized)[0].clone();
    let swept = model.successors("RetentionSweep", &queued)[0].clone();
    assert_eq!(swept["artifact"], 1);
    assert_eq!(swept["guard"], 1);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let authorized = buggy.successors("AuthorizeCommit", &buggy.init_state())[0].clone();
    let queued = buggy.successors("QueueGuard", &authorized)[0].clone();
    let prepared = buggy.successors("PrepareWire", &queued)[0].clone();
    let written = buggy.successors("WriteWire", &prepared)[0].clone();
    let silent_release = buggy.successors("BuggyReleaseWithoutAck", &written)[0].clone();
    assert_eq!(
        (
            silent_release["phase"],
            silent_release["ack"],
            silent_release["ack_failed"],
            silent_release["guard"],
        ),
        (10, 0, 0, 0)
    );
    assert!(
        !buggy.check_invariant("ImmediateReleaseRequiresValidAck", &silent_release),
        "negative control: only a valid nonce ACK permits immediate release"
    );

    let prechallenge_ack = buggy.successors("BuggyAcceptPreChallengeAck", &prepared)[0].clone();
    assert_eq!(
        (
            prechallenge_ack["phase"],
            prechallenge_ack["reply"],
            prechallenge_ack["challenge"],
            prechallenge_ack["ack"],
        ),
        (4, 0, 0, 1)
    );
    assert!(
        !buggy.check_invariant("SuccessfulAckRequiresCausalChallenge", &prechallenge_ack),
        "negative control: a pre-pipelined ACK cannot count before the nonce challenge"
    );

    let quarantined = buggy.successors("AcknowledgeFailed", &written)[0].clone();
    let early_release = buggy.successors("BuggyReleaseQuarantineEarly", &quarantined)[0].clone();
    assert_eq!(
        (
            early_release["phase"],
            early_release["quarantine_age"],
            early_release["guard"],
        ),
        (11, 0, 0)
    );
    assert!(
        !buggy.check_invariant("QuarantineReleaseRequiresExpiry", &early_release),
        "negative control: failed/half-closed clients retain the guard for the full quarantine"
    );

    let pruned = buggy.successors("BuggyPruneLeased", &queued)[0].clone();
    assert_eq!(pruned["artifact"], 0);
    assert!(
        !buggy.check_invariant("LeasedArtifactSurvivesRetention", &pruned),
        "negative control: pruning a leased queued artifact violates the model"
    );

    // The trailer written ahead of the body: an echo of it no longer proves
    // the client read the complete frame.
    let early_challenge = buggy.successors("BuggyChallengeBeforeBody", &prepared)[0].clone();
    assert_eq!(
        (
            early_challenge["phase"],
            early_challenge["reply"],
            early_challenge["challenge"]
        ),
        (4, 0, 1)
    );
    assert!(
        !buggy.check_invariant("ChallengeRequiresCompleteWire", &early_challenge),
        "negative control: the nonce challenge follows the complete reply frame"
    );

    // The OK body out before the guard revalidated, which then failed: the
    // client read OK for a file the abort is about to remove.
    let ok_then_abort = buggy.successors("BuggyOkBeforeRevalidation", &queued)[0].clone();
    assert_eq!((ok_then_abort["phase"], ok_then_abort["committed"]), (9, 1));
    assert!(
        !buggy.check_invariant("CommitRequiresWirePreparation", &ok_then_abort),
        "negative control: no OK byte precedes the reply-time revalidation"
    );

    // The trailer's write error dropped: the frame awaits an ACK to a
    // challenge that never went out, instead of entering quarantine.
    let partial = buggy.successors("BuggyTrailerErrorIgnored", &prepared)[0].clone();
    assert_eq!(
        (partial["phase"], partial["reply"], partial["challenge"]),
        (5, 0, 0)
    );
    assert!(
        !buggy.check_invariant("CompleteReplyPrecedesAck", &partial),
        "negative control: only a complete frame awaits its ACK"
    );

    // An abort whose drop loses the uncommitted `remove_exact` arm.
    let aborting = buggy.successors("AbortQueued", &queued)[0].clone();
    let orphaned = buggy.successors("BuggyAbortRetainsArtifact", &aborting)[0].clone();
    assert_eq!(
        (orphaned["phase"], orphaned["artifact"], orphaned["guard"]),
        (12, 1, 0)
    );
    assert!(
        !buggy.check_invariant("AbortReleaseRemovesUncommittedArtifact", &orphaned),
        "negative control: a pre-wire abort leaves no unpublished file behind"
    );
    assert!(
        buggy.check_invariant("LeasedArtifactSurvivesRetention", &orphaned),
        "the orphaned file is the abort law's alone; retention does not own phase 12"
    );

    let mut cancelled = model.init_state();
    assert!(model.fire("Cancel", &mut cancelled));
    assert_eq!((cancelled["artifact"], cancelled["reply"]), (0, 0));
    assert!(model.check_invariant("CancelledPublishesNothing", &cancelled));

    assert_every_invariant_breaks_first(&model, &["Bounds"]);
}

#[test]
fn derived_artifact_handoff_capacity_refuses_overbooking() {
    let model = artifact_handoff_capacity_model();
    assert_proves_and_catches(&model);

    let one = model.successors("Acquire", &model.init_state())[0].clone();
    let grown = model.successors("ReconcileGrow", &one)[0].clone();
    assert_eq!(
        (
            grown["live"],
            grown["descriptor_units"],
            grown["charge_two"],
            grown["charge_three"],
            grown["selected"],
        ),
        (1, 3, 0, 1, 3)
    );
    let grown_release = model.successors("Release", &grown)[0].clone();
    assert_eq!(
        (grown_release["live"], grown_release["descriptor_units"]),
        (0, 0),
        "release returns the reconciled exact charge"
    );
    let shrunk = model.successors("ReconcileShrink", &one)[0].clone();
    assert_eq!(
        (
            shrunk["live"],
            shrunk["descriptor_units"],
            shrunk["charge_one"],
            shrunk["charge_two"],
            shrunk["selected"],
        ),
        (1, 1, 1, 0, 1)
    );
    let shrunk_release = model.successors("Release", &shrunk)[0].clone();
    assert_eq!(
        (shrunk_release["live"], shrunk_release["descriptor_units"]),
        (0, 0),
        "a charge-one permit remains releasable"
    );

    let mixed = model.successors("Acquire", &grown)[0].clone();
    assert_eq!(
        (
            mixed["live"],
            mixed["descriptor_units"],
            mixed["charge_two"],
            mixed["charge_three"],
            mixed["selected"],
        ),
        (2, 5, 1, 1, 2),
        "a later acquisition must not overwrite the earlier permit's charge"
    );
    let ordinary_first = model.successors("Release", &mixed)[0].clone();
    let select_grown = model.successors("SelectThree", &ordinary_first)[0].clone();
    let all_released = model.successors("Release", &select_grown)[0].clone();
    assert_eq!(
        (all_released["live"], all_released["descriptor_units"]),
        (0, 0),
        "mixed charges release without orphaning units"
    );
    let select_grown = model.successors("SelectThree", &mixed)[0].clone();
    let grown_first = model.successors("Release", &select_grown)[0].clone();
    let select_ordinary = model.successors("SelectTwo", &grown_first)[0].clone();
    let reverse_released = model.successors("Release", &select_ordinary)[0].clone();
    assert_eq!(
        (
            reverse_released["live"],
            reverse_released["descriptor_units"],
        ),
        (0, 0),
        "the opposite mixed-charge release order is also balanced"
    );

    let one_and_two = model.successors("Acquire", &shrunk)[0].clone();
    let one_and_two_and_two = model.successors("Acquire", &one_and_two)[0].clone();
    let selected_one = model.successors("SelectOne", &one_and_two_and_two)[0].clone();
    assert_eq!(
        (
            selected_one["live"],
            selected_one["descriptor_units"],
            selected_one["charge_one"],
            selected_one["charge_two"],
            selected_one["selected"],
        ),
        (3, 5, 1, 2, 1)
    );
    assert_eq!(
        model.successors("ReconcileGrow", &selected_one)[0]["descriptor_units"],
        6,
        "one unit of headroom is real"
    );
    assert_eq!(
        model.successors("RefuseReconcile", &selected_one)[0],
        selected_one,
        "a direct one-to-three jump is refused atomically despite that headroom"
    );

    let mut state = model.init_state();
    for _ in 0..3 {
        state = model.successors("Acquire", &state)[0].clone();
    }
    assert_eq!((state["live"], state["descriptor_units"]), (3, 6));
    assert!(model.successors("Acquire", &state).is_empty());
    assert_eq!(
        model.successors("RefuseAtCap", &state)[0]["live"],
        3,
        "descriptor capacity binds before the count cap"
    );
    assert_eq!(
        model.successors("RefuseReconcile", &state)[0]["descriptor_units"],
        6
    );
    assert_eq!(model.successors("Release", &state)[0]["live"], 2);

    let mutants = [
        "BuggyOverbook",
        "BuggyReconcileOverbook",
        "BuggyRefuseLeaksSlot",
        "BuggyReleaseProvisionalCharge",
    ];
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &mutants),
        Ok(mutants.len()),
        "every admission and release slip must fire and fail on its own"
    );
    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    // A refusal at the unit cap that took the count slot first and never gave
    // it back: three permits, four slots charged.
    let leaked = buggy.successors("BuggyRefuseLeaksSlot", &state)[0].clone();
    assert_eq!((leaked["live"], leaked["descriptor_units"]), (4, 6));
    assert!(!buggy.check_invariant("CountMatchesCharges", &leaked));

    // A reconciled permit whose drop returns its provisional charge strands the
    // unit reconciliation added.
    let stale = buggy.successors("BuggyReleaseProvisionalCharge", &grown)[0].clone();
    assert_eq!((stale["live"], stale["descriptor_units"]), (0, 1));
    assert!(buggy.check_invariant("CountMatchesCharges", &stale));
    assert!(!buggy.check_invariant("UnitsMatchCharges", &stale));

    assert_every_invariant_breaks_first(&model, &[]);
}

#[test]
fn derived_video_batch_publication_requires_current_directory_barrier() {
    let model = video_batch_publication_durability_model();
    assert_proves_and_catches(&model);
    let mutants = ["BuggyPublishBeforeSync", "BuggySyncAhead"];
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &mutants),
        Ok(mutants.len()),
        "the pre-barrier marker and the run-ahead barrier must each be reachable and \
         violate an ordering invariant on their own"
    );
    assert_every_invariant_carries_a_mutant(&model, &["Bounds"]);

    // The run-ahead barrier, pinned: synced at one member, it claims the whole
    // batch, and once the second member lands the counts agree — the marker
    // then publishes over a member written AFTER the barrier, which only the
    // coverage law refuses.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut ahead = buggy.init_state();
    assert!(buggy.fire("WriteMember", &mut ahead));
    assert!(buggy.fire("BuggySyncAhead", &mut ahead));
    assert_eq!((ahead["members"], ahead["synced_members"]), (1, 2));
    assert!(!buggy.check_invariant("BarrierCoversOnlyWrittenMembers", &ahead));
    assert!(buggy.fire("WriteMember", &mut ahead));
    assert!(buggy.fire("PublishMarker", &mut ahead));
    assert!(buggy.check_invariant("MarkerCoversEveryMember", &ahead));

    let mut state = model.init_state();
    assert!(model.fire("WriteMember", &mut state));
    assert_eq!((state["members"], state["synced_members"]), (1, 0));
    assert!(
        !model.action_enabled("PublishMarker", &state),
        "a file-synced member alone cannot make the batch visible"
    );

    assert!(model.fire("SyncBatch", &mut state));
    assert_eq!((state["members"], state["synced_members"]), (1, 1));
    assert!(model.fire("WriteMember", &mut state));
    assert_eq!((state["members"], state["synced_members"]), (2, 1));
    assert!(
        !model.action_enabled("PublishMarker", &state),
        "a later member invalidates an earlier directory barrier"
    );

    assert!(model.fire("SyncBatch", &mut state));
    assert!(model.fire("PublishMarker", &mut state));
    assert_eq!(
        (state["members"], state["synced_members"], state["marker"]),
        (2, 2, 1)
    );
    assert!(model.check_invariant("MarkerCoversEveryMember", &state));
}

#[test]
fn derived_artifact_reader_lease_sweeps_only_after_final_release() {
    let model = artifact_reader_lease_model();
    assert_proves_and_catches(&model);
    let negative_controls = [
        "BuggyAcquireDuringSweep",
        "BuggyAcquireReplacedIdentity",
        "BuggyArmedReleaseDropsEntry",
        "BuggyFinishKeepsEntry",
        "BuggyReleaseSweepsUnarmed",
        "BuggyStartSweepEarly",
    ];
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &negative_controls),
        Ok(negative_controls.len()),
        "every refcount, sweep, and identity mutant must fire and fail independently"
    );

    let mut state = model.init_state();
    assert!(model.fire("Acquire", &mut state));
    assert!(model.fire("Acquire", &mut state));
    assert!(model.fire("Arm", &mut state));
    assert_eq!((state["leases"], state["armed"]), (2, 1));

    assert!(model.fire("Release", &mut state));
    assert_eq!(
        (state["leases"], state["pending"], state["sweeping"]),
        (1, 0, 0),
        "the first of two readers cannot start the convergence sweep"
    );
    assert!(!model.action_enabled("StartSweep", &state));

    assert!(model.fire("Release", &mut state));
    assert_eq!(
        (state["leases"], state["pending"], state["sweeping"]),
        (0, 1, 0),
        "the final release schedules exactly one capability-bound sweep"
    );
    assert!(model.fire("RejectAcquireWhileSweeping", &mut state));
    assert_eq!(state["leases"], 0);
    assert!(model.fire("StartSweep", &mut state));
    assert_eq!(
        (
            state["pending"],
            state["sweeping"],
            state["admission_spent"]
        ),
        (0, 1, 1),
        "the sweep takes the entry's reserved admission"
    );
    assert!(model.fire("RejectAcquireWhileSweeping", &mut state));
    assert_eq!(state["leases"], 0);
    assert!(model.fire("FinishSweep", &mut state));
    assert_eq!(
        (
            state["leases"],
            state["armed"],
            state["requested"],
            state["pending"],
            state["sweeping"],
            state["admission_spent"],
        ),
        (0, 0, 0, 0, 0, 0)
    );
    assert!(model.fire("Acquire", &mut state));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let acquired = buggy.successors("Acquire", &buggy.init_state())[0].clone();
    let armed = buggy.successors("Arm", &acquired)[0].clone();
    let early = buggy.successors("BuggyStartSweepEarly", &armed)[0].clone();
    assert!(
        !buggy.check_invariant("MaintenanceExcludesLeases", &early),
        "negative control: maintenance cannot begin with a live lease"
    );

    let mut pending = model.init_state();
    assert!(model.fire("Acquire", &mut pending));
    assert!(model.fire("Arm", &mut pending));
    assert!(model.fire("Release", &mut pending));
    let pending = aterm_spec::interp::with_buggy(&model, 1)
        .successors("BuggyAcquireDuringSweep", &pending)[0]
        .clone();
    assert!(
        !buggy.check_invariant("MaintenanceExcludesLeases", &pending),
        "negative control: a new lease cannot enter the last-release/sweep interval"
    );

    let mut replaced = model.init_state();
    assert!(model.fire("Acquire", &mut replaced));
    assert!(model.fire("ReplaceIdentity", &mut replaced));
    assert!(model.fire("RejectReplacedIdentity", &mut replaced));
    assert!(!model.action_enabled("Acquire", &replaced));
    let joined = buggy.successors("BuggyAcquireReplacedIdentity", &replaced)[0].clone();
    assert!(
        !buggy.check_invariant("ReplacementNeverJoinsLeaseGroup", &joined),
        "negative control: a replacement identity cannot join a live lease group"
    );

    // The last release of a lease nothing armed schedules a sweep anyway.
    let unarmed = buggy.successors("Acquire", &buggy.init_state())[0].clone();
    let stranded = buggy.successors("BuggyReleaseSweepsUnarmed", &unarmed)[0].clone();
    assert_eq!(
        (stranded["leases"], stranded["armed"], stranded["pending"]),
        (0, 0, 1)
    );
    assert!(!buggy.action_enabled("StartSweep", &stranded));
    assert!(!buggy.check_invariant("MaintenanceRequiresArm", &stranded));

    // The armed last release that removes the entry instead of sweeping it:
    // the registry forgets the arm with the entry, and the retention it was
    // armed for never runs. Only the caller's obligation still records it.
    let acquired = buggy.successors("Acquire", &buggy.init_state())[0].clone();
    let armed = buggy.successors("Arm", &acquired)[0].clone();
    let forgotten = buggy.successors("BuggyArmedReleaseDropsEntry", &armed)[0].clone();
    assert_eq!(
        (
            forgotten["leases"],
            forgotten["armed"],
            forgotten["requested"],
            forgotten["pending"]
        ),
        (0, 0, 1, 0)
    );
    assert!(!buggy.check_invariant("RequestedRetentionRunsAtLastRelease", &forgotten));
    assert!(buggy.check_invariant("IdleNameAdmitsReaders", &forgotten));

    // A finished sweep that resets the entry's flags instead of removing it
    // keeps an entry whose admission the sweep spent: every later reader is
    // refused, whether or not a same-name replacement was ever refused.
    let mut plain = model.init_state();
    for action in ["Acquire", "Arm", "Release", "StartSweep"] {
        assert!(model.fire(action, &mut plain), "{action}");
    }
    let kept_plain = buggy.successors("BuggyFinishKeepsEntry", &plain)[0].clone();
    assert_eq!(
        (
            kept_plain["admission_spent"],
            kept_plain["identity_mismatch"]
        ),
        (1, 0)
    );
    assert!(!buggy.action_enabled("Acquire", &kept_plain));
    assert!(!buggy.check_invariant("IdleNameAdmitsReaders", &kept_plain));
    let mut sweeping = model.init_state();
    for action in [
        "Acquire",
        "Arm",
        "ReplaceIdentity",
        "RejectReplacedIdentity",
        "Release",
        "StartSweep",
    ] {
        assert!(model.fire(action, &mut sweeping), "{action}");
    }
    let reopened = model.successors("FinishSweep", &sweeping)[0].clone();
    assert!(model.action_enabled("Acquire", &reopened));
    let kept = buggy.successors("BuggyFinishKeepsEntry", &sweeping)[0].clone();
    assert_eq!(
        (
            kept["admission_spent"],
            kept["armed"],
            kept["identity_mismatch"]
        ),
        (1, 0, 1)
    );
    assert!(!buggy.action_enabled("Acquire", &kept));
    assert!(buggy.check_invariant("RequestedRetentionRunsAtLastRelease", &kept));
    assert!(!buggy.check_invariant("IdleNameAdmitsReaders", &kept));

    assert_every_invariant_breaks_first(&model, &["Bounds"]);
}

/// Beginning a newer fixed-path snapshot invalidates the old generation before
/// it can publish a completion marker. The mutant lets the overtaken worker
/// certify generation-one payload after generation two is current.
#[test]
fn derived_snapshot_generation_commit_proves_and_catches_stale_publish() {
    let model = snapshot_generation_commit_model();
    assert_proves_and_catches(&model);

    let committed = model.successors("CommitCurrent", &model.init_state())[0].clone();
    assert_eq!(committed["payload"], 1);
    assert_eq!(committed["done"], 1);

    let superseded = model.successors("BeginNew", &committed)[0].clone();
    assert_eq!(superseded["latest"], 2);
    assert_eq!(superseded["done"], 0);

    let stale_completion = model.successors("CommitOld", &superseded)[0].clone();
    assert_eq!(stale_completion["payload"], 1);
    assert_eq!(stale_completion["done"], 0);
    assert!(model.check_invariant("CommittedPayloadIsCurrent", &stale_completion));

    let selected = model.successors("SelectCurrent", &stale_completion)[0].clone();
    let current = model.successors("CommitCurrent", &selected)[0].clone();
    assert_eq!(current["payload"], 2);
    assert_eq!(current["done"], 1);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let committed = buggy.successors("CommitCurrent", &buggy.init_state())[0].clone();
    let superseded = buggy.successors("BeginNew", &committed)[0].clone();
    let stale_publish = buggy.successors("CommitOld", &superseded)[0].clone();
    assert_eq!(stale_publish["latest"], 2);
    assert_eq!(stale_publish["payload"], 1);
    assert_eq!(stale_publish["done"], 1);
    assert!(!buggy.check_invariant("CommittedPayloadIsCurrent", &stale_publish));
}

/// A WindowServer photograph is not native-content authority, and neither is a
/// semantic offscreen rerender made after the barrier. The client source must be
/// the exact serial-bound successful PRESENT destination. Its physical size and
/// client-origin offset must both validate before it is stitched under OS chrome.
///
/// Model field `renderer_bound` and decision code 1 retain their historical
/// names for trace compatibility; they mean destination-bound here.
#[test]
fn derived_native_capture_source_proves_and_catches_stale_compositor_pixels() {
    let model = native_capture_source_model();
    assert_proves_and_catches(&model);

    let presented = model.successors("MarkFramePresented", &model.init_state())[0].clone();
    let missing_origin = model.successors("Decide", &presented)[0].clone();
    assert_eq!(
        missing_origin["decision"], 2,
        "a serial-bound destination without validated size/client origin fails closed"
    );
    let validated = model.successors("ValidateGeometry", &presented)[0].clone();
    let captured = model.successors("Decide", &validated)[0].clone();
    assert_eq!(captured["decision"], 1);
    assert_eq!(captured["renderer_bound"], 1);
    assert_eq!(captured["captured"], 1);
    assert_eq!(captured["stale_capture"], 0);

    let geometry_only = model.successors("ValidateGeometry", &model.init_state())[0].clone();
    let failed = model.successors("Decide", &geometry_only)[0].clone();
    assert_eq!(failed["decision"], 2);
    assert_eq!(failed["captured"], 0);
    assert_eq!(failed["failed"], 1);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let coincidentally_current =
        buggy.successors("PromoteOsClient", &buggy.init_state())[0].clone();
    let stale = buggy.successors("Decide", &coincidentally_current)[0].clone();
    assert_eq!(stale["captured"], 1);
    assert_eq!(stale["renderer_bound"], 0);
    assert_eq!(
        stale["stale_capture"], 1,
        "even current-looking OS/offscreen pixels lack destination provenance"
    );
    assert!(!buggy.check_invariant("NoStaleCapture", &stale));
    assert!(!buggy.check_invariant("CaptureUsesRenderer", &stale));
}

/// One-shot destination capture must cross every async ownership phase before
/// publishing pixels; validation/map failures terminate as errors. The mutant
/// publishes a frame from the failed-map edge.
#[test]
fn derived_presented_frame_tap_proves_and_catches_fail_open_map() {
    let model = presented_frame_tap_model();
    assert_proves_and_catches(&model);

    let pending = model.successors("EnqueueValid", &model.init_state())[0].clone();
    assert_eq!(pending["phase"], 1);
    let in_flight = model.successors("StartMap", &pending)[0].clone();
    assert_eq!(in_flight["phase"], 2);
    let complete = model.successors("CompleteMap", &in_flight)[0].clone();
    assert_eq!(complete["phase"], 3);
    assert_eq!(complete["mapped"], 1);
    assert_eq!(complete["result"], 1);

    let rejected = model.successors("RejectEnqueue", &model.init_state())[0].clone();
    assert_eq!(rejected["phase"], 3);
    assert_eq!(rejected["result"], 2);
    assert_eq!(rejected["accepted"], 0);

    let mutants = [
        "BuggyMapErrorPublishesFrame",
        "BuggyRejectReservesSlot",
        "BuggyMapErrorWithoutOutcome",
    ];
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &mutants),
        Ok(mutants.len()),
        "each gate-arm slip must fire and be caught on its own"
    );
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let pending = buggy.successors("EnqueueValid", &buggy.init_state())[0].clone();
    let in_flight = buggy.successors("StartMap", &pending)[0].clone();
    let fail_open = buggy.successors("BuggyMapErrorPublishesFrame", &in_flight)[0].clone();
    assert_eq!(fail_open["result"], 1);
    assert_eq!(fail_open["mapped"], 0);
    assert!(!buggy.check_invariant("SuccessRequiresMappedCopy", &fail_open));

    // The reject arm copied from EnqueueValid reserves the staging buffer with
    // no copy encoded into it.
    let reserved_empty =
        buggy.successors("BuggyRejectReservesSlot", &buggy.init_state())[0].clone();
    assert_eq!(
        (reserved_empty["phase"], reserved_empty["accepted"]),
        (1, 0)
    );
    assert!(!buggy.check_invariant("ReservedPhaseRequiresAcceptedCopy", &reserved_empty));

    // A failed map whose arm forgot its outcome leaves the waiter no error.
    let silent = buggy.successors("BuggyMapErrorWithoutOutcome", &in_flight)[0].clone();
    assert_eq!((silent["phase"], silent["result"]), (3, 0));
    assert!(!buggy.check_invariant("TerminalPhaseHasResult", &silent));
    assert!(buggy.check_invariant("SuccessRequiresMappedCopy", &silent));

    assert_every_invariant_breaks_first(&model, &["ValuesBounded"]);
}

/// Every streaming staging slot is reusable after either map outcome, while an
/// accepted present with impossible metadata increments the honest drop count
/// without reserving a slot. Out-of-order callback arrival 3,1,2 is sorted and
/// the bounded store retains tail 2,3. The mutant leaks the failed slot, silently
/// loses invalid-metadata opportunities, and appends/evicts in callback order.
#[test]
fn derived_video_tap_slot_proves_and_catches_lifecycle_and_ordering_mutants() {
    let model = video_tap_slot_model();
    assert_proves_and_catches(&model);

    let pending = model.successors("Enqueue", &model.init_state())[0].clone();
    let in_flight = model.successors("StartMap", &pending)[0].clone();
    let free = model.successors("MapOk", &in_flight)[0].clone();
    assert_eq!(free["phase"], 0);
    assert_eq!(free["dropped"], 0);

    let failed = model.successors("MapError", &in_flight)[0].clone();
    assert_eq!(failed["phase"], 0);
    assert_eq!((failed["dropped"], failed["errors"]), (1, 1));
    assert_eq!(failed["last_error"], 1);

    let invalid = model.successors("RejectInvalidMetadata", &model.init_state())[0].clone();
    assert_eq!(invalid["phase"], 0);
    assert_eq!(invalid["invalid"], 1);
    assert_eq!(invalid["dropped"], 1);

    let aborted = model.successors("Abort", &pending)[0].clone();
    assert_eq!(aborted["phase"], 0);
    assert_eq!(aborted["dropped"], 1);

    let three = model.successors("HarvestThree", &model.init_state())[0].clone();
    assert_eq!(three["store_first"], 3);
    let one = model.successors("HarvestOne", &three)[0].clone();
    assert_eq!(one["store_first"], 1);
    assert_eq!(one["store_second"], 3);
    let two = model.successors("HarvestTwo", &one)[0].clone();
    assert_eq!(two["store_first"], 2);
    assert_eq!(two["store_second"], 3);
    assert_eq!(two["evicted"], 1);

    let mutants = [
        "BuggyMapErrorLeaksSlot",
        "BuggyRejectInvalidUncounted",
        "BuggyHarvestCallbackOrder",
        "BuggyEvictNewest",
        "BuggyEvictionUnreported",
    ];
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &mutants),
        Ok(mutants.len()),
        "each fail-open slip must fire and be caught on its own"
    );
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let pending = buggy.successors("Enqueue", &buggy.init_state())[0].clone();
    let in_flight = buggy.successors("StartMap", &pending)[0].clone();
    let leaked = buggy.successors("BuggyMapErrorLeaksSlot", &in_flight)[0].clone();
    assert_eq!((leaked["phase"], leaked["last_error"]), (2, 1));
    assert!(!buggy.check_invariant("ErrorResolutionFreesSlot", &leaked));

    let silent = buggy.successors("BuggyRejectInvalidUncounted", &buggy.init_state())[0].clone();
    assert_eq!(silent["invalid"], 1);
    assert_eq!(silent["dropped"], 0);
    assert!(!buggy.check_invariant("InvalidMetadataIsCounted", &silent));
    // Also after an earlier counted loss: a bound like `invalid <= dropped`
    // would read the map error's count as the invalid frame's.
    let failed = buggy.successors("MapError", &in_flight)[0].clone();
    let masked = buggy.successors("BuggyRejectInvalidUncounted", &failed)[0].clone();
    assert_eq!(
        (masked["dropped"], masked["errors"], masked["invalid"]),
        (1, 1, 1)
    );
    assert!(!buggy.check_invariant("InvalidMetadataIsCounted", &masked));

    let three = buggy.successors("HarvestThree", &buggy.init_state())[0].clone();
    let callback_ordered = buggy.successors("BuggyHarvestCallbackOrder", &three)[0].clone();
    assert_eq!(
        (
            callback_ordered["store_first"],
            callback_ordered["store_second"]
        ),
        (3, 1)
    );
    assert!(!buggy.check_invariant("HarvestedStoreSorted", &callback_ordered));

    // Both eviction slips start from the correctly sorted `1,3`, so each is the
    // first thing its own law sees.
    let one = buggy.successors("HarvestOne", &three)[0].clone();
    let newest_dropped = buggy.successors("BuggyEvictNewest", &one)[0].clone();
    assert_eq!(
        (
            newest_dropped["store_first"],
            newest_dropped["store_second"],
            newest_dropped["evicted"]
        ),
        (1, 2, 1)
    );
    assert!(buggy.check_invariant("EvictionMatchesOverflow", &newest_dropped));
    assert!(!buggy.check_invariant("BudgetKeepsNewestTail", &newest_dropped));
    let unreported = buggy.successors("BuggyEvictionUnreported", &one)[0].clone();
    assert_eq!(
        (
            unreported["store_first"],
            unreported["store_second"],
            unreported["evicted"]
        ),
        (2, 3, 0)
    );
    assert!(buggy.check_invariant("BudgetKeepsNewestTail", &unreported));
    assert!(!buggy.check_invariant("EvictionMatchesOverflow", &unreported));

    assert_every_invariant_breaks_first(&model, &["DropCountBounded", "ValuesBounded"]);
    assert_every_invariant_carries_a_mutant(&model, &["DropCountBounded", "ValuesBounded"]);
}

/// Every capture lifecycle model must participate in the global spec-link,
/// strict-vacuity, and source-anchor closure.
#[test]
fn capture_tap_models_are_registered_for_global_verification() {
    let registered: std::collections::BTreeSet<_> = aterm_spec::xref::model_registry()
        .into_iter()
        .map(|model| model.name)
        .collect();
    for expected in [
        "SnapshotGenerationCommit",
        "VideoRecordingLifecycle",
        "ExactInstanceRetention",
        "AnchoredArtifactTransaction",
        "ArtifactReplyPublication",
        "ArtifactHandoffCapacity",
        "VideoBatchPublicationDurability",
        "ArtifactReaderLease",
        "PresentedFrameTap",
        "VideoTapSlot",
    ] {
        assert!(
            registered.contains(expected),
            "{expected} must resolve through the global spec↔source registry"
        );
    }
}

/// A focused leaf can change cell coordinates without changing the coarse
/// single/composed class (divider drag, zoom transition, or focus move). The
/// retained cursor-effect state must be reset before that frame is eligible to
/// present. The mutant keeps the old coordinate binding.
#[test]
fn derived_layout_coordinate_reset_proves_and_catches_stale_trail() {
    let model = layout_coordinate_reset_model();
    assert_proves_and_catches(&model);

    let charged = model.successors("Charge", &model.init_state())[0].clone();
    assert_eq!(charged["charged"], 1);
    assert_eq!(charged["bound_coordinate"], charged["coordinate"]);

    let moved = model.successors("ChangeCoordinate", &charged)[0].clone();
    assert_eq!(moved["prepared"], 0);
    assert_ne!(moved["bound_coordinate"], moved["coordinate"]);

    let prepared = model.successors("Prepare", &moved)[0].clone();
    assert_eq!(prepared["prepared"], 1);
    assert_eq!(prepared["charged"], 0);
    assert_eq!(prepared["bound_coordinate"], prepared["coordinate"]);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let charged = buggy.successors("Charge", &buggy.init_state())[0].clone();
    let moved = buggy.successors("ChangeCoordinate", &charged)[0].clone();
    let stale = buggy.successors("Prepare", &moved)[0].clone();
    assert_eq!(stale["charged"], 1);
    assert_ne!(stale["bound_coordinate"], stale["coordinate"]);
    assert!(!buggy.check_invariant("PreparedEffectsMatchCoordinate", &stale));
}

/// A reload may leave one old prewarm already running, but its completed result
/// cannot replace the current semantic renderer. The mutant accepts that stale
/// generation and is caught by both decision and installed-generation invariants.
#[test]
fn derived_semantic_prewarm_generation_proves_and_catches_stale_worker() {
    let model = semantic_prewarm_generation_model();
    assert_proves_and_catches(&model);

    let requested = model.successors("Request", &model.init_state())[0].clone();
    let running = model.successors("Start", &requested)[0].clone();
    let reloaded = model.successors("Reload", &running)[0].clone();
    let stale_result = model.successors("Finish", &reloaded)[0].clone();
    let ignored = model.successors("Decide", &stale_result)[0].clone();
    assert_eq!(ignored["decision"], 0);
    assert_eq!(ignored["ready"], 0);
    assert_eq!(ignored["installed"], 0);

    let mutants = [
        "BuggyInstallStaleResult",
        "BuggyIgnoreCurrentResult",
        "BuggyReloadKeepsQueued",
        "BuggyReloadKeepsInstalled",
    ];
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &mutants),
        Ok(mutants.len()),
        "each generation-guard slip must fire and be caught on its own"
    );
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let requested = buggy.successors("Request", &buggy.init_state())[0].clone();
    let running = buggy.successors("Start", &requested)[0].clone();
    let reloaded = buggy.successors("Reload", &running)[0].clone();
    let stale_result = buggy.successors("Finish", &reloaded)[0].clone();
    let installed = buggy.successors("BuggyInstallStaleResult", &stale_result)[0].clone();
    assert!(!buggy.check_invariant("CurrentResultOnly", &installed));
    assert!(!buggy.check_invariant("ReadyGenerationIsCurrent", &installed));

    // A strict guard drops the current generation's own result: nothing
    // installs, so only the decision law sees it.
    let current_result = buggy.successors("Finish", &running)[0].clone();
    let dropped = buggy.successors("BuggyIgnoreCurrentResult", &current_result)[0].clone();
    assert_eq!((dropped["decision"], dropped["ready"]), (0, 0));
    assert!(!buggy.check_invariant("CurrentResultOnly", &dropped));
    assert!(buggy.check_invariant("ReadyGenerationIsCurrent", &dropped));

    // A reload that keeps the installed renderer: the decision was right, but
    // the previous generation's renderer outlives its configuration.
    let installed_current = buggy.successors("Decide", &current_result)[0].clone();
    assert_eq!(installed_current["ready"], 1);
    let outlived = buggy.successors("BuggyReloadKeepsInstalled", &installed_current)[0].clone();
    assert_eq!(
        (
            outlived["current"],
            outlived["installed"],
            outlived["ready"]
        ),
        (2, 1, 1)
    );
    assert!(buggy.check_invariant("CurrentResultOnly", &outlived));
    assert!(!buggy.check_invariant("ReadyGenerationIsCurrent", &outlived));

    // A reload that bumps the generation without `cancel_queued` keeps the
    // obsolete fork queued behind it.
    let requested = buggy.successors("Request", &buggy.init_state())[0].clone();
    let reloaded = buggy.successors("BuggyReloadKeepsQueued", &requested)[0].clone();
    assert_eq!((reloaded["current"], reloaded["queued"]), (2, 1));
    assert!(!buggy.check_invariant("QueueContainsOnlyCurrent", &reloaded));

    assert_every_invariant_breaks_first(&model, &["GenerationsBounded", "FlagsBounded"]);
}

#[test]
fn derived_native_preview_font_convergence_proves_and_catches_lost_ready_frame() {
    let model = aterm_spec::derive::native_preview_font_convergence_model();
    assert_proves_and_catches(&model);
}

#[test]
fn derived_claude_light_admission_proves_and_catches_ignored_rejections() {
    let model = aterm_spec::derive::claude_light_admission_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model, &[]);
}

/// The mode light's return (owner, 2026-09-27: "when I toggle auto-approve,
/// it turns off auto mode (what?!)"): proved at `Buggy=0`, and each defect it
/// was written for is its own law's counterexample at `Buggy=1` — a press
/// from an expected mode, a press into a box or mid-turn, a stop that does
/// not say where. The paths, pinned: from manual, three presses reach the
/// expected mode and nothing presses on; from don't ask, four; a turn in
/// flight holds aterm's own next press (and a box can then open over the
/// session without taking one); a cycle without bypass or auto laps back and
/// is NAMED; a rejected press is named.
#[test]
fn derived_claude_mode_return_proves_and_catches_overshoot_box_and_silent_stop() {
    let model = aterm_spec::derive::claude_mode_return_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_breaks_first(&model, &[]);

    let mut state = model.init_state();
    assert!(model.fire("Click", &mut state));
    for _ in 0..2 {
        assert!(model.fire("Answer", &mut state));
        assert!(model.fire("Press", &mut state));
    }
    assert!(model.fire("Answer", &mut state));
    assert_eq!((state["mode"], state["presses"]), (0, 3));
    assert!(
        model.successors("Press", &state).is_empty(),
        "no press from an expected mode"
    );
    assert!(model.fire("Arrive", &mut state));
    assert!(model.fire("Rest", &mut state));

    // From don't ask: four presses, forward through manual.
    assert!(model.fire("DontAsk", &mut state));
    assert!(model.fire("Click", &mut state));
    for _ in 0..3 {
        assert!(model.fire("Answer", &mut state));
        assert!(model.fire("Press", &mut state));
    }
    assert!(model.fire("Answer", &mut state));
    assert_eq!((state["mode"], state["presses"]), (0, 4));
    assert!(model.fire("Arrive", &mut state));

    // Mid-turn: the click presses, the answer comes, and aterm's own next
    // press waits — a box may open, and takes nothing.
    let mut turn = model.init_state();
    assert!(model.fire("TurnStarts", &mut turn));
    assert!(model.fire("Click", &mut turn));
    assert!(model.fire("Answer", &mut turn));
    assert!(model.successors("Press", &turn).is_empty(), "held mid-turn");
    assert!(model.fire("Cover", &mut turn));
    assert!(model.fire("Expire", &mut turn));
    assert_eq!(turn["named"], 1);

    // A cycle without bypass or auto: back at the start, named.
    let mut ring = model.init_state();
    assert!(model.fire("Narrow", &mut ring));
    assert!(model.fire("Click", &mut ring));
    for _ in 0..2 {
        assert!(model.fire("Answer", &mut ring));
        assert!(model.fire("Press", &mut ring));
    }
    assert!(model.fire("Answer", &mut ring));
    assert_eq!(ring["mode"], ring["start"]);
    assert!(model.fire("Lap", &mut ring));
    assert!(model.check_invariant("AStopIsNamed", &ring));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut stuck = buggy.init_state();
    assert!(buggy.fire("Click", &mut stuck));
    assert!(buggy.fire("Expire", &mut stuck));
    assert!(
        !buggy.check_invariant("AStopIsNamed", &stuck),
        "the retired generic refusal"
    );
    let mut rejected = buggy.init_state();
    assert!(buggy.fire("Click", &mut rejected));
    assert!(buggy.fire("Answer", &mut rejected));
    assert!(buggy.fire("Reject", &mut rejected));
    assert!(
        !buggy.check_invariant("AStopIsNamed", &rejected),
        "the old \"input was not accepted\" stop, after the session had moved"
    );
    let mut raced = buggy.init_state();
    assert!(buggy.fire("TurnStarts", &mut raced));
    assert!(buggy.fire("Click", &mut raced));
    assert!(buggy.fire("Answer", &mut raced));
    assert!(buggy.fire("Press", &mut raced), "a press mid-turn");
    assert!(buggy.fire("Cover", &mut raced));
    assert!(
        !buggy.check_invariant("NoPressIntoABox", &raced),
        "the box opened before Claude read aterm's own key"
    );
}

/// Queue replacement carries the unique renderer base before worker start, and
/// completion requires the latest request/candidate identity. A current failed
/// candidate clears the active renderer rather than retaining mismatched pixels.
#[test]
fn derived_semantic_prewarm_handshake_proves_and_catches_dropped_or_mixed_candidate() {
    let model = semantic_prewarm_handshake_model();
    assert_proves_and_catches(&model);

    let with_base = model.successors("MarkReplacedBase", &model.init_state())[0].clone();
    let carried = model.successors("ResolveReplacement", &with_base)[0].clone();
    assert_eq!(carried["replacement_base"], 1);
    assert!(model.check_invariant("ReplacementCarriesBase", &carried));

    let mut current_failure = model.init_state();
    for action in [
        "MarkGenerationCurrent",
        "MarkRequestCurrent",
        "MarkCandidateCurrent",
        "MarkActiveBeforeLatest",
    ] {
        current_failure = model.successors(action, &current_failure)[0].clone();
    }
    let failed_closed = model.successors("DecideResult", &current_failure)[0].clone();
    assert_eq!(failed_closed["decision"], 3);
    assert_eq!(failed_closed["active_after"], 0);

    let mutants = [
        "BuggyReplacementKeepsOnlyNewBase",
        "BuggyMixedCandidateInstalls",
        "BuggyReadinessBeforeGeneration",
        "BuggyInstallKeepsStaleIdentity",
        "BuggyFailClosedKeepsPrevious",
        "BuggyDropSuperseded",
        "BuggyCacheParksActive",
    ];
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &mutants),
        Ok(mutants.len()),
        "each handshake slip must fire and be caught on its own"
    );
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let with_base = buggy.successors("MarkReplacedBase", &buggy.init_state())[0].clone();
    let dropped = buggy.successors("BuggyReplacementKeepsOnlyNewBase", &with_base)[0].clone();
    assert!(!buggy.check_invariant("ReplacementCarriesBase", &dropped));

    let mut mixed = buggy.init_state();
    for action in ["MarkGenerationCurrent", "MarkRendererReady"] {
        mixed = buggy.successors(action, &mixed)[0].clone();
    }
    let wrongly_installed = buggy.successors("BuggyMixedCandidateInstalls", &mixed)[0].clone();
    assert_eq!(wrongly_installed["decision"], 2);
    assert!(!buggy.check_invariant("DecisionMatchesIdentity", &wrongly_installed));
    assert!(!buggy.check_invariant("InstallOnlyLatestReady", &wrongly_installed));

    // The InstallCurrent arm that forgets the candidate identity: the right
    // renderer paints, under no identity the preview recognises.
    let mut exact_ready = buggy.init_state();
    for action in [
        "MarkGenerationCurrent",
        "MarkRequestCurrent",
        "MarkCandidateCurrent",
        "MarkRendererReady",
    ] {
        exact_ready = buggy.successors(action, &exact_ready)[0].clone();
    }
    let anonymous = buggy.successors("BuggyInstallKeepsStaleIdentity", &exact_ready)[0].clone();
    assert_eq!(
        (
            anonymous["decision"],
            anonymous["active_after"],
            anonymous["active_after_latest"]
        ),
        (2, 1, 0)
    );
    assert!(buggy.check_invariant("DecisionMatchesIdentity", &anonymous));
    assert!(!buggy.check_invariant("InstallOnlyLatestReady", &anonymous));

    // The FailClosedCurrent arm that forgets `self.semantic = None` keeps the
    // previous candidate painting after the exact request failed.
    let mut current_failure = buggy.init_state();
    for action in [
        "MarkGenerationCurrent",
        "MarkRequestCurrent",
        "MarkCandidateCurrent",
        "MarkActiveBeforeLatest",
    ] {
        current_failure = buggy.successors(action, &current_failure)[0].clone();
    }
    let fail_open = buggy.successors("BuggyFailClosedKeepsPrevious", &current_failure)[0].clone();
    assert_eq!((fail_open["decision"], fail_open["active_after"]), (3, 1));
    assert!(!buggy.check_invariant("CurrentFailureFailsClosed", &fail_open));

    // Readiness tested before generation files a renderer forked from an
    // obsolete base in the candidate cache: the classification is wrong, and
    // the cache arm then does what that arm does.
    let stale_ready = buggy.successors("MarkRendererReady", &buggy.init_state())[0].clone();
    let stale_cached = buggy.successors("BuggyReadinessBeforeGeneration", &stale_ready)[0].clone();
    assert_eq!(
        (
            stale_cached["generation_matches"],
            stale_cached["decision"],
            stale_cached["cached"],
        ),
        (0, 4, 1)
    );
    assert!(!buggy.check_invariant("DecisionMatchesIdentity", &stale_cached));
    assert!(buggy.check_invariant("CacheOnlySupersededReady", &stale_cached));

    // The two CacheSuperseded-arm slips, over a live active renderer: one
    // drops the superseded renderer, the other parks the active one.
    let mut superseded = buggy.init_state();
    for action in [
        "MarkGenerationCurrent",
        "MarkRendererReady",
        "MarkActiveBeforeLatest",
    ] {
        superseded = buggy.successors(action, &superseded)[0].clone();
    }
    let uncached = buggy.successors("BuggyDropSuperseded", &superseded)[0].clone();
    assert_eq!((uncached["decision"], uncached["cached"]), (4, 0));
    assert!(!buggy.check_invariant("CacheOnlySupersededReady", &uncached));
    let parked = buggy.successors("BuggyCacheParksActive", &superseded)[0].clone();
    assert_eq!((parked["cached"], parked["active_after"]), (1, 0));
    assert!(buggy.check_invariant("CacheOnlySupersededReady", &parked));
    assert!(!buggy.check_invariant("NoncurrentPreservesActive", &parked));

    assert_every_invariant_breaks_first(&model, &["InputsBounded", "OutputsBounded"]);
}

/// A ready renderer for candidate B becomes cache-only before uncached A starts;
/// otherwise B remains the active paint source under A's pending identity.
#[test]
fn derived_semantic_prewarm_request_swap_proves_and_catches_mixed_active_paint() {
    let model = semantic_prewarm_request_swap_model();
    assert_proves_and_catches(&model);

    let ready_b = model.successors("MarkReadyMismatch", &model.init_state())[0].clone();
    let requesting_a = model.successors("Decide", &ready_b)[0].clone();
    assert_eq!(requesting_a["should_cache"], 1);
    assert_eq!(requesting_a["active_after"], 0);
    assert!(model.check_invariant("MismatchedReadyMovesToCache", &requesting_a));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let ready_b = buggy.successors("MarkReadyMismatch", &buggy.init_state())[0].clone();
    let mixed = buggy.successors("Decide", &ready_b)[0].clone();
    assert_eq!(mixed["should_cache"], 0);
    assert_eq!(mixed["active_after"], 1);
    assert!(!buggy.check_invariant("MismatchedReadyMovesToCache", &mixed));

    assert_every_invariant_breaks_first(&model, &["FlagsBounded"]);
}

/// Every semantic-prewarm race model participates in the global spec-link and
/// strict-vacuity closure, not only its direct Tier-0/Tier-1 test.
#[test]
fn semantic_prewarm_models_are_registered_for_global_verification() {
    let registered: std::collections::BTreeSet<_> = aterm_spec::xref::model_registry()
        .into_iter()
        .map(|model| model.name)
        .collect();
    for expected in [
        "SemanticPrewarmGeneration",
        "SemanticPrewarmHandshake",
        "SemanticPrewarmRequestSwap",
    ] {
        assert!(
            registered.contains(expected),
            "{expected} must resolve through the global spec↔source registry"
        );
    }
}

/// Versioned preference writes accept an unchanged touched key across unrelated
/// edits, reject same-key conflicts, make undo conditional, reset atomically, and
/// mint exactly one revision per write. Mutants blind-overwrite a conflict,
/// expose a half-reset file, or skip the shared revision bump.
#[test]
fn derived_native_config_transaction_proves_and_catches_stale_overwrite() {
    let model = native_config_transaction_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let begun = buggy.successors("BeginPatchA", &buggy.init_state())[0].clone();
    let external = buggy.successors("ExternalAFromOne", &begun)[0].clone();
    let overwritten = buggy.successors("CommitPatchA", &external)[0].clone();
    assert!(!buggy.check_invariant("NoBlindOverwrite", &overwritten));

    let begun = buggy.successors("BeginPatchA", &buggy.init_state())[0].clone();
    let committed = buggy.successors("CommitPatchA", &begun)[0].clone();
    let external = buggy.successors("ExternalAFromZero", &committed)[0].clone();
    let undone = buggy.successors("UndoPatchA", &external)[0].clone();
    assert!(!buggy.check_invariant("NoBlindOverwrite", &undone));

    let partial = buggy.successors("ResetAll", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("AtomicResetVisibility", &partial));

    // Every revision is one write, on every trace: after an external edit
    // (the spare revision the old `accepted <= revision` let a slip hide in),
    // the healthy commit and undo each mint their own.
    let mut counted = model.init_state();
    for action in ["ExternalB", "BeginPatchA", "CommitPatchA", "UndoPatchA"] {
        counted = model.successors(action, &counted)[0].clone();
    }
    assert_eq!(counted["revision"], 3);
    assert_eq!(counted["accepted"] + counted["external_edits"], 3);

    // The shared bump skipped: the same trace publishes its undo under the
    // revision the commit already published.
    let mut unbumped = buggy.init_state();
    for action in ["ExternalB", "BeginPatchA", "CommitPatchA", "UndoPatchA"] {
        unbumped = buggy.successors(action, &unbumped)[0].clone();
    }
    assert_eq!(unbumped["revision"], 1);
    assert_eq!(unbumped["accepted"], 2);
    assert!(!buggy.check_invariant("RevisionCountsWrites", &unbumped));

    assert_every_invariant_carries_a_mutant(&model, &["KeysBounded", "RevisionBounded"]);
}

/// The worker/event-loop config handoff retains exact external generations
/// across failed reconciliation, fences queued writes while authority is
/// unknown, and resamples when a newer watcher edge overtakes a sample.
#[test]
fn derived_native_config_observation_handoff_proves_and_catches_loss() {
    let model = native_config_observation_handoff_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let observed = buggy.successors("ObserveFirst", &buggy.init_state())[0].clone();
    let reconciling = buggy.successors("StartReconcile", &observed)[0].clone();
    let lost = buggy.successors("FailReconcile", &reconciling)[0].clone();
    assert!(!buggy.check_invariant("DeferredGenerationNeverLost", &lost));

    let queued = buggy.successors("QueueWrite", &buggy.init_state())[0].clone();
    let observed = buggy.successors("ObserveFirst", &queued)[0].clone();
    let blind = buggy.successors("StartBlindWrite", &observed)[0].clone();
    assert!(!buggy.check_invariant("UnknownAuthorityFencesWrites", &blind));

    let observed = buggy.successors("ObserveFirst", &buggy.init_state())[0].clone();
    let sampled = buggy.successors("StartReconcile", &observed)[0].clone();
    let overtaken = buggy.successors("ObserveNewer", &sampled)[0].clone();
    let stale = buggy.successors("AdmitStaleSample", &overtaken)[0].clone();
    assert!(!buggy.check_invariant("LatestExactGenerationWins", &stale));
}

/// Three rapid Serious Mode toggles are semantic intents. Each queued intent
/// rebases when it reaches the serialized config head, so ON→OFF→ON completes
/// without a stale expected-value conflict. The mutant captures expectations at
/// enqueue time and conflicts the third toggle after the second reduces.
#[test]
fn derived_serious_mode_intent_queue_proves_and_catches_stale_third_toggle() {
    let model = serious_mode_intent_queue_model();
    assert_proves_and_catches(&model);

    let mut healthy = model.init_state();
    for action in [
        "StartToggle",
        "QueueToggle",
        "QueueToggle",
        "Complete",
        "Complete",
        "Complete",
    ] {
        healthy = model.successors(action, &healthy)[0].clone();
    }
    assert_eq!(healthy["issued"], 3);
    assert_eq!(healthy["completed"], 3);
    assert_eq!(healthy["live"], 1);
    assert_eq!(healthy["service"], 1);
    assert_eq!(healthy["conflict"], 0);
    assert!(model.check_invariant("IdleIsAuthoritative", &healthy));

    let mut mixed = model.init_state();
    for action in ["StartSetOn", "QueueToggle", "Complete", "Complete"] {
        mixed = model.successors(action, &mixed)[0].clone();
    }
    assert_eq!(mixed["issued"], 2);
    assert_eq!(mixed["completed"], 2);
    assert_eq!(mixed["live"], 0);
    assert_eq!(mixed["service"], 0);
    assert_eq!(mixed["projection"], 0);
    assert!(model.check_invariant("ProjectionTracksLatestIntent", &mixed));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut stale = buggy.init_state();
    for action in [
        "StartToggle",
        "QueueToggle",
        "QueueToggle",
        "Complete",
        "Complete",
    ] {
        stale = buggy.successors(action, &stale)[0].clone();
    }
    assert_eq!(stale["conflict"], 1);
    assert!(!buggy.check_invariant("NoSerializedConflict", &stale));
    assert!(!buggy.check_invariant("IdleIsAuthoritative", &stale));

    // ON, then OFF and ON queued: the first completion re-reads the pending
    // queue front-first and shows OFF while ON is the newest intent.
    let mut oldest_wins = buggy.init_state();
    for action in ["StartToggle", "QueueToggle", "QueueToggle", "Complete"] {
        oldest_wins = buggy.successors(action, &oldest_wins)[0].clone();
    }
    assert_eq!(oldest_wins["last_desired"], 1);
    assert_eq!(oldest_wins["projection"], 0);
    assert!(!buggy.check_invariant("ProjectionTracksLatestIntent", &oldest_wins));

    assert_every_invariant_carries_a_mutant(
        &model,
        &["QueueBounded", "IssuedBounded", "ValuesBoolean"],
    );
}

#[test]
fn derived_config_file_commit_cas_proves_and_catches_dual_lane_loss() {
    let model = config_file_commit_cas_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let manual = buggy.successors("BeginManual", &buggy.init_state())[0].clone();
    let locked = buggy.successors("LockManual", &manual)[0].clone();
    let unsynchronized = buggy.successors("ResolveManual", &locked)[0].clone();
    assert!(!buggy.check_invariant("ManualDurableSynchronizesImmediately", &unsynchronized));

    let begun = buggy.successors("BeginSettings", &buggy.init_state())[0].clone();
    let retargeted = buggy.successors("Retarget", &begun)[0].clone();
    let locked = buggy.successors("LockSettings", &retargeted)[0].clone();
    let split = buggy.successors("ResolveSettings", &locked)[0].clone();
    assert!(!buggy.check_invariant("NoSplitTargetCommit", &split));

    // A stable admitted config symlink is a valid capability in the fixed model.
    let stable_symlink = model.successors("BeginSettingsSymlink", &model.init_state())[0].clone();
    let stable_locked = model.successors("LockSettings", &stable_symlink)[0].clone();
    let stable_publish = model.successors("ResolveSettings", &stable_locked)[0].clone();
    assert_eq!(stable_publish["settings_phase"], 3);
    assert_eq!(stable_publish["disk"], 2);
    assert!(model.check_invariant("NoChangedLinkPublication", &stable_publish));

    // Recreating or retargeting that link after capture changes its generation;
    // the mutant's blind publication makes the negative control non-vacuous.
    let symlink = buggy.successors("BeginSettingsSymlink", &buggy.init_state())[0].clone();
    let relinked = buggy.successors("Relink", &symlink)[0].clone();
    let locked = buggy.successors("LockSettings", &relinked)[0].clone();
    let published = buggy.successors("ResolveSettings", &locked)[0].clone();
    assert!(!buggy.check_invariant("NoChangedLinkPublication", &published));

    let begun = model.successors("BeginManual", &model.init_state())[0].clone();
    let locked = model.successors("LockManual", &begun)[0].clone();
    let indeterminate = model.successors("ResolveManualIndeterminate", &locked)[0].clone();
    assert_eq!(indeterminate["manual_phase"], 5);
    assert_eq!(indeterminate["manual_committed"], 0);
    assert!(model.check_invariant("IndeterminateDoesNotClaimDurability", &indeterminate));
    // The mutant books the unverified publication as committed, and then
    // retries it blind.
    let claimed = buggy.successors("ResolveManualIndeterminate", &locked)[0].clone();
    assert_eq!(claimed["manual_phase"], 5);
    assert_eq!(claimed["manual_committed"], 1);
    assert!(!buggy.check_invariant("IndeterminateDoesNotClaimDurability", &claimed));
    let blind_retry = buggy.successors("RetryIndeterminate", &claimed)[0].clone();
    assert!(!buggy.check_invariant("ReconcileBeforeRetry", &blind_retry));

    let mut same_base = buggy.init_state();
    for action in [
        "BeginManual",
        "BeginSettings",
        "LockManual",
        "ResolveManual",
        "LockSettings",
        "ResolveSettings",
    ] {
        same_base = buggy.successors(action, &same_base)[0].clone();
    }
    assert!(!buggy.check_invariant("SameBaselineHasOneWinner", &same_base));
    assert!(!buggy.check_invariant("NoStalePublication", &same_base));

    assert_every_invariant_carries_a_mutant(&model, &["Bounded"]);
}

#[test]
fn derived_config_catalog_snapshot_proves_and_catches_split_generation() {
    let model = config_catalog_snapshot_model();
    assert_proves_and_catches(&model);

    let refreshed = model.successors("RefreshAssets", &model.init_state())[0].clone();
    assert!(model.check_invariant("SnapshotAtomic", &refreshed));
    assert_eq!(refreshed["revision"], 1);
    assert_eq!(refreshed["trail_generation"], 1);
    assert_eq!(refreshed["theme_generation"], 1);
    assert_eq!(refreshed["sparkle_generation"], 1);
    assert_eq!(refreshed["asset_refresh"], 1);

    let theme_refreshed = model.successors("RefreshThemes", &model.init_state())[0].clone();
    assert!(model.check_invariant("SnapshotAtomic", &theme_refreshed));
    assert_eq!(theme_refreshed["theme_generation"], 1);
    assert_eq!(theme_refreshed["asset_refresh"], 2);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let stale_trail = buggy.successors("AdmitStaleTrail", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("SnapshotAtomic", &stale_trail));
    assert_eq!(stale_trail["trail_generation"], 0);
    assert_eq!(stale_trail["kitty_generation"], 1);
    assert_eq!(stale_trail["theme_generation"], 1);
    assert_eq!(stale_trail["sparkle_generation"], 1);

    let stale_kitty = buggy.successors("AdmitStaleKitty", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("SnapshotAtomic", &stale_kitty));
    assert_eq!(stale_kitty["trail_generation"], 1);
    assert_eq!(stale_kitty["kitty_generation"], 0);

    let stale_theme = buggy.successors("AdmitStaleTheme", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("SnapshotAtomic", &stale_theme));
    assert_eq!(stale_theme["trail_generation"], 1);
    assert_eq!(stale_theme["kitty_generation"], 1);
    assert_eq!(stale_theme["theme_generation"], 0);

    let stale_sparkle = buggy.successors("AdmitStaleSparkle", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("SnapshotAtomic", &stale_sparkle));
    assert_eq!(stale_sparkle["trail_generation"], 1);
    assert_eq!(stale_sparkle["kitty_generation"], 1);
    assert_eq!(stale_sparkle["theme_generation"], 1);
    assert_eq!(stale_sparkle["sparkle_generation"], 0);

    // The live host runs a generation the service never admitted.
    let unadmitted = buggy.successors("PublishLiveUnadmitted", &buggy.init_state())[0].clone();
    assert_eq!(unadmitted["live_generation"], unadmitted["revision"] + 1);
    assert!(buggy.check_invariant("SnapshotAtomic", &unadmitted));
    assert!(!buggy.check_invariant("ViewsNeverAhead", &unadmitted));

    assert_every_invariant_carries_a_mutant(&model, &["RevisionBounded"]);
}

#[test]
fn derived_composite_accessibility_route_proves_and_catches_wrong_or_stale_owner() {
    let model = composite_accessibility_route_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let published = buggy.successors("Publish", &buggy.init_state())[0].clone();
    let requested_first = buggy.successors("RequestOne", &published)[0].clone();
    let cross_routed = buggy.successors("Route", &requested_first)[0].clone();
    assert!(!buggy.check_invariant("NoCrossViewDispatch", &cross_routed));
    assert_eq!(cross_routed["target_owner"], 1);
    assert_eq!(cross_routed["dispatched_owner"], 2);

    let advanced_second = buggy.successors("AdvanceTwo", &published)[0].clone();
    let requested_second = buggy.successors("RequestTwo", &advanced_second)[0].clone();
    let stale_routed = buggy.successors("Route", &requested_second)[0].clone();
    assert!(!buggy.check_invariant("NoStaleGenerationDispatch", &stale_routed));
    assert_eq!(stale_routed["target_generation"], 1);
    assert_eq!(stale_routed["owner_two_generation"], 2);

    assert_every_invariant_carries_a_mutant(&model, &["GenerationsBounded", "OwnerDomain"]);
}

/// A shared document commit advances canonical text, immutable snapshot, both
/// controllers, and selection anchors together. Mutants accept a stale base or
/// publish the new sequence only to Editor.
#[test]
fn derived_native_document_publication_proves_and_catches_partial_publish() {
    let model = native_document_publication_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let begun = buggy.successors("BeginTxn", &buggy.init_state())[0].clone();
    let partial = buggy.successors("CommitClean", &begun)[0].clone();
    assert!(!buggy.check_invariant("PublishIsAtomic", &partial));

    assert!(!buggy.check_invariant("MarkdownCurrent", &partial));
    assert!(!buggy.check_invariant("SnapshotCurrent", &partial));
    assert!(buggy.check_invariant("EditorCurrent", &partial));

    let begun = buggy.successors("BeginTxn", &buggy.init_state())[0].clone();
    let concurrent = buggy.successors("OtherCommit", &begun)[0].clone();
    let stale = buggy.successors("CommitClean", &concurrent)[0].clone();
    assert!(!buggy.check_invariant("StaleTxnIsNoOp", &stale));

    // The other controller's commit, published only to its author: the Editor
    // neither observes it nor rebases its anchor, and the snapshot stays old.
    let foreign = buggy.successors("OtherCommit", &buggy.init_state())[0].clone();
    assert_eq!(foreign["markdown_seen"], foreign["edit_seq"]);
    assert!(!buggy.check_invariant("EditorCurrent", &foreign));
    assert!(!buggy.check_invariant("AnchorsTransformed", &foreign));
    assert!(!buggy.check_invariant("SnapshotCurrent", &foreign));

    assert_every_invariant_carries_a_mutant(&model, &["SequenceBounded"]);
}

/// Watch observations defer behind an in-flight save, rebind byte-equivalent
/// disk generations without touching a draft, then preserve dirty local bytes
/// ahead of a clean reload. The dirty-first mutant is observable when a higher
/// priority fact and dirty are both true.
#[test]
fn derived_native_file_watch_proves_and_catches_priority_inversion() {
    let model = native_file_watch_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let changed = buggy.successors("ObserveChange", &buggy.init_state())[0].clone();
    let dirty = buggy.successors("MarkDirty", &changed)[0].clone();
    let equivalent = buggy.successors("MarkEquivalent", &dirty)[0].clone();
    let inverted = buggy.successors("Resolve", &equivalent)[0].clone();
    assert!(!buggy.check_invariant("PriorityIsDeterministic", &inverted));
}

/// Repeated identical watcher failures emit one warning, preserve the admitted
/// catalog, and only a successful theme observation or the newest exact config
/// admission clears it. The mutant also lets stale config generation one clear
/// a warning after generation two has become current.
#[test]
fn derived_watcher_failure_recovery_proves_and_catches_duplicate_or_hidden_failure() {
    let model = watcher_failure_recovery_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let failed = buggy.successors("ObserveFailure", &buggy.init_state())[0].clone();
    let repeated = buggy.successors("RepeatFailure", &failed)[0].clone();
    assert!(!buggy.check_invariant("FailureStatusExact", &repeated));
    assert!(!buggy.check_invariant("FailureWakeDeduped", &repeated));
    assert!(!buggy.check_invariant("FailedPollRetainsCatalog", &repeated));

    let failed = buggy.successors("ObserveFailure", &buggy.init_state())[0].clone();
    let first = buggy.successors("ObserveCandidateOne", &failed)[0].clone();
    let second = buggy.successors("ObserveCandidateTwo", &first)[0].clone();
    let stale = buggy.successors("AdmitCandidateOne", &second)[0].clone();
    assert!(!buggy.check_invariant("ConfigRecoveryAdmitsLatest", &stale));
}

/// Draft fsync completions are generation-exact and a journal baseline is
/// pruned/rebased only after the corresponding atomic file-save proof.
#[test]
fn derived_native_draft_journal_proves_and_catches_stale_or_unsafe_prune() {
    let model = native_draft_journal_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let edited = buggy.successors("Edit", &buggy.init_state())[0].clone();
    let unsafe_checkpoint = buggy.successors("BeginCheckpoint", &edited)[0].clone();
    assert!(!buggy.check_invariant("PruneOnlyAfterFileDurable", &unsafe_checkpoint));
    assert!(!buggy.check_invariant("NoUnsafePrune", &unsafe_checkpoint));

    let journal = buggy.successors("BeginJournal", &edited)[0].clone();
    let journaled = buggy.successors("AcceptJournal", &journal)[0].clone();
    let saved = buggy.successors("ProveFileSave", &journaled)[0].clone();
    let checkpoint = buggy.successors("BeginCheckpoint", &saved)[0].clone();
    let stale = buggy.successors("RejectStaleProof", &checkpoint)[0].clone();
    assert!(!buggy.check_invariant("StaleProofIsNoOp", &stale));

    let journal = buggy.successors("BeginJournal", &edited)[0].clone();
    let external = buggy.successors("ExternalJournalCommit", &journal)[0].clone();
    let wrong_image = buggy.successors("AcceptJournal", &external)[0].clone();
    assert!(!buggy.check_invariant("JournalImageCas", &wrong_image));
}

#[test]
fn derived_restore_manifest_claim_is_durable_single_use_and_unique() {
    let model = restore_manifest_single_use_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let locked = buggy.successors("LockTakeA", &buggy.init_state())[0].clone();
    let claimed = buggy.successors("ClaimA", &locked)[0].clone();
    let unsafe_return = buggy.successors("ReturnUnsynced", &claimed)[0].clone();
    assert!(!buggy.check_invariant("ReturnOnlyAfterDurableClaim", &unsafe_return));

    let writer = buggy.successors("LockWriter", &buggy.init_state())[0].clone();
    let alias = buggy.successors("ReuseFixedTemporary", &writer)[0].clone();
    assert!(!buggy.check_invariant("UniqueTemporaryNeverAliases", &alias));

    // The pre-3473ced57 take: no lock, read, remove, return. A has read and
    // not yet removed, so the name is still visible...
    let read = buggy.successors("HistoricalTakeA", &buggy.init_state())[0].clone();
    assert_eq!(read["visible"], 1);
    assert!(!buggy.check_invariant("ClaimRemovesVisibleName", &read));
    // ...and B reads in that window: A removes and returns, B's remove finds
    // nothing, and B returns the same manifest.
    let mut twice = read.clone();
    for action in ["HistoricalTakeB", "HistoricalTakeA", "HistoricalTakeB"] {
        twice = buggy.successors(action, &twice)[0].clone();
    }
    assert_eq!(twice["visible"], 0);
    assert_eq!(twice["returned"], 2);
    assert!(!buggy.check_invariant("AtMostOneConsumer", &twice));
    // A lone historical taker consumes once, and a second one that starts
    // after it removed the name finds nothing: the defect is the interleaving.
    let alone = buggy.successors("HistoricalTakeA", &read)[0].clone();
    assert_eq!(alone["returned"], 1);
    assert!(buggy.successors("HistoricalTakeB", &alone).is_empty());
    for mutant in [
        "ReturnUnsynced",
        "ReuseFixedTemporary",
        "HistoricalTakeA",
        "HistoricalTakeB",
    ] {
        assert!(
            !aterm_spec::interp::fired_actions(&model).contains(mutant),
            "{mutant} is dead in the healthy machine"
        );
    }

    assert_every_invariant_carries_a_mutant(&model, &["OwnerBounded", "FlagsBounded"]);
}

/// Final-view close freezes the requested sequence and detaches no split leaf
/// until durability and every leaf's readiness agree. The mutant detaches early.
#[test]
fn derived_native_close_plan_proves_and_catches_partial_detach() {
    let model = native_close_plan_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let one_view = buggy.successors("CloseMarkdownNonFinal", &buggy.init_state())[0].clone();
    let detached = buggy.successors("BeginFinalClose", &one_view)[0].clone();
    assert!(!buggy.check_invariant("AtomicTreeClose", &detached));

    // An edit admitted while Closing moves the head past the frozen request.
    let edited = buggy.successors("Edit", &detached)[0].clone();
    assert_eq!(edited["phase"], 1);
    assert!(!buggy.check_invariant("FrozenFinalSequence", &edited));

    // A dirty final close acknowledged by an older save closes below its
    // request.
    let mut stale_ack = model.successors("Edit", &model.init_state())[0].clone();
    for action in ["CloseMarkdownNonFinal", "BeginFinalClose", "ReadyOtherLeaf"] {
        stale_ack = model.successors(action, &stale_ack)[0].clone();
    }
    for action in ["AckCheckpoint", "CommitClose"] {
        stale_ack = buggy.successors(action, &stale_ack)[0].clone();
    }
    assert_eq!(stale_ack["phase"], 3);
    assert!(stale_ack["checkpoint_seq"] < stale_ack["requested_seq"]);
    assert!(buggy.check_invariant("AtomicTreeClose", &stale_ack));
    assert!(!buggy.check_invariant("NoSilentLoss", &stale_ack));

    assert_every_invariant_carries_a_mutant(&model, &["SequenceBounded"]);
}

/// A completion with a newer document-owned Save/close intent either pumps the
/// next generation or atomically resolves the chain. It cannot publish a final
/// Saved state or leave a close plan idle below its frozen sequence.
#[test]
fn derived_native_save_intent_latch_proves_and_catches_dropped_completion_pump() {
    let model = native_save_intent_latch_model();
    assert_proves_and_catches(&model);

    let mut healthy = model.init_state();
    for action in [
        "Edit",
        "BeginSave",
        "Edit",
        "BeginCloseInflight",
        "CompleteAndPump",
        "CompleteFinal",
        "CommitClose",
    ] {
        healthy = model.successors(action, &healthy)[0].clone();
    }
    assert_eq!(healthy["durable"], 2);
    assert_eq!(healthy["closed"], 1);
    assert_eq!(healthy["settled"], 1);
    assert!(model.check_invariant("SettledCoversLatestRequest", &healthy));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut dropped = buggy.init_state();
    for action in [
        "Edit",
        "BeginSave",
        "Edit",
        "BeginCloseInflight",
        "CompleteAndPump",
    ] {
        dropped = buggy.successors(action, &dropped)[0].clone();
    }
    assert!(!buggy.check_invariant("SettledCoversLatestRequest", &dropped));
    assert!(!buggy.check_invariant("WaitingCloseHasCompletionPump", &dropped));

    // A close committed the moment it is armed, before any checkpoint covers
    // its frozen sequence.
    let mut armed = model.init_state();
    for action in ["Edit", "BeginSave", "Edit", "BeginCloseInflight"] {
        armed = model.successors(action, &armed)[0].clone();
    }
    assert!(model.successors("CommitClose", &armed).is_empty());
    let early = buggy.successors("CommitClose", &armed)[0].clone();
    assert_eq!(early["closed"], 1);
    assert!(early["durable"] < early["close_seq"]);
    assert!(!buggy.check_invariant("ClosedSequenceIsDurable", &early));

    assert_every_invariant_carries_a_mutant(&model, &["SequenceBounded"]);
}

/// Async completion is accepted only for its live owner/sink generation, service
/// work survives requester navigation, and a reply is reduced at most once.
/// Mutants deliver to focus, cancel service work with the view, or re-reduce a
/// consumed reply.
#[test]
fn derived_native_async_delivery_proves_and_catches_focus_routing() {
    let model = native_async_delivery_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let service = buggy.successors("IssueService", &buggy.init_state())[0].clone();
    let dropped = buggy.successors("NavigateCancelsService", &service)[0].clone();
    assert!(!buggy.check_invariant("ServiceOutlivesRequester", &dropped));

    let view = buggy.successors("IssueView", &buggy.init_state())[0].clone();
    let stale = buggy.successors("NavigateView", &view)[0].clone();
    let misdelivered = buggy.successors("DeliverStaleView", &stale)[0].clone();
    assert!(!buggy.check_invariant("IdentityAndGenerationChecked", &misdelivered));

    // A reduced reply delivered again: the healthy machine has no such step,
    // and the reducer without its `pending.remove` guard reduces it twice.
    let completed = model.successors("CompleteView", &view)[0].clone();
    assert_eq!(completed["reduced"], 1);
    assert!(model.successors("RedeliverView", &completed).is_empty());
    let duplicate = buggy.successors("RedeliverView", &completed)[0].clone();
    assert_eq!(duplicate["reduced"], 2);
    assert!(!buggy.check_invariant("ReducedAtMostOnce", &duplicate));
    // A new operation starts its own count.
    let reissued = model.successors("IssueView", &completed)[0].clone();
    assert_eq!(reissued["reduced"], 0);

    assert_every_invariant_carries_a_mutant(&model, &["GenerationsBounded"]);
}

/// Smart-title snapshots may be sent and completions accepted only for the
/// latest terminal-content/settings generations while enabled. The mutant lets
/// captured context cross revocation and applies stale work.
#[test]
fn derived_title_summary_proves_and_catches_stale_completion() {
    let model = title_summary_model();
    assert_proves_and_catches(&model);

    let first = model.successors("Request", &model.init_state())[0].clone();
    let running = model.successors("Start", &first)[0].clone();
    let superseded = model.successors("Request", &running)[0].clone();
    let discarded = model.successors("Complete", &superseded)[0].clone();
    assert_eq!(discarded["applied_generation"], 0);
    assert!(model.check_invariant("StaleCompletionNeverApplies", &discarded));

    let throttled_boundary = model.successors("Boundary", &running)[0].clone();
    assert_eq!(throttled_boundary["current_generation"], 2);
    let boundary_discarded = model.successors("Complete", &throttled_boundary)[0].clone();
    assert_eq!(boundary_discarded["applied_generation"], 0);
    assert!(model.check_invariant("AppliedResultIsCurrent", &boundary_discarded));

    let queued_before_boundary = model.successors("Request", &model.init_state())[0].clone();
    let queued_revoked = model.successors("Boundary", &queued_before_boundary)[0].clone();
    assert_eq!(queued_revoked["pending"], 0);

    let queued = model.successors("Request", &model.init_state())[0].clone();
    let disabled = model.successors("Disable", &queued)[0].clone();
    assert_eq!(disabled["pending"], 0);
    let reenabled = model.successors("Enable", &disabled)[0].clone();
    assert!(model.successors("Start", &reenabled).is_empty());

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let queued = buggy.successors("Request", &buggy.init_state())[0].clone();
    let disabled = buggy.successors("Disable", &queued)[0].clone();
    let reenabled = buggy.successors("Enable", &disabled)[0].clone();
    let leaked = buggy.successors("Start", &reenabled)[0].clone();
    assert!(!buggy.check_invariant("SnapshotNeverCrossesRevocation", &leaked));

    let first = buggy.successors("Request", &buggy.init_state())[0].clone();
    let running = buggy.successors("Start", &first)[0].clone();
    let superseded = buggy.successors("Request", &running)[0].clone();
    let stale = buggy.successors("Complete", &superseded)[0].clone();
    assert!(!buggy.check_invariant("StaleCompletionNeverApplies", &stale));
    assert!(!buggy.check_invariant("AppliedResultIsCurrent", &stale));

    let queued = buggy.successors("Request", &buggy.init_state())[0].clone();
    let leaked_queue = buggy.successors("Boundary", &queued)[0].clone();
    assert!(!buggy.check_invariant("PendingSnapshotHasCurrentAuthority", &leaked_queue));

    let first = buggy.successors("Request", &buggy.init_state())[0].clone();
    let running = buggy.successors("Start", &first)[0].clone();
    let missed_revocation = buggy.successors("Boundary", &running)[0].clone();
    let stale = buggy.successors("Complete", &missed_revocation)[0].clone();
    assert!(!buggy.check_invariant("AppliedResultIsCurrent", &stale));
}

/// A timer refresh supersedes inference work without revoking a label whose
/// semantic/configuration authority is unchanged. Real boundaries still clear.
#[test]
fn derived_title_summary_refresh_preserves_authorized_refinement() {
    let model = title_summary_model();
    let requested = model.successors("Request", &model.init_state())[0].clone();
    let running = model.successors("Start", &requested)[0].clone();
    let refined = model.successors("Complete", &running)[0].clone();
    assert_eq!(refined["applied_generation"], 1);

    let refreshed = model.successors("Refresh", &refined)[0].clone();
    assert_eq!(refreshed["semantic_generation"], 1);
    assert_eq!(refreshed["applied_generation"], 1);
    assert!(model.check_invariant("AppliedResultIsCurrent", &refreshed));

    let boundary = model.successors("Request", &refined)[0].clone();
    assert_eq!(boundary["semantic_generation"], 2);
    assert_eq!(boundary["applied_generation"], 0);
}

/// A failed nonblocking terminal observation owns exactly one retry. Repeated
/// contention keeps it armed; success, disable, and retirement clear it.
#[test]
fn derived_title_summary_observation_retry_is_bounded_and_cleans_up() {
    let model = title_summary_model();
    let armed = model.successors("LockContended", &model.init_state())[0].clone();
    let still_armed = model.successors("RetryContended", &armed)[0].clone();
    assert_eq!(still_armed["retry_pending"], 1);
    let observed = model.successors("ObserveSuccess", &still_armed)[0].clone();
    assert_eq!(observed["retry_pending"], 0);

    let disabled = model.successors("Disable", &armed)[0].clone();
    assert_eq!(disabled["retry_pending"], 0);
    assert!(model.check_invariant("DisabledHasNoObservationRetry", &disabled));

    let retired = model.successors("Retire", &armed)[0].clone();
    assert_eq!(retired["retired"], 1);
    assert!(model.successors("Enable", &retired).is_empty());
    assert!(model.check_invariant("RetiredObservationIsQuiescent", &retired));

    // The teardown paths that forget the retry.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let armed = buggy.successors("LockContended", &buggy.init_state())[0].clone();
    let disabled = buggy.successors("Disable", &armed)[0].clone();
    assert_eq!(disabled["retry_pending"], 1);
    assert!(!buggy.check_invariant("DisabledHasNoObservationRetry", &disabled));
    let retired = buggy.successors("Retire", &armed)[0].clone();
    assert_eq!(retired["retry_pending"], 1);
    assert!(!buggy.check_invariant("RetiredObservationIsQuiescent", &retired));

    assert_every_invariant_carries_a_mutant(&model, &["ObservationRetryIsBoolean"]);
}

/// Quiet relative-age chrome owns an explicit expiry wake. Synchronized
/// expirations drain over bounded turns, and retiring the cache disarms idle.
#[test]
fn derived_session_chrome_expiry_is_waking_bounded_and_self_disarming() {
    let model = session_chrome_expiry_model();
    assert_proves_and_catches(&model);

    let seeded = model.successors("Seed", &model.init_state())[0].clone();
    assert_eq!(seeded["armed"], 1);
    let expired = model.successors("Expire", &seeded)[0].clone();
    assert_eq!(expired["due"], 3);
    let begun = model.successors("Begin", &expired)[0].clone();
    assert_eq!(begun["due"], 2, "other due caches remain retained");
    assert_eq!(begun["remaining"], 3);
    let first = model.successors("Scan", &begun)[0].clone();
    assert_eq!(first["work"], 1, "shipping window-scan budget");
    assert_eq!(first["remaining"], 2, "fan-out cursor retains remainder");
    assert_eq!(first["armed"], 1, "remainder retains a deadline");
    let second = model.successors("Scan", &first)[0].clone();
    assert_eq!(second["work"], 1);
    assert_eq!(second["remaining"], 1);
    let third = model.successors("Scan", &second)[0].clone();
    assert_eq!(third["remaining"], 0);
    assert_eq!(third["scanning"], 0);
    assert_eq!(third["fresh"], 1);
    let retired = model.successors("Retire", &third)[0].clone();
    assert_eq!(retired["armed"], 0, "empty cache owns no idle wake");

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let seeded = buggy.successors("Seed", &buggy.init_state())[0].clone();
    let expired = buggy.successors("Expire", &seeded)[0].clone();
    let begun = buggy.successors("Begin", &expired)[0].clone();
    let bulk = buggy.successors("Scan", &begun)[0].clone();
    assert_eq!(bulk["work"], 3);
    assert!(
        !buggy.check_invariant("WorkPerTurnBounded", &bulk),
        "negative control: the former bulk sweep must be rejected"
    );
}

/// Due Smart-Title observations are admitted one per event-loop turn. The active
/// session starts a fresh batch, then the queued remainder makes bounded progress.
#[test]
fn derived_title_summary_observation_scheduler_proves_cap_and_fairness() {
    let model = title_summary_observation_scheduler_model();
    assert_proves_and_catches(&model);

    let mut state = model.init_state();
    let mut chosen = Vec::new();
    for _ in 0..3 {
        state = model.successors("ObserveTurn", &state)[0].clone();
        chosen.push(state["chosen"]);
        assert_eq!(state["observations_this_turn"], 1);
    }
    assert_eq!(chosen, vec![2, 1, 3]);
    assert!(model.check_invariant("PreservedRemainderIsFair", &state));

    let mut worker = model.init_state();
    let mut worker_chosen = Vec::new();
    for _ in 0..3 {
        worker = model.successors("DispatchWorker", &worker)[0].clone();
        worker_chosen.push(worker["worker_chosen"]);
    }
    assert_eq!(worker_chosen, vec![1, 2, 1]);
    assert!(model.check_invariant("PriorityCannotStarveBackground", &worker));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let bulk = buggy.successors("ObserveTurn", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("OneObservationPerTurn", &bulk));
    let priority_once = buggy.successors("DispatchWorker", &buggy.init_state())[0].clone();
    let priority_twice = buggy.successors("DispatchWorker", &priority_once)[0].clone();
    assert!(!buggy.check_invariant("PriorityCannotStarveBackground", &priority_twice));
    // The fresh batch started at the sorted head, not the active session.
    assert_eq!(bulk["first_chosen"], 1);
    assert!(!buggy.check_invariant("ActiveSessionStartsBatch", &bulk));

    assert_every_invariant_carries_a_mutant(
        &model,
        &["SelectedSessionIsValid", "WorkerSelectionIsValid", "Bounds"],
    );
}

/// Two live sessions have independent coalescing slots and bounded round-robin
/// service. Retirement revokes a dequeued job before I/O/publication, requests are
/// strictly timer-spaced, and an owned runtime cannot outlive its worker.
#[test]
fn derived_title_summary_runtime_proves_fair_cancellable_bounded_lifecycle() {
    let model = title_summary_runtime_model();
    assert_proves_and_catches(&model);

    let started = model.successors("StartWorker", &model.init_state())[0].clone();
    assert_eq!(started["managed_runtime"], 1);
    let queued1 = model.successors("Queue1", &started)[0].clone();
    let queued2 = model.successors("Queue2", &queued1)[0].clone();
    let first = model.successors("Start", &queued2)[0].clone();
    assert_eq!(first["job_session"], 1);
    assert_eq!(first["wait2"], 1);
    let io = model.successors("BeginIo", &first)[0].clone();
    let transmitted = model.successors("Transmit", &io)[0].clone();
    let mut ready = model.successors("Complete", &transmitted)[0].clone();
    ready = model.successors("Tick", &ready)[0].clone();
    ready = model.successors("Tick", &ready)[0].clone();
    ready = model.successors("Observe1", &ready)[0].clone();
    ready = model.successors("Queue1", &ready)[0].clone();
    let second = model.successors("Start", &ready)[0].clone();
    assert_eq!(second["job_session"], 2);
    assert!(model.check_invariant("RoundRobinWaitIsBounded", &second));

    let queued = model.successors("Queue1", &started)[0].clone();
    let dequeued = model.successors("Start", &queued)[0].clone();
    let retired = model.successors("Retire1", &dequeued)[0].clone();
    assert!(model.successors("BeginIo", &retired).is_empty());
    let cancelled = model.successors("Cancel", &retired)[0].clone();
    assert_eq!(cancelled["phase"], 0);

    let queued = model.successors("Queue1", &started)[0].clone();
    let dequeued = model.successors("Start", &queued)[0].clone();
    let connected = model.successors("BeginIo", &dequeued)[0].clone();
    let retired_while_connecting = model.successors("Retire1", &connected)[0].clone();
    assert!(
        model
            .successors("Transmit", &retired_while_connecting)
            .is_empty()
    );
    assert_eq!(
        model.successors("Cancel", &retired_while_connecting)[0]["phase"],
        0
    );

    let stopped = model.successors("StopWorker", &started)[0].clone();
    assert_eq!(stopped["managed_runtime"], 0);
    assert!(model.check_invariant("ManagedRuntimeHasWorker", &stopped));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let started = buggy.successors("StartWorker", &buggy.init_state())[0].clone();
    let queued = buggy.successors("Queue1", &started)[0].clone();
    let dequeued = buggy.successors("Start", &queued)[0].clone();
    let retired = buggy.successors("Retire1", &dequeued)[0].clone();
    let unauthorized = buggy.successors("BeginIo", &retired)[0].clone();
    assert!(!buggy.check_invariant("NoIoAfterRetirement", &unauthorized));

    let queued = buggy.successors("Queue1", &started)[0].clone();
    let dequeued = buggy.successors("Start", &queued)[0].clone();
    let io = buggy.successors("BeginIo", &dequeued)[0].clone();
    let transmitted = buggy.successors("Transmit", &io)[0].clone();
    let complete = buggy.successors("Complete", &transmitted)[0].clone();
    let dirty = buggy.successors("Observe1", &complete)[0].clone();
    let too_soon = buggy.successors("Queue1", &dirty)[0].clone();
    assert!(!buggy.check_invariant("StrictMinimumInterval", &too_soon));

    let queued = buggy.successors("Queue1", &started)[0].clone();
    let dequeued = buggy.successors("Start", &queued)[0].clone();
    let connected = buggy.successors("BeginIo", &dequeued)[0].clone();
    let retired = buggy.successors("Retire1", &connected)[0].clone();
    let leaked = buggy.successors("Transmit", &retired)[0].clone();
    assert!(!buggy.check_invariant("NoTransmitAfterRetirement", &leaked));
}

/// Automatic managed endpoints are process-owned, distinct, reusable by the same
/// authority, absent from the historical shared default, and cleared on revoke.
#[test]
fn derived_title_summary_managed_endpoints_are_distinct_and_revocation_safe() {
    let model = title_summary_managed_endpoint_model();
    assert_proves_and_catches(&model);

    let launched1 = model.successors("Launch1", &model.init_state())[0].clone();
    let launched2 = model.successors("Launch2", &launched1)[0].clone();
    assert_eq!(launched1["endpoint1"], 1);
    assert_eq!(launched2["endpoint2"], 2);
    assert!(model.check_invariant("ConcurrentAutomaticEndpointsAreDistinct", &launched2));
    assert!(model.check_invariant("AutomaticEndpointNeverUsesSharedDefault", &launched2));

    let reused = model.successors("Reuse1", &launched2)[0].clone();
    assert_eq!(reused["health_endpoint1"], reused["endpoint1"]);
    let revoked = model.successors("Reconfigure1", &reused)[0].clone();
    assert_eq!(revoked["endpoint1"], 0);
    assert_eq!(revoked["health_endpoint1"], 0);
    let stale = model.successors("StaleResult1", &revoked)[0].clone();
    assert_eq!(stale["health_endpoint1"], 0);

    let launched = model.successors("Launch1", &model.init_state())[0].clone();
    let healthy = model.successors("Reuse1", &launched)[0].clone();
    let crashed = model.successors("Crash1", &healthy)[0].clone();
    assert_eq!(crashed["process1"], 0);
    assert_eq!(crashed["endpoint1"], 0);
    assert_eq!(crashed["health_endpoint1"], 0);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let launched1 = buggy.successors("Launch1", &buggy.init_state())[0].clone();
    let collision = buggy.successors("Launch2", &launched1)[0].clone();
    assert!(!buggy.check_invariant("ConcurrentAutomaticEndpointsAreDistinct", &collision,));
    // Health published after revocation.
    let revoked = buggy.successors("Reconfigure1", &buggy.init_state())[0].clone();
    let republished = buggy.successors("StaleResult1", &revoked)[0].clone();
    assert!(!buggy.check_invariant("RevokedHealthIsClear", &republished));

    // Automatic endpoints resolved through configuration: both processes land
    // on the shared default.
    assert_eq!(collision["endpoint1"], 3);
    assert_eq!(collision["endpoint2"], 3);
    assert!(!buggy.check_invariant("AutomaticEndpointNeverUsesSharedDefault", &launched1));
    // The one exit handler keeps the dead daemon's endpoint, health and reuse
    // capability — in either process.
    for (launch, reuse, crash, n) in [
        ("Launch1", "Reuse1", "Crash1", "1"),
        ("Launch2", "Reuse2", "Crash2", "2"),
    ] {
        let launched = buggy.successors(launch, &buggy.init_state())[0].clone();
        let reused = buggy.successors(reuse, &launched)[0].clone();
        let dead = buggy.successors(crash, &reused)[0].clone();
        assert_eq!(dead[format!("process{n}").as_str()], 0);
        assert!(dead[format!("endpoint{n}").as_str()] > 0);
        assert_eq!(dead[format!("reused{n}").as_str()], 1);
        assert!(!buggy.check_invariant("EndpointBelongsToOwnedProcess", &dead));
        assert!(!buggy.check_invariant("ReuseRetainsOwnedEndpoint", &dead));
    }

    assert_every_invariant_carries_a_mutant(&model, &["Bounds"]);
}

/// Exact macOS socket-owner observations retry on both transient shapes, accept
/// one unique owner, fail closed on structural/permanent errors, and exhaust a
/// finite retry budget. The mutant prematurely fails an ambiguous observation.
#[test]
fn derived_title_summary_socket_owner_retry_proves_and_catches_ambiguity_drop() {
    let model = title_summary_socket_owner_retry_model();
    assert_proves_and_catches(&model);

    let missing = model.successors("ObserveMissing", &model.init_state())[0].clone();
    assert_eq!(missing["phase"], 1);
    assert_eq!(missing["retries"], 1);

    let ambiguous = model.successors("ObserveAmbiguous", &missing)[0].clone();
    assert_eq!(ambiguous["phase"], 1);
    assert_eq!(ambiguous["retries"], 2);
    assert!(model.check_invariant("TransientObservationsRetry", &ambiguous));

    let unique = model.successors("ObserveUnique", &ambiguous)[0].clone();
    assert_eq!(unique["phase"], 2);
    assert!(model.check_invariant("UniqueObservationSucceeds", &unique));

    let structural = model.successors("ObserveStructuralError", &model.init_state())[0].clone();
    assert_eq!(structural["phase"], 3);
    assert!(model.check_invariant("PermanentErrorsFailClosed", &structural));

    let permanent = model.successors("ObservePermanentError", &model.init_state())[0].clone();
    assert_eq!(permanent["phase"], 3);
    assert!(model.check_invariant("PermanentErrorsFailClosed", &permanent));

    let third_transient = model.successors("ObserveMissing", &ambiguous)[0].clone();
    assert_eq!(third_transient["retries"], 3);
    assert!(
        model
            .successors("ObserveAmbiguous", &third_transient)
            .is_empty(),
        "the retry train must stop at its finite budget"
    );
    let timeout = model.successors("Timeout", &third_transient)[0].clone();
    assert_eq!(timeout["timed_out"], 1);
    assert_eq!(timeout["phase"], 3);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let prematurely_failed = buggy.successors("ObserveAmbiguous", &buggy.init_state())[0].clone();
    assert_eq!(prematurely_failed["phase"], 3);
    assert!(
        !buggy.check_invariant("TransientObservationsRetry", &prematurely_failed),
        "negative control: transient ambiguity must not fail prematurely"
    );

    // The deadline read before the verdict: the owner the last attempt found
    // is failed as a timeout.
    let mut spent = buggy.init_state();
    for _ in 0..3 {
        spent = buggy.successors("ObserveMissing", &spent)[0].clone();
    }
    let late_owner = buggy.successors("ObserveUnique", &spent)[0].clone();
    assert_eq!(late_owner["timed_out"], 1);
    assert!(!buggy.check_invariant("UniqueObservationSucceeds", &late_owner));

    // An unfiltered retry arm retries a structural error.
    let retried = buggy.successors("ObserveStructuralError", &buggy.init_state())[0].clone();
    assert_eq!(retried["phase"], 1);
    assert!(!buggy.check_invariant("PermanentErrorsFailClosed", &retried));

    assert_every_invariant_carries_a_mutant(&model, &["RetryBudgetIsBounded", "Bounds"]);
}

/// The updater is generation-stamped and single-flight: a current verified
/// artifact plus close preflight is required, and only one apply authority may
/// be live. A safely aborted process replacement re-arms the same stage without
/// permitting two simultaneous authorities. Mutants stage a stale completion or
/// apply twice.
#[test]
fn derived_native_updater_proves_and_catches_stale_or_double_apply() {
    let model = native_updater_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let checking = buggy.successors("StartCheck", &buggy.init_state())[0].clone();
    let available = buggy.successors("CheckAvailable", &checking)[0].clone();
    let downloading = buggy.successors("StartDownload", &available)[0].clone();
    let superseded = buggy.successors("SupersedeDownload", &downloading)[0].clone();
    let stale = buggy.successors("DropStaleDownload", &superseded)[0].clone();
    assert!(!buggy.check_invariant("CurrentStagedArtifact", &stale));

    let downloaded = buggy.successors("CompleteDownload", &downloading)[0].clone();
    let ready = buggy.successors("MarkCloseReady", &downloaded)[0].clone();
    let applied = buggy.successors("Apply", &ready)[0].clone();
    let applied_twice = buggy.successors("Apply", &applied)[0].clone();
    assert!(!buggy.check_invariant("OneLiveApplyAuthority", &applied_twice));
}

/// Draft creation and asset upload each consume one process-local POST permit
/// granted only after durable intent persistence. Crashes erase the permit,
/// while eventual exact-object visibility remains convergent without a retry.
#[test]
fn derived_release_post_intents_are_durable_and_one_shot() {
    let model = release_durable_post_intent_model();
    assert_proves_and_catches(&model);

    // Crash before the create POST: intent survives, authority does not. Resume
    // cannot issue even the first request from that old intent.
    let mut before_post = model.init_state();
    assert!(model.fire("PersistCreateIntent", &mut before_post));
    assert_eq!(before_post["create_post_authority"], 1);
    assert!(model.fire("Crash", &mut before_post));
    assert_eq!(before_post["create_intent"], 1);
    assert_eq!(before_post["create_post_authority"], 0);
    assert!(model.fire("Resume", &mut before_post));
    assert!(!model.action_enabled("IssueCreatePost", &before_post));

    // Crash after a landed request but before its response is trusted: resume
    // cannot POST again, then delayed visibility converges the exact object.
    let mut after_post = model.init_state();
    assert!(model.fire("PersistCreateIntent", &mut after_post));
    assert!(model.fire("IssueCreatePost", &mut after_post));
    assert!(model.fire("Crash", &mut after_post));
    assert!(model.fire("Resume", &mut after_post));
    assert!(!model.action_enabled("IssueCreatePost", &after_post));
    assert!(model.fire("RevealCreatedDraft", &mut after_post));
    assert!(model.fire("ConvergeCreatedDraft", &mut after_post));

    // Upload has its own newly granted permit. It remains usable even though a
    // prior create-stage crash consumed the first operation's permit.
    assert!(model.fire("PersistUploadIntent", &mut after_post));
    assert!(model.fire("IssueUploadPost", &mut after_post));
    assert!(model.fire("Crash", &mut after_post));
    assert!(model.fire("Resume", &mut after_post));
    assert!(!model.action_enabled("IssueUploadPost", &after_post));
    assert!(model.fire("RevealUploadedAsset", &mut after_post));
    assert!(model.fire("ConvergeUploadedAsset", &mut after_post));
    assert_eq!(after_post["upload_converged"], 1);

    // Negative control: the mutant can reuse the erased pre-POST permit after
    // resume, exactly reproducing a duplicate/non-authorized request.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut retried = buggy.init_state();
    assert!(buggy.fire("PersistCreateIntent", &mut retried));
    assert!(buggy.fire("Crash", &mut retried));
    assert!(buggy.fire("Resume", &mut retried));
    assert!(buggy.fire("IssueCreatePost", &mut retried));
    assert!(!buggy.check_invariant("LostCreatePermitCannotPost", &retried));

    let mut unjournaled = buggy.init_state();
    assert!(buggy.fire("IssueCreatePost", &mut unjournaled));
    assert!(!buggy.check_invariant("CreatePostRequiresDurableIntent", &unjournaled));

    // Negative controls: a landed POST taken at its word. The draft converges,
    // its asset is journaled against it, and the asset converges, each with no
    // re-listed object behind it — the re-read `step_draft` and the asset upload
    // both perform before believing a POST.
    let mut trusted = buggy.init_state();
    for action in ["PersistCreateIntent", "IssueCreatePost"] {
        assert!(buggy.fire(action, &mut trusted), "{action}");
    }
    assert!(!model.action_enabled("ConvergeCreatedDraft", &trusted));
    let mut upload_early = trusted.clone();
    assert!(buggy.fire("ConvergeCreatedDraft", &mut trusted));
    assert!(!buggy.check_invariant("CreateConvergenceRequiresVisibility", &trusted));
    assert!(buggy.fire("PersistUploadIntent", &mut upload_early));
    assert!(!buggy.check_invariant("UploadRequiresConvergedDraft", &upload_early));
    for action in [
        "RevealCreatedDraft",
        "PersistUploadIntent",
        "IssueUploadPost",
    ] {
        assert!(buggy.fire(action, &mut trusted), "{action}");
    }
    assert!(!model.action_enabled("ConvergeUploadedAsset", &trusted));
    assert!(buggy.fire("ConvergeUploadedAsset", &mut trusted));
    assert!(!buggy.check_invariant("UploadConvergenceRequiresVisibility", &trusted));
    assert_every_invariant_carries_a_mutant(&model, &["DurableIntentStateBounded"]);
}

/// The roster body and master signature commit through one durable redo marker.
/// Every known crash cut recovers to the exact pair; a newer/unrelated half is a
/// refusal, and check-only acquisition has no replay authority. Tier-1 lives in
/// `crates/atpkg-keys/tests/roster_redo_model.rs`.
#[test]
fn derived_roster_pair_redo_proves_crash_recovery_and_foreign_preservation() {
    let model = roster_pair_redo_model();
    assert_proves_and_catches(&model);

    let mut body_cut = model.init_state();
    for action in ["AcquireWriter", "AcceptSnapshot", "CrashAfterBody"] {
        assert!(model.fire(action, &mut body_cut), "disabled {action}");
    }
    assert_eq!(body_cut["body"], 1);
    assert_eq!(body_cut["signature"], 0);
    assert_eq!(body_cut["redo"], 1);
    assert!(model.fire("ReadOnlyRejectRedo", &mut body_cut));
    assert_eq!(body_cut["readonly_writes"], 0);
    assert!(model.fire("RecoverKnown", &mut body_cut));
    assert_eq!(body_cut["body"], 1);
    assert_eq!(body_cut["signature"], 1);
    assert_eq!(body_cut["redo"], 0);
    assert_eq!(body_cut["result"], 1);

    let mut foreign = model.init_state();
    for action in [
        "AcquireWriter",
        "AcceptSnapshot",
        "CrashAfterRedo",
        "ReplaceSignatureWhileDown",
        "RejectForeignRecovery",
    ] {
        assert!(model.fire(action, &mut foreign), "disabled {action}");
    }
    assert_eq!(foreign["signature"], 2);
    assert_eq!(foreign["redo"], 1);
    assert_eq!(foreign["foreign_overwritten"], 0);
    assert_eq!(foreign["writer_writes"], 0);

    // NEGATIVE CONTROLS: the healthy model cannot retire a mixed pair or replay
    // over foreign bytes; Buggy=1 admits both and violates the named invariants.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut partial = buggy.init_state();
    for action in ["AcquireWriter", "AcceptSnapshot", "CrashAfterBody"] {
        assert!(buggy.fire(action, &mut partial), "disabled {action}");
    }
    assert!(buggy.fire("BuggyRetirePartial", &mut partial));
    assert!(!buggy.check_invariant("SuccessfulPairIsExact", &partial));

    let mut overwritten = buggy.init_state();
    for action in [
        "AcquireWriter",
        "AcceptSnapshot",
        "CrashAfterRedo",
        "ReplaceBodyWhileDown",
        "BuggyOverwriteForeign",
    ] {
        assert!(buggy.fire(action, &mut overwritten), "disabled {action}");
    }
    assert!(!buggy.check_invariant("ForeignBytesAreNeverOverwritten", &overwritten));

    // The pre-8dbc4e967 publisher: the body renamed into place with no redo
    // record, then death before the signature. A torn pair nothing can replay.
    let mut torn = buggy.init_state();
    for action in [
        "AcquireWriter",
        "AcceptSnapshot",
        "BuggyPromoteBodyWithoutRedo",
    ] {
        assert!(buggy.fire(action, &mut torn), "disabled {action}");
    }
    assert_eq!((torn["body"], torn["signature"], torn["redo"]), (1, 0, 0));
    assert!(!buggy.check_invariant("TargetHalfHasRedoAuthority", &torn));
    assert!(!buggy.action_enabled("RecoverKnown", &torn));

    // A per-half CAS: the body is promoted on its own premise before the stale
    // signature is found, so the refusal has already written.
    let mut per_half = buggy.init_state();
    for action in [
        "AdvanceSignatureBeforeCas",
        "AcquireWriter",
        "BuggyPromoteBeforeSignatureCas",
    ] {
        assert!(buggy.fire(action, &mut per_half), "disabled {action}");
    }
    assert_eq!(per_half["result"], 2);
    assert!(!buggy.check_invariant("StaleSnapshotWritesNothing", &per_half));
    assert_every_invariant_carries_a_mutant(&model, &["StateBounded"]);
}

/// A release floor is frozen as channel state, survives resume unchanged, and is
/// revalidated against a potentially newer live floor immediately before publish.
/// The exact-commit lease remains held until the published head is proved and is
/// released only by the final unlock. The healthy lifecycle can neither forget an
/// observed floor, publish through a late ratchet, nor unlock early.
#[test]
fn derived_release_channel_floor_proves_carry_forward_and_late_guard() {
    let model = release_channel_floor_model();
    assert_proves_and_catches(&model);

    // Healthy carry-forward: operator=1, observed=2, claim=3 freezes 2 in the
    // journal; resume leaves it byte-policy equivalent.
    let mut frozen = model.init_state();
    assert!(model.fire("RaiseOperator", &mut frozen));
    assert!(model.fire("RaiseObserved", &mut frozen));
    assert!(model.fire("RaiseObserved", &mut frozen));
    for _ in 0..3 {
        assert!(model.fire("RaiseClaim", &mut frozen));
    }
    assert!(model.fire("Resolve", &mut frozen));
    assert_eq!(frozen["phase"], 1);
    assert_eq!(frozen["frozen_floor"], 2);
    assert_eq!(frozen["journal_floor"], 2);
    assert!(model.fire("CrashBeforeResume", &mut frozen));
    assert_eq!(frozen["phase"], 5);
    assert_eq!(frozen["frozen_floor"], 0);
    assert_eq!(frozen["journal_floor"], 2);
    assert!(model.fire("ResumeFrozen", &mut frozen));
    assert_eq!(frozen["frozen_floor"], frozen["journal_floor"]);

    // A concurrent raise to 3 is visible before the lease; once the lease is held,
    // the revalidation rejects it but retains ownership until explicit abandon.
    assert!(model.fire("RaiseChannelFloor", &mut frozen));
    assert!(model.fire("AcquireLease", &mut frozen));
    assert!(model.fire("RejectAdvanced", &mut frozen));
    assert_eq!(frozen["phase"], 4);
    assert_eq!(
        frozen["lease_owned"], 1,
        "late refusal retains the lease for explicit recovery/abandon"
    );
    assert!(model.fire("AbandonRejected", &mut frozen));
    assert_eq!(frozen["lease_owned"], 0);

    // A covered cut keeps the same owner after the head PATCH and through the proof
    // of what a stranger sees. Only the journaled final unlock releases it.
    let mut complete = model.init_state();
    assert!(model.fire("RaiseObserved", &mut complete));
    for _ in 0..2 {
        assert!(model.fire("RaiseClaim", &mut complete));
    }
    assert!(model.fire("Resolve", &mut complete));
    assert!(model.fire("AcquireLease", &mut complete));
    assert!(model.fire("ConfirmCovered", &mut complete));
    assert!(model.fire("PublishChecked", &mut complete));
    assert_eq!(complete["phase"], 3);
    assert_eq!(complete["lease_owned"], 1);
    assert!(model.check_invariant("VisibleWorkOwnsLease", &complete));
    assert!(model.fire("ProveHead", &mut complete));
    assert_eq!(complete["lease_owned"], 1);
    assert!(model.fire("Unlock", &mut complete));
    assert_eq!(complete["phase"], 7);
    assert_eq!(complete["lease_owned"], 0);
    assert!(model.check_invariant("CompletionRequiresProvedHead", &complete));

    // Mutant 1: dropping the observed channel input immediately violates the
    // frozen carry-forward invariant.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut dropped = buggy.init_state();
    assert!(buggy.fire("RaiseObserved", &mut dropped));
    assert!(buggy.fire("RaiseClaim", &mut dropped));
    assert!(buggy.fire("ResolveOperatorOnly", &mut dropped));
    assert_eq!(dropped["frozen_floor"], 0);
    assert!(!buggy.check_invariant("FrozenCoversInitialInputs", &dropped));

    // Mutant 2: with an otherwise sound frozen floor, a later channel raise plus
    // PublishUnchecked from Frozen skips the required guard and lowers live policy.
    let mut skipped = buggy.init_state();
    assert!(buggy.fire("RaiseOperator", &mut skipped));
    assert!(buggy.fire("RaiseObserved", &mut skipped));
    for _ in 0..2 {
        assert!(buggy.fire("RaiseClaim", &mut skipped));
    }
    assert!(buggy.fire("Resolve", &mut skipped));
    assert!(buggy.fire("RaiseChannelFloor", &mut skipped));
    assert!(buggy.fire("PublishUnchecked", &mut skipped));
    assert_eq!(skipped["phase"], 3);
    assert!(!buggy.check_invariant("PublishedNeverLowersLatest", &skipped));
    assert!(!buggy.check_invariant("PublishedRequiresLateGuard", &skipped));
    assert!(!buggy.check_invariant("VisibleWorkOwnsLease", &skipped));

    // Mutant 3: even a correct covered verdict is unsafe if another publisher can
    // bypass the supposedly shared lease before visibility.
    let mut lease_bug = buggy.init_state();
    assert!(buggy.fire("RaiseOperator", &mut lease_bug));
    assert!(buggy.fire("RaiseObserved", &mut lease_bug));
    for _ in 0..2 {
        assert!(buggy.fire("RaiseClaim", &mut lease_bug));
    }
    assert!(buggy.fire("Resolve", &mut lease_bug));
    assert!(buggy.fire("AcquireLease", &mut lease_bug));
    assert!(buggy.fire("ConfirmCovered", &mut lease_bug));
    assert!(buggy.fire("BypassLeaseAdvance", &mut lease_bug));
    assert!(!buggy.check_invariant("LeaseCannotBeBypassed", &lease_bug));
    assert!(buggy.fire("PublishChecked", &mut lease_bug));
    assert!(!buggy.check_invariant("PublishedNeverLowersLatest", &lease_bug));

    // Mutant 4: releasing the remote owner immediately after the head PATCH exposes
    // the unproved head to a competing cut.
    let mut early_unlock = buggy.init_state();
    assert!(buggy.fire("RaiseClaim", &mut early_unlock));
    assert!(buggy.fire("Resolve", &mut early_unlock));
    assert!(buggy.fire("AcquireLease", &mut early_unlock));
    assert!(buggy.fire("ConfirmCovered", &mut early_unlock));
    assert!(buggy.fire("PublishChecked", &mut early_unlock));
    assert!(!model.action_enabled("UnlockBeforeHeadProof", &early_unlock));
    assert!(buggy.fire("UnlockBeforeHeadProof", &mut early_unlock));
    assert!(!buggy.check_invariant("CompletionRequiresProvedHead", &early_unlock));
    assert!(!buggy.check_invariant("UnlockCannotBeBypassed", &early_unlock));

    // Mutant 5: the claim check applied to the operator's request only, so a
    // channel floor above the claimed build is carried forward.
    let mut over_claim = buggy.init_state();
    assert!(buggy.fire("RaiseObserved", &mut over_claim));
    assert!(!model.action_enabled("Resolve", &over_claim));
    assert!(buggy.fire("ResolveUncheckedCarryForward", &mut over_claim));
    assert!(!buggy.check_invariant("FrozenFloorFitsClaim", &over_claim));

    // Mutant 6: resume rebuilds its floor from the resume command's request, not
    // the journal: operator=0, observed=1 freezes 1 and resumes at 0.
    let mut resumed = buggy.init_state();
    for action in [
        "RaiseObserved",
        "RaiseClaim",
        "Resolve",
        "CrashBeforeResume",
    ] {
        assert!(buggy.fire(action, &mut resumed), "{action}");
    }
    assert!(buggy.fire("ResumeFromOperatorRequest", &mut resumed));
    assert_ne!(resumed["frozen_floor"], resumed["journal_floor"]);
    assert!(!buggy.check_invariant("RuntimeMatchesFrozenJournal", &resumed));

    // Mutant 7: the floor check without the owner check PublishChecked pairs it with.
    let mut unowned = buggy.init_state();
    for action in ["RaiseClaim", "Resolve"] {
        assert!(buggy.fire(action, &mut unowned), "{action}");
    }
    assert!(!model.action_enabled("ConfirmCovered", &unowned));
    assert!(buggy.fire("ConfirmCoveredWithoutLease", &mut unowned));
    assert!(!buggy.check_invariant("RevalidatedOwnsLease", &unowned));

    // Mutant 8: completion journaled over a CAS delete that never landed.
    let mut leaked = buggy.init_state();
    for action in [
        "RaiseClaim",
        "Resolve",
        "AcquireLease",
        "ConfirmCovered",
        "PublishChecked",
        "ProveHead",
        "CompleteWithoutUnlock",
    ] {
        assert!(buggy.fire(action, &mut leaked), "{action}");
    }
    assert!(!buggy.check_invariant("CompletedReleasesLease", &leaked));

    // Mutant 9: the late guard's error path drops the remote lease.
    let mut dropped_lease = buggy.init_state();
    for action in [
        "RaiseClaim",
        "Resolve",
        "RaiseChannelFloor",
        "AcquireLease",
        "RejectAdvancedReleasingLease",
    ] {
        assert!(buggy.fire(action, &mut dropped_lease), "{action}");
    }
    assert!(!buggy.check_invariant("RejectionCannotSilentlyDropLease", &dropped_lease));

    // Mutant 10: an abandon marked done whose CAS delete never landed.
    let mut half_abandoned = buggy.init_state();
    for action in [
        "RaiseClaim",
        "Resolve",
        "RaiseChannelFloor",
        "AcquireLease",
        "RejectAdvanced",
        "AbandonIgnoringFailedCas",
    ] {
        assert!(buggy.fire(action, &mut half_abandoned), "{action}");
    }
    assert!(!buggy.check_invariant("AbandonIsExplicitAndTerminal", &half_abandoned));
    assert_every_invariant_carries_a_mutant(&model, &["FloorStateBounds"]);
}

/// The release claim's writer/reader contract (owner ruling R2, 2026-09-23): the
/// release commit carries only the published code, main keeps every peer commit
/// and every ledger line, build numbers strictly increase, and a claimed-unpublished
/// version is read as a recut — never fresh, never "cut elsewhere", never claimed
/// again once published. Each mutant is replayed and caught by its own law.
#[test]
fn derived_release_claim_landing_proves_the_writer_reader_contract() {
    let model = release_claim_landing_model();
    assert_operator_model_shape(&model, |state| state[&"phase"] == 5 || state[&"seq"] == 6);
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &["ClaimStateBounds"]);

    // Peers push, the claim loses a race to another version's claim, re-reads,
    // and lands as a merge; the cut dies; the recut claims again and publishes; a
    // third cut is refused.
    let mut state = model.init_state();
    for action in [
        "PeerPush",
        "ClassifyFresh",
        "ClaimRead",
        "RivalClaim",
        "ClaimRetry",
        "ClaimLand",
        "Die",
        "ClassifyRecut",
        "ClaimRead",
        "PeerPush",
        "ClaimRetry",
        "ClaimLand",
        "Publish",
        "ClassifyRefuse",
    ] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, &state),
                "{action} broke {}: {state:?}",
                invariant.name
            );
        }
    }
    assert_eq!(state["landings"], 2);
    assert_eq!(state["main_code"], 2);
    assert_eq!(state["main_lines"], 4);
    assert_eq!(state["tail"], 4);
    assert_eq!(state["phase"], 5);

    // Another machine claiming this version while ours is in flight IS a cut
    // elsewhere — the abort is legitimate there, and only there.
    let mut raced = model.init_state();
    for action in ["ClassifyFresh", "ClaimRead", "ElsewhereClaim"] {
        assert!(model.fire(action, &mut raced), "{action}");
    }
    assert!(!model.action_enabled("ClaimRetry", &raced));
    assert!(model.fire("ClaimElsewhere", &mut raced));
    assert!(model.check_invariant("OwnSectionIsNeverCutElsewhere", &raced));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    // The pre-R2 cut: the release built from main's tip carries a peer's code.
    let mut tip_build = buggy.init_state();
    for action in ["PeerPush", "ClassifyFresh", "ClaimRead"] {
        assert!(buggy.fire(action, &mut tip_build), "{action}");
    }
    assert!(!buggy.check_invariant("ReleaseCarriesOnlyThePublishedCode", &tip_build));

    // A landing built from the release commit's tree drops the rival's ledger line.
    let mut r_tree = buggy.init_state();
    for action in ["RivalClaim", "ClassifyFresh", "ClaimRead", "ClaimLand"] {
        assert!(buggy.fire(action, &mut r_tree), "{action}");
    }
    assert!(!buggy.check_invariant("MainKeepsEveryLedgerLine", &r_tree));

    // A retry that keeps its stale number lands at or below the winner's tail.
    let mut stale = buggy.init_state();
    for action in [
        "ClassifyFresh",
        "ClaimRead",
        "RivalClaim",
        "ClaimRetry",
        "ClaimLand",
    ] {
        assert!(buggy.fire(action, &mut stale), "{action}");
    }
    assert!(!buggy.check_invariant("BuildsStrictlyIncrease", &stale));

    // The reader that takes the recut signal from the published commit's changelog:
    // a claimed-unpublished version is fresh to it, and its own claim's section then
    // aborts a lost race as "cut elsewhere".
    let mut wedged = model.init_state();
    for action in ["ClassifyFresh", "ClaimRead", "ClaimLand", "Die"] {
        assert!(model.fire(action, &mut wedged), "{action}");
    }
    assert!(!model.action_enabled("ClassifyFresh", &wedged));
    assert!(buggy.fire("ClassifyFresh", &mut wedged));
    assert!(!buggy.check_invariant("ClaimedUnpublishedIsNeverFresh", &wedged));
    for action in ["ClaimRead", "PeerPush", "ClaimElsewhere"] {
        assert!(buggy.fire(action, &mut wedged), "{action}");
    }
    assert!(!buggy.check_invariant("OwnSectionIsNeverCutElsewhere", &wedged));

    // ...and a published version is claimed again.
    let mut republished = model.init_state();
    for action in ["ClassifyFresh", "ClaimRead", "ClaimLand", "Publish"] {
        assert!(model.fire(action, &mut republished), "{action}");
    }
    assert!(!model.action_enabled("ClassifyFresh", &republished));
    for action in ["ClassifyFresh", "ClaimRead", "ClaimLand"] {
        assert!(buggy.fire(action, &mut republished), "{action}");
    }
    assert!(!buggy.check_invariant("NoClaimAfterPublish", &republished));
}

/// A current release journal is an exact canonical prefix. Resume starts at the
/// first gap and can never use later membership to skip an ordered mutation.
#[test]
fn derived_release_journal_requires_exact_prefix_and_ordered_resume() {
    let model = release_journal_prefix_model();
    assert_proves_and_catches(&model);

    // A valid persisted prefix resumes at its first incomplete step. A crash
    // drops only the local attachment, then the same prefix continues in order.
    let mut state = model.init_state();
    assert!(model.fire("InputLock", &mut state));
    assert!(model.fire("InputPrepare", &mut state));
    assert!(model.fire("AdmitPreparePrefix", &mut state));
    assert_eq!(state["resume_cursor"], 2);
    assert!(model.fire("CrashAfterAdmission", &mut state));
    assert_eq!(state["attached"], 0);
    assert!(model.fire("ReattachCanonicalPrefix", &mut state));
    assert!(model.fire("RunVisibleConvergence", &mut state));
    assert!(model.fire("RunVerifyAndUnlock", &mut state));
    assert_eq!(state["phase"], 2);
    assert_eq!(state["resume_cursor"], 4);
    assert!(model.check_invariant("CompletionRequiresEveryStep", &state));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    // The historical corruption class: a later completed membership sits past
    // the first gap. Healthy admission is structurally absent; the mutant maps
    // exactly to skipping that earlier remote mutation on resume.
    let mut gap = model.init_state();
    assert!(model.fire("InputLock", &mut gap));
    assert!(model.fire("InputVisible", &mut gap));
    assert!(!model.action_enabled("AdmitGappedJournal", &gap));
    assert!(buggy.fire("AdmitGappedJournal", &mut gap));
    assert!(!buggy.check_invariant("AdmittedDoneIsCanonicalPrefix", &gap));
    assert!(!buggy.check_invariant("CorruptJournalCannotResume", &gap));

    let mut unknown = model.init_state();
    assert!(model.fire("InputUnknown", &mut unknown));
    assert!(buggy.fire("AdmitUnknownJournal", &mut unknown));
    assert!(!buggy.check_invariant("AdmittedDoneIsCanonicalPrefix", &unknown));

    let mut duplicate = model.init_state();
    assert!(model.fire("InputDuplicate", &mut duplicate));
    assert!(buggy.fire("AdmitDuplicateJournal", &mut duplicate));
    assert!(!buggy.check_invariant("AdmittedDoneIsCanonicalPrefix", &duplicate));

    let mut bad_identity = model.init_state();
    assert!(model.fire("InputBadVersion", &mut bad_identity));
    assert!(model.fire("InputBadOwner", &mut bad_identity));
    assert!(buggy.fire("AdmitBadIdentityJournal", &mut bad_identity));
    assert!(!buggy.check_invariant("AdmittedDoneIsCanonicalPrefix", &bad_identity));

    let mut skip = model.init_state();
    assert!(model.fire("InputLock", &mut skip));
    assert!(model.fire("AdmitLockPrefix", &mut skip));
    assert!(!model.action_enabled("SkipPreparationAfterResume", &skip));
    assert!(buggy.fire("SkipPreparationAfterResume", &mut skip));
    assert!(!buggy.check_invariant("AdmittedDoneIsCanonicalPrefix", &skip));
    assert!(!buggy.check_invariant("CursorIsFirstIncomplete", &skip));
    assert!(!buggy.check_invariant("ResumeCannotSkipOrderedMutation", &skip));

    // The cut reported DONE over an unjournaled final verify/unlock step: the
    // prefix is still canonical and the cursor still says 4, so only the
    // completion law sees the missing step.
    let mut unfinished = model.init_state();
    for action in [
        "InputLock",
        "InputPrepare",
        "InputVisible",
        "AdmitVisiblePrefix",
    ] {
        assert!(model.fire(action, &mut unfinished), "{action}");
    }
    assert!(buggy.fire("CompleteBeforeUnlockJournaled", &mut unfinished));
    assert!(buggy.check_invariant("AdmittedDoneIsCanonicalPrefix", &unfinished));
    assert!(buggy.check_invariant("CursorIsFirstIncomplete", &unfinished));
    assert!(!buggy.check_invariant("CompletionRequiresEveryStep", &unfinished));
    assert_every_invariant_carries_a_mutant(&model, &["JournalPrefixBounds"]);
}

/// The persistent claim lease is shared by same-commit resumes, while the
/// annotated publisher token is unique per process. After the explicit old-process
/// stop precondition, exact-CAS rotation invalidates residual guard data; stale
/// cleanup and ambiguous transport cannot confer authority.
#[test]
fn derived_release_publisher_fence_proves_unique_mutation_session() {
    let model = release_publisher_fence_model();
    assert_proves_and_catches(&model);

    let mut raced = model.init_state();
    assert!(model.fire("AcquireA", &mut raced));
    assert!(model.fire("LoseBCreateRace", &mut raced));
    assert!(!model.action_enabled("MutateB", &raced));
    assert!(model.fire("MutateA", &mut raced));

    let mut recovered = model.init_state();
    assert!(model.fire("AcquireA", &mut recovered));
    assert!(!model.action_enabled("RotateAtoB", &recovered));
    assert!(model.fire("StopA", &mut recovered));
    assert!(model.fire("RotateAtoB", &mut recovered));
    assert_eq!(recovered["remote_token"], 2);
    assert_eq!(recovered["local_a_token"], 1);
    assert!(!model.action_enabled("MutateA", &recovered));
    assert!(model.fire("MutateB", &mut recovered));
    assert!(model.fire("ObserveStaleARelease", &mut recovered));
    assert_eq!(recovered["remote_token"], 2);
    assert!(model.fire("AtomicFinalDeleteB", &mut recovered));
    assert_eq!(recovered["remote_token"], 0);
    assert_eq!(recovered["remote_fence_owner"], 0);
    assert_eq!(recovered["lease_owner"], 0);

    let mut direct_final = model.init_state();
    assert!(model.fire("AcquireA", &mut direct_final));
    assert!(model.fire("AtomicFinalDeleteA", &mut direct_final));
    assert_eq!(direct_final["remote_token"], 0);
    assert_eq!(direct_final["lease_owner"], 0);

    // A delete whose response/mark is lost may be followed by a successor. The
    // stale A cleanup observes B and leaves its exact token untouched.
    let mut uncertain = model.init_state();
    assert!(model.fire("AcquireA", &mut uncertain));
    assert!(model.fire("DeleteALandsResponseLost", &mut uncertain));
    assert_eq!(uncertain["remote_token"], 0);
    assert!(model.fire("AcquireB", &mut uncertain));
    assert!(model.fire("ObserveStaleARelease", &mut uncertain));
    assert_eq!(uncertain["remote_token"], 2);

    let mut final_unlock = model.init_state();
    assert!(model.fire("AcquireA", &mut final_unlock));
    assert!(model.fire("AtomicFinalDeleteAResponseLost", &mut final_unlock));
    assert_eq!(final_unlock["lease_owner"], 0);
    assert!(model.fire("AcquireSuccessorB", &mut final_unlock));
    assert_eq!(final_unlock["lease_owner"], 2);
    assert_eq!(final_unlock["remote_fence_owner"], 2);
    assert!(model.fire("ObserveStaleARelease", &mut final_unlock));
    assert_eq!(final_unlock["remote_token"], 2);

    let mut incoherent = model.init_state();
    assert!(model.fire("ObserveIncoherentSuccessor", &mut incoherent));
    assert!(model.fire("RefuseIncoherentRemote", &mut incoherent));
    assert_eq!(incoherent["refused"], 1);

    let mut ambiguous = model.init_state();
    assert!(model.fire("ObserveAmbiguousRemote", &mut ambiguous));
    assert!(!model.action_enabled("AcquireA", &ambiguous));
    assert!(!model.action_enabled("AcquireB", &ambiguous));
    assert!(model.fire("RefuseAmbiguousRemote", &mut ambiguous));

    let mut active_ambiguity = model.init_state();
    assert!(model.fire("AcquireA", &mut active_ambiguity));
    assert!(model.fire("StopA", &mut active_ambiguity));
    assert!(model.fire("ObserveAmbiguousRemote", &mut active_ambiguity));
    assert!(!model.action_enabled("MutateA", &active_ambiguity));
    assert!(!model.action_enabled("ReleaseA", &active_ambiguity));
    assert!(!model.action_enabled("RotateAtoB", &active_ambiguity));
    assert!(model.fire("RefuseAmbiguousRemote", &mut active_ambiguity));

    // A's name may be reused after an ordinary release, but it denotes a new
    // process session. The old external stop proof must not survive reentry.
    let mut reentered = model.init_state();
    assert!(model.fire("AcquireA", &mut reentered));
    assert!(model.fire("StopA", &mut reentered));
    assert!(model.fire("ReleaseA", &mut reentered));
    assert!(model.fire("AcquireA", &mut reentered));
    assert_eq!(reentered["old_process_stopped"], 0);
    assert!(!model.action_enabled("RotateAtoB", &reentered));
    assert!(model.action_enabled("MutateA", &reentered));
    assert!(model.fire("StopA", &mut reentered));
    assert!(model.action_enabled("RotateAtoB", &reentered));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut stale_mutation = buggy.init_state();
    assert!(buggy.fire("AcquireA", &mut stale_mutation));
    assert!(buggy.fire("StopA", &mut stale_mutation));
    assert!(buggy.fire("RotateAtoB", &mut stale_mutation));
    assert!(buggy.fire("MutateStaleA", &mut stale_mutation));
    assert!(!buggy.check_invariant("StaleSessionCannotMutate", &stale_mutation));

    let mut stale_delete = buggy.init_state();
    assert!(buggy.fire("AcquireA", &mut stale_delete));
    assert!(buggy.fire("StopA", &mut stale_delete));
    assert!(buggy.fire("RotateAtoB", &mut stale_delete));
    assert!(buggy.fire("StaleADeletesB", &mut stale_delete));
    assert!(!buggy.check_invariant("StaleSessionCannotDeleteWinner", &stale_delete));

    let mut stale_rotation = buggy.init_state();
    assert!(buggy.fire("AcquireA", &mut stale_rotation));
    assert!(buggy.fire("StopA", &mut stale_rotation));
    assert!(buggy.fire("RotateAtoB", &mut stale_rotation));
    assert!(buggy.fire("StaleARotatesB", &mut stale_rotation));
    assert!(!buggy.check_invariant("StaleSessionCannotRotateWinner", &stale_rotation));

    let mut unsafe_recovery = buggy.init_state();
    assert!(buggy.fire("AcquireA", &mut unsafe_recovery));
    assert!(buggy.fire("RotateLiveAtoB", &mut unsafe_recovery));
    assert!(!buggy.check_invariant("RecoveryRequiresStoppedOldProcess", &unsafe_recovery));

    let mut unsafe_reentry = buggy.init_state();
    for action in ["AcquireA", "StopA", "ReleaseA"] {
        assert!(buggy.fire(action, &mut unsafe_reentry), "{action}");
    }
    assert!(!model.action_enabled("AcquireAReusingStoppedProof", &unsafe_reentry));
    assert!(buggy.fire("AcquireAReusingStoppedProof", &mut unsafe_reentry));
    assert_eq!(unsafe_reentry["old_process_stopped"], 1);
    assert!(buggy.fire("RotateAtoB", &mut unsafe_reentry));
    assert!(!buggy.check_invariant("StoppedProofIsPerProcess", &unsafe_reentry));

    let mut ambiguity_bypass = buggy.init_state();
    assert!(buggy.fire("ObserveAmbiguousRemote", &mut ambiguity_bypass));
    assert!(buggy.fire("AcquireAThroughAmbiguity", &mut ambiguity_bypass));
    assert!(!buggy.check_invariant("AmbiguousTransportCannotBeBypassed", &ambiguity_bypass));

    let mut active_ambiguity_bypass = buggy.init_state();
    assert!(buggy.fire("AcquireA", &mut active_ambiguity_bypass));
    assert!(buggy.fire("ObserveAmbiguousRemote", &mut active_ambiguity_bypass));
    assert!(buggy.fire("MutateAThroughAmbiguity", &mut active_ambiguity_bypass));
    assert!(!buggy.check_invariant(
        "AmbiguousTransportCannotBeBypassed",
        &active_ambiguity_bypass
    ));

    let mut lease_loss = buggy.init_state();
    assert!(buggy.fire("AcquireA", &mut lease_loss));
    assert!(buggy.fire("LosePersistentLease", &mut lease_loss));
    assert!(!model.action_enabled("MutateA", &lease_loss));
    assert!(buggy.fire("MutateAAfterLeaseLoss", &mut lease_loss));
    assert!(!buggy.check_invariant("MutationRequiresPersistentLease", &lease_loss));

    let mut incoherent_bypass = buggy.init_state();
    assert!(buggy.fire("ObserveIncoherentSuccessor", &mut incoherent_bypass));
    assert!(buggy.fire("AcceptIncoherentSuccessor", &mut incoherent_bypass));
    assert!(!buggy.check_invariant("IncoherentSuccessorCannotConverge", &incoherent_bypass));

    // The opposite failure: a coherent, unambiguous fence refused, which wedges
    // every resume behind a refusal no observed fault explains.
    let mut wedged = buggy.init_state();
    assert!(buggy.fire("AcquireA", &mut wedged));
    assert!(!model.action_enabled("RefuseWellFormedFence", &wedged));
    assert!(buggy.fire("RefuseWellFormedFence", &mut wedged));
    assert!(!buggy.check_invariant("RefusalHasObservedTransportFault", &wedged));
    assert_every_invariant_carries_a_mutant(&model, &["FenceStateBounds"]);
}

/// A pre-activation lease remains recoverable without reopening historical
/// publication: unpublished state is abandoned, while already-public state is
/// only finished under its original retired-key/unsigned identity.
#[test]
fn derived_historical_recovery_converges_without_republication() {
    let model = release_historical_recovery_model();
    assert_proves_and_catches(&model);
    assert_eq!(
        aterm_spec::verify::audit_dead_negative_controls(
            &model,
            &[
                "RepublishLegacyDuringRecovery",
                "FinishSignedLegacyWithCurrentKey",
                "AbandonUnknownAbsent",
                "AbandonIssuedAbsent",
                "DeleteUnknownDraft",
                "DeleteIssuedDraftWithoutCapabilityCheck",
                "ReleaseOwnerBeforeTagCleanup",
            ],
        ),
        Ok(7)
    );

    let mut abandoned = model.init_state();
    assert!(model.fire("LearnNoPostFromCurrentJournal", &mut abandoned));
    assert!(model.fire("AbandonProvenNoPost", &mut abandoned));
    assert_eq!(abandoned["phase"], 2);
    assert_eq!(abandoned["owner_held"], 0);

    let mut unsigned = model.init_state();
    assert!(model.fire("ObserveUnsignedPublishedLegacy", &mut unsigned));
    assert!(model.fire("FinishUnsignedPublishedLegacy", &mut unsigned));
    assert_eq!(unsigned["phase"], 3);
    assert_eq!(unsigned["selected_key"], 0);

    let mut signed = model.init_state();
    assert!(model.fire("ObserveSignedPublishedLegacy", &mut signed));
    assert_eq!(signed["selected_key"], 1);
    assert!(model.fire("FinishSignedPublishedLegacy", &mut signed));
    assert_eq!(signed["owner_held"], 0);

    let mut deleted = model.init_state();
    assert!(model.fire("LearnIssuedIntentFromCurrentJournal", &mut deleted));
    assert!(model.fire("ObserveExactDraft", &mut deleted));
    assert!(model.fire("DeleteExactDraft", &mut deleted));
    assert!(model.fire("AbandonDeletedIssuedDraft", &mut deleted));
    assert_eq!(deleted["owner_held"], 0);

    // A lost journal (728af7315): a draft bound to the claim's commit is this
    // claim's, so the remote's binding stands for issued intent; with nothing
    // visible, the operator's `--no-draft-was-posted` stands for a no-POST journal.
    // An unbound draft on a lost journal has neither, and nothing is enabled.
    let mut rebound = model.init_state();
    assert!(model.fire("ObserveExactDraft", &mut rebound));
    assert!(!model.action_enabled("DeleteExactDraft", &rebound));
    assert!(model.fire("LearnIssuedIntentFromClaimBinding", &mut rebound));
    assert!(model.fire("DeleteExactDraft", &mut rebound));
    assert!(model.fire("AbandonDeletedIssuedDraft", &mut rebound));
    assert_eq!(rebound["phase"], 2);
    let mut answered = model.init_state();
    assert!(model.fire("AbandonOnOperatorNoPostAnswer", &mut answered));
    assert_eq!((answered["phase"], answered["owner_held"]), (2, 0));
    let mut foreign = model.init_state();
    assert!(model.fire("ObserveUnboundDraft", &mut foreign));
    for action in [
        "LearnIssuedIntentFromClaimBinding",
        "DeleteExactDraft",
        "AbandonProvenNoPost",
        "AbandonOnOperatorNoPostAnswer",
    ] {
        assert!(!model.action_enabled(action, &foreign), "{action}");
    }

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut republished = buggy.init_state();
    assert!(buggy.fire("RepublishLegacyDuringRecovery", &mut republished));
    assert!(!buggy.check_invariant("RecoveryNeverPublishesRetiredEpoch", &republished));

    let mut wrong_key = buggy.init_state();
    assert!(buggy.fire("ObserveSignedPublishedLegacy", &mut wrong_key));
    assert!(buggy.fire("FinishSignedLegacyWithCurrentKey", &mut wrong_key));
    assert!(!buggy.check_invariant("SignedLegacyUsesOnlyRetiredKey", &wrong_key));
    assert!(!buggy.check_invariant("HistoricalKeySubstitutionCannotBeBypassed", &wrong_key));

    let mut delayed = buggy.init_state();
    assert!(buggy.fire("AbandonUnknownAbsent", &mut delayed));
    assert!(!buggy.check_invariant("AmbiguousAbsenceRetainsOwner", &delayed));
    assert!(!buggy.check_invariant("NoDelayedDraftAfterUnlock", &delayed));

    let mut issued_absent = buggy.init_state();
    assert!(buggy.fire("LearnIssuedIntentFromCurrentJournal", &mut issued_absent));
    assert!(buggy.fire("AbandonIssuedAbsent", &mut issued_absent));
    assert!(!buggy.check_invariant("AmbiguousAbsenceRetainsOwner", &issued_absent));
    assert!(!buggy.check_invariant("NoDelayedDraftAfterUnlock", &issued_absent));

    // The lost-journal row with its `claim_bound` conjunct dropped: someone
    // else's draft under this tag, deleted with no journal behind the delete.
    let mut legacy_duplicate = buggy.init_state();
    assert!(buggy.fire("ObserveUnboundDraft", &mut legacy_duplicate));
    assert!(buggy.fire("DeleteUnknownDraft", &mut legacy_duplicate));
    assert!(!buggy.check_invariant("DraftDeletionRequiresIssuedIntent", &legacy_duplicate));

    // An issued journal and an unbound draft: `draft_cleanup_decision` says
    // delete, and only the capability check refuses. Skipped, the delete lands.
    let mut unchecked = buggy.init_state();
    assert!(buggy.fire("LearnIssuedIntentFromCurrentJournal", &mut unchecked));
    assert!(buggy.fire("ObserveUnboundDraft", &mut unchecked));
    assert!(!model.action_enabled("DeleteExactDraft", &unchecked));
    assert!(buggy.fire("DeleteIssuedDraftWithoutCapabilityCheck", &mut unchecked));
    assert!(buggy.check_invariant("DraftDeletionRequiresIssuedIntent", &unchecked));
    assert!(!buggy.check_invariant("DeletedDraftTargetsTheClaim", &unchecked));

    // The owner released ahead of the tag cleanup: recovery has not finished,
    // and the lease is already free for a successor to find the half-cleaned tag.
    let mut early_owner_release = buggy.init_state();
    for action in [
        "LearnIssuedIntentFromCurrentJournal",
        "ObserveExactDraft",
        "DeleteExactDraft",
        "ReleaseOwnerBeforeTagCleanup",
    ] {
        assert!(buggy.fire(action, &mut early_owner_release), "{action}");
    }
    assert_eq!(early_owner_release["phase"], 0);
    assert!(!buggy.check_invariant("CompletionReleasesOwner", &early_owner_release));
    assert_every_invariant_carries_a_mutant(&model, &["HistoricalRecoveryBounds"]);
}

/// A published release's captured target may be symbolic, but mutation still
/// requires the byte-exact snapshot and tag-to-manifest binding to remain true.
#[test]
fn derived_published_identity_accepts_symbolic_history_and_rejects_drift() {
    let model = release_published_identity_model();
    assert_proves_and_catches(&model);

    let mut valid = model.init_state();
    assert!(model.fire("AcceptSymbolicHistory", &mut valid));
    assert_eq!(valid["history_accepted"], 1);
    assert!(model.fire("DeleteWithExactPublishedIdentity", &mut valid));
    assert!(model.check_invariant("DeleteRequiresExactSnapshotAndTag", &valid));

    let mut target_drift = model.init_state();
    assert!(model.fire("AcceptSymbolicHistory", &mut target_drift));
    assert!(model.fire("DriftCapturedTarget", &mut target_drift));
    assert!(!model.action_enabled("DeleteWithExactPublishedIdentity", &target_drift));
    assert!(model.fire("RefuseTargetDrift", &mut target_drift));

    let mut tag_drift = model.init_state();
    assert!(model.fire("AcceptSymbolicHistory", &mut tag_drift));
    assert!(model.fire("DriftResolvedTag", &mut tag_drift));
    assert!(!model.action_enabled("DeleteWithExactPublishedIdentity", &tag_drift));
    assert!(model.fire("RefuseTagDrift", &mut tag_drift));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut false_rejection = buggy.init_state();
    assert!(buggy.fire("RejectValidSymbolicHistoryAsNonSha", &mut false_rejection));
    assert!(!buggy.check_invariant("ValidSymbolicHistoryIsNotRejected", &false_rejection));

    let mut unbound = buggy.init_state();
    assert!(buggy.fire("AcceptUnboundSymbolicWithoutTag", &mut unbound));
    assert!(!buggy.check_invariant("UnboundSymbolicHistoryFailsClosed", &unbound));

    let mut ignored_target = buggy.init_state();
    assert!(buggy.fire("AcceptSymbolicHistory", &mut ignored_target));
    assert!(buggy.fire("DriftCapturedTarget", &mut ignored_target));
    assert!(buggy.fire("DeleteIgnoringTargetDrift", &mut ignored_target));
    assert!(!buggy.check_invariant("TargetDriftCannotBeBypassed", &ignored_target));
    assert!(!buggy.check_invariant("DeleteRequiresExactSnapshotAndTag", &ignored_target));

    let mut ignored_tag = buggy.init_state();
    assert!(buggy.fire("AcceptSymbolicHistory", &mut ignored_tag));
    assert!(buggy.fire("DriftResolvedTag", &mut ignored_tag));
    assert!(buggy.fire("DeleteIgnoringTagDrift", &mut ignored_tag));
    assert!(!buggy.check_invariant("TagDriftCannotBeBypassed", &ignored_tag));
}

/// Yank is a poison-first protocol: a verified newer floor makes the target inert,
/// exact-CAS tag deletion happens while the release is still a durable identity
/// receipt, and only then may convergent release deletion run. Response loss and a
/// crash after either mutation remain resumable.
#[test]
fn derived_release_yank_is_successor_first_and_crash_convergent() {
    let model = release_yank_successor_first_model();
    assert_proves_and_catches(&model);

    let mut state = model.init_state();
    assert!(model.fire("PublishVerifiedSuccessor", &mut state));
    assert!(!model.action_enabled("TagDeleteLandsResponseLost", &state));
    assert!(model.fire("AcquireCleanupLease", &mut state));
    assert!(model.fire("AcquireCleanupFence", &mut state));
    assert!(model.fire("ReproveVerifiedSuccessor", &mut state));
    assert!(model.fire("TagDeleteLandsResponseLost", &mut state));
    assert_eq!(state["bad_tag_present"], 0);
    assert_eq!(state["bad_release_present"], 1);
    assert!(model.fire("CrashDuringCleanup", &mut state));
    assert_eq!(state["target_known"], 0);
    assert!(model.fire("RediscoverTargetFromPublishedReceipt", &mut state));
    assert!(model.fire("ProveCleanupPublisherStopped", &mut state));
    assert!(model.fire("RecoverAndReleaseCleanupSession", &mut state));
    assert!(model.fire("AcquireCleanupLease", &mut state));
    assert!(model.fire("AcquireCleanupFence", &mut state));
    assert!(model.fire("ReproveVerifiedSuccessor", &mut state));
    assert!(model.fire("ReleaseDeleteLandsResponseLost", &mut state));
    assert_eq!(state["bad_release_present"], 0);
    assert!(model.fire("CrashDuringCleanup", &mut state));
    assert!(model.fire("ConvergeObservedAbsent", &mut state));
    assert_eq!(state["cleanup_complete"], 1);
    assert!(model.check_invariant("CompleteMeansConverged", &state));
    assert!(model.fire("ProveCleanupPublisherStopped", &mut state));
    assert!(model.fire("RecoverAndReleaseCleanupSession", &mut state));

    // The non-crashing path keeps both refs through observed convergence and
    // releases them atomically. A completed cleanup cannot reacquire them.
    let mut clean = model.init_state();
    for action in [
        "PublishVerifiedSuccessor",
        "AcquireCleanupLease",
        "AcquireCleanupFence",
        "ReproveVerifiedSuccessor",
        "DeleteExactTagAfterSuccessor",
        "ReproveVerifiedSuccessor",
        "DeleteReleaseAfterTag",
        "ReleaseCleanupSession",
    ] {
        assert!(model.fire(action, &mut clean), "{action}");
    }
    assert_eq!(clean["cleanup_complete"], 1);
    assert_eq!(clean["cleanup_session_released"], 1);
    assert!(!model.action_enabled("AcquireCleanupLease", &clean));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    let mut delete_first = buggy.init_state();
    assert!(buggy.fire("DeleteTagBeforeSuccessor", &mut delete_first));
    assert!(!buggy.check_invariant("TagDeletionRequiresVerifiedSuccessor", &delete_first));
    assert!(!buggy.check_invariant("SuccessorMustPrecedeCleanup", &delete_first));

    let mut weak_floor = buggy.init_state();
    assert!(buggy.fire("DeleteTagWithWeakFloor", &mut weak_floor));
    assert!(!buggy.check_invariant("TagDeletionRequiresVerifiedSuccessor", &weak_floor));
    assert!(!buggy.check_invariant("RequiredFloorCannotBeWeakened", &weak_floor));

    let mut wrong_identity = buggy.init_state();
    assert!(buggy.fire("PublishVerifiedSuccessor", &mut wrong_identity));
    assert!(buggy.fire("ObserveTargetIdentityMismatch", &mut wrong_identity));
    assert!(!model.action_enabled("DeleteExactTagAfterSuccessor", &wrong_identity));
    assert!(buggy.fire("DeleteTagWithWrongIdentity", &mut wrong_identity));
    assert!(!buggy.check_invariant("ExactIdentityCannotBeBypassed", &wrong_identity));

    let mut release_first = buggy.init_state();
    assert!(buggy.fire("PublishVerifiedSuccessor", &mut release_first));
    assert!(buggy.fire("DeleteReleaseFirstAfterSuccessor", &mut release_first));
    assert!(!buggy.check_invariant("ReleaseDeletionRequiresTagGone", &release_first));
    assert!(!buggy.check_invariant("ReceiptSurvivesUntilTagGone", &release_first));
    assert!(!buggy.check_invariant("ReleaseFirstOrderingIsForbidden", &release_first));

    let mut lease_loss = buggy.init_state();
    for action in [
        "PublishVerifiedSuccessor",
        "AcquireCleanupLease",
        "AcquireCleanupFence",
        "ReproveVerifiedSuccessor",
        "LoseCleanupLease",
    ] {
        assert!(buggy.fire(action, &mut lease_loss), "{action}");
    }
    assert!(!model.action_enabled("DeleteExactTagAfterSuccessor", &lease_loss));
    assert!(buggy.fire("DeleteTagAfterCleanupLeaseLoss", &mut lease_loss));
    assert!(!buggy.check_invariant("TagDeletionHeldUniqueCleanupSession", &lease_loss));
    assert!(!buggy.check_invariant("CleanupSessionCannotBeBypassed", &lease_loss));

    let mut early_release = buggy.init_state();
    for action in [
        "PublishVerifiedSuccessor",
        "AcquireCleanupLease",
        "AcquireCleanupFence",
    ] {
        assert!(buggy.fire(action, &mut early_release), "{action}");
    }
    assert!(!model.action_enabled("ReleaseCleanupSession", &early_release));
    assert!(buggy.fire("ReleaseCleanupSessionEarly", &mut early_release));
    assert!(!buggy.check_invariant("CleanupSessionReleasesOnlyAfterConvergence", &early_release));
    assert!(!buggy.check_invariant("EarlySessionReleaseIsForbidden", &early_release));

    // A convergence probe that reads only the tag: cleanup declared complete
    // while the bad release is still listed.
    let mut tag_only = buggy.init_state();
    for action in [
        "PublishVerifiedSuccessor",
        "AcquireCleanupLease",
        "AcquireCleanupFence",
        "ReproveVerifiedSuccessor",
        "DeleteExactTagAfterSuccessor",
    ] {
        assert!(buggy.fire(action, &mut tag_only), "{action}");
    }
    assert!(!model.action_enabled("ConvergeObservedAbsent", &tag_only));
    assert!(buggy.fire("ConvergeOnTagAbsenceOnly", &mut tag_only));
    assert!(!buggy.check_invariant("CompleteMeansConverged", &tag_only));
    assert_every_invariant_carries_a_mutant(&model, &["YankStateBounds"]);
}

#[test]
fn release_channel_models_are_registered_for_xref_resolution() {
    let registered: std::collections::BTreeSet<_> = aterm_spec::xref::model_registry()
        .into_iter()
        .map(|model| model.name)
        .collect();
    for expected in [
        "ReleaseDurablePostIntent",
        "ReleaseChannelFloor",
        "ReleaseJournalPrefix",
        "ReleasePublisherFence",
        "ReleasePublishedIdentity",
        "ReleaseYankSuccessorFirst",
        "ReleasePublishOnce",
        "NativeUpdateHiddenOutputQuiet",
    ] {
        assert!(
            registered.contains(expected),
            "{expected} must resolve through the spec↔source registry"
        );
    }
}

/// Foreground terminal work is carried across a seamless updater handoff. It
/// cannot be classified like dirty native UI state, while a failed handoff may
/// never fall back to a destructive cold re-exec with that work still live.
#[test]
fn derived_native_update_admission_proves_and_catches_foreground_blocker() {
    let model = native_update_admission_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let foreground = buggy.successors("ObserveForegroundJob", &buggy.init_state())[0].clone();
    let blocked = buggy.successors("BlockForegroundDespiteSeamless", &foreground)[0].clone();
    assert!(!buggy.check_invariant("ForegroundJobsDoNotBlockSeamless", &blocked));

    // A seamless replacement that does not carry the job's PTY across.
    let authorized = model.successors("ClassifySeamless", &foreground)[0].clone();
    let hung_up = buggy.successors("HandoffWithoutAdoptingForeground", &authorized)[0].clone();
    assert!(!buggy.check_invariant("ReplacementPreservesForeground", &hung_up));

    // `classify`'s live-session check dropped in a release macOS build, whose
    // cold arm re-checks only by a compiled-out `debug_assert!`: the destructive
    // re-exec runs over the live job. The healthy completion re-tests it.
    let no_lane = buggy.successors("LoseSeamlessLane", &foreground)[0].clone();
    let dropped = buggy.successors("ReexecColdOverLiveSessions", &no_lane)[0].clone();
    assert!(!buggy.check_invariant("ColdFallbackNeverDropsForeground", &dropped));
    assert!(!buggy.check_invariant("ReplacementPreservesForeground", &dropped));
    let mut cold_decided = no_lane.clone();
    cold_decided.insert("phase", 1);
    cold_decided.insert("decision", 2);
    assert!(
        model
            .successors("CompleteColdFallback", &cold_decided)
            .is_empty(),
        "the healthy cold completion re-checks the live job"
    );

    // Its native-state check dropped: a dirty editor rides a re-exec.
    let dirty = buggy.successors("ObserveUnsafeNativeState", &buggy.init_state())[0].clone();
    let over_dirty = buggy.successors("ClassifySeamlessOverUncertifiedState", &dirty)[0].clone();
    let reexecuted = buggy.successors("CompleteSeamlessHandoff", &over_dirty)[0].clone();
    assert!(!buggy.check_invariant("UnsafeStateNeverReexecutes", &reexecuted));

    // 0dea6c38c: a block filed as a failed handoff latches the build off.
    let latched = buggy.successors("BlockLatchesWithoutRetry", &no_lane)[0].clone();
    assert!(!buggy.check_invariant("BlockedIsRetryableWithoutReexec", &latched));
    assert_every_invariant_carries_a_mutant(&model, &["AttemptsBounded"]);
    assert_committed_dead_are_caught_mutants(
        &model,
        &[
            "BlockForegroundDespiteSeamless",
            "ClassifySeamlessOverUncertifiedState",
            "ReexecColdOverLiveSessions",
            "BlockLatchesWithoutRetry",
            "HandoffWithoutAdoptingForeground",
        ],
    );
}

/// A stage notification cannot disappear behind an active manual check. The
/// retained intent becomes eligible when completion imports the stage, and any
/// unsuccessful apply returns to that same bounded retry state.
#[test]
fn derived_native_update_auto_intent_proves_and_catches_lost_stage_wake() {
    let model = native_update_auto_intent_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let checking = buggy.successors("StartManualCheck", &buggy.init_state())[0].clone();
    let lost = buggy.successors("StageWakeDroppingIntent", &checking)[0].clone();
    assert!(!buggy.check_invariant("StageDuringCheckRetainsIntent", &lost));

    let newer = buggy.successors("ArmNewerIntent", &buggy.init_state())[0].clone();
    let stale = buggy.successors("StaleWakeClearsNewerIntent", &newer)[0].clone();
    assert!(!buggy.check_invariant("NewerIntentSurvivesStaleWake", &stale));

    // The park with neither quiet nor a closed window, and a Commit over a screen
    // nobody froze.
    let ready = buggy.successors("StageWakeIdle", &buggy.init_state())[0].clone();
    let quiet = buggy.successors("QuietElapsed", &ready)[0].clone();
    let attempting = buggy.successors("Attempt", &quiet)[0].clone();
    let busy_again = buggy.successors("HoldActivity", &attempting)[0].clone();
    assert!(model.successors("ParkReaders", &busy_again).is_empty());
    let parked_busy = buggy.successors("ParkWithoutQuietOrGrace", &busy_again)[0].clone();
    assert!(!buggy.check_invariant(
        "AutomaticAttemptRequiresQuietOrClosedGraceWindow",
        &parked_busy
    ));
    let unparked = buggy.successors("AcceptWithoutPark", &attempting)[0].clone();
    assert!(!buggy.check_invariant("AcceptedRequiresParkedReaders", &unparked));

    // An attempt that did not replace spends the intent; a physical failure
    // latches manual-only with the intent still armed. The healthy endings stay
    // reachable in the same `Buggy=1` world, so a retry after a non-replacing
    // attempt is still there for a later mutant to break.
    let spent = buggy.successors("AttemptDidNotReplaceSpendingIntent", &attempting)[0].clone();
    assert!(!buggy.check_invariant("UnsuccessfulAttemptRetainsIntent", &spent));
    let armed_latch =
        buggy.successors("AttemptPhysicalFailureKeepingIntent", &attempting)[0].clone();
    assert!(!buggy.check_invariant("PhysicalFailureIsManualOnly", &armed_latch));
    let retryable = buggy.successors("AttemptDidNotReplace", &attempting)[0].clone();
    assert!(!buggy.successors("Attempt", &retryable).is_empty());
    assert_every_invariant_carries_a_mutant(&model, &["DeferralsBounded", "AttemptsBounded"]);
    assert_committed_dead_are_caught_mutants(
        &model,
        &[
            "StageWakeDroppingIntent",
            "StaleWakeClearsNewerIntent",
            "ParkWithoutQuietOrGrace",
            "AttemptDidNotReplaceSpendingIntent",
            "AttemptPhysicalFailureKeepingIntent",
            "AcceptWithoutPark",
        ],
    );
}

/// THE APPLY LADDER: a never-quiet terminal lands at the bound and activity
/// never latches the automatic lane manual-only. The mutant is the 2026-09-20
/// incident — a busy terminal stood down past the typing hold — and it WEDGES
/// before landing, which is exactly what the owner's aterm did eleven times.
#[test]
fn derived_native_update_apply_ladder_lands_a_busy_terminal_and_catches_the_stand_down() {
    let model = native_update_apply_ladder_model();
    assert_proves_and_catches(&model);

    // The healthy ladder never wedges before landing…
    let landed = |state: &aterm_spec::interp::State| state["landed"] == 1;
    assert!(
        aterm_spec::interp::find_deadlock(&model, landed).is_none(),
        "the healthy ladder always reaches a landing"
    );
    // …and a busy terminal that never goes quiet lands at the bound: walk the
    // clock with the terminal streaming and focused, keys up.
    let mut busy = model.init_state();
    for _ in 0..3 {
        assert!(
            model.successors("Park", &busy).is_empty() || busy["phase"] >= 2,
            "no park before KeysOnly on a focused, streaming terminal: {busy:?}"
        );
        busy = model.successors("Advance", &busy)[0].clone();
    }
    assert_eq!(busy["phase"], 3);
    let parked = model.successors("Park", &busy)[0].clone();
    assert_eq!(parked["landed"], 1);
    assert!(model.check_invariant("ParkedOnlyWhenTheLadderAdmits", &parked));

    // The incident: the mutant stands the busy terminal down and wedges.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let wedge = aterm_spec::interp::find_deadlock(&buggy, landed)
        .expect("the stand-down wedges the mutant before it lands");
    assert_eq!(wedge["manual_only"], 1);
    assert_eq!(wedge["landed"], 0);
    assert!(!buggy.check_invariant("ActivityNeverLatchesManualOnly", &wedge));
    // And the mutant's ruleless park is the other catch — its own dead action,
    // never enabled at the committed config.
    let mut mid_word = buggy.init_state();
    mid_word.insert("keys", 0);
    assert!(
        model.successors("ParkWithoutTheRule", &mid_word).is_empty(),
        "the healthy ladder has no park that skips the rule"
    );
    let ruleless = buggy.successors("ParkWithoutTheRule", &mid_word)[0].clone();
    assert!(!buggy.check_invariant("ParkedOnlyWhenTheLadderAdmits", &ruleless));

    // THE 2026-09-21 AUDIT. A genuine physical failure at KeysOnly latches the
    // lane; the clock keeps running underneath the latch; the lapse resumes
    // the ladder at the phase the clock reached — never a fresh one.
    let mut keys_only = model.init_state();
    for _ in 0..2 {
        keys_only = model.successors("Advance", &keys_only)[0].clone();
    }
    assert_eq!(keys_only["phase"], 2);
    let latched = model.successors("PhysicalFailure", &keys_only)[0].clone();
    assert_eq!(latched["latched"], 1);
    assert!(
        model.successors("Park", &latched).is_empty(),
        "no park while a physical latch holds"
    );
    let aged = model.successors("Advance", &latched)[0].clone();
    assert_eq!(aged["phase"], 3, "the clock advances under the latch");
    assert!(
        model.successors("Lapse", &aged).is_empty(),
        "no lapse before the latch's deadline (plan P0-6)"
    );
    let due = model.successors("Due", &aged)[0].clone();
    let lapsed = model.successors("Lapse", &due)[0].clone();
    assert_eq!(lapsed["latched"], 0);
    assert_eq!(
        lapsed["phase"], 3,
        "the lapse keeps the phase the clock reached"
    );
    assert!(model.check_invariant("TheLadderNeverRestarts", &lapsed));
    assert!(model.check_invariant("NoEarlyRelease", &lapsed));
    assert_eq!(
        model.successors("Park", &lapsed)[0]["landed"],
        1,
        "and a lapse at Land lands at the next poll"
    );
    assert!(
        model.successors("LapseRestartsTheLadder", &due).is_empty(),
        "the healthy ladder has no lapse that restarts it"
    );
    let restarted = buggy.successors("LapseRestartsTheLadder", &due)[0].clone();
    assert_eq!(
        restarted["phase"], 0,
        "the mutant: a 600 s latch bought a fresh ladder"
    );
    assert!(!buggy.check_invariant("TheLadderNeverRestarts", &restarted));

    // A park that missed its budget is the machine being busy: the lane keeps
    // its phase and is gated afresh. The mutant files it as a failure and
    // latches — the same wedge as the incident, from a stopwatch.
    let missed = model.successors("ParkMissed", &keys_only)[0].clone();
    assert_eq!(missed["phase"], 2);
    assert_eq!(missed["latched"], 0);
    assert_eq!(missed["manual_only"], 0);
    assert!(model.check_invariant("ActivityNeverLatchesManualOnly", &missed));
    assert!(
        !model.successors("Park", &missed).is_empty(),
        "the re-park is gated afresh at the same phase"
    );
    assert!(model.successors("ParkMissLatches", &keys_only).is_empty());
    let miss_latched = buggy.successors("ParkMissLatches", &keys_only)[0].clone();
    assert!(!buggy.check_invariant("ActivityNeverLatchesManualOnly", &miss_latched));

    // THE CONSENT WARM-UP (2026-09-23): the one hold no phase relaxes. At the
    // bound, a warm-up the user started blocks every admitted park; its end —
    // always enabled while it holds — lets the landing through. The mutant's
    // ruleless park lands on top of the dialog and is caught.
    let mut at_bound = model.init_state();
    for _ in 0..3 {
        at_bound = model.successors("Advance", &at_bound)[0].clone();
    }
    let warming = model.successors("WarmupStarts", &at_bound)[0].clone();
    for park in ["Park", "ParkMissed", "PhysicalFailure"] {
        assert!(
            model.successors(park, &warming).is_empty(),
            "no {park} over the user's warm-up, even at Land"
        );
    }
    let ended = model.successors("WarmupEnds", &warming)[0].clone();
    assert_eq!(model.successors("Park", &ended)[0]["landed"], 1);
    let over_dialog = buggy.successors("ParkWithoutTheRule", &warming)[0].clone();
    assert!(!buggy.check_invariant("ParkedOnlyWhenTheLadderAdmits", &over_dialog));

    // The capture-refusal half is walked by its own function, and pinned by
    // the `assert_proves_and_catches` above (one run per machine: two tests
    // checking the same machine concurrently race over its emitted module in
    // the external `ty` tier).
    ladder_never_retries_a_capture_refusal_as_activity(&model);
    ladder_keeps_a_latch_across_its_own_bundle_swap(&model, &latched);
}

/// THE 2026-09-22/23 UPDATE AUDIT (plan P0-6): a physical latch survives the
/// failed candidate's own bundle swap. On the launched lane the candidate
/// boot-applies the bundle before it dials, so the reconcile after its failure
/// retires the download for the installed-bundle activation of the same update.
/// The healthy lane re-keys the latch and keeps its deadline; the mutant — what
/// v0.87–v0.91 shipped — clears it, the activation arms half a second later,
/// and a structural failure's ten-minute confirming retry runs at once.
/// `NoEarlyRelease` catches it.
fn ladder_keeps_a_latch_across_its_own_bundle_swap(
    model: &Model,
    latched: &aterm_spec::interp::State,
) {
    assert_eq!((latched["latched"], latched["due"]), (1, 0));
    let swapped = model.successors("BundleSwap", latched)[0].clone();
    assert_eq!(swapped["swapped"], 1);
    assert_eq!(
        swapped["latched"], 1,
        "the swap re-keys the latch, never clears it"
    );
    assert!(model.check_invariant("NoEarlyRelease", &swapped));
    assert!(
        model.successors("Park", &swapped).is_empty()
            && model.successors("Lapse", &swapped).is_empty(),
        "nothing parks and nothing lapses before the deadline"
    );
    let due = model.successors("Due", &swapped)[0].clone();
    let lapsed = model.successors("Lapse", &due)[0].clone();
    assert_eq!(
        lapsed["latched"], 0,
        "the deadline releases it, and only the deadline"
    );
    assert!(model.check_invariant("NoEarlyRelease", &lapsed));

    // THE MUTANT: the retirement clears the latch before its deadline.
    let buggy = aterm_spec::interp::with_buggy(model, 1);
    assert!(
        model
            .successors("BundleSwapClearsLatch", latched)
            .is_empty(),
        "the healthy ladder has no swap that clears the latch"
    );
    let cleared = buggy.successors("BundleSwapClearsLatch", latched)[0].clone();
    assert_eq!((cleared["latched"], cleared["due"]), (0, 0));
    assert!(!buggy.check_invariant("NoEarlyRelease", &cleared));
    assert!(
        !buggy.successors("Park", &cleared).is_empty(),
        "the early release re-opens the park at once — the half-second retry"
    );
}

/// THE 2026-09-22/23 UPDATE AUDIT (plan P0-3): a park the capture REFUSES is a
/// fact about the desk, never about the moment. The healthy ladder answers for
/// it — the refusing session is carried at a degraded rung and the park lands —
/// and a refusal is never filed as the machine being busy. The mutant is the
/// v0.91 shape: the refusal re-filed as a park miss, stood down as activity and
/// retried every fifteen minutes into the same refusal, with nothing on any
/// surface. `RefusalNeverRetriesAsActivity` catches it.
fn ladder_never_retries_a_capture_refusal_as_activity(model: &Model) {
    // Walk the clock to Land (the phase where nothing else holds a park back)
    // and let the desk refuse.
    let mut land = model.init_state();
    for _ in 0..3 {
        land = model.successors("Advance", &land)[0].clone();
    }
    assert_eq!(land["phase"], 3);
    let refusing = model.successors("DeskRefuses", &land)[0].clone();
    assert_eq!(refusing["refusing"], 1);
    assert!(
        model.successors("Park", &refusing).is_empty(),
        "a refusing desk does not park before the capture has answered for it"
    );

    // The healthy capture answers with a degraded carry, and the park lands.
    let answered = model.successors("CaptureRefused", &refusing)[0].clone();
    assert_eq!(answered["degraded"], 1);
    assert_eq!(answered["refusals"], 0);
    assert_eq!(
        answered["quiet"], refusing["quiet"],
        "never filed as activity"
    );
    assert!(model.check_invariant("RefusalNeverRetriesAsActivity", &answered));
    let landed = model.successors("Park", &answered)[0].clone();
    assert_eq!(landed["landed"], 1);
    assert!(model.check_invariant("ParkedOnlyWhenTheLadderAdmits", &landed));

    // A desk that changed invalidates the degraded carry: the next refusal is
    // answered afresh rather than parked on the old answer.
    let moved = model.successors("DeskChanges", &answered)[0].clone();
    assert_eq!((moved["refusing"], moved["degraded"]), (0, 0));

    // The healthy ladder still never wedges before landing.
    let is_landed = |state: &aterm_spec::interp::State| state["landed"] == 1;
    assert!(aterm_spec::interp::find_deadlock(model, is_landed).is_none());

    // THE MUTANT: the same refusal re-filed as the machine being busy.
    let buggy = aterm_spec::interp::with_buggy(model, 1);
    let mut quiet_refusing = refusing.clone();
    quiet_refusing.insert("quiet", 1);
    assert!(
        model
            .successors("CaptureRefusedAsActivity", &quiet_refusing)
            .is_empty(),
        "the healthy ladder has no refusal it files as activity"
    );
    let refiled = buggy.successors("CaptureRefusedAsActivity", &quiet_refusing)[0].clone();
    assert_eq!(
        refiled["quiet"], 0,
        "the mutant reads the refusal as activity"
    );
    assert_eq!(refiled["refusals"], 1);
    assert_eq!(refiled["degraded"], 0, "and never answers for the session");
    assert!(!buggy.check_invariant("RefusalNeverRetriesAsActivity", &refiled));
    assert!(
        buggy.successors("Park", &refiled).is_empty(),
        "the mutant's refusing desk never parks until the desk itself moves"
    );
}

/// A hidden tab may never present after its output wake. Its old latency sample
/// cannot become a permanent updater gate, and every activity wait must schedule
/// a deadline later than the poll that created it.
#[test]
fn derived_native_update_hidden_output_quiet_proves_liveness_and_future_retry() {
    let model = native_update_hidden_output_quiet_model();
    assert_proves_and_catches(&model);

    let mut healthy = model.init_state();
    for action in ["HiddenOutput", "WakeHandledNoPresent", "PollRecentActivity"] {
        assert!(model.fire(action, &mut healthy), "healthy trace: {action}");
    }
    assert!(model.check_invariant("ActivityRetryIsStrictlyFuture", &healthy));
    assert!(model.fire("QuietEpochElapses", &mut healthy));
    assert_eq!(healthy["presentation_stamp"], 1);
    assert!(model.check_invariant("OldHiddenPresentationCannotGate", &healthy));
    assert!(model.fire("Attempt", &mut healthy));
    assert_eq!(healthy["attempted"], 1);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut stuck = buggy.init_state();
    for action in [
        "HiddenOutput",
        "WakeHandledNoPresent",
        "PollRecentActivity",
        "QuietGatedOnPresentationSample",
    ] {
        assert!(buggy.fire(action, &mut stuck), "mutant trace: {action}");
    }
    assert!(!buggy.check_invariant("OldHiddenPresentationCannotGate", &stuck));
    assert!(!buggy.check_invariant("ActivityRetryIsStrictlyFuture", &stuck));

    // Idleness read off the last present: the hidden tab streaming output it
    // never presents is computed quiet with its output 0 ticks old, and the
    // ORDINARY attempt then lands mid-output. The flag it sets breaks nothing on
    // its own; the clock law sees the attempt.
    let mut mid_output = buggy.init_state();
    for action in [
        "HiddenOutput",
        "WakeHandledNoPresent",
        "QuietFromLastPresent",
    ] {
        assert!(
            buggy.fire(action, &mut mid_output),
            "mutant trace: {action}"
        );
    }
    for invariant in &model.invariants {
        assert!(buggy.check_invariant(invariant.name, &mid_output));
    }
    assert!(buggy.fire("Attempt", &mut mid_output));
    assert_eq!(mid_output["now_tick"], mid_output["latest_output_tick"]);
    assert!(!buggy.check_invariant("AttemptOnlyAfterAgedQuiet", &mid_output));

    // The wrong fix: the wake closes the latency sample with no present.
    let mut acked = buggy.init_state();
    for action in ["HiddenOutput", "AckSampleOnHiddenWake"] {
        assert!(buggy.fire(action, &mut acked), "mutant trace: {action}");
    }
    assert!(!buggy.check_invariant("HiddenSampleRemainsUnacknowledged", &acked));
    assert_every_invariant_carries_a_mutant(&model, &["Bounds"]);
    assert_committed_dead_are_caught_mutants(
        &model,
        &[
            "QuietGatedOnPresentationSample",
            "QuietFromLastPresent",
            "AckSampleOnHiddenWake",
        ],
    );
}

#[test]
fn derived_native_update_attempt_identity_proves_and_catches_stale_abort() {
    let model = native_update_attempt_identity_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let first = buggy.successors("StartAttempt", &buggy.init_state())[0].clone();
    let retryable = buggy.successors("AbortCurrent", &first)[0].clone();
    let retry = buggy.successors("StartAttempt", &retryable)[0].clone();
    assert_eq!(
        model.successors("ReplayOldAbort", &retry),
        std::slice::from_ref(&retry),
        "a replayed old failure is inert"
    );
    let canceled = buggy.successors("AcceptStaleAbort", &retry)[0].clone();
    assert!(!buggy.check_invariant("StaleAbortCannotCancelRetry", &canceled));

    // The retry re-armed under the failed attempt's own nonce.
    let reused = buggy.successors("RetryReusingFailedNonce", &retryable)[0].clone();
    assert!(!buggy.check_invariant("RetryUsesFreshIdentity", &reused));

    // A failure that returns to retryable with the dead nonce still active.
    let lingering = buggy.successors("AbortKeepingActiveIdentity", &first)[0].clone();
    assert!(!buggy.check_invariant("ActiveIdentityIsCurrent", &lingering));
    assert_every_invariant_carries_a_mutant(&model, &["NonceBounded", "AbortsBounded"]);
}

/// The process-wide native-update facts queue has capacity one. Saturation is
/// accepted only because the request is retained in a coalesced latch and every
/// worker dequeue produces a retry edge; disconnection has one bounded restart
/// and then becomes an explicit unavailable result. The mutant reproduces both
/// historical loss mechanisms: dropping the full-queue latch and dropping the
/// dequeue edge that releases it.
#[test]
fn derived_native_update_worker_queue_proves_and_catches_lost_latch_or_drain() {
    let model = native_update_worker_queue_model();
    assert_proves_and_catches(&model);
    assert!(
        aterm_spec::interp::find_deadlock(&model, |_| false).is_none(),
        "healthy queue protocol must always complete, fail explicitly, or settle"
    );

    // An event-loop park with no retained reconcile intent is a genuine action,
    // not an omitted no-op: it performs neither proxy/wake materialization nor
    // warning/log work. Pin its independent mutant before exercising saturation.
    let idle = model.successors("ParkIdle", &model.init_state())[0].clone();
    assert_eq!(idle.get("idle_proxy_wakes"), Some(&0));
    assert_eq!(idle.get("idle_warnings"), Some(&0));
    assert!(model.check_invariant("IdleParkHasNoProxyWake", &idle));
    assert!(model.check_invariant("IdleParkHasNoWarning", &idle));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let polluted_idle =
        buggy.successors("ParkIdleMaterializingProxy", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("IdleParkHasNoProxyWake", &polluted_idle));
    assert!(!buggy.check_invariant("IdleParkHasNoWarning", &polluted_idle));
    let occupied = buggy.successors("OccupyWorker", &buggy.init_state())[0].clone();
    let silently_lost = buggy.successors("DropApplyLatchWhenFull", &occupied)[0].clone();
    assert!(!buggy.check_invariant("NoSilentlyLostAcceptedIntent", &silently_lost));
    assert!(!buggy.check_invariant("ApplyPurposeSurvivesCoalescing", &silently_lost));

    // The lost-wake mutation, now its own action: the healthy pending latch
    // survives, and the dequeue omits the edge that would release it.
    let pending = buggy.successors("RequestStageFull", &occupied)[0].clone();
    let lost_edge = buggy.successors("DrainWithoutRetryEdge", &pending)[0].clone();
    assert!(!buggy.check_invariant("PendingEmptyQueueHasRetryEdge", &lost_edge));

    // Unavailable reported with the latch left set: the next turn restarts and
    // delivers the same request, which is now settled both ways.
    let disconnected = buggy.successors("DisconnectWithPending", &pending)[0].clone();
    let mut twice = buggy.successors("ReportUnavailableKeepingLatch", &disconnected)[0].clone();
    for action in [
        "RestartPendingSuccess",
        "WorkerCompletesIntent",
        "ReduceCompletion",
    ] {
        assert!(buggy.fire(action, &mut twice), "{action}");
    }
    assert!(!buggy.check_invariant("SettlementIsExplicit", &twice));

    // A failed restart retried on every event turn: the hot loop.
    let mut spinning = disconnected.clone();
    for _ in 0..2 {
        assert!(buggy.fire("RetryRestartEveryTurn", &mut spinning));
    }
    assert!(!buggy.check_invariant("RestartAtMostOnce", &spinning));
    assert_every_invariant_carries_a_mutant(&model, &[]);

    // Healthy witnesses pin the exact saturation/coalescing/retry/completion and
    // bounded-disconnect paths so the proof cannot pass over unreachable actions.
    let occupied = model.successors("OccupyWorker", &model.init_state())[0].clone();
    let pending = model.successors("RequestStageFull", &occupied)[0].clone();
    let apply = model.successors("UpgradePendingToApply", &pending)[0].clone();
    let drained = model.successors("WorkerDrainsFiller", &apply)[0].clone();
    let queued = model.successors("RetryPendingOnDrain", &drained)[0].clone();
    let completed = model.successors("WorkerCompletesIntent", &queued)[0].clone();
    let reduced = model.successors("ReduceCompletion", &completed)[0].clone();
    assert_eq!(reduced.get("delivered"), Some(&1));
    assert_eq!(reduced.get("purpose"), Some(&0));

    let disconnected = model.successors("DisconnectWithPending", &pending)[0].clone();
    let unavailable = model.successors("RestartPendingUnavailable", &disconnected)[0].clone();
    assert_eq!(unavailable.get("failed_explicitly"), Some(&1));
    assert_eq!(unavailable.get("restarts"), Some(&1));
}

/// The status reader reports the caller's running build, never a historical
/// ledger writer, and an absent canonical Ready marker cannot leave staged
/// authority or staged prose behind. The explicit mutant simultaneously pins
/// caller-build drift, absent-stage drift, and mismatch neutralization.
#[test]
fn derived_native_update_status_reconciliation_proves_caller_and_ready_authority() {
    let model = native_update_status_reconciliation_model();
    assert_proves_and_catches(&model);

    let picked = model
        .successors("PickStatusInputs", &model.init_state())
        .into_iter()
        .find(|state| {
            state["running_build"] == 2
                && state["ledger_build"] == 1
                && state["ready_present"] == 0
                && state["persisted_staged_claim"] == 1
        })
        .expect("bounded stale-ledger fixture");
    let reconciled = model.successors("ReconcileStatus", &picked)[0].clone();
    assert_eq!(reconciled["reported_build"], 2);
    assert_eq!(reconciled["reported_staged_claim"], 0);
    assert_eq!(reconciled["neutralized"], 1);
    assert!(model.check_invariant("CallerBuildIsAuthoritative", &reconciled));
    assert!(model.check_invariant("AbsentReadyCannotAdvertiseStage", &reconciled));
    assert!(model.check_invariant("MismatchedAbsentReadyIsNeutralized", &reconciled));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let pick = |running, ledger, ready, claim| {
        buggy
            .successors("PickStatusInputs", &buggy.init_state())
            .into_iter()
            .find(|state| {
                state["running_build"] == running
                    && state["ledger_build"] == ledger
                    && state["ready_present"] == ready
                    && state["persisted_staged_claim"] == claim
            })
            .expect("bounded buggy input class")
    };
    let buggy_picked = pick(2, 1, 0, 1);
    let stale = buggy.successors("ReconcileTrustingLedger", &buggy_picked)[0].clone();
    assert!(!buggy.check_invariant("CallerBuildIsAuthoritative", &stale));
    assert!(!buggy.check_invariant("AbsentReadyCannotAdvertiseStage", &stale));
    assert!(!buggy.check_invariant("MismatchedAbsentReadyIsNeutralized", &stale));

    let present = buggy.successors("ReconcileOnMarkerPresence", &pick(2, 2, 0, 1))[0].clone();
    assert!(buggy.check_invariant("CallerBuildIsAuthoritative", &present));
    assert!(!buggy.check_invariant("AbsentReadyCannotAdvertiseStage", &present));

    // The over-correction, one conjunct at a time.
    let rewritten_stage = buggy.successors("NeutralizeDespiteReady", &pick(1, 2, 1, 1))[0].clone();
    assert!(!buggy.check_invariant("ReadyPreservesPersistedOutcome", &rewritten_stage));
    let rewritten_honest =
        buggy.successors("NeutralizeEveryAbsentReady", &pick(2, 2, 0, 0))[0].clone();
    assert!(!buggy.check_invariant("HonestTerminalOutcomeIsPreserved", &rewritten_honest));
    assert_every_invariant_carries_a_mutant(&model, &[]);
}

/// The FailedMark writer/reader suppression contract: a quarantine verdict is
/// recorded in the field the reader consults and suppresses at EVERY probe
/// time, while a stage-failure backoff suppresses exactly until its deadline.
/// `Buggy=1` replays both halves of the 5ffcc15d crash-loop regression — the
/// verdict written as a plain timed memo (writer half) and the deadline
/// comparison direction flipped (reader half) — each falsifying its own
/// invariant without masking the other.
#[test]
fn derived_native_update_failed_mark_suppression_proves_quarantine_and_backoff() {
    let model = native_update_failed_mark_suppression_model();
    assert_proves_and_catches(&model);

    // Healthy witness, quarantine lane: the verdict lands in the field and a
    // matching probe is suppressed regardless of the probed timeline class.
    let quarantine_picked = model
        .successors("PickSuppressionInputs", &model.init_state())
        .into_iter()
        .find(|state| {
            state["writer_kind"] == 1 && state["probe_now"] == 3 && state["candidate_matches"] == 1
        })
        .expect("bounded quarantine fixture");
    let recorded = model.successors("RecordQuarantine", &quarantine_picked)[0].clone();
    assert_eq!(recorded["quarantined"], 1);
    assert_eq!(recorded["deadline"], 0, "the legacy sentinel stays 0");
    let probed = model.successors("ProbeSuppresses", &recorded)[0].clone();
    assert_eq!(probed["suppressed"], 1);
    assert!(model.check_invariant("QuarantineVerdictLandsInTheQuarantineField", &probed));
    assert!(model.check_invariant("QuarantineSuppressesAtEveryProbe", &probed));

    // Healthy witness, backoff lane: suppressed strictly before the deadline,
    // retryable from the deadline on.
    for (probe_now, suppressed) in [(1, 1), (2, 0), (3, 0)] {
        let picked = model
            .successors("PickSuppressionInputs", &model.init_state())
            .into_iter()
            .find(|state| {
                state["writer_kind"] == 2
                    && state["probe_now"] == probe_now
                    && state["candidate_matches"] == 1
            })
            .expect("bounded backoff fixture");
        let recorded = model.successors("RecordStageFailure", &picked)[0].clone();
        assert_eq!(recorded["quarantined"], 0);
        assert_eq!(recorded["deadline"], 2);
        let probed = model.successors("ProbeSuppresses", &recorded)[0].clone();
        assert_eq!(probed["suppressed"], suppressed);
        assert!(model.check_invariant("BackoffSuppressesIffBeforeDeadline", &probed));
    }

    // Buggy writer half: the quarantine verdict goes out as a plain timed memo
    // and the very next matching probe reads it as already elapsed — the memo
    // written and then ignored.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let buggy_picked = buggy
        .successors("PickSuppressionInputs", &buggy.init_state())
        .into_iter()
        .find(|state| {
            state["writer_kind"] == 1 && state["probe_now"] == 0 && state["candidate_matches"] == 1
        })
        .expect("bounded buggy quarantine fixture");
    let leaked = buggy.successors("RecordQuarantine", &buggy_picked)[0].clone();
    assert_eq!(leaked["quarantined"], 0);
    assert!(!buggy.check_invariant("QuarantineVerdictLandsInTheQuarantineField", &leaked));
    let retried = buggy.successors("ProbeSuppresses", &leaked)[0].clone();
    assert_eq!(
        retried["suppressed"], 0,
        "the crash-looped build is retried"
    );
    assert!(!buggy.check_invariant("QuarantineSuppressesAtEveryProbe", &retried));

    // Buggy reader half: the flipped comparison suppresses FROM the deadline
    // instead of until it, falsifying the iff in both directions.
    for (probe_now, wrong_suppressed) in [(1, 0), (3, 1)] {
        let picked = buggy
            .successors("PickSuppressionInputs", &buggy.init_state())
            .into_iter()
            .find(|state| {
                state["writer_kind"] == 2
                    && state["probe_now"] == probe_now
                    && state["candidate_matches"] == 1
            })
            .expect("bounded buggy backoff fixture");
        let recorded = buggy.successors("RecordStageFailure", &picked)[0].clone();
        let probed = buggy.successors("ProbeSuppresses", &recorded)[0].clone();
        assert_eq!(probed["suppressed"], wrong_suppressed);
        assert!(!buggy.check_invariant("BackoffSuppressesIffBeforeDeadline", &probed));
    }
}

/// The input thread owns only a bounded nonblocking enqueue. Filling the abstract
/// FIFO preserves its queued contents and accounts a newest-cue drop instead of
/// waiting. Device start/push and exact-silence reset are worker transitions; the
/// sole worker timeout is present iff the queue runs and disappears on idle
/// pause/failure.
#[test]
fn derived_trail_audio_lifecycle_proves_nonblocking_reset_and_idle_pause() {
    let model = trail_audio_lifecycle_model();
    assert_proves_and_catches(&model);

    let parked = model.successors("ParkIdle", &model.init_state())[0].clone();
    assert_eq!(parked["service_deadline"], 0);

    let queued = model.successors("PushCueAvailable", &model.init_state())[0].clone();
    let full = model.successors("PushCueAvailable", &queued)[0].clone();
    assert_eq!(full["queued"], 2);
    let dropped = model.successors("PushCueFull", &full)[0].clone();
    assert_eq!(dropped["queued"], 2, "full ingress preserves older cues");
    assert_eq!(dropped["dropped"], 1, "full ingress accounts newest drop");
    assert_eq!(dropped["ui_blocked"], 0);
    assert_eq!(dropped["ui_platform_calls"], 0);
    assert!(model.check_invariant("FullIngressDropsNewest", &dropped));

    let mut lifecycle = model.successors("WorkerStart", &queued)[0].clone();
    assert!(model.fire("RenderAudible", &mut lifecycle));
    assert!(model.fire("RenderSilent", &mut lifecycle));
    assert!(model.fire("ServiceRunning", &mut lifecycle));
    assert!(model.fire("RenderSilent", &mut lifecycle));
    assert_eq!(lifecycle["silent"], 2);
    assert!(model.fire("PushCueAvailable", &mut lifecycle));
    assert!(model.fire("WorkerPushRunning", &mut lifecycle));
    assert_eq!(
        lifecycle["silent"], 0,
        "a running-queue cue resets stale silence"
    );
    assert_eq!(lifecycle["cue_applied"], 1);
    assert!(model.check_invariant("AppliedCueResetsSilence", &lifecycle));
    assert!(model.fire("RenderAudible", &mut lifecycle));
    assert!(model.fire("RenderSilent", &mut lifecycle));
    assert!(model.fire("RenderSilent", &mut lifecycle));
    assert!(model.fire("PauseIdle", &mut lifecycle));
    assert_eq!(lifecycle["running"], 0);
    assert_eq!(lifecycle["service_deadline"], 0);
    assert_eq!(lifecycle["paused"], 1);
    assert!(model.check_invariant("RunningOwnsOneDeadline", &lifecycle));

    let queued = model.successors("PushCueAvailable", &model.init_state())[0].clone();
    let failed = model.successors("WorkerStartFails", &queued)[0].clone();
    assert_eq!(failed["failed"], 1);
    assert!(model.check_invariant("StartFailureIsExplicitAndTerminal", &failed));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let bad_ui = buggy.successors("PushCueAvailable", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("UiNeverTouchesPlatform", &bad_ui));
    let bad_full = buggy.successors("PushCueFull", &full)[0].clone();
    assert_eq!(bad_full["queued"], 2);
    assert_eq!(bad_full["dropped"], 0);
    assert!(!buggy.check_invariant("UiEnqueueNeverBlocks", &bad_full));
    assert!(!buggy.check_invariant("FullIngressDropsNewest", &bad_full));

    let mut stale_silence = buggy.successors("WorkerStart", &bad_ui)[0].clone();
    assert!(buggy.fire("RenderAudible", &mut stale_silence));
    assert!(buggy.fire("RenderSilent", &mut stale_silence));
    assert!(buggy.fire("RenderSilent", &mut stale_silence));
    assert!(buggy.fire("PushCueAvailable", &mut stale_silence));
    assert!(buggy.fire("WorkerPushRunning", &mut stale_silence));
    assert!(!buggy.check_invariant("AppliedCueResetsSilence", &stale_silence));

    // The pause that keeps polling: the worker stopped the queue but still
    // waits on `recv_timeout`, so the deadline outlives the running device.
    let mut quiet = model.successors("WorkerStart", &queued)[0].clone();
    for action in ["RenderAudible", "RenderSilent", "RenderSilent"] {
        assert!(model.fire(action, &mut quiet), "{action}: {quiet:?}");
    }
    let parked = model.successors("PauseIdle", &quiet)[0].clone();
    let polling = buggy.successors("PauseIdle", &quiet)[0].clone();
    assert_eq!((parked["running"], parked["service_deadline"]), (0, 0));
    assert_eq!((polling["running"], polling["service_deadline"]), (0, 1));
    assert!(!buggy.check_invariant("RunningOwnsOneDeadline", &polling));

    // The unbounded mailbox: a full FIFO admits one more cue instead of
    // dropping it.
    let mut flooded = model.init_state();
    for _ in 0..2 {
        assert!(model.fire("PushCueAvailable", &mut flooded));
    }
    assert!(!model.action_enabled("PushCueAvailable", &flooded));
    assert!(buggy.fire("PushCueAvailable", &mut flooded));
    assert_eq!(flooded["queued"], 3);
    assert!(!buggy.check_invariant("WorkerMailboxIsBounded", &flooded));

    // Exhaustion that does not return: ingress stays open and the next cue
    // reopens the device, so the failure is neither sealed nor terminal.
    let mut revived = buggy.successors("WorkerStartFails", &queued)[0].clone();
    assert!(!model.action_enabled("PushCueAvailable", &revived));
    assert!(buggy.fire("PushCueAvailable", &mut revived));
    assert!(!buggy.check_invariant("StartFailureIsExplicitAndTerminal", &revived));
    assert!(buggy.fire("WorkerStart", &mut revived));
    assert_eq!((revived["failed"], revived["running"]), (1, 1));
}

/// Cold start and resume both render from the post-cue synth state into owned
/// buffers before enqueue. The retired three-silent-buffer priming and the
/// unsafe overwrite shortcut are explicit Buggy counterexamples.
#[test]
fn derived_trail_audio_start_latency_proves_one_buffer_and_safe_reclaim() {
    let model = trail_audio_start_latency_model();
    assert_proves_and_catches(&model);

    let mut state = model.init_state();
    for action in ["CueCold", "PrimeCold", "StartCold"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert_eq!(state["audible_buffer"], 1);
    assert_eq!(state["queued"], 3);
    assert!(model.fire("CallbackEnqueueBegins", &mut state));
    assert_eq!(state["enqueue_in_flight"], 1);
    assert!(
        !model.action_enabled("StopIdle", &state),
        "the worker gate must wait for an in-flight callback enqueue"
    );
    assert!(model.fire("CallbackEnqueueEnds", &mut state));
    assert!(model.fire("StopIdle", &mut state));
    assert_eq!(state["available"], 3);
    assert_eq!(state["queued"], 0);
    assert!(model.fire("ParkIdle", &mut state));
    for action in ["CueResume", "PrimeResume", "StartResume"] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert_eq!(state["audible_buffer"], 1);
    assert_eq!(state["unsafe_writes"], 0);
    assert_eq!(state["callback_generation"], 1);
    assert_eq!(state["generation"], 3);
    assert!(model.fire("OldCallbackReturns", &mut state));
    assert_eq!(state["stale_enqueue"], 0);
    for invariant in [
        "BufferOwnershipConserved",
        "AudibleWithinOneBuffer",
        "WritesRequireAvailableOwnership",
        "StaleCallbackCannotReenqueue",
        "StopNeverOverlapsEnqueue",
    ] {
        assert!(model.check_invariant(invariant, &state), "{invariant}");
    }

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    // A Buggy stop with no enqueue in flight is the retired pause: the queue
    // keeps its buffers and the books agree, so only the refused stop below
    // miscounts ownership.
    let mut paused = buggy.init_state();
    for action in ["CueCold", "PrimeCold", "StartCold", "StopIdle"] {
        assert!(buggy.fire(action, &mut paused), "{action}: {paused:?}");
    }
    assert_eq!((paused["available"], paused["queued"]), (0, 3));
    assert!(buggy.check_invariant("BufferOwnershipConserved", &paused));

    let mut delayed = buggy.init_state();
    assert!(buggy.fire("CueCold", &mut delayed));
    assert!(buggy.fire("PrimeCold", &mut delayed));
    assert_eq!(delayed["audible_buffer"], 4);
    assert!(!buggy.check_invariant("AudibleWithinOneBuffer", &delayed));

    assert!(buggy.fire("StartCold", &mut delayed));
    assert!(buggy.fire("CallbackEnqueueBegins", &mut delayed));
    assert!(buggy.action_enabled("StopIdle", &delayed));
    assert!(buggy.fire("StopIdle", &mut delayed));
    assert!(!buggy.check_invariant("StopNeverOverlapsEnqueue", &delayed));
    assert_eq!(
        (delayed["available"], delayed["queued"]),
        (3, 3),
        "the queue kept all three buffers while the worker's books freed them"
    );
    assert!(!buggy.check_invariant("BufferOwnershipConserved", &delayed));
    assert!(buggy.fire("CueResume", &mut delayed));
    assert!(buggy.fire("PrimeResume", &mut delayed));
    assert!(!buggy.check_invariant("WritesRequireAvailableOwnership", &delayed));
    assert!(buggy.fire("StartResume", &mut delayed));
    assert!(buggy.fire("OldCallbackReturns", &mut delayed));
    assert!(!buggy.check_invariant("StaleCallbackCannotReenqueue", &delayed));
}

#[test]
fn derived_tab_stop_handoff_proves_and_catches_narrow_truncation() {
    let model = tab_stop_handoff_model();
    assert_proves_and_catches(&model);

    let mut preserved = model.init_state();
    for action in [
        "GrowSourceWide",
        "SetCustomFutureStop",
        "ShrinkSourceNarrow",
        "CaptureProjection",
        "AdmitCoveringProjection",
        "RestoreProjection",
        "GrowDestinationWide",
        "TabUsesRestoredStop",
    ] {
        assert!(
            model.fire(action, &mut preserved),
            "{action}: {preserved:?}"
        );
    }
    assert_eq!(preserved.get("tab_target"), Some(&6));

    for (supply, reject) in [
        ("SupplyUndersizeProjection", "RejectUndersizeProjection"),
        ("SupplyOversizeProjection", "RejectOversizeProjection"),
    ] {
        let invalid = model.successors(supply, &model.init_state())[0].clone();
        let rejected = model.successors(reject, &invalid)[0].clone();
        assert_eq!(rejected.get("rejected"), Some(&1));
        assert_eq!(rejected.get("admitted"), Some(&0));
    }

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut truncated = buggy.init_state();
    for action in [
        "GrowSourceWide",
        "SetCustomFutureStop",
        "ShrinkSourceNarrow",
    ] {
        assert!(buggy.fire(action, &mut truncated));
    }
    assert!(!buggy.check_invariant("NarrowShrinkKeepsBoundedBacking", &truncated));

    // The pre-fix restore admitted any length: both invalid projections are
    // taken, and each admission breaks the covering/bounded window.
    for (supply, reject) in [
        ("SupplyUndersizeProjection", "RejectUndersizeProjection"),
        ("SupplyOversizeProjection", "RejectOversizeProjection"),
    ] {
        let invalid = buggy.successors(supply, &buggy.init_state())[0].clone();
        let taken = buggy.successors(reject, &invalid)[0].clone();
        assert_eq!(taken.get("admitted"), Some(&1), "{supply}");
        assert!(!buggy.check_invariant("AdmissionIsCoveringAndBounded", &taken));
        assert!(!buggy.check_invariant("InvalidProjectionIsNeverAdmitted", &taken));
    }
}

#[test]
fn derived_scrollback_maintenance_lane_proves_output_isolation() {
    let model = scrollback_maintenance_lane_model();
    assert_proves_and_catches(&model);

    let mut output = model.init_state();
    assert!(model.fire("ObserveOutput", &mut output));
    assert_eq!(output.get("blocking_lock"), Some(&0));
    assert_eq!(output.get("unbounded_work"), Some(&0));
    assert_eq!(output.get("mutation"), Some(&0));
    assert!(model.successors("BeginBulkTrim", &output).is_empty());

    let mut pressure = model.init_state();
    for action in ["ObserveMemoryPressure", "BeginBulkTrim", "CompleteBulkTrim"] {
        assert!(model.fire(action, &mut pressure), "{action}: {pressure:?}");
    }
    assert_eq!(pressure.get("mutation"), Some(&1));
    assert_eq!(pressure.get("completed"), Some(&1));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut regressed = buggy.init_state();
    assert!(buggy.fire("ObserveOutput", &mut regressed));
    assert!(!buggy.check_invariant("OrdinaryOutputIsMaintenanceFree", &regressed));
    assert_eq!(
        regressed.get("mutation"),
        Some(&1),
        "the old cap check evicted"
    );
    assert!(!buggy.check_invariant("MutationRequiresCompletedPressureTrim", &regressed));
}

#[test]
fn derived_top_anchored_scroll_proves_history_retention() {
    let model = top_anchored_scroll_history_model();
    assert_proves_and_catches(&model);

    // The OVERLAPPING flavours reproduce the original four regimes exactly: a
    // selection sitting in the damaged rows is cleared by every non-archival scroll,
    // and piecewise-remapped by the archival one.
    for (choice, expected_history) in [
        ("ChooseArchivalOverlapping", 1),
        ("ChooseInteriorOverlapping", 0),
        ("ChooseMarginedOverlapping", 0),
        ("ChooseEphemeralOverlapping", 0),
    ] {
        let mut state = model.init_state();
        assert!(model.fire(choice, &mut state));
        assert!(model.fire("Scroll", &mut state));
        assert_eq!(
            state.get("history_len"),
            Some(&expected_history),
            "{choice}"
        );
        assert_eq!(state.get("footer"), Some(&1), "{choice}");
        assert_eq!(
            state.get("footer_anchor"),
            Some(&expected_history),
            "{choice}"
        );
        assert_eq!(
            state.get("selection_alive"),
            Some(&expected_history),
            "{choice}"
        );
        assert_eq!(
            state.get("selection_region_row"),
            Some(&(2 - expected_history)),
            "{choice}"
        );
        assert_eq!(state.get("selection_footer_row"), Some(&4), "{choice}");
    }

    // SELECTION CUSTODY Phase 4: the DISJOINT flavours are the new half. A selection
    // outside the damaged rows survives every regime — including the interior and
    // margined scrolls that used to kill it unconditionally. This is the reported
    // bug, stated as a model property: a status bar repainting in a scroll region
    // must not destroy a highlight anchored up in scrollback.
    //
    // `ChooseArchivalDisjoint` belongs in this loop and used to be missing from it,
    // because the invariant's archival arm ignored `selection_disjoint` and demanded
    // the remap for every archival state. Archiving a row and MOVING the selection
    // are two different questions: the splice shifts rows above its boundary, and a
    // selection below the region is not one of them. So the archival regime still
    // retains its displaced row (`expected_history == 1`) while leaving a disjoint
    // selection exactly where it was.
    for (choice, expected_history) in [
        ("ChooseArchivalDisjoint", 1),
        ("ChooseInteriorDisjoint", 0),
        ("ChooseMarginedDisjoint", 0),
        ("ChooseEphemeralDisjoint", 0),
    ] {
        let mut state = model.init_state();
        assert!(model.fire(choice, &mut state));
        assert!(model.fire("Scroll", &mut state));
        assert_eq!(
            state.get("selection_alive"),
            Some(&1),
            "{choice}: a disjoint selection must survive"
        );
        assert_eq!(
            state.get("selection_region_row"),
            Some(&2),
            "{choice}: and must not be remapped — nothing moved under it"
        );
        assert_eq!(
            state.get("history_len"),
            Some(&expected_history),
            "{choice}"
        );
    }

    // …and the archival regime's Buggy=1 mutant is still caught through the disjoint
    // label: it drops the displaced row and kills the selection, and both halves are
    // named by invariants.
    {
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut state = buggy.init_state();
        assert!(buggy.fire("ChooseArchivalDisjoint", &mut state));
        assert!(buggy.fire("Scroll", &mut state));
        assert!(!buggy.check_invariant("EligibleDisplacementIsRetained", &state));
        assert!(!buggy.check_invariant("EligibleSelectionUsesPiecewiseRemap", &state));
    }

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut dropped = buggy.init_state();
    assert!(buggy.fire("ChooseArchivalOverlapping", &mut dropped));
    assert!(buggy.fire("Scroll", &mut dropped));
    assert!(!buggy.check_invariant("EligibleDisplacementIsRetained", &dropped));
    assert!(!buggy.check_invariant("FixedFooterAnchorTracksLogicalInsertion", &dropped));
    assert!(!buggy.check_invariant("EligibleSelectionUsesPiecewiseRemap", &dropped));

    // …and the Phase 4 mutant: an interior scroll that clears a DISJOINT selection —
    // the literal shipping defect the damage lattice removes.
    let mut over_cleared = buggy.init_state();
    assert!(buggy.fire("ChooseInteriorDisjoint", &mut over_cleared));
    assert!(buggy.fire("Scroll", &mut over_cleared));
    assert!(
        !buggy.check_invariant("EligibleSelectionUsesPiecewiseRemap", &over_cleared),
        "the restated invariant must catch an over-clear, not just the archival drop"
    );
    // …and the same scroll runs past its bottom margin, moving the footer.
    assert_eq!(over_cleared.get("footer"), Some(&0));
    assert!(!buggy.check_invariant("FixedFooterIsPreserved", &over_cleared));
}

#[test]
fn derived_manual_diagnostics_lane_proves_latest_revision_and_stale_rejection() {
    let model = manual_config_diagnostics_lane_model();
    assert_proves_and_catches(&model);

    let mut burst = model.init_state();
    for action in [
        "RequestFirst",
        "RequestSecond",
        "RequestThird",
        "WorkerTakes",
        "DispatchLatestPending",
        "WorkerCompletes",
        "RejectStale",
        "WorkerTakes",
        "WorkerCompletes",
        "AcceptCurrent",
    ] {
        assert!(model.fire(action, &mut burst), "{action}: {burst:?}");
    }
    assert_eq!(burst.get("published_revision"), Some(&3));
    assert_eq!(burst.get("pending_revision"), Some(&0));
    assert_eq!(burst.get("stale_published"), Some(&0));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut lost_latest = buggy.init_state();
    for action in ["RequestFirst", "RequestSecond", "RequestThird"] {
        assert!(
            buggy.fire(action, &mut lost_latest),
            "{action}: {lost_latest:?}"
        );
    }
    assert!(!buggy.check_invariant("LatestRequestRemainsRepresented", &lost_latest));
    assert!(!buggy.check_invariant("PendingSlotNamesLatest", &lost_latest));
}

#[test]
fn derived_font_catalog_generation_rejects_stale_completion() {
    let model = aterm_spec::derive::font_catalog_generation_model();
    assert_proves_and_catches(&model);
    let mut state = model.init_state();
    for action in [
        "RequestFirst",
        "RequestSecond",
        "CompleteFirst",
        "RejectStale",
        "CompleteSecond",
        "PublishCurrent",
    ] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert_eq!(state.get("published"), Some(&2));
    assert_eq!(state.get("stale_published"), Some(&0));
}

#[test]
fn derived_path_feed_snapshot_proves_same_read_binding_and_catches_reread() {
    let model = path_feed_snapshot_model();
    assert_proves_and_catches(&model);

    // The host's after-read hook replaces the path only after the admitted
    // String has fed both the parser and fingerprint. Publication retains that
    // exact pair even though the live pathname now names generation two.
    let mut replaced = model.init_state();
    for action in ["Read", "LiveMutate", "Publish"] {
        assert!(model.fire(action, &mut replaced), "{action}: {replaced:?}");
    }
    assert_eq!(replaced["live"], 2);
    assert_eq!(replaced["admitted"], 1);
    assert_eq!(replaced["prepared"], 1);
    assert_eq!(replaced["fingerprint"], 1);
    assert_eq!(replaced["published"], 1);
    assert!(model.check_invariant("PublishedPairComesFromAdmittedRead", &replaced));

    // ABA is irrelevant to the transaction: generation two may be admitted
    // while the pathname later returns to generation one, but both published
    // projections remain generation two.
    let mut aba = model.init_state();
    for action in ["LiveMutate", "Read", "LiveRestore", "Publish"] {
        assert!(model.fire(action, &mut aba), "{action}: {aba:?}");
    }
    assert_eq!(aba["live"], 1);
    assert_eq!(aba["admitted"], 2);
    assert_eq!(aba["prepared"], 2);
    assert_eq!(aba["fingerprint"], 2);
    assert!(model.check_invariant("PublishedPairComesFromAdmittedRead", &aba));

    // The mutant performs a second live read at publication. The same ordinary
    // replacement trace pairs the generation-one consumer with a generation-two
    // fingerprint and must violate the invariant.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut mixed = buggy.init_state();
    for action in ["Read", "LiveMutate", "Publish"] {
        assert!(buggy.fire(action, &mut mixed), "{action}: {mixed:?}");
    }
    assert_eq!(mixed["prepared"], 1);
    assert_eq!(mixed["fingerprint"], 2);
    assert!(
        !buggy.check_invariant("PublishedPairComesFromAdmittedRead", &mixed),
        "negative control: a live reread must not relabel the admitted consumer"
    );
}

#[test]
fn derived_font_theme_generation_reprepares_overtaken_config() {
    let model = aterm_spec::derive::font_theme_generation_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &["GenerationsBounded"]);
    let mut state = model.init_state();
    for action in [
        "RequestConfig",
        "ThemeChanged",
        "CompleteOldTheme",
        "ReprepareLatestTheme",
        "CompleteLatestTheme",
        "PublishCurrent",
    ] {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
    }
    assert_eq!(state.get("published"), Some(&2));
    assert_eq!(state.get("published_theme"), Some(&1));
    assert_eq!(state.get("stale_published"), Some(&0));
}

/// A staged update remains a real, selectable `ApplyUpdate` command over both
/// terminal and native Settings/About tabs. The model's mutant applies the
/// terminal-only menu gate to that global action and must be caught.
#[test]
fn derived_native_update_menu_activation_is_independent_of_active_tab_kind() {
    let model = native_update_menu_activation_model();
    assert_proves_and_catches(&model);

    let mut native_tab = model.init_state();
    for action in [
        "StageUpdate",
        "RefreshStagedVersionMenu",
        "DecodeApplyTag",
        "DispatchApply",
    ] {
        assert!(
            model.fire(action, &mut native_tab),
            "{action}: {native_tab:?}"
        );
    }
    assert_eq!(native_tab.get("terminal_tab"), Some(&0));
    assert_eq!(native_tab.get("apply_dispatched"), Some(&1));

    let mut terminal_then_native = model.init_state();
    for action in [
        "StageUpdate",
        "ActivateTerminalTab",
        "RefreshStagedVersionMenu",
        "ActivateNativeTab",
        "DecodeApplyTag",
        "DispatchApply",
    ] {
        assert!(
            model.fire(action, &mut terminal_then_native),
            "{action}: {terminal_then_native:?}"
        );
    }
    assert_eq!(terminal_then_native.get("terminal_tab"), Some(&0));
    assert_eq!(terminal_then_native.get("row_enabled"), Some(&1));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut disabled = buggy.init_state();
    assert!(buggy.fire("StageUpdate", &mut disabled));
    assert!(buggy.fire("RefreshStagedVersionMenu", &mut disabled));
    assert!(!buggy.check_invariant("RefreshedStagedRowIsPresentAndEnabled", &disabled,));
    assert!(buggy.successors("DecodeApplyTag", &disabled).is_empty());
}

/// Focus loss clears the previous ambient snapshot. Every subsequent Winit
/// snapshot is authoritative, including a valid report before `Focused(true)`.
/// The mutant retains stale Ctrl during the reset transition itself.
#[test]
fn derived_focus_modifier_cache_resets_only_at_focus_loss() {
    let model = focus_modifier_cache_model();
    assert_proves_and_catches(&model);

    let mut healthy = model.init_state();
    for action in ["ReportCtrl", "FocusOut"] {
        assert!(model.fire(action, &mut healthy), "{action}: {healthy:?}");
    }
    assert_eq!(healthy["cached_ctrl"], 0);
    assert_eq!(healthy["fresh_ctrl"], 0);

    // Duplicate focus events are legal and the reset remains idempotent.
    for action in ["FocusOut", "FocusIn", "FocusIn", "FocusOut"] {
        assert!(model.fire(action, &mut healthy), "{action}: {healthy:?}");
    }
    assert_eq!(healthy["focused"], 0);
    assert_eq!(healthy["cached_ctrl"], 0);
    assert_eq!(healthy["fresh_ctrl"], 0);

    // A fresh snapshot before focus-in is accepted and survives the focus event.
    for action in ["ReportCtrl", "FocusIn", "PressL"] {
        assert!(model.fire(action, &mut healthy), "{action}: {healthy:?}");
    }
    assert_eq!(healthy["focused"], 1);
    assert_eq!(healthy["cached_ctrl"], 1);
    assert_eq!(healthy["fresh_ctrl"], 1);
    assert_eq!(healthy["delivered_ctrl"], 1);

    // Negative control: the unsafe mutant retains Ctrl in FocusOut itself.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut stale = buggy.init_state();
    for action in ["ReportCtrl", "FocusOut"] {
        assert!(buggy.fire(action, &mut stale), "{action}: {stale:?}");
    }
    assert_eq!(stale["cached_ctrl"], 1);
    assert_eq!(stale["fresh_ctrl"], 0);
    assert!(!buggy.check_invariant("CachedCtrlRequiresAuthoritativeReport", &stale));
}

/// SELECTION CUSTODY: only a byte-producing typing press may take the user's
/// reading position. A bare modifier, an auto-repeat tick and a key release must
/// move neither the viewport nor the selection — and output must repin rather than
/// snap the reader back to live.
///
/// The invariants are stated over the OBSERVABLE state against a shadow of the
/// pre-action values, not over a self-reported "did this disturb anything" flag,
/// so an implementation that moved the viewport silently cannot satisfy them.
///
/// Output and the SELECTION are separate: Phase 4 lets output that REPLACED the
/// selected rows clear the highlight, and ED 3 / `clear_scrollback` / RIS take the
/// viewport back outright. Both are their own event kinds, because the earlier
/// `OutputNeverTakesCustodyOrSelection` asserted the opposite of what shipped — a
/// model that contradicts the code is worse than no model, since the next reader
/// deletes the code to satisfy it.
///
/// `Buggy=1` is the regression family — inert press, repeat, release, output that
/// snaps, damaging output that also takes custody, and typing that deselects
/// without snapping — so `assert_proves_and_catches` fails unless the invariants
/// genuinely catch it, and `assert_every_invariant_carries_a_mutant` fails unless
/// each named law catches a member of its own.
#[test]
fn derived_press_custody_keeps_the_viewport_and_selection_off_inert_presses() {
    let model = press_custody_model();
    assert_proves_and_catches(&model);
    // …and every named law catches its OWN member, not just the model as a whole.
    assert_every_invariant_carries_a_mutant(&model, &["StateBounds"]);

    // The reported bug, replayed as a trace: read history, select, then press ⌘.
    let mut reading = model.init_state();
    for action in ["UserScroll", "UserSelect", "InertPress"] {
        assert!(model.fire(action, &mut reading), "{action}: {reading:?}");
    }
    assert_eq!(
        reading["offset"], 1,
        "bare ⌘ left the viewport where it was"
    );
    assert_eq!(reading["selection"], 1, "…and left the selection alive");
    assert_eq!(reading["owner"], 1, "…and the user still owns the viewport");

    // A held key's repeat ticks are equally inert, and so is the key-up.
    let mut held = model.init_state();
    for action in [
        "UserScroll",
        "UserSelect",
        "RepeatPress",
        "RepeatPress",
        "ReleaseEvent",
    ] {
        assert!(model.fire(action, &mut held), "{action}: {held:?}");
    }
    assert_eq!(held["selection"], 1, "repeat and release must not deselect");
    assert_eq!(held["offset"], 1, "repeat and release must not snap");

    // Output arriving while the user reads repins rather than sliding the view,
    // and never touches the selection.
    let mut flooded = model.init_state();
    for action in ["UserScroll", "UserSelect", "OutputWhileReading"] {
        assert!(model.fire(action, &mut flooded), "{action}: {flooded:?}");
    }
    assert_eq!(
        flooded["owner"], 1,
        "output does not take the viewport back"
    );
    assert_eq!(
        flooded["offset"], 2,
        "the repin keeps the content under the eye"
    );
    assert_eq!(
        flooded["selection"], 1,
        "output does not drop the selection"
    );

    // Output that REPLACED the selected rows is the Phase-4 case the old
    // `OutputNeverTakesCustodyOrSelection` denied: the highlight goes, because
    // leaving it painted over new text makes ⌘-C return text the user never
    // selected — and the reading position still does not.
    let mut damaged = model.init_state();
    for action in ["UserScroll", "UserSelect", "OutputDamagesTheSelectedRows"] {
        assert!(model.fire(action, &mut damaged), "{action}: {damaged:?}");
    }
    assert_eq!(
        damaged["selection"], 0,
        "output that replaced the selected rows clears the highlight"
    );
    assert_eq!(
        damaged["owner"], 1,
        "…and still does not take the viewport back"
    );
    assert_eq!(
        damaged["offset"], 2,
        "…the repin keeps the same content under the eye"
    );

    // ED 3 / clear_scrollback / RIS: the coordinate space the offset named is
    // gone, so this one legitimately DOES hand the viewport back. Modelled as its
    // own event kind so the custody law above stays true of the shipped engine
    // rather than being quietly false of it.
    let mut wiped = model.init_state();
    for action in [
        "UserScroll",
        "UserSelect",
        "OutputInvalidatesTheCoordinateSpace",
    ] {
        assert!(model.fire(action, &mut wiped), "{action}: {wiped:?}");
    }
    assert_eq!(wiped["offset"], 0, "the space the offset named is gone");
    assert_eq!(wiped["owner"], 0);
    assert_eq!(wiped["selection"], 0);

    // An IN-PLACE take — a rewrite over the selected rows that scrolls nothing, or
    // a fail-closed `post_process` arm — clears the highlight and leaves the
    // reader exactly where they were (closed 2026-09-25; a KNOWN GAP before).
    for take in [
        "OutputDamagesTheSelectedRowsInPlace",
        "OutputTookTheSelectionUnattributedInPlace",
    ] {
        let mut rewritten = model.init_state();
        for action in ["UserScroll", "UserSelect", take] {
            assert!(
                model.fire(action, &mut rewritten),
                "{action}: {rewritten:?}"
            );
        }
        assert_eq!(rewritten["selection"], 0, "{take}: the highlight goes");
        assert_eq!(
            [rewritten["offset"], rewritten["owner"]],
            [1, 1],
            "{take}: the view does not move"
        );
    }

    // Back TOWARD live without typing — End, a downward scroll, the ⌘-V / IME
    // snaps — moves the view down and KEEPS the highlight.
    let mut returned = model.init_state();
    for action in [
        "UserScroll",
        "UserScroll",
        "UserSelect",
        "UserScrollTowardLive",
        "SnapToLive",
    ] {
        assert!(model.fire(action, &mut returned), "{action}: {returned:?}");
    }
    assert_eq!(
        [returned["offset"], returned["owner"], returned["selection"]],
        [0, 0, 1],
        "at live, tail-owned, and the highlight survived the trip down"
    );

    // …and the one handover is intact: typing still lands at live and deselects.
    let mut typed = model.init_state();
    for action in ["UserScroll", "UserSelect", "TypingPress"] {
        assert!(model.fire(action, &mut typed), "{action}: {typed:?}");
    }
    assert_eq!(typed["offset"], 0, "typing still snaps to live");
    assert_eq!(typed["selection"], 0, "typing still deselects");
    assert_eq!(
        typed["owner"], 0,
        "typing hands the viewport back to the tail"
    );

    // The Phase-4 member specifically: output allowed to clear the rows it damaged
    // must not smuggle the viewport snap in with it. Named here as well as covered
    // by the per-invariant sweep, because this is the law the previous, false
    // `OutputNeverTakesCustodyOrSelection` would have been deleted to satisfy.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut snapped = buggy.init_state();
    for action in ["UserScroll", "UserSelect", "OutputDamagesTheSelectedRows"] {
        assert!(buggy.fire(action, &mut snapped), "{action}: {snapped:?}");
    }
    assert!(
        !buggy.check_invariant("OutputNeverTakesCustody", &snapped),
        "damaging output that also snapped to live must violate the custody law"
    );
}

/// SELECTION CUSTODY — a highlight is destroyed only by something that destroys the
/// content it names.
///
/// `Buggy = 1` is a regression FAMILY with one member per destroyer. `RegionDamageLow`
/// is the literal shipping defect (ANY region damage clears, whether or not it
/// overlapped — what made a status bar repainting at the bottom of the screen destroy
/// a highlight anchored far up in scrollback); `RegionDamageHigh` is its inverse (a
/// highlight left alive over replaced text, so the copy returns something the user
/// never selected); `InertPress` and `UniformScroll` destroy the selection outright —
/// the two user complaints; and `Evict` reports the truncation without clamping the
/// head or dropping a fully-evicted selection.
///
/// Each member is asserted against the invariant it falsifies, ONE BY ONE, because
/// the whole-model verdict stops at the first violation: without that,
/// `assert_proves_and_catches` passed while five of the eight invariants were ghosts
/// that no implementation could violate.
#[test]
fn derived_selection_custody_spares_damage_that_missed_the_selection() {
    let model = selection_custody_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &["StateIsBounded"]);

    // THE REPORTED BUG, as a trace: select high, damage low, highlight survives.
    let mut spared = model.init_state();
    for action in ["SelectHigh", "RegionDamageLow"] {
        assert!(model.fire(action, &mut spared), "{action}: {spared:?}");
    }
    assert_eq!(
        spared["alive"], 1,
        "damage to rows 0..1 must not touch a selection on rows 2..3"
    );

    // …and the inverse hole: damage that DID hit must clear, or a copy returns text
    // the user never selected.
    let mut hit = model.init_state();
    for action in ["SelectHigh", "RegionDamageHigh"] {
        assert!(model.fire(action, &mut hit), "{action}: {hit:?}");
    }
    assert_eq!(
        hit["alive"], 0,
        "damage to row 3 must clear a selection on 2..3"
    );

    // Partial eviction TRUNCATES: the head clamps to the floor and says so.
    let mut evicted = model.init_state();
    for action in ["SelectLow", "Evict"] {
        assert!(model.fire(action, &mut evicted), "{action}: {evicted:?}");
    }
    assert_eq!(
        evicted["alive"], 1,
        "losing the oldest row is not losing the selection"
    );
    assert_eq!(evicted["truncated"], 1, "…and the loss is recorded");
    assert_eq!(
        evicted["sel_lo"], 1,
        "…with the head clamped to the new floor"
    );

    // …but eviction that took the WHOLE interval destroys it. This half of the law
    // was unmodelled until `SelectOldest` existed: with only the two-row selections
    // no reachable state had `sel_hi == 0`, so `Evict`'s clear branch was dead code
    // and the invariant's `else` arm was never evaluated.
    let mut gone = model.init_state();
    for action in ["SelectOldest", "Evict"] {
        assert!(model.fire(action, &mut gone), "{action}: {gone:?}");
    }
    assert_eq!(
        gone["alive"], 0,
        "both endpoints evicted leaves nothing to name"
    );
    assert_eq!(
        gone["truncated"], 0,
        "…and a destroyed selection records no truncation"
    );

    // An eviction that missed the selection entirely moves nothing at all.
    let mut untouched = model.init_state();
    for action in ["SelectHigh", "Evict"] {
        assert!(
            model.fire(action, &mut untouched),
            "{action}: {untouched:?}"
        );
    }
    assert_eq!(untouched["alive"], 1);
    assert_eq!(untouched["sel_lo"], 2, "the head never moved");
    assert_eq!(
        untouched["truncated"], 0,
        "nothing was lost, so nothing is recorded"
    );

    // A bare modifier and ordinary output are both inert.
    let mut inert = model.init_state();
    for action in ["SelectHigh", "InertPress", "UniformScroll"] {
        assert!(model.fire(action, &mut inert), "{action}: {inert:?}");
    }
    assert_eq!(
        inert["alive"], 1,
        "neither a modifier nor output may take it"
    );

    // Typing still deselects — the one handover, unchanged.
    let mut typed = model.init_state();
    for action in ["SelectHigh", "TypingPress"] {
        assert!(model.fire(action, &mut typed), "{action}: {typed:?}");
    }
    assert_eq!(typed["alive"], 0, "typing still deselects");

    // THE FAMILY, MEMBER BY MEMBER. Each mutant is driven to the state it breaks
    // and the NAMED invariant is checked there, so a pass says "this law caught
    // this defect" rather than "some law caught something". The whole-model verdict
    // cannot say that: it stops at the first violated invariant.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    // The shipped sentinel: damage that never touched the selected rows clears.
    let mut over_cleared = buggy.init_state();
    for action in ["SelectHigh", "RegionDamageLow"] {
        assert!(buggy.fire(action, &mut over_cleared));
    }
    assert!(
        !buggy.check_invariant("DisjointDamagePreserves", &over_cleared),
        "the shipped sentinel cleared on damage it never touched; the model must say so"
    );

    // The inverse hole: damage that HIT the selected rows leaves the highlight.
    let mut stale = buggy.init_state();
    for action in ["SelectHigh", "RegionDamageHigh"] {
        assert!(buggy.fire(action, &mut stale));
    }
    assert!(
        !buggy.check_invariant("OverlapDamageClears", &stale),
        "a highlight surviving over replaced text is a wrong-copy path; the model must say so"
    );

    // Complaint (1): a bare modifier destroying the selection. Unfalsifiable in the
    // first draft — `InertPress` wrote only `last_event`, so `alive == 1` followed
    // from the action's own guard whatever the engine did.
    let mut modifier = buggy.init_state();
    for action in ["SelectHigh", "InertPress"] {
        assert!(buggy.fire(action, &mut modifier));
    }
    assert!(
        !buggy.check_invariant("InertPressPreservesTheSelection", &modifier),
        "a bare modifier that deselects must violate the inert-press law"
    );

    // Complaint (2): ordinary output taking the highlight.
    let mut flooded = buggy.init_state();
    for action in ["SelectHigh", "UniformScroll"] {
        assert!(buggy.fire(action, &mut flooded));
    }
    assert!(
        !buggy.check_invariant("UniformScrollPreservesTheSelection", &flooded),
        "output that deselects must violate the uniform-scroll law"
    );

    // Eviction that reports the loss without acting on it: the head stays below the
    // new floor (a dangling anchor) and a fully-evicted selection stays alive.
    let mut dangling = buggy.init_state();
    for action in ["SelectLow", "Evict"] {
        assert!(buggy.fire(action, &mut dangling));
    }
    assert!(
        !buggy.check_invariant("PartialEvictionTruncates", &dangling),
        "an eviction that never clamps the head must violate the truncation law"
    );
    assert!(
        !buggy.check_invariant("NoDanglingAnchors", &dangling),
        "…and a head below the floor is a dangling anchor"
    );
    assert!(
        !buggy.check_invariant("TruncationImpliesAClampedHead", &dangling),
        "…and a truncation recorded against an unclamped head is a lie"
    );
    let mut kept = buggy.init_state();
    for action in ["SelectOldest", "Evict"] {
        assert!(buggy.fire(action, &mut kept));
    }
    assert!(
        !buggy.check_invariant("PartialEvictionTruncates", &kept),
        "a selection with both endpoints evicted must not survive"
    );
}

/// SELECTION CUSTODY — a selection is scoped to the screen it was made on.
///
/// The park is ASYMMETRIC on purpose: entering the alt screen takes the main
/// selection into the parked slot, leaving takes it back out and leaves the slot
/// empty. The alt screen's own selection dies with the buffer it named. The
/// obvious alternative — a symmetric swap — makes the parked slot a durable second
/// selection with a lifetime of its own, and `ParkedEmptyOffAlt` is what says so.
///
/// `Buggy=1` also neuters every destroyer's parked half, the silent failure mode of
/// the coordinated clear sites (`clear_scrollback` and a width resize in place;
/// `Terminal::reset` and byte-stream RIS, which also leave the alt screen;
/// `restore_checkpoint`, which keeps the live selection) — none of which the
/// compiler checks. Those members are caught by DIFFERENT invariants, so a pass
/// here is not one property doing all the work.
#[test]
fn derived_alt_selection_park_never_leaves_a_selection_parked_off_alt() {
    let model = alt_selection_park_model();
    assert_proves_and_catches(&model);
    // Each design law catches a member of its own; only the space guard does not.
    assert_every_invariant_carries_a_mutant(&model, &["StateBounds"]);

    // The reported shape: select on main, run a pager, quit. The highlight is the
    // same one, and nothing is left behind in the slot.
    let mut pager = model.init_state();
    for action in ["Select", "Enter", "Leave"] {
        assert!(model.fire(action, &mut pager), "{action}: {pager:?}");
    }
    assert_eq!(pager["live_sel"], 1, "the main selection came back");
    assert_eq!(pager["parked_sel"], 0, "…and the slot is empty again");
    assert_eq!(pager["on_alt"], 0);

    // The asymmetry, stated as a trace: a selection made ON alt does not leak back
    // to main, because the restore OVERWRITES rather than swaps.
    let mut on_alt = model.init_state();
    for action in ["Enter", "Select", "Leave"] {
        assert!(model.fire(action, &mut on_alt), "{action}: {on_alt:?}");
    }
    assert_eq!(
        on_alt["live_sel"], 0,
        "an alt-screen selection names a buffer the user can no longer see"
    );
    assert_eq!(on_alt["parked_sel"], 0);

    // A wholesale destroyer reaches the parked slot, not just the live one.
    let mut destroyed = model.init_state();
    for action in ["Select", "Enter", "Wholesale", "Leave"] {
        assert!(
            model.fire(action, &mut destroyed),
            "{action}: {destroyed:?}"
        );
    }
    assert_eq!(
        destroyed["live_sel"], 0,
        "nothing may come back over content that was destroyed while parked"
    );

    // A reset from the pager lands on main with NOTHING parked — and RIS then a
    // re-entry in one batch parks nothing either, because the reset already
    // emptied the live slot the re-entry parks.
    for tail in [&["Reset"][..], &["Reset", "Enter"]] {
        let mut reset = model.init_state();
        for action in ["Select", "Enter"].iter().chain(tail) {
            assert!(model.fire(action, &mut reset), "{action}: {reset:?}");
        }
        assert_eq!(reset["parked_sel"], 0, "{tail:?}: {reset:?}");
        assert_eq!(reset["live_sel"], 0, "{tail:?}: {reset:?}");
    }

    // A checkpoint restore retires the parked slot onto EITHER screen, and leaves
    // the live selection standing — the seamless-update adopt path.
    for restore in ["RestoreMain", "RestoreAlt"] {
        let mut adopted = model.init_state();
        for action in ["Select", "Enter", "Select", restore] {
            assert!(model.fire(action, &mut adopted), "{action}: {adopted:?}");
        }
        assert_eq!(adopted["parked_sel"], 0, "{restore}: {adopted:?}");
        assert_eq!(adopted["live_sel"], 1, "{restore}: {adopted:?}");
    }
}

/// Press-time disposition owns the entire key episode: consumed presses keep a
/// tracker across repeats/overlay close and produce no Kitty event bytes, while
/// forwarded presses retain their owed release even if an overlay opens mid-hold.
#[test]
fn derived_input_release_pairing_prevents_orphan_csi_u_bytes() {
    let model = input_release_pairing_model();
    assert_proves_and_catches(&model);

    let mut untracked_repeat = model.init_state();
    assert!(model.fire("SwallowUntrackedRepeat", &mut untracked_repeat));
    assert_eq!(untracked_repeat["repeat_observed"], 1);
    assert_eq!(untracked_repeat["repeat_emitted"], 0);
    assert_eq!(untracked_repeat["orphan_csi_u"], 0);

    let mut physical = model.init_state();
    for action in [
        "ConsumePhysicalPress",
        "RepeatOfConsumedPress",
        "RepeatOfConsumedPress",
        "ReleaseConsumedPress",
    ] {
        assert!(model.fire(action, &mut physical), "{action}: {physical:?}");
    }
    assert_eq!(physical.get("tracker"), Some(&0));
    assert_eq!(physical.get("repeat_emitted"), Some(&0));
    assert_eq!(physical.get("release_emitted"), Some(&0));

    let mut overlay_close = model.init_state();
    for action in [
        "OpenOverlay",
        "ConsumeOverlayPress",
        "CloseOverlay",
        "ReleaseConsumedPress",
    ] {
        assert!(
            model.fire(action, &mut overlay_close),
            "{action}: {overlay_close:?}"
        );
    }
    assert_eq!(overlay_close.get("orphan_csi_u"), Some(&0));

    let mut forwarded = model.init_state();
    for action in [
        "ForwardPress",
        "OpenOverlay",
        "GateConsumesRepeatOfForwardedPress",
        "ReleaseForwardedPress",
    ] {
        assert!(
            model.fire(action, &mut forwarded),
            "{action}: {forwarded:?}"
        );
    }
    assert_eq!(forwarded.get("pty_press_outstanding"), Some(&0));
    assert_eq!(forwarded.get("release_emitted"), Some(&1));

    // Winit may surface key-up through the newly focused window. Consumed
    // ownership remains byte-silent; forwarded ownership keeps the exact
    // press-time destination instead of re-resolving current focus.
    let mut consumed_after_transfer = model.init_state();
    for action in [
        "ConsumePhysicalPress",
        "TransferFocusWhileHeld",
        "ReleaseConsumedPress",
    ] {
        assert!(
            model.fire(action, &mut consumed_after_transfer),
            "{action}: {consumed_after_transfer:?}"
        );
    }
    assert_eq!(consumed_after_transfer["press_window"], 1);
    assert_eq!(consumed_after_transfer["release_arrival_window"], 2);
    assert_eq!(consumed_after_transfer["release_routed_window"], 0);
    assert_eq!(consumed_after_transfer["release_emitted"], 0);
    assert!(model.check_invariant(
        "ConsumedReleaseIsSwallowedAtAnyFocus",
        &consumed_after_transfer,
    ));

    let mut forwarded_after_transfer = model.init_state();
    for action in [
        "ForwardPress",
        "TransferFocusWhileHeld",
        "ForwardRepeatOfForwardedPress",
        "ReleaseForwardedPress",
    ] {
        assert!(
            model.fire(action, &mut forwarded_after_transfer),
            "{action}: {forwarded_after_transfer:?}"
        );
    }
    assert_eq!(forwarded_after_transfer["press_window"], 1);
    assert_eq!(forwarded_after_transfer["repeat_routed_window"], 1);
    assert_eq!(forwarded_after_transfer["release_arrival_window"], 2);
    assert_eq!(forwarded_after_transfer["release_routed_window"], 1);
    assert_eq!(forwarded_after_transfer["release_emitted"], 1);
    assert!(model.check_invariant(
        "ForwardedReleaseUsesOriginalPressTarget",
        &forwarded_after_transfer,
    ));

    let mut raw_after_transfer = model.init_state();
    for action in [
        "ForwardLiteralPress",
        "TransferFocusWhileHeld",
        "ForwardRepeatOfLiteralPress",
        "ReleaseLiteralPress",
    ] {
        assert!(
            model.fire(action, &mut raw_after_transfer),
            "{action}: {raw_after_transfer:?}"
        );
    }
    assert_eq!(raw_after_transfer["press_window"], 1);
    assert_eq!(raw_after_transfer["repeat_routed_window"], 1);
    assert_eq!(raw_after_transfer["release_arrival_window"], 2);
    assert_eq!(raw_after_transfer["release_routed_window"], 0);
    assert_eq!(raw_after_transfer["release_emitted"], 0);
    assert!(model.check_invariant(
        "LiteralInputRetainsSilentReleaseOwnership",
        &raw_after_transfer,
    ));

    let mut local_after_transfer = model.init_state();
    for action in [
        "CaptureLocalRepeatPress",
        "TransferFocusWhileHeld",
        "ForwardLocalRepeat",
        "ReleaseLocalRepeatPress",
    ] {
        assert!(
            model.fire(action, &mut local_after_transfer),
            "{action}: {local_after_transfer:?}"
        );
    }
    assert_eq!(local_after_transfer["press_window"], 1);
    assert_eq!(local_after_transfer["repeat_routed_window"], 1);
    assert_eq!(local_after_transfer["release_arrival_window"], 2);
    assert_eq!(local_after_transfer["release_routed_window"], 0);
    assert_eq!(local_after_transfer["release_emitted"], 0);
    assert!(model.check_invariant(
        "LocalRepeatRetainsSilentReleaseOwnership",
        &local_after_transfer,
    ));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut orphan_repeat = buggy.init_state();
    assert!(buggy.fire("SwallowUntrackedRepeat", &mut orphan_repeat));
    assert!(!buggy.check_invariant("NoOrphanCsiUBytes", &orphan_repeat));

    let mut orphan_release = buggy.init_state();
    assert!(buggy.fire("ConsumePhysicalPress", &mut orphan_release));
    assert!(buggy.fire("ReleaseConsumedPress", &mut orphan_release));
    assert!(!buggy.check_invariant("NoOrphanCsiUBytes", &orphan_release));
    assert!(!buggy.check_invariant("ConsumedPressEpisodeIsByteSilent", &orphan_release,));

    // The release-time chord re-lookup: a literal or local-repeat hold whose
    // release misses its chord falls through and reports a release the PTY
    // never saw a press for.
    for (press, release, law) in [
        (
            "ForwardLiteralPress",
            "ReleaseLiteralPress",
            "LiteralInputRetainsSilentReleaseOwnership",
        ),
        (
            "CaptureLocalRepeatPress",
            "ReleaseLocalRepeatPress",
            "LocalRepeatRetainsSilentReleaseOwnership",
        ),
    ] {
        let mut leaked = buggy.init_state();
        assert!(buggy.fire(press, &mut leaked));
        assert!(buggy.fire(release, &mut leaked));
        assert_eq!(leaked["release_emitted"], 1, "{release}");
        assert!(!buggy.check_invariant(law, &leaked), "{law}");
    }

    let mut repeat_redecides = buggy.init_state();
    assert!(buggy.fire("ForwardPress", &mut repeat_redecides));
    assert!(buggy.fire("OpenOverlay", &mut repeat_redecides));
    assert!(buggy.fire("GateConsumesRepeatOfForwardedPress", &mut repeat_redecides,));
    assert!(!buggy.check_invariant("TrackerAndPtyOutstandingAreExclusive", &repeat_redecides,));

    let mut swallowed_owed_release = buggy.init_state();
    for action in ["ForwardPress", "OpenOverlay", "ReleaseForwardedPress"] {
        assert!(
            buggy.fire(action, &mut swallowed_owed_release),
            "{action}: {swallowed_owed_release:?}"
        );
    }
    assert!(!buggy.check_invariant("UntrackedReleaseNeverSwallowed", &swallowed_owed_release,));
    assert!(!buggy.check_invariant(
        "ForwardedPressRemainsOwedUntilRelease",
        &swallowed_owed_release,
    ));

    let mut misrouted = buggy.init_state();
    for action in [
        "ForwardPress",
        "TransferFocusWhileHeld",
        "ReleaseForwardedPress",
    ] {
        assert!(
            buggy.fire(action, &mut misrouted),
            "{action}: {misrouted:?}"
        );
    }
    assert_eq!(misrouted["press_window"], 1);
    assert_eq!(misrouted["release_routed_window"], 2);
    assert!(!buggy.check_invariant("ForwardedReleaseUsesOriginalPressTarget", &misrouted,));
    assert!(!buggy.check_invariant("NoFabricatedReleaseTarget", &misrouted));

    let mut repeat_misrouted = buggy.init_state();
    for action in [
        "ForwardLiteralPress",
        "TransferFocusWhileHeld",
        "ForwardRepeatOfLiteralPress",
    ] {
        assert!(
            buggy.fire(action, &mut repeat_misrouted),
            "{action}: {repeat_misrouted:?}"
        );
    }
    assert_eq!(repeat_misrouted["press_window"], 1);
    assert_eq!(repeat_misrouted["repeat_routed_window"], 2);
    assert!(!buggy.check_invariant("EmittedRepeatUsesOriginalPressTarget", &repeat_misrouted,));

    let mut local_repeat_misrouted = buggy.init_state();
    for action in [
        "CaptureLocalRepeatPress",
        "TransferFocusWhileHeld",
        "ForwardLocalRepeat",
    ] {
        assert!(
            buggy.fire(action, &mut local_repeat_misrouted),
            "{action}: {local_repeat_misrouted:?}"
        );
    }
    assert_eq!(local_repeat_misrouted["press_window"], 1);
    assert_eq!(local_repeat_misrouted["repeat_routed_window"], 2);
    assert!(!buggy.check_invariant(
        "EmittedRepeatUsesOriginalPressTarget",
        &local_repeat_misrouted,
    ));
}

/// Full overlap handoff: modern ProofReady is provisional until every mutable
/// parent fact is rechecked and Commit+exit occurs; the child remains readerless
/// until Commit. Every rejection kills/reaps before parent resume/teardown. The
/// legacy one-byte branch is admitted only for an exact, strictly-newer,
/// zero-history payload.
/// PER-SESSION ownership across the seamless handoff, which is the half the
/// overlap model above does not state: it counts readers globally
/// (`parent_readers + child_readers <= 1`) and never says WHICH process owns
/// WHICH pty. That distinction is the whole feature — a handoff that keeps the
/// reader count legal while dropping one session on the floor satisfies the
/// older model and loses a user's terminal.
///
/// So this one carries ownership per session and proves the five properties
/// that together mean "seamless": no session is ever orphaned, an owner still
/// holds its master, no master is ever read twice, Commit needs a matching
/// proof and happens at most once, and rollback stays available until it.
#[test]
fn derived_seamless_handoff_ownership_proves_and_catches_orphan_and_double_reader() {
    let model = native_update_seamless_handoff_ownership_model();
    assert_proves_and_catches(&model);
    assert!(
        aterm_spec::interp::find_deadlock(&model, |_| false).is_none(),
        "the healthy handoff must always commit, roll back, or settle"
    );
}

#[test]
fn derived_native_update_overlap_handoff_proves_and_catches_ownership_regressions() {
    let model = native_update_overlap_handoff_model();
    assert_proves_and_catches(&model);

    // Regression for the global xref gate's strict-vacuity pass: once one
    // admission fact rejects the handoff, later independent revocations must
    // not create a power set of semantically equivalent refusal states.  Keep
    // this focused check here so a state-space regression fails in seconds,
    // rather than several minutes into `aterm-gui::spec_xref_closure`.
    const EXPECTED_DEAD: [&str; 11] = [
        "CommitWithoutFreshExactProof",
        "AckInexactLegacyBridge",
        "BuggyExitIgnoringFailedCommitWrite",
        "BuggyReleaseReadersOnProof",
        "BuggySignalLeaderOnly",
        "BuggyResumeParentBeforeReap",
        "BuggyWaitBeforeGroupSignal",
        "BuggyKillAfterCommitWin",
        "BuggyGrantBeforePark",
        "BuggyReleaseReadersUngranted",
        "BuggyRetireUngrantedWithSuccessorLive",
    ];
    let healthy_fired =
        aterm_spec::interp::fired_actions(&aterm_spec::interp::with_buggy(&model, 0));
    let healthy_dead: Vec<_> = model
        .actions
        .iter()
        .map(|action| action.name)
        .filter(|name| !healthy_fired.contains(name))
        .collect();
    assert_eq!(healthy_dead, EXPECTED_DEAD);
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &EXPECTED_DEAD),
        Ok(EXPECTED_DEAD.len()),
        "every committed-dead action must remain an independently caught mutant"
    );

    let deadlock = aterm_spec::interp::find_deadlock(&model, |_| false);
    assert!(
        deadlock.is_none(),
        "every handoff must activate or restore the parent without wedging: {deadlock:?}"
    );

    let mut modern = model.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "ChildPaintsExactProof",
        "MainWinsCommitArbiter",
        "CommitModern",
        // The diagnostic event-loop Wake can disappear after irreversible
        // Commit; reader activation remains directly enabled.
        "LoseDiagnosticWake",
        "ReleaseModernReaders",
    ] {
        assert!(model.fire(action, &mut modern), "{action}: {modern:?}");
    }
    assert_eq!(modern.get("commit"), Some(&1));
    assert_eq!(modern.get("child_readers"), Some(&1));
    assert_eq!(modern.get("diagnostic_wake"), Some(&0));

    let mut revoked = model.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "ChildPaintsExactProof",
        "ActivityRevokesEpoch",
        "WorkerWinsRejectArbiter",
    ] {
        assert!(model.fire(action, &mut revoked));
    }
    assert!(model.successors("CommitModern", &revoked).is_empty());
    for action in [
        "KillRejectedChild",
        "ReapKilledChild",
        "ResumeParentAfterReap",
    ] {
        assert!(model.fire(action, &mut revoked), "{action}: {revoked:?}");
    }
    assert_eq!(revoked.get("parent_readers"), Some(&1));
    assert_eq!(revoked.get("child_reaped"), Some(&1));

    let mut write_failed = model.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "ChildPaintsExactProof",
        "MainWinsCommitArbiter",
        "CommitWriteFails",
        "KillRejectedChild",
        "ReapKilledChild",
        "ResumeParentAfterReap",
    ] {
        assert!(
            model.fire(action, &mut write_failed),
            "{action}: {write_failed:?}"
        );
    }
    assert_eq!(write_failed.get("commit"), Some(&0));
    assert_eq!(write_failed.get("parent_exited"), Some(&0));
    assert_eq!(write_failed.get("parent_readers"), Some(&1));
    assert_eq!(write_failed.get("commit_write_failed"), Some(&1));

    let mut exited_leader = model.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "SpawnProcessGroupDescendant",
        "LeaderDiesLeavingLiveDescendant",
        "WorkerWinsRejectArbiter",
        "KillRejectedChild",
        "ReapKilledChild",
        "ResumeParentAfterReap",
    ] {
        assert!(
            model.fire(action, &mut exited_leader),
            "{action}: {exited_leader:?}"
        );
    }
    assert_eq!(exited_leader.get("leader_dead_with_descendant"), Some(&1));
    assert_eq!(exited_leader.get("group_signaled"), Some(&1));
    assert_eq!(exited_leader.get("descendant_live"), Some(&0));
    assert_eq!(exited_leader.get("child_reaped"), Some(&1));
    assert_eq!(exited_leader.get("parent_resumed"), Some(&1));

    let mut teardown = model.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "DestructiveIntentRevokesCommit",
        "WorkerWinsRejectArbiter",
        "KillRejectedChild",
        "ReapKilledChild",
        "ResumeParentAfterReap",
        "ReplayDeferredTeardown",
    ] {
        assert!(model.fire(action, &mut teardown), "{action}: {teardown:?}");
    }
    assert_eq!(teardown.get("teardown_replayed"), Some(&1));

    let mut legacy = model.init_state();
    for action in [
        "SelectLegacyBridge",
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "ChildPaintsExactProof",
        "AckGuardedLegacyBridge",
    ] {
        assert!(model.fire(action, &mut legacy), "{action}: {legacy:?}");
    }
    assert_eq!(legacy.get("legacy_ack"), Some(&1));
    assert_eq!(legacy.get("child_readers"), Some(&1));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut partial = buggy.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "ChildSendsPartialProof",
        "CommitWithoutFreshExactProof",
        "CommitModern",
    ] {
        assert!(buggy.fire(action, &mut partial), "{action}: {partial:?}");
    }
    assert!(!buggy.check_invariant("ModernCommitRequiresFreshExactProof", &partial));

    let mut release_on_proof = buggy.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "ChildPaintsExactProof",
        "BuggyReleaseReadersOnProof",
    ] {
        assert!(buggy.fire(action, &mut release_on_proof));
    }
    assert!(!buggy.check_invariant(
        "ChildReadersRequireIrreversibleAuthority",
        &release_on_proof,
    ));

    let mut early_resume = buggy.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "ChildSendsMismatchedProof",
        "WorkerWinsRejectArbiter",
        "KillRejectedChild",
        "BuggyResumeParentBeforeReap",
    ] {
        assert!(buggy.fire(action, &mut early_resume));
    }
    assert!(!buggy.check_invariant("RollbackResumeRequiresKillAndReap", &early_resume,));

    let mut wait_before_kill = buggy.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "SpawnProcessGroupDescendant",
        "LeaderDiesLeavingLiveDescendant",
        "WorkerWinsRejectArbiter",
        "BuggyWaitBeforeGroupSignal",
    ] {
        assert!(
            buggy.fire(action, &mut wait_before_kill),
            "{action}: {wait_before_kill:?}"
        );
    }
    assert!(!buggy.check_invariant(
        "ProcessGroupSignalPrecedesDirectChildReap",
        &wait_before_kill,
    ));
    assert_eq!(wait_before_kill.get("descendant_live"), Some(&1));

    // Explicit commit-vs-kill collision: whichever CAS wins is the only legal
    // irreversible continuation. A late worker cancellation after the main
    // winner cannot signal or kill; if the worker wins first, Commit is disabled.
    let mut main_wins = model.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "ChildPaintsExactProof",
        "ArmConcurrentRejectContender",
        "MainWinsCommitArbiter",
        "WorkerLosesRejectRace",
        "CommitModern",
        "ReleaseModernReaders",
    ] {
        assert!(
            model.fire(action, &mut main_wins),
            "{action}: {main_wins:?}"
        );
    }
    assert_eq!(main_wins.get("commit"), Some(&1));
    assert_eq!(main_wins.get("child_killed"), Some(&0));

    let mut worker_wins = model.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "ChildPaintsExactProof",
        "ArmConcurrentRejectContender",
        "WorkerWinsRejectArbiter",
    ] {
        assert!(model.fire(action, &mut worker_wins));
    }
    assert!(
        model
            .successors("MainWinsCommitArbiter", &worker_wins)
            .is_empty()
    );
    assert!(model.successors("CommitModern", &worker_wins).is_empty());
    assert!(model.fire("KillRejectedChild", &mut worker_wins));

    let mut kill_after_commit_win = buggy.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "ChildPaintsExactProof",
        "MainWinsCommitArbiter",
        "BuggyKillAfterCommitWin",
    ] {
        assert!(buggy.fire(action, &mut kill_after_commit_win));
    }
    assert!(!buggy.check_invariant(
        "AtomicArbiterExcludesKillAfterCommitWin",
        &kill_after_commit_win,
    ));

    // THE LATE PARK (2026-09-19): the launched lane. The successor exists before
    // the park, holding nothing; the grant follows the park; from there the
    // decision is the fork lane's.
    let mut launched = model.init_state();
    for action in [
        "LaunchSuccessorBeforePark",
        "ParkParentReaders",
        "GrantDescriptorsToBootedSuccessor",
        "ChildPaintsExactProof",
        "MainWinsCommitArbiter",
        "CommitModern",
        "ReleaseModernReaders",
    ] {
        assert!(model.fire(action, &mut launched), "{action}: {launched:?}");
    }
    assert_eq!(launched.get("commit"), Some(&1));
    assert_eq!(launched.get("child_readers"), Some(&1));
    assert_eq!(launched.get("launched_early"), Some(&1));

    // A revocation BEFORE the park kills, reaps and retires the candidate with
    // the parent's readers never stopped: no rollback, no resume.
    let mut stood_down = model.init_state();
    for action in [
        "LaunchSuccessorBeforePark",
        "RevokeUngrantedSuccessor",
        "KillUngrantedSuccessor",
        "ReapUngrantedSuccessor",
        "RetireUngrantedAttempt",
    ] {
        assert!(
            model.fire(action, &mut stood_down),
            "{action}: {stood_down:?}"
        );
    }
    assert_eq!(stood_down.get("parent_readers"), Some(&1));
    assert_eq!(stood_down.get("parent_parked"), Some(&0));
    assert_eq!(stood_down.get("retired"), Some(&1));
    assert!(
        model
            .successors("ResumeParentAfterReap", &stood_down)
            .is_empty()
    );
    assert!(
        model
            .successors("ParkParentReaders", &stood_down)
            .is_empty()
    );
    assert!(
        model
            .successors("LaunchSuccessorBeforePark", &stood_down)
            .is_empty()
    );

    // A park whose capture missed resumes the readers beside the live,
    // ungranted candidate and parks again — legitimately, since nothing left.
    let mut reparked = model.init_state();
    for action in [
        "LaunchSuccessorBeforePark",
        "ParkParentReaders",
        "UnparkForRepark",
        "ParkParentReaders",
        "GrantDescriptorsToBootedSuccessor",
        "ChildPaintsExactProof",
        "MainWinsCommitArbiter",
        "CommitModern",
    ] {
        assert!(model.fire(action, &mut reparked), "{action}: {reparked:?}");
    }
    assert_eq!(reparked.get("commit"), Some(&1));
    // …and a revocation after the un-park still retires the candidate.
    let mut unparked_then_revoked = model.init_state();
    for action in [
        "LaunchSuccessorBeforePark",
        "ParkParentReaders",
        "UnparkForRepark",
        "RevokeUngrantedSuccessor",
        "KillUngrantedSuccessor",
        "ReapUngrantedSuccessor",
        "RetireUngrantedAttempt",
    ] {
        assert!(
            model.fire(action, &mut unparked_then_revoked),
            "{action}: {unparked_then_revoked:?}"
        );
    }
    assert_eq!(unparked_then_revoked.get("parent_readers"), Some(&1));

    // The three laws the lane adds, each falsified by its own mutant.
    let mut grant_before_park = buggy.init_state();
    for action in ["LaunchSuccessorBeforePark", "BuggyGrantBeforePark"] {
        assert!(buggy.fire(action, &mut grant_before_park));
    }
    assert!(!buggy.check_invariant("GrantRequiresParkedParent", &grant_before_park));
    assert!(!buggy.check_invariant(
        "ReadersBesideLiveCandidateOnlyUngranted",
        &grant_before_park
    ));
    let mut reads_ungranted = buggy.init_state();
    for action in ["LaunchSuccessorBeforePark", "BuggyReleaseReadersUngranted"] {
        assert!(buggy.fire(action, &mut reads_ungranted));
    }
    assert!(!buggy.check_invariant("UngrantedSuccessorNeverReads", &reads_ungranted));
    let mut retired_live = buggy.init_state();
    for action in [
        "LaunchSuccessorBeforePark",
        "RevokeUngrantedSuccessor",
        "BuggyRetireUngrantedWithSuccessorLive",
    ] {
        assert!(buggy.fire(action, &mut retired_live));
    }
    assert!(!buggy.check_invariant("UngrantedRetireRequiresReap", &retired_live));

    let mut scrolled_legacy = buggy.init_state();
    for action in [
        "SelectLegacyBridge",
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "ChildPaintsExactProof",
        "LegacyPayloadIsScrolledOrAmbiguous",
        "AckInexactLegacyBridge",
    ] {
        assert!(buggy.fire(action, &mut scrolled_legacy));
    }
    assert!(!buggy.check_invariant("LegacyAckRequiresExactZeroHistoryBridge", &scrolled_legacy,));

    // The Commit write's result ignored: an EPIPE from a child that is already
    // gone still ends the parent, and nobody owns the terminal.
    let mut exited_on_epipe = buggy.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "ChildPaintsExactProof",
        "MainWinsCommitArbiter",
        "BuggyExitIgnoringFailedCommitWrite",
    ] {
        assert!(buggy.fire(action, &mut exited_on_epipe), "{action}");
    }
    assert_eq!(exited_on_epipe.get("commit"), Some(&0));
    assert!(!buggy.check_invariant("ParentExitRequiresCommitOrLegacyAck", &exited_on_epipe));

    // The reject sweep signals the leader alone; its descendant runs on.
    let mut leader_only = buggy.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "SpawnProcessGroupDescendant",
        "ActivityRevokesEpoch",
        "WorkerWinsRejectArbiter",
        "BuggySignalLeaderOnly",
    ] {
        assert!(buggy.fire(action, &mut leader_only), "{action}");
    }
    assert_eq!(leader_only.get("descendant_live"), Some(&1));
    assert!(!buggy.check_invariant("GroupSignalEliminatesLiveDescendants", &leader_only));
    assert_every_invariant_carries_a_mutant(&model, &[]);
}

/// Every incomplete prefix remains ordinary. Only the complete canonical token
/// activates, while `fuc`, `fix`, `future`, and `fuchsia` remain inactive.
#[test]
fn derived_exact_profanity_completion_rejects_predictive_fuc() {
    let model = exact_profanity_completion_model();
    assert_proves_and_catches(&model);
    assert!(
        aterm_spec::interp::find_deadlock(&model, |_| false).is_none(),
        "every bounded prefix/context state must classify or settle"
    );

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let f = buggy.successors("TypeF", &buggy.init_state())[0].clone();
    let fu = buggy.successors("TypeU", &f)[0].clone();
    let fuc = buggy.successors("TypeC", &fu)[0].clone();
    assert!(!buggy.check_invariant("EveryProperPrefixIsOrdinary", &fuc));
    assert!(!buggy.check_invariant("ActivationRequiresCompleteFuck", &fuc));
    // The prefix's cue cannot be taken back: a harmless continuation or a
    // delimiter retracts the highlight but keeps the episode it fired…
    for settle in ["TypeFuchsiaAfterFuc", "SettleFuc"] {
        let kept = buggy.successors(settle, &fuc)[0].clone();
        assert_eq!((kept["active"], kept["episode"]), (0, 1), "{settle}");
        assert!(!buggy.check_invariant("HarmlessAndSettledAreInactive", &kept));
    }
    // …and the completing `k` cues a second time.
    let doubled = buggy.successors("TypeK", &fuc)[0].clone();
    assert_eq!(doubled["episode"], 2);
    assert!(!buggy.check_invariant("CompletionCreatesExactlyOneEpisode", &doubled));

    for actions in [
        &["TypeF", "TypeFixAfterF"][..],
        &["TypeF", "TypeU", "TypeFutureAfterFu"][..],
        &["TypeF", "TypeU", "TypeC", "TypeFuchsiaAfterFuc"][..],
        &["SuppressedFucContext"][..],
        &["IgnoredFuc"][..],
    ] {
        let mut state = model.init_state();
        for action in actions {
            assert!(model.fire(action, &mut state), "{action} must be reachable");
        }
        assert_eq!(state.get("active"), Some(&0), "{actions:?} activated");
    }

    let mut completed = model.init_state();
    for action in ["TypeF", "TypeU", "TypeC", "TypeK"] {
        assert!(model.fire(action, &mut completed));
    }
    assert_eq!(completed.get("active"), Some(&1));
    assert_eq!(completed.get("episode"), Some(&1));
}

/// Native updater physical transaction: exact OLD and NEW identities, fixed
/// rollback retention across every modeled process-crash cut, exact receipt
/// recovery, first-present + proof + disarm before GC, and startup authority
/// consumption before boot-health observation. The negative control admits an
/// inherited-authority early return, build-only OLD identity, pre-present
/// disarm, and premature GC after a failed health proof.
#[test]
fn derived_native_update_disk_transaction_proves_and_catches_identity_or_early_gc() {
    let model = native_update_disk_transaction_model();
    assert_proves_and_catches(&model);
    assert!(
        aterm_spec::interp::find_deadlock(&model, |_| false).is_none(),
        "healthy transaction must recover/reject supersession and every process-crash cut"
    );

    let startup = model.successors("ConsumeStartupAuthority", &model.init_state())[0].clone();
    let observed = model.successors("ObserveBootHealth", &startup)[0].clone();
    let disk = model.successors("EnterDiskLane", &observed)[0].clone();

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let inherited = buggy.successors("InheritMalformedAuthority", &buggy.init_state())[0].clone();
    let returned = buggy.successors("BuggyReturnInheritedAuthority", &inherited)[0].clone();
    assert!(!buggy.check_invariant("InheritedAuthorityClearedBeforeVerification", &returned,));
    assert!(!buggy.check_invariant("BootHealthObservedBeforeStartupVerdict", &returned,));

    // Genuine v0.52 child startup is a reachable POST-swap state, not an OLD
    // transaction init: NEW is canonical, exact OLD is fixed, trial is armed,
    // and both ready + receipt are absent. The retired v0.52 branch synthesized a
    // receipt from sealed disk evidence at this point; it is gone, and the
    // surviving route rebuilds the receipt from the `ready.toml` the current
    // updater still retains until its receipt commits.
    let legacy = model.successors("EnterLegacyPostSwapExact", &model.init_state())[0].clone();
    assert_eq!(legacy.get("installed"), Some(&2));
    assert_eq!(legacy.get("fixed"), Some(&1));
    assert_eq!(legacy.get("trial"), Some(&1));
    assert_eq!(legacy.get("receipt"), Some(&0));
    assert_eq!(legacy.get("ready_present"), Some(&0));
    let ready = model.successors("SupplyModernReadyRecovery", &legacy)[0].clone();
    let consumed = model.successors("ConsumeStartupAuthority", &ready)[0].clone();
    let migrated = model.successors("RecoverModernReceiptFromReady", &consumed)[0].clone();
    let observed_legacy = model.successors("ObserveBootHealth", &migrated)[0].clone();
    let returned_legacy =
        model.successors("ReturnAfterRecoveredLegacy", &observed_legacy)[0].clone();
    assert_eq!(returned_legacy.get("receipt_exact"), Some(&1));
    assert_eq!(returned_legacy.get("rollback_verified"), Some(&1));

    // A parent that crashed before writing its re-exec stamp AND left no ready
    // record now has nothing to rebuild from. The retired v0.52 branch recovered
    // this from sealed disk facts alone; today it DEFERS, keeping the armed trial
    // and fixed rollback so a later launch can still act. `legacy_variant` is the
    // model's mutex, so losing the stamp and supplying ready are distinct runs.
    let no_stamp = model.successors("LoseLegacyReexecAuthority", &legacy)[0].clone();
    let consumed = model.successors("ConsumeStartupAuthority", &no_stamp)[0].clone();
    assert!(
        model
            .successors("RecoverModernReceiptFromReady", &consumed)
            .is_empty(),
        "no ready record leaves no recovery route"
    );
    let deferred = model.successors("RefuseLegacyMissingRecoveryEvidence", &consumed)[0].clone();
    assert_eq!(deferred.get("startup_deferred"), Some(&1));
    assert_eq!(deferred.get("trial"), Some(&1));
    assert_eq!(deferred.get("fixed"), Some(&1));
    assert_eq!(deferred.get("receipt"), Some(&0));

    // Every mismatched sealed build/commit, trial build/digest, or predecessor
    // proof refuses synthesis and preserves the armed trial + fixed rollback.
    for (corrupt, refuse) in [
        (
            "CorruptLegacyCurrentBuild",
            "RefuseLegacyCurrentBuildMismatch",
        ),
        ("CorruptLegacySentinel", "RefuseLegacySentinelMismatch"),
        (
            "CorruptLegacyCurrentCommit",
            "RefuseLegacyCurrentCommitMismatch",
        ),
        ("CorruptLegacyTrialBuild", "RefuseLegacyTrialBuildMismatch"),
        (
            "CorruptLegacyTrialDigest",
            "RefuseLegacyTrialDigestMismatch",
        ),
        ("CorruptLegacyRollback", "RefuseLegacyRollbackMismatch"),
    ] {
        let legacy = model.successors("EnterLegacyPostSwapExact", &model.init_state())[0].clone();
        let corrupt_state = model.successors(corrupt, &legacy)[0].clone();
        let consumed = model.successors("ConsumeStartupAuthority", &corrupt_state)[0].clone();
        assert!(
            model
                .successors("RecoverModernReceiptFromReady", &consumed)
                .is_empty(),
            "{corrupt} still authorized receipt recovery"
        );
        let refused = model.successors(refuse, &consumed)[0].clone();
        assert_eq!(refused.get("startup_deferred"), Some(&1));
        assert_eq!(refused.get("trial"), Some(&1));
        assert_eq!(refused.get("fixed"), Some(&1));
        assert_eq!(refused.get("receipt"), Some(&0));
    }

    // Recovery requires the ready record: without it there is no route at all,
    // which is what makes `ready.toml` the load-bearing evidence now that the
    // sealed-disk synthesis branch is gone.
    let legacy = model.successors("EnterLegacyPostSwapExact", &model.init_state())[0].clone();
    let consumed_dry = model.successors("ConsumeStartupAuthority", &legacy)[0].clone();
    assert!(
        model
            .successors("RecoverModernReceiptFromReady", &consumed_dry)
            .is_empty(),
        "receipt recovery without a ready record must have no route"
    );
    let modern = model.successors("SupplyModernReadyRecovery", &legacy)[0].clone();
    let consumed = model.successors("ConsumeStartupAuthority", &modern)[0].clone();
    let recovered = model.successors("RecoverModernReceiptFromReady", &consumed)[0].clone();
    assert_eq!(recovered.get("modern_receipt_recovered"), Some(&1));

    let buggy_legacy = buggy.successors("EnterLegacyPostSwapExact", &buggy.init_state())[0].clone();

    let early = buggy.successors("BuggyReturnInheritedAuthority", &buggy_legacy)[0].clone();
    assert!(!buggy.check_invariant("LegacyStartupReturnRequiresReceiptProof", &early,));

    let buggy_startup = buggy.successors("ConsumeStartupAuthority", &buggy.init_state())[0].clone();
    let buggy_observed = buggy.successors("ObserveBootHealth", &buggy_startup)[0].clone();
    let buggy_disk = buggy.successors("EnterDiskLane", &buggy_observed)[0].clone();
    let wrong_old = buggy.successors("CorruptOldCommit", &buggy_disk)[0].clone();
    let prepared = buggy.successors("PrepareFromBuildOnlyOld", &wrong_old)[0].clone();
    assert!(!buggy.check_invariant("PreparedRequiresExactOldIdentity", &prepared));

    let stale_previous = buggy.successors("CorruptPreviousReceipt", &buggy_disk)[0].clone();
    let retained =
        buggy.successors("PrepareSavingMismatchedPreviousReceipt", &stale_previous)[0].clone();
    assert!(!buggy.check_invariant("SavedPreviousReceiptBindsSealedOld", &retained));

    let prepared = buggy.successors("PrepareFixedNew", &buggy_disk)[0].clone();
    let armed = buggy.successors("ArmExactTrial", &prepared)[0].clone();
    let swapped = buggy.successors("AtomicSwap", &armed)[0].clone();
    let receipt = buggy.successors("RecordExactReceipt", &swapped)[0].clone();
    let pre_present_disarm = buggy.successors("DisarmBeforeHealthProof", &receipt)[0].clone();
    assert!(!buggy.check_invariant("HealthDisarmRequiresFirstPresent", &pre_present_disarm,));

    let presented = buggy.successors("PresentInstalledUi", &receipt)[0].clone();
    let early_gc = buggy.successors("DiscardRollbackAfterFailedProof", &presented)[0].clone();
    assert!(!buggy.check_invariant("GarbageCollectionRequiresProofAndDisarm", &early_gc,));
    assert!(!buggy.check_invariant("FailedProofPreservesRecoveryAuthority", &early_gc,));

    // Healthy startup and failure actions are also executable, not decorative.
    let prepared = model.successors("PrepareFixedNew", &disk)[0].clone();
    let armed = model.successors("ArmExactTrial", &prepared)[0].clone();
    let disarm_failed = model.successors("SwapFailsDisarmFails", &armed)[0].clone();
    assert!(model.check_invariant("FailedDisarmPreservesRecoveryAuthority", &disarm_failed,));
    assert!(model.check_invariant("FailedSwapNeverReplacesOld", &disarm_failed,));

    let armed = model.successors("ArmExactTrial", &prepared)[0].clone();
    let swapped = model.successors("AtomicSwap", &armed)[0].clone();
    let receipt_cut = model.successors("CrashAfterSwapBeforeReceipt", &swapped)[0].clone();
    let receipt = model.successors("RecoverExactReceipt", &receipt_cut)[0].clone();
    let verified = model.successors("VerifyExactRollback", &receipt)[0].clone();
    let exec_failed = model.successors("ExecFails", &verified)[0].clone();
    let rollback_failed = model.successors("RestoreExactOldFails", &exec_failed)[0].clone();
    assert!(model.check_invariant("FailedRollbackPreservesRecoveryAuthority", &rollback_failed,));
    assert!(model.check_invariant("ExecFailureCannotGcBeforeRestore", &rollback_failed,));

    let exact_restored = model.successors("RestoreExactOld", &exec_failed)[0].clone();
    let receipt_restored =
        model.successors("DisarmRestoredTrialAndRestoreBoundReceipt", &exact_restored)[0].clone();
    assert_eq!(receipt_restored.get("old_receipt_restored"), Some(&1));
    assert_eq!(receipt_restored.get("receipt"), Some(&0));

    // A parsed local receipt that does not bind the just-verified OLD identity
    // is never retained. Inverse rollback clears NEW's receipt instead of
    // resurrecting the stale value as OLD authority.
    let stale = model.successors("CorruptPreviousReceipt", &disk)[0].clone();
    let prepared = model.successors("PrepareFixedNew", &stale)[0].clone();
    assert_eq!(prepared.get("previous_receipt_saved"), Some(&0));
    let armed = model.successors("ArmExactTrial", &prepared)[0].clone();
    let swapped = model.successors("AtomicSwap", &armed)[0].clone();
    let receipt = model.successors("RecordExactReceipt", &swapped)[0].clone();
    let verified = model.successors("VerifyExactRollback", &receipt)[0].clone();
    let exec_failed = model.successors("ExecFails", &verified)[0].clone();
    let stale_restored = model.successors("RestoreExactOld", &exec_failed)[0].clone();
    let cleared =
        model.successors("DisarmRestoredTrialAndClearUnboundReceipt", &stale_restored)[0].clone();
    assert_eq!(cleared.get("old_receipt_restored"), Some(&0));
    assert_eq!(cleared.get("superseded_receipt_cleared"), Some(&1));

    let fail_open =
        buggy.successors("KeepSupersededReceiptAfterRestoreFailure", &exact_restored)[0].clone();
    assert!(!buggy.check_invariant("RestoreFailureClearsSupersededNewReceipt", &fail_open));

    let superseded = buggy.successors("SupersedeStagedIdentity", &armed)[0].clone();
    let wrong_swap = buggy.successors("SwapSupersededStagedIdentity", &superseded)[0].clone();
    assert!(!buggy.check_invariant("InstalledBundleNeverMissing", &wrong_swap));
    assert!(!buggy.check_invariant("SwapMatchesAuthorizedNew", &wrong_swap));

    // Even a write failure after OLD is restored cannot leave failed NEW's
    // receipt as authority. The failure is explicit and the receipt is absent.
    assert!(
        model
            .successors(
                "DisarmRestoredTrialReceiptRestoreFailsClosed",
                &stale_restored,
            )
            .is_empty(),
        "restore-write failure requires a bound previous receipt"
    );
    let failed_closed = model.successors(
        "DisarmRestoredTrialReceiptRestoreFailsClosed",
        &exact_restored,
    )[0]
    .clone();
    assert_eq!(failed_closed.get("receipt_restore_failed"), Some(&1));
    assert_eq!(failed_closed.get("superseded_receipt_cleared"), Some(&1));
    assert_eq!(failed_closed.get("receipt"), Some(&0));

    // The recovery laws, one mutant apiece: a legacy refusal that disarms the
    // trial it defers for.
    let disarmed_deferral =
        buggy.successors("BuggyDeferLegacyAndDisarmTrial", &consumed_dry)[0].clone();
    assert!(!buggy.check_invariant(
        "LegacyRefusalPreservesRecoveryAuthority",
        &disarmed_deferral
    ));

    // A receipt written ahead of its swap, and a two-rename swap failing between
    // its renames: NEW installed, OLD off the fixed path, trial still armed.
    let early_receipt = buggy.successors("BuggyWriteReceiptBeforeSwap", &armed)[0].clone();
    assert!(!buggy.check_invariant("ReceiptBindsExactNewIdentity", &early_receipt));
    let torn_swap = buggy.successors("BuggyTwoRenameSwapFailsMidway", &armed)[0].clone();
    assert!(!buggy.check_invariant("FailedSwapNeverReplacesOld", &torn_swap));

    // After an exec failure: a failed restore that disarms to stop the loop, and
    // the rollback GC run before any restore.
    let gave_up = buggy.successors("BuggyRestoreFailureDisarmsTrial", &exec_failed)[0].clone();
    assert!(!buggy.check_invariant("FailedRollbackPreservesRecoveryAuthority", &gave_up));
    let collected = buggy.successors("BuggyGcRollbackAfterExecFailure", &exec_failed)[0].clone();
    assert!(!buggy.check_invariant("ExecFailureCannotGcBeforeRestore", &collected));

    // A crash-loop restore of a rollback nobody verified, and a restored
    // rollback GC'd while its trial is still armed.
    assert_eq!(receipt.get("rollback_verified"), Some(&0));
    let unverified = buggy.successors("BuggyRestoreUnverifiedRollback", &receipt)[0].clone();
    assert!(!buggy.check_invariant("CrashLoopRestoreUsesExactOld", &unverified));
    let armed_gc = buggy.successors("BuggyGcRestoredBeforeDisarm", &stale_restored)[0].clone();
    assert!(!buggy.check_invariant("RollbackGcRequiresRestoreAndDisarm", &armed_gc));
    assert_every_invariant_carries_a_mutant(&model, &["CrashBudgetBounded"]);
}

#[test]
fn derived_active_handle_proves_and_catches_stale_handle() {
    // The GLOBAL control-socket ActiveHandle mirror (`active_handle` in aterm-gui's
    // App): `ty` PROVES HandleMirrorsFront at Buggy=0 — every path that moves the
    // frontmost window's active session ALSO re-points the global control handle (the
    // resync_active_or_window -> sync_active_session discipline), so introspection /
    // drive verbs (text/feed/signal) always target the session the user is looking at,
    // under ANY interleaving of front-active changes over the whole bounded space — and
    // CATCHES the "swallow class" at Buggy=1 (a close-collapse / new-window path that
    // re-mirrors only the per-window state via sync_window and forgets the global
    // re-point) -> counterexample on HandleMirrorsFront. This holds the multi-window
    // control target to the same Trust bar as the per-window tab strip: the one global
    // handle never drives a stale or just-closed session (the bug fixed by routing
    // apply_close_outcome / create_window_internal / push_stub_tab through
    // resync_active_or_window).
    assert_proves_and_catches(&active_handle_model());
}

/// Sparkle-words v2 identity episodes (design §3.6/§9): the GUI's grace-TTL
/// persist map freezes the genome per episode (`GenomeFrozen: rolls = births`)
/// and caps novas at one per logical episode (`OneNovaPerEpisode` /
/// `PlayedOnce`), including across a position-key `Rekey`. `ty` PROVES the
/// lifecycle at Buggy=0 and at Buggy=1 catches both v1's grace amnesia and the
/// horizontal-redraw bugs: treating a moved occurrence as fresh re-rolls its
/// genome, while missing a logical move solely because its row-local context
/// changed creates a false birth, resets its spent guards, and re-admits
/// ignition. `RecognitionComplete` and `NoFalseBirths` make that classifier
/// obligation explicit rather than relying on genome equality. Tier-1 binding:
/// `sparkle_identity_conformance_real_persist_map` plus its grace/rekey
/// negative controls drive the real `WordDecorations` map against this model.
#[test]
fn derived_sparkle_identity_proves_and_catches_amnesia_and_rekey() {
    assert_proves_and_catches(&sparkle_identity_model());

    // Pin the rekey-specific counterexample rather than relying only on the
    // earlier GenomeFrozen violation that the aggregate checker reports first.
    let mut buggy = sparkle_identity_model();
    for cst in &mut buggy.consts {
        if cst.0 == "Buggy" {
            cst.1 = 1;
        }
    }
    let mut state = buggy.init_state();
    for action in ["Appear", "Ignite", "Rekey", "Ignite"] {
        assert!(buggy.fire(action, &mut state), "{action} must be reachable");
    }
    assert!(
        !buggy.check_invariant("PlayedOnce", &state),
        "a fresh-identity rekey admits the second logical fire"
    );

    // Pin the stronger recognition-completeness counterexample. ContextMove
    // toggles the bounded row-local context while preserving the logical
    // surface/move. Buggy=1 takes the internally self-consistent fresh path
    // (births and rolls both advance), so GenomeFrozen alone cannot catch it.
    let mut state = buggy.init_state();
    for action in ["Appear", "Ignite", "ContextMove"] {
        assert!(buggy.fire(action, &mut state), "{action} must be reachable");
    }
    assert!(
        buggy.check_invariant("GenomeFrozen", &state),
        "the false birth rolls exactly once, so recognition needs its own invariant"
    );
    assert!(
        !buggy.check_invariant("RecognitionComplete", &state),
        "a changed-context logical move must still be recognized"
    );
    assert!(
        !buggy.check_invariant("NoFalseBirths", &state),
        "a logical move must never allocate a fresh episode"
    );
    assert!(buggy.fire("Ignite", &mut state));
    assert!(
        !buggy.check_invariant("PlayedOnce", &state),
        "the false birth must reproduce the visible second fire"
    );
}

/// Group complement to SparkleIdentity over six explicit matcher premises:
/// global redraw 2→2, global redraw 2→3, anchored log rotation, and a typed
/// retype versus blank grace outside the two-scan weak window. This deliberately
/// does NOT infer identity from cardinality or raw recency alone. Buggy branches
/// cover both directions: blanket recency/context gating creates false births,
/// while anchor/continuity-taint blindness steals an old episode for a logically
/// new occurrence.
#[test]
fn derived_sparkle_reflow_cardinality_proves_and_catches_context_gate() {
    let healthy = sparkle_reflow_cardinality_model();
    assert_proves_and_catches(&healthy);

    // Pin the healthy policy table, including the two 2→2 cases with opposite
    // answers. Their premises — global redraw with NO stationary anchor versus
    // anchored log rotation — are the distinction the old cardinality-only
    // abstraction omitted.
    for (
        action,
        new_count,
        logical_new,
        expected_recognized,
        transferred,
        fresh,
        armed,
        global_redraw,
        stationary_anchor,
        blank_grace,
        typed_retype,
        recent,
        seq_gap,
        stale_same_seed,
        exact_context,
        continuity_tainted,
    ) in [
        ("MovePair", 2, 0, 2, 2, 0, 0, 1, 0, 0, 0, 1, 1, 0, 0, 0),
        ("GrowOne", 3, 1, 2, 2, 1, 1, 1, 0, 0, 0, 1, 1, 0, 0, 0),
        ("RotatePair", 2, 1, 1, 1, 1, 1, 0, 1, 0, 0, 1, 1, 0, 0, 0),
        ("BlankGrace", 1, 0, 1, 1, 0, 0, 0, 0, 1, 0, 0, 3, 1, 1, 0),
        ("TypedRetype", 1, 1, 0, 0, 1, 1, 0, 0, 0, 1, 0, 3, 1, 1, 1),
        (
            "RecentTypedRetype",
            1,
            1,
            0,
            0,
            1,
            1,
            0,
            0,
            0,
            1,
            1,
            2,
            0,
            1,
            1,
        ),
    ] {
        let mut state = healthy.init_state();
        assert!(healthy.fire(action, &mut state));
        assert_eq!(state[&"new_count"], new_count, "{action}");
        assert_eq!(state[&"logical_new"], logical_new, "{action}");
        assert_eq!(state[&"expected_fresh"], logical_new, "{action}");
        assert_eq!(
            state[&"expected_recognized"], expected_recognized,
            "{action}"
        );
        assert_eq!(state[&"recognized"], expected_recognized, "{action}");
        assert_eq!(state[&"transferred"], transferred, "{action}");
        assert_eq!(state[&"fresh"], fresh, "{action}");
        assert_eq!(state[&"armed"], armed, "{action}");
        assert_eq!(state[&"false_births"], 0, "{action}");
        assert_eq!(state[&"false_transfers"], 0, "{action}");
        assert_eq!(state[&"global_redraw"], global_redraw, "{action}");
        assert_eq!(state[&"stationary_anchor"], stationary_anchor, "{action}");
        assert_eq!(state[&"blank_grace"], blank_grace, "{action}");
        assert_eq!(state[&"typed_retype"], typed_retype, "{action}");
        assert_eq!(state[&"recent"], recent, "{action}");
        assert_eq!(state[&"seq_gap"], seq_gap, "{action}");
        assert_eq!(state[&"stale_same_seed"], stale_same_seed, "{action}");
        assert_eq!(state[&"exact_context"], exact_context, "{action}");
        assert_eq!(state[&"continuity_tainted"], continuity_tainted, "{action}");
    }

    let mut buggy = sparkle_reflow_cardinality_model();
    for cst in &mut buggy.consts {
        if cst.0 == "Buggy" {
            cst.1 = 1;
        }
    }
    // Exact-context-only recognition misses both licensed global-redraw
    // survivors. Births/arms exceed logical-new count, and recognition is
    // incomplete even though candidate accounting remains internally sane.
    for (action, fresh, armed) in [("MovePair", 2, 2), ("GrowOne", 3, 3)] {
        let mut state = buggy.init_state();
        assert!(buggy.fire(action, &mut state));
        assert_eq!(state[&"fresh"], fresh);
        assert_eq!(state[&"armed"], armed);
        assert_eq!(state[&"false_births"], 2);
        assert_eq!(state[&"false_transfers"], 0);
        assert_eq!(state[&"expected_fresh"], state[&"logical_new"]);
        assert!(
            !buggy.check_invariant("FreshAtMostNetGrowth", &state),
            "{action}: exact-context gating must exceed net growth"
        );
        assert!(
            !buggy.check_invariant("ArmedAtMostNetGrowth", &state),
            "{action}: false births must expose visible re-arming"
        );
        assert!(!buggy.check_invariant("NoFalseBirths", &state));
        assert!(!buggy.check_invariant("RecognitionComplete", &state));
        assert!(buggy.check_invariant("NoFalseTransfers", &state));
    }

    // A cardinality-only rotation transfers the departed twin into the new
    // bottom slot. The real stationary survivor is still recognized, so the
    // new NoFalseTransfers/FreshMatchesExpected obligations are load-bearing.
    let mut rotated = buggy.init_state();
    assert!(buggy.fire("RotatePair", &mut rotated));
    assert_eq!(rotated[&"transferred"], 2);
    assert_eq!(rotated[&"recognized"], 1);
    assert_eq!(rotated[&"false_transfers"], 1);
    assert_eq!(rotated[&"fresh"], 0);
    assert_eq!(rotated[&"armed"], 0);
    assert_eq!(rotated[&"expected_fresh"], 1);
    assert!(buggy.check_invariant("RecognitionComplete", &rotated));
    assert!(!buggy.check_invariant("NoFalseTransfers", &rotated));
    assert!(!buggy.check_invariant("FreshMatchesExpected", &rotated));

    // Exact seed+context after >2 BLANK occlusion scans remains an untainted
    // grace continuation. A blanket recency gate falsely births and arms it;
    // the recognition and no-false-birth obligations must both reject that.
    let mut blank = buggy.init_state();
    assert!(buggy.fire("BlankGrace", &mut blank));
    assert_eq!(blank[&"blank_grace"], 1);
    assert_eq!(blank[&"recent"], 0);
    assert_eq!(blank[&"seq_gap"], 3);
    assert_eq!(blank[&"stale_same_seed"], 1);
    assert_eq!(blank[&"exact_context"], 1);
    assert_eq!(blank[&"continuity_tainted"], 0);
    assert_eq!(blank[&"logical_new"], 0);
    assert_eq!(blank[&"expected_fresh"], 0);
    assert_eq!(blank[&"expected_recognized"], 1);
    assert_eq!(blank[&"transferred"], 0);
    assert_eq!(blank[&"recognized"], 0);
    assert_eq!(blank[&"fresh"], 1);
    assert_eq!(blank[&"armed"], 1);
    assert_eq!(blank[&"false_births"], 1);
    assert_eq!(blank[&"false_transfers"], 0);
    assert!(!buggy.check_invariant("RecognitionComplete", &blank));
    assert!(!buggy.check_invariant("NoFalseBirths", &blank));
    assert!(buggy.check_invariant("NoFalseTransfers", &blank));
    assert!(!buggy.check_invariant("FreshMatchesExpected", &blank));
    assert!(!buggy.check_invariant("ArmedMatchesExpected", &blank));
    assert!(buggy.check_invariant("BlankGraceUntainted", &blank));
    assert!(buggy.check_invariant("ExactContextCases", &blank));

    // For the feline class, after >2 NONBLANK incremental replacement scans,
    // the SAME seed and exact context return with continuity tainted. An
    // exact-evidence fast path that ignores taint steals the spent episode
    // instead of creating the required visible fresh/armed birth; this is
    // false transfer, not missed recognition. Profanity intentionally follows
    // the conservative full-grace transfer policy and is outside this action.
    let mut retyped = buggy.init_state();
    assert!(buggy.fire("TypedRetype", &mut retyped));
    assert_eq!(retyped[&"typed_retype"], 1);
    assert_eq!(retyped[&"recent"], 0);
    assert_eq!(retyped[&"seq_gap"], 3);
    assert_eq!(retyped[&"stale_same_seed"], 1);
    assert_eq!(retyped[&"exact_context"], 1);
    assert_eq!(retyped[&"continuity_tainted"], 1);
    assert_eq!(retyped[&"logical_new"], 1);
    assert_eq!(retyped[&"expected_fresh"], 1);
    assert_eq!(retyped[&"transferred"], 1);
    assert_eq!(retyped[&"recognized"], 0);
    assert_eq!(retyped[&"false_transfers"], 1);
    assert_eq!(retyped[&"fresh"], 0);
    assert_eq!(retyped[&"armed"], 0);
    assert!(buggy.check_invariant("RecognitionComplete", &retyped));
    assert!(buggy.check_invariant("NoFalseBirths", &retyped));
    assert!(!buggy.check_invariant("NoFalseTransfers", &retyped));
    assert!(!buggy.check_invariant("FreshMatchesExpected", &retyped));
    assert!(!buggy.check_invariant("ArmedMatchesExpected", &retyped));
    assert_eq!(retyped[&"recent_typed_retype"], 0);
    assert!(buggy.check_invariant("RecentTypedRetypeIsTyped", &retyped));
    assert!(buggy.check_invariant("TaintSelectsTypedRetype", &retyped));
    assert!(buggy.check_invariant("ExactContextCases", &retyped));

    // A single partial-token damage frame can be the only observable typing
    // evidence before the complete token returns. Taint must override the
    // recent same-seed fast path too; Buggy steals the spent episode inside
    // the nominal weak-continuity window.
    let mut recent_retyped = buggy.init_state();
    assert!(buggy.fire("RecentTypedRetype", &mut recent_retyped));
    assert_eq!(recent_retyped[&"typed_retype"], 1);
    assert_eq!(recent_retyped[&"recent_typed_retype"], 1);
    assert_eq!(recent_retyped[&"recent"], 1);
    assert_eq!(recent_retyped[&"seq_gap"], 2);
    assert_eq!(recent_retyped[&"stale_same_seed"], 0);
    assert_eq!(recent_retyped[&"continuity_tainted"], 1);
    assert_eq!(recent_retyped[&"transferred"], 1);
    assert_eq!(recent_retyped[&"fresh"], 0);
    assert_eq!(recent_retyped[&"armed"], 0);
    assert_eq!(recent_retyped[&"false_transfers"], 1);
    assert!(!buggy.check_invariant("NoFalseTransfers", &recent_retyped));
    assert!(!buggy.check_invariant("FreshMatchesExpected", &recent_retyped));
    assert!(buggy.check_invariant("RecentTypedRetypeIsRecent", &recent_retyped));
    assert!(buggy.check_invariant("RecentTypedRetypeIsTyped", &recent_retyped));
    assert!(buggy.check_invariant("TaintSelectsTypedRetype", &recent_retyped));
}

/// Every explicit, continuity-tainted feline retype is independently armed.
/// Buggy records the first replaced episode as done, so the second completed
/// token is poisoned and born inert.
#[test]
fn derived_sparkle_retype_rearm_proves_and_catches_second_inert_birth() {
    let healthy = sparkle_retype_rearm_model();
    assert_proves_and_catches(&healthy);
    let mut state = healthy.init_state();
    for expected in 1..=2 {
        assert!(healthy.fire("TypeAgain", &mut state));
        assert_eq!(state[&"retypes"], expected);
        assert_eq!(state[&"armed"], expected);
        assert!(healthy.check_invariant("EveryRetypeArmed", &state));
    }

    let mut buggy = sparkle_retype_rearm_model();
    for cst in &mut buggy.consts {
        if cst.0 == "Buggy" {
            cst.1 = 1;
        }
    }
    let mut poisoned = buggy.init_state();
    assert!(buggy.fire("TypeAgain", &mut poisoned));
    assert!(buggy.check_invariant("EveryRetypeArmed", &poisoned));
    assert!(buggy.fire("TypeAgain", &mut poisoned));
    assert_eq!(poisoned[&"retypes"], 2);
    assert_eq!(poisoned[&"armed"], 1);
    assert!(!buggy.check_invariant("EveryRetypeArmed", &poisoned));
}

/// The persist-map alignment transaction is bounded even when a full map
/// temporarily pulls an unmatched old episode, refills the slot with a fresh
/// visible episode, then offers the old one back for grace. The healthy LRU
/// union departs one episode; Buggy reproduces the former raw reinsertion and
/// reaches `Cap + 1`.
#[test]
fn derived_sparkle_persist_capacity_proves_and_catches_grace_overflow() {
    let healthy = sparkle_persist_capacity_model();
    assert_proves_and_catches(&healthy);

    let mut state = healthy.init_state();
    for (action, resident, pulled, admitted, departed, phase) in [
        ("Pull", 2, 1, 0, 0, 1),
        ("Fresh", 3, 1, 1, 0, 2),
        ("Reinsert", 3, 0, 1, 1, 3),
    ] {
        assert!(healthy.fire(action, &mut state));
        assert_eq!(state[&"resident"], resident, "{action}");
        assert_eq!(state[&"pulled"], pulled, "{action}");
        assert_eq!(state[&"admitted"], admitted, "{action}");
        assert_eq!(state[&"departed"], departed, "{action}");
        assert_eq!(state[&"phase"], phase, "{action}");
        assert!(healthy.check_invariant("ResidentBounded", &state));
        assert!(healthy.check_invariant("Conservation", &state));
    }

    let mut buggy = sparkle_persist_capacity_model();
    for cst in &mut buggy.consts {
        if cst.0 == "Buggy" {
            cst.1 = 1;
        }
    }
    let mut overflow = buggy.init_state();
    for action in ["Pull", "Fresh", "Reinsert"] {
        assert!(buggy.fire(action, &mut overflow));
    }
    assert_eq!(overflow[&"resident"], 4);
    assert!(!buggy.check_invariant("ResidentBounded", &overflow));
    assert!(buggy.check_invariant("Conservation", &overflow));
}

/// Generated cat-art unlocks form a bounded set, so adding a new semantic key
/// grows discovery exactly once and every later sighting of that key is
/// idempotent. The Buggy trace treats a duplicate as a fresh append, proving
/// both the uniqueness and finite-roster checks are non-vacuous.
#[test]
fn derived_kitty_collectibles_proves_and_catches_duplicate_growth() {
    assert_proves_and_catches(&kitty_collectibles_model());
}

/// The sidecar and embedded mirror reconcile collectible-aware rollback
/// discoveries/repeats without duplicate inflation, then restore the mirror
/// after a pre-collectibles rewrite. The Buggy base-only branch stays
/// apparently healthy through `Discover`; `OldRewrite` erases its sole key and
/// event count and supplies the required rollback counterexample.
#[test]
fn derived_kitty_sidecar_proves_bidirectional_reconcile_and_catches_rollback() {
    assert_proves_and_catches(&kitty_sidecar_durability_model());
}

/// Contended Kitty Log batches remain conserved while the worker retries
/// without a new delivery; the full ordinary lane and retained exit tail move
/// through distinct ownership states before coalescing; exit either joins after
/// its finite lock budget or detaches a regular-IO stall only at the UI-owned
/// deadline. The Buggy branch drops the host tail instead of moving it into the
/// dedicated exit lane.
#[test]
fn derived_kitty_flush_worker_proves_finite_exit_ownership() {
    let model = kitty_flush_worker_model();
    assert_proves_and_catches(&model);

    let mut state = model.init_state();
    for action in ["QueueNormal", "DrainNormal"] {
        assert!(model.fire(action, &mut state), "{action}");
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, &state),
                "{action} violated {} in {state:?}",
                invariant.name
            );
        }
    }
    assert_eq!(state["pending"], 1);
    assert_eq!(state["exiting"], 0);
    assert!(
        !model.action_enabled("Contend", &state),
        "ordinary-runtime contention must not spend the terminal retry budget"
    );
    for action in [
        "QueueNormal",
        "RetainTailOnFull",
        "BeginExit",
        "OfferTail",
        "DrainNormal",
        "AbsorbTail",
        "Flush",
        "Join",
    ] {
        assert!(model.fire(action, &mut state), "{action}");
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, &state),
                "{action} violated {} in {state:?}",
                invariant.name
            );
        }
    }
    assert_eq!(state["accepted"], 3);
    assert_eq!(state["persisted"], 3);
    assert_eq!(state["joined"], 1);

    let mut exhausted = model.init_state();
    for action in ["QueueNormal", "DrainNormal", "BeginExit"] {
        assert!(model.fire(action, &mut exhausted), "exhaustion {action}");
    }
    for _ in 0..4 {
        assert!(model.fire("Contend", &mut exhausted));
    }
    assert_eq!(exhausted["retries"], 4);
    assert!(
        !model.action_enabled("Flush", &exhausted),
        "RetryCap must not admit an unbudgeted fifth flush attempt"
    );
    assert!(
        !model.action_enabled("StallIo", &exhausted),
        "RetryCap must not admit an unbudgeted fifth potentially-stalled attempt"
    );
    assert!(model.action_enabled("Join", &exhausted));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut dropped = buggy.init_state();
    for action in [
        "QueueNormal",
        "DrainNormal",
        "QueueNormal",
        "RetainTailOnFull",
        "BeginExit",
        "OfferTail",
    ] {
        assert!(buggy.fire(action, &mut dropped), "buggy {action}");
    }
    assert_eq!(dropped["accepted"], 3);
    assert_eq!(dropped["normal_lane"], 1);
    assert_eq!(dropped["pending"], 1);
    assert_eq!(dropped["host_tail"], 0);
    assert_eq!(dropped["exit_lane"], 0);
    assert!(
        !buggy.check_invariant("AcceptedConserved", &dropped),
        "Buggy=1 must reproduce the one-lane exit-tail loss"
    );
}

/// Sparkle-words v2 supernova phases (design §6.1/§9): the monotone
/// Armed→Dip→Flash→Ring→Debris→Ember→Settled walk flashes AT MOST ONCE per
/// arm (`OneFlashPerArm`), with a Rearm that must reset BOTH `phase` and
/// `flashes` — `ty` proves it at Buggy=0 and at Buggy=1 catches the re-arm
/// that re-enters Flash directly (a re-flash without a true re-arm — the
/// strobe class the per-episode `nova_done` guard exists to stop).
#[test]
fn derived_nova_phase_proves_and_catches_reflash() {
    assert_proves_and_catches(&nova_phase_model());
}

/// Sparkle-words v3 one-shot peek (design v3 §1.2/§1.3, replacing the v2.2
/// PeekCycle bob model): the graphic plays exactly once per word appearance —
/// Idle→Rise→Dwell→Descend→Done with Done ABSORBING per episode (`NoRepeek`),
/// phase-bounded, and fuel-terminating (`CanFinish`). `ty` PROVES all three
/// at Buggy=0 and at Buggy=1 CATCHES the re-Rise after Done — the §1.1
/// replay classes (ordinal churn / grace recount / reset) reborn as a second
/// entrance. Tier-1 binding: aterm-effects'
/// `one_shot_peek_conformance_real_engine` drives the real engine across
/// rescans, occlusion, twin growth/shrink/rotation, freeze/thaw mid-rise and
/// an unfocused birth against this model.
#[test]
fn derived_one_shot_peek_proves_and_catches_repeek() {
    assert_proves_and_catches(&one_shot_peek_model());
}

/// Cursor-cat collectibles are an explicit lifecycle contract, not just an art
/// path: the discovery hello cannot be consumed while its window is unfocused
/// or otherwise suppressed. Presentable samples advance the bounded hold and
/// eventually return the animation to fully Hidden; hidden samples only hide
/// the draw. The Buggy trace advances wall time while hidden until the promise
/// expires without being presented, proving this check is non-vacuous.
#[test]
fn derived_cursor_cat_proves_and_catches_hidden_expiry() {
    assert_proves_and_catches(&cursor_cat_model());
}

/// The classic flying kitty changes viewport edges only after a complete body
/// rectangle was sampled wholly off glass. The same bounded machine covers
/// forward (right→left) and reverse (left→right) folds. `Buggy=1` is the old
/// pure-placement teleport: its first transition lands directly on the other
/// on-glass edge, so the history invariant fails in both directions.
#[test]
fn derived_cursor_cat_fold_proves_and_catches_direct_edge_teleport() {
    let healthy = cursor_cat_fold_model();
    assert_proves_and_catches(&healthy);
    assert_every_invariant_carries_a_mutant(&healthy, &["StateBounded"]);

    for (start, origin, destination) in [("StartForward", 1, 0), ("StartReverse", 0, 1)] {
        let mut st = healthy.init_state();
        assert!(healthy.fire(start, &mut st));
        assert_eq!(st[&"origin_side"], origin, "{start}");
        assert_eq!(st[&"side"], origin, "{start}");
        assert!(healthy.fire("LeaveOff", &mut st));
        assert_eq!(st[&"phase"], 2, "{start}");
        assert_eq!(st[&"off_glass"], 1, "{start}");
        assert_eq!(st[&"off_samples"], 1, "{start}");
        assert_eq!(st[&"side"], origin, "{start}");
        assert!(healthy.fire("CrossSide", &mut st));
        assert_eq!(st[&"phase"], 3, "{start}");
        assert_eq!(st[&"off_glass"], 1, "{start}");
        assert_eq!(st[&"side"], destination, "{start}");
        assert!(healthy.check_invariant("OffGlassBeforeSideChange", &st));
        assert!(healthy.fire("EnterGlass", &mut st));
        assert_eq!(st[&"phase"], 4, "{start}");
        assert_eq!(st[&"off_glass"], 0, "{start}");
        assert_eq!(st[&"side"], destination, "{start}");
        assert!(healthy.check_invariant("OffGlassBeforeSideChange", &st));

        let mut buggy = cursor_cat_fold_model();
        for cst in &mut buggy.consts {
            if cst.0 == "Buggy" {
                cst.1 = 1;
            }
        }
        let mut teleported = buggy.init_state();
        assert!(buggy.fire(start, &mut teleported));
        assert!(buggy.fire("LeaveOff", &mut teleported));
        assert_eq!(teleported[&"phase"], 4, "{start}");
        assert_eq!(teleported[&"side"], destination, "{start}");
        assert_eq!(teleported[&"off_glass"], 0, "{start}");
        assert_eq!(teleported[&"off_samples"], 0, "{start}");
        assert!(
            !buggy.check_invariant("OffGlassBeforeSideChange", &teleported),
            "{start}: direct on-glass side change must be caught"
        );
    }
}

/// The cursor-trail master owns only ordinary rainbow kitty momentum. With the master
/// off, typing is a semantic no-op for the ordinary host arm; a collection
/// still enters its promised visible hello. The mutant reproduces the former
/// leak by arming and drawing the ordinary branch while its owner is off.
#[test]
fn derived_cursor_cat_trail_master_blocks_ordinary_but_not_hello() {
    let healthy = cursor_cat_model();

    let mut off = healthy.init_state();
    assert!(healthy.fire("TypeWhileTrailOff", &mut off));
    assert_eq!(off[&"trail_master"], 0);
    assert_eq!(off[&"ordinary_armed"], 0);
    assert_eq!(off[&"ordinary_visible"], 0);
    assert!(healthy.check_invariant("TrailMasterOwnsOrdinary", &off));

    let mut hello = healthy.init_state();
    assert!(healthy.fire("Collect", &mut hello));
    assert_eq!(hello[&"trail_master"], 0);
    assert_eq!(hello[&"phase"], 1);
    assert_eq!(hello[&"visible"], 1);
    assert!(healthy.check_invariant("HelloIndependentOfTrailMaster", &hello));

    let mut retracted = healthy.init_state();
    assert!(healthy.fire("EnableTrail", &mut retracted));
    assert!(healthy.fire("TypeOrdinary", &mut retracted));
    assert_eq!(retracted[&"ordinary_visible"], 1);
    assert!(healthy.fire("DisableTrail", &mut retracted));
    assert_eq!(retracted[&"ordinary_armed"], 0);
    assert_eq!(retracted[&"ordinary_visible"], 0);

    let mut buggy = cursor_cat_model();
    for cst in &mut buggy.consts {
        if cst.0 == "Buggy" {
            cst.1 = 1;
        }
    }
    let mut leaked = buggy.init_state();
    assert!(buggy.fire("TypeWhileTrailOff", &mut leaked));
    assert_eq!(leaked[&"ordinary_armed"], 1);
    assert_eq!(leaked[&"ordinary_visible"], 1);
    assert!(
        !buggy.check_invariant("TrailMasterOwnsOrdinary", &leaked),
        "the master-off ordinary-flight mutant must violate the owner gate"
    );
}

/// Partial text is inert; complete curses produce distinct, bounded wince
/// beats, and a hidden cat is never summoned by the reaction path.
#[test]
fn derived_cursor_cat_curse_wince_rejects_fuc_and_catches_preview_mutant() {
    let model = cursor_cat_curse_wince_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let fuc = buggy.successors("TypeFuc", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("PrefixNeverWinces", &fuc));
    assert!(!buggy.check_invariant("WinceRequiresComplete", &fuc));

    // A complete curse at a hidden cat: the committed seam refuses it; the mutant
    // without the `is_active` refusal accepts it — a spurious redraw request and
    // wince/reaction state leaked into a companion that is not on glass.
    let hidden = model.successors("Hide", &model.init_state())[0].clone();
    let ignored = model.successors("HiddenComplete", &hidden)[0].clone();
    assert_eq!((ignored["reaction"], ignored["winces"]), (0, 0));
    let accepted = buggy.successors("HiddenComplete", &hidden)[0].clone();
    assert!(!buggy.check_invariant("HiddenCueNeverSummons", &accepted));

    let mut healthy = model.init_state();
    for _ in 0..4 {
        assert!(model.fire("Complete", &mut healthy));
    }
    assert_eq!(healthy.get("winces"), Some(&4));
    assert_eq!(healthy.get("chain"), Some(&4));
}

/// SING-ALONG is earned by a deliberate held key, not a short burst: the
/// committed detector arms exactly on press sixteen, releases through a bounded
/// wind-down, and the original eight-press threshold is a required
/// counterexample rather than a dead configuration dial.
#[test]
fn derived_kitty_sing_detector_proves_and_catches_eight_press_arm() {
    let model = kitty_sing_detector_model();
    assert_proves_and_catches(&model);

    // The two `Buggy=1` arms are alternatives, each reachable on its own. The
    // historical one arms on the eighth press at full drive …
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut early = buggy.init_state();
    while buggy.action_enabled("Repeat", &early) {
        assert!(buggy.fire("Repeat", &mut early));
    }
    assert_eq!(
        (early["phase"], early["count"], early["drive_live"]),
        (1, 8, 1),
        "the shipped eight-press arm"
    );
    assert!(!buggy.check_invariant("ArmedRequiresCurrentThreshold", &early));
    assert!(buggy.check_invariant("ArmedRunIsAtFullDrive", &early));

    // … and the ramped-in one arms on the sixteenth press with its drive still
    // at zero, so the armed phase has no celebration to show.
    assert_eq!(
        model.successors("PickRampedArm", &model.init_state()),
        vec![model.init_state()],
        "no arm fault to pick at Buggy=0"
    );
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &[]),
        Ok(0),
        "no action is dead at the committed config (strict vacuity)"
    );
    let mut ramped = buggy.init_state();
    assert!(buggy.fire("PickRampedArm", &mut ramped));
    while buggy.action_enabled("Repeat", &ramped) {
        assert!(buggy.fire("Repeat", &mut ramped));
    }
    assert_eq!(
        (ramped["phase"], ramped["count"], ramped["drive_live"]),
        (1, 16, 0),
        "armed, silent"
    );
    assert!(buggy.check_invariant("ArmedRequiresCurrentThreshold", &ramped));
    assert!(!buggy.check_invariant("ArmedRunIsAtFullDrive", &ramped));
    assert!(buggy.fire("Release", &mut ramped));
    assert_eq!(
        (ramped["phase"], ramped["drive_live"]),
        (2, 1),
        "the release still crossfades"
    );
}

/// The singing momentum bypass cannot make the cursor companion skip its own
/// travel floor. The committed model stays hidden through event fifteen; the
/// v0.56 ten-event floor must violate `NoCatBeforeSixteen` at Buggy=1.
#[test]
fn derived_cursor_cat_earn_floor_proves_and_catches_v056_threshold() {
    assert_proves_and_catches(&cursor_cat_earn_floor_model());
}

/// Reduced motion keeps one opaque companion on glass throughout the singer's
/// handoff to the resident pet. The ordinary sampled cadence and a direct late
/// 1.0 -> 0.49 observation take different actions but converge on the same
/// static-tail custody; direct 1.0 -> 0.30 and 1.0 -> 0.0 samples return the
/// already-ready resident immediately. The mutant restores the historical
/// all-transparent handoff hole on every late route.
#[test]
fn derived_reduced_motion_companion_handoff_proves_and_catches_blackout() {
    let registered: std::collections::BTreeSet<_> = aterm_spec::xref::model_registry()
        .into_iter()
        .map(|candidate| candidate.name)
        .collect();
    for expected in [
        "ReducedMotionCompanionHandoff",
        "CursorCatMotionPulseRouting",
    ] {
        assert!(
            registered.contains(expected),
            "{expected} must participate in the global spec registry"
        );
    }

    let model = reduced_motion_companion_handoff_model();
    assert_proves_and_catches(&model);

    let mut cadenced = model.init_state();
    assert!(model.fire("StartReducedSong", &mut cadenced));
    assert_eq!(cadenced[&"singer_visible"], 0);
    assert_eq!(cadenced[&"pet_visible"], 1);
    assert!(model.fire("SampleAtHalfCutoff", &mut cadenced));
    assert_eq!(cadenced[&"pet_ready"], 1);
    assert_eq!(cadenced[&"singer_visible"], 0);
    assert!(model.fire("SampleCadencedBelowHalf", &mut cadenced));
    assert_eq!(cadenced[&"phase"], 3);
    assert_eq!(cadenced[&"singer_visible"] + cadenced[&"pet_visible"], 1);
    assert!(model.check_invariant("LiveTailKeepsCompanionVisible", &cadenced));
    assert!(model.fire("SampleBelowFaceSwap", &mut cadenced));
    assert_eq!(cadenced[&"singer_visible"], 0);
    assert_eq!(cadenced[&"pet_visible"], 1);
    assert!(model.fire("DrainSongTail", &mut cadenced));
    assert_eq!(cadenced[&"song_tail_live"], 0);
    assert_eq!(cadenced[&"pet_visible"], 1);

    let mut late = model.init_state();
    assert!(model.fire("StartReducedSong", &mut late));
    assert_eq!(late[&"pet_ready"], 1);
    assert!(model.fire("SampleLateBelowHalf", &mut late));
    assert_eq!(late[&"phase"], 3);
    assert_eq!(late[&"pet_ready"], 1);
    assert_eq!(late[&"singer_visible"], 0);
    assert_eq!(late[&"pet_visible"], 1);
    assert!(model.check_invariant("LiveTailKeepsCompanionVisible", &late));
    assert!(model.fire("SampleBelowFaceSwap", &mut late));
    assert_eq!(late[&"pet_visible"], 1);

    let mut late_below_swap = model.init_state();
    assert!(model.fire("StartReducedSong", &mut late_below_swap));
    assert!(model.fire("SampleLateBelowFaceSwap", &mut late_below_swap));
    assert_eq!(late_below_swap[&"phase"], 4);
    assert_eq!(late_below_swap[&"song_tail_live"], 1);
    assert_eq!(late_below_swap[&"singer_visible"], 0);
    assert_eq!(late_below_swap[&"pet_ready"], 1);
    assert_eq!(late_below_swap[&"pet_visible"], 1);
    assert!(model.check_invariant("LiveTailKeepsCompanionVisible", &late_below_swap));

    let mut late_drained = model.init_state();
    assert!(model.fire("StartReducedSong", &mut late_drained));
    assert!(model.fire("SampleLateDrained", &mut late_drained));
    assert_eq!(late_drained[&"phase"], 5);
    assert_eq!(late_drained[&"song_tail_live"], 0);
    assert_eq!(late_drained[&"singer_visible"], 0);
    assert_eq!(late_drained[&"pet_ready"], 1);
    assert_eq!(late_drained[&"pet_visible"], 1);
    assert!(model.check_invariant("GlassCustodyIsExclusive", &late_drained));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut at_half = buggy.init_state();
    assert!(buggy.fire("StartReducedSong", &mut at_half));
    assert!(
        !buggy.check_invariant("ResidentAlwaysOwnsPetMode", &at_half),
        "the former head substitution must fail even before a blackout"
    );
    assert!(buggy.fire("SampleAtHalfCutoff", &mut at_half));
    assert_eq!(at_half[&"pet_ready"], 1);
    assert_eq!(at_half[&"singer_visible"] + at_half[&"pet_visible"], 0);
    assert!(!buggy.check_invariant("LiveTailKeepsCompanionVisible", &at_half));

    let mut late_gap = buggy.init_state();
    assert!(buggy.fire("StartReducedSong", &mut late_gap));
    assert!(buggy.fire("SampleLateBelowHalf", &mut late_gap));
    assert_eq!(late_gap[&"phase"], 3);
    assert_eq!(late_gap[&"singer_visible"] + late_gap[&"pet_visible"], 0);
    assert!(!buggy.check_invariant("LiveTailKeepsCompanionVisible", &late_gap));

    let mut late_below_swap_gap = buggy.init_state();
    assert!(buggy.fire("StartReducedSong", &mut late_below_swap_gap));
    assert!(buggy.fire("SampleLateBelowFaceSwap", &mut late_below_swap_gap));
    assert_eq!(late_below_swap_gap[&"pet_ready"], 1);
    assert_eq!(
        late_below_swap_gap[&"singer_visible"] + late_below_swap_gap[&"pet_visible"],
        0
    );
    assert!(!buggy.check_invariant("LiveTailKeepsCompanionVisible", &late_below_swap_gap));

    let mut late_drained_gap = buggy.init_state();
    assert!(buggy.fire("StartReducedSong", &mut late_drained_gap));
    assert!(buggy.fire("SampleLateDrained", &mut late_drained_gap));
    assert_eq!(late_drained_gap[&"pet_ready"], 1);
    assert_eq!(
        late_drained_gap[&"singer_visible"] + late_drained_gap[&"pet_visible"],
        0
    );
    assert!(!buggy.check_invariant("GlassCustodyIsExclusive", &late_drained_gap));
}

/// Both render routes take and forward the producer's classifier-minted cat
/// pulse in the frame that creates it. After either route consumes it, changing
/// routes cannot expose another delivery. The mutant strands the composed
/// pulse, then demonstrates the stale delivery after switching to ordinary —
/// and, independently, mints a pulse from a move no keypress licensed.
#[test]
fn derived_cursor_cat_motion_pulse_routing_proves_and_catches_composed_strand() {
    let model = cursor_cat_motion_pulse_routing_model();
    assert_proves_and_catches(&model);

    // PROVENANCE: the pulse exists only inside a LICENSED, classified spawn.
    // With no licence the producer never reaches the classifier that mints one.
    let mut unlicensed = model.init_state();
    assert!(
        !model.fire("ClassifyPulse", &mut unlicensed),
        "an unlicensed move never reaches the classifier that mints the pulse"
    );

    let mut ordinary = model.init_state();
    assert!(model.fire("LicenseMove", &mut ordinary));
    assert!(model.fire("ClassifyPulse", &mut ordinary));
    assert_eq!(ordinary[&"classified"], 1);
    assert_eq!(ordinary[&"cold_pulse"], 0);
    assert!(model.fire("SelectOrdinaryRoute", &mut ordinary));
    assert!(model.fire("RenderOrdinaryRoute", &mut ordinary));
    assert_eq!(ordinary[&"attempted_route"], 1);
    assert_eq!(ordinary[&"pending"], 0);
    assert_eq!(ordinary[&"consumes"], 1);
    assert_eq!(ordinary[&"deliveries"], 1);
    assert!(model.fire("SwitchToComposedExtractedRoute", &mut ordinary));
    assert!(
        !model.fire("RenderComposedExtractedRoute", &mut ordinary),
        "the ordinary route already consumed the one-shot pulse"
    );
    assert_eq!(ordinary[&"deliveries"], 1);

    let mut composed = model.init_state();
    assert!(model.fire("LicenseMove", &mut composed));
    assert!(model.fire("ClassifyPulse", &mut composed));
    assert!(model.fire("SelectComposedExtractedRoute", &mut composed));
    assert!(model.fire("RenderComposedExtractedRoute", &mut composed));
    assert_eq!(composed[&"attempted_route"], 2);
    assert_eq!(composed[&"pending"], 0);
    assert_eq!(composed[&"consumes"], 1);
    assert_eq!(composed[&"deliveries"], 1);
    assert!(model.fire("SwitchToOrdinaryRoute", &mut composed));
    assert!(
        !model.fire("RenderOrdinaryRoute", &mut composed),
        "the composed route already consumed the one-shot pulse"
    );
    assert_eq!(composed[&"deliveries"], 1);
    assert!(model.check_invariant("RouteSwitchCannotReplay", &composed));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut cold = buggy.init_state();
    assert!(
        buggy.fire("ClassifyPulse", &mut cold),
        "the mutant mints a pulse with no licence behind it"
    );
    assert_eq!(cold[&"cold_pulse"], 1);
    assert!(!buggy.check_invariant("NoPulseFromAnUnlicensedMove", &cold));

    let mut stranded = buggy.init_state();
    assert!(buggy.fire("LicenseMove", &mut stranded));
    assert!(buggy.fire("ClassifyPulse", &mut stranded));
    assert!(buggy.fire("SelectComposedExtractedRoute", &mut stranded));
    assert!(buggy.fire("RenderComposedExtractedRoute", &mut stranded));
    assert_eq!(stranded[&"pending"], 1);
    assert_eq!(stranded[&"consumes"], 0);
    assert_eq!(stranded[&"deliveries"], 0);
    assert_eq!(stranded[&"stranded"], 1);
    assert!(!buggy.check_invariant("NoComposedRouteStrand", &stranded));
    assert!(!buggy.check_invariant("AttemptConsumesAndDeliversExactlyOnce", &stranded));

    assert!(buggy.fire("SwitchToOrdinaryRoute", &mut stranded));
    assert!(buggy.fire("RenderOrdinaryRoute", &mut stranded));
    assert_eq!(stranded[&"pending"], 0);
    assert_eq!(stranded[&"deliveries"], 1);
    assert_eq!(stranded[&"stale_replay"], 1);
    assert!(!buggy.check_invariant("RouteSwitchCannotReplay", &stranded));
}

/// THE LICENSE (`docs/design/EFFECTS-LICENSE-REDESIGN.md`): a cursor move may
/// mint light only if a human touched the keyboard within the freshness window,
/// and an unlicensed move mints NOTHING and destroys NOTHING.
///
/// The warm Codex take's retired `and ` run must rejoin only on an exact,
/// adjacent typed continuation. Tier-0 checks the whole bounded state space;
/// each named mutant is also caught on its particular witness path.
#[test]
fn derived_rainbow_typed_continuity_rejoins_only_exact_same_row_text() {
    let model = rainbow_typed_continuity_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|candidate| candidate.name == "RainbowTypedContinuity"),
        "RainbowTypedContinuity must participate in the global spec registry"
    );
    assert_proves_and_catches(&model);

    let mut resumed = model.init_state();
    for action in [
        "TypeRun",
        "RetireNaturally",
        "ClearLicence",
        "IdleTick",
        "ResumeTyped",
    ] {
        assert!(model.fire(action, &mut resumed));
    }
    assert_eq!(resumed["revived"], 1);
    assert_eq!(resumed["idle_work"], 0);

    for refusal in [
        "ChangeGlyph",
        "MoveRow",
        "LeaveAdjacentEnd",
        "Navigation",
        "AgePastChain",
    ] {
        let mut state = model.init_state();
        for action in ["TypeRun", "RetireNaturally", refusal, "ResumeTyped"] {
            assert!(model.fire(action, &mut state));
        }
        assert_eq!(state["revived"], 0, "{refusal} revived old text");
    }
    let mut keyless = model.init_state();
    for action in ["TypeRun", "RetireNaturally", "ProgramPaint"] {
        assert!(model.fire(action, &mut keyless));
    }
    assert_eq!(keyless["revived"], 0);

    let program_bug = aterm_spec::interp::with_buggy(&model, 2);
    let mut stolen = program_bug.init_state();
    for action in ["TypeRun", "RetireNaturally", "ProgramPaint"] {
        assert!(program_bug.fire(action, &mut stolen));
    }
    assert!(!program_bug.check_invariant("RejoinNeedsTypedEcho", &stolen));

    let idle_bug = aterm_spec::interp::with_buggy(&model, 3);
    let mut idle = idle_bug.init_state();
    for action in ["TypeRun", "RetireNaturally", "IdleTick"] {
        assert!(idle_bug.fire(action, &mut idle));
    }
    assert!(!idle_bug.check_invariant("DormantCacheDoesNotWake", &idle));
}

#[test]
fn derived_same_caret_typed_echo_requires_exact_oldest_key_and_fresh_probe() {
    let model = same_caret_typed_echo_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|candidate| candidate.name == "SameCaretTypedEcho")
    );
    assert_proves_and_catches(&model);

    let mut exact = model.init_state();
    for action in ["BankA", "BankN", "ExactA", "Resolve"] {
        assert!(model.fire(action, &mut exact));
    }
    assert_eq!(
        exact["admitted"], 1,
        "later typeahead `n` replaced oldest `a`"
    );
    assert_eq!(exact["spent"], 1);
    let mut delayed = model.init_state();
    for action in ["BankA", "DelayedExactA", "Resolve"] {
        assert!(model.fire(action, &mut delayed));
    }
    assert_eq!(delayed["admitted"], 1, "a live delayed exact echo was dark");
    assert_eq!(delayed["spent"], 1);
    let mut expired = model.init_state();
    for action in ["BankA", "ExpiredExactA", "Resolve"] {
        assert!(model.fire(action, &mut expired));
    }
    assert_eq!(expired["admitted"], 0, "an expired key claimed a print");
    for refusal in [
        "AmbientOtherGlyph",
        "DelayedAmbientOtherGlyph",
        "EarlierMatchingPaint",
        "NoRowProbe",
        "NoHandOwner",
    ] {
        let mut state = model.init_state();
        for action in ["BankA", refusal, "Resolve"] {
            assert!(model.fire(action, &mut state));
        }
        assert_eq!(state["admitted"], 0, "{refusal} claimed a key");
    }
    let mut batch = model.init_state();
    for action in ["BankA", "BankN", "ExactBatch", "Resolve"] {
        assert!(model.fire(action, &mut batch));
    }
    assert_eq!(
        batch["admitted"], 1,
        "the exact in-flight batch was refused"
    );
    for refusal in ["BatchOtherGlyph", "BatchNoPriorProbe", "BatchStaleTail"] {
        let mut state = model.init_state();
        for action in ["BankA", "BankN", refusal, "Resolve"] {
            assert!(model.fire(action, &mut state));
        }
        assert_eq!(state["admitted"], 0, "{refusal} claimed a key");
    }
    let mut keyless = model.init_state();
    for action in ["KeylessPaint", "Resolve"] {
        assert!(model.fire(action, &mut keyless));
    }
    assert_eq!(keyless["admitted"], 0);
    let mut batch_keyless = model.init_state();
    for action in ["BatchKeyless", "Resolve"] {
        assert!(model.fire(action, &mut batch_keyless));
    }
    assert_eq!(batch_keyless["admitted"], 0);
}

#[test]
fn derived_unknown_insert_orphan_keys_are_exact_ordered_and_invalidated() {
    let model = unknown_insert_orphan_key_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|candidate| candidate.name == "UnknownInsertOrphanKey")
    );
    assert_proves_and_catches(&model);

    let mut exact = model.init_state();
    for action in [
        "QueueOne",
        "LayWithSite",
        "Cleanup",
        "ExactNewPrint",
        "LaterProgramPrint",
    ] {
        assert!(model.fire(action, &mut exact));
    }
    assert_eq!(exact["lit"], 1, "the exact later key stayed dark");
    assert_eq!(exact["generic"], 0, "escrow leaked into generic credits");
    assert_eq!(exact["escrow"], 0, "the key spent more than once");

    let mut two = model.init_state();
    for action in ["QueueTwo", "LayWithSite", "Cleanup", "ExactNewPrint"] {
        assert!(model.fire(action, &mut two));
    }
    assert_eq!(two["lit"], 1, "the first exact key stayed dark");
    assert_eq!(two["escrow"], 1, "the second key became generic credit");
    assert!(model.fire("SecondExactNewPrint", &mut two));
    assert_eq!(two["lit"], 2, "the second exact key stayed dark");
    assert_eq!(two["escrow"], 0, "the two-key escrow was spent twice");

    let mut combined = model.init_state();
    for action in [
        "QueueTwo",
        "LayWithSite",
        "Cleanup",
        "CoalescedTwoExactNewPrint",
    ] {
        assert!(model.fire(action, &mut combined));
    }
    assert_eq!(combined["lit"], 2, "the exact two-cell frame stayed dark");
    assert_eq!(combined["escrow"], 0, "the coalesced keys were reusable");
    assert_eq!(combined["generic"], 0, "the batch leaked generic credits");

    for refusal in [
        "CoalescedTwoWrongOrPartial",
        "CoalescedTwoOldPrint",
        "CoalescedTwoMissingProbe",
        "CoalescedTwoAltWithoutBlink",
    ] {
        let mut state = model.init_state();
        for action in ["QueueTwo", "LayWithSite", "Cleanup", refusal] {
            assert!(model.fire(action, &mut state));
        }
        assert_eq!(state["lit"], 0, "{refusal} claimed the batch");
        assert_eq!(state["escrow"], 0, "{refusal} retained ambiguity");
    }
    let mut no_site = model.init_state();
    for action in [
        "QueueTwo",
        "LayWithoutSite",
        "Cleanup",
        "CoalescedTwoExactNewPrint",
    ] {
        assert!(model.fire(action, &mut no_site));
    }
    assert_eq!(no_site["lit"], 0, "a missing insert site claimed two keys");
    let mut too_many = model.init_state();
    for action in ["QueueThree", "LayWithSite", "Cleanup", "CoalescedTooMany"] {
        assert!(model.fire(action, &mut too_many));
    }
    assert_eq!(too_many["lit"], 0, "three queued keys claimed two cells");
    assert_eq!(too_many["generic"], 0, "third key leaked generic credit");

    for refusal in [
        "SecondAmbientOtherGlyph",
        "SecondExactOldPrint",
        "SecondPrefixRewrite",
    ] {
        let mut state = model.init_state();
        for action in [
            "QueueTwo",
            "LayWithSite",
            "Cleanup",
            "ExactNewPrint",
            refusal,
        ] {
            assert!(model.fire(action, &mut state));
        }
        assert_eq!(state["lit"], 1, "{refusal} claimed the second key");
        assert_eq!(state["escrow"], 0, "{refusal} kept ambiguous credit");
    }

    for refusal in [
        "AmbientOtherGlyph",
        "ExactOldPrint",
        "Expire",
        "Scroll",
        "Rewrite",
        "Reset",
    ] {
        let mut state = model.init_state();
        for action in ["QueueOne", "LayWithSite", "Cleanup", refusal] {
            assert!(model.fire(action, &mut state));
        }
        assert_eq!(state["lit"], 0, "{refusal} claimed the key");
        assert_eq!(state["escrow"], 0, "{refusal} retained the escrow");
    }
    for queue in ["QueueNone", "QueueBlank", "QueueThree"] {
        let mut state = model.init_state();
        for action in [queue, "LayWithSite", "Cleanup", "ExactNewPrint"] {
            assert!(model.fire(action, &mut state));
        }
        assert_eq!(state["lit"], 0, "{queue} claimed a single exact key");
    }
}

#[test]
fn derived_cursor_hint_license_proves_and_catches_cold_light() {
    let registered: std::collections::BTreeSet<_> = aterm_spec::xref::model_registry()
        .into_iter()
        .map(|candidate| candidate.name)
        .collect();
    assert!(
        registered.contains("CursorHintLicense"),
        "CursorHintLicense must participate in the global spec registry"
    );

    let model = cursor_hint_license_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &["StateBounded"]);

    // Delivery and all later keys can reach the renderer before either
    // output frame. The first echo spends the insert, leaving the later
    // press's credit and stamp for its own frame.
    let mut delayed = model.init_state();
    for action in [
        "PasteEnqueues",
        "PressBehindQueuedInsert",
        "WriteCompletesArmsInsertLicence",
        "DelayedInsertEchoPrecedesLaterKey",
    ] {
        assert!(model.fire(action, &mut delayed), "{action}");
    }
    assert_eq!(delayed["insert_hint"], 0);
    assert_eq!(delayed["hint"], 1);
    assert_eq!(delayed["later_key_credit"], 1);
    assert_eq!(delayed["spent"], 0);
    assert!(model.fire("LicensedTypedMoveMintsLight", &mut delayed));
    assert_eq!(delayed["spent"], 1);
    assert_eq!(delayed["births"], 2);

    // A PRESS ARMS A LICENCE, and the licensed echo spends it: one hint, one
    // echo. The ring scores the move as `licensed`, and light exists.
    let mut typed = model.init_state();
    assert!(model.fire("PressArmsLicense", &mut typed));
    assert_eq!(typed["hint"], 1);
    assert_eq!(typed["arms"], 1);
    assert_eq!(typed["credit_arms"], 1);
    let before_hover = typed.clone();
    assert!(model.fire("ByteSilentPointerPreservesLicense", &mut typed));
    assert_eq!(
        typed, before_hover,
        "byte-silent hover disposes of no press"
    );
    assert!(model.fire("LicensedTypedMoveMintsLight", &mut typed));
    assert_eq!(typed["hint"], 0, "the paired echo consumes the stamp");
    assert_eq!(typed["consumed"], 1);
    assert_eq!(typed["births"], 1);
    assert_eq!(typed["licensed_tally"], 1);
    assert_eq!(typed["resident"], 1);
    assert_eq!(
        typed["spent"], 1,
        "the echo SPENDS the press's credit (2026-09-10: a credit is spent by the cells it lays)"
    );
    assert!(
        !model.fire("LicensedTypedMoveMintsLight", &mut typed),
        "a spent licence cannot fund a second echo"
    );

    // THE COLD MOVE over that earned light: it mints nothing AND it destroys
    // nothing. Retention is decay + note_scroll translation + reset, and the
    // ring names the refusal so `ctl trail` can tell the truth about it.
    let mut cold = typed.clone();
    assert!(model.fire("ColdMoveOverEarnedLight", &mut cold));
    assert_eq!(cold["births"], 1, "an unlicensed move mints nothing");
    assert_eq!(cold["resident"], 1, "…and destroys nothing it did not earn");
    assert_eq!(cold["wiped"], 0);
    assert_eq!(cold["declined_tally"], 1);
    assert_eq!(cold["licensed_tally"], 1);

    // FRESHNESS, not merely presence: an expired stamp is still SET and is not
    // a licence. Retiring it is the classifier's `take_if`, not a refund.
    let mut expiry = model.init_state();
    assert!(model.fire("PressArmsLicense", &mut expiry));
    assert!(model.fire("LicenseExpires", &mut expiry));
    assert_eq!(expiry["hint"], 2, "the stamp survives its own freshness");
    assert_eq!(expiry["expired"], 1);
    assert!(
        !model.fire("LicensedTypedMoveMintsLight", &mut expiry),
        "a stale stamp licenses nothing"
    );
    // …but the PRESS behind it is still in flight, and THAT licenses its own
    // one-cell echo (2026-09-12, the stalled last key): spent on admission,
    // so it funds one cell, once. The model carries no hop width, so at ONE
    // press the refusal is enabled beside it — the engine's real transition
    // for a hop wider than one cell: refused, and the press KEPT.
    let mut wide = expiry.clone();
    assert!(
        model.fire("StaleStampMoveDeclines", &mut wide),
        "one press against a wider hop is refused"
    );
    assert_eq!(wide["births"], 0);
    assert_eq!(
        wide["credit_arms"] - wide["spent"] - wide["forfeited"],
        1,
        "…and the press is kept, not forgotten"
    );
    let mut late = expiry.clone();
    assert!(model.fire("InFlightPressEchoMintsLight", &mut late));
    assert_eq!(late["births"], 1, "the press's own cell is lit");
    assert_eq!(late["spent"], 1, "…and the press is spent");
    assert_eq!(late["hint"], 2, "the stale stamp is left in place");
    assert!(
        !model.fire("InFlightPressEchoMintsLight", &mut late),
        "one press, one cell, once"
    );
    // The stale stamp with NO press in flight — forgotten by an edge — is
    // the shape `StaleStampMoveDeclines` is right about.
    assert!(model.fire("UnexplainedHopForgetsCredits", &mut expiry));
    assert_eq!(expiry["forfeited"], 1);

    // THE DELIVERED INSERT (2026-09-10): enqueue arms nothing; the writer
    // thread's completed write arms the insert's own slot; its echo spends
    // it exactly once and the ring scores the sweep.
    let mut insert = model.init_state();
    assert!(model.fire("PasteEnqueues", &mut insert));
    assert_eq!(insert["write_pending"], 1);
    assert_eq!(insert["insert_hint"], 0, "enqueue is not delivery");
    assert_eq!(insert["arms"], 0);
    assert!(
        !model.fire("LicensedInsertEchoMintsLight", &mut insert),
        "an echo before the bytes landed is program output"
    );
    assert!(model.fire("WriteCompletesArmsInsertLicence", &mut insert));
    assert_eq!(insert["write_pending"], 0);
    assert_eq!(
        insert["insert_hint"], 1,
        "the completed write is the licence"
    );
    assert_eq!(insert["arms"], 1);
    assert_eq!(
        insert["credit_arms"], 0,
        "the insert's width is its own, not a press credit"
    );
    assert!(model.fire("LicensedInsertEchoMintsLight", &mut insert));
    assert_eq!(insert["insert_hint"], 0, "one delivery, one echo");
    assert_eq!(insert["consumed"], 1);
    assert_eq!(insert["births"], 1);
    assert_eq!(insert["licensed_tally"], 1);
    assert!(
        !model.fire("LicensedInsertEchoMintsLight", &mut insert),
        "a spent insert licence cannot fund a second hop"
    );
    let mut stale_insert = model.init_state();
    assert!(model.fire("PasteEnqueues", &mut stale_insert));
    assert!(model.fire("WriteCompletesArmsInsertLicence", &mut stale_insert));
    assert!(model.fire("InsertLicenceExpires", &mut stale_insert));
    assert!(
        !model.fire("LicensedInsertEchoMintsLight", &mut stale_insert),
        "a stale insert stamp licenses nothing"
    );
    assert!(model.fire("StaleInsertStampDeclines", &mut stale_insert));
    assert_eq!(stale_insert["declined_tally"], 1);
    // The mutant arms at enqueue, and the echo it then admits is the witness
    // `AnInsertLicenceIsMintedOnlyByACompletedWrite` refuses.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut undelivered = buggy.init_state();
    assert!(buggy.fire("PasteEnqueues", &mut undelivered));
    assert_eq!(undelivered["insert_hint"], 1, "the mutant arms at enqueue");
    assert!(buggy.fire("LicensedInsertEchoMintsLight", &mut undelivered));
    assert!(
        !buggy.check_invariant("AnInsertLicenceIsMintedOnlyByACompletedWrite", &undelivered),
        "an echo lit on an undelivered arm must be caught"
    );
    assert!(model.fire("StaleStampMoveDeclines", &mut expiry));
    assert_eq!(expiry["births"], 0);
    assert!(model.fire("RetireStaleLicense", &mut expiry));
    assert_eq!(expiry["spent"], 0);
    assert_eq!(expiry["credit_refunded"], 0);

    // THE ONE-PRESS MUTANT: the late echo that does not spend — one press
    // funds a second +1, and `PairedAdmissionsNeverExceedArms` names it.
    let buggy_press = aterm_spec::interp::with_buggy(&model, 1);
    let mut twice = buggy_press.init_state();
    assert!(buggy_press.fire("PressArmsLicense", &mut twice));
    assert!(buggy_press.fire("LicenseExpires", &mut twice));
    assert!(buggy_press.fire("InFlightPressEchoMintsLight", &mut twice));
    assert_eq!(twice["spent"], 0, "the mutant does not spend");
    assert!(buggy_press.fire("InFlightPressEchoMintsLight", &mut twice));
    assert!(
        !buggy_press.check_invariant("PairedAdmissionsNeverExceedArms", &twice),
        "two admissions on one arm must be caught"
    );

    // THE PRESS CREDIT BUDGET, the one anti-stray law kept from the proof era.
    // Two banked cells pay for a two-cell coalesce and are spent by it; one
    // banked cell buys nothing at all (vim's `w` — one press echoing as a
    // multi-cell hop) and the move stays dark.
    let mut paid = model.init_state();
    assert!(model.fire("PressArmsLicense", &mut paid));
    assert!(model.fire("PressArmsLicense", &mut paid));
    assert_eq!(paid["credit_arms"], 2);
    assert_eq!(paid["superseded"], 1, "the one-slot stamp supersedes");
    assert!(model.fire("AdmitCoalesceSpendsCredits", &mut paid));
    assert_eq!(paid["spent"], 2, "the admitted ribbon paid for its cells");
    assert_eq!(paid["coalesce_births"], 1);

    let mut starved = model.init_state();
    assert!(model.fire("PressArmsLicense", &mut starved));
    assert!(
        !model.fire("AdmitCoalesceSpendsCredits", &mut starved),
        "one credit must never paint a ribbon over a word the user skimmed"
    );
    assert!(model.fire("StarvedCoalesceDeclines", &mut starved));
    assert_eq!(starved["births"], 0);
    assert_eq!(starved["spent"], 0, "a refused coalesce is billed nothing");
    assert_eq!(starved["declined_tally"], 1);
    assert_eq!(
        starved["forfeited"], 1,
        "a `no-credits` refusal is an unexplained hop: the pool is forgotten with it"
    );

    // THE STALL (2026-09-12, the owner's "I t" screenshot): two presses, the
    // stamp window elapses while the row stays silent, and the row echoes
    // both as one batch — no stamp is fresh, and the LEDGER licenses it:
    // the presses are in flight, and the batch spends them.
    let pool = |st: &aterm_spec::interp::State| st["credit_arms"] - st["spent"] - st["forfeited"];
    let mut stall = model.init_state();
    assert!(model.fire("PressArmsLicense", &mut stall));
    assert!(model.fire("PressArmsLicense", &mut stall));
    assert!(model.fire("LicenseExpires", &mut stall));
    assert_eq!(stall["hint"], 2, "the stamps went stale in place");
    assert_eq!(pool(&stall), 2, "…and both presses are still in flight");
    assert!(
        !model.fire("LicensedTypedMoveMintsLight", &mut stall),
        "no stamp is fresh"
    );
    assert!(
        !model.fire("StaleStampMoveDeclines", &mut stall),
        "two presses in flight are a batch, not a stale stamp"
    );
    assert!(model.fire("InFlightBatchEchoMintsLight", &mut stall));
    assert_eq!(stall["licensed_tally"], 1, "the batch is licensed");
    assert_eq!(stall["births"], 1, "…and lit");
    assert_eq!(stall["coalesce_births"], 1);
    assert_eq!(
        pool(&stall),
        0,
        "the batch spent every press it was licensed by"
    );
    assert_eq!(stall["phantom_admitted"], 0);
    assert!(
        !model.fire("InFlightBatchEchoMintsLight", &mut stall),
        "spent presses license nothing more"
    );

    // THE FORGET: the same two presses, then a keyless hop the echo shape
    // refuses (backward, cross-row, a refused share). The presses did not
    // describe it and are forgotten with it; the batch shape then finds an
    // empty pool.
    let mut forgot = model.init_state();
    assert!(model.fire("PressArmsLicense", &mut forgot));
    assert!(model.fire("PressArmsLicense", &mut forgot));
    assert!(model.fire("LicenseExpires", &mut forgot));
    assert!(model.fire("UnexplainedHopForgetsCredits", &mut forgot));
    assert_eq!(forgot["declined_tally"], 1);
    assert_eq!(forgot["forfeited"], 2, "both presses forgotten");
    assert_eq!(pool(&forgot), 0);
    assert_eq!(forgot["just_forgot"], 1);
    assert!(
        !model.fire("InFlightBatchEchoMintsLight", &mut forgot),
        "a forgotten press licenses nothing"
    );
    assert!(model.check_invariant("ForgottenCreditsNeverReturn", &forgot));
    // A new press reopens the pool — the forget was the edge's, not a ban.
    let mut reopened = model.init_state();
    assert!(model.fire("PressArmsLicense", &mut reopened));
    assert!(model.fire("LicenseExpires", &mut reopened));
    assert!(model.fire("UnexplainedHopForgetsCredits", &mut reopened));
    assert_eq!((reopened["just_forgot"], pool(&reopened)), (1, 0));
    assert!(model.fire("PressArmsLicense", &mut reopened));
    assert_eq!((reopened["just_forgot"], pool(&reopened)), (0, 1));

    // THE BOUND: the patience elapses with the row still silent — a forget
    // by the clock, and the batch shape finds nothing.
    let mut elapsed = model.init_state();
    assert!(model.fire("PressArmsLicense", &mut elapsed));
    assert!(model.fire("PressArmsLicense", &mut elapsed));
    assert!(model.fire("LicenseExpires", &mut elapsed));
    assert!(model.fire("PatienceElapses", &mut elapsed));
    assert_eq!(pool(&elapsed), 0);
    assert!(!model.fire("InFlightBatchEchoMintsLight", &mut elapsed));
    assert!(
        !model.fire("PatienceElapses", &mut elapsed),
        "nothing left to forget"
    );

    // THE MUTANT — a patience that does not forget: the pool survives the
    // unexplained hop, the batch shape licenses it, and both in-flight
    // invariants name the defect.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut phantom = buggy.init_state();
    assert!(buggy.fire("PressArmsLicense", &mut phantom));
    assert!(buggy.fire("PressArmsLicense", &mut phantom));
    assert!(buggy.fire("LicenseExpires", &mut phantom));
    assert!(buggy.fire("UnexplainedHopForgetsCredits", &mut phantom));
    assert_eq!(
        pool(&phantom),
        2,
        "the mutant keeps the pool across the hop"
    );
    assert!(
        !buggy.check_invariant("ForgottenCreditsNeverReturn", &phantom),
        "the kept pool is the defect, and the law names it"
    );
    assert!(buggy.fire("InFlightBatchEchoMintsLight", &mut phantom));
    assert_eq!(phantom["phantom_admitted"], 1);
    assert!(
        !buggy.check_invariant("AForgottenPressNeverLicences", &phantom),
        "a batch licensed out of a forgotten pool must be caught"
    );

    // A KEY THIS WINDOW SWALLOWED is not an answer to the licence question.
    let mut swallowed = model.init_state();
    assert!(model.fire("PressArmsLicense", &mut swallowed));
    assert!(model.fire("SwallowedKeyClearsLicense", &mut swallowed));
    assert_eq!(swallowed["hint"], 0);
    assert_eq!(swallowed["cleared"], 1);
    assert!(
        !model.fire("LicensedTypedMoveMintsLight", &mut swallowed),
        "a swallowed key licenses nothing"
    );

    // reflow/blink are morphology, never licence terms.
    let mut morphology = model.init_state();
    assert!(model.fire("MorphologyStampArrives", &mut morphology));
    assert!(
        !model.fire("LicensedTypedMoveMintsLight", &mut morphology),
        "a resize settle is not a keypress"
    );
    assert!(model.fire("MorphologyOnlyMoveDeclines", &mut morphology));
    assert_eq!(morphology["births"], 0);
    assert_eq!(morphology["morphology_admitted"], 0);

    // THE MUTANTS, each named and each with its own concrete counterexample.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    // v0.43.0's own seam: the tick spawned on EVERY presented cursor delta, so
    // a cold token streamer walking the caret earned heat. Row 9 of the paint
    // matrix is this state.
    let mut cold_light = buggy.init_state();
    assert!(buggy.fire("ColdMoveDeclines", &mut cold_light));
    assert_eq!(cold_light["cold_admitted"], 1);
    assert_eq!(cold_light["births"], 1);
    assert!(
        !buggy.check_invariant("NoLightWithoutAFreshLicence", &cold_light),
        "the pre-licence cold fall-through must have its own counterexample"
    );

    // The freshness window ignored: a stamp that is set but stale still paints
    // — with its press no longer in flight (the patience elapsed), so the
    // stamp is all there is.
    let mut stale_light = buggy.init_state();
    assert!(buggy.fire("PressArmsLicense", &mut stale_light));
    assert!(buggy.fire("LicenseExpires", &mut stale_light));
    assert!(buggy.fire("PatienceElapses", &mut stale_light));
    assert!(buggy.fire("StaleStampMoveDeclines", &mut stale_light));
    assert_eq!(stale_light["stale_admitted"], 1);
    assert!(!buggy.check_invariant("AStaleStampIsNotALicence", &stale_light));

    // `reflow_hint` added to the disjunction — the plausible "helpful" edit.
    let mut reflow_light = buggy.init_state();
    assert!(buggy.fire("MorphologyStampArrives", &mut reflow_light));
    assert!(buggy.fire("MorphologyOnlyMoveDeclines", &mut reflow_light));
    assert_eq!(reflow_light["morphology_admitted"], 1);
    assert!(!buggy.check_invariant("MorphologyStampsNeverLicence", &reflow_light));

    // A stamp that outlives the boundary that swallowed its key.
    let mut swallow_light = buggy.init_state();
    assert!(buggy.fire("PressArmsLicense", &mut swallow_light));
    assert!(buggy.fire("SwallowedKeyClearsLicense", &mut swallow_light));
    assert_eq!(
        swallow_light["hint"], 1,
        "the mutant keeps the swallowed stamp"
    );
    assert!(buggy.fire("LicensedTypedMoveMintsLight", &mut swallow_light));
    assert_eq!(swallow_light["swallow_admitted"], 1);
    assert!(!buggy.check_invariant("ASwallowedKeyNeverLicences", &swallow_light));

    // ONE HINT, MANY ECHOES: the stamp survives the echo that consumed it, so
    // one press funds every move that follows. Caught from both sides.
    let mut reused = buggy.init_state();
    assert!(buggy.fire("PressArmsLicense", &mut reused));
    assert!(buggy.fire("LicensedTypedMoveMintsLight", &mut reused));
    assert_eq!(reused["hint"], 1);
    assert!(!buggy.check_invariant("EveryArmReachesExactlyOneDisposition", &reused));
    assert!(buggy.fire("LicensedTypedMoveMintsLight", &mut reused));
    assert_eq!(reused["admissions"], 2);
    assert_eq!(reused["arms"], 1);
    assert!(!buggy.check_invariant("PairedAdmissionsNeverExceedArms", &reused));

    // THE WIPE — `clear_denied_move_visuals`, the darkness the owner reported:
    // a spinner two rows away moves its caret and the ribbon the user's own
    // typing earned goes with it, invisibly to the diagnosis ring.
    let mut wipe = buggy.init_state();
    assert!(buggy.fire("PressArmsLicense", &mut wipe));
    assert!(buggy.fire("LicensedTypedMoveMintsLight", &mut wipe));
    assert_eq!(wipe["resident"], 1);
    // The mutant kept the stamp live; age it out so the next move is genuinely
    // unlicensed, which is the only shape whose denial ran the wipe.
    assert!(buggy.fire("LicenseExpires", &mut wipe));
    assert!(buggy.fire("RetireStaleLicense", &mut wipe));
    // The mutant refunded the credit too; forget it by the clock so the
    // move finds an empty pool — the cold shape.
    assert!(buggy.fire("PatienceElapses", &mut wipe));
    assert!(buggy.fire("ColdMoveOverEarnedLight", &mut wipe));
    assert_eq!(wipe["resident"], 0);
    assert_eq!(wipe["wiped"], 1);
    assert!(
        !buggy.check_invariant("DeclinedMovesNeverDestroyEarnedLight", &wipe),
        "the light-destroying denial path must have its own counterexample"
    );
    assert!(
        !buggy.check_invariant("TheRingScoresEverySpawnOnce", &wipe),
        "…and a wipe the ring never scored is a spawn `ctl trail` cannot explain"
    );

    // The coalesce that painted a ribbon it did not pay for.
    let mut unpaid = buggy.init_state();
    assert!(buggy.fire("PressArmsLicense", &mut unpaid));
    assert!(buggy.fire("PressArmsLicense", &mut unpaid));
    assert!(buggy.fire("AdmitCoalesceSpendsCredits", &mut unpaid));
    assert_eq!(unpaid["coalesce_births"], 1);
    assert_eq!(unpaid["spent"], 0);
    assert!(!buggy.check_invariant("ACoalesceRibbonIsPaidFor", &unpaid));

    // The starved coalesce that billed the budget it could not afford, and told
    // `ctl trail` it had painted.
    let mut overbilled = buggy.init_state();
    assert!(buggy.fire("PressArmsLicense", &mut overbilled));
    assert!(buggy.fire("StarvedCoalesceDeclines", &mut overbilled));
    assert_eq!(overbilled["spent"], 2);
    assert_eq!(overbilled["credit_arms"], 1);
    assert!(!buggy.check_invariant("SpentCreditsNeverExceedArmed", &overbilled));
    assert_eq!(overbilled["licensed_tally"], 1);
    assert_eq!(overbilled["births"], 0);
    assert!(
        !buggy.check_invariant("TheRingCannotClaimUnmintedLight", &overbilled),
        "a `licensed` row over a dark screen is the ring lying to its reader"
    );

    // Retiring a stale hint refunded its cells, and the refund funded a second
    // stray sweep inside the same window.
    let mut refund = buggy.init_state();
    assert!(buggy.fire("PressArmsLicense", &mut refund));
    assert!(buggy.fire("StarvedCoalesceDeclines", &mut refund));
    assert_eq!(refund["spent"], 2);
    assert!(buggy.fire("PressArmsLicense", &mut refund));
    assert!(buggy.fire("LicenseExpires", &mut refund));
    assert!(buggy.fire("RetireStaleLicense", &mut refund));
    assert_eq!(refund["spent"], 1, "a retired hint gave a spent cell back");
    assert_eq!(refund["credit_refunded"], 1);
    assert!(!buggy.check_invariant("SpentCreditsNeverComeBack", &refund));
}

/// THE ECHO LEDGER (`docs/design/RAINBOW-KITTY-V2.md`, "The late echo's
/// one-press case"): a licensed typed move may lay the hole the seam refused
/// only out of presses OLDER than its licensing key, exactly, one press one
/// cell; a hop those presses do not explain lays nothing and forgets them; a
/// press past the patience buys nothing; no cell is bridged outside a
/// licensed move, and none on a refused one. Tier-0 proves the seven laws
/// over the whole bounded space and requires the `Buggy=1` family to falsify
/// every one when isolated;
/// `StateBounded` is the space, not a claim. Tier-1 drives the real `Engine`
/// in `aterm-effects` (`the_real_engine_conforms_to_the_echo_ledger_model`).
#[test]
fn derived_echo_ledger_bridge_proves_and_catches_the_unexplained_hole() {
    let registered: std::collections::BTreeSet<_> = aterm_spec::xref::model_registry()
        .into_iter()
        .map(|candidate| candidate.name)
        .collect();
    assert!(
        registered.contains("EchoLedgerBridge"),
        "EchoLedgerBridge must participate in the global spec registry"
    );

    let model = echo_ledger_bridge_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &["StateBounded"]);

    // THE SCREENSHOT: a key whose echo the seam refused (the caret advanced,
    // nothing arrived), then the next key, echoed under the host's sweep on
    // ITS clock. The hole is one cell, the older press is one, the bridge lays
    // it and spends both presses.
    let mut relit = model.init_state();
    assert!(model.fire("KeyPressed", &mut relit));
    assert!(model.fire("UnlicensedAdvance", &mut relit));
    assert!(model.fire("KeyPressed", &mut relit));
    assert_eq!(
        relit["older"], 1,
        "the refused key is older than the licensing key"
    );
    assert_eq!(relit["younger"], 1, "the licensing key itself");
    assert_eq!(relit["hole"], 1);
    assert!(
        !model.fire("SweptMoveRefuses", &mut relit),
        "an exactly explained hole is not a refusal"
    );
    assert!(model.fire("SweptMovePays", &mut relit));
    assert_eq!(relit["bridged"], 1, "the hole is laid");
    assert_eq!(relit["spent"], 2, "…and both presses are spent");
    assert_eq!(relit["older"] + relit["younger"], 0);
    assert_eq!(relit["hole"], 0);
    assert_eq!(relit["laid_hole"], 1);
    assert_eq!(relit["last_older"], 1);

    // THE CONTROL: the same hop with no press behind it. Program output moved
    // the caret; the ledger lays nothing and forgets the key it was holding.
    let mut nudged = model.init_state();
    assert!(model.fire("UnlicensedAdvance", &mut nudged));
    assert!(model.fire("KeyPressed", &mut nudged));
    assert!(
        !model.fire("SweptMovePays", &mut nudged),
        "a hole no older press explains is not paid"
    );
    assert!(model.fire("SweptMoveRefuses", &mut nudged));
    assert_eq!(nudged["bridged"], 0);
    assert_eq!(nudged["forfeited"], 1, "the key's press is forfeited");
    assert_eq!(nudged["just_refused"], 1);
    assert!(model.check_invariant("ForfeitedCreditsNeverReturn", &nudged));

    // THE ADVERSARY'S BREAK: a nudge, then a fast burst of two keys. The first
    // key's echo lands one past the mirror with the second press in flight
    // behind it; the in-flight press's glyph lies to the RIGHT, so it cannot
    // pay for the nudge's cell — and the refusal forgets both.
    let mut burst = model.init_state();
    assert!(model.fire("UnlicensedAdvance", &mut burst));
    assert!(model.fire("KeyPressed", &mut burst));
    assert!(model.fire("PressInFlight", &mut burst));
    assert_eq!((burst["older"], burst["younger"], burst["hole"]), (0, 2, 1));
    assert!(!model.fire("SweptMovePays", &mut burst));
    assert!(model.fire("SweptMoveRefuses", &mut burst));
    assert_eq!(burst["bridged"], 0, "three presses never light four cells");
    assert_eq!(burst["forfeited"], 2);

    // THE PHANTOM: a swallowed press, then an ordinary echo with no hole. The
    // older press explains nothing, so the ordinary echo forgets it rather
    // than carrying it forward to pay for the first program nudge.
    let mut phantom = model.init_state();
    assert!(model.fire("KeyPressed", &mut phantom));
    assert!(model.fire("KeyPressed", &mut phantom));
    assert_eq!(
        (phantom["older"], phantom["younger"], phantom["hole"]),
        (1, 1, 0)
    );
    assert!(!model.fire("SweptMovePays", &mut phantom));
    assert!(model.fire("SweptMoveRefuses", &mut phantom));
    assert_eq!(phantom["older"] + phantom["younger"], 0);
    assert!(model.fire("UnlicensedAdvance", &mut phantom));
    assert!(model.fire("KeyPressed", &mut phantom));
    assert!(!model.fire("SweptMovePays", &mut phantom));
    assert!(model.fire("SweptMoveRefuses", &mut phantom));
    assert_eq!(
        phantom["bridged"], 0,
        "the phantom credit never rolls forward"
    );

    // THE UNSWEPT ECHO: two presses, a two-cell licensed move the host did not
    // sweep, starting at the mirror. Both cells are the presses' own.
    let mut batch = model.init_state();
    assert!(model.fire("KeyPressed", &mut batch));
    assert!(model.fire("PressInFlight", &mut batch));
    assert!(!model.fire("UnsweptMoveRefuses", &mut batch));
    assert!(model.fire("UnsweptMovePays", &mut batch));
    assert_eq!(batch["bridged"], 2);
    assert_eq!(batch["spent"], 2);
    assert_eq!(batch["older"] + batch["younger"], 0);
    // …and with a hole before it there is no clock to partition by: refused.
    let mut holed = model.init_state();
    assert!(model.fire("KeyPressed", &mut holed));
    assert!(model.fire("PressInFlight", &mut holed));
    assert!(model.fire("UnlicensedAdvance", &mut holed));
    assert!(!model.fire("UnsweptMovePays", &mut holed));
    assert!(model.fire("UnsweptMoveRefuses", &mut holed));
    assert_eq!(holed["bridged"], 0);
    assert_eq!(holed["forfeited"], 2);

    // THE PATIENCE: a press the program swallowed goes stale; the next move
    // drops it as expired, and it pays for nothing.
    let mut swallowed = model.init_state();
    assert!(model.fire("KeyPressed", &mut swallowed));
    assert!(model.fire("TimePasses", &mut swallowed));
    assert_eq!(
        (swallowed["older"], swallowed["younger"], swallowed["stale"]),
        (0, 0, 1)
    );
    assert!(model.fire("UnlicensedAdvance", &mut swallowed));
    assert!(model.fire("KeyPressed", &mut swallowed));
    assert!(!model.fire("SweptMovePays", &mut swallowed));
    assert!(model.fire("SweptMoveRefuses", &mut swallowed));
    assert_eq!(
        swallowed["expired"], 1,
        "the stale press was dropped, not spent"
    );
    assert_eq!(swallowed["stale"], 0);
    assert_eq!(swallowed["bridged"], 0);
    assert!(model.check_invariant("ExpiredPressesBuyNothing", &swallowed));

    // A navigation licence (a move-shaped clear), an erase (an in-place
    // clear), and a retreat: the first forgets the waiting press and the
    // hole, the second forgets the press and leaves the hole standing, the
    // third leaves the fresh press waiting.
    let mut nav = model.init_state();
    assert!(model.fire("KeyPressed", &mut nav));
    assert!(model.fire("UnlicensedAdvance", &mut nav));
    assert!(model.fire("LedgerForgotten", &mut nav));
    assert_eq!((nav["older"], nav["younger"], nav["hole"]), (0, 0, 0));
    assert_eq!(nav["forfeited"], 1);
    let mut erased = model.init_state();
    assert!(model.fire("KeyPressed", &mut erased));
    assert!(model.fire("UnlicensedAdvance", &mut erased));
    assert!(model.fire("LedgerClearedInPlace", &mut erased));
    assert_eq!(
        (erased["older"], erased["younger"], erased["hole"]),
        (0, 0, 1),
        "an in-place clear leaves the mirror, and so the hole, where it was"
    );
    assert_eq!(erased["forfeited"], 1);
    assert!(
        !model.fire("SweptMovePays", &mut erased),
        "…and nothing is left to explain that hole"
    );
    let mut retreat = model.init_state();
    assert!(model.fire("KeyPressed", &mut retreat));
    assert!(model.fire("UnlicensedAdvance", &mut retreat));
    assert!(model.fire("RetreatKeepsPresses", &mut retreat));
    assert_eq!(
        (retreat["older"], retreat["younger"], retreat["hole"]),
        (0, 1, 0)
    );
    assert_eq!(retreat["forfeited"], 0);
    // …but a retreat over a STALE press drops it: the engine expires before
    // it looks at the move's shape.
    let mut stale_retreat = model.init_state();
    assert!(model.fire("KeyPressed", &mut stale_retreat));
    assert!(model.fire("TimePasses", &mut stale_retreat));
    assert!(model.fire("RetreatKeepsPresses", &mut stale_retreat));
    assert_eq!(
        (stale_retreat["stale"], stale_retreat["expired"]),
        (0, 1),
        "the stale press is dropped by the retreat, not kept"
    );
    assert!(model.check_invariant("ExpiredPressesBuyNothing", &stale_retreat));

    // THE PARTIAL EXPIRY: presses are banked in clock order, so the patience
    // elapses oldest-first — the older press goes stale while the key stays
    // fresh, and the key's echo pays only its own cell.
    let mut partial = model.init_state();
    assert!(model.fire("KeyPressed", &mut partial));
    assert!(model.fire("KeyPressed", &mut partial));
    assert_eq!((partial["older"], partial["younger"]), (1, 1));
    assert!(model.fire("OnePressGoesStale", &mut partial));
    assert_eq!(
        (partial["older"], partial["younger"], partial["stale"]),
        (0, 1, 1),
        "the OLDER press is the one that went stale"
    );
    assert!(model.fire("SweptMovePays", &mut partial));
    assert_eq!(
        (partial["expired"], partial["spent"], partial["bridged"]),
        (1, 1, 0)
    );
    assert!(model.check_invariant("OnePressOneCell", &partial));

    // THE MUTANTS, each named and each with its own concrete counterexample.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    // The first ledger: the burst's in-flight press pays for the nudge's cell.
    let mut four_cells = buggy.init_state();
    assert!(buggy.fire("UnlicensedAdvance", &mut four_cells));
    assert!(buggy.fire("KeyPressed", &mut four_cells));
    assert!(buggy.fire("PressInFlight", &mut four_cells));
    assert!(buggy.fire("SweptMovePays", &mut four_cells));
    assert_eq!(four_cells["laid_hole"], 1);
    assert_eq!(four_cells["last_older"], 0);
    assert!(!buggy.check_invariant("BridgedNeverExceedsOlderPresses", &four_cells));
    assert!(!buggy.check_invariant("AProgramGapIsNeverBridged", &four_cells));

    // …and the other half of the partition: two swallowed keys before a
    // one-cell nudge, the nudge laid from a press that never produced it.
    let mut inexact = buggy.init_state();
    assert!(buggy.fire("KeyPressed", &mut inexact));
    assert!(buggy.fire("KeyPressed", &mut inexact));
    assert!(buggy.fire("UnlicensedAdvance", &mut inexact));
    assert!(buggy.fire("KeyPressed", &mut inexact));
    assert_eq!((inexact["older"], inexact["hole"]), (2, 1));
    assert!(buggy.fire("SweptMovePays", &mut inexact));
    assert!(buggy.check_invariant("BridgedNeverExceedsOlderPresses", &inexact));
    assert!(!buggy.check_invariant("AProgramGapIsNeverBridged", &inexact));

    // A refusal that keeps the ledger: the presses survive to fund a later cell.
    let mut kept = buggy.init_state();
    assert!(buggy.fire("KeyPressed", &mut kept));
    assert!(buggy.fire("KeyPressed", &mut kept));
    assert!(buggy.fire("SweptMoveRefuses", &mut kept));
    assert_eq!(kept["older"] + kept["younger"], 2);
    assert!(!buggy.check_invariant("ForfeitedCreditsNeverReturn", &kept));

    // The keydown that pre-draws its cell — T1 broken in ledger terms.
    let mut predrawn = buggy.init_state();
    assert!(buggy.fire("KeyPressed", &mut predrawn));
    assert_eq!(predrawn["bridged"], 1);
    assert_eq!(predrawn["spent"], 0);
    assert!(!buggy.check_invariant("NoBridgeOutsideALicensedMove", &predrawn));

    // One press, two cells: the unswept batch billed against a single press.
    let mut double = buggy.init_state();
    assert!(buggy.fire("KeyPressed", &mut double));
    assert!(buggy.fire("UnsweptMovePays", &mut double));
    assert_eq!(double["spent"], 2);
    assert_eq!(double["banked"], 1);
    assert!(!buggy.check_invariant("OnePressOneCell", &double));

    // A stale press spent as payment instead of dropped.
    let mut stale_paid = buggy.init_state();
    assert!(buggy.fire("KeyPressed", &mut stale_paid));
    assert!(buggy.fire("TimePasses", &mut stale_paid));
    assert!(buggy.fire("KeyPressed", &mut stale_paid));
    assert!(buggy.fire("SweptMovePays", &mut stale_paid));
    assert_eq!(stale_paid["stale_gone"], 1);
    assert_eq!(
        stale_paid["expired"], 0,
        "the mutant spent the swallowed press"
    );
    assert!(!buggy.check_invariant("ExpiredPressesBuyNothing", &stale_paid));
    // …and when the stale presses alone cover the bill, the older ones
    // SURVIVE on the mutant's ledger: the mutant spends, it does not
    // evaporate — conservation holds, only the patience law falls.
    let mut stale_covers = buggy.init_state();
    assert!(buggy.fire("KeyPressed", &mut stale_covers));
    assert!(buggy.fire("TimePasses", &mut stale_covers));
    assert!(buggy.fire("KeyPressed", &mut stale_covers));
    assert!(buggy.fire("KeyPressed", &mut stale_covers));
    assert_eq!(
        (
            stale_covers["older"],
            stale_covers["younger"],
            stale_covers["stale"]
        ),
        (1, 1, 1)
    );
    assert!(buggy.fire("SweptMovePays", &mut stale_covers));
    assert_eq!(
        (
            stale_covers["older"],
            stale_covers["younger"],
            stale_covers["stale"]
        ),
        (1, 1, 0),
        "the stale press paid; the older and the key are still banked"
    );
    assert!(buggy.check_invariant("OnePressOneCell", &stale_covers));
    assert!(!buggy.check_invariant("ExpiredPressesBuyNothing", &stale_covers));

    // A refusal that lays the hop's cell anyway.
    let mut laid_anyway = buggy.init_state();
    assert!(buggy.fire("KeyPressed", &mut laid_anyway));
    assert!(buggy.fire("KeyPressed", &mut laid_anyway));
    assert!(buggy.fire("SweptMoveRefuses", &mut laid_anyway));
    assert_eq!(
        (laid_anyway["just_refused"], laid_anyway["bridged_delta"]),
        (1, 1)
    );
    assert!(!buggy.check_invariant("NoBridgeOnARefusedMove", &laid_anyway));
}

/// Retained history is a different coordinate space from the active cursor.
/// Every cursor-owned pixel/cell is suppressed immediately, a retained capture
/// stays dark, and the hidden resident-pet lifecycle keeps progressing until
/// its scheduler can settle. The mutant preserves the old overlays and stalls
/// the pet tick.
#[test]
fn derived_cursor_viewport_lifecycle_proves_and_catches_history_overlays() {
    let model = cursor_viewport_lifecycle_model();
    assert_proves_and_catches(&model);

    let charged = model.successors("ChargeLive", &model.init_state())[0].clone();
    let history = model.successors("EnterHistory", &charged)[0].clone();
    assert_eq!(history["live_viewport"], 0);
    for key in [
        "glow_visible",
        "trail_visible",
        "cursor_body_visible",
        "pet_visible",
        "base_cursor_visible",
    ] {
        assert_eq!(history[key], 0, "{key} must be dark in history");
    }
    assert_eq!(history["pet_brain_ticked"], 1);
    let retained = model.successors("RetainHistory", &history)[0].clone();
    assert_eq!(retained["retained_checked"], 1);
    assert!(model.check_invariant("HistorySuppressesCursorOwnedPixels", &retained));
    let settled = model.successors("SettleHistoryBrain", &retained)[0].clone();
    assert_eq!(settled["pet_brain_pending"], 0);
    assert_eq!(settled["scheduler_stuck"], 0);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let charged = buggy.successors("ChargeLive", &buggy.init_state())[0].clone();
    let stale = buggy.successors("EnterHistory", &charged)[0].clone();
    assert!(
        !buggy.check_invariant("HistorySuppressesCursorOwnedPixels", &stale),
        "the history-overlay mutant must retain visible cursor-owned pixels"
    );
    assert!(
        !buggy.check_invariant("HiddenSchedulerNeverSticks", &stale),
        "the history-overlay mutant must expose its stalled pet scheduler"
    );
    assert_eq!(stale["pet_brain_ticked"], 0);
    assert!(
        !buggy.check_invariant("HiddenPetLifecycleProgresses", &stale),
        "the `cur=None` mutant gives the hidden pet brain no tick"
    );
}

/// A resident cursor companion's visible body and hit target are coordinates,
/// while its configured identity is not. Pane/tab replacement and a truly
/// unpresentable blur retire the former; typed-wake and recording pins are
/// explicit preservation controls. The mutant retains stale coordinates at
/// the four retiring boundaries.
#[test]
fn derived_cursor_companion_owner_lifecycle_proves_and_catches_stale_coordinates() {
    let model = cursor_companion_owner_lifecycle_model();
    assert_proves_and_catches(&model);

    let materialized = model.successors("Materialize", &model.init_state())[0].clone();
    assert_eq!(materialized["pet_visible"], 1);
    assert_eq!(materialized["hit_target"], 1);

    for action in [
        "PaneOwnerSwitch",
        "TabOwnerSwitch",
        "ScreenBufferSwitch",
        "UnpresentableFocusLoss",
    ] {
        let retired = model.successors(action, &materialized)[0].clone();
        assert_eq!(retired["pet_visible"], 0, "{action}");
        assert_eq!(retired["hit_target"], 0, "{action}");
        assert_eq!(retired["durable_identity"], 1, "{action}");
        assert!(model.check_invariant("RetiringBoundariesAreDark", &retired));
    }

    for action in ["TypedWakeFocusLoss", "RecordingFocusLoss"] {
        let pinned = model.successors(action, &materialized)[0].clone();
        assert_eq!(pinned["pet_visible"], 1, "{action}");
        assert_eq!(pinned["hit_target"], 1, "{action}");
        assert_eq!(pinned["durable_identity"], 1, "{action}");
        assert!(model.check_invariant("PresentationPinsPreserveTheSighting", &pinned));
        let mut overbroad_blur = pinned;
        overbroad_blur.insert("pet_visible", 0);
        overbroad_blur.insert("hit_target", 0);
        assert!(
            !model.check_invariant("PresentationPinsPreserveTheSighting", &overbroad_blur),
            "{action}: an overbroad raw-blur retirement must be rejected"
        );
    }

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let materialized = buggy.successors("Materialize", &buggy.init_state())[0].clone();
    let stale = buggy.successors("PaneOwnerSwitch", &materialized)[0].clone();
    assert!(stale["pet_visible"] == 1 && stale["hit_target"] == 1);
    assert!(
        !buggy.check_invariant("RetiringBoundariesAreDark", &stale),
        "the retained-coordinate mutant must expose a visible stale body and hit target"
    );
    for action in ["TypedWakeFocusLoss", "RecordingFocusLoss"] {
        let rebuilt = buggy.successors(action, &materialized)[0].clone();
        assert!(
            !buggy.check_invariant("PresentationPinsPreserveTheSighting", &rebuilt),
            "{action}: the raw-blur rebuild hides a pinned body"
        );
        assert!(
            !buggy.check_invariant("DurableIdentitySurvives", &rebuilt),
            "{action}: and forgets the configured species"
        );
    }
}

#[test]
fn derived_composed_witness_generation_preserves_captured_rows_and_fences_fresh_reads() {
    let model = aterm_spec::derive::composed_witness_generation_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|candidate| candidate.name == "ComposedWitnessGeneration")
    );
    assert_proves_and_catches(&model);

    let mut captured = model.init_state();
    assert!(!model.action_enabled("ReadCaptured", &captured));
    for action in ["Capture", "Mutate", "ReadCaptured"] {
        assert!(model.fire(action, &mut captured));
    }
    assert_eq!(captured["admitted"], 2);

    let mut fresh = model.init_state();
    assert!(model.fire("Mutate", &mut fresh));
    assert!(!model.action_enabled("ReadFresh", &fresh));
}

#[test]
fn derived_composed_sync_hold_requires_every_pane_to_release() {
    let model = composed_sync_hold_model();
    assert_proves_and_catches(&model);

    let armed = model.successors("ArmBoth", &model.init_state())[0].clone();
    let a_closed = model.successors("CloseOnlyA", &armed)[0].clone();
    assert_eq!(a_closed["a_hold"], 0);
    assert_eq!(a_closed["b_hold"], 1);
    assert_eq!(a_closed["presented"], 0);
    assert!(model.check_invariant("NoPartialCompositePresent", &a_closed));
    let released = model.successors("CloseRemainingB", &a_closed)[0].clone();
    assert_eq!(released["presented"], 1);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let armed = buggy.successors("ArmBoth", &buggy.init_state())[0].clone();
    let partial = buggy.successors("CloseOnlyA", &armed)[0].clone();
    assert!(
        !buggy.check_invariant("NoPartialCompositePresent", &partial),
        "the aggregate-close mutant must present while pane B remains held"
    );
}

/// A close followed immediately by a clean reopen may still publish the
/// completed close boundary. Once a parser action dirties that reopened
/// episode, no part of it is visible until the next close.
#[test]
fn derived_sync_reopen_visibility_holds_dirty_new_episode_until_close() {
    let model = sync_reopen_visibility_model();
    assert_proves_and_catches(&model);
    assert!(
        aterm_spec::xref::model_registry()
            .into_iter()
            .any(|candidate| candidate.name == model.name),
        "the close/reopen visibility law must remain enrolled in the xref registry"
    );

    let first_closed = model.successors("CloseFirstEpisode", &model.init_state())[0].clone();
    assert_eq!(first_closed["completed_generation"], 1);
    assert_eq!(first_closed["hold"], 0);

    let clean_reopen = model.successors("ReopenClean", &first_closed)[0].clone();
    assert_eq!(clean_reopen["sync_active"], 1);
    assert_eq!(clean_reopen["open_dirty"], 0);
    assert_eq!(clean_reopen["hold"], 0);
    assert_eq!(clean_reopen["presented_generation"], 1);
    assert!(model.check_invariant("CleanReopenMayPresentCompletedBoundary", &clean_reopen));

    let dirty_reopen = model.successors("DirtyReopenedEpisode", &clean_reopen)[0].clone();
    assert_eq!(dirty_reopen["open_dirty"], 1);
    assert_eq!(dirty_reopen["hold"], 1);
    assert_eq!(dirty_reopen["partial_visible"], 0);
    assert_eq!(dirty_reopen["presented_generation"], 1);
    assert!(model.check_invariant("DirtyReopenHoldsUntilClose", &dirty_reopen));

    let reopened_closed = model.successors("CloseReopenedEpisode", &dirty_reopen)[0].clone();
    assert_eq!(reopened_closed["open_dirty"], 0);
    assert_eq!(reopened_closed["hold"], 0);
    assert_eq!(reopened_closed["completed_generation"], 2);
    assert_eq!(reopened_closed["presented_generation"], 2);

    // The two `Buggy=1` hold rules are alternatives. The close-sequence
    // license presents the clean reopen, as the healthy rule does, and then
    // leaks the dirty episode …
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    assert_eq!(
        model.successors("PickLevelHold", &model.init_state()),
        vec![model.init_state()],
        "no hold fault to pick at Buggy=0"
    );
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &[]),
        Ok(0),
        "no action is dead at the committed config (strict vacuity)"
    );
    let first_closed = buggy.successors("CloseFirstEpisode", &buggy.init_state())[0].clone();
    let clean_reopen = buggy.successors("ReopenClean", &first_closed)[0].clone();
    assert_eq!(
        (clean_reopen["hold"], clean_reopen["presented_generation"]),
        (0, 1)
    );
    assert!(buggy.check_invariant("CleanReopenMayPresentCompletedBoundary", &clean_reopen));
    let leaked = buggy.successors("DirtyReopenedEpisode", &clean_reopen)[0].clone();
    assert_eq!(leaked["open_dirty"], 1);
    assert_eq!(leaked["hold"], 0);
    assert_eq!(leaked["partial_visible"], 1);
    assert_eq!(leaked["presented_generation"], 1);
    assert!(
        !buggy.check_invariant("DirtyReopenHoldsUntilClose", &leaked),
        "the close-sequence-only mutant must leak the dirty reopened episode"
    );

    // … while the level-sampled hold holds the clean reopen and the dirty
    // episode alike, and never presents the completed boundary.
    let mut level = buggy.init_state();
    for action in ["PickLevelHold", "CloseFirstEpisode", "ReopenClean"] {
        assert!(buggy.fire(action, &mut level), "{action}: {level:?}");
    }
    assert_eq!((level["hold"], level["presented_generation"]), (1, 0));
    assert!(
        !buggy.check_invariant("CleanReopenMayPresentCompletedBoundary", &level),
        "the level-sampled hold must starve the completed boundary"
    );
    assert!(buggy.fire("DirtyReopenedEpisode", &mut level));
    assert_eq!((level["hold"], level["partial_visible"]), (1, 0));
}

/// Every surviving cursor-effect anchor follows a PTY scroll by the exact row
/// delta and every off-top member is retired. The mutant strands both families
/// in their old coordinates.
#[test]
fn derived_cursor_effect_scroll_proves_and_catches_stranded_geometry() {
    assert_proves_and_catches(&cursor_effect_scroll_model());
}

/// Capped/zero retained history cannot hide a uniform scroll from the host,
/// including on the active alternate screen; non-uniform region/reset motion
/// instead invalidates all cached effect coordinates. The mutant restores
/// retained-count diffing (and the old alt-only reset) and strands light.
#[test]
fn derived_cursor_scroll_signal_proves_and_catches_capped_history_stranding() {
    assert_proves_and_catches(&cursor_scroll_signal_model());
}

/// The v2 landing pool admits every arrival and evicts the OLDEST at capacity, so
/// it always holds exactly the latest arrivals. The mutant drops the arrival at
/// saturation instead — the newest jump lands with no impact.
#[test]
fn derived_rainbow_landing_pool_keeps_the_latest_arrivals() {
    let model = rainbow_landing_pool_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &["StateBounds"]);

    // A fourth arrival into a full pool evicts arrival 1 and keeps arrival 4.
    let mut full = model.init_state();
    for _ in 0..4 {
        assert!(model.fire("Land", &mut full), "{full:?}");
    }
    assert_eq!(
        (full["resident"], full["oldest"], full["newest"]),
        (3, 2, 4)
    );
    // Expiry takes the oldest first.
    assert!(model.fire("ExpireOne", &mut full));
    assert_eq!(
        (full["resident"], full["oldest"], full["newest"]),
        (2, 3, 4)
    );
}

/// A delayed *presentable* callback beyond the full hello lifetime is not a
/// request to start the fade clock late. Once an earlier visible frame was
/// delivered, the healthy machine consumes that elapsed tail atomically and
/// returns Hidden; Buggy=1 reproduces the late opaque Fade/animation tail.
/// The same action is deliberately disabled after HiddenTick so this stronger
/// latency guarantee cannot weaken the existing hidden-pause promise.
#[test]
fn derived_cursor_cat_long_gap_settles_directly_and_preserves_hidden_pause() {
    let healthy = cursor_cat_model();
    let mut early = healthy.init_state();
    assert!(healthy.fire("Collect", &mut early));
    assert_eq!(early[&"presented_once"], 1);
    assert_eq!(early[&"presentable"], 1);
    assert_eq!(early[&"visible"], 1);

    let mut hidden = early.clone();
    assert!(healthy.fire("HiddenTick", &mut hidden));
    assert_eq!(hidden[&"elapsed"], early[&"elapsed"]);
    assert_eq!(hidden[&"presented"], early[&"presented"]);
    assert_eq!(hidden[&"forced"], early[&"forced"]);
    assert_eq!(hidden[&"wall_expired"], 0);
    assert!(
        !healthy.fire("LongPresentableGap", &mut hidden),
        "a hidden wall-clock gap is not licensed as a presentable long gap"
    );

    let mut settled = early.clone();
    assert!(healthy.fire("LongPresentableGap", &mut settled));
    assert_eq!(settled[&"wall_expired"], 1);
    assert_eq!(settled[&"elapsed"], 5);
    assert_eq!(settled[&"phase"], 0);
    assert_eq!(settled[&"visible"], 0);
    assert_eq!(settled[&"forced"], 0);
    assert!(healthy.check_invariant("LongGapSettlesHidden", &settled));
    assert!(healthy.check_invariant("HiddenAtDeadline", &settled));

    let mut buggy = cursor_cat_model();
    for cst in &mut buggy.consts {
        if cst.0 == "Buggy" {
            cst.1 = 1;
        }
    }
    let mut late_fade = buggy.init_state();
    assert!(buggy.fire("Collect", &mut late_fade));
    assert!(buggy.fire("LongPresentableGap", &mut late_fade));
    assert_eq!(late_fade[&"wall_expired"], 1);
    assert_eq!(late_fade[&"phase"], 2);
    assert_eq!(late_fade[&"visible"], 1);
    assert_eq!(late_fade[&"forced"], 0);
    assert!(
        !buggy.check_invariant("LongGapSettlesHidden", &late_fade),
        "the late Fade witness must violate direct settlement"
    );
    assert!(
        !buggy.check_invariant("HiddenAtDeadline", &late_fade),
        "the late Fade witness must remain visible at the elapsed deadline"
    );
}

/// Future ignition slots remain one-to-one with live owners; already-fired
/// history alone may outlive an owner, for at most the two-slot rolling-window
/// allowance. The expiry-only mutant leaves a future reservation behind on
/// owner departure and must produce a counterexample.
#[test]
fn derived_ignition_reservation_lifecycle_proves_and_catches_stale_future_work() {
    assert_proves_and_catches(&ignition_reservation_lifecycle_model());
}

/// Alignment rekeys a delayed ignition's limiter owner atomically. The mutant
/// leaves the slot under the retired identity, so prune drops it and admits a
/// competing overlapping flash inside the original episode's safety window.
#[test]
fn derived_ignition_reservation_rekey_proves_and_catches_overlap() {
    assert_proves_and_catches(&ignition_reservation_rekey_model());
}

/// Done-mark LRU replacement is cardinality-bounded and selects its oldest
/// node with one direct head lookup. The retired full-map scan is the Buggy
/// branch and must violate the constant-selection invariant at capacity.
#[test]
fn derived_done_mark_lru_proves_and_catches_full_map_selection() {
    assert_proves_and_catches(&done_mark_lru_model());
}

/// Sparkle-words v2 flash limiter (design §6.4/§9): WCAG 2.3.1, model-checked.
/// `ty` PROVES `IgnitionBound` (≤ 2 ignitions per rolling second, ≤ 1 under
/// overlap) and the REGION-scoped `RegionFlashPairs ≤ 3` at `Buggy = 0` for
/// BOTH `Overlap ∈ {0, 1}`, and CATCHES the overlap-blind limiter at
/// `Buggy = 1, Overlap = 1` (two overlapping ignitions in one second ⇒ 4
/// transition pairs on the shared region). `Buggy = 1, Overlap = 0` stays
/// green by design — disjoint novas at 2/s are legal, which is why the pair
/// bound is per-region, not window-global (the §9 binding-spec erratum).
#[test]
fn derived_flash_limiter_proves_and_catches_overlap_blindness() {
    let m = flash_limiter_model();
    // A copy of `m` with the named constants overridden — the multi-scenario
    // (Overlap ∈ {0,1}) analogue of `interp::with_buggy`.
    let with = |overrides: &[(&str, i64)]| -> Model {
        let mut m = m.clone();
        for c in &mut m.consts {
            if let Some((_, v)) = overrides.iter().find(|(n, _)| *n == c.0) {
                c.1 = *v;
            }
        }
        m
    };
    // INTERPRETER TIER (always): prove Buggy=0 at both scenario values, prove the
    // legal-by-design (Buggy=1, Overlap=0), catch (Buggy=1, Overlap=1).
    let interp_check =
        |overrides: &[(&str, i64)], must_hold: bool, label: &str| match aterm_spec::interp::bmc(
            &with(overrides),
        ) {
            Ok(n) => assert!(
                must_hold,
                "FlashLimiter {label}: held over {n} states but MUST violate"
            ),
            Err((st, inv)) => assert!(
                !must_hold,
                "FlashLimiter {label}: `{inv}` VIOLATED at {st:?} but must hold"
            ),
        };
    interp_check(&[], true, "(Buggy=0, Overlap=0)");
    interp_check(&[("Overlap", 1)], true, "(Buggy=0, Overlap=1)");
    interp_check(
        &[("Buggy", 1)],
        true,
        "(Buggy=1, Overlap=0) — disjoint novas at 2/s are legal",
    );
    interp_check(
        &[("Buggy", 1), ("Overlap", 1)],
        false,
        "(Buggy=1, Overlap=1)",
    );

    // TY ESCALATION TIER (wherever installed): the same four scenarios.
    if let Some(typ) = verify::ty_escalation("derived flash limiter spec") {
        let dir = std::env::temp_dir().join(format!("aterm-{}-{}", m.name, std::process::id()));
        std::fs::create_dir_all(&dir).expect("mk tempdir");
        let spec = dir.join(format!("{}.tla", m.name));
        std::fs::write(&spec, m.to_tla()).expect("write spec");
        let run = |cfg_name: &str, cfg: String| -> (bool, String) {
            let cfgp = dir.join(cfg_name);
            std::fs::write(&cfgp, cfg).expect("write cfg");
            let mut cmd = Command::new(&typ);
            cmd.arg("check").arg(&spec).arg("--config").arg(&cfgp);
            // ARMED: `FlashLimiter` is a derived model that is NOT in
            // `model_registry()`, so the xref sweep never covered it and its
            // prove halves would fail open under a bad reduction.
            aterm_spec::verify::arm_whole_space_check(&mut cmd);
            let out = cmd.output().expect("run ty check");
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            (out.status.success(), combined)
        };
        // Prove: Buggy = 0 at both scenario values.
        let (ok, out) = run("ok-disjoint.cfg", m.to_cfg());
        assert!(ok, "FlashLimiter (Buggy=0, Overlap=0) must prove\n{out}");
        let (ok, out) = run("ok-overlap.cfg", m.to_cfg_with(&[("Overlap", 1)]));
        assert!(ok, "FlashLimiter (Buggy=0, Overlap=1) must prove\n{out}");
        // Legal-by-design: an overlap-blind limiter on DISJOINT regions is fine.
        let (ok, out) = run("bug-disjoint.cfg", m.to_cfg_with(&[("Buggy", 1)]));
        assert!(
            ok,
            "FlashLimiter (Buggy=1, Overlap=0) must stay green — disjoint novas at 2/s are legal\n{out}"
        );
        // Catch: overlap-blindness on overlapping regions is the WCAG violation.
        let (ok, out) = run(
            "bug-overlap.cfg",
            m.to_cfg_with(&[("Buggy", 1), ("Overlap", 1)]),
        );
        assert!(
            !ok,
            "FlashLimiter (Buggy=1, Overlap=1) MUST yield a counterexample\n{out}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    eprintln!(
        "derived FlashLimiter: proven for Overlap in {{0,1}} and caught at (Buggy=1, Overlap=1)."
    );
}

/// The WINDOW-WIDE twin (`FlashLimiterWindow`): the WCAG 2.3.1 budget belongs
/// to the retina, so it is charged against EVERY live enforcer at once.
///
/// This is the machine-checked form of the sentence the scope-cardinality
/// census (`aterm-census` OB-13..OB-18, claim `flash-limiter`) would otherwise
/// assert in prose: "if N enforcers each apply the bound locally, the
/// aggregate is violated". It does NOT catch the per-pane refactor — no state
/// machine can know how many engines the host builds; the census does that,
/// and OB-16 fails the build if this model is deleted while
/// `prove_catch_and_multiply_scalar` fails this test if it goes toothless.
///
/// G4 fires at BOTH `Overlap` corners while G3 needs `Overlap = 1`: an
/// overlap-blind limiter misbehaves only where regions coincide, but
/// multiplying enforcers is wrong unconditionally.
#[test]
fn derived_flash_limiter_window_proves_catches_and_multiplies() {
    let _ = verify::prove_catch_and_multiply_scalar(
        &flash_limiter_window_model(),
        &[&[], &[("Overlap", 1)]],
        &[("Overlap", 1)],
        "derived flash limiter (window-wide)",
    );
}

/// **THE ALT-SCREEN ARCHIVE POOL, CHARGED AGAINST THE PROCESS.** The retention
/// budget belongs to the machine's memory, so it is charged against every live
/// archive at once — the same aggregation shape as the window-wide flash
/// limiter, and for the same reason: "this archive never exceeds its budget" is
/// a true theorem about ONE archive that stays true however many exist, so it
/// cannot see the defect where each tab keeps a whole 4 MiB and the process
/// pays eight times over.
///
/// The two invariants have one victim each, which is what makes the pair
/// honest. `PoolBounded` is charged against the SUM and stated over SETTLED
/// archives, because an archive can only lower its OWN retention — so the bound
/// CONVERGES rather than holding at every instant, and the antecedent is where
/// that is written down. `Local = 1` breaks it at every corner: multiplication
/// alone is the defect, no scenario needed. `IdleTakesNothing` is the review
/// blocker from the pooling round — an archive that holds nothing must cost the
/// ones that do nothing — and `Buggy = 1` breaks it immediately, with both
/// archives still empty. `Local` cannot damage that second invariant (a local
/// archive's allowance IS the whole total), which is what gives the G5
/// attribution corner something to say.
#[test]
fn derived_alt_archive_pool_proves_catches_and_multiplies() {
    let _ = verify::prove_catch_and_multiply_scalar(
        &alt_archive_pool_model(),
        &[&[]],
        &[],
        "derived alt-screen archive pool",
    );
}

/// **ONE LIVE HOLDER PER SESSION ID**, machine-checked — the safety property
/// behind a real incident, where the wrong answer is a keystroke delivered into
/// a human's window.
///
/// A pane's shell exports the identity the OUTER aterm preminted for the inner
/// aterm that pane may launch, and that export outlives the launch on purpose:
/// a child that exits must be able to relaunch under its original identity.
/// What the old spelling could not tell apart is a RELAUNCH from a SECOND
/// SIMULTANEOUS LAUNCH. Two instances answered to one id, and `@<sid>` resolves
/// through ONE discovery entry.
///
/// The two defects fail differently, which is the whole argument for the
/// shipped rule having TWO gates rather than one:
///
/// * `Local = 1` — each launch decides from its own premint read alone. Every
///   launch is individually "correct" and the id is held N times, so this
///   breaks `AtMostOneHolder` at EVERY corner. Multiplication alone is enough.
/// * `Buggy = 1` — the lock is consulted, the live entry is not. That is
///   invisible until `Handoff = 1`, because a seamless-update successor keeps
///   its ids by design and CANNOT inherit the predecessor's flock: the lock
///   goes free while the successor's own entry is the thing holding the id.
///   The G3 corner is therefore `Handoff = 1`, and it is why the entry gate
///   exists BESIDE the lock rather than instead of it.
///
/// The relaunch direction is checked too, so the safety property cannot be
/// satisfied by the lazy fix of never adopting: `Exit` releases the lock with
/// its holder (which is why the claim file is never unlinked — unlinking one a
/// peer holds open is how two processes could both come to hold it), and the
/// trace adopt → exit → adopt lands back on exactly one holder.
#[test]
fn derived_session_id_claim_proves_catches_and_multiplies() {
    let m = session_id_claim_model();
    let _ = verify::prove_catch_and_multiply_scalar(
        &m,
        &[&[], &[("Handoff", 1)]],
        &[("Handoff", 1)],
        "derived session id claim",
    );

    // THE OTHER DIRECTION, so `AtMostOneHolder` cannot be satisfied by refusing
    // every adoption: the premint survives a relaunch.
    let mut st = m.init_state();
    assert!(m.fire("Launch", &mut st), "the first launch adopts");
    assert_eq!(st.get("holders"), Some(&1));
    assert!(m.fire("Exit", &mut st), "the holder exits");
    assert_eq!(st.get("lock"), Some(&0), "the lock dies WITH its holder");
    assert!(m.fire("Launch", &mut st), "and the relaunch adopts again");
    assert_eq!(st.get("holders"), Some(&1));

    // THE HANDOFF WINDOW (closed 2026-09-25): the predecessor has exited, its
    // lock is gone, and the successor has not published yet. The successor
    // MARKER is the only gate standing; with it the launch is refused.
    let handoff = aterm_spec::interp::with_consts(&m, &[("Handoff", 1)]);
    let mut st = handoff.init_state();
    for action in ["Launch", "PredecessorExits", "Launch"] {
        assert!(handoff.fire(action, &mut st), "{action}: {st:?}");
    }
    assert_eq!(
        st.get("lock"),
        Some(&0),
        "the predecessor's lock died with it"
    );
    assert_eq!(
        st.get("entry"),
        Some(&0),
        "nothing of the successor is published"
    );
    assert_eq!(st.get("holders"), Some(&1), "the marker refused the launch");
    assert!(handoff.fire("SuccessorPublishes", &mut st));
    assert_eq!(
        st.get("marker"),
        Some(&0),
        "the successor retires its marker"
    );
    assert!(handoff.fire("Launch", &mut st));
    assert_eq!(
        st.get("holders"),
        Some(&1),
        "the entry refuses after publish"
    );
    assert!(
        aterm_spec::interp::bmc(&handoff).is_ok(),
        "the marked handoff holds AtMostOneHolder over its whole space"
    );
    // …and WITHOUT the marker (`Unmarked = 1`, the pre-fix exit) the same window
    // duplicates the id: the mutant that makes the marker non-vacuous.
    let unmarked = aterm_spec::interp::with_consts(&m, &[("Handoff", 1), ("Unmarked", 1)]);
    assert!(
        aterm_spec::interp::bmc(&unmarked).is_err(),
        "an unmarked handoff window must admit a second holder"
    );
}

/// **A FLICKERING PROGRAM NEVER TAKES THE CURSOR.** The anti-flap law of the
/// cursor companion's tenure gate — what stands between the user and a cat that
/// changes identity every time a command block opens and closes.
///
/// `KittyTenure::observe` is a DWELL gate, and the load-bearing arm is the one
/// that is easy to read past: ANY observation that does not match the standing
/// candidate restarts it. So a pane whose claim alternates never accumulates
/// dwell and the cat does not move. This model is the flicker scenario and
/// nothing else — `last` forces the two observations to alternate, so every
/// trace in the state space is a flicker storm — which lets the invariant be
/// the flat sentence `the identity never changes` rather than a rate.
///
/// `Buggy = 1` is the pre-tenure behaviour, landing the raw claim at once, and
/// it breaks the law on the FIRST observation.
#[test]
fn derived_companion_tenure_flicker_proves_and_catches_a_flapping_cat() {
    let m = companion_tenure_flicker_model();
    assert_proves_and_catches(&m);
    assert_every_invariant_carries_a_mutant(&m, &[]);

    // And the storm is really a storm: the trace alternates, and the dwell
    // clock never reaches the tenure because each observation resets it.
    let mut st = m.init_state();
    for _ in 0..3 {
        assert!(m.fire("SeeProgram", &mut st));
        assert!(m.fire("SeeNone", &mut st));
    }
    assert_eq!(
        st.get("changes"),
        Some(&0),
        "six flickers, and the cat held still"
    );
    assert_eq!(st.get("worn"), Some(&0), "still the base cat");
}

/// **WEARING A CAT IS A JOIN, SO NO REPLICA LOSES A PICK.** The kitty
/// collection is replicated — every instance keeps a copy and folds the others
/// in — and `merge_collectible` takes `max_ts` on the favourite stamp, which
/// makes the merge a join: commutative, idempotent, order-independent. That is
/// the whole reason a wear needs no unpin and no tombstone, and it shipped as a
/// sentence in a commit message with nothing behind it.
///
/// `Buggy = 1` is the wrong merge a reader reaches for when a merge looks like
/// an assignment: TAKE the incoming value rather than the max. The mutant is not
/// abstract — it is a stale delta landing on a replica that has since worn
/// something, destroying a pin the user made.
///
/// Convergence is witnessed here as a TRACE rather than stated as an invariant,
/// because no mutant of one dial can falsify it (an assignment merge converges
/// too, just on the wrong value) and this suite fails a ghost invariant on
/// purpose. `assert_every_invariant_carries_a_mutant` is run below to prove the
/// one law that remains is not one.
#[test]
fn derived_kitty_pin_merge_proves_and_catches_a_lost_pin() {
    let m = kitty_pin_merge_model();
    assert_proves_and_catches(&m);
    assert_every_invariant_carries_a_mutant(&m, &[]);

    // THE MUTANT, concretely: B wears a cat, then A's older delta lands and
    // takes B's stamp back to nothing.
    let bug = aterm_spec::interp::with_buggy(&m, 1);
    let mut st = bug.init_state();
    assert!(bug.fire("WearOneOnA", &mut st));
    assert!(bug.fire("WearTwoOnB", &mut st));
    assert_eq!(st.get("b2"), Some(&2), "B's own pick is stamped");
    assert!(bug.fire("FlushAToB", &mut st));
    assert_eq!(
        st.get("b2"),
        Some(&0),
        "and an assignment merge destroyed it"
    );
    assert!(!bug.check_invariant("AMergeNeverLosesAPick", &st));

    // AND THE TRACE the sentence describes: with the join, a flush each way
    // leaves both ledgers holding both picks, each at its own stamp.
    let mut st = m.init_state();
    assert!(m.fire("WearOneOnA", &mut st));
    assert!(m.fire("WearTwoOnB", &mut st));
    assert!(m.fire("FlushAToB", &mut st));
    assert!(m.fire("FlushBToA", &mut st));
    assert_eq!(
        (st.get("a1"), st.get("a2")),
        (st.get("b1"), st.get("b2")),
        "the two ledgers agree"
    );
    assert_eq!(st.get("a1"), Some(&1), "and cat one kept A's stamp");
    assert_eq!(st.get("a2"), Some(&2), "and cat two kept B's");
}

/// Rainbow kitty scheduler regression family: idle blink edges are silent, a content
/// present consumes the stale effect timer, and brisk tails remain phase-locked
/// instead of adding redraw cost to every interval. Each model proves the fixed
/// rule at Buggy=0 and independently requires a counterexample at Buggy=1.
#[test]
fn derived_rainbow_idle_twinkle_proves_and_catches_idle_wakes() {
    assert_proves_and_catches(&rainbow_idle_twinkle_model());
}

/// A missing compositor callback cannot restart the rainbow kitty exit lifecycle. The
/// first sample after the logical completion deadline is settled and disarmed;
/// Buggy restarts visible reach/retract motion at callback time. Bound to the
/// real engine by aterm-effects'
/// `rainbow_kitty::tests::a_late_tick_samples_the_settled_exit_swoosh`.
#[test]
fn derived_rainbow_exit_sampling_proves_and_catches_sparse_restart() {
    let healthy = rainbow_exit_sampling_model();
    assert_proves_and_catches(&healthy);
    assert_every_invariant_carries_a_mutant(&healthy, &["SampleBounded"]);
    let mut state = healthy.init_state();
    assert!(healthy.fire("ElapseDone", &mut state));
    assert!(healthy.fire("ObserveDone", &mut state));
    assert_eq!(state[&"logical_done"], 1);
    assert_eq!(state[&"sampled"], 1);
    assert_eq!(state[&"visible"], 0);
    assert_eq!(state[&"active"], 0);
    assert!(healthy.check_invariant("SettledSampleHasNoLight", &state));
    assert!(healthy.check_invariant("SettledSampleDisarms", &state));

    let buggy = aterm_spec::interp::with_buggy(&healthy, 1);
    let mut restarted = buggy.init_state();
    assert!(buggy.fire("ElapseDone", &mut restarted));
    assert!(buggy.fire("ObserveDone", &mut restarted));
    assert_eq!(restarted[&"visible"], 1);
    assert_eq!(restarted[&"active"], 1);
    assert!(!buggy.check_invariant("SettledSampleHasNoLight", &restarted));
    assert!(!buggy.check_invariant("SettledSampleDisarms", &restarted));
}

#[test]
fn derived_effect_present_rebase_proves_and_catches_frame_doublets() {
    assert_proves_and_catches(&effect_present_rebase_model());
}

#[test]
fn derived_effect_presentability_settle_proves_and_catches_stranded_pixels() {
    let model = effect_presentability_settle_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut stranded = buggy.init_state();
    assert!(buggy.fire("ExpireWake", &mut stranded));
    assert!(buggy.fire("UnrelatedPark", &mut stranded));
    assert_eq!(stranded["pending"], 0);
    assert!(
        !buggy.check_invariant("LitPixelsHaveSettleRoute", &stranded),
        "the edge-triggered park must supply the concrete stranded-raster counterexample"
    );

    let mut healthy = model.init_state();
    assert!(model.fire("ExpireWake", &mut healthy));
    assert!(model.fire("UnrelatedPark", &mut healthy));
    assert_eq!(healthy["pending"], 1);
    assert!(model.fire("PaintSettle", &mut healthy));
    assert_eq!(healthy["pixels_lit"], 0);
    assert_eq!(healthy["effect_active"], 0);
}

#[test]
fn derived_effect_phase_lock_proves_and_catches_cadence_slide() {
    assert_proves_and_catches(&effect_phase_lock_model());
}

/// PRISM WAKE crosses three independently governed components: the visual
/// episode, the host's parked scheduler, and the shared audio admission gate.
/// Prove the healthy chain and require every historical mutant to be both
/// reachable and invariant-breaking.
#[test]
fn derived_output_streak_delivery_proves_and_catches_lost_episode_edges() {
    let model = output_streak_episode_delivery_model();
    assert_proves_and_catches(&model);
    let negative_controls = [
        "BuggyThinOpening",
        "BuggyRetireWithoutWake",
        "BuggyClaimHumanGap",
    ];
    assert_eq!(
        verify::audit_dead_negative_controls(&model, &negative_controls),
        Ok(negative_controls.len()),
        "every cue, wake, and human-slot mutant must fire and fail independently"
    );

    let mut state = model.init_state();
    for action in [
        "HumanVoice",
        "OpenOutput",
        "RetireVisuals",
        "SettleAtWake",
        "HumanGapElapses",
        "CheckOutputDoesNotClaimHuman",
        "NextHuman",
    ] {
        assert!(model.fire(action, &mut state), "healthy action {action}");
    }
    assert_eq!(
        (
            state["phase"],
            state["shimmer"],
            state["settle"],
            state["wake"],
            state["next_human"],
        ),
        (3, 1, 1, 0, 1)
    );
}

#[test]
fn derived_output_streak_attribution_proves_and_catches_unrelated_output() {
    let model = output_streak_attribution_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &["Bounded"]);
}

/// PHOSPHOR rain lifecycle (docs/matrix-rain-design.md §10): the
/// Idle→Raining→Draining→Idle machine — `ty` PROVES `NoUnlicensedRain` (every
/// Raining entry is paid for by a host activity event), the `CanReachIdle`
/// fuel invariant (a Draining pane ALWAYS lands Idle within the fixed 30-tick
/// drain bound — "no configuration animates forever"), and the structural
/// bounds at Buggy=0, and CATCHES the phantom-relight at Buggy=1 (a drained
/// pane re-enters Raining with NO activity event — the cmd-tab-alone replay)
/// -> counterexample on `NoUnlicensedRain`. Tier-1 binding: aterm-effects'
/// `rain_lifecycle_conformance_real_engine_projects_onto_model`.
#[test]
fn derived_rain_lifecycle_proves_and_catches_phantom_relight() {
    assert_proves_and_catches(&rain_lifecycle_model());
}

/// PHOSPHOR rain band containment (docs/matrix-rain-design.md §7/§10): the
/// damage law behind `aterm_render::compute_dirty_rows` — `ty` PROVES
/// `Contained` (every emitted quad whose bytes changed this tick lies in a
/// marked dirty row, INCLUDING the mutation-tick case where the glyph hash
/// window rolls and the WHOLE lit band changes at once) at Buggy=0, and
/// CATCHES the skipped mutation-tick marking at Buggy=1 (a strictly-interior
/// trail row changes UNMARKED — the stale-glyph ghost) -> counterexample on
/// `Contained`. Bound to the shipping marker by
/// `aterm-render/tests/rain_render.rs::rain_marking_conforms_to_the_rain_band_containment_model`.
#[test]
fn derived_rain_band_containment_proves_and_catches_stale_glyph_ghost() {
    let model = rain_band_containment_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &[]);
}

/// PHOSPHOR rain ignition floor (docs/matrix-rain-design.md §4/§10): the
/// flash-safety theorem — `ty` PROVES `HeadPassFloor` (a column's head passes
/// any given cell at most once per second, `C·p·tick_ms >= 1000 ms`, because
/// the cycle length carries the runtime G-extension over even the smallest
/// grids) at Buggy=0, and CATCHES the dropped G-extension at Buggy=1 (a 3-row
/// grid at p=2 cycles in 462 ms — the head re-flashes the same cell twice a
/// second) -> counterexample on `HeadPassFloor`. `CycleExceedsViewport` is the
/// always-true non-vacuity control. Tier-1 binding: aterm-effects' field
/// tests drive the REAL `col_params` over the same small-grid lattice.
#[test]
fn derived_rain_ignition_proves_and_catches_dropped_flash_floor() {
    assert_proves_and_catches(&rain_ignition_model());
}

#[test]
fn derived_ligature_gate_proves_and_catches_unflagged_collapse() {
    // The M4 ligature-slicing shaping gate (aterm-render's pure
    // `ligature_shaping::classify_shape`): `ty` PROVES ConservativeAccept at
    // Buggy=0 — an accepted shape is ALWAYS grid-mappable, either 1:1 (the shipping
    // Fira/JetBrains spacer form, n_out==n_in) or an N:1 collapse (Cascadia,
    // n_out==1 && n_in>=2) AND ONLY when the `admit` flag is set — over the whole
    // bounded (n_in, n_out, admit) space, so a partial collapse, an expansion, or a
    // flag-off collapse can never reach the blitter — and CATCHES the defect at
    // Buggy=1 (a gate that drops the `admit` guard and admits a Cascadia collapse
    // WITHOUT the flag, drawing a wide glyph the raster-slicing present path is not
    // wired for) -> counterexample on ConservativeAccept. This is the conservative
    // half of M4: the N:1 tile arithmetic (slice at cell_w boundaries) is proven
    // separately by the L0 lattice `tests/ligature_slice.rs` (ty has no
    // multiplication).
    assert_proves_and_catches(&ligature_gate_model());
}

#[test]
fn derived_shared_budget_proves_and_catches_global_overrun() {
    // Module-global scrollback budget sharing (audit E1): PROVES that once
    // every live pane has applied its equal share (`min(cfg, global/live)`),
    // the applied budgets sum within the ONE global cap, across every
    // join/leave/apply interleaving — and that a departed pane holds no
    // share. CATCHES the global-less mutant (each pane applies its full
    // configured budget) as two fresh live panes overrunning the cap — the
    // exact N-panes-multiply-into-OOM class the global budget exists to
    // close. Tier-1 binding: aterm-core/tests/conformance_shared_budget.rs.
    assert_proves_and_catches(&shared_budget_model());
}

#[test]
fn derived_proxy_forward_proves_and_catches_forward_cycle() {
    // The cross-process @child proxy forward (control.rs proxy_forward_plan): `ty`
    // PROVES OneHopNoCycle at Buggy=0 — rewriting the child's selector to `@.` caps the
    // forward chain at one cross-process hop, so no A->B->A ping-pong or unbounded
    // relay-thread/fd growth can form (the structural invariant that REPLACED the
    // removed explicit hop-cap) — and CATCHES the loop class at Buggy=1 (a forward that
    // relays the original cross-selector instead of `@.`, so the child re-forwards and
    // the chain grows past one hop) -> counterexample on OneHopNoCycle. If the `@.`
    // rewrite ever regresses, this exhaustive check fails.
    assert_proves_and_catches(&proxy_forward_model());
}

#[test]
fn derived_console_life_episodes_prove_and_catch_replay_and_stale_ownership() {
    let model = aterm_spec::derive::console_life_episode_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &["StateBounded"]);
}

#[test]
fn derived_console_resident_handoff_proves_and_catches_stranded_or_perpetual_wakes() {
    let model = aterm_spec::derive::console_resident_handoff_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &["StateBounded"]);
}

/// `aterm.log` rotation across writing processes: every line written stays in
/// `aterm.log` or `aterm.log.1` until the older copy is replaced, and both files
/// stay bounded. `Buggy=1` — no look while running and a truncating start, the
/// code this replaced — must break both, each on its own. Tier-1:
/// `aterm-gui/src/logging.rs` `rotation_conformance`.
#[test]
fn derived_log_rotation_proves_and_catches_lost_lines_and_unbounded_growth() {
    let model = aterm_spec::derive::log_rotation_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &[]);

    // A writer two rotations behind cannot write before it looks: the second
    // rotation needs a tick after the first, and that tick makes A's look due.
    let mut state = model.init_state();
    for action in [
        "WriteB", "CheckB", "WriteB", "CheckB", // B fills the file and rotates it
        "WriteB", "CheckB", "Tick", "CheckB", "WriteB", "CheckB", // …and again
    ] {
        assert!(model.fire(action, &mut state), "{action} from {state:?}");
    }
    assert_eq!(state["ga"], 2, "A holds a deleted copy: {state:?}");
    assert!(!model.action_enabled("WriteA", &state), "{state:?}");
    assert!(model.fire("CheckA", &mut state));
    assert_eq!(state["ga"], 0, "A's look reopens aterm.log: {state:?}");
    assert_eq!(
        state["live"] + state["old"] + state["aged"],
        state["written"]
    );
}

/// THE MODEL LADDER: every due model move lands. The healthy ladder never
/// waits past the bound and never wedges before moving; the mutant is the
/// 2026-09-25 incident's rule (move only on a cold cache), and it walks the
/// wait past the bound on a session that simply keeps answering.
#[test]
fn derived_harness_model_ladder_lands_every_due_move_and_catches_the_cold_only_rule() {
    let model = harness_model_ladder_model();
    assert_proves_and_catches(&model);

    let moved = |state: &aterm_spec::interp::State| state["moved"] == 1;
    assert!(
        aterm_spec::interp::find_deadlock(&model, moved).is_none(),
        "the healthy ladder always reaches the move"
    );

    // The incident, on the healthy ladder: warm, a newer build arrives, the
    // very next readable visit moves.
    let warm = model.init_state();
    let restarting = model.successors("BuildArrives", &warm)[0].clone();
    assert!(
        model.successors("VisitWaits", &restarting).is_empty(),
        "a build restart must not wait for a cold cache"
    );
    assert_eq!(model.successors("VisitMoves", &restarting)[0]["moved"], 1);

    // Warm and no restart coming, forever: it waits exactly `Warm` visits and
    // then moves — never once more.
    let mut s = warm.clone();
    for _ in 0..3 {
        assert!(
            model.successors("VisitMoves", &s).is_empty(),
            "moved early: {s:?}"
        );
        s = model.successors("VisitWaits", &s)[0].clone();
    }
    assert!(
        model.successors("VisitWaits", &s).is_empty(),
        "waited past the bound"
    );
    assert_eq!(model.successors("VisitMoves", &s)[0]["moved"], 1);

    // A flicker neither moves nor resets: the clock stands across it.
    let flickered = model.successors("Flicker", &s)[0].clone();
    assert_eq!(flickered["clock"], s["clock"]);
    assert!(model.successors("VisitMoves", &flickered).is_empty());
    assert!(model.successors("VisitWaits", &flickered).is_empty());

    // THE INCIDENT, on the mutant: warm, the build restart in hand, and it
    // still waits — past the bound.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut b = buggy.successors("BuildArrives", &buggy.init_state())[0].clone();
    for _ in 0..4 {
        b = buggy.successors("VisitWaits", &b)[0].clone();
    }
    assert!(
        !buggy.check_invariant("NeverPastTheBound", &b),
        "the cold-only rule must be caught waiting past the bound: {b:?}"
    );
}

/// The model priority list: `Priority::admit` and `models set` write it,
/// target selection reads it. Every law carries its own mutant.
#[test]
fn derived_harness_model_priority_proves_and_catches_every_writer_and_reader_law() {
    let model = harness_model_priority_model();
    assert_operator_model_shape(&model, |_| false);
    assert_every_invariant_carries_a_mutant(&model, &[]);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    // The seed plus a newer Opus the build knows: it goes directly above
    // claude-opus-5-5, and, available, it is what the reader picks.
    let mut s = model.init_state();
    fire_all(
        &model,
        &mut s,
        &[
            "BuildLearnsNewerOpus",
            "AdmitNewerOpus",
            "ToggleA3",
            "Select",
        ],
    );
    assert_eq!((s["p_a3"], s["p_a2"], s["p_b1"], s["p_a1"]), (1, 2, 3, 4));
    assert_eq!(s["pick"], 1);

    // The owner ranked Fable first: the newer Opus goes above the family's
    // best (claude-opus-5), never above Fable, and an available Fable still
    // wins.
    let mut fable = model.init_state();
    fire_all(
        &model,
        &mut fable,
        &[
            "HumanSetsFableFirst",
            "BuildLearnsNewerOpus",
            "AdmitNewerOpus",
            "ToggleA3",
            "ToggleB1",
            "Select",
        ],
    );
    assert_eq!(
        (fable["p_b1"], fable["p_a3"], fable["p_a1"], fable["p_a2"]),
        (1, 2, 3, 4)
    );
    assert_eq!(fable["pick"], 1, "the owner's first choice stands");
    let mut top = model.init_state();
    fire_all(
        &model,
        &mut top,
        &["HumanSetsFableFirst", "BuildLearnsNewerOpus"],
    );
    fire_all(&buggy, &mut top, &["AdmitAtTheTop"]);
    assert!(!buggy.check_invariant("AutoInsertSitsDirectlyAboveTheFamilysBest", &top));

    // The historical newest-listed defect: with claude-opus-5 ranked above
    // claude-opus-5-5, claude-opus-5-1 is newer than the best-RANKED Opus but
    // older than the newest — admitting it extends the list downward.
    let mut hand = model.init_state();
    fire_all(
        &model,
        &mut hand,
        &["HumanSetsOlderOpusFirst", "OfferBelowNewest"],
    );
    assert_eq!(hand["p_ah"], 0, "the healthy writer refuses it");
    let mut down = model.init_state();
    fire_all(
        &buggy,
        &mut down,
        &["HumanSetsOlderOpusFirst", "OfferBelowNewest"],
    );
    assert!(!buggy.check_invariant("NeverExtendedDownward", &down));

    // An insertion that re-sorts the family rewrites the owner's order.
    let mut resort = model.init_state();
    fire_all(
        &buggy,
        &mut resort,
        &[
            "HumanSetsOlderOpusFirst",
            "BuildLearnsNewerOpus",
            "AdmitAndResort",
        ],
    );
    assert!(!buggy.check_invariant("HumanOrderIsNeverRewritten", &resort));

    // A reader that answers the head whether or not it is available.
    let mut head = buggy.init_state();
    fire_all(&buggy, &mut head, &["SelectHead"]);
    assert!(!buggy.check_invariant("ReaderPicksTheFirstAvailable", &head));

    assert_proves_and_catches(&model);
}

/// Every `(DefaultFable, PersonFlag, Build)` configuration of
/// `HarnessModelSwitch`: the person's standing settings default and launch
/// flag (none, an id, a family alias), and whether a newer build is due all
/// along.
const MODEL_SWITCH_CONFIGS: [(i64, i64, i64); 12] = [
    (0, 0, 0),
    (1, 0, 0),
    (0, 1, 0),
    (1, 1, 0),
    (0, 2, 0),
    (1, 2, 0),
    (0, 0, 1),
    (1, 0, 1),
    (0, 1, 1),
    (1, 1, 1),
    (0, 2, 1),
    (1, 2, 1),
];

/// Each `HarnessModelSwitch` mutant and the law it is checked against alone.
const MODEL_SWITCH_MUTANTS: [(&str, &str); 9] = [
    ("JudgeMovesOffList", "UpTheListOnly"),
    (
        "JudgeForgetsTheRememberedChoice",
        "PersonsChoiceStaysInFamily",
    ),
    ("JudgeRetriesAFailedModel", "NeverOntoAppliedOrFailed"),
    ("JudgeReadsAStaleAnnouncement", "NeverOntoAppliedOrFailed"),
    ("LadderWaitsForCold", "MovesExactlyWhenTheLadderSays"),
    ("LadderRetakenAfterReady", "AnnouncedModelRides"),
    ("SettleNeverFails", "SettleRecordsWhatRan"),
    ("SettleNeverVerifies", "SettleRecordsWhatRan"),
    ("SettleFailsAnAskThatRan", "AppliedAskStands"),
];

/// THE MODEL SWITCH: the live upgrade's model rule (`model_due`), ladder
/// (`model_moves_now`), announcement (`model_to`) and settle step
/// (`ModelRecord::settle`, the due clock, the ask) as one conversation's
/// lifecycle. Every law is the only law some mutant step breaks, in every
/// configuration of the person's standing choices; each mutant, alone
/// against its own law alone, is proven clean at `Buggy = 0` and caught at
/// `Buggy = 1` by the interpreter and by `ty` wherever it is installed
/// (`JudgeReadsAStaleAnnouncement` pins the DECISION half of
/// `NeverOntoAppliedOrFailed`: its verdict is right, so the law's verdict
/// half alone would not see it). Under a person's launch ALIAS
/// (`PersonFlag = 2`) the rule keeps everything (`model-alias`), so no
/// mutant's defect can show there: those configurations are proven at
/// `Buggy = 0` and shown to move nothing, ever. Tier-1: aterm-agent's
/// `conformance_upgrade_models/switch.rs`.
#[test]
fn derived_harness_model_switch_proves_and_catches_every_law() {
    let model = harness_model_switch_model();
    for (default_fable, person_flag, build) in MODEL_SWITCH_CONFIGS {
        let m = aterm_spec::interp::with_consts(
            &model,
            &[
                ("DefaultFable", default_fable),
                ("PersonFlag", person_flag),
                ("Build", build),
            ],
        );
        if person_flag == 2 {
            let healthy = aterm_spec::interp::with_buggy(&m, 0);
            let states = aterm_spec::interp::bmc(&healthy).expect("every law holds at Buggy=0");
            let key = |st: &aterm_spec::interp::State| -> Vec<(&'static str, i64)> {
                st.iter().map(|(k, v)| (*k, *v)).collect()
            };
            let mut seen = std::collections::BTreeSet::new();
            let mut queue = std::collections::VecDeque::from([healthy.init_state()]);
            while let Some(st) = queue.pop_front() {
                if !seen.insert(key(&st)) {
                    continue;
                }
                assert!(
                    st["due"] == 0 && st["mto"] == 0 && st["set"] == 0 && st["flag"] == 0,
                    "a person's launch alias was moved: {st:?}"
                );
                for action in &healthy.actions {
                    queue.extend(healthy.successors(action.name, &st));
                }
            }
            eprintln!(
                "HarnessModelSwitch DefaultFable={default_fable} PersonFlag=2 (alias) \
                 Build={build}: {states} states, every law proven (Buggy=0), nothing ever \
                 due, asked for or moved"
            );
            continue;
        }
        assert_operator_model_shape(&m, |_| false);
        assert_every_invariant_breaks_first(&m, &[]);
        let states = aterm_spec::interp::bmc(&aterm_spec::interp::with_buggy(&m, 0))
            .expect("every law holds at Buggy=0");
        eprintln!(
            "HarnessModelSwitch DefaultFable={default_fable} PersonFlag={person_flag} \
             Build={build}: {states} states, every law proven (Buggy=0) and the only law \
             some mutant step breaks (Buggy=1)"
        );
        assert_proves_and_catches(&m);
    }

    // Each mutant alone against its own law alone: the interpreter's
    // counterexample printed, and `ty` (where installed) proves the healthy
    // arm and catches the mutant.
    let mutants: Vec<&str> = MODEL_SWITCH_MUTANTS.iter().map(|(a, _)| *a).collect();
    for (mutant, law) in MODEL_SWITCH_MUTANTS {
        let mut single = model.clone();
        single
            .actions
            .retain(|a| !mutants.contains(&a.name) || a.name == mutant);
        single.invariants.retain(|inv| inv.name == law);
        let (state, broken) = aterm_spec::interp::bmc(&aterm_spec::interp::with_buggy(&single, 1))
            .expect_err("the mutant alone must break its law");
        assert_eq!(broken, law);
        eprintln!("HarnessModelSwitch: `{mutant}` is caught by `{law}` at {state:?}");
        assert_proves_and_catches(&single);
    }

    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    // The ordinary move, Opus 5 -> Opus 5.5: due on a cold cache, announced,
    // the READY answer warms the cache and the model still rides, the relaunch
    // runs it and the next visit records it applied — and nothing more is due.
    let mut s = model.init_state();
    fire_all(&model, &mut s, &["GoCold", "Visit"]);
    assert_eq!(
        (s["due"], s["mto"]),
        (1, 1),
        "due, and the cold cache takes it"
    );
    fire_all(&model, &mut s, &["Announce", "Answer", "Visit"]);
    assert_eq!(
        (s["cold"], s["ann"], s["mto"]),
        (0, 1, 1),
        "warm again, still rides"
    );
    fire_all(&model, &mut s, &["Relaunch", "Visit"]);
    assert_eq!((s["live"], s["set"], s["ap"], s["due"]), (1, 1, 1, 0));

    // THE LADDER on a warm cache: the move waits, then lands when the warm
    // wait is over.
    let mut warm = model.init_state();
    fire_all(&model, &mut warm, &["Visit"]);
    assert_eq!((warm["due"], warm["mto"]), (1, 0), "warm: it waits");
    fire_all(&model, &mut warm, &["WarmWaitElapses", "Visit"]);
    assert_eq!(warm["mto"], 1, "the warm wait is over: it moves");
    let mut incident = warm.clone();
    fire_all(&buggy, &mut incident, &["LadderWaitsForCold"]);
    assert!(!buggy.check_invariant("MovesExactlyWhenTheLadderSays", &incident));

    // SETTLE: asked for, and Claude Code runs another — recorded failed after
    // MODEL_SETTLE_S, and never due again.
    let mut other = model.init_state();
    fire_all(
        &model,
        &mut other,
        &[
            "GoCold",
            "Visit",
            "Announce",
            "Visit",
            "RelaunchRunsOther",
            "Visit",
        ],
    );
    assert_eq!(
        (other["set"], other["fl"]),
        (1, 0),
        "pending until it settles"
    );
    fire_all(&model, &mut other, &["SettleElapses", "Visit"]);
    assert_eq!((other["set"], other["fl"], other["due"]), (0, 1, 0));
    let mut never = other.clone();
    never.insert("set", 1);
    never.insert("sage", 1);
    never.insert("fl", 0);
    never.insert("tgt", 3);
    never.insert("fresh", 0);
    fire_all(&buggy, &mut never, &["SettleNeverFails"]);
    assert!(!buggy.check_invariant("SettleRecordsWhatRan", &never));

    // AN ASK THAT RAN STANDS: applied, then a person moves off it, and the
    // settle window passes — never recorded failed, the ask still the
    // harness's own. The settle step before 2026-09-27 failed it.
    let mut ran = model.init_state();
    fire_all(
        &model,
        &mut ran,
        &["GoCold", "Visit", "Announce", "Visit", "Relaunch", "Visit"],
    );
    assert_eq!((ran["set"], ran["ap"]), (1, 1), "the ask ran");
    fire_all(
        &model,
        &mut ran,
        &["PersonTypesModel", "Answer", "SettleElapses", "Visit"],
    );
    assert_eq!(
        (ran["live"], ran["set"], ran["ap"], ran["fl"], ran["due"]),
        (2, 1, 1, 0, 0)
    );
    let mut failed_it = ran.clone();
    failed_it.insert("fresh", 0);
    fire_all(&buggy, &mut failed_it, &["SettleFailsAnAskThatRan"]);
    assert!(!buggy.check_invariant("AppliedAskStands", &failed_it));

    // A person's answered `/model claude-fable-5-1`, remembered by the visit
    // that saw it, holds Fable against a cross-family move; the rule before
    // 18090b6ae forgot it at the answer.
    let mut fable = model.init_state();
    fire_all(
        &model,
        &mut fable,
        &["PersonTypesModel", "Visit", "Answer", "Visit"],
    );
    assert_eq!((fable["live"], fable["hum"], fable["due"]), (2, 1, 0));
    let mut forgot = fable.clone();
    forgot.insert("fresh", 0);
    fire_all(&buggy, &mut forgot, &["JudgeForgetsTheRememberedChoice"]);
    assert!(!buggy.check_invariant("PersonsChoiceStaysInFamily", &forgot));

    // STICKINESS: announced, then the target moves away — the announced
    // model still rides; the decision re-taken drops it.
    let mut sticky = model.init_state();
    fire_all(
        &model,
        &mut sticky,
        &["GoCold", "Visit", "Announce", "Retarget", "Visit"],
    );
    assert_eq!((sticky["tgt"], sticky["due"], sticky["mto"]), (3, 0, 1));
    let mut retaken = sticky.clone();
    retaken.insert("fresh", 0);
    fire_all(&buggy, &mut retaken, &["LadderRetakenAfterReady"]);
    assert!(!buggy.check_invariant("AnnouncedModelRides", &retaken));
}

/// OPEN — THE RIDE'S EXEMPTION IS A GAP IN THE CODE (skeptic review of
/// `HarnessModelSwitch`, 2026-09-27). `UpTheListOnly`,
/// `PersonsChoiceStaysInFamily` and `NeverOntoAppliedOrFailed` hold for the
/// rule's verdict and for every visit's decision EXCEPT while an announced
/// model rides: `upgrade_drive::visit_models` carries the upgrade's
/// `model_list` whatever the visit decides, and a gave-up upgrade's late READY
/// restarts with that `model_list` too (`restart` asks `st.model_list`). So
/// the HEALTHY model reaches, in every configuration, a relaunch about to ask
/// for `claude-opus-5-5`:
///
/// * (a) after it was recorded FAILED — both from a standing notice and from
///   a gave-up one (the latter where a newer build gives the late READY a
///   restart to make);
/// * (b) over a person's `/model claude-fable-5-1` typed after the notice,
///   across families;
/// * (c) off a model the list does not name (a person's `/model
///   claude-sonnet-5`).
///
/// This test pins each as reachable. A fix re-takes those three laws inside
/// the ride as well (the ride's model then gives way to a verdict of
/// `model-failed-before`, `model-chosen-by-hand` or `model-off-list`), and
/// flips this test into their proof.
#[test]
fn derived_harness_model_switch_open_ride_gaps_are_reachable() {
    for (default_fable, person_flag, build) in MODEL_SWITCH_CONFIGS {
        let m = aterm_spec::interp::with_buggy(
            &aterm_spec::interp::with_consts(
                &harness_model_switch_model(),
                &[
                    ("DefaultFable", default_fable),
                    ("PersonFlag", person_flag),
                    ("Build", build),
                ],
            ),
            0,
        );
        let key = |st: &aterm_spec::interp::State| -> Vec<(&'static str, i64)> {
            st.iter().map(|(k, v)| (*k, *v)).collect()
        };
        let mut seen = std::collections::BTreeSet::new();
        let mut queue = std::collections::VecDeque::from([m.init_state()]);
        let mut found = std::collections::BTreeSet::new();
        while let Some(st) = queue.pop_front() {
            if !seen.insert(key(&st)) {
                continue;
            }
            let rides = st["fresh"] == 1 && st["phase"] == 1 && st["ann"] == 1 && st["mto"] == 1;
            let relaunches = !m.successors("Relaunch", &st).is_empty();
            if rides && relaunches && st["fl"] == 1 {
                found.insert("(a) a ride asks for a model recorded failed");
            }
            if st["fresh"] == 1 && st["phase"] == 2 && st["ann"] == 1 && st["fl"] == 1 && relaunches
            {
                found.insert("(a) a late READY after a give-up asks for a model recorded failed");
            }
            if rides && relaunches && st["live"] == 2 && st["cmd"] == 1 {
                found.insert("(b) a ride moves a person's /model across families");
            }
            if rides && relaunches && st["live"] == 4 {
                found.insert("(c) a ride moves a model the list does not name");
            }
            for action in &m.actions {
                queue.extend(m.successors(action.name, &st));
            }
        }
        // A person's launch alias keeps everything: nothing is ever
        // announced with a model, so no ride exists to carry one.
        let want = if person_flag == 2 {
            0
        } else if build == 1 {
            4
        } else {
            3
        };
        assert_eq!(
            found.len(),
            want,
            "DefaultFable={default_fable} PersonFlag={person_flag} Build={build}: {found:?} — \
             a gap no longer reachable is a FIX: turn this test into the laws' proof inside \
             the ride"
        );
    }
}

/// Fire `actions` in order on `model` from `state`, each of which must be
/// enabled.
fn fire_all(model: &Model, state: &mut aterm_spec::interp::State, actions: &[&str]) {
    for action in actions {
        assert!(
            model.fire(action, state),
            "{}: `{action}` must be enabled at {state:?}",
            model.name
        );
    }
}

/// The §3.2 two-way burst mutex: proves at `Buggy = 0`, is caught at
/// `Buggy = 1`, and every named law catches a member of its own. The trace is
/// the shipped serialization: a live supernova defers a classic grant, and a
/// live classic defers a supernova grant.
#[test]
fn derived_supernova_burst_mutex_keeps_the_nova_share_funded() {
    let model = supernova_burst_mutex_model();
    assert_proves_and_catches(&model);
    assert_every_invariant_carries_a_mutant(&model, &[]);

    let mut s = model.init_state();
    assert!(model.fire("IgniteSuper", &mut s));
    assert!(
        !model.action_enabled("IgniteClassic", &s),
        "a live supernova defers classic grants"
    );
    assert!(
        !model.action_enabled("IgniteSuper", &s),
        "MAX_ACTIVE_SUPERNOVAE = 1"
    );
    assert!(model.fire("RetireSuper", &mut s));
    assert!(model.fire("IgniteClassic", &mut s));
    assert!(
        !model.action_enabled("IgniteSuper", &s),
        "a live classic defers the supernova grant (the mutex is two-way)"
    );
    assert!(model.fire("IgniteClassic", &mut s));
    assert!(model.fire("IgniteClassic", &mut s));
    assert_eq!(s["funded"], 3 * 392, "three classics fund 1176 <= 1536");
}
