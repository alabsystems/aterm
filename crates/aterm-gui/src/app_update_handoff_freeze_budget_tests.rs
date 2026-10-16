// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{FreezeSeed, handoff_freeze_budget};
use crate::native_updater_service::ApplyMode;
use std::time::Duration;

/// The first automatic attempt gets 20 ms; a physical failure of the same bytes
/// buys a wider window, then the widest; an explicit apply gets the widest at
/// once. More failures never buy less time.
#[test]
fn freeze_budget_widens_on_retries_and_for_an_explicit_apply() {
    assert_eq!(
        handoff_freeze_budget(ApplyMode::Automatic, 0, FreezeSeed::Default),
        Duration::from_millis(20)
    );
    assert_eq!(
        handoff_freeze_budget(ApplyMode::AutomaticPastGrace, 0, FreezeSeed::Default),
        Duration::from_millis(20)
    );
    assert_eq!(
        handoff_freeze_budget(ApplyMode::Automatic, 1, FreezeSeed::Default),
        Duration::from_millis(80)
    );
    assert_eq!(
        handoff_freeze_budget(ApplyMode::Automatic, 2, FreezeSeed::Default),
        Duration::from_millis(250)
    );
    assert_eq!(
        handoff_freeze_budget(ApplyMode::AutomaticPastGrace, 7, FreezeSeed::Default),
        Duration::from_millis(250)
    );
    assert_eq!(
        handoff_freeze_budget(ApplyMode::Immediate, 0, FreezeSeed::Default),
        Duration::from_millis(250)
    );
    let mut last = Duration::ZERO;
    for prior in 0..=u8::MAX {
        let now = handoff_freeze_budget(ApplyMode::Automatic, prior, FreezeSeed::Default);
        assert!(
            now >= last,
            "the ladder must be monotone: {prior} prior failures gave {now:?} after {last:?}"
        );
        last = now;
    }
}

/// Rung 0 is seeded from a timed capture: `clamp(1.5 × capture, 20 ms, 80 ms)`, so a desk
/// capturing in 22 ms gets 33 ms at once instead of missing 20 ms and re-parking. Nothing
/// measured, or a capture of 13⅓ ms or less, stays 20 ms.
#[test]
fn rung_zero_is_seeded_from_the_timed_capture_within_its_clamp() {
    use super::{FREEZE_RUNG0_CEILING, FREEZE_RUNG0_FLOOR, freeze_rung0_from_capture};
    let ms = Duration::from_millis;
    for (capture, rung0) in [
        (0, 20),
        (12, 20),
        (14, 21),
        (20, 30),
        (22, 33),
        (40, 60),
        (52, 78),
        (54, 80),
        (100, 80),
    ] {
        assert_eq!(
            freeze_rung0_from_capture(ms(capture)),
            ms(rung0),
            "a {capture} ms capture"
        );
    }
    assert_eq!(
        freeze_rung0_from_capture(Duration::MAX),
        FREEZE_RUNG0_CEILING,
        "an absurd reading saturates, never wraps"
    );
    let seed = FreezeSeed::DryRun {
        capture: ms(22),
        sessions: 2,
        last_park: None,
    };
    for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
        assert_eq!(handoff_freeze_budget(mode, 0, seed), ms(33), "{mode:?}");
        assert_eq!(handoff_freeze_budget(mode, 1, seed), ms(80), "{mode:?}");
        assert_eq!(handoff_freeze_budget(mode, 2, seed), ms(250), "{mode:?}");
    }
    assert_eq!(
        handoff_freeze_budget(ApplyMode::Immediate, 0, seed),
        ms(250),
        "an explicit apply's window is the widest whatever rung 0 is"
    );
    assert_eq!(FreezeSeed::Default.rung0(), FREEZE_RUNG0_FLOOR);
}

/// Whatever the seed, every rung is strictly wider than the one before, rung 0
/// stays inside its clamp, rung 1 is at least 80 ms, and the top is a quarter
/// second.
#[test]
fn every_seed_keeps_the_ladder_strictly_wider_rung_over_rung() {
    use super::{FREEZE_RUNG0_CEILING, FREEZE_RUNG0_FLOOR};
    for capture_ms in 0..=200 {
        let seed = FreezeSeed::Ledger {
            capture: Duration::from_millis(capture_ms),
            build: 1,
        };
        let rung = |prior| handoff_freeze_budget(ApplyMode::Automatic, prior, seed);
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
        assert!(rung(1) >= Duration::from_millis(80), "{capture_ms} ms");
        assert_eq!(rung(2), Duration::from_millis(250), "{capture_ms} ms");
    }
}

/// The ledger's record is a prior: no record is the default, a record seeds rung 0 from its
/// capture and names the build that took it.
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
    assert_eq!(seed.rung0(), Duration::from_millis(33));
    assert_eq!(FreezeSeed::last_park_of(Some(&record)), None);
    let said = seed.to_string();
    assert!(
        said.contains("22.0 ms") && said.contains("1790278596"),
        "the log line names the number and the build it came from: {said}"
    );
    // Neither measured: the default, not a zero-millisecond "measurement".
    let empty = aterm_update::HandoffCaptureRecord::default();
    assert_eq!(FreezeSeed::from_prior(Some(&empty)), FreezeSeed::Default);
}

/// The park the dry run cannot time: the capture itself is around a millisecond,
/// while most of a park is stopping the readers, which only a real park
/// measures. So a landed park's cost is kept and rung 0 is never below it, from
/// the ledger alone (fork lane) or beside the dry run.
#[test]
fn a_landed_parks_cost_is_the_floor_the_dry_run_cannot_see() {
    let ms = Duration::from_millis;
    let beside = |capture, last_park| FreezeSeed::DryRun {
        capture,
        sessions: 2,
        last_park,
    };
    assert_eq!(beside(ms(1), None).rung0(), ms(20), "the capture alone");
    assert_eq!(
        beside(ms(1), Some(ms(22))).rung0(),
        ms(33),
        "0.91's landed park: the next first rung fits it"
    );
    assert_eq!(
        beside(ms(40), Some(ms(22))).rung0(),
        ms(60),
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
    let said = beside(ms(1), Some(ms(22))).to_string();
    assert!(said.contains("22.0 ms"), "{said}");
    let alone = beside(ms(1), None).to_string();
    assert!(alone.contains("no landed park"), "{alone}");
}
