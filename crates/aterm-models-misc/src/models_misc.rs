// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Formal models for audio trails, input pairing, maintenance lanes, search, capture, and budgets.

use aterm_spec::derive::*;

/// Trail-audio UI-to-worker handoff and `AudioQueue` idle lifecycle model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn trail_audio_lifecycle_model() -> Model {
    aterm_spec::ty_model! {
        TrailAudioLifecycle {
            const Buggy = 0;
            const CommandCap = 2;
            const DropCap = 2;
            const SilentCap = 2;
            var queued = 0;
            var dropped = 0;
            var last_full = 0;
            var running = 0;
            var silent = 0;
            var service_deadline = 0;
            var cue_applied = 0;
            var failed = 0;
            var paused = 0;
            var ui_blocked = 0;
            var ui_platform_calls = 0;

            action PushCueAvailable when (
                queued <= CommandCap - 1 + Buggy && failed <= Buggy
            ) {
                queued = queued + 1;
                last_full = 0;
                ui_platform_calls = if Buggy == 1 { 1 } else { 0 };
            }
            action PushCueFull when (queued == CommandCap && failed == 0) {
                queued = queued;
                dropped = if Buggy == 1 {
                    dropped
                } else {
                    if dropped <= DropCap - 1 { dropped + 1 } else { dropped }
                };
                last_full = 1;
                ui_blocked = if Buggy == 1 { 1 } else { 0 };
            }
            action WorkerStart when (queued > 0 && running == 0 && failed <= Buggy) {
                queued = queued - 1;
                last_full = 0;
                running = 1;
                silent = 0;
                service_deadline = 1;
                cue_applied = 1;
                paused = 0;
            }
            action WorkerPushRunning when (queued > 0 && running == 1 && failed == 0) {
                queued = queued - 1;
                last_full = 0;
                silent = if Buggy == 1 { silent } else { 0 };
                service_deadline = 1;
                cue_applied = 1;
                paused = 0;
            }
            action WorkerStartFails when (queued > 0 && running == 0 && failed == 0) {
                queued = 0;
                last_full = 0;
                running = 0;
                service_deadline = 0;
                cue_applied = 0;
                failed = 1;
                paused = 0;
            }
            action RenderAudible when (running == 1) {
                silent = 0;
                cue_applied = 0;
            }
            action RenderSilent when (
                running == 1 && cue_applied == 0 && silent <= SilentCap - 1
            ) {
                silent = silent + 1;
            }
            action ServiceRunning when (
                running == 1 && service_deadline == 1 && silent <= SilentCap - 1
            ) {
                service_deadline = 1;
            }
            action PauseIdle when (
                running == 1 && queued == 0 && cue_applied == 0 &&
                service_deadline == 1 && silent == SilentCap
            ) {
                running = 0;
                service_deadline = if Buggy == 1 { 1 } else { 0 };
                paused = 1;
            }
            action ParkIdle when (running == 0 && queued == 0 && service_deadline == 0) {
                running = running;
            }

            invariant WorkerMailboxIsBounded: queued <= CommandCap;
            bound DropAccountingIsBounded: dropped <= DropCap;
            invariant FullIngressDropsNewest:
                if last_full == 1 {
                    queued == CommandCap && dropped > 0
                } else {
                    last_full == 0
                };
            invariant UiEnqueueNeverBlocks: ui_blocked == 0;
            invariant UiNeverTouchesPlatform: ui_platform_calls == 0;
            invariant AppliedCueResetsSilence:
                if cue_applied == 1 {
                    running == 1 && silent == 0 && service_deadline == 1
                } else {
                    cue_applied == 0
                };
            invariant RunningOwnsOneDeadline:
                if running == 1 { service_deadline == 1 } else { service_deadline == 0 };
            invariant StartFailureIsExplicitAndTerminal:
                if failed == 1 {
                    running == 0 && queued == 0 && service_deadline == 0
                } else {
                    failed == 0
                };
        }
    }
}

/// `AudioQueue` buffer ownership and first-audible-buffer latency across cold start and idle resume.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn trail_audio_start_latency_model() -> Model {
    aterm_spec::ty_model! {
        TrailAudioStartLatency {
            const Buggy = 0;
            const BufferCount = 3;
            const MaxAudibleBuffer = 1;
            var phase = 0;
            var available = 3;
            var queued = 0;
            var recycling = 0;
            var running = 0;
            var audible_buffer = 0;
            var unsafe_writes = 0;
            var generation = 0;
            var callback_generation = 0;
            var stale_enqueue = 0;
            var enqueue_in_flight = 0;
            var stop_overlap = 0;

            action CueCold when (phase == 0) {
                phase = 1;
            }
            action PrimeCold when (phase == 1) {
                phase = 2;
                available = 0;
                queued = BufferCount;
                recycling = 1;
                audible_buffer = if Buggy == 1 { BufferCount + 1 } else { 1 };
                generation = 1;
                callback_generation = 1;
            }
            action StartCold when (phase == 2) {
                phase = 3;
                running = 1;
            }
            action CallbackEnqueueBegins when (
                phase == 3 && recycling == 1 && enqueue_in_flight == 0
            ) {
                enqueue_in_flight = 1;
            }
            action CallbackEnqueueEnds when (phase == 3 && enqueue_in_flight == 1) {
                enqueue_in_flight = 0;
            }
            action StopIdle when (phase == 3 && enqueue_in_flight <= Buggy) {
                phase = 4;
                available = if Buggy == 1 && enqueue_in_flight == 0 { 0 } else { BufferCount };
                queued = if Buggy == 1 { BufferCount } else { 0 };
                recycling = 0;
                running = 0;
                audible_buffer = 0;
                generation = if Buggy == 1 { generation } else { generation + 1 };
                stop_overlap = if enqueue_in_flight == 1 { 1 } else { 0 };
                enqueue_in_flight = 0;
            }
            action ParkIdle when (phase == 4) {
                phase = 4;
            }
            action CueResume when (phase == 4) {
                phase = 5;
            }
            action PrimeResume when (phase == 5) {
                phase = 6;
                available = 0;
                queued = BufferCount;
                recycling = 1;
                audible_buffer = if Buggy == 1 { BufferCount + 1 } else { 1 };
                unsafe_writes = if Buggy == 1 { 1 } else { 0 };
                generation = if Buggy == 1 { generation } else { generation + 1 };
            }
            action StartResume when (phase == 6) {
                phase = 7;
                running = 1;
            }
            action OldCallbackReturns when (phase == 7) {
                phase = 8;
                stale_enqueue = if callback_generation == generation { 1 } else { 0 };
            }

            invariant BufferOwnershipConserved: available + queued == BufferCount;
            invariant AudibleWithinOneBuffer: audible_buffer <= MaxAudibleBuffer;
            invariant WritesRequireAvailableOwnership: unsafe_writes == 0;
            invariant StaleCallbackCannotReenqueue: stale_enqueue == 0;
            invariant StopNeverOverlapsEnqueue: stop_overlap == 0;
            bound PhaseBounded: phase <= 8;
        }
    }
}

/// Typing-sound scheduling seam and `tone` probe agreement model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn trail_sound_seam_model() -> Model {
    aterm_spec::ty_model! {
        TrailSoundSeam {
            const Buggy = 0;
            var master = 0;
            var amplitude = 0;
            var dark_cause = 0;
            var seam = 0;
            var key = 0;
            var tone = 0;

            action TickLit {
                master = 1;
                amplitude = 1;
                dark_cause = 0;
                seam = 1;
                key = 0;
                tone = 0;
            }
            action TickDarkMotion {
                master = 1;
                amplitude = 0;
                dark_cause = 1;
                seam = if Buggy == 1 { 0 } else { 1 };
                key = 0;
                tone = 0;
            }
            action TickDarkUnfocused {
                master = 1;
                amplitude = 0;
                dark_cause = 2;
                seam = if Buggy == 1 { 1 } else { 0 };
                key = 0;
                tone = 0;
            }
            action TickDarkUserOff {
                master = 1;
                amplitude = 0;
                dark_cause = 3;
                seam = 0;
                key = 0;
                tone = 0;
            }
            action TickMasterOff {
                master = 0;
                amplitude = 0;
                dark_cause = 0;
                seam = 0;
                key = 0;
                tone = 0;
            }
            action Key {
                key = if seam == 1 { 1 } else { 2 };
            }
            action Tone {
                tone = if master == 0 {
                    2
                } else {
                    if dark_cause == 3 {
                        if Buggy == 1 {
                            if seam == 1 { 1 } else { 3 }
                        } else {
                            2
                        }
                    } else {
                        if dark_cause == 2 {
                            2
                        } else {
                            if seam == 1 { 1 } else { 3 }
                        }
                    }
                };
            }

            invariant FocusedKeyIsHeard:
                if key > 0 && master == 1 && dark_cause <= 1 {
                    key == 1
                } else {
                    key <= 2
                };
            invariant ForeignKeyIsSilent:
                if key > 0 && (master == 0 || dark_cause > 1) {
                    key == 2
                } else {
                    key <= 2
                };
            invariant ToneAgreesWithTheKey:
                if key > 0 && tone > 0 {
                    if key == 1 { tone == 1 } else { tone > 1 }
                } else {
                    tone <= 3
                };
            invariant EngineSilentIsUnreachable: tone <= 2;
        }
    }
}

/// Bounded audio device reopen ladder and terminal fault latch model.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn trail_audio_reopen_ladder_model() -> Model {
    aterm_spec::ty_model! {
        TrailAudioReopenLadder {
            const Buggy = 0;
            const Budget = 2;
            var attempts = 0;
            var spent = 0;
            var device = 0;
            var waiting = 0;
            var played = 0;
            var stale = 1;
            var failed = 0;
            var evidence = 2;

            action Cue when (device == 0) {
                device = if failed == 1 || waiting == 1 { 0 } else { 1 };
                attempts = if Buggy == 1 && waiting == 0 && failed == 0 && stale == 0 {
                    0
                } else {
                    attempts
                };
            }
            action CueSounds {
                attempts = if device == 1 && played == 2 {
                    0
                } else {
                    if Buggy == 1 && device == 1 && stale == 1 { 0 } else { attempts }
                };
                spent = if device == 1 && evidence == 2 { 0 } else { spent };
                evidence = if device == 1 && evidence == 0 { 1 } else { evidence };
                played = if device == 1 && played == 2 {
                    0
                } else {
                    if device == 1 && played == 0 && attempts > 0 { 1 } else { played }
                };
            }
            action DeviceFaults when (failed == 0 && (device == 1 || waiting == 0)) {
                failed = if Buggy == 1 && device == 0 {
                    1
                } else {
                    if attempts <= Budget - 1 { 0 } else { 1 }
                };
                attempts = if Buggy == 1 && device == 0 {
                    attempts
                } else {
                    if attempts <= Budget - 1 { attempts + 1 } else { attempts }
                };
                spent = if Buggy == 1 && device == 0 {
                    spent
                } else {
                    if attempts <= Budget - 1 {
                        if spent <= Budget { spent + 1 } else { spent }
                    } else {
                        spent
                    }
                };
                waiting = if Buggy == 1 && device == 0 {
                    waiting
                } else {
                    if attempts <= Budget - 1 {
                        if attempts == 0 { 0 } else { 1 }
                    } else {
                        waiting
                    }
                };
                played = if Buggy == 1 && device == 0 {
                    played
                } else {
                    if attempts <= Budget - 1 { 0 } else { played }
                };
                stale = if Buggy == 1 && device == 0 {
                    stale
                } else {
                    if attempts <= Budget - 1 { 0 } else { stale }
                };
                evidence = if Buggy == 1 && device == 0 {
                    evidence
                } else {
                    if attempts <= Budget - 1 { 0 } else { evidence }
                };
                device = 0;
            }
            action BackoffElapses when (waiting == 1) {
                waiting = 0;
                stale = if attempts == Budget { 1 } else { stale };
            }
            action StaleElapses when (stale == 0) {
                stale = 1;
                waiting = 0;
            }
            action HealthyWindowElapses when (played == 1) {
                played = 2;
                stale = 1;
                evidence = if evidence == 1 { 2 } else { evidence };
            }

            invariant RecoveryIsBounded: spent <= Budget;
            invariant ReopensLeftIsTruthful: attempts == spent;
            invariant SilenceIsPermanentOnlyWhenTheBudgetIsSpent:
                if failed == 1 { attempts == Budget } else { failed == 0 };
            invariant ResetOnlyOnPlayedWindow:
                if attempts == 0 { evidence == 2 } else { evidence <= 2 };
        }
    }
}

/// Tab-stop checkpoint preservation across a narrow seamless handoff.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn tab_stop_handoff_model() -> Model {
    aterm_spec::ty_model! {
        TabStopHandoff {
            const Buggy = 0;
            const Narrow = 4;
            const Wide = 8;
            const Future = 6;
            const MaxCols = 8;
            var phase = 0;
            var cols = 4;
            var backing_len = 4;
            var future_stop = 0;
            var carried_len = 0;
            var carried_future_stop = 0;
            var admitted = 0;
            var rejected = 0;
            var restored_len = 0;
            var restored_future_stop = 0;
            var tab_target = 0;

            action GrowSourceWide when (phase == 0) {
                phase = 1;
                cols = Wide;
                backing_len = Wide;
            }
            action SetCustomFutureStop when (
                phase == 1 && cols == Wide && backing_len == Wide
            ) {
                phase = 2;
                future_stop = 1;
            }
            action ShrinkSourceNarrow when (
                phase == 2 && cols == Wide && future_stop == 1
            ) {
                phase = 3;
                cols = Narrow;
                backing_len = if Buggy == 1 { Narrow } else { backing_len };
                future_stop = if Buggy == 1 { 0 } else { future_stop };
            }
            action CaptureProjection when (phase == 3) {
                phase = 4;
                carried_len = backing_len;
                carried_future_stop = future_stop;
            }
            action AdmitCoveringProjection when (
                phase == 4 && cols <= carried_len && carried_len <= MaxCols
            ) {
                phase = 5;
                admitted = 1;
            }
            action RestoreProjection when (phase == 5 && admitted == 1) {
                phase = 6;
                restored_len = if Buggy == 1 { cols } else { carried_len };
                restored_future_stop = if Buggy == 1 { 0 } else { carried_future_stop };
            }
            action GrowDestinationWide when (phase == 6) {
                phase = 7;
                cols = Wide;
                restored_len = if restored_len <= Wide - 1 { Wide } else { restored_len };
            }
            action TabUsesRestoredStop when (phase == 7 && cols == Wide) {
                phase = 8;
                tab_target = if restored_future_stop == 1 { Future } else { Wide - 1 };
            }
            action SupplyUndersizeProjection when (phase == 0) {
                phase = 9;
                carried_len = Narrow - 1;
            }
            action SupplyOversizeProjection when (phase == 0) {
                phase = 9;
                carried_len = MaxCols + 1;
            }
            action RejectUndersizeProjection when (
                phase == 9 && carried_len <= Narrow - 1
            ) {
                phase = 10;
                admitted = if Buggy == 1 { 1 } else { admitted };
                rejected = if Buggy == 1 { 0 } else { 1 };
                restored_len = if Buggy == 1 { cols } else { restored_len };
            }
            action RejectOversizeProjection when (
                phase == 9 && carried_len > MaxCols
            ) {
                phase = 10;
                admitted = if Buggy == 1 { 1 } else { admitted };
                rejected = if Buggy == 1 { 0 } else { 1 };
                restored_len = if Buggy == 1 { cols } else { restored_len };
            }
            action SettledPreserved when (phase == 8) {
                phase = 8;
            }
            action SettledRejected when (phase == 10) {
                phase = 10;
            }

            invariant NarrowShrinkKeepsBoundedBacking:
                if 3 <= phase && phase <= 8 {
                    backing_len == Wide && future_stop == 1
                } else {
                    backing_len <= MaxCols
                };
            invariant CapturedProjectionPreservesFutureStop:
                if 4 <= phase && phase <= 8 {
                    carried_len == Wide && carried_future_stop == 1
                } else {
                    carried_len <= MaxCols + 1
                };
            invariant AdmissionIsCoveringAndBounded:
                if admitted == 1 {
                    Narrow <= carried_len && carried_len <= MaxCols
                } else {
                    admitted == 0
                };
            invariant RestorePreservesCarriedSemantics:
                if 6 <= phase && phase <= 8 {
                    Wide <= restored_len && restored_len <= MaxCols &&
                    restored_future_stop == 1
                } else {
                    restored_len <= MaxCols
                };
            invariant FutureGrowUsesCustomStop:
                if phase == 8 { tab_target == Future } else { tab_target == 0 };
            invariant InvalidProjectionIsNeverAdmitted:
                if 9 <= phase {
                    admitted == 0 && restored_len == 0
                } else {
                    rejected == 0
                };
        }
    }
}

/// Event-loop lane isolation for bulk scrollback maintenance.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn scrollback_maintenance_lane_model() -> Model {
    aterm_spec::ty_model! {
        ScrollbackMaintenanceLane {
            const Buggy = 0;
            var event = 0;
            var scan_started = 0;
            var blocking_lock = 0;
            var unbounded_work = 0;
            var mutation = 0;
            var completed = 0;

            action ObserveOutput when (event == 0) {
                event = 1;
                scan_started = if Buggy == 1 { 1 } else { 0 };
                blocking_lock = if Buggy == 1 { 1 } else { 0 };
                unbounded_work = if Buggy == 1 { 1 } else { 0 };
                mutation = if Buggy == 1 { 1 } else { 0 };
            }
            action ObserveMemoryPressure when (event == 0) {
                event = 2;
            }
            action BeginBulkTrim when (
                event == 2 && scan_started == 0 && completed == 0
            ) {
                scan_started = 1;
                blocking_lock = 1;
                unbounded_work = 1;
            }
            action CompleteBulkTrim when (
                event == 2 && scan_started == 1 && completed == 0
            ) {
                scan_started = 0;
                mutation = 1;
                completed = 1;
            }
            action SettledOutput when (event == 1 && scan_started == 0) {
                event = 1;
            }
            action SettledPressure when (event == 2 && completed == 1) {
                event = 2;
            }

            invariant OrdinaryOutputIsMaintenanceFree:
                if event == 1 {
                    scan_started == 0 && blocking_lock == 0 &&
                    unbounded_work == 0 && mutation == 0 && completed == 0
                } else {
                    event <= 2
                };
            invariant BlockingLockRequiresMemoryPressure:
                if blocking_lock == 1 { event == 2 } else { event <= 2 };
            invariant UnboundedWorkRequiresMemoryPressure:
                if unbounded_work == 1 { event == 2 } else { event <= 2 };
            invariant MutationRequiresCompletedPressureTrim:
                if mutation == 1 {
                    event == 2 && completed == 1 && scan_started == 0
                } else {
                    completed == 0
                };
            invariant StartedTrimIsPressureOnly:
                if scan_started == 1 { event == 2 && completed == 0 } else { event <= 2 };
        }
    }
}

/// History retention across full-width, top-anchored partial scrolls.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn top_anchored_scroll_history_model() -> Model {
    aterm_spec::ty_model! {
        TopAnchoredScrollHistory {
            const Buggy = 0;
            var phase = 0;
            var top = 0;
            var full_width = 0;
            var history_enabled = 0;
            var history_len = 0;
            var footer = 1;
            var footer_anchor = 0;
            var selection_alive = 1;
            var selection_region_row = 2;
            var selection_footer_row = 4;
            var selection_disjoint = 0;

            action ChooseArchivalOverlapping when (phase == 0) {
                phase = 1;
                top = 0;
                full_width = 1;
                history_enabled = 1;
                selection_disjoint = 0;
            }
            action ChooseArchivalDisjoint when (phase == 0) {
                phase = 1;
                top = 0;
                full_width = 1;
                history_enabled = 1;
                selection_disjoint = 1;
            }
            action ChooseInteriorOverlapping when (phase == 0) {
                phase = 1;
                top = 1;
                full_width = 1;
                history_enabled = 1;
                selection_disjoint = 0;
            }
            action ChooseInteriorDisjoint when (phase == 0) {
                phase = 1;
                top = 1;
                full_width = 1;
                history_enabled = 1;
                selection_disjoint = 1;
            }
            action ChooseMarginedOverlapping when (phase == 0) {
                phase = 1;
                top = 0;
                full_width = 0;
                history_enabled = 1;
                selection_disjoint = 0;
            }
            action ChooseMarginedDisjoint when (phase == 0) {
                phase = 1;
                top = 0;
                full_width = 0;
                history_enabled = 1;
                selection_disjoint = 1;
            }
            action ChooseEphemeralOverlapping when (phase == 0) {
                phase = 1;
                top = 0;
                full_width = 1;
                history_enabled = 0;
                selection_disjoint = 0;
            }
            action ChooseEphemeralDisjoint when (phase == 0) {
                phase = 1;
                top = 0;
                full_width = 1;
                history_enabled = 0;
                selection_disjoint = 1;
            }
            action Scroll when (phase == 1) {
                phase = 2;
                history_len = if top == 0 && full_width == 1 && history_enabled == 1 {
                    if Buggy == 1 { history_len } else { history_len + 1 }
                } else {
                    history_len
                };
                footer_anchor = if top == 0 && full_width == 1 && history_enabled == 1 {
                    if Buggy == 1 { footer_anchor } else { footer_anchor + 1 }
                } else {
                    footer_anchor
                };
                selection_alive = if top == 0 && full_width == 1 && history_enabled == 1 {
                    if Buggy == 1 { 0 } else { selection_alive }
                } else if selection_disjoint == 1 {
                    if Buggy == 1 { 0 } else { selection_alive }
                } else {
                    0
                };
                selection_region_row = if top == 0 && full_width == 1 && history_enabled == 1
                    && selection_disjoint == 0 {
                    if Buggy == 1 { selection_region_row } else { selection_region_row - 1 }
                } else {
                    selection_region_row
                };
                selection_footer_row = selection_footer_row;
                footer = if Buggy == 1 { 0 } else { footer };
            }
            action Settled when (phase == 2) {
                phase = 2;
            }

            invariant EligibleDisplacementIsRetained:
                if phase == 2 && top == 0 && full_width == 1 && history_enabled == 1 {
                    history_len == 1
                } else {
                    history_len == 0
                };
            invariant FixedFooterIsPreserved: footer == 1;
            invariant FixedFooterAnchorTracksLogicalInsertion:
                if phase == 2 && top == 0 && full_width == 1 && history_enabled == 1 {
                    footer_anchor == 1
                } else {
                    footer_anchor == 0
                };
            invariant EligibleSelectionUsesPiecewiseRemap:
                if phase == 2 && top == 0 && full_width == 1 && history_enabled == 1
                    && selection_disjoint == 0 {
                    selection_alive == 1 && selection_region_row == 1 &&
                    selection_footer_row == 4
                } else if phase == 2 && selection_disjoint == 1 {
                    selection_alive == 1 && selection_region_row == 2 &&
                    selection_footer_row == 4
                } else if phase == 2 {
                    selection_alive == 0 && selection_region_row == 2 &&
                    selection_footer_row == 4
                } else {
                    selection_alive == 1 && selection_region_row == 2 &&
                    selection_footer_row == 4
                };
            bound StateIsBounded:
                phase <= 2 && top <= 1 && full_width <= 1 &&
                history_enabled <= 1 && history_len <= 1 && footer <= 1 &&
                footer_anchor <= 1 && selection_alive <= 1 &&
                selection_region_row <= 2 && selection_footer_row <= 4 &&
                selection_disjoint <= 1;
        }
    }
}

/// Single-worker diagnostics channel and latest-wins pending slot for Settings ▸ Manual.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn manual_config_diagnostics_lane_model() -> Model {
    aterm_spec::ty_model! {
        ManualConfigDiagnosticsLane {
            const Buggy = 0;
            const MaxRevision = 3;
            var current_revision = 0;
            var channel_revision = 0;
            var active_revision = 0;
            var pending_revision = 0;
            var completed_revision = 0;
            var published_revision = 0;
            var stale_published = 0;

            action RequestFirst when (current_revision == 0) {
                current_revision = 1;
                channel_revision = 1;
            }
            action RequestSecond when (current_revision == 1) {
                current_revision = 2;
                channel_revision = if channel_revision == 0 { 2 } else { channel_revision };
                pending_revision = if channel_revision == 0 { 0 } else { 2 };
                published_revision = 0;
            }
            action RequestThird when (current_revision == 2) {
                current_revision = 3;
                channel_revision = if channel_revision == 0 { 3 } else { channel_revision };
                pending_revision = if channel_revision == 0 {
                    0
                } else {
                    if Buggy == 1 { 2 } else { 3 }
                };
                published_revision = 0;
            }
            action WorkerTakes when (
                channel_revision > 0 && active_revision == 0
            ) {
                active_revision = channel_revision;
                channel_revision = 0;
            }
            action WorkerCompletes when (
                active_revision > 0 && completed_revision == 0
            ) {
                completed_revision = active_revision;
                active_revision = 0;
            }
            action DispatchLatestPending when (
                channel_revision == 0 && pending_revision > 0
            ) {
                channel_revision = pending_revision;
                pending_revision = 0;
            }
            action AcceptCurrent when (
                completed_revision > 0 && completed_revision == current_revision
            ) {
                published_revision = completed_revision;
                completed_revision = 0;
            }
            action RejectStale when (
                completed_revision > 0 && current_revision > completed_revision
            ) {
                published_revision = if Buggy == 1 {
                    completed_revision
                } else { published_revision };
                stale_published = if Buggy == 1 { 1 } else { stale_published };
                completed_revision = 0;
            }
            action Settled when (
                published_revision == MaxRevision && completed_revision == 0
            ) {
                published_revision = published_revision;
            }

            invariant LatestRequestRemainsRepresented:
                if current_revision > published_revision {
                    current_revision == channel_revision ||
                    current_revision == active_revision ||
                    current_revision == pending_revision ||
                    current_revision == completed_revision
                } else {
                    published_revision == current_revision
                };
            invariant PendingSlotNamesLatest:
                if pending_revision > 0 {
                    pending_revision == current_revision
                } else {
                    pending_revision == 0
                };
            invariant StaleCompletionNeverPublishes: stale_published == 0;
            invariant PublicationIsCurrentOrEmpty:
                published_revision == 0 || published_revision == current_revision;
            bound RevisionsBounded:
                current_revision <= MaxRevision &&
                channel_revision <= MaxRevision && active_revision <= MaxRevision &&
                pending_revision <= MaxRevision && completed_revision <= MaxRevision &&
                published_revision <= MaxRevision;
        }
    }
}

/// Monotonic generation gate for asynchronous config and font publication.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn font_catalog_generation_model() -> Model {
    aterm_spec::ty_model! {
        FontCatalogGeneration {
            const Buggy = 0;
            var requested = 0;
            var completed = 0;
            var published = 0;
            var stale_published = 0;

            action RequestFirst when (requested == 0) { requested = 1; }
            action RequestSecond when (requested == 1) { requested = 2; }
            action CompleteFirst when (requested == 2 && completed == 0) {
                completed = 1;
            }
            action RejectStale when (completed == 1) {
                published = if Buggy == 1 { 1 } else { published };
                stale_published = if Buggy == 1 { 1 } else { stale_published };
                completed = 0;
            }
            action CompleteSecond when (requested == 2 && completed == 0) {
                completed = 2;
            }
            action PublishCurrent when (completed == requested) {
                published = completed;
                completed = 0;
            }

            invariant StaleCompletionNeverPublishes: stale_published == 0;
            invariant PublicationIsCurrentOrEmpty:
                published == 0 || published == requested;
            bound GenerationsBounded:
                requested <= 2 && completed <= 2 && published <= 2;
        }
    }
}

/// Same-handle snapshot admission and fingerprint binding for path feeds.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn path_feed_snapshot_model() -> Model {
    aterm_spec::ty_model! {
        PathFeedSnapshot {
            const Buggy = 0;
            var live = 1;
            var admitted = 0;
            var prepared = 0;
            var fingerprint = 0;
            var read = 0;
            var published = 0;

            action Read when (read == 0 && published == 0) {
                admitted = live;
                prepared = live;
                fingerprint = live;
                read = 1;
            }
            action LiveMutate when (live == 1 && published == 0) {
                live = 2;
            }
            action LiveRestore when (live == 2 && published == 0) {
                live = 1;
            }
            action Publish when (read == 1 && published == 0) {
                fingerprint = if Buggy == 1 { live } else { fingerprint };
                published = 1;
            }

            invariant PublishedPairComesFromAdmittedRead:
                if published == 1 {
                    prepared == admitted && fingerprint == admitted
                } else {
                    1 == 1
                };
            bound Bounds:
                live <= 2 && admitted <= 2 && prepared <= 2 &&
                fingerprint <= 2 && read <= 1 && published <= 1;
        }
    }
}

/// Coupled theme-catalog, appearance-side, and typography re-prepare gate.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn font_theme_generation_model() -> Model {
    aterm_spec::ty_model! {
        FontThemeGeneration {
            const Buggy = 0;
            var requested = 0;
            var completed = 0;
            var published = 0;
            var theme = 0;
            var completed_theme = 0;
            var published_theme = 0;
            var side = 0;
            var completed_side = 0;
            var published_side = 0;
            var colour_theme = 0;
            var colour_side = 0;
            var held = 0;
            var degraded = 0;

            action RequestConfig when (requested == 0 && degraded == 0) {
                requested = 1;
            }
            action ThemeChanged when (
                requested == 1 && completed == 0 && theme == 0
            ) {
                theme = 1;
                colour_theme = if Buggy == 2 { 1 } else { colour_theme };
                held = if Buggy == 2 { held } else { 1 };
            }
            action OsFlip when (
                requested == 1 && completed == 0 && theme == 1 && side == 0
            ) {
                side = 1;
                colour_side = if Buggy == 2 { 1 } else { colour_side };
                held = if Buggy == 2 { held } else { 1 };
            }
            action CompleteOld when (
                requested == 1 && side == 1 && completed == 0
            ) {
                completed = 1;
                completed_theme = 0;
                completed_side = 0;
            }
            action ReprepareLatestTheme when (
                completed == requested && completed > 0 &&
                (theme > completed_theme || side > completed_side)
            ) {
                published = if Buggy == 1 { completed } else { published };
                published_theme = if Buggy == 1 {
                    completed_theme
                } else { published_theme };
                published_side = if Buggy == 1 {
                    completed_side
                } else { published_side };
                requested = 2;
                completed = 0;
            }
            action CompleteLatest when (requested == 2 && completed == 0) {
                completed = 2;
                completed_theme = theme;
                completed_side = side;
            }
            action PublishCurrent when (
                completed == requested && completed > 0 &&
                completed_theme == theme && completed_side == side
            ) {
                published = completed;
                published_theme = completed_theme;
                published_side = completed_side;
                colour_theme = theme;
                colour_side = side;
                held = 0;
                completed = 0;
            }
            action WorkerRefused when (
                requested == 0 && side == 0 && degraded == 0
            ) {
                side = 1;
                colour_side = if Buggy == 3 { colour_side } else { 1 };
                held = if Buggy == 3 { 1 } else { 0 };
                degraded = 1;
            }

            invariant PublishedThemeIsCurrentOrEmpty:
                published == 0 || (published_theme == theme && published_side == side);
            invariant FaceMatchesColours:
                held == 1 || degraded == 1 ||
                (colour_theme == published_theme && colour_side == published_side);
            invariant HeldSwapIsOwed:
                held == 0 || requested > published;
            bound GenerationsBounded:
                requested <= 2 && completed <= 2 && published <= 2 &&
                theme <= 1 && completed_theme <= 1 && published_theme <= 1 &&
                side <= 1 && completed_side <= 1 && published_side <= 1 &&
                colour_theme <= 1 && colour_side <= 1 && held <= 1 && degraded <= 1;
        }
    }
}

/// Staged-update activation across native and portable menu surfaces.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_menu_activation_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateMenuActivation {
            const Buggy = 0;
            var staged = 0;
            var terminal_tab = 0;
            var row_present = 0;
            var row_enabled = 0;
            var apply_decoded = 0;
            var apply_dispatched = 0;

            action StageUpdate when (staged == 0) {
                staged = 1;
            }
            action ActivateTerminalTab when (terminal_tab == 0) {
                terminal_tab = 1;
                row_enabled = if row_present == 1 { 1 } else { 0 };
            }
            action ActivateNativeTab when (terminal_tab == 1) {
                terminal_tab = 0;
                row_enabled = if row_present == 1 {
                    if Buggy == 1 { 0 } else { 1 }
                } else { 0 };
            }
            action RefreshStagedVersionMenu when (staged == 1) {
                row_present = 1;
                row_enabled = if Buggy == 1 { terminal_tab } else { 1 };
            }
            action DecodeApplyTag when (
                row_present == 1 && row_enabled == 1 && apply_decoded == 0
            ) {
                apply_decoded = 1;
            }
            action DispatchApply when (
                staged == 1 && row_present == 1 && row_enabled == 1 &&
                apply_decoded == 1 && apply_dispatched == 0
            ) {
                apply_dispatched = 1;
            }
            action SettledApplied when (apply_dispatched == 1) {
                apply_dispatched = 1;
            }

            invariant RefreshedStagedRowIsPresentAndEnabled:
                if staged == 1 && row_present == 1 {
                    row_enabled == 1
                } else {
                    row_present == 0 && row_enabled == 0
                };
            invariant NativeTabCannotDisableStagedApply:
                if staged == 1 && row_present == 1 && terminal_tab == 0 {
                    row_enabled == 1
                } else {
                    row_enabled <= 1
                };
            invariant ApplyDecodeRequiresSelectableRow:
                if apply_decoded == 1 {
                    staged == 1 && row_present == 1 && row_enabled == 1
                } else {
                    apply_decoded == 0
                };
            invariant ApplyDispatchRequiresExactDecode:
                if apply_dispatched == 1 {
                    staged == 1 && row_present == 1 && row_enabled == 1 &&
                    apply_decoded == 1
                } else {
                    apply_dispatched == 0
                };
        }
    }
}

/// Per-window modifier-cache reset at the focus-loss boundary.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn focus_modifier_cache_model() -> Model {
    aterm_spec::ty_model! {
        FocusModifierCache {
            const Buggy = 0;
            var focused = 1;
            var cached_ctrl = 0;
            var fresh_ctrl = 0;
            var delivered_ctrl = 0;

            action ReportCtrl {
                cached_ctrl = 1;
                fresh_ctrl = 1;
                delivered_ctrl = 0;
            }
            action ReportNone {
                cached_ctrl = 0;
                fresh_ctrl = 0;
                delivered_ctrl = 0;
            }
            action FocusOut {
                focused = 0;
                cached_ctrl = if Buggy == 1 { cached_ctrl } else { 0 };
                fresh_ctrl = 0;
                delivered_ctrl = 0;
            }
            action FocusIn {
                focused = 1;
                delivered_ctrl = 0;
            }
            action PressL when (focused == 1) {
                delivered_ctrl = cached_ctrl;
            }

            invariant CachedCtrlRequiresAuthoritativeReport:
                if cached_ctrl == 1 {
                    fresh_ctrl == 1
                } else {
                    cached_ctrl == 0
                };
            invariant CtrlDeliveryRequiresAuthoritativeReport:
                if delivered_ctrl == 1 {
                    focused == 1 && cached_ctrl == 1 && fresh_ctrl == 1
                } else {
                    delivered_ctrl == 0
                };
            bound StateBounds:
                focused <= 1 && cached_ctrl <= 1 && fresh_ctrl <= 1 &&
                delivered_ctrl <= 1;
        }
    }
}

/// Selection custody across printable, navigation, and viewport-scroll key events.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn press_custody_model() -> Model {
    aterm_spec::ty_model! {
        PressCustody {
            const Buggy = 0;
            const MaxOffset = 2;

            var owner = 0;
            var offset = 0;
            var selection = 0;
            var prev_owner = 0;
            var prev_offset = 0;
            var prev_selection = 0;
            var last_event = 0;

            action UserScroll when (offset <= MaxOffset - 1) {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = offset + 1;
                owner = 1;
                last_event = 0;
            }
            action UserSelect {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                selection = 1;
                last_event = 0;
            }
            action UserClear {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                selection = 0;
                last_event = 0;
            }
            action OutputWhileReading when (owner == 1 && offset <= MaxOffset - 1) {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = if Buggy == 1 { 0 } else { offset + 1 };
                owner = if Buggy == 1 { 0 } else { 1 };
                selection = if Buggy == 1 { 0 } else { selection };
                last_event = 5;
            }
            action OutputAtLive when (owner == 0) {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = 0;
                last_event = 5;
            }
            action OutputDamagesTheSelectedRows
                when (selection == 1 && offset <= MaxOffset - 1)
            {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = if Buggy == 1 { 0 } else { if owner == 1 { offset + 1 } else { 0 } };
                owner = if Buggy == 1 { 0 } else { owner };
                selection = 0;
                last_event = 6;
            }
            action OutputDamagesTheSelectedRowsInPlace when (selection == 1) {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = if Buggy == 1 { 0 } else { offset };
                owner = if Buggy == 1 { 0 } else { owner };
                selection = 0;
                last_event = 6;
            }
            action OutputTookTheSelectionUnattributed
                when (selection == 1 && offset <= MaxOffset - 1)
            {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = if Buggy == 1 { 0 } else { if owner == 1 { offset + 1 } else { 0 } };
                owner = if Buggy == 1 { 0 } else { owner };
                selection = 0;
                last_event = 6;
            }
            action OutputTookTheSelectionUnattributedInPlace when (selection == 1) {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = if Buggy == 1 { 0 } else { offset };
                owner = if Buggy == 1 { 0 } else { owner };
                selection = 0;
                last_event = 6;
            }
            action OutputInvalidatesTheCoordinateSpace {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = 0;
                owner = 0;
                selection = if Buggy == 1 { selection } else { 0 };
                last_event = 7;
            }
            action SnapToLive when (offset > 0) {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = 0;
                owner = 0;
                selection = if Buggy == 1 { 0 } else { selection };
                last_event = 8;
            }
            action UserScrollTowardLive when (offset > 1) {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = offset - 1;
                selection = if Buggy == 1 { 0 } else { selection };
                last_event = 8;
            }
            action TypingPress {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = if Buggy == 1 { offset } else { 0 };
                owner = 0;
                selection = 0;
                last_event = 1;
            }
            action RepeatPress {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = if Buggy == 1 { 0 } else { offset };
                owner = if Buggy == 1 { 0 } else { owner };
                selection = if Buggy == 1 { 0 } else { selection };
                last_event = 2;
            }
            action InertPress {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = if Buggy == 1 { 0 } else { offset };
                owner = if Buggy == 1 { 0 } else { owner };
                selection = if Buggy == 1 { 0 } else { selection };
                last_event = 3;
            }
            action ReleaseEvent {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = if Buggy == 1 { 0 } else { offset };
                owner = if Buggy == 1 { 0 } else { owner };
                selection = if Buggy == 1 { 0 } else { selection };
                last_event = 4;
            }

            invariant TailOwnerAtBottom:
                if owner == 0 { offset == 0 } else { offset > 0 };
            invariant InertPressIsInert:
                if last_event == 3 {
                    offset == prev_offset && owner == prev_owner &&
                    selection == prev_selection
                } else {
                    last_event <= 8
                };
            invariant RepeatPressIsInert:
                if last_event == 2 {
                    offset == prev_offset && owner == prev_owner &&
                    selection == prev_selection
                } else {
                    last_event <= 8
                };
            invariant ReleaseIsInert:
                if last_event == 4 {
                    offset == prev_offset && owner == prev_owner &&
                    selection == prev_selection
                } else {
                    last_event <= 8
                };
            invariant OutputNeverTakesCustody:
                if last_event == 5 {
                    owner == prev_owner && prev_offset <= offset
                } else {
                    if last_event == 6 {
                        owner == prev_owner && prev_offset <= offset
                    } else {
                        last_event <= 8
                    }
                };
            invariant OutputSparesAnUndamagedSelection:
                if last_event == 5 {
                    selection == prev_selection
                } else {
                    last_event <= 8
                };
            invariant TypingLandsAtLive:
                if last_event == 1 {
                    offset == 0 && owner == 0 && selection == 0
                } else {
                    last_event <= 8
                };
            invariant InvalidationCannotLeaveADanglingSelection:
                if last_event == 7 {
                    offset == 0 && owner == 0 && selection == 0
                } else {
                    last_event <= 8
                };
            invariant ReturningTowardLiveKeepsTheSelection:
                if last_event == 8 {
                    selection == prev_selection && offset <= prev_offset - 1
                } else {
                    last_event <= 8
                };
            bound StateBounds:
                owner <= 1 && offset <= MaxOffset && selection <= 1 &&
                prev_owner <= 1 && prev_offset <= MaxOffset && prev_selection <= 1 &&
                last_event <= 8;
        }
    }
}

/// Selection lifecycle across screen-clearing and scrollback mutations.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn selection_custody_model() -> Model {
    aterm_spec::ty_model! {
        SelectionCustody {
            const Buggy = 0;

            var alive = 0;
            var sel_lo = 0;
            var sel_hi = 0;
            var band_lo = 0;
            var band_hi = 0;
            var floor = 0;
            var truncated = 0;
            var prev_alive = 0;
            var prev_sel_lo = 0;
            var prev_sel_hi = 0;
            var last_event = 0;

            action SelectLow when (floor == 0) {
                prev_alive = alive;
                prev_sel_lo = sel_lo;
                prev_sel_hi = sel_hi;
                alive = 1;
                sel_lo = 0;
                sel_hi = 1;
                truncated = 0;
                last_event = 1;
            }
            action SelectOldest when (floor == 0) {
                prev_alive = alive;
                prev_sel_lo = sel_lo;
                prev_sel_hi = sel_hi;
                alive = 1;
                sel_lo = 0;
                sel_hi = 0;
                truncated = 0;
                last_event = 1;
            }
            action SelectHigh {
                prev_alive = alive;
                prev_sel_lo = sel_lo;
                prev_sel_hi = sel_hi;
                alive = 1;
                sel_lo = 2;
                sel_hi = 3;
                truncated = 0;
                last_event = 1;
            }
            action UserClear {
                prev_alive = alive;
                prev_sel_lo = sel_lo;
                prev_sel_hi = sel_hi;
                alive = 0;
                truncated = 0;
                last_event = 1;
            }
            action TypingPress {
                prev_alive = alive;
                prev_sel_lo = sel_lo;
                prev_sel_hi = sel_hi;
                alive = 0;
                truncated = 0;
                last_event = 2;
            }
            action InertPress when (alive == 1) {
                prev_alive = alive;
                prev_sel_lo = sel_lo;
                prev_sel_hi = sel_hi;
                alive = if Buggy == 1 { 0 } else { alive };
                last_event = 3;
            }
            action UniformScroll when (alive == 1) {
                prev_alive = alive;
                prev_sel_lo = sel_lo;
                prev_sel_hi = sel_hi;
                alive = if Buggy == 1 { 0 } else { alive };
                last_event = 5;
            }
            action RegionDamageLow when (alive == 1) {
                prev_alive = alive;
                prev_sel_lo = sel_lo;
                prev_sel_hi = sel_hi;
                band_lo = 0;
                band_hi = 1;
                alive = if Buggy == 1 {
                    0
                } else {
                    if sel_lo <= 1 { 0 } else { 1 }
                };
                last_event = 4;
            }
            action RegionDamageHigh when (alive == 1) {
                prev_alive = alive;
                prev_sel_lo = sel_lo;
                prev_sel_hi = sel_hi;
                band_lo = 3;
                band_hi = 3;
                alive = if Buggy == 1 {
                    1
                } else {
                    if sel_hi > 2 { 0 } else { 1 }
                };
                last_event = 4;
            }
            action Evict when (alive == 1 && floor == 0) {
                prev_alive = alive;
                prev_sel_lo = sel_lo;
                prev_sel_hi = sel_hi;
                floor = 1;
                alive = if Buggy == 1 {
                    1
                } else {
                    if sel_hi > 0 { 1 } else { 0 }
                };
                sel_lo = if Buggy == 1 {
                    sel_lo
                } else {
                    if sel_lo > 0 { sel_lo } else { 1 }
                };
                truncated = if Buggy == 1 {
                    1
                } else {
                    if sel_hi > 0 {
                        if sel_lo > 0 { 0 } else { 1 }
                    } else {
                        0
                    }
                };
                last_event = 6;
            }
            action WholesaleInvalidate when (alive == 1) {
                prev_alive = alive;
                prev_sel_lo = sel_lo;
                prev_sel_hi = sel_hi;
                alive = 0;
                truncated = 0;
                last_event = 7;
            }

            invariant InertPressPreservesTheSelection:
                if last_event == 3 {
                    alive == prev_alive && sel_lo == prev_sel_lo &&
                    sel_hi == prev_sel_hi
                } else {
                    alive <= 1
                };
            invariant UniformScrollPreservesTheSelection:
                if last_event == 5 {
                    alive == prev_alive && sel_lo == prev_sel_lo &&
                    sel_hi == prev_sel_hi
                } else {
                    alive <= 1
                };
            invariant DisjointDamagePreserves:
                if last_event == 4 {
                    if prev_sel_lo > band_hi {
                        alive == prev_alive && sel_lo == prev_sel_lo &&
                        sel_hi == prev_sel_hi
                    } else {
                        if band_lo > prev_sel_hi {
                            alive == prev_alive && sel_lo == prev_sel_lo &&
                            sel_hi == prev_sel_hi
                        } else {
                            alive <= 1
                        }
                    }
                } else {
                    alive <= 1
                };
            invariant OverlapDamageClears:
                if last_event == 4 {
                    if prev_sel_lo > band_hi {
                        alive <= 1
                    } else {
                        if band_lo > prev_sel_hi { alive <= 1 } else { alive == 0 }
                    }
                } else {
                    alive <= 1
                };
            invariant PartialEvictionTruncates:
                if last_event == 6 {
                    if prev_sel_hi > 0 {
                        if prev_sel_lo > 0 {
                            alive == 1 && sel_lo == prev_sel_lo && truncated == 0
                        } else {
                            alive == 1 && sel_lo == floor && truncated == 1
                        }
                    } else {
                        alive == 0
                    }
                } else {
                    alive <= 1
                };
            invariant NoDanglingAnchors:
                if alive == 1 { floor <= sel_lo } else { alive == 0 };
            invariant TruncationImpliesAClampedHead:
                if truncated == 1 { floor <= sel_lo } else { truncated == 0 };
            bound StateIsBounded:
                alive <= 1 && truncated <= 1 && floor <= 1 && sel_lo <= 3 &&
                sel_hi <= 3 && band_lo <= 3 && band_hi <= 3 &&
                prev_alive <= 1 && prev_sel_lo <= 3 && prev_sel_hi <= 3 &&
                last_event <= 7;
        }
    }
}

/// Alternate-screen selection park and restore lifecycle.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn alt_selection_park_model() -> Model {
    aterm_spec::ty_model! {
        AltSelectionPark {
            const Buggy = 0;

            var on_alt = 0;
            var live_sel = 0;
            var parked_sel = 0;
            var last_event = 0;

            action Select {
                live_sel = 1;
                last_event = 0;
            }
            action Deselect {
                live_sel = 0;
                last_event = 0;
            }
            action Enter when (on_alt == 0) {
                parked_sel = live_sel;
                live_sel = if Buggy == 1 { parked_sel } else { 0 };
                on_alt = 1;
                last_event = 1;
            }
            action Leave when (on_alt == 1) {
                live_sel = parked_sel;
                parked_sel = if Buggy == 1 { live_sel } else { 0 };
                on_alt = 0;
                last_event = 3;
            }
            action Wholesale {
                live_sel = 0;
                parked_sel = if Buggy == 1 { parked_sel } else { 0 };
                last_event = 2;
            }
            action Reset {
                on_alt = 0;
                live_sel = 0;
                parked_sel = if Buggy == 1 { parked_sel } else { 0 };
                last_event = 4;
            }
            action RestoreMain {
                on_alt = 0;
                parked_sel = if Buggy == 1 { parked_sel } else { 0 };
                last_event = 5;
            }
            action RestoreAlt {
                on_alt = 1;
                parked_sel = if Buggy == 1 { parked_sel } else { 0 };
                last_event = 5;
            }

            invariant ParkedEmptyOffAlt:
                if on_alt == 0 { parked_sel == 0 } else { parked_sel <= 1 };
            invariant DestroyerLeavesNothingSelected:
                if last_event == 2 || last_event == 4 {
                    live_sel == 0 && parked_sel == 0
                } else {
                    last_event <= 5
                };
            invariant RestoreRetiresTheParkedSlot:
                if last_event == 5 { parked_sel == 0 } else { last_event <= 5 };
            bound StateBounds:
                on_alt <= 1 && live_sel <= 1 && parked_sel <= 1 && last_event <= 5;
        }
    }
}

/// Single-key press, repeat, and release pairing at the GUI-to-PTY boundary.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn input_release_pairing_model() -> Model {
    aterm_spec::ty_model! {
        InputReleasePairing {
            const Buggy = 0;
            var phase = 0;
            var tracker = 0;
            var overlay_open = 0;
            var press_consumed = 0;
            var press_forwarded = 0;
            var press_literal = 0;
            var press_local = 0;
            var pty_press_outstanding = 0;
            var repeat_observed = 0;
            var repeat_emitted = 0;
            var release_emitted = 0;
            var untracked_release_swallowed = 0;
            var orphan_csi_u = 0;
            var focused_window = 1;
            var press_window = 0;
            var repeat_routed_window = 0;
            var release_arrival_window = 0;
            var release_routed_window = 0;

            action OpenOverlay when (phase <= 1 && overlay_open == 0) {
                overlay_open = 1;
            }
            action CloseOverlay when (phase <= 1 && overlay_open == 1) {
                overlay_open = 0;
                tracker = if Buggy == 1 && tracker == 2 { 0 } else { tracker };
            }
            action ConsumePhysicalPress when (phase == 0) {
                phase = 1;
                tracker = 1;
                press_consumed = 1;
                press_window = focused_window;
            }
            action ConsumeOverlayPress when (phase == 0 && overlay_open == 1) {
                phase = 1;
                tracker = 2;
                press_consumed = 1;
                press_window = focused_window;
            }
            action ForwardPress when (phase == 0) {
                phase = 1;
                press_forwarded = 1;
                pty_press_outstanding = 1;
                press_window = focused_window;
            }
            action ForwardLiteralPress when (phase == 0) {
                phase = 1;
                tracker = 3;
                press_literal = 1;
                press_window = focused_window;
            }
            action CaptureLocalRepeatPress when (phase == 0) {
                phase = 1;
                tracker = 4;
                press_local = 1;
                press_window = focused_window;
            }
            action SwallowUntrackedRepeat when (phase == 0) {
                repeat_observed = 1;
                repeat_emitted = if Buggy == 1 { 1 } else { 0 };
                orphan_csi_u = if Buggy == 1 { 1 } else { 0 };
            }
            action TransferFocusWhileHeld when (phase == 1) {
                focused_window = if focused_window == 1 { 2 } else { 1 };
            }
            action RepeatOfConsumedPress when (
                phase == 1 && press_consumed == 1 && tracker > 0
            ) {
                repeat_observed = 1;
                tracker = if Buggy == 1 { 0 } else { tracker };
                repeat_emitted = if Buggy == 1 { 1 } else { 0 };
                orphan_csi_u = if Buggy == 1 { 1 } else { 0 };
            }
            action GateConsumesRepeatOfForwardedPress when (
                phase == 1 && press_forwarded == 1 && pty_press_outstanding == 1
            ) {
                repeat_observed = 1;
                tracker = if Buggy == 1 { 2 } else { tracker };
            }
            action ForwardRepeatOfForwardedPress when (
                phase == 1 && press_forwarded == 1 &&
                pty_press_outstanding == 1 && tracker == 0
            ) {
                repeat_observed = 1;
                repeat_emitted = 1;
                repeat_routed_window =
                    if Buggy == 1 { focused_window } else { press_window };
            }
            action ForwardRepeatOfLiteralPress when (
                phase == 1 && press_literal == 1 && tracker == 3
            ) {
                repeat_observed = 1;
                repeat_emitted = 1;
                repeat_routed_window =
                    if Buggy == 1 { focused_window } else { press_window };
            }
            action ForwardLocalRepeat when (
                phase == 1 && press_local == 1 && tracker == 4
            ) {
                repeat_observed = 1;
                repeat_emitted = 1;
                repeat_routed_window =
                    if Buggy == 1 { focused_window } else { press_window };
            }
            action ReleaseConsumedPress when (
                phase == 1 && press_consumed == 1 && tracker > 0
            ) {
                phase = 2;
                tracker = 0;
                release_emitted = if Buggy == 1 { 1 } else { 0 };
                orphan_csi_u = if Buggy == 1 { 1 } else { 0 };
                release_arrival_window = focused_window;
                release_routed_window = if Buggy == 1 { focused_window } else { 0 };
            }
            action ReleaseForwardedPress when (
                phase == 1 && press_forwarded == 1 &&
                pty_press_outstanding == 1 && tracker == 0
            ) {
                phase = 2;
                pty_press_outstanding = 0;
                release_emitted = if Buggy == 1 && overlay_open == 1 { 0 } else { 1 };
                untracked_release_swallowed =
                    if Buggy == 1 && overlay_open == 1 { 1 } else { 0 };
                release_arrival_window = focused_window;
                release_routed_window =
                    if Buggy == 1 { focused_window } else { press_window };
            }
            action ReleaseLiteralPress when (
                phase == 1 && press_literal == 1 && tracker == 3
            ) {
                phase = 2;
                tracker = 0;
                release_emitted = if Buggy == 1 { 1 } else { release_emitted };
                orphan_csi_u = if Buggy == 1 { 1 } else { orphan_csi_u };
                release_arrival_window = focused_window;
            }
            action ReleaseLocalRepeatPress when (
                phase == 1 && press_local == 1 && tracker == 4
            ) {
                phase = 2;
                tracker = 0;
                release_emitted = if Buggy == 1 { 1 } else { release_emitted };
                orphan_csi_u = if Buggy == 1 { 1 } else { orphan_csi_u };
                release_arrival_window = focused_window;
            }
            action PhysicalFocusLoss when (
                phase == 1 && tracker == 1 && press_consumed == 1
            ) {
                phase = 3;
                tracker = 0;
            }
            action SettledRelease when (phase == 2) {
                phase = 2;
            }
            action SettledFocusEpoch when (phase == 3) {
                phase = 3;
            }

            invariant NoOrphanCsiUBytes: orphan_csi_u == 0;
            invariant UntrackedReleaseNeverSwallowed: untracked_release_swallowed == 0;
            invariant TrackerAndPtyOutstandingAreExclusive:
                if tracker > 0 {
                    press_consumed + press_literal + press_local == 1 &&
                    pty_press_outstanding == 0
                } else {
                    tracker == 0
                };
            invariant ConsumedPressEpisodeIsByteSilent:
                if press_consumed == 1 {
                    press_forwarded == 0 && pty_press_outstanding == 0 &&
                    repeat_emitted == 0 && release_emitted == 0
                } else {
                    press_consumed == 0
                };
            invariant ConsumedHoldRetainsPressTimeDisposition:
                if phase == 1 && press_consumed == 1 {
                    tracker > 0
                } else {
                    tracker <= 4
                };
            invariant ForwardedPressRemainsOwedUntilRelease:
                if press_forwarded == 1 {
                    press_consumed == 0 && tracker == 0 &&
                    if phase == 1 {
                        pty_press_outstanding == 1 && release_emitted == 0
                    } else {
                        phase == 2 && pty_press_outstanding == 0 && release_emitted == 1
                    }
                } else {
                    press_forwarded == 0
                };
            invariant LiteralInputRetainsSilentReleaseOwnership:
                if press_literal == 1 {
                    press_consumed == 0 && press_forwarded == 0 &&
                    pty_press_outstanding == 0 &&
                    if phase == 1 {
                        tracker == 3 && release_emitted == 0
                    } else {
                        phase == 2 && tracker == 0 && release_emitted == 0
                    }
                } else {
                    press_literal == 0
                };
            invariant LocalRepeatRetainsSilentReleaseOwnership:
                if press_local == 1 {
                    press_consumed == 0 && press_forwarded == 0 &&
                    press_literal == 0 && pty_press_outstanding == 0 &&
                    if phase == 1 {
                        tracker == 4 && release_emitted == 0
                    } else {
                        phase == 2 && tracker == 0 && release_emitted == 0
                    }
                } else {
                    press_local == 0
                };
            invariant CompletedReleaseClearsPairingState:
                if phase == 2 {
                    tracker == 0 && pty_press_outstanding == 0 &&
                    release_emitted == press_forwarded
                } else {
                    phase <= 3
                };
            invariant ConsumedReleaseIsSwallowedAtAnyFocus:
                if phase == 2 && press_consumed == 1 {
                    release_arrival_window > 0 && release_emitted == 0 &&
                    release_routed_window == 0
                } else {
                    release_routed_window <= 2
                };
            invariant ForwardedReleaseUsesOriginalPressTarget:
                if phase == 2 && press_forwarded == 1 {
                    release_arrival_window > 0 && release_emitted == 1 &&
                    release_routed_window == press_window
                } else {
                    release_routed_window <= 2
                };
            invariant EmittedRepeatUsesOriginalPressTarget:
                if repeat_emitted == 1 {
                    press_forwarded + press_literal + press_local == 1 &&
                    repeat_routed_window == press_window
                } else {
                    repeat_routed_window == 0
                };
            invariant NoFabricatedReleaseTarget:
                if release_routed_window > 0 {
                    press_forwarded == 1 && release_emitted == 1 &&
                    release_routed_window == press_window
                } else {
                    release_routed_window == 0
                };
            bound StateBounds:
                phase <= 3 && tracker <= 4 && overlay_open <= 1 &&
                press_literal <= 1 && press_local <= 1 &&
                repeat_observed <= 1 && repeat_emitted <= 1 && release_emitted <= 1 &&
                untracked_release_swallowed <= 1 && focused_window > 0 &&
                focused_window <= 2 && press_window <= 2 &&
                repeat_routed_window <= 2 &&
                release_arrival_window <= 2 && release_routed_window <= 2;
        }
    }
}

/// Two-phase overlap handoff between native-update parent and child processes.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_overlap_handoff_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateOverlapHandoff {
            const Buggy = 0;
            var protocol = 0;
            var phase = 0;
            var parent_readers = 1;
            var child_readers = 0;
            var child_live = 0;
            var descendant_live = 0;
            var group_signaled = 0;
            var leader_dead_with_descendant = 0;
            var waited_before_group_signal = 0;
            var parent_parked = 0;
            var painted = 0;
            var proof_complete = 0;
            var proof_exact = 0;
            var sessions_exact = 1;
            var layout_exact = 1;
            var epoch_exact = 1;
            var teardown_allows = 1;
            var sessions_alive = 1;
            var pty_output_queued = 0;
            var input_queue_quiet = 1;
            var native_safe = 1;
            var commit_channel = 1;
            var arbiter = 0;
            var reject_contender = 0;
            var late_cancel = 0;
            var commit_admission_exact = 0;
            var commit_write_failed = 0;
            var commit = 0;
            var parent_exited = 0;
            var failure = 0;
            var child_killed = 0;
            var child_reaped = 0;
            var parent_resumed = 0;
            var parent_resumed_early = 0;
            var teardown_replayed = 0;
            var legacy_zero_history = 1;
            var legacy_strict_newer = 1;
            var legacy_ack = 0;
            var diagnostic_wake = 1;
            var launched_early = 0;
            var granted = 0;
            var retired = 0;
            var successor_exited = 0;
            var dialled = 0;

            action SelectLegacyBridge when (
                phase == 0 && protocol == 0 && launched_early == 0
            ) {
                protocol = 1;
                commit_channel = 0;
            }
            action ParkParentReaders when (
                phase == 0 && parent_readers == 1 && failure == 0 && retired == 0 &&
                (launched_early == 0 || dialled == 1)
            ) {
                phase = 1;
                parent_readers = 0;
                parent_parked = 1;
            }
            action SpawnReaderlessChild when (
                phase == 1 && parent_parked == 1 && child_live == 0 && launched_early == 0
            ) {
                phase = 2;
                child_live = 1;
                granted = 1;
            }
            action LaunchSuccessorBeforePark when (
                phase == 0 && protocol == 0 && child_live == 0 && launched_early == 0 &&
                retired == 0 && failure == 0
            ) {
                child_live = 1;
                launched_early = 1;
            }
            action SuccessorDials when (
                phase == 0 && launched_early == 1 && child_live == 1 && dialled == 0 &&
                failure == 0
            ) {
                dialled = 1;
            }
            action GrantDescriptorsToBootedSuccessor when (
                phase == 1 && parent_parked == 1 && launched_early == 1 &&
                child_live == 1 && granted == 0 && failure == 0
            ) {
                phase = 2;
                granted = 1;
            }
            action UnparkForRepark when (
                phase == 1 && parent_parked == 1 && launched_early == 1 &&
                granted == 0 && failure == 0
            ) {
                phase = 0;
                parent_parked = 0;
                parent_readers = 1;
            }
            action RevokeUngrantedSuccessor when (
                phase == 0 && launched_early == 1 && child_live == 1 &&
                granted == 0 && failure == 0
            ) {
                arbiter = 2;
                failure = 1;
            }
            action KillUngrantedSuccessor when (
                phase == 0 && launched_early == 1 && child_live == 1 &&
                granted == 0 && failure == 1 && arbiter == 2 && child_killed == 0
            ) {
                child_live = 0;
                descendant_live = 0;
                child_killed = 1;
                group_signaled = 1;
            }
            action SuccessorExitsOnRevoke when (
                phase == 0 && launched_early == 1 && child_live == 1 &&
                granted == 0 && failure == 1 && arbiter == 2 &&
                child_killed == 0 && successor_exited == 0
            ) {
                child_live = 0;
                successor_exited = 1;
            }
            action ReapUngrantedSuccessor when (
                phase == 0 && launched_early == 1 && granted == 0 &&
                child_live == 0 && child_reaped == 0 &&
                (successor_exited == 1 || (child_killed == 1 && group_signaled == 1))
            ) {
                child_reaped = 1;
            }
            action RetireUngrantedAttempt when (
                phase == 0 && launched_early == 1 && granted == 0 &&
                child_reaped == 1 && retired == 0
            ) {
                retired = 1;
            }
            action SettledStandDown when (phase == 0 && retired == 1) {
                retired = 1;
            }
            action SpawnProcessGroupDescendant when (
                phase > 1 && phase <= 3 && child_live == 1 &&
                descendant_live == 0 && arbiter == 0
            ) {
                descendant_live = 1;
            }
            action LeaderDiesLeavingLiveDescendant when (
                phase > 1 && phase <= 3 && child_live == 1 &&
                descendant_live == 1 && arbiter == 0 && failure == 0
            ) {
                child_live = 0;
                leader_dead_with_descendant = 1;
                failure = 1;
            }
            action ChildPaintsExactProof when (
                phase == 2 && child_live == 1 && failure == 0
            ) {
                phase = 3;
                painted = 1;
                proof_complete = 1;
                proof_exact = 1;
            }
            action ChildSendsPartialProof when (
                phase == 2 && child_live == 1 && failure == 0
            ) {
                phase = 3;
                painted = 1;
                failure = 1;
            }
            action ChildSendsMismatchedProof when (
                phase == 2 && child_live == 1 && failure == 0
            ) {
                phase = 3;
                painted = 1;
                proof_complete = 1;
                failure = 1;
            }
            action ActivityRevokesEpoch when (
                phase > 1 && phase <= 3 && epoch_exact == 1 &&
                arbiter == 0 && failure == 0
            ) {
                epoch_exact = 0;
                failure = 1;
            }
            action SessionsChange when (
                phase > 1 && phase <= 3 && sessions_exact == 1 &&
                arbiter == 0 && failure == 0
            ) {
                sessions_exact = 0;
                failure = 1;
            }
            action LayoutChanges when (
                phase > 1 && phase <= 3 && layout_exact == 1 &&
                arbiter == 0 && failure == 0
            ) {
                layout_exact = 0;
                failure = 1;
            }
            action DestructiveIntentRevokesCommit when (
                phase > 1 && phase <= 3 && teardown_allows == 1 &&
                arbiter == 0 && failure == 0
            ) {
                teardown_allows = 0;
                epoch_exact = 0;
                failure = 1;
            }
            action PtyOutputQueues when (
                phase == 3 && pty_output_queued == 0 && arbiter == 0
            ) {
                pty_output_queued = 1;
            }
            action PtySessionDies when (
                phase > 1 && phase <= 3 && sessions_alive == 1 &&
                arbiter == 0 && failure == 0
            ) {
                sessions_alive = 0;
                failure = 1;
            }
            action QueueHardwareInput when (
                phase == 3 && input_queue_quiet == 1 && arbiter <= 1
            ) {
                input_queue_quiet = 0;
            }
            action DrainQueuedHardwareInput when (
                phase == 3 && input_queue_quiet == 0 && arbiter <= 1
            ) {
                input_queue_quiet = 1;
            }
            action RevokeNativeSafety when (
                phase > 1 && phase <= 3 && native_safe == 1 &&
                arbiter == 0 && failure == 0
            ) {
                native_safe = 0;
                failure = 1;
            }
            action ParentReaderResumesBeforeCommit when (
                protocol == 0 && phase > 1 && phase <= 3 &&
                parent_parked == 1 && parent_readers == 0 &&
                arbiter == 0 && failure == 0
            ) {
                parent_parked = 0;
                parent_readers = 1;
                parent_resumed_early = 1;
                failure = 1;
            }
            action LoseCommitChannel when (
                protocol == 0 && phase > 1 && phase <= 3 &&
                commit_channel == 1 && arbiter == 0 && failure == 0
            ) {
                commit_channel = 0;
                failure = 1;
            }
            action LegacyPayloadIsScrolledOrAmbiguous when (
                protocol == 1 && phase > 1 && phase <= 3 &&
                legacy_zero_history == 1 && arbiter == 0 && failure == 0
            ) {
                legacy_zero_history = 0;
                failure = 1;
            }
            action LegacyChildIsNotStrictlyNewer when (
                protocol == 1 && phase > 1 && phase <= 3 &&
                legacy_strict_newer == 1 && arbiter == 0 && failure == 0
            ) {
                legacy_strict_newer = 0;
                failure = 1;
            }
            action ArmConcurrentRejectContender when (
                protocol == 0 && phase == 3 && arbiter == 0 && reject_contender == 0
            ) {
                reject_contender = 1;
            }
            action MainWinsCommitArbiter when (
                protocol == 0 && phase == 3 && child_live == 1 &&
                arbiter == 0 && commit_channel == 1 && parent_parked == 1 && painted == 1 &&
                proof_complete == 1 && proof_exact == 1 &&
                sessions_exact == 1 && layout_exact == 1 &&
                epoch_exact == 1 &&
                teardown_allows == 1 && sessions_alive == 1 &&
                input_queue_quiet == 1 && native_safe == 1 && failure == 0
            ) {
                arbiter = 1;
                commit_admission_exact = 1;
            }
            action CommitWithoutFreshExactProof when (
                Buggy == 1 && protocol == 0 && phase == 3 && child_live == 1 &&
                arbiter == 0 && commit_channel == 1 && parent_parked == 1 && painted == 1 &&
                sessions_exact == 1 && layout_exact == 1 &&
                teardown_allows == 1 && sessions_alive == 1 &&
                input_queue_quiet == 1 && native_safe == 1
            ) {
                arbiter = 1;
                commit_admission_exact = 1;
            }
            action WorkerWinsRejectArbiter when (
                protocol == 0 && phase > 1 && phase <= 3 && child_reaped == 0 &&
                arbiter == 0 &&
                (if failure == 1 { 1 } else { reject_contender }) == 1
            ) {
                arbiter = 2;
                failure = 1;
            }
            action LegacyRejectsBeforeAck when (
                protocol == 1 && phase > 1 && phase <= 3 && child_reaped == 0 &&
                arbiter == 0 && failure == 1
            ) {
                arbiter = 2;
            }
            action WorkerLosesRejectRace when (
                protocol == 0 && phase == 3 && arbiter == 1 && reject_contender == 1
            ) {
                late_cancel = 1;
            }
            action CommitModern when (
                protocol == 0 && phase == 3 && child_live == 1 &&
                arbiter == 1 && commit_admission_exact == 1 && input_queue_quiet == 1
            ) {
                phase = 4;
                arbiter = 3;
                commit = 1;
                parent_exited = 1;
            }
            action HeldClaimRefused when (
                protocol == 0 && phase == 3 && arbiter == 1 &&
                commit_admission_exact == 1 && commit == 0 && input_queue_quiet == 0
            ) {
                arbiter = 2;
                failure = 1;
                commit_admission_exact = 0;
            }
            action CommitWriteFails when (
                protocol == 0 && phase == 3 && child_live == 1 &&
                arbiter == 1 && commit_admission_exact == 1 && commit == 0
            ) {
                arbiter = 2;
                failure = 1;
                commit_admission_exact = 0;
                commit_write_failed = 1;
            }
            action LoseDiagnosticWake when (
                protocol == 0 && phase == 4 && commit == 1 && diagnostic_wake == 1
            ) {
                diagnostic_wake = 0;
            }
            action ReleaseModernReaders when (
                protocol == 0 && phase == 4 && commit == 1 && parent_exited == 1
            ) {
                phase = 5;
                child_readers = 1;
            }
            action AckGuardedLegacyBridge when (
                protocol == 1 && phase == 3 && child_live == 1 && painted == 1 &&
                proof_complete == 1 && proof_exact == 1 &&
                sessions_exact == 1 && layout_exact == 1 &&
                legacy_zero_history == 1 && legacy_strict_newer == 1 &&
                failure == 0
            ) {
                phase = 5;
                legacy_ack = 1;
                parent_exited = 1;
                child_readers = 1;
            }
            action AckInexactLegacyBridge when (
                Buggy == 1 && protocol == 1 && phase == 3 &&
                child_live == 1 && painted == 1 &&
                sessions_exact == 1 && layout_exact == 1
            ) {
                phase = 5;
                legacy_ack = 1;
                parent_exited = 1;
                child_readers = 1;
            }
            action BuggyExitIgnoringFailedCommitWrite when (
                Buggy == 1 && protocol == 0 && phase == 3 && child_live == 1 &&
                arbiter == 1 && commit_admission_exact == 1 && commit == 0
            ) {
                phase = 4;
                parent_exited = 1;
                commit_write_failed = 1;
            }
            action BuggyCommitOverQueuedInput when (
                Buggy == 1 && protocol == 0 && phase == 3 && child_live == 1 &&
                arbiter == 1 && commit_admission_exact == 1 && input_queue_quiet == 0
            ) {
                phase = 4;
                arbiter = 3;
                commit = 1;
                parent_exited = 1;
            }
            action BuggyReleaseReadersOnProof when (
                Buggy == 1 && phase == 3 && proof_complete == 1 && proof_exact == 1
            ) {
                child_readers = 1;
            }
            action KillRejectedChild when (
                phase > 1 && phase <= 6 && child_reaped == 0 && failure == 1 &&
                commit == 0 && legacy_ack == 0 && arbiter == 2 &&
                group_signaled == 0
            ) {
                phase = 6;
                child_live = 0;
                descendant_live = 0;
                child_killed = 1;
                group_signaled = 1;
            }
            action BuggySignalLeaderOnly when (
                Buggy == 1 && phase > 1 && phase <= 6 && child_reaped == 0 &&
                failure == 1 && commit == 0 && legacy_ack == 0 && arbiter == 2 &&
                group_signaled == 0
            ) {
                phase = 6;
                child_live = 0;
                child_killed = 1;
                group_signaled = 1;
            }
            action ReapKilledChild when (
                phase == 6 && child_killed == 1 && group_signaled == 1 &&
                child_reaped == 0
            ) {
                phase = 7;
                child_reaped = 1;
            }
            action ResumeParentAfterReap when (
                phase == 7 && child_reaped == 1
            ) {
                phase = 8;
                parent_readers = 1;
                parent_resumed = 1;
            }
            action BuggyResumeParentBeforeReap when (
                Buggy == 1 && phase == 6 && child_killed == 1 && child_reaped == 0
            ) {
                phase = 8;
                parent_readers = 1;
                parent_resumed = 1;
            }
            action BuggyWaitBeforeGroupSignal when (
                Buggy == 1 && phase > 1 && phase <= 6 && arbiter == 2 &&
                leader_dead_with_descendant == 1 && descendant_live == 1 &&
                child_live == 0 && child_reaped == 0 && group_signaled == 0
            ) {
                phase = 7;
                child_reaped = 1;
                waited_before_group_signal = 1;
            }
            action BuggyKillAfterCommitWin when (
                Buggy == 1 && phase == 3 && arbiter == 1 && child_live == 1
            ) {
                phase = 6;
                child_live = 0;
                descendant_live = 0;
                child_killed = 1;
                group_signaled = 1;
            }
            action BuggyGrantBeforePark when (
                Buggy == 1 && phase == 0 && launched_early == 1 && child_live == 1 &&
                granted == 0 && failure == 0
            ) {
                phase = 2;
                granted = 1;
            }
            action BuggyReleaseReadersUngranted when (
                Buggy == 1 && launched_early == 1 && child_live == 1 && granted == 0
            ) {
                child_readers = 1;
            }
            action BuggyRetireUngrantedWithSuccessorLive when (
                Buggy == 1 && phase == 0 && launched_early == 1 && child_live == 1 &&
                granted == 0 && failure == 1
            ) {
                retired = 1;
            }
            action ReplayDeferredTeardown when (
                phase == 8 && parent_resumed == 1 && teardown_allows == 0
            ) {
                phase = 9;
                teardown_replayed = 1;
            }
            action SettledSuccess when (phase == 5) {
                phase = 5;
            }
            action SettledRollback when (
                phase == 8 && teardown_allows == 1
            ) {
                phase = 8;
            }
            action SettledTeardown when (phase == 9) {
                phase = 9;
            }

            invariant AtMostOneReaderOwner:
                parent_readers + child_readers <= 1;
            invariant ChildReadersRequireIrreversibleAuthority:
                if child_readers == 1 {
                    parent_exited == 1 &&
                    (if commit == 1 { 1 } else { legacy_ack }) == 1
                } else {
                    child_readers == 0
                };
            invariant ModernCommitRequiresFreshExactProof:
                if commit == 1 {
                    protocol == 0 && parent_parked == 1 && painted == 1 &&
                    proof_complete == 1 && proof_exact == 1 && sessions_exact == 1 &&
                    layout_exact == 1 && epoch_exact == 1 && teardown_allows == 1 &&
                    sessions_alive == 1 && input_queue_quiet == 1 && native_safe == 1 &&
                    commit_channel == 1 && failure == 0 &&
                    commit_admission_exact == 1 && arbiter == 3
                } else {
                    commit == 0
                };
            invariant LegacyAckRequiresExactZeroHistoryBridge:
                if legacy_ack == 1 {
                    protocol == 1 && painted == 1 && proof_complete == 1 &&
                    proof_exact == 1 && sessions_exact == 1 && layout_exact == 1 &&
                    legacy_zero_history == 1 && legacy_strict_newer == 1 && failure == 0
                } else {
                    legacy_ack == 0
                };
            invariant ParentExitRequiresCommitOrLegacyAck:
                if parent_exited == 1 {
                    (if commit == 1 { 1 } else { legacy_ack }) == 1
                } else {
                    parent_exited == 0
                };
            invariant AtomicArbiterExcludesKillAfterCommitWin:
                if arbiter == 1 {
                    commit == 0 && child_killed == 0 && child_live == 1
                } else {
                    arbiter <= 3
                };
            invariant AtomicArbiterRejectWinnerForbidsCommit:
                if arbiter == 2 {
                    commit == 0 && parent_exited == 0 && commit_admission_exact == 0
                } else {
                    arbiter <= 3
                };
            invariant FailedCommitWriteTransfersNoAuthority:
                if commit_write_failed == 1 {
                    arbiter == 2 && commit == 0 && parent_exited == 0 &&
                    child_readers == 0
                } else {
                    commit_write_failed == 0
                };
            invariant KillRequiresRejectArbiter:
                if child_killed == 1 {
                    arbiter == 2
                } else {
                    child_killed == 0
                };
            invariant RollbackResumeRequiresKillAndReap:
                if parent_resumed == 1 {
                    child_killed == 1 && child_reaped == 1 && child_live == 0 &&
                    group_signaled == 1 && descendant_live == 0 &&
                    commit == 0 && legacy_ack == 0
                } else {
                    parent_resumed == 0
                };
            invariant ProcessGroupSignalPrecedesDirectChildReap:
                if child_reaped == 1 && successor_exited == 0 {
                    group_signaled == 1 && waited_before_group_signal == 0
                } else {
                    child_reaped == 0 || successor_exited == 1
                };
            invariant GroupSignalEliminatesLiveDescendants:
                if group_signaled == 1 {
                    descendant_live == 0
                } else {
                    group_signaled == 0
                };
            invariant ExitedLeaderWithDescendantCannotRollbackUnsignaled:
                if leader_dead_with_descendant == 1 && parent_resumed == 1 {
                    group_signaled == 1 && child_reaped == 1 && descendant_live == 0
                } else {
                    parent_resumed <= 1
                };
            invariant TeardownReplayRequiresReapAndResume:
                if teardown_replayed == 1 {
                    child_reaped == 1 && parent_resumed == 1 && parent_readers == 1
                } else {
                    teardown_replayed == 0
                };
            invariant FailedPreCommitChildStaysReaderless:
                if failure == 1 && commit == 0 && legacy_ack == 0 && child_reaped == 0 {
                    child_readers == 0 &&
                    parent_readers ==
                        if parent_resumed_early == 1 { 1 } else {
                            if granted == 0 { 1 } else { 0 }
                        }
                } else {
                    child_readers <= 1
                };
            invariant GrantRequiresParkedParent:
                if granted == 1 && parent_resumed_early == 0 {
                    parent_parked == 1
                } else {
                    granted <= 1
                };
            invariant UngrantedSuccessorNeverReads:
                if granted == 0 {
                    child_readers == 0
                } else {
                    granted == 1
                };
            invariant ReadersBesideLiveCandidateOnlyUngranted:
                if parent_readers == 1 && child_live == 1 && parent_resumed_early == 0 {
                    granted == 0
                } else {
                    parent_readers <= 1
                };
            invariant UngrantedRetireRequiresReap:
                if retired == 1 {
                    (child_killed == 1 || successor_exited == 1) &&
                    child_reaped == 1 && granted == 0 && child_live == 0
                } else {
                    retired == 0
                };
            invariant EarlyParentResumeCannotCommit:
                if parent_resumed_early == 1 {
                    parent_parked == 0 && commit == 0 && legacy_ack == 0 &&
                    child_readers == 0
                } else {
                    parent_resumed_early == 0
                };
        }
    }
}

/// Live-caret DFA for canonical profanity completion.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn exact_profanity_completion_model() -> Model {
    aterm_spec::ty_model! {
        ExactProfanityCompletion {
            const Buggy = 0;
            var phase = 0;
            var active = 0;
            var canonical_identity = 0;
            var episode = 0;

            action TypeF when (phase == 0) {
                phase = 1;
            }
            action TypeU when (phase == 1) {
                phase = 2;
            }
            action TypeC when (phase == 2) {
                phase = 3;
                active = if Buggy == 1 { 1 } else { 0 };
                canonical_identity = if Buggy == 1 { 1 } else { 0 };
                episode = if Buggy == 1 { 1 } else { 0 };
            }
            action TypeUpperFuc when (phase == 0) {
                phase = 3;
                active = if Buggy == 1 { 1 } else { 0 };
                canonical_identity = if Buggy == 1 { 1 } else { 0 };
                episode = if Buggy == 1 { 1 } else { 0 };
            }
            action TypeK when (phase == 3) {
                phase = 4;
                active = 1;
                canonical_identity = 1;
                episode = if Buggy == 1 { episode + 1 } else { 1 };
            }
            action TypeFixAfterF when (phase == 1) {
                phase = 5;
            }
            action TypeFutureAfterFu when (phase == 2) {
                phase = 5;
                active = 0;
                canonical_identity = 0;
                episode = 0;
            }
            action TypeFuchsiaAfterFuc when (phase == 3) {
                phase = 5;
                active = 0;
                canonical_identity = 0;
                episode = if Buggy == 1 { episode } else { 0 };
            }
            action TypeOtherAfterF when (phase == 1) {
                phase = 5;
            }
            action TypeOtherAfterFu when (phase == 2) {
                phase = 5;
                active = 0;
                canonical_identity = 0;
                episode = 0;
            }
            action SettleFuc when (phase == 3) {
                phase = 6;
                active = 0;
                canonical_identity = 0;
                episode = if Buggy == 1 { episode } else { 0 };
            }
            action SuppressedFucContext when (phase == 0) {
                phase = 5;
            }
            action IgnoredFuc when (phase == 0) {
                phase = 5;
            }
            action Done when (phase > 3) {
                phase = phase;
            }

            invariant EveryProperPrefixIsOrdinary:
                if phase <= 3 {
                    active == 0 && canonical_identity == 0 && episode == 0
                } else {
                    active <= 1
                };
            invariant ActivationRequiresCompleteFuck:
                if active == 1 {
                    phase == 4
                } else {
                    active == 0
                };
            invariant HarmlessAndSettledAreInactive:
                if phase > 4 {
                    active == 0 && canonical_identity == 0 && episode == 0
                } else {
                    phase <= 4
                };
            invariant CompletionCreatesExactlyOneEpisode:
                if phase == 4 {
                    active == 1 && canonical_identity == 1 && episode == 1
                } else {
                    episode <= 1
                };
        }
    }
}

/// Process-crash and interleaving model for fixed-path native update installation and boot-health confirmation.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_disk_transaction_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdateDiskTransaction {
            const Buggy = 0;
            const MaxCrashes = 3;
            var startup_phase = 0;
            var inherited_authority = 0;
            var inherited_env_live = 0;
            var boot_health_attempted = 0;
            var boot_health_observed = 0;
            var startup_returned = 0;
            var startup_deferred = 0;
            var disk_lane_ready = 0;
            var legacy_postswap = 0;
            var legacy_variant = 0;
            var ready_present = 0;
            var legacy_armed_current_trial = 0;
            var legacy_current_build_exact = 0;
            var legacy_current_commit_exact = 0;
            var legacy_trial_build_exact = 0;
            var legacy_trial_digest_exact = 0;
            var legacy_rollback_strict_old = 0;
            var modern_receipt_recovered = 0;
            var phase = 0;
            var installed = 1;
            var fixed = 0;
            var fixed_exact = 0;
            var old_build_exact = 1;
            var old_commit_exact = 1;
            var staged_identity = 2;
            var authorized_new = 2;
            var swap_identity = 0;
            var trial = 0;
            var receipt = 0;
            var receipt_exact = 0;
            var rollback_verified = 0;
            var fixed_refused = 0;
            var settled_without_revert = 0;
            var first_present_done = 0;
            var health_proved = 0;
            var proof_failed = 0;
            var disarmed = 0;
            var health_disarmed = 0;
            var disarm_failed = 0;
            var swap_failed = 0;
            var exec_failed = 0;
            var rollback_failed = 0;
            var gc = 0;
            var rollback_restored = 0;
            var rollback_gc = 0;
            var previous_receipt_present = 1;
            var previous_receipt_matches_old = 1;
            var previous_receipt_saved = 0;
            var old_receipt_restored = 0;
            var receipt_restore_failed = 0;
            var superseded_receipt_cleared = 0;
            var rejected = 0;
            var crashes = 0;

            action EnterLegacyPostSwapExact when (
                startup_phase == 0 && inherited_authority == 0 &&
                phase == 0 && installed == 1 && fixed == 0 && trial == 0
            ) {
                inherited_authority = 1;
                inherited_env_live = 1;
                legacy_postswap = 1;
                phase = 7;
                installed = 2;
                fixed = 1;
                fixed_exact = 1;
                trial = 1;
                swap_identity = 2;
                legacy_armed_current_trial = 1;
                legacy_current_build_exact = 1;
                legacy_current_commit_exact = 1;
                legacy_trial_build_exact = 1;
                legacy_trial_digest_exact = 1;
                legacy_rollback_strict_old = 1;
            }
            action LoseLegacyReexecAuthority when (
                legacy_postswap == 1 && startup_phase == 0 && legacy_variant == 0
            ) {
                inherited_authority = 0;
                inherited_env_live = 0;
                legacy_variant = 1;
            }
            action CorruptLegacyCurrentBuild when (
                legacy_postswap == 1 && startup_phase == 0 && legacy_variant == 0
            ) {
                legacy_current_build_exact = 0;
                legacy_variant = 2;
            }
            action CorruptLegacySentinel when (
                legacy_postswap == 1 && startup_phase == 0 && legacy_variant == 0
            ) {
                legacy_armed_current_trial = 0;
                legacy_variant = 9;
            }
            action CorruptLegacyCurrentCommit when (
                legacy_postswap == 1 && startup_phase == 0 && legacy_variant == 0
            ) {
                legacy_current_commit_exact = 0;
                legacy_variant = 3;
            }
            action CorruptLegacyTrialBuild when (
                legacy_postswap == 1 && startup_phase == 0 && legacy_variant == 0
            ) {
                legacy_trial_build_exact = 0;
                legacy_variant = 4;
            }
            action CorruptLegacyTrialDigest when (
                legacy_postswap == 1 && startup_phase == 0 && legacy_variant == 0
            ) {
                legacy_trial_digest_exact = 0;
                legacy_variant = 5;
            }
            action CorruptLegacyRollback when (
                legacy_postswap == 1 && startup_phase == 0 && legacy_variant == 0
            ) {
                legacy_rollback_strict_old = 0;
                legacy_variant = 6;
            }
            action SupplyModernReadyRecovery when (
                legacy_postswap == 1 && startup_phase == 0 && legacy_variant == 0
            ) {
                ready_present = 1;
                legacy_variant = 7;
            }
            action SupplyExistingExactReceipt when (
                legacy_postswap == 1 && startup_phase == 0 && legacy_variant == 0
            ) {
                phase = 4;
                receipt = 1;
                receipt_exact = 1;
                legacy_variant = 8;
            }
            action InheritMatchingAuthority when (
                startup_phase == 0 && inherited_authority == 0
            ) {
                inherited_authority = 1;
                inherited_env_live = 1;
            }
            action InheritMalformedAuthority when (
                startup_phase == 0 && inherited_authority == 0
            ) {
                inherited_authority = 2;
                inherited_env_live = 1;
            }
            action ConsumeStartupAuthority when (startup_phase == 0) {
                startup_phase = 1;
                inherited_env_live = 0;
            }
            action RecoverModernReceiptFromReady when (
                legacy_postswap == 1 && startup_phase == 1 && receipt == 0 &&
                ready_present == 1 &&
                legacy_armed_current_trial == 1 &&
                legacy_current_build_exact == 1 && legacy_current_commit_exact == 1 &&
                legacy_trial_build_exact == 1 && legacy_trial_digest_exact == 1 &&
                legacy_rollback_strict_old == 1
            ) {
                phase = 4;
                receipt = 1;
                receipt_exact = 1;
                rollback_verified = 1;
                boot_health_attempted = 1;
                modern_receipt_recovered = 1;
            }
            action AcceptExistingLegacyReceipt when (
                legacy_postswap == 1 && startup_phase == 1 && receipt == 1 &&
                receipt_exact == 1 && legacy_rollback_strict_old == 1
            ) {
                rollback_verified = 1;
                boot_health_attempted = 1;
            }
            action RefuseLegacyCurrentBuildMismatch when (
                legacy_postswap == 1 && startup_phase == 1 &&
                legacy_current_build_exact == 0
            ) {
                startup_phase = 3;
                startup_deferred = 1;
                boot_health_attempted = 1;
            }
            action RefuseLegacySentinelMismatch when (
                legacy_postswap == 1 && startup_phase == 1 &&
                legacy_armed_current_trial == 0
            ) {
                startup_phase = 3;
                startup_deferred = 1;
                boot_health_attempted = 1;
            }
            action RefuseLegacyCurrentCommitMismatch when (
                legacy_postswap == 1 && startup_phase == 1 &&
                legacy_current_commit_exact == 0
            ) {
                startup_phase = 3;
                startup_deferred = 1;
                boot_health_attempted = 1;
            }
            action RefuseLegacyTrialBuildMismatch when (
                legacy_postswap == 1 && startup_phase == 1 &&
                legacy_trial_build_exact == 0
            ) {
                startup_phase = 3;
                startup_deferred = 1;
                boot_health_attempted = 1;
            }
            action RefuseLegacyTrialDigestMismatch when (
                legacy_postswap == 1 && startup_phase == 1 &&
                legacy_trial_digest_exact == 0
            ) {
                startup_phase = 3;
                startup_deferred = 1;
                boot_health_attempted = 1;
            }
            action RefuseLegacyRollbackMismatch when (
                legacy_postswap == 1 && startup_phase == 1 &&
                legacy_rollback_strict_old == 0
            ) {
                startup_phase = 3;
                startup_deferred = 1;
                boot_health_attempted = 1;
            }
            action RefuseLegacyMissingRecoveryEvidence when (
                legacy_postswap == 1 && startup_phase == 1 &&
                receipt == 0 && ready_present == 0
            ) {
                startup_phase = 3;
                startup_deferred = 1;
                boot_health_attempted = 1;
            }
            action BuggyReturnInheritedAuthority when (
                startup_phase == 0 && inherited_authority > 0 && Buggy == 1
            ) {
                startup_phase = 3;
                startup_returned = 1;
            }
            action BuggyDeferLegacyAndDisarmTrial when (
                Buggy == 1 && legacy_postswap == 1 && startup_phase == 1 &&
                receipt == 0 && ready_present == 0
            ) {
                startup_phase = 3;
                startup_deferred = 1;
                boot_health_attempted = 1;
                trial = 0;
                disarmed = 1;
            }
            action ObserveBootHealth when (
                startup_phase == 1 && inherited_env_live == 0 &&
                (if legacy_postswap == 1 {
                    receipt_exact + rollback_verified
                } else { 2 }) == 2
            ) {
                startup_phase = 2;
                boot_health_attempted = 1;
                boot_health_observed = 1;
            }
            action ReturnAfterObservedAuthority when (
                startup_phase == 2 && inherited_authority > 0 &&
                boot_health_observed == 1 && inherited_env_live == 0 &&
                (if legacy_postswap == 1 {
                    receipt_exact + rollback_verified
                } else { 2 }) == 2
            ) {
                startup_phase = 3;
                startup_returned = 1;
            }
            action ReturnAfterRecoveredLegacy when (
                startup_phase == 2 && legacy_postswap == 1 &&
                receipt_exact == 1 && rollback_verified == 1 &&
                boot_health_observed == 1 && inherited_env_live == 0
            ) {
                startup_phase = 3;
                startup_returned = 1;
            }
            action EnterDiskLane when (
                startup_phase == 2 && inherited_authority == 0 &&
                legacy_postswap == 0 && boot_health_observed == 1 &&
                inherited_env_live == 0
            ) {
                startup_phase = 3;
                disk_lane_ready = 1;
            }
            action StartupReturned when (
                startup_phase == 3 && startup_returned == 1
            ) {
                startup_phase = 3;
            }
            action StartupDeferred when (
                startup_phase == 3 && startup_deferred == 1
            ) {
                startup_phase = 3;
            }
            action CorruptOldCommit when (
                disk_lane_ready == 1 && phase == 0 && old_build_exact == 1 &&
                old_commit_exact == 1
            ) {
                old_commit_exact = 0;
            }
            action CorruptPreviousReceipt when (
                disk_lane_ready == 1 && phase == 0 &&
                previous_receipt_present == 1 && previous_receipt_matches_old == 1 &&
                previous_receipt_saved == 0
            ) {
                previous_receipt_matches_old = 0;
            }
            action RemovePreviousReceipt when (
                disk_lane_ready == 1 && phase == 0 && previous_receipt_present == 1 &&
                previous_receipt_saved == 0
            ) {
                previous_receipt_present = 0;
                previous_receipt_matches_old = 0;
            }
            action PrepareFixedNew when (
                disk_lane_ready == 1 && phase == 0 && installed == 1 &&
                old_build_exact == 1 && old_commit_exact == 1 &&
                staged_identity == authorized_new
            ) {
                phase = 1;
                fixed = 2;
                fixed_exact = 1;
                disarmed = 0;
                disarm_failed = 0;
                swap_failed = 0;
                previous_receipt_saved = if previous_receipt_present == 1 &&
                    previous_receipt_matches_old == 1 {
                    1
                } else { 0 };
            }
            action PrepareFromBuildOnlyOld when (
                Buggy == 1 && disk_lane_ready == 1 && phase == 0 &&
                installed == 1 && old_build_exact == 1 && old_commit_exact == 0 &&
                staged_identity == authorized_new
            ) {
                phase = 1;
                fixed = 2;
                fixed_exact = 1;
                disarmed = 0;
                disarm_failed = 0;
                swap_failed = 0;
                previous_receipt_saved = if previous_receipt_present == 1 &&
                    previous_receipt_matches_old == 1 {
                    1
                } else { 0 };
            }
            action PrepareSavingMismatchedPreviousReceipt when (
                Buggy == 1 && disk_lane_ready == 1 && phase == 0 &&
                installed == 1 && old_build_exact == 1 && old_commit_exact == 1 &&
                staged_identity == authorized_new &&
                previous_receipt_present == 1 && previous_receipt_matches_old == 0
            ) {
                phase = 1;
                fixed = 2;
                fixed_exact = 1;
                disarmed = 0;
                disarm_failed = 0;
                swap_failed = 0;
                previous_receipt_saved = 1;
            }
            action RejectInvalidOld when (
                phase == 0 && old_build_exact == 1 && old_commit_exact == 0
            ) {
                phase = 6;
                rejected = 1;
            }
            action CrashPrepared when (
                phase == 1 && crashes <= MaxCrashes - 1
            ) {
                crashes = crashes + 1;
            }
            action ArmExactTrial when (
                phase == 1 && fixed == 2 && fixed_exact == 1
            ) {
                phase = 2;
                trial = 1;
            }
            action CrashArmedBeforeSwap when (
                phase == 2 && crashes <= MaxCrashes - 1
            ) {
                crashes = crashes + 1;
            }
            action SupersedeStagedIdentity when (
                phase == 2 && staged_identity == authorized_new &&
                swap_failed == 0 && disarm_failed == 0
            ) {
                staged_identity = 3;
            }
            action RecoverSupersededBeforeSwap when (
                phase == 2 && staged_identity == 3 && trial == 1 &&
                fixed == 2 && swap_failed == 0 &&
                disarm_failed == 0
            ) {
                phase = 0;
                fixed = 0;
                fixed_exact = 0;
                trial = 0;
                disarmed = 1;
                previous_receipt_saved = 0;
            }
            action RetireSuperseded when (
                phase == 0 && staged_identity == 3
            ) {
                phase = 6;
                rejected = 1;
            }
            action AtomicSwap when (
                phase == 2 && trial == 1 && fixed == 2 && fixed_exact == 1 &&
                swap_failed == 0 && disarm_failed == 0 &&
                staged_identity == authorized_new
            ) {
                phase = 3;
                installed = authorized_new;
                fixed = 1;
                fixed_exact = 1;
                swap_identity = authorized_new;
                first_present_done = 0;
            }
            action SwapSupersededStagedIdentity when (
                Buggy == 1 && phase == 2 && trial == 1 && fixed == 2 &&
                fixed_exact == 1 && swap_failed == 0 && disarm_failed == 0 &&
                staged_identity == 3
            ) {
                phase = 3;
                installed = staged_identity;
                fixed = 1;
                fixed_exact = 1;
                swap_identity = staged_identity;
                first_present_done = 0;
            }
            action BuggyWriteReceiptBeforeSwap when (
                Buggy == 1 && phase == 2 && trial == 1 && fixed == 2 &&
                fixed_exact == 1 && receipt == 0
            ) {
                receipt = 1;
                receipt_exact = 1;
            }
            action BuggyTwoRenameSwapFailsMidway when (
                Buggy == 1 && phase == 2 && trial == 1 && fixed == 2 &&
                fixed_exact == 1 && swap_failed == 0 && disarm_failed == 0 &&
                staged_identity == authorized_new
            ) {
                installed = authorized_new;
                fixed = 0;
                fixed_exact = 0;
                swap_failed = 1;
            }
            action SwapFailsAndDisarms when (
                phase == 2 && trial == 1 && fixed == 2 && fixed_exact == 1 &&
                swap_failed == 0 && disarm_failed == 0
            ) {
                phase = 0;
                fixed = 0;
                fixed_exact = 0;
                trial = 0;
                disarmed = 1;
                swap_failed = 1;
                previous_receipt_saved = 0;
            }
            action SwapFailsDisarmFails when (
                phase == 2 && trial == 1 && fixed == 2 && fixed_exact == 1 &&
                swap_failed == 0 && disarm_failed == 0
            ) {
                disarm_failed = 1;
                swap_failed = 1;
            }
            action RecoverFailedSwapTrial when (
                phase == 2 && installed == 1 && fixed == 2 &&
                fixed_exact == 1 && trial == 1 && swap_failed == 1 &&
                disarm_failed == 1
            ) {
                phase = 0;
                fixed = 0;
                fixed_exact = 0;
                trial = 0;
                disarmed = 1;
                disarm_failed = 0;
                swap_failed = 0;
                previous_receipt_saved = 0;
            }
            action CrashAfterSwapBeforeReceipt when (
                phase == 3 && crashes <= MaxCrashes - 1
            ) {
                phase = 7;
                crashes = crashes + 1;
            }
            action RecordExactReceipt when (
                phase == 3 && installed == authorized_new &&
                swap_identity == authorized_new && trial == 1
            ) {
                phase = 4;
                receipt = 1;
                receipt_exact = 1;
            }
            action RecoverExactReceipt when (
                phase == 7 && legacy_postswap == 0 &&
                installed == authorized_new && fixed == 1 &&
                fixed_exact == 1 && trial == 1
            ) {
                phase = 4;
                receipt = 1;
                receipt_exact = 1;
            }
            action VerifyExactRollback when (
                phase == 4 && fixed == 1 && fixed_exact == 1 &&
                inherited_env_live == 0 && fixed_refused == 0
            ) {
                rollback_verified = 1;
            }
            action RunningVerifierRefusesRetainedOld when (
                phase == 4 && legacy_postswap == 0 && installed == authorized_new &&
                trial == 1 && fixed == 1 && fixed_exact == 1 &&
                rollback_verified == 0 && fixed_refused == 0
            ) {
                fixed_refused = 1;
            }
            action CrashAfterReceipt when (
                phase == 4 && crashes <= MaxCrashes - 1
            ) {
                crashes = crashes + 1;
                first_present_done = 0;
                health_proved = 0;
            }
            action PresentInstalledUi when (
                phase == 4 && installed == authorized_new &&
                receipt_exact == 1 && first_present_done == 0 && startup_phase == 3
            ) {
                first_present_done = 1;
            }
            action HealthProofFails when (
                phase == 4 && receipt_exact == 1 && proof_failed == 0 &&
                first_present_done == 1
            ) {
                proof_failed = 1;
            }
            action DiscardRollbackAfterFailedProof when (
                Buggy == 1 && phase == 4 && receipt_exact == 1 &&
                proof_failed == 0 && first_present_done == 1
            ) {
                proof_failed = 1;
                gc = 1;
                fixed = 0;
            }
            action RetryHealthProof when (
                phase == 4 && proof_failed == 1 && gc == 0 &&
                fixed == 1 && trial == 1
            ) {
                proof_failed = 0;
            }
            action ProveInstalledHealth when (
                phase == 4 && receipt == 1 && receipt_exact == 1 &&
                (rollback_verified == 1 || fixed_refused == 1) && proof_failed == 0 &&
                first_present_done == 1
            ) {
                health_proved = 1;
            }
            action DisarmTrial when (
                phase == 4 && trial == 1 && health_proved == 1
            ) {
                phase = 5;
                trial = 0;
                disarmed = 1;
                health_disarmed = 1;
                disarm_failed = 0;
            }
            action DisarmBeforeHealthProof when (
                Buggy == 1 && phase == 4 && trial == 1 && health_proved == 0
            ) {
                phase = 5;
                trial = 0;
                disarmed = 1;
                health_disarmed = 1;
                disarm_failed = 0;
            }
            action DisarmHealthTrialFails when (
                phase == 4 && health_proved == 1 && trial == 1 &&
                first_present_done == 1
            ) {
                disarm_failed = 1;
            }
            action CrashAfterDisarm when (
                phase == 5 && crashes <= MaxCrashes - 1
            ) {
                crashes = crashes + 1;
            }
            action GarbageCollectRollback when (
                phase == 5 && health_proved == 1 && disarmed == 1 &&
                receipt_exact == 1 && (rollback_verified == 1 || fixed_refused == 1) &&
                fixed == 1
            ) {
                phase = 6;
                fixed = 4;
                fixed_exact = 0;
                gc = 1;
            }
            action ReleaseKeptPredecessor when (
                phase == 6 && fixed == 4 && trial == 0
            ) {
                fixed = 0;
            }
            action BuggyKeepPredecessorBeforeDisarm when (
                Buggy == 1 && phase == 4 && trial == 1 && health_proved == 1 && fixed == 1
            ) {
                fixed = 4;
            }
            action SettleWithoutRevert when (
                phase == 4 && trial == 1 && installed == authorized_new &&
                swap_identity == authorized_new && receipt == 1 && receipt_exact == 1 &&
                fixed == 1 && fixed_refused == 1 && rollback_verified == 0 &&
                startup_phase == 3
            ) {
                phase = 6;
                trial = 0;
                disarmed = 1;
                disarm_failed = 0;
                fixed = 0;
                fixed_exact = 0;
                gc = 1;
                settled_without_revert = 1;
            }
            action BuggySettleOverVerifiedRollback when (
                Buggy == 1 && phase == 4 && trial == 1 && installed == authorized_new &&
                receipt_exact == 1 && fixed == 1 && fixed_refused == 0 &&
                rollback_verified == 1 && health_proved == 0 && startup_phase == 3
            ) {
                phase = 6;
                trial = 0;
                disarmed = 1;
                disarm_failed = 0;
                fixed = 0;
                fixed_exact = 0;
                gc = 1;
                settled_without_revert = 1;
            }
            action DetectCrashLoop when (
                phase == 4 && trial == 1 && receipt_exact == 1 &&
                rollback_verified == 1 && fixed == 1 && health_proved == 0 &&
                startup_phase == 3
            ) {
                phase = 8;
            }
            action ExecFails when (
                phase == 4 && trial == 1 && receipt_exact == 1 &&
                rollback_verified == 1 && fixed == 1 &&
                first_present_done == 0 && health_proved == 0 && startup_phase == 3
            ) {
                phase = 8;
                exec_failed = 1;
            }
            action RestoreExactOldFails when (
                phase == 8 && trial == 1 && installed == 2 && fixed == 1 &&
                fixed_exact == 1 && rollback_verified == 1
            ) {
                rollback_failed = 1;
            }
            action BuggyRestoreFailureDisarmsTrial when (
                Buggy == 1 && phase == 8 && trial == 1 && installed == 2 &&
                fixed == 1 && fixed_exact == 1 && rollback_verified == 1
            ) {
                rollback_failed = 1;
                trial = 0;
                disarmed = 1;
            }
            action BuggyGcRollbackAfterExecFailure when (
                Buggy == 1 && phase == 8 && exec_failed == 1 &&
                rollback_restored == 0 && fixed == 1
            ) {
                fixed = 0;
                fixed_exact = 0;
                gc = 1;
            }
            action BuggyRestoreUnverifiedRollback when (
                Buggy == 1 && phase == 4 && trial == 1 && receipt_exact == 1 &&
                fixed == 1 && rollback_verified == 0 && health_proved == 0 &&
                startup_phase == 3
            ) {
                phase = 9;
                installed = 1;
                fixed = 2;
                rollback_restored = 1;
                first_present_done = 0;
            }
            action RestoreExactOld when (
                phase == 8 && trial == 1 && fixed == 1 && fixed_exact == 1 &&
                rollback_verified == 1
            ) {
                phase = 9;
                installed = 1;
                fixed = 2;
                rollback_restored = 1;
                rollback_failed = 0;
                first_present_done = 0;
            }
            action DisarmRestoredTrialFails when (
                phase == 9 && installed == 1 && fixed == 2 &&
                rollback_restored == 1 && trial == 1
            ) {
                disarm_failed = 1;
            }
            action BuggyGcRestoredBeforeDisarm when (
                Buggy == 1 && phase == 9 && installed == 1 && fixed == 2 &&
                rollback_restored == 1 && trial == 1
            ) {
                fixed = 0;
                fixed_exact = 0;
                rollback_gc = 1;
            }
            action DisarmRestoredTrialAndRestoreBoundReceipt when (
                phase == 9 && installed == 1 && fixed == 2 &&
                rollback_restored == 1 && trial == 1 &&
                previous_receipt_saved == 1
            ) {
                phase = 6;
                fixed = 0;
                fixed_exact = 0;
                trial = 0;
                receipt = 0;
                receipt_exact = 0;
                disarmed = 1;
                disarm_failed = 0;
                rollback_gc = 1;
                old_receipt_restored = 1;
            }
            action DisarmRestoredTrialAndClearUnboundReceipt when (
                phase == 9 && installed == 1 && fixed == 2 &&
                rollback_restored == 1 && trial == 1 &&
                previous_receipt_saved == 0
            ) {
                phase = 6;
                fixed = 0;
                fixed_exact = 0;
                trial = 0;
                receipt = 0;
                receipt_exact = 0;
                disarmed = 1;
                disarm_failed = 0;
                rollback_gc = 1;
                superseded_receipt_cleared = 1;
            }
            action DisarmRestoredTrialReceiptRestoreFailsClosed when (
                phase == 9 && installed == 1 && fixed == 2 &&
                rollback_restored == 1 && trial == 1 &&
                previous_receipt_saved == 1
            ) {
                phase = 6;
                fixed = 0;
                fixed_exact = 0;
                trial = 0;
                receipt = 0;
                receipt_exact = 0;
                disarmed = 1;
                disarm_failed = 0;
                rollback_gc = 1;
                receipt_restore_failed = 1;
                superseded_receipt_cleared = 1;
            }
            action KeepSupersededReceiptAfterRestoreFailure when (
                Buggy == 1 && phase == 9 && installed == 1 && fixed == 2 &&
                rollback_restored == 1 && trial == 1 &&
                previous_receipt_saved == 1
            ) {
                phase = 6;
                fixed = 0;
                fixed_exact = 0;
                trial = 0;
                disarmed = 1;
                disarm_failed = 0;
                rollback_gc = 1;
                receipt_restore_failed = 1;
                superseded_receipt_cleared = 0;
            }
            action Done when (phase == 6) {
                phase = 6;
            }

            invariant InstalledBundleNeverMissing:
                installed > 0 && installed <= 2;
            invariant InheritedAuthorityClearedBeforeVerification:
                if (if disk_lane_ready == 1 { 1 } else { startup_returned }) == 1 {
                    inherited_env_live == 0
                } else {
                    inherited_env_live <= 1
                };
            invariant BootHealthObservedBeforeStartupVerdict:
                if (if disk_lane_ready == 1 { 1 } else { startup_returned }) == 1 {
                    boot_health_observed == 1
                } else {
                    boot_health_observed <= 1
                };
            invariant DiskWorkFollowsStartupObservation:
                if legacy_postswap == 1 {
                    if startup_phase <= 2 {
                        installed == 2 && fixed == 1 && trial == 1 &&
                        disk_lane_ready == 0
                    } else {
                        disk_lane_ready == 0
                    }
                } else {
                    if (if phase > 0 { 1 } else { if fixed > 0 { 1 } else { trial } }) == 1 {
                        disk_lane_ready == 1 && boot_health_observed == 1 &&
                        inherited_env_live == 0
                    } else {
                        disk_lane_ready <= 1
                    }
                };
            invariant LegacyStartupReturnRequiresReceiptProof:
                if legacy_postswap == 1 && startup_returned == 1 {
                    if rollback_restored == 1 {
                        rollback_verified == 1 && boot_health_attempted == 1 &&
                        boot_health_observed == 1 && inherited_env_live == 0
                    } else {
                        receipt_exact == 1 && rollback_verified == 1 &&
                        boot_health_attempted == 1 && boot_health_observed == 1 &&
                        inherited_env_live == 0
                    }
                } else {
                    startup_returned <= 1
                };
            invariant LegacyRefusalPreservesRecoveryAuthority:
                if legacy_postswap == 1 && startup_deferred == 1 {
                    trial == 1 && installed == 2 && fixed == 1 && gc == 0 &&
                    receipt == 0 &&
                    boot_health_attempted == 1 && inherited_env_live == 0
                } else {
                    startup_deferred <= 1
                };
            invariant ModernReadyRecoveryIsExact:
                if modern_receipt_recovered == 1 {
                    if rollback_restored == 1 {
                        if rollback_gc == 1 { receipt_exact == 0 } else { receipt_exact == 1 }
                    } else {
                        receipt_exact == 1
                    }
                } else {
                    modern_receipt_recovered == 0
                };
            invariant LegacyReceiptPrecedesPresentAndDisarm:
                if legacy_postswap == 1 && (if first_present_done == 1 {
                    1
                } else { health_disarmed }) == 1 {
                    receipt_exact == 1 && rollback_verified == 1 &&
                    startup_returned == 1
                } else {
                    first_present_done <= 1
                };
            invariant PreparedRequiresExactOldIdentity:
                if phase > 0 && rejected == 0 {
                    old_build_exact == 1 && old_commit_exact == 1
                } else {
                    old_build_exact == 1
                };
            invariant SwapMatchesAuthorizedNew:
                if swap_identity > 0 {
                    swap_identity == authorized_new
                } else {
                    installed == 1
                };
            invariant ArmedNewRetainsExactRollback:
                if trial == 1 && installed == 2 {
                    fixed == 1 && fixed_exact == 1
                } else {
                    (if installed == 1 { 1 } else { if trial == 0 { 1 } else { 0 } }) == 1
                };
            invariant ReceiptBindsExactNewIdentity:
                if receipt == 1 {
                    receipt_exact == 1 && swap_identity == authorized_new &&
                    (if rollback_restored == 1 {
                        installed == 1
                    } else {
                        installed == authorized_new
                    })
                } else {
                    receipt_exact == 0
                };
            invariant GarbageCollectionRequiresProofAndDisarm:
                if gc == 1 {
                    disarmed == 1 && trial == 0 && receipt_exact == 1 &&
                    (fixed == 0 || fixed == 4) &&
                    (if fixed_refused == 1 {
                        rollback_verified == 0
                    } else {
                        health_proved == 1 && health_disarmed == 1 &&
                        first_present_done == 1 && rollback_verified == 1
                    })
                } else {
                    disarmed <= 1
                };
            invariant KeptPredecessorIsNoAuthority:
                if fixed == 4 {
                    phase == 6 && gc == 1 && trial == 0 && disarmed == 1 &&
                    health_proved == 1 && fixed_exact == 0
                } else {
                    fixed <= 3
                };
            invariant SettleWithoutRevertOnlyOverAnUnrevertiblePredecessor:
                if settled_without_revert == 1 {
                    fixed_refused == 1 && rollback_verified == 0 &&
                    rollback_restored == 0 && installed == authorized_new &&
                    health_disarmed == 0
                } else {
                    settled_without_revert == 0
                };
            invariant HealthDisarmRequiresFirstPresent:
                if health_disarmed == 1 {
                    first_present_done == 1 && health_proved == 1
                } else {
                    health_proved <= 1
                };
            invariant FailedProofPreservesRecoveryAuthority:
                if proof_failed == 1 && trial == 1 {
                    gc == 0 &&
                    (if rollback_restored == 1 {
                        installed == 1
                    } else {
                        fixed == 1 && fixed_exact == 1
                    })
                } else {
                    proof_failed <= 1
                };
            invariant FailedDisarmPreservesRecoveryAuthority:
                if disarm_failed == 1 {
                    trial == 1 && gc == 0 && fixed_exact == 1 &&
                    (if installed == 2 { fixed == 1 } else { fixed == 2 })
                } else {
                    disarm_failed == 0
                };
            invariant FailedSwapNeverReplacesOld:
                if swap_failed == 1 && trial == 1 {
                    installed == 1 && fixed == 2 && fixed_exact == 1
                } else {
                    swap_failed <= 1
                };
            invariant FailedRollbackPreservesRecoveryAuthority:
                if rollback_failed == 1 {
                    phase == 8 && trial == 1 && installed == 2 &&
                    fixed == 1 && fixed_exact == 1 && receipt_exact == 1 &&
                    gc == 0 && rollback_gc == 0
                } else {
                    rollback_failed == 0
                };
            invariant ExecFailureCannotGcBeforeRestore:
                if exec_failed == 1 && rollback_restored == 0 {
                    trial == 1 && installed == 2 && fixed == 1 &&
                    fixed_exact == 1 && gc == 0 && rollback_gc == 0
                } else {
                    exec_failed <= 1
                };
            invariant CrashLoopRestoreUsesExactOld:
                if rollback_restored == 1 {
                    installed == 1 && rollback_verified == 1
                } else {
                    rollback_gc == 0
                };
            invariant RollbackGcRequiresRestoreAndDisarm:
                if rollback_gc == 1 {
                    rollback_restored == 1 && installed == 1 &&
                    disarmed == 1 && trial == 0 && fixed == 0
                } else {
                    rejected <= 1
                };
            invariant SavedPreviousReceiptBindsSealedOld:
                if previous_receipt_saved == 1 {
                    previous_receipt_present == 1 && previous_receipt_matches_old == 1
                } else {
                    previous_receipt_saved == 0
                };
            invariant RestoredOldReceiptWasBound:
                if old_receipt_restored == 1 {
                    previous_receipt_saved == 1 && previous_receipt_matches_old == 1 &&
                    rollback_restored == 1 && installed == 1 && receipt == 0
                } else {
                    old_receipt_restored == 0
                };
            invariant RestoreFailureClearsSupersededNewReceipt:
                if receipt_restore_failed == 1 {
                    rollback_restored == 1 && installed == 1 && receipt == 0 &&
                    receipt_exact == 0 && superseded_receipt_cleared == 1
                } else {
                    receipt_restore_failed == 0
                };
            invariant MismatchedReceiptNeverBecomesOldAuthority:
                if previous_receipt_matches_old == 0 {
                    previous_receipt_saved == 0 && old_receipt_restored == 0
                } else {
                    old_receipt_restored <= 1
                };
            bound CrashBudgetBounded: crashes <= MaxCrashes;
        }
    }
}

/// Bounded scroll-cursor clamping across wheel, arrow, page, and accessibility motions.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn settings_page_scroll_model() -> Model {
    aterm_spec::ty_model! {
        SettingsPageScroll {
            const Buggy = 0;
            const Max = 3;
            var limit = 0;
            var cursor = 0;
            var target = 0;
            action GrowLimit when (limit <= Max - 1) {
                limit = limit + 1;
            }
            action ShrinkLimit when (limit > 0) {
                cursor = if cursor <= limit - 1 { cursor } else { limit - 1 };
                limit = limit - 1;
            }
            action ChooseTarget when (target <= Max) {
                target = target + 1;
            }
            action RowBack {
                cursor = if cursor > 0 { cursor - 1 } else { 0 };
            }
            action RowForward {
                cursor = if Buggy == 1 {
                    cursor + 1
                } else if cursor <= limit - 1 {
                    cursor + 1
                } else {
                    limit
                };
            }
            action Absolute {
                cursor = if target <= limit {
                    target
                } else if Buggy == 1 {
                    target
                } else {
                    limit
                };
            }
            action ScrollBackward {
                cursor = if cursor > 1 { cursor - 2 } else { 0 };
            }
            action ScrollForward {
                cursor = if Buggy == 1 {
                    cursor + 2
                } else if cursor <= limit - 2 {
                    cursor + 2
                } else {
                    limit
                };
            }
            invariant CursorBounded: cursor <= limit;
            bound LimitBounded: limit <= Max;
            bound TargetBounded: target <= Max + 1;
        }
    }
}

/// Serialized fail-closed video recording and publication lifecycle.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn video_recording_lifecycle_model() -> Model {
    aterm_spec::ty_model! {
        VideoRecordingLifecycle {
            const Buggy = 0;
            var phase = 0;
            var glass = 0;
            var pace = 0;
            var mode = 0;
            var timer = 0;
            var translucent = 0;
            var recording_slot = 0;
            var export_permit = 0;
            var private_dirs = 0;
            var published = 0;
            var cancel_state = 0;

            action AttachGlass when (phase == 0) { glass = 1; }
            action DetachGlass when (phase == 0) { glass = 0; }
            action RequestPaced when (phase == 0) { pace = 1; }
            action RequestUnpaced when (phase == 0) { pace = 0; }
            action MakeOpaque when (phase == 0 && translucent == 1) {
                translucent = 0;
            }
            action Reserve when (
                phase == 0 && recording_slot == 0 && export_permit == 0
            ) {
                phase = 1;
                mode = 0;
                timer = 0;
                recording_slot = 1;
                private_dirs = 1;
                published = 0;
                cancel_state = 0;
            }
            action BeginOnGlass when (
                phase == 1 && glass == 1 && translucent == 0
            ) {
                phase = 2;
                mode = 1;
                timer = pace;
            }
            action BeginHeadless when (phase == 1 && glass == 0) {
                phase = 2;
                mode = 2;
                timer = 1;
            }
            action BuggyBeginOnGlassOffscreen when (
                Buggy == 1 && phase == 1 && glass == 1
            ) {
                phase = 2;
                mode = 2;
                timer = 1;
            }
            action Tick when (phase == 2 && timer == 1) {
                timer = 1;
            }
            action BeginExport when (phase == 2) {
                phase = 3;
                mode = 0;
                timer = 0;
                recording_slot = 0;
                export_permit = 1;
            }
            action AuthorizeCommit when (
                phase == 3 && cancel_state == 0
            ) {
                cancel_state = 2;
            }
            action CancelAfterAuthorization when (
                phase == 3 && cancel_state == 2
            ) {
                cancel_state = 2;
            }
            action PublishSuccess when (
                phase == 3 && cancel_state == 2
            ) {
                phase = 0;
                mode = 0;
                timer = 0;
                export_permit = 0;
                private_dirs = 0;
                published = 1;
            }
            action BuggyPublishWithoutAuthorization when (
                Buggy == 1 && phase == 3 && cancel_state == 0
            ) {
                phase = 0;
                mode = 0;
                timer = 0;
                export_permit = 0;
                private_dirs = 0;
                published = 1;
            }
            action RejectBegin when (phase == 1) {
                phase = 0;
                mode = 0;
                timer = 0;
                recording_slot = 0;
                private_dirs = 0;
                published = 0;
                cancel_state = 1;
            }
            action CancelLive when (phase > 0 && cancel_state == 0) {
                phase = 0;
                mode = 0;
                timer = 0;
                recording_slot = 0;
                export_permit = 0;
                private_dirs = 0;
                published = 0;
                cancel_state = 1;
            }
            action Fail when (phase > 0) {
                phase = 0;
                mode = 0;
                timer = 0;
                recording_slot = 0;
                export_permit = 0;
                private_dirs = 0;
                published = 0;
                cancel_state = 1;
            }
            action OwnerLost when (phase > 0) {
                phase = 0;
                mode = 0;
                timer = 0;
                recording_slot = 0;
                export_permit = 0;
                private_dirs = 0;
                published = 0;
                cancel_state = 1;
            }
            action BuggyStrandPrivateDirOnCleanup when (
                Buggy == 1 && phase > 0
            ) {
                phase = 0;
                mode = 0;
                timer = 0;
                recording_slot = 0;
                export_permit = 0;
                private_dirs = 1;
                published = 0;
                cancel_state = 1;
            }
            action MakeTapTranslucent when (
                phase == 2 && mode == 1 && translucent == 0
            ) {
                translucent = 1;
                phase = 0;
                mode = 0;
                timer = 0;
                recording_slot = 0;
                private_dirs = 0;
                published = 0;
                cancel_state = 1;
            }
            action BuggyRetainTapWhenTranslucent when (
                Buggy == 1 && phase == 2 && mode == 1 && translucent == 0
            ) {
                translucent = 1;
            }
            action BuggyStartSecondWhileExporting when (
                Buggy == 1 && phase == 3 && export_permit == 1
            ) {
                recording_slot = 1;
                private_dirs = 2;
            }
            action BuggyExportRetainsVirtualTarget when (
                Buggy == 1 && phase == 2 && mode == 2
            ) {
                phase = 3;
                timer = 0;
                recording_slot = 0;
                export_permit = 1;
            }
            action BuggyDetachGlassDuringTap when (
                Buggy == 1 && phase == 2 && mode == 1
            ) {
                glass = 0;
            }
            action BuggyBeginHeadlessUntimed when (
                Buggy == 1 && phase == 1 && glass == 0
            ) {
                phase = 2;
                mode = 2;
                timer = pace;
            }
            action BuggyLateCancelRevokes when (
                Buggy == 1 && phase == 3 && cancel_state == 2
            ) {
                cancel_state = 1;
            }
            action BuggyFinalizeKeepsRecordingSlot when (
                Buggy == 1 && phase == 2
            ) {
                phase = 3;
                mode = 0;
                timer = 0;
                export_permit = 1;
            }
            action BuggyPacedTapUntimed when (
                Buggy == 1 && phase == 1 && glass == 1 && translucent == 0 &&
                pace == 1
            ) {
                phase = 2;
                mode = 1;
                timer = 0;
            }

            bound Bounds:
                phase <= 3 && glass <= 1 && pace <= 1 && mode <= 2 &&
                timer <= 1 && translucent <= 1 && recording_slot <= 1 &&
                export_permit <= 1 && private_dirs <= 2 &&
                published <= 1 && cancel_state <= 2;
            invariant ModeMatchesRecordingPhase:
                if phase == 2 { mode > 0 } else { mode == 0 };
            invariant OffscreenOnlyWithoutGlass:
                (if mode == 2 { glass } else { 0 }) == 0;
            invariant TapOnlyOnGlass:
                (if mode == 1 { glass } else { 1 }) == 1;
            invariant NoTranslucentTap:
                (if mode == 1 { translucent } else { 0 }) == 0;
            invariant RecordingOwnsItsPacingTimer:
                if phase == 2 && mode == 2 {
                    timer == 1
                } else if phase == 2 && mode == 1 {
                    timer == pace
                } else {
                    timer <= 1
                };
            invariant PrivateDirectoryOwnedByLifecycle:
                if phase > 0 { private_dirs == 1 } else { private_dirs == 0 };
            invariant CancelledOwnsNothing:
                if cancel_state == 1 { phase == 0 } else { cancel_state <= 2 };
            invariant CommitAuthorizationScope:
                if cancel_state == 2 {
                    (phase == 3 && published == 0) ||
                    (phase == 0 && published == 1)
                } else {
                    published == 0
                };
            invariant SlotMatchesPhase:
                if phase == 1 || phase == 2 {
                    recording_slot == 1 && export_permit == 0
                } else if phase == 3 {
                    recording_slot == 0 && export_permit == 1
                } else {
                    recording_slot == 0 && export_permit == 0
                };
        }
    }
}

/// Instance-lease retention of per-process capture namespaces.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn exact_instance_retention_model() -> Model {
    aterm_spec::ty_model! {
        ExactInstanceRetention {
            const Buggy = 0;
            var lease = 0;
            var pid_alive = 0;
            var decision = 0;

            action SelectHeld when (decision == 0) { lease = 1; }
            action SelectFree when (decision == 0) { lease = 2; }
            action SelectMalformed when (decision == 0) { lease = 3; }
            action ObservePidAlive when (
                decision == 0 && pid_alive == 0
            ) {
                pid_alive = 1;
            }
            action Decide when (decision == 0) {
                decision = if lease == 0 {
                    if pid_alive == 1 || Buggy == 1 { 1 } else { 2 }
                } else if lease == 1 {
                    if Buggy == 1 { 2 } else { 1 }
                } else if lease == 2 {
                    if Buggy == 1 && pid_alive == 1 { 1 } else { 2 }
                } else {
                    if Buggy == 1 { 2 } else { 1 }
                };
            }

            bound Bounds:
                lease <= 3 && pid_alive <= 1 && decision <= 2;
            invariant HeldNeverRemoved:
                if lease == 1 { decision <= 1 } else { decision <= 2 };
            invariant MalformedNeverRemoved:
                if lease == 3 { decision <= 1 } else { decision <= 2 };
            invariant FreeAlwaysRemoved:
                if lease == 2 && decision > 0 {
                    decision == 2
                } else {
                    decision <= 2
                };
            invariant MissingAloneUsesPidFallback:
                if lease == 0 && decision > 0 {
                    if pid_alive == 1 { decision == 1 } else { decision == 2 }
                } else {
                    decision <= 2
                };
        }
    }
}

/// Retained-handle confinement across ancestor directory swaps.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn anchored_artifact_transaction_model() -> Model {
    aterm_spec::ty_model! {
        AnchoredArtifactTransaction {
            const Buggy = 0;
            var phase = 0;
            var swapped = 0;
            var path_identity = 0;
            var operation = 0;
            var effect_target = 0;
            var reply = 0;
            var certified_identity = 0;

            action ConfinePin when (phase == 0) {
                phase = 1;
                swapped = 0;
                path_identity = 1;
                operation = 0;
                effect_target = 0;
                reply = 0;
                certified_identity = 0;
            }
            action SwapAncestor when (
                phase > 0 && phase <= 2 && swapped == 0
            ) {
                swapped = 1;
                path_identity = 2;
            }
            action ReadPinned when (phase == 1) {
                phase = 2;
                operation = 1;
                effect_target = 1;
            }
            action WritePinned when (phase == 1) {
                phase = 2;
                operation = 2;
                effect_target = 1;
            }
            action ValidateReply when (
                (phase == 1 && swapped == 1) || phase == 2
            ) {
                phase = 3;
                reply = if swapped == 1 { 2 } else { 1 };
                certified_identity = if swapped == 1 { 0 } else { 1 };
            }
            action BuggyReresolveRead when (
                Buggy == 1 && phase == 1 && swapped == 1
            ) {
                phase = 2;
                operation = 1;
                effect_target = 2;
            }
            action BuggyReresolveWrite when (
                Buggy == 1 && phase == 1 && swapped == 1
            ) {
                phase = 2;
                operation = 2;
                effect_target = 2;
            }
            action BuggyCertifySwapped when (
                Buggy == 1 && phase == 2 && swapped == 1
            ) {
                phase = 3;
                reply = 1;
                certified_identity = 2;
            }

            bound Bounds:
                phase <= 3 && swapped <= 1 && path_identity <= 2 &&
                operation <= 2 && effect_target <= 2 && reply <= 2 &&
                certified_identity <= 2;
            invariant AnchoredAccessNeverOutside:
                effect_target <= 1;
            invariant SuccessfulReplyCertifiesOriginal:
                if reply == 1 {
                    swapped == 0 && certified_identity == 1
                } else {
                    certified_identity == 0
                };
        }
    }
}

/// Two-thread filesystem artifact publication, nonce challenge, and quarantine lifecycle.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn artifact_reply_publication_model() -> Model {
    aterm_spec::ty_model! {
        ArtifactReplyPublication {
            const QuarantineDelay = 1;
            const Buggy = 0;
            var phase = 0;
            var artifact = 0;
            var guard = 0;
            var committed = 0;
            var reply = 0;
            var challenge = 0;
            var ack = 0;
            var ack_failed = 0;
            var write_error = 0;
            var quarantine_age = 0;

            action Cancel when (phase == 0) {
                phase = 1;
            }
            action AuthorizeCommit when (phase == 0) {
                phase = 2;
                artifact = 1;
                guard = 1;
            }
            action AbortAuthorized when (
                phase == 2 && artifact == 1 && guard == 1
            ) {
                phase = 9;
            }
            action QueueGuard when (phase == 2 && artifact == 1 && guard == 1) {
                phase = 3;
            }
            action AbortQueued when (
                phase == 3 && artifact == 1 && guard == 1
            ) {
                phase = 9;
            }
            action PrepareWire when (phase == 3 && guard == 1) {
                phase = 4;
                committed = 1;
            }
            action PrepareFailed when (
                phase == 3 && artifact == 1 && guard == 1
            ) {
                phase = 9;
            }
            action WriteWire when (phase == 4 && guard == 1 && committed == 1) {
                phase = 5;
                reply = 1;
                challenge = 1;
            }
            action WriteFailed when (
                phase == 4 && artifact == 1 && guard == 1 && committed == 1
            ) {
                phase = 7;
                write_error = 1;
                quarantine_age = 0;
            }
            action AcknowledgePeer when (
                phase == 5 && artifact == 1 && guard == 1 &&
                committed == 1 && reply == 1 && challenge == 1
            ) {
                phase = 6;
                ack = 1;
            }
            action AcknowledgeFailed when (
                phase == 5 && artifact == 1 && guard == 1 &&
                committed == 1 && reply == 1 && challenge == 1
            ) {
                phase = 7;
                ack_failed = 1;
                quarantine_age = 0;
            }
            action AdvanceQuarantine when (
                phase == 7 && quarantine_age <= QuarantineDelay - 1
            ) {
                quarantine_age = quarantine_age + 1;
            }
            action ExpireQuarantine when (
                phase == 7 && quarantine_age == QuarantineDelay
            ) {
                phase = 8;
            }
            action ReleaseGuard when (
                guard == 1 && (phase == 6 || phase == 8 || phase == 9)
            ) {
                artifact = if phase == 9 { 0 } else { artifact };
                phase = if phase == 6 {
                    10
                } else {
                    if phase == 8 { 11 } else { 12 }
                };
                guard = 0;
            }
            action RetentionSweep when (
                phase > 1 && phase <= 9 && artifact == 1 && guard == 1
            ) {
                artifact = 1;
            }
            action BuggyPublishAfterCancel when (Buggy == 1 && phase == 1) {
                artifact = 1;
            }
            action BuggyDropBeforeWrite when (
                Buggy == 1 && phase == 3 && guard == 1
            ) {
                guard = 0;
            }
            action BuggyPruneLeased when (
                Buggy == 1 && phase > 1 && phase <= 9 &&
                artifact == 1 && guard == 1
            ) {
                artifact = 0;
            }
            action BuggyReleaseWithoutAck when (
                Buggy == 1 && phase == 5 && guard == 1
            ) {
                phase = 10;
                guard = 0;
            }
            action BuggyAcceptPreChallengeAck when (
                Buggy == 1 && phase == 4 && artifact == 1 &&
                guard == 1 && committed == 1 && challenge == 0 && ack == 0
            ) {
                ack = 1;
            }
            action BuggyReleaseQuarantineEarly when (
                Buggy == 1 && phase == 7 &&
                quarantine_age <= QuarantineDelay - 1 && guard == 1
            ) {
                phase = 11;
                guard = 0;
            }
            action BuggyChallengeBeforeBody when (
                Buggy == 1 && phase == 4 && guard == 1 && committed == 1 &&
                challenge == 0
            ) {
                challenge = 1;
            }
            action BuggyOkBeforeRevalidation when (
                Buggy == 1 && phase == 3 && artifact == 1 && guard == 1
            ) {
                phase = 9;
                committed = 1;
            }
            action BuggyTrailerErrorIgnored when (
                Buggy == 1 && phase == 4 && guard == 1 && committed == 1 &&
                challenge == 0
            ) {
                phase = 5;
            }
            action BuggyAbortRetainsArtifact when (
                Buggy == 1 && phase == 9 && guard == 1
            ) {
                phase = 12;
                guard = 0;
            }

            bound Bounds:
                phase <= 12 && artifact <= 1 && guard <= 1 &&
                committed <= 1 && reply <= 1 &&
                challenge <= 1 && ack <= 1 && ack_failed <= 1 &&
                write_error <= 1 && quarantine_age <= QuarantineDelay;
            invariant CancelledPublishesNothing:
                if phase == 1 { artifact == 0 } else { artifact <= 1 };
            invariant OwnedThroughClassifiedExitRetainsGuard:
                if phase > 1 && phase <= 9 { guard == 1 } else { guard <= 1 };
            invariant LeasedArtifactSurvivesRetention:
                if phase > 1 && phase <= 11 { artifact == 1 } else { artifact <= 1 };
            invariant CommitRequiresWirePreparation:
                if committed == 1 {
                    (phase > 3 && phase <= 8) ||
                    phase == 10 || phase == 11
                } else {
                    phase <= 3 || phase == 9 || phase == 12
                };
            invariant CompleteReplyPrecedesAck:
                if reply == 1 {
                    phase == 5 || phase == 6 || phase == 7 ||
                    phase == 8 || phase == 10 || phase == 11
                } else {
                    phase <= 4 || phase == 7 || phase == 8 ||
                    phase == 9 || phase == 11 || phase == 12
                };
            invariant ChallengeRequiresCompleteWire:
                if challenge == 1 { reply == 1 } else { challenge == 0 };
            invariant SuccessfulAckRequiresCausalChallenge:
                if ack == 1 { challenge == 1 } else { ack == 0 };
            invariant ImmediateReleaseRequiresValidAck:
                if phase == 10 { ack == 1 } else { ack <= 1 };
            invariant QuarantineReleaseRequiresExpiry:
                if phase == 8 || phase == 11 {
                    quarantine_age == QuarantineDelay
                } else {
                    quarantine_age <= QuarantineDelay
                };
            invariant AbortReleaseRemovesUncommittedArtifact:
                if phase == 12 { artifact == 0 } else { artifact <= 1 };
        }
    }
}

/// Aggregate object and descriptor-unit budget for queued, in-flight, and quarantined path artifacts.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn artifact_handoff_capacity_model() -> Model {
    aterm_spec::ty_model! {
        ArtifactHandoffCapacity {
            const Cap = 4;
            const DescriptorCap = 6;
            const Charge = 2;
            const ReconcileDelta = 1;
            const RequestedCharge = 3;
            const Buggy = 0;
            var live = 0;
            var descriptor_units = 0;
            var charge_one = 0;
            var charge_two = 0;
            var charge_three = 0;
            var selected = 2;

            action Acquire when (live <= Cap - 1 && descriptor_units <= DescriptorCap - Charge) {
                live = live + 1;
                descriptor_units = descriptor_units + Charge;
                charge_one = charge_one;
                charge_two = charge_two + 1;
                charge_three = charge_three;
                selected = Charge;
            }
            action RefuseAtCap when (live == Cap || descriptor_units > DescriptorCap - Charge) {
                live = live;
                descriptor_units = descriptor_units;
                charge_one = charge_one;
                charge_two = charge_two;
                charge_three = charge_three;
                selected = selected;
            }
            action SelectOne when (charge_one > 0) {
                live = live;
                descriptor_units = descriptor_units;
                charge_one = charge_one;
                charge_two = charge_two;
                charge_three = charge_three;
                selected = 1;
            }
            action SelectTwo when (charge_two > 0) {
                live = live;
                descriptor_units = descriptor_units;
                charge_one = charge_one;
                charge_two = charge_two;
                charge_three = charge_three;
                selected = 2;
            }
            action SelectThree when (charge_three > 0) {
                live = live;
                descriptor_units = descriptor_units;
                charge_one = charge_one;
                charge_two = charge_two;
                charge_three = charge_three;
                selected = 3;
            }
            action Release when (
                if selected == 1 {
                    charge_one > 0
                } else {
                    if selected == 2 { charge_two > 0 } else { charge_three > 0 }
                }
            ) {
                live = live - 1;
                descriptor_units = descriptor_units - selected;
                charge_one = if selected == 1 { charge_one - 1 } else { charge_one };
                charge_two = if selected == 2 { charge_two - 1 } else { charge_two };
                charge_three = if selected == 3 { charge_three - 1 } else { charge_three };
                selected = selected;
            }
            action ReconcileGrow when (
                selected <= 2 &&
                descriptor_units <= DescriptorCap - ReconcileDelta &&
                (if selected == 1 { charge_one > 0 } else { charge_two > 0 })
            ) {
                live = live;
                descriptor_units = descriptor_units + ReconcileDelta;
                charge_one = if selected == 1 { charge_one - 1 } else { charge_one };
                charge_two =
                    if selected == 1 { charge_two + 1 } else { charge_two - 1 };
                charge_three =
                    if selected == 2 { charge_three + 1 } else { charge_three };
                selected = selected + ReconcileDelta;
            }
            action ReconcileShrink when (
                selected > 1 &&
                (if selected == 2 { charge_two > 0 } else { charge_three > 0 })
            ) {
                live = live;
                descriptor_units = descriptor_units - ReconcileDelta;
                charge_one = if selected == 2 { charge_one + 1 } else { charge_one };
                charge_two =
                    if selected == 2 { charge_two - 1 } else { charge_two + 1 };
                charge_three =
                    if selected == 3 { charge_three - 1 } else { charge_three };
                selected = selected - ReconcileDelta;
            }
            action RefuseReconcile when (
                selected <= RequestedCharge - 1 &&
                descriptor_units - selected > DescriptorCap - RequestedCharge &&
                (if selected == 1 { charge_one > 0 } else { charge_two > 0 })
            ) {
                live = live;
                descriptor_units = descriptor_units;
                charge_one = charge_one;
                charge_two = charge_two;
                charge_three = charge_three;
                selected = selected;
            }
            action BuggyOverbook when (
                Buggy == 1 && live <= Cap &&
                (live == Cap || descriptor_units > DescriptorCap - Charge)
            ) {
                live = live + 1;
                descriptor_units = descriptor_units + Charge;
                charge_one = charge_one;
                charge_two = charge_two + 1;
                charge_three = charge_three;
                selected = Charge;
            }
            action BuggyReconcileOverbook when (
                Buggy == 1 &&
                selected <= RequestedCharge - 1 &&
                descriptor_units - selected > DescriptorCap - RequestedCharge &&
                (if selected == 1 { charge_one > 0 } else { charge_two > 0 })
            ) {
                live = live;
                descriptor_units = descriptor_units + RequestedCharge - selected;
                charge_one = if selected == 1 { charge_one - 1 } else { charge_one };
                charge_two = if selected == 2 { charge_two - 1 } else { charge_two };
                charge_three = charge_three + 1;
                selected = RequestedCharge;
            }
            action BuggyRefuseLeaksSlot when (
                Buggy == 1 && live <= Cap - 1 &&
                descriptor_units > DescriptorCap - Charge
            ) {
                live = live + 1;
                descriptor_units = descriptor_units;
                charge_one = charge_one;
                charge_two = charge_two;
                charge_three = charge_three;
                selected = selected;
            }
            action BuggyReleaseProvisionalCharge when (
                Buggy == 1 && selected == 3 && charge_three > 0 &&
                descriptor_units ==
                    charge_one + charge_two + charge_two +
                    charge_three + charge_three + charge_three
            ) {
                live = live - 1;
                descriptor_units = descriptor_units - Charge;
                charge_one = charge_one;
                charge_two = charge_two;
                charge_three = charge_three - 1;
                selected = selected;
            }

            invariant Bounded:
                live <= Cap && descriptor_units <= DescriptorCap &&
                selected > 0 && selected <= 3;
            invariant CountMatchesCharges:
                live == charge_one + charge_two + charge_three;
            invariant UnitsMatchCharges:
                descriptor_units ==
                    charge_one + charge_two + charge_two +
                    charge_three + charge_three + charge_three;
        }
    }
}

/// Directory-sync barrier ordering before reader-visible video batch publication.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn video_batch_publication_durability_model() -> Model {
    aterm_spec::ty_model! {
        VideoBatchPublicationDurability {
            const MaxMembers = 2;
            const Buggy = 0;
            var members = 0;
            var synced_members = 0;
            var marker = 0;

            action WriteMember when (
                marker == 0 && members <= MaxMembers - 1
            ) {
                members = members + 1;
            }
            action SyncBatch when (
                marker == 0 && members > 0
            ) {
                synced_members = members;
            }
            action PublishMarker when (
                marker == 0 && members > 0 && synced_members == members
            ) {
                marker = 1;
            }
            action BuggyPublishBeforeSync when (
                Buggy == 1 && marker == 0 && members > synced_members
            ) {
                marker = 1;
            }
            action BuggySyncAhead when (
                Buggy == 1 && marker == 0 && members > 0
            ) {
                synced_members = MaxMembers;
            }

            bound Bounds: members <= MaxMembers && marker <= 1;
            invariant BarrierCoversOnlyWrittenMembers: synced_members <= members;
            invariant MarkerCoversEveryMember:
                if marker == 1 {
                    members > 0 && synced_members == members
                } else {
                    marker == 0
                };
        }
    }
}

/// Refcounted video retention and convergence-sweep scheduling lifecycle.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn artifact_reader_lease_model() -> Model {
    aterm_spec::ty_model! {
        ArtifactReaderLease {
            const Cap = 2;
            const Buggy = 0;
            var leases = 0;
            var armed = 0;
            var requested = 0;
            var pending = 0;
            var sweeping = 0;
            var admission_spent = 0;
            var identity_mismatch = 0;
            var replacement_joined = 0;

            action Acquire when (
                leases <= Cap - 1 && pending == 0 && sweeping == 0 &&
                identity_mismatch == 0 && admission_spent == 0
            ) {
                leases = leases + 1;
            }
            action Arm when (
                leases > 0 && pending == 0 && sweeping == 0 &&
                identity_mismatch == 0
            ) {
                armed = 1;
                requested = 1;
            }
            action ReplaceIdentity when (
                leases > 0 && identity_mismatch == 0 &&
                pending == 0 && sweeping == 0
            ) {
                identity_mismatch = 1;
            }
            action RejectReplacedIdentity when (
                leases > 0 && identity_mismatch == 1 &&
                pending == 0 && sweeping == 0
            ) {
                leases = leases;
            }
            action Release when (leases > 0) {
                pending = if leases == 1 && armed == 1 { 1 } else { pending };
                identity_mismatch =
                    if leases == 1 && armed == 0 { 0 } else { identity_mismatch };
                leases = leases - 1;
            }
            action StartSweep when (
                leases == 0 && armed == 1 && pending == 1 && sweeping == 0
            ) {
                pending = 0;
                sweeping = 1;
                admission_spent = 1;
            }
            action RejectAcquireWhileSweeping when (
                pending + sweeping > 0
            ) {
                leases = leases;
            }
            action FinishSweep when (
                leases == 0 && armed == 1 && pending == 0 && sweeping == 1
            ) {
                armed = 0;
                requested = 0;
                sweeping = 0;
                admission_spent = 0;
                identity_mismatch = 0;
            }
            action BuggyStartSweepEarly when (
                Buggy == 1 && leases > 0 && armed == 1 &&
                pending == 0 && sweeping == 0
            ) {
                sweeping = 1;
            }
            action BuggyAcquireDuringSweep when (
                Buggy == 1 && pending + sweeping > 0 &&
                leases <= Cap - 1
            ) {
                leases = leases + 1;
            }
            action BuggyAcquireReplacedIdentity when (
                Buggy == 1 && leases <= Cap - 1 && identity_mismatch == 1 &&
                pending == 0 && sweeping == 0
            ) {
                leases = leases + 1;
                replacement_joined = 1;
            }
            action BuggyArmedReleaseDropsEntry when (
                Buggy == 1 && leases == 1 && armed == 1 &&
                pending == 0 && sweeping == 0
            ) {
                leases = 0;
                armed = 0;
                identity_mismatch = 0;
            }
            action BuggyReleaseSweepsUnarmed when (
                Buggy == 1 && leases == 1 && armed == 0 &&
                pending == 0 && sweeping == 0
            ) {
                leases = 0;
                pending = 1;
                identity_mismatch = 0;
            }
            action BuggyFinishKeepsEntry when (
                Buggy == 1 && leases == 0 && armed == 1 && pending == 0 &&
                sweeping == 1
            ) {
                armed = 0;
                requested = 0;
                sweeping = 0;
            }

            bound Bounds:
                leases <= Cap && armed <= 1 && requested <= 1 &&
                pending <= 1 && sweeping <= 1 && admission_spent <= 1 &&
                identity_mismatch <= 1 && replacement_joined <= 1;
            invariant MaintenanceExcludesLeases:
                if pending + sweeping > 0 { leases == 0 } else { leases <= Cap };
            invariant MaintenanceRequiresArm:
                if pending + sweeping > 0 { armed == 1 } else { armed <= 1 };
            invariant RequestedRetentionRunsAtLastRelease:
                if leases == 0 && requested == 1 {
                    pending + sweeping == 1
                } else {
                    pending + sweeping <= 1
                };
            invariant IdleNameAdmitsReaders:
                if leases == 0 && pending + sweeping == 0 {
                    admission_spent == 0 && identity_mismatch == 0
                } else {
                    admission_spent <= 1
                };
            invariant ReplacementNeverJoinsLeaseGroup:
                replacement_joined == 0;
        }
    }
}

/// Generation-fenced fixed-path snapshot publication.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn snapshot_generation_commit_model() -> Model {
    aterm_spec::ty_model! {
        SnapshotGenerationCommit {
            const Buggy = 0;
            var latest = 1;
            var job = 1;
            var payload = 0;
            var done = 0;
            action BeginNew when (latest == 1) {
                latest = 2;
                done = 0;
            }
            action CommitOld when (latest == 2 && job == 1) {
                payload = if Buggy == 1 { 1 } else { payload };
                done = if Buggy == 1 { 1 } else { 0 };
            }
            action SelectCurrent when (latest == 2 && job == 1) { job = 2; }
            action CommitCurrent when (job == latest) {
                payload = job;
                done = 1;
            }
            bound Bounds:
                latest <= 2 && job <= 2 && payload <= 2 && done <= 1;
            invariant CommittedPayloadIsCurrent:
                (if done == 1 { payload } else { latest }) == latest;
        }
    }
}

/// Compositor-present fencing and bounded retry for native control screenshot capture.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn capture_after_present_model() -> Model {
    aterm_spec::ty_model! {
        CaptureAfterPresent {
            const Buggy = 0;
            const AttemptLimit = 3;
            var staged = 0;
            var present_succeeded = 0;
            var attempts = 0;
            var decision = 0;
            var captured = 0;
            var failed = 0;
            var stale_capture = 0;
            action Mutate when (staged == 0 && captured == 0 && failed == 0) {
                staged = 1;
                present_succeeded = 0;
                attempts = 1;
                decision = 0;
            }
            action MarkPresentSucceeded when (staged == 1 && decision == 0) {
                present_succeeded = 1;
            }
            action Decide when (
                staged == 1 && decision == 0 &&
                attempts > 0 && attempts <= AttemptLimit
            ) {
                decision = if present_succeeded == 1 {
                    1
                } else if attempts <= AttemptLimit - 1 {
                    2
                } else {
                    3
                };
                captured = present_succeeded;
                failed = if (
                    present_succeeded == 0 && attempts == AttemptLimit
                ) { 1 } else { 0 };
                staged = if present_succeeded == 1 { 0 } else { staged };
            }
            action BuggyStaleCapture when (
                Buggy == 1 && staged == 1 && decision == 0 &&
                attempts > 0 && present_succeeded == 0
            ) {
                decision = 1;
                captured = 1;
                stale_capture = 1;
            }
            action BuggyRetryAtLimit when (
                Buggy == 1 && staged == 1 && decision == 0 &&
                attempts == AttemptLimit && present_succeeded == 0
            ) {
                decision = 2;
            }
            action Retry when (decision == 2 && attempts <= AttemptLimit - 1) {
                attempts = attempts + 1;
                present_succeeded = 0;
                decision = 0;
            }
            invariant NoStaleCapture: stale_capture == 0;
            invariant CaptureRequiresPresent:
                if captured == 1 {
                    present_succeeded == 1 && staged == 0 && decision == 1
                } else {
                    stale_capture == 0
                };
            invariant DecisionMatchesOutcome:
                if decision == 0 {
                    captured == 0 && failed == 0
                } else if present_succeeded == 1 {
                    decision == 1 && captured == 1 && failed == 0
                } else if attempts <= AttemptLimit - 1 {
                    decision == 2 && captured == 0 && failed == 0
                } else {
                    decision == 3 && captured == 0 && failed == 1
                };
            bound ValuesBounded:
                attempts <= AttemptLimit &&
                staged <= 1 && present_succeeded <= 1 && decision <= 3 &&
                captured <= 1 && failed <= 1 && stale_capture <= 1;
        }
    }
}

/// Single-slot staging buffer lifecycle for the one-shot presented-destination tap.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn presented_frame_tap_model() -> Model {
    aterm_spec::ty_model! {
        PresentedFrameTap {
            const Buggy = 0;
            var phase = 0;
            var accepted = 0;
            var mapped = 0;
            var result = 0;
            action EnqueueValid when (phase == 0) {
                phase = 1;
                accepted = 1;
                mapped = 0;
                result = 0;
            }
            action RejectEnqueue when (phase == 0) {
                phase = 3;
                accepted = 0;
                mapped = 0;
                result = 2;
            }
            action StartMap when (phase == 1) {
                phase = 2;
            }
            action CompleteMap when (phase == 2) {
                phase = 3;
                mapped = 1;
                result = 1;
            }
            action MapError when (phase == 2) {
                phase = 3;
                mapped = 0;
                result = 2;
            }
            action BuggyMapErrorPublishesFrame when (Buggy == 1 && phase == 2) {
                phase = 3;
                mapped = 0;
                result = 1;
            }
            action BuggyRejectReservesSlot when (Buggy == 1 && phase == 0) {
                phase = 1;
                accepted = 0;
                mapped = 0;
                result = 0;
            }
            action BuggyMapErrorWithoutOutcome when (Buggy == 1 && phase == 2) {
                phase = 3;
                mapped = 0;
                result = 0;
            }
            invariant SuccessRequiresMappedCopy:
                if result == 1 { mapped == 1 } else { mapped <= 1 };
            invariant ReservedPhaseRequiresAcceptedCopy:
                if phase > 0 && phase <= 2 { accepted == 1 } else { phase <= 3 };
            invariant TerminalPhaseHasResult:
                if phase == 3 { result > 0 } else { result == 0 };
            bound ValuesBounded:
                phase <= 3 && accepted <= 1 && mapped <= 1 && result <= 2;
        }
    }
}

/// Streaming video tap slot lifecycle, loss accounting, and bounded sequence-ordered harvest.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn video_tap_slot_model() -> Model {
    aterm_spec::ty_model! {
        VideoTapSlot {
            const Buggy = 0;
            const MaxDrops = 2;
            var phase = 0;
            var dropped = 0;
            var errors = 0;
            var invalid = 0;
            var last_error = 0;
            var harvest_phase = 0;
            var store_first = 0;
            var store_second = 0;
            var evicted = 0;
            action Enqueue when (phase == 0) {
                phase = 1;
                last_error = 0;
            }
            action StartMap when (phase == 1) {
                phase = 2;
            }
            action MapOk when (phase == 2) {
                phase = 0;
                last_error = 0;
            }
            action MapError when (phase == 2 && dropped <= MaxDrops - 1) {
                phase = 0;
                dropped = dropped + 1;
                errors = errors + 1;
                last_error = 1;
            }
            action Abort when (phase > 0 && dropped <= MaxDrops - 1) {
                phase = 0;
                dropped = dropped + 1;
                errors = errors + 1;
                last_error = 1;
            }
            action RejectInvalidMetadata when (
                invalid == 0 && dropped <= MaxDrops - 1
            ) {
                invalid = 1;
                dropped = dropped + 1;
            }
            action HarvestThree when (harvest_phase == 0) {
                harvest_phase = 1;
                store_first = 3;
                store_second = 0;
                evicted = 0;
            }
            action HarvestOne when (harvest_phase == 1) {
                harvest_phase = 2;
                store_first = 1;
                store_second = 3;
                evicted = 0;
            }
            action HarvestTwo when (harvest_phase == 2) {
                harvest_phase = 3;
                store_first = 2;
                store_second = 3;
                evicted = 1;
            }
            action BuggyMapErrorLeaksSlot when (
                Buggy == 1 && phase == 2 && dropped <= MaxDrops - 1
            ) {
                dropped = dropped + 1;
                errors = errors + 1;
                last_error = 1;
            }
            action BuggyRejectInvalidUncounted when (
                Buggy == 1 && invalid == 0 && dropped <= MaxDrops - 1
            ) {
                invalid = 1;
            }
            action BuggyHarvestCallbackOrder when (
                Buggy == 1 && harvest_phase == 1
            ) {
                harvest_phase = 2;
                store_first = 3;
                store_second = 1;
                evicted = 0;
            }
            action BuggyEvictNewest when (Buggy == 1 && harvest_phase == 2) {
                harvest_phase = 3;
                store_first = 1;
                store_second = 2;
                evicted = 1;
            }
            action BuggyEvictionUnreported when (
                Buggy == 1 && harvest_phase == 2
            ) {
                harvest_phase = 3;
                store_first = 2;
                store_second = 3;
                evicted = 0;
            }
            invariant ErrorResolutionFreesSlot:
                if last_error == 1 { phase == 0 } else { phase <= 2 };
            invariant InvalidMetadataIsCounted: dropped == errors + invalid;
            bound DropCountBounded: dropped <= MaxDrops && errors <= MaxDrops;
            invariant HarvestedStoreSorted:
                if harvest_phase > 1 {
                    store_first <= store_second
                } else {
                    store_first <= 3
                };
            invariant BudgetKeepsNewestTail:
                if harvest_phase == 3 {
                    store_first == 2 && store_second == 3
                } else {
                    harvest_phase <= 2
                };
            invariant EvictionMatchesOverflow:
                if harvest_phase == 3 { evicted == 1 } else { evicted == 0 };
            bound ValuesBounded:
                phase <= 2 && invalid <= 1 && last_error <= 1 &&
                harvest_phase <= 3 && store_first <= 3 &&
                store_second <= 3 && evicted <= 1;
        }
    }
}

/// Window-local effect coordinate reset and rebind across leaf geometry and DPI changes.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn layout_coordinate_reset_model() -> Model {
    aterm_spec::ty_model! {
        LayoutCoordinateReset {
            const Buggy = 0;
            const MaxCoordinate = 2;
            var coordinate = 0;
            var bound_coordinate = 0;
            var charged = 0;
            var prepared = 1;
            var stale_present = 0;
            action Charge when (prepared == 1) {
                charged = 1;
                bound_coordinate = coordinate;
            }
            action ChangeCoordinate when (prepared == 1) {
                coordinate = if coordinate <= MaxCoordinate - 1 {
                    coordinate + 1
                } else {
                    0
                };
                prepared = 0;
            }
            action Prepare when (prepared == 0) {
                charged = if Buggy == 1 { charged } else { 0 };
                bound_coordinate = if Buggy == 1 {
                    bound_coordinate
                } else {
                    coordinate
                };
                prepared = 1;
            }
            action Present when (prepared == 1) {
                stale_present = if charged == 1 {
                    if bound_coordinate == coordinate {
                        stale_present
                    } else {
                        1
                    }
                } else {
                    stale_present
                };
            }
            invariant NoStaleCoordinatePresent: stale_present == 0;
            invariant PreparedEffectsMatchCoordinate:
                if prepared == 1 && charged == 1 {
                    bound_coordinate == coordinate
                } else {
                    0 == 0
                };
            bound CoordinateBounded:
                coordinate <= MaxCoordinate && bound_coordinate <= MaxCoordinate;
            bound ValuesBounded:
                charged <= 1 && prepared <= 1 && stale_present <= 1;
        }
    }
}

/// Generation-tagged replacement queue and stale-result rejection for the semantic preview font worker.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn semantic_prewarm_generation_model() -> Model {
    aterm_spec::ty_model! {
        SemanticPrewarmGeneration {
            const Buggy = 0;
            const MaxGeneration = 3;
            var current = 1;
            var queued = 0;
            var running = 0;
            var result = 0;
            var ready = 0;
            var installed = 0;
            var observed_current = 0;
            var observed_result = 0;
            var decision = 0;
            var resolved = 0;
            action Request when (ready == 0) {
                queued = current;
                resolved = 0;
            }
            action Start when (running == 0 && result == 0 && queued > 0) {
                running = queued;
                queued = 0;
                resolved = 0;
            }
            action Reload when (current <= MaxGeneration - 1) {
                current = current + 1;
                queued = 0;
                ready = 0;
                installed = 0;
                resolved = 0;
            }
            action Finish when (running > 0 && result == 0) {
                result = running;
                running = 0;
                resolved = 0;
            }
            action Decide when (result > 0) {
                observed_current = current;
                observed_result = result;
                decision = if result == current { 1 } else { 0 };
                ready = if result == current { 1 } else { ready };
                installed = if result == current { result } else { installed };
                result = 0;
                resolved = 1;
            }
            action BuggyInstallStaleResult when (
                Buggy == 1 && result > 0 && (result > current || current > result)
            ) {
                observed_current = current;
                observed_result = result;
                decision = 1;
                ready = 1;
                installed = result;
                result = 0;
                resolved = 1;
            }
            action BuggyIgnoreCurrentResult when (
                Buggy == 1 && result > 0 && result == current
            ) {
                observed_current = current;
                observed_result = result;
                decision = 0;
                result = 0;
                resolved = 1;
            }
            action BuggyReloadKeepsQueued when (
                Buggy == 1 && current <= MaxGeneration - 1
            ) {
                current = current + 1;
                ready = 0;
                installed = 0;
                resolved = 0;
            }
            action BuggyReloadKeepsInstalled when (
                Buggy == 1 && current <= MaxGeneration - 1
            ) {
                current = current + 1;
                queued = 0;
                resolved = 0;
            }
            invariant CurrentResultOnly:
                if resolved == 0 {
                    decision <= 1
                } else if observed_current == observed_result {
                    decision == 1
                } else {
                    decision == 0
                };
            invariant ReadyGenerationIsCurrent:
                if ready == 1 { installed == current } else { installed <= MaxGeneration };
            invariant QueueContainsOnlyCurrent:
                if queued > 0 { queued == current } else { queued == 0 };
            bound GenerationsBounded:
                current <= MaxGeneration && queued <= MaxGeneration &&
                running <= MaxGeneration && result <= MaxGeneration &&
                installed <= MaxGeneration && observed_current <= MaxGeneration &&
                observed_result <= MaxGeneration;
            bound FlagsBounded: ready <= 1 && decision <= 1 && resolved <= 1;
        }
    }
}

/// Convergence-cadence retention until completed font epochs reach the retained Settings preview raster.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_preview_font_convergence_model() -> Model {
    aterm_spec::ty_model! {
        NativePreviewFontConvergence {
            const Buggy = 0;
            var pending = 1;
            var ready = 0;
            var retained = 0;
            var dirty = 0;
            var decision = 0;
            var resolved = 0;
            action Finish when (pending == 1) {
                pending = 0;
                ready = 1;
                resolved = 0;
            }
            action Decide {
                decision = if pending == 1 { 1 }
                    else if Buggy == 0 && ready > retained { 1 }
                    else { 0 };
                resolved = 1;
            }
            action Invalidate when (resolved == 1 && decision == 1) {
                dirty = 1;
                resolved = 0;
            }
            action Paint when (dirty == 1) {
                retained = ready;
                dirty = 0;
                resolved = 0;
            }
            invariant CadenceMatchesRetainedFont:
                if resolved == 1 {
                    decision == if pending == 1 || ready > retained { 1 } else { 0 }
                } else { 0 == 0 };
        }
    }
}

/// Base-renderer carry-over and exact-match activation for the semantic preview font worker.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn semantic_prewarm_handshake_model() -> Model {
    aterm_spec::ty_model! {
        SemanticPrewarmHandshake {
            const Buggy = 0;
            var new_base = 0;
            var replaced_base = 0;
            var replacement_base = 0;
            var replacement_resolved = 0;
            var generation_matches = 0;
            var request_matches = 0;
            var candidate_matches = 0;
            var renderer_ready = 0;
            var active_before = 0;
            var active_before_latest = 0;
            var decision = 0;
            var cached = 0;
            var active_after = 0;
            var active_after_latest = 0;
            var result_resolved = 0;
            action MarkNewBase when (replacement_resolved == 0) {
                new_base = 1;
            }
            action MarkReplacedBase when (replacement_resolved == 0) {
                replaced_base = 1;
            }
            action ResolveReplacement when (replacement_resolved == 0) {
                replacement_base = if new_base + replaced_base > 0 { 1 } else { 0 };
                replacement_resolved = 1;
            }
            action MarkGenerationCurrent when (result_resolved == 0) {
                generation_matches = 1;
            }
            action MarkRequestCurrent when (result_resolved == 0) {
                request_matches = 1;
            }
            action MarkCandidateCurrent when (result_resolved == 0) {
                candidate_matches = 1;
            }
            action MarkRendererReady when (result_resolved == 0) {
                renderer_ready = 1;
            }
            action MarkActiveBefore when (result_resolved == 0) {
                active_before = 1;
                active_after = 1;
            }
            action MarkActiveBeforeLatest when (result_resolved == 0) {
                active_before = 1;
                active_before_latest = 1;
                active_after = 1;
                active_after_latest = 1;
            }
            action DecideResult when (result_resolved == 0) {
                decision = if generation_matches == 0 {
                    1
                } else if request_matches == 1 && candidate_matches == 1 {
                    if renderer_ready == 1 { 2 } else { 3 }
                } else if renderer_ready == 1 {
                    4
                } else {
                    5
                };
                cached = if (
                    generation_matches == 1 && renderer_ready == 1 &&
                    request_matches + candidate_matches <= 1
                ) { 1 } else { 0 };
                active_after = if (
                    generation_matches == 1 && request_matches == 1 &&
                    candidate_matches == 1
                ) {
                    renderer_ready
                } else {
                    active_before
                };
                active_after_latest = if (
                    generation_matches == 1 && request_matches == 1 &&
                    candidate_matches == 1
                ) {
                    renderer_ready
                } else {
                    active_before_latest
                };
                result_resolved = 1;
            }
            action BuggyReplacementKeepsOnlyNewBase when (
                Buggy == 1 && replacement_resolved == 0
            ) {
                replacement_base = new_base;
                replacement_resolved = 1;
            }
            action BuggyMixedCandidateInstalls when (
                Buggy == 1 && result_resolved == 0 && generation_matches == 1 &&
                request_matches + candidate_matches <= 1 && renderer_ready == 1
            ) {
                decision = 2;
                cached = 0;
                active_after = 1;
                active_after_latest = 0;
                result_resolved = 1;
            }
            action BuggyReadinessBeforeGeneration when (
                Buggy == 1 && result_resolved == 0 && generation_matches == 0 &&
                renderer_ready == 1
            ) {
                decision = 4;
                cached = 1;
                result_resolved = 1;
            }
            action BuggyInstallKeepsStaleIdentity when (
                Buggy == 1 && result_resolved == 0 && generation_matches == 1 &&
                request_matches == 1 && candidate_matches == 1 &&
                renderer_ready == 1
            ) {
                decision = 2;
                cached = 0;
                active_after = 1;
                active_after_latest = 0;
                result_resolved = 1;
            }
            action BuggyFailClosedKeepsPrevious when (
                Buggy == 1 && result_resolved == 0 && generation_matches == 1 &&
                request_matches == 1 && candidate_matches == 1 &&
                renderer_ready == 0
            ) {
                decision = 3;
                cached = 0;
                result_resolved = 1;
            }
            action BuggyDropSuperseded when (
                Buggy == 1 && result_resolved == 0 && generation_matches == 1 &&
                request_matches + candidate_matches <= 1 && renderer_ready == 1
            ) {
                decision = 4;
                cached = 0;
                result_resolved = 1;
            }
            action BuggyCacheParksActive when (
                Buggy == 1 && result_resolved == 0 && generation_matches == 1 &&
                request_matches + candidate_matches <= 1 && renderer_ready == 1
            ) {
                decision = 4;
                cached = 1;
                active_after = 0;
                active_after_latest = 0;
                result_resolved = 1;
            }
            invariant ReplacementCarriesBase:
                if replacement_resolved == 0 {
                    replacement_base == 0
                } else if new_base + replaced_base > 0 {
                    replacement_base == 1
                } else {
                    replacement_base == 0
                };
            invariant DecisionMatchesIdentity:
                if result_resolved == 0 {
                    decision == 0
                } else if generation_matches == 0 {
                    decision == 1
                } else if request_matches == 1 && candidate_matches == 1 {
                    if renderer_ready == 1 { decision == 2 } else { decision == 3 }
                } else if renderer_ready == 1 {
                    decision == 4
                } else {
                    decision == 5
                };
            invariant InstallOnlyLatestReady:
                if decision == 2 {
                    active_after == 1 && active_after_latest == 1
                } else {
                    active_after <= 1
                };
            invariant CurrentFailureFailsClosed:
                if decision == 3 {
                    active_after == 0 && active_after_latest == 0
                } else {
                    active_after <= 1
                };
            invariant CacheOnlySupersededReady:
                if decision == 4 { cached == 1 } else { cached == 0 };
            invariant NoncurrentPreservesActive:
                if decision == 2 || decision == 3 {
                    active_after <= 1
                } else {
                    active_after == active_before &&
                    active_after_latest == active_before_latest
                };
            bound InputsBounded:
                new_base <= 1 && replaced_base <= 1 && generation_matches <= 1 &&
                request_matches <= 1 && candidate_matches <= 1 && renderer_ready <= 1 &&
                active_before <= 1 && active_before_latest <= 1;
            bound OutputsBounded:
                replacement_base <= 1 && replacement_resolved <= 1 && decision <= 5 &&
                cached <= 1 && active_after <= 1 && active_after_latest <= 1 &&
                result_resolved <= 1;
        }
    }
}

/// Active-to-cache demotion of ready non-host semantic preview renderers when switching candidates.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn semantic_prewarm_request_swap_model() -> Model {
    aterm_spec::ty_model! {
        SemanticPrewarmRequestSwap {
            const Buggy = 0;
            var active_present = 0;
            var active_ready = 0;
            var candidate_matches = 0;
            var should_cache = 0;
            var active_after = 0;
            var resolved = 0;
            action MarkHostSeed when (resolved == 0 && active_present == 0) {
                active_present = 1;
            }
            action MarkReadyMismatch when (resolved == 0 && active_present == 0) {
                active_present = 1;
                active_ready = 1;
            }
            action MarkReadyMatch when (resolved == 0 && active_present == 0) {
                active_present = 1;
                active_ready = 1;
                candidate_matches = 1;
            }
            action Decide when (resolved == 0) {
                should_cache = if (
                    Buggy == 0 && active_present == 1 && active_ready == 1 &&
                    candidate_matches == 0
                ) { 1 } else { 0 };
                active_after = if (
                    Buggy == 0 && active_present == 1 && active_ready == 1 &&
                    candidate_matches == 0
                ) { 0 } else { active_present };
                resolved = 1;
            }
            invariant MismatchedReadyMovesToCache:
                if (
                    resolved == 1 && active_present == 1 &&
                    active_ready == 1 && candidate_matches == 0
                ) {
                    should_cache == 1 && active_after == 0
                } else if resolved == 1 {
                    should_cache == 0 && active_after == active_present
                } else {
                    should_cache == 0 && active_after == 0
                };
            bound FlagsBounded:
                active_present <= 1 && active_ready <= 1 && candidate_matches <= 1 &&
                should_cache <= 1 && active_after <= 1 && resolved <= 1;
        }
    }
}

/// Incremental fan-out cursor and expiry-deadline lifecycle for session chrome caches.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn session_chrome_expiry_model() -> Model {
    aterm_spec::ty_model! {
        SessionChromeExpiry {
            const Buggy = 0;
            const Capacity = 3;
            const Windows = 3;
            const Budget = 1;
            var fresh = 0;
            var due = 0;
            var scanning = 0;
            var remaining = 0;
            var armed = 0;
            var work = 0;
            action Seed when (fresh + due + scanning == 0) {
                fresh = Capacity;
                due = 0;
                scanning = 0;
                remaining = 0;
                armed = 1;
                work = 0;
            }
            action Expire when (fresh > 0 && due == 0 && scanning == 0) {
                due = fresh;
                fresh = 0;
                scanning = 0;
                remaining = 0;
                armed = 1;
                work = 0;
            }
            action Begin when (due > 0 && scanning == 0 && armed == 1) {
                due = due - 1;
                scanning = 1;
                remaining = Windows;
                armed = 1;
                work = 0;
            }
            action Scan when (scanning == 1 && remaining > 0) {
                work = if Buggy == 1 { remaining } else { 1 };
                fresh = if Buggy == 1 || remaining == 1 {
                    if Buggy == 3 { fresh } else { fresh + 1 }
                } else {
                    fresh
                };
                remaining = if Buggy == 1 { 0 } else { remaining - 1 };
                scanning = if Buggy == 1 || remaining == 1 {
                    if Buggy == 4 { 1 } else { 0 }
                } else {
                    1
                };
                armed = 1;
            }
            action Retire when (fresh + due + scanning > 0) {
                fresh = 0;
                due = 0;
                scanning = 0;
                remaining = 0;
                armed = if Buggy == 2 { 1 } else { 0 };
                work = 0;
            }
            invariant WorkPerTurnBounded: work <= Budget;
            invariant DeadlineTracksRetainedCache:
                armed == if fresh + due + scanning > 0 { 1 } else { 0 };
            invariant CapacityConservedOrRetired:
                fresh + due + scanning == 0 || fresh + due + scanning == Capacity;
            invariant CursorShape:
                if scanning == 1 {
                    remaining > 0 && remaining <= Windows
                } else {
                    remaining == 0
                };
            bound ValuesBounded:
                fresh <= Capacity && due <= Capacity && scanning <= 1 && armed <= 1;
        }
    }
}

/// Budgeted incremental search resume and continuation validation for `Terminal::search_budgeted`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn budgeted_search_resume_model() -> Model {
    aterm_spec::ty_model! {
        BudgetedSearchResume {
            const Rows = 3;
            const DeliveryTurns = 3;
            const MaxTokens = 10;
            const Buggy = 0;

            var live = 0;
            var progress = 0;
            var total = 0;
            var scan_done = 0;
            var complete = 0;
            var cursor = 0;
            var search_id = 0;
            var issued = 0;
            var reset = 0;
            var prior_id = 0;
            var delivery = 0;
            var stream_id = 0;

            action Start when (live == 0 && issued <= MaxTokens - 1) {
                live = 1;
                progress = 1;
                total = Rows;
                scan_done = 0;
                complete = 0;
                cursor = issued + 1;
                search_id = issued + 1;
                issued = issued + 1;
                reset = 1;
                prior_id = search_id;
                delivery = 0;
                stream_id = issued + 1;
            }

            action StartComplete when (live == 0 && issued <= MaxTokens - 1) {
                live = if Buggy == 1 { 1 } else { 0 };
                progress = Rows;
                total = Rows;
                scan_done = 1;
                complete = 1;
                cursor = if Buggy == 1 { issued + 1 } else { 0 };
                search_id = issued + 1;
                issued = issued + 1;
                reset = 1;
                prior_id = search_id;
                delivery = 0;
                stream_id = issued + 1;
            }

            action StartBacklog when (live == 0 && issued <= MaxTokens - 1) {
                live = 1;
                progress = Rows;
                total = Rows;
                scan_done = 1;
                complete = 0;
                cursor = issued + 1;
                search_id = issued + 1;
                issued = issued + 1;
                reset = 1;
                prior_id = search_id;
                delivery = 1;
                stream_id = issued + 1;
            }

            action Resume when (live == 1 && scan_done == 0 && progress <= Rows - 2) {
                progress = progress + 1;
                cursor = if Buggy == 1 && issued <= MaxTokens - 1 { issued + 1 } else { cursor };
                search_id = if Buggy == 1 && issued <= MaxTokens - 1 { issued + 1 } else { search_id };
                issued = if Buggy == 1 && issued <= MaxTokens - 1 { issued + 1 } else { issued };
                reset = 0;
                prior_id = 0;
            }
            action FinishScan when (live == 1 && scan_done == 0 && progress == Rows - 1) {
                live = if Buggy == 1 { 1 } else { 0 };
                progress = progress + 1;
                scan_done = 1;
                complete = 1;
                cursor = if Buggy == 1 { cursor } else { 0 };
                reset = 0;
                prior_id = 0;
            }

            action Supersede when (live == 1 && issued <= MaxTokens - 1) {
                live = 1;
                progress = if Buggy == 1 { progress } else { 1 };
                total = Rows;
                scan_done = if Buggy == 1 { scan_done } else { 0 };
                complete = 0;
                cursor = if Buggy == 1 { cursor } else { issued + 1 };
                search_id = if Buggy == 1 { search_id } else { issued + 1 };
                issued = if Buggy == 1 { issued } else { issued + 1 };
                reset = 1;
                prior_id = search_id;
                delivery = if Buggy == 1 { delivery } else { 0 };
                stream_id = if Buggy == 1 { search_id } else { issued + 1 };
            }
            action ContentRestart when (live == 1 && issued <= MaxTokens - 1) {
                live = 1;
                progress = if Buggy == 1 { progress } else { 1 };
                total = Rows;
                scan_done = if Buggy == 1 { scan_done } else { 0 };
                complete = 0;
                cursor = if Buggy == 1 { cursor } else { issued + 1 };
                search_id = if Buggy == 1 { search_id } else { issued + 1 };
                issued = if Buggy == 1 { issued } else { issued + 1 };
                reset = 1;
                prior_id = search_id;
                delivery = if Buggy == 1 { delivery } else { 0 };
                stream_id = if Buggy == 1 { search_id } else { issued + 1 };
            }
            action QueryRestart when (live == 1 && issued <= MaxTokens - 1) {
                live = 1;
                progress = if Buggy == 1 { progress } else { 1 };
                total = Rows;
                scan_done = if Buggy == 1 { scan_done } else { 0 };
                complete = 0;
                cursor = if Buggy == 1 { cursor } else { issued + 1 };
                search_id = if Buggy == 1 { search_id } else { issued + 1 };
                issued = if Buggy == 1 { issued } else { issued + 1 };
                reset = 1;
                prior_id = search_id;
                delivery = if Buggy == 1 { delivery } else { 0 };
                stream_id = if Buggy == 1 { search_id } else { issued + 1 };
            }
            action ForgedRestart when (live == 1 && issued <= MaxTokens - 1) {
                live = 1;
                progress = if Buggy == 1 { progress } else { 1 };
                total = Rows;
                scan_done = if Buggy == 1 { scan_done } else { 0 };
                complete = 0;
                cursor = if Buggy == 1 { cursor } else { issued + 1 };
                search_id = if Buggy == 1 { search_id } else { issued + 1 };
                issued = if Buggy == 1 { issued } else { issued + 1 };
                reset = 1;
                prior_id = search_id;
                delivery = if Buggy == 1 { delivery } else { 0 };
                stream_id = if Buggy == 1 { search_id } else { issued + 1 };
            }

            action RestartComplete when (live == 1 && issued <= MaxTokens - 1) {
                live = if Buggy == 1 { 1 } else { 0 };
                progress = Rows;
                total = Rows;
                scan_done = 1;
                complete = 1;
                cursor = if Buggy == 1 { search_id } else { 0 };
                search_id = if Buggy == 1 { search_id } else { issued + 1 };
                issued = if Buggy == 1 { issued } else { issued + 1 };
                reset = 1;
                prior_id = search_id;
                delivery = 0;
                stream_id = if Buggy == 1 { search_id } else { issued + 1 };
            }

            action BuggyScanOnlyCompletion when (live == 0 && issued <= MaxTokens - 1) {
                live = live;
                progress = if Buggy == 1 { Rows } else { progress };
                total = if Buggy == 1 { Rows } else { total };
                scan_done = if Buggy == 1 { 1 } else { scan_done };
                complete = if Buggy == 1 { 1 } else { complete };
                cursor = cursor;
                search_id = if Buggy == 1 { issued + 1 } else { search_id };
                issued = if Buggy == 1 { issued + 1 } else { issued };
                reset = if Buggy == 1 { 1 } else { reset };
                prior_id = if Buggy == 1 { search_id } else { prior_id };
                delivery = if Buggy == 1 { 1 } else { delivery };
                stream_id = if Buggy == 1 { issued + 1 } else { stream_id };
            }

            action Drain when (
                live == 1 && scan_done == 1 && delivery > 0 &&
                delivery <= DeliveryTurns - 2
            ) {
                delivery = delivery + 1;
                reset = 0;
                prior_id = 0;
            }
            action DrainComplete when (
                live == 1 && scan_done == 1 && delivery == DeliveryTurns - 1
            ) {
                live = if Buggy == 1 { 1 } else { 0 };
                complete = 1;
                cursor = if Buggy == 1 { cursor } else { 0 };
                reset = 0;
                prior_id = 0;
                delivery = delivery + 1;
            }

            action Cancel when (live == 1) {
                live = 0;
                progress = 0;
                total = 0;
                scan_done = 0;
                complete = 0;
                cursor = 0;
                search_id = 0;
                reset = 0;
                prior_id = 0;
                delivery = 0;
                stream_id = 0;
            }

            invariant LifecycleShape:
                (live == 0 && cursor == 0 &&
                    ((complete == 0 && progress == 0 && total == 0 &&
                        scan_done == 0 && search_id == 0 && delivery == 0) ||
                     (complete == 1 && progress == Rows && total == Rows &&
                        scan_done == 1 && search_id > 0))) ||
                (live == 1 && complete == 0 && cursor > 0 &&
                    search_id > 0 && total == Rows &&
                    ((scan_done == 0 && progress > 0 &&
                        progress <= Rows - 1 && delivery == 0) ||
                     (scan_done == 1 && progress == Rows && delivery > 0 &&
                        delivery <= DeliveryTurns - 1)));
            invariant IdentityStableWithoutReset:
                reset == 1 || search_id == stream_id;
            invariant ResetMintsFresh:
                if reset == 1 {
                    search_id == issued && search_id > prior_id
                } else {
                    prior_id == 0
                };
            invariant ResetStartsAtBeginning:
                if reset == 1 && live == 1 && scan_done == 0 {
                    progress == 1 && delivery == 0
                } else {
                    1 == 1
                };
            invariant DeliveryShape:
                delivery == 0 ||
                (scan_done == 1 && progress == Rows && total == Rows &&
                    ((live == 1 && complete == 0 && delivery > 0 &&
                        delivery <= DeliveryTurns - 1) ||
                     (live == 0 && complete == 1 && delivery == DeliveryTurns)));
            bound ValuesBounded:
                live <= 1 && scan_done <= 1 && complete <= 1 && reset <= 1 &&
                progress <= Rows && total <= Rows && delivery <= DeliveryTurns &&
                issued <= MaxTokens && cursor <= MaxTokens &&
                search_id <= MaxTokens && prior_id <= MaxTokens &&
                stream_id <= MaxTokens;
        }
    }
}

/// Host-minted OSC-8 hyperlink scheme allowlist and capacity model.
#[cfg_attr(trust_verify, trust::skip)]
pub fn hyperlink_scheme_cap_model() -> Model {
    aterm_spec::ty_model! {
        HyperlinkSchemeCap {
            const Buggy = 0;
            const Cap = 4;

            var orca = 0;
            var others = 0;
            var never = 0;

            action Authorize when (orca == 0 && orca + others <= Cap - 1) {
                orca = 1;
            }
            action AuthorizeOther when (
                (Buggy == 1 && others <= Cap) || orca + others <= Cap - 1)
            {
                others = others + 1;
            }
            action RefuseAtCap when (orca + others == Cap) {
                others = others;
            }
            action RefuseNeverAllow {
                never = if Buggy == 1 { 1 } else { never };
            }
            action Revoke when (orca == 1) {
                orca = 0;
            }
            action Accept when (orca == 1) {
                orca = orca;
            }

            invariant Bounded: orca + others <= Cap;
            invariant NeverAllowRefused: never == 0;
        }
    }
}

/// Module-wide scrollback budget sharing across live panes.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn shared_budget_model() -> Model {
    aterm_spec::ty_model! {
        SharedScrollbackBudget {
            const Global = 6;
            const HalfShare = 3;
            const Cfg = 6;
            const Buggy = 0;

            var live2 = 0;
            var a1 = 6;
            var a2 = 0;
            var fresh1 = 0;
            var fresh2 = 0;
            var steps = 0;

            action Join when (live2 == 0 && steps <= 5) {
                live2 = 1;
                a2 = Cfg;
                fresh1 = 0;
                fresh2 = 0;
                steps = steps + 1;
            }
            action Leave when (live2 == 1 && steps <= 5) {
                live2 = 0;
                a2 = if Buggy == 2 { a2 } else { 0 };
                fresh1 = 0;
                fresh2 = 0;
                steps = steps + 1;
            }
            action Apply1 when (steps <= 5) {
                a1 = if Buggy == 1 {
                    Cfg
                } else if live2 == 1 {
                    if Cfg > HalfShare { HalfShare } else { Cfg }
                } else if Cfg > Global {
                    Global
                } else {
                    Cfg
                };
                fresh1 = 1;
                steps = steps + 1;
            }
            action Apply2 when (live2 == 1 && steps <= 5) {
                a2 = if Buggy == 1 {
                    Cfg
                } else if Cfg > HalfShare {
                    HalfShare
                } else {
                    Cfg
                };
                fresh2 = 1;
                steps = steps + 1;
            }

            invariant QuiescentSumBounded:
                if fresh1 == 0 {
                    0 <= 1
                } else if live2 == 1 && fresh2 == 0 {
                    0 <= 1
                } else {
                    a1 + a2 <= Global
                };
            invariant DepartedHoldsNothing:
                if live2 == 0 { a2 == 0 } else { 0 <= 1 };
        }
    }
}

/// Multi-process `O_APPEND` log rotation and bounded retention for `aterm.log`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn log_rotation_model() -> Model {
    aterm_spec::ty_model! {
        LogRotation {
            const Cap = 1;
            const Step = 1;
            const Window = 1;
            const MaxLines = 4;
            const Buggy = 0;

            var live = 0;
            var old = 0;
            var aged = 0;
            var written = 0;
            var ga = 0;
            var gb = 0;
            var na = 0;
            var nb = 0;
            var ea = 0;
            var eb = 0;
            var er = 1;

            action Tick when (ea <= Window - 1 || eb <= Window - 1 || er <= Window - 1) {
                ea = if ea <= Window - 1 { ea + 1 } else { ea };
                eb = if eb <= Window - 1 { eb + 1 } else { eb };
                er = if er <= Window - 1 { er + 1 } else { er };
            }

            action WriteA when (
                written <= MaxLines - 1
                && (ga > 0 || live <= Cap - 1 || Window <= er)
                && (Buggy == 1 || (na <= Step - 1 && ea <= Window - 1))
            ) {
                live = if ga == 0 { live + 1 } else { live };
                old = if ga == 1 { old + 1 } else { old };
                written = written + 1;
                na = if na <= Step - 1 { na + 1 } else { na };
            }
            action WriteB when (
                written <= MaxLines - 1
                && (gb > 0 || live <= Cap - 1 || Window <= er)
                && (Buggy == 1 || (nb <= Step - 1 && eb <= Window - 1))
            ) {
                live = if gb == 0 { live + 1 } else { live };
                old = if gb == 1 { old + 1 } else { old };
                written = written + 1;
                nb = if nb <= Step - 1 { nb + 1 } else { nb };
            }

            action CheckA when (Buggy == 0 && (Step <= na || Window <= ea)) {
                aged = if live > Cap { aged + old } else { aged };
                old = if live > Cap { live } else { old };
                live = if live > Cap { 0 } else { live };
                gb = if live > Cap { if gb == 0 { 1 } else { 2 } } else { gb };
                er = if live > Cap { 0 } else { er };
                ga = 0;
                na = 0;
                ea = 0;
            }
            action CheckB when (Buggy == 0 && (Step <= nb || Window <= eb)) {
                aged = if live > Cap { aged + old } else { aged };
                old = if live > Cap { live } else { old };
                live = if live > Cap { 0 } else { live };
                ga = if live > Cap { if ga == 0 { 1 } else { 2 } } else { ga };
                er = if live > Cap { 0 } else { er };
                gb = 0;
                nb = 0;
                eb = 0;
            }

            action RestartB {
                aged = if live > Cap && Buggy == 0 { aged + old } else { aged };
                old = if live > Cap && Buggy == 0 { live } else { old };
                live = if live > Cap { 0 } else { live };
                ga = if live > Cap && Buggy == 0 { if ga == 0 { 1 } else { 2 } } else { ga };
                er = if live > Cap && Buggy == 0 { 0 } else { er };
                gb = 0;
                nb = 0;
                eb = 0;
            }

            invariant EveryLineOnce: live + old + aged == written;
            invariant FilesBounded:
                live <= Cap + Step + Step && old <= Cap + Step + Step + Step;
        }
    }
}
