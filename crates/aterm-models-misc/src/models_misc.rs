// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Models for audio trails, input pairing, maintenance lanes, search, capture,
//! prewarm and budgets. Each constructor is re-exported from this crate's
//! root.

use aterm_spec::derive::*;

/// Trail-audio UI→worker handoff and AudioQueue idle lifecycle.
///
/// The event loop may only do a bounded nonblocking enqueue. The shipping FIFO has 64 slots;
/// `CommandCap=2` is its finite-state projection: an available slot accepts the newest cue,
/// and a full FIFO keeps every queued cue and drops (and counts) the newest. Every device
/// open/start/push belongs to the worker. A worker that applies a cue resets the exact-silence
/// counter before pause housekeeping can observe it. A running queue owns exactly one low-rate
/// worker timeout, which a successful idle pause (or explicit start failure) disarms. The
/// device may fail, but failure is explicit and cannot turn every later keystroke into another
/// synchronous platform call.
///
/// `Buggy=1` combines the defects: the UI touches the platform while enqueuing, blocks and
/// omits drop accounting on a full queue, and a cue applied to a running device keeps the
/// pre-cue silence count. One slip per remaining law:
/// * the idle pause keeps its housekeeping timeout armed (the loop choosing `recv_timeout` on
///   `output.is_some()` instead of `is_running()`, so a paused worker polls for ever;
///   `RunningOwnsOneDeadline`);
/// * exhaustion latches `STATE_FAILED` but the loop does not `return`, so its receiver lives,
///   ingress stays open and the next cue reopens the device
///   (`StartFailureIsExplicitAndTerminal`; both guards read `failed <= Buggy`, which over the
///   0..1 flag is `failed == 0` at the committed config);
/// * an unbounded `mpsc::channel()` in place of `sync_channel(COMMAND_CAPACITY)`, which admits
///   a cue past capacity instead of dropping it (`WorkerMailboxIsBounded`; `PushCueAvailable`'s
///   guard admits one slot more). The Tier-1
///   `shipping_ingress_conforms_to_bounded_drop_newest_model` fills the real 64 slots and sees
///   the 65th dropped.
///
/// One device fault is not terminal: a failed open, start or enqueue, a faulted or stalled
/// callback, or a queue AudioToolbox stopped (the `kAudioQueueProperty_IsRunning` listener)
/// each spend one of `REOPEN_BUDGET` reopens, and only exhaustion latches `STATE_FAILED` and
/// seals ingress. Here that ladder is abstracted as its end (`WorkerStartFails`, the exhausting
/// fault); [`trail_audio_reopen_ladder_model`] is the ladder itself.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn trail_audio_lifecycle_model() -> Model {
    aterm_spec::ty_model! {
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
            // No "idle pause disarms" law: `PauseIdle`, the one writer of `paused = 1`, writes
            // `running = 0`, and every writer of `running = 1` clears `paused`, so it is
            // `RunningOwnsOneDeadline` at `running = 0`.
            invariant StartFailureIsExplicitAndTerminal:
                if failed == 1 {
                    running == 0 && queued == 0 && service_deadline == 0
                } else {
                    failed == 0
                };
        }
    }
}

/// AudioQueue buffer ownership and first-audible-buffer latency across cold start and idle
/// resume.
///
/// The three queue buffers are allocated but available while stopped. A cue is applied before
/// the worker renders and enqueues all three, so the first audible samples occupy buffer 1
/// (`MaxAudibleBuffer`), not buffer 4 behind three silent priming buffers. Idle uses a
/// synchronous immediate stop/reset: callback recycling is disabled first, and every scheduled
/// buffer is reclaimed before a resume prime can write it. That the parked worker holds no
/// timeout is [`trail_audio_lifecycle_model`]'s `RunningOwnsOneDeadline`; the fake queue here
/// has no worker to wake.
///
/// `Buggy=1` has both dangerous alternatives: pre-enqueued, pause-retained silence puts sound
/// in buffer 4, and overwriting those still-scheduled pointers on resume is an ownership
/// violation. It also removes the enqueue/stop gate, admitting a stop while the callback is
/// inside its enqueue critical section: the shipping `stop_and_reclaim` refuses that stop
/// (`false`, every buffer still queued), while the mutant ignores the refusal and marks all
/// three buffers available while the queue owns them (`BufferOwnershipConserved`). Tier-1:
/// `audio_queue_post_cue_prime_conforms_with_callback_free_fake` drives the shipping
/// prime/stop helpers with an ownership-checking fake queue.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn trail_audio_start_latency_model() -> Model {
    aterm_spec::ty_model! {
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
            // Over 0..1, `enqueue_in_flight <= Buggy` is `Buggy == 1 || enqueue_in_flight == 0`.
            // Buggy: the pause keeps its buffers queued (and says so), while a refused
            // mid-enqueue stop's books free them anyway.
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
            // No "idle is callback-free" law: `StopIdle` is phase 4's only
            // entry and writes `running`, `recycling` and `enqueue_in_flight`
            // to zero at every `Buggy`, and `ParkIdle` writes none of them.
            bound PhaseBounded: phase <= 8;
        }
    }
}

/// The typing-sound seam: which ticks leave a keystroke heard, and whether the `tone` verb's
/// answer agrees with the key path.
///
/// One writer and two readers share one bit. The writer is `CursorGlow::tick`, which leaves
/// `sound_live` open or closed on every frame; the readers are the key path
/// (`keystroke_click_audible` then `CursorGlow::cue_keystroke_shifted`, which records nothing
/// unless the seam is open) and the `aterm ctl tone` verb (`sound_seam`, which names the gate
/// that closed it). Each tick action is one class of host config, as `App::cursor_frame` folds
/// it (`sound_seam::fold_window_audibility`):
///
/// * `TickLit`: master on, the window focused, amplitude nonzero.
/// * `TickDarkMotion`: dark only because of a motion policy (macOS Reduce Motion,
///   `motion = "reduced"`, or the load-shed envelope).
/// * `TickDarkUnfocused`: dark because the key belongs to another window.
/// * `TickDarkUserOff`: dark because the person set the aurora's own brightness knob to zero.
/// * `TickMasterOff`: the trail is off (`cursor_trail_style = "off"`) or serious mode.
///
/// A tick class is the host's config class whatever the engine then does: every style (the
/// classic wake included) and every grid (a degenerate one too) must answer each class the
/// same way, and the Tier-1 conformance drives them all.
///
/// `key` and `tone` are what the two readers answered since the last tick (`0` not asked, `1`
/// heard / `open`, `2` refused / `closed:<named gate>`, `3` `closed:engine-silent`); every tick
/// clears them, so each invariant relates a reader's answer to the tick it read.
///
/// Invariants: a key pressed while the only darkness is a motion or performance policy is
/// heard (`FocusedKeyIsHeard`); a key in an unfocused window, with the aurora dimmed to zero,
/// or with the master off is not (`ForeignKeyIsSilent`); the verb and the key path never
/// disagree (`ToneAgreesWithTheKey`); and `closed:engine-silent`, the sensor for a future
/// policy that re-subordinates audio, is unreachable while every closed gate has a name
/// (`EngineSilentIsUnreachable`). That last holds for a window that has run a frame: before
/// its first frame the real verb does say `engine-silent` (no config gate explains a seam no
/// tick has resolved), which the conformance pins as a separate reading.
///
/// There are no action guards: every tick class and both readers are enabled in every state,
/// so no invariant can hold merely because a guard made its counterexample unreachable. Every
/// decision lives in an update, which is what the Tier-1 conformance binds.
///
/// `Buggy=1` combines three defects, each on its own action: `TickDarkMotion` closing the seam
/// (an unconditional zero-amplitude return), `TickDarkUnfocused` opening it
/// (`sound_live = true` on every dark return), and the verb reading `trail_on` as
/// `GlowConfig::enabled` alone, which reports a user-dimmed aurora as `closed:engine-silent`.
///
/// Tier-1: `aterm-gui/src/sound_seam_conformance.rs` drives the real resolver,
/// `MotionPolicy::resolve`, host fold, `CursorGlow::tick` on every style and grid, key seam and
/// the verb's own input constructor into the real `sound_seam` table over the whole host input
/// domain, and rejects replays of each defect (and of a classic wake that writes no seam and an
/// empty grid that closes it like the master switch). `app_input`'s `tone_status_tests` bind
/// the verb's call site. Not modelled: the host's own sound knobs (`trail_sounds`, volume,
/// resize-quiet, the audio host), which `Key` pins open and the `sound_seam` table tests cover.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn trail_sound_seam_model() -> Model {
    aterm_spec::ty_model! {
        TrailSoundSeam {
            const Buggy = 0;
            // The last tick's config class, as the engine and the verb saw it.
            var master = 0;
            var amplitude = 0;
            // 0 none (lit, or master off), 1 motion or shed, 2 unfocused,
            // 3 the user's brightness knob at zero.
            var dark_cause = 0;
            // `CursorGlow::sound_seam_open()`.
            var seam = 0;
            // What each reader answered since the last tick.
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
            // The key path, with the host's own sound knobs open: it records
            // a click exactly when the engine's seam is open.
            action Key {
                key = if seam == 1 { 1 } else { 2 };
            }
            // The verb, in `sound_seam`'s precedence: what the user set (`trail_on`), then
            // where the user is (`focused`), then the engine's bare refusal.
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

/// The audio host's bounded device-reopen ladder: a fault spends one reopen, a device that
/// plays through a healthy window hands the budget back, and only exhaustion is terminal.
///
/// The sibling of [`trail_audio_lifecycle_model`] (ingress, the idle pause and the terminal
/// failed state): this machine models what happens between a fault and that terminal state,
/// which the lifecycle abstracts as `WorkerStartFails`. It projects `RetryState`,
/// `reopen_after_fault` and the `worker_loop` call sites that drive them
/// (`aterm-gui/src/trail_audio.rs`):
///
/// * `attempts` is `RetryState::attempts` (the wire's `reopens_left` is `Budget - attempts`);
///   `Budget` is `REOPEN_BACKOFF.len()`.
/// * `spent` is the spec's own count of reopens scheduled since a device last delivered after
///   playing through a healthy window, kept apart from `attempts` so the code's counter can be
///   checked against it. Its reset reads `evidence`, never `played`: sharing the code's reset
///   condition would make `attempts == spent` true of any reset law.
/// * `evidence` is the spec's own record of what the device did since the last scheduled
///   reopen, kept apart from the code's `played` clock: `0` nothing delivered, `1` delivered,
///   `2` delivered and then the window elapsed (also the value before any fault). The two agree
///   on the committed machine; a change to the code's reset law, or to what starts or finishes
///   its clock, makes them disagree, and `ResetOnlyOnPlayedWindow` says so.
/// * `waiting` is inside the backoff window (`!RetryState::ready`). The schedule's first step
///   is zero and the rest strictly increase, so the first reopen is immediate and the last
///   wait is the longest step, which is also the healthy window.
/// * `played` is the healthy window's clock: wall time from the first delivery after the last
///   fault (`RetryState::played_since`) with no fault since; `0` no delivery since the last
///   fault, `1` running, `2` run out. A delivery is a successful enqueue and start
///   (`Delivery::Sounded`). A device that stops its queue without flagging a fault is not seen
///   as one here, so the window can run over idle time; the IsRunning-listener lane is what
///   would make it true play time.
/// * `stale` is the clock a fault-anchored reset would read: time since the last fault has
///   reached the window's length.
///
/// Actions: `Cue` (a cue reaches a worker holding no device: it opens one, or is dropped and
/// counted inside the window or against sealed ingress), `CueSounds` (a delivery,
/// `RetryState::delivered`), `DeviceFaults` (a failed open, start or enqueue, a faulted or
/// stalled callback: the one handler, which schedules a reopen or reports exhaustion), and the
/// three clock actions. Every decision the invariants depend on lives in an update: a
/// guard-conjunct audit refused two guards the first draft of this machine had, each carrying
/// `SilenceIsPermanentOnlyWhenTheBudgetIsSpent` by premise. The guards that remain are
/// environment facts (a clock can only run out once started, a worker that exited touches no
/// platform), and the Tier-1 conformance binds each to the real predicate
/// (`Model::action_enabled` against the real ladder in every visited configuration).
///
/// `Buggy=1` replays three defects, scoped so none masks another: an open failure terminal on
/// the spot ("one fault is for ever"); a successful open inside the window resetting the
/// ladder (defeated by a device that constructs and never plays); and a delivery once the
/// fault is stale resetting it (with the window measured from the fault, and the last wait
/// being the window, the first delivery after the final reopen hands the whole budget back, so
/// a device that plays one block and stalls cycles the ladder for ever).
///
/// Tier-1: `aterm-gui/src/trail_audio_reopen_conformance.rs` drives the real
/// `RetryState`/`reopen_after_fault` exhaustively on a fabricated clock with the shipping
/// schedule (two-way: every model state is reached), and validates traces of the real
/// `worker_loop` against this machine, with each defect replayed as a negative control.
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
            // No fault has happened yet, so nothing is recent.
            var stale = 1;
            var failed = 0;
            // Nothing is owed before the first fault.
            var evidence = 2;

            // A cue after exhaustion meets sealed ingress and opens nothing. The law is in the
            // update: as a `failed == 0` guard it would carry
            // `SilenceIsPermanentOnlyWhenTheBudgetIsSpent` by premise, which no conformance can
            // refute.
            action Cue when (device == 0) {
                device = if failed == 1 || waiting == 1 { 0 } else { 1 };
                attempts = if Buggy == 1 && waiting == 0 && failed == 0 && stale == 0 {
                    0
                } else {
                    attempts
                };
            }
            // The reset law, in the update: the ladder is handed back only on evidence the
            // device played through the window. As a `device == 1` guard it would carry the
            // terminal law by premise, so the action is unguarded and a delivery with no device
            // changes nothing.
            action CueSounds {
                attempts = if device == 1 && played == 2 {
                    0
                } else {
                    if Buggy == 1 && device == 1 && stale == 1 { 0 } else { attempts }
                };
                // The spec's reset law: on evidence, not on the code's clock.
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
                // A scheduled reopen restarts both clocks; exhaustion returns before touching
                // either (`reopen_after_fault`).
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
                // A scheduled reopen owes new evidence.
                evidence = if Buggy == 1 && device == 0 {
                    evidence
                } else {
                    if attempts <= Budget - 1 { 0 } else { evidence }
                };
                device = 0;
            }
            action BackoffElapses when (waiting == 1) {
                waiting = 0;
                // The last wait is the schedule's longest step: the window.
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
            // The ladder is handed back only on evidence the device played through a healthy
            // window since the last reopen.
            invariant ResetOnlyOnPlayedWindow:
                if attempts == 0 { evidence == 2 } else { evidence <= 2 };
        }
    }
}

/// Tab-stop checkpoint preservation across a narrow seamless handoff.
///
/// Grid width and tab-stop backing length differ on purpose: shrinking a terminal keeps
/// bounded stops beyond the visible width so a later grow has the same tab semantics. A
/// carried projection is admitted exactly when it covers the current columns and stays within
/// the protocol maximum. The mutant truncates backing storage at shrink/restore, losing the
/// off-width custom stop, and admits a projection of any length, copying whatever prefix
/// overlaps the destination's stops (an undersize vector applied over the defaults, an
/// oversize one truncated), where `restore_tab_stops` rejects both
/// (`AdmissionIsCoveringAndBounded`, `InvalidProjectionIsNeverAdmitted`).
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
            // Buggy: the restore admits the invalid vector and copies its overlapping prefix
            // into the destination's own stops.
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

/// Event-loop lane-isolation contract for bulk scrollback maintenance.
///
/// Ordinary PTY output is latency-critical and must never begin a terminal scan, take a
/// blocking lock, or evict. The explicitly delivered OS memory-pressure event is the sole
/// bulk-work capability. The allocator/evictor is abstracted behind `unbounded_work` and
/// `mutation`; this is about event routing, not a bound on emergency reclamation itself. The
/// mutant admits output directly into the bulk lane (an amortized
/// `enforce_global_scrollback_cap` on the output-wake arm, which scans every session, takes
/// their locks and evicts mid-scan), so all four capabilities fire on ordinary output and the
/// eviction lands with no completed pressure trim behind it
/// (`MutationRequiresCompletedPressureTrim`).
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
            // Its first conjunct is the whole of "mutation requires memory
            // pressure", so that weaker law is not stated separately.
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
/// Terminal UIs such as Codex keep status/input rows below a DECSTBM region and scroll
/// committed transcript lines through row zero. Such a scroll archives exactly the displaced
/// row when primary-screen history is enabled, while an interior region, a horizontally
/// margined rectangle, or a history-free alternate-screen grid stays ephemeral. Rows below the
/// vertical margin are preserved in every regime. The mutant drops the eligible displaced row
/// because the region is not full-height, and its scroll runs to the screen bottom instead of
/// the DECSTBM bottom margin, displacing the fixed footer (`FixedFooterIsPreserved`; a
/// hypothetical slip). `footer` is a flag only the slip writes, so at Tier-0 the law is the
/// flag; the Tier-1 bind gives it content by reading `footer` off the real grid's bottom row
/// and replaying the slip as a real interior scroll whose bottom margin is the screen bottom.
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
            // Whether the selection lies outside the rows this scroll damages: with the
            // regime, it decides the selection's fate.
            var selection_disjoint = 0;

            // Disjoint/overlapping flavours of each regime rather than a new phase, so the
            // `phase == 2` tests stay intact.
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
                // An interior or margined scroll damages only its rows, so a disjoint
                // selection survives (a status-bar repaint must not destroy a highlight in
                // scrollback). An overlapping one clears in every regime: over-clearing is
                // safe, a stale highlight over replaced content is not.
                selection_alive = if top == 0 && full_width == 1 && history_enabled == 1 {
                    if Buggy == 1 { 0 } else { selection_alive }
                } else if selection_disjoint == 1 {
                    if Buggy == 1 { 0 } else { selection_alive }
                } else {
                    0
                };
                // The remap moves only a selection above the splice's boundary, one the
                // scrolled region covers; `adjust_for_row_splice` leaves one below it alone.
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
            // A selection survives iff it was piecewise-remapped (the archival regime) or was
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
            bound StateIsBounded:
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

/// Config/font generation publication: a completion may publish only when its
/// ticket is still the newest request. The mutant accepts generation one after
/// generation two was requested, reproducing a stale face/config rollback.
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

/// Path-feed snapshot publication binds a prepared consumer and its content
/// fingerprint to one bounded, same-handle admitted read. `live` is the
/// pathname's currently visible byte generation; `admitted` is the immutable
/// source returned by that read; and `prepared`/`fingerprint` are the two
/// projections derived from those admitted bytes. Later pathname replacement
/// or ABA restoration may move `live`, but cannot alter either projection.
///
/// `Buggy=1` is the TOCTOU shape: `Publish` re-reads `live` for the fingerprint instead of
/// keeping the value `Read` produced, so a mutation after admission publishes a consumer from
/// one generation with another generation's fingerprint.
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

/// A theme-catalog generation or an OS appearance flip can overtake an
/// expensive font/config prepare, and a theme's typography makes both of them
/// face changes (DTP §9.1). The current config observation must be
/// re-prepared against the newer theme AND side rather than publishing a face
/// resolved from the old ones, and the colours of the new theme or side must
/// not reach the screen before the face that belongs to them: the colour swap
/// is HELD while a generation is owed, and the publish installs both.
///
/// Sources: config tickets (`requested`, `completed`, `published`), the theme
/// generation (`theme`, `completed_theme`, `published_theme`), the appearance
/// (`side`, `completed_side`, `published_side`), the colours on screen
/// (`colour_theme`, `colour_side`), the held swap (`held`), and `degraded`:
/// the one documented exception, a flip the font worker could not take,
/// whose colours install at once over the old face (a flip is never lost; the
/// next config generation brings the face). Before any publish, `published_*`
/// name the launch face (theme 0, side 0).
///
/// Laws, each with its own mutant:
/// - `PublishedThemeIsCurrentOrEmpty` (Buggy 1: the re-preparation publishes
///   the stale completion);
/// - `FaceMatchesColours` (Buggy 2: the colours swap at once on a theme
///   change or flip while the face is still being prepared);
/// - `HeldSwapIsOwed` (Buggy 3: a refused worker leaves the swap held with no
///   generation coming, a lost flip).
///
/// Tier-1: aterm-gui's
/// `overtaking_theme_catalog_conforms_to_font_theme_generation_model`
/// (`native_font_catalog.rs`) drives this model's whole reachable space on a
/// real headless App and the worker's real `prepare` — a typography catalog
/// reload and an OS flip during a prepare, and a refused worker — reading back
/// which completion the config service holds, which colours are installed and
/// whether a swap is held; it rejects the pre-theme-generation guard and a
/// colours-first finish, and the shipped App is refused by every mutant.
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

/// Staged-update activation through both native and portable menu faces.
///
/// Once a strictly-newer update is staged and the Version surface is refreshed,
/// its `ApplyUpdate` row is present, enabled, and decodes to the apply command.
/// That decision is independent of whether a terminal or a native Settings/About
/// tab is frontmost: only genuinely terminal-only actions use that content gate.
/// This is a menu-model/dispatch contract; the AppKit click relay is bound at its
/// pure tag/validation seams rather than claiming an automated OS click harness.
/// The mutant applies the terminal-tab gate to `ApplyUpdate`, leaving the visible update row
/// grey and unselectable.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_menu_activation_model() -> Model {
    aterm_spec::ty_model! {
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
/// Winit reports modifier snapshots and focus as independent window events. Focus loss must
/// unconditionally invalidate the previous ambient snapshot, because a backend may omit the
/// matching key-up. Every later snapshot stays authoritative whatever the focus bit: Windows
/// can report the genuinely held modifiers before `Focused(true)` and need not repeat them.
///
/// `Buggy=1` reproduces only the unsafe reset transition: `FocusOut` retains Ctrl.
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

/// Selection custody: which key events may take the user's reading position.
///
/// The viewport has two owners: the tail-follower while `display_offset == 0`, the user from
/// the moment a scroll gesture lifts the view off the bottom. Ownership transfers back on
/// exactly one event, a byte-producing key press ("start typing and jump back to the prompt").
/// The selection has one owner, the user, and is dropped only by that press or a deliberate
/// deselect.
///
/// Three event classes must disturb neither:
///
/// * a key release (a Kitty `REPORT_EVENT_TYPES` key-up is not typing),
/// * an auto-repeat tick (the same held press continuing; snapping at the ~30 Hz repeat rate
///   destroys any scroll or selection made mid-hold),
/// * a bare modifier or lock key (Shift/Control/Alt/Super/Caps…), the first half of ⌘-C and
///   the Shift of a shift-click extend.
///
/// Nor must output: a program writing while the user reads repins the offset so the same
/// content stays under the eye, never sliding the view or handing it back to the
/// tail-follower.
///
/// Output and the selection differ: output that replaced the selected rows may destroy the
/// highlight (a highlight over new text makes a copy return something never selected), and
/// ED 3, `clear_scrollback` and RIS take the viewport back outright because the space the
/// offset named is gone. Both are their own event kinds below, not exceptions under one
/// invariant.
///
/// The custody invariants are stated over the observable state (`offset`, `owner`,
/// `selection`) against a shadow of the pre-action values (`prev_*`) that every action writes
/// from the pre-state, never over a self-reported "this press disturbed something" flag: a
/// flag written by hand at every site lets an implementation that moves the viewport without
/// setting it pass. Here, moving the viewport is the violation.
///
/// `Buggy=1` is the regression family: an inert press and an auto-repeat tick each snap the
/// viewport and clear the selection (so the ⌘ keydown destroys the selection before the `c`
/// arrives and ⌘-C copies nothing); a release that disturbs; output that snaps the reader back
/// to live instead of repinning; output that takes custody while clearing the rows it damaged;
/// and a typing press that deselects without snapping. Every invariant except the
/// state-consistency guard `TailOwnerAtBottom` is falsified by one of them, and
/// `derived_ring_ty.rs` checks that invariant by invariant.
///
/// Output that takes the highlight without moving the view (an in-place rewrite over the
/// selected rows, or a re-pin saturated at the history floor), and output that takes it for a
/// reason the damage lattice cannot attribute (`post_process`'s fail-closed arms), are their
/// own actions under the same tag and law as a damaging take. A user gesture back toward live
/// that is not typing (End, a downward scroll, the ⌘-V and IME snaps) is `SnapToLive` /
/// `UserScrollTowardLive`: it moves the view down and keeps the highlight.
///
/// Scope: the press-path half of custody plus the output repin it must not fight. Eviction
/// and wholesale invalidation are [`selection_custody_model`]'s; the alt-screen round-trip is
/// [`alt_selection_park_model`]'s.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn press_custody_model() -> Model {
    aterm_spec::ty_model! {
        PressCustody {
            const Buggy = 0;
            const MaxOffset = 2;

            // 0 = the tail-follower owns the viewport, 1 = the user does.
            var owner = 0;
            // Bounded projection of `Grid::display_offset`.
            var offset = 0;
            // A completed text selection exists.
            var selection = 0;
            // Shadow of the pre-action observable state, written by every action from the
            // pre-state.
            var prev_owner = 0;
            var prev_offset = 0;
            var prev_selection = 0;
            // What kind of event just fired: 0 a user gesture, 1 a typing press, 2 an
            // auto-repeat tick, 3 a bare modifier, 4 a release, 5 output that missed the
            // selected rows, 6 output that took the highlight (replaced the selected rows,
            // or `post_process` failed closed on it), 7 output that invalidated the
            // coordinate space (ED 3, clear_scrollback, RIS), 8 a user gesture back toward
            // live that is not typing (a downward scroll, End / ScrollToBottom, the paste
            // and IME snaps).
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
            // Output while the user is reading: the repin keeps the same content under the
            // eye, so the offset rises with the new lines and ownership stays with the user.
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
            // Output that replaced the selected rows (an overlapping `SelectionDamage`): the
            // engine destroys the selection here (a highlight over replaced text makes ⌘-C
            // return something never selected), but may not take the reading position. The
            // commonest case is at live (a status line repainting the highlighted rows), so
            // the guard does not read `owner`; the repin rides the new lines only for a
            // reader. `Buggy = 1` snaps the reader to live (`OutputNeverTakesCustody`).
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
            // The same take in place: a damaging batch that advanced no rows (`\r` plus EL
            // over the selected row, a DECERA, a status line repainting itself) or whose
            // re-pin saturated at the history floor. The view does not move; only the
            // highlight goes. `Buggy = 1` snaps the reader to live (`OutputNeverTakesCustody`).
            action OutputDamagesTheSelectedRowsInPlace when (selection == 1) {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = if Buggy == 1 { 0 } else { offset };
                owner = if Buggy == 1 { 0 } else { owner };
                selection = 0;
                last_event = 6;
            }
            // Output took the highlight for a reason other than damage overlap
            // (`post_process`'s fail-closed arms: a malformed or saturated splice projection,
            // a splice mixed with another scroll, the `left_alt` re-check, a whole-interval
            // eviction at the history floor). Same tag and law as a damaging take: the
            // highlight may go, the reading position may not. The re-pin rides the lines…
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
            // …or, when no row arrived or the re-pin saturated, stays where it was.
            action OutputTookTheSelectionUnattributedInPlace when (selection == 1) {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = if Buggy == 1 { 0 } else { offset };
                owner = if Buggy == 1 { 0 } else { owner };
                selection = 0;
                last_event = 6;
            }
            // ED 3, `clear_scrollback` and RIS: `repin_display_offset` clamps to 0 because
            // the coordinate space the offset named is gone, so output hands the viewport
            // back, spelled as its own event kind. The model does not police the label (as
            // with `selection_custody_model`'s `WholesaleInvalidate`, an implementation that
            // called everything wholesale would satisfy it); the separate kind keeps the
            // ordinary path checked and the exception visible.
            action OutputInvalidatesTheCoordinateSpace {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = 0;
                owner = 0;
                // `Buggy = 1` keeps the highlight across the invalidation: a selection
                // naming rows that no longer exist, the fail-open direction.
                selection = if Buggy == 1 { selection } else { 0 };
                last_event = 7;
            }
            // The user brings the view back to live without typing (End / `ScrollToBottom`,
            // a page or wheel step that lands at the tail, the ⌘-V and IME snaps). Ownership
            // returns to the tail-follower; the highlight is the user's and stays. `Buggy = 1`
            // deselects on the way (`ReturningTowardLiveKeepsTheSelection`).
            action SnapToLive when (offset > 0) {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = 0;
                owner = 0;
                selection = if Buggy == 1 { 0 } else { selection };
                last_event = 8;
            }
            // A downward scroll that stops short of live: the user still owns the
            // view, one row nearer the tail. (Granularity as for `UserScroll`: a
            // real page or notch moves further; Tier-1 drives the one-row form.)
            action UserScrollTowardLive when (offset > 1) {
                prev_owner = owner;
                prev_offset = offset;
                prev_selection = selection;
                offset = offset - 1;
                selection = if Buggy == 1 { 0 } else { selection };
                last_event = 8;
            }
            // The one handover. `Buggy = 1` gives it a member too (typing that deselects
            // without snapping): an invariant no mutant can falsify states nothing.
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
            // Output never takes the reading position, damaging or not. `prev_offset <=
            // offset` rather than equality: the repin raises the offset by the lines that
            // arrived; what it may never do is move the view back toward live.
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
            // …and the selection half, only where it holds: output that did not touch the
            // selected rows leaves the highlight alone. Which damage may clear is
            // `selection_custody_model`'s `OverlapDamageClears`.
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
            // ED 3, `clear_scrollback` and RIS destroy the space the anchors are stated in,
            // so handing the viewport back is correct, but a selection may not outlive it.
            // Every other invariant reaches event 7 only through a trivially true `else` arm;
            // without this one the model would say nothing about the action.
            invariant InvalidationCannotLeaveADanglingSelection:
                if last_event == 7 {
                    offset == 0 && owner == 0 && selection == 0
                } else {
                    last_event <= 8
                };
            // A gesture back toward live moves the view down and nothing else: it is not
            // typing, so the highlight survives it.
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

/// Selection custody: when a selection may be destroyed, as a lifecycle.
///
/// A highlight is the user's, and only an act that destroys the content it names may destroy
/// it; a status bar repainting at the bottom of the screen must not destroy a highlight
/// anchored far up in scrollback.
///
/// The model works in the absolute row space that `intersects_absolute_band` converts into, not
/// in the selection's own space: `TextSelection` anchors are relative `i32` rows, converted at
/// the damage boundary only. A uniform scroll advances `absolute_row_counter` by the same delta
/// `adjust_for_scroll` subtracts, so the interval is stable under ordinary output.
///
/// Damage bands are literal per action, because the `ty_model!` grammar has no
/// nondeterministic range arm. Two bands and three selections give all four overlap/disjoint
/// pairings plus the single-row shape eviction needs: `RegionDamageLow` covers rows 0..1,
/// `RegionDamageHigh` row 3; `SelectLow` takes rows 0..1, `SelectHigh` rows 2..3, and
/// `SelectOldest` row 0 alone.
///
/// Every invariant is stated over the observable change, the post-state against a shadow of
/// the pre-state (`prev_alive`, `prev_sel_lo`, `prev_sel_hi`), never over a value the action
/// just wrote; and every action that may not destroy a selection carries a `Buggy = 1` branch
/// that destroys one, so no invariant follows from a guard alone.
///
/// Scope: the engine's destroyers, reachable from `Terminal::post_process` and its callers. The
/// GUI's own clear sites (`app_search`, `app_mouse`, `subscribe`, `control_host`) and the wasm
/// bridges' are not in this environment.
///
/// `WholesaleInvalidate` is one action, not an alt-enter/alt-exit pair: the three engine sites
/// that record `SelectionDamage::All` outside the screen switch (ED 3 / `clear_scrollback` via
/// `Grid::discard_history_selection`, reflow pullback, and a Kitty unscroll that renumbers
/// history via `force_selection_invalidation`) are one class: the coordinate space the
/// selection named is gone, so no band can describe the damage. Whether a site also moves host
/// coordinates is outside this model: ED 3 evicts history without moving a live coordinate
/// (`discarding_scrollback_evicts_history_and_moves_no_live_coordinate`), a Kitty unscroll
/// renumbers retained rows; both destroy the selection identically.
///
/// `Buggy = 1` is the regression family, one member per destroyer, each falsifying a named
/// invariant (`derived_ring_ty.rs` asserts the mapping member by member):
///
/// * `RegionDamageLow` clears whatever it hit (`DisjointDamagePreserves`).
/// * `RegionDamageHigh` never clears, so a highlight survives over replaced text
///   (`OverlapDamageClears`).
/// * `InertPress` and `UniformScroll` each destroy the selection (their own invariants).
/// * `Evict` reports the loss without acting on it: it raises the floor and records
///   `truncated`, but neither clamps the head nor drops a selection both of whose endpoints
///   are gone (`PartialEvictionTruncates`, `NoDanglingAnchors`,
///   `TruncationImpliesAClampedHead`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn selection_custody_model() -> Model {
    aterm_spec::ty_model! {
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
            // Shadow of the pre-action selection, written by every action from the pre-state
            // (assignments are simultaneous), so an invariant can say "this event changed
            // nothing" about an action that writes nothing.
            var prev_alive = 0;
            var prev_sel_lo = 0;
            var prev_sel_hi = 0;
            // What just happened. Ordered, so membership tests are prefixes: the grammar
            // renders `&&` unparenthesised and `||` parenthesised, so every test below is a
            // nested `if`. 0 none, 1 user gesture, 2 typing press, 3 inert press, 4 region
            // damage, 5 uniform scroll, 6 eviction, 7 wholesale.
            var last_event = 0;

            // Guarded on the floor: an evicted row is not on screen to select. Without it the
            // model reaches a dangling anchor no code path can produce.
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
            // A one-row selection on the oldest retained row, so `Evict`'s both-endpoints-gone
            // arm is reachable (`SelectLow` and `SelectHigh` both have `sel_hi > 0`).
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
            // A deliberate deselect, always allowed.
            action UserClear {
                prev_alive = alive;
                prev_sel_lo = sel_lo;
                prev_sel_hi = sel_hi;
                alive = 0;
                truncated = 0;
                last_event = 1;
            }
            // The one handover: typing means take me to the prompt, and deselect.
            action TypingPress {
                prev_alive = alive;
                prev_sel_lo = sel_lo;
                prev_sel_hi = sel_hi;
                alive = 0;
                truncated = 0;
                last_event = 2;
            }
            // A bare modifier. The guard keeps the space to selections that exist to be
            // destroyed; the invariant compares against `prev_alive`, not the guard.
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
            // Row 3 replaced. Overlap reduces to `sel_hi > 2`, because `sel_lo <= 3 ==
            // band_hi` holds for every reachable selection. Its mutant is the inverse of
            // `RegionDamageLow`'s (damage that hit and did not clear).
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
            // Retention drops the oldest row. A head below the new floor clamps and records
            // the loss; only both endpoints gone destroys the selection. Every right-hand
            // side reads the pre-eviction state (assignments are simultaneous), so
            // `truncated` and `sel_lo` agree. `truncated = 0` on the survivor branch restates
            // the pre-value: `Evict` is the only writer of `1` and its `floor == 0` guard
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

            // A bare modifier expresses no intent and may not destroy anything: stated as
            // "the event changed nothing", which is falsifiable, not "a selection exists",
            // which the guard would grant for free.
            invariant InertPressPreservesTheSelection:
                if last_event == 3 {
                    alive == prev_alive && sel_lo == prev_sel_lo &&
                    sel_hi == prev_sel_hi
                } else {
                    alive <= 1
                };
            // Ordinary output cannot take a highlight.
            invariant UniformScrollPreservesTheSelection:
                if last_event == 5 {
                    alive == prev_alive && sel_lo == prev_sel_lo &&
                    sel_hi == prev_sel_hi
                } else {
                    alive <= 1
                };
            // Damage that missed the selected rows leaves them alone. Disjointness is decided
            // on the pre-state anchors and the conclusion read off the post-state.
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
            // …and damage that hit them must clear: a highlight over replaced text makes a
            // copy return something the user never selected.
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
            // Losing the oldest line of a selection is not losing it: the head clamps to the
            // new floor and the loss is recorded. Losing every line of it is. The arm is
            // decided by the pre-eviction interval.
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
            bound StateIsBounded:
                alive <= 1 && truncated <= 1 && floor <= 1 && sel_lo <= 3 &&
                sel_hi <= 3 && band_lo <= 3 && band_hi <= 3 &&
                prev_alive <= 1 && prev_sel_lo <= 3 && prev_sel_hi <= 3 &&
                last_event <= 7;
        }
    }
}

/// Selection custody: the alt-screen selection park, as a lifecycle.
///
/// A text selection belongs to the screen it was made on. Entering the alternate screen parks
/// the main screen's selection and leaves the alt buffer with none; leaving restores it and
/// empties the parked slot (two `mem::take`s at the top of `Terminal::post_process`).
///
/// The property is the slot's lifetime: `parked_sel` is alive only between an enter and the
/// next leave, which bounds the clear-site list to a handful of destroyers rather than giving
/// every future destroyer a second obligation. They come in three shapes, one action each:
///
/// * `Wholesale`: `clear_scrollback` and a width resize destroy what both slots named and leave
///   the screen where it is;
/// * `Reset`: `Terminal::reset` and byte-stream RIS destroy both and drop the alternate buffer,
///   so they also land on the main screen (for a direct reset an alt->main switch the restore
///   in `post_process` never sees, and for RIS followed by a re-entry in the same batch a
///   switch it never runs);
/// * `RestoreMain` / `RestoreAlt`: `restore_checkpoint` adopts another session's grids and
///   whichever screen it was on. It retires the parked slot, whose anchors name a lineage this
///   terminal never saw, and leaves the live selection standing: that is the seamless-update
///   adopt path, where clearing it would make a highlight vanish across an in-place update.
///
/// `Buggy=1` has two members naming different invariants: a symmetric swap instead of an
/// asymmetric take, under which the alt screen's own selection stays parked after the leave
/// and reappears on the next round trip over a buffer the user cannot see; and a destroyer
/// that forgets the parked slot, at every one of the three shapes, the failure mode of
/// coordinated clear sites that no compiler notices.
///
/// Tier-1: `aterm-core/src/terminal/alt_selection_park_conformance.rs` walks the whole
/// reachable space on the real `Terminal` (real `?1049h` / `?1049l` bytes, `clear_scrollback`,
/// a width resize, `Terminal::reset`, byte-stream RIS alone and RIS then `?1049h` in one batch,
/// and `restore_checkpoint` from a main-screen and an alt-screen checkpoint) from both screens,
/// reading the parked slot itself, and rejects each `Buggy = 1` member injected into the same
/// real trace at its own action.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn alt_selection_park_model() -> Model {
    aterm_spec::ty_model! {
        AltSelectionPark {
            const Buggy = 0;

            // 1 while the alternate screen is up.
            var on_alt = 0;
            // A selection on the active screen, whichever that is.
            var live_sel = 0;
            // The other screen's selection, held across the switch.
            var parked_sel = 0;
            // What just fired: 0 a user gesture, 1 an enter, 2 an in-place
            // destroyer, 3 a leave, 4 a reset, 5 a checkpoint restore.
            var last_event = 0;

            action Select {
                live_sel = 1;
                last_event = 0;
            }
            action Deselect {
                live_sel = 0;
                last_event = 0;
            }
            // The park. Assignments are simultaneous, so every right-hand side reads the
            // pre-state: at Buggy=1 this pair is a swap, at Buggy=0 the live slot is emptied.
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
            // clear_scrollback, a width resize: the content under both selections is gone, so
            // both slots go. The screen stays where it is.
            action Wholesale {
                live_sel = 0;
                parked_sel = if Buggy == 1 { parked_sel } else { 0 };
                last_event = 2;
            }
            // `Terminal::reset`, RIS: both slots go, and so does the alt buffer.
            action Reset {
                on_alt = 0;
                live_sel = 0;
                parked_sel = if Buggy == 1 { parked_sel } else { 0 };
                last_event = 4;
            }
            // `restore_checkpoint` of a session that was on the main screen / on
            // the alternate screen. The live selection is left standing.
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
            // Caught on its own by a forgetful restore of an alt checkpoint, which
            // `ParkedEmptyOffAlt` cannot see: the stale slot is still "on alt".
            invariant RestoreRetiresTheParkedSlot:
                if last_event == 5 { parked_sel == 0 } else { last_event <= 5 };
            bound StateBounds:
                on_alt <= 1 && live_sel <= 1 && parked_sel <= 1 && last_event <= 5;
        }
    }
}

/// One-key press/repeat/release pairing at the GUI-to-PTY boundary.
///
/// A press consumed by a physical-key GUI gate or by the engine-key overlay gate records a
/// per-key disposition. Repeats only peek at it, and the matching release removes it without
/// emitting Kitty CSI-u bytes. A press already forwarded to the PTY stays outstanding through
/// overlay/gate changes and is owed its repeat/release reports at the exact press-time target.
/// The two-window lattice moves focus while the key is held: the physical release may arrive
/// through window 2, but a consumed owner stays byte-silent, a forwarded encoded/literal owner
/// routes to window/session 1, and an untracked repeat is byte-silent. The model projects one
/// entry of the process-wide physical-key owner map; other-key independence and the full
/// window/session identity are Tier-1 negative controls. Explicit controller releases with no
/// preceding physical episode are outside this pairing environment.
///
/// `Buggy=1` is the regression family: a consumed press emits an orphan release/repeat, a
/// repeat re-decides or consumes the press-time disposition, closing an overlay clears its
/// still-needed tracker entry, an overlay swallows the untracked release owed to a pre-overlay
/// forwarded press, or repeat/release routing re-resolves current focus instead of keeping the
/// original target. The literal (`[key_sequences]`) and local-repeat (`[keybindings]`) releases
/// are decided again at release time (a chord re-lookup): a modifier let go mid-hold misses the
/// chord, and the release falls through to the encoder as an orphan Kitty report for a press
/// the PTY never saw.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn input_release_pairing_model() -> Model {
    aterm_spec::ty_model! {
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

/// Full two-phase overlap handoff shared by native-update parent and child.
///
/// The parent parks its readers, a readerless child paints and proves the exact
/// adoption/layout/session set, the parent rechecks every mutable admission fact, and one
/// atomic Commit+exit grants ownership. The child releases its prebuilt reader gate directly
/// from Commit; the later UI wake is diagnostic only. Every pre-Commit failure signals the
/// candidate's entire process group and then reaps the direct child before parent readers
/// resume or deferred teardown replays, including when the group leader has exited but a
/// descendant lives. The guarded v0.52 one-byte branch requires an exact, strictly-newer,
/// zero-history legacy payload before ACK+release.
///
/// This is a process/event-loop interleaving model through the atomic Commit linearization
/// point. It claims no power-loss durability, and physical input arriving after Commit belongs
/// to the replacement process's input path. Input that reaches this parent while its main
/// thread holds the claim (`arbiter == 1`) is the parent's: what the claim answers before the
/// write (a live `video` request, the messages flush) can take real time, so the Commit drains
/// that input through one more dispatch fence before it writes, or, undrained, gives the claim
/// to the rejection.
///
/// `Buggy=1` admits the regression family: a partial/stale proof Commit, reader release on
/// ProofReady, legacy ACK for ambiguous/scrolled state, parent resume before reap, a wait
/// before the group signal (stranding a live descendant after its leader exited), a parent
/// that exits after its Commit write without checking the result (an EPIPE from a child
/// already gone still ends the parent, and nobody owns the terminal), a reject sweep whose
/// signal reaches only the leader (`kill(pid)` for `kill(-pgid)`), and a Commit written over
/// input queued to the parent after its claim, which dies with the parent's `_exit`.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_overlap_handoff_model() -> Model {
    aterm_spec::ty_model! {
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
            // Session death (HUP/ERR) rejects, while queued readable output
            // (`pty_output_queued`) is tolerated: post-park bytes wait gap-free in the kernel
            // and replay through the child's fresh parser after Commit.
            var sessions_alive = 1;
            var pty_output_queued = 0;
            // Queued-but-undispatched hardware input defers Commit (it must drain through the
            // live input path into the persistent masters before `_exit`) but is no failure.
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
            // The late park. On the launched lane the successor exists before the parent
            // parks: launched, booted and dialled with every reader live, holding nothing
            // until the post-park grant's first descriptor message (`granted`; a grant of more
            // than 62 sessions continues in further messages, `NativeUpdateRendezvousGrant`).
            // Until then a revocation kills, reaps and retires the candidate with the parent's
            // readers never stopped, and a parked parent whose capture missed may resume its
            // readers beside the live candidate and park again, since no descriptor has left.
            // `granted` is the one flag both lanes share: the fork lane grants at the spawn.
            var launched_early = 0;
            var granted = 0;
            var retired = 0;
            // A revoked successor is proven gone one of two ways: with no signal of ours (this
            // flag: it exits on the closed rendezvous, or had died before), or it ignores the
            // EOF for the whole grace and we kill it. Both reach `child_reaped`. Who gives its
            // counted trial launch back is the death evidence's to decide, not modelled here.
            var successor_exited = 0;
            // The launched successor has dialled the rendezvous: the hold, the park and the
            // grant all come after it. Before it the attempt can still end — the dial budget
            // runs out under load, or the cancel comes — with the successor alive in its
            // boot, and that end is a revocation like any other: its retire needs the reap
            // too (round eight of the update audit, lead U41).
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
            // Its claim is accepted, with every reader live; a revocation before this is
            // the dial wait's (`app_update_handoff::await_launched_dial`'s budget or cancel).
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
            // The other, commoner way a revoked successor ends: the closed rendezvous is a
            // zero-byte `recv` it maps to "the parent closed the rendezvous", so it forgives
            // its own trial launch and exits before any window, unsignalled
            // (`group_signaled` stays 0). A successor already dead when the revoke lands (a
            // hang-up, the machine's kill during the hold) takes this action too.
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
            // Queued output during the overlap is no failure: the parent consumes no post-park
            // bytes, so the carried checkpoint stays a valid ground-state prefix and the child
            // drains the queue after Commit. Commit admission never reads this variable.
            action PtyOutputQueues when (
                phase == 3 && pty_output_queued == 0 && arbiter == 0
            ) {
                pty_output_queued = 1;
            }
            // Session death fails closed: the adoption proof's live-set identity is stale, so
            // the attempt must reject.
            action PtySessionDies when (
                phase > 1 && phase <= 3 && sessions_alive == 1 &&
                arbiter == 0 && failure == 0
            ) {
                sessions_alive = 0;
                failure = 1;
            }
            // Hardware input queued in the OS is no failure: it parks Commit (admission needs
            // the queue drained) until the run loop delivers it to the persistent masters.
            // Input queues to the parent until it exits, including while its claim is held
            // (`arbiter == 1`), since what the claim answers before the write takes real time.
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
            // The write waits for the input queued under the claim to drain
            // into the masters: the dispatch fence the held claim goes through
            // (`App::finish_proven_handoff`, `HeldCommit`).
            action CommitModern when (
                protocol == 0 && phase == 3 && child_live == 1 &&
                arbiter == 1 && commit_admission_exact == 1 && input_queue_quiet == 1
            ) {
                phase = 4;
                arbiter = 3;
                commit = 1;
                parent_exited = 1;
            }
            // The held claim's fence ran out with input still undelivered (its
            // drain deadline, a sink that never settles), or its admission, read
            // again after the fence, refused it: the claim goes to the
            // rejection, as a failed Commit write's does, and grants nothing.
            action HeldClaimRefused when (
                protocol == 0 && phase == 3 && arbiter == 1 &&
                commit_admission_exact == 1 && commit == 0 && input_queue_quiet == 0
            ) {
                arbiter = 2;
                failure = 1;
                commit_admission_exact = 0;
            }
            // Shipping never reaps (`try_wait`) after ProofReady. A child that exits before
            // Commit is seen by the worker's non-reaping liveness probe, which rejects through
            // `WorkerWinsRejectArbiter`; once the main thread's Commit has won the arbiter, the
            // exit shows only when the atomic Commit-pipe write fails (normally EPIPE), and
            // this transfers the arbiter to rollback without granting authority.
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
            // The Commit written at once after what its claim answered, over the input queued
            // to the parent meanwhile, which then dies with the `_exit`.
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
            // A reap follows the signal that licensed it, unless there was nothing to signal:
            // a direct child is never `wait`ed before its group is swept (the `wait` reaps the
            // leader and destroys the pid identity the sweep needs). A successor that exited
            // on the closed rendezvous was never signalled: a vacant pid proves its end.
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
            // An attempt retires only over a successor proven gone, so a retry never launches
            // beside a candidate still running, dialled or not. The proof is the reap,
            // licensed either by the successor's own exit on the closed rendezvous or by our
            // kill.
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

/// Exact live-caret DFA for canonical profanity completion.
///
/// Every proper prefix of `fuck`, including `fuc`, is ordinary text. Only the complete token
/// activates and creates an episode; harmless continuations, non-token contexts, ignore rules,
/// and a delimiter-settled `fuc` stay inactive. The mutant activates one character early, with
/// two consequences: the cue that fired at `fuc` cannot be taken back, so a `fuchsia`
/// continuation or a delimiter retracts the highlight but keeps the episode
/// (`HarmlessAndSettledAreInactive`); and the completing `k` mints a fresh episode instead of
/// continuing the prefix's, so `fuck` cues twice (`CompletionCreatesExactlyOneEpisode`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn exact_profanity_completion_model() -> Model {
    aterm_spec::ty_model! {
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
            // `active`, `canonical_identity` and `episode` move together in
            // every phase: the prefix, completion and settled laws pin all three
            // at once, so no separate law ties them to each other.
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

/// Process-crash/interleaving model for the native updater's fixed-path install transaction
/// and boot-health confirmation. It models process crashes, which preserve every completed
/// filesystem transition represented here, not sudden power loss: every ledger record goes
/// through `manifest::write_durable` (file `sync_all`, rename, parent-directory sync) and the
/// fixed-path swap syncs its parent directory, but a filesystem that reports sync unsupported
/// degrades to non-durable, so power-loss durability is no claim of this model.
///
/// Identity `1` is exact OLD, `2` exact authorized NEW, and `3` a same-build or
/// superseding-but-unauthorized artifact. The committed path verifies OLD's build+sealed commit
/// before preparation, arms the exact trial before one atomic fixed-path swap, records NEW's
/// exact build+commit+digest receipt, reconstructs that receipt after the swap-before-receipt
/// crash cut, and GCs rollback only after a real first-present, health proof, and successful
/// disarm. Startup consumes inherited re-exec/expected-artifact authority before any verifier
/// can inherit it, then observes an armed trial before interpreting that authority.
///
/// A WINDOW'S CONFIRMATION KEEPS THE PREDECESSOR'S BYTES (2026-10-17). A window that makes
/// the swap itself and then execs (a cold launch with a stage ready, a fork-lane successor,
/// and before 2026-10-19 the window LaunchServices started for an update) is judged by macOS
/// for its whole life by the bundle at the path its first consent check saw: the fixed path.
/// Deleted there, the window's consent checks are refused even with the grant on. Since
/// 2026-10-19 a LaunchServices-launched successor swaps nothing (its parent runs this same
/// transaction before the launch; the model has no process identity, so that is no new
/// step), and identity `4` remains for the lanes that exec. So
/// `GarbageCollectRollback` ends the predecessor's authority but keeps the bytes for that
/// window (identity `4`, `install::keep_settled_predecessor`), and `ReleaseKeptPredecessor`
/// deletes them once the window is gone and no trial is armed
/// (`install::release_kept_predecessor_under_apply_lock`). A kept predecessor is never a
/// revert source (`KeptPredecessorIsNoAuthority`); the next transaction's preparation reuses
/// the path as it reuses any stale rollback.
///
/// THE VERIFIER IS NOT THE SAME ACROSS BUILDS (2026-10-16). OLD passed the swap's
/// rollback-source check under its own policy, and the trial build's Team-ID-pinned verifier
/// may refuse the same bytes with a verdict (`fixed_refused`, the environment step
/// `RunningVerifierRefusesRetainedOld`). The bytes are still exact OLD, but no revert can
/// restore them (`install::Predecessor::Unavailable`), and they are no candidate this build
/// placed (every candidate passes its verifier first), so they are no reason to hold the
/// trial. A window's confirmation proves health and collects them as any confirmation does;
/// a counted launch settles the trial WITHOUT REVERT (`SettleWithoutRevert`,
/// `install::settle_trial_without_revert`) once the installed identity and its exact receipt
/// are proved, disarming before it removes the fixed path. In this model nothing a running
/// build VERIFIES is collected without a health proof and a disarm
/// (`GarbageCollectionRequiresProofAndDisarm`).
///
/// THE MODEL TAKES THE RUNNING VERIFIER'S ANSWER AS STABLE WITHIN A TRIAL:
/// `RunningVerifierRefusesRetainedOld` needs `rollback_verified == 0`, and
/// `VerifyExactRollback` needs `fixed_refused == 0`. The code keeps no such memory: each
/// FullProof launch asks `install::fixed_predecessor` afresh, so a verdict after an earlier
/// pass (a transient Gatekeeper rejection of a genuine predecessor) settles the trial over a
/// predecessor that once verified, which is `BuggySettleOverVerifiedRollback`'s shape. That is
/// the accepted transient-verdict risk, and this model does not cover it. Only a verdict
/// settles: a helper ended by a signal, or a failed wait on one, is a passing refusal and
/// keeps the trial. A fixed path emptied by something outside the updater, or holding a
/// non-directory, is answered like a refused one by the code and is not modelled: the updater
/// itself never empties it or leaves a non-directory there under an armed NEW
/// (`ArmedNewRetainsExactRollback`).
///
/// `Buggy=1` enables independent shortcut controls, each its own action dead at `Buggy=0`:
/// build-only OLD or mismatched prior-receipt authority, an inherited-authority early return, a
/// superseded swap, pre-present disarm, proof-failed early GC, and failed receipt restoration
/// that leaves NEW's receipt authoritative. Nine more are slips the recovery laws refuse: a
/// legacy refusal that disarms the trial it was deferring for; a receipt written ahead of the
/// swap it describes; a two-rename "swap" failing between its renames with NEW installed and
/// OLD off the fixed path (`RENAME_SWAP` makes that cut impossible); a failed restore that
/// disarms the trial to stop the crash loop; an exec failure that runs the rollback GC before
/// any restore; a crash-loop restore of an unverified rollback; a restored rollback GC'd
/// while its trial is still armed; a settle without revert over a predecessor that verifies,
/// which discards a revert the trial still has; and a predecessor kept as its window's bundle
/// while its trial is still armed.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_update_disk_transaction_model() -> Model {
    aterm_spec::ty_model! {
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
            // A v0.52 parent can hand off an already-swapped NEW with an armed trial and exact
            // OLD rollback but no ready/installed receipt: a reachable startup state of its own,
            // not a post-swap start treated as staged OLD.
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
            // 3 unauthorized identity, 4 (fixed only) settled OLD kept for its window.
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
            // The RUNNING build's verifier refused the exact OLD at the fixed path with a
            // verdict: no revert can restore it. A fact about the verifier, not the bytes.
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
            // No receipt and no ready record: no evidence is left to rebuild from, so the
            // shipping code returns "no exact receipt or authorized recovery record" and
            // defers, keeping the armed trial and fixed rollback for a later launch. Modelling
            // that refusal keeps this state reachable but not stuck.
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
            // The window the settled predecessor was kept for is gone, and no trial owns
            // the fixed path: a later launch deletes the bytes.
            action ReleaseKeptPredecessor when (
                phase == 6 && fixed == 4 && trial == 0
            ) {
                fixed = 0;
            }
            // The slip `KeptPredecessorIsNoAuthority` refuses: the predecessor kept as the
            // window's bundle while its trial is still armed, where a crash-loop revert
            // would still take it as the rollback.
            action BuggyKeepPredecessorBeforeDisarm when (
                Buggy == 1 && phase == 4 && trial == 1 && health_proved == 1 && fixed == 1
            ) {
                fixed = 4;
            }
            // A counted launch of the trial: the installed identity and its exact receipt
            // are proved and the predecessor is unrevertible, so the trial is disarmed and
            // only then the fixed path cleared. No health was proved, so none is booked.
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
            // A receipt rebuilt from Ready stays exact until a crash-loop restore
            // retires it with its rollback. (That it needs a Ready record at all is
            // `RecoverModernReceiptFromReady`'s guard, the flag's only writer.)
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
            // The fixed path goes, or stays for its window as no authority, only after a
            // disarm, with the receipt exact. A bundle the running build VERIFIES goes only
            // after a health proof too; one it refused is no revert, so a settle needs no
            // proof of health for it.
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
            // A predecessor kept for its window is no revert source: it outlives only a
            // proved, disarmed trial, and never as an exact rollback.
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

/// A Settings page's scroll list is a bounded cursor over its stops, shared by the wheel and
/// arrow keys (a row), Page Up/Down (the rows shown, a signed multi-row delta) and absolute
/// accessibility scroll positions. Every transition first clamps an obsolete cursor to the
/// current limit (a grown window or a changed layout shrinks it), then applies its motion and
/// clamps again. `Buggy=1` drops the upper clamp for a row forward, Absolute, and a forward
/// page.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn settings_page_scroll_model() -> Model {
    aterm_spec::ty_model! {
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

/// Video recording and publication are one serialized, fail-closed lifecycle.
///
/// The control thread pre-creates a private, server-named directory before the event-loop
/// request is armed. `recording_slot` owns that request through the pending/recording phases;
/// `BeginExport` atomically hands ownership to the process-wide `export_permit`. Success
/// transfers the directory from private ownership to a published artifact, while rejection,
/// cancellation, encode failure, owner loss, and a live opacity transition all remove it.
///
/// Mode is honest about the presentation source: a swapchain tap requires glass, and an
/// offscreen present-real recording requires no glass and owns a pacing timer, its sole redraw
/// driver. A glass recording owns one exactly when the request asked for `pace`
/// (`video <seconds> ... pace`), because the post-present hook alone starves once an unchanged
/// frame is skipped; `video_initial_next_frame` arms both (`RecordingOwnsItsPacingTimer`). The
/// timer lives in the recording itself, so outside the recording phase it does not exist. A
/// translucent transition aborts a live tap before another frame can be accepted.
///
/// `Buggy=1` exposes independently reachable failure classes: cleanup that strands the private
/// directory, a windowed request taking the headless arm, a second recording acquiring
/// ownership while the first still exports (the begin guard testing `video_rec` without
/// `video_export.is_busy()`), publication without winning the cancellation CAS, and a tap
/// retained after the glass becomes translucent. Six more dead mutants are each one line of
/// the shipping lifecycle written wrong:
/// * `BuggyExportRetainsVirtualTarget`: `video_finalize` hands the take to the exporter
///   without `ws.present = None`, so a headless window keeps its recording-only `Virtual`
///   target (the idle-zero law `finalize_drops_virtual_target_and_timer` pins).
///   `ModeMatchesRecordingPhase`.
/// * `BuggyFinalizeKeepsRecordingSlot`: `video_finalize` reading `video_rec` through
///   `as_mut()` instead of `take()`, so the slot still reads occupied during the export and
///   every later `video` request is refused as "already in progress". `SlotMatchesPhase`.
/// * `BuggyDetachGlassDuringTap`: a window close drops its glass without
///   `video_abort_window_close`, so the swapchain tap outlives the surface it copies.
///   `TapOnlyOnGlass`.
/// * `BuggyBeginHeadlessUntimed`: `video_initial_next_frame` written as `pace.then_some(now)`,
///   so an unpaced offscreen loop begins with no deadline and records nothing.
///   `RecordingOwnsItsPacingTimer`.
/// * `BuggyPacedTapUntimed`: the same function returning `None` for every swapchain tap, so a
///   paced glass recording starves on its first skipped unchanged frame.
///   `RecordingOwnsItsPacingTimer`.
/// * `BuggyLateCancelRevokes`: `VideoCancellation::cancel` as a blind store of
///   `VIDEO_CANCELLED` instead of the CAS from `VIDEO_CANCEL_LIVE`, so a timeout after
///   `authorize_commit` revokes an export already past its rename boundary.
///   `CancelledOwnsNothing`: a won cancellation has ended the lifecycle, while this one still
///   exports.
///
/// No law restates another. "Recording and export never overlap" and "at most one lifecycle is
/// live" are `SlotMatchesPhase` and `PrivateDirectoryOwnedByLifecycle`, which the
/// second-recording mutant breaks on its first step; "a published take owns nothing" is
/// `CommitAuthorizationScope` (published only at phase 0) read through the same two laws, and
/// "a cancelled lifecycle owns nothing" is `CancelledOwnsNothing` (cancelled only at phase 0)
/// read through them too. A losing cancel changes nothing, so `CancelAfterAuthorization` is a
/// stutter.
///
/// Tier-0 exercises each mutant directly in addition to the exhaustive prove-and-catch check.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn video_recording_lifecycle_model() -> Model {
    aterm_spec::ty_model! {
        VideoRecordingLifecycle {
            const Buggy = 0;
            // 0 Idle, 1 Reserved/pending, 2 Recording, 3 Exporting.
            var phase = 0;
            var glass = 0;
            // The request's `pace` flag.
            var pace = 0;
            // 0 None, 1 SwapchainTap, 2 OffscreenPresentReal.
            var mode = 0;
            var timer = 0;
            var translucent = 0;
            // The pending/recording side and exporting side of the serialized
            // lifecycle. They must never overlap.
            var recording_slot = 0;
            var export_permit = 0;
            // Private, server-owned directories for the current lifecycle.
            var private_dirs = 0;
            // The current lifecycle completed its atomic publication.
            var published = 0;
            // Publication CAS: 0 Live, 1 Cancelled, 2 CommitAuthorized.
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
            // The losing side of the CAS: a cancel after authorization changes
            // nothing.
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
            // Each mutant is its own action, not a `Buggy` arm inside a live one: the
            // strict-vacuity audit removes the dead set and requires the remaining Buggy=1
            // baseline to be safe, so an inline arm would make every dead action uncreditable
            // as a negative control.
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
            // Phase 0 owns no slot, permit, or directory
            // (`SlotMatchesPhase`, `PrivateDirectoryOwnedByLifecycle`) and a
            // cancelled take publishes nothing (`CommitAuthorizationScope`), so
            // "a won cancellation has ended the lifecycle" is all this law adds.
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

/// Retention of per-process capture namespaces is decided by the exact instance lease, not
/// merely by PID liveness.
///
/// Lease state `0` is a legacy namespace with no lease file, `1` a lock held by a live exact
/// process instance, `2` a well-formed but freely lockable stale instance, and `3`
/// malformed/untrusted lease metadata. A held lease and malformed metadata fail closed to keep.
/// A free lease is exact proof of staleness and is removed even if its recorded PID has been
/// reused. Only the missing-legacy case falls back to the coarse PID probe.
///
/// `Buggy=1` independently recreates removal of held/malformed namespaces, the PID-reuse leak
/// that keeps a free lease when the numeric PID is alive, and a legacy namespace kept though its
/// PID is dead (the `InstanceLeaseState::Missing if !pid_alive => Remove` arm of
/// `decide_instance_namespace_sweep` folded into the fail-closed `Keep` arm, so every pre-lease
/// capture namespace leaks for ever).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn exact_instance_retention_model() -> Model {
    aterm_spec::ty_model! {
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

/// A confined artifact operation is a retained-handle transaction, not a path string that may
/// be resolved again after an attacker swaps an ancestor.
///
/// `ConfinePin` retains the original inside object and its identity. Reads and writes target
/// that pinned object even when the path's ancestor is swapped. `ValidateReply` then compares
/// the reply-time path identity with the pin: unchanged identity may be certified; a swap fails
/// closed, whether it happened before the operation or in the operation-to-reply interval.
///
/// `Buggy=1` exposes both forbidden TOCTOU classes: re-resolving a swapped path reads or writes
/// the outside object (`BuggyReresolveRead`/`Write`), which is every operation under path-only
/// confinement (`AnchoredAccessNeverOutside`); and reply construction certifies the swapped
/// identity despite having pinned a different object (`BuggyCertifySwapped`), which is what a
/// reply built without `validate_for_reply` says once the ancestor has moved.
///
/// No state records that the transaction is pinned or that the reply was validated: such a
/// flag would be written only by the action it names, so a law over it restates that
/// assignment. The harm is the outside effect and the false certificate, and the two laws
/// judge exactly those. "A swapped path is never certified" and "a successful reply certifies
/// the original" break together (a reply can certify something other than the pinned original
/// only once the path has moved), so `SuccessfulReplyCertifiesOriginal` states both.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn anchored_artifact_transaction_model() -> Model {
    aterm_spec::ty_model! {
        AnchoredArtifactTransaction {
            const Buggy = 0;
            // 0 Unconfined, 1 Pinned, 2 Operated, 3 Replied.
            var phase = 0;
            var swapped = 0;
            // 0 None, 1 OriginalInside, 2 SwappedOutside.
            var path_identity = 0;
            // 0 None, 1 Read, 2 Write.
            var operation = 0;
            // 0 None/fail-closed, 1 OriginalInside, 2 Outside.
            var effect_target = 0;
            // 0 Pending, 1 Success, 2 FailClosed.
            var reply = 0;
            // 0 None, 1 OriginalInside, 2 SwappedOutside.
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

/// A filesystem artifact reply is a transaction that spans two threads, the
/// complete control-socket write, and a nonce-bound peer acknowledgement. Timeout
/// cancellation and final-name authorization have one winner. A successful
/// worker queues a publication guard; the socket thread revalidates it before
/// any OK byte, then writes and flushes the complete frame plus a fresh nonce challenge.
/// Only the matching post-challenge echo permits immediate release. A failed or
/// abandoned ACK enters an additional central nonblocking quarantine; its two
/// bounded ticks abstract the shipping 30-second delay, and the guard survives
/// until explicit expiry. A partial `write_all` failure enters the same
/// quarantine because path bytes may already be visible even though the complete
/// frame receipt guarantee was never established. A pre-wire abort remains the
/// only direct cleanup path because it removes an unpublished artifact.
///
/// `Buggy=1` exposes ten independently audited failures: publishing after
/// timeout won, dropping a queued guard before the reply reaches the wire,
/// pruning a leased artifact, releasing without a valid ACK, reading an ACK
/// before the server's causal challenge exists, releasing quarantine before
/// expiry, and four lines of the shipping writer or guard written wrong:
/// * `BuggyChallengeBeforeBody` — `ControlReply::into_wire` queueing the
///   challenge ahead of the body, so an echo no longer proves the client read
///   the complete frame (`ChallengeRequiresCompleteWire`);
/// * `BuggyOkBeforeRevalidation` — `Outbox::attach` queueing the OK body before
///   `retention.prepare_write()`: when revalidation then fails, the client has
///   read OK for a file the abort removes (`CommitRequiresWirePreparation`);
/// * `BuggyTrailerErrorIgnored` — `Outbox::deliver_to` dropping the error of a
///   write that left the challenge unsent, so the partial frame awaits an ACK
///   that cannot come, instead of entering quarantine
///   (`CompleteReplyPrecedesAck`);
/// * `BuggyAbortRetainsArtifact` — `CaptureReplyRetention`'s drop without its
///   uncommitted `remove_exact` arm (`AbortReleaseRemovesUncommittedArtifact`).
///
/// Each law is one claim, placed by `phase`; none repeats another's clause.
/// "A failed ACK or partial write enters quarantine" is not a law of its own:
/// every other exit is refused by the law of the phase it would land in — an
/// immediate release needs a valid ACK (`ImmediateReleaseRequiresValidAck`), a
/// committed reply never takes the abort path (`CommitRequiresWirePreparation`),
/// and a quarantined guard is held (`OwnedThroughClassifiedExitRetainsGuard`)
/// until the delay has run (`QuarantineReleaseRequiresExpiry`). Quarantined is
/// phase 7 and expired is phases 8/11; the verdicts `ack_failed`/`write_error`
/// are the ACK wait's and writer's observations, which the Tier-1 binds compare
/// step by step.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn artifact_reply_publication_model() -> Model {
    aterm_spec::ty_model! {
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
            // The ACK phases (5, 6, 10) carry the complete frame, and the frame
            // exists only once it was written after preparation.
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
            // Only the file: the commit/reply/challenge/ACK laws place their variables in
            // phases that exclude 12, and the guard's drop is the release that enters it.
            invariant AbortReleaseRemovesUncommittedArtifact:
                if phase == 12 { artifact == 0 } else { artifact <= 1 };
        }
    }
}

/// Aggregate admission bound for queued, in-flight, and quarantined path
/// artifacts and their directory chains (aterm-gui's
/// `ArtifactBudgets::descriptors`, a `UnitBudget`). A failed acknowledgement
/// releases its worker immediately but not its filesystem guard, so worker
/// count alone cannot bound retained resources.
/// Admission stops at either the object cap or the aggregate descriptor-unit
/// cap; any release returns both charges atomically. The three charge buckets
/// are a bounded multiset of live permits (one, two, or three units). `Select*`
/// are ghost scheduler choices that let the checker explore every release and
/// reconciliation order without collapsing heterogeneous permits into one
/// unsound "last charge" scalar. `Charge` is the representative acquisition
/// cost, chosen so descriptor saturation occurs while the count still has room.
///
/// `Buggy=1` enables four dead negative controls, each one line of
/// `UnitBudget` written wrong:
/// * `BuggyOverbook` admits past either cap (`Bounded`);
/// * `BuggyReconcileOverbook` grows a permit past the unit cap (`Bounded`);
/// * `BuggyRefuseLeaksSlot` is `try_take` taking the count slot before the unit
///   check and returning `None` without giving it back — each refusal at the
///   unit cap leaks one slot until the pool answers busy forever
///   (`CountMatchesCharges`);
/// * `BuggyReleaseProvisionalCharge` is `UnitPermit::reconcile` updating the
///   pool without recording the permit's new charge, so the grown permit's
///   drop returns its provisional charge and strands the unit reconciliation
///   added (`UnitsMatchCharges`).
///
/// No separate "each live permit owns a unit" law: every bucket charges at
/// least one unit, so that follows from the two conservation laws.
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
            // Fires only while the charges still balance: the slip strands one
            // unit per firing, and letting it repeat would grow
            // `descriptor_units` without bound and leave the Buggy=1 space
            // unenumerable.
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

/// Durability ordering for one published video batch. Each `WriteMember` is a member whose file
/// contents are already synced; `SyncBatch` records the directory barrier covering every member
/// written so far, and a later member invalidates that coverage until another barrier
/// completes. The reader-visible marker may be published only while the barrier covers the
/// whole current batch.
///
/// `Buggy=1` enables two dead negative controls. `BuggyPublishBeforeSync` shows the marker over
/// members no barrier covers (`MarkerCoversEveryMember`). `BuggySyncAhead` is
/// `ConfinedVideoDir::write_batch_member_authorized` without its `batch_synced.set(false)`: a
/// flag that stays set claims every later member too. `MarkerCoversEveryMember` cannot see it
/// (once the batch fills the counts agree), so `BarrierCoversOnlyWrittenMembers` catches it.
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

/// Refcounted video retention is a bounded lifecycle separate from publication. The producer
/// and several `video frames` readers may share one exact recording identity. Marker
/// publication or final reader validation requests one capability-bound convergence sweep. A
/// non-final release cannot start maintenance; the last release schedules it, new acquisitions
/// fail closed while that schedule/sweep is live, and only completion reopens acquisition. A
/// replacement recording at the same lexical name is refused while any original-identity lease
/// remains.
///
/// `pending` is the seam between the last count decrement and `StartSweep`. Shipping code does
/// both under one registry mutex, so no acquisition observes it; it is explicit so the ordering
/// obligation is checkable.
///
/// Two variables are not registry fields; each makes a law independent of the entry it judges:
/// * `requested` is the obligation `arm_video_retention_sweep` created, set by `Arm` and
///   discharged only by a finished sweep. The registry's own record (`armed`, the entry's
///   `video_sweep_requested`) disappears with the entry, so a law over `armed` alone could not
///   see a release that removes an armed entry instead of sweeping it.
/// * `admission_spent` is an entry whose reserved retention admission (`video_retention`) the
///   last release has taken; `join_video_artifact_state` refuses every reader of it.
///
/// `Buggy=1` enables six dead negative controls: a sweep started before the last release, an
/// acquisition during the sweep, a replaced recording joining the lease group, and three that
/// are one line of the registry written wrong:
/// * `BuggyArmedReleaseDropsEntry`: `ArtifactPathLease::drop` removing an armed entry at count
///   zero instead of sweeping it (the `video_sweep_requested` case folded into
///   `else { held.remove(&key) }`, or the `_ =>` arm a release build takes when the capability
///   or admission is missing), so the requested retention never runs
///   (`RequestedRetentionRunsAtLastRelease`).
/// * `BuggyReleaseSweepsUnarmed`: `ArtifactPathLease::drop` without its
///   `state.video_sweep_requested` test, so the last release of an unarmed recording schedules
///   the sweep around a publication nothing confirmed (`MaintenanceRequiresArm`).
/// * `BuggyFinishKeepsEntry`: `finish_video_retention_sweep` resetting `sweeping` and
///   `video_sweep_requested` instead of `held.remove(key)`; the kept entry has spent its
///   admission, so every later reader is refused with `WouldBlock`, and after a same-name
///   replacement with the stale identity's `NotFound` (`IdleNameAdmitsReaders`).
///
/// "One maintenance phase at a time" and "a finished sweep leaves the entry unarmed" are not
/// laws of their own: both branches of `RequestedRetentionRunsAtLastRelease` bound
/// `pending + sweeping` by one, and an armed count-zero entry with neither is what it refuses.
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

/// A native control mutation may stage pixels before the compositor accepts a
/// present. Screenshot capture is authorized only after that present succeeds.
/// A capture makes at most `AttemptLimit` present attempts and then fails closed;
/// dropped attempts before that bound may retry, but they never fall through to
/// reading the previous composited frame.
///
/// `Buggy=1` enables two independent slips in `capture_after_present_decision`, each a
/// dead-at-`Buggy=0` action beside the healthy `Decide`:
/// * `BuggyStaleCapture` treats a dropped present as capture authorization (`NoStaleCapture`,
///   `CaptureRequiresPresent`);
/// * `BuggyRetryAtLimit` reads the retry arm as `attempts <= attempt_limit` for the shipping
///   `<`, so the last dropped present asks for a retry instead of failing closed
///   (`DecisionMatchesOutcome`'s fail-closed arm). The barrier loop's own
///   `while attempts < attempt_limit` still stops it (`Retry` keeps that guard), so only the
///   decision law sees it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn capture_after_present_model() -> Model {
    aterm_spec::ty_model! {
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
            // A dropped present authorizes capture, and the pixels read are the
            // previous composited frame's.
            action BuggyStaleCapture when (
                Buggy == 1 && staged == 1 && decision == 0 &&
                attempts > 0 && present_succeeded == 0
            ) {
                decision = 1;
                captured = 1;
                stale_capture = 1;
            }
            // The retry arm's `<` read as `<=`: at the bound a dropped present
            // decides Retry, not FailClosed.
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
            // The attempt bound only restates `Retry`'s guard (the barrier loop's
            // `while attempts < attempt_limit`), so it is the space, not a law; Tier-1 binds the
            // real loop's call bound. Failing closed at the bound is `DecisionMatchesOutcome`'s,
            // and `BuggyRetryAtLimit` is its counterexample.
            bound ValuesBounded:
                attempts <= AttemptLimit &&
                staged <= 1 && present_succeeded <= 1 && decision <= 3 &&
                captured <= 1 && failed <= 1 && stale_capture <= 1;
        }
    }
}

/// The one-shot presented-destination tap owns exactly one staging buffer. A
/// validated destination copy advances `Armed -> Pending`; the serial-bound
/// successful-present hook advances `Pending -> InFlight`; and only a successful
/// map/conversion produces a frame. Geometry/metadata rejection and every async
/// completion failure terminate explicitly with an error.
///
/// `Buggy=1` enables three dead negative controls, each one arm of the shipping
/// `presented_frame_transition` gate written wrong:
/// * `BuggyMapErrorPublishesFrame` — the `MapError` arm returning
///   `(Complete, Frame)`: the fail-open map callback publishes a frame without
///   any successful mapping (`SuccessRequiresMappedCopy`);
/// * `BuggyRejectReservesSlot` — the `RejectEnqueue` arm copied from
///   `EnqueueValid`, `(Pending, None)`, so a rejected destination reserves the
///   staging buffer with no copy in it and maps whatever an earlier capture
///   left there (`ReservedPhaseRequiresAcceptedCopy`);
/// * `BuggyMapErrorWithoutOutcome` — the `MapError` arm returning
///   `(Complete, None)`: the one-shot ends with no error for its waiter to take
///   (`TerminalPhaseHasResult`).
///
/// "A result exists only at the terminal" is `TerminalPhaseHasResult` read
/// backwards, so it is not stated twice; nor is "a frame is published only at
/// the terminal, from an accepted copy" beside `SuccessRequiresMappedCopy`: a
/// result lives only at phase 3, and only an accepted copy is ever mapped.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn presented_frame_tap_model() -> Model {
    aterm_spec::ty_model! {
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

/// One staging slot in the streaming video tap cycles
/// `Free -> Pending -> InFlight -> Free`. Map errors and finalization aborts
/// release the reserved slot while counting one loss. Invalid live colour
/// metadata never reserves a slot and counts one loss only after the fps gate
/// accepted that sampling opportunity (decimation is outside this transition).
/// The bounded harvested-store projection then drives callback arrival
/// `3,1,2`: insertion must publish sorted `1,2,3`, and a two-frame budget must
/// evict the lowest sequence and retain tail `2,3`.
///
/// `Buggy=1` enables five dead negative controls, one fail-open slip each, so
/// every law is the first thing its own slip breaks:
/// * `BuggyMapErrorLeaksSlot` — a failed map counts its loss but leaves the
///   slot `InFlight` (`ErrorResolutionFreesSlot`);
/// * `BuggyRejectInvalidUncounted` — invalid metadata discarded as though it
///   were client decimation, `dropped` untouched (`InvalidMetadataIsCounted`,
///   which counts losses by cause, so the slip is caught whatever error or
///   abort was counted before it);
/// * `BuggyHarvestCallbackOrder`: an append in callback order, `3,1` (`HarvestedStoreSorted`);
/// * `BuggyEvictNewest` — `ordered_capture_store_push` with `pop_back` for
///   `pop_front`: the budget drops the newest capture and keeps `1,2`
///   (`BudgetKeepsNewestTail`);
/// * `BuggyEvictionUnreported` — the same push with its `evicted += 1` lost, so
///   the take's `head_truncated` reads false over a recording whose head is
///   gone (`EvictionMatchesOverflow`).
///
/// "A reserved slot carries no resolved error" is not stated beside
/// `ErrorResolutionFreesSlot`: it is the same law read backwards.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn video_tap_slot_model() -> Model {
    aterm_spec::ty_model! {
        VideoTapSlot {
            const Buggy = 0;
            const MaxDrops = 2;
            // 0 Free, 1 Pending, 2 InFlight.
            var phase = 0;
            var dropped = 0;
            // The losses by cause: map errors and aborts, and a bounded
            // one-shot witness that invalid metadata was rejected.
            var errors = 0;
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

/// Cursor trails, predictive ghosts, fades, and rain occupancy are retained in
/// window-local renderer coordinates. Before a frame may present after the
/// focused visible leaf moves, resizes, changes identity, enters/leaves zoom, or
/// changes physical surface/DPI metrics, every charged coordinate-bound effect
/// must be reset and rebound to the new space.
///
/// `Buggy=1` is a composed/single boolean-only gate: a same-class geometry change leaves the
/// charged effect bound to the old coordinate space.
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
            bound CoordinateBounded:
                coordinate <= MaxCoordinate && bound_coordinate <= MaxCoordinate;
            bound ValuesBounded:
                charged <= 1 && prepared <= 1 && stale_present <= 1;
        }
    }
}

/// The semantic preview font worker owns one replacement queue slot and tags
/// every job/result with the live renderer generation. Reload clears an obsolete
/// queued fork while an already-running old parse may finish; polling must ignore
/// that stale result and install only the current generation. `Buggy=1` enables
/// four dead negative controls:
/// * `BuggyInstallStaleResult` — the stale-worker installation defect, accepting
///   an obsolete generation's result: the classification is wrong
///   (`CurrentResultOnly`) and the stale renderer installs
///   (`ReadyGenerationIsCurrent`);
/// * `BuggyReloadKeepsInstalled` — `install_chrome_faces_locked` without
///   `fonts.semantic = semantic`: the reload bumps the generation but the
///   previous generation's renderer keeps painting under the new configuration
///   (`ReadyGenerationIsCurrent`);
/// * `BuggyIgnoreCurrentResult` — `semantic_prewarm_result_is_current` written
///   strictly (`result_generation > current_generation`): the current
///   generation's result is dropped with the stale ones and the semantic preview
///   never installs (`CurrentResultOnly`, whose other direction this is);
/// * `BuggyReloadKeepsQueued` — `reload_chrome_fonts_locked`, the one reload
///   body `set_chrome_fonts` and `set_prepared_chrome_fonts` share, without
///   `worker.cancel_queued()`: the obsolete fork stays queued
///   (`QueueContainsOnlyCurrent`). Tier-1 drives that body on a local font
///   context (`tray_raster`'s `chrome_font_reload_conforms_to_the_generation_queue_reset`).
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn semantic_prewarm_generation_model() -> Model {
    aterm_spec::ty_model! {
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

/// A static Settings preview must retain its convergence cadence until the completed font epoch
/// reaches the retained raster. Polling completion can happen while arming or firing a deadline;
/// neither may silently abandon the last repaint. `Buggy=1` decides on `pending` alone.
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

/// The semantic preview font worker has two coupled ownership decisions beyond its generation
/// guard. Replacing a queued, not-yet-started install job must carry its unique committed
/// renderer base into the newest candidate job. At completion, only an exact generation +
/// request + candidate match may become active; an exact-match construction failure clears the
/// active renderer, while superseded successes are cache-only and leave the active slot alone.
///
/// `DecideResult` is the shipping `poll_semantic_renderer` over
/// `semantic_prewarm_result_decision`, and each law judges one arm of that match:
/// `DecisionMatchesIdentity` the pure classification, `InstallOnlyLatestReady` the
/// `InstallCurrent` arm, `CurrentFailureFailsClosed` the `FailClosedCurrent` arm,
/// `CacheOnlySupersededReady` the `CacheSuperseded` arm, and `NoncurrentPreservesActive` every
/// arm that is not exact-current. Each is conditioned on the decision and says what its arm
/// does to the slot or the cache.
///
/// `Buggy=1` enables seven dead negative controls. `BuggyReplacementKeepsOnlyNewBase`
/// (replacement keeps only the new job's optional base, dropping the displaced one;
/// `ReplacementCarriesBase`) and `BuggyMixedCandidateInstalls` (a ready mixed/superseded
/// candidate installs as current), and five that are one line of the main-thread boundary
/// written wrong:
/// * `BuggyReadinessBeforeGeneration`: `semantic_prewarm_result_decision` testing readiness
///   before generation, so a ready renderer forked from an obsolete generation's base is filed
///   as `CacheSuperseded` and later served under the new configuration
///   (`DecisionMatchesIdentity`);
/// * `BuggyInstallKeepsStaleIdentity`: the `InstallCurrent` arm without
///   `self.semantic_identity = Some(result.candidate)`, so the preview never recognises its own
///   install (`InstallOnlyLatestReady`);
/// * `BuggyFailClosedKeepsPrevious`: the `FailClosedCurrent` arm without
///   `self.semantic = None`, so an exact-current construction failure leaves the previous
///   candidate painting (`CurrentFailureFailsClosed`);
/// * `BuggyDropSuperseded`: the `CacheSuperseded` arm folded into the ignore arms, so a ready
///   superseded renderer is dropped and its warmup paid again (`CacheOnlySupersededReady`);
/// * `BuggyCacheParksActive`: the `CacheSuperseded` arm opening with `InstallCurrent`'s
///   `cache_active_semantic()`, so a superseded result parks the active renderer and nothing
///   paints (`NoncurrentPreservesActive`).
///
/// Tier-1 (aterm-gui), in two halves: `src/semantic_prewarm_conformance.rs` enumerates the pure
/// `semantic_prewarm_result_decision` over the lattice, and `tray_raster`'s
/// `semantic_prewarm_poll_conforms_to_the_handshake_slot_effects` lands every lattice point
/// through the real `poll_semantic_renderer` and reads the active slot (`active_after`,
/// `active_after_latest`) back off the fonts.
///
/// The bounds are per-variable. "The latest active renderer is an active renderer" is a
/// definition, not a law: the `Mark*` inputs are built that way, every decision arm assigns the
/// two fields together, and the Tier-1 projection defines `latest` as `active` and the
/// requested identity.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn semantic_prewarm_handshake_model() -> Model {
    aterm_spec::ty_model! {
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
            // No "inputs are well formed" law: the three `Mark*` inputs are the
            // only writers of `active_present`/`active_ready`/`candidate_matches`,
            // each guarded on an empty slot, so such a law restates their guards.
            // Nor "retained paint is exact or the host seed": once resolved, the
            // law above keeps the active renderer exactly when it is not a
            // ready mismatch, which is that sentence.
            bound FlagsBounded:
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
            // The deadline is armed exactly while the cache holds something (dial 2 leaves
            // it armed after a retire); the cache holds all of it or none (dial 3 drops an
            // entry at the end of its scan); a scan has windows left exactly while it runs
            // (dial 4 never ends one).
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

/// Budgeted search resume: the public lifecycle contract of
/// `aterm_core::terminal::Terminal::search_budgeted`.
///
/// This machine models the owner-level capability that binds a partial full-buffer scan and
/// its result-delta stream to one query/content snapshot and one non-repeating `search_id`. The
/// projection covers every public field that controls host accumulation:
///
/// * `progress` / `total` are `rows_fed` / `total_rows`;
/// * `scan_done` distinguishes indexing completion from API completion;
/// * `live` / `cursor` encode the optional resume capability;
/// * `search_id` and `reset` are projected directly from the returned step;
/// * `delivery` counts a bounded dense-result trace: the scan turn delivers the first delta,
///   one drain turn leaves a backlog, and the final drain completes;
/// * `issued`, `prior_id` and `stream_id` are trace ghosts. Real process-global `u64` IDs are
///   normalized to first-seen rank, proving freshness without assuming IDs are contiguous
///   within one `Terminal`; `stream_id` is the identity the host last saw `reset` announce.
///
/// The live cursor and `search_id` are one field in the shipping step
/// (`cursor: (!complete).then_some(state.cursor)`, `search_id: state.cursor`), and a fresh
/// identity is minted only where the new state is installed, so every action writes the two
/// together. What a slip can break is that the identity changes only with `reset`
/// (`IdentityStableWithoutReset`).
///
/// `Rows = 3` is the three-row Tier-1 terminal; `DeliveryTurns = 3` stands for an arbitrary
/// match backlog, not the shipping payload cap. `StartBacklog`/`Drain`/`DrainComplete` prove
/// the seam: scanning may finish while the cursor stays live, drain-only resumes preserve row
/// progress and identity, and only the last delta retires the cursor. `StartComplete` and
/// `RestartComplete` scan and deliver everything in one turn; the latter still returns
/// `reset = true` and a fresh `search_id` with no cursor. `ContentRestart` covers either
/// content-snapshot key, `QueryRestart` query/options, and `ForgedRestart` forged, foreign,
/// dropped, or otherwise stale tokens. Construction/exhaustion errors return no step and are
/// covered by the owner's fail-closed unit tests.
///
/// `Buggy = 1` is stale continuation: restart actions preserve the old identity/progress
/// instead of minting/resetting (`ResetMintsFresh`, `ResetStartsAtBeginning`). Two more slips,
/// each applied at every step it touches:
/// * `cursor: Some(state.cursor)` without its `(!complete).then_some` gate, so every completing
///   turn (`StartComplete`, `FinishScan`, `DrainComplete`, `RestartComplete`) hands back a live
///   token for a completed stream (`LifecycleShape`);
/// * a resume that allocates a fresh token each turn (`state.cursor = allocate_cursor(..)` on
///   the resume path), rotating cursor and `search_id` together without `reset`, so a host
///   keyed on `search_id` discards live results as a superseded snapshot
///   (`IdentityStableWithoutReset`).
///
/// A third slip is an alternative to `StartBacklog`, so it is its own action (a stutter at
/// `Buggy=0`) and the healthy backlog stays reachable beside it: `BuggyScanOnlyCompletion` is
/// the dense scan turn under `complete = engine.is_complete()` without the
/// `&& emitted_matches >= result_count` term a bounded delta needs, so the first delta comes
/// back complete and cursorless and the host never sees the rest (`DeliveryShape`).
///
/// Tier-1: `aterm-core/tests/conformance_budgeted_search.rs` replays all three on real steps
/// (`delivery` is read from the host's delivery ledger against the snapshot's real match
/// count, not from `complete`), with transition negative controls.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn budgeted_search_resume_model() -> Model {
    aterm_spec::ty_model! {
        BudgetedSearchResume {
            const Rows = 3;          // real bounded Terminal row window in Tier-1
            const DeliveryTurns = 3; // scan delta + repeated drain + final drain
            const MaxTokens = 10;    // bounded fresh-start/restart trace budget
            const Buggy = 0;         // 1 = the stale-continuation family documented above

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
                stream_id = issued + 1;
            }

            // A sparse fresh search may scan and deliver all rows in one call.
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
                stream_id = issued + 1;
            }

            // A valid unit-budget resume preserves stream identity and advances one
            // non-final scan row. The final sparse scan turn is separate so its
            // cursor retirement is explicit.
            action Resume when (live == 1 && scan_done == 0 && progress <= Rows - 2) {
                progress = progress + 1;
                // Buggy mints a fresh token per turn: cursor and search_id move
                // together, as the one field they are, and `reset` stays off.
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

            // A stale/superseding call may scan and deliver its fresh snapshot in
            // one turn. There is no cursor, but reset + fresh search_id remain
            // observable so hosts discard the superseded stream before appending.
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

            // The dense scan turn with completion read from the scan alone: the
            // same step as StartBacklog, except that it reports complete, hands
            // back no cursor, and so strands the undelivered backlog. A stutter
            // at `Buggy=0`, so no action is dead at the committed config (the
            // strict-vacuity rule for negative controls).
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
                live = if Buggy == 1 { 1 } else { 0 };
                complete = 1;
                cursor = if Buggy == 1 { cursor } else { 0 };
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

/// Host-minted hyperlink scheme capability: the abstract twin of `HyperlinkAuth`'s
/// extra-scheme allowlist (#4384). The host may mint a small set of extra OSC-8 URI schemes
/// (e.g. `orca`) on top of the hardcoded safe allowlist:
///
///   * the extra set is bounded (`MAX_EXTRA_SCHEMES` in the code, `Cap` here):
///     `Authorize`/`AuthorizeOther` grow it only under the bound, and `RefuseAtCap` is the
///     at-capacity refusal (the API returns `false`, state unchanged);
///   * a never-allow scheme (`javascript`/`data`/`file`/…) is refused even when the host
///     asks: `RefuseNeverAllow` is a no-op at `Buggy=0`, and `Buggy=1` models the admission
///     defect the invariant must catch;
///   * `Revoke` removes the distinguished scheme, restoring the default allowlist: `Accept`
///     (the OSC-8 gate saying yes to an extra-scheme URI) is enabled iff the scheme is
///     currently minted, which Tier-1 binds to the real `is_allowed_scheme` decision.
///
/// `orca` is the distinguished tracked scheme (0/1); `others` counts the rest of the extra set
/// so the bound is proven over the whole set, not one entry. Proves `Bounded` +
/// `NeverAllowRefused` at `Buggy=0`; at `Buggy=1` the unbounded grow and the never-allow
/// admission each yield a counterexample.
#[cfg_attr(trust_verify, trust::skip)]
pub fn hyperlink_scheme_cap_model() -> Model {
    aterm_spec::ty_model! {
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

/// Module-wide scrollback budget sharing: N panes in one memory space each apply
/// `min(configured, global / live)` at their own touch points, so the applied budgets sum within
/// the one global cap at every quiescent point and panes cannot multiply the per-pane budget into
/// an OOM. Two panes bound the membership lattice (join/leave/apply in all orders); `fresh*`
/// tracks "applied since the last membership change", so the invariant is exact at quiescence
/// and waived while a share is stale (for one touch). `Buggy=1` drops the global: each pane
/// applies its full configured budget, which two fresh live panes overrun.
///
/// Proves `QuiescentSumBounded` and `DepartedHoldsNothing`. Tier-1:
/// `aterm-core/tests/conformance_shared_budget.rs` drives the real `ScrollbackBudgetShare`
/// registry (and real `Terminal` eviction) in lockstep.
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn shared_budget_model() -> Model {
    aterm_spec::ty_model! {
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
                a2 = if Buggy == 2 { a2 } else { 0 };
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

            // The global contract: once every live pane has applied its current share, applied
            // budgets sum within the one global cap.
            invariant QuiescentSumBounded:
                if fresh1 == 0 {
                    0 <= 1
                } else if live2 == 1 && fresh2 == 0 {
                    0 <= 1
                } else {
                    a1 + a2 <= Global
                };
            // A departed pane holds no share.
            // (Dial 2 keeps the departed pane's share.)
            invariant DepartedHoldsNothing:
                if live2 == 0 { a2 == 0 } else { 0 <= 1 };
        }
    }
}

/// `aterm.log` rotation shared by several writing processes — the GUI, every
/// `aterm --session`, an update's successor (`aterm-gui/src/logging.rs`,
/// `RotatingFile`).
///
/// Each writer holds the file open with `O_APPEND`. Before a write it looks at
/// the path again once `Step` lines (64 KiB) or one `Tick` (30 s) have passed
/// since its last look: if the path names another file it reopens it (another
/// writer rotated), and if the live file is over `Cap` it renames it to
/// `aterm.log.1`, replacing the older copy, and starts a fresh one. That
/// rename runs under a sibling lock taken without waiting and looks at the
/// file again under it, which is what lets the model treat a look as one step.
/// A process that starts (`RestartB`) rotates an oversized file the same way.
///
/// Writer handles: 0 = `aterm.log`, 1 = `aterm.log.1`, 2 = a deleted older
/// copy (a line written there is gone). `aged` counts lines retired with an
/// older copy — the retention limit, not a loss. `ea`/`eb`/`er` are ticks since
/// each writer's last look and since the last rotation, saturating at `Window`.
///
/// Environment assumption: `aterm.log` fills more slowly than one whole file per check interval
/// (4 MiB in 30 s), stated as the guard on a write into it: `live <= Cap - 1 || Window <= er`.
/// Under it no writer falls two files behind before its time check comes due, so no line lands
/// in a deleted file.
///
/// Proves `EveryLineOnce` (every line written is in `aterm.log`, in `aterm.log.1`, or was
/// retired with an older copy: none lost, none duplicated) and `FilesBounded` (each file stays
/// within the cap plus the writers' slack between looks). `Buggy=1`: no look while running, so
/// the file grows without limit, and a start that truncates an oversized file, erasing lines
/// other writers put there. Tier-1: `aterm-gui/src/logging.rs` `rotation_conformance` drives
/// two real writers on one path through rotation, a follow and a restart.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn log_rotation_model() -> Model {
    aterm_spec::ty_model! {
        LogRotation {
            const Cap = 1;       // lines one file may hold before rotation
            const Step = 1;      // lines a writer adds between looks
            const Window = 1;    // ticks in one check interval
            const MaxLines = 4;  // run bound
            const Buggy = 0;     // 1 = no look while running, truncate at start

            var live = 0;     // lines in aterm.log
            var old = 0;      // lines in aterm.log.1
            var aged = 0;     // lines retired with a replaced older copy
            var written = 0;  // lines logged by any writer
            var ga = 0;       // writer A's handle (0 live, 1 older copy, 2 deleted)
            var gb = 0;       // writer B's handle
            var na = 0;       // lines A wrote since its last look
            var nb = 0;
            var ea = 0;       // ticks since A's last look
            var eb = 0;
            var er = 1;       // ticks since the last rotation (none yet: saturated)

            // Time passes.
            action Tick when (ea <= Window - 1 || eb <= Window - 1 || er <= Window - 1) {
                ea = if ea <= Window - 1 { ea + 1 } else { ea };
                eb = if eb <= Window - 1 { eb + 1 } else { eb };
                er = if er <= Window - 1 { er + 1 } else { er };
            }

            // One line into the file A holds, while no look is due.
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

            // A's look: follow another writer's rotation, then rotate the
            // live file if it is oversized — the same look does both.
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

            // B's process exits and a new one opens the log: an oversized
            // file is rotated (Buggy: truncated).
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
