// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

fn retention_limit_model() -> Model {
    ty_model! {
        UnifiedRetentionLimit {
            const RingCap = 2;   // fixed ring (fast tier) capacity
            const Limit = 3;     // one total retention limit (>= RingCap here)
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

            invariant TotalBounded: ring + store <= Limit;
            invariant RingBounded: ring <= RingCap;
        }
    }
}
