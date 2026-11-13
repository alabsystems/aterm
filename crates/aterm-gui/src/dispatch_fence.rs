// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Observe a fresh event-loop acknowledgement before judging native callbacks.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

const DISPATCH_HANG: Duration = Duration::from_secs(60);

/// Pump until this fence is acknowledged, disconnected, or hung.
pub(crate) fn observe(reply: &Receiver<()>, started: Instant, mut pump: impl FnMut()) -> bool {
    while started.elapsed() < DISPATCH_HANG {
        match reply.try_recv() {
            Ok(()) => return true,
            Err(TryRecvError::Disconnected) => return false,
            Err(TryRecvError::Empty) => pump(),
        }
    }
    false
}

#[cfg(test)]
#[path = "dispatch_fence_tests.rs"]
mod tests;
