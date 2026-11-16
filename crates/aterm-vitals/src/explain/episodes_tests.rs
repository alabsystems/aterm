// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::explain::{CoalSplit, WhyFacts};
use crate::published_cpu::CpuAccounting;
use std::sync::Arc;

#[test]
fn reported_cpu_retained_held_and_failing_episodes_never_restore_a_cause_clause() {
    for holds in [true, false] {
        for at_ns in [0, 15_000_000_000, 60_000_000_000] {
            let snapshot = Snapshot {
                at_ns,
                why: Some(Arc::new(WhyFacts {
                    split: CoalSplit {
                        cpu_accounting: CpuAccounting::PUBLISHED_COUNTERS,
                        coverage_complete: true,
                        ..CoalSplit::default()
                    },
                    open: vec![OpenCause {
                        id: CauseId::Activity(ActivityId::Spotlight),
                        opened_unix_ms: 1,
                        since_ms: Some(0),
                        holds,
                        covers_mc: 6000,
                        contexts: vec![Context::Startup],
                    }],
                    clause: Some(DeckCause {
                        activity: ActivityId::Spotlight,
                        word: ActivityId::Spotlight.activity().word,
                        context: Some(Context::Startup),
                    }),
                    ..WhyFacts::default()
                })),
                ..Snapshot::default()
            };
            assert_eq!(deck_cause(&snapshot), None, "holds={holds} at_ns={at_ns}");
        }
    }
}
