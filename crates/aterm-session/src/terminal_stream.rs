// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bytes observed by one terminal reader or writer, on the process's shared five-second clock.

use std::sync::OnceLock;
use std::time::Instant;

/// Closed intervals in the session panel's one-minute window, oldest first.
pub const BUCKETS: usize = 12;
/// Duration of one terminal-stream interval, in seconds.
pub const SECONDS: u64 = 5;
const SLOTS: usize = BUCKETS + 1;

/// The origin shared by this process's terminal readers, writers and resource display.
#[must_use]
pub fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// The open interval at `at`. The immediately preceding interval has closed.
#[must_use]
pub fn bucket(at: Instant) -> u64 {
    at.saturating_duration_since(epoch()).as_secs() / SECONDS
}

/// Exact byte counts from one continuously observed producer. A replacement gets its own history.
#[derive(Clone, Debug)]
pub struct ByteHistory {
    first_full: u64,
    // Keep the open interval in addition to the twelve the display reads.
    slots: [(u64, u64); SLOTS],
}

impl ByteHistory {
    /// Begin observing at `started`. A producer born partway through a bucket cannot prove its rate.
    #[must_use]
    pub fn new(started: Instant) -> Self {
        let age = started.saturating_duration_since(epoch());
        let partial = !age.as_secs().is_multiple_of(SECONDS) || age.subsec_nanos() != 0;
        Self {
            first_full: age.as_secs() / SECONDS + u64::from(partial),
            slots: [(0, 0); SLOTS],
        }
    }

    /// Count bytes when this producer observed the completed transfer.
    pub fn record(&mut self, at: Instant, bytes: u64) {
        let key = bucket(at);
        let slot = &mut self.slots[(key % SLOTS as u64) as usize];
        match key.cmp(&slot.0) {
            std::cmp::Ordering::Greater => *slot = (key, bytes),
            std::cmp::Ordering::Equal => slot.1 = slot.1.saturating_add(bytes),
            std::cmp::Ordering::Less => {}
        }
    }

    /// Bytes per second in the twelve buckets before `at`, observed until `through`.
    /// A full quiet bucket is zero. An incomplete or overwritten bucket is unknown.
    #[must_use]
    pub fn rates(&self, at: Instant, through: Instant) -> [Option<f64>; BUCKETS] {
        let end = bucket(at);
        let covered_until = bucket(through);
        std::array::from_fn(|index| {
            let key = end.checked_sub((BUCKETS - index) as u64)?;
            if key < self.first_full || key >= covered_until {
                return None;
            }
            let (held, bytes) = self.slots[(key % SLOTS as u64) as usize];
            match held.cmp(&key) {
                std::cmp::Ordering::Less => Some(0.0),
                std::cmp::Ordering::Equal => Some(bytes as f64 / SECONDS as f64),
                std::cmp::Ordering::Greater => None,
            }
        })
    }
}
