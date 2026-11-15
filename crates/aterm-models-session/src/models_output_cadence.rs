// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One held-output episode. Clock ordering and concurrent atomic interleavings
//! are exercised by production conformance controls, not abstracted as a proof.

use aterm_spec::derive::Model;

#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn output_cadence_frontier_model() -> Model {
    aterm_spec::ty_model! {
        OutputCadenceFrontier {
            const Buggy = 0;
            var held = 0;
            var indexed = 0;
            var dirty = 0;
            var used = 0;
            var retired = 0;
            var delivered = 0;
            var phantom = 0;
            var stale = 0;
            action Hold when (used == 0 && retired == 0) {
                held = 1;
                indexed = if Buggy == 1 { 0 } else { 1 };
                used = 1;
            }
            action Dirty when (held == 1 && delivered == 0) {
                dirty = 1;
            }
            action Clear when (held == 1) {
                held = 0;
                dirty = 0;
            }
            action Retire when (retired == 0) {
                retired = 1;
                held = 0;
                dirty = 0;
                indexed = if Buggy == 3 { indexed } else { 0 };
            }
            action Service when (used == 1) {
                phantom = if Buggy == 2 && indexed == 1 && dirty == 0 && retired == 0 { 1 } else { phantom };
                stale = if Buggy == 3 && indexed == 1 && retired == 1 { 1 } else { stale };
                delivered = if held == 1 && dirty == 1 && retired == 0 { 1 } else { delivered };
                indexed = if held == 1 && dirty == 1 && retired == 0 { 1 } else { 0 };
                held = if held == 1 && dirty == 1 && retired == 0 { 1 } else { 0 };
                dirty = 0;
            }
            invariant HeldHasFrontier: held == 0 || indexed == 1;
            invariant NoPhantomDelivery: phantom == 0;
            invariant ExactLifetime: stale == 0;
            bound EpisodeBounded: used <= 1 && retired <= 1 && delivered <= 1;
        }
    }
}
