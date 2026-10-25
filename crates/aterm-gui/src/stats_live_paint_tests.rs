// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The deck painter's live edge (`docs/DESIGN-stats-deck-live-2026-10-24.md`): the now marker at
//! each strip's right end, the strips' glide, the grey of a stalled deck, and the region patch
//! that draws a live frame. The clock and the schedule are `stats_live_tests.rs`.

use super::paint_tests::{AT, Scene, scene, scene_input};
use super::*;
use crate::stats_live::{BEAT_REACH, DOT_R, DOT_RISE, LINE_DX};

/// 10:42:10: a 10 s column opens (the scene's reading, 10:42:17, moved back to it).
const OPEN: i64 = AT - 7_000;

fn geom_at(w: f64, s: f64, form: DeckForm) -> DeckGeom {
    let h = deck_px(StatsBars::Deck, form, s, (14.0 * s) as usize) as f64;
    DeckGeom {
        w: (w * s) as f32,
        h: h as f32,
        pad: (12.0 * s) as f32,
        scale: s as f32,
        font_px: (13.0 * s) as f32,
    }
}

/// One strip the deck draws: the strip, its header's baseline, and its first row's top, its
/// cells' height and its last row's bottom, pt.
#[derive(Clone, Copy, Debug)]
struct Drawn {
    strip: Strip,
    base: f64,
    top: f64,
    cell: f64,
    last: f64,
}

/// Each strip the deck draws in `form` at `w`.
fn strips(form: DeckForm, w: f64) -> Vec<Drawn> {
    let m = deck_metrics();
    match form {
        DeckForm::Full => {
            let p = plan::plan_full(w, &m);
            vec![Drawn {
                strip: p.strip,
                base: HEADER_BASE_FULL,
                top: p.rows.top(0),
                cell: p.rows.cell,
                last: p.rows.top(DECK_RESOURCES.len() as u32 - 1) + p.rows.cell,
            }]
        }
        DeckForm::Folded => {
            let p = plan::plan_folded(w, &m);
            // Block A holds the first four rows, block B the rest (`Deck::folded`).
            p.blocks
                .iter()
                .zip([4, DECK_RESOURCES.len() - 4])
                .map(|(b, rows)| Drawn {
                    strip: b.strip,
                    base: HEADER_BASE_FOLDED,
                    top: b.rows.top(0),
                    cell: b.rows.cell,
                    last: b.rows.top(rows as u32 - 1) + b.rows.cell,
                })
                .collect()
        }
    }
}

fn paint(snap: &Snapshot, form: DeckForm, w: f64, s: f64, live: DeckLive) -> Vec<DrawPrim> {
    let mut input = scene_input(snap, form);
    input.live = live;
    deck_paint(&geom_at(w, s, form), &input).0
}

fn px(v: f64, s: f64) -> f32 {
    (v * s + 0.5).floor() as f32
}

/// The left edges of the filled cells of `d`'s first row (the heat panels, which stand on the
/// row's foot), sorted.
fn row_cells(prims: &[DrawPrim], d: &Drawn, s: f64) -> Vec<f32> {
    let strip = &d.strip;
    let (x0, cw, foot) = (
        px(strip.x, s),
        px(strip.cell, s),
        px(d.top, s) + px(d.cell, s),
    );
    let pitch = px(strip.pitch, s);
    let mut xs: Vec<f32> = prims
        .iter()
        .filter_map(|p| match *p {
            DrawPrim::Panel { x, y, w, h, .. }
                if (w - cw).abs() < 0.01
                    && (y + h - foot).abs() < 0.01
                    && x > x0 - pitch - 0.01
                    && x < x0 + strip.cols as f32 * pitch =>
            {
                Some(x)
            }
            _ => None,
        })
        .collect();
    xs.sort_by(f32::total_cmp);
    xs
}

/// The left edges of the unread (hollow) cells of `d`'s first row, sorted.
fn row_hollow(prims: &[DrawPrim], d: &Drawn, s: f64) -> Vec<f32> {
    let strip = &d.strip;
    let (cw, top) = (px(strip.cell, s), px(d.top, s));
    let mut xs: Vec<f32> = prims
        .iter()
        .filter_map(|p| match *p {
            DrawPrim::Stroke { x, y, w, .. }
                if (w - (cw - 1.0)).abs() < 0.01 && (y - 0.5 - top).abs() < 0.01 =>
            {
                Some(x - 0.5)
            }
            _ => None,
        })
        .collect();
    xs.sort_by(f32::total_cmp);
    xs
}

fn dots(prims: &[DrawPrim]) -> Vec<(f32, f32, f32, [u8; 4])> {
    prims
        .iter()
        .filter_map(|p| match *p {
            DrawPrim::Dot { cx, cy, r, color } => Some((cx, cy, r, color)),
            _ => None,
        })
        .collect()
}

fn rings_at(prims: &[DrawPrim], at: (f32, f32)) -> Vec<(f32, [u8; 4])> {
    prims
        .iter()
        .filter_map(|p| match *p {
            DrawPrim::Ring {
                cx,
                cy,
                r_outer,
                fill,
                ..
            } if (cx, cy) == at => Some((r_outer, fill)),
            _ => None,
        })
        .collect()
}

/// The now marker's line under `cx`: its top and bottom, device px, and its ink.
fn line_under(prims: &[DrawPrim], cx: f32) -> Option<(f32, f32, [u8; 4])> {
    prims.iter().find_map(|p| match *p {
        DrawPrim::Panel {
            x, y, w, h, fill, ..
        } if w <= 2.0 && x <= cx && cx <= x + w + 0.5 && h > 10.0 => Some((y, y + h, fill)),
        _ => None,
    })
}

fn ink(c: Rgb, a: u8) -> [u8; 4] {
    rgba(c, a)
}

/// The far right of every strip is now: its newest column ends exactly at the strip's right
/// end, and the marker stands just past it, a dot on the header line in the words' ink and a
/// line from it down every row, at both scales and in both forms.
#[test]
fn the_newest_column_ends_under_the_now_marker() {
    let snap = scene(Scene::Calm);
    let inks = super::tests::dark_inks();
    for (form, w) in [
        (DeckForm::Full, 2560.0),
        (DeckForm::Full, 1424.0),
        (DeckForm::Full, 1119.0),
        (DeckForm::Full, 800.0),
        (DeckForm::Full, 550.0),
        (DeckForm::Folded, 1119.0),
        (DeckForm::Folded, 800.0),
    ] {
        for s in [1.0, 2.0] {
            let prims = paint(&snap, form, w, s, DeckLive::STILL);
            let drawn = dots(&prims);
            let strips = strips(form, w);
            assert!(!strips.is_empty());
            for d in strips {
                let Drawn {
                    strip,
                    base,
                    top,
                    last,
                    ..
                } = d;
                let at = format!("{form:?} {w} pt {s}x, strip at {}", strip.x);
                let cells = row_cells(&prims, &d, s);
                assert_eq!(
                    cells.len(),
                    strip.cols as usize,
                    "{at}: the first row's cells"
                );
                let newest = cells.last().copied().unwrap_or_default();
                assert!(
                    (newest + px(strip.cell, s) - px(strip.end(), s)).abs() < 0.01,
                    "{at}: the newest column ends at {} not {}",
                    newest + px(strip.cell, s),
                    px(strip.end(), s)
                );
                let (cx, cy) = (
                    ((strip.end() + LINE_DX) * s) as f32,
                    ((base - DOT_RISE) * s) as f32,
                );
                let dot = drawn
                    .iter()
                    .find(|d| (d.0, d.1) == (cx, cy))
                    .unwrap_or_else(|| panic!("{at}: no dot at {cx},{cy} in {drawn:?}"));
                assert_eq!(dot.2, (DOT_R * s) as f32, "{at}");
                assert_eq!(dot.3, ink(inks.words, 0xFF), "{at}: the dot's ink");
                assert!(
                    cx > newest + px(strip.cell, s) - 0.01,
                    "{at}: the dot is past the strip"
                );
                let (y0, y1, line) =
                    line_under(&prims, cx).unwrap_or_else(|| panic!("{at}: no line"));
                assert!(
                    y0 <= px(top, s) && y1 >= px(last, s) - 0.01,
                    "{at}: the line {y0}..{y1}"
                );
                assert_eq!(line, ink(inks.words, crate::stats_live::LINE_ALPHA), "{at}");
                assert!(
                    rings_at(&prims, (cx, cy)).is_empty(),
                    "{at}: a still deck rings nothing"
                );
            }
        }
    }
}

/// Between readings the strips glide left with the wall clock: a reading on a column's first
/// instant draws every closed column in its slot, half a period later each sits half a pitch
/// right of it with the open column's first half showing at the right end, and the strip clips
/// what passes its ends. Still frames (no motion) stand every column in its slot.
#[test]
fn the_strips_glide_left_by_the_part_of_the_period_run() {
    let mut snap = scene(Scene::Calm);
    snap.at_unix_ms = OPEN;
    let (form, w) = (DeckForm::Full, 1424.0);
    let d = strips(form, w)[0];
    let strip = d.strip;
    assert_eq!(strip.period_s, 10, "CONTROL: the wide strip's 10 s columns");
    for s in [1.0, 2.0] {
        let glide = |ms: i64| DeckLive {
            glide_ms: Some(ms),
            beat: None,
            fresh: true,
        };
        let still = paint(&snap, form, w, s, DeckLive::STILL);
        let slots = row_cells(&still, &d, s);
        assert_eq!(slots.len(), strip.cols as usize, "CONTROL");
        // The strips' clips: the gauge left of the names clips its own bands.
        let clips = |prims: &[DrawPrim]| {
            prims
                .iter()
                .filter(
                    |p| matches!(p, DrawPrim::ClipPush { x, .. } if *x >= px(strip.x - 20.0, s)),
                )
                .count()
        };
        assert_eq!(clips(&still), 0, "{s}x: a still strip needs no clip");
        // A whole period run (or more) is the still frame, prim for prim.
        let ran = paint(&snap, form, w, s, glide(OPEN + 10_000));
        assert_eq!(
            prims_fp(&ran),
            prims_fp(&still),
            "{s}x: a run-out glide moved"
        );
        assert_eq!(
            prims_fp(&paint(&snap, form, w, s, glide(OPEN + 60_000))),
            prims_fp(&still),
            "{s}x"
        );
        assert!(
            row_hollow(&still, &d, s).is_empty(),
            "CONTROL: every slot read"
        );
        let pitch = px(strip.pitch, s);
        let x0 = px(strip.x, s);
        // t = 0: the closed columns in their slots, the open one not yet on the strip: each
        // column stands one slot left of where the still frame drew it, so the oldest slot holds
        // the column before the span (unread in this scene, whose readings span ten minutes).
        let t0 = paint(&snap, form, w, s, glide(OPEN));
        assert_eq!(row_cells(&t0, &d, s), slots[1..], "{s}x: t = 0");
        assert_eq!(row_hollow(&t0, &d, s), [x0], "{s}x: t = 0");
        assert_eq!(
            clips(&t0),
            DECK_RESOURCES.len(),
            "{s}x: each row clips its strip"
        );
        // t = half the period: every column half a pitch right of its slot, the one before the
        // span half in at the left, and the open column's first half at the right end.
        let half = paint(&snap, form, w, s, glide(OPEN + 5_000));
        let shifted: Vec<f32> = slots.iter().map(|x| x + pitch / 2.0).collect();
        assert_eq!(row_cells(&half, &d, s), shifted, "{s}x: t = half");
        assert_eq!(
            row_hollow(&half, &d, s),
            [x0 - pitch / 2.0],
            "{s}x: t = half"
        );
        let right = px(strip.end(), s);
        assert!(
            shifted
                .last()
                .is_some_and(|x| *x < right && x + px(strip.cell, s) > right),
            "{s}x: the open column runs off the strip's end, clipped"
        );
    }
}

/// When readings stop the marker turns grey and hollow and rings nothing, so a frozen deck never
/// looks live; while they come a beat's ring widens around the dot and fades.
#[test]
fn a_stalled_deck_greys_its_marker_and_beats_nothing() {
    let snap = scene(Scene::Calm);
    let inks = super::tests::dark_inks();
    let (form, w, s) = (DeckForm::Full, 1424.0, 2.0);
    let Drawn { strip, base, .. } = strips(form, w)[0];
    let at = (
        ((strip.end() + LINE_DX) * s) as f32,
        ((base - DOT_RISE) * s) as f32,
    );
    let stale = paint(&snap, form, w, s, DeckLive::STALE);
    assert!(
        dots(&stale).iter().all(|d| (d.0, d.1) != at),
        "a stalled deck draws no solid dot"
    );
    assert_eq!(
        rings_at(&stale, at),
        vec![((DOT_R * s) as f32, ink(inks.hollow, 0xFF))],
        "a hollow grey ring in the dot's place"
    );
    let (_, _, line) = line_under(&stale, at.0).expect("the line stays");
    assert_eq!(line, ink(inks.hollow, 0xFF), "the line greys");
    assert_ne!(
        inks.hollow, inks.words,
        "CONTROL: grey is not the words' ink"
    );

    let beat = |phase| DeckLive {
        glide_ms: None,
        beat: Some(phase),
        fresh: true,
    };
    let early = paint(&snap, form, w, s, beat(0.25));
    let late = paint(&snap, form, w, s, beat(0.75));
    let ring = |prims: &[DrawPrim]| rings_at(prims, at).first().copied();
    let (r_early, c_early) = ring(&early).expect("a beat rings");
    let (r_late, c_late) = ring(&late).expect("a beat rings to its end");
    assert!(
        r_early > (DOT_R * s) as f32 && r_late > r_early,
        "{r_early} then {r_late}"
    );
    assert!(r_late <= ((DOT_R + BEAT_REACH) * s) as f32);
    assert!(
        c_early[3] > c_late[3] && c_early[3] < 0xFF,
        "{c_early:?} then {c_late:?}"
    );
    assert!(
        dots(&early).iter().any(|d| (d.0, d.1) == at),
        "the dot stays over its beat"
    );
    assert!(
        ring(&paint(&snap, form, w, s, beat(0.999))).is_none(),
        "the beat ends unseen"
    );
}

/// A frame that moves only the live edge re-rasters only the card's rects its layers moved, and
/// what it gives is the whole raster of the new frame, byte for byte.
#[test]
fn a_live_frame_patches_only_what_moved_to_the_whole_rasters_bytes() {
    let mut snap = scene(Scene::Calm);
    snap.at_unix_ms = OPEN;
    let still = DeckLive::STILL;
    let gliding = |ms| DeckLive {
        glide_ms: Some(ms),
        beat: Some(0.0),
        fresh: true,
    };
    let cases = [
        (
            DeckForm::Full,
            1424.0,
            1.0,
            gliding(OPEN + 2_000),
            gliding(OPEN + 2_250),
        ),
        (DeckForm::Full, 1119.0, 2.0, still, DeckLive::STALE),
        (
            DeckForm::Folded,
            1119.0,
            1.0,
            gliding(OPEN + 2_000),
            DeckLive {
                beat: Some(0.5),
                ..gliding(OPEN + 2_000)
            },
        ),
        (DeckForm::Folded, 1119.0, 1.0, gliding(OPEN + 2_000), still),
    ];
    for (form, w, s, was, now) in cases {
        let at = format!("{form:?} {w} pt {s}x, {was:?} to {now:?}");
        let g = geom_at(w, s, form);
        let mut input = scene_input(&snap, form);
        input.live = was;
        let (before, _, _) = deck_paint_live(&g, &input);
        input.live = now;
        let (after, _, out) = deck_paint_live(&g, &input);
        let whole = |prims: &[DrawPrim]| {
            crate::tray_raster::rasterize_tray(prims, g.w as u32, g.h as u32, 1.0, [0, 0, 0, 0])
        };
        let (rgba, pw, ph) = whole(&before);
        let mut card = crate::SettingsCard {
            rgba,
            pw,
            ph,
            dx: 0,
            dy: 0,
            fp: 1,
            geom: 1,
            raster_fingerprint_once: std::sync::OnceLock::new(),
            scrim: None,
        };
        let (want, _, _) = whole(&after);
        assert_ne!(card.rgba, want, "CONTROL {at}: the frames differ");
        assert!(
            patch_live(&mut card, &after, &out.rects, &was, &now),
            "{at}"
        );
        assert!(card.rgba == want, "{at}: the patch is not the whole raster");
        // A beat or a grey moves only the marker's narrow rects.
        if was.glide_ms == now.glide_ms {
            let moved: f32 = out
                .rects
                .iter()
                .filter(|(layer, _)| layer.moved(&was, &now))
                .map(|(_, r)| r.2 * r.3)
                .sum();
            assert!(
                moved < 0.05 * pw as f32 * ph as f32,
                "{at}: {moved} px of {}",
                pw * ph
            );
        }
    }
}

/// A beat's patch costs its narrow rects, not the card: of the text the whole raster blends, it
/// blends only the glyphs over its rects (`Canvas::texels_meet_canvas`), so a beat frame touches
/// under 5% of the card's glyphs as it does of its pixels.
#[test]
fn a_beat_patch_blends_only_the_glyphs_over_its_rects() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut snap = scene(Scene::Calm);
    snap.at_unix_ms = OPEN;
    let was = DeckLive {
        glide_ms: Some(OPEN + 2_000),
        beat: Some(0.0),
        fresh: true,
    };
    let now = DeckLive {
        beat: Some(0.5),
        ..was
    };
    // The Folded deck at 1524 pt 2x is the window the cost was measured in.
    for (form, w, s) in [
        (DeckForm::Full, 1524.0, 2.0),
        (DeckForm::Folded, 1524.0, 2.0),
        (DeckForm::Folded, 1119.0, 1.0),
    ] {
        let at = format!("{form:?} {w} pt {s}x");
        let g = geom_at(w, s, form);
        let mut input = scene_input(&snap, form);
        input.live = was;
        let (before, _, _) = deck_paint_live(&g, &input);
        input.live = now;
        let (after, _, out) = deck_paint_live(&g, &input);
        let blits = crate::tray_raster::glyph_blits();
        let (rgba, pw, ph) =
            crate::tray_raster::rasterize_tray(&before, g.w as u32, g.h as u32, 1.0, [0, 0, 0, 0]);
        let whole = crate::tray_raster::glyph_blits() - blits;
        assert!(
            whole > 100,
            "CONTROL {at}: the card's text blends {whole} glyphs"
        );
        let mut card = crate::SettingsCard {
            rgba,
            pw,
            ph,
            dx: 0,
            dy: 0,
            fp: 1,
            geom: 1,
            raster_fingerprint_once: std::sync::OnceLock::new(),
            scrim: None,
        };
        let blits = crate::tray_raster::glyph_blits();
        assert!(
            patch_live(&mut card, &after, &out.rects, &was, &now),
            "{at}"
        );
        let patched = crate::tray_raster::glyph_blits() - blits;
        assert!(
            patched * 20 < whole,
            "{at}: the beat's patch blends {patched} of the card's {whole} glyphs"
        );
    }
}

/// The scale's words and tick marks are drawn where the header plan puts them, and the marker's
/// dot is a box the overlap sweep (T-G4) reads.
#[test]
fn the_header_draws_the_scale_and_the_marker_as_planned() {
    let m = FaceMetrics::helvetica_neue();
    let snap = scene(Scene::Calm);
    for form in [DeckForm::Full, DeckForm::Folded] {
        let input = scene_input(&snap, form);
        let words = DeckWords::new(&input, &m);
        let laid = lay(&geom_at(1424.0, 2.0, form), &input, &words, &m, true);
        let head = laid.head.as_ref().expect("a header");
        assert!(!head.ruler.is_empty(), "{form:?}: a scale");
        for tick in &head.ruler {
            assert!(
                laid.boxes.iter().any(|b| b.kind == BoxKind::Text
                    && b.text == tick.text
                    && (b.r[0] - tick.left).abs() < 1e-6),
                "{form:?}: {} not drawn at {}",
                tick.text,
                tick.left
            );
            let x = (tick.x * 2.0) as f32;
            assert!(
                laid.prims.iter().any(|p| matches!(*p,
                    DrawPrim::Panel { x: px0, w, h, .. }
                        if w <= 1.01 && h <= 4.0 && (px0 - x).abs() <= 1.0)),
                "{form:?}: no tick mark at {x}"
            );
        }
        let marks = laid
            .boxes
            .iter()
            .filter(|b| b.kind == BoxKind::Mark && b.text == "now")
            .count();
        assert_eq!(
            marks,
            if form == DeckForm::Full { 1 } else { 2 },
            "{form:?}: one marker per strip"
        );
    }
}

/// T-G9's budget holds while the strips glide and the dot beats: the column sliding in, the
/// clips, the marker and the scale stay inside the worst case's 1000 prims.
#[test]
fn a_live_frame_keeps_the_prim_budget() {
    let snap = scene(Scene::Costly);
    let live = DeckLive {
        glide_ms: Some(snap.at_unix_ms + 1),
        beat: Some(0.0),
        fresh: true,
    };
    for form in [DeckForm::Full, DeckForm::Folded] {
        let still = paint(&snap, form, 2560.0, 2.0, DeckLive::STILL).len();
        let moving = paint(&snap, form, 2560.0, 2.0, live).len();
        assert!(
            moving > still,
            "CONTROL {form:?}: the live frame draws more ({moving})"
        );
        assert!(moving <= 1000, "{form:?}: {moving} prims");
    }
}

/// The live edge's frames for a person to read: still, gliding half a period, beating, and
/// stalled, in both forms and grounds, at 2× and 1×.
#[test]
#[ignore = "writes PNGs for a person to read: set ATERM_DECK_GALLERY to a directory"]
fn deck_live_gallery() {
    let Some(dir) = std::env::var_os("ATERM_DECK_GALLERY") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("the gallery directory");
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut snap = scene(Scene::Calm);
    snap.at_unix_ms = OPEN;
    let frames = [
        ("still", DeckLive::STILL),
        (
            "glide_half",
            DeckLive {
                glide_ms: Some(OPEN + 5_000),
                beat: None,
                fresh: true,
            },
        ),
        (
            "beat",
            DeckLive {
                glide_ms: Some(OPEN + 1_000),
                beat: Some(0.2),
                fresh: true,
            },
        ),
        ("stalled", DeckLive::STALE),
    ];
    let mut written = 0;
    for (name, live) in frames {
        for (w, s) in [(1119.0, 2.0), (550.0, 2.0), (1119.0, 1.0)] {
            for form in [DeckForm::Full, DeckForm::Folded] {
                for (ground, inks) in [
                    ("dark", super::tests::dark_inks()),
                    ("light", super::paint_tests::light_inks()),
                ] {
                    let mut input = scene_input(&snap, form);
                    input.inks = inks;
                    input.live = live;
                    let g = geom_at(w, s, form);
                    let (prims, _) = deck_paint(&g, &input);
                    let (rgba, pw, ph) = crate::tray_raster::rasterize_tray(
                        &prims,
                        g.w as u32,
                        g.h as u32,
                        1.0,
                        [0, 0, 0, 0],
                    );
                    let png = crate::app_introspect::encode_rgba8_png(&rgba, pw, ph)
                        .expect("the frame encodes");
                    let form = if form == DeckForm::Full {
                        "full"
                    } else {
                        "folded"
                    };
                    let file = dir.join(format!("live_{name}_{w}pt_{s}x_{form}_{ground}.png"));
                    std::fs::write(&file, png).expect("the PNG writes");
                    written += 1;
                }
            }
        }
    }
    assert_eq!(written, frames.len() * 3 * 2 * 2);
}
