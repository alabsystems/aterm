// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_spec::interp::State;

/// Project the bounded lifecycle fields shared by the worker/device actions.
#[allow(
    clippy::too_many_arguments,
    reason = "named scalar projection mirrors the derived model variables exactly"
)]
pub(crate) fn project_worker(
    queued: usize,
    dropped: u64,
    last_full: bool,
    running: bool,
    silent: u32,
    service_deadline: bool,
    cue_applied: bool,
    failed: bool,
    paused: bool,
) -> State {
    [
        ("queued", queued.min(2) as i64),
        ("dropped", dropped.min(2) as i64),
        ("last_full", i64::from(last_full)),
        ("running", i64::from(running)),
        ("silent", i64::from(silent.min(2))),
        ("service_deadline", i64::from(service_deadline)),
        ("cue_applied", i64::from(cue_applied)),
        ("failed", i64::from(failed)),
        ("paused", i64::from(paused)),
        ("ui_blocked", 0),
        ("ui_platform_calls", 0),
    ]
    .into_iter()
    .collect()
}

pub(crate) fn project_ingress(queued: usize, dropped: u64, last_full: bool) -> State {
    project_worker(
        queued, dropped, last_full, false, 0, false, false, false, false,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "exact scalar projection of the queue ownership/latency model"
)]
pub(crate) fn project_start_latency(
    phase: i64,
    available: usize,
    queued: usize,
    recycling: bool,
    running: bool,
    audible_buffer: usize,
    unsafe_writes: usize,
    generation: u64,
    callback_generation: u64,
    stale_enqueues: usize,
    enqueue_in_flight: bool,
    stop_overlaps: usize,
) -> State {
    [
        ("phase", phase),
        ("available", available as i64),
        ("queued", queued as i64),
        ("recycling", i64::from(recycling)),
        ("running", i64::from(running)),
        ("audible_buffer", audible_buffer as i64),
        ("unsafe_writes", unsafe_writes.min(1) as i64),
        ("generation", generation as i64),
        ("callback_generation", callback_generation as i64),
        ("stale_enqueue", stale_enqueues.min(1) as i64),
        ("enqueue_in_flight", i64::from(enqueue_in_flight)),
        ("stop_overlap", stop_overlaps.min(1) as i64),
    ]
    .into_iter()
    .collect()
}
