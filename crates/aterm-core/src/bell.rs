// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Visual-bell flash and audible-bell rate-limiting state.

use aterm_time::Instant;
use std::time::Duration;

/// How long a visual bell keeps the frame flashed (inverted/dimmed).
pub const FLASH_DURATION: Duration = Duration::from_millis(100);

/// Visual-bell flash state machine.
#[derive(Debug, Default)]
pub struct BellFlash {
    deadline: Option<Instant>,
}

impl BellFlash {
    /// A new, inactive flash (no deadline, nothing to paint).
    #[must_use]
    #[inline]
    pub const fn new() -> Self {
        Self { deadline: None }
    }

    /// Arms or extends the flash window until `now + FLASH_DURATION`.
    #[inline]
    pub fn ring(&mut self, now: Instant) {
        self.deadline = Some(now + FLASH_DURATION);
    }

    /// Whether the frame should be drawn flashed at `now`.
    #[must_use]
    #[inline]
    pub fn is_active(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|d| now < d)
    }

    /// Wakeup deadline while a flash is pending, or `None` when idle.
    #[must_use]
    #[inline]
    pub const fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Clears the deadline and returns `true` when it has passed at `now`.
    #[inline]
    pub fn expire(&mut self, now: Instant) -> bool {
        match self.deadline {
            Some(d) if now >= d => {
                self.deadline = None;
                true
            }
            _ => false,
        }
    }
}

/// Minimum-interval gate for the audible bell.
#[derive(Debug)]
pub struct BellRateLimiter {
    interval: Duration,
    last: Option<Instant>,
}

impl BellRateLimiter {
    /// A gate that allows at most one firing per `interval`.
    #[must_use]
    #[inline]
    pub const fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    /// Returns whether a beep may fire at `now`, recording the timestamp when allowed.
    #[inline]
    pub fn try_fire(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|l| now.duration_since(l) < self.interval)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

#[cfg(test)]
#[path = "bell_tests.rs"]
mod tests;
