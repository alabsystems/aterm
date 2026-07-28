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

// The 7 introspection models are iterated via `harness::instances()`, not named here.
use aterm_spec::derive::{
    Model, aa_edge_hardening_model, active_handle_model, asymmetric_pad_layout_model,
    capture_after_present_model, channel_bind_model, chrome_face_gate_model,
    closed_recovery_ledgers_model, coalesce_model, composite_accessibility_route_model,
    config_catalog_snapshot_model, config_file_commit_cas_model, contrast_floor_model,
    control_connection_admission_model, ct_frac_bearing_model, cursor_cat_curse_wince_model,
    cursor_cat_earn_floor_model, cursor_cat_model, cursor_cutout_clip_model, cursor_model,
    damage_to_present_model, deco_band_containment_model, deco_phase_model, done_mark_lru_model,
    dsu_quiescence_model, effect_phase_lock_model, effect_present_rebase_model,
    emacs_search_navigation_model, emacs_search_repeat_work_model, evict_full_model,
    exact_profanity_completion_model, fallback_band_clip_model, fallback_precedence_model,
    fallback_scale_clamp_model, fd_handoff_no_leak_model, flash_limiter_model,
    focus_modifier_cache_model, gpu_loss_recovery_model, gpu_loss_route_model,
    grid_translate_model, handoff_roundtrip_model, hdr_present_gate_model,
    hyperlink_scheme_cap_model, idle_deadline_model, ignition_reservation_lifecycle_model,
    ignition_reservation_rekey_model, inject_floor_model, input_release_pairing_model,
    kernel_model, key_injectivity_model, kitty_collectibles_model, kitty_flush_worker_model,
    kitty_sidecar_durability_model, ligature_gate_model, manual_config_completion_model,
    manual_config_diagnostics_lane_model, manual_config_handoff_model,
    manual_config_problem_navigation_model, mint_reachability_model, motion_policy_model,
    native_async_delivery_model, native_capture_source_model, native_close_plan_model,
    native_config_observation_handoff_model, native_config_transaction_model,
    native_control_routing_model, native_document_publication_model, native_draft_journal_model,
    native_editor_command_palette_model, native_editor_modal_model, native_editor_viewport_model,
    native_file_watch_model, native_markdown_history_model, native_markdown_viewport_model,
    native_packages_worker_model, native_recovery_interaction_model, native_reopen_ledger_model,
    native_save_intent_latch_model, native_settings_draft_close_model,
    native_settings_singleton_model, native_tab_identity_model, native_update_admission_model,
    native_update_attempt_identity_model, native_update_auto_intent_model,
    native_update_channel_scan_model, native_update_disk_transaction_model,
    native_update_hidden_output_quiet_model, native_update_menu_activation_model,
    native_update_overlap_handoff_model, native_update_status_reconciliation_model,
    native_update_worker_queue_model, native_updater_model, net_capability_grant_model,
    net_dial_after_grant_model, nova_phase_model, nyan_exit_sampling_model,
    nyan_idle_twinkle_model, nyan_jump_burst_lifecycle_model, nyan_sing_detector_model,
    nyan_terminus_admission_model, one_shot_peek_model, pad_absorption_model, pane_tree_model,
    per_window_metrics_model, predictive_echo_visibility_model, present_retry_model,
    presentation_gate_model, proxy_forward_model, rain_band_containment_model, rain_ignition_model,
    rain_lifecycle_model, read_image_seq_model, recording_model, recovery_redraw_model,
    release_channel_floor_model, release_channel_single_head_model,
    release_durable_post_intent_model, release_historical_recovery_model,
    release_journal_prefix_model, release_key_epoch_transition_model,
    release_published_identity_model, release_publisher_fence_model,
    release_yank_successor_first_model, restore_manifest_single_use_model, ring_model,
    scroll_glide_model, scrollback_maintenance_lane_model, seamless_nonce_model,
    self_governor_model, semantic_prewarm_generation_model, semantic_prewarm_handshake_model,
    semantic_prewarm_request_swap_model, serious_mode_intent_queue_model, serious_mode_model,
    session_chrome_expiry_model, session_pool_model, settings_page_scroll_model, shade_phase_model,
    shared_budget_model, snapshot_model, sparkle_identity_model, sparkle_persist_capacity_model,
    sparkle_reflow_cardinality_model, sparkle_retype_rearm_model, spawn_locale_model,
    stream_fade_gate_model, strike_selection_model, styled_run_face_model, subscribe_model,
    surface_coverage_model, tab_nav_model, tab_stop_handoff_model, tab_strip_model,
    text_blend_gate_model, tier_residency_model, title_summary_managed_endpoint_model,
    title_summary_model, title_summary_observation_scheduler_model, title_summary_runtime_model,
    title_summary_socket_owner_retry_model, top_anchored_scroll_history_model,
    trail_audio_lifecycle_model, trail_audio_start_latency_model, transact_model,
    vf_axis_clamp_model, vf_nudge_gate_model, vibrancy_contrast_model, visible_pad_crop_model,
    watcher_failure_recovery_model, watcher_latch_model, wide_center_model, window_routing_model,
};
use aterm_spec::verify;
use std::process::Command;

/// The POLICY this file states for the FOUR function-valued models it drives
/// (EvictFull, TierResidency, Recording, Coalesce): report the miss and keep
/// going. For every other model here — all scalar — the interpreter tier
/// discharges the obligation unconditionally and this is a no-op, since
/// [`verify::NotRun`] is only reachable when a function-valued model meets a
/// machine with no Trust `ty`.
///
/// Skipping rather than failing is deliberate. A hard require would make
/// `cargo test -p aterm-spec --test derived_ring_ty` — the file you iterate on
/// while editing a model — unrunnable without the Trust toolchain, and it would
/// buy no coverage: each of the four has a toolchain-free Tier-1 conformance
/// twin binding it to shipping code (`aterm-buffer`'s `conformance_evict_full`
/// and `conformance_temporal`, `aterm-core`'s `conformance_recording` and
/// `replay_corpus_probe`). What must never happen is the miss passing SILENTLY,
/// which is exactly what dropping the old `Discharge::NotRun` with a bare
/// statement did: now the `Result` makes stating a policy unskippable, and this
/// line makes the chosen one visible in the test output.
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

#[test]
fn derived_ring_spec_model_checks() {
    assert_model_checks(&ring_model());
}

#[test]
fn derived_cursor_spec_model_checks() {
    // Exercises the multi-action / UNCHANGED generation path through `ty`.
    assert_model_checks(&cursor_model());
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
/// wherever installed (see [`verify::prove_and_catch_tiered`]). The three
/// function-valued models routed here (TierResidency, Recording, Coalesce) take
/// [`tier0_or_skip`]'s skip-loudly path when `ty` is absent.
fn assert_proves_and_catches(m: &Model) {
    tier0_or_skip(verify::prove_and_catch_tiered(m, m.name));
}

#[test]
fn derived_subscribe_proves_and_catches_silent_loss() {
    assert_proves_and_catches(&subscribe_model());
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

/// A dropped surface may autonomously retry only on strictly-future deadlines
/// and only while finite episode fuel remains. PROVES the delayed/bounded train
/// and CATCHES the old immediate, non-consuming redraw loop.
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
/// `mint_sites_are_launcher_only` (the sealed `aterm_cap::Authority` constructor is
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
/// (`y + t <= cell_h`) at Buggy=0 and CATCHES the pre-fix order — top clamped
/// against the whole cell, spilling a low thick band past the bottom — at
/// Buggy=1. Bound to the shipping emitters by
/// `aterm-render/tests/deco_lines.rs::decoration_writes_stay_within_the_run_band`.
#[test]
fn derived_deco_band_containment_proves_and_catches_spill() {
    assert_proves_and_catches(&deco_band_containment_model());
}

/// W6 (TOML fallback chain): an explicit config font entry strictly outranks
/// the `$ATERM_*_FONT` env compat alias, which outranks built-in discovery.
/// `ty` PROVES `ConfigOutranksEnv` (Buggy=0) and CATCHES the inverted
/// precedence (Buggy=1 → counterexample). Bound to the shipping
/// `aterm_render::fallback_chain_order` by `aterm-render/tests/styled_faces.rs`
/// (Tier-1, presence-lattice first-element classes).
#[test]
fn derived_fallback_precedence_proves_and_catches_env_over_config() {
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
/// with wraparound, cancel restores the captured viewport, and accept retains
/// the selected match.  `RepeatWorkBounded` proves the navigation step is O(1)
/// in the number of hits; construction latency is measured separately against
/// the real search engine instead of being overclaimed as a wall-clock theorem.
#[test]
fn derived_emacs_search_navigation_proves_and_catches_leak_and_linear_repeat() {
    assert_proves_and_catches(&emacs_search_navigation_model());
}

/// Independent mutant: `ty` must catch repeat work proportional to hit count,
/// rather than relying on the navigation model's separate PTY-leak defect.
#[test]
fn derived_emacs_cached_repeat_proves_and_catches_linear_work() {
    assert_proves_and_catches(&emacs_search_repeat_work_model());
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
    assert!(model.check_invariant("BottomAbsorbsFreedPixels", &changed));
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
        })
        .expect("bounded buggy cache fixture");
    let buggy_initial = buggy.successors("ApplyInitialTop", &buggy_picked)[0].clone();
    let buggy_cached = buggy.successors("PrimeLayoutCache", &buggy_initial)[0].clone();
    let buggy_changed = buggy.successors("ApplyChangedTop", &buggy_cached)[0].clone();
    assert!(!buggy.check_invariant("ExactVerticalPadCover", &buggy_changed));
    let stale = buggy.successors("RenderWithLayoutCache", &buggy_changed)[0].clone();
    assert_eq!(stale["cache_hit"], 1);
    assert_eq!(stale["full_repaint"], 0);
    assert!(!buggy.check_invariant("LayoutChangeForcesFullRepaint", &stale));

    let oversized = buggy
        .successors("PickLayout", &buggy.init_state())
        .into_iter()
        .find(|state| state["pad"] == 2 && state["initial_request"] == 4)
        .expect("bounded oversized top fixture");
    let unbounded = buggy.successors("ApplyInitialTop", &oversized)[0].clone();
    assert!(!buggy.check_invariant("TopPadIsBounded", &unbounded));
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
        "TopIsClamped",
        "RawTransportConservesTwoPads",
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
// liveness twin, and the F1 info-flow / ordering / reply-fidelity class models) are
// now `derive::props` combinator INSTANCES, driven by ONE umbrella test over the
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
/// tiers. The 7 introspection models (dispatch/relay/registry/secrecy/ordering/
/// reply-fidelity + forward-handshake) are iterated from the ONE shared table;
/// a new property adds a row there, not a test fn here.
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
fn derived_read_image_seq_proves_and_catches_torn_read() {
    // REARCH A-3: the read_image snapshot-seq protocol — monotone seq,
    // snapshot internal-consistency (no torn read), staleness-detectable.
    // `ty` PROVES NoTornRead + SeqIsStaleOrCurrent at Buggy=0, and CATCHES the
    // torn read at Buggy=1 (a later Write leaks into the active snapshot).
    assert_proves_and_catches(&read_image_seq_model());
}

#[test]
fn derived_window_routing_proves_and_catches_missed_exit() {
    // In-process multi-window routing (GUI multi-window work): `ty` PROVES
    // ExitIffEmpty + FrontmostLive + FrontmostAllocated at Buggy=0 (closing the
    // last window exits the app; the frontmost is null iff there are no windows
    // and is never a future/reused id), and CATCHES the missed exit at Buggy=1
    // (the last close fails to exit, leaving win_count=0 with exited=0) ->
    // counterexample on ExitIffEmpty.
    assert_proves_and_catches(&window_routing_model());
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
    // TreeNonEmpty + FocusInRange at Buggy=0 — a tab's pane tree always keeps >= 1
    // leaf and the focused leaf index never leaves the renderer's `0..leaf_count-1`
    // range under ANY interleaving of Split (Cmd-D/Cmd-Shift-D) / Close (Cmd-W) over
    // the whole bounded (Cap=4) space — and CATCHES the dangling focus at Buggy=1 (a
    // Close that forgets to re-point `focused` to a surviving sibling after the leaf
    // count shrinks, so closing the focused last leaf leaves `focused = leaf_count`
    // past the new end) -> counterexample on FocusInRange. This holds the split-pane
    // feature to the same Trust bar as tabs: input + the solid cursor never route to
    // a pane that no longer exists, and the tree is never empty while the tab is open.
    assert_proves_and_catches(&pane_tree_model());
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
}

/// Native front content cannot inherit a hidden PTY target, while Owner App/Meta
/// and explicitly addressed live sessions remain independent of front focus.
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
}

/// Socket admission never exceeds its queued-plus-running worker lanes, every
/// accepted/rejected arrival is accounted once, and completion cannot fabricate
/// work. The mutant over-admits while every worker is already owned.
#[test]
fn derived_control_connection_admission_proves_and_catches_overflow() {
    let model = control_connection_admission_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let first = buggy.successors("Admit", &buggy.init_state())[0].clone();
    let full = buggy.successors("Admit", &first)[0].clone();
    let overflow = buggy.successors("Admit", &full)[0].clone();
    assert!(!buggy.check_invariant("LaneBounded", &overflow));
}

/// Native Settings has one process instance and at most one ordinary implicit
/// view per window. The mutant allocates again on repeated activation.
#[test]
fn derived_native_settings_singleton_proves_and_catches_duplicate_activation() {
    assert_proves_and_catches(&native_settings_singleton_model());
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
/// then fail closed. The mutant captures the old compositor pixels on a drop.
#[test]
fn derived_capture_after_present_proves_and_catches_stale_pixels() {
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

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mutated = buggy.successors("Mutate", &buggy.init_state())[0].clone();
    let stale = buggy.successors("Decide", &mutated)[0].clone();
    assert_eq!(stale["captured"], 1);
    assert_eq!(stale["staged"], 1);
    assert!(!buggy.check_invariant("NoStaleCapture", &stale));
}

/// A WindowServer photograph is not native-content authority: even after a
/// successful present it may be one compositor interval old. The current renderer
/// frame must be presented, geometry-validated, and stitched under OS-owned chrome.
#[test]
fn derived_native_capture_source_proves_and_catches_stale_compositor_pixels() {
    let model = native_capture_source_model();
    assert_proves_and_catches(&model);

    let presented = model.successors("MarkFramePresented", &model.init_state())[0].clone();
    let validated = model.successors("ValidateGeometry", &presented)[0].clone();
    let captured = model.successors("Decide", &validated)[0].clone();
    assert_eq!(captured["decision"], 1);
    assert_eq!(captured["renderer_bound"], 1);
    assert_eq!(captured["captured"], 1);
    assert_eq!(captured["stale_capture"], 0);

    let failed = model.successors("Decide", &model.init_state())[0].clone();
    assert_eq!(failed["decision"], 2);
    assert_eq!(failed["captured"], 0);
    assert_eq!(failed["failed"], 1);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let stale = buggy.successors("Decide", &buggy.init_state())[0].clone();
    assert_eq!(stale["captured"], 1);
    assert_eq!(stale["renderer_bound"], 0);
    assert!(!buggy.check_invariant("NoStaleCapture", &stale));
    assert!(!buggy.check_invariant("CaptureUsesRenderer", &stale));
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

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let requested = buggy.successors("Request", &buggy.init_state())[0].clone();
    let running = buggy.successors("Start", &requested)[0].clone();
    let reloaded = buggy.successors("Reload", &running)[0].clone();
    let stale_result = buggy.successors("Finish", &reloaded)[0].clone();
    let installed = buggy.successors("Decide", &stale_result)[0].clone();
    assert!(!buggy.check_invariant("CurrentResultOnly", &installed));
    assert!(!buggy.check_invariant("ReadyGenerationIsCurrent", &installed));
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
    assert_eq!(failed_closed["failed_closed"], 1);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let with_base = buggy.successors("MarkReplacedBase", &buggy.init_state())[0].clone();
    let dropped = buggy.successors("ResolveReplacement", &with_base)[0].clone();
    assert!(!buggy.check_invariant("ReplacementCarriesBase", &dropped));

    let mut mixed = buggy.init_state();
    for action in ["MarkGenerationCurrent", "MarkRendererReady"] {
        mixed = buggy.successors(action, &mixed)[0].clone();
    }
    let wrongly_installed = buggy.successors("DecideResult", &mixed)[0].clone();
    assert_eq!(wrongly_installed["decision"], 2);
    assert!(!buggy.check_invariant("DecisionMatchesIdentity", &wrongly_installed));
    assert!(!buggy.check_invariant("InstallOnlyLatestReady", &wrongly_installed));
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
    assert!(model.check_invariant("RetainedPaintIsExactOrHostSeed", &requesting_a));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let ready_b = buggy.successors("MarkReadyMismatch", &buggy.init_state())[0].clone();
    let mixed = buggy.successors("Decide", &ready_b)[0].clone();
    assert_eq!(mixed["should_cache"], 0);
    assert_eq!(mixed["active_after"], 1);
    assert!(!buggy.check_invariant("MismatchedReadyMovesToCache", &mixed));
    assert!(!buggy.check_invariant("RetainedPaintIsExactOrHostSeed", &mixed));
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
/// edits, reject same-key conflicts, make undo conditional, and reset atomically.
/// Mutants blind-overwrite a conflict or expose a half-reset file.
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

    let begun = buggy.successors("BeginManual", &buggy.init_state())[0].clone();
    let locked = buggy.successors("LockManual", &begun)[0].clone();
    let indeterminate = buggy.successors("ResolveManualIndeterminate", &locked)[0].clone();
    assert_eq!(indeterminate["manual_phase"], 5);
    assert_eq!(indeterminate["manual_committed"], 0);
    assert!(buggy.check_invariant("IndeterminateDoesNotClaimDurability", &indeterminate));
    let blind_retry = buggy.successors("RetryIndeterminate", &indeterminate)[0].clone();
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
    assert_eq!(stale_trail["nyan_generation"], 1);
    assert_eq!(stale_trail["theme_generation"], 1);
    assert_eq!(stale_trail["sparkle_generation"], 1);

    let stale_nyan = buggy.successors("AdmitStaleNyan", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("SnapshotAtomic", &stale_nyan));
    assert_eq!(stale_nyan["trail_generation"], 1);
    assert_eq!(stale_nyan["nyan_generation"], 0);

    let stale_theme = buggy.successors("AdmitStaleTheme", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("SnapshotAtomic", &stale_theme));
    assert_eq!(stale_theme["trail_generation"], 1);
    assert_eq!(stale_theme["nyan_generation"], 1);
    assert_eq!(stale_theme["theme_generation"], 0);

    let stale_sparkle = buggy.successors("AdmitStaleSparkle", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("SnapshotAtomic", &stale_sparkle));
    assert_eq!(stale_sparkle["trail_generation"], 1);
    assert_eq!(stale_sparkle["nyan_generation"], 1);
    assert_eq!(stale_sparkle["theme_generation"], 1);
    assert_eq!(stale_sparkle["sparkle_generation"], 0);
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

    let begun = buggy.successors("BeginTxn", &buggy.init_state())[0].clone();
    let concurrent = buggy.successors("OtherCommit", &begun)[0].clone();
    let stale = buggy.successors("CommitClean", &concurrent)[0].clone();
    assert!(!buggy.check_invariant("StaleTxnIsNoOp", &stale));
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
    let unsafe_return = buggy.successors("ReturnA", &claimed)[0].clone();
    assert!(!buggy.check_invariant("ReturnOnlyAfterDurableClaim", &unsafe_return));

    let writer = buggy.successors("LockWriter", &buggy.init_state())[0].clone();
    let alias = buggy.successors("ReuseFixedTemporary", &writer)[0].clone();
    assert!(!buggy.check_invariant("UniqueTemporaryNeverAliases", &alias));
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
}

/// Async completion is accepted only for its live owner/sink generation;
/// service work survives requester navigation and document results reach both
/// subscribers. Mutants deliver to focus or cancel service work with the view.
#[test]
fn derived_native_async_delivery_proves_and_catches_focus_routing() {
    let model = native_async_delivery_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let service = buggy.successors("IssueService", &buggy.init_state())[0].clone();
    let dropped = buggy.successors("NavigateView", &service)[0].clone();
    assert!(!buggy.check_invariant("ServiceOutlivesRequester", &dropped));

    let view = buggy.successors("IssueView", &buggy.init_state())[0].clone();
    let stale = buggy.successors("NavigateView", &view)[0].clone();
    let misdelivered = buggy.successors("DropStaleView", &stale)[0].clone();
    assert!(!buggy.check_invariant("IdentityAndGenerationChecked", &misdelivered));
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
    let healthy = buggy.successors("Reuse1", &launched1)[0].clone();
    let crashed = buggy.successors("Crash1", &healthy)[0].clone();
    assert!(!buggy.check_invariant("RevokedHealthIsClear", &crashed));
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
    assert!(model.check_invariant("TimeoutConsumesTheBound", &timeout));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let prematurely_failed = buggy.successors("ObserveAmbiguous", &buggy.init_state())[0].clone();
    assert_eq!(prematurely_failed["phase"], 3);
    assert!(
        !buggy.check_invariant("TransientObservationsRetry", &prematurely_failed),
        "negative control: transient ambiguity must not fail prematurely"
    );
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
}

/// A release floor is frozen as channel state, survives resume unchanged, and is
/// revalidated against a potentially newer live floor immediately before publish.
/// The exact-commit lease remains held through archive, cask, and verify and is
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

    // A covered cut keeps the same owner after visibility and through every
    // downstream release step. Only the journaled final unlock releases it.
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
    assert!(model.fire("ArchiveAfterPublish", &mut complete));
    assert_eq!(complete["lease_owned"], 1);
    assert!(model.fire("PinCask", &mut complete));
    assert_eq!(complete["lease_owned"], 1);
    assert!(model.fire("VerifyRelease", &mut complete));
    assert_eq!(complete["lease_owned"], 1);
    assert!(model.fire("Unlock", &mut complete));
    assert_eq!(complete["phase"], 9);
    assert_eq!(complete["lease_owned"], 0);
    assert!(model.check_invariant("CompletionRequiresPostPublishSteps", &complete));

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

    // Mutant 4: releasing the remote owner immediately after flip exposes the
    // archive/cask/verify suffix to a competing cut.
    let mut early_unlock = buggy.init_state();
    assert!(buggy.fire("RaiseClaim", &mut early_unlock));
    assert!(buggy.fire("Resolve", &mut early_unlock));
    assert!(buggy.fire("AcquireLease", &mut early_unlock));
    assert!(buggy.fire("ConfirmCovered", &mut early_unlock));
    assert!(buggy.fire("PublishChecked", &mut early_unlock));
    assert!(!model.action_enabled("UnlockBeforeVerification", &early_unlock));
    assert!(buggy.fire("UnlockBeforeVerification", &mut early_unlock));
    assert!(!buggy.check_invariant("CompletionRequiresPostPublishSteps", &early_unlock));
    assert!(!buggy.check_invariant("UnlockCannotBeBypassed", &early_unlock));
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
}

/// The v0.55 lost-key transition is a committed, one-use epoch—not a generic
/// rotation escape hatch. The old fingerprint remains auditable, while the repo
/// policy, embedded updater pin, signing key, and published manifest all agree on
/// one replacement key before the transition can be consumed.
#[test]
fn derived_release_key_epoch_transition_is_atomic_and_one_shot() {
    let model = release_key_epoch_transition_model();
    assert_proves_and_catches(&model);

    let mut state = model.init_state();
    assert!(model.fire("AuthorizeLostKeyEpoch", &mut state));
    assert!(model.fire("PersistOneShotEpochRecord", &mut state));
    assert_eq!(state["retired_old_fingerprint"], 1);
    assert_eq!(state["repo_current_key"], 2);
    assert!(model.fire("BuildV055WithPersistedPin", &mut state));
    assert_eq!(state["binary_pin"], 2);
    assert!(model.fire("SignV055Manifest", &mut state));
    assert_eq!(state["manifest_signing_key"], 2);
    assert_eq!(state["signature_valid"], 1);
    assert!(model.fire("PublishV055Epoch", &mut state));
    assert!(model.fire("CloseOneShotEpoch", &mut state));
    assert_eq!(state["phase"], 6);
    assert_eq!(state["epoch_consumed"], 1);
    assert_eq!(state["transition_count"], 1);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    let mut no_replacement = buggy.init_state();
    assert!(buggy.fire("RetireOldWithoutReplacement", &mut no_replacement));
    assert!(!buggy.check_invariant("RetirementIsAtomicWithReplacement", &no_replacement));

    let mut erased_evidence = buggy.init_state();
    assert!(buggy.fire("AuthorizeLostKeyEpoch", &mut erased_evidence));
    assert!(buggy.fire("PersistOneShotEpochRecord", &mut erased_evidence));
    assert!(buggy.fire("EraseRetiredKeyEvidence", &mut erased_evidence));
    assert!(!buggy.check_invariant("OldFingerprintIsNeverErased", &erased_evidence));
    assert!(!buggy.check_invariant("PersistedEpochRetainsRetiredEvidence", &erased_evidence));
    assert!(!buggy.check_invariant("HistoricalEvidenceCannotBeErased", &erased_evidence));

    let mut wrong_pin = buggy.init_state();
    assert!(buggy.fire("AuthorizeLostKeyEpoch", &mut wrong_pin));
    assert!(buggy.fire("PersistOneShotEpochRecord", &mut wrong_pin));
    assert!(buggy.fire("BuildV055WithWrongPin", &mut wrong_pin));
    assert!(!buggy.check_invariant("KeyIdentityCannotChangeSilently", &wrong_pin));

    let mut substituted_signer = buggy.init_state();
    assert!(buggy.fire("AuthorizeLostKeyEpoch", &mut substituted_signer));
    assert!(buggy.fire("PersistOneShotEpochRecord", &mut substituted_signer));
    assert!(buggy.fire("BuildV055WithPersistedPin", &mut substituted_signer));
    assert!(buggy.fire("SignWithSubstitutedKey", &mut substituted_signer));
    assert!(!buggy.check_invariant("KeyIdentityCannotChangeSilently", &substituted_signer));

    let mut unsigned = buggy.init_state();
    assert!(buggy.fire("AuthorizeLostKeyEpoch", &mut unsigned));
    assert!(buggy.fire("PersistOneShotEpochRecord", &mut unsigned));
    assert!(buggy.fire("BuildV055WithPersistedPin", &mut unsigned));
    assert!(buggy.fire("PublishUnsignedV055", &mut unsigned));
    assert!(!buggy.check_invariant("PublishedEpochUsesOneExactKey", &unsigned));
    assert!(!buggy.check_invariant("UnsignedEpochCannotPublish", &unsigned));

    let mut second_rotation = buggy.init_state();
    for action in [
        "AuthorizeLostKeyEpoch",
        "PersistOneShotEpochRecord",
        "BuildV055WithPersistedPin",
        "SignV055Manifest",
        "PublishV055Epoch",
        "CloseOneShotEpoch",
        "GenericRotateAfterClose",
    ] {
        assert!(buggy.fire(action, &mut second_rotation), "{action}");
    }
    assert!(!buggy.check_invariant("EpochIsOneShot", &second_rotation));
    assert!(!buggy.check_invariant("GenericRotationDoesNotExist", &second_rotation));
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
            ],
        ),
        Ok(5)
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

    let mut legacy_duplicate = buggy.init_state();
    assert!(buggy.fire("ObserveExactDraft", &mut legacy_duplicate));
    assert!(buggy.fire("DeleteUnknownDraft", &mut legacy_duplicate));
    assert!(!buggy.check_invariant("DraftDeletionRequiresIssuedIntent", &legacy_duplicate));
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
}

/// Metadata-only archive renames preserve every historical appcast object while
/// converging a flipped channel to one exact current head. Crash/resume retains the
/// completed rename prefix but must reacquire ownership and revalidate the journal's
/// exact tag/build. Signed channels require the current signature. Explicit mutants
/// prove stale resume, wrong-tag resume, competing ownership, head regression,
/// signature bypass, and premature finalization are all observable.
#[test]
fn derived_release_channel_single_head_proves_archive_convergence() {
    let model = release_channel_single_head_model();
    assert_proves_and_catches(&model);

    // Signed history can advance after the initial scan but before visibility.
    // The frozen cut must refuse under its still-held session; silently changing
    // signing policy/key mid-cut would make the built binary and manifest diverge.
    let mut late_signature = model.init_state();
    assert!(model.fire(
        "DetectSignaturePolicyAdvanceUnderSession",
        &mut late_signature
    ));
    assert!(!model.action_enabled("Flip", &late_signature));
    assert!(model.fire("RejectSignaturePolicyAdvance", &mut late_signature));
    assert_eq!(late_signature["phase"], 4);
    assert_eq!(late_signature["owner"], 1);
    assert_eq!(late_signature["guard_attached"], 1);
    assert!(model.fire("ExitAfterRefusal", &mut late_signature));
    assert_eq!(late_signature["owner"], 1);
    assert_eq!(late_signature["guard_attached"], 0);

    let mut state = model.init_state();
    assert_eq!(state["old_exact_manifest"], 2);
    assert_eq!(state["old_exact_signature"], 0);
    assert!(model.fire("ConfigureSignatures", &mut state));
    assert_eq!(state["old_exact_signature"], 2);
    assert!(model.fire("Flip", &mut state));
    assert_eq!(state["current_exact_manifest"], 1);
    assert_eq!(state["current_exact_signature"], 1);
    assert_eq!(state["head_build"], 2);
    assert_eq!(state["head_tag"], 2);
    assert_eq!(state["journal_tag_build"], 2);
    assert_eq!(state["old_exact_manifest"], 2);
    assert!(model.fire("BeginArchive", &mut state));

    // Complete a prefix, crash, and resume from the same journaled step. Counts
    // prove the metadata rename moved—not deleted—the two asset identities. A
    // process-local resume without reacquiring the shared owner cannot mutate.
    assert!(model.fire("RenameHistoricalManifest", &mut state));
    assert!(model.fire("RenameHistoricalSignature", &mut state));
    assert_eq!(state["old_archived_manifest"], 1);
    assert_eq!(state["old_archived_signature"], 1);
    assert!(model.fire("CrashDuringArchive", &mut state));
    assert_eq!(state["phase"], 1);
    assert_eq!(state["owner"], 1);
    assert_eq!(state["guard_attached"], 0);
    assert_eq!(state["old_archived_manifest"], 1);
    assert_eq!(state["old_archived_signature"], 1);
    assert!(!model.action_enabled("BeginArchive", &state));
    assert!(!model.action_enabled("RenameHistoricalManifest", &state));
    assert!(!model.action_enabled("AcquireCompetingOwner", &state));
    assert!(model.check_invariant("NominalCrashPreservesRemoteLease", &state));
    assert!(model.fire("ReattachJournalOwner", &mut state));
    assert!(model.fire("BeginArchive", &mut state));
    assert!(model.fire("RenameHistoricalManifest", &mut state));
    assert!(model.fire("RenameHistoricalSignature", &mut state));
    assert!(model.fire("FinalizeArchived", &mut state));
    assert_eq!(state["phase"], 3);
    assert_eq!(state["owner"], 1);
    assert_eq!(state["guard_attached"], 1);
    assert!(model.check_invariant("StableHasSingleExactHead", &state));
    assert!(model.check_invariant("HistoricalManifestNeverDeleted", &state));
    assert!(model.check_invariant("HistoricalSignatureNeverDeleted", &state));
    assert!(model.check_invariant("StablePreservesArchivedHistory", &state));
    assert!(model.check_invariant("CurrentHeadNeverRegresses", &state));

    let stable_partition = (
        state["old_exact_manifest"],
        state["old_archived_manifest"],
        state["old_exact_signature"],
        state["old_archived_signature"],
        state["current_exact_manifest"],
        state["current_exact_signature"],
        state["head_build"],
        state["head_tag"],
        state["journal_tag_build"],
    );
    assert!(model.fire("RecheckStable", &mut state));
    assert_eq!(
        stable_partition,
        (
            state["old_exact_manifest"],
            state["old_archived_manifest"],
            state["old_exact_signature"],
            state["old_archived_signature"],
            state["current_exact_manifest"],
            state["current_exact_signature"],
            state["head_build"],
            state["head_tag"],
            state["journal_tag_build"],
        ),
        "idempotent convergence must produce an empty rename plan"
    );

    // Unsigned channels carry no historical or current signature. Their manifest
    // archive still converges, proving the signature rule is conditional rather
    // than an accidentally mandatory asset.
    let mut unsigned = model.init_state();
    assert!(model.fire("Flip", &mut unsigned));
    assert_eq!(unsigned["current_exact_signature"], 0);
    assert!(model.fire("BeginArchive", &mut unsigned));
    assert!(model.fire("RenameHistoricalManifest", &mut unsigned));
    assert!(model.fire("RenameHistoricalManifest", &mut unsigned));
    assert!(model.fire("FinalizeArchived", &mut unsigned));
    assert!(model.check_invariant("StableHasSingleExactHead", &unsigned));
    assert!(model.check_invariant("StablePreservesArchivedHistory", &unsigned));

    // Source + deterministic archive target is a hard collision. Planning cannot
    // begin, and abort leaves every historical object untouched.
    let mut collision = model.init_state();
    assert!(model.fire("Flip", &mut collision));
    assert!(model.fire("ExposeCollision", &mut collision));
    assert!(!model.action_enabled("BeginArchive", &collision));
    assert!(model.fire("AbortCollision", &mut collision));
    assert_eq!(collision["phase"], 4);
    assert_eq!(collision["finalized"], 0);
    assert!(model.check_invariant("HistoricalManifestNeverDeleted", &collision));
    assert!(model.check_invariant("HistoricalSignatureNeverDeleted", &collision));

    // Configured signatures are part of the current-head identity. Losing the
    // current signature disables archive and takes the explicit refusal path.
    let mut missing_signature = model.init_state();
    assert!(model.fire("ConfigureSignatures", &mut missing_signature));
    assert!(model.fire("Flip", &mut missing_signature));
    assert!(model.fire("ObserveMissingCurrentSignature", &mut missing_signature));
    assert!(!model.action_enabled("BeginArchive", &missing_signature));
    assert!(!model.action_enabled("RenameHistoricalManifest", &missing_signature));
    assert!(model.fire("AbortMissingSignature", &mut missing_signature));
    assert_eq!(missing_signature["phase"], 4);

    // The current tag's live manifest is a second authority proof. Missing bytes or
    // any observed build other than the journal claim disables BeginArchive before
    // the first metadata rename.
    let mut missing_manifest = model.init_state();
    assert!(model.fire("Flip", &mut missing_manifest));
    assert!(model.fire("ObserveMissingCurrentManifest", &mut missing_manifest));
    assert!(!model.action_enabled("BeginArchive", &missing_manifest));
    assert!(model.fire("AbortMissingCurrentManifest", &mut missing_manifest));
    assert_eq!(missing_manifest["owner"], 1);
    assert_eq!(missing_manifest["guard_attached"], 1);
    assert!(model.fire("ExitAfterRefusal", &mut missing_manifest));
    assert_eq!(missing_manifest["owner"], 1);
    assert_eq!(missing_manifest["guard_attached"], 0);

    let mut wrong_build = model.init_state();
    assert!(model.fire("Flip", &mut wrong_build));
    assert!(model.fire("ObserveWrongCurrentBuild", &mut wrong_build));
    assert_eq!(wrong_build["journal_tag_build"], 1);
    assert!(!model.action_enabled("BeginArchive", &wrong_build));
    assert!(!model.action_enabled("RenameHistoricalManifest", &wrong_build));
    assert!(model.fire("AbortWrongCurrentBuild", &mut wrong_build));

    let mut advanced_build = model.init_state();
    assert!(model.fire("Flip", &mut advanced_build));
    assert!(model.fire("ObserveAdvancedCurrentBuild", &mut advanced_build));
    assert_eq!(advanced_build["journal_tag_build"], 3);
    assert!(!model.action_enabled("BeginArchive", &advanced_build));
    assert!(model.fire("AbortAdvancedCurrentBuild", &mut advanced_build));

    // Every unfinished pre-v3 journal fails closed. It cannot be interpreted as
    // an unleased v3 resume or reacquire mutation authority.
    let mut legacy = model.init_state();
    assert!(model.fire("LoadUnfinishedLegacyJournal", &mut legacy));
    assert!(!model.action_enabled("ReattachJournalOwner", &legacy));
    assert!(!model.action_enabled("ObserveLegacyJournalWithoutLease", &legacy));
    assert!(model.fire("RefuseLegacyJournal", &mut legacy));
    assert_eq!(legacy["phase"], 4);
    assert_eq!(legacy["owner"], 0);
    assert_eq!(legacy["guard_attached"], 0);

    let mut newer_head = model.init_state();
    for action in [
        "LoadUnfinishedLegacyJournal",
        "AcquireCompetingOwner",
        "PublishNewerHead",
        "AbortNewerHead",
    ] {
        assert!(
            model.fire(action, &mut newer_head),
            "{action}: {newer_head:?}"
        );
    }
    assert_eq!(newer_head["phase"], 4);

    let mut wrong_tag = model.init_state();
    for action in [
        "LoadUnfinishedLegacyJournal",
        "AcquireCompetingOwner",
        "ReplaceTagAtSameBuild",
        "AbortWrongTag",
    ] {
        assert!(
            model.fire(action, &mut wrong_tag),
            "{action}: {wrong_tag:?}"
        );
    }
    assert_eq!(wrong_tag["phase"], 4);

    // NEGATIVE CONTROL 1: Buggy=1 journals archive complete while historical exact
    // heads remain. Healthy disables the edge; both stable invariants catch it.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    let mut late_signature_bypass = buggy.init_state();
    assert!(buggy.fire(
        "DetectSignaturePolicyAdvanceUnderSession",
        &mut late_signature_bypass
    ));
    assert!(buggy.fire(
        "FlipBeforeSignatureRevalidation",
        &mut late_signature_bypass
    ));
    assert_eq!(late_signature_bypass["current_exact_signature"], 0);
    assert!(!buggy.check_invariant("SignedHistoryRatchetsCurrentPolicy", &late_signature_bypass));
    assert!(!buggy.check_invariant("SignatureRatchetCannotBeBypassed", &late_signature_bypass));

    let mut premature = buggy.init_state();
    assert!(buggy.fire("Flip", &mut premature));
    assert!(buggy.fire("BeginArchive", &mut premature));
    assert!(!model.action_enabled("FinalizeWithoutArchive", &premature));
    assert!(buggy.fire("FinalizeWithoutArchive", &mut premature));
    assert_eq!(premature["phase"], 3);
    assert_eq!(premature["old_exact_manifest"], 2);
    assert!(!buggy.check_invariant("StableHasSingleExactHead", &premature));
    assert!(!buggy.check_invariant("StablePreservesArchivedHistory", &premature));

    // NEGATIVE CONTROL 2: each legacy lease bypass is independently observable.
    let mut observed_unleased = buggy.init_state();
    assert!(buggy.fire("LoadUnfinishedLegacyJournal", &mut observed_unleased));
    assert!(buggy.fire("ObserveLegacyJournalWithoutLease", &mut observed_unleased));
    assert!(!buggy.check_invariant("LegacyJournalCannotResumeMutation", &observed_unleased));

    let mut acquired_unleased = buggy.init_state();
    assert!(buggy.fire("LoadUnfinishedLegacyJournal", &mut acquired_unleased));
    assert!(buggy.fire("AcquireJournalOwner", &mut acquired_unleased));
    assert!(!buggy.check_invariant("LegacyJournalCannotResumeMutation", &acquired_unleased));

    // A stale journal cannot enter archive after a newer head wins the crash
    // handoff; the mutant attempts mutation directly from the unleased state.
    let mut stale = buggy.init_state();
    assert!(buggy.fire("LoadUnfinishedLegacyJournal", &mut stale));
    assert!(buggy.fire("AcquireCompetingOwner", &mut stale));
    assert!(buggy.fire("PublishNewerHead", &mut stale));
    assert!(!model.action_enabled("BeginArchiveStaleHead", &stale));
    assert!(buggy.fire("BeginArchiveStaleHead", &mut stale));
    assert!(!buggy.check_invariant("ArchiveUsesExactJournalHead", &stale));
    assert!(!buggy.check_invariant("StaleHeadCannotBeBypassed", &stale));

    // NEGATIVE CONTROL 3: exact build with the wrong tag is independently caught.
    let mut stale_tag = buggy.init_state();
    assert!(buggy.fire("LoadUnfinishedLegacyJournal", &mut stale_tag));
    assert!(buggy.fire("AcquireCompetingOwner", &mut stale_tag));
    assert!(buggy.fire("ReplaceTagAtSameBuild", &mut stale_tag));
    assert!(buggy.fire("BeginArchiveWrongTag", &mut stale_tag));
    assert!(!buggy.check_invariant("ArchiveUsesExactJournalHead", &stale_tag));
    assert!(!buggy.check_invariant("StaleHeadCannotBeBypassed", &stale_tag));

    // NEGATIVE CONTROL 4: configured signing cannot be bypassed at archive entry.
    let mut unsigned_bug = buggy.init_state();
    assert!(buggy.fire("ConfigureSignatures", &mut unsigned_bug));
    assert!(buggy.fire("Flip", &mut unsigned_bug));
    assert!(buggy.fire("ObserveMissingCurrentSignature", &mut unsigned_bug));
    assert!(buggy.fire("BeginArchiveMissingSignature", &mut unsigned_bug));
    assert!(!buggy.check_invariant("ConfiguredSignatureRequiredForArchive", &unsigned_bug));
    assert!(!buggy.check_invariant("SignaturePolicyCannotBeBypassed", &unsigned_bug));

    // NEGATIVE CONTROL 4b: signed historical metadata is itself a monotonic
    // channel policy. A caller-local false cannot discard that observation;
    // healthy Flip is disabled, while the mutant is caught immediately.
    let mut dropped_ratchet = buggy.init_state();
    assert!(buggy.fire("IgnoreSignedHistory", &mut dropped_ratchet));
    assert!(buggy.check_invariant("HistoricalSignatureNeverDeleted", &dropped_ratchet));
    assert!(!buggy.check_invariant("SignedHistoryRatchetsCurrentPolicy", &dropped_ratchet));
    assert!(!buggy.check_invariant("SignatureRatchetCannotBeBypassed", &dropped_ratchet));
    assert!(!model.action_enabled("Flip", &dropped_ratchet));

    // NEGATIVE CONTROL 5: a mismatched observed build cannot bypass the production
    // full validate_live_release_identity guard.
    let mut wrong_observed_build = buggy.init_state();
    assert!(buggy.fire("Flip", &mut wrong_observed_build));
    assert!(buggy.fire("ObserveWrongCurrentBuild", &mut wrong_observed_build));
    assert!(buggy.fire("BeginArchiveWrongObservedBuild", &mut wrong_observed_build));
    assert!(!buggy.check_invariant("ArchiveObservedExactJournalBuild", &wrong_observed_build));
    assert!(!buggy.check_invariant("ObservedBuildGuardCannotBeBypassed", &wrong_observed_build));

    let mut advanced_observed_build = buggy.init_state();
    assert!(buggy.fire("Flip", &mut advanced_observed_build));
    assert!(buggy.fire("ObserveAdvancedCurrentBuild", &mut advanced_observed_build));
    assert!(buggy.fire(
        "BeginArchiveAdvancedObservedBuild",
        &mut advanced_observed_build
    ));
    assert!(!buggy.check_invariant("ArchiveObservedExactJournalBuild", &advanced_observed_build));

    // NEGATIVE CONTROL 5b: matching tag/build alone is insufficient when the
    // live manifest's version/commit/DMG/bytes or signed identity drifted.
    let mut invalid_live_identity = buggy.init_state();
    assert!(buggy.fire("Flip", &mut invalid_live_identity));
    assert!(buggy.fire("ObserveLiveIdentityMismatch", &mut invalid_live_identity));
    assert!(buggy.fire(
        "BeginArchiveInvalidLiveIdentity",
        &mut invalid_live_identity
    ));
    assert!(!buggy.check_invariant("ArchiveUsesValidatedLiveIdentity", &invalid_live_identity));
    assert!(!buggy.check_invariant("LiveIdentityGuardCannotBeBypassed", &invalid_live_identity));

    // NEGATIVE CONTROL 6: a competing owner cannot enter archive, nor can it
    // advance the current head after this journal began mutating history.
    let mut wrong_owner = buggy.init_state();
    assert!(buggy.fire("LoadUnfinishedLegacyJournal", &mut wrong_owner));
    assert!(buggy.fire("ObserveLegacyJournalWithoutLease", &mut wrong_owner));
    assert!(buggy.fire("AcquireCompetingOwner", &mut wrong_owner));
    assert!(buggy.fire("BeginArchiveAsCompetingOwner", &mut wrong_owner));
    assert!(!buggy.check_invariant("ArchiveOwnsSharedLease", &wrong_owner));
    assert!(!buggy.check_invariant("CompetingOwnerCannotBypassLease", &wrong_owner));

    let mut advanced_during_archive = buggy.init_state();
    assert!(buggy.fire("Flip", &mut advanced_during_archive));
    assert!(buggy.fire("BeginArchive", &mut advanced_during_archive));
    assert!(buggy.fire(
        "CompetingOwnerAdvancesDuringArchive",
        &mut advanced_during_archive
    ));
    assert!(!buggy.check_invariant("ArchiveHeadIsImmutable", &advanced_during_archive));
    assert!(!buggy.check_invariant("CompetingOwnerCannotBypassLease", &advanced_during_archive));

    // NEGATIVE CONTROL 7: current channel generations never move backward.
    let mut regressed = buggy.init_state();
    assert!(buggy.fire("LoadUnfinishedLegacyJournal", &mut regressed));
    assert!(buggy.fire("ObserveLegacyJournalWithoutLease", &mut regressed));
    assert!(buggy.fire("AcquireCompetingOwner", &mut regressed));
    assert!(buggy.fire("RegressCurrentHead", &mut regressed));
    assert!(!buggy.check_invariant("CurrentHeadNeverRegresses", &regressed));

    // NEGATIVE CONTROL 8: delete+recreate can preserve scalar counts while
    // replacing the asset object/bytes. Identity invariants catch both asset
    // classes independently.
    let mut replaced_manifest = buggy.init_state();
    assert!(buggy.fire("Flip", &mut replaced_manifest));
    assert!(buggy.fire("BeginArchive", &mut replaced_manifest));
    assert!(buggy.fire(
        "DeleteAndRecreateHistoricalManifest",
        &mut replaced_manifest
    ));
    assert!(buggy.check_invariant("HistoricalManifestNeverDeleted", &replaced_manifest));
    assert!(!buggy.check_invariant("HistoricalManifestIdentityPreserved", &replaced_manifest));

    let mut replaced_signature = buggy.init_state();
    assert!(buggy.fire("ConfigureSignatures", &mut replaced_signature));
    assert!(buggy.fire("Flip", &mut replaced_signature));
    assert!(buggy.fire("BeginArchive", &mut replaced_signature));
    assert!(buggy.fire(
        "DeleteAndRecreateHistoricalSignature",
        &mut replaced_signature
    ));
    assert!(buggy.check_invariant("HistoricalSignatureNeverDeleted", &replaced_signature));
    assert!(!buggy.check_invariant("HistoricalSignatureIdentityPreserved", &replaced_signature));
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
        "ReleaseKeyEpochTransition",
        "ReleasePublishedIdentity",
        "ReleaseYankSuccessorFirst",
        "ReleaseChannelSingleHead",
        "NativeUpdateChannelScan",
        "NativeUpdateHiddenOutputQuiet",
    ] {
        assert!(
            registered.contains(expected),
            "{expected} must resolve through the spec↔source registry"
        );
    }
}

/// GitHub release rows are unordered. The complete metadata catalog is arbitrated by
/// canonical numeric tag, so every permutation of v0.8/v0.9/v0.10 selects v0.10.
/// Strictly-lower numeric multi-part legacy tags remain migration-compatible, while a
/// same/newer noncanonical maximum refuses. Only the canonical authority's manifest
/// (and, when configured, signature) is fetched. An older 503, malformed/duplicate
/// metadata, authoritative rejection, signature failure, or a missing/duplicate/
/// noncanonical authoritative DMG identity can never cause fallback.
///
/// Production's `select_authoritative_release` and `fetch_authoritative_release`
/// seams use the same two-phase split. Their local fixtures enumerate all six
/// permutations and count transport calls; this derived test exhaustively checks the
/// bounded transition system and explicit row-order/over-fetch/fallback mutants.
#[test]
fn derived_native_update_channel_scan_proves_permutation_invariance_and_one_fetch() {
    let model = native_update_channel_scan_model();
    assert_proves_and_catches(&model);

    let orders = [
        ["ObserveMinor8", "ObserveMinor9", "ObserveMinor10"],
        ["ObserveMinor8", "ObserveMinor10", "ObserveMinor9"],
        ["ObserveMinor9", "ObserveMinor8", "ObserveMinor10"],
        ["ObserveMinor9", "ObserveMinor10", "ObserveMinor8"],
        ["ObserveMinor10", "ObserveMinor8", "ObserveMinor9"],
        ["ObserveMinor10", "ObserveMinor9", "ObserveMinor8"],
    ];
    for order in orders {
        let mut state = model.init_state();
        for action in order {
            assert!(model.fire(action, &mut state));
            assert!(model.check_invariant("CatalogMaximumIsNumericAndOrderIndependent", &state));
        }
        assert_eq!(state["max_minor"], 10, "numeric 9 must sort below 10");
        assert!(model.fire("CompleteMetadataArbitration", &mut state));
        assert_eq!(state["selected_minor"], 10);
        assert!(model.fire("ExposeOlderUnreadable", &mut state));
        assert!(model.fire("FetchAuthoritativeVerified", &mut state));
        assert_eq!(state["manifest_fetch_count"], 1);
        assert_eq!(state["fetched_minor"], 10);
        assert_eq!(state["older_manifest_fetch_count"], 0);
        assert!(model.check_invariant("ManifestFetchBudgetIsOne", &state));
        assert!(model.check_invariant("OlderUnreadableIsNeverFetched", &state));
        assert!(model.fire("FinalizeAccepted", &mut state));
        assert!(model.check_invariant("AcceptedUsesAuthoritativeRelease", &state));
    }

    // A lower numeric multi-part migration tag can appear before or after the
    // canonical rows without changing authority or adding a transport call.
    for legacy_first in [false, true] {
        let mut state = model.init_state();
        if legacy_first {
            assert!(model.fire("ObserveLowerLegacy", &mut state));
        }
        for action in orders[4] {
            assert!(model.fire(action, &mut state));
        }
        if !legacy_first {
            assert!(model.fire("ObserveLowerLegacy", &mut state));
        }
        assert_eq!(state["max_minor"], 10);
        assert!(model.fire("CompleteMetadataArbitration", &mut state));
        assert_eq!(state["selected_minor"], 10);
    }

    // A same-prefix newer numeric vector (v0.10.1) wins ordering but is not a
    // canonical two-component authority, so refusal happens before fetch.
    let mut noncanonical = model.init_state();
    for action in orders[0] {
        assert!(model.fire(action, &mut noncanonical));
    }
    assert!(model.fire("ObserveNewerNoncanonical", &mut noncanonical));
    assert!(!model.action_enabled("CompleteMetadataArbitration", &noncanonical));
    assert!(model.fire("RefuseNoncanonicalAuthority", &mut noncanonical));
    assert_eq!(noncanonical["phase"], 3);
    assert_eq!(noncanonical["manifest_fetch_count"], 0);
    assert!(model.check_invariant("NoncanonicalMaximumCannotSelect", &noncanonical));

    // A pinned channel fetches exactly the selected authority's manifest and one
    // detached signature. The signature remains subordinate to the manifest budget.
    let mut signed = model.init_state();
    assert!(model.fire("ConfigureSignatures", &mut signed));
    for action in orders[5] {
        assert!(model.fire(action, &mut signed));
    }
    assert!(model.fire("CompleteMetadataArbitration", &mut signed));
    assert!(model.fire("FetchAuthoritativeVerified", &mut signed));
    assert_eq!(signed["manifest_fetch_count"], 1);
    assert_eq!(signed["signature_fetch_count"], 1);
    assert!(model.check_invariant("VerifiedFetchHonorsSignaturePolicy", &signed));

    let mut signature_unreadable = model.init_state();
    assert!(model.fire("ConfigureSignatures", &mut signature_unreadable));
    for action in orders[1] {
        assert!(model.fire(action, &mut signature_unreadable));
    }
    assert!(model.fire("CompleteMetadataArbitration", &mut signature_unreadable));
    assert!(model.fire(
        "FetchAuthoritativeSignatureUnreadable",
        &mut signature_unreadable
    ));
    assert_eq!(signature_unreadable["manifest_fetch_count"], 1);
    assert_eq!(signature_unreadable["signature_fetch_count"], 1);
    assert_eq!(signature_unreadable["phase"], 3);
    assert!(model.check_invariant("RefusalIsTerminalForThisCheck", &signature_unreadable));

    // Malformed or duplicate candidate metadata is terminal before any download.
    for bad_action in [
        "ObserveMalformedCandidate",
        "ObserveDuplicateCanonicalCandidate",
    ] {
        let mut bad_metadata = model.init_state();
        assert!(model.fire(bad_action, &mut bad_metadata));
        assert!(model.fire("RefuseMetadata", &mut bad_metadata));
        assert_eq!(bad_metadata["phase"], 3);
        assert_eq!(bad_metadata["manifest_fetch_count"], 0);
        assert!(model.check_invariant("MetadataFailureFetchesNothing", &bad_metadata));
    }

    // Missing or ambiguous signature on the numeric maximum does not fall back to
    // the older signed release and never starts manifest transport.
    for signature_failure in [
        "ObserveMissingAuthoritativeSignature",
        "ObserveAmbiguousAuthoritativeSignature",
    ] {
        let mut missing_signature = model.init_state();
        assert!(model.fire("ConfigureSignatures", &mut missing_signature));
        for action in orders[0] {
            assert!(model.fire(action, &mut missing_signature));
        }
        assert!(model.fire("CompleteMetadataArbitration", &mut missing_signature));
        assert!(model.fire(signature_failure, &mut missing_signature));
        assert!(!model.action_enabled("FetchAuthoritativeVerified", &missing_signature));
        assert!(model.fire("RefuseSignaturePolicy", &mut missing_signature));
        assert_eq!(missing_signature["manifest_fetch_count"], 0);
        assert_eq!(missing_signature["accepted"], 0);
    }

    // Authoritative 503 or tag/manifest mismatch is one attempted authoritative
    // fetch followed by refusal. No lower release is probed.
    for failure in [
        "FetchAuthoritativeUnreadable",
        "RejectAuthoritativeManifest",
    ] {
        let mut refused = model.init_state();
        for action in orders[3] {
            assert!(model.fire(action, &mut refused));
        }
        assert!(model.fire("CompleteMetadataArbitration", &mut refused));
        assert!(model.fire(failure, &mut refused));
        assert_eq!(refused["phase"], 3);
        assert_eq!(refused["manifest_fetch_count"], 1);
        assert_eq!(refused["fetched_minor"], 10);
        assert_eq!(refused["older_manifest_fetch_count"], 0);
        assert!(model.check_invariant("RefusalIsTerminalForThisCheck", &refused));
        assert!(model.check_invariant("AuthorityFailureNeverFallsBack", &refused));
    }

    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    // NEGATIVE CONTROL 1: trusting row order can overwrite v0.10 with any later,
    // numerically older migration row.
    for (mutant, regressed_to) in [
        ("ObserveLowerLegacyByRowOrder", 5),
        ("ObserveMinor8ByRowOrder", 8),
        ("ObserveMinor9ByRowOrder", 9),
    ] {
        let mut row_order = buggy.init_state();
        assert!(buggy.fire("ObserveMinor10", &mut row_order));
        assert!(buggy.fire(mutant, &mut row_order));
        assert_eq!(row_order["max_minor"], regressed_to);
        assert!(!buggy.check_invariant("CatalogMaximumIsNumericAndOrderIndependent", &row_order));
    }

    // NEGATIVE CONTROL 2: a partial first page cannot become authoritative.
    let mut early = buggy.init_state();
    assert!(buggy.fire("ObserveMinor8", &mut early));
    assert!(buggy.fire("SelectBeforeCatalogComplete", &mut early));
    assert!(!buggy.check_invariant("SelectionWaitsForCompleteCatalog", &early));
    assert!(!buggy.check_invariant("EnumerationCannotBeBypassed", &early));

    // NEGATIVE CONTROL 3: a noncanonical maximum cannot be accepted merely because
    // its numeric vector is newest.
    let mut noncanonical_bug = buggy.init_state();
    for action in orders[0] {
        assert!(buggy.fire(action, &mut noncanonical_bug));
    }
    assert!(buggy.fire("ObserveNewerNoncanonical", &mut noncanonical_bug));
    assert!(buggy.fire("AcceptNoncanonicalAuthority", &mut noncanonical_bug));
    assert!(!buggy.check_invariant("NoncanonicalMaximumCannotSelect", &noncanonical_bug));
    assert!(!buggy.check_invariant("NoncanonicalAuthorityCannotBeBypassed", &noncanonical_bug));

    // NEGATIVE CONTROL 4: fetching an older 503 after the verified authority breaks
    // both the one-manifest budget and the no-historical-fetch obligation.
    let mut old_503 = buggy.init_state();
    for action in orders[0] {
        assert!(buggy.fire(action, &mut old_503));
    }
    assert!(buggy.fire("CompleteMetadataArbitration", &mut old_503));
    assert!(buggy.fire("ExposeOlderUnreadable", &mut old_503));
    assert!(buggy.fire("FetchAuthoritativeVerified", &mut old_503));
    assert!(buggy.fire("FetchOlderUnreadable", &mut old_503));
    assert_eq!(old_503["manifest_fetch_count"], 2);
    assert!(!buggy.check_invariant("ManifestFetchBudgetIsOne", &old_503));
    assert!(!buggy.check_invariant("OlderUnreadableIsNeverFetched", &old_503));

    // NEGATIVE CONTROL 5: failure of the authority never authorizes v0.9 fallback.
    let mut fallback = buggy.init_state();
    for action in orders[0] {
        assert!(buggy.fire(action, &mut fallback));
    }
    assert!(buggy.fire("CompleteMetadataArbitration", &mut fallback));
    assert!(buggy.fire("FetchAuthoritativeUnreadable", &mut fallback));
    assert!(buggy.fire("FallbackAfterFetchFailure", &mut fallback));
    assert_eq!(fallback["fetched_minor"], 9);
    assert!(!buggy.check_invariant("AuthorityFailureNeverFallsBack", &fallback));
    assert!(!buggy.check_invariant("AcceptedUsesAuthoritativeRelease", &fallback));

    // NEGATIVE CONTROL 6: a fetched-but-rejected manifest/signature is equally
    // authoritative and cannot authorize an older candidate.
    let mut reject_fallback = buggy.init_state();
    for action in orders[2] {
        assert!(buggy.fire(action, &mut reject_fallback));
    }
    assert!(buggy.fire("CompleteMetadataArbitration", &mut reject_fallback));
    assert!(buggy.fire("RejectAuthoritativeManifest", &mut reject_fallback));
    assert!(buggy.fire("FallbackAfterManifestReject", &mut reject_fallback));
    assert!(!buggy.check_invariant("AuthorityFailureNeverFallsBack", &reject_fallback));

    // NEGATIVE CONTROL 7: missing/ambiguous signature policy refuses before
    // transport and may never be repaired by probing an older signed row.
    let mut signature_fallback = buggy.init_state();
    assert!(buggy.fire("ConfigureSignatures", &mut signature_fallback));
    for action in orders[4] {
        assert!(buggy.fire(action, &mut signature_fallback));
    }
    assert!(buggy.fire("CompleteMetadataArbitration", &mut signature_fallback));
    assert!(buggy.fire(
        "ObserveMissingAuthoritativeSignature",
        &mut signature_fallback
    ));
    assert!(buggy.fire("RefuseSignaturePolicy", &mut signature_fallback));
    assert!(buggy.fire("FallbackAfterSignatureRefusal", &mut signature_fallback));
    assert!(!buggy.check_invariant("AuthorityFailureNeverFallsBack", &signature_fallback));
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
    let blocked = buggy.successors("ClassifySeamless", &foreground)[0].clone();
    assert!(!buggy.check_invariant("ForegroundJobsDoNotBlockSeamless", &blocked));
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
    let lost = buggy.successors("StageWakeDuringCheck", &checking)[0].clone();
    assert!(!buggy.check_invariant("StageDuringCheckRetainsIntent", &lost));

    let newer = buggy.successors("ArmNewerIntent", &buggy.init_state())[0].clone();
    let stale = buggy.successors("ObserveStaleWake", &newer)[0].clone();
    assert!(!buggy.check_invariant("NewerIntentSurvivesStaleWake", &stale));
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
        "QuietEpochElapses",
    ] {
        assert!(buggy.fire(action, &mut stuck), "mutant trace: {action}");
    }
    assert!(!buggy.check_invariant("OldHiddenPresentationCannotGate", &stuck));
    assert!(!buggy.check_invariant("ActivityRetryIsStrictlyFuture", &stuck));
}

#[test]
fn derived_native_update_attempt_identity_proves_and_catches_stale_abort() {
    let model = native_update_attempt_identity_model();
    assert_proves_and_catches(&model);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let first = buggy.successors("StartAttempt", &buggy.init_state())[0].clone();
    let retryable = buggy.successors("AbortCurrent", &first)[0].clone();
    let retry = buggy.successors("StartAttempt", &retryable)[0].clone();
    let canceled = buggy.successors("ReplayOldAbort", &retry)[0].clone();
    assert!(!buggy.check_invariant("StaleAbortCannotCancelRetry", &canceled));
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
    let polluted_idle = buggy.successors("ParkIdle", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("IdleParkHasNoProxyWake", &polluted_idle));
    assert!(!buggy.check_invariant("IdleParkHasNoWarning", &polluted_idle));
    let occupied = buggy.successors("OccupyWorker", &buggy.init_state())[0].clone();
    let silently_lost = buggy.successors("RequestStageFull", &occupied)[0].clone();
    assert!(!buggy.check_invariant("NoSilentlyLostAcceptedIntent", &silently_lost));

    // Pin the independent lost-wake mutation: retain the healthy pending latch,
    // then let the buggy worker dequeue omit its drain edge.
    let occupied = model.successors("OccupyWorker", &model.init_state())[0].clone();
    let pending = model.successors("RequestStageFull", &occupied)[0].clone();
    let lost_edge = buggy.successors("WorkerDrainsFiller", &pending)[0].clone();
    assert!(!buggy.check_invariant("PendingEmptyQueueHasRetryEdge", &lost_edge));

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
    let buggy_picked = buggy
        .successors("PickStatusInputs", &buggy.init_state())
        .into_iter()
        .find(|state| {
            state["running_build"] == 2
                && state["ledger_build"] == 1
                && state["ready_present"] == 0
                && state["persisted_staged_claim"] == 1
        })
        .expect("bounded buggy stale-ledger fixture");
    let stale = buggy.successors("ReconcileStatus", &buggy_picked)[0].clone();
    assert!(!buggy.check_invariant("CallerBuildIsAuthoritative", &stale));
    assert!(!buggy.check_invariant("AbsentReadyCannotAdvertiseStage", &stale));
    assert!(!buggy.check_invariant("MismatchedAbsentReadyIsNeutralized", &stale));
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
    assert!(model.check_invariant("IdlePauseDisarmsDeadline", &lifecycle));

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
        "IdleIsCallbackAndWakeFree",
    ] {
        assert!(model.check_invariant(invariant, &state), "{invariant}");
    }

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
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
    assert_eq!(delayed["available"], 0, "pause retained queue ownership");
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
}

#[test]
fn derived_top_anchored_scroll_proves_history_retention() {
    let model = top_anchored_scroll_history_model();
    assert_proves_and_catches(&model);

    for (choice, expected_history) in [
        ("ChooseArchival", 1),
        ("ChooseInterior", 0),
        ("ChooseMargined", 0),
        ("ChooseEphemeral", 0),
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

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut dropped = buggy.init_state();
    assert!(buggy.fire("ChooseArchival", &mut dropped));
    assert!(buggy.fire("Scroll", &mut dropped));
    assert!(!buggy.check_invariant("EligibleDisplacementIsRetained", &dropped));
    assert!(!buggy.check_invariant("FixedFooterAnchorTracksLogicalInsertion", &dropped));
    assert!(!buggy.check_invariant("EligibleSelectionUsesPiecewiseRemap", &dropped));
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
fn derived_font_theme_generation_reprepares_overtaken_config() {
    let model = aterm_spec::derive::font_theme_generation_model();
    assert_proves_and_catches(&model);
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
#[test]
fn derived_native_update_overlap_handoff_proves_and_catches_ownership_regressions() {
    let model = native_update_overlap_handoff_model();
    assert_proves_and_catches(&model);

    // Regression for the global xref gate's strict-vacuity pass: once one
    // admission fact rejects the handoff, later independent revocations must
    // not create a power set of semantically equivalent refusal states.  Keep
    // this focused check here so a state-space regression fails in seconds,
    // rather than several minutes into `aterm-gui::spec_xref_closure`.
    const EXPECTED_DEAD: [&str; 6] = [
        "CommitWithoutFreshExactProof",
        "AckInexactLegacyBridge",
        "BuggyReleaseReadersOnProof",
        "BuggyResumeParentBeforeReap",
        "BuggyWaitBeforeGroupSignal",
        "BuggyKillAfterCommitWin",
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
    // and both ready + receipt are absent. Exact sealed/trial/rollback disk
    // evidence synthesizes the receipt before the boot-health verdict. The same
    // transition remains available after the old parent crashed before writing
    // its re-exec stamp, because this precise no-ready shape identifies the old
    // protocol while modern swaps retain ready until receipt commit.
    let legacy = model.successors("EnterLegacyPostSwapExact", &model.init_state())[0].clone();
    assert_eq!(legacy.get("installed"), Some(&2));
    assert_eq!(legacy.get("fixed"), Some(&1));
    assert_eq!(legacy.get("trial"), Some(&1));
    assert_eq!(legacy.get("receipt"), Some(&0));
    assert_eq!(legacy.get("ready_present"), Some(&0));
    let consumed = model.successors("ConsumeStartupAuthority", &legacy)[0].clone();
    let migrated = model.successors("SynthesizeAuthenticatedLegacyReceipt", &consumed)[0].clone();
    let observed_legacy = model.successors("ObserveBootHealth", &migrated)[0].clone();
    let returned_legacy =
        model.successors("ReturnAfterRecoveredLegacy", &observed_legacy)[0].clone();
    assert_eq!(returned_legacy.get("receipt_exact"), Some(&1));
    assert_eq!(returned_legacy.get("rollback_verified"), Some(&1));

    let no_stamp = model.successors("LoseLegacyReexecAuthority", &legacy)[0].clone();
    let consumed = model.successors("ConsumeStartupAuthority", &no_stamp)[0].clone();
    let migrated = model.successors("SynthesizeAuthenticatedLegacyReceipt", &consumed)[0].clone();
    let observed = model.successors("ObserveBootHealth", &migrated)[0].clone();
    let returned = model.successors("ReturnAfterRecoveredLegacy", &observed)[0].clone();
    assert_eq!(returned.get("startup_returned"), Some(&1));

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
        (
            "CorruptLegacyShortCommit",
            "RefuseLegacyShortCommitMismatch",
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
                .successors("SynthesizeAuthenticatedLegacyReceipt", &consumed)
                .is_empty(),
            "{corrupt} still authorized receipt synthesis"
        );
        let refused = model.successors(refuse, &consumed)[0].clone();
        assert_eq!(refused.get("startup_deferred"), Some(&1));
        assert_eq!(refused.get("trial"), Some(&1));
        assert_eq!(refused.get("fixed"), Some(&1));
        assert_eq!(refused.get("receipt"), Some(&0));
    }

    // Modern recovery is separate: a surviving exact ready record takes the
    // existing full-commit route and never sets the legacy migration bit.
    let legacy = model.successors("EnterLegacyPostSwapExact", &model.init_state())[0].clone();
    let modern = model.successors("SupplyModernReadyRecovery", &legacy)[0].clone();
    let consumed = model.successors("ConsumeStartupAuthority", &modern)[0].clone();
    assert!(
        model
            .successors("SynthesizeAuthenticatedLegacyReceipt", &consumed)
            .is_empty()
    );
    let recovered = model.successors("RecoverModernReceiptFromReady", &consumed)[0].clone();
    assert_eq!(recovered.get("modern_receipt_recovered"), Some(&1));
    assert_eq!(recovered.get("legacy_receipt_migrated"), Some(&0));

    let buggy_legacy = buggy.successors("EnterLegacyPostSwapExact", &buggy.init_state())[0].clone();
    let wrong_commit = buggy.successors("CorruptLegacyCurrentCommit", &buggy_legacy)[0].clone();
    let consumed = buggy.successors("ConsumeStartupAuthority", &wrong_commit)[0].clone();
    let forged = buggy.successors("SynthesizeLegacyReceiptFromInexactShape", &consumed)[0].clone();
    assert!(!buggy.check_invariant("LegacySynthesisUsesExactDiskShape", &forged));

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

/// The cursor-trail master owns only ordinary Nyan momentum. With the master
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

    let mut healthy = model.init_state();
    for _ in 0..4 {
        assert!(model.fire("Complete", &mut healthy));
    }
    assert_eq!(healthy.get("winces"), Some(&4));
    assert_eq!(healthy.get("chain"), Some(&4));
}

/// FULL-NYAN is earned by a deliberate held key, not a short burst: the
/// committed detector arms exactly on press sixteen, releases through a bounded
/// wind-down, and the original eight-press threshold is a required
/// counterexample rather than a dead configuration dial.
#[test]
fn derived_nyan_sing_detector_proves_and_catches_eight_press_arm() {
    assert_proves_and_catches(&nyan_sing_detector_model());
}

/// The singing momentum bypass cannot make the cursor companion skip its own
/// travel floor. The committed model stays hidden through event fifteen; the
/// v0.56 ten-event floor must violate `NoCatBeforeSixteen` at Buggy=1.
#[test]
fn derived_cursor_cat_earn_floor_proves_and_catches_v056_threshold() {
    assert_proves_and_catches(&cursor_cat_earn_floor_model());
}

/// Fast-jump starbursts retain the newest item under their FIFO cap, move into
/// outgoing style ownership without loss, and keep the brisk scheduler armed
/// exactly while either owner contains work.
#[test]
fn derived_nyan_jump_burst_lifecycle_proves_and_catches_drop_or_loss() {
    assert_proves_and_catches(&nyan_jump_burst_lifecycle_model());
}

/// Terminus twinkles are admitted only for a live, full-motion jump (or the
/// live right-margin route), stay within the shared particle cap, and disarm
/// after expiry/reset. The mutant bypasses both the gate and cap.
#[test]
fn derived_nyan_terminus_admission_proves_and_catches_false_scatter() {
    assert_proves_and_catches(&nyan_terminus_admission_model());
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
            let out = Command::new(&typ)
                .arg("check")
                .arg(&spec)
                .arg("--config")
                .arg(&cfgp)
                .output()
                .expect("run ty check");
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

/// Nyan scheduler regression family: idle blink edges are silent, a content
/// present consumes the stale effect timer, and brisk tails remain phase-locked
/// instead of adding redraw cost to every interval. Each model proves the fixed
/// rule at Buggy=0 and independently requires a counterexample at Buggy=1.
#[test]
fn derived_nyan_idle_twinkle_proves_and_catches_idle_wakes() {
    assert_proves_and_catches(&nyan_idle_twinkle_model());
}

/// A missing compositor callback cannot restart the Nyan exit lifecycle. The
/// first sample after the logical completion deadline is settled and disarmed;
/// Buggy restarts visible reach/retract motion at callback time.
#[test]
fn derived_nyan_exit_sampling_proves_and_catches_sparse_restart() {
    let healthy = nyan_exit_sampling_model();
    assert_proves_and_catches(&healthy);
    let mut state = healthy.init_state();
    assert!(healthy.fire("ElapseDone", &mut state));
    assert!(healthy.fire("ObserveDone", &mut state));
    assert_eq!(state[&"logical_done"], 1);
    assert_eq!(state[&"sampled"], 1);
    assert_eq!(state[&"visible"], 0);
    assert_eq!(state[&"active"], 0);
    assert!(healthy.check_invariant("SettledSampleHasNoLight", &state));
    assert!(healthy.check_invariant("SettledSampleDisarms", &state));

    let mut buggy = nyan_exit_sampling_model();
    for cst in &mut buggy.consts {
        if cst.0 == "Buggy" {
            cst.1 = 1;
        }
    }
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
fn derived_effect_phase_lock_proves_and_catches_cadence_slide() {
    assert_proves_and_catches(&effect_phase_lock_model());
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
/// `Contained`. `StepEdgesMarked` is the always-true non-vacuity control.
#[test]
fn derived_rain_band_containment_proves_and_catches_stale_glyph_ghost() {
    assert_proves_and_catches(&rain_band_containment_model());
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
