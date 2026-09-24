// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The motion layer (design §10.4.6, amended by rulings 136–141 on the merge
//! with main's full-width meter): what a row's indicator looks like at one
//! frame instant, as data the host paints — never as time folded into the
//! layout. The [`crate::glass::Presentation`] stays TIME-FREE and
//! fingerprinted as before; a [`BandMotion`] overlays only the row's
//! SURFACE, the time slots, the glyph cell and an echo's fade, so an
//! animation never re-runs the width law and NEVER RE-GRIDS.
//!
//! # Fractions of the row, not cells (ruling 140)
//!
//! THE METER IS THE ROW (ruling 55): 0 % is the window's left edge and 100 %
//! its right edge, gutters included. So the motion speaks in FRACTIONS of the
//! row ([`ROW`] = the whole of it, Q16) and the host maps them onto its
//! window's pixels: a [`Surface`] is a piecewise-linear profile of
//! [`Tone`]s along the row, and [`Surface::span`] gives the tone of any pixel
//! span of it — a cell, a gutter — as the MEAN over the span. That mean IS
//! the sub-cell coverage ruling 138 asks for: the cell under a fill's edge
//! takes `mix(track, fill, coverage)`, where coverage is the lit fraction of
//! its window-pixel span, and the comet's soft gradient and the glint come
//! through the same integral. Every cell is a whole-cell BACKGROUND tone —
//! never meter ink, which the per-cell contrast floor could repaint (ruling
//! 55's reason) — and the motion is smooth because the TONES move
//! continuously, not because a glyph splits a cell.
//!
//! Everything here is clockless and integer: positions are Q16 fractions of
//! the row, phases milliseconds. The same state at the same frame instant is
//! the same frame on every target, so CPU, GPU, native and wasm draw
//! identical pixels.
//!
//! # The looks
//!
//! * **Moving, graded** — the comet for BUSY work: a soft gradient of tones
//!   about a fifth of the row long ([`COMET_PERMILLE`]) — a tail rising as
//!   the square of its length into a hot head, and a soft leading edge — that
//!   enters through the window's left edge and leaves through its right, one
//!   breathing crossing per [`COMET_PERIOD`], the next entering as it leaves;
//!   with it the braille [`SPINNER`] in the glyph cell every
//!   [`SPIN_FRAMES`] frames. The bar for work with a fill: its edge at the
//!   data (sub-cell coverage), a data glide and a travelling glint (dimmed,
//!   the glint parked, while the bytes are stalled). The echo's fill, glow
//!   and fade on completion, over the whole row.
//! * **Still** — the bar at its data, a busy row's unlit track, a held ✓:
//!   the information stays, the movement goes.
//! * **Flat** (High Contrast) — track and full fill only: a cell takes the
//!   tone at its centre ([`Surface::flat`]), no glint, no tint, no fade.

#![allow(
    clippy::many_single_char_names,
    reason = "the recipes use the design's own symbols — x, e, g, h, t, u, q — so each line reads against §10.4.6"
)]

use crate::center::{Echo, EchoKind};
use crate::glass::Fnv;
use crate::{
    ANIM_FRAME, COMET_HEAD_LIFT, COMET_LEAD_PERMILLE, COMET_PERIOD, COMET_PERMILLE, Duration,
    ECHO_FADE, ECHO_FAULT_FLASH, ECHO_FILL, ECHO_GLOW, FILL_GLIDE, GLINT_DELAY, GLINT_PEAK,
    GLINT_PERIOD, GLINT_RADIUS_PERMILLE, GLINT_TRAVEL, Instant, SPIN_FRAMES, SPINNER,
};

/// The whole row as a fraction: Q16, `0` at the window's left edge and `ROW`
/// at its right edge (ruling 55's mapping).
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

/// One tone of the row's surface, as three mixes the host resolves against
/// its palette: `fill` from the track toward the fill ink (the theme's cursor
/// accent — the owner's "cursor trail theme" — `warn` on a Warn/Error row,
/// HIGHLIGHT under High Contrast: ruling 137), then `lift` toward the glint
/// ink (a lift of the fill tone), then `warn` toward the fault hue (each
/// 0–255).
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
    pub const FULL: Tone = Tone {
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
    /// A determinate fill whose bytes have STALLED: a dim slate of the fill
    /// ink that reads as DORMANT, the glint parked — the yellow `stalled`
    /// word beside it carries the warning. (Mixing warn into the fill gave
    /// sage green, the success hue, right beside that word: review round 2,
    /// 2026-09-23.)
    pub const STALLED: Tone = Tone {
        fill: 120,
        lift: 0,
        warn: 0,
    };

    /// Whether the tone lights anything.
    #[must_use]
    pub const fn lit(self) -> bool {
        self.fill > 0 || self.lift > 0 || self.warn > 0
    }
}

/// A [`Tone`] read to 1/256 of a step in each channel (`0..=255·256`) —
/// what [`Surface::span_fine`] gives a host that mixes a cell's colour and
/// rounds it ONCE: a byte tone per cell rounds each cell's mean on its own,
/// and at the comet's tail those ±½-step errors alternated along the row
/// (design ruling 157).
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

/// A row's surface at one frame: the tones along the WHOLE row, left edge to
/// right edge, piecewise-linear between [`Stop`]s. Two stops at one `at` are
/// a hard edge (the left limit, then the right). Empty: the row has no meter
/// and no track — it paints its plain band.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct Surface {
    /// Left to right, `at` non-decreasing; the first at 0 and the last at
    /// [`ROW`] when not empty.
    pub stops: Vec<Stop>,
    /// The flat look: a pixel span takes the tone at its CENTRE — hard edges,
    /// whole palette inks (High Contrast). Otherwise the MEAN over the span:
    /// sub-cell coverage.
    pub flat: bool,
}

impl Surface {
    /// The whole row in one tone.
    #[must_use]
    pub fn uniform(tone: Tone, flat: bool) -> Self {
        Self {
            stops: vec![Stop { at: 0, tone }, Stop { at: ROW, tone }],
            flat,
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
    pub fn tone_at(&self, at: u32) -> Tone {
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

    /// The tone of the pixel span `[x0, x1)` of a row `w` pixels wide — a
    /// cell, or a gutter — mapped as ruling 55 maps the fill: pixel 0 is the
    /// row's left edge and pixel `w` its right. Graded: the MEAN tone over
    /// the span (the sub-cell coverage of ruling 138: the cell under a fill's
    /// edge is `mix(track, fill, coverage)`). Flat: the tone at the span's
    /// centre — except at the row's ENDS, which keep ruling 55's honest ends
    /// in whole cells (main's `lit.clamp(1, cols − 1)`): the span that starts
    /// at the left edge takes the tone AT that edge, so any started fill
    /// lights it, and the span that ends at the right edge takes the tone
    /// just inside it, so only a whole fill does (review 2026-09-24: with
    /// the gutters in the edge columns, their centres left 1 ‰ an empty
    /// track and lit the whole window at 99 %). A span that is the whole row
    /// takes its centre. The centre is read as its left limit, so a span
    /// whose centre sits exactly on a fill's edge is lit (main's "every
    /// column whose centre is at or left of the edge"). [`Tone::TRACK`] on an empty surface or a degenerate
    /// row.
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

    /// The span's tone when it is a POINT (an empty surface, a degenerate
    /// row, the flat look), else each channel's trapezoid sum scaled by
    /// 2^20 and the span it is over.
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
                    // The LEFT limit at the centre: a column whose centre is
                    // at or left of a fill's edge is lit (main's mapping), so
                    // one column is lit from 500 ‰.
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

    /// The tones of `cols` equal cells across the row — the engine's own
    /// cell-resolution reading of a frame, which the motion deadline compares
    /// to ask only for frames that draw something new. (The host's cells sit
    /// between its gutters; this grid is the row without them.)
    #[must_use]
    pub fn cells(&self, cols: usize) -> Vec<Tone> {
        let w = cols as u64;
        (0..w).map(|c| self.span(c, c + 1, w)).collect()
    }

    /// Every lit stop's tone with its WARN mixed to `w` — the empty track
    /// keeps its own tone (a warn-tinted track read as mud, review
    /// 2026-09-23) unless `track_too` (a busy row's echo, whose whole row is
    /// the only surface there is to flash).
    #[must_use]
    fn warned(mut self, w: u8, track_too: bool) -> Self {
        for s in &mut self.stops {
            if s.tone != Tone::TRACK || track_too {
                s.tone.warn = w;
            }
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

/// Clip a profile given at positions that may lie OUTSIDE the row (a comet
/// entering or leaving) to `[0, ROW]`: the tones at the two ends are
/// interpolated, the points inside kept — hard edges included. Beyond the
/// first and last point the profile is the track.
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
    // One allocation per surface: the start, every interior point, the end
    // (a busy row's 32-chord tail would otherwise regrow a `vec![_]` five
    // times on every frame).
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
    Surface { stops, flat }
}

/// What a row's motion is doing — the descriptor the tests read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Anim {
    /// No motion, no indicator.
    #[default]
    None,
    /// The comet and the spinner.
    Comet {
        /// Milliseconds into the current comet's crossing.
        phase_ms: u32,
        /// The spinner's frame, an index into [`SPINNER`].
        spin: u8,
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
    /// The elapsed words of busy work.
    pub readout: Option<String>,
    /// The ETA words, `stalled`, or the elapsed clock while the estimate is
    /// hidden.
    pub eta: Option<String>,
    /// An echo's fade: 0 opaque … 255 gone.
    pub fade: u8,
    /// The glyph cell's paint when it is not the row's own glyph: the
    /// spinner's frame on a moving busy row, ✓ / ⚠ on an echo.
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
    /// The repaint-key term: **0 when no row has motion or an indicator**,
    /// else a nonzero FNV-1a over what the frame draws — never over `at`, so
    /// two frames that draw the same surfaces are the same key.
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
pub fn smooth(p: u32) -> u32 {
    let p = u64::from(p).min(ONE);
    let p2 = (p * p) >> 16;
    u32::try_from((p2 * (3 * ONE - 2 * p)) >> 16).unwrap_or(u32::MAX)
}

/// The comet's breathing sweep: a blend of the linear and smoothstep ramps
/// whose velocity runs from 0.72 to 1.14 of the mean — the head never stops
/// and never lingers, and its pace is the same at both ends, so one crossing
/// hands over to the next without a jolt.
#[must_use]
pub fn breath(p: u32) -> u32 {
    let p = u64::from(p).min(ONE);
    let v = (47_186 * p + 18_350 * u64::from(smooth(u32::try_from(p).unwrap_or(0)))) >> 16;
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// Ease-out cubic: `1 − (1 − u)³`.
#[must_use]
pub fn ease_out_cubic(u: u32) -> u32 {
    let v = ONE - u64::from(u).min(ONE);
    u32::try_from(ONE - ((((v * v) >> 16) * v) >> 16)).unwrap_or(u32::MAX)
}

/// The bell: `4u(1 − u)`, 0 at both ends and 1 in the middle.
#[must_use]
pub fn bell(u: u32) -> u32 {
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

/// The shown fill of a data jump from `from` to `to`, `t` into its
/// [`FILL_GLIDE`] — eased out, so the bar lands softly.
#[must_use]
pub fn glide(from: u16, to: u16, t: Duration) -> u16 {
    let u = u64::from(ease_out_cubic(frac(t, FILL_GLIDE)));
    let (from, to) = (i64::from(from), i64::from(to));
    let v = from + ((to - from) * i64::try_from(u).unwrap_or(0)) / 65_536;
    u16::try_from(v.clamp(0, 1000)).unwrap_or(1000)
}

/// The comet's head, Q16 of the row, `age` into its crossing: from one
/// leading edge LEFT of the row (nothing lit) to one tail RIGHT of it
/// (nothing lit), breathing, over [`COMET_PERIOD`].
#[must_use]
pub fn comet_head(age: Duration) -> i64 {
    let lead = permille_q16(COMET_LEAD_PERMILLE);
    let tail = permille_q16(COMET_PERMILLE - COMET_LEAD_PERMILLE);
    let travel = i64::from(ROW) + lead + tail;
    -lead + ((i64::from(breath(frac(age, COMET_PERIOD))) * travel) >> 16)
}

/// The comet `since` into the motion: one comet on the row at a time — it
/// enters through the window's left edge as the last one leaves through the
/// right, so the row is never empty for longer than the crossing's last
/// frame.
///
/// Graded: a tail [`COMET_PERMILLE`] − [`COMET_LEAD_PERMILLE`] of the row
/// long, its fill rising as the square of the distance into the hot head
/// ([`Tone::HEAD`], lifted toward the glint over the tail's last half), then
/// a soft leading edge falling to the track over [`COMET_LEAD_PERMILLE`] —
/// one continuous gradient with no hard edge anywhere (every stop at its own
/// position); the window's own edges are the only ends it has. Flat: a solid
/// segment in the full ink with hard ends, the same timing.
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
/// 60 columns, ~32 at 200) sees the curve and not the chords. At 8 the
/// steps between neighbouring cells came in pairs and jumped at each chord's
/// end — faint vertical stripes along the tail at capture scale (design
/// ruling 157).
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

/// The glint's lift at Q16 position `c` for a glint centred at `g`: a
/// smooth bump `(1 − d²/r²)²` of radius [`GLINT_RADIUS_PERMILLE`].
fn glint_lift(c: i64, g: i64) -> u8 {
    let r = permille_q16(GLINT_RADIUS_PERMILLE).max(1);
    let d = (c - g).abs();
    if d >= r {
        return 0;
    }
    let q = 256 - d * d * 256 / (r * r);
    u8::try_from((i64::from(GLINT_PEAK) * ((q * q) >> 8)) >> 8).unwrap_or(255)
}

/// Whether a bar at `shown` carries a glint: a fill of at least half the
/// glint's radius (on a shorter fill its centre is never on it).
#[must_use]
pub fn bar_glints(shown: u16) -> bool {
    u32::from(shown) * 2 >= GLINT_RADIUS_PERMILLE
}

/// A determinate bar at `shown` permille OF THE ROW (ruling 55: 0 % an empty
/// track, 100 % the window edge to edge, 50 % its middle): the fill ink from
/// the row's left edge to the data, a hard edge there — the cell under it
/// takes its coverage through [`Surface::span`] — and the track beyond.
/// `glint` is how far into its travel a glint is (graded bars only; it lifts
/// the fill, never the track).
#[must_use]
pub fn bar(shown: u16, glint: Option<Duration>, graded: bool) -> Surface {
    bar_in(shown, glint, graded, Tone::FULL)
}

/// The bar of a download whose bytes have STALLED: the fill dimmed
/// ([`Tone::STALLED`]) with no glint — the travelling sheen said "working"
/// while the words said "stalled" (review 2026-09-23). Flat looks keep the
/// full ink: High Contrast discards gradation, and its words carry the
/// stall.
#[must_use]
pub fn stalled_bar(shown: u16, graded: bool) -> Surface {
    let tone = if graded { Tone::STALLED } else { Tone::FULL };
    bar_in(shown, None, graded, tone)
}

fn bar_in(shown: u16, glint: Option<Duration>, graded: bool, ink: Tone) -> Surface {
    let e = permille_q16(u32::from(shown));
    let row = i64::from(ROW);
    let g = glint.filter(|_| graded && bar_glints(shown)).map(|t| {
        let r = permille_q16(GLINT_RADIUS_PERMILLE);
        let u = i64::from(smooth(frac(t, GLINT_TRAVEL)));
        -r + ((u * (e + 2 * r)) >> 16)
    });
    let tone = |c: i64| match g {
        Some(g) => Tone {
            lift: glint_lift(c, g),
            ..ink
        },
        None => ink,
    };
    let mut points: Vec<(i64, Tone)> = vec![(0, tone(0))];
    if let Some(g) = g {
        let r = permille_q16(GLINT_RADIUS_PERMILLE);
        for k in -8..=8i64 {
            let x = g + r * k / 8;
            if x > 0 && x < e {
                points.push((x, tone(x)));
            }
        }
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
    Surface { stops, flat }
}

/// A busy row's still form: the unlit track (main's still busy row).
#[must_use]
pub fn track(graded: bool) -> Surface {
    Surface::uniform(Tone::TRACK, !graded)
}

/// The spinner's glyph at `q` for a motion epoch `epoch`: one step of
/// [`SPINNER`] every [`SPIN_FRAMES`] frames of the grid (main's 125 ms
/// cadence).
#[must_use]
pub fn spin_at(q: Instant, epoch: Instant) -> u8 {
    let frames = q.saturating_duration_since(epoch).as_millis() / ANIM_FRAME.as_millis().max(1);
    let step = frames / u128::from(SPIN_FRAMES.max(1));
    u8::try_from(step % SPINNER.len() as u128).unwrap_or(0)
}

/// A completion echo `t` into its span: the row's surface and the fade. The
/// band's end of its own indicator — not a row, not pressable, not
/// announced — over the WHOLE row (ruling 141).
///
/// * Complete, moving and graded: the fill wipes to the window's right edge
///   ([`ECHO_FILL`]), blooms toward the glint ([`ECHO_GLOW`], a bell), then
///   fades ([`ECHO_FADE`], ease-in).
/// * Fault: the last bar's FILL (a busy row: the whole row) rises to the
///   fault hue ([`ECHO_FAULT_FLASH`], ease-out) and holds it through the
///   fade — a failure never ends on a success-coloured bar, and the empty
///   track keeps its own tone.
/// * Vanish: the last bar fades.
/// * A busy row's "last bar" is its comet, running on from where the row
///   retired ([`Echo::comet_since`]) under a moving graded look — the echo's
///   first frame is the next live one, never a bare track (ruling 162) — and
///   the unlit track otherwise.
/// * Still: a full bar held (Complete), the fault tint held (Fault), blank
///   (Vanish).
/// * Flat: no lift, no tint and no fade; Complete wipes with a hard edge,
///   then holds.
#[must_use]
pub fn echo(e: &Echo, t: Duration, look: Look) -> (Surface, u8) {
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
        let u = u64::from(frac(t.saturating_sub(start), ECHO_FADE));
        u8::try_from((255 * ((u * u) >> 16)) >> 16).unwrap_or(255)
    };
    let full = || bar(1000, None, look.graded);
    match (e.kind, look.pace, look.graded) {
        (EchoKind::Complete, Pace::Moving, true) => {
            if t < ECHO_FILL {
                (bar(glide(from, 1000, t), None, true), 0)
            } else if t < ECHO_FILL + ECHO_GLOW {
                let u = frac(t.saturating_sub(ECHO_FILL), ECHO_GLOW);
                let lift = u8::try_from((200 * u64::from(bell(u))) >> 16).unwrap_or(200);
                let tone = Tone {
                    fill: 255,
                    lift,
                    warn: 0,
                };
                (Surface::uniform(tone, false), 0)
            } else {
                (full(), fade_over(t, ECHO_FILL + ECHO_GLOW))
            }
        }
        (EchoKind::Complete, Pace::Moving, false) => {
            if t < ECHO_FILL {
                (bar(glide(from, 1000, t), None, false), 0)
            } else {
                (full(), 0)
            }
        }
        (EchoKind::Complete, Pace::Still, _) => (full(), 0),
        (EchoKind::Fault | EchoKind::Vanish, _, false) => (last(), 0),
        (EchoKind::Fault, Pace::Still, true) => (last().warned(255, e.indeterminate), 0),
        (EchoKind::Fault, Pace::Moving, true) => {
            if t < ECHO_FAULT_FLASH {
                let w = scale(255, ease_out_cubic(frac(t, ECHO_FAULT_FLASH)));
                (last().warned(w, e.indeterminate), 0)
            } else {
                (
                    last().warned(255, e.indeterminate),
                    fade_over(t, ECHO_FAULT_FLASH),
                )
            }
        }
        (EchoKind::Vanish, Pace::Moving, true) => (last(), fade_over(t, Duration::ZERO)),
        (EchoKind::Vanish, Pace::Still, true) => (last(), 255),
    }
}

/// The current comet's phase at `q` for a motion epoch `epoch` (the
/// descriptor's; [`comet`] reads the whole span since the epoch).
#[must_use]
pub fn comet_phase(q: Instant, epoch: Instant) -> Duration {
    let since = q.saturating_duration_since(epoch).as_millis();
    Duration::from_millis(u64::try_from(since % COMET_PERIOD.as_millis()).unwrap_or(0))
}

/// How far into its travel the glint is at `q`, or `None` while it rests
/// (or before its first travel, [`GLINT_DELAY`] after the epoch).
#[must_use]
pub fn glint_at(q: Instant, epoch: Instant) -> Option<Duration> {
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
pub fn anim_ms(d: Duration) -> u32 {
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
        assert_eq!(glide(100, 900, Duration::ZERO), 100);
        assert_eq!(glide(100, 900, FILL_GLIDE), 900);
        assert!(glide(100, 900, FILL_GLIDE / 2) > 500, "eased out");
        assert_eq!(glide(900, 100, FILL_GLIDE), 100, "down too");
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

    /// RULING 55'S MAPPING, KEPT BY RULINGS 136 AND 138: a bar's fill is a
    /// fraction of the WINDOW — 0 % lights nothing, 100 % the whole row edge
    /// to edge (both gutters included), 50 % exactly the left half — and the
    /// one cell under the edge takes `mix(track, fill, coverage)`: the lit
    /// fraction of its window-pixel span, never a glyph split. Measured over
    /// a window of 1283 px with 7 px and 12 px gutters around 80 cells of
    /// 15.8 px (whole pixels: the host's `MeterSpan` geometry).
    #[test]
    fn a_bar_maps_its_fraction_onto_the_window_with_edge_coverage() {
        let (w, lo, cw, cols) = (1283u64, 7u64, 16u64, 79u64);
        let hi_start = lo + cw * cols; // the right gutter
        let cell = |s: &Surface, i: u64| s.span(lo + i * cw, lo + (i + 1) * cw, w);
        let empty = bar(0, None, true);
        assert!((0..cols).all(|i| cell(&empty, i) == Tone::TRACK));
        assert_eq!(empty.span(0, lo, w), Tone::TRACK, "the left gutter");
        let full = bar(1000, None, true);
        assert!((0..cols).all(|i| cell(&full, i) == Tone::FULL));
        assert_eq!(full.span(0, lo, w), Tone::FULL, "the left gutter");
        assert_eq!(full.span(hi_start, w, w), Tone::FULL, "the right gutter");
        let half = bar(500, None, true);
        let x = w / 2; // 641.5 → the edge cell spans it
        for i in 0..cols {
            let (a, b) = (lo + i * cw, lo + (i + 1) * cw);
            let t = cell(&half, i);
            if b * 2 <= w {
                assert_eq!(t, Tone::FULL, "cell {i} left of the middle");
            } else if a * 2 >= w {
                assert_eq!(t, Tone::TRACK, "cell {i} right of the middle");
            } else {
                // The coverage: (w/2 − a) / cw of the cell is lit.
                let want = (255 * (w - 2 * a) / (2 * cw)) as i64;
                assert!(
                    (i64::from(t.fill) - want).abs() <= 1,
                    "the edge cell {i} at {x}: {t:?}, want {want}"
                );
            }
        }
        // Coverage moves continuously: a one-permille step moves the edge
        // cell's tone, never a whole cell at once.
        let mut prev = cell(&bar(400, None, true), 31);
        for p in 401..=420u16 {
            let t = cell(&bar(p, None, true), 31).fill;
            assert!(t >= prev.fill && t - prev.fill <= 22, "{p}: {t}");
            prev = cell(&bar(p, None, true), 31);
        }
        // Flat (High Contrast): the centre decides, whole inks only.
        let flat = bar(500, None, false);
        assert!((0..cols).all(|i| {
            let t = cell(&flat, i);
            t == Tone::TRACK || t == Tone::FULL
        }));
        // …and the ENDS keep ruling 55's honest ends in whole cells (main's
        // `lit.clamp(1, cols − 1)`), over the host's columns, whose first
        // and last carry the gutters out to the window's edges: any started
        // fill lights the first, only a whole one the last (review
        // 2026-09-24 — the centres left 1 ‰ an empty track and lit the whole
        // window at 99.x %).
        let column = |s: &Surface, i: u64| {
            let x0 = if i == 0 { 0 } else { lo + i * cw };
            let x1 = if i + 1 == cols { w } else { lo + (i + 1) * cw };
            s.span(x0, x1, w)
        };
        let lit = |p: u16| -> Vec<bool> {
            let s = bar(p, None, false);
            (0..cols).map(|i| column(&s, i) == Tone::FULL).collect()
        };
        assert!(lit(0).iter().all(|l| !l), "0 % is an empty track");
        assert!(lit(1000).iter().all(|l| *l), "100 % is the whole window");
        for p in 1..1000u16 {
            let l = lit(p);
            assert!(l[0], "{p}: a started pass lights the first column");
            assert!(
                !l[cols as usize - 1],
                "{p}: only a whole fill lights the last column"
            );
            assert!(
                l.windows(2).all(|pair| pair[0] || !pair[1]),
                "{p}: the lit columns are one run from the left"
            );
        }
        // A one-column row is its centre.
        assert_eq!(bar(499, None, false).span(0, w, w), Tone::TRACK);
        assert_eq!(bar(500, None, false).span(0, w, w), Tone::FULL);
    }

    /// The integral is exact: a span's mean over a linear ramp is the ramp at
    /// its middle; a span straddling a hard edge is the covered fraction; a
    /// zero-width span is the point tone; an empty surface is the track.
    #[test]
    fn a_span_is_the_mean_of_the_profile_over_it() {
        let ramp = Surface {
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
        assert_eq!(bar(620, None, true), bar_in(620, None, true, Tone::FULL));
        assert!(!bar_glints(10) && bar_glints(15));
    }

    /// Every echo covers the whole row: a Complete echo's wipe ends at the
    /// window's right edge (100 % is the window edge to edge), a busy row's
    /// Fault flashes its whole row, and the track keeps its own tone on a
    /// bar's Fault.
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
            load_slot: false,
        };
        let (s, fade) = echo(&e, ECHO_FILL, Look::MOVING);
        assert_eq!(fade, 0);
        assert!(s.cells(80).iter().all(|t| t.fill == 255), "{s:?}");
        e.kind = EchoKind::Fault;
        let (s, _) = echo(&e, ECHO_FAULT_FLASH, Look::MOVING);
        let cells = s.cells(80);
        assert!(cells[..20].iter().all(|t| t.warn == 255));
        assert!(
            cells[30..].iter().all(|t| *t == Tone::TRACK),
            "the track keeps its tone"
        );
        e.indeterminate = true;
        let (s, _) = echo(&e, ECHO_FAULT_FLASH, Look::MOVING);
        assert!(
            s.cells(80).iter().all(|t| t.warn == 255),
            "a busy row's whole row"
        );
    }
}
