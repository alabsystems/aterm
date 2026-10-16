// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Model of the unified scrollback retention limit, `include!`d by
// `tests/conformance_retention.rs`, which proves it and drives the real `Grid`
// against it. Requires `Model` + `ty_model` in scope at the include site.
//
// `set_scrollback_line_limit(L)` is one total: the ring's share is its cap and
// the store's share is `L - RingCap`, so ring + store never exceeds `L`. Each
// `Scroll` pushes one line into the ring; at capacity the evicted oldest line
// moves into the store, which truncates to its share. `Buggy=1` caps the store
// at `Limit` instead, which the Tier-0 test must catch.

/// The unified retention limit as a bounded state machine (see file comment).
fn retention_limit_model() -> Model {
    ty_model! {
        UnifiedRetentionLimit {
            const RingCap = 2;   // fixed ring (fast tier) capacity
            const Limit = 3;     // one total retention limit (>= RingCap here;
                                 // the < RingCap arm is covered by the
                                 // conformance suite)
            const Buggy = 0;     // 0 = unified split; 1 = store capped at Limit
            var ring = 0;        // lines retained in the ring
            var store = 0;       // lines retained in the tiered store
            var produced = 0;    // total lines scrolled off (run bound)

            action Scroll when (produced <= 6) {
                produced = produced + 1;
                store = if ring > RingCap - 1 {
                    (if Buggy > 0 {
                        (if store > Limit - 1 { store } else { store + 1 })
                    } else {
                        (if store > Limit - RingCap - 1 { store } else { store + 1 })
                    })
                } else {
                    store
                };
                ring = if ring > RingCap - 1 { ring } else { ring + 1 };
            }

            // Buggy=1 violates this once over-share store lines stack on the full ring.
            invariant TotalBounded: ring + store <= Limit;
            invariant RingBounded: ring <= RingCap;
        }
    }
}
