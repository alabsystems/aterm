// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A monotonic clock advanced by an isolated test in another process. Callers obtain the
//! optional directory through `dev_seam!`, so shipped callers always use the native clock.
//! A reading is acknowledged only after the caller has judged the property under test.
//! `<key>.ready` announces a read; an optional `<key>.hold` barrier holds that read until the
//! fixture removes it. Both barriers use a one-minute hang detector.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A native clock plus a monotonic millisecond offset, scoped to one fixture and operation.
pub struct Clock {
    path: Option<PathBuf>,
    offset: AtomicU64,
}

impl Clock {
    /// Use `directory/key` for externally advanced time, or the native clock without a directory.
    pub fn new(directory: Option<std::ffi::OsString>, key: &str) -> Self {
        Self {
            path: directory.map(|dir| PathBuf::from(dir).join(key)),
            offset: AtomicU64::new(0),
        }
    }

    /// Read time. Missing or partially written input keeps the last offset; time never retreats.
    ///
    /// # Panics
    /// Panics when the fixture leaves its read barrier held for a minute.
    pub fn read(&self) -> Reading<'_> {
        if let Some(path) = &self.path {
            let _ = std::fs::write(path.with_extension("ready"), b"");
            let hang = Instant::now() + Duration::from_secs(60);
            while path.with_extension("hold").exists() {
                assert!(
                    Instant::now() < hang,
                    "test clock held past its hang detector: {path:?}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let real = Instant::now();
        let offset = self
            .path
            .as_ref()
            .and_then(|path| {
                std::fs::read_to_string(path)
                    .ok()?
                    .trim()
                    .parse::<u64>()
                    .ok()
            })
            .filter(|&offset| real.checked_add(Duration::from_millis(offset)).is_some())
            .unwrap_or(0);
        let offset = self.offset.fetch_max(offset, Ordering::SeqCst).max(offset);
        Reading {
            clock: self,
            now: real
                .checked_add(Duration::from_millis(offset))
                .unwrap_or(real),
            offset,
        }
    }

    /// Whether this operation has an external clock.
    pub fn advanced(&self) -> bool {
        self.path.is_some()
    }
}

/// One reading, and the receipt that its consumer has finished judging it.
pub struct Reading<'a> {
    clock: &'a Clock,
    /// Native monotonic time plus the fixture's last accepted offset.
    pub now: Instant,
    offset: u64,
}

impl Reading<'_> {
    /// Record that the consumer judged this reading. The fixture waits for this receipt before
    /// drawing its next event; a clock read alone is not proof the decision used it.
    pub fn observed(self) {
        if let Some(path) = &self.clock.path {
            let _ = std::fs::write(path.with_extension("seen"), self.offset.to_string());
        }
    }

    /// A deadline on this clock expressed on the native wait clock. Advanced clocks recheck
    /// within 20 ms so a change needs no unrelated terminal event to wake the consumer.
    pub fn wait_until(&self, until: Instant) -> Instant {
        if !self.clock.advanced() {
            return until;
        }
        let left = until.saturating_duration_since(self.now);
        Instant::now() + left.min(Duration::from_millis(20))
    }
}

#[cfg(test)]
#[path = "advance_clock_tests.rs"]
mod tests;
