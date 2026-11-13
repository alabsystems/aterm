// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! M1 PROVE bullets, Tier-1 (real code): lattice/exhaustive proofs of the
//! decomposition law, glide convergence, spring overshoot-freedom, and
//! the pill laws — over the SHIPPING kinematics above.
use super::*;

/// Cell heights on an odd/even lattice.
const CELL_HEIGHTS: [i64; 8] = [1, 2, 3, 7, 8, 15, 16, 33];

/// PROVE (1) — decomposition law: `rows*cell_h + frac == px` with
/// `frac ∈ [0, cell_h)` for EVERY point of a signed px lattice × the
/// cell-height lattice.
#[test]
fn scroll_px_decomposition_law() {
    for &ch in &CELL_HEIGHTS {
        for px in (-3 * ch - 2..=3 * ch + 2).chain([-1_000_003, -65_536, 65_537, 1_000_003]) {
            let (rows, frac) = decompose(px, ch);
            assert_eq!(
                rows * ch + frac,
                px,
                "recomposition must be exact (px={px}, cell_h={ch})"
            );
            assert!(
                (0..ch).contains(&frac),
                "frac out of [0, cell_h): px={px} cell_h={ch} frac={frac}"
            );
        }
    }
    assert_eq!(decompose(17, 8), (2, 1));
    assert_eq!(decompose(-1, 8), (-1, 7), "negative px borrows a full row");
    assert!(nearest_row_px(i64::MAX, 34) > 0);
    assert!(nearest_row_px(i64::MIN, 34) < 0);
}

/// (2) Position law: exact start and target, monotone, no overstep, over start/target
/// pairs in both directions × step cadences.
#[test]
fn glide_converges_monotonically_and_exactly() {
    let cases: [(i64, i64); 8] = [
        (0, 960),
        (960, 0),
        (0, 0),
        (-64, 128),
        (7, 8),
        (100_000, -3),
        (i64::MIN, i64::MAX),
        (i64::MAX, i64::MIN),
    ];
    for (start, target) in cases {
        for step_ms in [1u64, 7, 16, 33] {
            let mut prev = glide_position(start, target, 0, GLIDE_MS);
            assert_eq!(prev, start, "t=0 must sit at start");
            let mut t = 0u64;
            while t < GLIDE_MS + 2 * step_ms {
                t += step_ms;
                let pos = glide_position(start, target, t, GLIDE_MS);
                if target >= start {
                    assert!(pos >= prev, "must not retreat ({start}->{target} @ {t}ms)");
                    assert!(
                        pos <= target,
                        "must not overshoot ({start}->{target} @ {t}ms)"
                    );
                } else {
                    assert!(pos <= prev, "must not retreat ({start}->{target} @ {t}ms)");
                    assert!(
                        pos >= target,
                        "must not overshoot ({start}->{target} @ {t}ms)"
                    );
                }
                prev = pos;
            }
            assert_eq!(
                glide_position(start, target, GLIDE_MS, GLIDE_MS),
                target,
                "elapsed == dur must land EXACTLY on target"
            );
        }
    }
}

/// (2) Wake bound: the `Glide` wrapper on a fixed cadence is done within
/// `ceil(dur/step) + 1` wakes, its done sample is the target, and `end()` is
/// `t0 + GLIDE_MS`.
#[test]
fn glide_disarms_in_bounded_wakes() {
    let t0 = Instant::now();
    for step_ms in [4u64, 8, 16, 33] {
        let g = Glide::new(0, 960, t0);
        assert_eq!(g.end(), t0 + Duration::from_millis(GLIDE_MS));
        let bound = GLIDE_MS.div_ceil(step_ms) + 1;
        let mut wakes = 0u64;
        let mut now = t0;
        loop {
            now += Duration::from_millis(step_ms);
            wakes += 1;
            assert!(
                wakes <= bound,
                "glide must disarm within {bound} wakes at {step_ms}ms cadence"
            );
            let (pos, done) = g.sample(now);
            if done {
                assert_eq!(pos, 960, "the done sample must BE the target");
                break;
            }
        }
        let mut g = Glide::new(0, 960, t0);
        let mid = t0 + Duration::from_millis(90);
        let (mid_pos, mid_done) = g.sample(mid);
        assert!(!mid_done, "half-way through the ease is not done");
        g.retarget(-240, mid);
        assert_eq!(g.target_px(), -240);
        let (pos, done) = g.sample(mid + Duration::from_millis(GLIDE_MS));
        assert!(done && pos == -240, "retargeted glide converges exactly");
        assert!(
            mid_pos > 0 && mid_pos < 960,
            "non-vacuity: the mid sample is a genuine intermediate"
        );
    }
}

/// A 120 Hz precise stream at constant speed through `Glide::track` moves the band 1:1
/// (within a pixel, sub-pixel carry included) and evenly (per-frame `max <= 2 * min`, no
/// zero-motion frame) at 0.5 to 4 rows per 100 ms; after the last delta it holds for
/// `TRACK_REST_MS` with one wake, then settles to the nearest whole row; `release` starts
/// the settle at once.
#[test]
fn a_precise_stream_tracks_one_to_one() {
    let t0 = Instant::now();
    let cell = 34i64;
    let tick = Duration::from_micros(8_333);
    let max_px = 1_000 * cell;
    for rows_per_100ms in [0.5f64, 1.0, 2.0, 4.0] {
        let px_per_tick = rows_per_100ms * cell as f64 * 8.333 / 100.0;

        let mut g = Glide::tracked(0, cell, t0);
        let mut now = t0;
        let mut finger = 0.0f64;
        let mut prev = 0i64;
        let mut vels: Vec<i64> = Vec::new();
        for i in 1..=120u32 {
            now += tick;
            finger += px_per_tick;
            assert_eq!(
                g.track(px_per_tick, cell, max_px, now),
                0,
                "nothing clamped away far from the edges"
            );
            let (pos, done) = g.sample(now);
            assert!(!done, "a tracked glide is never done while deltas arrive");
            assert!(
                (pos as f64 - finger).abs() <= 1.0,
                "1:1 — the band is within a pixel of the finger ({pos} vs {finger:.2} at tick {i}, {rows_per_100ms} rows/100 ms)"
            );
            if i > 12 {
                vels.push(pos - prev);
            }
            prev = pos;
        }
        let (mx, mn) = (*vels.iter().max().unwrap(), *vels.iter().min().unwrap());
        assert!(
            mn >= 1 && mx <= 2 * mn,
            "even motion at {rows_per_100ms} rows/100 ms: max {mx} px/frame, min {mn}"
        );
        let rest_end = now + Duration::from_millis(TRACK_REST_MS);
        assert_eq!(
            g.next_wake(now, tick),
            rest_end,
            "parked: ONE wake at the rest boundary, not a frame cadence"
        );
        assert_eq!(
            g.sample(rest_end - Duration::from_millis(1)).0,
            prev,
            "the band holds the finger's position through the rest window"
        );
        let end = g.end();
        assert_eq!(end, rest_end + Duration::from_millis(TRACK_SETTLE_MS));
        assert_eq!(
            g.next_wake(rest_end, tick),
            rest_end + tick,
            "settling: frame-paced wakes"
        );
        let (landed, done) = g.sample(end);
        assert!(done, "done at end()");
        assert_eq!(landed % cell, 0, "…on a whole row");
        assert!(
            (landed - prev).abs() <= cell / 2,
            "…the NEAREST one ({landed} from {prev})"
        );
        let mut r = Glide::tracked(7, cell, t0);
        r.release(t0 + Duration::from_millis(3));
        assert_eq!(
            r.end(),
            t0 + Duration::from_millis(3 + TRACK_SETTLE_MS),
            "release moves the settle to now"
        );
        let mut e = Glide::new(0, cell, t0);
        e.release(t0 + Duration::from_millis(3));
        assert_eq!(
            e.end(),
            t0 + Duration::from_millis(GLIDE_MS),
            "a notch glide ignores release"
        );
    }
}

/// (3) Spring: over an amplitude × omega lattice and a dense time sweep, the displacement
/// keeps the sign of `x0`, never grows in magnitude, and falls below `SPRING_EPS_PX` in
/// bounded time.
#[test]
fn spring_never_overshoots_and_decays() {
    for x0 in [-480.0f64, -32.0, -1.0, 1.0, 8.5, 120.0, 480.0] {
        for omega in [8.0f64, SPRING_OMEGA, 30.0] {
            let mut prev_mag = x0.abs();
            let mut settled_at: Option<f64> = None;
            for ms in 0..=2000u32 {
                let t = f64::from(ms) / 1000.0;
                let x = spring_displacement(x0, omega, t);
                assert!(
                    x * x0 >= 0.0,
                    "sign flip = overshoot (x0={x0}, ω={omega}, t={t}: x={x})"
                );
                assert!(
                    x.abs() <= prev_mag + 1e-9,
                    "magnitude must be monotone non-increasing (x0={x0}, ω={omega}, t={t})"
                );
                prev_mag = x.abs();
                if settled_at.is_none() && x.abs() < SPRING_EPS_PX {
                    settled_at = Some(t);
                }
            }
            let settle = settled_at.expect("spring must settle below ε within 2s");
            assert!(
                settle <= 1.5,
                "bounded settle: x0={x0} ω={omega} took {settle}s"
            );
        }
    }
    let underdamped =
        |x0: f64, t: f64| x0 * (0.5 * SPRING_OMEGA * t).cos() * (-0.3 * SPRING_OMEGA * t).exp();
    assert!(
        (0..=2000).any(|ms| underdamped(100.0, f64::from(ms) / 1000.0) < 0.0),
        "control: the underdamped curve overshoots (sign flip)"
    );
}

/// The bounce keeps its sign, decays monotonically, is done within the settle cap in
/// bounded wakes, and a chained impulse re-releases from the live displacement, clamped
/// to ±max.
#[test]
fn overscroll_spring_bounces_and_self_disarms() {
    let t0 = Instant::now();
    let sp = OverscrollSpring::new(-12.0, t0);
    assert_eq!(sp.end(), t0 + Duration::from_millis(SPRING_SETTLE_MS));
    let (d0, done0) = sp.sample(t0);
    assert!(
        (d0 - -12.0).abs() < 1e-9 && !done0,
        "t=0 sits at the release amplitude"
    );
    let mut prev_mag = d0.abs();
    let mut wakes = 0u64;
    let mut now = t0;
    let disarm = loop {
        now += Duration::from_millis(16);
        wakes += 1;
        assert!(
            wakes <= SPRING_SETTLE_MS / 16 + 2,
            "must settle in bounded wakes"
        );
        let (d, done) = sp.sample(now);
        assert!(
            d <= 0.0,
            "sign preserved (no overshoot past rest) for a down bounce"
        );
        assert!(
            d.abs() <= prev_mag + 1e-9,
            "magnitude monotone non-increasing"
        );
        prev_mag = d.abs();
        if done {
            break now;
        }
    };
    assert!(
        disarm <= sp.end() + Duration::from_millis(16),
        "disarms by the settle cap"
    );

    let mut sp = OverscrollSpring::new(6.0, t0);
    let mid = t0 + Duration::from_millis(20);
    let (live, _) = sp.sample(mid);
    sp.add_impulse(6.0, 8.0, mid);
    let (after, _) = sp.sample(mid);
    assert!(
        (after - (live + 6.0).min(8.0)).abs() < 1e-9,
        "impulse stacks, clamped to max"
    );
    assert!(after <= 8.0, "amplitude never exceeds the one-cell clamp");
    sp.add_impulse(4.0, -1.0, mid);
    assert_eq!(sp.sample(mid).0, 0.0);
    sp.add_impulse(f64::NAN, f64::NAN, mid);
    assert_eq!(sp.sample(mid).0, 0.0);
}

/// Overscroll resistance: odd, sign-preserving, monotone, and bounded by
/// 0.3× — over a signed lattice.
#[test]
fn overscroll_resistance_laws() {
    let mut prev = overscroll_resist(-1001);
    for raw in -1000..=1000i64 {
        let r = overscroll_resist(raw);
        assert_eq!(r, -overscroll_resist(-raw), "odd function");
        assert!(r.signum() == raw.signum() || r == 0, "sign preserved");
        assert!(r.abs() * 10 <= raw.abs() * 3, "|out| <= 0.3|in|");
        assert!(r >= prev, "monotone");
        prev = r;
    }
    assert_eq!(overscroll_resist(100), 30, "non-vacuity: 0.3x is genuine");
    assert_eq!(overscroll_resist(i64::MAX), -overscroll_resist(-i64::MAX));
    assert!(overscroll_resist(i64::MIN) < overscroll_resist(i64::MIN + 10));
}

/// Pill geometry laws over a track × viewport × history × offset lattice:
/// containment, length floor, monotone position, exact endpoints.
#[test]
fn pill_geometry_laws() {
    for track in [10u32, 64, 479, 1080] {
        for view in [1u32, 24, 60] {
            for hist in [1u32, 5, 240, 100_000] {
                let mut prev_y: Option<u32> = None;
                let offsets = [0, 1, hist / 3, hist / 2, hist.saturating_sub(1), hist];
                let mut uniq: Vec<u32> = offsets.to_vec();
                uniq.sort_unstable();
                uniq.dedup();
                for off in uniq {
                    let (y, len) = pill_geometry(track, 8, view, hist, off)
                        .expect("history > 0 must yield a pill");
                    assert!(
                        y + len <= track,
                        "containment (t={track} v={view} h={hist})"
                    );
                    assert!(len >= 8.min(track), "length floor");
                    if off == 0 {
                        assert_eq!(y + len, track, "offset 0 sits flush at the bottom");
                    }
                    if off == hist {
                        assert_eq!(y, 0, "offset == history sits flush at the top");
                    }
                    if let Some(p) = prev_y {
                        assert!(y <= p, "y monotone non-increasing in offset");
                    }
                    prev_y = Some(y);
                }
            }
        }
    }
    assert_eq!(pill_geometry(100, 8, 24, 0, 0), None);
    assert_eq!(pill_geometry(0, 8, 24, 50, 0), None);
    assert_eq!(pill_geometry(100, 8, 0, 50, 0), None);
}

/// The pill's quads are the thumb sliced into its row bands, inside the `u16` pixel space a
/// quad stores. On an ordinary grid that is the whole thumb, one quad per band. 4096 columns
/// of 16 px put the pill at x = 8 + 65,536 - 7 = 65,537, past the space: no quad (narrowed
/// with `as u16` it was drawn at x = 1, the far left). On 2,100 rows of 32 px the thumb is
/// clipped at 65,535 and no band past it is laid (a wrapped `y` sat at the top of the window).
#[test]
fn pill_quads_stay_in_their_bands_inside_the_u16_space() {
    let pill = |cols: usize, rows: usize, (cw, ch): (usize, usize), (y, len): (usize, usize)| {
        let (gox, goy, pw) = (8usize, 8usize, (cw / 3).clamp(2, 6));
        let x = gox + (cols * cw).saturating_sub(pw + 2);
        let (top, bot) = (goy + 2 + y, goy + 2 + y + len);
        let mut out = Vec::new();
        push_pill_quads(&mut out, (x, pw), (top, bot), (goy, ch, rows), 0x00A8_A8A8);
        for q in &out {
            let band = goy + usize::from(q.row) * ch;
            let (qy, qh) = (usize::from(q.y), usize::from(q.h));
            assert!(
                qy >= band.max(top) && qy + qh <= (band + ch).min(bot),
                "a quad outside its row band or the thumb: {q:?}"
            );
            assert!(
                usize::from(q.x) == x && usize::from(q.x) + usize::from(q.w) <= 65_535,
                "a quad off the pill or past the u16 space: {q:?}"
            );
        }
        out
    };
    // Ordinary: 24 rows of 19 px, the thumb across rows 3..=9.
    let ordinary = pill(80, 24, (10, 19), (60, 120));
    let covered: usize = ordinary.iter().map(|q| usize::from(q.h)).sum();
    assert_eq!(covered, 120, "the whole thumb, sliced");
    assert_eq!(ordinary.len(), 7, "one quad per band crossed");
    // 4096 columns of 16 px: the pill is past the space.
    assert!(pill(4096, 3, (16, 32), (0, 40)).is_empty());
    // 2,100 rows of 32 px, the thumb across the space's bottom edge (window y
    // 65,000..66,000): clipped at 65,535, nothing below it.
    let tall = pill(80, 2100, (10, 32), (64_990, 1_000));
    let bottom = tall
        .iter()
        .map(|q| usize::from(q.y) + usize::from(q.h))
        .max();
    assert_eq!(bottom, Some(65_535), "clipped at the space's edge");
    assert!(tall.iter().all(|q| q.row < 2048), "no band past the space");
}

/// Pill fade: opaque at touch, monotone non-increasing, zero by `HOLD+FADE`, and binary
/// under Reduced motion.
#[test]
fn pill_alpha_envelope() {
    for animated in [true, false] {
        assert_eq!(pill_alpha(0, animated), 255, "opaque at touch");
        let mut prev = 255u8;
        for ms in 0..=(PILL_HOLD_MS + PILL_FADE_MS + 50) {
            let a = pill_alpha(ms, animated);
            assert!(a <= prev, "monotone non-increasing at {ms}ms");
            if !animated {
                assert!(a == 0 || a == 255, "Reduced motion is binary, got {a}");
            }
            prev = a;
        }
        assert_eq!(
            pill_alpha(PILL_HOLD_MS + PILL_FADE_MS, animated),
            0,
            "invisible by HOLD+FADE (the disarm bound)"
        );
    }
    let mid = pill_alpha(PILL_HOLD_MS + PILL_FADE_MS / 2, true);
    assert!(mid > 0 && mid < 255, "fade ramp passes through mid alphas");
}

/// PillFade deadlines: a hold-boundary wake while opaque, frame-paced during the ramp,
/// `None` once invisible.
#[test]
fn pill_fade_deadlines_self_disarm() {
    let iv = Duration::from_millis(16);
    let t0 = Instant::now();
    let mut p = PillFade::default();
    assert_eq!(
        p.next_deadline(t0, true, iv),
        None,
        "untouched pill arms nothing"
    );
    p.touch(t0);
    assert_eq!(
        p.next_deadline(t0, true, iv),
        Some(t0 + Duration::from_millis(PILL_HOLD_MS))
    );
    let in_fade = t0 + Duration::from_millis(PILL_HOLD_MS + PILL_FADE_MS / 2);
    let d = p
        .next_deadline(in_fade, true, iv)
        .expect("fading pill arms");
    assert!(d <= t0 + Duration::from_millis(PILL_HOLD_MS + PILL_FADE_MS));
    assert!(d > in_fade);
    let after = t0 + Duration::from_millis(PILL_HOLD_MS + PILL_FADE_MS);
    assert_eq!(p.alpha(after, true), 0);
    assert_eq!(p.next_deadline(after, true, iv), None, "self-disarm");
    let hold_end = t0 + Duration::from_millis(PILL_HOLD_MS);
    assert_eq!(p.next_deadline(hold_end, false, iv), None);
    assert!(!p.is_active(hold_end, false), "binary hide at the boundary");
    assert!(
        p.is_active(hold_end, true),
        "animated pill still fading there"
    );
}
