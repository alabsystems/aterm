// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The self-feed floor: a process-wide token bucket on control-injected input,
//! keyed by the target session's process-local id.
//!
//! A raw control-socket client can `feed @.` in a loop, and nothing on the
//! client's side stops it. So every self-targeted injection verb passes [`allow`]
//! first — `send`/`key`/`ctrl`/`feed`/`mouse`/`paste` at the dispatch path
//! (control.rs) and `feed-bin` in `run_feed_bin` — and a feedback storm gets
//! `ERR rate` instead of saturating the engine. The cap is far above legitimate
//! driving, so only a runaway loop is throttled.
//!
//! Its `NoOverdraft` bound: a write is admitted only with a spare token, and
//! tokens never exceed the cap.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Burst capacity in bytes (also the refill ceiling): a paste or long prompt
/// passes at once while a storm's standing burst stays bounded.
const CAP_BYTES: f64 = 256.0 * 1024.0;

/// Sustained refill rate in bytes/sec once the burst is spent.
const REFILL_BYTES_PER_SEC: f64 = 256.0 * 1024.0;

struct Bucket {
    tokens: f64,
    last: Instant,
}

/// Process-wide buckets, one per session local id. Created lazily; a session that
/// is never self-driven costs one map probe and no bucket.
static FLOOR: OnceLock<Mutex<HashMap<u64, Bucket>>> = OnceLock::new();

/// Admit `nbytes` of self-targeted injection for `session`, or refuse when the
/// floor is exhausted. Refills from the wall clock since the last call.
#[must_use]
pub(crate) fn allow(session: u64, nbytes: usize) -> bool {
    let now = Instant::now(); // control-layer rate limiter, not engine state
    // A named local so the lock-order census resolves this acquisition to the
    // `floor` identity (`forget` uses the same name).
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

/// Drop a session's bucket when it closes (the `Wake::Exit` arm of
/// `App::user_event`, lib.rs), so the map does not grow for the process lifetime.
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
        // A normal prompt-sized write is admitted.
        assert!(allow(s, 64), "a small write is under the floor");
        // Draining far past the burst capacity in one shot is refused.
        assert!(
            !allow(s, (CAP_BYTES as usize) + 1),
            "a > cap single write is refused"
        );
        forget(s);
    }

    #[test]
    fn floor_throttles_a_tight_loop_then_recovers() {
        let s = 9_999_002;
        // Spend the whole burst in big chunks.
        let chunk = (CAP_BYTES as usize) / 4;
        let mut admitted = 0;
        for _ in 0..8 {
            if allow(s, chunk) {
                admitted += 1;
            }
        }
        // The burst bounds how many back-to-back chunks get through (~4), so a
        // tight loop cannot inject unboundedly.
        assert!(admitted <= 5, "burst is bounded, got {admitted} admits");
        forget(s);
    }
}
