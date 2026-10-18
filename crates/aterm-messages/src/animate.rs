// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The motion layer (design §10.4.6): what a row's indicator looks like at one
//! frame instant, as data the host paints. The [`crate::glass::Presentation`]
//! stays time-free and fingerprinted; a [`BandMotion`] overlays only the row's
//! surface, the time slots, the glyph cell and an echo's fade, so an animation
//! never re-runs the width law and never re-grids.
//!
//! The meter is the row: 0 % is the window's left edge and 100 % its right
//! edge, gutters included. Motion speaks in Q16 fractions of the row ([`ROW`]);
//! a [`Surface`] is a piecewise-linear profile of [`Tone`]s along it, and
//! [`Surface::span`] gives any pixel span (a cell, a gutter) its mean tone,
//! which is the sub-cell coverage under a fill's edge. Every cell is a
//! whole-cell background tone, never meter ink (the per-cell contrast floor
//! could repaint ink); motion is smooth because the tones move continuously.
//!
//! Clockless and integer: the same state at the same frame instant is the same
//! frame on every target (CPU, GPU, native, wasm).
//!
//! # The looks
//!
//! * **Moving, graded**: for busy work, the comet (a gradient a quarter of the
//!   row long, [`COMET_PERMILLE`]: a tail rising as the square into a hot head,
//!   then a soft leading edge) crossing the row once per [`COMET_PERIOD`], the
//!   next entering as it leaves; the glyph cell keeps the row's glyph. For work
//!   with a fill, the bar: its edge at the data, a data glide and a travelling
//!   glint (dimmed, glint parked, while the bytes stall). On completion, the
//!   echo's fill, glow and fade over the whole row.
//! * **Still**: the bar at its data, a busy row's unlit track, a held ✓.
//! * **Flat** (High Contrast): track and full fill only; a cell takes the tone
//!   at its centre ([`Surface::flat`]), no glint, no tint, no fade.

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

/// The whole row as a fraction: Q16, `0` at the window's left edge and `ROW`
/// at its right edge.
pub const ROW: u32 = 65_536;

/// Whether the band moves: [`Pace::Moving`] on a focused, on-screen window
/// with motion allowed; [`Pace::Still`] everywhere else (unfocused, Reduce
/// Motion, `motion = "reduced"`, the load-shed latch, Serious Mode, the
/// handoff freeze).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pace {
    /// Frames on the grid.
    Moving,
    /// Text ticks only.
    Still,
}

/// How the band draws motion: its pace, and whether it may grade tones
/// (`graded = false` under High Contrast: palette inks only, hard edges).
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

/// One tone of the row's surface, as three mixes (each 0–255) the host
/// resolves against its palette: `fill` from the track toward the fill ink
/// (the theme's cursor accent, `warn` on a Warn/Error row, HIGHLIGHT under
/// High Contrast), then `lift` toward the glint ink, then `warn` toward the
/// fault hue.
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
    /// A determinate fill whose bytes have stalled: a dim slate of the fill
    /// ink, the glint parked; the `stalled` word carries the warning (warn
    /// mixed into the fill reads as the success green beside it).
    pub const STALLED: Tone = Tone {
        fill: 120,
        lift: 0,
        warn: 0,
    };
}

/// A [`Tone`] to 1/256 of a step per channel (`0..=255·256`), from
/// [`Surface::span_fine`], for a host that mixes a cell's colour and rounds it
/// once: per-cell byte rounding alternates ±½ step along the comet's tail.
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
    fn from(t: Tone) -> Self {
        Self {
            fill: u16::from(t.fill) << 8,
            lift: u16::from(t.lift) << 8,
            warn: u16::from(t.warn) << 8,
        }
    }
}

/// One point of a [`Surface`]: the tone at `at`, a Q16 fraction of the row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Stop {
    /// Where along the row, `0..=ROW`.
    pub at: u32,
    /// The tone there.
    pub tone: Tone,
}

/// A row's surface at one frame: the tones along the whole row, edge to edge,
/// piecewise-linear between [`Stop`]s. Two stops at one `at` are a hard edge
/// (the left limit, then the right). Empty: no meter and no track, the plain
/// band.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct Surface {
    /// Left to right, `at` non-decreasing; the first at 0 and the last at
    /// [`ROW`] when not empty.
    pub stops: Vec<Stop>,
    /// The flat look (High Contrast): a pixel span takes the tone at its
    /// centre, hard edges and whole palette inks. Otherwise the mean over the
    /// span: sub-cell coverage.
    pub flat: bool,
    /// Where a determinate fill ends (Q16 of the row), when inside the row: the
    /// surface's one hard edge, which the host draws as a crisp line through
    /// the letters (fill ink left, track right). `None` for a comet, an empty
    /// track and a whole fill.
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
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stops.is_empty()
    }

    /// The tone at `at` (Q16 of the row); at a hard edge, the tone to its
    /// right. [`Tone::TRACK`] on an empty surface.
    #[must_use]
    pub(crate) fn tone_at(&self, at: u32) -> Tone {
        let s = &self.stops;
        let Some(first) = s.first() else {
            return Tone::TRACK;
        };
        if at < first.at {
            return first.tone;
        }
        // The last stop at or left of `at` (the right limit at an edge).
        let i = s.iter().rposition(|p| p.at <= at).unwrap_or(0);
        match s.get(i + 1) {
            Some(next) if next.at > s[i].at => {
                let (a, b) = (s[i], *next);
                lerp_tone(a.tone, b.tone, u64::from(at - a.at), u64::from(b.at - a.at))
            }
            _ => s[i].tone,
        }
    }

    /// The tone of the pixel span `[x0, x1)` (a cell or a gutter) of a row `w`
    /// pixels wide, pixel 0 at its left edge. Graded: the mean tone over the
    /// span, so the cell under a fill's edge is `mix(track, fill, coverage)`.
    /// Flat: the tone at the span's centre, read as its left limit so a centre
    /// exactly on a fill's edge is lit; but a span starting at the left edge
    /// takes the tone at that edge (any started fill lights it), one ending at
    /// the right edge the tone just inside it (only a whole fill lights it),
    /// and a whole-row span its centre. [`Tone::TRACK`] on an empty surface or
    /// a degenerate row.
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

    /// [`Surface::span`] to 1/256 of a step ([`FineTone`]): the same mean
    /// (or, flat, the same point), unrounded to bytes.
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
                    // The left limit at the centre: a column whose centre is
                    // at or left of a fill's edge is lit, so one column is lit
                    // from 500 ‰.
                    let mid = (x0 + x1) * u64::from(ROW) / (2 * w);
                    u32::try_from(mid.saturating_sub(1)).unwrap_or(ROW)
                }
            };
            return Ok(self.tone_at(at));
        }
        // The exact mean of each channel over [x0, x1], in pixel·Q16 units:
        // a stop at `at` sits at `at·w`, a pixel `x` at `x·ROW`. Each
        // segment adds its trapezoid; the sum is kept scaled by 2^20 so the
        // rounding stays under a thousandth of a level.
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
                // The value at x, times (s1 − s0).
                let v = |x: i128| cp[k] * (s1 - x) + cr[k] * (x - s0);
                sum[k] += (((v(lo) + v(hi)) * (hi - lo)) << 20) / (2 * (s1 - s0));
            }
        }
        Err((sum, b - a))
    }

    /// The tones of `cols` equal cells across the row (no gutters): the
    /// engine's cell-resolution reading of a frame, which the motion deadline
    /// compares to ask only for frames that draw something new.
    #[must_use]
    pub fn cells(&self, cols: usize) -> Vec<Tone> {
        let w = cols as u64;
        (0..w).map(|c| self.span(c, c + 1, w)).collect()
    }

    /// The fault wash at `u` (0–255): each stop's fill hands its coverage to
    /// the warn ink (`fill · (1 − u)` fill, `fill · u` warn), a premultiplied
    /// cross-fade that stays inside the track–fill–warn hull and never passes
    /// through another hue. The empty track keeps its tone.
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
/// kept, hard edges included. Beyond the first and last point is the track.
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
    // One allocation per surface: the start, every interior point, the end.
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
    // The right limit at the row's end.
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

/// What a row's motion is doing — the descriptor the tests read.
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
    /// The still busy form: the unlit track.
    Track,
    /// A completion echo.
    Echo {
        /// Which.
        kind: EchoKind,
        /// Milliseconds into it.
        t_ms: u32,
    },
}

/// One row's motion at one frame: the row's surface, the time words, the
/// glyph cell and an echo's fade. [`RowMotion::default`] is "nothing on this
/// row moves and it has no indicator".
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct RowMotion {
    /// The row's surface, edge to edge of the window (empty: the plain band).
    pub surface: Surface,
    /// The elapsed words of busy work; an echo's `done` or `failed` when the
    /// row has no ETA slot.
    pub readout: Option<String>,
    /// The ETA words or `stalled`, blank while the estimate is hidden; an
    /// echo's `done` or `failed`.
    pub eta: Option<String>,
    /// An echo's fade: 0 opaque … 255 gone.
    pub fade: u8,
    /// The glyph cell's paint when it is not the row's own glyph: ✓ / ⚠ on
    /// an echo.
    pub glyph: Option<char>,
    /// What the motion is.
    pub anim: Anim,
}

/// One frame of the whole band: the frame instant and one [`RowMotion`] per
/// row of the [`crate::glass::Presentation`] it was computed against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BandMotion {
    /// The frame instant (on the center's grid).
    pub at: Instant,
    /// Parallel to `Presentation::rows`.
    pub rows: Vec<RowMotion>,
}

impl BandMotion {
    /// The repaint-key term: 0 when no row has motion or an indicator, else a
    /// nonzero FNV-1a over what the frame draws, never over `at`, so two
    /// frames that draw the same surfaces are the same key.
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

// ---- easing (Q16) ------------------------------------------------------------

const ONE: u64 = 65_536;

/// Smoothstep: `p²(3 − 2p)`.
#[must_use]
pub(crate) fn smooth(p: u32) -> u32 {
    let p = u64::from(p).min(ONE);
    let p2 = (p * p) >> 16;
    u32::try_from((p2 * (3 * ONE - 2 * p)) >> 16).unwrap_or(u32::MAX)
}

/// The comet's breathing sweep: a blend of the linear and smoothstep ramps
/// whose velocity runs from 0.72 to 1.14 of the mean — the head never stops
/// and never lingers, and its pace is the same at both ends, so one crossing
/// hands over to the next without a jolt.
#[must_use]
pub(crate) fn breath(p: u32) -> u32 {
    let p = u64::from(p).min(ONE);
    let v = (47_186 * p + 18_350 * u64::from(smooth(u32::try_from(p).unwrap_or(0)))) >> 16;
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// Ease-out cubic: `1 − (1 − u)³`.
#[must_use]
#[cfg(test)]
pub(crate) fn ease_out_cubic(u: u32) -> u32 {
    let v = ONE - u64::from(u).min(ONE);
    u32::try_from(ONE - ((((v * v) >> 16) * v) >> 16)).unwrap_or(u32::MAX)
}

/// Ease-out quadratic: `1 − (1 − u)²`, a glide's curve: peak speed twice the
/// mean, at the start, and no overshoot.
#[must_use]
pub fn ease_out_quad(u: u32) -> u32 {
    let v = ONE - u64::from(u).min(ONE);
    u32::try_from(ONE - ((v * v) >> 16)).unwrap_or(u32::MAX)
}

/// The bell: `4u(1 − u)`, 0 at both ends and 1 in the middle.
#[must_use]
#[cfg(test)]
pub(crate) fn bell(u: u32) -> u32 {
    let u = u64::from(u).min(ONE);
    u32::try_from((4 * u * (ONE - u)) >> 16).unwrap_or(u32::MAX)
}

/// `t` as a Q16 fraction of `span`, clamped to [0, 1].
fn frac(t: Duration, span: Duration) -> u32 {
    let span = span.as_millis().max(1);
    let t = t.as_millis().min(span);
    u32::try_from(t * u128::from(ONE) / span).unwrap_or(u32::MAX)
}

fn ms32(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
}

/// `q` 0..=255 scaled by a Q16 fraction.
fn scale(q: u8, f: u32) -> u8 {
    u8::try_from((u64::from(q) * u64::from(f).min(ONE)) >> 16).unwrap_or(255)
}

/// Permille of the row as a Q16 fraction.
fn permille_q16(p: u32) -> i64 {
    i64::from(p.min(1000)) * i64::from(ROW) / 1000
}

// ---- the recipes --------------------------------------------------------------

/// How long a glide from `from` to `to` (permille) takes: [`FILL_GLIDE`] plus
/// [`FILL_GLIDE_PER_POINT`] per percentage point, at most [`FILL_GLIDE_MAX`],
/// so a big jump is not a staircase of hops and a small one lands quickly.
#[must_use]
pub fn glide_span(from: u16, to: u16) -> Duration {
    let points_x10 = u64::from(from.abs_diff(to));
    let per = u64::try_from(FILL_GLIDE_PER_POINT.as_micros()).unwrap_or(u64::MAX);
    let extra = Duration::from_micros(per.saturating_mul(points_x10) / 10);
    (FILL_GLIDE + extra).min(FILL_GLIDE_MAX)
}

/// The shown fill of a data jump from `from` to `to`, `t` into its
/// [`glide_span`] — eased out (quadratic), so the bar lands softly and moves
/// monotonically toward the data.
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

/// The comet's head, Q16 of the row, `age` into its crossing: from one
/// leading edge left of the row (nothing lit) to one tail right of it
/// (nothing lit), breathing, over [`COMET_PERIOD`].
#[must_use]
pub(crate) fn comet_head(age: Duration) -> i64 {
    let lead = permille_q16(COMET_LEAD_PERMILLE);
    let tail = permille_q16(COMET_PERMILLE - COMET_LEAD_PERMILLE);
    let travel = i64::from(ROW) + lead + tail;
    -lead + ((i64::from(breath(frac(age, COMET_PERIOD))) * travel) >> 16)
}

/// The comet `since` into the motion: one comet on the row at a time, entering
/// at the left edge as the last one leaves at the right.
///
/// Graded: a tail [`COMET_PERMILLE`] − [`COMET_LEAD_PERMILLE`] of the row long,
/// its fill rising as the square of the distance into the hot head
/// ([`Tone::HEAD`], lifted toward the glint over the tail's last half), then a
/// soft leading edge falling to the track over [`COMET_LEAD_PERMILLE`]; no hard
/// edge anywhere. Flat: a solid full-ink segment with hard ends, same timing.
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
    // Rounded UP, so the tail's faint end is never a run of empty stops.
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

/// The glint's radius, Q16 of the row, on a row `cols` columns wide:
/// [`GLINT_RADIUS_PERMILLE`], but never under [`GLINT_MIN_HALF_CELLS`] half
/// cells (`cols` 0: no floor).
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
    // ½(1 + cos πx) = cos²(πx/2), x = d/r in Q16, with cos(πx/2) by its Taylor
    // series to the sixth power (under 2e-4 off after squaring, over [0, 1]).
    let x = (d << 16) / r; // 0..65536
    let y = (x * 102_944) >> 16; // πx/2 in Q16 (π/2 = 1.5708 → 102 944)
    let y2 = (y * y) >> 16;
    let y4 = (y2 * y2) >> 16;
    let y6 = (y4 * y2) >> 16;
    let c = 65_536 - y2 / 2 + y4 / 24 - y6 / 720;
    let c = c.clamp(0, 65_536);
    let c2 = (c * c) >> 16;
    u8::try_from((i64::from(peak) * c2 + 32_768) >> 16).unwrap_or(peak)
}

/// Whether a bar at `shown` carries a glint: a fill of at least half the
/// glint's radius (on a shorter fill its centre is never on it).
#[must_use]
pub(crate) fn bar_glints(shown: u16) -> bool {
    u32::from(shown) * 2 >= GLINT_RADIUS_PERMILLE
}

/// A determinate bar at `shown` permille of the row (0 % an empty track, 100 %
/// the window edge to edge): the fill ink from the left edge to the data, a
/// hard edge there ([`Surface::edge`]), and the track beyond. `glint` is how
/// far into its travel a glint is (graded bars only; it lifts the fill, never
/// the track).
#[must_use]
pub fn bar(shown: u16, glint: Option<Duration>, graded: bool) -> Surface {
    bar_at(permille_q16(u32::from(shown)), glint, graded, 0)
}

/// [`bar`] with its edge at `edge` (Q16 of the row, so a glide reads finer
/// than a permille) on a row `cols` columns wide (the glint's least width,
/// [`glint_radius`]; 0 for none).
#[must_use]
pub fn bar_at(edge: i64, glint: Option<Duration>, graded: bool, cols: usize) -> Surface {
    bar_in(edge, glint, graded, Tone::FULL, cols)
}

/// The bar of a download whose bytes have stalled: the fill dimmed
/// ([`Tone::STALLED`]) with no glint, so no sheen says "working" beside the
/// `stalled` words. Flat looks keep the full ink; the words carry the stall.
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

/// A busy row's still form: the unlit track.
#[must_use]
pub(crate) fn track(graded: bool) -> Surface {
    Surface::uniform(Tone::TRACK, !graded)
}

/// How long an echo holds its slot: a Complete's fill glide ([`glide_span`]
/// from where the bar stood to the window's edge), sweep and fade; a Fault's
/// wash and fade; a Vanish's fade.
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

/// A completion echo `t` into its span: the row's surface and the fade, over
/// the whole row. Not a row: not pressable, not announced.
///
/// * Complete, moving and graded: the fill glides to the right edge
///   ([`glide_span`]), one sweep crosses the row ([`ECHO_SWEEP`]), then the row
///   fades ([`ECHO_FADE`], ease-in).
/// * Fault on a bar: the fill cross-fades to a warn wash over what it covered
///   ([`ECHO_FAULT_CROSS`], premultiplied), holds to [`ECHO_FAULT_FLASH`], and
///   fades; the empty track keeps its tone.
/// * Fault on a busy row: the comet freezes where the work failed and drains to
///   the track ([`ECHO_DRAIN`]); no warn on the surface, the ⚠ and `failed`
///   carry it.
/// * Vanish: the last bar fades; a busy row's comet runs on from where the row
///   retired ([`Echo::comet_since`]) while it fades.
/// * Still: a full bar (Complete), the wash (a bar's Fault), the track (a busy
///   Fault), blank (Vanish).
/// * Flat: no lift, tint or fade; Complete wipes with a hard edge, then holds.
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
            // Frozen where it failed (never running on), drained to the
            // track: the row goes neutral and the ⚠ and `failed` say it.
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

/// [`glide`] in Q16 of the row, unquantized: the edge a glide shows at `t`,
/// monotone toward `to`.
#[must_use]
pub fn glide_q16(from: u16, to: u16, t: Duration) -> i64 {
    let u = i64::from(ease_out_quad(frac_fine(t, glide_span(from, to))));
    let (a, b) = (permille_q16(u32::from(from)), permille_q16(u32::from(to)));
    a + (((b - a) * u) >> 16)
}

/// The current comet's phase at `q` for a motion epoch `epoch` (the
/// descriptor's; [`comet`] reads the whole span since the epoch).
#[must_use]
pub(crate) fn comet_phase(q: Instant, epoch: Instant) -> Duration {
    let since = q.saturating_duration_since(epoch).as_millis();
    Duration::from_millis(u64::try_from(since % COMET_PERIOD.as_millis()).unwrap_or(0))
}

/// How far into its travel the glint is at `q`, or `None` while it rests
/// (or before its first travel, [`GLINT_DELAY`] after the epoch).
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
        assert_eq!(bell(0), 0);
        assert_eq!(bell(65_536), 0);
        assert_eq!(bell(32_768), 65_536);
        assert_eq!(ease_out_quad(0), 0);
        assert_eq!(ease_out_quad(65_536), 65_536);
        assert_eq!(ease_out_quad(32_768), 49_152, "three quarters at half time");
        let span = glide_span(100, 900);
        assert_eq!(span, FILL_GLIDE_MAX, "80 points: 180 + 560 ms, capped");
        assert_eq!(glide(100, 900, Duration::ZERO), 100);
        assert_eq!(glide(100, 900, span), 900);
        assert!(glide(100, 900, span / 2) > 500, "eased out");
        assert_eq!(glide(900, 100, span), 100, "down too");
        // The span scales with the distance.
        assert_eq!(glide_span(500, 500), FILL_GLIDE);
        assert!(glide_span(0, 100) < glide_span(0, 500));
        assert_eq!(glide_span(600, 100), glide_span(0, 500), "both ways");
        assert_eq!(glide_span(0, 1000), FILL_GLIDE_MAX);
        // Breath's velocity: never zero, never more than 1.14 of the mean.
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
        // Premultiplied: the fill and warn shares never sum past the
        // coverage, so every point is inside the track–fill–warn hull.
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
