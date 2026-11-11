// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::Duration;

const fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn the_first_key_after_a_long_green_run_is_born_hot() {
    let t = Instant::now();

    let mut s = Spine::new();
    s.update(t);
    assert_eq!(
        s.birth_disp(),
        0.0,
        "the shipped identity: with no verdict, nothing is floored"
    );

    s.note_verdict(t);
    s.update(t + ms(200));
    assert!(s.verdict_open(t + ms(200)), "the door is open");
    assert_eq!(
        s.birth_disp(),
        VERDICT_BIRTH_DISP,
        "the first key after a long green run is born hot"
    );

    let price = s.birth_disp();
    s.note_keys(t + ms(200), 1, price);
    assert_eq!(
        s.birth_disp(),
        0.0,
        "the door is spent by the key that walked through it — the SECOND \
         key prices itself honestly"
    );

    let mut cold = Spine::new();
    cold.note_verdict(t);
    cold.update(t + Duration::from_secs_f32(VERDICT_WINDOW_S + 0.5));
    assert_eq!(
        cold.birth_disp(),
        0.0,
        "five seconds later the build is not news any more"
    );
}

fn held(s: &mut Spine, from: Instant, to: Instant, step: Duration, target: f32) {
    let floor = target / METRIC_GAIN;
    let mut t = from;
    while t + step <= to {
        t += step;
        s.drive(t, floor);
        s.update(t);
    }
    if t < to {
        s.drive(to, floor);
        s.update(to);
    }
}

#[test]
fn a_big_rise_slams_faster_than_it_attacks_and_the_slam_is_latched() {
    let t0 = Instant::now();
    let step = ms(20);

    let slam = {
        let mut x = Spine::new();
        x.update(t0);
        x.drive(t0, 1.0);
        x.update(t0 + step);
        x.disp()
    };
    let attack = {
        let mut x = Spine::new();
        x.update(t0);
        x.drive(t0, 0.4 / METRIC_GAIN);
        x.update(t0 + step);
        x.disp() / 0.4
    };
    assert!(
        slam > attack,
        "slam {slam} should cover more of its gap than attack {attack}"
    );

    let mut x = Spine::new();
    x.update(t0);
    held(&mut x, t0, t0 + ms(60), ms(10), 1.0);
    let gap = 1.0 - x.disp();
    assert!(
        gap < DISP_SLAM_RISE,
        "the remaining gap is under the threshold"
    );
    x.drive(t0 + ms(70), 1.0);
    x.update(t0 + ms(70));
    let covered = (1.0 - x.disp()) / gap;
    let slam_rate = (-0.010f32 / DISP_SLAM_S).exp();
    let attack_rate = (-0.010f32 / DISP_ATTACK_S).exp();
    assert!(
        (covered - slam_rate).abs() < 1e-4 && (covered - attack_rate).abs() > 1e-3,
        "the step past the threshold must still be a slam step: {covered}"
    );
}

#[test]
fn idle_drains_to_exactly_zero() {
    let t0 = Instant::now();
    let mut s = Spine::new();
    s.update(t0);
    let mut t = t0;
    for _ in 0..20 {
        t += ms(30);
        s.advance(t);
        s.update(t);
    }
    assert!(s.disp() > 0.0, "warm before the pause");
    assert!(!s.at_rest(t));

    for _ in 0..60 {
        t += ms(1000);
        s.update(t);
    }
    assert_eq!(s.disp(), 0.0, "the follower must reach exact zero");
    assert_eq!(s.disp_peak(), 0.0, "so must the resume memory");
    assert_eq!(s.birth_disp(), 0.0);
    assert!(s.at_rest(t), "idle → zero (T6)");
    t += ms(16);
    s.update(t);
    assert_eq!(s.disp(), 0.0);
}

#[test]
fn deletes_drain_and_never_build() {
    let t0 = Instant::now();
    let mut s = Spine::new();
    s.update(t0);
    let mut t = t0;
    for _ in 0..30 {
        t += ms(30);
        s.advance(t);
        s.update(t);
    }
    let warm = s.momentum(t);

    let mut deleted = s;
    t += ms(30);
    deleted.drain_delete(t);
    let after_delete = deleted.momentum(t);

    let mut killed = s;
    killed.drain_kill(t);
    let after_kill = killed.momentum(t);

    assert!(after_delete < warm, "a delete must not build");
    assert!(
        after_kill < after_delete,
        "a kill un-earns harder than a delete: {after_kill} vs {after_delete}"
    );
}

#[test]
fn resume_floors_births_without_touching_the_honest_spine() {
    let t0 = Instant::now();
    let mut s = Spine::new();
    s.update(t0);
    let mut t = t0;
    for _ in 0..40 {
        t += ms(25);
        s.advance(t);
        s.update(t);
    }
    let peak = s.disp();
    for _ in 0..25 {
        t += ms(100);
        s.update(t);
    }
    assert!(s.disp() < peak, "the honest spine decayed");
    assert!(
        s.birth_disp() > s.disp(),
        "the resume memory must floor the birth price"
    );
    assert!(
        s.birth_disp() >= DISP_PEAK_FLOOR_SHARE * s.disp_peak() - 1e-6,
        "the floor is exactly {DISP_PEAK_FLOOR_SHARE} of the decayed peak"
    );
}

#[test]
fn the_follower_is_frame_rate_independent() {
    let t0 = Instant::now();
    let end = t0 + ms(600);
    let run_held = |hz: u64| {
        let mut s = Spine::new();
        s.update(t0);
        held(&mut s, t0, end, ms(1000 / hz), 1.0);
        s.disp()
    };
    let (a, b) = (run_held(60), run_held(120));
    assert!(
        (a - b).abs() < 1e-4,
        "held target: 60 Hz {a} and 120 Hz {b} must agree at the same wall time"
    );

    let run_decaying = |hz: u64| {
        let step = 1000 / hz;
        let mut s = Spine::new();
        s.update(t0);
        s.drive(t0, 1.0);
        let mut t = t0;
        while t + ms(step) <= end {
            t += ms(step);
            s.update(t);
        }
        s.update(end);
        s.disp()
    };
    let (a, b) = (run_decaying(60), run_decaying(120));
    assert!(
        (a - b).abs() < 5e-3,
        "decaying target: 60 Hz {a} and 120 Hz {b} must agree at the same wall time"
    );
}

#[test]
fn the_follower_settles_on_the_gained_metric_and_reports_the_raw_one() {
    let t0 = Instant::now();
    for (metric, want) in [(0.5f32, 0.5 * METRIC_GAIN), (0.9, 1.0)] {
        let mut s = Spine::new();
        s.update(t0);
        let mut t = t0;
        for _ in 0..100 {
            t += ms(10);
            s.drive(t, metric);
            s.update(t);
        }
        assert!(
            (s.disp() - want).abs() < 1e-3,
            "metric {metric} held for a second: disp {}, want {want}",
            s.disp()
        );
        assert!(
            (s.momentum(t) - metric).abs() < 1e-6,
            "the raw metric is reported raw: {} vs {metric}",
            s.momentum(t)
        );
    }

    let mut tiny = Spine::new();
    tiny.drive(t0, 0.001);
    tiny.update(t0);
    assert!(tiny.disp() > 0.0 && tiny.disp() < DISP_SNAP_ZERO);
    // Hold the first tick's target exactly: no spring step is needed, but the snap is.
    let next = t0 + ms(16);
    tiny.drive(next, 0.001);
    tiny.update(next);
    assert_eq!(tiny.disp(), 0.0, "a steady sub-snap target reaches zero");
}

#[test]
fn a_backspace_leaves_a_mark_that_counts_the_run_and_a_typed_key_closes_it() {
    let t0 = Instant::now();
    let mut s = Spine::new();
    s.update(t0);
    let mut t = t0;
    for _ in 0..16 {
        t += ms(125);
        s.advance(t);
        s.update(t);
    }
    assert_eq!(s.erase_mark(), None, "typing leaves no mark");
    let interrupted = s.birth_disp();
    let metric_before = s.momentum(t + ms(125));

    let mut twin = s;
    t += ms(125);
    s.drain_delete(t);
    twin.drain_delete(t);
    let mark = s.erase_mark().expect("a delete leaves the mark");
    assert_eq!(mark.at, t);
    assert_eq!(mark.count, 1);
    assert!(
        (mark.disp - interrupted).abs() < 1e-6,
        "the mark carries the birth spine the delete interrupted: {} vs {interrupted}",
        mark.disp
    );
    assert!(
        s.momentum(t) < metric_before,
        "the delete still drains the metric"
    );
    assert_eq!(s.mend(t + ms(125)), Some(mark), "one delete is a mend");
    assert_eq!(
        s.mend(t + Duration::from_secs_f32(MEND_WINDOW_S + 0.01)),
        None,
        "…until the window closes"
    );

    s.update(t + ms(125));
    t += ms(125);
    s.drain_delete(t);
    let second = s.erase_mark().expect("still marked");
    assert_eq!((second.count, second.at), (2, t));
    assert!(
        (second.disp - interrupted).abs() < 1e-6,
        "the run keeps the momentum its first delete interrupted"
    );
    assert!(s.mend(t + ms(125)).is_some(), "two deletes are a mend");

    t += ms(125);
    s.drain_delete(t);
    assert_eq!(s.erase_mark().map(|m| m.count), Some(3));
    assert_eq!(s.mend(t), None, "three deletes are an edit");

    t += ms(125);
    s.advance(t);
    assert_eq!(s.erase_mark(), None, "a typed key clears the mark");

    t += ms(125);
    twin.drain_kill(t);
    assert_eq!(twin.erase_mark(), None, "a kill is an edit, not a typo");

    let mut stale = Spine::new();
    stale.update(t0);
    stale.drain_delete(t0);
    stale.update(t0 + ms(2000));
    stale.drain_delete(t0 + ms(2000));
    assert_eq!(stale.erase_mark().map(|m| m.count), Some(1));

    let mut a = Spine::new();
    let mut b = Spine::new();
    a.update(t0);
    b.update(t0);
    for i in 1..=8u32 {
        let at = t0 + ms(125) * i;
        a.advance(at);
        b.advance(at);
        a.update(at);
        b.update(at);
    }
    let at = t0 + ms(125 * 9);
    a.drain_delete(at);
    b.drain_delete(at);
    let _ = a.mend(at);
    a.update(at + ms(125));
    b.update(at + ms(125));
    assert_eq!(a.disp(), b.disp());
    assert_eq!(a.momentum(at + ms(125)), b.momentum(at + ms(125)));
}

fn typing(s: &mut Spine, t0: Instant, n: u32, period: Duration) -> Vec<(Duration, Flow)> {
    let tick = Duration::from_micros(8_333);
    let mut out = Vec::new();
    let mut t = t0;
    s.update(t0);
    for k in 1..=n {
        let key = t0 + period * k;
        while t + tick <= key {
            t += tick;
            s.update(t);
        }
        t = key;
        s.advance(t);
        s.update(t);
        let price = s.birth_disp();
        s.note_keys(t, 1, price);
        out.push((key.saturating_duration_since(t0), s.flow()));
    }
    out
}

#[test]
fn a_held_key_cannot_farm_the_combo() {
    let t0 = Instant::now();
    let mut s = Spine::new();
    let earned = typing(&mut s, t0, 40, ms(83));
    let held = earned.last().expect("keys").1;
    assert!(held.open(), "the fixture must reach flow first: {held:?}");

    let mut t = t0 + ms(83) * 40;
    let mut repeats = 0u32;
    for _ in 0..90 {
        t += ms(33);
        s.drive(t, 1.0);
        s.advance(t);
        s.update(t);
        let price = s.birth_disp();
        s.note_keys(t, 1, price);
        repeats += 1;
    }
    assert_eq!(
        s.flow().combo,
        held.combo,
        "{repeats} auto-repeat keys under a celebration moved the combo from {} to {}",
        held.combo,
        s.flow().combo
    );
    assert_eq!(s.flow().best, held.best, "…and the high-water mark with it");

    let after = t + Duration::from_secs_f32(FLOW_FREEZE_S) + ms(1);
    s.advance(after);
    s.update(after);
    let price = s.birth_disp();
    s.note_keys(after, 1, price);
    assert_eq!(s.flow().combo, held.combo + 1, "the freeze never lifted");
}

#[test]
fn the_phase_clock_rides_the_spine_and_stays_on_its_ring() {
    let t0 = Instant::now();
    let mut cold = Spine::new();
    cold.update(t0);
    cold.update(t0 + ms(500));
    assert_eq!(cold.phase(), 0.0, "a cold spine freezes the clock");

    let mut hot = Spine::new();
    hot.update(t0);
    hot.drive(t0, 1.0);
    let mut t = t0;
    for _ in 0..200 {
        t += ms(16);
        hot.update(t);
    }
    assert!(hot.phase() > 0.0, "a lit spine advances the clock");
    assert!(
        (0.0..PHASE_RING).contains(&hot.phase()),
        "phase must stay on the ring"
    );
}
