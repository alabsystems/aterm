// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Later-wave models: audio trails, input pairing, maintenance lanes, search, capture, prewarm, budgets — spec-model
//! data constructors moved verbatim out of the one-file catalog in `derive.rs`
//! (pure code motion; every constructor keeps its `crate::derive` path via the
//! `pub use` re-exports there).

use super::*;

/// Trail-audio UI→worker handoff and AudioQueue idle lifecycle.
///
/// The event loop may perform only a bounded nonblocking enqueue. The shipping
/// FIFO has 64 slots; `CommandCap=2` is its finite-state projection, preserving
/// the only decisions that matter here: an available slot accepts the newest cue,
/// while a full FIFO leaves all queued cues intact and drops/accounts the newest.
/// Every device open/start/push belongs to the worker. Once a worker applies a cue,
/// it resets the exact-silence counter before pause housekeeping can observe it. A
/// running queue owns exactly one low-rate worker timeout, and a successful idle
/// pause (or explicit start failure) disarms it. This is a safety model: the audio
/// device may fail, but failure is explicit and cannot turn every future keystroke
/// into another synchronous platform call.
///
/// `Buggy=1` combines the regression family deliberately: the UI touches the
/// platform while enqueuing, blocks and omits drop accounting on a full queue,
/// and a cue applied to an already-running device retains the pre-cue silence
/// threshold.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn trail_audio_lifecycle_model() -> Model {
    crate::ty_model! {
        TrailAudioLifecycle {
            const Buggy = 0;
            const CommandCap = 2;
            const DropCap = 2;
            const SilentCap = 2;
            // Bounded projection of the shipping capacity-64 SyncSender.
            var queued = 0;
            var dropped = 0;
            // Marks the most recent ingress decision as a full-queue drop.
            var last_full = 0;
            var running = 0;
            var silent = 0;
            var service_deadline = 0;
            // True only between worker cue application and its first render.
            var cue_applied = 0;
            var failed = 0;
            var paused = 0;
            // Observable event-loop violations; healthy executions keep both zero.
            var ui_blocked = 0;
            var ui_platform_calls = 0;

            action PushCueAvailable when (
                queued <= CommandCap - 1 && failed == 0
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
            action WorkerStart when (queued > 0 && running == 0 && failed == 0) {
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
                service_deadline = 0;
                paused = 1;
            }
            action ParkIdle when (running == 0 && queued == 0 && service_deadline == 0) {
                running = running;
            }

            invariant WorkerMailboxIsBounded: queued <= CommandCap;
            invariant DropAccountingIsBounded: dropped <= DropCap;
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
            invariant IdlePauseDisarmsDeadline:
                if paused == 1 {
                    running == 0 && silent == SilentCap && service_deadline == 0
                } else {
                    paused == 0
                };
            invariant StartFailureIsExplicitAndTerminal:
                if failed == 1 {
                    running == 0 && queued == 0 && service_deadline == 0
                } else {
                    failed == 0
                };
        }
    }
}

/// AudioQueue buffer ownership and first-audible-buffer latency across cold
/// start and idle resume.
///
/// The three queue buffers are allocated but AVAILABLE while stopped. A cue is
/// applied before the worker renders and enqueues all three, so the first
/// audible samples occupy buffer 1 (`MaxAudibleBuffer`), not buffer 4 behind
/// three silent priming buffers. Idle uses synchronous immediate stop/reset:
/// callback recycling is disabled first, and all scheduled buffers are
/// reclaimed before a resume prime can write them. The idle worker remains
/// disarmed throughout.
///
/// `Buggy=1` reproduces both dangerous alternatives around the regression: the
/// retired pre-enqueued/pause-retained silence puts sound in buffer 4, while a
/// naive attempt to overwrite those still-scheduled pointers on resume records
/// an ownership violation. It also removes the enqueue/stop gate, admitting a
/// stop while the callback is inside its queue-enqueue critical section. Tier-1 binding:
/// `audio_queue_post_cue_prime_conforms_with_callback_free_fake` drives the
/// shipping generic prime/stop helpers with an ownership-checking fake queue.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn trail_audio_start_latency_model() -> Model {
    crate::ty_model! {
        TrailAudioStartLatency {
            const Buggy = 0;
            const BufferCount = 3;
            const MaxAudibleBuffer = 1;
            // 0 available/cold, 1 cold cue, 2 cold primed, 3 cold running,
            // 4 idle stopped, 5 resume cue, 6 resume primed, 7 resumed,
            // 8 an old-generation callback returned after resume.
            var phase = 0;
            var available = 3;
            var queued = 0;
            var recycling = 0;
            var running = 0;
            // One-based queue position; zero means no cue has been primed.
            var audible_buffer = 0;
            var unsafe_writes = 0;
            var idle_wakes = 0;
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
            // With both values bounded to 0..1, `enqueue_in_flight <= Buggy`
            // is exactly `Buggy == 1 OR enqueue_in_flight == 0`, expressed in
            // the intentionally small ty_model expression grammar.
            action StopIdle when (phase == 3 && enqueue_in_flight <= Buggy) {
                phase = 4;
                available = if Buggy == 1 { 0 } else { BufferCount };
                queued = if Buggy == 1 { BufferCount } else { 0 };
                recycling = 0;
                running = 0;
                audible_buffer = 0;
                generation = if Buggy == 1 { generation } else { generation + 1 };
                stop_overlap = if enqueue_in_flight == 1 { 1 } else { 0 };
                enqueue_in_flight = 0;
            }
            action ParkIdle when (phase == 4 && idle_wakes == 0) {
                idle_wakes = 0;
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
            invariant IdleIsCallbackAndWakeFree:
                if phase == 4 {
                    running == 0 && recycling == 0 && idle_wakes == 0 &&
                    enqueue_in_flight == 0
                } else {
                    idle_wakes == 0
                };
            invariant PhaseBounded: phase <= 8;
        }
    }
}

/// Tab-stop checkpoint preservation across a narrow seamless handoff.
///
/// Grid width and tab-stop backing length are deliberately different: shrinking
/// a terminal keeps bounded stops beyond the visible width so a later grow has
/// the same tab semantics. A carried projection is admitted exactly when it
/// covers the current columns and remains within the protocol maximum. The
/// mutant truncates backing storage at shrink/restore and loses the off-width
/// custom stop.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn tab_stop_handoff_model() -> Model {
    crate::ty_model! {
        TabStopHandoff {
            const Buggy = 0;
            const Narrow = 4;
            const Wide = 8;
            const Future = 6;
            const MaxCols = 8;
            // phase: 0 source narrow, 1 source wide, 2 custom stop set,
            // 3 source narrowed, 4 captured, 5 admitted, 6 restored,
            // 7 destination grown, 8 tab executed, 9 invalid supplied,
            // 10 invalid rejected.
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
                rejected = 1;
            }
            action RejectOversizeProjection when (
                phase == 9 && carried_len > MaxCols
            ) {
                phase = 10;
                rejected = 1;
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

/// Event-loop lane-isolation contract for bulk scrollback maintenance.
///
/// Ordinary PTY output is latency-critical and must never begin a terminal scan,
/// take a blocking lock, or perform eviction. The explicitly delivered OS-memory-
/// pressure event is the sole bulk-work capability. The allocator/evictor is
/// abstracted behind `unbounded_work` and `mutation`; this property is about event
/// routing, not a claim that emergency reclamation itself has a small bound. The
/// mutant reintroduces the historical defect by admitting output directly into
/// the bulk lane, so prove-and-catch is non-vacuous.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn scrollback_maintenance_lane_model() -> Model {
    crate::ty_model! {
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
            invariant MutationRequiresMemoryPressure:
                if mutation == 1 { event == 2 } else { event <= 2 };
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

/// History-retention contract for a full-width, top-anchored partial scroll.
///
/// Terminal UIs such as Codex keep status/input rows below a DECSTBM region and
/// scroll committed transcript lines through row zero. Such a scroll archives
/// exactly the displaced row when primary-screen history is enabled, while an
/// interior region, a horizontally margined rectangle, or a history-free
/// alternate-screen grid remains ephemeral. Rows below the vertical margin are
/// preserved in every regime. The mutant is the historical aterm behavior: it
/// silently drops the eligible displaced row merely because the region is not
/// full-height.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn top_anchored_scroll_history_model() -> Model {
    crate::ty_model! {
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
            // SELECTION CUSTODY Phase 4: whether the selection lies OUTSIDE the rows
            // this scroll damages. Chosen alongside the regime below, because the two
            // together are what decide the selection's fate — and before Phase 4 the
            // model could not express the question at all.
            var selection_disjoint = 0;

            // Split into DISJOINT / OVERLAPPING flavours rather than adding a phase:
            // renumbering phases would touch every invariant in this model, while a
            // second dimension on the existing choice keeps `phase == 2` tests intact.
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
                // SELECTION CUSTODY Phase 4: the non-archival regimes no longer clear
                // unconditionally. An interior or margined scroll damages only ITS
                // rows, so a selection disjoint from them survives — which is the
                // whole reported bug (a status bar repaint destroying a highlight up
                // in scrollback). An OVERLAPPING selection still clears in both
                // variants: over-clearing is safe, a stale highlight over replaced
                // content is not.
                selection_alive = if top == 0 && full_width == 1 && history_enabled == 1 {
                    if Buggy == 1 { 0 } else { selection_alive }
                } else if selection_disjoint == 1 {
                    if Buggy == 1 { 0 } else { selection_alive }
                } else {
                    0
                };
                // The remap is what a splice does to rows ABOVE its boundary — i.e.
                // to a selection the scrolled region actually covers. A selection
                // BELOW the region does not move, archival regime or not, so
                // `selection_disjoint` governs here too. Without the second
                // conjunct this said that every archival scroll shifts the
                // selection by one, which is false of `adjust_for_row_splice` for a
                // selection under the boundary — and unwitnessable: the only real
                // grid transition the old invariant admitted for
                // `ChooseArchivalDisjoint` was one whose region DID cover the
                // selection, i.e. the overlapping shape wearing the disjoint label.
                selection_region_row = if top == 0 && full_width == 1 && history_enabled == 1
                    && selection_disjoint == 0 {
                    if Buggy == 1 { selection_region_row } else { selection_region_row - 1 }
                } else {
                    selection_region_row
                };
                selection_footer_row = selection_footer_row;
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
            // SELECTION CUSTODY Phase 4 restated this. It used to assert
            // `selection_alive == 0` for EVERY settled non-archival regime — i.e. it
            // PROVED that an interior or margined region scroll must kill the
            // selection, which is exactly the defect. Now it says: a selection
            // survives iff it was piecewise-remapped (the archival regime) OR it was
            // disjoint from the damage.
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
            invariant StateIsBounded:
                phase <= 2 && top <= 1 && full_width <= 1 &&
                history_enabled <= 1 && history_len <= 1 && footer <= 1 &&
                footer_anchor <= 1 && selection_alive <= 1 &&
                selection_region_row <= 2 && selection_footer_row <= 4 &&
                selection_disjoint <= 1;
        }
    }
}

/// Settings ▸ Manual host diagnostics run on one worker behind a one-entry
/// request channel and a one-entry latest-wins pending slot. The modeled
/// revision is the monotonic identity of a `(document revision, host analysis
/// generation)` pair, so byte-identical environment refreshes participate in
/// the same stale-completion law as text edits. Every request remains
/// represented until it completes; only the exact current identity may publish.
/// `Buggy=1` retains the older pending request when a third generation arrives
/// while the channel is full, reproducing stale diagnostics or loss of the
/// final edit.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn manual_config_diagnostics_lane_model() -> Model {
    crate::ty_model! {
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
            invariant RevisionsBounded:
                current_revision <= MaxRevision &&
                channel_revision <= MaxRevision && active_revision <= MaxRevision &&
                pending_revision <= MaxRevision && completed_revision <= MaxRevision &&
                published_revision <= MaxRevision;
        }
    }
}

/// Config/font generation publication: a completion may publish only when its
/// ticket is still the newest request. The mutant accepts generation one after
/// generation two was requested, reproducing a stale face/config rollback.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn font_catalog_generation_model() -> Model {
    crate::ty_model! {
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
            invariant GenerationsBounded:
                requested <= 2 && completed <= 2 && published <= 2;
        }
    }
}

/// Path-feed snapshot publication binds a prepared consumer and its content
/// fingerprint to one bounded, same-handle admitted read. `live` is the
/// pathname's currently visible byte generation; `admitted` is the immutable
/// source returned by that read; and `prepared`/`fingerprint` are the two
/// projections derived from those admitted bytes. Later pathname replacement
/// or ABA restoration may move `live`, but cannot alter either projection.
///
/// `Buggy=1` reproduces the former TOCTOU shape: `Publish` re-reads `live` for
/// the fingerprint instead of retaining the value produced by `Read`. A
/// mutation after admission then publishes a consumer from one generation with
/// another generation's fingerprint.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn path_feed_snapshot_model() -> Model {
    crate::ty_model! {
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
            invariant Bounds:
                live <= 2 && admitted <= 2 && prepared <= 2 &&
                fingerprint <= 2 && read <= 1 && published <= 1;
        }
    }
}

/// A theme-catalog generation can overtake an expensive font/config prepare.
/// The current config observation must be re-prepared against that newer theme
/// rather than publishing assets derived from the old theme or being dropped.
/// The mutant publishes the sequence-current but theme-stale completion.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn font_theme_generation_model() -> Model {
    crate::ty_model! {
        FontThemeGeneration {
            const Buggy = 0;
            var requested = 0;
            var theme = 0;
            var completed = 0;
            var completed_theme = 0;
            var published = 0;
            var published_theme = 0;
            var stale_published = 0;

            action RequestConfig when (requested == 0) { requested = 1; }
            action ThemeChanged when (requested == 1 && theme == 0) { theme = 1; }
            action CompleteOldTheme when (
                requested == 1 && theme == 1 && completed == 0
            ) {
                completed = 1;
                completed_theme = 0;
            }
            action ReprepareLatestTheme when (
                completed == requested && theme > completed_theme
            ) {
                published = if Buggy == 1 { completed } else { published };
                published_theme = if Buggy == 1 {
                    completed_theme
                } else { published_theme };
                stale_published = if Buggy == 1 { 1 } else { stale_published };
                requested = 2;
                completed = 0;
            }
            action CompleteLatestTheme when (
                requested == 2 && theme == 1 && completed == 0
            ) {
                completed = 2;
                completed_theme = 1;
            }
            action PublishCurrent when (
                completed == requested && completed_theme == theme
            ) {
                published = completed;
                published_theme = completed_theme;
                completed = 0;
            }

            invariant StaleThemeNeverPublishes: stale_published == 0;
            invariant PublishedThemeIsCurrentOrEmpty:
                published == 0 || published_theme == theme;
            invariant GenerationsBounded:
                requested <= 2 && theme <= 1 && completed <= 2 &&
                completed_theme <= 1 && published <= 2 && published_theme <= 1;
        }
    }
}

/// Staged-update activation through both native and portable menu faces.
///
/// Once a strictly-newer update is staged and the Version surface is refreshed,
/// its `ApplyUpdate` row is present, enabled, and decodes to the apply command.
/// That decision is independent of whether a terminal or a native Settings/About
/// tab is frontmost: only genuinely terminal-only actions use that content gate.
/// This is a menu-model/dispatch contract; the AppKit click relay is bound at its
/// pure tag/validation seams rather than claiming an automated OS click harness.
/// The mutant reproduces the v0.53 regression by applying the terminal-tab gate
/// to `ApplyUpdate`, leaving the visible update row grey and unselectable.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_menu_activation_model() -> Model {
    crate::ty_model! {
        NativeUpdateMenuActivation {
            const Buggy = 0;
            var staged = 0;
            // 0 native Settings/About tab, 1 terminal tab.
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
///
/// Winit reports modifier snapshots and focus as independent window events. Focus
/// loss must unconditionally invalidate the previous ambient snapshot, because a
/// backend may omit the matching key-up. Every SUBSEQUENT snapshot remains
/// authoritative regardless of the current focus bit: Windows can report the
/// genuinely held modifiers before `Focused(true)` and need not repeat them.
///
/// `Buggy=1` reproduces only the unsafe reset transition: `FocusOut` retains Ctrl.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn focus_modifier_cache_model() -> Model {
    crate::ty_model! {
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
            invariant StateBounds:
                focused <= 1 && cached_ctrl <= 1 && fresh_ctrl <= 1 &&
                delivered_ctrl <= 1;
        }
    }
}

/// SELECTION CUSTODY: which key events may take the user's reading position.
///
/// The viewport has two owners. The TAIL-FOLLOWER owns it while
/// `display_offset == 0`; the USER owns it from the moment a scroll gesture lifts
/// the view off the bottom. Ownership transfers back on exactly ONE event — a
/// byte-producing key PRESS, which is what a user means by "start typing and jump
/// back to the prompt". The selection has one owner, the user, and is dropped only
/// by that same press or by a deliberate deselect.
///
/// Three event classes must NOT disturb either:
///
/// * a key RELEASE (a Kitty `REPORT_EVENT_TYPES` key-up is not typing),
/// * an auto-REPEAT tick (the same held press continuing — re-running the snap at
///   the ~30 Hz repeat rate destroyed any scroll or selection made mid-hold),
/// * a bare MODIFIER or lock key (Shift/Control/Alt/Super/Caps…), which is the
///   first half of ⌘-C and the Shift of a shift-click extend.
///
/// …and neither must OUTPUT: a program writing while the user reads must repin the
/// offset so the SAME content stays under the eye, never slide the view or hand
/// the viewport back to the tail-follower. That is the second half of the user's
/// complaint.
///
/// Output and the SELECTION are a different matter, and the distinction is the
/// whole point of Phase 4: output that REPLACED the selected rows may destroy the
/// highlight (leaving it painted over new text is what makes a copy return
/// something the user never selected), and ED 3 / `clear_scrollback` / RIS take
/// the viewport back outright because the space the offset named is gone. Both are
/// their own event kinds below rather than exceptions swept under one invariant —
/// the earlier `OutputNeverTakesCustodyOrSelection` asserted the opposite of what
/// the engine does and would have licensed deleting the damage test.
///
/// The custody invariants are stated over the OBSERVABLE state — `offset`,
/// `owner`, `selection` — against a shadow of the pre-action values
/// (`prev_offset`, `prev_owner`, `prev_selection`) that every action writes from
/// the pre-state. They deliberately do NOT read a self-reported "this press
/// disturbed something" flag: such a flag is written by hand at every site, so an
/// implementation that moved the viewport without setting it would satisfy the
/// invariant while shipping the bug. Here, moving the viewport IS the violation.
///
/// `Buggy=1` reproduces the regression FAMILY. Two members are the literal
/// shipping defect — an inert press and an auto-repeat tick each snap the viewport
/// and clear the selection, which is what made ⌘-C copy nothing (the ⌘ keydown
/// destroyed the selection before the `c` arrived). The others are the
/// neighbouring regressions the same invariants must catch: a release that
/// disturbs, output that snaps the reader back to live instead of repinning,
/// output that takes custody while clearing the rows it damaged, and a typing
/// press that deselects without snapping. Every invariant here except the two
/// state-consistency guards (`TailOwnerAtBottom`, `StateBounds`) is falsified by
/// one of them, and `derived_ring_ty.rs` checks that invariant by invariant — an
/// invariant no mutant can falsify is a ghost, which is how this model's first
/// draft passed while stating nothing.
///
/// Scope: this is the PRESS-PATH half of custody (design Phase 1) plus the output
/// repin it must not fight. The alt-screen round-trip, scrollback eviction and
/// reflow anchoring are Phase 3 and are deliberately absent rather than asserted
/// against code that does not yet implement them.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn press_custody_model() -> Model {
    crate::ty_model! {
        PressCustody {
            const Buggy = 0;
            const MaxOffset = 2;

            // 0 = the tail-follower owns the viewport, 1 = the user does.
            var owner = 0;
            // Bounded projection of `Grid::display_offset`.
            var offset = 0;
            // A completed text selection exists.
            var selection = 0;
            // Shadow of the pre-action observable state. Every action writes all
            // three from the PRE-state, so an invariant can compare what an event
            // did against what was there before it.
            var prev_owner = 0;
            var prev_offset = 0;
            var prev_selection = 0;
            // What kind of event just fired: 0 a user gesture, 1 a typing press,
            // 2 an auto-repeat tick, 3 a bare modifier, 4 a release, 5 output that
            // missed the selected rows, 6 output that REPLACED them, 7 output that
            // invalidated the coordinate space (ED 3 / clear_scrollback / RIS).
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
            // A plain click that deselects — a deliberate clear, always allowed.
            action UserClear {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                selection = 0;
                last_event = 0;
            }
            // Output while the user is reading: the repin keeps the same content
            // under the eye, so the offset RISES with the new lines and ownership
            // stays with the user.
            action OutputWhileReading when (owner == 1 && offset <= MaxOffset - 1) {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = if Buggy == 1 { 0 } else { offset + 1 };
                owner = if Buggy == 1 { 0 } else { 1 };
                selection = if Buggy == 1 { 0 } else { selection };
                last_event = 5;
            }
            // Output while the tail-follower owns the viewport: stays at live.
            action OutputAtLive when (owner == 0) {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = 0;
                last_event = 5;
            }
            // Output that REPLACED the selected rows (Phase 4's overlapping
            // `SelectionDamage`). It is a separate event kind because the shipped
            // engine really does destroy a selection here — a highlight left over
            // replaced text makes ⌘-C return something the user never selected —
            // and a model that forbade it would contradict the code it stands for.
            // What it still may not do is take the reading position: the repin
            // rides the new lines exactly as for undamaged output.
            //
            // BOTH OWNERSHIPS. The guard used to read `owner == 1`, which disabled
            // the action outright at live — and at live is where its commonest real
            // instance happens: a status line or progress bar repainting the rows a
            // highlight sits on, which is the shipped defect this design came from.
            // The engine records that shape unconditionally, so the narrow guard
            // meant the dominant instance of one of the eleven actions was a step the
            // spec could not admit, while the gate called the machine 11/11 bound.
            // The OFFSET clause carries the ownership split instead of the guard: the
            // repin rides the arriving lines only for a reader, and at live the view
            // was already at live and stays there. The `Buggy = 1` member survives
            // unchanged — at `owner == 1` it still snaps the reader to live, which is
            // what `OutputNeverTakesCustody` catches.
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
            // ED 3 / `clear_scrollback` / RIS: `repin_display_offset` clamps to 0
            // because the coordinate space the offset named is gone. This IS output
            // handing the viewport back to the tail-follower, so it is spelled as
            // its own event kind rather than left to contradict the custody
            // invariant. The model does not police the LABEL — as with
            // `selection_custody_model`'s `WholesaleInvalidate`, an implementation
            // that called everything wholesale would satisfy it; what the pair of
            // kinds buys is that the ordinary path is checked and the exception is
            // enumerated where a reader can see it.
            action OutputInvalidatesTheCoordinateSpace {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = 0;
                owner = 0;
                // `Buggy = 1` keeps the highlight across the invalidation — a
                // selection naming rows that no longer exist, which is the
                // fail-OPEN direction this design exists to rule out.
                selection = if Buggy == 1 { selection } else { 0 };
                last_event = 7;
            }
            // The ONE handover. Unchanged by this design — but `Buggy = 1` still
            // gives it a member (typing that deselects without snapping), because
            // an invariant no mutant can falsify states nothing.
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
                    last_event <= 7
                };
            invariant RepeatPressIsInert:
                if last_event == 2 {
                    offset == prev_offset && owner == prev_owner &&
                    selection == prev_selection
                } else {
                    last_event <= 7
                };
            invariant ReleaseIsInert:
                if last_event == 4 {
                    offset == prev_offset && owner == prev_owner &&
                    selection == prev_selection
                } else {
                    last_event <= 7
                };
            // Output never takes the reading position — the half of the old
            // `OutputNeverTakesCustodyOrSelection` that is TRUE of the shipped
            // system. It covers BOTH output kinds: damaging the selected rows is
            // not a licence to snap. `prev_offset <= offset` rather than equality,
            // because the repin RAISES the offset by the lines that arrived; what
            // it may never do is move the view back toward live.
            invariant OutputNeverTakesCustody:
                if last_event == 5 {
                    owner == prev_owner && prev_offset <= offset
                } else {
                    if last_event == 6 {
                        owner == prev_owner && prev_offset <= offset
                    } else {
                        last_event <= 7
                    }
                };
            // …and the selection half, narrowed to the case where it holds. The
            // old form asserted this of ALL output and was false of the system
            // that shipped: Phase 4 clears a selection whose rows the output
            // replaced (event 6), and `post_process`'s fail-closed arm clears one
            // it cannot place. WHICH damage may clear is `selection_custody_model`'s
            // `OverlapDamageClears`; here the claim is only that output which did
            // not touch the selected rows leaves the highlight alone.
            invariant OutputSparesAnUndamagedSelection:
                if last_event == 5 {
                    selection == prev_selection
                } else {
                    last_event <= 7
                };
            invariant TypingLandsAtLive:
                if last_event == 1 {
                    offset == 0 && owner == 0 && selection == 0
                } else {
                    last_event <= 7
                };
            // The ONE handover, and the discipline it owes. ED 3 / `clear_scrollback`
            // / RIS destroy the coordinate space the anchors are stated in, so
            // handing the viewport back is correct — but a selection may not
            // OUTLIVE the space that gives its rows meaning. Every other invariant
            // reaches event 7 only through an `else { last_event <= 7 }` arm, which
            // is trivially true; without this one the model admits the action and
            // then says nothing whatever about it.
            invariant InvalidationCannotLeaveADanglingSelection:
                if last_event == 7 {
                    offset == 0 && owner == 0 && selection == 0
                } else {
                    last_event <= 7
                };
            invariant StateBounds:
                owner <= 1 && offset <= MaxOffset && selection <= 1 &&
                prev_owner <= 1 && prev_offset <= MaxOffset && prev_selection <= 1 &&
                last_event <= 7;
        }
    }
}

/// SELECTION CUSTODY — when a selection may be destroyed, as a lifecycle.
///
/// The whole design in one model: a highlight is the user's, and only an act that
/// destroys the CONTENT it names may destroy it. The shipped defect was the
/// opposite — twenty-five grid sites set a `content_scroll_delta = i32::MAX`
/// sentinel meaning "kill the selection", fired by ops that had not touched the
/// selected rows at all, so a status bar repainting at the bottom of the screen
/// destroyed a highlight anchored far up in scrollback.
///
/// The model works in the ABSOLUTE row space that `intersects_absolute_band`
/// converts into — NOT in the selection's own space. `TextSelection` anchors are
/// relative `i32` rows and stay that way; the conversion happens at the damage
/// boundary only. Absolute is the right abstraction here because a uniform scroll
/// advances `absolute_row_counter` by the same delta `adjust_for_scroll` subtracts,
/// so the interval is stable under ordinary output.
///
/// Damage bands are LITERAL per action rather than chosen, because the `ty_model!`
/// grammar has no nondeterministic range arm. Two bands and three selections give
/// all four overlap/disjoint pairings plus the single-row shape eviction needs:
/// `RegionDamageLow` covers rows 0..1, `RegionDamageHigh` covers row 3; `SelectLow`
/// takes rows 0..1, `SelectHigh` rows 2..3, and `SelectOldest` row 0 alone.
///
/// Every invariant is stated over the OBSERVABLE change — the post-state against a
/// shadow of the pre-state (`prev_alive`, `prev_sel_lo`, `prev_sel_hi`) that every
/// action writes before mutating — never over a value the action itself just wrote.
/// That is not a style preference: as first written this model guarded `InertPress`
/// and `UniformScroll` on `alive == 1` and had them write nothing but `last_event`,
/// so `alive == 1` followed from the GUARD and the two invariants whose comments
/// claim to state the user's complaints could not fail whatever the engine did.
/// They are ghosts unless a mutant can falsify them, which is why every action that
/// may not destroy a selection now carries a `Buggy = 1` branch that destroys one.
///
/// SCOPE, stated rather than implied: this models the ENGINE's destroyers — the
/// ones reachable from `Terminal::post_process` and its callers. The GUI has its
/// own clear sites (`app_search`, `app_mouse`, `subscribe`, `control_host`) and the
/// wasm bridges have theirs; none of them are in this environment, and a claim that
/// this enumerates "every legitimate destroyer" would be false.
///
/// `WholesaleInvalidate` is ONE action, not an alt-enter/alt-exit pair, because the
/// three `force_selection_invalidation` callers outside the screen switch — ED 3,
/// `clear_scrollback`, and a Kitty unscroll that renumbers history — are one class:
/// the coordinate space itself is gone, so no band can describe the damage and
/// `All` is the honest answer.
///
/// `Buggy = 1` is the regression FAMILY, one member per destroyer, each falsifying
/// a NAMED invariant (`derived_ring_ty.rs` asserts that mapping member by member,
/// so a future ghost fails the suite rather than passing it):
///
/// * `RegionDamageLow` clears whatever it hit — the literal shipping sentinel,
///   caught by `DisjointDamagePreserves`.
/// * `RegionDamageHigh` never clears — the inverse hole Phase 4 closed, where a
///   highlight survives over replaced text and the copy returns something the user
///   never selected. Caught by `OverlapDamageClears`.
/// * `InertPress` and `UniformScroll` each destroy the selection — the two user
///   complaints at the engine layer, caught by their own invariants.
/// * `Evict` reports the loss without acting on it: it raises the floor and records
///   `truncated`, but neither clamps the head nor drops a selection both of whose
///   endpoints are gone. Caught by `PartialEvictionTruncates`, `NoDanglingAnchors`
///   and `TruncationImpliesAClampedHead`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn selection_custody_model() -> Model {
    crate::ty_model! {
        SelectionCustody {
            const Buggy = 0;

            // A completed selection exists.
            var alive = 0;
            // Its absolute row interval, `sel_lo <= sel_hi`.
            var sel_lo = 0;
            var sel_hi = 0;
            // The last damage band recorded this step.
            var band_lo = 0;
            var band_hi = 0;
            // The oldest retained absolute row. Rises on eviction.
            var floor = 0;
            // A head clamped to the floor by partial eviction.
            var truncated = 0;
            // Shadow of the PRE-action selection, written by every action from the
            // pre-state. Assignments are TLA+ simultaneous, so these read the state
            // as it was BEFORE the step even where they are spelled first. This is
            // what lets an invariant say "this event changed nothing" about an
            // action that writes nothing.
            var prev_alive = 0;
            var prev_sel_lo = 0;
            var prev_sel_hi = 0;
            // What just happened. ORDERED, so membership tests are prefixes — the
            // grammar renders `&&` unparenthesised and `||` parenthesised, so
            // mixing them is unsafe and every test below is a nested `if`.
            // 0 none, 1 user gesture, 2 typing press, 3 inert press,
            // 4 region damage, 5 uniform scroll, 6 eviction, 7 wholesale.
            var last_event = 0;

            // Guarded on the floor: you cannot select a row that has been evicted,
            // because it is not on screen to select. Without this the model reaches
            // `alive` with `sel_lo` below `floor` by selecting row 0 after an
            // eviction — a dangling anchor no code path can produce, which would
            // make `NoDanglingAnchors` false for a reason the terminal never has.
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
            // A one-row selection on the oldest retained row. It exists so the
            // BOTH-ENDPOINTS-GONE arm of eviction is reachable at all: with only
            // `SelectLow` and `SelectHigh` every live selection has `sel_hi > 0`,
            // so `Evict`'s clear branch was dead and the law "a selection whose
            // whole interval fell off the back is gone" went unmodelled.
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
            // A deliberate deselect — always allowed, it IS the user's intent.
            action UserClear {
                prev_alive = alive;
                prev_sel_lo = sel_lo;
                prev_sel_hi = sel_hi;
                alive = 0;
                truncated = 0;
                last_event = 1;
            }
            // The ONE handover: typing means take me to the prompt, and deselect.
            action TypingPress {
                prev_alive = alive;
                prev_sel_lo = sel_lo;
                prev_sel_hi = sel_hi;
                alive = 0;
                truncated = 0;
                last_event = 2;
            }
            // A bare modifier. The guard keeps the state space to selections that
            // existed to be destroyed; the INVARIANT does not lean on it — it
            // compares against `prev_alive`, so removing the guard later cannot
            // silently turn the property back into a tautology.
            action InertPress when (alive == 1) {
                prev_alive = alive;
                prev_sel_lo = sel_lo;
                prev_sel_hi = sel_hi;
                alive = if Buggy == 1 { 0 } else { alive };
                last_event = 3;
            }
            // Ordinary output while the user reads: the anchors ride the content, so
            // in the absolute space the interval does not move at all.
            action UniformScroll when (alive == 1) {
                prev_alive = alive;
                prev_sel_lo = sel_lo;
                prev_sel_hi = sel_hi;
                alive = if Buggy == 1 { 0 } else { alive };
                last_event = 5;
            }
            // Rows 0..1 replaced. Overlap reduces to `sel_lo <= 1`, because
            // `band_lo == 0 <= sel_hi` holds for every reachable selection.
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
            // Row 3 replaced. Overlap reduces to `sel_hi > 2`, because
            // `sel_lo <= 3 == band_hi` holds for every reachable selection. Its
            // mutant is the INVERSE of `RegionDamageLow`'s — damage that hit and
            // did not clear — so the two halves of the lattice each have a member.
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
            // Retention drops the oldest row. A head below the new floor CLAMPS and
            // records the loss; only both endpoints gone destroys the selection.
            // Every right-hand side reads the PRE-eviction state — assignments are
            // simultaneous — which is what lets `truncated` and `sel_lo` agree.
            // `truncated = 0` on the survivor branch is the pre-value restated, not
            // a reset: `Evict` is the only writer of `1` and its `floor == 0` guard
            // lets it fire at most once per trace.
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
            // ED 3 / clear_scrollback / RIS / Kitty unscroll: the coordinate space
            // itself is gone.
            action WholesaleInvalidate when (alive == 1) {
                prev_alive = alive;
                prev_sel_lo = sel_lo;
                prev_sel_hi = sel_hi;
                alive = 0;
                truncated = 0;
                last_event = 7;
            }

            // A bare modifier expresses no intent and may not destroy anything.
            // This is complaint (1), as a model property — stated as "the event
            // CHANGED nothing", which is falsifiable, rather than "a selection
            // exists", which the guard would have granted for free.
            invariant InertPressPreservesTheSelection:
                if last_event == 3 {
                    alive == prev_alive && sel_lo == prev_sel_lo &&
                    sel_hi == prev_sel_hi
                } else {
                    alive <= 1
                };
            // Ordinary output cannot take a highlight. This is complaint (2).
            invariant UniformScrollPreservesTheSelection:
                if last_event == 5 {
                    alive == prev_alive && sel_lo == prev_sel_lo &&
                    sel_hi == prev_sel_hi
                } else {
                    alive <= 1
                };
            // Damage that missed the selected rows leaves them alone — the half the
            // sentinel could not express, and the reason a status bar destroyed a
            // scrollback highlight. Disjointness is decided on the PRE-state
            // anchors and the conclusion is read off the POST-state.
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
            // …and damage that HIT them must clear: a highlight left over replaced
            // text is worse than a lost one, because a copy then returns something
            // the user never selected.
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
            // Losing the oldest line of a selection is not losing the selection —
            // the head clamps to the new floor and the loss is RECORDED. Losing
            // every line of it is: both endpoints gone means there is nothing left
            // to name. Which arm applies is decided by the PRE-eviction interval.
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
            // Whatever else happens, a live selection never names an evicted row.
            invariant NoDanglingAnchors:
                if alive == 1 { floor <= sel_lo } else { alive == 0 };
            // A truncation is only ever recorded against a clamped head.
            invariant TruncationImpliesAClampedHead:
                if truncated == 1 { floor <= sel_lo } else { truncated == 0 };
            // A model-bounds guard, not a design claim: it states the space is the
            // one the interpreter was asked to walk. It is the ONE invariant here
            // with no `Buggy = 1` member, and `derived_ring_ty.rs` names it as such.
            invariant StateIsBounded:
                alive <= 1 && truncated <= 1 && floor <= 1 && sel_lo <= 3 &&
                sel_hi <= 3 && band_lo <= 3 && band_hi <= 3 &&
                prev_alive <= 1 && prev_sel_lo <= 3 && prev_sel_hi <= 3 &&
                last_event <= 7;
        }
    }
}

/// SELECTION CUSTODY — the alt-screen selection PARK, as a lifecycle.
///
/// A text selection belongs to the screen it was made on. Entering the alternate
/// screen parks the main screen's selection and leaves the alt buffer with none;
/// leaving restores it and leaves the parked slot EMPTY. The engine spells that as
/// two `mem::take`s at the top of `Terminal::post_process`.
///
/// The one property worth stating is the slot's lifetime: `parked_sel` is alive
/// only between an enter and the next leave. That is what bounds the clear-site
/// list to the handful of wholesale destroyers (`Terminal::reset`, byte-stream RIS,
/// `clear_scrollback`, a width resize, `restore_checkpoint`) instead of making every
/// future destroyer acquire a second obligation for a selection that outlived it.
///
/// `Buggy=1` is the regression family, two members naming two different invariants.
/// The first is the obvious alternative implementation — a symmetric SWAP instead of
/// an asymmetric take — under which the alt screen's own selection stays parked
/// after the leave and reappears on the next round trip, over a buffer the user
/// cannot see. The second is the wholesale destroyer that clears the live selection
/// and forgets the parked one, which is exactly the failure mode of five of the six
/// coordinated clear sites: nothing in the compiler notices.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn alt_selection_park_model() -> Model {
    crate::ty_model! {
        AltSelectionPark {
            const Buggy = 0;

            // 1 while the alternate screen is up.
            var on_alt = 0;
            // A selection on the ACTIVE screen, whichever that is.
            var live_sel = 0;
            // The OTHER screen's selection, held across the switch.
            var parked_sel = 0;
            // What just fired: 0 a user gesture, 1 an enter, 2 a wholesale
            // destroyer, 3 a leave.
            var last_event = 0;

            action Select {
                live_sel = 1;
                last_event = 0;
            }
            action Deselect {
                live_sel = 0;
                last_event = 0;
            }
            // The park. Assignments are TLA+ simultaneous, so every right-hand side
            // reads the PRE-state: at Buggy=1 this pair is a swap, and at Buggy=0 the
            // live slot is emptied outright.
            action Enter when (on_alt == 0) {
                parked_sel = live_sel;
                live_sel = if Buggy == 1 { parked_sel } else { 0 };
                on_alt = 1;
                last_event = 1;
            }
            // The restore. At Buggy=1 the alt screen's selection lands in the parked
            // slot instead of dying with the buffer it named.
            action Leave when (on_alt == 1) {
                live_sel = parked_sel;
                parked_sel = if Buggy == 1 { live_sel } else { 0 };
                on_alt = 0;
                last_event = 3;
            }
            // RIS, clear_scrollback, a width resize, restore_checkpoint: the content
            // under BOTH selections is gone, so both slots must go.
            action Wholesale {
                live_sel = 0;
                parked_sel = if Buggy == 1 { parked_sel } else { 0 };
                last_event = 2;
            }

            invariant ParkedEmptyOffAlt:
                if on_alt == 0 { parked_sel == 0 } else { parked_sel <= 1 };
            invariant WholesaleLeavesNothingParked:
                if last_event == 2 {
                    live_sel == 0 && parked_sel == 0
                } else {
                    last_event <= 3
                };
            invariant StateBounds:
                on_alt <= 1 && live_sel <= 1 && parked_sel <= 1 && last_event <= 3;
        }
    }
}

/// One-key press/repeat/release pairing at the GUI-to-PTY boundary.
///
/// A press consumed by a physical-key GUI gate or by the engine-key overlay gate
/// records a per-key disposition. Repeats only peek at that disposition, and the
/// matching release removes it without emitting Kitty CSI-u bytes. Conversely, a
/// press already forwarded to the PTY remains outstanding through overlay/gate
/// changes and is owed its repeat/release reports at the exact press-time target.
/// The two-window lattice makes focus move while the key is held: the physical
/// release may arrive through window 2, but a consumed owner remains byte-silent
/// a forwarded encoded/literal owner routes to window/session 1, and an
/// untracked repeat is byte-silent. The model projects one entry
/// of the process-wide physical-key owner map; other-key independence and the full
/// window/session identity are Tier-1 negative controls. Explicit controller
/// releases with no preceding physical episode are outside this pairing environment.
///
/// `Buggy=1` reproduces the regression family: a consumed press emits an orphan
/// release/repeat, a repeat re-decides or consumes the press-time disposition,
/// closing an overlay clears its still-needed tracker entry, an overlay swallows
/// the untracked release owed to a pre-overlay forwarded press, or repeat/release
/// routing re-resolves current focus instead of retaining the original target.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn input_release_pairing_model() -> Model {
    crate::ty_model! {
        InputReleasePairing {
            const Buggy = 0;
            // phase: 0 idle, 1 held, 2 release complete, 3 physical focus epoch ended.
            var phase = 0;
            // tracker: 0 none, 1 physical GUI gate, 2 engine overlay gate,
            // 3 literal text/raw payload, 4 repeatable local/native action.
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
            // Two-window focus/ownership projection. Physical repeats/releases
            // may arrive through `focused_window`, but routing authority remains
            // the immutable `press_window` for the entire held-key episode.
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
                release_arrival_window = focused_window;
            }
            action ReleaseLocalRepeatPress when (
                phase == 1 && press_local == 1 && tracker == 4
            ) {
                phase = 2;
                tracker = 0;
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
            invariant StateBounds:
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

/// Full two-phase overlap handoff shared by native-update parent and child.
///
/// The parent parks its readers, a readerless child paints and proves the exact
/// adoption/layout/session set, the parent rechecks every mutable admission fact,
/// and one atomic Commit+exit grants ownership. The child releases its prebuilt
/// reader gate directly from Commit; the later UI wake is diagnostic only. Every
/// pre-Commit failure signals the candidate's entire process group and then reaps
/// the direct child before parent readers resume or deferred teardown replays —
/// including when the group leader has already exited but a descendant is live.
/// The guarded v0.52 one-byte branch requires an
/// exact, strictly-newer, zero-history legacy payload before ACK+release.
/// This is a process/event-loop interleaving model through the atomic Commit
/// linearization point. It does not claim power-loss durability, and physical
/// input arriving after Commit linearizes belongs to the replacement process's
/// normal input path rather than this parent's pre-Commit admission snapshot.
///
/// `Buggy=1` admits the regression family: partial/stale proof Commit, reader
/// release on ProofReady, legacy ACK for ambiguous/scrolled state, parent resume
/// before reap, and the old wait-before-group-signal ordering that could strand a
/// live descendant after its leader exited.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_overlap_handoff_model() -> Model {
    crate::ty_model! {
        NativeUpdateOverlapHandoff {
            const Buggy = 0;
            // protocol: 0 modern two-channel, 1 guarded legacy one-byte ACK.
            var protocol = 0;
            // phase: 0 live parent, 1 parked, 2 readerless child, 3 proof ready,
            // 4 modern Commit+parent exit, 5 child active, 6 killed, 7 reaped,
            // 8 parent resumed, 9 deferred teardown replayed.
            var phase = 0;
            var parent_readers = 1;
            var child_readers = 0;
            // `child_live` is the direct process-group leader. Its waitable
            // identity remains owned after exit until `child_reaped`.
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
            // SEAMLESS (2026-07): sessions_alive is the fail-closed core of the
            // retired ptys_quiet fact — session DEATH (HUP/ERR) still rejects,
            // while queued readable output (pty_output_queued below) is now
            // tolerated: post-park bytes wait gap-free in the kernel and replay
            // through the child's fresh parser after Commit.
            var sessions_alive = 1;
            var pty_output_queued = 0;
            // Queued-but-undispatched hardware input DEFERS Commit (it must be
            // drained through the live input path into the persistent masters
            // before `_exit` can be allowed to run) but is no longer a failure.
            var input_queue_quiet = 1;
            var native_safe = 1;
            var commit_channel = 1;
            // Shared atomic linearization point after ProofReady:
            // 0 Waiting, 1 Committing, 2 Rejecting, 3 Committed.
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

            action SelectLegacyBridge when (phase == 0 && protocol == 0) {
                protocol = 1;
                commit_channel = 0;
            }
            action ParkParentReaders when (phase == 0 && parent_readers == 1) {
                phase = 1;
                parent_readers = 0;
                parent_parked = 1;
            }
            action SpawnReaderlessChild when (
                phase == 1 && parent_parked == 1 && child_live == 0
            ) {
                phase = 2;
                child_live = 1;
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
            // Queued OUTPUT during the overlap is NOT a failure: the parent
            // provably consumes no post-park bytes, so the carried checkpoint
            // stays a valid ground-state prefix and the child drains the queue
            // after Commit. Commit admission never reads this variable.
            action PtyOutputQueues when (
                phase == 3 && pty_output_queued == 0 && arbiter == 0
            ) {
                pty_output_queued = 1;
            }
            // Session DEATH keeps the old fail-closed semantics: the adoption
            // proof's live-set identity is stale, so the attempt must reject.
            action PtySessionDies when (
                phase > 1 && phase <= 3 && sessions_alive == 1 &&
                arbiter == 0 && failure == 0
            ) {
                sessions_alive = 0;
                failure = 1;
            }
            // Hardware input queued in the OS is no longer a failure — it
            // parks Commit (the admission guard requires the queue drained)
            // until the run loop delivers it to the persistent masters.
            action QueueHardwareInput when (
                phase == 3 && input_queue_quiet == 1 && arbiter == 0
            ) {
                input_queue_quiet = 0;
            }
            action DrainQueuedHardwareInput when (
                phase == 3 && input_queue_quiet == 0 && arbiter == 0
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
                input_queue_quiet == 1 && native_safe == 1 &&
                failure == 0
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
                arbiter == 1 && commit_admission_exact == 1
            ) {
                phase = 4;
                arbiter = 3;
                commit = 1;
                parent_exited = 1;
            }
            // Shipping deliberately never `try_wait`s after ProofReady. A child
            // that exits before Commit is observed only when the real atomic
            // Commit-pipe write fails (normally EPIPE), then this transition
            // transfers the arbiter to rollback without granting authority.
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
                if child_reaped == 1 {
                    group_signaled == 1 && waited_before_group_signal == 0
                } else {
                    child_reaped == 0
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
                    parent_readers == if parent_resumed_early == 1 { 1 } else { 0 }
                } else {
                    child_readers <= 1
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

/// Exact live-caret DFA for canonical profanity completion.
///
/// Every proper prefix of `fuck`, including `fuc`, is ordinary text. Only the
/// complete token activates and creates an episode; harmless continuations,
/// non-token contexts, ignore rules, and a delimiter-settled `fuc` stay
/// inactive. The mutant reproduces the predictive-prefix regression by
/// activating one character early.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn exact_profanity_completion_model() -> Model {
    crate::ty_model! {
        ExactProfanityCompletion {
            const Buggy = 0;
            // phase: 0 empty, 1 f, 2 fu, 3 fuc, 4 canonical fuck,
            // 5 harmless/context-suppressed, 6 delimiter-settled fuc.
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
                episode = 1;
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
                episode = 0;
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
                episode = 0;
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
            invariant ActiveUsesCanonicalIdentity:
                if active == 1 {
                    canonical_identity == 1 && episode == 1
                } else {
                    canonical_identity == 0 && episode == 0
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

/// Process-crash/interleaving model for the native updater's fixed-path install
/// transaction and boot-health confirmation. This deliberately does **not**
/// claim sudden-power-loss durability: the current atomic ledger writes and
/// renames are not yet bound to file+directory fsync. Process crashes preserve
/// every completed filesystem transition represented here.
///
/// Identity `1` is exact OLD, `2` exact authorized NEW, and `3` a same-build or
/// superseding-but-unauthorized artifact. The committed path verifies OLD's
/// build+sealed commit before preparation, arms the exact trial before one
/// atomic fixed-path swap, records NEW's exact build+commit+digest receipt,
/// reconstructs that receipt after the swap-before-receipt crash cut, and only
/// GCs rollback after a real first-present, health proof, and successful disarm.
/// Startup consumes inherited re-exec/expected-artifact authority before any
/// verifier can inherit it, then observes an armed trial before interpreting that
/// authority. `Buggy=1` enables independent historical-shortcut controls:
/// inexact legacy synthesis, build-only OLD or mismatched prior-receipt
/// authority, an inherited-authority early return, a superseded swap,
/// pre-present disarm, proof-failed early GC, and failed receipt restoration
/// that leaves NEW's receipt authoritative.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_disk_transaction_model() -> Model {
    crate::ty_model! {
        NativeUpdateDiskTransaction {
            const Buggy = 0;
            const MaxCrashes = 3;
            // startup_phase: 0 inherited environment may still be present,
            // 1 authority consumed/cleared, 2 boot-health observed, 3 verdict
            // applied (either the disk lane is live or startup safely returned).
            var startup_phase = 0;
            // inherited_authority: 0 absent, 1 exact matching re-exec/trial,
            // 2 malformed or incomplete expected-artifact authority.
            var inherited_authority = 0;
            var inherited_env_live = 0;
            var boot_health_attempted = 0;
            var boot_health_observed = 0;
            var startup_returned = 0;
            var startup_deferred = 0;
            var disk_lane_ready = 0;
            // A v0.52 parent can hand off an already-swapped NEW with an armed
            // trial and exact OLD rollback but no ready/installed receipt. Keep
            // this as a first-class reachable startup state rather than treating
            // post-swap child startup as if it began from staged OLD.
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
            // phase: 0 staged OLD, 1 fixed NEW prepared, 2 trial armed,
            // 3 atomically swapped, 4 exact receipt available, 5 disarmed,
            // 6 terminal, 7 crashed after swap/before receipt, 8 crash-loop
            // rollback required, 9 exact OLD restored but trial still armed.
            var phase = 0;
            // Installed/fixed identity: 0 absent, 1 exact OLD, 2 exact NEW,
            // 3 unauthorized identity.
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
            // OLD's pre-transaction receipt is unsigned local state until it is
            // bound to OLD's just-verified sealed build+commit. Only that bound
            // value may be retained for an exec-failure inverse swap.
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
            // No receipt and no ready record: there is no evidence left to rebuild
            // from. The retired v0.52 branch synthesized one here from sealed disk
            // facts; without it the shipping code returns "no exact receipt or
            // authorized recovery record" and defers, preserving the armed trial
            // and fixed rollback for a later launch. Modeling that refusal is what
            // keeps this state reachable-but-not-stuck.
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
                inherited_env_live == 0
            ) {
                rollback_verified = 1;
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
                rollback_verified == 1 && proof_failed == 0 &&
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
                receipt_exact == 1 && rollback_verified == 1 && fixed == 1
            ) {
                phase = 6;
                fixed = 0;
                fixed_exact = 0;
                gc = 1;
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
            invariant ModernReadyRecoveryRequiresReadyAndIsExact:
                if modern_receipt_recovered == 1 {
                    ready_present == 1 &&
                    (if rollback_restored == 1 {
                        if rollback_gc == 1 { receipt_exact == 0 } else { receipt_exact == 1 }
                    } else {
                        receipt_exact == 1
                    })
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
                    health_proved == 1 && disarmed == 1 && trial == 0 &&
                    health_disarmed == 1 && first_present_done == 1 &&
                    receipt_exact == 1 && rollback_verified == 1 && fixed == 0
                } else {
                    disarmed <= 1
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
            invariant CrashBudgetBounded: crashes <= MaxCrashes;
        }
    }
}

/// Settings' semantic pager is a bounded virtual cursor shared by visible
/// Previous/Next controls, absolute accessibility scroll positions, and signed
/// wheel/controller line deltas. Every transition first clamps an obsolete cursor
/// to the current limit, then applies its motion and clamps again. `Buggy=1`
/// recreates the missing upper clamp for Next, Absolute, and forward line scroll.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn settings_page_scroll_model() -> Model {
    crate::ty_model! {
        SettingsPageScroll {
            const Buggy = 0;
            const Max = 3;
            var limit = 0;
            var cursor = 0;
            // Absolute-scroll input; Max + 1 exercises an out-of-range request.
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
            action PreviousPage {
                cursor = if cursor > 0 { cursor - 1 } else { 0 };
            }
            action NextPage {
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
            invariant LimitBounded: limit <= Max;
            invariant TargetBounded: target <= Max + 1;
        }
    }
}

/// VIDEO recording and publication are one serialized, fail-closed lifecycle.
///
/// The control thread pre-creates a private, server-named directory before the
/// event-loop request is armed. `recording_slot` owns that request through the
/// pending/recording phases; `BeginExport` atomically hands ownership to the
/// process-wide `export_permit`. Success transfers the directory from private
/// ownership to a published artifact, while rejection, cancellation, encode
/// failure, owner loss, and a live opacity transition all remove it.
///
/// Mode is honest about the presentation source: a swapchain tap requires
/// glass, an offscreen present-real recording requires no glass, and only the
/// latter owns a pacing timer. A translucent transition aborts a live tap before
/// another frame can be accepted.
///
/// `Buggy=1` exposes independently reachable historical failure classes:
/// cleanup may strand the private directory, a windowed request may take the
/// headless arm, and a second recording may acquire ownership while the first is
/// still exporting. It also exposes publication without winning the cancellation
/// CAS and retention of a tap after the glass becomes translucent. Tier-0
/// exercises each mutant directly in addition to the exhaustive prove-and-catch
/// check.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn video_recording_lifecycle_model() -> Model {
    crate::ty_model! {
        VideoRecordingLifecycle {
            const Buggy = 0;
            // 0 Idle, 1 Reserved/pending, 2 Recording, 3 Exporting.
            var phase = 0;
            var glass = 0;
            // 0 None, 1 SwapchainTap, 2 OffscreenPresentReal.
            var mode = 0;
            var timer = 0;
            var translucent = 0;
            // The pending/recording side and exporting side of the serialized
            // lifecycle. They must never overlap.
            var recording_slot = 0;
            var export_permit = 0;
            var active = 0;
            // Private, server-owned directories for the current lifecycle.
            var private_dirs = 0;
            // The current lifecycle completed its atomic publication.
            var published = 0;
            // Publication CAS: 0 Live, 1 Cancelled, 2 CommitAuthorized.
            var cancel_state = 0;
            // A bounded witness that cancellation lost to commit authorization.
            var late_cancel = 0;

            action AttachGlass when (phase == 0) { glass = 1; }
            action DetachGlass when (phase == 0) { glass = 0; }
            action MakeOpaque when (phase == 0 && translucent == 1) {
                translucent = 0;
            }
            action Reserve when (
                phase == 0 && recording_slot == 0 &&
                export_permit == 0 && active == 0
            ) {
                phase = 1;
                mode = 0;
                timer = 0;
                recording_slot = 1;
                active = 1;
                private_dirs = 1;
                published = 0;
                cancel_state = 0;
                late_cancel = 0;
            }
            action BeginOnGlass when (
                phase == 1 && glass == 1 && translucent == 0
            ) {
                phase = 2;
                mode = 1;
                timer = 0;
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
            action Tick when (phase == 2 && mode == 2 && timer == 1) {
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
                phase == 3 && cancel_state == 2 && late_cancel == 0
            ) {
                cancel_state = 2;
                late_cancel = 1;
            }
            action PublishSuccess when (
                phase == 3 && cancel_state == 2
            ) {
                phase = 0;
                mode = 0;
                timer = 0;
                export_permit = 0;
                active = 0;
                private_dirs = 0;
                published = 1;
                late_cancel = 0;
            }
            action BuggyPublishWithoutAuthorization when (
                Buggy == 1 && phase == 3 && cancel_state == 0
            ) {
                phase = 0;
                mode = 0;
                timer = 0;
                export_permit = 0;
                active = 0;
                private_dirs = 0;
                published = 1;
                late_cancel = 0;
            }
            action RejectBegin when (phase == 1) {
                phase = 0;
                mode = 0;
                timer = 0;
                recording_slot = 0;
                active = 0;
                private_dirs = 0;
                published = 0;
                cancel_state = 1;
                late_cancel = 0;
            }
            action CancelLive when (phase > 0 && cancel_state == 0) {
                phase = 0;
                mode = 0;
                timer = 0;
                recording_slot = 0;
                export_permit = 0;
                active = 0;
                private_dirs = 0;
                published = 0;
                cancel_state = 1;
                late_cancel = 0;
            }
            action Fail when (phase > 0) {
                phase = 0;
                mode = 0;
                timer = 0;
                recording_slot = 0;
                export_permit = 0;
                active = 0;
                private_dirs = 0;
                published = 0;
                cancel_state = 1;
                late_cancel = 0;
            }
            action OwnerLost when (phase > 0) {
                phase = 0;
                mode = 0;
                timer = 0;
                recording_slot = 0;
                export_permit = 0;
                active = 0;
                private_dirs = 0;
                published = 0;
                cancel_state = 1;
                late_cancel = 0;
            }
            // Why: the mutant must be its OWN action, not a `Buggy` arm inside a
            // live one — the strict-vacuity audit removes the dead set and
            // requires the remaining Buggy=1 baseline to be safe, so an inline
            // arm would make every dead action uncreditable as a negative control.
            action BuggyStrandPrivateDirOnCleanup when (
                Buggy == 1 && phase > 0
            ) {
                phase = 0;
                mode = 0;
                timer = 0;
                recording_slot = 0;
                export_permit = 0;
                active = 0;
                private_dirs = 1;
                published = 0;
                cancel_state = 1;
                late_cancel = 0;
            }
            action MakeTapTranslucent when (
                phase == 2 && mode == 1 && translucent == 0
            ) {
                translucent = 1;
                phase = 0;
                mode = 0;
                timer = 0;
                recording_slot = 0;
                active = 0;
                private_dirs = 0;
                published = 0;
                cancel_state = 1;
                late_cancel = 0;
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
                active = 2;
                private_dirs = 2;
            }

            invariant Bounds:
                phase <= 3 && glass <= 1 && mode <= 2 && timer <= 1 &&
                translucent <= 1 && recording_slot <= 1 &&
                export_permit <= 1 && active <= 2 && private_dirs <= 2 &&
                published <= 1 && cancel_state <= 2 && late_cancel <= 1;
            invariant ModeMatchesRecordingPhase:
                if phase == 2 { mode > 0 } else { mode == 0 };
            invariant OffscreenOnlyWithoutGlass:
                (if mode == 2 { glass } else { 0 }) == 0;
            invariant TapOnlyOnGlass:
                (if mode == 1 { glass } else { 1 }) == 1;
            invariant NoTranslucentTap:
                (if mode == 1 { translucent } else { 0 }) == 0;
            invariant OffscreenTimerExact:
                if phase == 2 && mode == 2 { timer == 1 } else { timer == 0 };
            invariant RecordingExportSerialized:
                recording_slot + export_permit <= 1;
            invariant ActiveAccounting:
                active == recording_slot + export_permit;
            invariant AtMostOneActiveLifecycle:
                active <= 1 && private_dirs <= 1;
            invariant PrivateDirectoryOwnedByLifecycle:
                if phase > 0 { private_dirs == 1 } else { private_dirs == 0 };
            invariant CancelledOwnsNothing:
                if cancel_state == 1 {
                    phase == 0 && active == 0 && private_dirs == 0 &&
                    recording_slot == 0 && export_permit == 0 &&
                    published == 0
                } else {
                    cancel_state == 0 || cancel_state == 2
                };
            invariant CommitAuthorizationScope:
                if cancel_state == 2 {
                    (phase == 3 && published == 0) ||
                    (phase == 0 && published == 1)
                } else {
                    published == 0
                };
            invariant LateCancellationCannotRevoke:
                if late_cancel == 1 {
                    cancel_state == 2 && phase == 3 && export_permit == 1
                } else {
                    late_cancel == 0
                };
            invariant SlotMatchesPhase:
                if phase == 1 || phase == 2 {
                    recording_slot == 1 && export_permit == 0
                } else if phase == 3 {
                    recording_slot == 0 && export_permit == 1
                } else {
                    recording_slot == 0 && export_permit == 0
                };
            invariant PublishedOnlyAfterOwnershipTransfer:
                if published == 1 {
                    phase == 0 && active == 0 && private_dirs == 0 &&
                    recording_slot == 0 && export_permit == 0 &&
                    cancel_state == 2
                } else {
                    published == 0
                };
        }
    }
}

/// Retention of per-process capture namespaces is decided by the exact instance
/// lease, not merely by PID liveness.
///
/// Lease state `0` is a legacy namespace with no lease file, `1` is a lock held
/// by a live exact process instance, `2` is a well-formed but freely lockable
/// stale instance, and `3` is malformed/untrusted lease metadata. A held lease
/// and malformed metadata fail closed to KEEP. A free lease is exact proof of
/// staleness and is removed even if its recorded PID has since been reused.
/// Only the missing-legacy case falls back to the coarse PID probe.
///
/// `Buggy=1` independently recreates removal of held/malformed namespaces and
/// the PID-reuse leak that keeps a free lease when the numeric PID is alive.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn exact_instance_retention_model() -> Model {
    crate::ty_model! {
        ExactInstanceRetention {
            const Buggy = 0;
            // 0 MissingLegacy, 1 Held, 2 Free, 3 Malformed.
            var lease = 0;
            var pid_alive = 0;
            // 0 Pending, 1 Keep, 2 Remove.
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
                    if pid_alive == 1 { 1 } else { 2 }
                } else if lease == 1 {
                    if Buggy == 1 { 2 } else { 1 }
                } else if lease == 2 {
                    if Buggy == 1 && pid_alive == 1 { 1 } else { 2 }
                } else {
                    if Buggy == 1 { 2 } else { 1 }
                };
            }

            invariant Bounds:
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

/// A confined artifact operation is a retained-handle transaction, not a path
/// string that may be resolved again after an attacker swaps an ancestor.
///
/// `ConfinePin` retains the original inside object and its identity. Reads and
/// writes target that pinned object even when the path's ancestor is swapped.
/// `ValidateReply` then compares the reply-time path identity with the pin:
/// unchanged identity may be certified; a swap fails closed, whether it happened
/// before the operation or in the operation-to-reply interval.
///
/// `Buggy=1` exposes both forbidden TOCTOU classes: re-resolving a swapped path
/// can read/write the outside object, and reply construction can certify the
/// swapped identity despite having pinned a different object.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn anchored_artifact_transaction_model() -> Model {
    crate::ty_model! {
        AnchoredArtifactTransaction {
            const Buggy = 0;
            // 0 Unconfined, 1 Pinned, 2 Operated, 3 Replied.
            var phase = 0;
            var pinned = 0;
            var swapped = 0;
            // 0 None, 1 OriginalInside, 2 SwappedOutside.
            var path_identity = 0;
            // 0 None, 1 Read, 2 Write.
            var operation = 0;
            // 0 None/fail-closed, 1 OriginalInside, 2 Outside.
            var effect_target = 0;
            var validated = 0;
            // 0 Pending, 1 Success, 2 FailClosed.
            var reply = 0;
            // 0 None, 1 OriginalInside, 2 SwappedOutside.
            var certified_identity = 0;

            action ConfinePin when (phase == 0) {
                phase = 1;
                pinned = 1;
                swapped = 0;
                path_identity = 1;
                operation = 0;
                effect_target = 0;
                validated = 0;
                reply = 0;
                certified_identity = 0;
            }
            action SwapAncestor when (
                phase > 0 && phase <= 2 && swapped == 0
            ) {
                swapped = 1;
                path_identity = 2;
            }
            action ReadPinned when (phase == 1 && pinned == 1) {
                phase = 2;
                operation = 1;
                effect_target = 1;
            }
            action WritePinned when (phase == 1 && pinned == 1) {
                phase = 2;
                operation = 2;
                effect_target = 1;
            }
            action ValidateReply when (
                (phase == 1 && swapped == 1) || phase == 2
            ) {
                phase = 3;
                validated = 1;
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
                validated = 1;
                reply = 1;
                certified_identity = 2;
            }

            invariant Bounds:
                phase <= 3 && pinned <= 1 && swapped <= 1 &&
                path_identity <= 2 && operation <= 2 &&
                effect_target <= 2 && validated <= 1 && reply <= 2 &&
                certified_identity <= 2;
            invariant ActiveTransactionIsPinned:
                if phase > 0 { pinned == 1 } else { pinned == 0 };
            invariant PathIdentityTracksAncestor:
                if phase > 0 {
                    if swapped == 1 {
                        path_identity == 2
                    } else {
                        path_identity == 1
                    }
                } else {
                    path_identity == 0
                };
            invariant OperationRequiresPinnedObject:
                if operation > 0 {
                    pinned == 1 && phase > 1 && effect_target > 0
                } else {
                    effect_target == 0
                };
            invariant AnchoredAccessNeverOutside:
                effect_target <= 1;
            invariant CompletedReplyWasValidated:
                if phase == 3 {
                    validated == 1 && reply > 0
                } else {
                    validated == 0 && reply == 0 && certified_identity == 0
                };
            invariant FailedReplyCertifiesNothing:
                if reply == 2 { certified_identity == 0 } else { reply <= 1 };
            invariant SuccessfulReplyCertifiesOriginal:
                if reply == 1 {
                    operation > 0 && effect_target == 1 &&
                    path_identity == 1 && swapped == 0 &&
                    certified_identity == 1
                } else {
                    reply == 0 || reply == 2
                };
            invariant SwappedPathNeverCertified:
                if swapped == 1 {
                    reply == 0 || (reply == 2 && certified_identity == 0)
                } else {
                    certified_identity <= 1
                };
        }
    }
}

/// A filesystem artifact reply is a transaction that spans two threads, the
/// complete control-socket write, and a nonce-bound peer acknowledgement. Timeout
/// cancellation and final-name authorization have one winner. A successful
/// worker queues exact handles; the socket thread revalidates them before any OK
/// byte, then writes and flushes the complete frame plus a fresh nonce challenge.
/// Only the matching post-challenge echo permits immediate release. A failed or
/// abandoned ACK enters an additional central nonblocking quarantine; its two
/// bounded ticks abstract the shipping 30-second delay, and the guard survives
/// until explicit expiry. A partial `write_all` failure enters the same
/// quarantine because path bytes may already be visible even though the complete
/// frame receipt guarantee was never established. A pre-wire abort remains the
/// only direct cleanup path because it removes an unpublished artifact.
///
/// `Buggy=1` exposes six independently audited failures: publishing after timeout
/// won, dropping a queued handle before the reply reaches the wire, pruning a
/// leased artifact, releasing without a valid ACK, accepting a pre-pipelined ACK
/// before the server's causal challenge, or releasing quarantine before expiry.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn artifact_reply_publication_model() -> Model {
    crate::ty_model! {
        ArtifactReplyPublication {
            const QuarantineDelay = 1;
            const Buggy = 0;
            // 0 Live, 1 Cancelled, 2 Authorized, 3 Queued,
            // 4 WirePrepared, 5 WireWritten, 6 PeerAcked,
            // 7 Quarantined, 8 QuarantineExpired, 9 AbortPending,
            // 10 ReleasedAfterAck, 11 ReleasedAfterQuarantine,
            // 12 ReleasedAbort.
            var phase = 0;
            var artifact = 0;
            var guard = 0;
            var committed = 0;
            var reply = 0;
            var challenge = 0;
            var ack = 0;
            var ack_failed = 0;
            var write_error = 0;
            var quarantine = 0;
            var quarantine_age = 0;
            var expired = 0;

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
                quarantine = 1;
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
                quarantine = 1;
                quarantine_age = 0;
            }
            action AdvanceQuarantine when (
                phase == 7 && quarantine == 1 &&
                quarantine_age <= QuarantineDelay - 1
            ) {
                quarantine_age = quarantine_age + 1;
            }
            action ExpireQuarantine when (
                phase == 7 && quarantine == 1 &&
                quarantine_age == QuarantineDelay
            ) {
                phase = 8;
                quarantine = 0;
                expired = 1;
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
                committed = 1;
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
                guard == 1 && committed == 1 && challenge == 0
            ) {
                phase = 6;
                ack = 1;
            }
            action BuggyReleaseQuarantineEarly when (
                Buggy == 1 && phase == 7 && quarantine == 1 &&
                quarantine_age <= QuarantineDelay - 1 && guard == 1
            ) {
                phase = 11;
                guard = 0;
                quarantine = 0;
            }

            invariant Bounds:
                phase <= 12 && artifact <= 1 && guard <= 1 &&
                committed <= 1 && reply <= 1 &&
                challenge <= 1 && ack <= 1 && ack_failed <= 1 &&
                write_error <= 1 && quarantine <= 1 &&
                quarantine_age <= QuarantineDelay && expired <= 1 &&
                ack + ack_failed <= 1 &&
                ack_failed + write_error <= 1;
            invariant CancelledPublishesNothing:
                if phase == 1 {
                    artifact == 0 && guard == 0 && committed == 0 &&
                    reply == 0 && challenge == 0 &&
                    ack == 0 && ack_failed == 0 && write_error == 0 &&
                    quarantine == 0 &&
                    quarantine_age == 0 && expired == 0
                } else {
                    artifact <= 1
                };
            invariant OwnedThroughClassifiedExitRetainsGuard:
                if phase > 1 && phase <= 9 { guard == 1 } else { guard <= 1 };
            invariant LeasedArtifactSurvivesRetention:
                if phase > 1 && phase <= 11 {
                    artifact == 1
                } else {
                    if phase == 12 { artifact == 0 } else { artifact <= 1 }
                };
            invariant CommitRequiresWirePreparation:
                if committed == 1 {
                    (phase > 3 && phase <= 8) ||
                    phase == 10 || phase == 11
                } else {
                    phase <= 3 || phase == 9 || phase == 12
                };
            invariant ReplyRequiresCommittedArtifact:
                if reply == 1 {
                    (phase == 5 || phase == 6 || phase == 7 ||
                    phase == 8 || phase == 10 || phase == 11) &&
                    artifact == 1 && committed == 1 && challenge == 1
                } else {
                    phase <= 4 || phase == 7 || phase == 8 ||
                    phase == 9 || phase == 11 || phase == 12
                };
            invariant ChallengeRequiresCompleteWire:
                if challenge == 1 {
                    reply == 1 &&
                    (phase == 5 || phase == 6 || phase == 7 ||
                    phase == 8 || phase == 10 || phase == 11)
                } else {
                    challenge == 0
                };
            invariant SuccessfulAckRequiresCausalChallenge:
                if ack == 1 {
                    challenge == 1 && reply == 1 && committed == 1 &&
                    artifact == 1 && (phase == 6 || phase == 10)
                } else {
                    ack == 0
                };
            invariant AckFailureEntersQuarantine:
                if ack_failed == 1 {
                    challenge == 1 && reply == 1 && committed == 1 &&
                    artifact == 1 &&
                    (phase == 7 || phase == 8 || phase == 11)
                } else {
                    ack_failed == 0
                };
            invariant WriteFailureEntersQuarantine:
                if write_error == 1 {
                    reply == 0 && challenge == 0 && committed == 1 &&
                    artifact == 1 &&
                    (phase == 7 || phase == 8 || phase == 11)
                } else {
                    write_error == 0
                };
            invariant QuarantineRetainsClassifiedGuard:
                if quarantine == 1 {
                    phase == 7 && guard == 1 && artifact == 1 &&
                    expired == 0 && ack_failed + write_error == 1
                } else {
                    quarantine == 0
                };
            invariant QuarantineAgeMatchesPhase:
                if phase == 7 {
                    quarantine_age <= QuarantineDelay
                } else {
                    if phase == 8 || phase == 11 {
                        quarantine_age == QuarantineDelay
                    } else {
                        quarantine_age == 0
                    }
                };
            invariant QuarantineExpiryIsCausal:
                if expired == 1 {
                    (phase == 8 || phase == 11) &&
                    quarantine == 0 &&
                    quarantine_age == QuarantineDelay &&
                    ack_failed + write_error == 1
                } else {
                    phase <= 7 || phase == 9 ||
                    phase == 10 || phase == 12
                };
            invariant ImmediateReleaseRequiresValidAck:
                if phase == 10 {
                    guard == 0 && reply == 1 && committed == 1 &&
                    artifact == 1 && challenge == 1 &&
                    ack == 1 && ack_failed == 0 && write_error == 0 &&
                    quarantine == 0 && expired == 0 &&
                    quarantine_age == 0
                } else {
                    phase <= 9 || phase > 10
                };
            invariant QuarantineReleaseRequiresExpiry:
                if phase == 11 {
                    guard == 0 && artifact == 1 && committed == 1 &&
                    ack == 0 && ack_failed + write_error == 1 &&
                    quarantine == 0 && expired == 1 &&
                    quarantine_age == QuarantineDelay
                } else {
                    phase <= 10 || phase > 11
                };
            invariant AbortReleaseRemovesUncommittedArtifact:
                if phase == 12 {
                    guard == 0 && artifact == 0 && committed == 0 &&
                    reply == 0 && challenge == 0 &&
                    ack == 0 && ack_failed == 0 && write_error == 0 &&
                    quarantine == 0 &&
                    quarantine_age == 0 && expired == 0
                } else {
                    phase <= 11
                };
        }
    }
}

/// Refcounted `video frames` retention is a separate bounded lifecycle from
/// publication. Multiple readers may share one exact recording. Final identity
/// validation arms one capability-bound convergence sweep. A non-final release
/// cannot start maintenance; the last release schedules it, new acquisitions
/// fail closed while that schedule/sweep is live, and only completion reopens
/// acquisition.
///
/// `pending` is the abstract seam between the last count decrement and
/// `StartSweep`. Shipping code performs both under one registry mutex, so no
/// acquisition can observe the seam. Keeping it explicit makes the ordering
/// obligation checkable instead of hiding it inside one large action.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn artifact_reader_lease_model() -> Model {
    crate::ty_model! {
        ArtifactReaderLease {
            const Cap = 2;
            const Buggy = 0;
            var readers = 0;
            var armed = 0;
            var pending = 0;
            var sweeping = 0;
            var swept = 0;

            action Acquire when (
                readers <= Cap - 1 && pending == 0 && sweeping == 0
            ) {
                readers = readers + 1;
            }
            action Arm when (
                readers > 0 && pending == 0 && sweeping == 0
            ) {
                armed = 1;
            }
            action Release when (readers > 0) {
                pending = if readers == 1 && armed == 1 { 1 } else { pending };
                readers = readers - 1;
            }
            action StartSweep when (
                readers == 0 && armed == 1 && pending == 1 && sweeping == 0
            ) {
                pending = 0;
                sweeping = 1;
            }
            action RejectAcquireWhileSweeping when (
                pending + sweeping > 0
            ) {
                readers = readers;
            }
            action FinishSweep when (
                readers == 0 && armed == 1 && pending == 0 && sweeping == 1
            ) {
                armed = 0;
                sweeping = 0;
                swept = 1;
            }
            action BuggyStartSweepEarly when (
                Buggy == 1 && readers > 0 && armed == 1 &&
                pending == 0 && sweeping == 0
            ) {
                sweeping = 1;
            }
            action BuggyAcquireDuringSweep when (
                Buggy == 1 && pending + sweeping > 0 &&
                readers <= Cap - 1
            ) {
                readers = readers + 1;
            }

            invariant Bounds:
                readers <= Cap && armed <= 1 && pending <= 1 &&
                sweeping <= 1 && swept <= 1;
            invariant OneMaintenancePhase:
                pending + sweeping <= 1;
            invariant MaintenanceExcludesReaders:
                if pending + sweeping > 0 { readers == 0 } else { readers <= Cap };
            invariant MaintenanceRequiresArm:
                if pending + sweeping > 0 { armed == 1 } else { armed <= 1 };
            invariant ArmedLastReleaseSchedulesSweep:
                if readers == 0 && armed == 1 {
                    pending + sweeping == 1
                } else {
                    pending + sweeping <= 1
                };
            invariant FinishedSweepReopensIdle:
                if swept == 1 && readers == 0 &&
                    pending == 0 && sweeping == 0 {
                    armed == 0
                } else {
                    armed <= 1
                };
        }
    }
}

/// Fixed-path snapshot publication is a generation-fenced transaction. Beginning
/// generation two invalidates generation one's completion marker before the new
/// request returns; an overtaken worker may finish encoding privately, but it may
/// not publish either its stale payload or a marker certifying that payload.
///
/// `Buggy=1` recreates the stale-worker race by allowing generation one's commit
/// after generation two has begun. The resulting marker certifies payload one as
/// current even though the path's latest generation is two.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn snapshot_generation_commit_model() -> Model {
    crate::ty_model! {
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
            invariant Bounds:
                latest <= 2 && job <= 2 && payload <= 2 && done <= 1;
            invariant CommittedPayloadIsCurrent:
                (if done == 1 { payload } else { latest }) == latest;
        }
    }
}

/// A native control mutation may stage pixels before the compositor accepts a
/// present. Screenshot capture is authorized only after that present succeeds.
/// A capture makes at most `AttemptLimit` present attempts and then fails closed;
/// dropped attempts before that bound may retry, but they never fall through to
/// reading the previous composited frame. `Buggy=1`
/// reproduces that stale-capture defect by treating a dropped present as capture
/// authorization.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn capture_after_present_model() -> Model {
    crate::ty_model! {
        CaptureAfterPresent {
            const Buggy = 0;
            const AttemptLimit = 3;
            var staged = 0;
            var present_succeeded = 0;
            // One-based number of real present attempts already made. Mutate
            // enters the first attempt; Retry advances to the next one. This is
            // the exact accounting used by run_capture_present_barrier.
            var attempts = 0;
            // 0 Pending, 1 Capture, 2 Retry, 3 FailClosed.
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
                } else if Buggy == 1 {
                    1
                } else if attempts <= AttemptLimit - 1 {
                    2
                } else {
                    3
                };
                captured = if present_succeeded + Buggy > 0 { 1 } else { 0 };
                failed = if (
                    present_succeeded == 0 && Buggy == 0 && attempts == AttemptLimit
                ) { 1 } else { 0 };
                stale_capture = if (
                    present_succeeded == 0 && Buggy == 1
                ) { 1 } else { stale_capture };
                staged = if present_succeeded == 1 { 0 } else { staged };
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
            invariant AttemptsBounded: attempts <= AttemptLimit;
            invariant ValuesBounded:
                staged <= 1 && present_succeeded <= 1 && decision <= 3 &&
                captured <= 1 && failed <= 1 && stale_capture <= 1;
        }
    }
}

/// A platform window photograph may lag a successful native present by one
/// compositor interval. Full-window introspection therefore uses the OS image
/// only for platform chrome. Client authority is the exact physical destination
/// consumed by the serial-bound successful PRESENT — the swapchain image or
/// softbuffer surface — never a later offscreen rerender of equivalent semantic
/// state. Before those destination pixels are stitched under the chrome, both
/// their physical dimensions and their client-origin offset in the photograph
/// must validate.
///
/// The state/action names `frame_presented`, `renderer_bound`,
/// `MarkFramePresented`, and decision code `1` (`StitchRenderer`) are retained
/// for generated-trace and Tier-1 compatibility. Here, "renderer bound" means
/// *present-destination bound*. `geometry_valid` is the conjunction of size and
/// client-origin validation. `os_client_current` records only that the
/// untrusted photograph happens to look current; coincidence never grants
/// authority.
///
/// `Buggy=1` recreates the historical shortcut: accept an unbound client source
/// (a platform client photograph or semantic offscreen rerender) without the
/// exact destination/geometry provenance.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_capture_source_model() -> Model {
    crate::ty_model! {
        NativeCaptureSource {
            const Buggy = 0;
            var frame_presented = 0;
            var geometry_valid = 0;
            var os_client_current = 0;
            // 0 Pending, 1 StitchRenderer (legacy name: stitch the exact
            // successful PRESENT destination), 2 FailClosed.
            var decision = 0;
            // Legacy trace field name; 1 means exact present-destination bound.
            var renderer_bound = 0;
            var captured = 0;
            var failed = 0;
            var stale_capture = 0;
            action MarkFramePresented when (decision == 0) {
                frame_presented = 1;
            }
            action ValidateGeometry when (decision == 0) {
                geometry_valid = 1;
            }
            action PromoteOsClient when (decision == 0) {
                os_client_current = 1;
            }
            action Decide when (decision == 0) {
                decision = if (
                    frame_presented == 1 && geometry_valid == 1
                ) {
                    1
                } else if Buggy == 1 {
                    1
                } else {
                    2
                };
                renderer_bound = if (
                    frame_presented == 1 && geometry_valid == 1
                ) { 1 } else { 0 };
                captured = if Buggy == 1 {
                    1
                } else if frame_presented + geometry_valid == 2 {
                    1
                } else {
                    0
                };
                failed = if (
                    frame_presented + geometry_valid <= 1 && Buggy == 0
                ) { 1 } else { 0 };
                stale_capture = if (
                    Buggy == 1 &&
                    frame_presented + geometry_valid <= 1
                ) { 1 } else { stale_capture };
            }
            invariant NoStaleCapture: stale_capture == 0;
            invariant CaptureUsesRenderer:
                if captured == 1 {
                    decision == 1 && renderer_bound == 1 &&
                    frame_presented == 1 && geometry_valid == 1
                } else {
                    renderer_bound == 0
                };
            invariant DecisionMatchesProvenance:
                if decision == 0 {
                    captured == 0 && failed == 0 && renderer_bound == 0
                } else if frame_presented + geometry_valid == 2 {
                    decision == 1 && captured == 1 && failed == 0 &&
                    renderer_bound == 1
                } else {
                    decision == 2 && captured == 0 && failed == 1 &&
                    renderer_bound == 0
                };
            invariant ValuesBounded:
                frame_presented <= 1 && geometry_valid <= 1 &&
                os_client_current <= 1 && decision <= 2 &&
                renderer_bound <= 1 && captured <= 1 && failed <= 1 &&
                stale_capture <= 1;
        }
    }
}

/// The one-shot presented-destination tap owns exactly one staging buffer. A
/// validated destination copy advances `Armed -> Pending`; the serial-bound
/// successful-present hook advances `Pending -> InFlight`; and only a successful
/// map/conversion produces a frame. Geometry/metadata rejection and every async
/// completion failure terminate explicitly with an error.
///
/// `Buggy=1` recreates a fail-open map callback by publishing a frame result from
/// the error transition without any successful mapping.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn presented_frame_tap_model() -> Model {
    crate::ty_model! {
        PresentedFrameTap {
            const Buggy = 0;
            // 0 Armed, 1 Pending, 2 InFlight, 3 Complete.
            var phase = 0;
            var accepted = 0;
            var mapped = 0;
            // 0 None, 1 Frame, 2 Error.
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
                result = if Buggy == 1 { 1 } else { 2 };
            }
            invariant SuccessRequiresMappedCopy:
                if result == 1 {
                    phase == 3 && accepted == 1 && mapped == 1
                } else {
                    mapped == 0
                };
            invariant ReservedPhaseRequiresAcceptedCopy:
                if phase > 0 && phase <= 2 {
                    accepted == 1 && result == 0
                } else {
                    phase == 0 || phase == 3
                };
            invariant TerminalPhaseHasResult:
                if phase == 3 { result > 0 } else { result == 0 };
            invariant ResultOnlyAtTerminal:
                if result > 0 { phase == 3 } else { phase <= 2 };
            invariant ValuesBounded:
                phase <= 3 && accepted <= 1 && mapped <= 1 && result <= 2;
        }
    }
}

/// One staging slot in the streaming video tap cycles
/// `Free -> Pending -> InFlight -> Free`. Map errors and finalization aborts
/// release the reserved slot while counting one loss. Invalid live colour
/// metadata never reserves a slot and counts one loss only after the fps gate
/// accepted that sampling opportunity (decimation is outside this transition).
/// The bounded harvested-store projection then drives callback arrival
/// `3,1,2`: insertion must publish sorted `1,2,3`, and a two-frame budget must
/// evict the lowest sequence and retain tail `2,3`.
///
/// `Buggy=1` recreates all three fail-open classes: a failed map leaks the slot
/// in `InFlight`, invalid metadata is silently discarded without incrementing
/// the honest `dropped` count, and callback-order append produces `3,1` then
/// evicts the wrong head to retain `1,2`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn video_tap_slot_model() -> Model {
    crate::ty_model! {
        VideoTapSlot {
            const Buggy = 0;
            const MaxDrops = 2;
            // 0 Free, 1 Pending, 2 InFlight.
            var phase = 0;
            var dropped = 0;
            // Bounded one-shot witness that invalid metadata was rejected.
            var invalid = 0;
            // The immediately preceding slot resolution was an error/abort.
            var last_error = 0;
            // Bounded two-frame harvested store under callback arrival 3,1,2.
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
                phase = if Buggy == 1 { 2 } else { 0 };
                dropped = dropped + 1;
                last_error = 1;
            }
            action Abort when (phase > 0 && dropped <= MaxDrops - 1) {
                phase = 0;
                dropped = dropped + 1;
                last_error = 1;
            }
            action RejectInvalidMetadata when (
                invalid == 0 && dropped <= MaxDrops - 1
            ) {
                invalid = 1;
                dropped = if Buggy == 1 { dropped } else { dropped + 1 };
            }
            action HarvestThree when (harvest_phase == 0) {
                harvest_phase = 1;
                store_first = 3;
                store_second = 0;
                evicted = 0;
            }
            action HarvestOne when (harvest_phase == 1) {
                harvest_phase = 2;
                store_first = if Buggy == 1 { 3 } else { 1 };
                store_second = if Buggy == 1 { 1 } else { 3 };
                evicted = 0;
            }
            action HarvestTwo when (harvest_phase == 2) {
                harvest_phase = 3;
                store_first = if Buggy == 1 { 1 } else { 2 };
                store_second = if Buggy == 1 { 2 } else { 3 };
                evicted = 1;
            }
            invariant ErrorResolutionFreesSlot:
                if last_error == 1 { phase == 0 } else { phase <= 2 };
            invariant ReservedSlotHasNoResolvedError:
                if phase > 0 { last_error == 0 } else { phase == 0 };
            invariant InvalidMetadataIsCounted: invalid <= dropped;
            invariant DropCountBounded: dropped <= MaxDrops;
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
            invariant ValuesBounded:
                phase <= 2 && invalid <= 1 && last_error <= 1 &&
                harvest_phase <= 3 && store_first <= 3 &&
                store_second <= 3 && evicted <= 1;
        }
    }
}

/// Cursor trails, predictive ghosts, fades, and rain occupancy are retained in
/// window-local renderer coordinates. Before a frame may present after the
/// focused visible leaf moves, resizes, changes identity, enters/leaves zoom, or
/// changes physical surface/DPI metrics, every charged coordinate-bound effect
/// must be reset and rebound to the new space.
///
/// `Buggy=1` reproduces the former composed/single boolean-only gate: a
/// same-class geometry change leaves the charged effect bound to the old
/// coordinate space.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn layout_coordinate_reset_model() -> Model {
    crate::ty_model! {
        LayoutCoordinateReset {
            const Buggy = 0;
            const MaxCoordinate = 2;
            var coordinate = 0;
            var bound_coordinate = 0;
            var charged = 0;
            // A coordinate mutation must pass Prepare before another Present.
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
            invariant CoordinateBounded:
                coordinate <= MaxCoordinate && bound_coordinate <= MaxCoordinate;
            invariant ValuesBounded:
                charged <= 1 && prepared <= 1 && stale_present <= 1;
        }
    }
}

/// The semantic preview font worker owns one replacement queue slot and tags
/// every job/result with the live renderer generation. Reload clears an obsolete
/// queued fork while an already-running old parse may finish; polling must ignore
/// that stale result and install only the current generation. `Buggy=1` recreates
/// the stale-worker installation defect by accepting every completed generation.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn semantic_prewarm_generation_model() -> Model {
    crate::ty_model! {
        SemanticPrewarmGeneration {
            const Buggy = 0;
            const MaxGeneration = 3;
            var current = 1;
            // 0 means no queued/running/result generation.
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
                // Replacement slot: repeated demand retains the newest current fork.
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
                decision = if result == current {
                    1
                } else if Buggy == 1 {
                    1
                } else {
                    0
                };
                ready = if result == current {
                    1
                } else if Buggy == 1 {
                    1
                } else {
                    ready
                };
                installed = if result == current {
                    result
                } else if Buggy == 1 {
                    result
                } else {
                    installed
                };
                result = 0;
                resolved = 1;
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
            invariant GenerationsBounded:
                current <= MaxGeneration && queued <= MaxGeneration &&
                running <= MaxGeneration && result <= MaxGeneration &&
                installed <= MaxGeneration && observed_current <= MaxGeneration &&
                observed_result <= MaxGeneration;
            invariant FlagsBounded: ready <= 1 && decision <= 1 && resolved <= 1;
        }
    }
}

/// The semantic preview font worker has two coupled ownership decisions beyond
/// its generation guard. Replacing a queued, not-yet-started install job must
/// carry its unique committed renderer base into the newest candidate job. At
/// completion, only an exact generation + request + candidate match may become
/// active; an exact-match construction failure clears the active renderer, while
/// superseded successes are cache-only. `Buggy=1` recreates both historical bug
/// shapes: replacement keeps only the new job's optional base (dropping the
/// displaced base), and a ready mixed/superseded candidate installs as current.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn semantic_prewarm_handshake_model() -> Model {
    crate::ty_model! {
        SemanticPrewarmHandshake {
            const Buggy = 0;
            // Replacement inputs and their resolved one-slot ownership.
            var new_base = 0;
            var replaced_base = 0;
            var replacement_base = 0;
            var replacement_resolved = 0;
            // Result-classification inputs. Each Mark action spreads one Boolean
            // dimension, so DecideResult explores the complete 2^5 lattice.
            var generation_matches = 0;
            var request_matches = 0;
            var candidate_matches = 0;
            var renderer_ready = 0;
            var active_before = 0;
            var active_before_latest = 0;
            // Classification/effect projection. 0 Pending, 1 Ignore stale
            // generation, 2 Install current, 3 Fail closed current, 4 Cache
            // superseded, 5 Ignore failed superseded.
            var decision = 0;
            var installed = 0;
            var failed_closed = 0;
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
                replacement_base = if Buggy == 1 {
                    new_base
                } else if new_base + replaced_base > 0 {
                    1
                } else {
                    0
                };
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
                } else if Buggy == 1 && renderer_ready == 1 {
                    2
                } else if renderer_ready == 1 {
                    4
                } else {
                    5
                };
                installed = if generation_matches == 1 && renderer_ready == 1 {
                    if request_matches == 1 && candidate_matches == 1 {
                        1
                    } else if Buggy == 1 {
                        1
                    } else {
                        0
                    }
                } else {
                    0
                };
                failed_closed = if (
                    generation_matches == 1 && request_matches == 1 &&
                    candidate_matches == 1 && renderer_ready == 0
                ) { 1 } else { 0 };
                cached = if (
                    generation_matches == 1 && renderer_ready == 1 &&
                    request_matches + candidate_matches <= 1 && Buggy == 0
                ) { 1 } else { 0 };
                active_after = if generation_matches == 0 {
                    active_before
                } else if request_matches == 1 && candidate_matches == 1 {
                    if renderer_ready == 1 { 1 } else { 0 }
                } else if Buggy == 1 && renderer_ready == 1 {
                    1
                } else {
                    active_before
                };
                active_after_latest = if generation_matches == 0 {
                    active_before_latest
                } else if request_matches == 1 && candidate_matches == 1 {
                    if renderer_ready == 1 { 1 } else { 0 }
                } else if Buggy == 1 && renderer_ready == 1 {
                    0
                } else {
                    active_before_latest
                };
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
                if installed == 1 {
                    generation_matches == 1 && request_matches == 1 &&
                    candidate_matches == 1 && renderer_ready == 1 &&
                    decision == 2 && active_after == 1 && active_after_latest == 1
                } else {
                    installed == 0
                };
            invariant CurrentFailureFailsClosed:
                if (
                    result_resolved == 1 && generation_matches == 1 &&
                    request_matches == 1 && candidate_matches == 1 &&
                    renderer_ready == 0
                ) {
                    decision == 3 && installed == 0 && failed_closed == 1 &&
                    active_after == 0 && active_after_latest == 0
                } else {
                    failed_closed == 0
                };
            invariant CacheOnlySupersededReady:
                if cached == 1 {
                    result_resolved == 1 && generation_matches == 1 &&
                    request_matches + candidate_matches <= 1 && renderer_ready == 1 &&
                    decision == 4 && installed == 0
                } else if decision == 4 {
                    cached == 1
                } else {
                    cached == 0
                };
            invariant NoncurrentPreservesActive:
                if result_resolved == 0 {
                    active_after == active_before &&
                    active_after_latest == active_before_latest
                } else if generation_matches == 0 {
                    active_after == active_before &&
                    active_after_latest == active_before_latest && installed == 0
                } else if request_matches + candidate_matches <= 1 {
                    active_after == active_before &&
                    active_after_latest == active_before_latest && installed == 0
                } else {
                    active_after <= 1
                };
            invariant InputsBounded:
                new_base <= 1 && replaced_base <= 1 && generation_matches <= 1 &&
                request_matches <= 1 && candidate_matches <= 1 && renderer_ready <= 1 &&
                active_before <= 1 && active_before_latest <= active_before;
            invariant OutputsBounded:
                replacement_base <= 1 && replacement_resolved <= 1 && decision <= 5 &&
                installed <= 1 && failed_closed <= 1 && cached <= 1 &&
                active_after <= 1 && active_after_latest <= active_after &&
                result_resolved <= 1;
        }
    }
}

/// Once a non-host semantic preview renderer is ready, it is exact for that
/// candidate only. Scheduling a different uncached candidate must move the
/// ready renderer into the bounded cache, leaving no mismatched active paint
/// source while the new request is pending. The unready committed host seed is
/// retained because it is the deliberate broad fallback and carries the first
/// worker-generation base. `Buggy=1` recreates the race by retaining a ready,
/// mismatched renderer during the next request.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn semantic_prewarm_request_swap_model() -> Model {
    crate::ty_model! {
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
            invariant RetainedPaintIsExactOrHostSeed:
                if resolved == 1 && active_after == 1 {
                    candidate_matches == 1 || active_ready == 0
                } else {
                    active_after <= 1
                };
            invariant InputsWellFormed:
                if active_present == 0 {
                    active_ready == 0 && candidate_matches == 0
                } else if active_ready == 0 {
                    candidate_matches == 0
                } else {
                    candidate_matches <= 1
                };
            invariant FlagsBounded:
                active_present <= 1 && active_ready <= 1 && candidate_matches <= 1 &&
                should_cache <= 1 && active_after <= 1 && resolved <= 1;
        }
    }
}

/// Session chrome caches own an explicit expiry deadline. One due entry moves
/// into an incremental fan-out cursor; each event-loop turn may inspect/refresh
/// at most `Budget` windows, and the cursor retains the unvisited remainder.
/// Other due entries remain cached and armed. Retiring all cache/cursor state
/// removes the deadline, preserving the event loop's pure-Wait idle state.
///
/// `Buggy=1` models the bulk fan-out: every remaining window is scanned in one
/// turn, violating the UI work budget even though only one session was admitted.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn session_chrome_expiry_model() -> Model {
    crate::ty_model! {
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
                    fresh + 1
                } else {
                    fresh
                };
                remaining = if Buggy == 1 { 0 } else { remaining - 1 };
                scanning = if Buggy == 1 || remaining == 1 { 0 } else { 1 };
                armed = 1;
            }
            action Retire when (fresh + due + scanning > 0) {
                fresh = 0;
                due = 0;
                scanning = 0;
                remaining = 0;
                armed = 0;
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
            invariant ValuesBounded:
                fresh <= Capacity && due <= Capacity && scanning <= 1 && armed <= 1;
        }
    }
}

/// STREAMING SEARCH — the memory-bounded incremental search lifecycle: the
/// drift-free twin of `aterm-search::streaming::StreamingSearch`
/// (`crates/aterm-search/src/streaming/engine/`). SUPERSEDES the never-committed
/// hand `StreamingSearch.tla` the module docs used to reference — this derived
/// model is the spec of record; if the missing hand `.tla` ever surfaces it goes
/// to `aterm-spec-models/specs/legacy/`, never into the active checked set.
///
/// Variable encoding (all scalar; projections are the engine's public accessors):
///   * `state`  — 0 Idle, 1 Searching, 2 HasResults, 3 NoResults
///   * `scanp`  — `scan_progress + 1` (0 encodes the engine's idle `-1`; the
///     `ty_model!` grammar has no negative literals)
///   * `stored` — `results.len()`; `total` — `total_matches`; `cur` —
///     `current_index` (1-based, 0 = none)
///
/// `ScanHit`/`ScanMiss` FOLD the engine's atomic auto-complete on the last row
/// for the Tier-1 unit-effect alphabet (one match or none). `Add` and
/// `Invalidate` likewise represent exactly one added or removed stored match.
/// A general engine call may affect multiple matches atomically; those calls are
/// intentionally outside this exact scalar transition abstraction and remain
/// covered by local/Kani invariants. `Rows`/`MaxTotal` are trace-window bounds
/// (the offload `produced <= W - 1` idiom), NOT engine claims; `MaxResults`
/// mirrors the engine's memory bound (at capacity a hit COUNTS but does not
/// STORE). `Buggy = 1` drops the invalidation clamp — the pre-#7472/#7244
/// index-out-of-range class — so `CurrentIndexValid` catches it.
///
/// Invariants carry the module's historical IDs: `CurrentIndexValid`
/// (INV-SEARCH-1), `MemoryBounded` (INV-SEARCH-3), `ScanProgressConsistent`
/// (INV-SEARCH-5), `TotalMatchesConsistent` (INV-SEARCH-6), plus the
/// state/emptiness shape (`TerminalShape` — HasResults ⇒ stored ≥ 1 ∧ cur ≥ 1,
/// NoResults ⇒ empty). INV-SEARCH-2 (positions valid) and INV-SEARCH-4 (no
/// duplicates) are not scalar-expressible — they stay Kani-owned bounded-local
/// properties, ledger-joined via `proof_anchor!` in
/// `aterm-search/src/streaming/spec_proof_anchors.rs`.
///
/// Tier-1 binding: `aterm-search/tests/conformance_streaming.rs` (bounded
/// unit-effect lockstep against the real engine, with explicit multi-effect
/// boundary controls). Compile-time gate: `aterm-search/build.rs`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn streaming_search_model() -> Model {
    crate::ty_model! {
        StreamingSearch {
            const Rows = 3;        // bounded scan window (trace bound, not an engine claim)
            const MaxResults = 2;  // memory bound, deliberately < max reachable matches
            const MaxTotal = 4;    // bounds Add/ScanHit fan-out so the space is finite
            const Wrap = 1;        // wraparound nav config; Tier-0 also checked at Wrap=0
            const Buggy = 0;       // 1 = drop the invalidation clamp (pre-#7472/#7244 class)
            var state = 0;
            var scanp = 0;
            var stored = 0;
            var total = 0;
            var cur = 0;

            // start_search: valid from ANY state (a new pattern restarts even a
            // finished or mid-flight search); reset counters, scan from row 0.
            action Start {
                state = 1; scanp = 1; stored = 0; total = 0; cur = 0;
            }
            // scan_row finding one match; folds the auto-complete on the last row.
            // At capacity the hit COUNTS but does not STORE (INV-SEARCH-3 discipline).
            action ScanHit when (state == 1 && scanp <= Rows && total <= MaxTotal - 1) {
                stored = if stored <= MaxResults - 1 { stored + 1 } else { stored };
                total = total + 1;
                state = if scanp > Rows - 1 { 2 } else { 1 };
                cur   = if scanp > Rows - 1 { 1 } else { 0 };
                scanp = if scanp > Rows - 1 { 0 } else { scanp + 1 };
            }
            // scan_row finding nothing; completion picks HasResults/NoResults by
            // whether anything was stored earlier in the scan.
            action ScanMiss when (state == 1 && scanp <= Rows) {
                state = if scanp > Rows - 1 { if stored > 0 { 2 } else { 3 } } else { 1 };
                cur   = if scanp > Rows - 1 { if stored > 0 { 1 } else { 0 } } else { cur };
                scanp = if scanp > Rows - 1 { 0 } else { scanp + 1 };
            }
            // next_match / prev_match: 1-based cycle over the STORED results;
            // Wrap=0 clamps at the boundary instead (the engine's wrap_enabled).
            // (Named after the engine methods — a bare `Next` action would clash
            // with the generated TLA+ `Next ==` disjunction, the `Append` class.)
            action NextMatch when (state == 2 && stored > 0) {
                cur = if cur > stored - 1 { if Wrap > 0 { 1 } else { cur } } else { cur + 1 };
            }
            action PrevMatch when (state == 2 && stored > 0) {
                cur = if cur <= 1 { if Wrap > 0 { stored } else { cur } } else { cur - 1 };
            }
            // content_added with a fresh matching row: store-or-count, NoResults
            // revives to HasResults, first result claims cur = 1.
            action Add when ((state == 2 || state == 3) && total <= MaxTotal - 1) {
                stored = if stored <= MaxResults - 1 { stored + 1 } else { stored };
                total = total + 1;
                state = 2;
                cur = if cur == 0 { 1 } else { cur };
            }
            // content_invalidated of ONE stored match's row: the engine subtracts
            // removed STORED matches only (operations.rs), and clamps cur to the
            // new length — the clamp Buggy=1 drops.
            action Invalidate when (state == 2 && stored > 0) {
                stored = stored - 1;
                total = total - 1;
                state = if stored > 1 { 2 } else { 3 };
                cur = if Buggy > 0 { cur } else { if cur > stored - 1 { stored - 1 } else { cur } };
            }
            // content_reflowed: every coordinate is stale — restart from row 0.
            action Reflow when (state == 1 || state == 2 || state == 3) {
                state = 1; scanp = 1; stored = 0; total = 0; cur = 0;
            }
            // cancel: back to Idle, everything cleared.
            action Cancel when (state > 0) {
                state = 0; scanp = 0; stored = 0; total = 0; cur = 0;
            }

            invariant CurrentIndexValid: cur <= stored;                // INV-SEARCH-1
            invariant MemoryBounded: stored <= MaxResults;             // INV-SEARCH-3
            invariant TotalMatchesConsistent: stored <= total;         // INV-SEARCH-6
            invariant ScanProgressConsistent:                          // INV-SEARCH-5
                (state == 1 && scanp > 0) || (scanp == 0 && (state == 0 || state > 1));
            invariant TerminalShape:
                (state == 2 && stored > 0 && cur > 0) ||
                (state == 3 && stored == 0 && cur == 0) ||
                state == 0 || state == 1;
        }
    }
}

/// BUDGETED SEARCH RESUME — the public lifecycle contract of
/// `aterm_core::terminal::Terminal::search_budgeted`.
///
/// This is deliberately a separate machine from [`streaming_search_model`].
/// `StreamingSearch` models match storage/navigation inside the older streaming
/// engine; `BudgetedSearchResume` models the owner-level capability that binds a
/// partial full-buffer scan and its result-delta stream to one query/content
/// snapshot and one non-repeating `search_id`.
///
/// The projection is intentionally small but includes every public field that
/// controls host accumulation:
///
/// * `progress` / `total` are `rows_fed` / `total_rows`;
/// * `scan_done` distinguishes indexing completion from API completion;
/// * `live` / `cursor` encode the optional resume capability;
/// * `search_id` and `reset` are projected directly from the returned step;
/// * `delivery` counts a bounded dense-result trace: the scan turn delivers the
///   first delta, one drain turn leaves a backlog, and the final drain completes;
/// * `issued` and `prior_id` are trace ghosts. Real process-global `u64` IDs are
///   normalized to first-seen rank, proving freshness without assuming that IDs
///   are contiguous within one `Terminal`.
///
/// `Rows = 3` is the real three-row Tier-1 terminal. `DeliveryTurns = 3` is a
/// bounded representative of an arbitrary match backlog, not the shipping
/// 4,096-record payload cap. `StartBacklog`/`Drain`/`DrainComplete` prove the
/// important seam: scanning may be finished while the cursor remains live,
/// drain-only resumes preserve row progress and identity, and only the last
/// delta retires the cursor. `StartComplete` and `RestartComplete` cover calls
/// that scan and deliver everything in one turn; the latter still returns
/// `reset = true` and a fresh `search_id` despite returning no cursor.
/// The restart alphabet groups equivalent guards: `ContentRestart` covers either
/// content-snapshot key, `QueryRestart` covers query/options, and
/// `ForgedRestart` covers forged, foreign, dropped, or otherwise stale tokens.
/// This successful-step machine does not model construction/exhaustion errors,
/// which return no step and are covered by the owner's fail-closed unit tests.
///
/// `Buggy = 1` reproduces stale continuation: restart actions preserve the old
/// identity/progress instead of minting/resetting. `ResetMintsFresh` and
/// `ResetStartsAtBeginning` catch it. Tier-1 binding and transition negative
/// controls live in `aterm-core/tests/conformance_budgeted_search.rs`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn budgeted_search_resume_model() -> Model {
    crate::ty_model! {
        BudgetedSearchResume {
            const Rows = 3;          // real bounded Terminal row window in Tier-1
            const DeliveryTurns = 3; // scan delta + repeated drain + final drain
            const MaxTokens = 10;    // bounded fresh-start/restart trace budget
            const Buggy = 0;         // 1 = preserve stale identity/progress on restart

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

            // Unit-budget fresh start: mint a logical stream, expose reset, and
            // consume the first row. This action is also used after cancellation or
            // completion, where presenting the retired token cannot resume state.
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
            }

            // A sparse fresh search may scan and deliver all rows in one call.
            action StartComplete when (live == 0 && issued <= MaxTokens - 1) {
                live = 0;
                progress = Rows;
                total = Rows;
                scan_done = 1;
                complete = 1;
                cursor = 0;
                search_id = issued + 1;
                issued = issued + 1;
                reset = 1;
                prior_id = search_id;
                delivery = 0;
            }

            // A dense fresh search scans all rows and emits its first bounded
            // result delta, but remains live because more deltas are queued.
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
            }

            // A valid unit-budget resume preserves stream identity and advances one
            // non-final scan row. The final sparse scan turn is separate so its
            // cursor retirement is explicit.
            action Resume when (live == 1 && scan_done == 0 && progress <= Rows - 2) {
                progress = progress + 1;
                reset = 0;
                prior_id = 0;
            }
            action FinishScan when (live == 1 && scan_done == 0 && progress == Rows - 1) {
                live = 0;
                progress = progress + 1;
                scan_done = 1;
                complete = 1;
                cursor = 0;
                reset = 0;
                prior_id = 0;
            }

            // Explicit None while a scan is live supersedes it. The three stale
            // input classes below have the identical observable restart effect,
            // but remain separate actions so Tier-1 exercises every guard class.
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
            }

            // A stale/superseding call may scan and deliver its fresh snapshot in
            // one turn. There is no cursor, but reset + fresh search_id remain
            // observable so hosts discard the superseded stream before appending.
            action RestartComplete when (live == 1 && issued <= MaxTokens - 1) {
                live = 0;
                progress = Rows;
                total = Rows;
                scan_done = 1;
                complete = 1;
                cursor = 0;
                search_id = if Buggy == 1 { search_id } else { issued + 1 };
                issued = if Buggy == 1 { issued } else { issued + 1 };
                reset = 1;
                prior_id = search_id;
                delivery = 0;
            }

            // Once scanning has finished, a valid resume can make pure delivery
            // progress without changing rows_fed. The same cursor/search_id stays
            // live until the last pending delta has been emitted.
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
                live = 0;
                complete = 1;
                cursor = 0;
                reset = 0;
                prior_id = 0;
                delivery = delivery + 1;
            }

            // Cancellation retires the partial scan. A subsequent call is Start
            // even when the caller presents the now-stale retired token.
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
            invariant CursorMatchesSearchId:
                if live == 1 {
                    cursor == search_id && search_id == issued
                } else {
                    cursor == 0
                };
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
            invariant IdentityIsLatest:
                search_id == 0 || search_id == issued;
            invariant ValuesBounded:
                live <= 1 && scan_done <= 1 && complete <= 1 && reset <= 1 &&
                progress <= Rows && total <= Rows && delivery <= DeliveryTurns &&
                issued <= MaxTokens && cursor <= MaxTokens &&
                search_id <= MaxTokens && prior_id <= MaxTokens;
        }
    }
}

/// HOST-MINTED HYPERLINK SCHEME CAPABILITY — the abstract twin of
/// `HyperlinkAuth`'s extra-scheme allowlist (orca deep-links §7, #4384): the
/// host may mint a small set of EXTRA OSC-8 URI schemes (e.g. `orca`) on top
/// of the hardcoded safe allowlist. The stateful discipline is exactly:
///
///   * the extra set is BOUNDED (`MAX_EXTRA_SCHEMES` in the code, `Cap` here) —
///     `Authorize`/`AuthorizeOther` grow it only under the bound and
///     `RefuseAtCap` is the at-capacity refusal (observable: the API returns
///     `false`, state unchanged);
///   * a NEVER-ALLOW scheme (`javascript`/`data`/`file`/…) is refused even
///     when the host asks — `RefuseNeverAllow` is a no-op at `Buggy=0`, and
///     `Buggy=1` models the admission defect the invariant must catch;
///   * `Revoke` removes the distinguished scheme, restoring the default
///     allowlist: `Accept` (the OSC-8 gate saying yes to an extra-scheme URI)
///     is enabled IFF the scheme is currently minted, which Tier-1 binds to
///     the real `is_allowed_scheme` decision.
///
/// `orca` is the distinguished tracked scheme (0/1); `others` counts the rest
/// of the extra set so the bound is proven over the WHOLE set, not one entry.
/// PROVES `Bounded` + `NeverAllowRefused` at `Buggy=0`; at `Buggy=1` the
/// unbounded grow and the never-allow admission each yield a counterexample.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
pub fn hyperlink_scheme_cap_model() -> Model {
    crate::ty_model! {
        HyperlinkSchemeCap {
            const Buggy = 0;
            const Cap = 4; // == the real MAX_EXTRA_SCHEMES, so Tier-1 hits the true refusal point

            var orca = 0;   // distinguished host scheme currently minted (0/1)
            var others = 0; // count of OTHER live extra schemes
            var never = 0;  // 1 iff a never-allow scheme was ever admitted

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

/// Module-wide scrollback budget sharing (audit E1, Codex-required global cap):
/// N panes in one memory space each apply `min(configured, global / live)` at
/// their own touch points, so the applied budgets sum within the ONE global cap
/// at every quiescent point — panes cannot multiply the per-pane budget into an
/// OOM. Two panes bound the membership lattice (join/leave/apply in all
/// orders); `fresh*` tracks "applied since the last membership change", so the
/// invariant is exact at quiescence and honestly waived while a share is stale
/// (bounded staleness: one touch). `Buggy=1` is the global-less bug — each pane
/// applies its full configured budget — which two fresh panes must expose.
///
/// PROVES `QuiescentSumBounded` + `DepartedHoldsNothing` at `Buggy=0`; at
/// `Buggy=1` two fresh live panes overrun the global (counterexample required).
/// Tier-1: `aterm-core/tests/conformance_shared_budget.rs` drives the real
/// `ScrollbackBudgetShare` registry (and real `Terminal` eviction) in lockstep.
// Skip (T2 vcgen-budget lane): a spec-model DATA constructor (see the sibling
// models above) — the MODEL it returns is what `ty` machine-checks.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn shared_budget_model() -> Model {
    crate::ty_model! {
        SharedScrollbackBudget {
            const Global = 6;  // module-wide budget (scaled bytes)
            const HalfShare = 3;    // Global/2 — the two-pane equal share (no division in exprs)
            const Cfg = 6;     // per-pane configured budget (> HalfShare so the share binds)
            const Buggy = 0;   // 1 = apply configured budget, ignore the global divide

            var live2 = 0;  // pane 2 registered (pane 1 is always live)
            var a1 = 6;     // pane 1 APPLIED budget (= Cfg: a fresh store carries
                            // its configured budget until first touch)
            var a2 = 0;     // pane 2 APPLIED budget
            var fresh1 = 0; // pane 1 applied since the last membership change
            var fresh2 = 0;
            var steps = 0;  // run bound

            // Pane 2 registers (its store constructed at Cfg): both shares go
            // stale until each pane's next touch.
            action Join when (live2 == 0 && steps <= 5) {
                live2 = 1;
                a2 = Cfg;
                fresh1 = 0;
                fresh2 = 0;
                steps = steps + 1;
            }
            // Pane 2 drops: its Scrollback (and applied share) is freed with it.
            action Leave when (live2 == 1 && steps <= 5) {
                live2 = 0;
                a2 = 0;
                fresh1 = 0;
                fresh2 = 0;
                steps = steps + 1;
            }
            // Pane 1 touched: pending_effective() -> set_memory_budget.
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

            // THE global contract: once every live pane has applied its current
            // share, applied budgets sum within the ONE global cap.
            invariant QuiescentSumBounded:
                if fresh1 == 0 {
                    0 <= 1
                } else if live2 == 1 && fresh2 == 0 {
                    0 <= 1
                } else {
                    a1 + a2 <= Global
                };
            // A departed pane holds no share.
            invariant DepartedHoldsNothing:
                if live2 == 0 { a2 == 0 } else { 0 <= 1 };
        }
    }
}
