// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The PHASER EMITTER cursor — while the `phaser` trail style is
//! active the block cursor IS the emitter the beam fires from:
//!
//! * **the block FILL locks to the beam's rolling hue** — the same sweep phase
//!   the streak is being laid in (injected by the host from
//!   [`crate::cursor_glow::CursorGlow::beam_hue`]), so the beam reads as light
//!   LEAVING the cursor, not a ribbon that happens to end near it. At rest the
//!   fill is a whisper of the current beam hue over THE CURSOR'S OWN COLOUR
//!   ([`PhaserConfig::base`] — OSC 12 / the configured `cursor_color`); under
//!   the keys it saturates to the vivid beam colour.
//! * **beam-axis energy WINGS** — an additive pair of lens-shaped side lobes on
//!   the horizontal beam axis (the phaser's fat core runs near cell height, so
//!   the wings span the cell vertically and taper to points sideways). They sit
//!   at a dim static floor while idle — the emitter holding its charge — and
//!   breathe/swell with the typing cadence: the capacitor charging as you fire.
//! * **a prism fringe** — the outer wing layers split slightly off the core hue
//!   (trailing spectrum on the left lobe, leading on the right), so the emitter
//!   reads as a prism focusing the sweep, distinct from the rainbow kitty's banded ring.
//!
//! Text-safe by construction, mirroring [`crate::cursor_rainbow`] /
//! [`crate::cursor_fireball`]: the block FILL is returned for the renderer's
//! `floor_cursor_fill` contrast floor (the cut-out glyph stays razor-sharp), and
//! the wings are purely additive [`GlowQuad`] light with capped coverage — the
//! only layer that reaches over neighbouring glyphs is the dim outer tint. Like
//! its siblings it is a CLOCKLESS pure function of an injected `now`, settles to
//! inactive when the charge dies (the idle emitter then rides the blink cadence
//! — no perpetual wakeups), and emits identical premultiplied quads on the CPU
//! and Metal backends.

use aterm_time::Instant;

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::Geom;
use crate::effect_util::push_fx_rect as push_rect;

/// The block-cursor base the beam hue tints FROM when the host names none:
/// white on a dark theme, a soft near-black on a light theme (the same pair as
/// the rainbow cursor, so the two treatments feel like one family at rest).
///
/// A host that KNOWS the cursor's resolved colour (OSC 12, else the configured
/// theme cursor) passes it as [`PhaserConfig::base`] instead, and these two
/// stand only for the raw/embedder callers that have no such value.
const BASE_DARK_THEME: u32 = 0x00FF_FFFF;
const BASE_LIGHT_THEME: u32 = 0x0016_161C;

/// Charged breath (turns/sec of the wing pulse) + its depth. The phase freezes
/// below [`SETTLED_ENERGY`], so an unrelated present after a long idle gap never
/// advances a hidden clock and snaps the port to a different brightness.
const PULSE_HZ: f32 = 0.30;
const PULSE_DEPTH: f32 = 0.55;

/// Saturation / value / base-mix ramps from the calm idle whisper to the vivid
/// charged fill. The idle saturation sits above the rainbow cursor's (0.32): the
/// fill is the BEAM's hue and should read as such even at rest.
const SAT_IDLE: f32 = 0.45;
const SAT_MAX: f32 = 1.0;
const VAL_IDLE: f32 = 0.85;
const VAL_MAX: f32 = 1.0;
const MIX_IDLE: f32 = 0.22;
const MIX_MAX: f32 = 0.88;

/// Wing reach past the cell edge, in cell WIDTHS: a tight idle port glow, up to
/// well over a neighbour cell at full charge. Only the dim outer layer runs the
/// full reach (see [`WING_LAYERS`]), so the far tint never buries a glyph.
const REACH_IDLE: f32 = 0.30;
const REACH_MAX: f32 = 1.60;

/// The nested lens layers, innermost last: (reach ×, coverage ×, hue fringe in
/// turns, height ×). The bright core hugs the cell at a fraction of the reach
/// and spans the cell height (the phaser's fat beam core); the outer layers are
/// the faint prism fringe, FLATTER the further they reach, so the far light is a
/// slim flare on the beam axis — a beam leaving an emitter, not a ball — and
/// stays off the ascenders/descenders of neighbouring glyphs. Additive overlap
/// builds the sideways falloff without per-pixel math.
const WING_LAYERS: [(f32, f32, f32, f32); 4] = [
    (1.00, 0.18, 0.10, 0.45),
    (0.74, 0.38, 0.06, 0.62),
    (0.50, 0.68, 0.025, 0.82),
    (0.28, 1.00, 0.0, 1.00),
];

/// Innermost-layer peak coverage (× charge, pre-cap): a dim idle port, a bright
/// charged core. Every push is additionally clamped by [`COV_CAP`], matching the
/// fireball's text-safety band — the wings overlap the neighbouring glyph cells,
/// so the additive light must stay a tint out there.
const COV_IDLE: f32 = 46.0;
const COV_MAX: f32 = 132.0;
const COV_CAP: f32 = 96.0;

/// STACK budget for the strip where all four [`WING_LAYERS`] overlap (the band
/// hugging the cursor's cell edges — exactly where the 1-2 NEWEST typed glyphs
/// sit). The per-layer [`COV_CAP`] alone let the layered sum reach ~250 at full
/// charge, saturating those glyphs white (the audited head white-out); capping
/// the shared core so `Σ(layer × core) ≤ budget` keeps the stacked coverage at
/// the rainbow-ribbon text-safety ceiling while the lens SHAPE is untouched.
const WING_STACK_BUDGET: f32 = 150.0;
/// Σ of the [`WING_LAYERS`] coverage multipliers (the worst-case overlap).
const WING_STACK_OVERLAP: f32 = 2.24;

/// The charge below which the emitter is SETTLED — the animator reports itself
/// inactive so the host stops arming the 60 fps tick (the idle port glow then
/// rides the blink cadence, at zero extra wakeup cost).
const SETTLED_ENERGY: f32 = 0.02;

/// Resolved per-frame inputs (Copy so the host reads it out before borrowing state).
#[derive(Clone, Copy, Debug)]
pub struct PhaserConfig {
    /// Master on/off (the `phaser` style opted in AND the cursor is a focused,
    /// visible block).
    pub enabled: bool,
    /// Overall scale `0..1` — the reduced-motion / load-shed amplitude, folded in
    /// by the host exactly like the aurora. 0 ⇒ fully inert (plain themed cursor).
    pub intensity: f32,
    /// The colour the emitter block wears AT REST, `0x00RRGGBB` — the terminal's
    /// resolved cursor colour (OSC 12 when set, else the configured theme
    /// cursor, else the live OSC 10 foreground). The beam hue is a TINT over
    /// this base, so a settled caret is the user's cursor colour and charging
    /// blooms it toward the sweep.
    ///
    /// The rainbow block's hole, one style over (`d602f8cd`): this fill leaves
    /// the tick as [`aterm_render::RenderInput::cursor_fill_override`], which
    /// the renderer applies INSTEAD of the frame cursor colour — so a
    /// hard-coded base meant OSC 12 and the theme cursor reached every cursor
    /// shape EXCEPT the block this style paints. The comet and the beam rod
    /// already build their bodies from a host-resolved colour
    /// (`CometConfig::color` / `BeamRodConfig::color`); the emitter simply
    /// never did.
    ///
    /// `None` keeps the historical theme-polar base ([`BASE_DARK_THEME`] /
    /// [`BASE_LIGHT_THEME`]) for callers with no cursor colour to hand — every
    /// such frame is byte-identical to before.
    pub base: Option<u32>,
}

/// What a tick produced: the beam-hued block FILL to hand the renderer (it floors
/// it for contrast) and a fingerprint that changes on every visible step (0 when off).
#[derive(Clone, Copy, Debug)]
pub struct PhaserFrame {
    /// The beam-hued block-fill colour `0x00RRGGBB`, or `None` when the emitter
    /// is off (the renderer then keeps the ordinary themed cursor fill).
    pub fill: Option<u32>,
    /// Fingerprint of the emitted fill + wings (0 ⇒ nothing this frame).
    pub fp: u64,
}

/// Per-window emitter animation state — one breath phase plus the last clock
/// reading and a latched charge. Tiny + Copy-cheap.
#[derive(Default)]
pub struct CursorPhaser {
    /// Idle-breath phase in turns `0..1`.
    pulse: f32,
    last: Option<Instant>,
    /// Latched charge at the last tick (so [`is_active`] answers without a clock).
    energy: f32,
}

impl CursorPhaser {
    /// Whether the host must keep arming the animation tick: only while the
    /// emitter is still CHARGED (typing or cooling). Once settled the idle port
    /// glow rides the blink cadence — no perpetual wakeups on a focused idle window.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.energy > SETTLED_ENERGY
    }

    /// Advance one frame at `now` with the beam's rolling `hue` (turns `0..1`,
    /// the host reads [`crate::cursor_glow::CursorGlow::beam_hue`] after the
    /// aurora tick) and the current typing `energy` (`0..1`), the block cursor
    /// cell `cur` (`None` ⇒ hidden), the theme darkness, grid `geom`, and the
    /// resolved `cfg`. Appends the additive wings to `out` and returns the
    /// beam-hued block FILL + a fingerprint. Pure: no wall-clock, unit-testable
    /// by injecting `now`/`hue`/`energy`.
    #[allow(clippy::too_many_arguments)]
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        hue: f32,
        energy: f32,
        dark_theme: bool,
        geom: Geom,
        cfg: &PhaserConfig,
        out: &mut Vec<GlowQuad>,
    ) -> PhaserFrame {
        let e = (energy.clamp(0.0, 1.0) * cfg.intensity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        // Fully inert — byte-identical to the plain themed cursor — when off, when
        // the geometry is degenerate, or when the amplitude is zero (reduced
        // motion / load-shed), mirroring the rainbow/fireball "0 ⇒ off" contract.
        if !cfg.enabled || geom.cw == 0 || geom.ch == 0 || cfg.intensity <= 0.0 {
            self.energy = 0.0; // inert: report settled so the host disarms the tick
            self.last = Some(now);
            return PhaserFrame { fill: None, fp: 0 };
        }
        let was_active = self.energy > SETTLED_ENERGY;
        self.energy = e;

        // Advance the breath only across a continuously CHARGED interval. An
        // idle emitter has no armed timer, so integrating a clamped slice on
        // the next unrelated present made its supposedly-settled pixels jump
        // (and made t=5/t=8 captures differ). A fresh charge also starts from
        // the frozen phase instead of charging for time that elapsed while it
        // was idle.
        let active = e > SETTLED_ENERGY;
        let dt = if was_active && active {
            self.last
                .map(|t| now.saturating_duration_since(t).as_secs_f32())
                .unwrap_or(0.0)
                .min(0.25)
        } else {
            0.0
        };
        self.last = Some(now);
        if active {
            self.pulse = (self.pulse + dt * PULSE_HZ).fract();
        }
        let breath = 0.5 + 0.5 * (self.pulse * std::f32::consts::TAU).sin(); // 0..1

        // The BLOCK FILL: the cursor's own colour tinted toward the BEAM's hue
        // with charge. The renderer floors this against the cell bg (the cut-out
        // glyph colour), so the glyph stays sharp however saturated the block gets.
        // …FROM the cursor's own colour when the host resolved one: the block IS
        // the cursor, so a settled caret must be whatever OSC 12 (or the theme
        // `cursor_color`) says it is, and the beam hue is the tint charging
        // lays over it. Only a caller that has no such value falls back to the
        // theme-polar constants.
        let beam = hsv2rgb_turns(hue, lerp(SAT_IDLE, SAT_MAX, e), lerp(VAL_IDLE, VAL_MAX, e));
        let base = cfg.base.unwrap_or(if dark_theme {
            BASE_DARK_THEME
        } else {
            BASE_LIGHT_THEME
        });
        let fill = mix_rgb(base, beam, lerp(MIX_IDLE, MIX_MAX, e));

        // The additive WINGS, drawn only over a visible in-grid cursor cell. A
        // small breathing idle floor keeps the port glow alive at rest.
        // DARK THEMES ONLY: additive lobes cannot brighten a white ground —
        // on light themes they only lifted the neighbouring glyphs toward the
        // background; the theme-aware block FILL is the emitter's tell there.
        let charge = COV_IDLE / COV_MAX * (0.45 + PULSE_DEPTH * breath) + e;
        if dark_theme
            && let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
            && charge > 0.01
        {
            self.emit_wings(cr, cc, e, charge, hue, geom, cfg, out);
        }

        // Fingerprint: quantized hue + breath + charge + fill, so a settled
        // emitter early-outs the present but any visible step (a beam-hue
        // advance, the breath, a charge change) forces a repaint. Never 0 while
        // enabled — the idle port glow is always drawable.
        let fp = ((hue * 1024.0) as u64)
            .wrapping_mul(1_000_003)
            .wrapping_add((charge * 255.0) as u64)
            .wrapping_add(((fill as u64) << 12) ^ ((self.pulse * 64.0) as u64))
            | 1;

        PhaserFrame {
            fill: Some(fill),
            fp,
        }
    }

    /// Emit the two lens-shaped wings as premultiplied additive quads. Bounded by
    /// construction: ≤ 3 layers × (cell_h / slab) slabs × 2 sides.
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor cell + charge/hue + geometry/config; one internal call site"
    )]
    fn emit_wings(
        &self,
        cr: u16,
        cc: u16,
        e: f32,
        charge: f32,
        hue: f32,
        geom: Geom,
        cfg: &PhaserConfig,
        out: &mut Vec<GlowQuad>,
    ) {
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        // WINDOW-ABSOLUTE cell edges (the window-space effects layer: Geom
        // carries the grid origin; without it the wings painted a full origin
        // up-left of the cursor — into the TITLEBAR for row 0 — and, tagged
        // with rows their pixels don't overlap, the strays froze on glass
        // when the emitter settled).
        let cell_l = i32::from(geom.origin_x) + cc as i32 * geom.cw as i32;
        let cell_r = cell_l + geom.cw as i32;
        let cy = f32::from(geom.origin_y) + (cr as f32 + 0.5) * ch;
        let reach = (REACH_IDLE + (REACH_MAX - REACH_IDLE) * e) * cw;
        // Core coverage, capped so the four-layer overlap strip at the cell
        // edge — over the newest typed glyphs — stays under the stack budget.
        let cov_core =
            (COV_MAX * charge * cfg.intensity).min(WING_STACK_BUDGET / WING_STACK_OVERLAP);

        // Slab height ~1/8 cell (min 2px): fine enough that the tapering lens
        // reads as a point, not a staircase.
        let slab = ((ch * 0.125) as i32).max(2);
        for &(rx, cov_x, fringe, hx) in &WING_LAYERS {
            let r = reach * rx;
            if r < 1.0 {
                continue;
            }
            let cov = (cov_core * cov_x).min(COV_CAP) as u8;
            if cov == 0 {
                continue;
            }
            // Prism fringe: the LEFT lobe trails the sweep, the RIGHT leads it.
            // The core layer (fringe 0) is the pure beam hue on both sides.
            let left_premul = premul_rgb(hsv2rgb_turns(hue - fringe, 0.95, 1.0), cov);
            let right_premul = premul_rgb(hsv2rgb_turns(hue + fringe, 0.95, 1.0), cov);
            let half_h = (ch * 0.5 * hx).max(1.0);
            let mut dy = -(half_h as i32);
            while dy < half_h as i32 {
                let h = slab.min(half_h as i32 - dy).max(1);
                // Elliptical lens: half-width at the slab's vertical centre —
                // full reach on the beam axis, tapering to a point at the
                // layer's top/bottom edges.
                let ym = (dy as f32 + h as f32 * 0.5) / half_h;
                let hw = (r * (1.0 - ym * ym).max(0.0).sqrt()) as i32;
                if hw >= 1 {
                    let y = cy as i32 + dy;
                    push_rect(out, geom, cell_l - hw, y, hw, h, left_premul);
                    push_rect(out, geom, cell_r, y, hw, h, right_premul);
                }
                dy += h;
            }
        }
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// HSV with hue in TURNS (`0..1`, any real — wrapped) → `0x00RRGGBB` (mirrors the
/// rainbow cursor's, kept local so this module needs no cross-import).
fn hsv2rgb_turns(h: f32, s: f32, v: f32) -> u32 {
    let h = (h.fract() + 1.0).fract() * 6.0;
    let i = h.floor() as i32;
    let f = h - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (r, g, b) = match i.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    let u = |c: f32| ((c.clamp(0.0, 1.0)) * 255.0 + 0.5) as u32;
    (u(r) << 16) | (u(g) << 8) | u(b)
}

/// Clamped per-channel RGB mix (`t` from a → b).
fn mix_rgb(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |sh: u32| {
        let ca = ((a >> sh) & 0xff) as f32;
        let cb = ((b >> sh) & 0xff) as f32;
        ((ca + (cb - ca) * t).round().clamp(0.0, 255.0) as u32) << sh
    };
    ch(16) | ch(8) | ch(0)
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
            rows: 6,
            cols: 40,
            origin_x: 0,
            origin_y: 0,
            win_w: 320,
            win_h: 96,
            head: 0,
        }
    }
    fn cfg() -> PhaserConfig {
        PhaserConfig {
            enabled: true,
            intensity: 1.0,
            // No host base: these fixtures pin the historical theme-polar
            // bloom, so they stay byte-identical to the pre-`base` tick.
            base: None,
        }
    }

    /// Disabled ⇒ no fill, no wings, fp 0 (byte-identical to the plain cursor).
    #[test]
    fn disabled_is_inert() {
        let mut cp = CursorPhaser::default();
        let mut out = Vec::new();
        let f = cp.tick(
            Some((1, 1)),
            Instant::now(),
            0.3,
            1.0,
            true,
            geom(),
            &PhaserConfig {
                enabled: false,
                intensity: 1.0,
                base: None,
            },
            &mut out,
        );
        assert!(f.fill.is_none());
        assert_eq!(f.fp, 0);
        assert!(out.is_empty());
        assert!(!cp.is_active());
    }

    /// Reduced motion / load-shed (`intensity == 0`) ⇒ fully inert even at full
    /// charge — no beam-hued fill, no wings, settled.
    #[test]
    fn zero_intensity_is_inert() {
        let mut cp = CursorPhaser::default();
        let mut out = Vec::new();
        let f = cp.tick(
            Some((1, 1)),
            Instant::now(),
            0.3,
            1.0,
            true,
            geom(),
            &PhaserConfig {
                enabled: true,
                intensity: 0.0,
                base: None,
            },
            &mut out,
        );
        assert!(f.fill.is_none(), "reduced motion keeps the plain cursor");
        assert_eq!(f.fp, 0);
        assert!(out.is_empty());
        assert!(!cp.is_active());
    }

    /// A hidden cursor draws no wings, but the fill stays resolved (harmless —
    /// the renderer draws no cursor to fill).
    #[test]
    fn hidden_cursor_draws_no_wings() {
        let mut cp = CursorPhaser::default();
        let mut out = Vec::new();
        let f = cp.tick(
            None,
            Instant::now(),
            0.3,
            0.5,
            true,
            geom(),
            &cfg(),
            &mut out,
        );
        assert!(out.is_empty(), "no additive light without a cursor cell");
        assert!(f.fill.is_some());
    }

    /// The block fill FOLLOWS the beam hue: two well-separated hues at full
    /// charge produce clearly different fills, and both sit close to the beam
    /// colour (the emitter and the streak are one piece of light).
    #[test]
    fn fill_locks_to_the_beam_hue() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let mut out = Vec::new();
        let mut fill_at = |hue: f32| -> u32 {
            let mut cp = CursorPhaser::default();
            cp.tick(Some((1, 1)), t, hue, 1.0, true, g, &c, &mut out)
                .fill
                .unwrap()
        };
        let red = fill_at(0.0);
        let cyan = fill_at(0.5);
        // Red-ish hue ⇒ red channel dominates; cyan-ish ⇒ red is the weakest.
        let (rr, rg, rb) = ((red >> 16) & 0xff, (red >> 8) & 0xff, red & 0xff);
        let (cr_, cg, cb) = ((cyan >> 16) & 0xff, (cyan >> 8) & 0xff, cyan & 0xff);
        assert!(
            rr > rg + 60 && rr > rb + 60,
            "hue 0 fill is red: {red:#08x}"
        );
        assert!(
            cg > cr_ + 60 && cb > cr_ + 60,
            "hue 0.5 fill is cyan: {cyan:#08x}"
        );
    }

    /// With NO host cursor colour to hand, the fill blooms from the theme base
    /// (near-white on dark) at rest toward the saturated beam hue under charge,
    /// and a light theme starts near-black — the raw/embedder fallback, kept
    /// byte-identical by [`PhaserConfig::base`] being `None` here.
    #[test]
    fn fill_blooms_from_theme_base() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let mut out = Vec::new();
        let mut idle = CursorPhaser::default();
        let f_idle = idle
            .tick(Some((1, 1)), t, 0.0, 0.0, true, g, &c, &mut out)
            .fill
            .unwrap();
        let minch = |c: u32| {
            [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff]
                .into_iter()
                .min()
                .unwrap()
        };
        assert!(
            minch(f_idle) > 140,
            "idle block stays near white on dark, got {f_idle:#08x}"
        );
        let mut hot = CursorPhaser::default();
        let f_hot = hot
            .tick(Some((1, 1)), t, 0.0, 1.0, true, g, &c, &mut out)
            .fill
            .unwrap();
        let spread = |c: u32| {
            let (r, gg, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
            r.max(gg).max(b) - r.min(gg).min(b)
        };
        assert!(
            spread(f_hot) > spread(f_idle) + 60,
            "charge saturates the fill toward the beam hue"
        );
        let mut light = CursorPhaser::default();
        let f_light = light
            .tick(Some((1, 1)), t, 0.0, 0.0, false, g, &c, &mut out)
            .fill
            .unwrap();
        let maxch = [
            (f_light >> 16) & 0xff,
            (f_light >> 8) & 0xff,
            f_light & 0xff,
        ]
        .into_iter()
        .max()
        .unwrap();
        assert!(
            maxch < 110,
            "idle block near black on a light theme, got {f_light:#08x}"
        );
    }

    /// THE HOST'S CURSOR COLOUR IS THE EMITTER BLOCK, on either theme polarity.
    ///
    /// The block fill leaves this tick as `RenderInput::cursor_fill_override`,
    /// which the renderer applies INSTEAD of `frame_cursor(input)` — so
    /// whatever base this returns is literally the cursor the user sees. With
    /// the base hard-coded to white/near-black, OSC 12 and the configured
    /// `cursor_color` reached every cursor shape except the one the `phaser`
    /// style paints. The rainbow block had the same hole and the same fix
    /// (`d602f8cd`); this is that test, one style over. A settled caret must
    /// therefore BE the host's colour, and two colours must never collapse to
    /// one block.
    #[test]
    fn host_base_is_the_settled_block_on_either_polarity() {
        let g = geom();
        let mut out = Vec::new();
        let t = Instant::now();
        // A SETTLED emitter (energy 0) with a FIXED beam hue: the only thing
        // that can move this fill between two ticks is the base.
        let settled = |base: u32, dark: bool, out: &mut Vec<GlowQuad>| {
            let c = PhaserConfig {
                base: Some(base),
                ..cfg()
            };
            out.clear();
            CursorPhaser::default()
                .tick(Some((1, 1)), t, 0.5, 0.0, dark, g, &c, out)
                .fill
                .unwrap()
        };
        let chans = |c: u32| ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
        for dark in [true, false] {
            let (rr, rg, rb) = chans(settled(0x00FF_0000, dark, &mut out));
            assert!(
                rr > 200 && rg < 130 && rb < 130,
                "a red cursor colour settles red (dark={dark})"
            );
            let (br, bg, bb) = chans(settled(0x0000_00FF, dark, &mut out));
            assert!(
                bb > 200 && br < 130 && bg < 130,
                "a blue cursor colour settles blue (dark={dark})"
            );
            assert_ne!(
                settled(0x00FF_0000, dark, &mut out),
                settled(0x0000_00FF, dark, &mut out),
                "two cursor colours must not paint one identical block (dark={dark})"
            );
        }
        // …and charge still blooms the BEAM hue over that base rather than
        // replacing the base's job: the charged fill differs markedly.
        let c = PhaserConfig {
            base: Some(0x00FF_0000),
            ..cfg()
        };
        out.clear();
        let hot = CursorPhaser::default()
            .tick(Some((1, 1)), t, 0.5, 1.0, true, g, &c, &mut out)
            .fill
            .unwrap();
        assert_ne!(hot, settled(0x00FF_0000, true, &mut out));
    }

    /// Full charge reaches further sideways and burns far brighter than the idle
    /// port glow — the capacitor-charging read.
    #[test]
    fn charge_widens_and_brightens_the_wings() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let run = |energy: f32| -> (u64, i32, i32) {
            let mut cp = CursorPhaser::default();
            let mut out = Vec::new();
            cp.tick(Some((3, 20)), t, 0.3, energy, true, g, &c, &mut out);
            out.clear();
            cp.tick(
                Some((3, 20)),
                t + Duration::from_millis(16),
                0.3,
                energy,
                true,
                g,
                &c,
                &mut out,
            );
            let x0 = out.iter().map(|q| q.x as i32).min().unwrap_or(0);
            let x1 = out.iter().map(|q| (q.x + q.w) as i32).max().unwrap_or(0);
            (ink(&out), x0, x1)
        };
        let (idle_ink, idle_x0, idle_x1) = run(0.0);
        let (hot_ink, hot_x0, hot_x1) = run(1.0);
        assert!(idle_ink > 0, "the idle port glow is still visibly lit");
        assert!(
            hot_ink > idle_ink * 2,
            "full charge far outshines idle ({hot_ink} vs {idle_ink})"
        );
        assert!(
            hot_x0 < idle_x0 && hot_x1 > idle_x1,
            "charged wings reach further ({hot_x0}..{hot_x1} vs {idle_x0}..{idle_x1})"
        );
    }

    /// The wings stay on the cursor's ROW (the beam axis) — no light above or
    /// below the cell — and never touch the cell's own interior columns (purely
    /// side lobes; the fill owns the cell).
    #[test]
    fn wings_hug_the_beam_axis() {
        let g = geom();
        let c = cfg();
        let mut cp = CursorPhaser::default();
        let mut out = Vec::new();
        let t = Instant::now();
        let (row, col) = (3u16, 20u16);
        cp.tick(Some((row, col)), t, 0.3, 1.0, true, g, &c, &mut out);
        assert!(!out.is_empty());
        let cell_l = col as u32 * g.cw as u32;
        let cell_r = cell_l + g.cw as u32;
        for q in &out {
            assert_eq!(q.row, row, "wings never leave the cursor row: {q:?}");
            assert!(
                (q.x as u32 + q.w as u32) <= cell_l || q.x as u32 >= cell_r,
                "side lobes only — the fill owns the cell: {q:?}"
            );
        }
    }

    /// Every emitted quad is single-row, grid-interior, with capped coverage
    /// (the renderer row-gate / parity / legibility invariants), even at a
    /// grid-edge cell where clamping is exercised.
    #[test]
    fn quads_respect_grid_and_cap() {
        let g = geom();
        let c = cfg();
        let mut cp = CursorPhaser::default();
        let mut out = Vec::new();
        let t = Instant::now();
        for cell in [(0u16, 0u16), (5, 39)] {
            cp.tick(Some(cell), t, 0.7, 1.0, true, g, &c, &mut out);
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
            for sh in [16u32, 8, 0] {
                assert!(
                    (q.color >> sh) & 0xff <= COV_CAP as u32 + 1,
                    "coverage capped: {q:?}"
                );
            }
        }
    }

    /// Charged ⇒ active (host keeps the tick armed); settled charge ⇒ inactive
    /// so the idle port glow rides the blink cadence at no extra wakeup cost.
    #[test]
    fn settles_to_inactive_when_charge_dies() {
        let g = geom();
        let c = cfg();
        let mut cp = CursorPhaser::default();
        let mut out = Vec::new();
        let t = Instant::now();
        cp.tick(Some((1, 1)), t, 0.3, 0.8, true, g, &c, &mut out);
        assert!(cp.is_active(), "a charged emitter keeps the tick armed");
        cp.tick(
            Some((1, 1)),
            t + Duration::from_millis(16),
            0.3,
            0.0,
            true,
            g,
            &c,
            &mut out,
        );
        assert!(
            !cp.is_active(),
            "the settled emitter idles on the blink cadence"
        );
    }

    /// REGRESSION: the idle port is static. Sparse captures or an unrelated
    /// repaint after seconds of silence must not integrate a hidden breath
    /// slice and produce a one-level cursor-edge flicker.
    #[test]
    fn settled_present_gaps_are_byte_identical() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let mut cp = CursorPhaser::default();
        let mut out = Vec::new();

        cp.tick(Some((1, 12)), t, 0.3, 0.8, true, g, &c, &mut out);
        out.clear();
        let first = cp.tick(
            Some((1, 12)),
            t + Duration::from_secs(5),
            0.3,
            0.0,
            true,
            g,
            &c,
            &mut out,
        );
        let first_quads = out.clone();
        out.clear();
        let late = cp.tick(
            Some((1, 12)),
            t + Duration::from_secs(30),
            0.3,
            0.0,
            true,
            g,
            &c,
            &mut out,
        );

        assert_eq!(late.fill, first.fill);
        assert_eq!(late.fp, first.fp);
        assert_eq!(out, first_quads);
        assert!(!cp.is_active());
    }

    /// The per-frame quad budget is bounded by construction (3 layers × slabs ×
    /// 2 sides), far under the aurora's MAX_QUADS defence.
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
        let mut cp = CursorPhaser::default();
        let mut out = Vec::new();
        cp.tick(
            Some((25, 100)),
            Instant::now(),
            0.3,
            1.0,
            true,
            g,
            &cfg(),
            &mut out,
        );
        assert!(out.len() < 200, "bounded wing geometry, got {}", out.len());
    }
}
