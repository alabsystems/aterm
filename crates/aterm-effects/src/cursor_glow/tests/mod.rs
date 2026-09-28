// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `CursorGlow`'s proofs. They are children of `cursor_glow` so they keep
//! private access to the engine, one file per area; the helpers two or more
//! areas share live here, and a helper one area uses lives in that area.

use super::*;
// The parts the hub does not re-export (their items are the engine's own).
use super::{emit::*, seam::*, spawn::*};
use crate::cursor_trail::{CursorTrail, TrailConfig};

/// Band moves and the Codex composer: `cursor_glow/tests/band_move.rs`.
mod band_move;
/// The beam style: `cursor_glow/tests/beam.rs`.
mod beam;
/// Cold program motion stays dark: `cursor_glow/tests/cold_motion.rs`.
mod cold_motion;
/// The comet style: `cursor_glow/tests/comet.rs`.
mod comet;
/// Backspace and kill: the poof, the steam and their licences: `cursor_glow/tests/erase.rs`.
mod erase;
/// The fire style: meteors, the EMBERFORGE thermal laws, the forge field: `cursor_glow/tests/fire.rs`.
mod fire;
/// The byte-exact deletion and flat-spelling goldens: `cursor_glow/tests/goldens.rs`.
mod goldens;
/// Typing heat and the laser, sparkle and laser bursts: `cursor_glow/tests/heat_and_laser.rs`.
mod heat_and_laser;
/// The Ink wrap seam trace: `cursor_glow/tests/ink_wrap_seam.rs`.
mod ink_wrap_seam;
/// Delivered inserts: `cursor_glow/tests/insert.rs`.
mod insert;
/// Insert witnesses, receipts and spinner rows: `cursor_glow/tests/insert_witness.rs`.
mod insert_witness;
/// The licence seam and its derived-model conformance: `cursor_glow/tests/licence.rs`.
mod licence;
/// Light is born on a move, fades to empty, and keeps its invariants: `cursor_glow/tests/lifecycle.rs`.
mod lifecycle;
/// Shared momentum and the composed-pane folds: `cursor_glow/tests/momentum.rs`.
mod momentum;
/// The park: Ink's rewrite observed as two sync brackets: `cursor_glow/tests/park.rs`.
mod park;
/// The phaser band and coalesced echo sweeps: `cursor_glow/tests/phaser.rs`.
mod phaser;
/// The rainbow kitty ribbon: `cursor_glow/tests/rainbow.rs`.
mod rainbow;
/// Wrap re-anchors, bridged moves and box growth: `cursor_glow/tests/reanchor.rs`.
mod reanchor;
/// The resident-state cap and its derived model: `cursor_glow/tests/resident_cap.rs`.
mod resident_cap;
/// The review's findings on the stall, fold and park: `cursor_glow/tests/review.rs`.
mod review;
/// Landing rings and halo culling: `cursor_glow/tests/rings.rs`.
mod rings;
/// The seam's licence-class laws — one-shots spend once, a glyph
/// supersedes nothing whose echo is owed, one kill one `Kill`, a held
/// park is a wake source: `cursor_glow/tests/seam_laws.rs`.
mod seam_laws;
/// Typing sound cues and the key-time ledger: `cursor_glow/tests/sound.rs`.
mod sound;
/// The stall: keys typed into a stalled event loop: `cursor_glow/tests/stall.rs`.
mod stall;
/// The fold after a stall: `cursor_glow/tests/stall_fold.rs`.
mod stall_fold;
/// Fresh-ink pop, the typing wake and the `trail` status row: `cursor_glow/tests/status.rs`.
mod status;
/// The stray a fresh hint licenses: `cursor_glow/tests/stray.rs`.
mod stray;
/// Style-switch crossfades, the non-fire crown and pooled bolts: `cursor_glow/tests/style_switch.rs`.
mod style_switch;
/// Glyph tint and light-theme ink: `cursor_glow/tests/tint.rs`.
mod tint;
/// Trail Packs (the `GlowStyle::Custom` data path): `cursor_glow/tests/trail_pack.rs`.
mod trail_pack;
/// Typed stamps across gestures, hidden carets and program rows: `cursor_glow/tests/typed_stamps.rs`.
mod typed_stamps;
/// Unknown-width inserts and their orphan keys: `cursor_glow/tests/unknown_insert.rs`.
mod unknown_insert;
/// Rainbow kitty v2 at the seam: `cursor_glow/tests/v2_seam.rs`.
mod v2_seam;
/// The wake census at the seam: `cursor_glow/tests/wake_census.rs`.
mod wake_census;
/// The water style: `cursor_glow/tests/water.rs`.
mod water;
/// The ribbon's witness rows and the exact-key credit ring: `cursor_glow/tests/witness.rs`.
mod witness;
/// The word move sounds every time: `cursor_glow/tests/word_nav.rs`.
mod word_nav;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// Type `text` at the owner's measured cadence (85 ms per key) from the
/// caret at `(row, col)`, each key echoed 3 ms after its press (a Space
/// carries its class, so the ribbon's moved-word relay has the witness
/// it keys on). Returns the clock of the last echo.
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

// C5 ("single source"): the test pinning `prefs::CURSOR_TRAIL_STYLES` to
// `GlowStyle::parse` lives beside the list in aterm-gui's `prefs.rs` — the
// list is a GUI picker domain, so the pin moved with it at extraction time.

/// Press one plain Backspace and let its one-column retreat be seen — the
/// LICENSE-era shape of what used to be an exact delete candidate: the
/// press stamps `quench_hint` (which licenses the retreat AND classifies it
/// as a deletion) and `bs_poof_hint` (the poof's own license), and the row
/// probe carries the erase evidence the poof lane reads directly.
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
    // Identity layout: origin 0 + win == grid extents (320×96) ⇒ every
    // emission is byte-identical to the historical pad-relative contract,
    // so this whole suite doubles as the window-space identity proof.
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

// ---- LIVING FLOW ------------------------------------------------------
//
// The channel's own gates. Four claims, and they are the four things the
// owner's ask and this file's standing rulings between them require:
// it MOVES, it does not FLICKER, it does not RE-COLOUR, and it SPENDS NO
// LIGHT — plus the reduced-motion stand-down.

fn cfg(style: GlowStyle, enabled: bool) -> GlowConfig {
    GlowConfig {
        enabled,
        // The SPECTRUM face — v0.28's shipped default, and the one the
        // whole-frame golden pins for `classic`.
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
        // Water and rainbow kitty are beam-less (WATER-1 / the kitty draws its
        // own continuous body);
        // every other enum style shows its beam. (The comet/lumen raw-string
        // distinction only exists in the GUI resolver, not at this enum helper.)
        beam: !matches!(style, GlowStyle::Water | GlowStyle::RainbowKitty),
        head_dx: 0.5,
        pack: None,
        // This enum-only fixture has no raw spelling to resolve, so it
        // starts on the DEFAULT tall presentation. Tests of shipping
        // presentation resolution use
        // `cfg_for_style_name`; body-specific tests set the field
        // deliberately.
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

/// Codex's 57-row screen (`ESC[{vt};57r`), 0-based: the measured
/// viewport tops (11, 16, 18, 22, 24, …, 51), the composer at row 13
/// after the first Enter, the pinned composer at row 54 — the band
/// tests are stated on it.
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
/// pixels for the bloomed stack to carry all six anchors, so the streak's
/// slab law can be measured rather than inferred.
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

/// **THE SHARED TEST COMPOSITOR** — the renderer's real painter order, its
/// real `add_sat` saturation, and its real opaque caret, in one place both
/// on-glass gates read.
///
/// **WHY IT EXISTS.** Two gates in this file each grew their OWN compositor
/// and each of them was structurally unable to see the defect it was
/// standing guard over.
///
/// * The ledge gate composited the ribbon's `under` stream ALONE — held to
///   [`RAINBOW_UNDER_COV_CAP`] — discarded `out` and `halos`, and then
///   multiplied every measured delta by `RAINBOW_FRAME_PEAK / cap`. **A
///   LINEAR RESCALE CANNOT REPRESENT `add_sat` CLIPPING**: the on-glass
///   frame reaches `255` and STOPS, so the shoulder a rescale predicts at
///   `238 · |p'|` is on glass a step down from a flat plateau, and the two
///   numbers are unrelated. Its `assert_eq!(ledges, 0)` really asserted "no
///   raw delta ≥ 18".
/// * The brightest-pixel gate composed the caret's own halo ON TOP of the
///   opaque block. Glass paints the block LAST ([`draw_cursor`] is the final
///   pass, and at the default opacity the fill REPLACES the cell), so that
///   halo is invisible where the gate was crediting it to the caret.
///
/// **THE ORDER IS THE RENDERER'S**, read off `composite_free` and the
/// post-passes that follow it:
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
/// Nothing here is a model of the renderer: `add_sat`, `premul_rgb`,
/// `over_rgb`, `halo_row_ny` and `halo_weight` are the renderer's own
/// exported functions, so a change to how light composites moves this with
/// it.
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

    /// A [`GlowQuad`] stream — `draw_flat_add`'s contract, PER QUAD.
    ///
    /// [`aterm_render::over_premul`] is the one equation both modes are and
    /// is byte-identical to `add_sat` at `alpha == 0`, so this is the
    /// renderer's own walk for an additive stream AND for the rainbow bed's
    /// source-over one. Reading `q.alpha` here is not a refinement: with
    /// `add_sat` hardcoded, every oracle in this file that composites the
    /// bed would be measuring a frame the renderer does not draw — brighter
    /// than the real one by the ground the bed displaces — and would go on
    /// certifying ink lift, plateaus and whites against it.
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

    /// A [`RainHalo`] stream — `draw_radial_add`'s contract, including its
    /// Add-before-Over mode split and the integer elliptical falloff both
    /// presenters compute byte-for-byte.
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

    /// Phase C — the GLYPH INK, laid over the finished under-ink stack and
    /// UNDER every additive stream that follows. A solid block of `fg`
    /// across the cell's x-height is the worst case the composition rule is
    /// stated against: whatever an ink pixel reads afterwards, the effect
    /// put it there.
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

    /// `draw_cursor`, LAST: at the default opacity the block cursor's fill
    /// REPLACES the cell's pixels, so nothing the effect laid inside the
    /// caret cell survives and the caret's own halo lights only what hangs
    /// OUTSIDE it.
    fn stamp_caret(&mut self, row: u16, col: u16, fill: u32) {
        let (x0, y0, x1, y1) = self.cell_rect(row, col);
        for y in y0.max(0)..y1.min(self.h as i32) {
            for x in x0.max(0)..x1.min(self.w as i32) {
                self.px[y as usize * self.w + x as usize] = fill;
            }
        }
    }
}

/// A quad is a RIBBON row (v0.31 smooth gradient) if it is a SATURATED
/// SPECTRUM colour: max−min channel above a floor AND a genuinely low
/// channel. This distinguishes the rainbow gradient (every spectrum hue has
/// a channel near 0) from BOTH the pure-white starfield (R≈G≈B) and the
/// warm-white sparkle stars (0xFFF2C0 — all channels high), neither of which
/// is ribbon or subject to the ribbon coverage cap. The chroma floor is a
/// modest 12: the calmer cold birth-brightness curve (`0.45 + 0.85·disp`)
/// plus the near-cursor head ramp mean a FRESHLY-laid cold ribbon cell (u≈0)
/// renders faint at the test-config intensity (0.7) — it is still a saturated
/// spectrum quad, just dim — so the floor stays below that to detect PRESENCE
/// while a pure/warm white (chroma 0, or all channels ≥100) is still excluded.
fn is_ribbon_quad(color: u32) -> bool {
    let (r, g, b) = ((color >> 16) & 0xff, (color >> 8) & 0xff, color & 0xff);
    let hi = r.max(g).max(b);
    let lo = r.min(g).min(b);
    hi.saturating_sub(lo) > 12 && lo < 100
}

/// The distinct cell columns (`(x - origin_x) / cw`) a ribbon occupies on
/// `row` — the smooth-gradient replacement for the old crisp-band cell
/// count. Origin-aware (quads are window-absolute); identity-safe at
/// `origin_x == 0`.
/// The columns of `row` that own v2 ribbon light — a laid glyph cell
/// (`Engine::field_at`), which is what "the ribbon lit this cell" means
/// since v1's per-cell sparks went. v2 lays the GLYPH cells, never the
/// caret's own.
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
// Trail Packs (the `GlowStyle::Custom` DATA path).
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
// RAINBOW KITTY v2 — the seam (`RAINBOW-KITTY-V2.md` §17.2, D14).
// ===================================================================

/// THE DELETION ORACLE'S SCRIPT (§17.3 phase 7): one scripted session
/// rich enough to reach every v2 producer — a probed row so stardust can
/// be born, ten typed keys at human cadence, an end-of-line fling, three
/// more keys, two backspaces, a kill, a home fling, a Return, four keys
/// on the new row, a five-cell word hop, and the decay tail out to 5 s —
/// folding EVERY observable the host reads each frame: the fingerprint,
/// the `out` / `under` quad bytes, every halo field, every cue kind, the
/// caret seam (`caret_paint`, `caret_flare_at`, `rainbow_field`,
/// `rainbow_head_rgb`), the spine (`momentum_display`, `rainbow_phase`,
/// `typing_momentum`), the scheduler (`needs_frame_cadence`,
/// `next_change_deadline`, `is_active`), the companion impulse and the
/// motion pulse, and the v2 status row. `reduced` drives the posture
/// seam. Deterministic given the relative schedule.
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
        acc = fp_fold(acc, fp);
        for q in out.iter().chain(glow.under_quads()) {
            for w in q.damage_words() {
                acc = fp_fold(acc, w);
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
                acc = fp_fold(acc, w);
            }
        }
        acc = fp_fold(acc, glow.sound_cues.len() as u64);
        for cue in glow.drain_sound_cues() {
            for b in format!("{cue:?}").bytes() {
                acc = fp_fold(acc, u64::from(b));
            }
        }
        acc = fp_fold(acc, u64::from(glow.caret_paint(at).to_bits()));
        acc = fp_fold(
            acc,
            glow.caret_flare_at().map_or(u64::MAX, |f| {
                at.saturating_duration_since(f).as_micros() as u64
            }),
        );
        acc = fp_fold(acc, u64::from(glow.rainbow_field().to_bits()));
        acc = fp_fold(
            acc,
            u64::from(glow.rainbow_head_rgb(cfg).unwrap_or(u32::MAX)),
        );
        acc = fp_fold(acc, u64::from(glow.momentum_display().to_bits()));
        acc = fp_fold(acc, u64::from(glow.rainbow_phase().to_bits()));
        acc = fp_fold(acc, u64::from(glow.typing_momentum(at).to_bits()));
        acc = fp_fold(acc, u64::from(glow.needs_frame_cadence()));
        acc = fp_fold(
            acc,
            glow.next_change_deadline(at, Duration::from_millis(8))
                .map_or(u64::MAX, |d| {
                    d.saturating_duration_since(at).as_micros() as u64
                }),
        );
        acc = fp_fold(acc, u64::from(glow.is_active()));
        acc = fp_fold(
            acc,
            match glow.take_companion_impulse() {
                None => 0,
                Some(CompanionImpulse::Land) => 1,
                Some(CompanionImpulse::Wince) => 2,
                Some(CompanionImpulse::Delight) => 3,
                Some(CompanionImpulse::Meteor { dir, t0, t_flight }) => {
                    let age = at.saturating_duration_since(t0).as_micros() as u64;
                    fp_fold(
                        fp_fold(fp_fold(4, dir as u64), age),
                        t_flight.as_micros() as u64,
                    )
                }
            },
        );
        for b in format!("{:?}", glow.take_cursor_cat_motion_pulse().map(|p| p.kind)).bytes() {
            acc = fp_fold(acc, u64::from(b));
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
                acc = fp_fold(acc, w);
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

/// THE DELETION GOLDENS: [`deletion_script`]'s fold for every variant —
/// the nine built-ins dark, then rainbow kitty light, underline and
/// reduced-motion — with v2 unconditional for the kitty. The EIGHT
/// non-kitty rows are the pre-seam engine's bytes, captured on
/// `19ffec73a` and never moved since: v2 never engages for those styles,
/// and `the_other_nine_styles_are_byte_identical_with_v2_unconditional`
/// is the hard law that says so. The FOUR kitty rows (dark, light,
/// underline, reduced) are the current v2's bytes on the merged tree,
/// pinned by `rainbow_kitty_is_byte_identical_to_the_seam_era_v2` so any
/// drift is a decision, not a surprise.
///
/// NOT RE-BAKED AT THE 0.86 CANDIDATE'S FINAL CATCH-UP WITH MAIN
/// (2026-09-15), and that is a measurement, not an omission. These four
/// kitty numbers are the candidate's, set when Rainbow Path v3 last
/// caught up with main; the catch-up before the cut moved none of them.
/// Main's incoming round carries this table with the SAME four kitty
/// rows it had at the merge base — every commit in it is a refactor, a
/// doc, a simplification or a test, and none moved a v2 byte — so the
/// candidate's numbers are the only ones that ever described the merged
/// tree, and the three-way merge kept them because only one side had
/// moved. Read off THIS tree's own run to confirm it rather than assume
/// it: all twelve rows green, `checked == 4` (the pin is not vacuous),
/// and the eight non-kitty rows green under
/// `the_other_nine_styles_are_byte_identical_with_v2_unconditional` —
/// the control that says the other nine trail styles did not move
/// either.
///
/// **RE-BAKED 2026-09-16 — THE CROSSING'S SHARE CAP.** All four kitty
/// rows moved and the eight non-kitty rows held. The cause is the ONE
/// SPECTRUM's table and nothing in this file:
/// [`crate::spectrum::SPECTRUM_CROSSING_SHARE_CAP`] holds the green→blue
/// leg to `167` of the table's `510` steps so that yellow keeps its share
/// of the arc, which re-spends every leg and so moves every entry of
/// `SPECTRUM_LUT` from index `50` on. Every quad a rainbow emitter writes
/// moves with it. That is the change, not a side effect of one, and no
/// smaller re-bake exists: a kitty row IS the arc's bytes.
///
/// THE CONTROLS THAT HELD, read off this tree's own run:
///
/// * the eight non-kitty rows of this table are unchanged to the bit, and
///   `the_other_nine_styles_are_byte_identical_with_v2_unconditional` is
///   green over them — the other trail styles do not read the arc;
/// * `licensed_typed_parity`'s nine-style golden moved **only entry 2**
///   (RainbowKitty); the other eight are byte-identical, which is that
///   test's own non-vacuity control and it still separates them;
/// * `checked == 4`, so the pin is not vacuous, and
///   `the_flat_spelling_collapses_every_comet_branch_byte_for_byte`'s own
///   `assert_ne!` still separates the flat body from the default one.
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
///
/// (MERGE, 0.86: main's re-bake law above SUPERSEDES the candidate's
/// running list of every past bake, which stood here and is now git's —
/// `git log -L` on this table still reads it, entry for entry, and the
/// law states what each entry had to show. The candidate loses no claim
/// by it: the one paragraph a bake must leave behind is the current
/// one, and that is the paragraph above. The table's name for the flat
/// twins is `FLAT_GOLDENS`, not `PRE_COMET_GOLDENS` — see its doc.)
/// **RE-BAKED 2026-09-15 (the four kitty rows only), AND THE REASON IS
/// THE ARC ITSELF.** `crate::spectrum`'s table moved: the green→blue
/// crossing's authored roof, its four pacing knots and — the part that
/// moves every entry — its exemption from the perceptual pace were
/// deleted as the retired no-cyan ruling's last machinery, so all seven
/// anchors landed at new indices (`[0, 63, 142, 258, 394, 471, 510]` ->
/// `[0, 50, 112, 204, 419, 479, 510]`) and every kitty row that reads a
/// colour off the arc folds a different byte. The eight non-kitty rows
/// are UNCHANGED, which is this pin doing its job: the deletion reached
/// the rainbow family and nothing else.
/// **RE-BAKED 2026-09-16, THE EXHAUST (§33), and the four kitty rows
/// stand where its last law left them.** The round's laws in order —
/// `Stardust::sow_exhaust` (one transient grain per key), the sky's share
/// `rk::stardust::STREAM_SHARE_CW` 0.35 → 1.05, the blank-frontier clamp
/// reading the band's row off the flight's own pixels, the grain yielding
/// within `rk::stardust::EXHAUST_HEADROOM` of the cap, and the grain
/// needing the cell behind the caret proved blank — moved dark, light and
/// underline; **reduced never moved**, which is the decomposition: the
/// exhaust's first gate is `cfg.reduced_motion` and the slipstream is a
/// position law `Star::pos` ignores under it. The last two laws moved
/// underline (and, for the headroom, dark) alone: the script's ~28 cps
/// tail sits in the headroom band, and under `underline` every grain was
/// a sub-cell twitch behind the just-echoed glyph. Each was decomposed
/// by switching it off and reading the previous number. The eight
/// non-kitty rows never moved. See `FLAT_GOLDENS` for the same bakes.
/// **RE-BAKED 2026-09-21: ribbon quads carry a per-column gradient**
/// (`GlowQuad::color2` — `ribbon_beam` samples the colour at each slab's
/// two edges and the quad ramps between them, so a whole-cell slab is no
/// longer a flat block; the coverage is still the slab's centre sample).
/// A gradient quad folds a THIRD damage word carrying its right edge, so
/// all four kitty rows move (dark `13_957_692_697_408_025_534` →
/// `383_721_728_202_092_274`, light `16_474_978_137_331_214_836` →
/// `5_202_336_454_404_496_754`, underline `11_627_668_675_227_657_479`
/// → `544_488_007_139_041_733`, reduced `11_365_708_919_978_438_696` →
/// `13_592_174_565_784_071_210`); the eight non-kitty rows fold flat
/// quads whose two words are the pre-gradient pair bit for bit, and
/// `the_other_nine_styles_are_byte_identical_with_v2_unconditional` held
/// them unchanged on the same run.
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
    ("RainbowKitty reduced", 13_592_174_565_784_071_210),
];

/// THE FLAT SPELLING'S GOLDENS: the four RainbowKitty rows of
/// [`DELETION_GOLDENS`] under the `rainbow kitty flat` spelling
/// (`GlowConfig::ribbon_flat`) — the owner's A/B control for the comet
/// body. Every comet branch in `ribbon.rs` (the comet profile, the vivid
/// rail, the from-the-hand wipe) collapses to the flat expression under
/// the flag, so
/// `the_flat_spelling_collapses_every_comet_branch_byte_for_byte` reads
/// these four with the flag on, and its `assert_ne!` proves the default
/// body moves every row away from them — the pin is never vacuous.
///
/// These rows move with [`DELETION_GOLDENS`] on every change that is NOT
/// a comet branch — the seam feeds both spellings the same events, and
/// the landing, the attach, the boundary law and the soft end stand
/// under the flat body too — and hold when only a comet branch changes.
/// Re-read them whenever the kitty rows are re-baked; the history is in
/// `git log -L` on this table.
///
/// THE TABLE IS NO LONGER A TIME CAPSULE, and that is why it is named
/// `FLAT_GOLDENS` and not `PRE_COMET_GOLDENS`. These four stopped being
/// the bytes of the tree as it stood before the comet landed, because
/// the v3 merge moves laws the flag does not gate: the phrase rest
/// (0.75 s → 0.90 s of grace at the floor), the erase/kill phrase hold,
/// the per-row retract target and the overlay wake on its flight clock.
/// The FLAG'S OWN CLAIM is unchanged and still measured here — under it
/// the four rows fold to THESE numbers, under the default body to four
/// others — so the comet's A/B gate is still byte-exact.
///
/// RE-BAKED 2026-09-15 WITH [`DELETION_GOLDENS`], AND FOR ITS ONE
/// REASON. The green→blue re-pace ([`rk::ribbon::CROSS_PACE`]) is not a
/// comet branch — the flag gates the comet profile, the vivid rail and
/// the from-the-hand wipe, not WHICH STOP a walk position resolves to —
/// so all four flat rows move with the four default ones, which is
/// exactly what the paragraph above says must happen. Measured the same
/// way: with the walk forced to the identity these four read
/// `13_180_914_282_442_234_019`, `16_869_065_810_935_775_429`,
/// `6_565_210_254_762_879_397` and `6_253_451_302_876_110_283` — the
/// bytes v0.86.0 shipped, to the bit — so the walk is the whole of the
/// move here too. The `assert_ne!` control in
/// `the_flat_spelling_collapses_every_comet_branch_byte_for_byte` is
/// green over the new numbers: the comet body still folds every one of
/// these four rows somewhere else, so the collapse is still a
/// measurement and not a vacuous pin.
///
/// (The catch-up note below is kept for the history it records.)
///
/// NOT RE-BAKED AT THE 0.86 CANDIDATE'S FINAL CATCH-UP WITH MAIN
/// (2026-09-15), for the same measured reason [`DELETION_GOLDENS`]
/// was not: main's incoming round moved no byte of either spelling, so
/// these four stand where the candidate's last catch-up left them.
/// Confirmed on this tree's own run, not assumed — all four fold to
/// these numbers under the flag, and the `assert_ne!` control below is
/// green over them, so the collapse the table pins is still a
/// measurement and not a vacuous pin.
///
/// (MERGE, 0.86: main's two paragraphs at the head are its statement of
/// this table and are kept verbatim but for the name; the candidate's
/// rename and its reason are kept because the code uses `FLAT_GOLDENS`
/// and "pre-comet" is no longer true of these bytes.)
///
/// RE-BAKED 2026-09-16 WITH THE DEFAULT ROWS, THE EXHAUST (§33). The
/// exhaust is a STARDUST population and the flat spelling is a RIBBON
/// BODY, so the two are orthogonal: every law of the round moves the
/// same rows here as on [`DELETION_GOLDENS`] (dark, light and underline
/// for the exhaust and the sky's share; underline alone for the frontier
/// clamp and the blank-cell gate; dark and underline for the headroom),
/// with reduced byte-identical on both tables throughout. The
/// `assert_ne!` control in
/// `the_flat_spelling_collapses_every_comet_branch_byte_for_byte` is
/// green over every bake: the comet body still folds all four rows
/// somewhere else, so the collapse is a measurement and not a vacuous
/// pin.
/// **RE-BAKED 2026-09-21: ribbon quads carry a per-column gradient** (the
/// same cause as [`DELETION_GOLDENS`]'s bake of that date — the flat
/// spelling collapses the comet BODY, not the colour walk, so its slabs
/// ramp too): dark `14_766_156_058_099_546_727` →
/// `17_890_278_511_770_188_755`, light `15_009_626_345_715_177_661` →
/// `6_430_388_712_169_283_571`, underline `18_121_524_929_398_043_921` →
/// `15_379_506_098_573_732_577`, reduced `9_256_275_961_493_287_119` →
/// `3_073_631_414_449_164_399`. The `assert_ne!` control still holds.
const FLAT_GOLDENS: [(&str, u64); 4] = [
    ("RainbowKitty dark", 17_890_278_511_770_188_755),
    ("RainbowKitty light", 6_430_388_712_169_283_571),
    ("RainbowKitty underline", 15_379_506_098_573_732_577),
    ("RainbowKitty reduced", 3_073_631_414_449_164_399),
];

// =======================================================================
// THE STALL (the owner's "I t" screenshot, attributed by
// measurement). Real Claude Code 2.1.268 in a private headless instance
// of the 0.82.0 build: keys typed into a 2.7 s event-loop stall came back
// as ONE merged frame, judged `declined no-credits origin=27,5
// target=27,35`, the ledger bridged nothing, and the glass read
// `..###..............................+######` — "I t" lit, thirty dark
// cells, the caret lit. Every dark cell was a key the owner pressed. A
// press is IN FLIGHT until its row echoes or an observed edge it cannot
// explain forgets it; it does not expire on a 2 s wall clock while its
// row stays silent.
// =======================================================================

/// One beat PAST THE PARK WINDOW: a held park ([`HeldPark`]) is judged on
/// the first tick more than [`CursorGlow::TYPE_HINT_FRESH`] after it, so
/// a verdict a park defers — a keyless backward hop's refusal and its
/// forget, a re-anchor's flush — lands on a tick this far after the move.
fn past_park_window() -> Duration {
    Duration::from_secs_f32(CursorGlow::TYPE_HINT_FRESH) + Duration::from_millis(50)
}

/// A 100-column grid — the measured instance's width — so a 50-cell
/// batch fits one row.
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

/// The harness's pre-roll: a seating tick at `(row, 2)`, then three
/// glyphs typed and echoed per key ([`type_echoed`]), leaving the caret
/// at `(row, 5)` on a live three-cell cohort. Returns the clock of the
/// last echo.
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
// THE FOLD AFTER A STALL (S8): a key whose echo WRAPS the
// row, observed after a stall. Measured on c45b13300 (tui model): the
// stalled wrap `11,99 -> 12,0` declined `no-fresh-hint` (the unpaid arm
// needed `cr == pr`), the row change cleared the ledger, and the last
// column stayed dark for good.
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
// THE PARK: Ink's rewrite observed as TWO sync brackets ~40 ms apart —
// the caret parked at the start of the input,
// then the row rewritten to its end plus the new glyph. Measured on
// c45b13300: the park licensed as a typed re-anchor (the stamp spent,
// the mirror moved to col 2, the key's `Typed` replay laying the PROMPT
// cell), the return refused `no-credits` / `no-fresh-hint`, six forgets,
// half the row dark.
// =======================================================================

/// One ring row as `(reason, licence, origin, target)`.
type RingRow = (&'static str, &'static str, (u16, u16), (u16, u16));

fn ring_rows(glow: &CursorGlow) -> Vec<RingRow> {
    glow.admission_log()
        .map(|r| (r.reason, r.licence, r.origin, r.target))
        .collect()
}

// ---- THE WORD MOVE SOUNDS EVERY TIME ----------------------
//
// Option+Left / Option+Right (and Ctrl+arrow, Alt-b/f) have NO key-time
// cue: the keyed seam opens only for a typed glyph, and D11 vetoes a nav
// pre-cue because a no-op Ctrl-E must stay silent. So a word move's WHOLE
// sound is echo-born — minted when this seam licenses the observed caret
// delta — and every refusal in the seam is a word hop the owner heard as
// "sometimes it just doesn't play".
//
// The five shapes below are the owner's two shells plus the controls the
// anti-stray law demands: zsh (a caret-only echo), an Ink-style TUI (hide,
// rewrite the line, place the caret, show), a fast pair, a program move
// with NO key behind it, and the word KILL.

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
