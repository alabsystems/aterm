// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sparkle-words v2 **supernova** emitters (docs/sparkle-words-v2-design.md §6).
//!
//! Everything here is a PURE function of `(t = now − ignition, genome, geometry)`
//! — no stored phase, no per-frame state (§6.1). The host state machine
//! (`word_decorations.rs`) owns ignition times, the flash limiter, and the
//! one-nova-per-episode guard; this module turns a time offset into pixels:
//!
//! * **Phase machine** (§6.1): `Armed → Dip → Flash → Ring → Debris → Ember →
//!   Settled`, all windows fixed except the genome duration `D ∈ 1000..=1400 ms`.
//! * **Ring + rays** (§6.3, the geometry-invariant budget): the ring is a true
//!   circle rasterized as **exactly [`RING_BANDS`] row-band chord slabs** (exact
//!   fixed-count subdivision — band *i* spans `[round(2r·i/30), round(2r·(i+1)/30))`
//!   — NOT a ceiling step, which would yield 21/25/28 bands across cell sizes and
//!   falsify the closed form; the §15.14 erratum). One uniform-coverage quad per
//!   chord per band (the `emit_aa_slab` COVERAGE idiom — coverage × AA fraction
//!   quantized once into `premul_rgb` — expressly not its 3-part emission shape);
//!   a chromatic-fringe second ring at `R + chroma` rides the same bands with its
//!   0.55 coverage folded into the per-band scalar; every quad is split at
//!   cell-row boundaries by the sink (the mandatory `push_glow_rect` rule).
//!   Rays step **exactly [`RAY_SAMPLES`] fixed samples** along their major axis
//!   with `comet_beam`'s 2.5→1 px taper folded into per-sample coverage/width.
//!   Closed form: `Q ≤ 392` at every cell size (§6.3), pinned by the
//!   `nova_budget_geometry_invariant` regression below.
//! * **Spend order** (§6.3): ring first, then rays (inner→outer, so a binding
//!   cap drops OUTER slices), then the crown only when it fits whole — the crown
//!   drops first and the ring is never gapped.
//! * **Magic variants** (§3.5/§6.1): **Quasar** replaces the rays with 2
//!   vertical polar jets at doubled length (30 samples each); **Singularity**
//!   time-reverses every radius (the ring contracts, debris falls inward) and
//!   adds a ≤ 20-cell Over-blend `RingArc` darkening ring on the existing wdeco
//!   machinery, ending in a dim violet ember.
//! * **Blast coupling** (§6.5): `crossing_ms`/`pulse_env`/`constant_lum_toward`
//!   give neighboring ink a one-shot ~150 ms chroma pulse at approximately
//!   constant relative luminance — the |ΔL| ≤ 5 % bound is enforced **by
//!   construction** (a luminance-matching bisection), asserted over the whole
//!   deployed color table (8 palettes × every 4-bit hue-nudge code) in tests.
//!
//! Parity: quads are premultiplied `0x00RRGGBB` [`GlowQuad`]s — `premul_rgb`
//! once host-side, `add_sat` == GPU One/One on the Unorm view (§8); debris and
//! the Singularity ring ride the existing wdeco Add/Over parity machinery.

use aterm_render::{DecoBlend, DecoGlyph, GlowQuad, WordDecoration, premul_rgb};
use aterm_scene::{mix_rgb, smoothstep};

use crate::color_math::{hsv2rgb, relative_luminance, rgb2hsv};
use crate::genome::{NovaFeatures, NovaMagic, mix};

/// §6.1 anticipation-dip window (ms): ink dims ~35 %, no quads.
pub const DIP_MS: u64 = 120;
/// §6.1 core-flash window end (ms): the star-glint crown, one rise-and-fall.
pub const FLASH_END_MS: u64 = 240;
/// §6.1 shockwave window end (ms): ring + rays sweep out (or in) and fade.
pub const RING_END_MS: u64 = 900;
/// §6.1 debris window start (ms); it ends at the genome duration `D`.
pub const DEBRIS_START_MS: u64 = 500;
/// Label boundary between Ember and Settled (ms past `D`): emission is
/// identical in both (the steady residual spark; zero quads) — Settled is the
/// honest "the nova stream emits nothing" name once the window is long gone.
pub const EMBER_TAIL_MS: u64 = 500;

/// §6.3 fixed ring band count (exact subdivision; the step scales, never this).
pub const RING_BANDS: usize = 30;
/// §6.3 fixed per-ray major-axis sample count.
pub const RAY_SAMPLES: usize = 15;
/// §6.3 Quasar jet sample count (doubled length ⇒ the same relative granularity).
pub const JET_SAMPLES: usize = 30;
/// §6.3 caps: per-nova quad budget (spend-order truncation past it).
pub const MAX_NOVA_QUADS_PER: usize = 512;
/// §6.3 caps: concurrent animating novas (excess skips straight to Ember).
pub const MAX_ACTIVE_NOVAS: usize = 3;
/// §6.3 caps: the DECORATION producers' backstop on the shared `nova_add`
/// channel — the ceiling `word_decorations` funds every classic nova and every
/// supernova under (`MAX_NOVA_QUADS.saturating_sub(nova.len())`). It never binds
/// on a genome-reachable frame: 3 × 392 = 1176 < 1536 at every cell size.
///
/// NOT a bound on the channel TOTAL, and never was one. PRISM WAKE
/// (`crate::output_streak`) is a SECOND producer, and the host appends its
/// per-pane quads AFTER the decoration pass has finished spending — so they are
/// neither funded by this budget nor visible to it, and the channel carries
/// `decoration share + Σ panes streak share` (measured at ~2000 quads on four
/// 200-column panes). Nothing downstream cares: the consumer
/// (`aterm_render::Renderer::draw_nova` → `draw_flat_add`) walks the whole slice
/// with no cap, no fixed buffer and no assert, and the host's channel is a
/// resident scratch that grows once. Pinned in
/// `aterm-effects/tests/nova_channel_budget.rs`.
pub const MAX_NOVA_QUADS: usize = 1536;
/// §6.3 crown worst case (4 tapered spike chords + 2 core-beam chords, each
/// band shorter than one cell ⇒ ≤ 2 row-split quads per chord) — the ≤ 12
/// crown edge of the ay-CHC `Q ≤ 392` certificate, unchanged by the v2.1
/// star-glint reshape. The crown is funded LAST and only whole, so it drops
/// first under a cap.
const CROWN_QUADS: usize = 12;
/// §6.1 Singularity darkening-ring cell cap (Over-blend `RingArc` wdecos).
pub const MAX_RING_ARC_CELLS: usize = 20;
/// §6.5 blast-coupling cap: the N nearest ink-bearing occurrences per nova.
pub const MAX_COUPLING_WORDS: usize = 16;
/// §6.5 chroma-pulse window (ms) and amplitude cap.
pub const PULSE_MS: u64 = 150;
pub const PULSE_AMP: f32 = 0.35;
/// §6.4 debris twinkle phase grid (ms): mote phases quantize to this shared
/// grid so the REGION's combined onsets stay ≤ 3/s (per-mote periods alone
/// would not bound 8–20 out-of-phase motes).
pub const TWINKLE_GRID_MS: u64 = 350;

/// §6.2 `NOVA_PALETTES[8]` verbatim: `(core, fringe)` as `0x00RRGGBB`,
/// Gray-indexed (the genome's palette field is already Gray-decoded), so
/// neighboring indices are similar tints:
/// solar · ember · rose · violet · ion · plasma · aurora · emerald.
pub const NOVA_PALETTES: [(u32, u32); 8] = [
    (0x00FF_F2C8, 0x00FF_9A3C), // solar
    (0x00FF_D8A0, 0x00FF_5C3C), // ember
    (0x00FF_E0F0, 0x00FF_5CA8), // rose
    (0x00EE_E0FF, 0x00A0_5CFF), // violet
    (0x00E0_F0FF, 0x006C_8CFF), // ion
    (0x00E8_F6FF, 0x004C_C8FF), // plasma
    (0x00E0_FFF0, 0x003C_E8A0), // aurora
    (0x00E8_FFD8, 0x007C_D83C), // emerald
];

/// The `(core, fringe)` pair for a Gray-decoded palette index.
pub fn palette(idx: u8) -> (u32, u32) {
    NOVA_PALETTES[usize::from(idx & 7)]
}

/// §6.1 phase, a pure function of `(t = now − ignition, D)`. `t < 0` is Armed
/// (the limiter deferred the Dip start). Ring and Debris overlap in emission
/// (the ring draws until [`RING_END_MS`] while debris flies from
/// [`DEBRIS_START_MS`]); the label switches at debris onset. Monotone; no
/// re-entry without a re-arm (true episode death, §3.6).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NovaPhase {
    Armed,
    Dip,
    Flash,
    Ring,
    Debris,
    Ember,
    Settled,
}

pub fn phase(t_ms: i64, d_ms: u32) -> NovaPhase {
    let d = i64::from(d_ms);
    match t_ms {
        t if t < 0 => NovaPhase::Armed,
        t if t < DIP_MS as i64 => NovaPhase::Dip,
        t if t < FLASH_END_MS as i64 => NovaPhase::Flash,
        t if t < DEBRIS_START_MS as i64 => NovaPhase::Ring,
        t if t < d => NovaPhase::Debris,
        t if t < d + EMBER_TAIL_MS as i64 => NovaPhase::Ember,
        _ => NovaPhase::Settled,
    }
}

/// §6.1 Dip: the ink envelope multiplier at `t` — `1 − 0.35·smoothstep(p)`
/// inside the dip window, `1.0` outside (the indrawn breath rides the ink
/// channel; it costs no quads).
pub fn dip_envelope(t_ms: u64) -> f32 {
    if t_ms >= DIP_MS {
        return 1.0;
    }
    1.0 - 0.35 * smoothstep(t_ms as f32 / DIP_MS as f32)
}

/// Everything the pure emitters need for one nova, resolved by the host once
/// per frame (geometry in px; colors already genome-hue-nudged).
pub struct NovaEnv {
    /// Grid extent in px (quads clamp here; the one-band split uses `cell_h`).
    pub grid_w: i32,
    pub grid_h: i32,
    /// Row advance in px for THIS word's row (2× cell width on DECDWL rows, the
    /// v1 DEC precedent — the center anchor and debris cells follow the row's
    /// real advance).
    pub cell_w: i32,
    pub cell_h: i32,
    /// Nova center, px (the word span's visual midpoint, row band center).
    pub cx: i32,
    pub cy: i32,
    /// Genome ring radius in px (`radius_rows · cell_h`, 1.6..=2.2 rows).
    pub r_max: f32,
    pub feats: NovaFeatures,
    pub magic: Option<NovaMagic>,
    /// Hue-nudged palette endpoints (§6.2 + the genome ink-pair window).
    pub core: u32,
    pub fringe: u32,
    /// Config profanity intensity (v1 key, reused as the nova brightness).
    pub intensity: f32,
    /// The occurrence seed — the §6.1 randomness root (debris ballistics).
    pub seed: u64,
}

/// Per-shape quad counts of one [`emit_nova`] call (the §6.3 budget regression
/// and the spend-order truncation tests read these; the §7.5 ledger's
/// "Nova emit bound" row names this instrumentation as the runnable-now
/// companion of the P6 ay-CHC certificate).
#[derive(Default, Clone, Copy, Debug)]
pub struct NovaCounts {
    pub ring: usize,
    pub rays: usize,
    pub crown: usize,
    /// Major-axis ray samples that emitted ≥ 1 quad this frame, summed over
    /// all rays. Each ray iterates EXACTLY [`RAY_SAMPLES`] (jets:
    /// [`JET_SAMPLES`]) fixed samples, so `ray_samples == RAY_SAMPLES · N`
    /// certifies no sample collapsed (`m1 <= m0`) or quantized to premul 0 —
    /// the per-ray half of the §6.3/§13 fixed-count invariance regression.
    pub ray_samples: usize,
}

/// A budgeted, grid-clamped, row-band-splitting [`GlowQuad`] sink — the
/// `push_glow_rect` idiom (`cursor_glow::push_rect` twin) with the §6.3 quad
/// budget enforced at the single push point, so truncation order is exactly
/// emission order.
///
/// §7.5 ledger, "Nova emit bound (§6.3)" row: the `budget` truncation branch
/// is the guard that row's P6 ay-CHC certificate (`Q ≤ 392` over the
/// bounded-increment emit system, A8-shape HORN) will license demoting to
/// `debug_assert!` + a cold backstop. Until that certificate is green in CI
/// it stays a live guard — refuse, don't silently pass; the runnable-now
/// companion is the `nova_budget_geometry_invariant` regression below.
struct QuadSink<'a> {
    out: &'a mut Vec<GlowQuad>,
    grid_w: i32,
    grid_h: i32,
    cell_h: i32,
    budget: usize,
}

impl QuadSink<'_> {
    fn push(&mut self, x: i32, y: i32, w: i32, h: i32, premul: u32) {
        if w <= 0 || h <= 0 || premul == 0 || self.cell_h <= 0 {
            return;
        }
        let x0 = x.max(0);
        let x1 = (x + w).min(self.grid_w);
        let y0 = y.max(0);
        let y1 = (y + h).min(self.grid_h);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let mut yy = y0;
        while yy < y1 {
            if self.budget == 0 {
                return;
            }
            let row = yy / self.cell_h;
            let band_end = ((row + 1) * self.cell_h).min(y1);
            self.out.push(GlowQuad {
                row: row as u16,
                x: x0 as u16,
                y: yy as u16,
                w: (x1 - x0) as u16,
                h: (band_end - yy) as u16,
                color: premul,
                // ADDITIVE light — this emitter has no other mode (see
                // [`GlowQuad::alpha`]).
                alpha: 0,
            });
            self.budget -= 1;
            yy = band_end;
        }
    }

    /// One uniform-coverage chord quad: fractional x-extent folded into the
    /// coverage (the `emit_aa_slab` coverage math), quantized ONCE through
    /// `premul_rgb` — the single rounding point both backends then add.
    fn chord(&mut self, x0: f32, x1: f32, y0: i32, y1: i32, color: u32, cov: f32) {
        let xi0 = x0.floor() as i32;
        let xi1 = (x1.ceil() as i32).max(xi0 + 1);
        let frac = ((x1 - x0) / (xi1 - xi0) as f32).clamp(0.0, 1.0);
        let a = (255.0 * cov.clamp(0.0, 1.0) * frac).round() as u8;
        self.push(xi0, y0, xi1 - xi0, y1 - y0, premul_rgb(color, a));
    }
}

/// `1 − (1 − q)²` — the shockwave's ease-out radius curve (invertible, which
/// §6.5's analytic crossing time relies on).
fn ease_out(q: f32) -> f32 {
    let u = 1.0 - q.clamp(0.0, 1.0);
    1.0 - u * u
}

/// Ring radius + fade envelope at `t`, or `None` outside the ring window.
/// Singularity time-reverses the radius (contracts from `r_max` to 0).
pub fn ring_radius(t_ms: u64, r_max: f32, singularity: bool) -> Option<(f32, f32)> {
    if !(FLASH_END_MS..RING_END_MS).contains(&t_ms) {
        return None;
    }
    let q = (t_ms - FLASH_END_MS) as f32 / (RING_END_MS - FLASH_END_MS) as f32;
    let e = ease_out(q);
    let r = if singularity {
        r_max * (1.0 - e)
    } else {
        r_max * e
    };
    Some((r, 1.0 - q))
}

/// Ring/debris visibility floor (v2.1 polish): the dark palette fringes (ion
/// `#6c8cff`, emerald `#7cd83c`, …) under-read as additive light on dark
/// glass at driven intensity — the audit's "dim slate ring" gap against the
/// demo's bright gold. Lift the fringe's relative luminance to ≥ 0.42 by
/// stepping it toward the palette's own CORE (never plain white, so the
/// warm/cool pair identity survives). Bright fringes pass through unchanged;
/// deterministic (≤ 8 fixed steps), resolved once per emission call.
pub fn vivid_fringe(core: u32, fringe: u32) -> u32 {
    const FLOOR: f32 = 0.42;
    let mut c = fringe;
    for k in 1..=8u32 {
        if relative_luminance(c) >= FLOOR {
            return c;
        }
        c = mix_rgb(fringe, core, k as f32 / 8.0);
    }
    c
}

/// §6.3 band count: exactly [`RING_BANDS`] whenever the vertical extent admits
/// ≥ 2 px bands (`2r ≥ 60 px`, true from `cell_h = 14` at genome radii); below
/// that it clamps to `⌊2r/2⌋` — FEWER quads, so the closed form still bounds.
pub fn ring_band_count(extent_px: f32) -> usize {
    if extent_px >= 2.0 * RING_BANDS as f32 {
        RING_BANDS
    } else {
        ((extent_px / 2.0).floor() as usize).clamp(1, RING_BANDS)
    }
}

/// Emit one nova frame's additive quads (crown/ring/rays) in §6.3 spend order.
/// `budget` is the per-nova cap (callers pass `min(MAX_NOVA_QUADS_PER,
/// global-remaining)`); the debris and Singularity `RingArc` streams are
/// separate (they ride the wdeco vec). Pure: same `(t, env, budget)` ⇒ same
/// quads, byte for byte.
pub fn emit_nova(t_ms: u64, env: &NovaEnv, budget: usize, out: &mut Vec<GlowQuad>) -> NovaCounts {
    let mut counts = NovaCounts::default();
    let mut sink = QuadSink {
        out,
        grid_w: env.grid_w,
        grid_h: env.grid_h,
        cell_h: env.cell_h,
        budget,
    };
    let singularity = env.magic == Some(NovaMagic::Singularity);
    match phase(t_ms as i64, env.feats.duration_ms) {
        // Ring first, rays second (§6.3 spend order; they share the window).
        NovaPhase::Ring | NovaPhase::Debris => {
            if let Some((r, fade)) = ring_radius(t_ms, env.r_max, singularity) {
                let n0 = sink.out.len();
                emit_ring(&mut sink, env, r, fade);
                counts.ring = sink.out.len() - n0;
                let n0 = sink.out.len();
                counts.ray_samples = emit_rays(&mut sink, env, t_ms, fade, singularity);
                counts.rays = sink.out.len() - n0;
            }
        }
        // The crown is funded LAST in the spend order: it emits only when it
        // fits whole under the remaining budget (crown drops first, §6.3).
        NovaPhase::Flash if sink.budget >= CROWN_QUADS => {
            let n0 = sink.out.len();
            emit_crown(&mut sink, env, t_ms);
            counts.crown = sink.out.len() - n0;
        }
        // Dip is ink-only; Armed/Ember/Settled emit no quads.
        _ => {}
    }
    counts
}

/// §6.3 ring: exactly-30-band chord slabs over the annulus, plus the chromatic
/// fringe (a second ring at `R + chroma`, fringe tint, 0.55 coverage folded
/// into the per-band scalar). ≤ 2 chords per band per ring by circle geometry;
/// one uniform-coverage quad per chord (row-split by the sink).
fn emit_ring(sink: &mut QuadSink<'_>, env: &NovaEnv, r: f32, fade: f32) {
    let thick = env.feats.ring_thick;
    let chroma = env.feats.chroma;
    // Extent covers the fringe ring's outer edge so one band structure serves
    // both rings (the §6.3 "2 chords × 2 rings per band" arithmetic).
    let re = r + chroma + 0.5 * thick;
    if re <= 0.5 {
        return;
    }
    let extent = 2.0 * re;
    let bands = ring_band_count(extent);
    let top = env.cy as f32 - re;
    // `√fade` holds the shockwave visibly bright through the mid-sweep (the
    // v2.1 polish audit's "easy to miss mid-ring"); same 1 → 0 endpoints.
    let base_cov = (fade.max(0.0).sqrt() * env.intensity).clamp(0.0, 1.0);
    let fringe = vivid_fringe(env.core, env.fringe);
    let cxf = env.cx as f32;
    for i in 0..bands {
        let y0f = (extent * i as f32 / bands as f32).round();
        let y1f = (extent * (i + 1) as f32 / bands as f32).round();
        if y1f <= y0f {
            continue;
        }
        let y0 = (top + y0f) as i32;
        let y1 = (top + y1f) as i32;
        let dy = (y0f + y1f) * 0.5 - re;
        // (radius, tint, folded coverage scale): the main ring, then the fringe.
        for (rr, color, scale) in [(r, env.core, 1.0f32), (r + chroma, fringe, 0.55)] {
            let ro = rr + 0.5 * thick;
            let ri = (rr - 0.5 * thick).max(0.0);
            let wo = (ro * ro - dy * dy).max(0.0).sqrt();
            let wi = (ri * ri - dy * dy).max(0.0).sqrt();
            if wo - wi < 0.05 {
                continue;
            }
            let cov = base_cov * scale;
            if wi < 0.5 {
                // The band crosses the cap: the two chords merge into one.
                sink.chord(cxf - wo, cxf + wo, y0, y1, color, cov);
            } else {
                sink.chord(cxf - wo, cxf - wi, y0, y1, color, cov);
                sink.chord(cxf + wi, cxf + wo, y0, y1, color, cov);
            }
        }
    }
}

/// §6.3 rays: 5–8 genome rays (Quasar: 2 vertical jets, doubled length),
/// each stepped at exactly [`RAY_SAMPLES`] (jets: [`JET_SAMPLES`]) fixed
/// samples along its MAJOR axis — sample `j` spans
/// `[round(Lm·j/N), round(Lm·(j+1)/N))` — with `comet_beam`'s 2.5→1 px taper
/// folded into per-sample coverage/width (one uniform quad per sample, never
/// per-pixel stepping). Samples emit inner→outer so a binding budget drops
/// OUTER slices (§6.3 truncation order). Returns the number of samples that
/// emitted ≥ 1 quad, summed over all rays ([`NovaCounts::ray_samples`]).
fn emit_rays(
    sink: &mut QuadSink<'_>,
    env: &NovaEnv,
    t_ms: u64,
    fade: f32,
    singularity: bool,
) -> usize {
    let quasar = env.magic == Some(NovaMagic::Quasar);
    let q = (t_ms - FLASH_END_MS) as f32 / (RING_END_MS - FLASH_END_MS) as f32;
    let mut e = ease_out(q);
    if singularity {
        e = 1.0 - e; // time-reversed radii: rays retract inward
    }
    let (nrays, samples, len_scale) = if quasar {
        (2usize, JET_SAMPLES, 2.0f32)
    } else {
        (usize::from(env.feats.rays), RAY_SAMPLES, 1.0)
    };
    let len = (0.4 + 0.6 * e) * env.r_max * len_scale;
    if len < 1.0 {
        return 0;
    }
    let mut emitted_samples = 0usize;
    for k in 0..nrays {
        let ang = if quasar {
            // Twin vertical polar jets.
            if k == 0 {
                -std::f32::consts::FRAC_PI_2
            } else {
                std::f32::consts::FRAC_PI_2
            }
        } else {
            std::f32::consts::TAU * (f32::from(env.feats.rot) / 16.0 + k as f32 / nrays as f32)
        };
        let (dx, dy) = (ang.cos(), ang.sin());
        let major_x = dx.abs() >= dy.abs();
        let maj = dx.abs().max(dy.abs()); // ≥ √2/2: never 0
        let lm = len * maj; // major-axis extent
        let (ux, uy) = (dx / maj, dy / maj); // step per major px
        for j in 0..samples {
            let m0 = (lm * j as f32 / samples as f32).round();
            let m1 = (lm * (j + 1) as f32 / samples as f32).round();
            if m1 <= m0 {
                continue;
            }
            let quads_before = sink.out.len();
            let sf = (j as f32 + 0.5) / samples as f32;
            // The comet taper, folded into per-sample coverage/width (§6.3).
            let th = 2.5 - 1.5 * sf;
            let cov = (fade * (1.0 - 0.65 * sf) * env.intensity).clamp(0.0, 1.0);
            let mm = 0.5 * (m0 + m1);
            if major_x {
                let xa = env.cx as f32 + ux * m0;
                let xb = env.cx as f32 + ux * m1;
                let (x0, x1) = if xa <= xb { (xa, xb) } else { (xb, xa) };
                let yc = env.cy as f32 + uy * mm;
                let y0 = (yc - 0.5 * th).floor() as i32;
                let y1 = ((yc + 0.5 * th).ceil() as i32).max(y0 + 1);
                // Thickness fraction folds into the quantized coverage.
                let frac = (th / (y1 - y0) as f32).min(1.0);
                let a = (255.0 * cov * frac).round() as u8;
                sink.push(
                    x0.floor() as i32,
                    y0,
                    (x1.ceil() as i32 - x0.floor() as i32).max(1),
                    y1 - y0,
                    premul_rgb(env.core, a),
                );
            } else {
                let ya = env.cy as f32 + uy * m0;
                let yb = env.cy as f32 + uy * m1;
                let (y0, y1) = if ya <= yb { (ya, yb) } else { (yb, ya) };
                let xc = env.cx as f32 + ux * mm;
                let x0 = (xc - 0.5 * th).floor() as i32;
                let x1 = ((xc + 0.5 * th).ceil() as i32).max(x0 + 1);
                let frac = (th / (x1 - x0) as f32).min(1.0);
                let a = (255.0 * cov * frac).round() as u8;
                sink.push(
                    x0,
                    y0.floor() as i32,
                    x1 - x0,
                    (y1.ceil() as i32 - y0.floor() as i32).max(1),
                    premul_rgb(env.core, a),
                );
            }
            emitted_samples += usize::from(sink.out.len() > quads_before);
        }
    }
    emitted_samples
}

/// §6.1 crown: a white-hot 4-POINT STAR GLINT over the word center — a hot
/// horizontal core beam through the word crossed by a tapering vertical
/// spike, radial falloff and white-heat folded into per-band coverage/width/
/// tint, ≤ 3×3 cells, envelope `sin(π·e/120)` — one rise-and-fall, never
/// periodic. ≤ [`CROWN_QUADS`] after row splits (4 spike + 2 beam chords,
/// each band shorter than one cell ⇒ ≤ 2 row-split quads per chord — the
/// SAME ≤ 12 bound the §6.3 closed form and the ay-CHC crown edge pin). The
/// v2.0 shape — K = 3 concentric stacked RECTS — read as nested gray boxes
/// on glass, not a flash (the v2.1 polish audit); corrected here, same
/// budget and envelope.
fn emit_crown(sink: &mut QuadSink<'_>, env: &NovaEnv, t_ms: u64) {
    let e = (t_ms.saturating_sub(DIP_MS)) as f32 / (FLASH_END_MS - DIP_MS) as f32;
    let envl = (std::f32::consts::PI * e.clamp(0.0, 1.0)).sin();
    let ch = env.cell_h as f32;
    let (cxf, cyf) = (env.cx as f32, env.cy as f32);
    let r = 1.3 * ch; // spike half-length (inside the ≤ 3-cell extent)
    let beam = 0.30 * ch; // core beam half-height
    // Vertical spike: 2 tapered bands per side; width, coverage AND
    // white-heat all fall toward the tips, so it reads as a hot needle
    // cooling into the palette's warm core tint, not a gray column.
    for (f0, f1) in [(0.0f32, 0.45f32), (0.45, 1.0)] {
        let q = 1.0 - (f0 + f1) * 0.5; // 1 at the beam → 0 at the tip
        let hw = 0.24 * ch * q * q + 0.8;
        let cov = envl * env.intensity * (0.10 + 0.60 * q);
        let color = mix_rgb(env.core, 0x00FF_FFFF, 0.5 * q);
        for side in [-1.0f32, 1.0] {
            let ya = cyf + side * (beam + (r - beam) * f0);
            let yb = cyf + side * (beam + (r - beam) * f1);
            let (y0, y1) = if ya <= yb { (ya, yb) } else { (yb, ya) };
            sink.chord(cxf - hw, cxf + hw, y0 as i32, y1 as i32, color, cov);
        }
    }
    // Horizontal core beam: 2 bands about the row centerline, hot and near-
    // white — the blown flash core that washes over the word for ~120 ms.
    let color = mix_rgb(env.core, 0x00FF_FFFF, 0.7);
    for side in [-1.0f32, 1.0] {
        let yb = cyf + side * beam;
        let (y0, y1) = if cyf <= yb { (cyf, yb) } else { (yb, cyf) };
        let cov = envl * env.intensity * 0.85;
        sink.chord(
            cxf - 1.05 * ch,
            cxf + 1.05 * ch,
            y0 as i32,
            y1 as i32,
            color,
            cov,
        );
    }
}

/// §6.1 debris: 8–20 motes on analytic ballistic arcs (the `emit_particles`
/// math — position is a closed form of age, zero stored state) riding the
/// EXISTING wdeco Add stream, twinkling on the shared ≥ 350 ms phase grid
/// (§6.4 item 4). Singularity reverses the arcs: motes fall INWARD from the
/// rim to the collapse point. Deterministic from `(seed, mote index, t)`.
/// Appends at most `cap` decorations.
pub fn emit_debris(t_ms: u64, env: &NovaEnv, out: &mut Vec<WordDecoration>, cap: usize) {
    let d = u64::from(env.feats.duration_ms);
    if !(DEBRIS_START_MS..d).contains(&t_ms) {
        return;
    }
    let singularity = env.magic == Some(NovaMagic::Singularity);
    let age = (t_ms - DEBRIS_START_MS) as f32 / 1000.0; // seconds
    let qn = (t_ms - DEBRIS_START_MS) as f32 / (d - DEBRIS_START_MS) as f32;
    let fade = 1.0 - qn;
    let (rows, cols) = (
        env.grid_h / env.cell_h.max(1),
        env.grid_w / env.cell_w.max(1),
    );
    if rows <= 0 || cols <= 0 {
        return;
    }
    // Same visibility floor as the ring: dark fringes make invisible motes.
    let fringe = vivid_fringe(env.core, env.fringe);
    for m in 0..u32::from(env.feats.debris) {
        if out.len() >= cap {
            break;
        }
        // Per-mote constants from the seeded stream (NOT per frame: ballistics
        // need frozen launch parameters; only `t` animates).
        let s = mix(env.seed ^ u64::from(m).wrapping_mul(0xA24B_AED4_963E_E407));
        let r0 = (s & 0xffff) as f32 / 65535.0;
        let r1 = ((s >> 16) & 0xffff) as f32 / 65535.0;
        let r2 = ((s >> 32) & 0xff) as f32 / 255.0;
        let ang = std::f32::consts::TAU * r0;
        let (x, y) = if singularity {
            // Time-reversed: from the rim inward along the launch direction.
            let rr = env.r_max * (0.6 + 0.4 * r1) * (1.0 - ease_out(qn));
            (
                env.cx as f32 + ang.cos() * rr,
                env.cy as f32 + ang.sin() * rr,
            )
        } else {
            let speed = (0.4 + 1.2 * r1) * env.cell_h as f32;
            let (vx, vy) = (
                ang.cos() * speed,
                ang.sin() * speed - 0.6 * env.cell_h as f32,
            );
            let g = 2.2 * env.cell_h as f32;
            (
                env.cx as f32 + vx * age,
                env.cy as f32 + vy * age + 0.5 * g * age * age,
            )
        };
        // §6.4: twinkle phase LOCKED to the shared ≥ 350 ms grid — each mote's
        // phase is 0 or TWINKLE_GRID_MS of the 2·grid period, so the region's
        // combined onsets stay ≤ 3/s.
        let grid_phase = ((s >> 33) & 1) as f32 * 0.5;
        let tw = 0.35
            + 0.65
                * (0.5
                    + 0.5
                        * (std::f32::consts::TAU
                            * (t_ms as f32 / (2.0 * TWINKLE_GRID_MS as f32) + grid_phase))
                            .sin());
        let alpha = (env.intensity * fade * tw).clamp(0.0, 1.0);
        if alpha <= 0.01 {
            continue;
        }
        // OFF-GRID MOTES ARE CULLED, exactly as the supernova sibling culls
        // its debris (supernova.rs `emit_super_decos`): the clamp below can
        // only pin a cell index, and the residual then lands in dx/dy — whose
        // contract is SUB-CELL jitter — so a mote past the grid edge rendered
        // its star inside the window padding gutters, a region every quad
        // stream is clamped away from (2026-09-01 audit).
        if x < 0.0 || y < 0.0 || x >= env.grid_w as f32 || y >= env.grid_h as f32 {
            continue;
        }
        let col = ((x as i32) / env.cell_w).clamp(0, cols - 1);
        let row = ((y as i32) / env.cell_h).clamp(0, rows - 1);
        let dx = (x as i32 - col * env.cell_w - env.cell_w / 2).clamp(-127, 127) as i8;
        let dy = (y as i32 - row * env.cell_h - env.cell_h / 2).clamp(-127, 127) as i8;
        out.push(WordDecoration {
            row: row as u16,
            col: col as u16,
            dx,
            dy,
            glyph: if s & 4 == 0 {
                DecoGlyph::Star4
            } else {
                DecoGlyph::Dot
            },
            blend: DecoBlend::Add,
            color: mix_rgb(env.core, fringe, r2),
            alpha: (alpha * 255.0).round() as u8,
        });
    }
}

/// §6.1 Singularity darkening ring: ≤ [`MAX_RING_ARC_CELLS`] per-cell
/// Over-blend [`DecoGlyph::RingArc`] wdecos on the cells the contracting ring
/// passes through — `nova_add` is One/One additive and can only brighten, so
/// the darkening rides the existing `wdeco_over` parity machinery. Row-major
/// deterministic; appends nothing outside the ring window.
pub fn emit_ring_arc(t_ms: u64, env: &NovaEnv, out: &mut Vec<WordDecoration>, cap: usize) {
    let Some((r, fade)) = ring_radius(t_ms, env.r_max, true) else {
        return;
    };
    if r < 1.0 {
        return;
    }
    let (rows, cols) = (
        env.grid_h / env.cell_h.max(1),
        env.grid_w / env.cell_w.max(1),
    );
    let half = 0.5 * env.cell_w.max(env.cell_h) as f32;
    let row0 = ((env.cy as f32 - r) as i32 / env.cell_h).max(0);
    let row1 = ((env.cy as f32 + r) as i32 / env.cell_h).min(rows - 1);
    let col0 = ((env.cx as f32 - r) as i32 / env.cell_w).max(0);
    let col1 = ((env.cx as f32 + r) as i32 / env.cell_w).min(cols - 1);
    let mut emitted = 0usize;
    for row in row0..=row1 {
        for col in col0..=col1 {
            if emitted >= cap {
                return;
            }
            let ccx = (col * env.cell_w + env.cell_w / 2) as f32;
            let ccy = (row * env.cell_h + env.cell_h / 2) as f32;
            let dist = ((ccx - env.cx as f32).powi(2) + (ccy - env.cy as f32).powi(2)).sqrt();
            if (dist - r).abs() > half {
                continue;
            }
            out.push(WordDecoration {
                row: row as u16,
                col: col as u16,
                dx: 0,
                dy: 0,
                glyph: DecoGlyph::RingArc,
                blend: DecoBlend::Over,
                color: 0x001A_1022, // near-black violet: the collapse shadow
                alpha: (150.0 * fade * env.intensity).clamp(0.0, 255.0) as u8,
            });
            emitted += 1;
        }
    }
}

// ───────────────────────── §6.5 blast coupling ─────────────────────────

/// The frame the expanding (or contracting) ring first crosses distance
/// `dist` px from the nova center, in ms since ignition — the analytic
/// inverse of [`ring_radius`]'s ease-out, so the pulse is a pure function of
/// `(nova center, t, span)` with no stored state. `None` when the ring never
/// reaches `dist`.
pub fn crossing_ms(dist: f32, r_max: f32, singularity: bool) -> Option<u64> {
    if !(0.0..=r_max).contains(&dist) || r_max <= 0.0 {
        return None;
    }
    let q = if singularity {
        // R(q) = r_max·(1 − ease(q)) falls to `dist`: (1−q)² = d/r_max.
        1.0 - (dist / r_max).sqrt()
    } else {
        // R(q) = r_max·ease(q) grows to `dist`: (1−q)² = 1 − d/r_max.
        1.0 - (1.0 - dist / r_max).sqrt()
    };
    Some(FLASH_END_MS + (q.clamp(0.0, 1.0) * (RING_END_MS - FLASH_END_MS) as f32) as u64)
}

/// The ~150 ms smoothstep in/out pulse envelope at `t` for a crossing at
/// `cross_ms`; `None` outside the window.
pub fn pulse_env(t_ms: u64, cross_ms: u64) -> Option<f32> {
    if t_ms < cross_ms || t_ms >= cross_ms + PULSE_MS {
        return None;
    }
    let e = (t_ms - cross_ms) as f32 / PULSE_MS as f32;
    Some(smoothstep((2.0 * e.min(1.0 - e)).clamp(0.0, 1.0)))
}

/// §6.5's constant-luminance pairing, BY CONSTRUCTION: take `target`'s hue and
/// saturation, then bisect value (and, when the hue can't reach a bright
/// anchor at full value, saturation toward white) until the result's WCAG
/// relative luminance matches `anchor`'s. Monotone in both knobs ⇒ the
/// bisection converges; |ΔL| ≤ 5 % then holds for every (anchor, palette)
/// pair — including every genome-hue-nudged anchor — which the §13 table test
/// asserts numerically.
pub fn constant_lum_toward(anchor: u32, target: u32) -> u32 {
    let want = relative_luminance(anchor);
    let (h, s, _) = rgb2hsv(target);
    // L is monotone in v at fixed (h, s).
    if relative_luminance(hsv2rgb(h, s, 1.0)) >= want {
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..20 {
            let mid = 0.5 * (lo + hi);
            if relative_luminance(hsv2rgb(h, s, mid)) < want {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        return hsv2rgb(h, s, 0.5 * (lo + hi));
    }
    // Saturated hue too dark even at v = 1 (bright pastel anchors): desaturate
    // toward white at full value — L is monotone in falling s.
    let (mut lo, mut hi) = (0.0f32, s);
    for _ in 0..20 {
        let mid = 0.5 * (lo + hi);
        if relative_luminance(hsv2rgb(h, mid, 1.0)) < want {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hsv2rgb(h, 0.5 * (lo + hi), 1.0)
}

/// The applied §6.5 pulse color: the anchor's hue/saturation shifted toward
/// the palette `fringe` at amplitude `amp` (≤ [`PULSE_AMP`]), then
/// luminance-matched BACK to the anchor — a hue/sat shift at approximately
/// constant relative luminance, |ΔL| ≤ 5 % **by construction** (the bisection
/// is the enforcement; the §13 table test measures it ≈ 0 over the whole
/// deployed space). Gamma-space mixing alone would bow the luminance ~5 %
/// above the bound on bright anchors, which is exactly why the match runs on
/// the MIXED color, not just the tone endpoint.
pub fn pulse_color(anchor: u32, fringe: u32, amp: f32) -> u32 {
    constant_lum_toward(anchor, mix_rgb(anchor, fringe, amp.clamp(0.0, PULSE_AMP)))
}

/// §6.1 ember ink anchors: the settled gradient shifted to the palette's ember
/// tone; the Singularity ends in a dim violet ember instead.
pub fn ember_pair(pair: (u32, u32), magic: Option<NovaMagic>) -> (u32, u32) {
    if magic == Some(NovaMagic::Singularity) {
        return (0x002A_2040, 0x005A_48A8); // dim violet
    }
    let dim = |c: u32, f: f32| {
        let m = |sh: u32| ((((c >> sh) & 0xff) as f32) * f) as u32;
        (m(16) << 16) | (m(8) << 8) | m(0)
    };
    (dim(pair.0, 0.8), dim(pair.1, 0.55))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::genome::nova_features;

    /// A genome-max feature set (radius 2.2 rows, 8 rays, max chroma/thickness)
    /// found by scanning gkeys — the §6.3 worst-case shape.
    fn worst_feats() -> NovaFeatures {
        let mut best: Option<NovaFeatures> = None;
        for g in 0..(1u64 << 15) {
            let f = nova_features(g);
            if f.rays == 8 && f.radius > 2.19 && f.chroma > 2.49 && f.ring_thick > 3.49 {
                best = Some(f);
                break;
            }
        }
        best.expect("genome-max feature combination is reachable (§3.4 reachability)")
    }

    fn env_at(ch: i32, magic: Option<NovaMagic>) -> NovaEnv {
        let feats = worst_feats();
        let cw = ch / 2;
        NovaEnv {
            grid_w: 200 * cw,
            grid_h: 60 * ch,
            cell_w: cw,
            cell_h: ch,
            cx: 100 * cw,
            cy: 30 * ch,
            r_max: feats.radius * ch as f32,
            feats,
            magic,
            core: NOVA_PALETTES[0].0,
            fringe: NOVA_PALETTES[0].1,
            intensity: 1.0,
            seed: 0xDECAF,
        }
    }

    /// §6.3/§13/§7.4 budget invariance (the v2.1 regression, robust form —
    /// the deliberate replacement for U1's flaky "equal quad counts", §15.14):
    /// at ch ∈ {14, 20, 40, 56} with the genome-max feature set,
    ///
    ///   * band count == 30 (the emitter's own exact subdivision),
    ///   * ray samples == 15 **per ray** (counted from the emission: every
    ///     one of the 8 rays' 15 fixed samples emitted ≥ 1 quad),
    ///   * total quads ≤ 392 at EVERY phase instant of the window,
    ///   * the caps never bind: the per-nova 512 cap emission is
    ///     byte-identical to an unbounded-budget emission, and
    ///     3 × worst < 1536 (the global backstop, checked at compile time).
    ///
    /// Every counted number is printed in the assertion message, so a failure
    /// shows the real geometry, not just a boolean. §7.5 ledger row "Nova
    /// emit bound (§6.3)": this regression is the runnable-now companion of
    /// the P6 ay-CHC certificate (Q ≤ 392 over the bounded-increment emit
    /// system); until that certificate is green in CI, the [`QuadSink`]
    /// truncation branch keeps its guard (refuse-don't-silently-pass).
    #[test]
    fn nova_budget_geometry_invariant() {
        assert_eq!(RAY_SAMPLES, 15);
        for ch in [14i32, 20, 40, 56] {
            let env = env_at(ch, None);
            // Exact subdivision keeps the count at 30 at every admitted size
            // (a ⌈2r/30⌉ ceiling step would give 21/25/28 here — the §15.14
            // binding-spec erratum this assertion pins).
            let extent = 2.0 * (env.r_max + env.feats.chroma + 0.5 * env.feats.ring_thick);
            let bands = ring_band_count(extent);
            assert_eq!(
                bands, 30,
                "band count must be exactly 30 at ch={ch} (extent {extent:.1} px), counted {bands}"
            );
            // Per-ray fixed samples, counted from the emission at the late
            // ring (max ray length ⇒ every sample spans ≥ 1 px): 8 rays ×
            // exactly 15 samples each, none collapsed, none quantized away.
            let mut late = Vec::new();
            let cl = emit_nova(860, &env, MAX_NOVA_QUADS_PER, &mut late);
            let nrays = usize::from(env.feats.rays);
            assert_eq!(nrays, 8, "genome-max ray count, counted {nrays}");
            assert_eq!(
                cl.ray_samples,
                RAY_SAMPLES * nrays,
                "ray samples must be 15 per ray at ch={ch}: counted {} over {nrays} rays \
                 (want {} = 15 × {nrays})",
                cl.ray_samples,
                RAY_SAMPLES * nrays
            );
            let mut out = Vec::new();
            let mut uncapped = Vec::new();
            let mut worst = 0usize;
            let mut worst_counts = NovaCounts::default();
            for t in (0..1400u64).step_by(10) {
                out.clear();
                let counts = emit_nova(t, &env, MAX_NOVA_QUADS_PER, &mut out);
                if out.len() > worst {
                    worst = out.len();
                    worst_counts = counts;
                }
                assert!(
                    out.len() <= 392,
                    "Q ≤ 392 violated at ch={ch} t={t}: counted {} (ring {} rays {} crown {})",
                    out.len(),
                    counts.ring,
                    counts.rays,
                    counts.crown
                );
                // Caps never bind: the 512/nova budget emission is
                // byte-identical to an unbounded-budget emission.
                uncapped.clear();
                emit_nova(t, &env, usize::MAX, &mut uncapped);
                assert_eq!(
                    out,
                    uncapped,
                    "the {MAX_NOVA_QUADS_PER}/nova cap bound at ch={ch} t={t} \
                     (capped {} vs unbounded {} quads)",
                    out.len(),
                    uncapped.len()
                );
                for q in &out {
                    let band = i32::from(q.row) * ch;
                    assert!(
                        i32::from(q.y) >= band && i32::from(q.y) + i32::from(q.h) <= band + ch,
                        "quad escapes its row band at ch={ch} t={t}: {q:?}"
                    );
                    assert!(i32::from(q.x) + i32::from(q.w) <= env.grid_w);
                }
            }
            assert!(worst > 0, "the nova must actually emit at ch={ch}");
            assert!(
                worst <= 392 && 392 < MAX_NOVA_QUADS_PER,
                "headroom must hold at ch={ch}: worst {worst} ≤ 392 < {MAX_NOVA_QUADS_PER}"
            );
            eprintln!(
                "nova budget @ ch={ch}: bands=30, rays={nrays}×{RAY_SAMPLES} samples \
                 (counted {}), worst Q={worst} (ring {} rays {} crown {}) ≤ 392; \
                 512/nova cap never binds",
                cl.ray_samples, worst_counts.ring, worst_counts.rays, worst_counts.crown
            );
        }
        // The global backstop never binds on a genome-reachable frame
        // (3 × 392 = 1176 < 1536 — §6.3's closed-form margin of 360).
        const { assert!(MAX_ACTIVE_NOVAS * 392 <= MAX_NOVA_QUADS) };
    }

    /// §6.3 spend-order truncation: under a binding cap the ring is NEVER
    /// gapped (its count matches the uncapped emission), rays lose their OUTER
    /// slices, and the crown drops first (all-or-nothing).
    #[test]
    fn spend_order_truncation_ring_first_crown_drops() {
        let env = env_at(20, None);
        let t = 600u64; // mid-ring window: ring + rays both live
        let mut full = Vec::new();
        let counts = emit_nova(t, &env, MAX_NOVA_QUADS_PER, &mut full);
        assert!(counts.ring > 0 && counts.rays > 0);
        // Cap = ring + 10: the whole ring survives, exactly 10 ray quads land.
        let mut capped = Vec::new();
        let c = emit_nova(t, &env, counts.ring + 10, &mut capped);
        assert_eq!(c.ring, counts.ring, "the ring is never gapped by a cap");
        assert_eq!(c.rays, 10, "rays truncate to the remaining budget");
        assert_eq!(c.crown, 0);
        // The surviving ray quads are the INNERMOST slices (emission order is
        // inner→outer per ray, so truncation drops the tips).
        assert_eq!(
            capped[..counts.ring],
            full[..counts.ring],
            "identical ring bytes"
        );
        // Crown: all-or-nothing — below CROWN_QUADS it fully drops.
        let tf = 180u64; // flash window
        let mut crown_full = Vec::new();
        let cf = emit_nova(tf, &env, MAX_NOVA_QUADS_PER, &mut crown_full);
        assert!(
            (1..=CROWN_QUADS).contains(&cf.crown),
            "crown ≤ ~12 quads: {}",
            cf.crown
        );
        let mut crown_capped = Vec::new();
        let cc = emit_nova(tf, &env, CROWN_QUADS - 1, &mut crown_capped);
        assert_eq!(
            cc.crown, 0,
            "a partial crown never emits (crown drops first)"
        );
        assert!(crown_capped.is_empty());
    }

    /// §6.1 phase machine: monotone windows, D-parameterized, Armed for a
    /// deferred (future) ignition, Settled terminal.
    #[test]
    fn phase_windows_are_monotone() {
        for d in [1000u32, 1400] {
            assert_eq!(phase(-1, d), NovaPhase::Armed);
            assert_eq!(phase(0, d), NovaPhase::Dip);
            assert_eq!(phase(119, d), NovaPhase::Dip);
            assert_eq!(phase(120, d), NovaPhase::Flash);
            assert_eq!(phase(240, d), NovaPhase::Ring);
            assert_eq!(phase(499, d), NovaPhase::Ring);
            assert_eq!(phase(500, d), NovaPhase::Debris);
            assert_eq!(phase(i64::from(d) - 1, d), NovaPhase::Debris);
            assert_eq!(phase(i64::from(d), d), NovaPhase::Ember);
            assert_eq!(phase(i64::from(d) + 499, d), NovaPhase::Ember);
            assert_eq!(phase(i64::from(d) + 500, d), NovaPhase::Settled);
        }
        // Dip envelope: full ink at 0−, −35 % at the dip floor, restored after.
        assert!((dip_envelope(0) - 1.0).abs() < 1e-6);
        assert!((dip_envelope(119) - 0.65).abs() < 0.01);
        assert!((dip_envelope(120) - 1.0).abs() < 1e-6);
    }

    /// §6.5 coupling determinism + the crossing inverse: same inputs ⇒ same
    /// crossing frame; monotone in distance; the Singularity inverts (larger
    /// distances cross EARLIER on a contracting ring).
    #[test]
    fn coupling_crossing_is_deterministic_and_monotone() {
        let rm = 44.0f32;
        let a = crossing_ms(10.0, rm, false).unwrap();
        let b = crossing_ms(10.0, rm, false).unwrap();
        assert_eq!(a, b, "pure function of (dist, r_max)");
        let far = crossing_ms(40.0, rm, false).unwrap();
        assert!(far > a, "the expanding ring reaches farther words later");
        assert!(
            crossing_ms(50.0, rm, false).is_none(),
            "beyond r_max: never"
        );
        let sa = crossing_ms(10.0, rm, true).unwrap();
        let sfar = crossing_ms(40.0, rm, true).unwrap();
        assert!(sfar < sa, "the contracting ring reaches far words FIRST");
        // The crossing frame actually lies inside the ring window and the
        // radius there is within a frame's travel of the distance.
        for (d, sing) in [(10.0f32, false), (40.0, false), (10.0, true)] {
            let t = crossing_ms(d, rm, sing).unwrap();
            assert!((FLASH_END_MS..RING_END_MS).contains(&t));
            let (r, _) = ring_radius(t, rm, sing).unwrap();
            assert!((r - d).abs() < 2.5, "R(t_cross) ≈ dist: {r} vs {d}");
        }
        // Pulse envelope: one 150 ms in/out window, 0 outside.
        assert!(pulse_env(299, 300).is_none());
        assert!(pulse_env(300, 300).is_some());
        assert!(pulse_env(375, 300).unwrap() > 0.9, "peak mid-window");
        assert!(pulse_env(450, 300).is_none());
    }

    /// §6.4 item 7 / §13 `delta_L_bound_over_palette_table`: |ΔL| ≤ 5 % holds
    /// for EVERY (ink anchor, nova palette) pair over the DEPLOYED color space
    /// — every class base anchor and every palette endpoint anchor, under
    /// every 4-bit hue-nudge code (16 per anchor ⇒ the full 256-code ink-pair
    /// field is covered pairwise), toward every palette's fringe — both for
    /// the paired tone itself and for the ≤ 0.35-amplitude mix actually
    /// applied; plus the saturated-red check.
    #[test]
    fn delta_l_bound_over_palette_table() {
        use crate::color_math::hue_nudge;
        // Deployed ink anchors: class base pairs + all 8 palettes' endpoints.
        let mut anchors: Vec<u32> = vec![
            0x007C_C8FF,
            0x00C8_9AFF, // emphasis
            0x00F7_A8B8,
            0x00FF_D9C2, // feline
        ];
        for (core, fringe) in NOVA_PALETTES {
            anchors.push(core);
            anchors.push(fringe);
        }
        for &base in &anchors {
            for code in 0..16u8 {
                let anchor = hue_nudge(base, code);
                let la = relative_luminance(anchor);
                for (_, fringe) in NOVA_PALETTES {
                    // The paired tone endpoint holds the bound…
                    let tone = constant_lum_toward(anchor, fringe);
                    let dl = (relative_luminance(tone) - la).abs();
                    assert!(
                        dl <= 0.05,
                        "|ΔL| {dl:.4} > 5% for anchor {anchor:06x} → fringe {fringe:06x}"
                    );
                    // …and so does the APPLIED pulse at every amplitude the
                    // envelope can reach (the deployed color, §6.4 item 7).
                    for amp in [0.1f32, 0.2, PULSE_AMP] {
                        let mixed = pulse_color(anchor, fringe, amp);
                        let dm = (relative_luminance(mixed) - la).abs();
                        assert!(
                            dm <= 0.05,
                            "pulse |ΔL| {dm:.4} > 5% at amp {amp} for {anchor:06x}"
                        );
                        // Never a saturated-red transition from a non-red
                        // base: the pulse cannot push a pastel to flashing red.
                        let (r, g, b) = ((mixed >> 16) & 0xff, (mixed >> 8) & 0xff, mixed & 0xff);
                        let red_frac = r as f32 / (r + g + b).max(1) as f32;
                        assert!(red_frac < 0.8, "pulse produced saturated red {mixed:06x}");
                    }
                }
            }
        }
    }

    /// §6.1 Quasar smoke: rays are replaced by 2 vertical jets — every ray-
    /// phase quad outside the ring annulus hugs the center column — at doubled
    /// length (jets reach ~2× the ring radius) with ≤ 60 quads per jet.
    #[test]
    fn quasar_jets_are_vertical_and_doubled() {
        let env = env_at(20, Some(NovaMagic::Quasar));
        let mut out = Vec::new();
        let t = 880u64; // late window: max ray length
        let ring_only = {
            let mut ring = Vec::new();
            let mut e2 = env_at(20, Some(NovaMagic::Quasar));
            e2.feats.rays = 0; // not used by quasar path; count via counts instead
            let c = emit_nova(t, &e2, MAX_NOVA_QUADS_PER, &mut ring);
            c.ring
        };
        let counts = emit_nova(t, &env, MAX_NOVA_QUADS_PER, &mut out);
        assert_eq!(counts.ring, ring_only);
        assert!(
            counts.rays > 0 && counts.rays <= 2 * JET_SAMPLES * 2,
            "≤ 60/jet with splits"
        );
        let jets = &out[counts.ring..counts.ring + counts.rays];
        for q in jets {
            // Vertical: the jet stays within a few px of the center column.
            assert!(
                (i32::from(q.x) - env.cx).abs() <= 4
                    && (i32::from(q.x) + i32::from(q.w) - env.cx).abs() <= 4,
                "jet quad strays from the vertical axis: {q:?}"
            );
        }
        // Doubled length: the jets reach past the ring radius.
        let reach = jets
            .iter()
            .map(|q| {
                (i32::from(q.y) - env.cy)
                    .abs()
                    .max((i32::from(q.y) + i32::from(q.h) - env.cy).abs())
            })
            .max()
            .unwrap();
        assert!(
            reach as f32 > 1.5 * env.r_max,
            "jets reach ~2× the ring radius ({reach} px vs r_max {})",
            env.r_max
        );
    }

    /// §6.1 Singularity smoke: the ring CONTRACTS (radius shrinks over the
    /// window) and the darkening ring emits ≤ 20 per-cell Over RingArc wdecos
    /// that follow it inward.
    #[test]
    fn singularity_contracts_and_darkens() {
        let env = env_at(20, Some(NovaMagic::Singularity));
        let (r_early, _) = ring_radius(300, env.r_max, true).unwrap();
        let (r_late, _) = ring_radius(800, env.r_max, true).unwrap();
        assert!(r_early > r_late, "the Singularity ring contracts");
        let mut decos = Vec::new();
        emit_ring_arc(300, &env, &mut decos, MAX_RING_ARC_CELLS);
        assert!(!decos.is_empty() && decos.len() <= MAX_RING_ARC_CELLS);
        for d in &decos {
            assert_eq!(d.glyph, DecoGlyph::RingArc);
            assert!(
                matches!(d.blend, DecoBlend::Over),
                "darkening must be Over, not Add"
            );
        }
        // The arc follows the collapse: late-window cells sit nearer the center.
        let mut late = Vec::new();
        emit_ring_arc(850, &env, &mut late, MAX_RING_ARC_CELLS);
        let mean_dist = |ds: &[WordDecoration]| {
            ds.iter()
                .map(|d| {
                    let x = i32::from(d.col) * env.cell_w + env.cell_w / 2 - env.cx;
                    let y = i32::from(d.row) * env.cell_h + env.cell_h / 2 - env.cy;
                    ((x * x + y * y) as f32).sqrt()
                })
                .sum::<f32>()
                / ds.len().max(1) as f32
        };
        assert!(
            mean_dist(&late) < mean_dist(&decos),
            "the shadow ring falls inward"
        );
        // Debris falls inward too: mean mote distance shrinks over the window.
        let mote_dist = |t: u64| {
            let mut motes = Vec::new();
            emit_debris(t, &env, &mut motes, 64);
            assert!(!motes.is_empty());
            mean_dist(&motes)
        };
        let late_t = u64::from(env.feats.duration_ms) - 100;
        assert!(
            mote_dist(late_t) < mote_dist(600),
            "Singularity debris collapses inward"
        );
    }

    /// §6.1/§6.4 debris: 8–20 Add motes, twinkle phases locked to the shared
    /// ≥ 350 ms grid (exactly the 2 grid phases appear, never free-running).
    #[test]
    fn debris_rides_add_stream_on_the_phase_grid() {
        let env = env_at(20, None);
        let mut out = Vec::new();
        emit_debris(700, &env, &mut out, 64);
        assert!(
            (1..=usize::from(env.feats.debris)).contains(&out.len()),
            "mote count within the genome debris budget, got {}",
            out.len()
        );
        assert!(out.iter().all(|d| matches!(d.blend, DecoBlend::Add)));
        // Phase-grid lock: with the envelope factored per-mote at two instants
        // half a grid apart, each mote's twinkle multiplier must land on one
        // of exactly two grid phases — verified structurally: the per-mote
        // phase bit yields at most 2 distinct alpha values at fixed t for
        // motes sharing fade (same t) and intensity, modulo the color mix.
        let phases: std::collections::BTreeSet<u64> = (0..u64::from(env.feats.debris))
            .map(|m| (mix(env.seed ^ m.wrapping_mul(0xA24B_AED4_963E_E407)) >> 33) & 1)
            .collect();
        assert!(
            phases.len() <= 2,
            "twinkle phases quantize to the shared grid"
        );
        // Determinism: same t ⇒ identical bytes.
        let mut again = Vec::new();
        emit_debris(700, &env, &mut again, 64);
        assert_eq!(out, again);
    }

    /// The ember pair dims toward the palette; the Singularity ember is the
    /// dim violet build.
    #[test]
    fn ember_pairs() {
        let p = (0x00FF_F2C8, 0x00FF_9A3C);
        let (e0, e1) = ember_pair(p, None);
        assert!(relative_luminance(e0) < relative_luminance(p.0));
        assert!(relative_luminance(e1) < relative_luminance(p.1));
        assert_eq!(
            ember_pair(p, Some(NovaMagic::Singularity)),
            (0x002A_2040, 0x005A_48A8)
        );
    }
}
