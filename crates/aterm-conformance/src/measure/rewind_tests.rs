// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const STEPS: RewindRow = RewindRow {
    scene: RewindScene::Steps {
        steps: 4,
        max_p99_us: 8_000,
    },
    chain_bytes: 2_000_000,
};

const IDLE: RewindRow = RewindRow {
    scene: RewindScene::Idle {
        window: Duration::from_secs(10),
    },
    chain_bytes: 2_000_000,
};

fn report(row: RewindRow) -> RewindReport {
    RewindReport {
        row,
        keyframes: 1,
        states: 900,
        steps: 4,
        booked: 4,
        p99_us: 2_100,
        done_wakes: 0,
        frames: 0,
        timer_wakes: 0,
        wakes: "Control:2".to_string(),
        verdict: ShapeVerdict::Fail,
        starvation: None,
        disposition: Disposition::Fail,
        checks: String::new(),
    }
}

#[test]
fn a_fast_step_row_passes_and_each_witness_fails_it() {
    assert_eq!(judge(&STEPS, &report(STEPS)), ShapeVerdict::Pass);
    let slow = RewindReport {
        p99_us: 41_000,
        ..report(STEPS)
    };
    assert_eq!(
        judge(&STEPS, &slow),
        ShapeVerdict::Fail,
        "proven red: a worker that refolds from the keyframe on every step"
    );
    let unbooked = RewindReport {
        booked: 3,
        ..report(STEPS)
    };
    assert_eq!(
        judge(&STEPS, &unbooked),
        ShapeVerdict::Fail,
        "a step never shown"
    );
    let rekeyed = RewindReport {
        keyframes: 2,
        ..report(STEPS)
    };
    assert_eq!(
        judge(&STEPS, &rekeyed),
        ShapeVerdict::Fail,
        "a re-keyframe split the chain: the row measured a shorter one"
    );
}

/// The driver's reading over 100 steps: `late_ms` its worst wake.
fn sched(late_ms: u64) -> SchedReading {
    let mut r = SchedReading::default();
    let tick = Duration::from_millis(2);
    for n in 1..=2_000 {
        let late = if n == 900 { late_ms } else { 0 };
        r.book(tick * n, tick * n + Duration::from_millis(late), false);
    }
    r
}

#[test]
fn a_step_row_missed_while_the_driver_stalled_as_long_as_its_bound_is_unproved() {
    let starved = steps_standing(&sched(9), 8_000);
    assert!(matches!(
        starved,
        Starvation::Descheduled {
            threshold_us: 8_000,
            ..
        }
    ));
    let kept = steps_standing(&sched(7), 8_000);
    assert!(matches!(kept, Starvation::KeptCadence { .. }));
    // The desk run under 28 busy loops at load 77.6 to 86.2 (QUIET-BOUND).
    let slow = |starvation| RewindReport {
        p99_us: 12_580,
        starvation: Some(starvation),
        ..report(STEPS)
    };
    assert_eq!(
        dispose(&STEPS, &slow(starved.clone())),
        Disposition::Unproved
    );
    assert_eq!(
        dispose(&STEPS, &slow(kept.clone())),
        Disposition::Fail,
        "proven red: a miss on a driver that kept its cadence is the subject's"
    );
    assert_eq!(
        dispose(
            &STEPS,
            &RewindReport {
                starvation: Some(starved.clone()),
                ..report(STEPS)
            }
        ),
        Disposition::Pass,
        "a bound met under stalls is met"
    );
    assert_eq!(
        dispose(
            &STEPS,
            &RewindReport {
                keyframes: 2,
                starvation: Some(starved),
                ..report(STEPS)
            }
        ),
        Disposition::Fail,
        "a stall never excuses a split chain"
    );
    let unread = RewindReport {
        p99_us: 12_580,
        ..report(STEPS)
    };
    assert_eq!(dispose(&STEPS, &unread), Disposition::Fail);
    assert_eq!(dispose(&IDLE, &report(IDLE)), Disposition::Pass);
}

#[test]
fn an_idle_view_passes_only_with_no_rewind_wake_and_no_frame() {
    assert_eq!(judge(&IDLE, &report(IDLE)), ShapeVerdict::Pass);
    let woke = RewindReport {
        done_wakes: 1,
        ..report(IDLE)
    };
    assert_eq!(judge(&IDLE, &woke), ShapeVerdict::Fail);
    let drew = RewindReport {
        frames: 3,
        ..report(IDLE)
    };
    assert_eq!(judge(&IDLE, &drew), ShapeVerdict::Fail);
}

#[test]
fn the_metrics_fields_read_as_counts_and_microseconds() {
    let line = "OK frames=2 wakes_by_variant=Control:4,RewindDone:3 rewind_step_p99_ms=7.25";
    assert_eq!(variant_wakes(line, "RewindDone"), 3);
    assert_eq!(variant_wakes(line, "Rewind"), 0);
    assert_eq!(variant_wakes("OK wakes_by_variant=none", "RewindDone"), 0);
    assert_eq!(ms_field_us(line, "rewind_step_p99_ms"), Some(7_250));
    assert_eq!(ms_field_us(line, "frames_ms"), None);
    assert_eq!(
        variant_wakes(
            &format!("OK wakes_by_variant=RewindDone:{},RewindDone:1", u64::MAX),
            "RewindDone"
        ),
        u64::MAX
    );
}

#[test]
fn the_chain_file_is_whole_lines_just_under_its_size() {
    let text = chain_text(2_000_000);
    assert!(
        text.len() <= 2_000_000 && text.len() > 2_000_000 - 2 * LINE,
        "{}",
        text.len()
    );
    assert!(text.lines().all(|l| {
        let visible: usize = l
            .split('\x1b')
            .enumerate()
            .map(|(i, part)| {
                if i == 0 {
                    part.len()
                } else {
                    part.split_once('m').map_or(0, |(_, rest)| rest.len())
                }
            })
            .sum();
        visible == LINE - 1
    }));
}
