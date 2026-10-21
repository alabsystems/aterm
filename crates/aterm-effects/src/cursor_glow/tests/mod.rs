// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `CursorGlow`'s proofs. They are children of `cursor_glow` so they keep
//! private access to the engine, one file per area; the helpers two or more
//! areas share live here, and a helper one area uses lives in that area.

use super::*;
// The parts the hub does not re-export (their items are the engine's own).
use super::{seam::*, spawn::*, thermal::*};
use crate::cursor_trail::{CursorTrail, TrailConfig};

mod band_move;
mod beam;
mod cold_motion;
mod comet;
mod departed;
mod erase;
mod fire;
mod heat_and_laser;
mod ink_wrap_seam;
mod insert;
mod insert_witness;
mod licence;
mod lifecycle;
mod light_ground;
mod momentum;
mod park;
mod particles;
mod phaser;
mod rainbow;
mod reanchor;
mod resident_cap;
mod review;
mod rings;
mod seam_laws;
mod sound;
mod stall;
mod stall_fold;
mod status;
mod stray;
mod streak_bed;
mod streak_budget;
mod style_switch;
mod tint;
mod typed_stamps;
mod unknown_insert;
mod v2_seam;
mod wake_census;
mod water;
mod witness;
mod word_nav;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// Type `text` at 85 ms per key from the caret at `(row, col)`, each key echoed
/// 3 ms after its press (a Space carries its class, for the ribbon's moved-word
/// relay). Returns the clock of the last echo.
fn type_echoed(
    glow: &mut CursorGlow,
    row: u16,
    col: u16,
    text: &str,
    t: Instant,
    c: &GlowConfig,
    g: Geom,
) -> Instant {
    let mut out = Vec::new();
    let mut t = t;
    let mut col = col;
    for ch in text.chars() {
        t += ms(85);
        let class = if ch == ' ' {
            rk::TypedClass::Space
        } else {
            rk::TypedClass::Glyph
        };
        glow.note_typed_glyph(t, 1, false, class);
        col += 1;
        glow.tick(Some((row, col)), t + ms(3), c, g, &mut out);
    }
    t + ms(3)
}

/// Press one plain Backspace and let its one-column retreat be seen: the press
/// stamps `quench_hint` (licensing the retreat and classifying it as a
/// deletion) and `bs_poof_hint`, and the row probe carries the erase evidence
/// the poof lane reads.
fn arm_exact_backspace(
    glow: &mut CursorGlow,
    now: Instant,
    origin: (u16, u16),
    target: (u16, u16),
) {
    assert_eq!(origin.0, target.0);
    assert_eq!(origin.1, target.1.saturating_add(1));
    let mut baseline = [' '; 40];
    baseline[..usize::from(origin.1)].fill('x');
    glow.note_backspace(now);
    let mut current = baseline;
    current[usize::from(target.1)] = ' ';
    glow.observe_row(target.0, target.1, &current, now);
}

fn geom() -> Geom {
    // Identity layout: origin 0 + win == grid extents (320×96), so the suite
    // also checks window-space identity.
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

fn cfg(style: GlowStyle, enabled: bool) -> GlowConfig {
    GlowConfig {
        enabled,
        // The spectrum face for `classic`.
        classic_mono: false,
        dark_theme: true,
        // The default dark palette: a coherent pair, never 0/0 (`fg == bg` reads as a
        // conceal-shaped theme and suppresses the tint).
        theme_fg: 0x00C8_D3F5,
        theme_bg: 0x001A_1B26,
        style,
        color: 0x0050_FA7B,
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 0.7,
        audible: true,
        radius: 0.6,
        ring: true,
        // Water and rainbow kitty are beam-less (the kitty draws its own
        // continuous body); every other enum style shows its beam.
        beam: !matches!(style, GlowStyle::Water | GlowStyle::RainbowKitty),
        head_dx: 0.5,
        // No raw spelling to resolve here, so start on the default tall
        // presentation; spelling tests use `cfg_for_style_name`.
        ribbon_tall: true,
        ribbon_flat: false,
    }
}

/// Test twin of the app/pipeline resolution seam: classify the family and
/// derive its presentation from the same raw spelling.
fn cfg_for_style_name(raw: &str, enabled: bool) -> GlowConfig {
    let mut c = cfg(GlowStyle::parse(raw), enabled);
    // The tall body is the default; only an explicit `... underline` spelling opts out, as at
    // the app_config seam.
    c.ribbon_tall = !GlowStyle::style_names_underline_ribbon(raw);
    c.ribbon_flat = GlowStyle::style_names_flat_ribbon(raw);
    c
}

/// Every emitted quad is single-row and inside the grid interior — the
/// invariant the renderer's row-scoped gate + parity rely on.
fn assert_invariants(out: &[GlowQuad], g: Geom) {
    let gw = (g.cols * g.cw) as u32;
    let gh = (g.rows * g.ch) as u32;
    for q in out {
        let y = q.y as u32;
        let band = q.row as u32 * g.ch as u32;
        assert!(
            y >= band && y + q.h as u32 <= band + g.ch as u32,
            "quad spans >1 row: {q:?}"
        );
        assert!(q.x as u32 + q.w as u32 <= gw, "quad past right edge: {q:?}");
        assert!(y + q.h as u32 <= gh, "quad past bottom edge: {q:?}");
        assert!((q.row as usize) < g.rows, "quad row out of grid: {q:?}");
    }
}

/// Codex's 57-row screen (`ESC[{vt};57r`), 0-based: viewport tops (11, 16, 18,
/// 22, 24, …, 51), the composer at row 13 after the first Enter, the pinned
/// composer at row 54. The band tests are stated on it.
fn geom_codex() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: 57,
        cols: 151,
        origin_x: 0,
        origin_y: 0,
        win_w: 1208,
        win_h: 912,
        head: 0,
    }
}

/// Premultiplied luminance proxy: the sum of all channel bytes over the frame.
fn lum(out: &[GlowQuad]) -> u64 {
    out.iter()
        .map(|q| (((q.color >> 16) & 0xff) + ((q.color >> 8) & 0xff) + (q.color & 0xff)) as u64)
        .sum()
}

/// Whether any visible cursor-effect stream is non-empty this frame.
fn frame_has_output(out: &[GlowQuad], glow: &CursorGlow) -> bool {
    !out.is_empty()
        || !glow.halos().is_empty()
        || !glow.under_quads().is_empty()
        || !glow.patches().is_empty()
        || !glow.charred().is_empty()
        || !glow.halo_cells().is_empty()
}

/// Seed a visible caret at `(3, 2)` and type three keys echoed one cell
/// each 8 ms after the press at a 100 ms cadence: the cohort `2..5`, the
/// caret at `(3, 5)`, `spawns == 3`. Returns the clock after the third
/// echo.
fn three_visible_echoes(glow: &mut CursorGlow, t0: Instant, out: &mut Vec<GlowQuad>) -> Instant {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    glow.tick(Some((3, 2)), t0, &c, g, out);
    let mut echo = t0;
    for (i, col) in (3..=5u16).enumerate() {
        let key = t0 + Duration::from_millis(100 * (i as u64 + 1));
        glow.note_typed(key);
        echo = key + Duration::from_millis(8);
        glow.tick(Some((3, col)), echo, &c, g, out);
    }
    assert_eq!(glow.spawns(), 3, "the three typed echoes light");
    echo
}

/// A retina-scale geometry: the strip's short axis is wide enough in device
/// pixels to carry all six anchors, so the slab law can be measured.
fn retina_geom() -> Geom {
    Geom {
        cw: 16,
        ch: 40,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 640,
        win_h: 240,
        head: 0,
    }
}

/// The shared test compositor: the renderer's painter order, `add_sat`
/// saturation and opaque caret, used by the on-glass gates. A linear rescale
/// cannot represent `add_sat` clipping, and the caret is painted last over its
/// own halo, so gates must composite the real way.
///
/// The order, as in `composite_free` and its post-passes:
///
/// ```text
///   ground                                   the theme background
///   glow_under        add_sat                phase B3
///   fire_patch        add_sat / source-over  phase B3, after glow_under
///   glyph ink         replace                phase C
///   cursor_glow_add   add_sat                draw_glow
///   glow_halo         add_sat / source-over  draw_glow_halo
///   the caret         REPLACE, opaque        draw_cursor — LAST
/// ```
///
/// `add_sat`, `premul_rgb`, `over_rgb`, `halo_row_ny` and `halo_weight` are the
/// renderer's own exported functions, so this moves with the renderer.
struct Glass {
    px: Vec<u32>,
    w: usize,
    h: usize,
    g: Geom,
}

impl Glass {
    /// A frame of theme background, at the geometry's own window size.
    fn new(g: Geom, bg: u32) -> Self {
        let (w, h) = (g.win_w as usize, g.win_h as usize);
        Self {
            px: vec![bg; w * h],
            w,
            h,
            g,
        }
    }

    fn at(&self, y: i32, x: i32) -> u32 {
        if y < 0 || x < 0 || y as usize >= self.h || x as usize >= self.w {
            return 0;
        }
        self.px[y as usize * self.w + x as usize]
    }

    /// One channel-max read of a composited pixel.
    fn level(&self, y: i32, x: i32) -> i32 {
        let p = self.at(y, x);
        (((p >> 16) & 0xff).max((p >> 8) & 0xff).max(p & 0xff)) as i32
    }

    /// A [`GlowQuad`] stream, per quad as `draw_flat_add` does.
    /// [`aterm_render::over_premul`] covers both modes (it equals `add_sat` at
    /// `alpha == 0`), so the rainbow bed's source-over quads composite as
    /// drawn rather than brighter.
    fn add_quads(&mut self, qs: &[GlowQuad]) {
        for q in qs {
            // A gradient quad's column `i` by [`GlowQuad::color2`]'s integer
            // law, as the rasterizers draw it.
            let w = u32::from(q.w.max(1));
            let at = |i: u32, a: u32, b: u32| {
                (a * (2 * w - (2 * i + 1)) + b * (2 * i + 1) + w) / (2 * w)
            };
            let lerp = |i: u32, a: u32, b: u32| {
                (at(i, (a >> 16) & 0xff, (b >> 16) & 0xff) << 16)
                    | (at(i, (a >> 8) & 0xff, (b >> 8) & 0xff) << 8)
                    | at(i, a & 0xff, b & 0xff)
            };
            for y in q.y as usize..(q.y as usize + q.h as usize).min(self.h) {
                for x in q.x as usize..(q.x as usize + q.w as usize).min(self.w) {
                    let i = (x - q.x as usize) as u32;
                    let (color, alpha) = if q.is_flat() {
                        (q.color, q.alpha)
                    } else {
                        (
                            lerp(i, q.color, q.color2),
                            at(i, u32::from(q.alpha), u32::from(q.alpha2)) as u8,
                        )
                    };
                    let slot = &mut self.px[y * self.w + x];
                    *slot = aterm_render::over_premul(*slot, color, alpha);
                }
            }
        }
    }

    /// A [`RainHalo`] stream as `draw_radial_add` draws it: Add-before-Over mode
    /// split and the integer elliptical falloff both presenters share.
    fn add_halos(&mut self, hs: &[RainHalo]) {
        for mode in [HaloMode::Add, HaloMode::Over] {
            for q in hs.iter().filter(|q| q.mode == mode) {
                let over_cap = aterm_render::halo_over_cap(q.color);
                let (cx, cy) = (q.cx as i32, q.cy as i32);
                let (rx2, ry2) = ((q.rx as i32).pow(2), (q.ry as i32).pow(2));
                for y in q.y as usize..(q.y as usize + q.h as usize).min(self.h) {
                    let ny = aterm_render::halo_row_ny(y as i32 - cy, ry2);
                    if ny >= 256 {
                        continue;
                    }
                    for x in q.x as usize..(q.x as usize + q.w as usize).min(self.w) {
                        let wt = aterm_render::halo_weight(x as i32 - cx, ny, rx2);
                        if wt == 0 {
                            continue;
                        }
                        let wt = wt.min(255) as u8;
                        let slot = &mut self.px[y * self.w + x];
                        *slot = match mode {
                            HaloMode::Add => {
                                aterm_render::add_sat(*slot, aterm_render::premul_rgb(q.color, wt))
                            }
                            HaloMode::Over => {
                                aterm_render::over_rgb(*slot, q.color, wt.min(over_cap))
                            }
                        };
                    }
                }
            }
        }
    }

    /// A [`FirePatch`] stream as `draw_fire_patch` draws it, Add before Over,
    /// through the renderer's own integer field sampler and its top-edge fade
    /// anchored at the pad (`origin_y - head`). It belongs under the glyph ink
    /// (phase B3, after `glow_under`).
    fn add_fire_patches(&mut self, patches: &[FirePatch]) {
        use aterm_render::fire_field as ff;
        for mode in [FireMode::Add, FireMode::Over] {
            for q in patches.iter().filter(|q| q.mode == mode) {
                let fp = ff::FireFieldParams {
                    base_y: i32::from(q.base_y),
                    peak_h: i32::from(q.peak_h),
                    phase: q.phase,
                    temp: i32::from(q.temp),
                    strength: i32::from(q.strength),
                    lean: i32::from(q.lean),
                    cov_cap: i32::from(q.cov_cap),
                    cell_h: i32::from(q.cell_h),
                    top_fade_y: i32::from(self.g.origin_y) - i32::from(self.g.head),
                };
                let pc = ff::fire_precomp(&fp);
                let (x0, y0) = (usize::from(q.x), usize::from(q.y));
                let x1 = (x0 + usize::from(q.w)).min(self.w);
                for y in y0..(y0 + usize::from(q.h)).min(self.h) {
                    let tf = ff::fire_top_fade(y as i32, &fp);
                    let mut row = ff::FireRow::new(y as i32, x0 as i32, &fp, &pc);
                    for x in x0..x1 {
                        let c = row.core(x as i32);
                        let slot = &mut self.px[y * self.w + x];
                        match mode {
                            FireMode::Add => {
                                let pm = ff::fire_shade_add(&c, &fp, tf);
                                if pm != 0 {
                                    *slot = aterm_render::add_sat(*slot, pm);
                                }
                            }
                            FireMode::Over => {
                                let (rgb, a) = ff::fire_shade_over(&c, &fp, tf);
                                if a != 0 {
                                    *slot = aterm_render::over_rgb(*slot, rgb, a);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// The pixel rect of one grid cell, window-absolute.
    fn cell_rect(&self, row: u16, col: u16) -> (i32, i32, i32, i32) {
        let x0 = i32::from(self.g.origin_x) + i32::from(col) * self.g.cw as i32;
        let y0 = i32::from(self.g.origin_y) + i32::from(row) * self.g.ch as i32;
        (x0, y0, x0 + self.g.cw as i32, y0 + self.g.ch as i32)
    }

    /// Phase C, the glyph ink, over the under-ink stack and under every later
    /// additive stream. A solid `fg` block across the x-height is the worst
    /// case: whatever an ink pixel reads afterwards, the effect put it there.
    fn stamp_ink(&mut self, row: u16, col: u16, fg: u32) {
        let (x0, y0, x1, y1) = self.cell_rect(row, col);
        let top = y0 + (self.g.ch as i32 * 3) / 10;
        let bot = y0 + (self.g.ch as i32 * 8) / 10;
        for y in top.max(0)..bot.min(self.h as i32) {
            for x in (x0 + 2).max(0)..(x1 - 2).min(self.w as i32) {
                self.px[y as usize * self.w + x as usize] = fg;
            }
        }
        let _ = y1;
    }

    /// `draw_cursor`, last: at default opacity the block fill replaces the
    /// cell, so only light outside the caret cell survives.
    fn stamp_caret(&mut self, row: u16, col: u16, fill: u32) {
        let (x0, y0, x1, y1) = self.cell_rect(row, col);
        for y in y0.max(0)..y1.min(self.h as i32) {
            for x in x0.max(0)..x1.min(self.w as i32) {
                self.px[y as usize * self.w + x as usize] = fill;
            }
        }
    }
}

/// A set of pixels, `(y, x)`.
type Pixels = Vec<(i32, i32)>;

/// A 22 px font's cell (13 × 26), the size the trail gallery captures at, so
/// an effect's layer thicknesses meet glyphs at their real proportions.
fn tall_geom() -> Geom {
    Geom {
        cw: 13,
        ch: 26,
        rows: 8,
        cols: 48,
        origin_x: 0,
        origin_y: 0,
        win_w: 624,
        win_h: 208,
        head: 0,
    }
}

/// One glyph at `(row, col)`: three 2 px stems across the x-height band (an
/// `m`), and the counters between and beside them in the same band. A stroke
/// is legible against the counters next to it, so those are the ground.
fn glyph(g: Geom, row: u16, col: u16) -> (Pixels, Pixels) {
    let x0 = i32::from(g.origin_x) + i32::from(col) * g.cw as i32;
    let y0 = i32::from(g.origin_y) + i32::from(row) * g.ch as i32;
    let (top, bot) = (y0 + (g.ch as i32 * 3) / 10, y0 + (g.ch as i32 * 8) / 10);
    let mut ink = Vec::new();
    let mut counters = Vec::new();
    for y in top..bot {
        for x in x0 + 1..x0 + g.cw as i32 - 1 {
            if matches!(x - x0, 2 | 3 | 6 | 7 | 10 | 11) {
                ink.push((y, x));
            } else {
                counters.push((y, x));
            }
        }
    }
    (ink, counters)
}

/// The WCAG ratio between the mean relative luminance of `ink` and that of
/// `ground`.
fn ratio(glass: &Glass, ink: &[(i32, i32)], ground: &[(i32, i32)]) -> f32 {
    let mean = |px: &[(i32, i32)]| {
        px.iter()
            .map(|&(y, x)| rel_luminance(glass.at(y, x) & 0x00FF_FFFF))
            .sum::<f32>()
            / px.len() as f32
    };
    let (a, b) = (mean(ink), mean(ground));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Whether a quad is a ribbon quad: a saturated spectrum colour (chroma above a
/// floor and one channel genuinely low). This excludes the white starfield
/// (R≈G≈B) and warm-white sparkles (0xFFF2C0). The floor of 12 still detects a
/// freshly laid, dim cold ribbon cell at the test intensity (0.7).
fn is_ribbon_quad(color: u32) -> bool {
    let (r, g, b) = ((color >> 16) & 0xff, (color >> 8) & 0xff, color & 0xff);
    let hi = r.max(g).max(b);
    let lo = r.min(g).min(b);
    hi.saturating_sub(lo) > 12 && lo < 100
}

/// The columns of `row` that own v2 ribbon light: laid glyph cells
/// (`Engine::field_at`), never the caret's own.
fn v2_cols(glow: &CursorGlow, row: u16) -> std::collections::BTreeSet<u16> {
    (0..256u16)
        .filter(|&col| glow.v2.field_at(row, col).is_some())
        .collect()
}

fn ribbon_cols(out: &[GlowQuad], row: u16, g: Geom) -> std::collections::BTreeSet<u16> {
    out.iter()
        .filter(|q| q.row == row && is_ribbon_quad(q.color))
        .map(|q| (q.x - g.origin_x) / g.cw as u16)
        .collect()
}

/// Every built-in style.
const BUILTINS: [GlowStyle; 9] = [
    GlowStyle::Lumen,
    GlowStyle::Phaser,
    GlowStyle::RainbowKitty,
    GlowStyle::Sparkle,
    GlowStyle::Fire,
    GlowStyle::Laser,
    GlowStyle::Beam,
    GlowStyle::Water,
    GlowStyle::Comet,
];

fn typed_scratch() -> Vec<GlowQuad> {
    Vec::new()
}

/// One beat past the park window: a held park ([`HeldPark`]) is judged on the
/// first tick more than [`CursorGlow::TYPE_HINT_FRESH`] after it, so a verdict
/// a park defers (a refusal and its forget, a re-anchor's flush) lands this far
/// after the move.
fn past_park_window() -> Duration {
    Duration::from_secs_f32(CursorGlow::TYPE_HINT_FRESH) + Duration::from_millis(50)
}

/// A 100-column grid, so a 50-cell batch fits one row.
fn wide_geom() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: 30,
        cols: 100,
        origin_x: 0,
        origin_y: 0,
        win_w: 800,
        win_h: 480,
        head: 0,
    }
}

/// Pre-roll: a seating tick at `(row, 2)`, then three glyphs typed and echoed
/// per key ([`type_echoed`]), leaving the caret at `(row, 5)` on a live
/// three-cell cohort. Returns the clock of the last echo.
fn pre_roll(glow: &mut CursorGlow, row: u16, t0: Instant, c: &GlowConfig, g: Geom) -> Instant {
    let mut out = Vec::new();
    glow.tick(Some((row, 2)), t0, c, g, &mut out);
    type_echoed(glow, row, 2, "xxx", t0, c, g)
}

/// `n` keys 85 ms apart from `t`, none echoed. Returns the clock of the last press.
fn stalled_keys(glow: &mut CursorGlow, n: u64, t: Instant) -> Instant {
    let mut last = t;
    for i in 0..n {
        last = t + Duration::from_millis(85 * (i + 1));
        glow.note_typed(last);
    }
    last
}

fn dark_in(glow: &CursorGlow, row: u16, cols: std::ops::Range<u16>) -> Vec<u16> {
    let lit = v2_cols(glow, row);
    cols.filter(|col| !lit.contains(col)).collect()
}

fn bridged(glow: &CursorGlow) -> u32 {
    glow.v2_status().map_or(0, |s| s.bridged)
}

/// Keys echoed on time up to `(row, col)` on a 100-column row, from
/// `col - 4`. Returns the clock of the last echo.
fn roll_to(
    glow: &mut CursorGlow,
    row: u16,
    col: u16,
    t0: Instant,
    c: &GlowConfig,
    g: Geom,
) -> Instant {
    let mut out = Vec::new();
    glow.tick(Some((row, col - 4)), t0, c, g, &mut out);
    let mut t = t0;
    for at in (col - 4)..col {
        t += Duration::from_millis(85);
        glow.note_typed(t);
        glow.tick(
            Some((row, at + 1)),
            t + Duration::from_millis(3),
            c,
            g,
            &mut out,
        );
    }
    t + Duration::from_millis(3)
}

/// One ring row as `(reason, licence, origin, target)`.
type RingRow = (&'static str, &'static str, (u16, u16), (u16, u16));

fn ring_rows(glow: &CursorGlow) -> Vec<RingRow> {
    glow.admission_log()
        .map(|r| (r.reason, r.licence, r.origin, r.target))
        .collect()
}

// A word move has no key-time cue (D11: a no-op Ctrl-E must stay silent), so its sound is minted
// when the seam licenses the observed caret delta, and every refusal is a silent word hop.

/// Nav ticks / meteors / word-kill voices drained since the last call.
#[derive(Default, PartialEq, Eq, Debug)]
struct WordNavCues {
    nav: u32,
    meteor: u32,
    kill_word: u32,
}

fn wordnav_drain(glow: &mut CursorGlow) -> WordNavCues {
    use crate::trail_sound::SoundKind;
    let mut out = WordNavCues::default();
    for cue in glow.drain_sound_cues() {
        match cue.kind {
            SoundKind::Navigation => out.nav += 1,
            SoundKind::Meteor { .. } => out.meteor += 1,
            SoundKind::KillWord => out.kill_word += 1,
            _ => {}
        }
    }
    out
}
