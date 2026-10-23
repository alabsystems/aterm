// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance test for `EventLog` ring eviction against `evict_full_model()`.

use crate::support::write_cap;
use aterm_buffer::{Edit, Surface};
use aterm_models_session::evict_full_model;
use std::collections::BTreeSet;

const CAP: u64 = 1 << 16;

#[test]
fn real_eventlog_live_set_is_contiguous_window() {
    let m = evict_full_model();
    assert_eq!(
        m.invariants[0].name, "EvictOldestContiguous",
        "the property verified here is the derived model's invariant"
    );

    let mut s = Surface::new();
    let n_appends = CAP + 8;
    for i in 0..n_appends {
        s.apply(&write_cap(), Edit::AppendLine(format!("e{i}")));
    }

    let seq = s.seq().0;
    let live: BTreeSet<u64> = s.log().live().map(|e| e.seq.0).collect();
    let lo = *live.iter().next().expect("ring is non-empty after appends");

    for n in 1..=seq {
        let is_live = live.contains(&n);
        let in_window = lo <= n && n <= seq;
        assert_eq!(
            is_live, in_window,
            "EvictOldestContiguous violated at n={n} (seq={seq}, lo={lo}): \
             live={is_live} but window membership={in_window}"
        );
    }

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
