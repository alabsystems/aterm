// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The needle's spring and its frames (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §4, §6),
//! stepped at explicit instants. The live edge's half (motion, stale, the grid's cap) is
//! `stats_live_tests.rs`; the dial's patch is `stats_live_paint_tests.rs`.

use super::*;
use crate::stats_gauge::tip_px_per_deg;

const MS: Duration = Duration::from_millis(1);

/// Full's dial at 2× and Folded's at 1×: the finest and the coarsest tip a window draws.
fn views(interval: Duration) -> [View; 2] {
    [
        View {
            px_per_deg: tip_px_per_deg(crate::stats_gauge::R_FACE, 2.0),
            interval,
        },
        View {
            px_per_deg: tip_px_per_deg(crate::stats_plan::FOLD_GAUGE_R, 1.0),
            interval,
        },
    ]
}

/// A needle at rest on `from` retargeted at `t0` to `to` (degrees), with a soak that holds.
fn swing(t0: Instant, from: f64, to: f64, px_per_deg: f64) -> Needle {
    let rest = Needle::at_rest(t0, (from, 0.2));
    Needle::retarget(Some(rest), t0, Some((to, 0.2)), px_per_deg).expect("a needle")
}

fn secs(s: f64) -> Duration {
    Duration::from_secs_f64(s)
}

#[test]
fn a_step_from_rest_settles_monotonically_and_never_overshoots() {
    let t0 = Instant::now();
    for (from, to) in [(START, 12.0), (12.0, -100.0), (-60.0, END), (100.0, 80.0)] {
        let n = swing(t0, from, to, 1.0);
        let way = |u: f64| {
            let x = n.tip.at(u);
            (x - from) / (to - from)
        };
        let mut last = 0.0;
        for i in 0..=2000 {
            let w = way(f64::from(i) * 1e-3);
            assert!(w >= last - 1e-12, "{from} to {to}: back at {i} ms");
            assert!(
                w <= 1.0 + 1e-12,
                "{from} to {to}: past the target at {i} ms"
            );
            last = w;
        }
        assert!(way(0.25) >= 0.80, "{from} to {to}: {} at 250 ms", way(0.25));
        assert!(way(0.56) >= 0.99, "{from} to {to}: {} at 560 ms", way(0.56));
        assert!(
            way(0.20) < 0.80,
            "CONTROL {from} to {to}: not there at once"
        );
        assert_eq!(n.target(), (to, 0.2));
    }
}

#[test]
fn a_retarget_carries_the_angle_and_its_velocity() {
    let t0 = Instant::now();
    let n = swing(t0, START, 60.0, 1.0);
    // A reading lands 500 ms after the last (the lens's cadence), mid-sweep, toward a lower
    // target: the needle turns from where it stands, at the speed it had.
    for (after, to) in [(0.12, -40.0), (0.5, 100.0), (0.05, START)] {
        let t1 = t0 + secs(after);
        let next = Needle::retarget(Some(n), t1, Some((to, 0.5)), 1.0).expect("a needle");
        let [x, v, s, _] = n.sample(t1);
        let [x1, v1, s1, _] = next.sample(t1);
        assert!((x - x1).abs() < 1e-9, "{after}: {x} then {x1}");
        assert!((v - v1).abs() < 1e-9, "{after}: {v} then {v1} deg/s");
        assert!((s - s1).abs() < 1e-9);
        assert!(
            v.abs() > 1.0,
            "CONTROL {after}: the needle moved at {v} deg/s"
        );
        assert!(
            (x1 - 60.0).abs() > 1.0,
            "CONTROL {after}: not from the last reading's value"
        );
        // A moment on, it has not jumped either.
        let [x2, ..] = next.sample(t1 + MS);
        assert!(
            (x2 - x1).abs() < v.abs() * 2e-3 + 1e-6,
            "{after}: {x1} to {x2}"
        );
    }
}

#[test]
fn the_needle_stays_inside_its_stops() {
    let t0 = Instant::now();
    // Fast toward the end with the target just short of it: the spring would carry it past.
    let n = Needle {
        t0,
        tip: Channel {
            x0: 100.0,
            v0: 1500.0,
            target: 130.0,
            lo: START,
            hi: END,
        },
        soak: Channel::rest(0.0, 0.0, 1.0),
    };
    let over = (0..1000).any(|i| n.tip.raw(f64::from(i) * 1e-3) > END);
    assert!(over, "CONTROL: the spring alone passes the stop");
    for i in 0..1000 {
        let u = f64::from(i) * 1e-3;
        assert!((START..=END).contains(&n.tip.at(u)), "{i} ms");
    }
    // Pinned at the stop it moves no faster than the stop, and a retarget starts it from rest
    // there.
    let at_stop = (0..1000)
        .map(|i| t0 + MS * i)
        .find(|&t| n.sample(t)[0] >= END)
        .expect("it reaches the stop");
    let [x, v, ..] = n.sample(at_stop);
    assert_eq!((x, v), (END, 0.0));
    let next = Needle::retarget(Some(n), at_stop, Some((0.0, 0.0)), 1.0).expect("a needle");
    assert_eq!((next.tip.x0, next.tip.v0), (END, 0.0));
    // A soak sweeping down stays at or above nothing.
    let s = Needle {
        soak: Channel {
            x0: 0.1,
            v0: -5.0,
            target: 0.0,
            lo: 0.0,
            hi: 1.0,
        },
        ..n
    };
    assert!((0..1000).all(|i| s.soak.at(f64::from(i) * 1e-3) >= 0.0));
}

/// The first moment a needle swung from rest stands within half a pixel of its target (from rest
/// it closes on the target monotonically, so it stays).
fn settles_at(n: &Needle, view: View) -> f64 {
    let step = 1.0 / view.px_per_deg;
    (0..20_000)
        .map(|i| f64::from(i) * 1e-4)
        .find(|&u| (n.tip.at(u) - n.tip.target).abs() < step / 2.0)
        .expect("it settles within two seconds")
}

/// Every frame an episode asks moves the tip a whole device pixel at least, no sooner than the
/// window's interval after the last, from rest always toward the target; the last draws the
/// reading itself and asks for nothing more, by the time the spring has settled to half a pixel.
/// Between frames the needle stands as at the last.
#[test]
fn every_needle_frame_moves_the_tip_a_whole_pixel_and_none_once_settled() {
    let t0 = Instant::now();
    for interval in [
        MS * 8,
        Duration::from_micros(16_667),
        Duration::from_micros(33_333),
    ] {
        for view in views(interval) {
            for (from, to) in [
                (START, END),
                (-20.0, 40.0),
                (10.0, 13.0),
                (10.0, 10.0 + 2.0 / view.px_per_deg),
                (90.0, -90.0),
            ] {
                let at = format!("{from}° to {to}° at {} px/°, {interval:?}", view.px_per_deg);
                let n = swing(t0, from, to, view.px_per_deg);
                let mut f = n.frame_at(view, t0);
                let mut drawn = vec![n.drawn(view, f)];
                let mut frames = 0_u32;
                while let Some(next) = n.next_after(view, f) {
                    let g = n.frame_at(view, next);
                    assert!(
                        g.at_ns >= f.at_ns + u64::try_from(interval.as_nanos()).unwrap_or(0),
                        "{at}: frames {} ns apart",
                        g.at_ns - f.at_ns
                    );
                    assert_eq!(
                        n.frame_at(view, next - Duration::from_nanos(1)),
                        f,
                        "{at}: the needle moved between frames"
                    );
                    let moved = g.k.0 - f.k.0;
                    assert!(
                        moved != 0,
                        "{at}: a frame that moves the tip no pixel: {f:?} {g:?}"
                    );
                    assert_eq!(
                        moved.signum(),
                        -f.k.0.signum(),
                        "{at}: a frame that moves away from the target"
                    );
                    drawn.push(n.drawn(view, g));
                    f = g;
                    frames += 1;
                    assert!(frames < 1000, "{at}: the episode never ends");
                }
                assert_eq!(
                    drawn.last(),
                    Some(&None),
                    "{at}: the last frame draws the reading"
                );
                assert_eq!(
                    n.next_after(view, f),
                    None,
                    "{at}: settled, it asks nothing"
                );
                assert_eq!(n.frame_at(view, t0 + secs(5.0)), f, "{at}");
                let settle = settles_at(&n, view);
                let last = f.at_ns as f64 / 1e9;
                assert!(
                    last <= settle + interval.as_secs_f64() + 1e-6,
                    "{at}: its last frame at {last} s, settled at {settle} s"
                );
                let bound = (settle / interval.as_secs_f64()).ceil() as u32 + 1;
                assert!(frames <= bound, "{at}: {frames} frames, at most {bound}");
                // Each drawn angle, as the live edge carries it, steps a pixel within its eighth of
                // a degree.
                let angle =
                    |d: &Option<(i32, i32)>| d.map_or(to, |(a, _)| f64::from(a) / ANGLE_PER_DEG);
                for w in drawn.windows(2) {
                    let px = (angle(&w[1]) - angle(&w[0])).abs() * view.px_per_deg;
                    assert!(
                        px >= 1.0 - view.px_per_deg / ANGLE_PER_DEG,
                        "{at}: {px} px between {:?} and {:?}",
                        w[0],
                        w[1]
                    );
                }
            }
        }
    }
}

/// A large swing runs at the display's rate: its frames come an interval apart while the tip
/// moves more than a pixel an interval, so the needle is drawn as often as the window presents.
#[test]
fn a_large_swing_runs_at_the_displays_rate_and_a_small_one_takes_a_few_frames() {
    let t0 = Instant::now();
    let interval = Duration::from_micros(16_667);
    let [view, _] = views(interval);
    let frames_of = |n: &Needle| {
        let mut f = n.frame_at(view, t0);
        let mut at = vec![f.at_ns];
        while let Some(next) = n.next_after(view, f) {
            f = n.frame_at(view, next);
            at.push(f.at_ns);
        }
        at
    };
    let big = frames_of(&swing(t0, START, END, view.px_per_deg));
    let paced = big
        .windows(2)
        .take(10)
        .all(|w| w[1] - w[0] == u64::try_from(interval.as_nanos()).unwrap_or(0));
    assert!(
        paced,
        "a full sweep's first frames are an interval apart: {big:?}"
    );
    let small = frames_of(&swing(
        t0,
        10.0,
        10.0 + 3.0 / view.px_per_deg,
        view.px_per_deg,
    ));
    assert!(
        (2..=4).contains(&small.len()),
        "a 3 px move takes its own frame and three more at most: {small:?}"
    );
}

/// A move under a device pixel at the finest tip rests on its target at once and asks nothing;
/// so does the first reading, and a reading no window animates a dial for (`px_per_deg` 0). A
/// soak that moves less than a step rests too, while the tip still sweeps.
#[test]
fn the_dead_band_and_the_first_reading_rest_on_the_target() {
    let t0 = Instant::now();
    let [view, _] = views(Duration::from_micros(16_667));
    let px = view.px_per_deg;
    let under = swing(t0, 10.0, 10.0 + 0.9 / px, px);
    assert_eq!(under.tip.x0, under.tip.target, "under a pixel: at rest");
    assert_eq!(under.drawn(view, under.frame_at(view, t0)), None);
    assert_eq!(under.next_after(view, under.frame_at(view, t0)), None);
    let over = swing(t0, 10.0, 10.0 + 1.5 / px, px);
    assert!(
        over.next_after(view, over.frame_at(view, t0)).is_some(),
        "CONTROL: a pixel and a half moves"
    );
    let nobody = swing(t0, START, END, 0.0);
    assert_eq!(nobody.tip.x0, END, "no window animates a dial: at rest");
    let first = Needle::retarget(None, t0, Some((40.0, 0.5)), px).expect("a needle");
    assert_eq!(first, Needle::at_rest(t0, (40.0, 0.5)));
    assert_eq!(
        Needle::retarget(Some(over), t0, None, px),
        None,
        "nothing read"
    );
    // The soak's dead-band is its own.
    let rest = Needle::at_rest(t0, (0.0, 0.30));
    let calm = Needle::retarget(Some(rest), t0, Some((60.0, 0.30 + SOAK_STEP / 2.0)), px)
        .expect("a needle");
    assert_eq!(
        calm.soak.x0, calm.soak.target,
        "a soak under a step: at rest"
    );
    assert_ne!(calm.tip.x0, calm.tip.target, "CONTROL: the tip sweeps");
    let warm = Needle::retarget(Some(rest), t0, Some((0.0, 0.30 + 4.0 * SOAK_STEP)), px)
        .expect("a needle");
    assert_eq!(
        warm.tip.x0, warm.tip.target,
        "the tip under a pixel: at rest"
    );
    let mut f = warm.frame_at(view, t0);
    assert_eq!(f.k, (0, -4), "the bezel eases from four steps back");
    let mut soaks = vec![f.k.1];
    while let Some(next) = warm.next_after(view, f) {
        f = warm.frame_at(view, next);
        assert_eq!(f.k.0, 0);
        soaks.push(f.k.1);
    }
    assert!(soaks.windows(2).all(|w| w[1] > w[0]), "{soaks:?}");
    assert_eq!(soaks.last(), Some(&0));
    // A snapped needle rests on its target.
    let snapped = over.snapped(t0 + MS * 30);
    assert_eq!(snapped.target(), over.target());
    assert_eq!(
        snapped.next_after(view, snapped.frame_at(view, t0 + MS * 40)),
        None
    );
}

/// The frames a reading's episode owes a window (`needle_frames_owed`, what the `measure` spin
/// row bounds the needle's deadlines by) are the frames a window draws after the reading's own,
/// to rest: none where it rests at once (under the dead-band, the first reading, a reading no
/// window animates, a snapped needle), some where a tip or a soak sweeps or a reading lands
/// mid-sweep.
#[test]
fn a_readings_owed_frames_are_the_frames_a_window_draws_after_its_own() {
    let t0 = Instant::now();
    let [view, _] = views(Duration::from_micros(16_667));
    let px = view.px_per_deg;
    let drawn_after = |n: &Needle| {
        let mut f = n.frame_at(view, n.t0);
        let mut frames = 0;
        while let Some(next) = n.next_after(view, f) {
            f = n.frame_at(view, next);
            frames += 1;
        }
        frames
    };
    let rest = Needle::at_rest(t0, (10.0, 0.30));
    let moving = swing(t0, START, END, px);
    let cases = [
        ("under a pixel", swing(t0, 10.0, 10.0 + 0.9 / px, px), false),
        (
            "a pixel and a half",
            swing(t0, 10.0, 10.0 + 1.5 / px, px),
            true,
        ),
        ("the whole dial", moving, true),
        (
            "no window animates a dial",
            swing(t0, START, END, 0.0),
            false,
        ),
        (
            "the first reading",
            Needle::retarget(None, t0, Some((40.0, 0.5)), px).expect("a needle"),
            false,
        ),
        (
            "the soak alone",
            Needle::retarget(Some(rest), t0, Some((10.0, 0.30 + 4.0 * SOAK_STEP)), px)
                .expect("a needle"),
            true,
        ),
        (
            "mid-sweep, onto where it stands",
            Needle::retarget(
                Some(moving),
                t0 + MS * 100,
                Some((moving.sample(t0 + MS * 100)[0], 0.2)),
                px,
            )
            .expect("a needle"),
            true,
        ),
        ("snapped", moving.snapped(t0), false),
    ];
    for (what, n, sweeps) in cases {
        let owed = n.frames_owed(view, None);
        assert_eq!(owed, drawn_after(&n), "{what}: owed as drawn");
        assert_eq!(owed > 0, sweeps, "{what}: {owed} owed");
    }
}

/// What a window draws at each millisecond from `from` to `to` after `n`'s retarget, recording
/// each frame as the splice does (`WindowState::deck_needle_drawn`), in `view(ms)`: the tip's
/// angle it draws, degrees, each millisecond.
fn drawn_each_ms(
    n: &Needle,
    view: impl Fn(u64) -> View,
    last: &mut Option<Drawn>,
    (from, to): (u64, u64),
) -> Vec<f64> {
    (from..=to)
        .map(|ms| {
            let d = n.follow(view(ms), *last, n.t0 + MS * u32::try_from(ms).unwrap_or(0));
            *last = Some(d);
            d.at.0
        })
        .collect()
}

/// How far `angles` ever step back, device px at `px_per_deg`, against the way `to` lies.
fn worst_back(angles: &[f64], to: f64, px_per_deg: f64) -> f64 {
    angles
        .windows(2)
        .map(|w| (w[0] - w[1]) * (to - w[0]).signum() * px_per_deg)
        .fold(0.0, f64::max)
}

/// A window whose effect interval changes mid-sweep (a display of another rate, a parked effect
/// lane) walks its frames on from the one it drew: the needle never steps back, and the sweep
/// still ends on the reading and asks nothing more. Walked again from the retarget at the new
/// interval, as a window did before it kept what it drew, the needle stepped back up to 16 px
/// (the CONTROL).
#[test]
fn a_change_of_interval_mid_sweep_never_takes_the_needle_back() {
    let t0 = Instant::now();
    let (fast, slow) = (Duration::from_micros(16_667), Duration::from_micros(33_333));
    let mut control = 0.0_f64;
    for (a, b) in [(fast, slow), (slow, fast), (MS * 8, slow)] {
        for view_a in views(a) {
            let view_b = View {
                interval: b,
                ..view_a
            };
            for (from, to) in [(-120.0, 120.0), (0.0, 60.0), (100.0, -30.0)] {
                let n = swing(t0, from, to, view_a.px_per_deg);
                for switch in [20_u64, 55, 84, 150, 300] {
                    let at = format!("{from}° to {to}°, {a:?} then {b:?} at {switch} ms");
                    let view = |ms: u64| if ms < switch { view_a } else { view_b };
                    let mut last = None;
                    let angles = drawn_each_ms(&n, view, &mut last, (0, 1500));
                    let back = worst_back(&angles, to, view_a.px_per_deg);
                    assert!(back <= 1e-6, "{at}: the needle stepped back {back} px");
                    let end = last.and_then(|d| d.frame).expect("a frame");
                    assert_eq!(end.k, (0, 0), "{at}: the sweep ends on the reading");
                    assert_eq!(n.next_after(view_b, end), None, "{at}: and asks nothing");
                    // The window as it walked before: the frame at each moment from the
                    // retarget's, at whatever interval it has then.
                    let walked: Vec<f64> = (0..=1500_u64)
                        .map(|ms| {
                            let v = view(ms);
                            let f = n.frame_at(v, t0 + MS * u32::try_from(ms).unwrap_or(0));
                            n.drawn(v, f)
                                .map_or(to, |(a, _)| f64::from(a) / ANGLE_PER_DEG)
                        })
                        .collect();
                    control = control.max(worst_back(&walked, to, view_a.px_per_deg));
                }
            }
        }
    }
    assert!(
        control > 10.0,
        "CONTROL: walked again from the retarget the needle stepped back {control} px"
    );
}

/// A window that drew a sweep at rest on its reading (without motion, or once readings stopped)
/// keeps drawing the reading to the sweep's end when its motion comes back: it never draws the
/// needle back where it stood at the reading and sweeps it up again. A window new to the sweep
/// draws it where the spring stands (the CONTROL).
#[test]
fn a_window_that_drew_a_sweep_at_rest_keeps_the_reading() {
    let t0 = Instant::now();
    let [view, _] = views(Duration::from_micros(16_667));
    let n = swing(t0, -120.0, 120.0, view.px_per_deg);
    let rested = n.rested();
    assert_eq!((rested.frame, rested.at), (None, n.target()));
    for ms in [0_u32, 100, 200, 400, 2000] {
        let now = t0 + MS * ms;
        assert_eq!(n.follow(view, Some(rested), now), rested, "{ms} ms");
    }
    let fresh = n.follow(view, None, t0 + MS * 200);
    assert!(
        fresh.frame.is_some_and(|f| f.k.0 != 0) && fresh.at.0 < 100.0,
        "CONTROL: new to the sweep, a window draws it mid-way: {fresh:?}"
    );
    // A rest drawn of an earlier sweep keeps nothing of this one.
    let earlier = swing(t0 - MS * 1000, 0.0, 10.0, view.px_per_deg).rested();
    assert_eq!(
        n.follow(view, Some(earlier), t0 + MS * 200).t0,
        n.t0,
        "a new sweep is the window's own"
    );
}

/// A reading that lands mid-sweep, further the same way or turning it back, never steps the
/// needle back as the window drew it: the new sweep's first frame is moved a step on where its
/// rounding, counted from the new target, would draw the tip behind what the window last drew,
/// and no later frame draws it against the way the spring moves. Every sweep still ends on its
/// reading. Rounded from the new target alone, a same-way reading stepped the tip back almost a
/// pixel (the CONTROL).
#[test]
fn a_reading_that_lands_mid_sweep_never_steps_the_needle_back() {
    let t0 = Instant::now();
    let mut control = 0.0_f64;
    for interval in [Duration::from_micros(16_667), Duration::from_micros(33_333)] {
        for view in views(interval) {
            let step = 1.0 / view.px_per_deg;
            let n = swing(t0, 0.0, 60.0, view.px_per_deg);
            let mut last = None;
            let before = drawn_each_ms(&n, |_| view, &mut last, (0, 299));
            assert!(worst_back(&before, 60.0, view.px_per_deg) <= 1e-6);
            for ms in (300..1000_u32).step_by(7) {
                let t1 = t0 + MS * ms;
                // What the window drew of the first sweep up to the reading.
                let mut was = last;
                let _ = drawn_each_ms(&n, |_| view, &mut was, (300, u64::from(ms)));
                let shown = was.expect("a frame").at.0;
                for j in [1_u32, 5, 17, 36] {
                    let to = 60.0 + f64::from(j) * step / 37.0;
                    let at = format!("{view:?}: reading at {ms} ms to {to}°");
                    let m = Needle::retarget(Some(n), t1, Some((to, 0.0)), view.px_per_deg)
                        .expect("a needle");
                    let mut drew = was;
                    let after = drawn_each_ms(&m, |_| view, &mut drew, (0, 1500));
                    let mut angles = vec![shown];
                    angles.extend(after);
                    let back = worst_back(&angles, to, view.px_per_deg);
                    assert!(back <= 1e-6, "{at}: the needle stepped back {back} px");
                    let end = drew.and_then(|d| d.frame).expect("a frame");
                    assert_eq!(end.k, (0, 0), "{at}: the sweep ends on the reading");
                    // As a window that knew nothing of the sweep before would round it.
                    let f = m.frame_at(view, t1);
                    let cold = m
                        .drawn(view, f)
                        .map_or(to, |(a, _)| f64::from(a) / ANGLE_PER_DEG);
                    control = control.max((shown - cold) * view.px_per_deg);
                }
                // A reading that turns the needle back, or sends it the other way at once: the
                // sweep ends on its reading, and its frames move the tip a pixel at least.
                for to in [40.0, -100.0, 59.0] {
                    let m = Needle::retarget(Some(n), t1, Some((to, 0.0)), view.px_per_deg)
                        .expect("a needle");
                    let mut d = m.follow(view, was, t1);
                    let mut frames = 0;
                    while let Some(next) = m.next_after(view, d.frame.expect("a frame")) {
                        let e = m.follow(view, Some(d), next);
                        assert_ne!(
                            e.at, d.at,
                            "{ms} ms to {to}°: a frame that draws nothing new"
                        );
                        d = e;
                        frames += 1;
                        assert!(frames < 1000, "{ms} ms to {to}°: the sweep never ends");
                    }
                    assert_eq!(
                        d.frame.map(|f| f.k),
                        Some((0, 0)),
                        "{ms} ms to {to}°: the sweep ends on the reading"
                    );
                }
            }
        }
    }
    assert!(
        control > 0.5,
        "CONTROL: rounded from the new target alone the tip stepped back {control} px"
    );
}

/// A reading no window animates a dial for rests the needle on its target at once, its soak
/// too: a window whose motion comes back draws the bezel at its reading, never easing toward it.
#[test]
fn a_reading_no_window_animates_rests_the_soak_too() {
    let t0 = Instant::now();
    let rest = Needle::at_rest(t0, (0.0, 0.10));
    let nobody = Needle::retarget(Some(rest), t0, Some((40.0, 0.60)), 0.0).expect("a needle");
    assert_eq!(nobody, Needle::at_rest(t0, (40.0, 0.60)));
    let [view, _] = views(Duration::from_micros(16_667));
    for ms in [0_u32, 100, 200, 400] {
        assert_eq!(
            nobody.drawn(view, nobody.frame_at(view, t0 + MS * ms)),
            None,
            "{ms} ms"
        );
    }
    let watched =
        Needle::retarget(Some(rest), t0, Some((40.0, 0.60)), view.px_per_deg).expect("a needle");
    assert_ne!(
        watched.soak.x0, watched.soak.target,
        "CONTROL: a window that animates the dial eases the soak"
    );
}

/// An episode's frames as the live design quotes them (`DESIGN-stats-deck-live-2026-10-24.md`
/// §6): the reading's own frame and each after it to the target, for a 3°, 10° and 60° move and
/// the whole sweep, on Full's dial and Folded's at 2×, at the effect lane's present interval
/// (`effect_present_interval_with`) on a 60 Hz Apple-silicon Mac (two panel periods, 33.3 ms),
/// a 120 Hz panel (two, 16.7 ms) and a 60 Hz Linux, Windows or Intel Mac (one, 16.7 ms); and when
/// each episode's last frame comes, which the spring sets whatever the interval.
#[test]
fn an_episode_takes_the_frames_the_live_design_quotes() {
    let t0 = Instant::now();
    let hz = |n: u64| Duration::from_nanos(1_000_000_000 / n);
    let lane = |panel: Duration, panel_rate_at_60hz: bool| {
        crate::effect_present_interval_with(
            Some(panel),
            crate::effect_present_panel_periods_with(Some(panel), panel_rate_at_60hz),
        )
    };
    let ms = |d: Duration| (d.as_secs_f64() * 1e4).round() / 10.0;
    let mac60 = lane(hz(60), false);
    let fast = [lane(hz(120), false), lane(hz(60), true)];
    assert_eq!(ms(mac60), 33.3);
    assert_eq!(fast.map(ms), [16.7, 16.7]);
    // Each dial: its radius, then the frames of each move at 16.7 ms and at 33.3 ms, and when
    // each move's last frame comes, ms.
    let dials = [
        (
            "Full",
            crate::stats_gauge::R_FACE,
            [4, 10, 26, 35],
            [4, 10, 15, 20],
            [255, 382, 558, 700],
        ),
        (
            "Folded",
            crate::stats_plan::FOLD_GAUGE_R,
            [3, 9, 25, 34],
            [3, 9, 15, 20],
            [240, 368, 545, 687],
        ),
    ];
    for (name, r, at_fast, at_mac60, last_ms) in dials {
        for (interval, want) in [(fast[0], at_fast), (fast[1], at_fast), (mac60, at_mac60)] {
            let view = View {
                px_per_deg: tip_px_per_deg(r, 2.0),
                interval,
            };
            let mut got = Vec::new();
            for (from, to) in [(0.0, 3.0), (0.0, 10.0), (0.0, 60.0), (START, END)] {
                let n = swing(t0, from, to, view.px_per_deg);
                let mut f = n.frame_at(view, t0);
                let mut frames = 1;
                while let Some(next) = n.next_after(view, f) {
                    f = n.frame_at(view, next);
                    frames += 1;
                }
                assert_eq!(
                    n.frames_owed(view, None),
                    frames - 1,
                    "{name} 2× at {interval:?}, {from}° to {to}°: owed after the reading's own"
                );
                got.push((frames, f.at_ns / 1_000_000));
            }
            let want = want.into_iter().zip(last_ms).collect::<Vec<_>>();
            assert_eq!(got, want, "{name} 2× at {interval:?}: (frames, last ms)");
        }
    }
}
