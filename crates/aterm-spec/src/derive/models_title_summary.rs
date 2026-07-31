// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The smart terminal-title summarization family — spec-model
//! data constructors moved verbatim out of the one-file catalog in `derive.rs`
//! (pure code motion; every constructor keeps its `crate::derive` path via the
//! `pub use` re-exports there).

use super::*;

/// Smart-title summaries are single-flight and stamped with the latest request,
/// semantic-content authority, and settings generations. Every semantic boundary
/// advances request and semantic generations, including `Boundary`, where minimum-
/// interval throttling admits no replacement request. An admitted semantic
/// `Request` additionally captures pending work. A periodic `Refresh` advances only
/// the request generation: it supersedes older inference work while retaining a
/// label that is still authorized by the same semantic/settings epochs. The request
/// generation is the shipping completion capability; advancing it at every semantic
/// boundary is what makes the implicit captured semantic stamp unpublishable. This
/// is the single-session projection of one
/// capacity-one per-session slot; [`title_summary_runtime_model`] composes two
/// such slots under the shipping round-robin worker. Reconfiguration or disable
/// destroys captured work. Starting the
/// worker is an authority boundary and may move a snapshot out of that slot only
/// while all stamps still match and the feature is enabled. Completion has the
/// same freshness gate before publishing a description. Disabling advances the
/// configuration stamp, so neither a queued snapshot nor a running result from
/// before disable can become live after a later re-enable.
///
/// `Buggy=1` models both missing revocation cleanup/dispatch validation and the
/// unsafe callback that publishes every completed job. The derived checker thus
/// proves the send and publication gates and catches a captured snapshot crossing
/// A -> Off -> A, stale-generation/configuration publication, and disabled-result
/// publication.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn title_summary_model() -> Model {
    crate::ty_model! {
        TitleSummary {
            const Buggy = 0;
            const MaxGeneration = 3;
            const MaxConfig = 3;

            var enabled = 1;
            var retired = 0;
            // A failed nonblocking terminal observation owns one delayed retry.
            // Further contention keeps that same retry armed; it never counts up.
            var retry_pending = 0;
            var config_generation = 1;
            var current_generation = 0;
            var semantic_generation = 0;

            // One session's capacity-one pending snapshot and worker lanes. Zero is the
            // empty/sentinel generation. Pending owns captured terminal text;
            // inflight abstracts a request after that text has been sent.
            var pending = 0;
            var pending_generation = 0;
            var pending_semantic = 0;
            var pending_config = 0;
            var inflight = 0;
            var job_generation = 0;
            var job_semantic = 0;
            var job_config = 0;
            var unauthorized_snapshot_sent = 0;

            // The optional model-produced description currently on screen.
            // A semantic/configuration change clears it immediately; a timer
            // refresh preserves it. The deterministic lane is outside this model.
            var applied_generation = 0;
            var applied_semantic = 0;
            var applied_config = 0;
            var stale_applied = 0;

            action Request when (
                enabled == 1 && current_generation <= MaxGeneration - 1
            ) {
                current_generation = current_generation + 1;
                semantic_generation = semantic_generation + 1;
                pending = 1;
                pending_generation = current_generation + 1;
                pending_semantic = semantic_generation + 1;
                pending_config = config_generation;
                applied_generation = 0;
                applied_semantic = 0;
                applied_config = 0;
            }

            // A real semantic boundary inside the minimum interval cannot capture a
            // replacement yet. It still revokes both a queued snapshot and a running
            // completion. The mutant preserves the old generation/pending capability.
            action Boundary when (
                enabled == 1 && retired == 0 &&
                current_generation <= MaxGeneration - 1 &&
                semantic_generation <= MaxGeneration - 1
            ) {
                current_generation =
                    if Buggy == 1 { current_generation } else { current_generation + 1 };
                semantic_generation = semantic_generation + 1;
                pending = if Buggy == 1 { pending } else { 0 };
                pending_generation =
                    if Buggy == 1 { pending_generation } else { 0 };
                pending_semantic = if Buggy == 1 { pending_semantic } else { 0 };
                pending_config = if Buggy == 1 { pending_config } else { 0 };
                applied_generation = 0;
                applied_semantic = 0;
                applied_config = 0;
            }

            action Refresh when (
                enabled == 1 && retired == 0 && semantic_generation > 0 &&
                current_generation <= MaxGeneration - 1
            ) {
                current_generation = current_generation + 1;
                pending = 1;
                pending_generation = current_generation + 1;
                pending_semantic = semantic_generation;
                pending_config = config_generation;
            }

            action Start when (
                enabled == 1 && inflight == 0 && pending == 1 &&
                pending_generation == current_generation &&
                pending_semantic == semantic_generation &&
                pending_config <= config_generation &&
                config_generation <= if Buggy == 1 { MaxConfig } else { pending_config }
            ) {
                pending = 0;
                inflight = 1;
                job_generation = pending_generation;
                job_semantic = pending_semantic;
                job_config = pending_config;
                unauthorized_snapshot_sent =
                    if pending_config == config_generation {
                        unauthorized_snapshot_sent
                    } else {
                        1
                };
                pending_generation = 0;
                pending_semantic = 0;
                pending_config = 0;
            }

            action Reconfigure when (config_generation <= MaxConfig - 1) {
                config_generation = config_generation + 1;
                pending = if Buggy == 1 { pending } else { 0 };
                pending_generation =
                    if Buggy == 1 { pending_generation } else { 0 };
                pending_semantic = if Buggy == 1 { pending_semantic } else { 0 };
                pending_config = if Buggy == 1 { pending_config } else { 0 };
                applied_generation = 0;
                applied_semantic = 0;
                applied_config = 0;
            }

            action Disable when (
                enabled == 1 && config_generation <= MaxConfig - 1
            ) {
                enabled = 0;
                config_generation = config_generation + 1;
                pending = if Buggy == 1 { pending } else { 0 };
                pending_generation =
                    if Buggy == 1 { pending_generation } else { 0 };
                pending_semantic = if Buggy == 1 { pending_semantic } else { 0 };
                pending_config = if Buggy == 1 { pending_config } else { 0 };
                applied_generation = 0;
                applied_semantic = 0;
                applied_config = 0;
                retry_pending = 0;
            }

            action Enable when (enabled == 0 && retired == 0) {
                enabled = 1;
            }

            action LockContended when (
                enabled == 1 && retired == 0 && retry_pending == 0
            ) {
                retry_pending = 1;
            }

            action RetryContended when (
                enabled == 1 && retired == 0 && retry_pending == 1
            ) {
                retry_pending = 1;
            }

            action ObserveSuccess when (
                enabled == 1 && retired == 0 && retry_pending == 1
            ) {
                retry_pending = 0;
            }

            action Retire when (retired == 0) {
                retired = 1;
                enabled = 0;
                retry_pending = 0;
                pending = 0;
                pending_generation = 0;
                pending_semantic = 0;
                pending_config = 0;
                applied_generation = 0;
                applied_semantic = 0;
                applied_config = 0;
            }

            action Complete when (inflight == 1) {
                inflight = 0;
                applied_generation = if (
                    enabled == 1 &&
                    job_generation == current_generation &&
                    job_config == config_generation
                ) {
                    job_generation
                } else {
                    if Buggy == 1 { job_generation } else { applied_generation }
                };
                applied_semantic = if (
                    enabled == 1 &&
                    job_generation == current_generation &&
                    job_config == config_generation
                ) {
                    job_semantic
                } else {
                    if Buggy == 1 { job_semantic } else { applied_semantic }
                };
                applied_config = if (
                    enabled == 1 &&
                    job_generation == current_generation &&
                    job_config == config_generation
                ) {
                    job_config
                } else {
                    if Buggy == 1 { job_config } else { applied_config }
                };
                stale_applied = if Buggy == 1 {
                    if (
                        enabled == 1 &&
                        job_generation == current_generation &&
                        job_config == config_generation
                    ) {
                        stale_applied
                    } else {
                        1
                    }
                } else {
                    stale_applied
                };
                job_generation = 0;
                job_semantic = 0;
                job_config = 0;
            }

            invariant StaleCompletionNeverApplies: stale_applied == 0;
            invariant ObservationRetryIsBoolean: retry_pending <= 1;
            invariant DisabledHasNoObservationRetry:
                if enabled == 0 { retry_pending == 0 } else { retry_pending <= 1 };
            invariant RetiredObservationIsQuiescent:
                if retired == 1 {
                    enabled == 0 && retry_pending == 0 && pending == 0
                } else {
                    retired == 0
                };
            invariant SnapshotNeverCrossesRevocation:
                unauthorized_snapshot_sent == 0;
            invariant PendingSnapshotHasCurrentAuthority:
                if pending == 1 {
                    enabled == 1 &&
                    pending_generation == current_generation &&
                    pending_semantic == semantic_generation &&
                    pending_config == config_generation
                } else {
                    pending_generation == 0 &&
                    pending_semantic == 0 && pending_config == 0
                };
            invariant AppliedResultIsCurrent:
                if applied_generation > 0 {
                    enabled == 1 &&
                    applied_generation <= current_generation &&
                    applied_semantic == semantic_generation &&
                    applied_config == config_generation
                } else {
                    applied_semantic == 0 && applied_config == 0
                };
            invariant DisabledCompletionIsInert:
                if enabled == 0 {
                    applied_generation == 0 && applied_config == 0
                } else {
                    applied_generation <= current_generation
                };
            invariant WorkerLaneHasOneStampedJob:
                if inflight == 1 {
                    job_generation > 0 &&
                    job_generation <= current_generation &&
                    job_semantic > 0 &&
                    job_semantic <= semantic_generation &&
                    job_config <= config_generation
                } else {
                    job_generation == 0 && job_semantic == 0 && job_config == 0
                };
            invariant GenerationsBounded:
                current_generation <= MaxGeneration &&
                semantic_generation <= MaxGeneration &&
                config_generation <= MaxConfig && retired <= 1;
        }
    }
}

/// Event-loop observation admission for Smart Titles. A due batch starts with the
/// active session, then preserves its deterministic round-robin remainder. Each
/// event-loop turn admits exactly one terminal snapshot, bounding aggregate
/// scrollback copying and parser-lock residency. The three-session cycle is the
/// smallest bound that distinguishes active-first priority from remainder fairness.
///
/// `Buggy=1` models the former bulk drain plus repeated active preference: two
/// observations occur in one turn and the non-active sessions wait without bound.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn title_summary_observation_scheduler_model() -> Model {
    crate::ty_model! {
        TitleSummaryObservationScheduler {
            const Buggy = 0;
            const MaxTurns = 3;

            var turns = 0;
            // Correct queue order for active session 2 and sorted remainder 1,3.
            var queue_turn = 0;
            var chosen = 0;
            var first_chosen = 0;
            var observations_this_turn = 0;
            var wait1 = 0;
            var wait2 = 0;
            var wait3 = 0;

            // Worst-case worker queue projection: priority and background work
            // remain continuously pending. Correct code alternates one priority
            // slot with a mandatory background handoff.
            var worker_dispatches = 0;
            var worker_chosen = 0;
            var last_worker_was_priority = 0;
            var background_wait = 0;

            action ObserveTurn when (turns <= MaxTurns - 1) {
                chosen = if Buggy == 1 {
                    2
                } else {
                    if queue_turn == 0 { 2 } else {
                        if queue_turn == 1 { 1 } else { 3 }
                    }
                };
                first_chosen = if turns == 0 { 2 } else { first_chosen };
                observations_this_turn = if Buggy == 1 { 2 } else { 1 };
                wait1 = if (
                    if Buggy == 1 { 2 } else {
                        if queue_turn == 0 { 2 } else {
                            if queue_turn == 1 { 1 } else { 3 }
                        }
                    }
                ) == 1 { 0 } else { wait1 + 1 };
                wait2 = if (
                    if Buggy == 1 { 2 } else {
                        if queue_turn == 0 { 2 } else {
                            if queue_turn == 1 { 1 } else { 3 }
                        }
                    }
                ) == 2 { 0 } else { wait2 + 1 };
                wait3 = if (
                    if Buggy == 1 { 2 } else {
                        if queue_turn == 0 { 2 } else {
                            if queue_turn == 1 { 1 } else { 3 }
                        }
                    }
                ) == 3 { 0 } else { wait3 + 1 };
                queue_turn = if Buggy == 1 {
                    0
                } else {
                    if queue_turn == 2 { 0 } else { queue_turn + 1 }
                };
                turns = turns + 1;
            }

            action DispatchWorker when (worker_dispatches <= MaxTurns - 1) {
                worker_chosen = if Buggy == 1 {
                    1
                } else {
                    if last_worker_was_priority == 0 { 1 } else { 2 }
                };
                last_worker_was_priority = if Buggy == 1 {
                    1
                } else {
                    if last_worker_was_priority == 0 { 1 } else { 0 }
                };
                background_wait = if (
                    if Buggy == 1 { 1 } else {
                        if last_worker_was_priority == 0 { 1 } else { 2 }
                    }
                ) == 2 { 0 } else { background_wait + 1 };
                worker_dispatches = worker_dispatches + 1;
            }

            invariant OneObservationPerTurn: observations_this_turn <= 1;
            invariant ActiveSessionStartsBatch:
                if turns > 0 { first_chosen == 2 } else { first_chosen == 0 };
            invariant PreservedRemainderIsFair:
                wait1 <= 2 && wait2 <= 2 && wait3 <= 2;
            invariant SelectedSessionIsValid: chosen <= 3;
            invariant PriorityCannotStarveBackground: background_wait <= 1;
            invariant WorkerSelectionIsValid: worker_chosen <= 2;
            invariant Bounds:
                turns <= MaxTurns && queue_turn <= 2 && observations_this_turn <= 2 &&
                wait1 <= 3 && wait2 <= 3 && wait3 <= 3 &&
                worker_dispatches <= MaxTurns && last_worker_was_priority <= 1 &&
                background_wait <= 3;
        }
    }
}

/// Two-session scheduler/lifecycle companion for [`title_summary_model`]. The
/// content/publication model above keeps the detailed generation projection used by
/// GUI Tier-1 tests; this model proves the process-wide properties that require more
/// than one live session: one coalesced slot per session, round-robin service,
/// retirement cancellation before I/O and publication, strict timer spacing, and an
/// owned worker/runtime lifecycle.
///
/// `Buggy=1` combines the historical failure modes: semantic boundaries can bypass
/// the minimum interval, the scheduler always favors session 1, and a retired job may
/// begin I/O or publish. The prove-and-catch gate therefore also establishes that the
/// fairness, rate, and cancellation invariants are non-vacuous.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn title_summary_runtime_model() -> Model {
    crate::ty_model! {
        TitleSummaryRuntime {
            const Buggy = 0;
            const MaxTime = 4;
            const MinInterval = 2;
            const MaxRequests = 3;

            var now = 0;
            // Worker 0=stopped, 1=running. An owned local runtime may exist only
            // while that worker is running.
            var worker = 0;
            var managed_runtime = 0;

            var live1 = 1;
            var live2 = 1;
            var epoch1 = 1;
            var epoch2 = 1;
            var dirty1 = 1;
            var dirty2 = 1;
            var pending1 = 0;
            var pending2 = 0;
            var next_allowed1 = 0;
            var next_allowed2 = 0;
            var requests1 = 0;
            var requests2 = 0;

            // turn=1/2 is the next session selected when both slots are full.
            // waitN counts how many other jobs started while N remained pending.
            var turn = 1;
            var wait1 = 0;
            var wait2 = 0;

            // phase: 0 idle, 1 dequeued, 2 DNS/connect/TLS complete but before
            // payload transmission, 3 payload transmitted.
            var phase = 0;
            var job_session = 0;
            var job_epoch = 0;
            var unauthorized_io = 0;
            var unauthorized_transmit = 0;
            var stale_publish = 0;
            var rate_violation = 0;

            action StartWorker when (worker == 0 && phase == 0) {
                worker = 1;
                managed_runtime = 1;
            }

            action StopWorker when (worker == 1 && phase == 0) {
                worker = 0;
                managed_runtime = 0;
            }

            action Tick when (now <= MaxTime - 1) {
                now = now + 1;
            }

            action Observe1 when (live1 == 1) {
                dirty1 = 1;
            }

            action Observe2 when (live2 == 1) {
                dirty2 = 1;
            }

            action Queue1 when (
                worker == 1 && live1 == 1 && dirty1 == 1 &&
                requests1 <= MaxRequests - 1 && now <= MaxTime - MinInterval &&
                (next_allowed1 <= now || Buggy == 1)
            ) {
                pending1 = 1;
                dirty1 = 0;
                rate_violation = if next_allowed1 <= now {
                    rate_violation
                } else {
                    1
                };
                next_allowed1 = now + MinInterval;
                requests1 = requests1 + 1;
            }

            action Queue2 when (
                worker == 1 && live2 == 1 && dirty2 == 1 &&
                requests2 <= MaxRequests - 1 && now <= MaxTime - MinInterval &&
                (next_allowed2 <= now || Buggy == 1)
            ) {
                pending2 = 1;
                dirty2 = 0;
                rate_violation = if next_allowed2 <= now {
                    rate_violation
                } else {
                    1
                };
                next_allowed2 = now + MinInterval;
                requests2 = requests2 + 1;
            }

            action Start when (
                worker == 1 && phase == 0 && (pending1 == 1 || pending2 == 1)
            ) {
                phase = 1;
                job_session = if (
                    pending1 == 1 &&
                    (pending2 == 0 || Buggy == 1 || turn == 1)
                ) { 1 } else { 2 };
                job_epoch = if (
                    pending1 == 1 &&
                    (pending2 == 0 || Buggy == 1 || turn == 1)
                ) { epoch1 } else { epoch2 };
                pending1 = if (
                    pending1 == 1 &&
                    (pending2 == 0 || Buggy == 1 || turn == 1)
                ) { 0 } else { pending1 };
                pending2 = if (
                    pending1 == 1 &&
                    (pending2 == 0 || Buggy == 1 || turn == 1)
                ) { pending2 } else { 0 };
                wait1 = if (
                    pending1 == 1 &&
                    (pending2 == 0 || Buggy == 1 || turn == 1)
                ) { 0 } else { if pending1 == 1 { wait1 + 1 } else { wait1 } };
                wait2 = if (
                    pending1 == 1 &&
                    (pending2 == 0 || Buggy == 1 || turn == 1)
                ) { if pending2 == 1 { wait2 + 1 } else { wait2 } } else { 0 };
                turn = if (
                    pending1 == 1 &&
                    (pending2 == 0 || Buggy == 1 || turn == 1)
                ) { 2 } else { 1 };
            }

            action BeginIo when (
                phase == 1 && (
                    Buggy == 1 ||
                    (job_session == 1 && live1 == 1 && job_epoch == epoch1) ||
                    (job_session == 2 && live2 == 1 && job_epoch == epoch2)
                )
            ) {
                phase = 2;
                unauthorized_io = if (
                    (job_session == 1 && live1 == 1 && job_epoch == epoch1) ||
                    (job_session == 2 && live2 == 1 && job_epoch == epoch2)
                ) { unauthorized_io } else { 1 };
            }

            action Transmit when (
                phase == 2 && (
                    Buggy == 1 ||
                    (job_session == 1 && live1 == 1 && job_epoch == epoch1) ||
                    (job_session == 2 && live2 == 1 && job_epoch == epoch2)
                )
            ) {
                phase = 3;
                unauthorized_transmit = if (
                    (job_session == 1 && live1 == 1 && job_epoch == epoch1) ||
                    (job_session == 2 && live2 == 1 && job_epoch == epoch2)
                ) { unauthorized_transmit } else { 1 };
            }

            action Cancel when (
                (phase == 1 || phase == 2) && (
                    (job_session == 1 && (live1 == 0 || epoch1 > job_epoch)) ||
                    (job_session == 2 && (live2 == 0 || epoch2 > job_epoch))
                )
            ) {
                phase = 0;
                job_session = 0;
                job_epoch = 0;
            }

            action Complete when (phase == 3) {
                stale_publish = if (
                    (job_session == 1 && live1 == 1 && job_epoch == epoch1) ||
                    (job_session == 2 && live2 == 1 && job_epoch == epoch2)
                ) {
                    stale_publish
                } else {
                    if Buggy == 1 { 1 } else { stale_publish }
                };
                phase = 0;
                job_session = 0;
                job_epoch = 0;
            }

            action Retire1 when (live1 == 1) {
                live1 = 0;
                epoch1 = epoch1 + 1;
                dirty1 = 0;
                pending1 = 0;
                wait1 = 0;
            }

            action Retire2 when (live2 == 1) {
                live2 = 0;
                epoch2 = epoch2 + 1;
                dirty2 = 0;
                pending2 = 0;
                wait2 = 0;
            }

            invariant PerSessionSlotsAreBounded:
                pending1 <= 1 && pending2 <= 1;
            invariant RetiredSessionsAreQuiescent:
                (if live1 == 0 { dirty1 == 0 && pending1 == 0 } else { live1 == 1 }) &&
                (if live2 == 0 { dirty2 == 0 && pending2 == 0 } else { live2 == 1 });
            invariant NoIoAfterRetirement: unauthorized_io == 0;
            invariant NoTransmitAfterRetirement: unauthorized_transmit == 0;
            invariant NoPublishAfterRetirement: stale_publish == 0;
            invariant StrictMinimumInterval: rate_violation == 0;
            invariant RoundRobinWaitIsBounded: wait1 <= 1 && wait2 <= 1;
            invariant ManagedRuntimeHasWorker:
                if worker == 0 { managed_runtime == 0 && phase == 0 } else { worker == 1 };
            invariant JobHasLiveShape:
                if phase > 0 {
                    (job_session == 1 || job_session == 2) && job_epoch > 0
                } else {
                    job_session == 0 && job_epoch == 0
                };
            invariant Bounds:
                now <= MaxTime && requests1 <= MaxRequests && requests2 <= MaxRequests &&
                live1 <= 1 && live2 <= 1 && worker <= 1 && managed_runtime <= 1 && phase <= 3;
        }
    }
}

/// Two independent aterm processes automatically own distinct ephemeral Ollama
/// endpoints. The selected endpoint belongs to the exact process/configuration
/// authority, is reused without falling back to the configured default, and is
/// cleared together with runtime health on reconfiguration or shutdown. A stale
/// completion may never republish an endpoint after revocation.
///
/// Ports are abstracted to the bounded values 1 and 2; value 3 represents the
/// historical shared default. `Buggy=1` makes process 2 collide with process 1 and
/// preserves/publishes stale health across revocation, providing non-vacuous
/// controls for both ownership and telemetry obligations.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn title_summary_managed_endpoint_model() -> Model {
    crate::ty_model! {
        TitleSummaryManagedEndpoint {
            const Buggy = 0;
            const MaxAuthority = 3;

            var authority1 = 1;
            var authority2 = 1;
            var process1 = 0;
            var process2 = 0;
            var endpoint1 = 0;
            var endpoint2 = 0;
            var health_endpoint1 = 0;
            var health_endpoint2 = 0;
            var reused1 = 0;
            var reused2 = 0;

            action Launch1 when (process1 == 0) {
                process1 = 1;
                endpoint1 = 1;
            }

            action Launch2 when (process2 == 0) {
                process2 = 1;
                endpoint2 = if Buggy == 1 { 1 } else { 2 };
            }

            action Reuse1 when (process1 == 1 && endpoint1 > 0) {
                reused1 = 1;
                health_endpoint1 = endpoint1;
            }

            action Reuse2 when (process2 == 1 && endpoint2 > 0) {
                reused2 = 1;
                health_endpoint2 = endpoint2;
            }

            action Reconfigure1 when (authority1 <= MaxAuthority - 1) {
                authority1 = authority1 + 1;
                process1 = 0;
                endpoint1 = 0;
                health_endpoint1 = if Buggy == 1 { health_endpoint1 } else { 0 };
                reused1 = 0;
            }

            action Reconfigure2 when (authority2 <= MaxAuthority - 1) {
                authority2 = authority2 + 1;
                process2 = 0;
                endpoint2 = 0;
                health_endpoint2 = if Buggy == 1 { health_endpoint2 } else { 0 };
                reused2 = 0;
            }

            action Crash1 when (process1 == 1) {
                process1 = 0;
                endpoint1 = 0;
                health_endpoint1 = if Buggy == 1 { health_endpoint1 } else { 0 };
                reused1 = 0;
            }

            action Crash2 when (process2 == 1) {
                process2 = 0;
                endpoint2 = 0;
                health_endpoint2 = if Buggy == 1 { health_endpoint2 } else { 0 };
                reused2 = 0;
            }

            action StaleResult1 when (authority1 > 1 && process1 == 0) {
                health_endpoint1 = if Buggy == 1 { 1 } else { health_endpoint1 };
            }

            action StaleResult2 when (authority2 > 1 && process2 == 0) {
                health_endpoint2 = if Buggy == 1 { 2 } else { health_endpoint2 };
            }

            action Shutdown1 when (process1 == 1) {
                process1 = 0;
                endpoint1 = 0;
                health_endpoint1 = 0;
                reused1 = 0;
            }

            action Shutdown2 when (process2 == 1) {
                process2 = 0;
                endpoint2 = 0;
                health_endpoint2 = 0;
                reused2 = 0;
            }

            invariant EndpointBelongsToOwnedProcess:
                (if process1 == 0 { endpoint1 == 0 } else { endpoint1 > 0 }) &&
                (if process2 == 0 { endpoint2 == 0 } else { endpoint2 > 0 });
            invariant ConcurrentAutomaticEndpointsAreDistinct:
                if process1 == 1 && process2 == 1 {
                    endpoint1 + endpoint2 == 3
                } else {
                    process1 <= 1 && process2 <= 1
                };
            invariant AutomaticEndpointNeverUsesSharedDefault:
                endpoint1 <= 2 && endpoint2 <= 2;
            invariant RevokedHealthIsClear:
                health_endpoint1 <= endpoint1 && health_endpoint2 <= endpoint2;
            invariant ReuseRetainsOwnedEndpoint:
                (if reused1 == 1 { process1 == 1 && health_endpoint1 == endpoint1 } else { reused1 == 0 }) &&
                (if reused2 == 1 { process2 == 1 && health_endpoint2 == endpoint2 } else { reused2 == 0 });
            invariant Bounds:
                authority1 <= MaxAuthority && authority2 <= MaxAuthority &&
                process1 <= 1 && process2 <= 1 && endpoint1 <= 3 && endpoint2 <= 3 &&
                health_endpoint1 <= 3 && health_endpoint2 <= 3 &&
                reused1 <= 1 && reused2 <= 1;
        }
    }
}

/// The macOS managed-runtime attestor accepts only one exact established socket
/// owner. A missing owner or a temporarily ambiguous owner is a transient `lsof`
/// observation and therefore retains the retry capability. A unique owner
/// succeeds, while malformed/structurally oversized output and permanent command
/// errors fail closed. Transient retries consume a finite budget before one
/// explicit timeout transition, so the inspection loop cannot remain live
/// forever.
///
/// `phase` is 0 for the initial inspection, 1 for a retained retry, 2 for unique
/// success, and 3 for terminal failure. `observation` is 0 initially, 1 missing,
/// 2 ambiguous, 3 unique, 4 structural error, 5 permanent error, and 6 timeout.
/// `Buggy=1` models the regression where an ambiguous snapshot is treated as a
/// permanent failure instead of retrying; `TransientObservationsRetry` catches it.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn title_summary_socket_owner_retry_model() -> Model {
    crate::ty_model! {
        TitleSummarySocketOwnerRetry {
            const Buggy = 0;
            const MaxRetries = 3;

            var phase = 0;
            var observation = 0;
            var retries = 0;
            var timed_out = 0;

            action ObserveMissing when (
                phase <= 1 && retries <= MaxRetries - 1
            ) {
                phase = 1;
                observation = 1;
                retries = retries + 1;
            }

            action ObserveAmbiguous when (
                phase <= 1 && retries <= MaxRetries - 1
            ) {
                phase = if Buggy == 1 { 3 } else { 1 };
                observation = 2;
                retries = retries + 1;
            }

            action ObserveUnique when (phase <= 1) {
                phase = 2;
                observation = 3;
            }

            action ObserveStructuralError when (phase <= 1) {
                phase = 3;
                observation = 4;
            }

            action ObservePermanentError when (phase <= 1) {
                phase = 3;
                observation = 5;
            }

            action Timeout when (phase == 1 && retries == MaxRetries) {
                phase = 3;
                observation = 6;
                timed_out = 1;
            }

            invariant TransientObservationsRetry:
                if observation == 1 || observation == 2 {
                    phase == 1 && timed_out == 0
                } else {
                    phase <= 3
                };
            invariant UniqueObservationSucceeds:
                if observation == 3 {
                    phase == 2 && timed_out == 0
                } else {
                    phase <= 3
                };
            invariant PermanentErrorsFailClosed:
                if observation == 4 || observation == 5 {
                    phase == 3 && timed_out == 0
                } else {
                    phase <= 3
                };
            invariant TimeoutConsumesTheBound:
                if timed_out == 1 {
                    phase == 3 && observation == 6 && retries == MaxRetries
                } else {
                    observation <= 5
                };
            invariant RetryBudgetIsBounded: retries <= MaxRetries;
            invariant Bounds:
                phase <= 3 && observation <= 6 && timed_out <= 1;
        }
    }
}
