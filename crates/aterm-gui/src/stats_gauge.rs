// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Stats Deck's strain gauge (prototype, 2026-10-24): a tachometer for the whole Mac in the
//! slot left of the row names, where the cores block was. It draws [`Strain`] and nothing else:
//!
//! - the needle and the lit arc are now: the busier engine's share of its capacity over the first
//!   210° of the sweep, in the deck's own heat ramp, and how far past capacity over the last 60°
//!   (the redline, printed as a fine over-orange line until the needle enters it);
//! - the bezel is the hour: the deck's rim grey while the soak is low, warming through bronze and
//!   copper to the over-max orange, with a soft bloom from a soak of 0.75. It draws the soak on
//!   the sketch's scale ([`drawn_soak`]: `Strain`'s soak to the power [`SOAK_CURVE`]).
//!
//! Geometry and inks are the 2026-10-24 sketch's (`deck134/r2/final/gauge/icon.py`). The deck's
//! prims have no arc that starts anywhere but 12 o'clock, no gradient and no blur, so:
//!
//! - the band's track is three [`DrawPrim::Ring`]s in two clipped halves, its round ends two dots;
//! - the lit arc is a chain of round-capped [`DrawPrim::Line`]s along the band's middle, one ink
//!   each, and a cut end (at the needle, at the redline) is squared off by short radial spokes,
//!   since a segment's round cap would reach past it;
//! - the face's gradient, the bezel's sheen and the hub's shading are horizontal bands, each a
//!   clipped dot or ring in the ink the sketch's gradient has at the band's middle;
//! - the bloom is concentric rings, each the ink a Gaussian blur of the bezel leaves at its radius
//!   laid over the deck's ground and drawn opaque, clipped to the slot as the sketch clips it;
//! - the needle is three round-capped lines: a core as wide as its point, and one each side whose
//!   outer edge runs straight from the hub's half-width to the point's; its shadow is two
//!   translucent lines that meet where the band begins (narrower over the band, so the lit arc's
//!   end keeps its ink) and a dot under the hub.
//!
//! The tray raster composites in linear light and the sketch's SVG in sRGB, so every ink the
//! sketch lays over another (the bloom, the ticks, the print, the bezel's edge) is mixed here in
//! sRGB and drawn opaque; only the needle's shadow stays translucent, at the linear opacity that
//! darkens the face as the sketch's sRGB one does.

use aterm_vitals::Strain;
use aterm_vitals::ink::{self, Lab, Rgb, VitalsInks};

use crate::widget::{DrawPrim, Rgba, rgba};

/// The face's radius, pt: the dial is 62 pt across.
pub(crate) const R_FACE: f64 = 31.0;
/// The bezel's inner edge, pt: the bezel is `R_BEZEL..R_FACE`, the hour's heat.
const R_BEZEL: f64 = 28.6;
/// The scale's band, pt.
const R_OUT: f64 = 27.0;
const R_IN: f64 = 20.6;
const R_MID: f64 = (R_OUT + R_IN) / 2.0;
const BAND_W: f64 = R_OUT - R_IN;
/// The stop, the redline and the end of the sweep, degrees clockwise from 12 o'clock.
const START: f64 = -135.0;
const RED: f64 = 75.0;
const END: f64 = 135.0;
/// The needle reaches into the band; its half-widths at the hub and at its point, pt.
const NEEDLE_TIP: f64 = 25.2;
const NEEDLE_BASE: f64 = 1.65;
const NEEDLE_POINT: f64 = 0.65;
const HUB_R: f64 = 3.4;
const HUB_PIN: f64 = 1.15;
/// The printed redline: its width along the band's outer edge, pt, and its opacity.
const PRINT_W: f64 = 1.0;
const PRINT_ALPHA: f32 = 0.7;
/// The power the bezel raises [`Strain::soak`] to ([`drawn_soak`]). `Strain`'s soak is an
/// exponential hour (τ 10 min); the sketch's was a 20-minute tally that halves every 6 calm
/// minutes, which reads a short or a fading hour lower. `soak^1.36` puts the owner's scenes where
/// the sketch drew them: spike 0.21 → 0.12 (the sketch's 0.12), recovering 0.54 → 0.43 (0.50),
/// light and idle 0 → 0 and hot-hard 1 → 1, and every soak keeps its order.
pub(crate) const SOAK_CURVE: f32 = 1.36;
/// Below this drawn soak the bezel is the rim's grey.
const SOAK_SHOWN: f32 = 0.03;
/// From this drawn soak the bezel blooms (`Strain`'s 0.81).
const BLOOM_FROM: f32 = 0.75;
/// The bloom's blur, pt (the sketch's `stdDeviation`), the stroke it blurs (the bezel and 1.2 pt
/// more), and the rings that draw it.
const BLOOM_SIGMA: f64 = 1.8;
const BLOOM_GROW: f64 = 1.2;
const BLOOM_STEP: f64 = 0.5;
const BLOOM_RINGS: u32 = 12;
/// A needle reading at or below this lights nothing (the sketch's threshold).
const LIT_FROM: f32 = 0.004;
/// The centre of each heat step, the ramp's stops: the lit arc ends in the ink the row's now %
/// is drawn in.
const STEP_AT: [f32; 7] = [0.025, 0.125, 0.30, 0.50, 0.70, 0.875, 0.975];
/// The lit chain's segment where its ink climbs the ramp, degrees (one ink per segment, fine
/// enough that no step shows), and where it holds one ink (the redline and its print): a 7.5°
/// chord leaves the band's edges 0.05 pt off the arc. Then the spokes that square off a cut end:
/// their pitch, degrees, and width, pt.
const SEG_DEG: f64 = 2.5;
const FLAT_SEG_DEG: f64 = 7.5;
const SPOKE_DEG: f64 = 1.0;
const SPOKE_W: f64 = 1.0;
/// Horizontal bands the bezel's sheen is drawn in, above and below the centre, and the hub's.
const BANDS_TOP: u32 = 6;
const BANDS_FOOT: u32 = 10;
const HUB_BANDS: u32 = 4;
/// How far the face overpaints the track's gap past the band's edges, pt, so no track is left
/// in its anti-aliased edge.
const GAP_OVER: f64 = 0.5;
/// The needle's shadow: its offset, pt, its growth for the blur, pt, and its alpha on a dark and
/// a light ground.
const SHADOW_OFF: (f64, f64) = (0.3, 0.8);
const SHADOW_W: f64 = 3.4;
/// The shadow's width over the band, where the needle narrows to its point, pt.
const SHADOW_TIP_W: f64 = 2.6;
const SHADOW_HUB_GROW: f64 = 0.5;
/// The shadow's opacity as the sketch's blur leaves it beside the needle (sRGB), on a dark and
/// a light ground.
const SHADOW_SRGB: (f32, f32) = (0.24, 0.10);
const WHITE: Rgb = [0xFF; 3];
const BLACK: Rgb = [0; 3];
const CLEAR: Rgba = [0; 4];

/// What the gauge draws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Gauge {
    /// The needle: `0..=1` the busier engine's share of its capacity, `1..=2` that far into the
    /// redline; `None` where nothing was read (no needle and no lit arc).
    pub(crate) needle: Option<f32>,
    /// The bezel's hour, `0..=1`.
    pub(crate) soak: f32,
}

impl Gauge {
    /// No reading yet: the dial alone.
    pub(crate) const UNREAD: Self = Self {
        needle: None,
        soak: 0.0,
    };

    /// The gauge of `strain`: the needle is the level within capacity, `1 + over` past it.
    pub(crate) fn of(strain: &Strain) -> Self {
        let finite = |v: f32| v.is_finite().then_some(v);
        let needle = match finite(strain.over).filter(|&o| o > 0.0) {
            Some(over) => Some(1.0 + over.min(1.0)),
            None => strain.level.and_then(finite).map(|l| l.clamp(0.0, 1.0)),
        };
        Self {
            needle,
            soak: finite(strain.soak).map_or(0.0, |s| s.clamp(0.0, 1.0)),
        }
    }

    /// Whether the needle is pinned at the end of the redline (the whole band lit).
    pub(crate) fn pinned(&self) -> bool {
        self.needle.is_some_and(|v| tip_deg(v) >= END - 0.01)
    }
}

/// The needle's angle for `v`, degrees clockwise from 12 o'clock.
fn tip_deg(v: f32) -> f64 {
    let v = f64::from(v);
    if v <= 1.0 {
        START + (RED - START) * v.max(0.0)
    } else {
        RED + (END - RED) * (v - 1.0).min(1.0)
    }
}

/// The gauge's inks, from the deck's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GaugeInks {
    dark: bool,
    /// The deck's ground, which the bloom is laid over.
    ground: Rgb,
    pub(crate) face: Rgb,
    pub(crate) bezel: Rgb,
    track: Rgb,
    needle: Rgb,
    tick: Rgb,
    over: Rgb,
    heat: [Rgb; 7],
    thermal: [Rgb; 5],
}

impl GaugeInks {
    /// The inks for `soak` on `inks`, the lit arc in `heat` (the deck's ramp, or Increase
    /// Contrast's).
    pub(crate) fn new(inks: &VitalsInks, heat: [Rgb; 7], soak: f32) -> Self {
        let dark = inks.dark;
        let thermal = if heat == inks.heat_ic {
            inks.thermal_ic
        } else {
            inks.thermal
        };
        Self {
            dark,
            ground: inks.ground,
            face: shade(inks.ground, if dark { -0.045 } else { 0.03 }, 1.0),
            bezel: soak_ink(inks.rim, inks.over, soak),
            track: if dark { inks.track } else { inks.rim },
            needle: if dark {
                ink::mix(inks.words, WHITE, 0.55)
            } else {
                ink::mix(inks.words, BLACK, 0.25)
            },
            tick: inks.words2,
            over: inks.over,
            heat,
            thermal,
        }
    }

    /// The scale's ink at `theta`: the heat ramp through each step's centre, warming into the
    /// theme-harmonized thermal orange-to-red ramp at high load and across the redline.
    fn scale(&self, theta: f64) -> Rgb {
        if theta > RED {
            let red_t = ((theta - RED) / (END - RED)).clamp(0.0, 1.0) as f32;
            let hot = ink::mix(self.thermal[3], self.thermal[4], red_t);
            return ink::mix(self.over, hot, 0.45 + 0.45 * red_t);
        }
        let v = ((theta - START) / (RED - START)).clamp(0.0, 1.0) as f32;
        let base = if v <= STEP_AT[0] {
            self.heat[0]
        } else {
            let mut c = self.heat[6];
            for i in 0..6 {
                let (a, b) = (STEP_AT[i], STEP_AT[i + 1]);
                if v <= b {
                    c = ink::mix(self.heat[i], self.heat[i + 1], (v - a) / (b - a));
                    break;
                }
            }
            c
        };
        if v <= 0.62 {
            base
        } else {
            let t = ((v - 0.62) / 0.38).clamp(0.0, 1.0);
            let warm = ink::mix(self.thermal[2], self.thermal[3], t);
            ink::mix(base, warm, 0.48 * t)
        }
    }

    /// Theme-harmonized glow color for instantaneous needle load `v` (`0..=2`).
    fn glow_ink(&self, v: f32) -> Rgb {
        if v <= 1.0 {
            let t = ((v - 0.55) / 0.45).clamp(0.0, 1.0);
            ink::mix(self.thermal[2], self.thermal[3], t)
        } else {
            let t = (v - 1.0).clamp(0.0, 1.0);
            ink::mix(self.thermal[3], self.thermal[4], t)
        }
    }
}

/// The soak the bezel and its bloom draw for `Strain`'s `soak`: `soak^`[`SOAK_CURVE`], on the
/// sketch's scale. `strain.rs` keeps the raw reading.
pub(crate) fn drawn_soak(soak: f32) -> f32 {
    if soak.is_finite() {
        soak.clamp(0.0, 1.0).powf(SOAK_CURVE)
    } else {
        0.0
    }
}

/// The bezel at `Strain`'s `soak`, drawn at [`drawn_soak`]: the rim's grey at rest, then bronze,
/// copper and, only fully soaked, the over-max orange. Lightness and chroma climb together, so a
/// cooling bezel is a dark metal, never a second alarm.
pub(crate) fn soak_ink(rim: Rgb, over: Rgb, soak: f32) -> Rgb {
    let soak = drawn_soak(soak);
    if soak <= SOAK_SHOWN {
        return rim;
    }
    let metal = shade(over, -0.40 + 0.40 * soak, 0.30 + 0.70 * soak);
    ink::mix(rim, metal, (soak / 0.25).min(1.0))
}

/// `c` with its OkLab lightness moved by `dl` and its chroma scaled by `cm`.
fn shade(c: Rgb, dl: f32, cm: f32) -> Rgb {
    let lab = ink::to_lab(c);
    ink::from_lab(Lab {
        l: (lab.l + dl).clamp(0.0, 1.0),
        a: lab.a * cm,
        b: lab.b * cm,
    })
}

/// `top` laid over `c` at `alpha`, composited in sRGB as the sketch's SVG composites it.
fn over_srgb(c: Rgb, top: Rgb, alpha: f32) -> Rgb {
    std::array::from_fn(|i| {
        let (base, add) = (f32::from(c[i]), f32::from(top[i]));
        (base + (add - base) * alpha).round().clamp(0.0, 255.0) as u8
    })
}

/// The error function (Abramowitz and Stegun 7.1.26, within 1.5e-7).
fn erf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.327_591_1 * x.abs());
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    (1.0 - poly * (-x * x).exp()).copysign(x)
}

/// Where the gauge sits: its centre (device px, whole pixels), the slot the bloom stays inside
/// (device px, `x0, y0, x1, y1`), the device px a point, and the face's radius, pt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Place {
    pub(crate) centre: (f32, f32),
    pub(crate) slot: [f32; 4],
    pub(crate) s: f64,
    /// [`R_FACE`] in Full's slot; Folded's band holds a smaller dial
    /// ([`crate::stats_plan::FOLD_GAUGE_R`]). Every part scales with it, strokes included, so a
    /// smaller dial is the same drawing.
    pub(crate) r: f64,
}

/// Draws `g` at `at` into `out`.
pub(crate) fn draw(g: Gauge, at: Place, k: &GaugeInks, out: &mut Vec<DrawPrim>) {
    let mut p = Pen {
        out,
        cx: at.centre.0,
        cy: at.centre.1,
        s: at.s * at.r / R_FACE,
    };
    p.bloom(g.soak, k, at.slot);
    p.bezel(k);
    let edge = over_srgb(k.face, BLACK, if k.dark { 0.45 } else { 0.12 });
    p.ring(R_BEZEL, 0.6, rgba(edge, 0xFF));
    p.track(k);
    let tip = g.needle.map(tip_deg);
    if let Some(v) = g.needle.filter(|&v| v > LIT_FROM) {
        p.arc_glow(v, tip_deg(v), k);
        p.lit(tip_deg(v), k);
    }
    if !g.pinned() {
        p.print(tip.map_or(RED, |t| t.max(RED)), k);
    }
    p.ticks(k);
    if let Some(tip) = tip {
        p.needle(tip, k);
    }
    p.hub(k);
}

struct Pen<'a> {
    out: &'a mut Vec<DrawPrim>,
    cx: f32,
    cy: f32,
    /// Device px per point of the sketch's 62 pt dial.
    s: f64,
}

impl Pen<'_> {
    /// The point at `theta` degrees clockwise from 12 o'clock, `r` pt from the centre, device px.
    fn at(&self, theta: f64, r: f64) -> (f32, f32) {
        let t = theta.to_radians();
        (
            self.cx + (t.sin() * r * self.s) as f32,
            self.cy - (t.cos() * r * self.s) as f32,
        )
    }

    fn len(&self, pt: f64) -> f32 {
        (pt * self.s) as f32
    }

    fn line(&mut self, (x1, y1): (f32, f32), (x2, y2): (f32, f32), width: f64, color: Rgba) {
        let width = self.len(width);
        self.out.push(DrawPrim::Line {
            x1,
            y1,
            x2,
            y2,
            width,
            color,
        });
    }

    fn dot(&mut self, (cx, cy): (f32, f32), r: f64, color: Rgba) {
        let r = self.len(r);
        self.out.push(DrawPrim::Dot { cx, cy, r, color });
    }

    /// A ring `thickness` pt thick inside `r_outer` pt, filled from 12 o'clock to `frac` of the
    /// turn, clockwise.
    fn ring_to(&mut self, r_outer: f64, thickness: f64, frac: f32, fill: Rgba) {
        let (r_outer, thickness) = (self.len(r_outer), self.len(thickness));
        self.out.push(DrawPrim::Ring {
            cx: self.cx,
            cy: self.cy,
            r_outer,
            thickness,
            track: CLEAR,
            frac,
            fill,
        });
    }

    fn ring(&mut self, r_outer: f64, thickness: f64, fill: Rgba) {
        self.ring_to(r_outer, thickness, 1.0, fill);
    }

    fn clip(&mut self, (x0, y0): (f32, f32), (x1, y1): (f32, f32)) {
        self.out.push(DrawPrim::ClipPush {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        });
    }

    fn unclip(&mut self) {
        self.out.push(DrawPrim::ClipPop);
    }

    /// Half the square a clip around the dial spans, device px.
    fn reach(&self) -> f32 {
        (self.len(R_FACE) + 2.0).ceil()
    }

    /// The bloom of a soaked bezel: what a blur of the bezel's stroke leaves outside the face,
    /// inside the `slot` (`x0, y0, x1, y1`, device px) as the sketch keeps it. The raster
    /// composites in linear light and the sketch in sRGB, so each step of the blur is laid over
    /// the deck's ground in sRGB here and drawn opaque: rings from the outermost in, each
    /// reaching under the bezel, so no ground shows between two of them.
    fn bloom(&mut self, soak: f32, k: &GaugeInks, [x0, y0, x1, y1]: [f32; 4]) {
        let soak = drawn_soak(soak);
        if soak < BLOOM_FROM {
            return;
        }
        self.clip((x0, y0), (x1, y1));
        let peak = 0.9 * (f64::from(soak) - 0.5) * 2.0;
        let mid = f64::midpoint(R_BEZEL, R_FACE);
        let half = (R_FACE - R_BEZEL + BLOOM_GROW) / 2.0;
        let z = BLOOM_SIGMA * std::f64::consts::SQRT_2;
        let under = R_FACE - 1.0;
        for i in (0..BLOOM_RINGS).rev() {
            let r = R_FACE + BLOOM_STEP * (f64::from(i) + 0.5);
            let a = peak * 0.5 * (erf((mid + half - r) / z) - erf((mid - half - r) / z));
            if a * 255.0 < 0.5 {
                continue;
            }
            let ink = over_srgb(k.ground, k.bezel, a as f32);
            let outer = R_FACE + BLOOM_STEP * f64::from(i + 1);
            self.ring(outer, outer - under, rgba(ink, 0xFF));
        }
        self.unclip();
    }

    /// The face and the bezel, in horizontal bands that carry their gradients: the face a shade
    /// darker at its top and lighter at its foot, the bezel's sheen white from its top to the
    /// middle and black from the middle to its foot, each over its circle's own box as the
    /// sketch's gradients span them.
    fn bezel(&mut self, k: &GaugeInks) {
        let (face_top, face_foot) = (shade(k.face, -0.014, 1.0), shade(k.face, 0.010, 1.0));
        let (top_a, foot_a) = if k.dark { (0.10, 0.22) } else { (0.35, 0.08) };
        let mid = f64::midpoint(R_BEZEL, R_FACE);
        let reach = self.reach();
        let mut edges = Vec::with_capacity((BANDS_TOP + BANDS_FOOT + 1) as usize);
        for i in 0..=BANDS_TOP {
            edges.push((self.cy - reach + reach * i as f32 / BANDS_TOP as f32).round());
        }
        for i in 1..=BANDS_FOOT {
            edges.push((self.cy + reach * i as f32 / BANDS_FOOT as f32).round());
        }
        let (x0, x1) = (self.cx - reach, self.cx + reach);
        for band in edges.windows(2) {
            let (y0, y1) = (band[0], band[1]);
            if y1 <= y0 {
                continue;
            }
            let y = f64::from((y0 + y1) / 2.0 - self.cy) / self.s;
            let t = ((y + mid) / (2.0 * mid)).clamp(0.0, 1.0) as f32;
            let tf = ((y + R_FACE) / (2.0 * R_FACE)).clamp(0.0, 1.0) as f32;
            let face = over_srgb(face_top, face_foot, tf);
            let ink = if t < 0.5 {
                over_srgb(k.bezel, WHITE, top_a * (1.0 - 2.0 * t))
            } else {
                over_srgb(k.bezel, BLACK, foot_a * (2.0 * t - 1.0))
            };
            self.clip((x0, y0), (x1, y1));
            self.dot((self.cx, self.cy), R_BEZEL + 0.5, rgba(face, 0xFF));
            self.ring(R_FACE, R_FACE - R_BEZEL, rgba(ink, 0xFF));
            self.unclip();
        }
    }

    /// The scale's plain track from the stop to the end, with round ends. A ring starts at 12
    /// o'clock, so the left half is the whole ring less the gap (the face, from 12 o'clock to the
    /// stop) and the right half the ring from 12 o'clock to the end, each clipped to its half.
    fn track(&mut self, k: &GaugeInks) {
        let track = rgba(k.track, 0xFF);
        let reach = self.reach();
        let (top, foot) = (self.cy - reach, self.cy + reach);
        let frac = |theta: f64| (theta.rem_euclid(360.0) / 360.0) as f32;
        self.clip((self.cx - reach, top), (self.cx, foot));
        self.ring(R_OUT, BAND_W, track);
        let face = rgba(k.face, 0xFF);
        self.ring_to(R_OUT + GAP_OVER, BAND_W + 2.0 * GAP_OVER, frac(START), face);
        self.unclip();
        self.clip((self.cx, top), (self.cx + reach, foot));
        self.ring_to(R_OUT, BAND_W, frac(END), track);
        self.unclip();
        for theta in [START, END] {
            self.dot(self.at(theta, R_MID), BAND_W / 2.0, track);
        }
    }

    /// Soft theme-harmonized halo behind the lit scale arc when instantaneous load enters warm/hot
    /// territory (`v >= 0.55`), illuminating the dial face inward without spilling past the bezel.
    fn arc_glow(&mut self, v: f32, tip: f64, k: &GaugeInks) {
        if v < 0.55 {
            return;
        }
        let strength = ((v - 0.55) / 0.65).clamp(0.0, 1.0);
        let glow = k.glow_ink(v);
        let end = tip.min(END);
        let span = (end - START).max(0.0);
        if span <= 1.0 {
            return;
        }
        let n = (span / 12.0).ceil().max(1.0);
        let step = span / n;
        for (r_shift, w_add, base_a) in [
            (-0.9, 4.2, if k.dark { 0.20 } else { 0.14 }),
            (-0.4, 2.0, if k.dark { 0.34 } else { 0.24 }),
        ] {
            let alpha = ((base_a * strength * 255.0).round() as u8).max(1);
            let color = rgba(glow, alpha);
            let r = R_MID + r_shift;
            let w = BAND_W + w_add;
            for i in 0..n as u32 {
                let t0 = START + step * f64::from(i);
                let t1 = t0 + step;
                self.line(self.at(t0, r), self.at(t1, r), w, color);
            }
        }
    }

    /// The lit arc from the stop to `tip`: the ramp to the redline, the over-max orange past it.
    fn lit(&mut self, tip: f64, k: &GaugeInks) {
        let pinned = tip >= END - 0.01;
        let ramp_end = tip.min(RED);
        self.run(START, ramp_end, (true, false), k);
        if ramp_end - START < cap_deg() {
            // The stop's round end reaches past so short a run: the track takes the rest back,
            // and the run's spokes then cover the overlap.
            let track = k.track;
            self.spokes(ramp_end + SPOKE_DEG, START + cap_deg(), &|_| track);
            self.spokes(START, ramp_end, &|t| k.scale(t));
        }
        if tip > RED {
            self.run(RED, if pinned { END } else { tip }, (false, pinned), k);
        }
    }

    /// The band from `a` to `b` degrees inked along the scale: a chain of round-capped segments
    /// along the band's middle, so a round end is the band's own; a cut end is squared off by
    /// spokes, since a segment's cap would reach past it.
    fn run(&mut self, a: f64, b: f64, (round_a, round_b): (bool, bool), k: &GaugeInks) {
        let cap = cap_deg();
        let a1 = if round_a { a } else { a + cap };
        let b1 = if round_b { b } else { b - cap };
        if b1 > a1 {
            let seg = if a >= RED { FLAT_SEG_DEG } else { SEG_DEG };
            let n = ((b1 - a1) / seg).ceil().max(1.0);
            let step = (b1 - a1) / n;
            for i in 0..n as u32 {
                let t0 = a1 + step * f64::from(i);
                let t1 = t0 + step;
                let ink = rgba(k.scale(f64::midpoint(t0, t1)), 0xFF);
                self.line(self.at(t0, R_MID), self.at(t1, R_MID), BAND_W, ink);
            }
            if !round_a {
                self.spokes(a, a1, &|t| k.scale(t));
            }
            if !round_b {
                self.spokes(b1, b, &|t| k.scale(t));
            }
            return;
        }
        if round_a {
            self.dot(self.at(a, R_MID), BAND_W / 2.0, rgba(k.scale(a), 0xFF));
        } else {
            self.spokes(a, b, &|t| k.scale(t));
        }
    }

    /// Radial strokes across the band from `a` to `b` degrees, at most [`SPOKE_DEG`] apart.
    fn spokes(&mut self, a: f64, b: f64, ink: &dyn Fn(f64) -> Rgb) {
        if b < a {
            return;
        }
        let n = ((b - a) / SPOKE_DEG).ceil() as u32;
        let (r0, r1) = (R_IN + SPOKE_W / 2.0, R_OUT - SPOKE_W / 2.0);
        for i in 0..=n {
            let t = if n == 0 {
                a
            } else {
                a + (b - a) * f64::from(i) / f64::from(n)
            };
            self.line(self.at(t, r0), self.at(t, r1), SPOKE_W, rgba(ink(t), 0xFF));
        }
    }

    /// The redline printed along the band's outer edge from `from` to the end.
    fn print(&mut self, from: f64, k: &GaugeInks) {
        let ink = rgba(over_srgb(k.track, k.over, PRINT_ALPHA), 0xFF);
        let r = R_OUT - PRINT_W / 2.0;
        let n = ((END - from) / FLAT_SEG_DEG).ceil().max(1.0);
        let step = (END - from) / n;
        for i in 0..n as u32 {
            let t0 = from + step * f64::from(i);
            self.line(self.at(t0, r), self.at(t0 + step, r), PRINT_W, ink);
        }
    }

    /// The scale's tenths just inside the band, the stop, the middle and the redline larger,
    /// laid over the face in sRGB as the sketch lays them.
    fn ticks(&mut self, k: &GaugeInks) {
        for i in 0..=10_u32 {
            let theta = START + (RED - START) * f64::from(i) / 10.0;
            let (r, alpha) = if i % 5 == 0 {
                (0.8, 0.75)
            } else {
                (0.55, 0.45)
            };
            let ink = over_srgb(k.face, k.tick, alpha);
            self.dot(self.at(theta, R_IN - 3.0), r, rgba(ink, 0xFF));
        }
    }

    /// The needle at `tip` over its shadow. The needle is the sketch's tapered path with its round
    /// point: a core line as wide as the point, from the hub to the point, and on each side a line
    /// as wide from the hub (offset by the taper) to the point, whose outer edge is the path's
    /// straight edge from the hub's half-width to the point's. Each side line's inner edge stays
    /// within the core and the other side, and all three end in the point's own cap, so the
    /// edges have no step (lines stacked narrower in steps notched a near-level needle). The
    /// shadow is two lines that meet at the band's inner edge without overlapping (a narrower one
    /// over the band, where the needle narrows), at the opacity that darkens in linear light as
    /// the sketch's darkens in sRGB, and its hub a dot laid over the face.
    fn needle(&mut self, tip: f64, k: &GaugeInks) {
        let a = if k.dark { SHADOW_SRGB.0 } else { SHADOW_SRGB.1 };
        let linear = 1.0 - (1.0 - a).powf(2.2);
        let shadow = rgba(BLACK, (linear * 255.0).round() as u8);
        let (dx, dy) = (self.len(SHADOW_OFF.0), self.len(SHADOW_OFF.1));
        let off = |(x, y): (f32, f32)| (x + dx, y + dy);
        let hub = off((self.cx, self.cy));
        let inner = off(self.at(tip, R_IN - SHADOW_W / 2.0));
        self.line(hub, inner, SHADOW_W, shadow);
        let band = off(self.at(tip, R_IN + SHADOW_TIP_W / 2.0));
        self.line(band, off(self.at(tip, NEEDLE_TIP)), SHADOW_TIP_W, shadow);
        let hub_ink = over_srgb(k.face, BLACK, 2.0 * a);
        self.dot(hub, HUB_R + SHADOW_HUB_GROW, rgba(hub_ink, 0xFF));
        let ink = rgba(k.needle, 0xFF);
        let point = self.at(tip, NEEDLE_TIP);
        let width = 2.0 * NEEDLE_POINT;
        self.line((self.cx, self.cy), point, width, ink);
        // Across the needle: (cos, sin) of its angle is perpendicular to (sin, -cos) along it.
        let t = tip.to_radians();
        let taper = NEEDLE_BASE - NEEDLE_POINT;
        let (nx, ny) = (self.len(t.cos() * taper), self.len(t.sin() * taper));
        for side in [1.0, -1.0] {
            let from = (self.cx + side * nx, self.cy + side * ny);
            self.line(from, point, width, ink);
        }
    }

    /// The hub, lit from above (bands from the needle's ink lightened to it darkened), and its
    /// pin in the face's ink.
    fn hub(&mut self, k: &GaugeInks) {
        let (top, foot) = (shade(k.needle, 0.03, 1.0), shade(k.needle, -0.15, 1.0));
        let (r, cy) = (self.len(HUB_R), self.cy);
        let reach = (r + 1.0).ceil();
        let (x0, x1) = (self.cx - reach, self.cx + reach);
        let edge = |i: u32| (cy - reach + 2.0 * reach * i as f32 / HUB_BANDS as f32).round();
        for i in 0..HUB_BANDS {
            let (y0, y1) = (edge(i), edge(i + 1));
            let t = (((y0 + y1) / 2.0 - (cy - r)) / (2.0 * r)).clamp(0.0, 1.0);
            self.clip((x0, y0), (x1, y1));
            self.dot(
                (self.cx, self.cy),
                HUB_R,
                rgba(over_srgb(top, foot, t), 0xFF),
            );
            self.unclip();
        }
        self.dot((self.cx, self.cy), HUB_PIN, rgba(k.face, 0xFF));
    }
}

/// The angle a round cap of the band reaches past its segment's end, degrees.
fn cap_deg() -> f64 {
    (BAND_W / 2.0 / R_MID).to_degrees()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strain(level: Option<f32>, over: f32, soak: f32) -> Strain {
        Strain { level, over, soak }
    }

    #[test]
    fn the_needle_reads_the_level_then_the_redline() {
        let g = Gauge::of(&strain(Some(0.7), 0.0, 0.0));
        assert_eq!(g.needle, Some(0.7));
        assert!(
            (tip_deg(0.7) - 12.0).abs() < 1e-4,
            "busy 70% points past 12 o'clock"
        );
        let hot = Gauge::of(&strain(Some(1.0), 1.0, 1.0));
        assert_eq!((hot.needle, hot.soak), (Some(2.0), 1.0));
        assert!(hot.pinned());
        let red = Gauge::of(&strain(Some(1.0), 0.12, 0.2));
        assert!(red.needle.is_some_and(|v| v > 1.0) && !red.pinned());
        assert!(tip_deg(1.12) > RED);
        assert_eq!(Gauge::of(&strain(None, 0.0, 0.0)), Gauge::UNREAD);
        let odd = Gauge::of(&strain(Some(f32::NAN), f32::NAN, f32::NAN));
        assert_eq!(odd, Gauge::UNREAD, "a non-finite reading draws as unread");
    }

    #[test]
    fn the_bezel_warms_with_the_soak_and_never_shouts_until_soaked() {
        let rim = [0x4A, 0x4C, 0x52];
        let over = [0xFF, 0x9A, 0x2E];
        assert_eq!(soak_ink(rim, over, 0.0), rim);
        assert_eq!(soak_ink(rim, over, SOAK_SHOWN), rim);
        assert_eq!(soak_ink(rim, over, 0.07), rim, "drawn 0.027");
        let chroma = |c: Rgb| {
            let l = ink::to_lab(c);
            l.a.hypot(l.b)
        };
        let ladder = [0.1, 0.25, 0.54, 0.8, 1.0].map(|s| soak_ink(rim, over, s));
        for w in ladder.windows(2) {
            assert!(chroma(w[1]) > chroma(w[0]), "warmer in turn: {ladder:?}");
            assert!(
                ink::to_lab(w[1]).l > ink::to_lab(w[0]).l,
                "lighter in turn: {ladder:?}"
            );
        }
        assert!(ink::distance(soak_ink(rim, over, 1.0), over) < 0.01);
        assert!(
            ink::distance(soak_ink(rim, over, 0.54), over) > 0.1,
            "a recovering bezel is copper, not the alarm"
        );
    }

    /// The bezel draws the soak on the sketch's scale: the owner's spike (`Strain` 0.21) about
    /// 0.12 and recovering (0.54) 0.43..0.50, as the sketch drew them; rest and full stay put.
    #[test]
    fn the_bezel_draws_the_soak_on_the_sketchs_scale() {
        let near = |raw: f32, lo: f32, hi: f32| {
            let d = drawn_soak(raw);
            assert!((lo..=hi).contains(&d), "{raw} draws {d}, not {lo}..{hi}");
        };
        near(0.21, 0.115, 0.125);
        near(0.54, 0.43, 0.50);
        assert_eq!((drawn_soak(0.0), drawn_soak(1.0)), (0.0, 1.0));
        assert_eq!(drawn_soak(f32::NAN), 0.0);
        let ladder = [0.05, 0.2, 0.4, 0.6, 0.8, 0.95].map(drawn_soak);
        assert!(ladder.windows(2).all(|w| w[0] < w[1]), "{ladder:?}");
        let rim = [0x4A, 0x4C, 0x52];
        let over = [0xFF, 0x9A, 0x2E];
        assert!(
            ink::distance(soak_ink(rim, over, 0.21), rim)
                < ink::distance(soak_ink(rim, over, 0.54), rim),
            "spike's trace is fainter than recovering's copper"
        );
    }

    #[test]
    fn erf_matches_its_table() {
        for (x, want) in [
            (0.0, 0.0),
            (0.5, 0.520_499_9),
            (1.0, 0.842_700_8),
            (2.0, 0.995_322_3),
        ] {
            assert!((erf(x) - want).abs() < 1e-6, "erf({x})");
            assert!((erf(-x) + want).abs() < 1e-6, "erf(-{x})");
        }
    }

    #[test]
    fn warm_and_hot_load_emit_themed_arc_glow_while_calm_does_not() {
        let inks = aterm_vitals::ink::vitals_inks(
            [0x16, 0x18, 0x1D],
            [0xE6, 0xE8, 0xEE],
            [0x58, 0xA6, 0xFF],
        );
        let k = GaugeInks::new(&inks, inks.heat, 0.0);
        let place = Place {
            centre: (80.0, 80.0),
            slot: [10.0, 10.0, 150.0, 150.0],
            s: 2.0,
            r: R_FACE,
        };
        let translucent_lines = |needle: f32| {
            let mut out = Vec::new();
            draw(
                Gauge {
                    needle: Some(needle),
                    soak: 0.0,
                },
                place,
                &k,
                &mut out,
            );
            out.into_iter()
                .filter(|p| matches!(p, DrawPrim::Line { color, .. } if color[3] < 0xFF))
                .count()
        };
        // Needle shadow uses 2 translucent lines; calm load adds zero glow lines.
        assert_eq!(translucent_lines(0.25), 2);
        // Warm (70%) and redline (140%) loads add the two-pass themed arc glow halo.
        let warm = translucent_lines(0.70);
        let redline = translucent_lines(1.40);
        assert!(warm > 2, "70% load should emit dial arc glow: {warm}");
        assert!(
            redline > warm,
            "redline sweep spans more glow segments than 70%: {redline} vs {warm}"
        );
        // Redline scale warms past base `over` toward `thermal[4]` crimson-red.
        assert_ne!(k.scale(RED + 5.0), k.scale(END));
    }
}
