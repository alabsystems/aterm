// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The LIQUID DROPLET cursor — while the `water` trail style is active the block
//! cursor IS water: a cool aqua block fill under an additive bead of water that
//! swallows the cell, lit by a glassy specular glint, beading DRIPS off its
//! belly and spreading flattened RIPPLE rings across the waterline under it.
//! The whole thing breathes off the water's single SURGE number (typing heat /
//! jump flare, injected by the host from
//! [`crate::cursor_glow::CursorGlow::blaze`]), so the droplet, the wave wake,
//! and the splash all belong to one body of water:
//!
//! * **at rest** — still water: a calm deep-aqua bead with a lazy caustic
//!   shimmer, one slow drip forming on its underside, the waterline glassy;
//! * **under the keys** — the water SURGES: the bead swells and sloshes, the
//!   fill brightens toward foam, drips bead off faster, and ripple rings chase
//!   each other outward;
//! * **on a jump** — the flare slams the surge to full, so the landing cursor
//!   SPLASHES — the bead erupts to foam-white and the widest rings roll out
//!   while it calms back down to still water.
//!
//! Text-safe by construction, mirroring [`crate::cursor_fireball`]: the block
//! FILL is returned for the renderer's `floor_cursor_fill` contrast floor (the
//! cut-out glyph stays razor-sharp), and the bead/drips/rings are purely
//! additive [`GlowQuad`] light with capped coverage. Like its siblings it is a
//! CLOCKLESS pure function of an injected `now`, settles to inactive when the
//! surge dies (the still bead then rides the blink cadence — no perpetual
//! wakeups), and emits identical premultiplied quads on the CPU and Metal
//! backends. Honours WATER-1: nothing here is a beam — only water.

use web_time::Instant;

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::Geom;
use crate::effect_util::{push_grid_rect as push_rect, water_ramp};

/// Slosh rate in turns/second: a glassy sway at rest, a churning slosh at full
/// surge. The phase only advances when frames render, so a settled bead sways
/// at the blink cadence for free.
const SLOSH_IDLE: f32 = 0.9;
const SLOSH_ACTIVE: f32 = 4.2;

/// Bead radius in cell-heights: the bead must SWALLOW the block (its diameter
/// at least the cell height), or the block's square corners poke out and the
/// whole thing reads as a coloured rectangle, not a droplet. Still under a cell
/// of reach past the cell edge so the water hugs the cursor.
const RADIUS_IDLE: f32 = 0.52;
const RADIUS_MAX: f32 = 0.78;

/// Innermost-core coverage (pre-intensity): still water → foaming heart. The
/// concentric discs below scale down from this, and every push is additionally
/// clamped by [`COV_CAP`] so the stacked additive light can never bury a
/// neighbouring glyph.
const COV_IDLE: f32 = 60.0;
const COV_MAX: f32 = 148.0;
/// Per-quad additive coverage ceiling — the same text-safety band as the
/// fireball's (the readable-at-full-blaze live-review tuning). Inner discs
/// saturate here (they sit over the cursor cell itself); the wide rim quads and
/// ripple rings that actually overlap neighbouring glyphs run at a fraction of
/// the core and stay a tint.
const COV_CAP: f32 = 92.0;

/// Ripple cadence in rings/second: a barely-moving glassy ring at rest (it is
/// also faded to nothing by the surge-scaled envelope), a lively chase at full
/// surge. Two rings run half a cycle apart so the water never stops rolling.
const RIPPLE_IDLE: f32 = 0.35;
const RIPPLE_ACTIVE: f32 = 1.15;
/// How far a ring rolls out, in bead-radii, over its life (`u` 0→1).
const RIPPLE_REACH: f32 = 2.2;

/// Drip cadence in drops/second/lane: a slow faucet-bead at rest, rain off the
/// belly at full surge. Lanes unlock with surge (see [`DRIP_LANES`]).
const DRIP_IDLE: f32 = 0.45;
const DRIP_ACTIVE: f32 = 1.8;
/// The drip lanes: horizontal offset from the bead's centre (× cell width),
/// the lane's phase offset (so drips never fall in lock-step), and the surge
/// at which the lane wakes. One lazy drip at rest; rain under the keys.
const DRIP_LANES: [(f32, f32, f32); 3] =
    [(-0.26, 0.00, 0.0), (0.06, 0.37, 0.30), (0.30, 0.71, 0.62)];
/// How far a drop falls, in cell-heights, before it has fully faded.
const DRIP_FALL: f32 = 1.05;

/// The surge below which the droplet is SETTLED — the animator reports itself
/// inactive so the host stops arming the 60 fps tick and the still bead rides
/// the blink cadence.
const SETTLED_SURGE: f32 = 0.02;

/// Resolved per-frame inputs (Copy so the host reads it out before borrowing state).
#[derive(Clone, Copy, Debug)]
pub struct DropletConfig {
    /// Master on/off (the `water` style opted in AND the cursor is a focused,
    /// visible block).
    pub enabled: bool,
    /// Overall scale `0..1` — the reduced-motion / load-shed amplitude, folded in
    /// by the host exactly like the aurora. 0 ⇒ fully inert (plain themed cursor).
    pub intensity: f32,
}

/// What a tick produced: the liquid block FILL to hand the renderer (it floors it
/// for contrast) and a fingerprint that changes on every visible step (0 when off).
#[derive(Clone, Copy, Debug)]
pub struct DropletFrame {
    /// The liquid block-fill colour `0x00RRGGBB`, or `None` when the droplet is
    /// off (the renderer then keeps the ordinary themed cursor fill).
    pub fill: Option<u32>,
    /// Fingerprint of the emitted fill + bead + rings + drips (0 ⇒ nothing this frame).
    pub fp: u64,
}

/// Per-window droplet animation state — a slosh phase, a ripple phase, a drip
/// phase, the last clock reading, and a latched surge. Tiny + Copy-cheap.
#[derive(Default)]
pub struct CursorDroplet {
    /// Rolling slosh (caustic shimmer) phase in turns `0..1`.
    slosh: f32,
    /// Rolling ripple-ring phase in ring-lives `0..1` (two rings ride it, half a
    /// cycle apart).
    ripple: f32,
    /// Rolling drip phase in drop-lives `0..1` (each lane adds its own offset).
    drip: f32,
    last: Option<Instant>,
    /// Latched surge at the last tick (so [`Self::is_active`] answers without a clock).
    surge: f32,
}

impl CursorDroplet {
    /// Whether the host must keep arming the animation tick: only while the water
    /// is still SURGING (typing or a jump splash calming). Once the surge settles
    /// the still bead rides the blink cadence at no extra idle cost.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.surge > SETTLED_SURGE
    }

    /// Advance one frame at `now` with the water's current `surge` (`0..1`, the
    /// host reads [`crate::cursor_glow::CursorGlow::blaze`] after the aurora
    /// tick), the block cursor cell `cur` (`None` ⇒ hidden), grid `geom`, and the
    /// resolved `cfg`. Appends the additive droplet to `out` and returns the
    /// liquid block FILL + a fingerprint. Pure: no wall-clock, unit-testable by
    /// injecting `now`/`surge`.
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        surge: f32,
        geom: Geom,
        cfg: &DropletConfig,
        out: &mut Vec<GlowQuad>,
    ) -> DropletFrame {
        let e = (surge.clamp(0.0, 1.0) * cfg.intensity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        // Fully inert — byte-identical to the plain themed cursor — when off, when
        // the geometry is degenerate, or when the amplitude is zero (reduced
        // motion / load-shed), mirroring the fireball's "0 ⇒ off" contract.
        if !cfg.enabled || geom.cw == 0 || geom.ch == 0 || cfg.intensity <= 0.0 {
            self.surge = 0.0; // inert: report settled so the host disarms the tick
            self.last = Some(now);
            return DropletFrame { fill: None, fp: 0 };
        }
        self.surge = e;

        // Advance the phases by real dt (clamped so a long stall — e.g. the
        // window slept — doesn't fling them). Livelier the harder it surges.
        let dt = self
            .last
            .map(|t| now.saturating_duration_since(t).as_secs_f32())
            .unwrap_or(0.0)
            .min(0.25);
        self.last = Some(now);
        self.slosh = (self.slosh + dt * (SLOSH_IDLE + SLOSH_ACTIVE * e)).fract();
        self.ripple = (self.ripple + dt * (RIPPLE_IDLE + RIPPLE_ACTIVE * e)).fract();
        self.drip = (self.drip + dt * (DRIP_IDLE + DRIP_ACTIVE * e)).fract();
        let phase = self.slosh * std::f32::consts::TAU;
        // The shared caustic envelope 0..1 — two incommensurate sines so the
        // shimmer never reads as a metronome pulse.
        let shimmer = 0.5 + 0.35 * phase.sin() + 0.15 * (phase * 2.7 + 1.3).sin();

        // The liquid BLOCK FILL: deep still water at rest, bright azure at full
        // surge, shimmering slightly with the caustics. Deliberately capped BELOW
        // the additive heart's foam — if the rectangular fill outshines the round
        // bead around it, the eye reads a bright BLOCK with a halo instead of a
        // droplet; keeping the fill in the bead body's hue band lets the round
        // foam-white heart (additive, below) be the brightest thing on screen.
        // The renderer floors this against the cell bg, so the glyph stays sharp.
        let fill = water_ramp(0.36 + 0.42 * e + 0.05 * (shimmer - 0.5));

        // The additive droplet, drawn only over a visible in-grid cursor cell.
        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
        {
            self.emit_bead(cr, cc, e, shimmer, phase, geom, cfg, out);
        }

        // Fingerprint: quantized phases + surge + fill, so a settled bead
        // early-outs the present but any visible step forces a repaint.
        let fp = ((self.slosh * 512.0) as u64)
            .wrapping_mul(1_000_003)
            .wrapping_add(((self.ripple * 512.0) as u64) << 20)
            .wrapping_add(((self.drip * 512.0) as u64) << 40)
            .wrapping_add((e * 255.0) as u64)
            .wrapping_add((fill as u64) << 12)
            | 1; // never 0 while enabled — still water is always drawable

        DropletFrame {
            fill: Some(fill),
            fp,
        }
    }

    /// Emit the bead + glint + ripple rings + drips as premultiplied additive
    /// quads. Bounded by construction: ≤ ~4 discs × (2·radius / slab) slabs, two
    /// rings × a handful of slab rows, three drips, two glints.
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor cell + surge/shimmer/phase + geometry/config; one internal call site"
    )]
    fn emit_bead(
        &self,
        cr: u16,
        cc: u16,
        e: f32,
        shimmer: f32,
        phase: f32,
        geom: Geom,
        cfg: &DropletConfig,
        out: &mut Vec<GlowQuad>,
    ) {
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        // Bead centre: mid-cell, floated slightly high so the drips and the
        // waterline have headroom below the glyph row.
        let cx = (cc as f32 + 0.5) * cw;
        let cy = (cr as f32 + 0.46) * ch;
        let radius = (RADIUS_IDLE + (RADIUS_MAX - RADIUS_IDLE) * e) * ch;
        let cov_core =
            (COV_IDLE + (COV_MAX - COV_IDLE) * e) * cfg.intensity * (0.84 + 0.16 * shimmer);

        // Concentric discs, outermost first: misty rim → deep body → bright
        // azure → the foam-white heart (present only as the surge climbs).
        // Additive overlap builds the radial gradient without per-pixel math.
        let discs: [(f32, f32, f32); 4] = [
            // (radius ×, coverage ×, water_ramp t)
            (1.00, 0.30, 0.24 + 0.06 * e),
            (0.82, 0.58, 0.46 + 0.12 * e),
            (0.62, 0.85, 0.66 + 0.16 * e),
            (0.40, 1.00, 0.86 + 0.14 * e),
        ];
        // Slab height ~1/8 cell (min 2px): fine enough to read as round.
        let slab = ((ch * 0.125) as i32).max(2);
        for &(rx, cov_x, ramp_t) in &discs {
            let r = radius * rx;
            if r < 1.0 {
                continue;
            }
            let cov = (cov_core * cov_x).min(COV_CAP) as u8;
            if cov == 0 {
                continue;
            }
            let premul = premul_rgb(water_ramp(ramp_t), cov);
            let r_i = r as i32;
            let mut dy = -r_i;
            while dy < r_i {
                let h = slab.min(r_i - dy);
                // Sample the bead's half-width at the slab's vertical centre.
                let ym = dy as f32 + h as f32 * 0.5;
                let base = (r * r - ym * ym).max(0.0).sqrt();
                // DROPLET taper: the upper half pinches toward the point a drop
                // hangs from, the lower half bulges with the water's weight — a
                // hanging drop's silhouette, not a billiard ball.
                let taper = if ym < 0.0 {
                    1.0 + 0.34 * ym / r
                } else {
                    1.0 + 0.08 * ym / r
                };
                // SLOSH: shear each slab sideways with a travelling wave down the
                // bead, so the liquid visibly rocks in place instead of standing
                // as a rigid sticker. Gentle at rest, churning at full surge.
                let shear = (phase + ym / r * 1.8).sin() * r * 0.10 * (0.30 + 0.70 * e);
                let hw = (base * taper) as i32;
                if hw >= 1 {
                    push_rect(
                        out,
                        geom,
                        (cx + shear) as i32 - hw,
                        cy as i32 + dy,
                        2 * hw,
                        h,
                        premul,
                    );
                }
                dy += h;
            }
        }

        // The specular GLINT: a glassy off-centre highlight riding high on the
        // bead's shoulder (plus a tiny echo below it), flickering softly with the
        // caustics — the one cue that makes the bead read as LIQUID instead of a
        // coloured glow. Near-foam hue, saturating the text-safety cap.
        let glint_cov = (cov_core * (0.80 + 0.20 * shimmer)).min(COV_CAP) as u8;
        if glint_cov > 0 {
            let gx = cx - radius * 0.30 + (phase.sin() * radius * 0.05);
            let gy = cy - radius * 0.34;
            let gw = ((radius * 0.24) as i32).max(2);
            let gh = (slab / 2).max(2);
            // Aqua-tinged, never glacial white — a wet gleam, not a glare of ice.
            let glint = premul_rgb(0x00B4_EAF0, glint_cov);
            push_rect(out, geom, gx as i32 - gw / 2, gy as i32, gw, gh, glint);
            // The echo: a pinprick lower-right, half the size, half the light.
            let echo = premul_rgb(0x00B4_EAF0, glint_cov / 2);
            push_rect(
                out,
                geom,
                (cx + radius * 0.18) as i32,
                (cy + radius * 0.10) as i32,
                (gw / 2).max(1),
                (gh / 2).max(1),
                echo,
            );
        }

        // RIPPLE RINGS: two flattened ellipse rings rolling out across the
        // waterline under the bead, half a life apart so the water never stops.
        // Surge-scaled: invisible on still water (so a settled frame is calm),
        // chasing each other wide on a splash.
        for half in [0.0f32, 0.5] {
            let u = (self.ripple + half).fract();
            let env = (1.0 - u) * (1.0 - u).sqrt() * e;
            let cov = (cov_core * 0.55 * env).min(COV_CAP) as u8;
            if cov == 0 {
                continue;
            }
            let rx = radius * (0.9 + RIPPLE_REACH * u);
            let ry = (rx * 0.22).max(2.0);
            let thick = (rx * 0.12).max(1.5);
            let ring = premul_rgb(water_ramp(0.74 + 0.10 * e), cov);
            let y0 = cy + radius * 0.88;
            let rslab = ((ry * 0.5) as i32).max(2);
            let mut dy = -(ry as i32);
            while dy < ry as i32 {
                let h = rslab.min(ry as i32 - dy);
                let ym = (dy as f32 + h as f32 * 0.5) / ry;
                let outer = rx * (1.0 - ym * ym).max(0.0).sqrt();
                let inner_rx = (rx - thick).max(0.0);
                let inner_ry = ry * (inner_rx / rx);
                let inner = if inner_ry > 0.5 {
                    let yin = (dy as f32 + h as f32 * 0.5) / inner_ry;
                    inner_rx * (1.0 - yin * yin).max(0.0).sqrt()
                } else {
                    0.0
                };
                let (o, i) = (outer as i32, inner as i32);
                if o > i {
                    push_rect(out, geom, cx as i32 - o, y0 as i32 + dy, o - i, h, ring);
                    push_rect(out, geom, cx as i32 + i, y0 as i32 + dy, o - i, h, ring);
                }
                dy += h;
            }
        }

        // DRIPS: drops beading off the bead's belly and falling away, each lane
        // on its own offset so they never fall in lock-step. One lazy drip at
        // rest (the faucet bead), the full rain only as the surge wakes the
        // outer lanes. A drop spends the first third of its life growing on the
        // belly, then detaches and accelerates down, fading as it goes.
        let belly = cy + radius * 0.80;
        for &(dx, off, wake) in &DRIP_LANES {
            if e < wake && wake > 0.0 {
                continue;
            }
            let u = (self.drip + off).fract();
            let x = cx + dx * cw + (phase + off * 7.0).sin() * 1.5;
            let body = water_ramp(0.68);
            if u < 0.30 {
                // Beading: the drop grows on the belly.
                let s = u / 0.30;
                let w = (2.0 + radius * 0.16 * s) as i32;
                let cov = (cov_core * 0.70 * s).min(COV_CAP) as u8;
                if cov > 0 {
                    push_rect(
                        out,
                        geom,
                        x as i32 - w / 2,
                        belly as i32,
                        w,
                        (w / 2).max(2),
                        premul_rgb(body, cov),
                    );
                }
            } else {
                // Falling: detach, accelerate, fade.
                let v = (u - 0.30) / 0.70;
                let y = belly + v * v * DRIP_FALL * ch;
                let cov = (cov_core * 0.70 * (1.0 - v)).min(COV_CAP) as u8;
                if cov > 0 {
                    let w = (2.0 + radius * 0.10) as i32;
                    // A falling drop: a narrow neck over a fatter belly.
                    push_rect(
                        out,
                        geom,
                        x as i32 - w / 4,
                        y as i32 - 2,
                        (w / 2).max(1),
                        2,
                        premul_rgb(body, cov / 2),
                    );
                    push_rect(
                        out,
                        geom,
                        x as i32 - w / 2,
                        y as i32,
                        w,
                        (w / 2).max(2),
                        premul_rgb(body, cov),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_util::ink;
    use std::time::Duration;

    fn geom() -> Geom {
        Geom {
            cw: 8,
            ch: 16,
            rows: 8,
            cols: 40,
            origin_x: 0,
            origin_y: 0,
            win_w: 320,
            win_h: 128,
            head: 0,
        }
    }
    fn cfg() -> DropletConfig {
        DropletConfig {
            enabled: true,
            intensity: 1.0,
        }
    }

    /// Disabled ⇒ no fill, no bead, fp 0 (byte-identical to the plain cursor).
    #[test]
    fn disabled_is_inert() {
        let mut d = CursorDroplet::default();
        let mut out = Vec::new();
        let f = d.tick(
            Some((1, 1)),
            Instant::now(),
            1.0,
            geom(),
            &DropletConfig {
                enabled: false,
                intensity: 1.0,
            },
            &mut out,
        );
        assert!(f.fill.is_none());
        assert_eq!(f.fp, 0);
        assert!(out.is_empty());
        assert!(!d.is_active());
    }

    /// Reduced motion / load-shed (`intensity == 0`) ⇒ fully inert even at full surge.
    #[test]
    fn zero_intensity_is_inert() {
        let mut d = CursorDroplet::default();
        let mut out = Vec::new();
        let f = d.tick(
            Some((1, 1)),
            Instant::now(),
            1.0,
            geom(),
            &DropletConfig {
                enabled: true,
                intensity: 0.0,
            },
            &mut out,
        );
        assert!(f.fill.is_none(), "reduced motion keeps the plain cursor");
        assert_eq!(f.fp, 0);
        assert!(out.is_empty());
        assert!(!d.is_active());
    }

    /// A hidden cursor draws no bead, but the fill stays resolved (harmless — the
    /// renderer draws no cursor to fill) and the water still reports its state.
    #[test]
    fn hidden_cursor_draws_no_bead() {
        let mut d = CursorDroplet::default();
        let mut out = Vec::new();
        let f = d.tick(None, Instant::now(), 0.5, geom(), &cfg(), &mut out);
        assert!(out.is_empty(), "no additive light without a cursor cell");
        assert!(f.fill.is_some());
    }

    /// Full surge swells the bead and shines far brighter than still water — the
    /// progressive-splash read.
    #[test]
    fn surge_swells_and_brightens() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let run = |surge: f32| -> u64 {
            let mut d = CursorDroplet::default();
            let mut out = Vec::new();
            d.tick(Some((3, 20)), t, surge, g, &c, &mut out);
            out.clear();
            d.tick(
                Some((3, 20)),
                t + Duration::from_millis(16),
                surge,
                g,
                &c,
                &mut out,
            );
            ink(&out)
        };
        let still_ink = run(0.0);
        let surge_ink = run(1.0);
        assert!(still_ink > 0, "still water is still visibly lit");
        assert!(
            surge_ink > still_ink * 2,
            "full surge far outshines still water ({surge_ink} vs {still_ink})"
        );
    }

    /// At full surge the water leaves the bead: drips fall well BELOW the cursor
    /// cell and ripple rings roll well WIDER than the bead — the splash read.
    #[test]
    fn surge_drips_below_and_ripples_wide() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let mut d = CursorDroplet::default();
        let mut out = Vec::new();
        // Advance a few frames so the ring/drip phases are mid-life.
        for i in 0..20 {
            out.clear();
            d.tick(
                Some((3, 20)),
                t + Duration::from_millis(16 * i),
                1.0,
                g,
                &c,
                &mut out,
            );
        }
        let ch = g.ch as i32;
        let cell_bottom = (3 + 1) * ch;
        let below = out
            .iter()
            .map(|q| q.y as i32 + q.h as i32 - cell_bottom)
            .max()
            .unwrap_or(0);
        assert!(
            below > ch / 3,
            "a drip reaches below the cursor row (got {below}px past the cell)"
        );
        let cx = (20 * g.cw + g.cw / 2) as i32;
        let reach = out
            .iter()
            .map(|q| (q.x as i32 - cx).abs().max(q.x as i32 + q.w as i32 - cx))
            .max()
            .unwrap_or(0);
        assert!(
            reach > (g.ch as f32 * RADIUS_MAX * 1.4) as i32,
            "a ripple ring rolls wider than the bead (got {reach}px)"
        );
    }

    /// The liquid fill brightens from deep water toward azure foam with the surge.
    #[test]
    fn fill_foams_with_surge() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let mut out = Vec::new();
        let mut still = CursorDroplet::default();
        let f0 = still
            .tick(Some((1, 1)), t, 0.0, g, &c, &mut out)
            .fill
            .unwrap();
        let mut hot = CursorDroplet::default();
        let f1 = hot
            .tick(Some((1, 1)), t, 1.0, g, &c, &mut out)
            .fill
            .unwrap();
        let bright = |c: u32| ((c >> 16) & 0xff) + ((c >> 8) & 0xff) + (c & 0xff);
        assert!(
            bright(f1) > bright(f0) + 80,
            "surge foams the fill ({f1:#08x} vs {f0:#08x})"
        );
        // Both stay in water hues: blue channel dominant.
        for f in [f0, f1] {
            let (r, gg, b) = ((f >> 16) & 0xff, (f >> 8) & 0xff, f & 0xff);
            assert!(b >= gg && gg >= r, "water hue ordering, got {f:#08x}");
        }
    }

    /// Every emitted quad is single-row, grid-interior, and water-hued with capped
    /// coverage (the renderer row-gate / parity / legibility invariants).
    #[test]
    fn quads_respect_grid_hue_and_cap() {
        let g = geom();
        let c = cfg();
        let mut d = CursorDroplet::default();
        let mut out = Vec::new();
        let t = Instant::now();
        // A corner cell so clamping is exercised.
        for i in 0..8 {
            if i == 7 {
                out.clear();
            }
            d.tick(
                Some((0, 0)),
                t + Duration::from_millis(16 * i),
                1.0,
                g,
                &c,
                &mut out,
            );
        }
        assert!(!out.is_empty());
        let gw = (g.cols * g.cw) as u32;
        let gh = (g.rows * g.ch) as u32;
        for q in &out {
            let band = q.row as u32 * g.ch as u32;
            assert!(
                q.y as u32 >= band && q.y as u32 + q.h as u32 <= band + g.ch as u32,
                "single-row: {q:?}"
            );
            assert!(
                q.x as u32 + q.w as u32 <= gw && q.y as u32 + q.h as u32 <= gh,
                "in grid: {q:?}"
            );
            let (r, gg, b) = (
                (q.color >> 16) & 0xff,
                (q.color >> 8) & 0xff,
                q.color & 0xff,
            );
            assert!(
                b + 1 >= gg && gg + 1 >= r,
                "premultiplied water hue ordering: {q:?}"
            );
            for sh in [16u32, 8, 0] {
                assert!(
                    (q.color >> sh) & 0xff <= COV_CAP as u32 + 1,
                    "coverage capped: {q:?}"
                );
            }
        }
    }

    /// Surging ⇒ active (host keeps the tick armed); settled surge ⇒ inactive so
    /// the still bead rides the blink cadence at no extra wakeup cost.
    #[test]
    fn settles_to_inactive_when_surge_dies() {
        let g = geom();
        let c = cfg();
        let mut d = CursorDroplet::default();
        let mut out = Vec::new();
        let t = Instant::now();
        d.tick(Some((1, 1)), t, 0.8, g, &c, &mut out);
        assert!(d.is_active(), "surging water keeps the tick armed");
        d.tick(
            Some((1, 1)),
            t + Duration::from_millis(16),
            0.0,
            g,
            &c,
            &mut out,
        );
        assert!(!d.is_active(), "still water idles on the blink cadence");
    }

    /// The per-frame quad budget is bounded by construction (discs + rings +
    /// drips + glints), far under the aurora's MAX_QUADS defence.
    #[test]
    fn quad_count_is_bounded() {
        let g = Geom {
            cw: 20,
            ch: 40,
            rows: 50,
            cols: 200,
            origin_x: 0,
            origin_y: 0,
            win_w: 4000,
            win_h: 2000,
            head: 0,
        };
        let mut d = CursorDroplet::default();
        let mut out = Vec::new();
        d.tick(Some((25, 100)), Instant::now(), 1.0, g, &cfg(), &mut out);
        d.tick(
            Some((25, 100)),
            Instant::now() + Duration::from_millis(16),
            1.0,
            g,
            &cfg(),
            &mut out,
        );
        assert!(
            out.len() < 400,
            "bounded droplet geometry, got {}",
            out.len()
        );
    }
}
