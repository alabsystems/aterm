// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{FreezeSeed, freeze_rung0_from_capture, handoff_freeze_budget};
use crate::native_updater_service::ApplyMode;
use std::time::Duration;

/// Whatever the seed, every rung is strictly wider than the one before, rung 0
/// stays inside its clamp, more failures never buy less time, and an explicit
/// apply gets the widest window at once.
#[test]
fn every_seed_keeps_the_ladder_strictly_wider_rung_over_rung() {
    use super::{FREEZE_RUNG0_CEILING, FREEZE_RUNG0_FLOOR};
    assert_eq!(FreezeSeed::Default.rung0(), FREEZE_RUNG0_FLOOR);
    assert_eq!(
        freeze_rung0_from_capture(Duration::MAX),
        FREEZE_RUNG0_CEILING,
        "an absurd reading saturates, never wraps"
    );
    for capture_ms in 0..=200 {
        let seed = FreezeSeed::Ledger {
            capture: Duration::from_millis(capture_ms),
            build: 1,
        };
        for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
            let rung = |prior| handoff_freeze_budget(mode, prior, seed);
            assert!(
                (FREEZE_RUNG0_FLOOR..=FREEZE_RUNG0_CEILING).contains(&rung(0)),
                "{capture_ms} ms: rung 0 {:?}",
                rung(0)
            );
            assert!(
                rung(0) < rung(1) && rung(1) < rung(2),
                "{capture_ms} ms: {:?} {:?} {:?}",
                rung(0),
                rung(1),
                rung(2)
            );
            assert!(
                (2..=u8::MAX).all(|prior| rung(prior) == rung(2)),
                "{capture_ms} ms: more failures never buy less (or more) than the top"
            );
            assert_eq!(
                handoff_freeze_budget(ApplyMode::Immediate, 0, seed),
                rung(2),
                "an explicit apply's window is the widest whatever rung 0 is"
            );
        }
    }
}

/// The ledger's record is a prior: no record (or one that measured nothing) is the default, a
/// record seeds rung 0 from its capture and names the build that took it.
#[test]
fn the_ledgers_last_timed_capture_seeds_a_lane_that_times_none() {
    assert_eq!(FreezeSeed::from_prior(None), FreezeSeed::Default);
    let record = aterm_update::HandoffCaptureRecord {
        capture_us: 22_000,
        sessions: 2,
        freeze_seed_ms: 33,
        build: 1_790_278_596,
        at: "2026-09-26T00:00:00Z".to_string(),
        park_us: 0,
        park_build: 0,
    };
    let seed = FreezeSeed::from_prior(Some(&record));
    assert_eq!(
        seed,
        FreezeSeed::Ledger {
            capture: Duration::from_millis(22),
            build: 1_790_278_596
        }
    );
    assert_eq!(FreezeSeed::last_park_of(Some(&record)), None);
    let empty = aterm_update::HandoffCaptureRecord::default();
    assert_eq!(FreezeSeed::from_prior(Some(&empty)), FreezeSeed::Default);
}

/// The park the dry run cannot time: the capture itself is around a millisecond,
/// while most of a park is stopping the readers, which only a real park
/// measures.
#[test]
fn a_landed_parks_cost_is_the_floor_the_dry_run_cannot_see() {
    let ms = Duration::from_millis;
    let beside = |capture, last_park| FreezeSeed::DryRun {
        capture,
        sessions: 2,
        last_park,
    };
    assert_eq!(
        beside(ms(1), Some(ms(22))).rung0(),
        freeze_rung0_from_capture(ms(22)),
        "0.91's landed park: the next first rung fits it"
    );
    assert!(beside(ms(1), Some(ms(22))).rung0() > beside(ms(1), None).rung0());
    assert_eq!(
        beside(ms(40), Some(ms(22))).rung0(),
        freeze_rung0_from_capture(ms(40)),
        "whichever is larger"
    );
    let record = aterm_update::HandoffCaptureRecord {
        capture_us: 1_100,
        sessions: 2,
        freeze_seed_ms: 20,
        build: 8,
        at: String::new(),
        park_us: 22_000,
        park_build: 9,
    };
    assert_eq!(
        FreezeSeed::from_prior(Some(&record)),
        FreezeSeed::Ledger {
            capture: ms(22),
            build: 9
        },
        "the ledger seeds from the larger, named by the build that recorded it"
    );
    assert_eq!(FreezeSeed::last_park_of(Some(&record)), Some(ms(22)));
}
