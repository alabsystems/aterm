// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tiny output bursts must spend an entry budget as well as a payload budget.
//! This is the one-byte append projection of the cast and live byte queues;
//! variable-sized payload eviction and draining have separate real-code tests.

use super::Model;

/// `Buggy=1` reproduces the payload-only bound: small bursts retain too many
/// entries before filling it. Tier-1 also checks that each queue discloses
/// exactly the lost prefix (`lo - 1`) after every append.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn output_retention_model() -> Model {
    crate::ty_model! {
        OutputRetention {
            const Cap = 3;
            const ByteBudget = 6;
            const MaxSeq = 6;
            const Buggy = 0;
            var seq = 0;
            var lo = 1;

            action Push when (seq <= MaxSeq - 1) {
                seq = seq + 1;
                lo = if seq - lo + 2 > ByteBudget || Buggy == 0 && seq - lo + 2 > Cap {
                    lo + 1
                } else { lo };
            }

            invariant EntriesBounded: seq - lo + 1 <= Cap;
        }
    }
}
