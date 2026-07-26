// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// SHARED model of the UNIFIED scrollback retention limit (audit E1, Codex-required
// modification), `include!`d by BOTH the compile-time gate (`build.rs`) and the
// conformance test (`tests/conformance_retention.rs`) — ONE source of truth, the
// same discipline as `offload_window_spec.rs` (extend, don't duplicate). Requires
// `Model` + `ty_model` in scope at the include site.
//
// THE BUG (pre-unification): a tiered grid's `set_scrollback_line_limit(L)` capped
// the STORE at `L` while the fixed ring kept retaining its full cap on top, so the
// user's "retain L lines" setting silently retained `L + RingCap` — double
// retention at the default ring size. THE FIX: `L` is ONE TOTAL — the ring's share
// is its cap, the store's share is `L - RingCap`, so ring + store never exceeds
// `L`.
//
// The model is the steady-state retention pipeline: each `Scroll` pushes one line
// off the viewport into the ring; at ring capacity the evicted oldest line moves
// into the store, which truncates to its share. `Buggy=1` reintroduces the
// store-capped-at-`Limit` split (must be CAUGHT — the gate requires the Buggy=1
// counterexample so `TotalBounded` cannot go vacuous).

/// The unified retention limit as a bounded state machine (see file comment).
fn retention_limit_model() -> Model {
    ty_model! {
        UnifiedRetentionLimit {
            const RingCap = 2;   // fixed ring (fast tier) capacity
            const Limit = 3;     // ONE total retention limit (>= RingCap here;
                                 // the < RingCap arm re-caps the ring and is
                                 // covered by the conformance suite)
            const Buggy = 0;     // 0 = unified split; 1 = pre-fix store-cap=Limit
            var ring = 0;        // lines retained in the ring
            var store = 0;       // lines retained in the tiered store
            var produced = 0;    // total lines scrolled off (run bound)

            // One line scrolls off the viewport. The ring absorbs it; at ring
            // capacity the evicted oldest line is staged into the store, which
            // truncates to its share of the total.
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

            // THE unification contract: total retention never exceeds the ONE
            // limit. Buggy=1 (store capped at Limit) violates this as soon as
            // the over-share store lines stack on the full ring.
            invariant TotalBounded: ring + store <= Limit;
            // The ring never exceeds its fixed cap regardless of split mode.
            invariant RingBounded: ring <= RingCap;
        }
    }
}
