// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The glyph law through a real cursor step, publication, yield and raster.

use super::*;
use aterm_core::render::{GlowQuad, RenderInput};
use aterm_core::terminal::ClockReading;
use aterm_effects::cursor_fx::CursorFxInput;
use aterm_effects::cursor_glow::glass_yield::without_body_glass_for_test as without_body_glass;
use aterm_effects::cursor_glow::{Geom, GlowConfig, GlowStyle, Light, Live, ProbeTrust};
use aterm_effects::cursor_trail::TrailConfig;
use aterm_effects::host::{PageFacts, motion_print_anchor};
use aterm_effects::pipeline::{CursorStep, EffectsPipeline, RowProbe};
use aterm_effects::rainbow_kitty::prism_glass::PAGE_SHARE;
use std::time::Duration;

const PAGE: (u32, u32) = (0x002B_2D33, 0x00F6_F7F9);

/// Observe the lowest actual census reading relative to its own glyph's bar.
/// The same face and input make the clean twin; no static glass background stands
/// in for the effect stream that the word yield can cut.
struct LiveReading {
    cell_height: usize,
    admitted: bool,
    thin_ink: f32,
    glass_frames: usize,
    relative: Option<f64>,
    retained_safe_glyphs: usize,
}

/// A final under-stream pixel inside the clipped bounds has nonzero alpha and
/// changes colour compared with the same frame with only its body glass removed.
fn visible_glass_pixel(
    input: &RenderInput,
    sheet: &Sheet,
    without_glass: &[u32],
    quads: &[GlowQuad],
    (x0, y0, x1, y1): (usize, usize, usize, usize),
) -> bool {
    quads.iter().any(|q| {
        if usize::from(q.row) >= input.rows {
            return false;
        }
        let x0 = x0.max(usize::from(q.x));
        let y0 = y0.max(usize::from(q.y));
        let x1 = x1.min(sheet.w).min(usize::from(q.x) + usize::from(q.w));
        let y1 = y1
            .min(sheet.frame.len() / sheet.w)
            .min(usize::from(q.y) + usize::from(q.h));
        (x0..x1).any(|x| {
            aterm_render::glow_lerp(
                u32::from(q.alpha),
                u32::from(q.alpha2),
                (x - usize::from(q.x)) as u32,
                u32::from(q.w),
            ) > 0
                && (y0..y1).any(|y| {
                    let at = y * sheet.w + x;
                    sheet.frame[at] != without_glass[at]
                })
        })
    })
}

fn live_reading(px: f32, line_height: f32, fg: u32, bg: u32) -> LiveReading {
    live_text_reading(px, line_height, fg, bg, GLYPHS)
}

fn live_text_reading(px: f32, line_height: f32, fg: u32, bg: u32, text: &str) -> LiveReading {
    let mut renderer = Renderer::from_system(px, Theme::default()).expect("the window face");
    renderer.set_line_height(line_height);
    renderer.debug_block_on_lazy_fallbacks();
    let thin_ink = renderer.glyph_ink().thin_share(
        aterm_render::linear_luminance(fg),
        aterm_render::linear_luminance(bg),
    );
    let admitted = PageFacts {
        thin_ink,
        ..PageFacts::OPAQUE
    }
    .glass();
    let (cw, ch) = renderer.cell_size();
    let cols = text.chars().count() + 2;
    let mut term = Terminal::new(3, cols as u16);
    term.set_default_foreground(rgb(fg));
    term.set_default_background(rgb(bg));
    term.process(b"\x1b[2;2H");
    let mut fx = EffectsPipeline::for_window();
    let cfg = GlowConfig {
        style: GlowStyle::RainbowKitty,
        dark_theme: false,
        theme_fg: fg,
        theme_bg: bg,
        color: fg,
        duration: Duration::from_millis(650),
        length: usize::MAX,
        intensity: 1.0,
        light: Light::at(Live::Glass).on_page(admitted),
        ..Default::default()
    };
    let (w, h) = renderer.frame_size(3, cols);
    let geom = Geom {
        cw,
        ch,
        rows: 3,
        cols,
        origin_x: renderer.pad() as u16,
        origin_y: renderer.grid_top() as u16,
        win_w: w as u16,
        win_h: h as u16,
        head: 0,
    };
    let mut row = Vec::new();
    let glyphs: Vec<_> = text.chars().collect();
    let t0 = ClockReading::now().monotonic;
    let (mut glass_frames, mut judged, mut judged_glass, mut least) = (0, 0, 0, f64::INFINITY);
    let mut retained_safe_glyphs = 0;
    for tick in 0..glyphs.len() * 8 + 48 {
        let now = t0 + Duration::from_nanos(1_000_000_000 / 120) * tick as u32;
        if tick % 8 == 0 && tick / 8 < glyphs.len() {
            fx.cursor_mut().glow.note_synthetic_typed(now, 1);
            term.process_at(
                glyphs[tick / 8].encode_utf8(&mut [0; 4]).as_bytes(),
                ClockReading {
                    monotonic: now,
                    wall_ms: None,
                },
            );
        }
        let base = term.cell_frame(3, cols);
        let observed = renderer.glyph_ink_for(&base);
        fx.cursor_mut()
            .glow
            .observe_glyph_ink(Some(std::sync::Arc::clone(&observed)));
        let caret = (base.cursor_row as u16, base.cursor_col as u16);
        term.row_cols_into(1, &mut row);
        fx.cursor_mut().glow.observe_ribbon_row(1, &row);
        let (print_anchor, print_anchor_glyph) = motion_print_anchor(&term);
        let _ = aterm_render::test_counters::take_glass_slabs();
        let _ = fx.cursor_step(
            &CursorStep::new(
                CursorFxInput {
                    now,
                    cur: Some(caret),
                    live_viewport: true,
                    cursor_style: term.cursor_style(),
                    blink_phase: true,
                    live_cursor: fg,
                    cursor_color_pinned: false,
                    default_bg: bg,
                    cursor_cell_bg: None,
                    geom,
                    focused: true,
                    shed_envelope: 1.0,
                    trail_brightness: 1.0,
                    body_allowed: true,
                    momentum_glow: true,
                    user_tinted: false,
                    accent_pinned: false,
                },
                &cfg,
                TrailConfig {
                    enabled: false,
                    duration: cfg.duration,
                    max_len: cfg.length,
                    color: cfg.color,
                    intensity: cfg.intensity,
                    warmth: 0.0,
                },
            )
            .with_row_probe(Some(RowProbe {
                row: 1,
                caret: caret.1,
                trust: ProbeTrust::Full,
                cols: &row,
                flanks: None,
            }))
            .with_frame_cells(Some(base.cells.as_slice()))
            .with_frame_cells_cols(base.cols)
            .with_frame_marks(&base.combining)
            .with_print_anchor(print_anchor)
            .with_print_anchor_glyph(print_anchor_glyph),
            std::iter::empty(),
        );
        let emitted_glass_slabs = aterm_render::test_counters::take_glass_slabs();
        let mut input = base.clone();
        fx.publish_cursor(&mut input);
        let without_glass = without_body_glass(&input, fx.cursor().glow.glass_laid());
        fx.yield_glass_unworded(&mut input);
        let glass_arm = fx.light_arms().iter().any(|(arm, _)| arm == "glass");
        let clean = renderer.render_input(&base);
        let frame = renderer.render_input(&input);
        let without_glass = renderer.render_input(&without_glass);
        assert_eq!(frame.width, cols * cw, "the judged frame has no padding");
        assert_eq!(
            (frame.width, frame.height),
            (without_glass.width, without_glass.height),
            "the no-body-glass twin has the same frame geometry"
        );
        let without_glass: Vec<u32> = without_glass
            .pixels
            .into_iter()
            .map(|pixel| pixel & 0x00FF_FFFF)
            .collect();
        let sheet = Sheet {
            frame: frame
                .pixels
                .into_iter()
                .map(|pixel| pixel & 0x00FF_FFFF)
                .collect(),
            clean: clean
                .pixels
                .into_iter()
                .map(|pixel| pixel & 0x00FF_FFFF)
                .collect(),
            w: frame.width,
            cw,
            ch,
        };
        let mut glass_quads = Vec::new();
        input
            .glow_under_ramps
            .expand_into(&input.glow_under, &mut glass_quads);
        let glass = glass_arm
            && emitted_glass_slabs > 0
            && visible_glass_pixel(
                &input,
                &sheet,
                &without_glass,
                &glass_quads,
                (0, 0, sheet.w, sheet.frame.len() / sheet.w),
            );
        glass_frames += usize::from(glass);
        for (col, glyph, reading, own) in sheet.glyph_readings_for(0, text) {
            judged += 1;
            assert!(reading.is_finite() && own.is_finite() && own > 0.0);
            if !glass {
                continue;
            }
            judged_glass += 1;
            let bar = GLASS_BAR.min(PAGE_SHARE * own);
            least = least.min(reading / bar);
            let retains_glass = visible_glass_pixel(
                &input,
                &sheet,
                &without_glass,
                &glass_quads,
                (col * cw, ch, (col + 1) * cw, 2 * ch),
            );
            let real_ink = observed
                .for_frame(&base, (cw, ch))
                .and_then(|frame| frame.coverage(1, col))
                .is_some_and(|(_, coverage)| coverage.max > 0);
            retained_safe_glyphs +=
                usize::from(glyph == 'M' && real_ink && retains_glass && reading >= bar);
            if reading < bar {
                eprintln!(
                    "{px}px/{line_height} at {tick}: '{glyph}' {reading:.4}, own {own:.4}, bar {bar:.4}"
                );
            }
        }
    }
    assert!(
        judged > 0,
        "{px}px/{line_height}: the census judged actual glyphs"
    );
    let relative = if glass_frames > 0 {
        assert!(judged_glass > 0, "glass frames must judge actual glyphs");
        assert!(least.is_finite(), "a finite glyph-to-own-bar minimum");
        Some(least)
    } else {
        None
    };
    LiveReading {
        cell_height: ch,
        admitted,
        thin_ink,
        glass_frames,
        relative,
        retained_safe_glyphs,
    }
}

fn keeps_current_page_law(reading: &LiveReading) {
    if reading.admitted {
        assert!(
            reading.glass_frames > 0,
            "an admitted window emits actual glass"
        );
        let relative = reading
            .relative
            .expect("actual glyphs judged in glass frames");
        assert!(relative >= 1.0, "{relative:.4} of the existing glyph bar");
    } else {
        assert_eq!(
            reading.glass_frames, 0,
            "a refused face keeps the paper arm"
        );
        assert!(
            reading.relative.is_none(),
            "no glass readability verdict on paper"
        );
    }
}

#[test]
fn ordinary_window_glyphs_keep_the_current_page_law() {
    for px in [20.0, 24.0] {
        let reading = live_reading(px, 1.0, PAGE.0, PAGE.1);
        if px == 24.0 {
            assert!(
                reading.admitted,
                "the ordinary 2x window is a glass positive"
            );
        }
        keeps_current_page_law(&reading);
    }
}

#[test]
fn a_supported_word_keeps_nonzero_final_glass_and_its_same_frame_census_bar() {
    let reading = live_text_reading(24.0, 1.0, PAGE.0, PAGE.1, "MMMM MMMM");
    assert!(reading.admitted, "the ordinary 12pt/2x face admits glass");
    keeps_current_page_law(&reading);
    assert!(
        reading.retained_safe_glyphs > 0,
        "a real M must keep final glass beneath its cell while passing its unchanged census bar in that frame"
    );
}

#[test]
fn the_twenty_six_pixel_underscore_keeps_the_current_page_law() {
    let reading = live_reading(26.0, 1.0, PAGE.0, PAGE.1);
    keeps_current_page_law(&reading);
}

#[test]
fn a_taller_cell_does_not_override_the_faces_actual_ink() {
    let ordinary = live_reading(19.0, 1.0, PAGE.0, PAGE.1);
    let padded = live_reading(19.0, 1.5, PAGE.0, PAGE.1);
    assert!(
        padded.cell_height > ordinary.cell_height,
        "the padding is real"
    );
    assert_eq!(
        padded.thin_ink, ordinary.thin_ink,
        "padding does not thicken the face"
    );
    assert_eq!(padded.admitted, ordinary.admitted);
    keeps_current_page_law(&ordinary);
    keeps_current_page_law(&padded);
}
