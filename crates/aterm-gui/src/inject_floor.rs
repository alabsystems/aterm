// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **D3 — the un-bypassable self-feed floor.** A process-wide token bucket on
//! control-injected input, keyed by the target session's process-local id.
//!
//! The L2 [`SelfGovernor`](../../aterm_agent/struct.SelfGovernor.html) is the
//! *rich* self-reflection policy — but it only binds a driver that links
//! `aterm-agent`. A raw control-socket client can `feed @.` in a loop without it,
//! so the **mandatory** backstop must live at the control dispatch path, where no
//! client can route around it. This is that floor: every SELF-targeted
//! input-injection verb passes [`allow`] first — `send`/`key`/`ctrl`/`feed`/
//! `mouse`/`paste` at the dispatch path (control.rs), and `feed-bin` in
//! `run_feed_bin` (it is intercepted before dispatch) — bounding the *rate* of
//! self-injection so an output→observe→
//! write→output feedback storm hits backpressure (`ERR rate`) instead of
//! saturating the engine. The cap is generous — legitimate driving (a prompt, an
//! Enter) is orders of magnitude under it — so only a runaway loop is throttled.
//!
//! Its `NoOverdraft` bound (a write is admitted only with a spare token; tokens
//! never exceed the cap) is model-checked by `inject_floor_model` (`aterm-spec`).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Burst capacity in bytes (also the refill ceiling). 256 KiB lets a paste or a
/// long prompt through instantly while bounding a storm's standing burst.
const CAP_BYTES: f64 = 256.0 * 1024.0;

/// Sustained refill rate (bytes/sec). A feedback loop driving faster than a human
/// could ever type is throttled to this once the burst is spent.
const REFILL_BYTES_PER_SEC: f64 = 256.0 * 1024.0;

struct Bucket {
    tokens: f64,
    last: Instant,
}

/// Process-wide buckets, one per session local id. Created lazily; a session that
/// is never self-driven costs one map probe and no bucket.
static FLOOR: OnceLock<Mutex<HashMap<u64, Bucket>>> = OnceLock::new();

/// Admit `nbytes` of self-targeted injection for `session`, or refuse (the floor
/// is exhausted). Refills continuously from the wall clock since the last call —
/// host-side timing only (this is the control layer, not the engine pipeline).
#[must_use]
pub(crate) fn allow(session: u64, nbytes: usize) -> bool {
    let now = Instant::now(); // CLOCK-EXEMPT: control-layer rate limiter, not engine state
    // Bound to a NAMED local so the lock-order census (OB-7) resolves the
    // acquisition to the `floor` identity (`forget` uses the same name).
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

/// Drop a session's bucket when it closes. Called from the `Wake::Exit` handler
/// (`main.rs`) so the per-session map cannot grow for the process lifetime as
/// sessions open and close.
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
