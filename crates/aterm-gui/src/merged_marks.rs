// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The merged deck's marks (`DESIGN-top-deck-merged-2026-10-21.md` §4): a part tile, the score
//! gauge, the run and its named ends, a scope's share bar, the capacity wall, the slats of work
//! waiting past it and the hatch of compressed memory, drawn on a [`Painter`] in device px.
//!
//! Lengths are the mockups' (`docs/design-studies/top-deck-*-2026-10-23.png`, drawn at 2×) in
//! points, scaled by the painter's `s`. Every mark draws inside the rect it is given except the
//! gauge's needs ring, which reaches [`Marks::ring_reach`] past the gauge.

use aterm_vitals::ink::{MergedInks, Rgb};
use aterm_vitals::{HeatCell, Motion, TrendView};

use crate::deck_face::FaceMetrics;
use crate::stats_paint::{Painter, Rect, a, panel};
use crate::type_scale::StepPx;
use crate::widget::{DrawPrim, Rgba, TextFace, TextWeight, rgba, text_prim};

/// Bars in a 10-minute run: one per 30 s.
pub(crate) const RUN_BARS: usize = 20;

/// The most slats the work waiting past the wall draws, one per core's worth.
pub(crate) const MAX_SLATS: usize = 10;

/// A share of capacity in `0..=1`; not a number is none.
fn unit(v: f32) -> f32 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

/// One reading as the marks draw it, each figure a share of capacity (`0..=1`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Reading {
    /// The fast average: the fill's height.
    pub(crate) level: f32,
    /// The slow average (τ 20 s, SA's ghost): the slow line.
    pub(crate) slow: f32,
    /// How far it swings, a gauge's jagged top: the median absolute deviation of the last 10
    /// samples, or for Network, which keeps none, its widest rise over a bucket's mean in the
    /// last 20 s.
    pub(crate) spread: f32,
    /// Which way the reading moves, past its gate.
    pub(crate) motion: Motion,
}

impl Reading {
    /// A trend in `capacity` units, with a motion gate in the same units.
    pub(crate) fn of(t: &TrendView, capacity: f32, gate: f32) -> Self {
        let cap = capacity.max(f32::EPSILON);
        Self {
            level: unit(t.level / cap),
            slow: unit(t.slow / cap),
            spread: unit(t.mad / cap),
            motion: t.motion(gate),
        }
    }

    /// A still reading at `level`: no slow line, no spread.
    pub(crate) fn still(level: f32) -> Self {
        let level = unit(level);
        Self {
            level,
            slow: level,
            spread: 0.0,
            motion: Motion::Steady,
        }
    }
}

/// What a gauge adds to its reading.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct GaugeLook {
    /// The fill's ink; the heat at the level when `None`.
    pub(crate) ink: Option<Rgb>,
    /// Past capacity: an orange top.
    pub(crate) over: bool,
    /// Needs the owner: a magenta ring around the gauge.
    pub(crate) needs: bool,
}

/// The merged deck's marks on `p`, in one set of merged inks.
pub(crate) struct Marks<'p, 'a> {
    pub(crate) p: &'p mut Painter<'a>,
    /// `merged`, or `merged_ic` under Increase Contrast.
    pub(crate) k: MergedInks,
}

impl<'p, 'a> Marks<'p, 'a> {
    /// The marks on `p`, in its Increase Contrast inks when `ic`.
    pub(crate) fn new(p: &'p mut Painter<'a>, ic: bool) -> Self {
        let k = if ic { p.inks.merged_ic } else { p.inks.merged };
        Self { p, k }
    }

    /// `pt` points in device px, unrounded.
    fn d(&self, pt: f32) -> f32 {
        pt * self.p.s
    }

    /// `pt` points in whole device px, never under one.
    fn whole(&self, pt: f32) -> f32 {
        self.d(pt).round().max(1.0)
    }

    fn rect(&mut self, (x, y, w, h): Rect, radius: f32, c: Rgba) {
        if w > 0.0 && h > 0.0 {
            self.p.prims.push(panel(x, y, w, h, radius, c));
        }
    }

    /// How far the gauge's needs ring reaches past the gauge on every side.
    #[cfg_attr(
        not(any(test, all(feature = "bench-support", debug_assertions))),
        expect(dead_code, reason = "Disk's needs ring (slice E9) is its first caller")
    )]
    pub(crate) fn ring_reach(&self) -> f32 {
        self.d(1.75) + self.d(1.25) / 2.0
    }

    /// A reading not yet read: its place outlined, dashed, and no fill (§11's absences).
    pub(crate) fn hollow(&mut self, r: Rect) {
        let c = a(self.k.secondary, 0.6);
        let dash = self.whole(1.0);
        self.p.dashed(r, self.p.hair(), (dash, dash), c);
    }

    /// One part of a resource (a core, 4 GiB, 100 GB): its well; its fill, as tall as its
    /// level and never under 1.25 pt from 1% up, in `ink` or the heat at its level; the slow
    /// line inside the fill while rising, the fallen band above it at 0.38 alpha while falling
    /// (its top is the slow level); and a rim from 3.5 pt and 7 px across. At most 4 prims.
    pub(crate) fn part_tile(&mut self, r: Rect, m: Reading, ink: Option<Rgb>) {
        let (x, y, w, h) = r;
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let ink = ink.unwrap_or_else(|| self.k.heat_at(m.level));
        let radius = (w.min(h) * 0.25).min(self.d(1.1));
        self.rect(r, radius, rgba(self.k.well, 0xFF));
        let mut fill = (m.level * h).round();
        if m.level >= 0.01 {
            fill = fill.max(self.d(1.25).ceil());
        }
        let fill = fill.min(h);
        let tide = y + h - fill;
        let slow = (y + h - m.slow * h).round();
        self.rect((x, tide, w, fill), radius, rgba(ink, 0xFF));
        match m.motion {
            Motion::Rising if slow - tide >= self.d(1.25) => {
                let lw = self.d(0.75).round().max(1.0);
                let alpha = if self.p.inks.dark { 0.92 } else { 0.85 };
                let ly = (slow - lw / 2.0).round().min(y + h - lw);
                self.rect((x, ly, w, lw), 0.0, a(self.p.inks.tide, alpha));
            }
            Motion::Falling if tide - slow >= self.d(0.75) => {
                self.rect((x, slow, w, tide - slow), radius, a(ink, 0.38));
            }
            _ => {}
        }
        if w.min(h) >= self.d(3.5).max(7.0) {
            self.p.prims.push(DrawPrim::Stroke {
                x: x + 0.5,
                y: y + 0.5,
                w: w - 1.0,
                h: h - 1.0,
                radius: (radius - 0.5).max(0.0),
                width: 1.0,
                color: a(self.p.inks.rim, 0.85),
            });
        }
    }

    /// The score: the part tile at the whole resource's size, rimmed, with ticks at its
    /// quarters, the slow line whichever way it moves once the slow level stands 1.25 pt from
    /// the level (the level, not the floored fill, so a still reading draws none), a jagged top
    /// of stepped panels (an odd count about 2.25 pt wide, as deep as the spread up to 3 pt) once
    /// the spread reaches 1.5 pt, an orange top when past capacity, and a magenta ring when it
    /// needs the owner.
    pub(crate) fn gauge(&mut self, r: Rect, m: Reading, look: GaugeLook) {
        let (x, y, w, h) = r;
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let (inks, k) = (*self.p.inks, self.k);
        let ink = look.ink.unwrap_or_else(|| k.heat_at(m.level));
        let radius = self.d(1.25).min(w.min(h) / 2.0);
        let tall = h >= self.d(12.0);
        self.rect(r, radius, rgba(k.well, 0xFF));
        let mut fill = (m.level * h).round();
        if m.level >= 0.01 {
            fill = fill.max(self.d(1.25).ceil());
        }
        let fill = fill.min(h);
        let tide = y + h - fill;
        let jag = (m.spread * h).min(self.d(3.0)).round();
        if tall && m.spread * h >= self.d(1.5) && fill > 0.0 {
            let (lo, hi) = (
                (tide + jag / 2.0).round().min(y + h),
                (tide - jag / 2.0).round().max(y),
            );
            self.rect((x, lo, w, y + h - lo), radius, rgba(ink, 0xFF));
            let n = 2 * ((w / self.d(4.5)).floor() as usize).max(1) + 1;
            let step = w / n as f32;
            for i in (0..n).step_by(2) {
                let (x0, x1) = (
                    (x + i as f32 * step).round(),
                    (x + (i + 1) as f32 * step).round().min(x + w),
                );
                self.rect((x0, hi, x1 - x0, lo - hi), 0.0, rgba(ink, 0xFF));
            }
        } else {
            self.rect((x, tide, w, fill), radius, rgba(ink, 0xFF));
        }
        let slow = (y + h - m.slow * h).round();
        if m.motion == Motion::Falling && tide - slow >= self.d(0.75) {
            self.rect((x, slow, w, tide - slow), radius, a(ink, 0.38));
        }
        if tall {
            let (tw, th) = (self.whole(2.0), self.p.hair());
            for q in [0.25, 0.5, 0.75] {
                let ty = (y + h - q * h - th / 2.0).round();
                self.rect((x + self.whole(0.5), ty, tw, th), 0.0, a(inks.rim, 0.9));
            }
        }
        if tall && ((m.slow - m.level) * h).abs() >= self.d(1.25) {
            let (inset, lw) = (self.whole(0.5), self.whole(1.0));
            let alpha = if inks.dark { 0.75 } else { 0.6 };
            let ly = (slow - lw / 2.0).round().clamp(y, y + h - lw);
            self.rect(
                (x + inset, ly, w - 2.0 * inset, lw),
                0.0,
                a(inks.tide, alpha),
            );
        }
        if look.over {
            let top = self.whole(2.0).min(h);
            self.p.prims.push(DrawPrim::ClipPush { x, y, w, h: top });
            self.rect(r, radius, rgba(inks.over, 0xFF));
            self.p.prims.push(DrawPrim::ClipPop);
        }
        self.p.prims.push(DrawPrim::Stroke {
            x: x + 0.5,
            y: y + 0.5,
            w: w - 1.0,
            h: h - 1.0,
            radius: (radius - 0.5).max(0.0),
            width: 1.0,
            color: a(inks.rim, 0.85),
        });
        if look.needs {
            let (out, width) = (self.d(1.75), self.d(1.25));
            self.p.prims.push(DrawPrim::Stroke {
                x: x - out,
                y: y - out,
                w: w + 2.0 * out,
                h: h + 2.0 * out,
                radius: self.d(2.5),
                width,
                color: rgba(k.needs, 0xFF),
            });
        }
    }

    /// The score through time, oldest bar at the left and now at the right, in the heat at each
    /// bar's level ([`Self::run_with`]).
    pub(crate) fn run(&mut self, r: Rect, cells: &[HeatCell]) {
        let k = self.k;
        self.run_with(r, cells, |_, v| k.heat_at(v));
    }

    /// The run with each bar in `ink(index, level)`: a scope's ink, or Memory's pressure kept
    /// for that column. A bar with no reading is hollow (dashed), never a zero; a reading too low
    /// to reach the stub's height keeps the stub, in the floor ink; a bar past capacity carries
    /// an orange cap; nothing fades with age.
    pub(crate) fn run_with(
        &mut self,
        r: Rect,
        cells: &[HeatCell],
        mut ink: impl FnMut(usize, f32) -> Rgb,
    ) {
        let (x, y, w, h) = r;
        if cells.is_empty() || w <= 0.0 || h <= 0.0 {
            return;
        }
        let n = cells.len() as f32;
        let gap = self.whole(0.6).min((w / n - 1.0).max(0.0));
        let pitch = (w + gap) / n;
        let radius = self.d(0.5);
        let stub = self.d(1.75).min(h * 0.2).round().max(1.0);
        let cap = self.whole(1.75).min(h);
        for (i, c) in cells.iter().enumerate() {
            let x0 = (x + i as f32 * pitch).round();
            let x1 = (x + (i + 1) as f32 * pitch - gap).round().max(x0 + 1.0);
            let bar = (x0, y, x1 - x0, h);
            let Some(v) = c.heat.filter(|v| !v.is_nan()) else {
                self.hollow(bar);
                continue;
            };
            let v = unit(v);
            let fill = (v * h).round().max(stub).min(h);
            if fill < h {
                self.rect(bar, radius, rgba(self.k.well, 0xFF));
            }
            let bar_ink = if v * h < stub {
                self.k.floor
            } else {
                ink(i, v)
            };
            self.rect((x0, y + h - fill, bar.2, fill), radius, rgba(bar_ink, 0xFF));
            if c.over {
                self.rect((x0, y, bar.2, cap), radius, rgba(self.p.inks.over, 0xFF));
            }
        }
    }

    /// A run's two ends named in the axis ink, `left` from `run.0 - lead` and `right` ending at
    /// `run.1`, a hairline between them when there is room, over a hairline bracket that spans
    /// the run with a tick down at each end. A run with no room to name both ends is not drawn:
    /// this draws nothing and answers `false`, and its caller draws no run.
    pub(crate) fn run_ends(
        &mut self,
        run: (f32, f32),
        lead: f32,
        baseline: f32,
        (left, right): (&str, &str),
        size: StepPx,
        m: &FaceMetrics,
    ) -> bool {
        let (x0, x1) = run;
        let start = (x0 - lead).ceil();
        let px = f64::from(size.get());
        let (lw, rw) = (
            m.width(left, px, false) as f32,
            m.width(right, px, false) as f32,
        );
        let gap = self.d(2.0);
        if x1 <= x0 || lw + gap + rw > x1 - start {
            return false;
        }
        let (inks, ink) = (*self.p.inks, self.k.axis);
        let word = |x: f32, s: &str| {
            text_prim(
                x,
                baseline.round(),
                s.to_owned(),
                size,
                TextWeight::Regular,
                TextFace::Ui,
                rgba(ink, 0xFF),
            )
        };
        self.p.prims.push(word(start, left));
        self.p.prims.push(word((x1 - rw).floor(), right));
        let (from, to) = ((start + lw + gap).round(), (x1 - rw - gap).floor());
        if to - from >= self.d(3.0) {
            let ly = (baseline - self.d(3.5)).round();
            self.rect((from, ly, to - from, self.whole(0.6)), 0.0, a(ink, 0.55));
        }
        let (t, yb) = (self.p.hair(), (baseline + self.d(1.7)).round());
        let tick = self.whole(1.1);
        let c = a(ink, if inks.dark { 0.6 } else { 0.7 });
        let (bx0, bx1) = (x0.round(), x1.round());
        self.rect((bx0, yb, bx1 - bx0, t), 0.0, c);
        self.rect((bx0, yb + t, t, tick), 0.0, c);
        self.rect((bx1 - t, yb + t, t, tick), 0.0, c);
        true
    }

    /// A scope's share of a resource under its parts: a rimmed track as wide as the parts (the
    /// whole capacity) and a bar of `share` of it from the left, in the scope's `ink`.
    #[cfg_attr(
        not(any(test, all(feature = "bench-support", debug_assertions))),
        expect(dead_code, reason = "the scope lanes (slice E4) are its first caller")
    )]
    pub(crate) fn share_bar(&mut self, r: Rect, share: f32, ink: Rgb) {
        let (x, y, w, h) = r;
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let inks = *self.p.inks;
        let radius = self.d(1.0).min(h / 2.0);
        self.rect(r, radius, rgba(self.k.well, 0xFF));
        self.p.prims.push(DrawPrim::Stroke {
            x: x + 0.5,
            y: y + 0.5,
            w: w - 1.0,
            h: h - 1.0,
            radius: (radius - 0.5).max(0.0),
            width: 1.0,
            color: a(inks.rim, if inks.dark { 0.7 } else { 0.9 }),
        });
        let share = unit(share);
        if share > 0.002 {
            let len = (w * share).round().max(self.whole(1.0)).min(w);
            self.rect((x, y, len, h), radius, rgba(ink, 0xFF));
        }
    }

    /// The capacity wall at `x` from `y0` to `y1`: everything left of it fits, past it waits.
    /// One dashed line, centred on `x`.
    pub(crate) fn wall(&mut self, x: f32, y0: f32, y1: f32) {
        let width = self.whole(1.0);
        let dash = self.whole(1.5);
        let c = a(self.k.secondary, 0.85);
        self.p.dashed(
            ((x - width / 2.0).round(), y0, 0.0, y1 - y0),
            width,
            (dash, dash),
            c,
        );
    }

    /// Work waiting past capacity in `r`: one orange slat per unit (a core's worth), stacked up
    /// from the bottom, the last at its part's width; at most [`MAX_SLATS`], which fit `r`. From
    /// 2 device px a slat the pitch is whole px, so every gap is the same (a fractional pitch
    /// rounds them to 1 and 2 px, and the slats read as pairs).
    pub(crate) fn slats(&mut self, r: Rect, units: f32) {
        let (x, y, w, h) = r;
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let units = if units.is_nan() { 0.0 } else { units };
        let pitch = h / MAX_SLATS as f32;
        let pitch = if pitch >= 2.0 { pitch.floor() } else { pitch };
        let slat = (pitch - (pitch * 0.35).round().max(1.0)).round().max(1.0);
        let c = rgba(self.p.inks.over, 0xFF);
        let radius = self.d(0.5);
        for i in 0..MAX_SLATS {
            let part = (units - i as f32).min(1.0);
            if part <= 0.05 {
                break;
            }
            let bottom = (y + h - i as f32 * pitch).floor();
            let top = (bottom - slat).max(y);
            let sw = (w * part).round().max(1.0).min(w);
            self.rect((x, top, sw, bottom - top), radius, c);
        }
    }

    /// Compressed memory over its fill in `r`: diagonal lines 2 pt apart in the ground's ink,
    /// clipped to `r`.
    #[cfg_attr(
        not(any(test, all(feature = "bench-support", debug_assertions))),
        expect(
            dead_code,
            reason = "Memory's compressed tiles (slice E5) are its first caller"
        )
    )]
    pub(crate) fn hatch(&mut self, r: Rect) {
        let (x, y, w, h) = r;
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let (step, width) = (self.d(2.0).max(2.0), self.d(0.7).max(1.0));
        let c = a(self.p.inks.ground, 0.85);
        self.p.prims.push(DrawPrim::ClipPush { x, y, w, h });
        let mut d = -h;
        while d < w {
            self.p.prims.push(DrawPrim::Line {
                x1: x + d,
                y1: y + h,
                x2: x + d + h,
                y2: y,
                width,
                color: c,
            });
            d += step;
        }
        self.p.prims.push(DrawPrim::ClipPop);
    }
}

/// The busy key's CPU and Memory blocks drawn with every mark, then a row of single marks: a
/// core tile rising, falling, steady, idle and barely active; gauges at 6, 57, 99 and 98% (a
/// spread, an over, a needs ring); an absent gauge; a hollow run and a run of zeros. From
/// `(0, top)` in `p`'s device px; answers its width and height. The marks sheet's and the
/// `deck_frame_cost` bench's specimen.
#[cfg(any(test, all(feature = "bench-support", debug_assertions)))]
pub(crate) fn specimen(p: &mut Painter<'_>, top: f32, m: &FaceMetrics) -> (f32, f32) {
    let (s, inks) = (p.s, *p.inks);
    let pt = |v: f32| (v * s).round();
    let (w, h) = (pt(360.0), pt(84.0));
    p.prims
        .push(panel(0.0, top, w, h, 0.0, rgba(inks.ground, 0xFF)));
    let mut mk = Marks::new(p, false);
    let k = mk.k;
    let lane = |a: f32, b: f32| (top + pt(a), pt(b) - pt(a));
    let (mac, all, this) = (lane(11.5, 28.5), lane(29.0, 39.0), lane(39.5, 49.5));
    for ((y, lh), tint) in [(all, k.all_tabs_tint), (this, k.this_tab_tint)] {
        mk.rect((0.0, y, w, lh), 0.0, rgba(tint, 0xFF));
    }
    let size = crate::type_scale::TypeStep::Caption.px_clamped(10.0 * s, 10.0 * s, 10.0 * s);
    let cells = |n: usize, f: &dyn Fn(usize) -> Option<f32>, over: usize| -> Vec<HeatCell> {
        (0..n)
            .map(|i| HeatCell {
                heat: f(i),
                max: f(i),
                over: i >= n - over,
            })
            .collect()
    };
    let ramp = |from: f32, to: f32| move |i: usize| Some(from + (to - from) * i as f32 / 19.0);
    // A block's score: its gauge at `gx` and the Mac's run in `ink` from `run_x`, the ends
    // over them, and each scope's run and its share under the parts (`parts`, in pt).
    let block = |mk: &mut Marks<'_, '_>,
                 (parts, gx, run_x): ((f32, f32), f32, f32),
                 (reading, over): (Reading, bool),
                 ink: &dyn Fn(usize, f32) -> Rgb,
                 shares: [f32; 2]| {
        let look = GaugeLook {
            over,
            ..GaugeLook::default()
        };
        mk.gauge((pt(gx), mac.0, pt(9.5), mac.1), reading, look);
        let run = (pt(run_x), pt(run_x + 66.0));
        mk.run_ends(run, pt(3.0), top + pt(9.0), ("10m", "now"), size, m);
        let mac_run = cells(
            RUN_BARS,
            &ramp(0.35, reading.level),
            if over { 6 } else { 0 },
        );
        mk.run_with((run.0, mac.0, run.1 - run.0, mac.1), &mac_run, ink);
        let late = |i: usize| (i >= 3).then_some(0.2 + 0.03 * i as f32);
        for ((y, lh), ink, share) in [(all, k.all_tabs, shares[0]), (this, k.this_tab, shares[1])] {
            let r = (run.0, y + pt(1.0), run.1 - run.0, lh - pt(2.0));
            mk.run_with(r, &cells(RUN_BARS, &late, 0), |_, _| ink);
            let bar = (
                pt(parts.0),
                y + ((lh - pt(4.5)) / 2.0).round(),
                pt(parts.1) - pt(parts.0),
                pt(4.5),
            );
            mk.share_bar(bar, share, ink);
        }
    };
    // CPU: 6 Super and 12 Performance cores, the wall, ~7.7 cores' worth waiting.
    for i in 0..6 {
        let level = [0.97, 0.99, 0.94, 0.99, 0.96, 0.98][i];
        let t = TrendView {
            raw: level,
            level,
            slow: level - 0.12 * (i % 2) as f32,
            mad: 0.01,
        };
        mk.part_tile(
            (pt(4.0 + 4.25 * i as f32), mac.0, pt(3.5), mac.1),
            Reading::of(&t, 1.0, 0.02),
            None,
        );
    }
    let p_top = mac.0 + mac.1 - pt(13.25);
    for i in 0..12 {
        let level = 0.9 + 0.008 * i as f32;
        let t = TrendView {
            raw: level,
            level,
            slow: level - 0.2,
            mad: 0.01,
        };
        let r = (
            pt(31.0 + 3.1 * i as f32),
            p_top,
            pt(2.3),
            mac.0 + mac.1 - p_top,
        );
        mk.part_tile(r, Reading::of(&t, 1.0, 0.02), None);
    }
    mk.wall(pt(69.0), mac.0, mac.0 + mac.1);
    mk.slats((pt(71.0), mac.0, pt(6.0), mac.1), 7.7);
    let cpu = TrendView {
        raw: 0.99,
        level: 0.99,
        slow: 0.86,
        mad: 0.02,
    };
    let heat = |_: usize, v: f32| k.heat_at(v);
    let cpu = (Reading::of(&cpu, 1.0, 0.02), true);
    block(&mut mk, ((4.0, 69.0), 79.0, 92.0), cpu, &heat, [0.57, 0.52]);
    // Memory: 32 tiles of 4 GiB, 104.4 GiB in use and 8.6 compressed, 6 GiB of swap past the
    // wall, its ink the pressure's (warn).
    let warn = k.heat_at(0.86);
    let (x0, th) = (164.0, (17.0 - 1.5) / 2.0);
    let (inuse, comp) = (104.4 / 4.0, 8.6 / 4.0);
    for i in 0..32 {
        let (col, row) = (i / 2, 1 - i % 2);
        let r = (
            pt(x0 + 5.0 * col as f32),
            mac.0 + pt((th + 1.5) * row as f32),
            pt(4.0),
            pt(th),
        );
        let (f_in, f_all) = (unit(inuse - i as f32), unit(inuse + comp - i as f32));
        mk.part_tile(r, Reading::still(f_all), Some(warn));
        if f_all > f_in {
            let (hi, lo) = (
                (r.1 + r.3 - f_all * r.3).round(),
                (r.1 + r.3 - f_in * r.3).round(),
            );
            mk.hatch((r.0, hi, r.2, lo - hi));
        }
    }
    let wall_x = x0 + 5.0 * 16.0;
    mk.wall(pt(wall_x), mac.0, mac.0 + mac.1);
    for j in 0..4 {
        let (col, row) = (j / 2, 1 - j % 2);
        let r = (
            pt(wall_x + 2.0 + 4.0 * col as f32),
            mac.0 + pt((th + 1.5) * row as f32),
            pt(3.0),
            pt(th),
        );
        mk.part_tile(r, Reading::still(unit(1.5 - j as f32)), Some(inks.over));
    }
    let mem = TrendView {
        raw: 0.88,
        level: 0.88,
        slow: 0.84,
        mad: 0.004,
    };
    let pressure = |i: usize, v: f32| if i < 12 { k.heat_at(v.min(0.45)) } else { warn };
    let mem = (Reading::of(&mem, 1.0, 0.02), false);
    block(
        &mut mk,
        ((x0, wall_x), 256.0, 268.0),
        mem,
        &pressure,
        [0.48, 0.14],
    );
    // A row of single marks.
    let row = top + pt(56.0);
    let mut x = pt(4.0);
    for (level, slow) in [
        (0.92, 0.6),
        (0.35, 0.8),
        (0.6, 0.6),
        (0.005, 0.005),
        (0.02, 0.02),
    ] {
        let t = TrendView {
            raw: level,
            level,
            slow,
            mad: 0.0,
        };
        mk.part_tile(
            (x, row, pt(3.5), pt(17.0)),
            Reading::of(&t, 1.0, 0.02),
            None,
        );
        x += pt(6.0);
    }
    x += pt(4.0);
    for (level, slow, mad, over, needs) in [
        (0.06, 0.07, 0.01, false, false),
        (0.57, 0.75, 0.15, false, false),
        (0.99, 0.86, 0.02, true, false),
        (0.98, 0.975, 0.0, false, true),
    ] {
        let t = TrendView {
            raw: level,
            level,
            slow,
            mad,
        };
        let look = GaugeLook {
            ink: None,
            over,
            needs,
        };
        mk.gauge(
            (x, row, pt(9.5), pt(17.0)),
            Reading::of(&t, 1.0, 0.02),
            look,
        );
        x += pt(9.5) + (2.0 * mk.ring_reach()).ceil() + pt(2.0);
    }
    mk.hollow((x, row, pt(9.5), pt(17.0)));
    x += pt(16.0);
    let hollow = cells(RUN_BARS, &|_| None, 0);
    mk.run((x, row, pt(66.0), pt(17.0)), &hollow);
    x += pt(72.0);
    let zeros = cells(RUN_BARS, &|i| (!(8..11).contains(&i)).then_some(0.0), 0);
    mk.run((x, row, pt(66.0), pt(17.0)), &zeros);
    (w, h)
}

#[cfg(test)]
#[path = "merged_marks_tests.rs"]
mod tests;
