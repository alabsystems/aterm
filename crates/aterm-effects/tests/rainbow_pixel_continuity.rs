// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic pixel-continuity scanner across rainbow cursor trail paths.
//!
//! Renders the real production quad + `GlowRamps` + `RainHalo` streams onto an
//! exact integer-pixel RGB buffer (`theme_bg` -> `glow_under` -> `cursor_glow_add`
//! -> `glow_halo`, including `shade_word_icons` and `veil_pet_letters`) and scans
//! horizontal pixel columns and character cells between the active trail tail and
//! the cursor/pet head for unexpected dark holes or seam pinches.

use crate::common::{FrameHold, geom, note_blink, observe_ribbon_rows_where};
use aterm_core::render::{COLOR_UNSET, GlowQuad, GlowRamps, HaloMode, RainHalo, RenderInput};
use aterm_core::terminal::{ClockReading, RenderCell, Terminal};
use aterm_effects::cursor_fx::{CursorFxInput, TrailColor};
use aterm_effects::cursor_glow::{CURSOR_WITNESS_ROWS, CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::cursor_trail::TrailConfig;
use aterm_effects::host::{hidden_caret_withheld, motion_caret, motion_print_anchor};
use aterm_effects::pipeline::{CursorStep, EffectsPipeline, RowProbe};
use aterm_effects::rainbow_kitty::TypedClass;
use std::time::{Duration, Instant};

const BACKSPACE: char = '\u{8}';
const SYNC_BEGIN: &str = "\x1b[?2026h";
const SYNC_END: &str = "\x1b[?2026l";

/// Per-frame pixel canvas for a slice of rows `[row_top..row_end)` in window coordinates.
struct PixelBand {
    width: usize,
    height: usize,
    y0: usize,
    bg: [u8; 3],
    px: Vec<[u8; 3]>,
}

impl PixelBand {
    fn new(width: usize, y0: usize, height: usize, theme_bg: u32) -> Self {
        let bg = [
            ((theme_bg >> 16) & 0xFF) as u8,
            ((theme_bg >> 8) & 0xFF) as u8,
            (theme_bg & 0xFF) as u8,
        ];
        Self {
            width,
            height,
            y0,
            bg,
            px: vec![bg; width * height],
        }
    }

    fn blend_quad(&mut self, q: GlowQuad) {
        if q.w == 0 || q.h == 0 {
            return;
        }
        let x0 = usize::from(q.x).min(self.width);
        let x1 = usize::from(q.x)
            .saturating_add(usize::from(q.w))
            .min(self.width);
        let qy0 = usize::from(q.y);
        let qy1 = qy0.saturating_add(usize::from(q.h));
        let by0 = qy0.max(self.y0);
        let by1 = qy1.min(self.y0 + self.height);
        if x0 >= x1 || by0 >= by1 {
            return;
        }
        let sr = ((q.color >> 16) & 0xFF) as u16;
        let sg = ((q.color >> 8) & 0xFF) as u16;
        let sb = (q.color & 0xFF) as u16;
        if sr == 0 && sg == 0 && sb == 0 && q.alpha == 0 {
            return;
        }
        let a = u16::from(q.alpha);
        for y in (by0 - self.y0)..(by1 - self.y0) {
            let row_off = y * self.width;
            for p in &mut self.px[row_off + x0..row_off + x1] {
                if a == 0 {
                    // GlowBlend::Add: premultiplied saturating additive light.
                    p[0] = (u16::from(p[0]) + sr).min(255) as u8;
                    p[1] = (u16::from(p[1]) + sg).min(255) as u8;
                    p[2] = (u16::from(p[2]) + sb).min(255) as u8;
                } else {
                    // GlowBlend::Over: premultiplied source-over paint.
                    let inv = 255 - a;
                    p[0] = (sr + (u16::from(p[0]) * inv + 127) / 255).min(255) as u8;
                    p[1] = (sg + (u16::from(p[1]) * inv + 127) / 255).min(255) as u8;
                    p[2] = (sb + (u16::from(p[2]) * inv + 127) / 255).min(255) as u8;
                }
            }
        }
    }

    fn blend_stream(&mut self, quads: &[GlowQuad], ramps: &GlowRamps) {
        if ramps.is_empty() {
            for &q in quads {
                self.blend_quad(q);
            }
            return;
        }
        let mut expanded = Vec::with_capacity(ramps.laid(quads.len()));
        ramps.expand_into(quads, &mut expanded);
        for q in expanded {
            self.blend_quad(q);
        }
    }

    fn blend_halos(&mut self, halos: &[RainHalo]) {
        for &h in halos {
            if h.w == 0 || h.h == 0 || h.rx == 0 || h.ry == 0 {
                continue;
            }
            let x0 = usize::from(h.x).min(self.width);
            let x1 = usize::from(h.x)
                .saturating_add(usize::from(h.w))
                .min(self.width);
            let hy0 = usize::from(h.y);
            let hy1 = hy0.saturating_add(usize::from(h.h));
            let by0 = hy0.max(self.y0);
            let by1 = hy1.min(self.y0 + self.height);
            if x0 >= x1 || by0 >= by1 {
                continue;
            }
            let cr = ((h.color >> 16) & 0xFF) as f32;
            let cg = ((h.color >> 8) & 0xFF) as f32;
            let cb = (h.color & 0xFF) as f32;
            let cx = f32::from(h.cx);
            let cy = f32::from(h.cy);
            let rx = f32::from(h.rx);
            let ry = f32::from(h.ry);
            for wy in by0..by1 {
                let dy = (wy as f32 + 0.5 - cy) / ry;
                let row_off = (wy - self.y0) * self.width;
                for wx in x0..x1 {
                    let dx = (wx as f32 + 0.5 - cx) / rx;
                    let d2 = dx * dx + dy * dy;
                    if d2 >= 1.0 {
                        continue;
                    }
                    let t = 1.0 - d2.sqrt();
                    let w = t * t * (3.0 - 2.0 * t);
                    let p = &mut self.px[row_off + wx];
                    match h.mode {
                        HaloMode::Add => {
                            p[0] = (f32::from(p[0]) + cr * w).round().clamp(0.0, 255.0) as u8;
                            p[1] = (f32::from(p[1]) + cg * w).round().clamp(0.0, 255.0) as u8;
                            p[2] = (f32::from(p[2]) + cb * w).round().clamp(0.0, 255.0) as u8;
                        }
                        HaloMode::Over => {
                            let inv = 1.0 - w;
                            p[0] = (cr * w + f32::from(p[0]) * inv).round().clamp(0.0, 255.0) as u8;
                            p[1] = (cg * w + f32::from(p[1]) * inv).round().clamp(0.0, 255.0) as u8;
                            p[2] = (cb * w + f32::from(p[2]) * inv).round().clamp(0.0, 255.0) as u8;
                        }
                    }
                }
            }
        }
    }

    /// Maximum RGB channel delta from `theme_bg` across all scanlines of pixel column `x`.
    fn column_lift(&self, x: usize) -> u8 {
        if x >= self.width {
            return 0;
        }
        let mut best = 0u8;
        for y in 0..self.height {
            let p = self.px[y * self.width + x];
            let dr = p[0].abs_diff(self.bg[0]);
            let dg = p[1].abs_diff(self.bg[1]);
            let db = p[2].abs_diff(self.bg[2]);
            best = best.max(dr.max(dg).max(db));
        }
        best
    }

    fn all_column_lifts(&self) -> Vec<u8> {
        (0..self.width).map(|x| self.column_lift(x)).collect()
    }
}

/// Composite a single terminal row (plus its subtle rail margin below) from `EffectsPipeline`.
fn composite_pipeline_row(fx: &EffectsPipeline, g: Geom, theme_bg: u32, row: u16) -> PixelBand {
    let glow = &fx.cursor().glow;
    let y0 = usize::from(g.origin_y) + usize::from(row) * g.ch;
    let rail_extra = (g.ch / 3).max(2);
    let win_h = usize::from(g.win_h).max(y0 + g.ch);
    let height = (g.ch + rail_extra).min(win_h.saturating_sub(y0)).max(1);
    let width = g.cols * g.cw;
    let mut band = PixelBand::new(width, y0, height, theme_bg);
    band.blend_stream(glow.under_quads(), glow.under_ramps());
    let add_laid = fx.cursor().glow_laid();
    band.blend_stream(&add_laid, &GlowRamps::EMPTY);
    band.blend_halos(glow.halos());
    band
}

/// Composite a single terminal row from a `RenderInput` after `EffectsPipeline::apply`.
fn composite_render_input_row(input: &RenderInput, g: Geom, theme_bg: u32, row: u16) -> PixelBand {
    let y0 = usize::from(g.origin_y) + usize::from(row) * g.ch;
    let rail_extra = (g.ch / 3).max(2);
    let win_h = usize::from(g.win_h).max(y0 + g.ch);
    let height = (g.ch + rail_extra).min(win_h.saturating_sub(y0)).max(1);
    let width = g.cols * g.cw;
    let mut band = PixelBand::new(width, y0, height, theme_bg);
    band.blend_stream(&input.glow_under, &input.glow_under_ramps);
    band.blend_stream(&input.cursor_glow_add, &input.cursor_glow_add_ramps);
    band.blend_halos(&input.glow_halo);
    band
}

/// Scan `lifts` (`0..cols*cw`) for any interior unpainted pixel-column gap or severe seam pinch,
/// and optionally verify every cell in `must_cover_cols` has continuous pixel coverage.
fn scan_band_gaps(
    lifts: &[u8],
    cw: usize,
    dark_theme: bool,
    must_cover_cols: Option<(u16, u16)>,
) -> Option<String> {
    let lit_thresh: u8 = if dark_theme { 14 } else { 10 };
    let hole_floor: u8 = if dark_theme { 3 } else { 2 };

    // 1. Interior pixel-column gap between leftmost lit pixel and rightmost lit pixel.
    if let (Some(first_lit), Some(last_lit)) = (
        lifts.iter().position(|&v| v >= lit_thresh),
        lifts.iter().rposition(|&v| v >= lit_thresh),
    ) && last_lit > first_lit + 2
    {
        let mut x = first_lit + 1;
        while x < last_lit {
            if lifts[x] <= hole_floor {
                let mut end = x + 1;
                while end < last_lit && lifts[end] <= hole_floor {
                    end += 1;
                }
                let left_peak = lifts[first_lit..x].iter().copied().max().unwrap_or(0);
                let right_peak = lifts[end..=last_lit].iter().copied().max().unwrap_or(0);
                if left_peak >= lit_thresh && right_peak >= lit_thresh {
                    return Some(format!(
                        "pixel hole x={x}..{end} (cols {}..{}, width={}px, min_lift={}) between left_peak={left_peak} (from x={first_lit}) and right_peak={right_peak} (to x={last_lit})",
                        x / cw,
                        (end - 1) / cw,
                        end - x,
                        lifts[x..end].iter().copied().min().unwrap_or(0),
                    ));
                }
                x = end;
            } else {
                x += 1;
            }
        }

        // 2. Interior seam pinch: if a column drops below 20% of BOTH neighbors 1 cell away
        //    while both neighbors are strongly lit (>= 36), flag a seam pinch.
        if last_lit > first_lit + 2 * cw {
            for px in (first_lit + cw)..=(last_lit - cw) {
                let l = lifts[px - cw];
                let r = lifts[px + cw];
                let m = lifts[px];
                let side_min = l.min(r);
                if side_min >= 40 && u16::from(m) * 5 < u16::from(side_min) {
                    return Some(format!(
                        "seam pinch at x={px} (col {}, offset {}px): lift={m} vs left({}px)={l}, right({}px)={r}",
                        px / cw,
                        px % cw,
                        px - cw,
                        px + cw,
                    ));
                }
            }
        }
    }

    // 3. Explicit active-typed cell span check (`col0..col1`).
    check_expected_span(lifts, cw, dark_theme, must_cover_cols)
}

fn check_expected_span(
    lifts: &[u8],
    cw: usize,
    dark_theme: bool,
    must_cover_cols: Option<(u16, u16)>,
) -> Option<String> {
    let hole_floor: u8 = if dark_theme { 3 } else { 2 };
    if let Some((c0, c1)) = must_cover_cols
        && c1 > c0
    {
        let px0 = (usize::from(c0) * cw + cw / 2).min(lifts.len());
        let px1 = (usize::from(c1) * cw)
            .saturating_sub(cw.div_ceil(3))
            .min(lifts.len());
        if px1 > px0 {
            let mut x = px0;
            while x < px1 {
                if lifts[x] <= hole_floor {
                    let mut end = x + 1;
                    while end < px1 && lifts[end] <= hole_floor {
                        end += 1;
                    }
                    return Some(format!(
                        "unpainted typed span x={x}..{end} (cols {}..{}) inside expected live cols {c0}..{c1}; cell_max_lifts={:?}",
                        x / cw,
                        (end - 1) / cw,
                        (c0..c1)
                            .map(|c| {
                                let a = usize::from(c) * cw;
                                let b = (a + cw).min(lifts.len());
                                (c, lifts[a..b].iter().copied().max().unwrap_or(0))
                            })
                            .collect::<Vec<_>>(),
                    ));
                }
                x += 1;
            }
        }
    }

    None
}

/// Check ribbon logical cell custody for interior holes on `row`.
fn scan_ribbon_cell_holes(glow: &CursorGlow, term: &Terminal, row: u16) -> Option<String> {
    let rib = glow.v2_ribbon()?;
    let mut live: Vec<u16> = rib
        .cells()
        .iter()
        .filter(|c| c.row == row && !c.leaving() && c.released_at.is_none())
        .map(|c| c.col)
        .collect();
    live.sort_unstable();
    live.dedup();
    if live.len() < 2 {
        return None;
    }
    for w in live.windows(2) {
        if w[1] > w[0] + 1 {
            let mut chars = Vec::new();
            term.row_cols_into(usize::from(row), &mut chars);
            let text: String = (w[0] + 1..w[1])
                .map(|c| chars.get(usize::from(c)).copied().unwrap_or('?'))
                .collect();
            return Some(format!(
                "ribbon cell hole on row {row}: cols {}..{} ({text:?}) between live {} and {}",
                w[0] + 1,
                w[1] - 1,
                w[0],
                w[1],
            ));
        }
    }
    None
}

fn make_glow_cfg(style: GlowStyle, ribbon_flat: bool, dark_theme: bool) -> GlowConfig {
    let (theme_fg, theme_bg) = if dark_theme {
        (0x00D0_D0D0, 0x0011_1318)
    } else {
        (0x001E_2028, 0x00F7_F5EE)
    };
    GlowConfig {
        classic_mono: false,
        ribbon_tall: !ribbon_flat && style == GlowStyle::RainbowKitty,
        ribbon_flat,
        enabled: true,
        dark_theme,
        theme_fg,
        theme_bg,
        style,
        color: 0x0050_FA7B,
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 0.7,
        audible: false,
        radius: 0.6,
        ring: true,
        beam: false,
        head_dx: 0.5,
    }
}

/// Native shipping host harness (`EffectsPipeline::cursor_step` with production `GlowRamps`).
struct NativeHarness {
    term: Terminal,
    fx: EffectsPipeline,
    cfg: GlowConfig,
    g: Geom,
    t0: Instant,
    now: Instant,
    hold: FrameHold,
    buf: Vec<char>,
    witness: Vec<char>,
    nbrs: [Vec<char>; 2],
    blink_seen: u64,
    last_blink: Option<Instant>,
    ime_text_mode: bool,
    gaps: Vec<String>,
}

impl NativeHarness {
    fn new(
        rows: usize,
        cols: usize,
        cw: usize,
        ch: usize,
        cfg: GlowConfig,
        fold_ramps: bool,
    ) -> Self {
        let g = geom(rows, cols, cw, ch);
        let mut fx = EffectsPipeline::default();
        fx.cursor_mut().glow.fold_glow_ramps(fold_ramps);
        fx.cursor_mut().glow.note_pane_columns(0, cols);
        fx.cursor_mut().glow.note_pane_rows(0, rows);
        let now = Instant::now();
        Self {
            term: Terminal::new(rows as u16, cols as u16),
            fx,
            cfg,
            g,
            t0: now,
            now,
            hold: FrameHold::default(),
            buf: Vec::new(),
            witness: Vec::new(),
            nbrs: [Vec::new(), Vec::new()],
            blink_seen: 0,
            last_blink: None,
            ime_text_mode: false,
            gaps: Vec::new(),
        }
    }

    fn ms(&self) -> u64 {
        self.now.duration_since(self.t0).as_millis() as u64
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.term.process_at(
            bytes,
            ClockReading {
                monotonic: self.now,
                wall_ms: None,
            },
        );
    }

    fn key_press(&mut self, ch: char) {
        self.fx.cursor_mut().cadence.on_keystroke(self.now);
        self.fx.cursor_mut().momentum.on_key(
            self.now,
            aterm_effects::cursor_momentum::MOMENTUM_GLOW_TAU_S,
        );
        if ch == BACKSPACE {
            self.fx.cursor_mut().glow.clear_typed_keeping_hand(self.now);
            self.fx
                .cursor_mut()
                .glow
                .note_backspace_erasing(self.now, Some(1));
            return;
        }
        let class = if ch == ' ' {
            TypedClass::Space
        } else {
            TypedClass::Glyph
        };
        let shifted = ch.is_uppercase() || "!@#$%^&*()_+{}|:\"<>?~".contains(ch);
        if self.ime_text_mode {
            // Mirrors `InputEvent::Text` / `ime.is_some()` in `app_input.rs` where the
            // glyph-less `note_typed_glyph` branch runs.
            self.fx
                .cursor_mut()
                .glow
                .note_typed_glyph(self.now, 1, shifted && ch != ' ', class);
        } else {
            self.fx.cursor_mut().glow.note_typed_expected(
                self.now,
                1,
                shifted && ch != ' ',
                class,
                ch,
            );
        }
        self.fx.cursor_mut().trail.note_typed(self.now);
    }

    fn nav_press(&mut self) {
        self.fx.cursor_mut().glow.clear_typed_keeping_hand(self.now);
        self.fx.cursor_mut().glow.note_motion(self.now);
        self.fx.cursor_mut().trail.note_navigation(self.now);
    }

    fn step_frame(&mut self, check_row: u16, expected_live: Option<(u16, u16)>, tag: &str) {
        if self.hold.withholds(&self.term, self.now) {
            return;
        }
        let c = self.term.cursor();
        let visible = self.term.cursor_visible();
        let cur = motion_caret(
            Some((c.row, c.col)),
            visible,
            hidden_caret_withheld(&self.term),
        );
        let scroll = self
            .fx
            .sync_scroll(self.term.content_scroll_state(), self.now);
        if note_blink(
            &self.term,
            &mut self.fx.cursor_mut().glow,
            &mut self.blink_seen,
            self.now,
        ) {
            self.last_blink = Some(self.now);
            self.fx.cursor_mut().trail.note_repaint_blink(self.now);
        }
        let alt = self.term.is_alternate_screen();
        self.fx.cursor_mut().glow.note_context(alt);
        self.fx.cursor_mut().trail.note_context(alt);

        let probe = if self.term.grid().display_offset() == 0 && !scroll.changed() {
            self.term.row_cols_into(usize::from(c.row), &mut self.buf);
            let [above, below] = &mut self.nbrs;
            if c.row > 0 {
                self.term.row_cols_into(usize::from(c.row) - 1, above);
            }
            if usize::from(c.row) + 1 < self.g.rows {
                self.term.row_cols_into(usize::from(c.row) + 1, below);
            }
            self.fx
                .cursor_mut()
                .glow
                .observe_ribbon_row(c.row, &self.buf);
            observe_ribbon_rows_where::<CURSOR_WITNESS_ROWS>(
                &self.term,
                &mut self.fx.cursor_mut().glow,
                &mut self.witness,
                |r| r != c.row,
            );
            let blink_recent = self
                .last_blink
                .is_some_and(|at| self.now.saturating_duration_since(at) <= Duration::from_secs(1));
            Some(RowProbe {
                row: c.row,
                caret: c.col,
                cols: &self.buf,
                trust: if !alt || blink_recent {
                    aterm_effects::cursor_glow::ProbeTrust::Full
                } else {
                    aterm_effects::cursor_glow::ProbeTrust::ContentOnly
                },
                flanks: Some((
                    (c.row > 0).then_some(above.as_slice()),
                    (usize::from(c.row) + 1 < self.g.rows).then_some(below.as_slice()),
                )),
            })
        } else {
            None
        };

        let (anchor, glyph) = motion_print_anchor(&self.term);
        self.fx.cursor_step(
            &CursorStep {
                input: CursorFxInput {
                    now: self.now,
                    cur,
                    live_viewport: self.term.grid().display_offset() == 0,
                    cursor_style: self.term.cursor_style(),
                    blink_phase: true,
                    live_cursor: self.cfg.color,
                    cursor_color_pinned: false,
                    default_bg: self.cfg.theme_bg,
                    geom: self.g,
                    focused: true,
                    cursor_cell_bg: None,
                    trail_brightness: 1.0,
                    shed_envelope: 1.0,
                    body_allowed: false,
                    momentum_glow: true,
                    user_tinted: false,
                    accent_pinned: false,
                },
                glow: &self.cfg,
                trail: TrailConfig {
                    enabled: false,
                    duration: self.cfg.duration,
                    max_len: self.cfg.length,
                    color: self.cfg.color,
                    intensity: self.cfg.intensity,
                    warmth: 0.0,
                },
                animate: true,
                row_probe: probe,
                pending_keys: 0,
                print_anchor: anchor,
                print_anchor_glyph: glyph,
                caret_drawn: visible,
                frame_cells: None,
                frame_cells_at: (0, 0),
            },
            std::iter::empty(),
        );

        if self.cfg.style == GlowStyle::RainbowKitty
            && let Some(cell_hole) =
                scan_ribbon_cell_holes(&self.fx.cursor().glow, &self.term, check_row)
        {
            self.gaps
                .push(format!("[{tag} t={}ms] {cell_hole}", self.ms()));
        }

        let band = composite_pipeline_row(&self.fx, self.g, self.cfg.theme_bg, check_row);
        let all_lifts = band.all_column_lifts();
        // When `v2_ribbon` owns live (`!leaving()`) cells on `check_row` (already verified
        // contiguous by `scan_ribbon_cell_holes`), scope Check #1/#2 to that live span so neither
        // a detached right-edge `leaving()` retract swoosh nor vertical rise bleed from `row - 1`
        // creates a false multi-cell bridge across blank columns, while Check #3 (`expected_live`)
        // always inspects the full unclipped `all_lifts` row.
        let interior_hole = if let Some(rib) = self.fx.cursor().glow.v2_ribbon() {
            let mut min_live = u16::MAX;
            let mut max_live = 0u16;
            let mut any = false;
            for c in rib.cells() {
                if c.row == check_row && !c.leaving() && c.released_at.is_none() {
                    min_live = min_live.min(c.col);
                    max_live = max_live.max(c.col);
                    any = true;
                }
            }
            if any {
                let x0 = (usize::from(min_live) * self.g.cw).min(all_lifts.len());
                let x1 = (usize::from(max_live + 1) * self.g.cw).min(all_lifts.len());
                scan_band_gaps(&all_lifts[x0..x1], self.g.cw, self.cfg.dark_theme, None)
            } else {
                None
            }
        } else {
            scan_band_gaps(&all_lifts, self.g.cw, self.cfg.dark_theme, None)
        };
        if let Some(px_hole) = interior_hole {
            self.gaps
                .push(format!("[{tag} t={}ms] {px_hole}", self.ms()));
        } else if expected_live.is_some()
            && let Some(cov_hole) = scan_band_gaps(&[], self.g.cw, self.cfg.dark_theme, None)
                .or_else(|| {
                    check_expected_span(&all_lifts, self.g.cw, self.cfg.dark_theme, expected_live)
                })
        {
            self.gaps
                .push(format!("[{tag} t={}ms] {cov_hole}", self.ms()));
        }
    }

    fn advance_us(
        &mut self,
        dt_us: u64,
        check_row: u16,
        expected_live: Option<(u16, u16)>,
        tag: &str,
    ) {
        self.now += Duration::from_micros(dt_us);
        self.step_frame(check_row, expected_live, tag);
    }
}

/// Diff-rendering TUI composer model (mirrors Ink / Codex / Claude Code composers).
struct DiffComposer {
    rows: usize,
    cols: usize,
    base_row: usize,
    screen: Vec<Vec<char>>,
    text: Vec<char>,
    caret: usize,
    visible_caret: bool,
    park_before_settle: bool,
    use_sync: bool,
    bordered: bool,
    tight_border: bool,
    margin: usize,
    top_anchored: bool,
    drawn_top: usize,
    drawn_bottom: usize,
}

type WrappedRows = Vec<(usize, Vec<char>)>;

impl DiffComposer {
    fn new(rows: usize, cols: usize, base_row: usize, visible_caret: bool) -> Self {
        Self {
            rows,
            cols,
            base_row,
            screen: vec![vec![' '; cols]; rows],
            text: Vec::new(),
            caret: 0,
            visible_caret,
            park_before_settle: true,
            use_sync: true,
            bordered: false,
            tight_border: false,
            margin: 0,
            top_anchored: false,
            drawn_top: rows,
            drawn_bottom: 0,
        }
    }

    const fn text_col0(&self) -> usize {
        self.margin
            + if self.tight_border {
                1
            } else if self.bordered {
                4
            } else {
                3
            }
    }

    fn apply_char(&mut self, ch: char) {
        if ch == BACKSPACE {
            if self.caret > 0 {
                self.caret -= 1;
                self.text.remove(self.caret);
            }
        } else {
            self.text.insert(self.caret, ch);
            self.caret += 1;
        }
    }

    fn wrap_lines(&self) -> (WrappedRows, (usize, usize)) {
        let col0 = self.text_col0();
        let right_pad = self.margin
            + if self.tight_border {
                1
            } else if self.bordered {
                2
            } else {
                1
            };
        let tw = self.cols.saturating_sub(col0 + right_pad).max(4);
        let mut lines: Vec<Vec<char>> = vec![Vec::new()];
        let mut pos: Vec<(usize, usize)> = Vec::with_capacity(self.text.len() + 1);
        let mut i = 0;
        while i < self.text.len() {
            if self.text[i] == ' ' {
                let nl = lines.len();
                let ll = lines[nl - 1].len();
                if ll + 1 > tw {
                    pos.push((nl, 0));
                    lines.push(Vec::new());
                } else {
                    pos.push((nl - 1, ll));
                    lines[nl - 1].push(' ');
                }
                i += 1;
                continue;
            }
            let mut j = i;
            while j < self.text.len() && self.text[j] != ' ' {
                j += 1;
            }
            let word = &self.text[i..j];
            let line_len = lines.last().unwrap().len();
            if line_len + word.len() > tw && line_len > 0 {
                let line = lines.last_mut().unwrap();
                if line.last() == Some(&' ') {
                    line.pop();
                }
                lines.push(Vec::new());
            }
            for &c in word {
                if lines[lines.len() - 1].len() >= tw {
                    lines.push(Vec::new());
                }
                let nl = lines.len();
                pos.push((nl - 1, lines[nl - 1].len()));
                lines[nl - 1].push(c);
            }
            i = j;
        }
        let end_line = lines.len() - 1;
        let end_col = lines[end_line].len();
        pos.push((end_line, end_col));

        let n = lines.len();
        let top = if self.top_anchored {
            self.base_row
        } else {
            self.base_row.saturating_sub(n + 1)
        };
        let m = self.margin;
        let r_edge = self.cols - 1 - m;
        let mut rows_out = Vec::with_capacity(n + 2);
        let mut top_rule = vec![' '; self.cols];
        let mut bot_rule = vec![' '; self.cols];
        for c in m..=r_edge {
            top_rule[c] = '\u{2500}';
            bot_rule[c] = '\u{2500}';
        }
        if self.bordered || self.tight_border {
            top_rule[m] = '\u{256d}';
            top_rule[r_edge] = '\u{256e}';
            bot_rule[m] = '\u{2570}';
            bot_rule[r_edge] = '\u{256f}';
        }
        rows_out.push((top, top_rule));
        for (idx, l) in lines.iter().enumerate() {
            let mut row = vec![' '; self.cols];
            if self.bordered || self.tight_border {
                row[m] = '\u{2502}';
                row[r_edge] = '\u{2502}';
                if self.bordered && idx == 0 {
                    row[m + 2] = '\u{203a}';
                }
            } else if idx == 0 {
                row[m + 1] = '\u{276f}';
            }
            for (k, &c) in l.iter().enumerate() {
                if col0 + k < self.cols {
                    row[col0 + k] = c;
                }
            }
            rows_out.push((top + 1 + idx, row));
        }
        rows_out.push((top + 1 + n, bot_rule));
        let (cl, cc) = pos[self.caret.min(pos.len() - 1)];
        let caret_rc = (top + 1 + cl, (col0 + cc).min(self.cols - 1));
        (rows_out, caret_rc)
    }

    fn render_diff(&mut self, status_tick: Option<u32>) -> (Vec<u8>, (u16, u16)) {
        let (mut rows_out, (cr, cc)) = self.wrap_lines();
        let new_top = rows_out.first().map_or(0, |(r, _)| *r);
        let new_bot = rows_out.last().map_or(0, |(r, _)| *r + 1);
        for r in self.drawn_top..new_top.min(self.rows) {
            rows_out.push((r, vec![' '; self.cols]));
        }
        for r in new_bot..self.drawn_bottom.min(self.rows) {
            rows_out.push((r, vec![' '; self.cols]));
        }
        rows_out.sort_unstable_by_key(|(r, _)| *r);
        self.drawn_top = new_top;
        self.drawn_bottom = new_bot;
        let mut s = String::new();
        if self.use_sync {
            s.push_str(SYNC_BEGIN);
        }
        s.push_str("\x1b[?25l");
        let mut cur_col = usize::MAX;
        for (r, mut new) in rows_out {
            if !self.visible_caret && r == cr && cc < self.cols {
                // Hidden-caret Ink composer paints an inverse-video block at caret,
                // or leaves the underlying cell glyph (' ') unchanged when using a bar/parked cursor.
            }
            if self.screen[r] == new {
                continue;
            }
            for (col, &ch) in new.iter().enumerate().take(self.cols) {
                if self.screen[r][col] == ch {
                    continue;
                }
                if cur_col != col {
                    s.push_str(&format!("\x1b[{};{}H", r + 1, col + 1));
                }
                s.push(ch);
                cur_col = col + 1;
            }
            new.truncate(self.cols);
            self.screen[r] = new;
        }
        if let Some(tick) = status_tick {
            let spin = ['⠋', '⠙', '⠹', '⠸'][tick as usize % 4];
            s.push_str(&format!("\x1b[1;2H{spin} synced {tick}"));
        }
        if self.park_before_settle {
            s.push_str(&format!("\x1b[{};1H", self.rows));
        }
        if self.visible_caret {
            s.push_str(&format!("\x1b[{};{}H\x1b[?25h", cr + 1, cc + 1));
        }
        if self.use_sync {
            s.push_str(SYNC_END);
        }
        (s.into_bytes(), (cr as u16, cc as u16))
    }
}

#[test]
fn pixel_continuity_across_single_and_batched_typing_cadences() {
    let phrases: &[(&str, &[usize])] = &[
        ("shared state in lab", &[1]),
        ("shared state in lab", &[2]),
        ("shared state in lab", &[1, 2, 1, 3, 1, 2]),
        ("Hello World! Testing 123", &[1, 4, 2, 3, 1]),
        // Coalesced batches starting directly on a space (' '):
        ("shared state. in that state, ", &[6, 2, 4, 2, 3, 2]),
        ("shared state. in that state, ", &[6, 3, 3, 3, 3, 3]),
        ("shared state. in that state, ", &[6, 4, 2, 4, 2]),
    ];
    let geoms = [(7, 15), (8, 16), (9, 19), (11, 22), (13, 26), (15, 28)];
    let styles = [
        ("rk-tall-dark", GlowStyle::RainbowKitty, false, true),
        ("rk-tall-light", GlowStyle::RainbowKitty, false, false),
        ("rk-flat-dark", GlowStyle::RainbowKitty, true, true),
        ("rk-flat-light", GlowStyle::RainbowKitty, true, false),
    ];
    let dts_us = [4_100u64, 7_300, 16_667];

    let mut failures = Vec::new();
    for &(sname, style, flat, dark) in &styles {
        for &(cw, ch) in &geoms {
            for &dt_us in &dts_us {
                for &(phrase, batch_pat) in phrases {
                    let cfg = make_glow_cfg(style, flat, dark);
                    let mut h = NativeHarness::new(24, 100, cw, ch, cfg, false);
                    h.feed(b"\x1b[6;3H$ ");
                    h.advance_us(16_667, 5, None, "init");

                    let chars: Vec<char> = phrase.chars().collect();
                    let start_col = 4u16;
                    let mut idx = 0usize;
                    let mut step = 0usize;
                    while idx < chars.len() {
                        let take = batch_pat[step % batch_pat.len()].min(chars.len() - idx);
                        let mut echo = String::new();
                        for &c in &chars[idx..idx + take] {
                            h.key_press(c);
                            echo.push(c);
                        }
                        h.feed(echo.as_bytes());
                        idx += take;
                        step += 1;
                        let end_col = start_col + idx as u16;
                        // Keep expected_live to the most recent 8 cells so natural fading on
                        // slow cadences isn't misread, while interior holes anywhere in the
                        // entire live ribbon are ALWAYS caught by scan_band_gaps step 1 & 2!
                        let live_lo = end_col.saturating_sub(8).max(start_col);
                        let tag =
                            format!("{sname} {cw}x{ch} dt={dt_us}us pat={batch_pat:?} idx={idx}");
                        h.advance_us(dt_us, 5, Some((live_lo, end_col)), &tag);
                    }
                    if !h.gaps.is_empty() {
                        failures.extend(h.gaps.into_iter().take(3));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "pixel continuity gaps in single/batched typing ({} total):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn pixel_continuity_across_owners_phrase_and_word_boundary_pauses() {
    let phrase = "here? we need to know that this aterm is synced with the overall lab shared state. in that state, ";
    // Test pauses right after "shared" (before ' '), right after "shared " (before "state"),
    // and coalesced " s" / " st" / " state" echoes after pauses of varying durations!
    let pause_ms_cases = [
        0u64, 17, 50, 100, 160, 220, 260, 310, 380, 450, 550, 700, 900, 1200,
    ];
    let mut failures = Vec::new();

    for &pause_at_space_ms in &pause_ms_cases {
        for &coalesce_after_space in &[1usize, 2, 3, 4] {
            for &flat in &[false, true] {
                let cfg = make_glow_cfg(GlowStyle::RainbowKitty, flat, true);
                let mut h = NativeHarness::new(53, 113, 15, 28, cfg, false);
                h.feed(b"\x1b[?1049h\x1b[2J\x1b[H");
                let mut comp = DiffComposer::new(53, 113, 48, true);
                let (init_bytes, _) = comp.render_diff(None);
                h.feed(&init_bytes);
                h.advance_us(16_667, 47, None, "init");

                let chars: Vec<char> = phrase.chars().collect();
                // Locate "shared state" boundary
                let shared_end = phrase.find(" state.").expect("shared_end"); // index of ' ' before "state."
                let mut i = 0usize;
                while i < chars.len() {
                    if i == shared_end && pause_at_space_ms > 0 {
                        // Type ' ', render it, then pause before "state", OR pause before ' ' and coalesce!
                        let frames = (pause_at_space_ms * 1000 / 16_667).max(1);
                        for f in 0..frames {
                            let tag = format!(
                                "pause={pause_at_space_ms}ms coal={coalesce_after_space} flat={flat} hold={f}"
                            );
                            h.advance_us(16_667, 47, None, &tag);
                        }
                        let take = coalesce_after_space.min(chars.len() - i);
                        for &c in &chars[i..i + take] {
                            h.key_press(c);
                            comp.apply_char(c);
                        }
                        let (bytes, (cr, cc)) = comp.render_diff(Some(i as u32));
                        h.feed(&bytes);
                        i += take;
                        let tag = format!(
                            "pause={pause_at_space_ms}ms coal={coalesce_after_space} flat={flat} after_space i={i}"
                        );
                        let lo = cc.saturating_sub(take as u16);
                        h.advance_us(16_667, cr, Some((lo, cc)), &tag);
                        continue;
                    }

                    h.key_press(chars[i]);
                    comp.apply_char(chars[i]);
                    let (bytes, (cr, cc)) = comp.render_diff(None);
                    h.feed(&bytes);
                    i += 1;
                    let tag = format!(
                        "pause={pause_at_space_ms}ms coal={coalesce_after_space} flat={flat} i={i}"
                    );
                    h.advance_us(16_667, cr, Some((cc.saturating_sub(1), cc)), &tag);
                }

                // Also test pausing AFTER typing ' ' (so ' ' belongs to Cohort A, then "state" starts Cohort B):
                if pause_at_space_ms > 0 {
                    let mut h2 = NativeHarness::new(53, 113, 15, 28, cfg, false);
                    h2.feed(b"\x1b[?1049h\x1b[2J\x1b[H");
                    let mut comp2 = DiffComposer::new(53, 113, 48, true);
                    let (b0, _) = comp2.render_diff(None);
                    h2.feed(&b0);
                    h2.advance_us(16_667, 47, None, "init2");
                    let mut j = 0usize;
                    while j < chars.len() {
                        if j == shared_end + 1 {
                            let frames = (pause_at_space_ms * 1000 / 16_667).max(1);
                            for f in 0..frames {
                                let tag = format!(
                                    "post_space_pause={pause_at_space_ms}ms coal={coalesce_after_space} flat={flat} hold={f}"
                                );
                                h2.advance_us(16_667, 47, None, &tag);
                            }
                            let take = coalesce_after_space.min(chars.len() - j);
                            for &c in &chars[j..j + take] {
                                h2.key_press(c);
                                comp2.apply_char(c);
                            }
                            let (bytes, (cr, cc)) = comp2.render_diff(Some(j as u32));
                            h2.feed(&bytes);
                            j += take;
                            let tag = format!(
                                "post_space_pause={pause_at_space_ms}ms coal={coalesce_after_space} flat={flat} j={j}"
                            );
                            h2.advance_us(
                                16_667,
                                cr,
                                Some((cc.saturating_sub(take as u16), cc)),
                                &tag,
                            );
                            continue;
                        }
                        h2.key_press(chars[j]);
                        comp2.apply_char(chars[j]);
                        let (bytes, (cr, cc)) = comp2.render_diff(None);
                        h2.feed(&bytes);
                        j += 1;
                        let tag = format!(
                            "post_space_pause={pause_at_space_ms}ms coal={coalesce_after_space} flat={flat} j={j}"
                        );
                        h2.advance_us(16_667, cr, Some((cc.saturating_sub(1), cc)), &tag);
                    }
                    if !h2.gaps.is_empty() {
                        failures.extend(h2.gaps.into_iter().take(3));
                    }
                }

                if !h.gaps.is_empty() {
                    failures.extend(h.gaps.into_iter().take(3));
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "pixel continuity gaps across owner's phrase & word-boundary pauses ({} total):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn pixel_continuity_across_tui_diff_spaces_status_prints_and_torn_frames() {
    let mut failures = Vec::new();
    let snippet = "overall lab shared state. in that state, ";

    // 1. Matrix:
    // - visible caret vs hidden DECTCEM caret
    // - status spinner print on row 0 on every frame / on space frames / none
    // - synced repaints torn mid-stream across short (8 ms, held by FrameHold) and
    //   post-cap (200 ms > SYNC_HOLD_CAP) stalls
    for &visible_caret in &[true, false] {
        for &status_mode in &[0u8, 1, 2] {
            for &stall_us in &[0u64, 8_000, 200_000] {
                let cfg = make_glow_cfg(GlowStyle::RainbowKitty, false, true);
                let mut h = NativeHarness::new(53, 113, 15, 28, cfg, false);
                h.feed(b"\x1b[?1049h\x1b[2J\x1b[H");
                let mut comp = DiffComposer::new(53, 113, 48, visible_caret);
                let (b0, _) = comp.render_diff(None);
                h.feed(&b0);
                h.advance_us(16_667, 47, None, "init");

                for (i, ch) in snippet.chars().enumerate() {
                    h.key_press(ch);
                    comp.apply_char(ch);
                    let status = match status_mode {
                        0 => None,
                        1 => Some(i as u32),
                        _ => (ch == ' ').then_some(i as u32),
                    };
                    let (bytes, (cr, _cc)) = comp.render_diff(status);
                    if stall_us > 0 && (ch == ' ' || i > 0 && snippet.as_bytes()[i - 1] == b' ') {
                        let park = b"\x1b[53;1H";
                        if let Some(cut) = bytes.windows(park.len()).rposition(|w| w == park) {
                            h.feed(&bytes[..cut]);
                            let tag = format!(
                                "vis={visible_caret} status={status_mode} stall={stall_us} tear1 i={i} ch={ch:?}"
                            );
                            h.advance_us(stall_us, cr, None, &tag);
                            h.feed(&bytes[cut..]);
                            let tag2 = format!(
                                "vis={visible_caret} status={status_mode} stall={stall_us} tear2 i={i} ch={ch:?}"
                            );
                            h.advance_us(8_667, cr, None, &tag2);
                            continue;
                        }
                    }
                    h.feed(&bytes);
                    let tag = format!(
                        "vis={visible_caret} status={status_mode} stall={stall_us} i={i} ch={ch:?}"
                    );
                    h.advance_us(16_667, cr, None, &tag);
                }

                if !h.gaps.is_empty() {
                    failures.extend(h.gaps.into_iter().take(4));
                }
            }
        }
    }

    // 2. Parked-caret TUI whose visible cursor stays parked at a fixed cell `(0, 12)` on status
    // row 0 while status row 0 updates AND the user's typed keystroke echoes on composer row 47
    // (`echo_anchor_pass`, matching `zsh_isearch_band.rs`).
    {
        let cfg = make_glow_cfg(GlowStyle::RainbowKitty, false, true);
        let mut h = NativeHarness::new(53, 113, 15, 28, cfg, false);
        h.feed(
            b"\x1b[?1049h\x1b[2J\x1b[H\x1b[1;2Hspin   0s\x1b[48;2H\xe2\x9d\xaf \x1b[1;13H\x1b[?25h",
        );
        h.advance_us(16_667, 47, None, "parked_status_init");
        for (i, ch) in "shared state in lab".chars().enumerate() {
            h.key_press(ch);
            let status_suffix = if i % 2 == 0 { "  9s" } else { "104s" };
            let mut s = String::from(SYNC_BEGIN);
            s.push_str(&format!("\x1b[1;2Hspin {status_suffix}"));
            s.push_str(&format!("\x1b[48;{}H{ch}", 4 + i));
            s.push_str("\x1b[1;13H");
            s.push_str(SYNC_END);
            h.feed(s.as_bytes());
            let end_col = 4 + i as u16;
            let lo_col = end_col.saturating_sub(5).max(3);
            let span = (i >= 1).then_some((lo_col, end_col));
            let tag = format!("parked_status i={i} ch={ch:?}");
            h.advance_us(16_667, 47, span, &tag);
        }
        if !h.gaps.is_empty() {
            failures.extend(h.gaps.into_iter().take(4));
        }
    }

    // 3. Claude Code faithful `cc_key_frame` (zero-byte space echo, `\x1b[H\r\x1b[...C\x1b[...B`
    // cursor-relative jumps, `ROW - 3` spinner, `cc_idle_frame` status erase, and 64-byte PTY
    // chunking across frames):
    for &pty_chunk in &[0usize, 64] {
        let cfg = make_glow_cfg(GlowStyle::RainbowKitty, false, true);
        let mut h = NativeHarness::new(53, 113, 15, 28, cfg, false);
        let row: u16 = 47;
        let col0: u16 = 3;
        h.feed(b"\x1b[?1049h\x1b[2J\x1b[H\x1b[48;1H\xe2\x9d\xaf\x1b[48;4H\x1b[?25h");
        h.advance_us(16_667, row, None, "cc_init");
        let mut nbsp_drawn = false;
        let cc_put = |r: u16, c: u16, txt: &str| -> String {
            let mut s = String::from("\x1b[H\r");
            if c > 0 {
                s.push_str(&format!("\x1b[{c}C"));
            }
            if r > 0 {
                s.push_str(&format!("\x1b[{r}B"));
            }
            s.push_str(txt);
            s
        };
        for (i, ch) in snippet.chars().enumerate() {
            h.key_press(ch);
            let mut f = String::from(SYNC_BEGIN);
            f.push_str("\x1b[?25l");
            let mut wrote = false;
            if ch != ' ' {
                if !nbsp_drawn {
                    f.push_str(&cc_put(row, 1, "\u{a0}"));
                    nbsp_drawn = true;
                }
                let mut b = [0u8; 4];
                f.push_str(&cc_put(row, col0 + i as u16, ch.encode_utf8(&mut b)));
                wrote = true;
            }
            if wrote {
                f.push_str("\x1b[53;1H");
            }
            let cc_caret_idx = i + 1;
            f.push_str(&format!(
                "\x1b[{};{}H\x1b[?25h",
                row + 1,
                usize::from(col0) + cc_caret_idx + 1
            ));
            f.push_str(SYNC_END);
            let raw = f.into_bytes();
            if pty_chunk > 0 && raw.len() > pty_chunk {
                for part in raw.chunks(pty_chunk) {
                    h.feed(part);
                    let tag = format!("cc chunk={pty_chunk} i={i} ch={ch:?}");
                    h.advance_us(4_000, row, None, &tag);
                }
            } else {
                h.feed(&raw);
                let tag = format!("cc chunk={pty_chunk} i={i} ch={ch:?}");
                h.advance_us(14_000, row, None, &tag);
            }
            if i % 5 == 2 {
                let mut sp = String::from(SYNC_BEGIN);
                sp.push_str("\x1b[?25l");
                sp.push_str(&cc_put(
                    row - 3,
                    0,
                    &format!("⠋ Transmuting… ({}s · ↓ 1.3k tokens)", 10 + i),
                ));
                sp.push_str("\x1b[53;1H");
                sp.push_str(&format!(
                    "\x1b[{};{}H\x1b[?25h",
                    row + 1,
                    usize::from(col0) + cc_caret_idx + 1
                ));
                sp.push_str(SYNC_END);
                h.feed(sp.as_bytes());
                h.advance_us(8_000, row, None, &format!("cc_spin i={i}"));
            }
        }
        if !h.gaps.is_empty() {
            failures.extend(h.gaps.into_iter().take(4));
        }
    }

    // 4. Ink composer with inverse-video cursor block (`\x1b[7m \x1b[27m`), hidden DECTCEM caret,
    // and 2-part split redraw (`head` without `?2026h` + 3ms split + `tail` CUP):
    for &hidden_caret in &[false, true] {
        for &cursor_cell in &[false, true] {
            let cfg = make_glow_cfg(GlowStyle::RainbowKitty, false, true);
            let mut h = NativeHarness::new(53, 113, 15, 28, cfg, false);
            let row: u16 = 47;
            let col0: usize = 2;
            h.feed(b"\x1b[?1049h\x1b[2J\x1b[H\x1b[48;1H\x1b[2K\xe2\x80\xba \x1b[48;3H\x1b[?25h");
            h.advance_us(16_667, row, None, "ink_init");
            let mut text = String::new();
            for (i, ch) in "overall lab shared state. in that state"
                .chars()
                .enumerate()
            {
                h.key_press(ch);
                text.push(ch);
                let mut head = format!("\x1b[?25l\x1b[{};1H\x1b[2K\u{203a} {}", row + 1, text);
                if cursor_cell {
                    head.push_str("\x1b[7m \x1b[27m");
                }
                h.feed(head.as_bytes());
                let tag1 =
                    format!("ink hid={hidden_caret} cell={cursor_cell} head i={i} ch={ch:?}");
                h.advance_us(3_000, row, None, &tag1);
                let mut tail = format!("\x1b[{};{}H", row + 1, col0 + text.len() + 1);
                if !hidden_caret {
                    tail.push_str("\x1b[?25h");
                }
                h.feed(tail.as_bytes());
                let tag2 =
                    format!("ink hid={hidden_caret} cell={cursor_cell} tail i={i} ch={ch:?}");
                h.advance_us(13_667, row, None, &tag2);
            }
            if !h.gaps.is_empty() {
                failures.extend(h.gaps.into_iter().take(4));
            }
        }
    }

    // 5. Coalesced echo starting with ' ' when the space keystroke stalled (0ms, 120ms, or 220ms
    // past `TYPE_HINT_FRESH_DUR`) before the rest of the batch arrived together:
    for &batch in &[" s", " st", " sta", " state"] {
        for &space_stall_us in &[0u64, 120_000, 220_000] {
            let cfg = make_glow_cfg(GlowStyle::RainbowKitty, false, true);
            let mut h = NativeHarness::new(24, 100, 12, 24, cfg, false);
            h.feed(b"\x1b[6;3H> ");
            h.advance_us(16_667, 5, None, "init");
            for ch in "shared".chars() {
                h.key_press(ch);
                h.feed(ch.encode_utf8(&mut [0; 4]).as_bytes());
                h.advance_us(16_667, 5, None, "shared");
            }
            let bchars: Vec<char> = batch.chars().collect();
            h.key_press(bchars[0]);
            if space_stall_us > 0 {
                h.now += Duration::from_micros(space_stall_us);
            }
            for &c in &bchars[1..] {
                h.key_press(c);
            }
            h.feed(batch.as_bytes());
            let end_col = 4 + 6 + bchars.len() as u16;
            let tag = format!("delayed_space_batch batch={batch:?} stall_us={space_stall_us}");
            h.advance_us(16_667, 5, Some((8, end_col)), &tag);
            for ch in " in".chars() {
                h.key_press(ch);
                h.feed(ch.encode_utf8(&mut [0; 4]).as_bytes());
                h.advance_us(16_667, 5, None, &tag);
            }
            if !h.gaps.is_empty() {
                failures.extend(h.gaps.into_iter().take(4));
            }
        }
    }

    // 6. `InputEvent::Text` / glyph-less credit path (`ime_text_mode = true`) in a TUI
    // diff-renderer that skips unchanged spaces and coalesces frames:
    for &coal in &[1usize, 2, 3] {
        let cfg = make_glow_cfg(GlowStyle::RainbowKitty, false, true);
        let mut h = NativeHarness::new(53, 113, 15, 28, cfg, false);
        h.ime_text_mode = true;
        h.feed(b"\x1b[?1049h\x1b[2J\x1b[H");
        let mut comp = DiffComposer::new(53, 113, 48, true);
        let (b0, _) = comp.render_diff(None);
        h.feed(&b0);
        h.advance_us(16_667, 47, None, "ime_init");
        let chars: Vec<char> = snippet.chars().collect();
        let mut idx = 0usize;
        while idx < chars.len() {
            let take = coal.min(chars.len() - idx);
            for &c in &chars[idx..idx + take] {
                h.key_press(c);
                comp.apply_char(c);
            }
            let (bytes, (cr, cc)) = comp.render_diff(Some(idx as u32));
            h.feed(&bytes);
            idx += take;
            let tag = format!("ime_text coal={coal} idx={idx}");
            h.advance_us(16_667, cr, Some((cc.saturating_sub(take as u16), cc)), &tag);
        }
        if !h.gaps.is_empty() {
            failures.extend(h.gaps.into_iter().take(4));
        }
    }

    assert!(
        failures.is_empty(),
        "pixel continuity gaps in TUI diff/status/torn/partial-credit scenarios ({} total):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn pixel_continuity_across_backspaces_retypes_glides_and_wraps() {
    let mut failures = Vec::new();
    let cfg = make_glow_cfg(GlowStyle::RainbowKitty, false, true);

    // 1. Backspace across space + word boundary and immediate retyping:
    for &erase_n in &[1usize, 2, 3, 5, 8] {
        let mut h = NativeHarness::new(24, 90, 12, 24, cfg, false);
        h.feed(b"\x1b[6;3H$ ");
        h.advance_us(16_667, 5, None, "init");
        for ch in "lab shared stx".chars() {
            h.key_press(ch);
            h.feed(ch.encode_utf8(&mut [0; 4]).as_bytes());
            h.advance_us(14_000, 5, None, "type1");
        }
        for k in 0..erase_n {
            h.key_press(BACKSPACE);
            h.feed(b"\x08 \x08");
            let tag = format!("bs erase_n={erase_n} step={k}");
            h.advance_us(14_000, 5, None, &tag);
        }
        let retype = &"lab shared state in lab"[("lab shared stx".len() - erase_n)..];
        for (k, ch) in retype.chars().enumerate() {
            h.key_press(ch);
            h.feed(ch.encode_utf8(&mut [0; 4]).as_bytes());
            let tag = format!("retype erase_n={erase_n} step={k} ch={ch:?}");
            h.advance_us(14_000, 5, None, &tag);
        }
        if !h.gaps.is_empty() {
            failures.extend(h.gaps.into_iter().take(3));
        }
    }

    // 2. Left/Right cursor glides across an active band + resume typing:
    for &glide in &[1usize, 3, 6, 10] {
        let mut h = NativeHarness::new(24, 90, 12, 24, cfg, false);
        h.feed(b"\x1b[6;3H$ ");
        h.advance_us(16_667, 5, None, "init");
        for ch in "shared state".chars() {
            h.key_press(ch);
            h.feed(ch.encode_utf8(&mut [0; 4]).as_bytes());
            h.advance_us(12_000, 5, None, "glide_seed");
        }
        h.nav_press();
        h.feed(format!("\x1b[{glide}D").as_bytes());
        h.advance_us(16_667, 5, None, &format!("glide_left_{glide}"));
        h.nav_press();
        h.feed(format!("\x1b[{glide}C").as_bytes());
        h.advance_us(16_667, 5, None, &format!("glide_right_{glide}"));
        for ch in " again".chars() {
            h.key_press(ch);
            h.feed(ch.encode_utf8(&mut [0; 4]).as_bytes());
            h.advance_us(12_000, 5, None, &format!("glide_after_{glide}"));
        }
        if !h.gaps.is_empty() {
            failures.extend(h.gaps.into_iter().take(3));
        }
    }

    // 3. Soft-wrapped bottom-anchored and top-anchored TUI composers (unbordered `❯ ...`,
    // 4-sided box-bordered `│ › ... │`, and tight box-bordered `│...│`) across multiple word
    // wraps under glyphed (`note_typed_expected`), glyph-less (`note_typed_glyph` / IME), and
    // 2-char coalesced typing:
    let wrap_modes = [
        ("plain-bottom", false, false, false, false, 1usize),
        ("plain-bottom-ime", false, false, false, true, 1),
        ("box-bottom", true, false, false, false, 1),
        ("box-bottom-coal2", true, false, false, false, 2),
        ("box-bottom-ime", true, false, false, true, 1),
        ("box-top", true, false, true, false, 1),
        ("box-top-coal2", true, false, true, false, 2),
        ("box-top-ime", true, false, true, true, 1),
        ("tight-box-bottom", false, true, false, false, 1),
        ("tight-box-bottom-ime", false, true, false, true, 1),
        ("tight-box-top", false, true, true, false, 1),
    ];
    for &(label, bordered, tight_border, top_anchored, ime_mode, coal) in &wrap_modes {
        let mut h = NativeHarness::new(30, 44, 11, 22, cfg, false);
        h.ime_text_mode = ime_mode;
        h.feed(b"\x1b[?1049h\x1b[2J\x1b[H");
        let base_row = if top_anchored { 4 } else { 26 };
        let mut comp = DiffComposer::new(30, 44, base_row, true);
        comp.bordered = bordered;
        comp.tight_border = tight_border;
        comp.top_anchored = top_anchored;
        let col0 = comp.text_col0() as u16;
        let (b0, (init_r, _)) = comp.render_diff(None);
        h.feed(&b0);
        h.advance_us(16_667, init_r, None, "wrap_init");
        let long_text =
            "this aterm is synced with the overall lab shared state. in that state, we keep typing";
        let chars: Vec<char> = long_text.chars().collect();
        let mut prev_pos = (init_r, col0);
        let mut idx = 0usize;
        while idx < chars.len() {
            let take = coal.min(chars.len() - idx);
            for &ch in &chars[idx..idx + take] {
                h.key_press(ch);
                comp.apply_char(ch);
            }
            let (bytes, (cr, cc)) = comp.render_diff(None);
            h.feed(&bytes);
            let wrapped_now = (cr == prev_pos.0 && cc < prev_pos.1) || cr > prev_pos.0;
            // On glyphed (`!ime_mode`) glyph-bearing wraps (`cc > col0`), the wrapped word/glyph
            // at `col0..cc` must be lit IMMEDIATELY on the wrap frame itself (zero HeldPark
            // blackout frames!).
            let expected_live = if !ime_mode && wrapped_now && cc > col0 {
                Some((col0, cc))
            } else if !ime_mode && cc > col0 + 3 {
                Some((cc.saturating_sub(4).max(col0), cc))
            } else {
                None
            };
            let tag = format!("wrap {label} idx={idx} caret=({cr},{cc})");
            h.advance_us(14_000, cr, expected_live, &tag);

            // Verify left box border (`col 0`) and indent (`cols 1..col0`) never acquire ribbon
            // cells on bordered composers (`SPECS.md` `rewrap-carries-word`: "never onto the indent").
            if (bordered || tight_border)
                && let Some(rib) = h.fx.cursor().glow.v2_ribbon()
            {
                let leaked: Vec<u16> = rib
                    .cells()
                    .iter()
                    .filter(|c| c.row == cr && !c.leaving() && c.col < col0)
                    .map(|c| c.col)
                    .collect();
                if !leaked.is_empty() {
                    failures.push(format!(
                        "[{tag}] ribbon leaked onto border/indent cols {leaked:?} (< col0={col0})"
                    ));
                }
            }
            prev_pos = (cr, cc);
            idx += take;
        }
        if !h.gaps.is_empty() {
            failures.extend(h.gaps.into_iter().take(6));
        }
    }

    // 4. Reverse word lift (`lifted_word`) inside a 4-sided box-bordered composer (`│ ... │`)
    // and direct regression for flushed-park same-instant `TypedClass::Space` ordering:
    {
        // 4a. 4-sided box-bordered reverse word lift when typing Space lifts a word onto the row
        // above while right box border `│` stands at col 43:
        let mut h = NativeHarness::new(12, 44, 11, 22, cfg, false);
        h.feed(b"\x1b[?1049h\x1b[2J\x1b[H");
        let pad_box = |inner: &str| -> String {
            let mut v: Vec<char> = vec![' '; 44];
            v[0] = '\u{2502}';
            v[43] = '\u{2502}';
            for (k, c) in inner.chars().enumerate() {
                if 1 + k < 43 {
                    v[1 + k] = c;
                }
            }
            v.into_iter().collect()
        };
        let r3_before = pad_box("the text");
        let r4_before = pad_box("already[Image #1]");
        let r3_after = pad_box("the text already");
        let r4_after = pad_box("[Image #1]");
        h.feed(format!("\x1b[4;1H{r3_before}\x1b[5;1H{r4_before}\x1b[5;9H\x1b[?25h").as_bytes());
        h.advance_us(16_667, 4, None, "box_lift_init");
        h.key_press(' ');
        h.feed(format!("\x1b[4;1H{r3_after}\x1b[5;1H{r4_after}\x1b[5;2H").as_bytes());
        h.advance_us(8_000, 4, None, "box_lift_echo");
        assert_eq!(
            h.fx.cursor().glow.in_flight_tally().forgotten,
            0,
            "4-sided bordered lifted word must spend, not forget, the Space credit"
        );
        if let Some(rib) = h.fx.cursor().glow.v2_ribbon() {
            assert!(
                !rib.cells().iter().any(|c| c.row == 4 && c.col == 0),
                "4-sided bordered lifted word must never light left border col 0"
            );
        }

        // 4b. Flushed-park same-instant `TypedClass::Space`: hold a count-only rewrap park at
        // `t_park`, bank `Space` at the exact same `Instant` (`h2.key_press(' ')` before clock
        // advances), and verify the flushed re-anchor relays the full wrapped word prefix `"labo"`
        // (`"labor "` at `cols 4..10`) with zero dark prefix cells.
        let mut h2 = NativeHarness::new(30, 44, 11, 22, cfg, false);
        h2.ime_text_mode = true;
        h2.feed(b"\x1b[?1049h\x1b[2J\x1b[H");
        let mut comp = DiffComposer::new(30, 44, 26, true);
        comp.bordered = true;
        let (b0, _) = comp.render_diff(None);
        h2.feed(&b0);
        h2.advance_us(16_667, 25, None, "same_instant_space_init");
        // For `bordered = true` on `cols = 44`, `tw = 38`: `"this aterm is synced with overall labo"`
        // is exactly 38 chars (`caret = (25, 42)`), so the next char `'r'` wraps `"labor"` to
        // `(25, 9)`!
        let prefix = "this aterm is synced with overall labo";
        for ch in prefix.chars() {
            h2.key_press(ch);
            comp.apply_char(ch);
            let (bytes, (cr, _)) = comp.render_diff(None);
            h2.feed(&bytes);
            h2.advance_us(14_000, cr, None, "same_instant_prefix");
        }
        let flushed_before = h2.fx.cursor().glow.in_flight_tally().park_flushed;
        let (_, prev_rc) = comp.wrap_lines();
        // Wrap key 'r' wraps `"labor"` (`(25, 42) -> (25, 9)`) and is held in HeldPark at `h2.now`:
        h2.key_press('r');
        comp.apply_char('r');
        let (bytes_r, (cr_r, cc_r)) = comp.render_diff(None);
        assert!(
            cc_r < prev_rc.1 as u16,
            "precondition: 'r' wrapped the word left"
        );
        h2.feed(&bytes_r);
        h2.advance_us(14_000, cr_r, None, "same_instant_hold_r");
        assert_eq!(
            h2.fx.cursor().glow.in_flight_tally().park_flushed,
            flushed_before,
            "count-only wrap is held in HeldPark on the wrap frame"
        );
        // Immediately press ' ' WITHOUT advancing `h2.now` (`at == p.at`), then render & advance:
        h2.key_press(' ');
        comp.apply_char(' ');
        let (bytes_sp, (cr_sp, cc_sp)) = comp.render_diff(None);
        h2.feed(&bytes_sp);
        h2.advance_us(
            14_000,
            cr_sp,
            Some((4, cc_sp)),
            "same_instant_flush_with_space",
        );
        assert_eq!(
            h2.fx.cursor().glow.in_flight_tally().park_flushed,
            flushed_before + 1,
            "the held park flushed ahead of the same-instant Space"
        );
        if !h2.gaps.is_empty() {
            failures.extend(h2.gaps);
        }

        // 4c. Coalesced `' '` + word-start (`" w"` / `" wo"`) right at the right margin when the
        // preceding word stays on the upper row (`!ended_at_space`, `moved_word > 0`), plus
        // 2-space margin-indented 4-sided box-bordered composers (`"  │ ... │  "`), across both
        // upward-growing (`top_anchored = false`) and downward-growing (`top_anchored = true`)
        // layouts and IME count-only wraps:
        for &(top_anchored, margin, ime) in &[
            (false, 0usize, false),
            (true, 0usize, false),
            (false, 2usize, false),
            (true, 2usize, false),
            (true, 2usize, true),
        ] {
            let base_row = if top_anchored { 6 } else { 26 };
            let mut h3 = NativeHarness::new(30, 48, 11, 22, cfg, false);
            h3.ime_text_mode = ime;
            h3.feed(b"\x1b[?1049h\x1b[2J\x1b[H");
            let mut comp3 = DiffComposer::new(30, 48, base_row, true);
            comp3.bordered = true;
            comp3.margin = margin;
            comp3.top_anchored = top_anchored;
            let c0 = comp3.text_col0() as u16;
            let tw = 48usize - (comp3.text_col0() + margin + 2);
            let (b0, (_, init_cc)) = comp3.render_diff(None);
            let init_cr = if top_anchored {
                (base_row + 1) as u16
            } else {
                (base_row - 1) as u16
            };
            let _ = init_cc;
            h3.feed(&b0);
            h3.advance_us(16_667, init_cr, None, "margin_box_init");

            // Fill line 0 to exact width `tw` ending in a non-space word (`!ended_at_space`), so
            // `moved_word > 0`, then coalesce `' '` + `'w'` + `'o'` in one redraw (or per-key on
            // the IME case to exercise `indented_wrap_landing` on `"  │ ... │  "`):
            let fill_word = "labor";
            let pad_len = tw - fill_word.len();
            let mut line0 = String::with_capacity(tw);
            while line0.len() + 5 < pad_len {
                line0.push_str("sync ");
            }
            while line0.len() + 1 < pad_len {
                line0.push('x');
            }
            if line0.len() < pad_len {
                line0.push(' ');
            }
            line0.push_str(fill_word);
            assert_eq!(line0.len(), tw);

            for ch in line0.chars() {
                h3.key_press(ch);
                comp3.apply_char(ch);
                let (bytes, (cr, cc)) = comp3.render_diff(None);
                h3.feed(&bytes);
                h3.advance_us(12_000, cr, Some((cc.saturating_sub(1), cc)), "fill_line0");
            }

            if ime {
                // IME count-only wrap inside `"  │ ... │  "` (`margin = 2`, `top_anchored = true`):
                // `'s'` extends `"labor"` -> `"labors"` and wraps onto `pr + 1` with zero HeldPark!
                let parked_before = h3.fx.cursor().glow.in_flight_tally().park_flushed;
                for ch in "s next".chars() {
                    h3.key_press(ch);
                    comp3.apply_char(ch);
                    let (bytes, (cr, cc)) = comp3.render_diff(None);
                    h3.feed(&bytes);
                    h3.advance_us(
                        12_000,
                        cr,
                        Some((cc.saturating_sub(1).max(c0), cc)),
                        "ime_margin_box_wrap",
                    );
                }
                assert_eq!(
                    h3.fx.cursor().glow.in_flight_tally().park_flushed,
                    parked_before,
                    "margin-indented 4-sided box wrap must not enter HeldPark"
                );
            } else {
                // Coalesce `' '` + `'w'` + `'o'` in one redraw right after `"labor"` at `tw`:
                for ch in [' ', 'w', 'o'] {
                    h3.key_press(ch);
                    comp3.apply_char(ch);
                }
                let (bytes, (cr, cc)) = comp3.render_diff(None);
                assert_eq!(cc, c0 + 2, "expected 'wo' to land at col0..col0+2");
                h3.feed(&bytes);
                let tag = format!("coal_space_word top={top_anchored} margin={margin}");
                h3.advance_us(12_000, cr, Some((c0, cc)), &tag);
                assert_eq!(
                    h3.fx.cursor().glow.in_flight_tally().forgotten,
                    0,
                    "{tag}: coalesced space+word wrap must spend, not forget, credits"
                );
            }
            if !h3.gaps.is_empty() {
                failures.extend(h3.gaps);
            }
        }
    }

    assert!(
        failures.is_empty(),
        "pixel continuity gaps across backspaces/glides/wraps ({} total):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn build_render_input(term: &Terminal, rows: usize, cols: usize, dark: bool) -> RenderInput {
    let (fg_u32, bg_u32) = if dark {
        (0x00D0_D0D0u32, 0x0011_1318u32)
    } else {
        (0x001E_2028u32, 0x00F7_F5EEu32)
    };
    let fg = [
        ((fg_u32 >> 16) & 0xFF) as u8,
        ((fg_u32 >> 8) & 0xFF) as u8,
        (fg_u32 & 0xFF) as u8,
    ];
    let bg = [
        ((bg_u32 >> 16) & 0xFF) as u8,
        ((bg_u32 >> 8) & 0xFF) as u8,
        (bg_u32 & 0xFF) as u8,
    ];
    let mut cells = Vec::with_capacity(rows);
    let mut row_chars = Vec::new();
    for r in 0..rows {
        term.row_cols_into(r, &mut row_chars);
        let mut line = Vec::with_capacity(cols);
        for c in 0..cols {
            let ch = row_chars.get(c).copied().unwrap_or(' ');
            line.push(RenderCell {
                ch,
                fg,
                bg,
                ..RenderCell::default()
            });
        }
        cells.push(line);
    }
    let c = term.cursor();
    RenderInput {
        rows,
        cols,
        cells,
        line_sizes: vec![aterm_core::grid::LineSize::default(); rows],
        cursor_row: usize::from(c.row),
        cursor_col: usize::from(c.col),
        cursor_visible: term.cursor_visible(),
        cursor_style: term.cursor_style(),
        cursor_color: COLOR_UNSET,
        default_fg: fg_u32,
        default_bg: bg_u32,
        process_sequence: term.pipeline_timestamps().process_sequence,
        terminal_id: term.render_identity(),
        engine_alt: term.is_alternate_screen(),
        display_offset: term.grid().display_offset() as i32,
        ..RenderInput::default()
    }
}

#[test]
fn pixel_continuity_with_resident_pet_flying_kitty_sparkle_words_and_classic_rainbow() {
    let styles = [
        ("rainbow kitty pet", true),
        ("rainbow kitty flying", true),
        ("rainbow", true),
        ("rainbow", false),
        ("phaser", true),
    ];
    let text = "the quick cat and fox shared state in lab ";
    let mut failures = Vec::new();

    for &(style_name, dark) in &styles {
        for &batch in &[1usize, 2, 3] {
            let rows = 24usize;
            let cols = 90usize;
            let cw = 12usize;
            let ch = 24usize;
            let g = geom(rows, cols, cw, ch);
            let bg_u32 = if dark { 0x0011_1318 } else { 0x00F7_F5EE };

            let mut term = Terminal::new(rows as u16, cols as u16);
            let mut fx = EffectsPipeline::new();
            fx.set_cursor_glow(
                true,
                style_name,
                TrailColor::automatic_or_pinned(None),
                None,
                240,
                18,
                0.7,
                0.6,
                true,
                0x0050_FA7B,
            );
            fx.set_sparkle_enabled(true);

            let t0 = fx.now();
            term.process_at(
                b"\x1b[10;4H$ ",
                ClockReading {
                    monotonic: t0,
                    wall_ms: None,
                },
            );
            let mut input = build_render_input(&term, rows, cols, dark);
            fx.apply(&mut term, &mut input, cw, ch);

            let chars: Vec<char> = text.chars().collect();
            let mut i = 0usize;
            while i < chars.len() {
                let take = batch.min(chars.len() - i);
                fx.advance(16.0);
                let t = fx.now();
                let mut echo = String::new();
                for &c in &chars[i..i + take] {
                    let _ = fx.note_committed_char(&mut term, &input, c);
                    echo.push(c);
                }
                term.process_at(
                    echo.as_bytes(),
                    ClockReading {
                        monotonic: t,
                        wall_ms: None,
                    },
                );
                let mut input = build_render_input(&term, rows, cols, dark);
                fx.apply(&mut term, &mut input, cw, ch);
                i += take;

                let band = composite_render_input_row(&input, g, bg_u32, 9);
                let lifts = band.all_column_lifts();
                let end_col = 5 + i as u16;
                let lo_col = end_col.saturating_sub(6).max(5);
                let span = (style_name != "phaser").then_some((lo_col, end_col));
                if let Some(gap) = scan_band_gaps(&lifts, cw, dark, span) {
                    failures.push(format!(
                        "[apply style={style_name:?} dark={dark} batch={batch} i={i}] {gap}"
                    ));
                    if failures.len() >= 12 {
                        break;
                    }
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "pixel continuity gaps with pet/flying/sparkle/apply ({} total):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
