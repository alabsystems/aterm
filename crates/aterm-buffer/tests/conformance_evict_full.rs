// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Conformance: the real `EventLog`'s live ring satisfies the derived
//! `evict_full_model()`'s `EvictOldestContiguous`
//! (`\A n : live[n] <=> (lo =< n /\ n =< seq)`): the live region is exactly the
//! window `[lo, seq]`, so eviction removes precisely the oldest event. The
//! scalar ring conformance binds only `seq`/`lo`; this binds the ring contents
//! past the cap. Pure Rust, so it always runs.

use crate::support::write_cap;
use aterm_buffer::{Edit, Surface};
use aterm_models_session::evict_full_model;
use std::collections::BTreeSet;

/// The real ring cap (`aterm_buffer::MAX_LOG_EVENTS`).
const CAP: u64 = 1 << 16;

#[test]
fn real_eventlog_live_set_is_contiguous_window() {
    // Tie this test to the derived model's actual invariant definition.
    let m = evict_full_model();
    assert_eq!(
        m.invariants[0].name, "EvictOldestContiguous",
        "the property verified here is the derived model's invariant"
    );

    let mut s = Surface::new();
    // Drive past the cap so eviction is actually exercised.
    let n_appends = CAP + 8;
    for i in 0..n_appends {
        s.apply(&write_cap(), Edit::AppendLine(format!("e{i}")));
    }

    let seq = s.seq().0;
    // The real live set: the seqs still in the ring.
    let live: BTreeSet<u64> = s.log().live().map(|e| e.seq.0).collect();
    let lo = *live.iter().next().expect("ring is non-empty after appends");

    // EvictOldestContiguous, on real ring data: n is live IFF lo <= n <= seq.
    for n in 1..=seq {
        let is_live = live.contains(&n);
        let in_window = lo <= n && n <= seq;
        assert_eq!(
            is_live, in_window,
            "EvictOldestContiguous violated at n={n} (seq={seq}, lo={lo}): \
             live={is_live} but window membership={in_window}"
        );
    }

    // Eviction genuinely happened, and the live set is exactly the window size.
    assert!(
        lo > 1,
        "expected eviction past the cap (lo advanced beyond 1), got lo={lo}"
    );
    assert_eq!(seq, n_appends, "seq == total appends");
    assert_eq!(
        live.len() as u64,
        seq - lo + 1,
        "the live set is exactly the contiguous window [{lo}, {seq}]"
    );
}
