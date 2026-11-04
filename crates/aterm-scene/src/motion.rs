// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The motion vocabulary every animated surface shares (`docs/DESIGN-init-on-by-default-2026-10-25.md`
//! §5.2): the duration tokens, the Q16 easings and the glide the message band has drawn with
//! since its first meter, two springs, and the grid a motion steps on.
//!
//! Every step is integer math. A frame drawn at `t` is the same frame on every machine, in every
//! run and at every frame rate, so a capture at a pinned clock is exact and a test compares
//! frames, never timings.

use core::time::Duration;

/// One whole in Q16: an easing's input and output run `0..=ONE`.
pub const ONE: u32 = 65_536;

/// A small change: a phrase's crossfade, a glyph's tint.
pub const FAST: Duration = Duration::from_millis(120);

/// The common change: a headline's crossfade, a card's fade.
pub const BASE: Duration = Duration::from_millis(200);

/// A change that travels: a card's rise, a group's height.
pub const SLOW: Duration = Duration::from_millis(320);

/// The gap between the starts of rows that change together, top down.
pub const STAGGER: Duration = Duration::from_millis(60);

/// A glide's floor: a fill that jumps glides at least this long ([`glide_span`]).
pub const GLIDE: Duration = Duration::from_millis(180);

/// What a glide adds per point (a tenth of a percent is a tenth of this) it travels.
pub const GLIDE_PER_POINT: Duration = Duration::from_millis(7);

/// A glide's ceiling, however far it travels.
pub const GLIDE_MAX: Duration = Duration::from_millis(600);

const ONE64: u64 = ONE as u64;

/// Ease-out quadratic: `1 − (1 − u)²`, `u` in Q16.
#[inline]
#[must_use]
pub fn ease_out_quad(u: u32) -> u32 {
    let v = ONE64 - u64::from(u).min(ONE64);
    u32::try_from(ONE64 - ((v * v) >> 16)).unwrap_or(u32::MAX)
}

/// Ease-out cubic: `1 − (1 − u)³`, `u` in Q16.
#[inline]
#[must_use]
pub fn ease_out_cubic(u: u32) -> u32 {
    let v = ONE64 - u64::from(u).min(ONE64);
    u32::try_from(ONE64 - ((((v * v) >> 16) * v) >> 16)).unwrap_or(u32::MAX)
}

/// Ease-in-out cubic: `4u³` to the middle, `1 − (2 − 2u)³ / 2` after it, `u` in Q16.
#[inline]
#[must_use]
pub fn ease_in_out_cubic(u: u32) -> u32 {
    let u = u64::from(u).min(ONE64);
    if u < ONE64 / 2 {
        let cube = (((u * u) >> 16) * u) >> 16;
        u32::try_from(cube * 4).unwrap_or(u32::MAX)
    } else {
        let w = 2 * (ONE64 - u);
        let cube = (((w * w) >> 16) * w) >> 16;
        u32::try_from(ONE64 - cube / 2).unwrap_or(u32::MAX)
    }
}

/// `t` as a Q16 fraction of `span`, to the millisecond, clamped to `0..=ONE`.
#[inline]
#[must_use]
pub fn frac(t: Duration, span: Duration) -> u32 {
    let span = span.as_millis().max(1);
    let t = t.as_millis().min(span);
    u32::try_from(t * u128::from(ONE) / span).unwrap_or(u32::MAX)
}

/// [`frac`] to the microsecond: a glide is read on the display's own cadence, whose period is
/// not a whole number of milliseconds.
#[inline]
#[must_use]
pub fn frac_fine(t: Duration, span: Duration) -> u32 {
    let span = span.as_micros().max(1);
    let t = t.as_micros().min(span);
    u32::try_from(t * u128::from(ONE) / span).unwrap_or(u32::MAX)
}

/// How long a glide from `from` to `to` (permille) takes: [`GLIDE`] and [`GLIDE_PER_POINT`] for
/// each point it travels, at most [`GLIDE_MAX`].
#[must_use]
pub fn glide_span(from: u16, to: u16) -> Duration {
    let points_x10 = u64::from(from.abs_diff(to));
    let per = u64::try_from(GLIDE_PER_POINT.as_micros()).unwrap_or(u64::MAX);
    let extra = Duration::from_micros(per.saturating_mul(points_x10) / 10);
    (GLIDE + extra).min(GLIDE_MAX)
}

/// The shown value (permille) of a jump from `from` to `to`, `t` into its [`glide_span`], eased
/// out ([`ease_out_quad`]).
#[must_use]
pub fn glide(from: u16, to: u16, t: Duration) -> u16 {
    let u = u64::from(ease_out_quad(frac_fine(t, glide_span(from, to))));
    let (from, to) = (i64::from(from), i64::from(to));
    let v = from + ((to - from) * i64::try_from(u).unwrap_or(0)) / 65_536;
    u16::try_from(v.clamp(0, 1000)).unwrap_or(1000)
}

/// The longest a spring is stepped: one still moving after it is drawn at rest.
pub const SPRING_MAX_MS: u32 = 4_000;

/// A damped spring that leaves rest at 0 for rest at [`ONE`]: its angular frequency `ω` (radians
/// a second, Q16) and damping ratio `ζ` (Q16: [`ONE`] is critical, less overshoots). It is
/// stepped every millisecond from its start, velocity then position (semi-implicit Euler) in
/// Q32, so its position at `t` is one integer every machine agrees on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Spring {
    omega: u32,
    zeta: u32,
}

/// One millisecond's step: a spring's position and velocity (Q32, per second).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SpringState {
    x: i64,
    v: i64,
}

impl Spring {
    /// A critically damped spring of angular frequency `omega` (radians a second, Q16): the
    /// fastest arrival that never passes its rest (a caret or a ribbon gliding to its place).
    #[must_use]
    pub const fn critical(omega: u32) -> Self {
        Self { omega, zeta: ONE }
    }

    /// An underdamped spring: `omega` (radians a second, Q16) and a damping ratio `zeta` (Q16)
    /// under [`ONE`], so it passes its rest and comes back (a pulse). A `zeta` of [`ONE`] or more
    /// is held just under it.
    #[must_use]
    pub const fn underdamped(omega: u32, zeta: u32) -> Self {
        Self {
            omega,
            zeta: if zeta >= ONE { ONE - 1 } else { zeta },
        }
    }

    /// The angular frequency, radians a second (Q16).
    #[must_use]
    pub const fn omega(self) -> u32 {
        self.omega
    }

    /// The damping ratio (Q16).
    #[must_use]
    pub const fn zeta(self) -> u32 {
        self.zeta
    }

    fn step(self, s: SpringState) -> SpringState {
        let omega = i128::from(self.omega);
        let omega_sq = (omega * omega) >> 16;
        let damping = (2 * i128::from(self.zeta) * omega) >> 16;
        let one = 1_i128 << 32;
        let accel =
            ((omega_sq * (one - i128::from(s.x))) >> 16) - ((damping * i128::from(s.v)) >> 16);
        let v = (i128::from(s.v) + accel / 1000).clamp(i128::from(i64::MIN), i128::from(i64::MAX))
            as i64;
        let x = (i128::from(s.x) + i128::from(v) / 1000)
            .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
        SpringState { x, v }
    }

    /// Where the spring stands `t_ms` after it starts, Q16 of the way from 0 to [`ONE`] (above
    /// [`ONE`] while an underdamped spring overshoots). From [`SPRING_MAX_MS`] on it stands at
    /// [`ONE`].
    #[must_use]
    pub fn at(self, t_ms: u32) -> i32 {
        if t_ms >= SPRING_MAX_MS {
            return ONE as i32;
        }
        let mut s = SpringState { x: 0, v: 0 };
        for _ in 0..t_ms {
            s = self.step(s);
        }
        i32::try_from(s.x >> 16).unwrap_or(if s.x < 0 { i32::MIN } else { i32::MAX })
    }

    /// When the spring has come to rest: the first millisecond from which it stays within
    /// 1/4096 of [`ONE`] with its speed under 1/64 of [`ONE`] a second, at most
    /// [`SPRING_MAX_MS`]. A motion that draws it ends here.
    #[must_use]
    pub fn settle_ms(self) -> u32 {
        let near = 1_u64 << 20;
        let still = 1_u64 << 26;
        let one = 1_i64 << 32;
        let mut s = SpringState { x: 0, v: 0 };
        let mut since = None;
        for ms in 0..SPRING_MAX_MS {
            s = self.step(s);
            let resting = one.abs_diff(s.x) <= near && s.v.unsigned_abs() <= still;
            match (resting, since) {
                (true, None) => since = Some(ms + 1),
                (false, Some(_)) => since = None,
                _ => {}
            }
        }
        since.unwrap_or(SPRING_MAX_MS)
    }
}

/// The grid a motion steps on: `hz` frames a second counted from its start, frame `k` starting
/// `⌈k · 1000 / hz⌉` ms in, so a 12 Hz spinner keeps its grid however long it turns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cadence {
    hz: u16,
}

impl Cadence {
    /// A working spinner's steps: 12 a second.
    pub const SPINNER: Self = Self::hz(12);

    /// A glide's frames: 60 a second.
    pub const SMOOTH: Self = Self::hz(60);

    /// `hz` frames a second (at least one).
    #[must_use]
    pub const fn hz(hz: u16) -> Self {
        Self {
            hz: if hz == 0 { 1 } else { hz },
        }
    }

    /// Frames a second.
    #[must_use]
    pub const fn per_second(self) -> u16 {
        self.hz
    }

    /// The frame showing at `t_ms` of a motion that starts at `start_ms` (frame 0 until its start,
    /// saturating at `u64::MAX`).
    #[must_use]
    pub const fn frame_at(self, start_ms: u64, t_ms: u64) -> u64 {
        let since = t_ms.saturating_sub(start_ms) as u128;
        let frame = since * self.hz as u128 / 1000;
        if frame > u64::MAX as u128 {
            u64::MAX
        } else {
            frame as u64
        }
    }

    /// When frame `frame` of a motion that starts at `start_ms` begins.
    #[must_use]
    pub const fn frame_start(self, start_ms: u64, frame: u64) -> u64 {
        let ms = (frame as u128 * 1000).div_ceil(self.hz as u128);
        start_ms.saturating_add(if ms > u64::MAX as u128 {
            u64::MAX
        } else {
            ms as u64
        })
    }

    /// When the frame after the one showing at `t_ms` begins: always after `t_ms` (saturating at
    /// `u64::MAX`).
    #[must_use]
    pub const fn next_frame(self, start_ms: u64, t_ms: u64) -> u64 {
        if t_ms < start_ms {
            return self.frame_start(start_ms, 1);
        }
        let since = (t_ms - start_ms) as u128;
        let next = since * self.hz as u128 / 1000 + 1;
        let ms = (next * 1000).div_ceil(self.hz as u128);
        start_ms.saturating_add(if ms > u64::MAX as u128 {
            u64::MAX
        } else {
            ms as u64
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easings_hit_their_ends_and_rise_monotonically() {
        for ease in [ease_out_quad, ease_out_cubic, ease_in_out_cubic] {
            assert_eq!(ease(0), 0);
            assert_eq!(ease(ONE), ONE);
            assert_eq!(ease(ONE * 2), ONE, "clamped past the end");
            let mut prev = 0;
            for u in (0..=ONE).step_by(256) {
                let v = ease(u);
                assert!(v >= prev, "{u}: {v} < {prev}");
                prev = v;
            }
        }
        assert_eq!(
            ease_out_quad(ONE / 2),
            49_152,
            "three quarters at half time"
        );
        assert_eq!(
            ease_out_cubic(ONE / 2),
            57_344,
            "seven eighths at half time"
        );
        assert_eq!(ease_in_out_cubic(ONE / 2), ONE / 2, "half at half time");
        assert_eq!(ease_in_out_cubic(ONE / 4), ONE / 16, "4u³ on the way in");
        assert_eq!(
            ease_in_out_cubic(ONE - ONE / 4),
            ONE - ONE / 16,
            "and its mirror on the way out"
        );
    }

    #[test]
    fn a_glide_spans_its_floor_per_point_and_ceiling() {
        assert_eq!(glide_span(500, 500), GLIDE);
        assert_eq!(
            glide_span(100, 900),
            GLIDE_MAX,
            "80 points: 180 + 560 ms, capped"
        );
        assert_eq!(
            glide_span(0, 100),
            Duration::from_millis(250),
            "10 points: 180 + 70"
        );
        assert_eq!(glide_span(600, 100), glide_span(0, 500), "both ways");
        let span = glide_span(100, 900);
        assert_eq!(glide(100, 900, Duration::ZERO), 100);
        assert_eq!(glide(100, 900, span), 900);
        assert!(glide(100, 900, span / 2) > 500, "eased out");
        assert_eq!(glide(900, 100, span), 100, "down too");
        assert_eq!(
            frac(Duration::from_millis(50), Duration::from_millis(100)),
            ONE / 2
        );
        assert_eq!(
            frac_fine(Duration::from_micros(8_333), Duration::from_micros(16_666)),
            ONE / 2
        );
        assert_eq!(
            frac(Duration::from_secs(9), Duration::ZERO),
            ONE,
            "a zero span is done"
        );
    }

    #[test]
    fn a_critical_spring_arrives_without_passing_its_rest() {
        let spring = Spring::critical(14 * ONE);
        assert_eq!(spring.at(0), 0);
        let mut prev = 0;
        for t in 1..1_500 {
            let x = spring.at(t);
            assert!(x >= prev, "{t}: {x} fell from {prev}");
            assert!(x <= ONE as i32, "{t}: {x} passed its rest");
            prev = x;
        }
        // 1 − (1 + ωt)e^(−ωt) at ω = 14, t = 0.2 s is 0.769.
        let at_200 = f64::from(spring.at(200)) / f64::from(ONE);
        assert!((at_200 - 0.769).abs() < 0.01, "{at_200}");
        let settle = spring.settle_ms();
        assert!((600..1_000).contains(&settle), "{settle}");
        assert_eq!(spring.at(SPRING_MAX_MS), ONE as i32);
    }

    /// NEGATIVE CONTROL: extreme `omega = u32::MAX` springs saturate intermediate Q32 steps
    /// without overflowing `i64` in `at` or `settle_ms` and settle to `ONE` at `SPRING_MAX_MS`.
    #[test]
    fn an_underdamped_spring_overshoots_once_and_settles() {
        let spring = Spring::underdamped(14 * ONE, 39_322);
        assert_eq!(spring.omega(), 14 * ONE);
        let peak = (0..1_000).map(|t| spring.at(t)).max().unwrap_or(0);
        // ζ = 0.6 overshoots by e^(−πζ/√(1−ζ²)) ≈ 9.5 %.
        let over = f64::from(peak - ONE as i32) / f64::from(ONE);
        assert!((0.085..0.105).contains(&over), "{over}");
        let settle = spring.settle_ms();
        assert!(
            settle > Spring::critical(14 * ONE).settle_ms() / 2,
            "{settle}"
        );
        assert!(settle < SPRING_MAX_MS, "it comes to rest");
        assert_eq!(
            Spring::underdamped(14 * ONE, ONE * 2).zeta(),
            ONE - 1,
            "never critical"
        );
        assert_eq!(spring.at(321), spring.at(321), "one integer every time");

        for extreme in [Spring::critical(u32::MAX), Spring::underdamped(u32::MAX, 0)] {
            let _ = extreme.at(1);
            let _ = extreme.at(SPRING_MAX_MS - 1);
            assert_eq!(extreme.at(SPRING_MAX_MS), ONE as i32);
            assert!(extreme.settle_ms() <= SPRING_MAX_MS);
        }
    }

    /// NEGATIVE CONTROL: high-rate cadences (`hz > 1000`) at large timestamps saturate
    /// `frame_at` at `u64::MAX` instead of truncating `u128 as u64` and keep `next_frame > t_ms`
    /// for all `t_ms < u64::MAX` without overflowing `frame_at + 1`.
    #[test]
    fn a_cadence_keeps_its_grid() {
        let spin = Cadence::SPINNER;
        assert_eq!(spin.frame_at(1_000, 0), 0, "before its start");
        assert_eq!(spin.frame_at(1_000, 1_000), 0);
        assert_eq!(spin.frame_at(1_000, 1_083), 0);
        assert_eq!(spin.frame_at(1_000, 1_084), 1);
        assert_eq!(spin.next_frame(1_000, 1_000), 1_084);
        assert_eq!(spin.next_frame(1_000, 1_084), 1_167);
        assert_eq!(spin.next_frame(1_000, 10), 1_084, "a motion yet to start");
        assert_eq!(
            spin.frame_at(0, 3_600_000),
            43_200,
            "an hour on, the 12 Hz grid has not drifted"
        );
        for t in [0, 1, 83, 84, 999, 1_000, 123_456] {
            let next = spin.next_frame(0, t);
            assert!(next > t);
            assert_eq!(spin.frame_at(0, next), spin.frame_at(0, t) + 1, "{t}");
        }
        assert_eq!(Cadence::hz(0).per_second(), 1);

        let fast = Cadence::hz(2_000);
        let far = 1_u64 << 63;
        assert_eq!(fast.frame_at(0, far), u64::MAX);
        assert_eq!(fast.next_frame(0, far), far + 1);
        assert_eq!(fast.frame_at(0, u64::MAX), u64::MAX);
        assert_eq!(fast.next_frame(0, u64::MAX), u64::MAX);
    }
}
