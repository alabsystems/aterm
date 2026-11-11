// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn t0() -> Instant {
    Instant::now()
}

fn at(base: Instant, ms_: u64) -> Instant {
    base + Duration::from_millis(ms_)
}

fn bytes(done: u64, total: u64) -> Amount {
    Amount {
        series: 7,
        done,
        total,
        unit: Unit::Bytes,
    }
}

/// A steady rate latches at 5 s (the first valid rate is at 3 s, and three
/// projections spanning STABLE_MIN agree by 5 s).
#[test]
fn eta_latches_only_when_three_projections_agree_over_two_seconds() {
    let base = t0();
    let total = 1_000_000_000;
    let mut t = ProgressTrack::default();
    let mut latched_at = None;
    for k in 0..=80u64 {
        let now = at(base, k * 100);
        t.observe(now, bytes(k * 1_000_000, total));
        if latched_at.is_none() && matches!(t.eta(now), Eta::Remaining(_)) {
            latched_at = Some(k * 100);
        }
    }
    assert_eq!(latched_at, Some(5000), "a steady rate latches at 5 s");
    let chunk = 1_000_000;
    let mut burst = ProgressTrack::default();
    let mut shown = None;
    for k in 0..=300u64 {
        let now = at(base, k * 100);
        burst.observe(now, bytes(chunk * (1 + k / 20), 100 * chunk));
        if let Eta::Remaining(r) = burst.eta(now) {
            shown.get_or_insert((k * 100, now + r));
        }
    }
    let (first, anchor) = shown.expect("the averaged stream latches");
    assert!(
        first >= 10_000,
        "no ETA while the bursts still swing the projections: {first} ms"
    );
    let truth = at(base, 99 * 2000);
    let err = if anchor > truth {
        anchor - truth
    } else {
        truth - anchor
    };
    assert!(
        err <= Duration::from_millis(99 * 2000 / 10),
        "latched {err:?} off the truth"
    );
}

/// Between reads a latched ETA counts down one for one; it never jumps
/// back up unless it re-anchors, and a real change of rate moves the
/// anchor only in the change's direction until it settles on the new
/// pace.
#[test]
fn eta_counts_down_between_reads_and_reanchors_once_on_a_real_change() {
    let base = t0();
    let total = 400_000_000;
    let mut t = ProgressTrack::default();
    let mut done = 0;
    for k in 0..=60u64 {
        done = (k + 1) * 1_000_000;
        t.observe(at(base, k * 100), bytes(done, total));
    }
    let now = at(base, 6000);
    let Eta::Remaining(r0) = t.eta(now) else {
        panic!("latched: {:?}", t.eta(now));
    };
    assert!(
        (33_000..=35_000).contains(&ms(r0)),
        "{r0:?} left at a steady 10 MB/s"
    );
    let Eta::Remaining(r1) = t.eta(at(base, 7500)) else {
        panic!()
    };
    assert_eq!(r0 - r1, Duration::from_millis(1500), "a countdown, 1:1");
    assert_eq!(t.next_change(now), Some(now + eta_word_change(r0).unwrap()));
    let mut anchors: Vec<Instant> = Vec::new();
    for k in 61..=400u64 {
        done += 500_000;
        let now = at(base, k * 100);
        t.observe(now, bytes(done, total));
        if let Eta::Remaining(r) = t.eta(now) {
            let anchor = now + r;
            if anchors.last() != Some(&anchor) {
                anchors.push(anchor);
            }
        }
    }
    assert!(anchors.len() >= 2, "the change moved the anchor");
    for pair in anchors.windows(2) {
        assert!(
            pair[1] > pair[0] + Duration::from_secs(3),
            "a re-anchor is a real step, in the change's direction: {pair:?}"
        );
    }
    let truth = at(base, 6100 + 67_800);
    let last = *anchors.last().unwrap();
    let err = if last > truth {
        last - truth
    } else {
        truth - last
    };
    assert!(
        err <= Duration::from_millis(67_800 / 10),
        "settled {err:?} off the new pace (within a tenth of the new remainder)"
    );
}

/// The words: quantized, never zero, and the table the design pins.
#[test]
fn eta_words_quantize_and_never_say_zero() {
    let w = |s: u64| eta_words(Duration::from_secs(s));
    assert_eq!(eta_words(Duration::ZERO).as_deref(), Some("<5 s left"));
    for (s, want) in [
        (1, "<5 s left"),
        (4, "<5 s left"),
        (5, "5 s left"),
        (6, "10 s left"),
        (35, "35 s left"),
        (55, "55 s left"),
        (56, "1 min left"),
        (89, "1 min left"),
        (90, "2 min left"),
        (170, "3 min left"),
        (3569, "59 min left"),
        (3570, "1 h left"),
        (86_400, "24 h left"),
    ] {
        assert_eq!(w(s).as_deref(), Some(want), "{s} s");
    }
    assert_eq!(w(86_401), None, "past a day");
    assert_eq!(
        eta_words(Duration::from_millis(5_001)).as_deref(),
        Some("10 s left"),
        "whole seconds rounded up"
    );
    for s in 0..90_000u64 {
        let words = w(s);
        assert!(
            words
                .as_deref()
                .is_none_or(|x| !x.starts_with('0') && !x.contains('~')),
            "{s}: {words:?}"
        );
        if let Some(x) = words {
            assert!(x.chars().count() <= crate::ETA_W, "{x:?}");
            assert!(x.ends_with(" left") && !x.contains(':'), "{x:?}");
        }
        let short = eta_words_short(Duration::from_secs(s));
        assert_eq!(short.is_some(), w(s).is_some(), "{s}");
        if let Some(x) = short {
            assert!(x.chars().count() <= crate::ETA_SHORT_W, "{x:?}");
            assert!(x.ends_with(" left") && !x.contains(':'), "{x:?}");
        }
    }
    for (s, want) in [
        (0, "<5s left"),
        (4, "<5s left"),
        (35, "35s left"),
        (170, "3m left"),
        (3569, "59m left"),
        (86_400, "24h left"),
    ] {
        assert_eq!(
            eta_words_short(Duration::from_secs(s)).as_deref(),
            Some(want),
            "{s} s"
        );
    }
    for e in [10u64, 28, 59, 60, 599, 3599, 3600, 36_000] {
        for short in [false, true] {
            let ran = elapsed_words(Duration::from_secs(e), short).unwrap();
            assert!(
                !ran.contains("left") && ran.starts_with("for "),
                "the elapsed time is not an ETA: {ran:?}"
            );
        }
    }
}

/// Every 10 ms over two hours, the words hold until the change and
/// differ at it.
#[test]
fn eta_word_change_is_exact() {
    for r_ms in (0..=7_200_000u64).step_by(10) {
        let r = Duration::from_millis(r_ms);
        let now = eta_words(r);
        match eta_word_change(r) {
            None => {
                assert_eq!(now.as_deref(), Some("<5 s left"), "{r_ms}");
                assert_eq!(eta_words(Duration::ZERO), now);
            }
            Some(d) => {
                assert!(d > Duration::ZERO && d <= r, "{r_ms}: {d:?}");
                assert_ne!(eta_words(r - d), now, "{r_ms}: differs at the change");
                let just_before = r - d + Duration::from_millis(1);
                assert_eq!(eta_words(just_before), now, "{r_ms}: holds until it");
                let short = eta_words_short(r);
                assert_ne!(eta_words_short(r - d), short, "{r_ms}: short too");
                assert_eq!(eta_words_short(just_before), short, "{r_ms}: short holds");
            }
        }
    }
}

/// Bytes that stop for STALL_AFTER read "stalled", only once they have
/// moved at all; a count stalls after its own patience.
#[test]
fn stalled_after_ten_seconds_of_no_bytes_and_a_counts_own_patience() {
    let base = t0();
    let mut t = ProgressTrack::default();
    t.observe(base, bytes(0, 1000));
    assert_eq!(
        t.eta(at(base, 60_000)),
        Eta::Hidden,
        "nothing moved yet: not a stall"
    );
    t.observe(at(base, 1000), bytes(10, 1000));
    assert_eq!(t.next_change(at(base, 1000)), Some(at(base, 11_000)));
    assert_eq!(t.eta(at(base, 10_999)), Eta::Hidden);
    assert_eq!(t.eta(at(base, 11_000)), Eta::Stalled);
    t.observe(at(base, 12_000), bytes(20, 1000));
    assert_eq!(t.eta(at(base, 12_000)), Eta::Hidden, "bytes again");
    let mut items = ProgressTrack::default();
    for k in 0..5u64 {
        items.observe(
            at(base, k * 1000),
            Amount {
                unit: Unit::Items,
                ..bytes(k, 100)
            },
        );
    }
    assert_eq!(
        items.eta(at(base, 4_000 + 9_999)),
        Eta::Hidden,
        "items may sit still"
    );
    assert_eq!(items.eta(at(base, 4_000 + 10_000)), Eta::Stalled);
    assert_eq!(
        items.stall_onset(at(base, 4_000)),
        Some(at(base, 14_000)),
        "the onset is a deadline"
    );
    let mut slow = ProgressTrack::default();
    for k in 0..5u64 {
        slow.observe(
            at(base, k * 5000),
            Amount {
                unit: Unit::Steps,
                ..bytes(k, 100)
            },
        );
    }
    assert_eq!(slow.eta(at(base, 20_000 + 19_999)), Eta::Hidden);
    assert_eq!(slow.eta(at(base, 20_000 + 20_000)), Eta::Stalled);
    let mut done = ProgressTrack::default();
    done.observe(base, bytes(1000, 1000));
    assert_eq!(done.eta(at(base, 60_000)), Eta::Hidden);
    assert_eq!(done.next_change(base), None);
}

/// A phase that holds the fill is not a stall: declared
/// [`Unit::HeldSteps`] it never reads stalled and arms no deadline, the
/// estimator keeps its readings across the switch, and when the phase
/// ends the count's patience starts over, so a count that then really
/// stops still says so.
#[test]
fn a_held_phase_never_reads_stalled_and_its_end_restarts_the_patience() {
    let base = t0();
    let steps = |done: u64, unit: Unit| Amount {
        unit,
        ..bytes(done, 1000)
    };
    let mut t = ProgressTrack::default();
    for k in 0..=50u64 {
        t.observe(at(base, k * 100), steps(k * 10, Unit::Steps));
    }
    let samples = t.samples.len();
    for k in 0..=30u64 {
        let now = at(base, 5_000 + k * 2_000);
        t.observe(now, steps(500, Unit::HeldSteps));
        assert_ne!(t.eta(now), Eta::Stalled, "held at {k}");
        assert_eq!(t.stall_onset(now), None, "no deadline while held");
    }
    assert!(t.samples.len() > samples, "the same series: readings kept");
    let resumed = at(base, 66_000);
    t.observe(resumed, steps(500, Unit::Steps));
    assert_ne!(t.eta(resumed), Eta::Stalled);
    assert!(
        t.stall_onset(resumed)
            .is_some_and(|onset| onset >= at(base, 66_000 + 10_000)),
        "the patience starts at the phase's end"
    );
    assert_eq!(t.eta(at(base, 66_000 + 60_000)), Eta::Stalled);
}

/// A new series, a new unit or a regression starts over; a new total
/// keeps the samples and drops the latch.
#[test]
fn a_series_change_or_a_regression_resets_the_estimator() {
    let base = t0();
    let feed = |t: &mut ProgressTrack, series: u64| {
        for k in 0..=60u64 {
            t.observe(
                at(base, k * 100),
                Amount {
                    series,
                    ..bytes(k * 1_000_000, 1_000_000_000)
                },
            );
        }
    };
    let mut t = ProgressTrack::default();
    feed(&mut t, 1);
    assert!(matches!(t.eta(at(base, 6000)), Eta::Remaining(_)));
    assert!(t.samples_len() > 3);
    t.observe(
        at(base, 6100),
        Amount {
            series: 2,
            ..bytes(61_000_000, 1_000_000_000)
        },
    );
    assert_eq!(t.samples_len(), 1, "a new series starts over");
    assert_eq!(t.eta(at(base, 6100)), Eta::Hidden);
    let mut t = ProgressTrack::default();
    feed(&mut t, 1);
    t.observe(at(base, 6100), bytes(1_000, 1_000_000_000));
    assert_eq!(t.samples_len(), 1, "a regression starts over");
    let mut t = ProgressTrack::default();
    feed(&mut t, 1);
    let kept = t.samples_len();
    t.observe(
        at(base, 6100),
        Amount {
            series: 1,
            ..bytes(61_000_000, 2_000_000_000)
        },
    );
    assert_eq!(t.samples_len(), kept, "same sample slot: nothing dropped");
    assert_eq!(t.projections_len(), 0, "a new total drops the votes");
    assert_eq!(t.eta(at(base, 6100)), Eta::Hidden, "…and the latch");
}

/// Ten thousand readings at 10 Hz keep both rings inside their caps.
#[test]
fn the_sample_ring_and_projections_are_bounded() {
    let base = t0();
    let mut t = ProgressTrack::default();
    for k in 0..10_000u64 {
        t.observe(at(base, k * 100), bytes(k * 1000, u64::MAX / 2));
        assert!(t.samples_len() <= SAMPLES_CAP);
        assert!(t.projections_len() <= PROJECTIONS_CAP);
    }
    let mut burst = ProgressTrack::default();
    for k in 0..10_000u64 {
        burst.observe(at(base, k), bytes(k, 20_000));
    }
    assert!(burst.samples_len() <= SAMPLES_CAP);
}

/// Elapsed words: nothing under ten seconds, then `for N s`, `for N min`,
/// `for N h` (never a bare clock), changing exactly at each boundary, the
/// short form at the same instants.
#[test]
fn elapsed_words_are_labelled_and_follow_their_cadence() {
    let w = |s: u64| elapsed_words(Duration::from_secs(s), false);
    let short = |s: u64| elapsed_words(Duration::from_secs(s), true);
    assert_eq!(w(9), None);
    assert_eq!(short(9), None);
    for (s, long, sh) in [
        (10, "for 10 s", "for 10s"),
        (41, "for 41 s", "for 41s"),
        (59, "for 59 s", "for 59s"),
        (60, "for 1 min", "for 1m"),
        (199, "for 3 min", "for 3m"),
        (3599, "for 59 min", "for 59m"),
        (3600, "for 1 h", "for 1h"),
        (35_999, "for 9 h", "for 9h"),
        (36_000, "for 10 h", "for 10h"),
        (99_999_999, "for 999 h", "for 999h"),
    ] {
        assert_eq!(w(s).as_deref(), Some(long), "{s}");
        assert_eq!(short(s).as_deref(), Some(sh), "{s}");
    }
    let clock = |x: &str| {
        let mut parts = x.split(':');
        matches!((parts.next(), parts.next(), parts.next()),
            (Some(a), Some(b), None) if !a.is_empty() && a.chars().all(|c| c.is_ascii_digit())
                && b.len() == 2 && b.chars().all(|c| c.is_ascii_digit()))
    };
    for e_ms in (0..=7_300_000u64)
        .step_by(250)
        .chain((35_000_000..=37_000_000u64).step_by(5_000))
    {
        let e = Duration::from_millis(e_ms);
        let d = elapsed_word_change(e).expect("below 999 h");
        assert!(d > Duration::ZERO, "{e_ms}");
        for sh in [false, true] {
            assert_ne!(
                elapsed_words(e + d, sh),
                elapsed_words(e, sh),
                "{e_ms}: changes at"
            );
            assert_eq!(
                elapsed_words(e + d - Duration::from_millis(1), sh),
                elapsed_words(e, sh),
                "{e_ms}: holds until"
            );
        }
        if let Some(words) = elapsed_words(e, false) {
            assert!(words.chars().count() <= crate::ELAPSED_W, "{words}");
            assert!(!clock(&words), "{words}");
        }
        if let Some(words) = elapsed_words(e, true) {
            assert!(words.chars().count() <= crate::ELAPSED_SHORT_W, "{words}");
            assert!(!clock(&words), "{words}");
        }
    }
    let just_below_cap = Duration::from_millis(999 * 3_600_000 - 1);
    assert_eq!(
        elapsed_word_change(just_below_cap),
        Some(Duration::from_millis(1))
    );
    assert_eq!(
        elapsed_words(just_below_cap, false).as_deref(),
        Some("for 998 h")
    );
    assert_eq!(
        elapsed_words(Duration::from_secs(999 * 3600), false).as_deref(),
        Some("for 999 h")
    );
    assert_eq!(elapsed_word_change(Duration::from_secs(999 * 3600)), None);
    assert_eq!(elapsed_word_change(Duration::from_secs(1000 * 3600)), None);
}
