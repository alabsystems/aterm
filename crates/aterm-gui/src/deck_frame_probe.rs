// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The deck's frame, timed stage by stage ([`deck_frame_cost`], `benches/deck_frame_cost.rs`)
//! over the merged deck's busy and calm scenes ([`aterm_vitals::fixtures`]) at the mockups'
//! widths: a measurement for a person, never a gate.

use std::time::Instant;

use aterm_vitals::fixtures::{self, DeckScene};

use crate::stats_deck::deck_fixtures::DeckContext;
use crate::stats_deck::{DeckGeom, StatsBars, deck_paint, deck_px, fold_cards, prims_fp};
use crate::stats_plan::DeckForm;

/// The window widths timed, in points, and their scales: the mockups' 2238, 1100 and 700
/// device px at 2×, and the widest at 1×.
const WIDTHS: [(f64, f64); 4] = [(1119.0, 2.0), (550.0, 2.0), (350.0, 2.0), (1119.0, 1.0)];

/// One stage of one case: µs per call at p50, p90 and p99, and the deck's prim count.
pub(crate) struct StageCost {
    pub(crate) what: String,
    pub(crate) prims: usize,
    pub(crate) p50_us: f64,
    pub(crate) p90_us: f64,
    pub(crate) p99_us: f64,
}

/// Each stage a reading puts the deck through, `frames` calls each, for the busy and calm scenes
/// at each of [`WIDTHS`] on the mockups' dark ground: the layout and paint (`deck_paint`), the
/// prims' fingerprint (`prims_fp`), the card's raster (`rasterize_tray`), the deck card's share
/// of the fold (`fold`: its composite onto the fold's surface; an open lens or a top card adds
/// its own), and the four in turn (`frame`), a reading's whole cost with a lens open.
pub(crate) fn deck_frame_cost(frames: usize) -> Vec<StageCost> {
    crate::tray_raster::set_chrome_fonts(None, None, None);
    let inks =
        aterm_vitals::ink::vitals_inks([0x30, 0x31, 0x35], [0xF8, 0xF8, 0xF2], [0x50, 0xFA, 0x7B]);
    let mut out = Vec::new();
    for scene in [DeckScene::Busy, DeckScene::Calm] {
        let snap = fixtures::snapshot(scene);
        let input = DeckContext::of(scene).input(Some(&snap), DeckForm::Full, inks);
        for (w, s) in WIDTHS {
            let h = deck_px(StatsBars::Deck, DeckForm::Full, s, (14.0 * s) as usize);
            let g = DeckGeom {
                w: (w * s) as f32,
                h: h as f32,
                pad: (12.0 * s) as f32,
                scale: s as f32,
                font_px: (13.0 * s) as f32,
            };
            let (prims, _) = deck_paint(&g, &input);
            let raster = |prims: &[crate::widget::DrawPrim]| {
                crate::tray_raster::rasterize_tray(prims, g.w as u32, g.h as u32, 1.0, [0; 4])
            };
            let card = card_of(raster(&prims), prims_fp(&prims));
            let case = format!("{} {} px {s}×", scene.name(), g.w);
            let mut stage = |name: &str, run: &mut dyn FnMut()| {
                let (p50_us, p90_us, p99_us) = timed(frames, run);
                out.push(StageCost {
                    what: format!("{case} {name}"),
                    prims: prims.len(),
                    p50_us,
                    p90_us,
                    p99_us,
                });
            };
            stage("deck_paint", &mut || {
                std::hint::black_box(deck_paint(&g, &input));
            });
            stage("prims_fp", &mut || {
                std::hint::black_box(prims_fp(&prims));
            });
            stage("rasterize_tray", &mut || {
                std::hint::black_box(raster(&prims));
            });
            stage("fold", &mut || {
                std::hint::black_box(fold_cards(&[&card], card.fp));
            });
            stage("frame", &mut || {
                let (prims, _) = deck_paint(&g, &input);
                let fp = prims_fp(&prims);
                let card = card_of(raster(&prims), fp);
                std::hint::black_box(fold_cards(&[&card], fp));
            });
        }
    }
    out
}

/// The deck's card of a raster, as `splice_stats_deck` keeps it, at the fold's origin.
fn card_of((rgba, pw, ph): (Vec<u8>, u32, u32), fp: u64) -> crate::SettingsCard {
    crate::SettingsCard {
        scrim: None,
        rgba,
        pw,
        ph,
        dx: 0,
        dy: 0,
        fp,
        geom: 1,
        raster_fingerprint_once: std::sync::OnceLock::new(),
    }
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
