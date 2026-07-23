// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Conformance: the real `EventLog`'s live ring satisfies the DERIVED faithful
//! ring's `EvictOldestContiguous`.
//!
//! `aterm_spec::derive::evict_full_model()` is the function-valued model whose
//! invariant `EvictOldestContiguous` (`\A n : live[n] <=> (lo =< n /\ n =< seq)`)
//! `ty` proves (Tier 0): the live region is EXACTLY the contiguous window
//! `[lo, seq]`, so eviction removes precisely the oldest event — never a hole,
//! never two. The scalar ring conformance binds only `seq`/`lo`; THIS test binds
//! the actual ring CONTENTS, by driving the genuine shipping `EventLog` past its
//! cap (so eviction fires) and checking that the set of live seqs is exactly the
//! window. A bug that evicted out of order, dropped an extra event, or left a hole
//! would violate it. Pure Rust + real code, so it always runs.

use aterm_buffer::{Edit, Surface, SurfaceId, WriteCap};
use aterm_spec::derive::evict_full_model;
use std::collections::BTreeSet;
use std::num::NonZeroU64;

/// The real ring cap (mirrors `aterm_buffer::MAX_LOG_EVENTS = 1<<16`).
const CAP: u64 = 1 << 16;

#[test]
fn real_eventlog_live_set_is_contiguous_window() {
    // Tie this test to the derived model's actual invariant definition.
    let m = evict_full_model();
    assert_eq!(
        m.invariants[0].name, "EvictOldestContiguous",
        "the property verified here is the derived model's invariant"
    );

    let mut s = Surface::new(SurfaceId(NonZeroU64::new(1).unwrap()));
    // Drive past the cap so eviction is actually exercised.
    let n_appends = CAP + 8;
    for i in 0..n_appends {
        s.apply(&WriteCap, Edit::AppendLine(format!("e{i}")));
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
