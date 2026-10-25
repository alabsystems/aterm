// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An open overlay card's redraw, timed ([`overlay_card_cost`], `benches/overlay_card_cost.rs`):
//! what a frame that changes the card pays, its tray's layout and paint and then
//! [`crate::overlay::card_raster`], for the Overview on v5's closing scene
//! ([`crate::overview::closing_scene`]) and for the palette, on one window at 1× and at 2×, and at
//! 2× with the width cap read as device pixels (`px_per_pt` 1, as before `fix/card-cap-in-points`)
//! to compare: a measurement for a person, never a gate.

use std::time::Instant;

use crate::chrome_band::ChromePalette;
use crate::overlay::OverlayModel;
use crate::settings::SettingsGeom;

/// One card at one scale: µs per redraw at p50, p90 and p99, and the raster's size in pixels.
pub(crate) struct CardCost {
    pub(crate) what: String,
    pub(crate) px: (u32, u32),
    pub(crate) p50_us: f64,
    pub(crate) p90_us: f64,
    pub(crate) p99_us: f64,
}

/// The window timed, at backing scale `scale`: 144 cells of 7 points by 40 rows of 16 points,
/// its type 13 points, the width cap read at `px_per_pt`.
fn window(scale: f32, px_per_pt: f32) -> SettingsGeom {
    SettingsGeom {
        cw: 7.0 * scale,
        ch: 16.0 * scale,
        font_px: 13.0 * scale,
        cols: 144,
        panel_rows: 40,
        px_per_pt,
    }
}

/// Each card's redraw, `frames` times per case, on the default theme's chrome.
pub(crate) fn overlay_card_cost(frames: usize) -> Vec<CardCost> {
    crate::tray_raster::set_chrome_fonts(None, None, None);
    let palette = ChromePalette::from(aterm_render::Theme::default());
    let overview = crate::overview::closing_scene();
    let menu = crate::palette::PaletteState::new();
    let cards: [(&str, &dyn OverlayModel); 2] = [("overview", &overview), ("palette", &menu)];
    let cases = [
        ("1x", 1.0, 1.0),
        ("2x", 2.0, 2.0),
        ("2x, cap in device px", 2.0, 1.0),
    ];
    let mut out = Vec::new();
    for (card, model) in cards {
        for (case, scale, px_per_pt) in cases {
            let g = window(scale, px_per_pt);
            let (tray_w, tray_h) = (g.cols as f32 * g.cw, g.panel_rows as f32 * g.ch);
            let redraw = || crate::overlay::card_raster(model.tray(&g, palette), tray_w, tray_h);
            let px = redraw().map_or((0, 0), |cut| (cut.pw, cut.ph));
            let (p50_us, p90_us, p99_us) = timed(frames, &mut || {
                std::hint::black_box(redraw());
            });
            out.push(CardCost {
                what: format!("{card} {case}"),
                px,
                p50_us,
                p90_us,
                p99_us,
            });
        }
    }
    out
}

/// `run`'s µs per call at p50, p90 and p99 over `frames` calls, after 16 unmeasured ones.
fn timed(frames: usize, run: &mut dyn FnMut()) -> (f64, f64, f64) {
    for _ in 0..16 {
        run();
    }
    let mut micros: Vec<f64> = (0..frames.max(1))
        .map(|_| {
            let t = Instant::now();
            run();
            t.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    micros.sort_by(f64::total_cmp);
    let at = |q: f64| micros[((micros.len() - 1) as f64 * q) as usize];
    (at(0.5), at(0.9), at(0.99))
}
