// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The motion layer (design §10.4.6).

#![allow(
    clippy::many_single_char_names,
    reason = "the recipes use the design's own symbols — x, e, g, h, t, u, q — so each line reads against §10.4.6"
)]

use crate::center::{Echo, EchoKind};
use crate::glass::Fnv;
use crate::{
    COMET_HEAD_LIFT, COMET_LEAD_PERMILLE, COMET_PERIOD, COMET_PERMILLE, Duration, ECHO_DRAIN,
    ECHO_FADE, ECHO_FAULT_CROSS, ECHO_FAULT_FLASH, ECHO_SWEEP, ECHO_SWEEP_PERMILLE, FILL_GLIDE,
    FILL_GLIDE_MAX, FILL_GLIDE_PER_POINT, GLINT_DELAY, GLINT_MIN_HALF_CELLS, GLINT_PEAK,
    GLINT_PERIOD, GLINT_RADIUS_PERMILLE, GLINT_TRAVEL, Instant,
};

/// The whole row as a fraction.
pub const ROW: u32 = 65_536;

/// Whether the band moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pace {
    /// Frames on the grid.
    Moving,
    /// Text ticks only.
    Still,
}

/// How the band draws motion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Look {
    /// Moving or still.
    pub pace: Pace,
    /// Gradients, glints and tints allowed.
    pub graded: bool,
}

impl Look {
    /// The full look.
    pub const MOVING: Look = Look {
        pace: Pace::Moving,
        graded: true,
    };
    /// The still look.
    pub const STILL: Look = Look {
        pace: Pace::Still,
        graded: true,
    };
}

/// One tone of the row's surface, as three mixes (each 0–255) the host resolves against its palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Tone {
    /// Track → fill.
    pub fill: u8,
    /// → glint.
    pub lift: u8,
    /// → warn.
    pub warn: u8,
}

impl Tone {
    /// The empty channel.
    pub const TRACK: Tone = Tone {
        fill: 0,
        lift: 0,
        warn: 0,
    };
    /// A full cell.
    pub(crate) const FULL: Tone = Tone {
        fill: 255,
        lift: 0,
        warn: 0,
    };
    /// The comet's hot head.
    pub const HEAD: Tone = Tone {
        fill: 255,
        lift: COMET_HEAD_LIFT,
        warn: 0,
    };
    /// A determinate fill whose bytes have stalled.
    pub const STALLED: Tone = Tone {
        fill: 120,
        lift: 0,
        warn: 0,
    };
}

/// A [`Tone`] to 1/256 of a step per channel (`0..=255·256`), from [`Surface::span_fine`], for a host that mixes a cell's colour and rounds it once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct FineTone {
    /// Track → fill, ×256.
    pub fill: u16,
    /// → glint, ×256.
    pub lift: u16,
    /// → warn, ×256.
    pub warn: u16,
}

impl From<Tone> for FineTone {
    #[inline]
    fn from(t: Tone) -> Self {
        Self {
            fill: u16::from(t.fill) << 8,
            lift: u16::from(t.lift) << 8,
            warn: u16::from(t.warn) << 8,
        }
    }
}

/// One point of a [`Surface`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Stop {
    /// Where along the row, `0..=ROW`.
    pub at: u32,
    /// The tone there.
    pub tone: Tone,
}

/// A row's surface at one frame.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct Surface {
    /// Left to right, `at` non-decreasing.
    pub stops: Vec<Stop>,
    /// The flat look (High Contrast).
    pub flat: bool,
    /// Where a determinate fill ends (Q16 of the row), when inside the row.
    pub edge: Option<u32>,
}

impl Surface {
    /// The whole row in one tone.
    #[must_use]
    pub(crate) fn uniform(tone: Tone, flat: bool) -> Self {
        Self {
            stops: vec![Stop { at: 0, tone }, Stop { at: ROW, tone }],
            flat,
            ..Self::default()
        }
    }

    /// Whether the row has no meter and no track.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.stops.is_empty()
    }

    /// The tone at `at` (Q16 of the row).
    #[must_use]
    pub(crate) fn tone_at(&self, at: u32) -> Tone {
        let s = &self.stops;
        let Some(first) = s.first() else {
            return Tone::TRACK;
        };
        if at < first.at {
            return first.tone;
        }
        let i = s.iter().rposition(|p| p.at <= at).unwrap_or(0);
        match s.get(i + 1) {
            Some(next) if next.at > s[i].at => {
                let (a, b) = (s[i], *next);
                lerp_tone(a.tone, b.tone, u64::from(at - a.at), u64::from(b.at - a.at))
            }
            _ => s[i].tone,
        }
    }

    /// The tone of the pixel span `[x0, x1)` (a cell or a gutter) of a row `w` pixels wide, pixel 0 at its left edge.
    #[must_use]
    pub fn span(&self, x0: u64, x1: u64, w: u64) -> Tone {
        match self.span_sums(x0, x1, w) {
            Ok(tone) => tone,
            Err((sum, span)) => {
                let ch =
                    |k: usize| u8::try_from(((sum[k] / span) + (1 << 19)) >> 20).unwrap_or(255);
                Tone {
                    fill: ch(0),
                    lift: ch(1),
                    warn: ch(2),
                }
            }
        }
    }

    /// [`Surface::span`] to 1/256 of a step ([`FineTone`]).
    #[must_use]
    pub fn span_fine(&self, x0: u64, x1: u64, w: u64) -> FineTone {
        match self.span_sums(x0, x1, w) {
            Ok(tone) => tone.into(),
            Err((sum, span)) => {
                let ch = |k: usize| {
                    u16::try_from(((sum[k] / span) + (1 << 11)) >> 12).unwrap_or(u16::MAX)
                };
                FineTone {
                    fill: ch(0),
                    lift: ch(1),
                    warn: ch(2),
                }
            }
        }
    }

    /// The span's tone when it is a point (an empty surface, a degenerate row,
    /// the flat look), else each channel's trapezoid sum scaled by 2^20 and
    /// the span it is over.
    fn span_sums(&self, x0: u64, x1: u64, w: u64) -> Result<Tone, ([i128; 3], i128)> {
        if self.stops.is_empty() || w == 0 {
            return Ok(Tone::TRACK);
        }
        let (x0, x1) = (x0.min(w), x1.min(w).max(x0.min(w)));
        if self.flat || x1 == x0 {
            let at = match (x0 == 0, x1 == w) {
                (true, false) if x1 > x0 => 0,
                (false, true) if x1 > x0 => ROW - 1,
                _ => {
                    let mid = (x0 + x1) * u64::from(ROW) / (2 * w);
                    u32::try_from(mid.saturating_sub(1)).unwrap_or(ROW)
                }
            };
            return Ok(self.tone_at(at));
        }
        let (a, b) = (
            i128::from(x0) * i128::from(ROW),
            i128::from(x1) * i128::from(ROW),
        );
        let w = i128::from(w);
        let chans = |t: Tone| [i128::from(t.fill), i128::from(t.lift), i128::from(t.warn)];
        let mut sum = [0i128; 3];
        for pair in self.stops.windows(2) {
            let (p, r) = (pair[0], pair[1]);
            let (s0, s1) = (i128::from(p.at) * w, i128::from(r.at) * w);
            let (lo, hi) = (a.max(s0), b.min(s1));
            if hi <= lo || s1 <= s0 {
                continue;
            }
            let (cp, cr) = (chans(p.tone), chans(r.tone));
            for k in 0..3 {
                let v = |x: i128| cp[k] * (s1 - x) + cr[k] * (x - s0);
                sum[k] += (((v(lo) + v(hi)) * (hi - lo)) << 20) / (2 * (s1 - s0));
            }
        }
        Err((sum, b - a))
    }

    /// The tones of `cols` equal cells across the row (no gutters).
    #[must_use]
    pub fn cells(&self, cols: usize) -> Vec<Tone> {
        let w = cols as u64;
        (0..w).map(|c| self.span(c, c + 1, w)).collect()
    }

    /// The fault wash at `u` (0–255): each stop's fill hands its coverage to
    /// the warn ink (`fill · (1 − u)` fill, `fill · u` warn), a premultiplied
    /// cross-fade that stays inside the track–fill–warn hull and never passes
    /// through another hue.
    #[must_use]
    fn washed(mut self, u: u8) -> Self {
        for s in &mut self.stops {
            let f = u16::from(s.tone.fill);
            let w = u8::try_from((f * u16::from(u) + 127) / 255).unwrap_or(255);
            s.tone = Tone {
                fill: s.tone.fill - w.min(s.tone.fill),
                lift: 0,
                warn: w,
            };
        }
        self
    }

    /// Every stop drained toward the track by `keep` (0–255 of its tone kept):
    /// a busy row's frozen comet at its Fault.
    #[must_use]
    fn drained(mut self, keep: u8) -> Self {
        let k = u16::from(keep);
        let sc = |v: u8| u8::try_from((u16::from(v) * k + 127) / 255).unwrap_or(255);
        for s in &mut self.stops {
            s.tone = Tone {
                fill: sc(s.tone.fill),
                lift: sc(s.tone.lift),
                warn: sc(s.tone.warn),
            };
        }
        self
    }
}

/// `a` toward `b` by `num/den`, per channel, rounded.
fn lerp_tone(a: Tone, b: Tone, num: u64, den: u64) -> Tone {
    let den = den.max(1);
    let l = |x: u8, y: u8| {
        let (x, y) = (i64::from(x), i64::from(y));
        let n = i64::try_from(num.min(den)).unwrap_or(0);
        let d = i64::try_from(den).unwrap_or(1);
        u8::try_from(x + ((y - x) * n * 2 + d).div_euclid(2 * d)).unwrap_or(255)
    };
    Tone {
        fill: l(a.fill, b.fill),
        lift: l(a.lift, b.lift),
        warn: l(a.warn, b.warn),
    }
}

/// Clip a profile whose positions may lie outside the row (a comet entering
/// or leaving) to `[0, ROW]`: the end tones are interpolated, the points inside
/// kept, hard edges included.
fn clipped(points: &[(i64, Tone)], flat: bool) -> Surface {
    let row = i64::from(ROW);
    let at = |x: i64| -> Tone {
        let Some(first) = points.first() else {
            return Tone::TRACK;
        };
        if x < first.0 {
            return Tone::TRACK;
        }
        let i = points.iter().rposition(|p| p.0 <= x).unwrap_or(0);
        match points.get(i + 1) {
            Some(n) if n.0 > points[i].0 => lerp_tone(
                points[i].1,
                n.1,
                u64::try_from(x - points[i].0).unwrap_or(0),
                u64::try_from(n.0 - points[i].0).unwrap_or(1),
            ),
            Some(_) => points[i].1,
            None => Tone::TRACK,
        }
    };
    let mut stops = Vec::with_capacity(points.len() + 2);
    stops.push(Stop { at: 0, tone: at(0) });
    for &(x, tone) in points {
        if x > 0 && x < row {
            stops.push(Stop {
                at: u32::try_from(x).unwrap_or(0),
                tone,
            });
        }
    }
    let end = {
        let i = points.iter().rposition(|p| p.0 < row);
        match i {
            None => Tone::TRACK,
            Some(i) => match points.get(i + 1) {
                Some(n) if n.0 >= row => lerp_tone(
                    points[i].1,
                    n.1,
                    u64::try_from(row - points[i].0).unwrap_or(0),
                    u64::try_from(n.0 - points[i].0).unwrap_or(1),
                ),
                _ => Tone::TRACK,
            },
        }
    };
    stops.push(Stop { at: ROW, tone: end });
    Surface {
        stops,
        flat,
        ..Surface::default()
    }
}

/// What a row's motion is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Anim {
    /// No motion, no indicator.
    #[default]
    None,
    /// The comet.
    Comet {
        /// Milliseconds into the current comet's crossing.
        phase_ms: u32,
    },
    /// A determinate bar.
    Bar {
        /// The fill shown, in permille (the glide's value).
        shown: u16,
        /// Milliseconds into the glint's travel, when it travels.
        glint_ms: Option<u32>,
    },
    /// The still busy form.
    Track,
    /// A completion echo.
    Echo {
        /// Which.
        kind: EchoKind,
        /// Milliseconds into it.
        t_ms: u32,
    },
}

/// One row's motion at one frame.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct RowMotion {
    /// The row's surface, edge to edge of the window (empty: the plain band).
    pub surface: Surface,
    /// The elapsed words of busy work.
    pub readout: Option<String>,
    /// The ETA words or `stalled`, blank while the estimate is hidden.
    pub eta: Option<String>,
    /// An echo's fade.
    pub fade: u8,
    /// The glyph cell's paint when it is not the row's own glyph.
    pub glyph: Option<char>,
    /// What the motion is.
    pub anim: Anim,
}

/// One frame of the whole band.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BandMotion {
    /// The frame instant (on the center's grid).
    pub at: Instant,
    /// Parallel to `Presentation::rows`.
    pub rows: Vec<RowMotion>,
}

impl BandMotion {
    /// The repaint-key term.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        if self.rows.iter().all(|r| *r == RowMotion::default()) {
            return 0;
        }
        let mut h = Fnv::new();
        for row in &self.rows {
            h.byte(u8::from(row.surface.flat));
            h.num(row.surface.stops.len() as u64);
            for s in &row.surface.stops {
                h.num(u64::from(s.at));
                h.byte(s.tone.fill);
                h.byte(s.tone.lift);
                h.byte(s.tone.warn);
            }
            h.str(row.readout.as_deref().unwrap_or(""));
            h.str(row.eta.as_deref().unwrap_or(""));
            h.byte(row.fade);
            h.num(row.glyph.map_or(0, u64::from));
        }
        h.finish()
    }
}

const ONE: u64 = 65_536;

/// Smoothstep: `p²(3 − 2p)`.
#[inline]
#[must_use]
pub(crate) fn smooth(p: u32) -> u32 {
    let p = u64::from(p).min(ONE);
    let p2 = (p * p) >> 16;
    u32::try_from((p2 * (3 * ONE - 2 * p)) >> 16).unwrap_or(u32::MAX)
}

/// The comet's breathing sweep.
#[inline]
#[must_use]
pub(crate) fn breath(p: u32) -> u32 {
    let p = u64::from(p).min(ONE);
    let v = (47_186 * p + 18_350 * u64::from(smooth(u32::try_from(p).unwrap_or(0)))) >> 16;
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// Ease-out cubic.
#[inline]
#[must_use]
#[cfg(test)]
pub(crate) fn ease_out_cubic(u: u32) -> u32 {
    let v = ONE - u64::from(u).min(ONE);
    u32::try_from(ONE - ((((v * v) >> 16) * v) >> 16)).unwrap_or(u32::MAX)
}

/// Ease-out quadratic.
#[inline]
#[must_use]
pub fn ease_out_quad(u: u32) -> u32 {
    let v = ONE - u64::from(u).min(ONE);
    u32::try_from(ONE - ((v * v) >> 16)).unwrap_or(u32::MAX)
}

/// `t` as a Q16 fraction of `span`, clamped to [0, 1].
#[inline]
fn frac(t: Duration, span: Duration) -> u32 {
    let span = span.as_millis().max(1);
    let t = t.as_millis().min(span);
    u32::try_from(t * u128::from(ONE) / span).unwrap_or(u32::MAX)
}

#[inline]
fn ms32(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
}

/// `q` 0..=255 scaled by a Q16 fraction.
#[inline]
fn scale(q: u8, f: u32) -> u8 {
    u8::try_from((u64::from(q) * u64::from(f).min(ONE)) >> 16).unwrap_or(255)
}

/// Permille of the row as a Q16 fraction.
#[inline]
const fn permille_q16(p: u32) -> i64 {
    (if p < 1000 { p } else { 1000 }) as i64 * (ROW as i64) / 1000
}

/// How long a glide from `from` to `to` (permille) takes.
#[must_use]
pub fn glide_span(from: u16, to: u16) -> Duration {
    let points_x10 = u64::from(from.abs_diff(to));
    let per = u64::try_from(FILL_GLIDE_PER_POINT.as_micros()).unwrap_or(u64::MAX);
    let extra = Duration::from_micros(per.saturating_mul(points_x10) / 10);
    (FILL_GLIDE + extra).min(FILL_GLIDE_MAX)
}

/// The shown fill of a data jump from `from` to `to`, `t` into its [`glide_span`].
#[must_use]
pub(crate) fn glide(from: u16, to: u16, t: Duration) -> u16 {
    let u = u64::from(ease_out_quad(frac_fine(t, glide_span(from, to))));
    let (from, to) = (i64::from(from), i64::from(to));
    let v = from + ((to - from) * i64::try_from(u).unwrap_or(0)) / 65_536;
    u16::try_from(v.clamp(0, 1000)).unwrap_or(1000)
}

/// [`frac`] to the microsecond: a glide is read on the display's own
/// cadence, whose period is not a whole number of milliseconds.
fn frac_fine(t: Duration, span: Duration) -> u32 {
    let span = span.as_micros().max(1);
    let t = t.as_micros().min(span);
    u32::try_from(t * u128::from(ONE) / span).unwrap_or(u32::MAX)
}

/// The comet's head, Q16 of the row, `age` into its crossing.
#[must_use]
pub(crate) fn comet_head(age: Duration) -> i64 {
    let lead = permille_q16(COMET_LEAD_PERMILLE);
    let tail = permille_q16(COMET_PERMILLE - COMET_LEAD_PERMILLE);
    let travel = i64::from(ROW) + lead + tail;
    -lead + ((i64::from(breath(frac(age, COMET_PERIOD))) * travel) >> 16)
}

/// The comet `since` into the motion.
#[must_use]
pub fn comet(since: Duration, graded: bool) -> Surface {
    let period = COMET_PERIOD.as_millis().max(1);
    let age = Duration::from_millis(u64::try_from(since.as_millis() % period).unwrap_or(0));
    let h = comet_head(age);
    let tail = permille_q16(COMET_PERMILLE - COMET_LEAD_PERMILLE);
    let lead = permille_q16(COMET_LEAD_PERMILLE);
    if !graded {
        let t0 = h - tail;
        return clipped(
            &[
                (t0, Tone::TRACK),
                (t0, Tone::FULL),
                (h, Tone::FULL),
                (h, Tone::TRACK),
            ],
            true,
        );
    }
    let mut points = Vec::with_capacity(usize::try_from(TAIL_STOPS).unwrap_or(0) + 3);
    for k in 0..=TAIL_STOPS {
        let x = h - tail + tail * k / TAIL_STOPS;
        points.push((x, tail_tone(k)));
    }
    points.push((h + lead, Tone::TRACK));
    clipped(&points, false)
}

/// The tail's segments: fine enough that a cell (a tail spans ~10 cells at
/// 60 columns, ~34 at 200) sees the curve and not the chords; 8 showed faint
/// vertical stripes along the tail.
const TAIL_STOPS: i64 = 32;

/// The tail's tone `k/TAIL_STOPS` of the way from its end to the head: the
/// fill `u²`, the lift rising as the square over the last half into
/// [`Tone::HEAD`] at `k = TAIL_STOPS`.
fn tail_tone(k: i64) -> Tone {
    let (n, half) = (TAIL_STOPS, TAIL_STOPS / 2);
    let u = k.clamp(0, n);
    let fill = u8::try_from((u * u * 255 + n * n - 1) / (n * n)).unwrap_or(255);
    let hot = (u - half).max(0);
    let lift =
        u8::try_from((i64::from(COMET_HEAD_LIFT) * hot * hot + half * half / 2) / (half * half))
            .unwrap_or(0);
    Tone {
        fill,
        lift,
        warn: 0,
    }
}

/// The glint's radius, Q16 of the row, on a row `cols` columns wide.
#[must_use]
pub fn glint_radius(cols: usize) -> i64 {
    let r = permille_q16(GLINT_RADIUS_PERMILLE);
    let cols = i64::try_from(cols).unwrap_or(i64::MAX);
    if cols == 0 {
        return r.max(1);
    }
    let floor = i64::from(ROW) * i64::from(GLINT_MIN_HALF_CELLS) / (2 * cols);
    r.max(floor).max(1)
}

/// A raised-cosine lift of radius `r` (Q16) at distance `d` from its centre,
/// scaled to `peak` (0–255): `peak · ½(1 + cos(π·d/r))`, 0 from `r` out, a
/// bump with no corner so it reads as light moving along the bar.
fn raised_cosine(d: i64, r: i64, peak: u8) -> u8 {
    let d = d.abs();
    if d >= r || r <= 0 {
        return 0;
    }
    let x = (d << 16) / r;
    let y = (x * 102_944) >> 16;
    let y2 = (y * y) >> 16;
    let y4 = (y2 * y2) >> 16;
    let y6 = (y4 * y2) >> 16;
    let c = 65_536 - y2 / 2 + y4 / 24 - y6 / 720;
    let c = c.clamp(0, 65_536);
    let c2 = (c * c) >> 16;
    u8::try_from((i64::from(peak) * c2 + 32_768) >> 16).unwrap_or(peak)
}

/// Whether a bar at `shown` carries a glint.
#[inline]
#[must_use]
pub(crate) const fn bar_glints(shown: u16) -> bool {
    (shown as u32) * 2 >= GLINT_RADIUS_PERMILLE
}

/// A determinate bar at `shown` permille of the row (0 % an empty track, 100 % the window edge to edge).
#[must_use]
pub fn bar(shown: u16, glint: Option<Duration>, graded: bool) -> Surface {
    bar_at(permille_q16(u32::from(shown)), glint, graded, 0)
}

/// [`bar`] with its edge at `edge` (Q16 of the row) on a `cols`-wide row ([`glint_radius`]; 0 for none).
#[must_use]
pub fn bar_at(edge: i64, glint: Option<Duration>, graded: bool, cols: usize) -> Surface {
    bar_in(edge, glint, graded, Tone::FULL, cols)
}

/// The bar of a download whose bytes have stalled.
#[must_use]
pub(crate) fn stalled_bar(shown: u16, graded: bool) -> Surface {
    let tone = if graded { Tone::STALLED } else { Tone::FULL };
    bar_in(permille_q16(u32::from(shown)), None, graded, tone, 0)
}

/// A glint's lift profile over `[0, e)`: the bump at `g` of radius `r`,
/// sampled finely enough that a cell sees the curve and not its chords.
fn lift_points(g: i64, r: i64, e: i64, peak: u8, ink: Tone) -> Vec<(i64, Tone)> {
    let mut points = Vec::with_capacity(LIFT_STOPS * 2 + 3);
    let n = i64::try_from(LIFT_STOPS).unwrap_or(16);
    for k in -n..=n {
        let x = g + r * k / n;
        if x > 0 && x < e {
            points.push((
                x,
                Tone {
                    lift: raised_cosine(x - g, r, peak),
                    ..ink
                },
            ));
        }
    }
    points
}

/// Chords per half of a glint's bump.
const LIFT_STOPS: usize = 16;

fn bar_in(e: i64, glint: Option<Duration>, graded: bool, ink: Tone, cols: usize) -> Surface {
    let row = i64::from(ROW);
    let e = e.clamp(0, row);
    let r = glint_radius(cols);
    let g = glint
        .filter(|_| graded && e * 2 >= r.min(permille_q16(GLINT_RADIUS_PERMILLE)))
        .map(|t| {
            let u = i64::from(smooth(frac(t, GLINT_TRAVEL)));
            -r + ((u * (e + 2 * r)) >> 16)
        });
    let lift_at = |c: i64| g.map_or(0, |g| raised_cosine(c - g, r, GLINT_PEAK));
    let tone = |c: i64| Tone {
        lift: lift_at(c),
        ..ink
    };
    let mut points: Vec<(i64, Tone)> = vec![(0, tone(0))];
    if let Some(g) = g {
        points.extend(lift_points(g, r, e, GLINT_PEAK, ink));
    }
    if e >= row {
        points.push((row, tone(row)));
    } else {
        if e > 0 {
            points.push((e, tone(e)));
        }
        points.push((e, Tone::TRACK));
        points.push((row, Tone::TRACK));
    }
    let flat = !graded;
    let stops = points
        .into_iter()
        .map(|(x, t)| Stop {
            at: u32::try_from(x.clamp(0, row)).unwrap_or(0),
            tone: t,
        })
        .collect();
    Surface {
        stops,
        flat,
        edge: (e > 0 && e < row).then(|| u32::try_from(e).unwrap_or(0)),
    }
}

/// A full bar with one conclusive sweep `t` into [`ECHO_SWEEP`]: a glint about
/// four cells wide at 80 columns ([`ECHO_SWEEP_PERMILLE`], never under a
/// glint's least width) crossing the row edge to edge, eased like the glint.
fn swept(t: Duration, cols: usize) -> Surface {
    let row = i64::from(ROW);
    let r = permille_q16(ECHO_SWEEP_PERMILLE).max(glint_radius(cols).min(permille_q16(60)));
    let u = i64::from(smooth(frac_fine(t, ECHO_SWEEP)));
    let g = -r + ((u * (row + 2 * r)) >> 16);
    let mut points: Vec<(i64, Tone)> = vec![(
        0,
        Tone {
            lift: raised_cosine(g, r, GLINT_PEAK),
            ..Tone::FULL
        },
    )];
    points.extend(lift_points(g, r, row, GLINT_PEAK, Tone::FULL));
    points.push((
        row,
        Tone {
            lift: raised_cosine(row - g, r, GLINT_PEAK),
            ..Tone::FULL
        },
    ));
    Surface {
        stops: points
            .into_iter()
            .map(|(x, tone)| Stop {
                at: u32::try_from(x.clamp(0, row)).unwrap_or(0),
                tone,
            })
            .collect(),
        ..Surface::default()
    }
}

/// A busy row's still form.
#[must_use]
pub(crate) fn track(graded: bool) -> Surface {
    Surface::uniform(Tone::TRACK, !graded)
}

/// How long an echo holds its slot.
#[must_use]
pub fn echo_span(kind: EchoKind, from_permille: u16, indeterminate: bool) -> Duration {
    match kind {
        EchoKind::Complete => {
            let from = if indeterminate { 0 } else { from_permille };
            glide_span(from, 1000) + ECHO_SWEEP + ECHO_FADE
        }
        EchoKind::Fault => ECHO_FAULT_FLASH + ECHO_FADE,
        EchoKind::Vanish => ECHO_FADE,
    }
}

/// A completion echo `t` into its span.
#[must_use]
pub(crate) fn echo(e: &Echo, t: Duration, look: Look, cols: usize) -> (Surface, u8) {
    let from = if e.indeterminate { 0 } else { e.from_permille };
    let last = || {
        if e.indeterminate {
            match e.comet_since {
                Some(since) if look.graded && look.pace == Pace::Moving => comet(since + t, true),
                _ => track(look.graded),
            }
        } else {
            bar(from, None, look.graded)
        }
    };
    let fade_over = |t: Duration, start: Duration| -> u8 {
        let u = u64::from(frac_fine(t.saturating_sub(start), ECHO_FADE));
        u8::try_from((255 * ((u * u) >> 16)) >> 16).unwrap_or(255)
    };
    let full = || bar(1000, None, look.graded);
    let wipe_span = glide_span(from, 1000);
    let wipe = |t: Duration| bar_at(glide_q16(from, 1000, t), None, look.graded, cols);
    match (e.kind, look.pace, look.graded) {
        (EchoKind::Complete, Pace::Moving, true) => {
            if t < wipe_span {
                (wipe(t), 0)
            } else if t < wipe_span + ECHO_SWEEP {
                (swept(t.saturating_sub(wipe_span), cols), 0)
            } else {
                (full(), fade_over(t, wipe_span + ECHO_SWEEP))
            }
        }
        (EchoKind::Complete, Pace::Moving, false) => {
            if t < wipe_span {
                (wipe(t), 0)
            } else {
                (full(), 0)
            }
        }
        (EchoKind::Complete, Pace::Still, _) => (full(), 0),
        (EchoKind::Fault | EchoKind::Vanish, _, false) => (last(), 0),
        (EchoKind::Fault, Pace::Still, true) if e.indeterminate => (track(true), 0),
        (EchoKind::Fault, Pace::Still, true) => (last().washed(255), 0),
        (EchoKind::Fault, Pace::Moving, true) if e.indeterminate => {
            let frozen = match e.comet_since {
                Some(since) => comet(since, true),
                None => track(true),
            };
            let keep = 255 - scale(255, frac_fine(t, ECHO_DRAIN));
            (frozen.drained(keep), fade_over(t, ECHO_FAULT_FLASH))
        }
        (EchoKind::Fault, Pace::Moving, true) => {
            let u = scale(255, frac_fine(t, ECHO_FAULT_CROSS));
            (last().washed(u), fade_over(t, ECHO_FAULT_FLASH))
        }
        (EchoKind::Vanish, Pace::Moving, true) => (last(), fade_over(t, Duration::ZERO)),
        (EchoKind::Vanish, Pace::Still, true) => (last(), 255),
    }
}

/// [`glide`] in Q16 of the row, unquantized.
#[must_use]
pub fn glide_q16(from: u16, to: u16, t: Duration) -> i64 {
    let u = i64::from(ease_out_quad(frac_fine(t, glide_span(from, to))));
    let (a, b) = (permille_q16(u32::from(from)), permille_q16(u32::from(to)));
    a + (((b - a) * u) >> 16)
}

/// The current comet's phase at `q` for a motion epoch `epoch` (the descriptor's; [`comet`] reads the whole span since the epoch).
#[must_use]
pub(crate) fn comet_phase(q: Instant, epoch: Instant) -> Duration {
    let since = q.saturating_duration_since(epoch).as_millis();
    Duration::from_millis(u64::try_from(since % COMET_PERIOD.as_millis()).unwrap_or(0))
}

/// How far into its travel the glint is at `q`, or `None` while it rests (or before its first travel, [`GLINT_DELAY`] after the epoch).
#[must_use]
pub(crate) fn glint_at(q: Instant, epoch: Instant) -> Option<Duration> {
    let since = q.saturating_duration_since(epoch);
    let after = since.checked_sub(GLINT_DELAY)?;
    let gph = Duration::from_millis(
        u64::try_from(after.as_millis() % GLINT_PERIOD.as_millis()).unwrap_or(0),
    );
    (gph < GLINT_TRAVEL).then_some(gph)
}

/// The next instant at or after `q` a glint starts travelling.
#[must_use]
pub fn next_glint_start(q: Instant, epoch: Instant) -> Instant {
    let first = epoch + GLINT_DELAY;
    if q <= first {
        return first;
    }
    let since = q.duration_since(first).as_millis();
    let period = GLINT_PERIOD.as_millis();
    let k = since.div_ceil(period);
    first + Duration::from_millis(u64::try_from(k * period).unwrap_or(u64::MAX))
}

/// `ms32` of a duration, for the descriptors.
#[must_use]
pub(crate) fn anim_ms(d: Duration) -> u32 {
    ms32(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easings_hit_their_ends() {
        assert_eq!(smooth(0), 0);
        assert_eq!(smooth(65_536), 65_536);
        assert_eq!(breath(0), 0);
        assert_eq!(breath(65_536), 65_536);
        assert_eq!(ease_out_cubic(0), 0);
        assert_eq!(ease_out_cubic(65_536), 65_536);
        assert_eq!(ease_out_quad(0), 0);
        assert_eq!(ease_out_quad(65_536), 65_536);
        assert_eq!(ease_out_quad(32_768), 49_152, "three quarters at half time");
        let span = glide_span(100, 900);
        assert_eq!(span, FILL_GLIDE_MAX, "80 points: 180 + 560 ms, capped");
        assert_eq!(glide(100, 900, Duration::ZERO), 100);
        assert_eq!(glide(100, 900, span), 900);
        assert!(glide(100, 900, span / 2) > 500, "eased out");
        assert_eq!(glide(900, 100, span), 100, "down too");
        assert_eq!(glide_span(500, 500), FILL_GLIDE);
        assert!(glide_span(0, 100) < glide_span(0, 500));
        assert_eq!(glide_span(600, 100), glide_span(0, 500), "both ways");
        assert_eq!(glide_span(0, 1000), FILL_GLIDE_MAX);
        let mut prev = 0u32;
        for p in (1024..=65_536).step_by(1024) {
            let v = breath(p);
            let step = v - prev;
            assert!(
                step >= 1024 * 70 / 100 && step <= 1024 * 115 / 100,
                "{p}: {step}"
            );
            prev = v;
        }
    }

    /// The integral is exact: a span's mean over a linear ramp is the ramp at
    /// its middle; a span straddling a hard edge is the covered fraction; a
    /// zero-width span is the point tone; an empty surface is the track.
    #[test]
    fn a_span_is_the_mean_of_the_profile_over_it() {
        let ramp = Surface {
            edge: None,
            stops: vec![
                Stop {
                    at: 0,
                    tone: Tone::TRACK,
                },
                Stop {
                    at: ROW,
                    tone: Tone::FULL,
                },
            ],
            flat: false,
        };
        assert_eq!(ramp.span(0, 1000, 1000).fill, 128);
        assert_eq!(ramp.span(0, 500, 1000).fill, 64);
        assert_eq!(ramp.span(250, 250, 1000).fill, 64);
        let edge = bar(250, None, true);
        assert_eq!(edge.span(200, 300, 1000).fill, 128, "half covered");
        assert_eq!(Surface::default().span(0, 10, 10), Tone::TRACK);
        assert_eq!(
            edge.cells(4),
            [Tone::FULL, Tone::TRACK, Tone::TRACK, Tone::TRACK]
        );
    }

    /// The stalled bar is dim and unlit by any glint; a still bar at its
    /// data is exactly the moving one without its glint.
    #[test]
    fn a_stalled_bar_is_dim_and_never_glints() {
        let s = stalled_bar(620, true);
        assert!(s.stops.iter().any(|p| p.tone == Tone::STALLED));
        assert!(s.stops.iter().all(|p| p.tone.lift == 0));
        assert_eq!(
            bar(620, None, true),
            bar_in(permille_q16(620), None, true, Tone::FULL, 0)
        );
        assert!(!bar_glints(10) && bar_glints(15));
    }

    /// Every echo covers the whole row: a Complete's wipe ends at the window's
    /// right edge; a bar's Fault washes what the fill covered and leaves the
    /// track; a busy row's Fault freezes its comet and drains it, with no warn.
    #[test]
    fn echoes_cover_the_whole_row() {
        let mut e = Echo {
            id: crate::MessageId::FIRST,
            msg: crate::Message::new(crate::tags::UPDATE, crate::Severity::Info, "t"),
            kind: EchoKind::Complete,
            from_permille: 300,
            indeterminate: false,
            comet_since: None,
            elapsed: Duration::ZERO,
            started: Instant::now(),
            until: Instant::now(),
            slot: 0,
            load: None,
            load_slot: crate::model::Loads::NONE,
        };
        let (s, fade) = echo(&e, glide_span(300, 1000), Look::MOVING, 80);
        assert_eq!(fade, 0);
        assert!(s.cells(80).iter().all(|t| t.fill == 255), "{s:?}");
        e.kind = EchoKind::Fault;
        let (s, _) = echo(&e, ECHO_FAULT_FLASH, Look::MOVING, 80);
        let cells = s.cells(80);
        assert!(
            cells[..20]
                .iter()
                .all(|t| t.warn == 255 && t.fill == 0 && t.lift == 0),
            "the fill has handed its coverage to warn"
        );
        assert!(
            cells[30..].iter().all(|t| *t == Tone::TRACK),
            "the track keeps its tone"
        );
        for k in 0..=12u64 {
            let (s, _) = echo(&e, Duration::from_millis(k * 10), Look::MOVING, 80);
            for p in &s.stops {
                assert!(
                    u16::from(p.tone.fill) + u16::from(p.tone.warn) <= 255,
                    "{k}: {p:?}"
                );
            }
        }
        e.indeterminate = true;
        e.comet_since = Some(Duration::from_millis(1500));
        let (s, _) = echo(&e, Duration::ZERO, Look::MOVING, 80);
        assert_eq!(
            s,
            comet(Duration::from_millis(1500), true),
            "frozen where it failed"
        );
        let (s, _) = echo(&e, ECHO_DRAIN, Look::MOVING, 80);
        assert!(
            s.cells(80).iter().all(|t| *t == Tone::TRACK),
            "drained to the track, no warn anywhere"
        );
    }
}
