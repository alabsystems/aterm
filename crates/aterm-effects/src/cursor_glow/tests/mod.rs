// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `CursorGlow`'s proofs. They are children of `cursor_glow` so they keep
//! private access to the engine, one file per area; the helpers two or more
//! areas share live here, and a helper one area uses lives in that area.

use super::*;
// The parts the hub does not re-export (their items are the engine's own).
use super::{emit::*, seam::*, spawn::*};
use crate::cursor_trail::{CursorTrail, TrailConfig};

/// Band moves and the Codex composer.
mod band_move;
/// The beam style.
mod beam;
/// Cold program motion stays dark.
mod cold_motion;
/// The comet style.
mod comet;
/// Backspace and kill: the poof, the steam and their licences.
mod erase;
/// The fire style: meteors, the EMBERFORGE thermal laws, the forge field.
mod fire;
/// The byte-exact deletion and flat-spelling goldens.
mod goldens;
/// Typing heat and the laser, sparkle and laser bursts.
mod heat_and_laser;
/// The Ink wrap seam trace.
mod ink_wrap_seam;
/// Delivered inserts.
mod insert;
/// Insert witnesses, receipts and spinner rows.
mod insert_witness;
/// The licence seam and its derived-model conformance.
mod licence;
/// Light is born on a move, fades to empty, and keeps its invariants.
mod lifecycle;
/// Shared momentum and the composed-pane folds.
mod momentum;
/// The park: Ink's rewrite observed as two sync brackets.
mod park;
/// The phaser band and coalesced echo sweeps.
mod phaser;
/// The rainbow kitty ribbon.
mod rainbow;
/// Wrap re-anchors, bridged moves and box growth.
mod reanchor;
/// The resident-state cap and its derived model.
mod resident_cap;
/// The review's findings on the stall, fold and park.
mod review;
/// Landing rings and halo culling.
mod rings;
/// The seam's licence-class laws (one-shots spend once, a glyph supersedes
/// nothing whose echo is owed, one kill one `Kill`, a held park is a wake
/// source).
mod seam_laws;
/// Typing sound cues and the key-time ledger.
mod sound;
/// The stall: keys typed into a stalled event loop.
mod stall;
/// The fold after a stall.
mod stall_fold;
/// Fresh-ink pop, the typing wake and the `trail` status row.
mod status;
/// The stray a fresh hint licenses.
mod stray;
/// Style-switch crossfades, the non-fire crown and pooled bolts.
mod style_switch;
/// Glyph tint and light-theme ink.
mod tint;
/// Trail Packs (the `GlowStyle::Custom` data path).
mod trail_pack;
/// Typed stamps across gestures, hidden carets and program rows.
mod typed_stamps;
/// Unknown-width inserts and their orphan keys.
mod unknown_insert;
/// Rainbow kitty v2 at the seam.
mod v2_seam;
/// The wake census at the seam.
mod wake_census;
/// The water style.
mod water;
/// The ribbon's witness rows and the exact-key credit ring.
mod witness;
/// The word move sounds every time.
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

// The test pinning `prefs::CURSOR_TRAIL_STYLES` to `GlowStyle::parse` lives
// beside the list in aterm-gui's `prefs.rs`.

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

// ---- Living flow ------------------------------------------------------
//
// The channel's own gates: it moves, does not flicker, does not re-colour, and
// spends no light, plus the reduced-motion stand-down.

fn cfg(style: GlowStyle, enabled: bool) -> GlowConfig {
    GlowConfig {
        enabled,
        // The SPECTRUM face, which the whole-frame golden pins for `classic`.
        classic_mono: false,
        dark_theme: true,
        // The documented default dark palette — a COHERENT pair, never 0/0
        // (`fg == bg` reads as a conceal-shaped theme and suppresses the tint).
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
        pack: None,
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
    // The tall body is the default; only an explicit `... underline`
    // spelling opts out (the app_config seam's law, mirrored).
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

/// Whether ANY visible cursor-effect stream is non-empty this frame.
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
            for y in q.y as usize..(q.y as usize + q.h as usize).min(self.h) {
                for x in q.x as usize..(q.x as usize + q.w as usize).min(self.w) {
                    let slot = &mut self.px[y * self.w + x];
                    *slot = aterm_render::over_premul(*slot, q.color, q.alpha);
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

// ===================================================================
// Trail Packs (the `GlowStyle::Custom` data path).
// ===================================================================

/// Every built-in style. Adding `Custom` must not change any of these.
const BUILTINS: [GlowStyle; 10] = [
    GlowStyle::Lumen,
    GlowStyle::Phaser,
    GlowStyle::RainbowKitty,
    GlowStyle::Sparkle,
    GlowStyle::Fire,
    GlowStyle::Laser,
    GlowStyle::Beam,
    GlowStyle::Water,
    GlowStyle::Comet,
    GlowStyle::Lumen, // padding kept intentionally distinct in the fold order
];

fn typed_scratch() -> Vec<GlowQuad> {
    Vec::new()
}

// ===================================================================
// Rainbow Kitty v2: the seam (`docs/design/RAINBOW-KITTY-V2.md` §17.2, D14).
// ===================================================================

/// The deletion oracle's script: one session reaching every v2 producer (a
/// probed row for stardust, ten keys at human cadence, an end-of-line fling,
/// three more keys, two backspaces, a kill, a home fling, a Return, four keys
/// on the new row, a five-cell word hop, and a decay tail to 5 s), folding
/// every observable the host reads each frame: fingerprint, `out` / `under`
/// quad bytes, halo fields, cue kinds, the caret seam (`caret_paint`,
/// `caret_flare_at`, `rainbow_field`, `rainbow_head_rgb`), the spine
/// (`momentum_display`, `rainbow_phase`, `typing_momentum`), the scheduler
/// (`needs_frame_cadence`, `next_change_deadline`, `is_active`), the companion
/// impulse, the motion pulse and the v2 status row. `reduced` drives the
/// posture seam. Deterministic given the relative schedule.
fn deletion_script(cfg: &GlowConfig, g: Geom, reduced: bool) -> u64 {
    let mut glow = CursorGlow::default();
    glow.set_reduced_motion(reduced);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut acc: u64 = 0;
    let blank = [' '; 40];
    let mut row_2 = [' '; 40];
    let mut row_3 = [' '; 40];
    let mut step = |glow: &mut CursorGlow,
                    at: Instant,
                    cell: (u16, u16),
                    row_2: &[char; 40],
                    row_3: &[char; 40]| {
        let (row, caret) = cell;
        let cols: &[char; 40] = if row == 2 { row_2 } else { row_3 };
        glow.observe_row(row, caret, cols, at);
        glow.observe_neighbor_rows(Some(&blank), Some(&blank));
        let fp = glow.tick(Some(cell), at, cfg, g, &mut out);
        acc = fnv_mix_u64(acc, fp);
        for q in out.iter().chain(glow.under_quads()) {
            for w in q.damage_words() {
                acc = fnv_mix_u64(acc, w);
            }
        }
        for h in glow.halos() {
            for w in [
                u64::from(h.row),
                u64::from(h.x),
                u64::from(h.y),
                u64::from(h.w),
                u64::from(h.h),
                u64::from(h.color),
                u64::from(h.cx),
                u64::from(h.cy),
                u64::from(h.rx),
                u64::from(h.ry),
                u64::from(matches!(h.mode, aterm_core::render::HaloMode::Over)),
            ] {
                acc = fnv_mix_u64(acc, w);
            }
        }
        acc = fnv_mix_u64(acc, glow.sound_cues.len() as u64);
        for cue in glow.drain_sound_cues() {
            for b in format!("{cue:?}").bytes() {
                acc = fnv_mix_u64(acc, u64::from(b));
            }
        }
        acc = fnv_mix_u64(acc, u64::from(glow.caret_paint(at).to_bits()));
        acc = fnv_mix_u64(
            acc,
            glow.caret_flare_at().map_or(u64::MAX, |f| {
                at.saturating_duration_since(f).as_micros() as u64
            }),
        );
        acc = fnv_mix_u64(acc, u64::from(glow.rainbow_field().to_bits()));
        acc = fnv_mix_u64(
            acc,
            u64::from(glow.rainbow_head_rgb(cfg).unwrap_or(u32::MAX)),
        );
        acc = fnv_mix_u64(acc, u64::from(glow.momentum_display().to_bits()));
        acc = fnv_mix_u64(acc, u64::from(glow.rainbow_phase().to_bits()));
        acc = fnv_mix_u64(acc, u64::from(glow.typing_momentum(at).to_bits()));
        acc = fnv_mix_u64(acc, u64::from(glow.needs_frame_cadence()));
        acc = fnv_mix_u64(
            acc,
            glow.next_change_deadline(at, Duration::from_millis(8))
                .map_or(u64::MAX, |d| {
                    d.saturating_duration_since(at).as_micros() as u64
                }),
        );
        acc = fnv_mix_u64(acc, u64::from(glow.is_active()));
        acc = fnv_mix_u64(
            acc,
            match glow.take_companion_impulse() {
                None => 0,
                Some(CompanionImpulse::Land) => 1,
                Some(CompanionImpulse::Wince) => 2,
                Some(CompanionImpulse::Delight) => 3,
                Some(CompanionImpulse::Meteor { dir, t0, t_flight }) => {
                    let age = at.saturating_duration_since(t0).as_micros() as u64;
                    fnv_mix_u64(
                        fnv_mix_u64(fnv_mix_u64(4, dir as u64), age),
                        t_flight.as_micros() as u64,
                    )
                }
            },
        );
        for b in format!("{:?}", glow.take_cursor_cat_motion_pulse().map(|p| p.kind)).bytes() {
            acc = fnv_mix_u64(acc, u64::from(b));
        }
        if let Some(st) = glow.v2_status() {
            for w in [
                u64::from(st.quads),
                u64::from(st.halos),
                u64::from(st.stars),
                u64::from(st.meteors),
                u64::from(st.cells),
                u64::from(st.disp.to_bits()),
                u64::from(st.tokens.to_bits()),
                st.fp,
            ] {
                acc = fnv_mix_u64(acc, w);
            }
        }
    };
    let ms = |m: u64| t0 + Duration::from_millis(m);
    step(&mut glow, t0, (2, 4), &row_2, &row_3);
    for k in 1..=10u64 {
        glow.note_synthetic_typed(ms(80 * k), 1);
        row_2[3 + k as usize] = 'a';
        step(&mut glow, ms(80 * k), (2, 4 + k as u16), &row_2, &row_3);
    }
    glow.note_motion(ms(1000));
    step(&mut glow, ms(1000), (2, 34), &row_2, &row_3);
    for (k, at) in [1100u64, 1180, 1260].into_iter().enumerate() {
        glow.note_synthetic_typed(ms(at), 1);
        row_2[34 + k] = 'b';
        step(&mut glow, ms(at), (2, 35 + k as u16), &row_2, &row_3);
    }
    glow.note_backspace(ms(1400));
    row_2[36] = ' ';
    step(&mut glow, ms(1400), (2, 36), &row_2, &row_3);
    glow.note_backspace(ms(1480));
    row_2[35] = ' ';
    step(&mut glow, ms(1480), (2, 35), &row_2, &row_3);
    glow.note_kill(ms(1600), true);
    row_2[34] = ' ';
    step(&mut glow, ms(1600), (2, 20), &row_2, &row_3);
    glow.note_motion(ms(1700));
    step(&mut glow, ms(1700), (2, 0), &row_2, &row_3);
    glow.note_return(ms(1800));
    step(&mut glow, ms(1800), (3, 0), &row_2, &row_3);
    for k in 1..=4u64 {
        glow.note_synthetic_typed(ms(1800 + 80 * k), 1);
        row_3[k as usize - 1] = 'c';
        step(&mut glow, ms(1800 + 80 * k), (3, k as u16), &row_2, &row_3);
    }
    glow.note_motion(ms(2300));
    step(&mut glow, ms(2300), (3, 9), &row_2, &row_3);
    for at in [2400u64, 2500, 2700, 3000, 3500, 4200, 5000] {
        step(&mut glow, ms(at), (3, 9), &row_2, &row_3);
    }
    acc
}

/// The deletion goldens: [`deletion_script`]'s fold for every variant (the
/// nine built-ins dark, then rainbow kitty light, underline and reduced
/// motion). The eight non-kitty rows never move: v2 does not engage for those
/// styles (`the_other_nine_styles_are_byte_identical_with_v2_unconditional`).
/// The four kitty rows are pinned by
/// `rainbow_kitty_is_byte_identical_to_the_seam_era_v2`, so any drift is a
/// decision. A kitty row is the arc's bytes: any change to
/// `crate::spectrum`'s table moves all four.
///
/// THE RE-BAKE LAW. A v2 change that moves a kitty byte re-reads exactly
/// the kitty rows it moves — and the flat spelling's twins,
/// [`FLAT_GOLDENS`], the same way — with the eight non-kitty rows
/// as the control (they must hold) and, where the change has a switch,
/// the decomposition as the proof: with the clause toggled back in place
/// every row reads its previous number. Which rows move is itself a
/// claim: light has no rail and no hot edge (L6), reduced has no wipe
/// and no flight (§6.11), so a change confined to one of those moves
/// the other rows and holds these. Every past re-bake — what moved, the
/// numbers before and after, which control held — is in `git log -L` on
/// this table and in CHANGELOG.md.
const DELETION_GOLDENS: [(&str, u64); 12] = [
    ("Lumen dark", 1_264_411_311_373_267_895),
    ("Phaser dark", 2_726_566_909_586_900_035),
    ("RainbowKitty dark", 383_721_728_202_092_274),
    ("Sparkle dark", 14_969_905_489_495_276_903),
    ("Fire dark", 12_618_090_056_209_866_428),
    ("Laser dark", 1_955_324_598_530_313_952),
    ("Beam dark", 15_086_158_732_367_022_435),
    ("Water dark", 482_165_703_607_766_578),
    ("Comet dark", 14_523_124_226_784_523_753),
    ("RainbowKitty light", 5_202_336_454_404_496_754),
    ("RainbowKitty underline", 544_488_007_139_041_733),
    ("RainbowKitty reduced", 8_179_550_295_111_973_776),
];

/// The flat spelling's goldens: the four kitty rows of [`DELETION_GOLDENS`]
/// under `rainbow kitty flat` (`GlowConfig::ribbon_flat`). Every comet branch
/// in `ribbon.rs` (comet profile, vivid rail, from-the-hand wipe) collapses to
/// the flat expression under the flag;
/// `the_flat_spelling_collapses_every_comet_branch_byte_for_byte` reads these
/// with the flag on, and its `assert_ne!` shows the default body moves every
/// row away from them.
///
/// These rows move with [`DELETION_GOLDENS`] on every change that is NOT
/// a comet branch — the seam feeds both spellings the same events, and
/// the landing, the attach, the boundary law, the soft end and the colour
/// walk stand under the flat body too — and hold when only a comet branch
/// changes. Re-read them whenever the kitty rows are re-baked; the history
/// is in `git log -L` on this table. Older docs call it `PRE_COMET_GOLDENS`;
/// it was renamed because laws the flag does not gate have since moved these
/// bytes away from the pre-comet tree's.
const FLAT_GOLDENS: [(&str, u64); 4] = [
    ("RainbowKitty dark", 17_890_278_511_770_188_755),
    ("RainbowKitty light", 6_430_388_712_169_283_571),
    ("RainbowKitty underline", 15_379_506_098_573_732_577),
    ("RainbowKitty reduced", 12_943_034_956_261_997_624),
];

// =======================================================================
// The stall: keys typed into an event-loop stall come back as one merged
// frame. A press is in flight until its row echoes or an observed edge it
// cannot explain forgets it; it does not expire on a wall clock while its row
// stays silent.
// =======================================================================

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

/// `n` keys at the owner's cadence from `t`, none echoed. Returns the
/// clock of the last press.
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

// =======================================================================
// The fold after a stall: a key whose echo wraps the row, observed after a
// stall (`11,99 -> 12,0`), must still light the last column.
// =======================================================================

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

// =======================================================================
// The park: Ink's rewrite seen as two sync brackets ~40 ms apart, the caret
// parked at the start of the input, then the row rewritten to its end plus
// the new glyph.
// =======================================================================

/// One ring row as `(reason, licence, origin, target)`.
type RingRow = (&'static str, &'static str, (u16, u16), (u16, u16));

fn ring_rows(glow: &CursorGlow) -> Vec<RingRow> {
    glow.admission_log()
        .map(|r| (r.reason, r.licence, r.origin, r.target))
        .collect()
}

// ---- The word move sounds every time ----------------------
//
// Option+Left / Option+Right (and Ctrl+arrow, Alt-b/f) have no key-time cue:
// the keyed seam opens only for a typed glyph, and D11 vetoes a nav pre-cue
// because a no-op Ctrl-E must stay silent. A word move's whole sound is minted
// when this seam licenses the observed caret delta, so every refusal is a
// silent word hop.
//
// Shapes: zsh (caret-only echo), an Ink-style TUI (hide, rewrite, place,
// show), a fast pair, a program move with no key behind it, and the word kill.

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
