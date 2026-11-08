// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dependency-free row boundaries; the caller supplies its CPU reader.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub(super) struct Timing {
    pub started: u64,
    pub ended: u64,
    pub seconds: f64,
    pub cpu_s: Option<f64>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

pub(super) fn measure<T>(run: impl FnOnce() -> T, cpu: fn() -> Option<u64>) -> (T, Timing) {
    let started = now();
    let clock = Instant::now();
    let before = cpu();
    let result = run();
    let after = cpu();
    (
        result,
        Timing {
            started,
            ended: now(),
            seconds: clock.elapsed().as_secs_f64(),
            cpu_s: before
                .zip(after)
                .and_then(|(a, b)| b.checked_sub(a))
                .map(|ns| ns as f64 / 1e9),
        },
    )
}
