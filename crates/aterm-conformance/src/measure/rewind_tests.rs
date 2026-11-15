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
        p99_bounds: matches!(row.scene, RewindScene::Steps { .. }).then_some(QuantileBounds {
            lower_ns: 2_000_000,
            upper_ns: 2_100_000,
        }),
        done_wakes: 0,
        frames: 0,
        timer_wakes: 0,
        wakes: "Control:2".to_string(),
        verdict: Verdict::Fail,
        starvation: None,
        disposition: Disposition::Fail,
        checks: String::new(),
    }
}

#[test]
fn a_fast_step_row_passes_and_each_witness_fails_it() {
    assert_eq!(judge(&STEPS, &report(STEPS)), Verdict::Pass);
    let slow = RewindReport {
        p99_us: 41_000,
        p99_bounds: Some(QuantileBounds {
            lower_ns: 40_000_000,
            upper_ns: 41_000_000,
        }),
        ..report(STEPS)
    };
    assert_eq!(
        judge(&STEPS, &slow),
        Verdict::Fail,
        "proven red: a worker that refolds from the keyframe on every step"
    );
    let unbooked = RewindReport {
        booked: 3,
        ..report(STEPS)
    };
    assert_eq!(
        judge(&STEPS, &unbooked),
        Verdict::Fail,
        "a step never shown"
    );
    let rekeyed = RewindReport {
        keyframes: 2,
        ..report(STEPS)
    };
    assert_eq!(
        judge(&STEPS, &rekeyed),
        Verdict::Fail,
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
        p99_bounds: Some(QuantileBounds {
            lower_ns: 12_000_000,
            upper_ns: 12_580_000,
        }),
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
        dispose(&STEPS, &slow(Starvation::NoSamples)),
        Disposition::Fail,
        "a missing scheduling reading does not excuse a definite miss"
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
        p99_bounds: Some(QuantileBounds {
            lower_ns: 12_000_000,
            upper_ns: 12_580_000,
        }),
        ..report(STEPS)
    };
    assert_eq!(dispose(&STEPS, &unread), Disposition::Fail);
    assert_eq!(dispose(&IDLE, &report(IDLE)), Disposition::Pass);
}

#[test]
fn an_idle_view_passes_only_with_no_rewind_wake_and_no_frame() {
    assert_eq!(judge(&IDLE, &report(IDLE)), Verdict::Pass);
    let woke = RewindReport {
        done_wakes: 1,
        ..report(IDLE)
    };
    assert_eq!(judge(&IDLE, &woke), Verdict::Fail);
    let drew = RewindReport {
        frames: 3,
        ..report(IDLE)
    };
    assert_eq!(judge(&IDLE, &drew), Verdict::Fail);
}

#[test]
fn inclusive_percentile_bounds_decide_the_limit_without_using_the_display_value() {
    for (lower_ns, upper_ns, expected) in [
        (0, 7_999_999, Verdict::Pass),
        (7_900_000, 8_000_000, Verdict::Pass),
        (8_000_000, 8_000_000, Verdict::Pass),
        (8_000_000, 8_000_001, Verdict::Unproved),
        (7_864_320, 8_388_607, Verdict::Unproved),
        (8_000_001, 8_000_001, Verdict::Fail),
        (8_388_608, 8_912_895, Verdict::Fail),
    ] {
        for p99_us in [0, u64::MAX] {
            let measured = RewindReport {
                p99_us,
                p99_bounds: Some(QuantileBounds { lower_ns, upper_ns }),
                ..report(STEPS)
            };
            assert_eq!(judge(&STEPS, &measured), expected, "{measured:?}");
            assert_eq!(
                dispose(&STEPS, &measured),
                match expected {
                    Verdict::Pass => Disposition::Pass,
                    Verdict::Fail => Disposition::Fail,
                    Verdict::Unproved => Disposition::Unproved,
                },
                "{measured:?}"
            );
        }
    }
}

#[test]
fn overlap_stays_unproved_at_any_load_and_never_excuses_a_structural_failure() {
    for starvation in [
        None,
        Some(steps_standing(&sched(7), 8_000)),
        Some(steps_standing(&sched(9), 8_000)),
        Some(steps_standing(&SchedReading::default(), 8_000)),
    ] {
        let measured = RewindReport {
            p99_us: 8_390,
            p99_bounds: Some(QuantileBounds {
                lower_ns: 7_864_320,
                upper_ns: 8_388_607,
            }),
            starvation,
            ..report(STEPS)
        };
        assert_eq!(judge(&STEPS, &measured), Verdict::Unproved);
        assert_eq!(dispose(&STEPS, &measured), Disposition::Unproved);
        for broken in [
            RewindReport {
                booked: 3,
                ..measured.clone()
            },
            RewindReport {
                keyframes: 2,
                ..measured.clone()
            },
        ] {
            assert_eq!(judge(&STEPS, &broken), Verdict::Fail);
            assert_eq!(dispose(&STEPS, &broken), Disposition::Fail);
        }
    }
}

#[test]
fn the_metrics_interval_requires_two_ordered_unsigned_nanosecond_counts() {
    for (lower_ns, upper_ns) in [(0, 0), (7_864_320, 8_388_607), (u64::MAX, u64::MAX)] {
        let line = format!(
            "OK rewind_shown_p99_ms=8.39 rewind_shown_p99_lower_ns={lower_ns} \
             rewind_shown_p99_upper_ns={upper_ns}"
        );
        assert_eq!(
            QuantileBounds::from_metrics(&line),
            Ok(QuantileBounds { lower_ns, upper_ns })
        );
    }
    for missing in [
        "OK rewind_shown_p99_ms=2.10",
        "OK rewind_shown_p99_lower_ns=0",
        "OK rewind_shown_p99_upper_ns=8000000",
    ] {
        assert!(matches!(
            QuantileBounds::from_metrics(missing),
            Err(CouldNotRun::Unanswered(_))
        ));
    }
    for bad in ["", "-1", "+1", "1.0", "NaN", "18446744073709551616"] {
        for (lower, upper) in [(bad, "8000000"), ("0", bad)] {
            let line =
                format!("OK rewind_shown_p99_lower_ns={lower} rewind_shown_p99_upper_ns={upper}");
            assert!(matches!(
                QuantileBounds::from_metrics(&line),
                Err(CouldNotRun::Refused(_))
            ));
        }
    }
    assert!(matches!(
        QuantileBounds::from_metrics(
            "OK rewind_shown_p99_lower_ns=8000001 rewind_shown_p99_upper_ns=8000000"
        ),
        Err(CouldNotRun::Refused(_))
    ));
}

#[test]
fn an_absent_or_reversed_interval_never_borrows_the_displayed_percentile() {
    for p99_bounds in [
        None,
        Some(QuantileBounds {
            lower_ns: 8_000_001,
            upper_ns: 0,
        }),
    ] {
        let measured = RewindReport {
            p99_us: 0,
            p99_bounds,
            ..report(STEPS)
        };
        assert_eq!(judge(&STEPS, &measured), Verdict::Unproved);
        assert_eq!(dispose(&STEPS, &measured), Disposition::Unproved);
    }
}

#[test]
fn nanosecond_bounds_and_microsecond_budgets_keep_their_full_integer_range() {
    for (max_p99_us, lower_ns, upper_ns, expected) in [
        (0, 0, 0, Verdict::Pass),
        (0, 0, 1, Verdict::Unproved),
        (0, 1, 1, Verdict::Fail),
        (u64::MAX / 1_000, u64::MAX, u64::MAX, Verdict::Fail),
        (u64::MAX / 1_000, 0, u64::MAX, Verdict::Unproved),
        (u64::MAX / 1_000 + 1, 0, u64::MAX, Verdict::Pass),
        (u64::MAX, u64::MAX, u64::MAX, Verdict::Pass),
    ] {
        let row = RewindRow {
            scene: RewindScene::Steps {
                steps: 4,
                max_p99_us,
            },
            ..STEPS
        };
        let measured = RewindReport {
            p99_bounds: Some(QuantileBounds { lower_ns, upper_ns }),
            ..report(row)
        };
        assert_eq!(
            judge(&row, &measured),
            expected,
            "{max_p99_us}: {measured:?}"
        );
    }
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
