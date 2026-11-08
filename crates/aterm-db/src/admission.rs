// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local custody and writer admission when the caller names no deadline.

use std::time::{Duration, Instant};

const HANG: Duration = Duration::from_secs(60);

/// When an ordinary local admission stops waiting.
pub(crate) fn deadline() -> Instant {
    deadline_at(Instant::now())
}

pub(crate) fn deadline_at(started: Instant) -> Instant {
    started + HANG
}
