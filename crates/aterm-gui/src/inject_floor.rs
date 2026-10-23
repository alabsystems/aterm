// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Self-feed floor: process-wide token bucket on control-injected input per local session id.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

const CAP_BYTES: f64 = 256.0 * 1024.0;

const REFILL_BYTES_PER_SEC: f64 = 256.0 * 1024.0;

struct Bucket {
    tokens: f64,
    last: Instant,
}

static FLOOR: OnceLock<Mutex<HashMap<u64, Bucket>>> = OnceLock::new();

/// Admit `nbytes` of self-targeted injection for `session`, or refuse when the floor is exhausted.
#[must_use]
pub(crate) fn allow(session: u64, nbytes: usize) -> bool {
    let now = Instant::now(); // control-layer rate limiter, not engine state
    let floor = FLOOR.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = floor.lock().unwrap_or_else(|p| p.into_inner());
    let b = map.entry(session).or_insert(Bucket {
        tokens: CAP_BYTES,
        last: now,
    });
    let elapsed = now.saturating_duration_since(b.last).as_secs_f64();
    b.tokens = (b.tokens + elapsed * REFILL_BYTES_PER_SEC).min(CAP_BYTES);
    b.last = now;
    let need = (nbytes as f64).max(1.0);
    if b.tokens >= need {
        b.tokens -= need;
        true
    } else {
        false
    }
}

/// Drop a session's bucket when it closes (`Wake::Exit` in `App::user_event`).
pub(crate) fn forget(session: u64) {
    if let Some(floor) = FLOOR.get() {
        floor
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_admits_under_cap_and_refuses_a_burst_storm() {
        let s = 9_999_001;
        assert!(allow(s, 64), "a small write is under the floor");
        assert!(
            !allow(s, (CAP_BYTES as usize) + 1),
            "a > cap single write is refused"
        );
        forget(s);
    }

    #[test]
    fn floor_throttles_a_tight_loop_then_recovers() {
        let s = 9_999_002;
        let chunk = (CAP_BYTES as usize) / 4;
        let mut admitted = 0;
        for _ in 0..8 {
            if allow(s, chunk) {
                admitted += 1;
            }
        }
        assert!(admitted <= 5, "burst is bounded, got {admitted} admits");
        forget(s);
    }
}
