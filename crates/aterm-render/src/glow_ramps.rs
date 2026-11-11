// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Laying and drawing a glow stream's ramps ([`GlowRamps`], lane B5).
//!
//! A beam lays its one-pixel rows through a [`BeamRows`] sink: the legacy
//! stream (`Vec<GlowQuad>`, a quad a row) or a [`RampSink`], which carries a
//! slab's rows within one band as one [`GlowRamp`] beside the stream's quads.
//! Both count the same lights, so every budget a beam keeps reads alike. A
//! comet slab's column of uniform pieces ([`BeamRows::rect`]) rides one ramp
//! too, its core the ramp's run (one light as tall as its rows), where a run
//! of its three lights rides one ([`BeamRows::rides`]); elsewhere it lays
//! them as the quads the sink would keep.
//!
//! A run of rows rides a ramp only where it pays ([`RAMP_COST`]): the sink
//! lays a run's rows on a ramp from its first light, and a run that ends
//! carrying fewer lights than pay unfolds into the quads the legacy stream
//! lays for it, so a short run (a comet slab's edge, core and far edge)
//! stays quads and a frame with no long run lays no ramp at all.

use crate::fx_raster;
use aterm_core::render::{FxInk, GlowEnd, GlowQuad, GlowRamp, GlowRamps, Laid, RampMark};
use aterm_core::render::{end_alpha, end_color, glow_end};

/// Where a beam lays its rows ([`self`]).
pub trait BeamRows {
    /// A place in the stream to truncate back to.
    type Mark: Copy;
    /// The lights the stream holds, every lit ramp row counted as its quad.
    fn laid(&self) -> usize;
    /// Where the stream stands now (a [`RampSink`] closes its slab).
    fn mark(&mut self) -> Self::Mark;
    /// Drop everything laid since `mark`.
    fn truncate(&mut self, mark: Self::Mark);
    /// A new slab begins: its rows never extend an earlier slab's.
    fn slab(&mut self);
    /// Lay row `y` of the slab, `(x, w)`, in band `band` (tagged `tag`), its
    /// premultiplied ends `(colour, opacity)`.
    fn row(&mut self, band: i32, tag: u16, xyw: (u16, u16, u16), a: (u32, u8), b: (u32, u8));
    /// Lay the slab's piece `(x, y, w, h)` in band `band` (tagged `tag`), of
    /// one premultiplied `(colour, opacity)` throughout: the one light its
    /// quad lays, however tall.
    fn rect(&mut self, band: i32, tag: u16, xywh: (u16, u16, u16, u16), end: (u32, u8));
    /// Lay `q` as itself, a light no ramp carries.
    fn quad(&mut self, q: GlowQuad);
    /// Whether a run of `lights` lights can ride one ramp here
    /// ([`RampCost::pays`]): a producer whose slab's runs carry no more lays
    /// its pieces as their quads ([`Self::quad`]), as the stream would keep
    /// them.
    fn rides(&self, lights: usize) -> bool;
}

impl BeamRows for Vec<GlowQuad> {
    type Mark = usize;

    #[inline]
    fn laid(&self) -> usize {
        self.len()
    }

    #[inline]
    fn mark(&mut self) -> usize {
        self.len()
    }

    #[inline]
    fn truncate(&mut self, mark: usize) {
        Vec::truncate(self, mark);
    }

    #[inline]
    fn slab(&mut self) {}

    #[inline]
    fn row(&mut self, _: i32, tag: u16, (x, y, w): (u16, u16, u16), a: (u32, u8), b: (u32, u8)) {
        self.push(GlowQuad {
            row: tag,
            x,
            y,
            w,
            h: 1,
            color: a.0,
            alpha: a.1,
            color2: b.0,
            alpha2: b.1,
        });
    }

    #[inline]
    fn rect(&mut self, _: i32, tag: u16, (x, y, w, h): (u16, u16, u16, u16), end: (u32, u8)) {
        self.push(GlowQuad {
            row: tag,
            x,
            y,
            w,
            h,
            color: end.0,
            alpha: end.1,
            color2: end.0,
            alpha2: end.1,
        });
    }

    #[inline]
    fn quad(&mut self, q: GlowQuad) {
        self.push(q);
    }

    #[inline]
    fn rides(&self, _: usize) -> bool {
        false
    }
}

/// What a run of a beam's rows costs a frame as one ramp rather than as
/// its quads: one cost model for every producer and every reader, each
/// figure the frame time it adds or spares summed over emission, the
/// readers, build + stage, encode and the GPU, in nanoseconds
/// (`docs/measured/card-engine-b5-2026-10-24.md`, round 2, has the
/// measurements it is derived from).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RampCost {
    /// What each light a ramp carries spares against its own quad.
    pub light: u32,
    /// What the ramp itself costs, beside the lights it carries.
    pub ramp: u32,
}

impl RampCost {
    /// A cost under which no run pays: every row laid as its quad, the legacy
    /// stream (a test's oracle).
    pub const NEVER: Self = Self { light: 0, ramp: 0 };

    /// A cost under which every run pays, however short: a law's fixture
    /// that must lay ramps whatever the measured costs are.
    pub const ALWAYS: Self = Self { light: 1, ramp: 0 };

    /// Whether a run of `lights` lights costs the frame less as one ramp
    /// than as their quads.
    #[must_use]
    #[inline]
    pub const fn pays(&self, lights: u32) -> bool {
        lights as u64 * self.light as u64 > self.ramp as u64
    }

    /// The fewest lights that pay as a ramp (`u32::MAX`: none).
    #[must_use]
    pub const fn floor(&self) -> u32 {
        if self.light == 0 {
            return u32::MAX;
        }
        self.ramp / self.light + 1
    }
}

/// The cost model's measured costs ([`RampCost`]): a ramp costs the frame
/// 76 ± 10 ns and each light it carries spares 23.9 ± 1.4 ns against its
/// quad, fitted within each effects-harness scene over a sweep of floors
/// (emission 38.7 and 11.8, splice 4.5 and 1.8, encode with build + stage
/// 18.9 and 3.9, the GPU 14.1 and 6.4), so a run pays from four lights
/// (break-even 3.19 ± 0.26) and a three-row wake stays quads.
pub const RAMP_COST: RampCost = RampCost {
    light: 24,
    ramp: 76,
};

/// The fewest lights a run needs to pay as a ramp ([`RampCost::floor`]).
const FLOOR: usize = RAMP_COST.floor() as usize;

/// A stream's quads and ramps, laid as ramps where they pay ([`self`]).
///
/// A run's rows (one band, one column, each light just below the last, at
/// most one taller than a row; a dark light none) are laid on one ramp from
/// the run's first light, drawn where its first quad would stand. A
/// slab ([`BeamRows::slab`]), any other light, a mark, a read of the stream
/// or the sink's drop ends the run, and a run that ends carrying fewer
/// lights than pay ([`RAMP_COST`]) unfolds into its quads, exactly as the
/// legacy stream lays them.
pub struct RampSink<'a> {
    /// The stream's quads (a ramp is drawn before the quad its `at` names).
    quads: &'a mut Vec<GlowQuad>,
    /// The stream's ramps.
    ramps: &'a mut GlowRamps,
    /// The run being laid, if any.
    run: Option<Run>,
    /// The fewest lights a run needs to pay as a ramp under its cost
    /// ([`RAMP_COST`], [`RampCost::floor`]).
    floor: usize,
}

/// The run a [`RampSink`] is laying on the stream's last ramp: the light
/// that would continue it (its band's tag, column and the pixel row just
/// below its last light, packed by [`Run::key`]), whether one of its lights
/// is taller than a row, and where its ends start (two a light). While the
/// run is laid the ramp's height lags its rows: the run's `next` row holds
/// it, and [`Run::seal`] sets it.
#[derive(Clone, Copy)]
struct Run {
    next: u64,
    tall: bool,
    ink: usize,
}

impl Run {
    /// A light's band tag, column and top as one word: a light continues a
    /// run where its word is the run's `next`.
    #[inline(always)]
    const fn key(tag: u16, (x, y, w): (u16, u16, u16)) -> u64 {
        tag as u64 | (x as u64) << 16 | (w as u64) << 32 | (y as u64) << 48
    }

    /// Whether the light `(x, y, w, h)` tagged `tag` continues the run: in
    /// its band and column, just below its last light, and not a second
    /// light taller than a row.
    #[inline(always)]
    const fn takes(&self, tag: u16, xyw: (u16, u16, u16), h: u16) -> bool {
        Self::key(tag, xyw) == self.next && (h == 1 || !self.tall)
    }

    /// Set the height of the run's ramp, the last of `ramps`, to reach the
    /// run's `next` row.
    #[inline]
    fn seal(&self, ramps: &mut GlowRamps) {
        if let Some(last) = ramps.items.last_mut() {
            last.h = ((self.next >> 48) as u16).wrapping_sub(last.y);
        }
    }
}

impl<'a> RampSink<'a> {
    /// A sink laying ramps into `ramps` beside `quads`.
    #[inline]
    pub fn new(quads: &'a mut Vec<GlowQuad>, ramps: &'a mut GlowRamps) -> Self {
        Self {
            quads,
            ramps,
            run: None,
            floor: FLOOR,
        }
    }

    /// The sink folding the runs that pay under `cost` instead of
    /// [`RAMP_COST`]: a law's fixture that must lay ramps whatever the
    /// measured costs are.
    #[inline]
    #[must_use]
    pub const fn costed(mut self, cost: RampCost) -> Self {
        let floor = cost.floor();
        self.floor = if floor == u32::MAX {
            usize::MAX
        } else {
            floor as usize
        };
        self
    }

    /// End the run being laid, unfolding it into its quads where it carries
    /// fewer lights than pay: whatever is laid next starts another.
    #[inline]
    pub fn close(&mut self) {
        if let Some(run) = self.run.take() {
            run.seal(self.ramps);
            if (self.ramps.ends.len() - run.ink) / 2 < self.floor {
                self.unfold();
            }
        }
    }

    /// The stream's quads, the run closed ([`Self::close`]): whatever is
    /// laid into them now lies after its ramps.
    #[inline]
    pub fn quads(&mut self) -> &mut Vec<GlowQuad> {
        self.close();
        self.quads
    }

    /// The stream's ramps, the run closed ([`Self::close`]).
    #[inline]
    pub fn ramps(&mut self) -> &mut GlowRamps {
        self.close();
        self.ramps
    }

    /// The stream's quads and ramps, the run closed ([`Self::close`]).
    #[inline]
    pub fn stream(&mut self) -> (&mut Vec<GlowQuad>, &mut GlowRamps) {
        self.close();
        (self.quads, self.ramps)
    }

    /// Lay the light `(x, y, w, h)` tagged `tag`, its ends `(a, b)`, and
    /// `quad` the quad it stands for: on the run's ramp where it continues
    /// the run, else on a new run's ramp where a run can pay, ending the last
    /// ([`Self::close`]). A dark light stays its quad whatever the cost and
    /// ends the run: a ramp row with both ends 0 is no light
    /// ([`GlowRamps::laid`]), so laying it on one would drop one from the
    /// stream's count.
    #[inline(always)]
    fn lay(
        &mut self,
        tag: u16,
        xyw: (u16, u16, u16),
        h: u16,
        ends: (GlowEnd, GlowEnd),
        quad: impl FnOnce() -> GlowQuad,
    ) {
        let lit = ends.0 | ends.1 != 0;
        if lit
            && let Some(run) = &mut self.run
            && run.takes(tag, xyw, h)
        {
            if h == 1 {
                // The run's common row, one pixel tall: its ends, the ramp's
                // height left to the run's `next` ([`Run::seal`]).
                run.next = run.next.wrapping_add(1 << 48);
                self.ramps.ends.extend_from_slice(&[ends.0, ends.1]);
                self.ramps.lit += 1;
                return;
            }
            run.seal(self.ramps);
            run.tall = true;
            run.next = Run::key(tag, (xyw.0, xyw.1 + h, xyw.2));
            let laid = self.ramps.extend_run(0, h, ends);
            debug_assert!(laid, "a run holds one light taller than a row");
            return;
        }
        self.close();
        if lit && self.floor != usize::MAX {
            self.run = Some(Run {
                next: Run::key(tag, (xyw.0, xyw.1 + h, xyw.2)),
                tall: h > 1,
                ink: self.ramps.ends.len(),
            });
            self.ramps.open_run(self.quads.len(), tag, xyw, h, ends);
        } else {
            self.quads.push(quad());
        }
    }

    /// Unfold the stream's last ramp, a run that does not pay, into the
    /// quads its lights stand for, where it stood.
    #[inline(never)]
    fn unfold(&mut self) {
        let Some(ramp) = self.ramps.items.pop() else {
            return;
        };
        let ink = ramp.ink as usize;
        self.quads.extend(ramp.quads(&self.ramps.ends));
        // A run's every light is lit, one pair of ends each.
        self.ramps.lit -= (self.ramps.ends.len() - ink) / 2;
        self.ramps.ends.truncate(ink);
    }

    /// Keep the stream's first `n` lights in draw order
    /// ([`GlowRamps::truncate_laid`]), as truncating its legacy quads would.
    #[inline]
    pub fn keep_laid(&mut self, n: usize) {
        self.close();
        self.ramps.truncate_laid(self.quads, n);
    }

    /// Drop the stream's quads and ramps.
    #[inline]
    pub fn clear(&mut self) {
        self.run = None;
        self.quads.clear();
        self.ramps.clear();
    }
}

impl Drop for RampSink<'_> {
    #[inline]
    fn drop(&mut self) {
        self.close();
    }
}

impl BeamRows for RampSink<'_> {
    type Mark = (usize, RampMark);

    #[inline]
    fn laid(&self) -> usize {
        self.ramps.laid(self.quads.len())
    }

    #[inline]
    fn mark(&mut self) -> (usize, RampMark) {
        self.close();
        (self.quads.len(), self.ramps.mark())
    }

    #[inline]
    fn truncate(&mut self, (quads, ramps): (usize, RampMark)) {
        // A mark closes the run, so every row laid since lies past it.
        self.close();
        self.quads.truncate(quads);
        self.ramps.truncate(ramps);
    }

    #[inline]
    fn slab(&mut self) {
        self.close();
    }

    #[inline]
    fn row(&mut self, _: i32, tag: u16, (x, y, w): (u16, u16, u16), a: (u32, u8), b: (u32, u8)) {
        let ends = (glow_end(a.0, a.1), glow_end(b.0, b.1));
        self.lay(tag, (x, y, w), 1, ends, || GlowQuad {
            row: tag,
            x,
            y,
            w,
            h: 1,
            color: a.0,
            alpha: a.1,
            color2: b.0,
            alpha2: b.1,
        });
    }

    #[inline]
    fn rect(&mut self, _: i32, tag: u16, (x, y, w, h): (u16, u16, u16, u16), end: (u32, u8)) {
        let e = glow_end(end.0, end.1);
        self.lay(tag, (x, y, w), h, (e, e), || GlowQuad {
            row: tag,
            x,
            y,
            w,
            h,
            color: end.0,
            alpha: end.1,
            color2: end.0,
            alpha2: end.1,
        });
    }

    #[inline]
    fn quad(&mut self, q: GlowQuad) {
        self.close();
        self.quads.push(q);
    }

    #[inline]
    fn rides(&self, lights: usize) -> bool {
        lights >= self.floor
    }
}

/// Draw `ramp`'s lit rows into `pixels` (`w`×`h`, the stream offset by
/// `(off_x, off_y)`), each as the quad it stands for.
#[inline]
pub(crate) fn draw_ramp(
    pixels: &mut [u32],
    (w, h): (usize, usize),
    ramp: &GlowRamp,
    ends: &[GlowEnd],
    (off_x, off_y): (usize, usize),
) {
    let x0 = off_x + usize::from(ramp.x);
    let xend = (x0 + usize::from(ramp.w)).min(w);
    if x0 >= xend {
        return;
    }
    let y0 = off_y + usize::from(ramp.y);
    let qw = u32::from(ramp.w);
    let rows = usize::from(ramp.h).min(h.saturating_sub(y0));
    for r in 0..rows {
        let (a, b) = ramp.row_ends(ends, r as u16);
        if a | b == 0 {
            continue;
        }
        let y = y0 + r;
        fx_raster::fill_glow_span(
            &mut pixels[y * w + x0..y * w + xend],
            0,
            qw,
            FxInk::over(end_color(a), end_alpha(a)),
            FxInk::over(end_color(b), end_alpha(b)),
        );
    }
}

/// One stretch of a glow stream in draw order ([`each_laid`]).
pub enum Lights<'a> {
    /// A run of the stream's quads.
    Quads(&'a [GlowQuad]),
    /// A ramp, drawn where it stands among them.
    Ramp(&'a GlowRamp),
}

/// Walk `quads` and `ramps` in draw order, quads in runs
/// ([`GlowRamps::in_order`] over the stream itself).
#[inline]
pub fn each_laid(quads: &[GlowQuad], ramps: &GlowRamps, mut each: impl FnMut(Lights<'_>)) {
    if ramps.is_empty() {
        if !quads.is_empty() {
            each(Lights::Quads(quads));
        }
        return;
    }
    ramps.in_order(quads.len(), |laid| match laid {
        Laid::Quads(r) => each(Lights::Quads(&quads[r])),
        Laid::Ramp(k) => each(Lights::Ramp(&ramps.items[k])),
    });
}

#[cfg(test)]
#[path = "glow_ramps_tests.rs"]
mod tests;
