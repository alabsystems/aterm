// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The process-global native updater (`NativeUpdater`).

use aterm_spec::derive::*;

/// The process-global updater is single-flight and generation-stamped.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn native_updater_model() -> Model {
    aterm_spec::ty_model! {
        NativeUpdater {
            const Buggy = 0;
            const MaxGeneration = 3;
            var phase = 0;
            var request_generation = 0;
            var work_generation = 0;
            var artifact_generation = 0;
            var active_work = 0;
            var stale_completion_pending = 0;
            var verified = 0;
            var close_preflight = 0;
            var reexec_count = 0;
            var stale_staged = 0;
            action StartCheck when (
                phase == 0 && request_generation <= MaxGeneration - 1
            ) {
                phase = 1;
                request_generation = request_generation + 1;
                active_work = 1;
            }
            action RetryCheck when (
                phase == 6 && request_generation <= MaxGeneration - 1
            ) {
                phase = 1;
                request_generation = request_generation + 1;
                active_work = 1;
            }
            action CheckAvailable when (phase == 1 && active_work == 1) {
                phase = 2;
                active_work = 0;
            }
            action CheckUpToDate when (phase == 1 && active_work == 1) {
                phase = 0;
                active_work = 0;
            }
            action CheckFailed when (phase == 1 && active_work == 1) {
                phase = 6;
                active_work = 0;
            }
            action StartDownload when (phase == 2 && active_work == 0) {
                phase = 3;
                work_generation = request_generation;
                active_work = 1;
            }
            action SupersedeDownload when (
                phase == 3 && request_generation <= MaxGeneration - 1
            ) {
                phase = 1;
                request_generation = request_generation + 1;
                active_work = 1;
                stale_completion_pending = 1;
            }
            action DropStaleDownload when (
                stale_completion_pending == 1 &&
                request_generation > work_generation
            ) {
                phase = if Buggy == 1 { 4 } else { phase };
                artifact_generation = if Buggy == 1 {
                    work_generation
                } else {
                    artifact_generation
                };
                active_work = if Buggy == 1 { 0 } else { active_work };
                stale_completion_pending = 0;
                verified = if Buggy == 1 { 1 } else { verified };
                stale_staged = if Buggy == 1 { 1 } else { stale_staged };
            }
            action CompleteDownload when (
                phase == 3 && active_work == 1 &&
                work_generation == request_generation
            ) {
                phase = 4;
                artifact_generation = work_generation;
                active_work = 0;
                verified = 1;
            }
            action MarkCloseReady when (phase == 4) {
                close_preflight = 1;
            }
            action Apply when (
                phase == if reexec_count == 0 { 4 } else { 5 } &&
                reexec_count <= if Buggy == 1 { 1 } else { 0 } &&
                verified == 1 &&
                artifact_generation == request_generation &&
                close_preflight == 1
            ) {
                phase = 5;
                reexec_count = reexec_count + 1;
            }
            action AbortApply when (
                phase == 5 && verified == 1 &&
                artifact_generation == request_generation &&
                close_preflight == 1 && reexec_count == 1
            ) {
                phase = 4;
                close_preflight = 0;
                reexec_count = 0;
            }
            bound SingleFlight: active_work <= 1;
            invariant CurrentStagedArtifact:
                if phase == 4 {
                    verified == 1 && artifact_generation == request_generation &&
                    stale_staged == 0
                } else {
                    request_generation <= MaxGeneration
                };
            invariant SafeApply:
                if phase == 5 {
                    verified == 1 && artifact_generation == request_generation &&
                    close_preflight == 1 && reexec_count == 1
                } else {
                    request_generation <= MaxGeneration
                };
            invariant OneLiveApplyAuthority: reexec_count <= 1;
            bound GenerationBounded: request_generation <= MaxGeneration;
        }
    }
}
