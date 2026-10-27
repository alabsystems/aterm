// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The deck painter's live edge (`docs/DESIGN-stats-deck-live-2026-10-24.md`, now at the left
//! since `docs/DESIGN-stats-deck-now-left-2026-10-26.md`): the now marker at each strip's left
//! end, the fine band's glide and slide, the grey of a stalled deck, and the region patch that
//! draws a live frame. The clock and the schedule are `stats_live_tests.rs`.

use super::paint_tests::{AT, Scene, scene, scene_input};
use super::*;
use crate::stats_live::{BEAT_REACH, DOT_R, DOT_RISE, FineBand, FlowAt, LINE_DX};

/// A frame whose fine band has run `ran` of the step of a reading that opened a second.
fn flowing(ran: f64) -> DeckLive {
    DeckLive {
        flow: Some(FlowAt {
            opened_ms: 1_000,
            ran,
        }),
        ..DeckLive::STILL
    }
}

/// 10:42:10: a second opens (the scene's reading, 10:42:17, moved back to it).
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

/// The `(x, w)` spans of the filled multi-scale cells of `d`'s first row, sorted left to right.
fn row_cells_xw(prims: &[DrawPrim], d: &Drawn, s: f64) -> Vec<(f32, f32)> {
    let strip = &d.strip;
    let (x0, x1, top, cell_h) = (
        px(strip.x, s),
        px(strip.end(), s),
        px(d.top, s),
        px(d.cell, s),
    );
    let pitch = px(strip.pitch, s);
    let mut xs: Vec<(f32, f32)> = prims
        .iter()
        .filter_map(|p| match *p {
            DrawPrim::Panel { x, y, w, h, .. }
                if (y - top).abs() < 0.01
                    && (h - cell_h).abs() < 0.01
                    && x + w > x0 - pitch - 0.01
                    && x < x1 + pitch + 0.01 =>
            {
                Some((x, w))
            }
            _ => None,
        })
        .collect();
    xs.sort_by(|a, b| a.0.total_cmp(&b.0));
    xs
}

/// The left edges of the filled cells of `d`'s first row, sorted.
fn row_cells(prims: &[DrawPrim], d: &Drawn, s: f64) -> Vec<f32> {
    row_cells_xw(prims, d, s)
        .into_iter()
        .map(|(x, _)| x)
        .collect()
}

/// The left edges of the unread (hollow) cells of `d`'s first row, sorted.
fn row_hollow(prims: &[DrawPrim], d: &Drawn, s: f64) -> Vec<f32> {
    let strip = &d.strip;
    let (x0, x1, top, cell_h) = (
        px(strip.x, s),
        px(strip.end(), s),
        px(d.top, s),
        px(d.cell, s),
    );
    let pitch = px(strip.pitch, s);
    let hollow = rgba(super::tests::dark_inks().hollow, 0xFF);
    let mut xs: Vec<f32> = prims
        .iter()
        .filter_map(|p| match *p {
            DrawPrim::Stroke {
                x, y, w, h, color, ..
            } if color == hollow
                && (y - 0.5 - top).abs() < 0.01
                && (h - (cell_h - 1.0)).abs() < 0.01
                && x + w > x0 - pitch - 0.01
                && x < x1 + pitch + 0.01 =>
            {
                Some(x - 0.5)
            }
            DrawPrim::Panel {
                x, y, w, h, fill, ..
            } if fill == hollow
                && (y - (top + cell_h - 1.0)).abs() < 0.01
                && (h - 1.0).abs() < 0.01
                && x + w > x0 - pitch - 0.01
                && x < x1 + pitch + 0.01 =>
            {
                Some(x)
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

/// The far left of every strip is now: its newest box, the live one, starts exactly at the
/// strip's left end, the older boxes widen toward its right, and the marker stands just left of
/// it, between the % column and the live box: a dot on the header line in the words' ink and a
/// line from it down every row, at both scales and in both forms.
#[test]
fn the_newest_column_starts_at_the_now_marker() {
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
        (DeckForm::Folded, 600.0),
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
                let expected = multi_scale_slot_rects(&strip, s, 0.0);
                let cells_xw = row_cells_xw(&prims, &d, s);
                let hollow = row_hollow(&prims, &d, s);
                assert_eq!(
                    cells_xw.len() + hollow.len(),
                    expected.len(),
                    "{at}: the first row's multi-scale slots (filled + hollow)"
                );
                assert!(expected[0].3, "{at}: the live box first: {expected:?}");
                if expected.len() >= 3 {
                    assert!(
                        expected[expected.len() - 1].2 >= expected[1].2,
                        "{at}: progressive boxes grow wider toward the right: {expected:?}"
                    );
                }
                let &(newest, newest_w) = cells_xw.first().expect("live now box filled");
                assert!(
                    (newest - px(strip.x, s)).abs() < 0.01
                        && (newest_w - expected[0].2).abs() < 0.01,
                    "{at}: the newest live box starts at {newest} not {}",
                    px(strip.x, s)
                );
                let &(_, oldest, oldest_w, _) = expected.last().expect("history");
                assert!(
                    (oldest + oldest_w - px(strip.end(), s)).abs() < 0.01,
                    "{at}: the oldest box ends at the horizon"
                );
                let (cx, cy) = (
                    ((strip.x - LINE_DX) * s) as f32,
                    ((base - DOT_RISE) * s) as f32,
                );
                let dot = drawn
                    .iter()
                    .find(|d| (d.0, d.1) == (cx, cy))
                    .unwrap_or_else(|| panic!("{at}: no dot at {cx},{cy} in {drawn:?}"));
                assert_eq!(dot.2, (DOT_R * s) as f32, "{at}");
                assert_eq!(dot.3, ink(inks.words, 0xFF), "{at}: the dot's ink");
                assert!(cx < newest - 0.01, "{at}: the dot is left of the strip");
                let (y0, y1, line) =
                    line_under(&prims, cx).unwrap_or_else(|| panic!("{at}: no line"));
                assert!(
                    y0 <= px(top, s) && y1 >= px(last, s) - 0.01,
                    "{at}: the line {y0}..{y1}"
                );
                assert_eq!(
                    line,
                    ink(inks.words, 0xFF),
                    "{at}: the line in solid words ink"
                );
                assert!(
                    rings_at(&prims, (cx, cy)).is_empty(),
                    "{at}: a still deck rings nothing"
                );
            }
        }
    }
}

/// Between readings Full's fine band glides right with the wall clock (the now-left design,
/// §3.4): at the reading it lags its slots by the reading's step (a second on 1 s boxes, a whole
/// box), half way through its run by half that (rounded up to a whole pixel), and once the run
/// is out every box stands in its slot. The band's clip cuts what passes its ends; the live box
/// and the coarser boxes stand still. Still frames (no motion) stand every box in its slot. The
/// run itself, a slot from the reading on the wall clock, is `stats_live_tests.rs`'s.
#[test]
fn the_strips_glide_right_by_the_part_of_the_run() {
    use crate::stats_live::lag_px;
    let mut snap = scene(Scene::Calm);
    snap.at_unix_ms = OPEN + 500;
    let (form, w) = (DeckForm::Full, 1424.0);
    let d = strips(form, w)[0];
    let strip = d.strip;
    let band = fine_band_slots(&strip.spec.slots()).expect("a fine band");
    for s in [1.0, 2.0] {
        let glide = flowing;
        let still = paint(&snap, form, w, s, DeckLive::STILL);
        let all_xs = |prims: &[DrawPrim]| -> Vec<f32> {
            let mut v = row_cells(prims, &d, s);
            v.extend(row_hollow(prims, &d, s));
            v.sort_by(f32::total_cmp);
            v
        };
        let rest = multi_scale_slot_rects(&strip, s, 0.0);
        assert_eq!(all_xs(&still).len(), rest.len(), "CONTROL");
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
        // A run out (or long past) is the still frame, prim for prim.
        let ran = paint(&snap, form, w, s, glide(1.0));
        assert_eq!(
            prims_fp(&ran),
            prims_fp(&still),
            "{s}x: a run-out glide moved"
        );
        assert_eq!(
            prims_fp(&paint(&snap, form, w, s, glide(60.0))),
            prims_fp(&still),
            "{s}x"
        );
        let step = fine_band(&strip, s).step_px(1_000);
        assert!(
            step >= 2.0,
            "CONTROL {s}x: a second moves the band {step} px"
        );
        // At the reading the band lags its whole step, under the band's clip on each row.
        let t0 = paint(&snap, form, w, s, glide(0.0));
        let lagged = |lag: f64| -> Vec<f32> {
            multi_scale_slot_rects(&strip, s, lag as f32)
                .into_iter()
                .map(|r| r.1)
                .collect()
        };
        assert_eq!(all_xs(&t0), lagged(step), "{s}x: t = 0");
        assert_eq!(
            clips(&t0),
            DECK_RESOURCES.len(),
            "{s}x: each row clips its band"
        );
        // Half way through its run: half the step left to run.
        let half = paint(&snap, form, w, s, glide(0.5));
        let half_lag = lag_px(step, 0.5);
        assert!(half_lag > 0.0 && half_lag < step, "{s}x: {half_lag}");
        assert_eq!(all_xs(&half), lagged(half_lag), "{s}x: t = half");
        // Only the band moved: the live box and the coarser boxes stand in their slots.
        let standing = |lag: f64| -> Vec<(usize, f32, f32, bool)> {
            multi_scale_slot_rects(&strip, s, lag as f32)
                .into_iter()
                .filter(|r| !band.contains(&r.0))
                .collect()
        };
        assert_eq!(standing(half_lag), standing(0.0), "{s}x");
        let half_filled = row_cells_xw(&half, &d, s);
        let &(now_x, now_w) = half_filled.first().expect("anchored live now box");
        assert!(
            (now_x - px(strip.x, s)).abs() < 0.01 && (now_w - rest[0].2).abs() < 0.01,
            "{s}x: the live now box stays anchored at strip.x"
        );
    }
}

/// Every live frame draws every cell on a whole device pixel (review of 2026-10-25): a Full
/// strip's glide when the reading landed between two of the glide's steps (the clock behind it
/// counts as the reading's moment) and at any instant after, and a Folded strip's slide at every
/// phase its frames draw. Unrounded, those lags fall between pixels (the control), and the cell
/// edges blur.
#[test]
fn every_live_frame_draws_its_cells_on_whole_pixels() {
    use crate::stats_live::{glide_end_ms, glide_frac};
    let mut fractional = 0;
    let at = OPEN + 3_457;
    let mut snap = scene(Scene::Calm);
    snap.at_unix_ms = at;
    let end = glide_end_ms(at, snap.slot_grid);
    for w in [1424.0, 1119.0, 800.0, 550.0] {
        let d = strips(DeckForm::Full, w)[0];
        for s in [1.0, 2.0] {
            let step = fine_band(&d.strip, s).step_px(1_000);
            for glide_ms in [OPEN + 3_000, at, OPEN + 3_521, OPEN + 3_777, OPEN + 12_345] {
                let f = glide_frac(at, glide_ms, end);
                let prims = paint(&snap, DeckForm::Full, w, s, flowing(f));
                let xs = row_cells(&prims, &d, s);
                assert!(!xs.is_empty(), "CONTROL Full {w} pt {s}x");
                assert!(
                    xs.iter().all(|x| x.fract() == 0.0),
                    "Full {w} pt {s}x at {glide_ms}: {xs:?}"
                );
                fractional += usize::from(((1.0 - f) * step).fract() != 0.0);
            }
        }
    }
    for w in [600.0, 800.0, 1119.0] {
        let d = strips(DeckForm::Folded, w)[0];
        for s in [1.0, 2.0] {
            let step = fine_band(&d.strip, s).step_px(1_000);
            for slide in [0.25, 1.0 / 3.0, 0.5, 2.0 / 3.0, 0.75] {
                let prims = paint(&snap, DeckForm::Folded, w, s, flowing(slide));
                let xs = row_cells(&prims, &d, s);
                assert!(!xs.is_empty(), "CONTROL Folded {w} pt {s}x");
                assert!(
                    xs.iter().all(|x| x.fract() == 0.0),
                    "Folded {w} pt {s}x at {slide}: {xs:?}"
                );
                fractional += usize::from(((1.0 - slide) * step).fract() != 0.0);
            }
        }
    }
    assert!(
        fractional >= 10,
        "CONTROL: unrounded, {fractional} of these frames fall between pixels"
    );
}

/// `snap` with its CPU row's readings replaced: one a second for the minute before `at` (its
/// snapshot's moment), each `heat(t)` of capacity, `t` its second.
fn with_cpu_readings(mut snap: Snapshot, at: i64, heat: impl Fn(i64) -> f64) -> Snapshot {
    let view = &mut snap.resources[Resource::Cpu.index()];
    let cap = view.capacity.expect("the scene's CPU capacity");
    view.recent = (0..60_i64)
        .rev()
        .map(|back| {
            let t = at - back * 1000;
            aterm_vitals::Reading {
                at_ms: t,
                v: heat(t.div_euclid(1000)) * cap,
                over: false,
            }
        })
        .collect();
    snap.at_unix_ms = at;
    snap
}

/// A CPU second's heat that never repeats within a strip: 0.05 to 0.95 by a step of 0.3 a
/// second over 0.9, so every heat step shows and no shift under 30 s lines it up again.
fn varied(t: i64) -> f64 {
    0.05 + ((t * 7).rem_euclid(31) as f64 / 31.0) * 0.9
}

/// The owner's ask, "I want to see a shifting to the left animation" (2026-10-24), read outward
/// from now since the now-left design (§3.4), where he looks: the default window, Folded at about
/// 600 pt and focused. Each reading a second on slides the strip's fine band right by the time it
/// opened, a whole 1 s box (3 device px at 1x, 6 at 2x; the owner's blocks of 2026-10-26), as
/// every Folded width's band moves at least two pixels, in whole-pixel moves under half a box,
/// over the beat's frames; and on the raster a second carries a reading's pixels one fine box
/// right, measured as two screenshots are. The label over it still names what the strip spans
/// (`the_time_scale_marks_how_far_back_at_every_width`).
#[test]
fn the_default_window_is_seen_sliding_right() {
    use crate::stats_live::{lag_px, slide_frames, slide_phase};
    let m = deck_metrics();
    for w in (300..=2560).map(f64::from) {
        let p = plan::plan_folded(w, &m);
        if p.ground_only {
            continue;
        }
        for b in &p.blocks {
            assert_eq!(b.strip.period_s, plan::FOLD_SLOT_S, "Folded {w} pt");
            for s in [1.0, 2.0] {
                let step = fine_band(&b.strip, s).step_px(1_000);
                assert!(
                    step >= 2.0 * s,
                    "Folded {w} pt {s}x: a reading a second slides the band {step} px: {:?}",
                    b.strip
                );
            }
        }
    }

    let (form, w) = (DeckForm::Folded, 600.0);
    let d = strips(form, w)[0];
    assert_eq!(
        (d.strip.period_s, d.strip.span_s(), d.strip.pitch),
        (1, 360, 3.0),
        "the default window's multi-scale strip spans 6 minutes with fine real-time slots by now"
    );
    let band = fine_band_slots(&d.strip.spec.slots()).expect("a fine band");
    let at0 = OPEN + 300;
    let snap_at = |at: i64| with_cpu_readings(scene(Scene::Calm), at, varied);
    let slid = |phase: Option<f32>| phase.map_or(DeckLive::STILL, |p| flowing(f64::from(p)));
    for s in [1.0, 2.0] {
        let g = geom_at(w, s, form);
        let fine = fine_band(&d.strip, s);
        let (_, _, out) = deck_paint_live(&g, &scene_input(&snap_at(at0), form));
        assert_eq!(
            (out.slide, out.glide),
            (fine, FineBand::default()),
            "{s}x: a Folded card slides and never glides: {out:?}"
        );
        let step = fine.step_px(1_000);
        let frames = slide_frames(step);
        assert!(frames >= 2, "{s}x: {frames} frames");
        let rest = multi_scale_slot_rects(&d.strip, s, 0.0);
        let box_px = rest[band.start + 1].1 - rest[band.start].1;
        let oldest = band.end - 1;
        let next = snap_at(at0 + 1_000);
        // The band's oldest box lags its slot by the step before the reading's frame (the last
        // reading's band stood at rest, a step right of where the new one's slots put that time).
        let mut lags = vec![step as f32];
        for k in 0..frames {
            let phase = slide_phase(k, frames);
            let prims = paint(&next, form, w, s, slid(phase));
            let cells = row_cells(&prims, &d, s);
            assert!(!cells.is_empty(), "{s}x: {cells:?}");
            let lag = lag_px(step, phase.map_or(1.0, f64::from)) as f32;
            let want = multi_scale_slot_rects(&d.strip, s, lag);
            let expected: Vec<f32> = want.iter().map(|r| r.1).collect();
            let mut got = cells.clone();
            got.extend(row_hollow(&prims, &d, s));
            got.sort_by(f32::total_cmp);
            assert_eq!(got, expected, "{s}x: slide frame {k}");
            let moved = want
                .iter()
                .find(|r| r.0 == oldest)
                .map_or(0.0, |r| rest[oldest].1 - r.1);
            lags.push(moved);
        }
        assert_eq!(lags.last(), Some(&0.0), "{s}x: it comes in: {lags:?}");
        for m in lags.windows(2).map(|w| w[0] - w[1]) {
            assert!(
                m >= 1.0 && m < box_px / 2.0 && m.fract() == 0.0,
                "{s}x: a frame moved {m} px of a {box_px} px box: {lags:?}"
            );
        }
        assert_eq!(
            row_cells(&paint(&next, form, w, s, slid(None)), &d, s),
            row_cells(&paint(&snap_at(at0), form, w, s, slid(None)), &d, s),
            "{s}x: come in, a reading's strip stands in the slots the last one's did"
        );
        // Over the fine 1 s boxes next to now (`1..2s` and `2..3s`), a one-second pulse at `at0`
        // stands in `1..2s`, and a second later (`at0 + 1_000`) has moved one fine box right into
        // `2..3s` on the raster.
        let raster = |snap: &Snapshot| {
            let mut input = scene_input(snap, form);
            input.live = DeckLive::STILL;
            let (prims, _) = deck_paint(&g, &input);
            crate::tray_raster::rasterize_tray(&prims, g.w as u32, g.h as u32, 1.0, [0; 4]).0
        };
        let pulse_sec = at0.div_euclid(1000) - 2;
        let pulse_at = |at: i64| {
            with_cpu_readings(scene(Scene::Calm), at, |t| {
                if t == pulse_sec { 1.0 } else { 0.0 }
            })
        };
        let (a, b) = (raster(&pulse_at(at0)), raster(&pulse_at(at0 + 1_000)));
        let y = (px(d.top, s) + px(d.cell, s) / 2.0) as u32;
        let (s1_x, s2_x) = (rest[band.start].1, rest[band.start + 1].1);
        let px_at = |img: &[u8], x: f32| -> [u8; 4] {
            let cx = (x + 1.0) as usize;
            let i = (y as usize * g.w as usize + cx) * 4;
            img[i..i + 4].try_into().expect("rgba")
        };
        assert_eq!(
            px_at(&a, s1_x),
            px_at(&b, s2_x),
            "{s}x: a second later, the hot second moved right by {box_px} px"
        );
        assert_ne!(
            px_at(&a, s1_x),
            px_at(&a, s2_x),
            "CONTROL {s}x: hot and cold slots differ on the raster"
        );
    }
}

/// The box entering the fine band at its old end, as a reading's slide carries the band right,
/// shows the band's own oldest box (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §3.4), never
/// the coarser box beside it: with the CPU hot only in the band's oldest box's two seconds, the
/// entering box draws that box's ink while the coarse box after it draws cold.
#[test]
fn the_box_entering_the_band_shows_its_own_oldest_data() {
    use crate::stats_live::{lag_px, slide_frames, slide_phase};
    let (form, w, s) = (DeckForm::Folded, 600.0, 2.0);
    let d = strips(form, w)[0];
    let slots = d.strip.spec.slots();
    let band = fine_band_slots(&slots).expect("a fine band");
    let oldest = band.end - 1;
    let at = OPEN + 300;
    // The reading that falls in the band's oldest box (ages 9..10 s on MINI_1M): a box holds
    // the readings older than its newer edge, up to its older one.
    let (lo, hi) = (slots[oldest].age_lo_ms, slots[oldest].age_hi_ms);
    let snap = with_cpu_readings(scene(Scene::Calm), at, |t| {
        let age = at - (t * 1000 + at.rem_euclid(1000));
        if age > lo && age <= hi { 1.0 } else { 0.0 }
    });
    let step = fine_band(&d.strip, s).step_px(1_000);
    let frames = slide_frames(step);
    let phase = slide_phase(0, frames);
    let lag = lag_px(step, phase.map_or(1.0, f64::from)) as f32;
    let rects = multi_scale_slot_rects(&d.strip, s, lag);
    let mut olds = rects.iter().filter(|r| r.0 == oldest);
    let (_, kept) = (olds.next(), olds.next());
    let &(_, ex, ew, _) = kept.expect("a box enters at the band's old end");
    let &(_, cx, cw, _) = rects
        .iter()
        .find(|r| r.0 == band.end)
        .expect("the coarse box after the band");
    let prims = paint(
        &snap,
        form,
        w,
        s,
        phase.map_or(DeckLive::STILL, |p| flowing(f64::from(p))),
    );
    let (top, cell_h) = (px(d.top, s), px(d.cell, s));
    let fill_at = |x: f32, w: f32| {
        prims.iter().find_map(|p| match *p {
            DrawPrim::Panel {
                x: px0,
                y,
                w: pw,
                h,
                fill,
                ..
            } if (px0 - x).abs() < 0.01
                && (pw - w).abs() < 0.01
                && (y - top).abs() < 0.01
                && (h - cell_h).abs() < 0.01 =>
            {
                Some(fill)
            }
            _ => None,
        })
    };
    let inks = super::tests::dark_inks();
    let cold = ink(inks.heat[usize::from(ink::heat_step(0.0))], 0xFF);
    let &(_, ox, ow, _) = rects
        .iter()
        .find(|r| r.0 == oldest)
        .expect("the band's oldest box, displaced");
    let warm = fill_at(ox, ow).expect("the band's oldest box is filled");
    assert_ne!(warm, cold, "CONTROL: the band's oldest box is warm");
    assert_eq!(
        fill_at(ex, ew),
        Some(warm),
        "the entering box shows the band's oldest"
    );
    assert_eq!(
        fill_at(cx, cw),
        Some(cold),
        "CONTROL: the coarse box beside it is cold"
    );
}

/// A Folded multi-scale strip draws fine real-time readings next to now, at the left, and
/// aggregated history to their right: with CPU hot for the last 6 s and cold for the 54 s
/// before, the fine 2 s slots inside the last 6 s draw hot while every slot older than 6 s and
/// within the minute read draws cold (and the `now` box at the far left draws the reading now).
#[test]
fn a_folded_strip_draws_each_seconds_reading() {
    // A six minutes' strip, whose every slot the scene's readings cover.
    let (form, w) = (DeckForm::Folded, 550.0);
    let d = strips(form, w)[0];
    let at = OPEN + 300;
    let hist_cells_of = |snap: &Snapshot, s: f64| -> Vec<[u8; 4]> {
        let prims = paint(snap, form, w, s, DeckLive::STILL);
        let rects = multi_scale_slot_rects(&d.strip, s, 0.0);
        let top = px(d.top, s);
        let cell_h = px(d.cell, s);
        rects
            .into_iter()
            .filter(|r| !r.3)
            .filter_map(|(_, rx, rw, _)| {
                prims.iter().find_map(|p| match *p {
                    DrawPrim::Panel {
                        x, y, w, h, fill, ..
                    } if (x - rx).abs() < 0.01
                        && (w - rw).abs() < 0.01
                        && (y - top).abs() < 0.01
                        && (h - cell_h).abs() < 0.01 =>
                    {
                        Some(fill)
                    }
                    _ => None,
                })
            })
            .collect()
    };
    let painted = |hot_last: bool, s: f64| -> Vec<[u8; 4]> {
        let last6 = at.div_euclid(1000) - 6;
        let snap = with_cpu_readings(scene(Scene::Calm), at, |t| {
            if (t >= last6) == hot_last { 1.0 } else { 0.0 }
        });
        hist_cells_of(&snap, s)
    };
    let inks = super::tests::dark_inks();
    let (hot, cold) = (
        ink(inks.heat[usize::from(ink::heat_step(1.0))], 0xFF),
        ink(inks.heat[usize::from(ink::heat_step(0.0))], 0xFF),
    );
    assert_ne!(hot, cold, "CONTROL: the ramp's ends differ");
    let slots = d.strip.spec.slots();
    let hist_slots: Vec<_> = slots.iter().filter(|sl| !sl.is_live_now).collect();
    let split = hist_slots
        .iter()
        .position(|sl| sl.age_hi_ms > 6_000)
        .expect("history older than 6s");
    assert!(
        split >= 1,
        "CONTROL: fine 2s slots inside the last 6s come first"
    );
    // The slots the minute of readings covers.
    let read = hist_slots
        .iter()
        .take_while(|sl| sl.age_hi_ms <= 60_000)
        .count();
    assert!(
        read > split,
        "CONTROL: slots past the last 6 s within the minute"
    );
    for s in [1.0, 2.0] {
        let mut newest = painted(true, s);
        assert_eq!(newest.len(), hist_slots.len(), "{s}x: {newest:?}");
        newest.truncate(read);
        assert!(
            newest[..split].iter().all(|&f| f == hot),
            "{s}x: {newest:?}"
        );
        assert!(
            newest[split..].iter().all(|&f| f == cold),
            "{s}x: {newest:?}"
        );
        let mut oldest = painted(false, s);
        assert_eq!(oldest.len(), hist_slots.len(), "{s}x");
        oldest.truncate(read);
        assert!(
            oldest[..split].iter().all(|&f| f == cold),
            "{s}x: {oldest:?}"
        );
        assert!(
            oldest[split..].iter().all(|&f| f == hot),
            "{s}x: {oldest:?}"
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
        ((strip.x - LINE_DX) * s) as f32,
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
    assert!(
        line_under(&stale, at.0).is_none(),
        "no solid line under a stalled marker"
    );
    let dashed: Vec<[u8; 4]> = stale
        .iter()
        .filter_map(|p| match *p {
            DrawPrim::Dashed {
                x,
                w,
                on,
                off,
                color,
                ..
            } if w == 0.0 && x <= at.0 && at.0 <= x + 2.0 && on > 0.0 && off > 0.0 => Some(color),
            _ => None,
        })
        .collect();
    assert_eq!(
        dashed,
        vec![ink(inks.hollow, 0xFF)],
        "the line greys and breaks into dashes"
    );
    assert_ne!(
        inks.hollow, inks.words,
        "CONTROL: grey is not the words' ink"
    );

    let beat = |phase| DeckLive {
        flow: None,
        beat: Some(phase),
        blend: None,
        needle: None,
        fresh: true,
        steam: None,
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

/// The relative luminance of `c` (WCAG 2).
fn luminance(c: [u8; 4]) -> f64 {
    let ch = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v <= 0.039_28 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * ch(c[0]) + 0.7152 * ch(c[1]) + 0.0722 * ch(c[2])
}

fn contrast(a: [u8; 4], b: [u8; 4]) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The now marker's line as rastered: the contrast of its pixels with the ground beside it (left
/// of it, in the room before the % column, clear of the live box on its right; the best
/// and the median along its run), and how many breaks the run has.
fn line_as_drawn(rgba: &[u8], pw: u32, d: &Drawn, s: f64) -> (f64, f64, usize) {
    let lw = (0.75 * s).round().max(1.0);
    let cx = (d.strip.x - LINE_DX) * s;
    let x = (cx - lw / 2.0).round() as usize;
    let ground_x = x.saturating_sub(2);
    let (y0, y1) = (px(d.top, s) as usize, px(d.last, s) as usize);
    let at = |x: usize, y: usize| -> [u8; 4] {
        let i = (y * pw as usize + x) * 4;
        rgba.get(i..i + 4)
            .and_then(|p| <[u8; 4]>::try_from(p).ok())
            .unwrap_or_default()
    };
    let mut seen: Vec<f64> = (y0..y1)
        .map(|y| contrast(at(x, y), at(ground_x, y)))
        .collect();
    let breaks = seen
        .windows(2)
        .filter(|w| w[0] >= 1.5 && w[1] < 1.1)
        .count();
    seen.sort_by(f64::total_cmp);
    let best = seen.last().copied().unwrap_or(1.0);
    let median = seen.get(seen.len() / 2).copied().unwrap_or(1.0);
    (best, median, breaks)
}

/// Review of 2026-10-24: live and stalled must differ by more than a dot. In both themes, at
/// both scales and in both forms, the live line as rastered stands out from the ground more
/// than the stalled one (it was the other way round in the light theme), and the stalled line
/// is broken into dashes, so a frozen deck reads as frozen with motion off.
#[test]
fn the_live_line_outranks_the_stalled_one_in_both_themes() {
    let snap = scene(Scene::Calm);
    for (ground, inks) in [
        ("dark", super::tests::dark_inks()),
        ("light", super::paint_tests::light_inks()),
    ] {
        for (form, w) in [
            (DeckForm::Full, 1119.0),
            (DeckForm::Folded, 1119.0),
            (DeckForm::Folded, 600.0),
        ] {
            for s in [1.0, 2.0] {
                let g = geom_at(w, s, form);
                let raster = |live: DeckLive| {
                    let mut input = scene_input(&snap, form);
                    input.inks = inks;
                    input.live = live;
                    let (prims, _) = deck_paint(&g, &input);
                    crate::tray_raster::rasterize_tray(&prims, g.w as u32, g.h as u32, 1.0, [0; 4])
                };
                let (live, _, _) = raster(DeckLive::STILL);
                let (stale, pw, _) = raster(DeckLive::STALE);
                for d in strips(form, w) {
                    let at = format!("{ground} {form:?} {w} pt {s}x, strip at {}", d.strip.x);
                    let (live_best, live_mid, live_breaks) = line_as_drawn(&live, pw, &d, s);
                    let (stale_best, _, stale_breaks) = line_as_drawn(&stale, pw, &d, s);
                    assert!(live_mid >= 4.5, "{at}: the live line at {live_mid:.2}:1");
                    assert_eq!(live_breaks, 0, "{at}: the live line is whole");
                    assert!(
                        live_best >= 1.3 * stale_best,
                        "{at}: live {live_best:.2}:1, stalled {stale_best:.2}:1"
                    );
                    assert!(
                        stale_best >= 2.0,
                        "CONTROL {at}: the stalled line is drawn ({stale_best:.2}:1)"
                    );
                    assert!(
                        stale_breaks >= 3,
                        "{at}: the stalled line breaks {stale_breaks} times"
                    );
                }
            }
        }
    }
}

/// A time-scale tick is a mark, not a dot: at least two device px tall at 1× as at 2×, one px
/// wide, ending short of the strip's top and its over-max caps; the strip's left-end tick under
/// a label beside it too.
#[test]
fn a_tick_is_a_mark_at_both_scales() {
    let m = FaceMetrics::helvetica_neue();
    let snap = scene(Scene::Calm);
    let mut checked = 0;
    for (form, w) in [
        (DeckForm::Full, 1119.0),
        (DeckForm::Folded, 1119.0),
        (DeckForm::Folded, 600.0),
        (DeckForm::Full, 550.0),
    ] {
        for s in [1.0, 2.0] {
            let input = scene_input(&snap, form);
            let words = DeckWords::new(&input, &m);
            let laid = lay(&geom_at(w, s, form), &input, &words, &m, true);
            let head = laid.head.as_ref().expect("a header");
            let top = strips(form, w)
                .iter()
                .map(|d| px(d.top, s))
                .fold(f32::MAX, f32::min);
            let caps = if s >= 2.0 {
                ((1.5 * s).round() + (0.5 * s).round()) as f32
            } else {
                0.0
            };
            let xs: Vec<f64> = head
                .ruler
                .iter()
                .map(|t| t.x)
                .chain(head.time_tick)
                .collect();
            assert!(!xs.is_empty(), "{form:?} {w} pt {s}x: a scale");
            for x in xs {
                let at = format!("{form:?} {w} pt {s}x, tick at {x}");
                let want = (x * s) as f32;
                let mark = laid.prims.iter().find_map(|p| match *p {
                    DrawPrim::Panel { x, y, w, h, .. }
                        if w <= 1.01 && (x + w / 2.0 - want).abs() <= 1.0 && y < top =>
                    {
                        Some((y, w, h))
                    }
                    _ => None,
                });
                let (y, mw, h) = mark.unwrap_or_else(|| panic!("{at}: no mark"));
                assert!(h >= 2.0, "{at}: {mw}x{h} px");
                assert!(
                    y + h <= top - caps,
                    "{at}: ends at {} into the strip at {top}",
                    y + h
                );
                checked += 1;
            }
        }
    }
    assert!(checked >= 8, "CONTROL: {checked} ticks");
}

/// The owner's default window (80 columns of the 12 px font, about 600 pt) draws v2's Folded
/// deck: from there to the width where Folded's strips widen, with the face the app draws its
/// header in, the header carries a time scale (review of 2026-10-24: it carried none below
/// about 700 pt).
#[test]
fn the_default_window_carries_a_time_scale() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let m = deck_metrics();
    let snap = scene(Scene::Calm);
    let (mut drew, mut bare) = (0, Vec::new());
    for form in [DeckForm::Folded, DeckForm::Full] {
        let input = scene_input(&snap, form);
        let words = DeckWords::new(&input, &m);
        for w in (360..=1200).step_by(8).map(f64::from) {
            for s in [1.0, 2.0] {
                let laid = lay(&geom_at(w, s, form), &input, &words, &m, true);
                let Some(head) = laid.head.as_ref() else {
                    continue;
                };
                if !head.labels.iter().any(|l| l.label == plan::Label::Time) {
                    continue;
                }
                drew += 1;
                if head.ruler.is_empty() {
                    bare.push(format!("{form:?} {w} pt {s}x"));
                }
            }
        }
    }
    assert!(drew > 300, "CONTROL: {drew} headers drew the time label");
    assert!(bare.is_empty(), "no scale at {bare:?}");
}

/// Every frame the live edge asks for draws something new (reviews of 2026-10-24 and
/// 2026-10-26): with readings landing each second, each glide or slide frame between them moves
/// the card's pixels, at the slowest widths too and for a reading early in its second (a glide of
/// a whole second, whose frame period is rounded to whole milliseconds); once its run is out the
/// glide holds and asks none.
#[test]
fn every_glide_frame_moves_the_cards_pixels() {
    use crate::stats_live::{Flow, LiveFacts, Opened};
    use std::time::{Duration, Instant};
    let t0 = Instant::now();
    let mut asked = 0;
    for (form, w, s, into) in [
        (DeckForm::Folded, 600.0, 2.0, 1_234),
        (DeckForm::Folded, 1119.0, 1.0, 1_234),
        (DeckForm::Folded, 1119.0, 2.0, 1_234),
        (DeckForm::Full, 1119.0, 1.0, 1_234),
        (DeckForm::Full, 1119.0, 2.0, 1_234),
        (DeckForm::Full, 1119.0, 1.0, 120),
        (DeckForm::Full, 860.0, 1.0, 120),
        (DeckForm::Full, 600.0, 1.0, 120),
        (DeckForm::Full, 600.0, 2.0, 870),
    ] {
        let ms0 = OPEN + into;
        let wall = |t: Instant| ms0 + i64::try_from((t - t0).as_millis()).unwrap_or(0);
        let g = geom_at(w, s, form);
        let snap_at = |at_ms: i64| {
            let mut snap = scene(Scene::Calm);
            snap.at_unix_ms = at_ms;
            snap
        };
        let first = snap_at(ms0);
        let (_, _, out) = deck_paint_live(&g, &scene_input(&first, form));
        let folded = form == DeckForm::Folded;
        let none = FineBand::default();
        assert!(
            if folded {
                out.slide.box_px >= 3.0 && out.glide == none
            } else {
                out.glide.box_px >= 3.0 && out.slide == none
            },
            "CONTROL: a Folded card slides, a Full one glides: {out:?}"
        );
        let raster = |snap: &Snapshot, live: DeckLive| {
            let mut input = scene_input(snap, form);
            input.live = live;
            let (prims, _) = deck_paint(&g, &input);
            crate::tray_raster::rasterize_tray(&prims, g.w as u32, g.h as u32, 1.0, [0; 4]).0
        };
        for r in 0..8_u32 {
            let landed = t0 + Duration::from_secs(u64::from(r));
            let at_ms = wall(landed);
            let snap = snap_at(at_ms);
            // A reading a second, each opening its own second.
            let opened = Opened {
                at_ms,
                ms: 1_000,
                landed,
            };
            let flow = Flow::of((out.glide, out.slide), opened, snap.slot_grid);
            assert_ne!(flow, Flow::None, "CONTROL {form:?} {w} pt {s}x: it moves");
            let facts = LiveFacts {
                landed: Some(landed),
                stale_after: Duration::from_secs(3),
                motion: true,
                beat: false,
                tween: false,
                needle: None,
                drawn: None,
                flow,
                steam: false,
            };
            let mut now = landed;
            let mut was = raster(&snap, facts.live(now, wall(now)));
            while let Some(next) = facts.next_change(now, wall(now)) {
                if next >= landed + Duration::from_secs(1) {
                    break;
                }
                let drawn = raster(&snap, facts.live(next, wall(next)));
                assert_ne!(
                    drawn,
                    was,
                    "{form:?} {w} pt {s}x: reading {r}, a frame at {:?} drew nothing new",
                    next - landed
                );
                asked += 1;
                was = drawn;
                now = next;
            }
        }
    }
    assert!(asked >= 10, "CONTROL: {asked} glide frames asked");
}

/// A frame that moves only the live edge re-rasters only the card's rects its layers moved, and
/// what it gives is the whole raster of the new frame, byte for byte.
#[test]
fn a_live_frame_patches_only_what_moved_to_the_whole_rasters_bytes() {
    let mut snap = scene(Scene::Calm);
    snap.at_unix_ms = OPEN + 500;
    let still = DeckLive::STILL;
    let gliding = |ran| DeckLive {
        beat: Some(0.0),
        ..flowing(ran)
    };
    let sliding = |slide: Option<f64>| DeckLive {
        beat: Some(0.0),
        ..slide.map_or(DeckLive::STILL, flowing)
    };
    // Full's fine band glides its step over a second from the reading: a frame of the grid on,
    // it has moved.
    let cases = [
        (DeckForm::Full, 1424.0, 1.0, gliding(0.0), gliding(0.25)),
        (DeckForm::Full, 1119.0, 2.0, still, DeckLive::STALE),
        (
            DeckForm::Folded,
            1119.0,
            1.0,
            sliding(Some(0.5)),
            DeckLive {
                beat: Some(0.5),
                ..sliding(Some(0.5))
            },
        ),
        (DeckForm::Folded, 1119.0, 1.0, sliding(Some(0.25)), still),
        (
            DeckForm::Folded,
            600.0,
            2.0,
            sliding(Some(0.25)),
            sliding(Some(0.5)),
        ),
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
        if was.flow == now.flow {
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

/// `deg` degrees and `soak` as the live edge carries a needle in motion ([`DeckLive::needle`]).
fn needle_at(deg: f64, soak: f64) -> Option<(i32, i32)> {
    Some((
        (deg * crate::stats_needle::ANGLE_PER_DEG).round() as i32,
        (soak * crate::stats_needle::SOAK_PER_ONE).round() as i32,
    ))
}

/// A needle frame re-rasters only the dial's slot ([`LiveLayer::Gauge`]), its bloom included,
/// and the patch is the whole raster's bytes there: nothing outside the slot changes as the
/// needle sweeps, through the hot bloom (0.78 of capacity, 28.8°) and into the redline (75°), or
/// as it comes to rest on the reading.
#[test]
fn a_needle_frame_patches_only_the_dial_to_the_whole_rasters_bytes() {
    let mut snap = scene(Scene::Calm);
    snap.at_unix_ms = OPEN;
    let still = DeckLive::STILL;
    let with = |needle| DeckLive { needle, ..still };
    let cases = [
        (
            DeckForm::Full,
            1119.0,
            2.0,
            with(None),
            with(needle_at(20.0, 0.0)),
        ),
        (
            DeckForm::Full,
            1119.0,
            1.0,
            with(needle_at(20.0, 0.1)),
            with(needle_at(40.0, 0.1)),
        ),
        (
            DeckForm::Folded,
            1119.0,
            2.0,
            with(needle_at(60.0, 0.0)),
            with(needle_at(90.0, 0.9)),
        ),
        (
            DeckForm::Folded,
            600.0,
            1.0,
            with(needle_at(-100.0, 0.3)),
            with(needle_at(-98.5, 0.3)),
        ),
        (
            DeckForm::Folded,
            600.0,
            2.0,
            with(needle_at(80.0, 0.0)),
            still,
        ),
    ];
    for (form, w, s, was, now) in cases {
        let at = format!("{form:?} {w} pt {s}x, {:?} to {:?}", was.needle, now.needle);
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
        let (want, _, _) = whole(&after);
        let moved: Vec<Rect> = out
            .rects
            .iter()
            .filter(|(layer, _)| layer.moved(&was, &now))
            .map(|&(layer, r)| {
                assert_eq!(layer, LiveLayer::Gauge, "{at}: only the dial moves");
                r
            })
            .collect();
        assert_eq!(moved.len(), 1, "{at}: one dial, {moved:?}");
        let (x, y, rw, rh) = moved[0];
        assert!(
            out.needle_px_per_deg > 0.0,
            "{at}: the layout says how far its tip travels"
        );
        let inside = |px: u32, py: u32| {
            let (px, py) = (px as f32, py as f32);
            px >= x.floor() && px < (x + rw).ceil() && py >= y.floor() && py < (y + rh).ceil()
        };
        let mut changed = 0;
        for py in 0..ph {
            for px in 0..pw {
                let i = (py * pw + px) as usize * 4;
                if rgba[i..i + 4] != want[i..i + 4] {
                    assert!(
                        inside(px, py),
                        "{at}: ({px}, {py}) changed outside the dial"
                    );
                    changed += 1;
                }
            }
        }
        assert!(
            changed > 0,
            "CONTROL {at}: the needle moved the dial's pixels"
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
        assert!(
            patch_live(&mut card, &after, &out.rects, &was, &now),
            "{at}"
        );
        assert!(card.rgba == want, "{at}: the patch is not the whole raster");
        assert!(
            rw * rh < 0.25 * pw as f32 * ph as f32,
            "{at}: the dial's slot is {} px of {}",
            rw * rh,
            pw * ph
        );
    }
}

/// What a live frame costs on this machine, for the budget rows of the designs
/// (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §4.4): p50 and p95 CPU of a needle frame
/// (the dial one pixel further on its sweep: cool with no soak, cool under a soaked bezel's
/// bloom, cool with the lens open, and hot through the bloom into the redline), of a grid frame
/// (a reading's beat, slide or glide, and tween stepping through their frames) and, for scale,
/// of a new reading's whole card, each what the splice does (`App::splice_stats_deck`): the
/// layout over the kept words and header line, then the patch of what moved (the reading: words,
/// header and the whole raster), and with the lens open the fold of the patched card and the
/// lens's (`App::fold_stats_deck`). At 700, 1100 and 2238 device px wide at 2×, both forms, the
/// owner's busy scene and the worst case. It times; it asserts nothing about time. Run it
/// optimized and alone:
/// `targo --unverified test -p aterm-gui --profile opt-test --lib -- --ignored deck_frame_cost
/// --nocapture --test-threads 1` (`ATERM_DECK_COST=<file>` also writes the table as TSV).
#[test]
#[ignore = "measures CPU for the designs' budget rows: run it optimized and alone"]
fn deck_frame_cost() {
    const RUNS: usize = 400;
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let pct = |v: &mut Vec<f64>, q: f64| {
        v.sort_by(f64::total_cmp);
        v[((v.len() - 1) as f64 * q).round() as usize]
    };
    let mut tsv = String::from("scene\tform\tdevice_px\tframe\tp50_us\tp95_us\tpatched_share\n");
    for (sname, sc) in [("busy", Scene::Calm), ("worst", Scene::Costly)] {
        let mut snap = scene(sc);
        snap.at_unix_ms = OPEN;
        for form in [DeckForm::Folded, DeckForm::Full] {
            for (w, s) in [(350.0, 2.0), (550.0, 2.0), (1119.0, 2.0)] {
                let g = geom_at(w, s, form);
                let mut input = scene_input(&snap, form);
                let whole = |prims: &[DrawPrim]| {
                    crate::tray_raster::rasterize_tray(
                        prims,
                        g.w as u32,
                        g.h as u32,
                        1.0,
                        [0, 0, 0, 0],
                    )
                };
                let mut still = DeckStill::default();
                let (prims, _, out) = deck_paint_kept(&g, &input, &mut still);
                let (rgba, pw, ph) = whole(&prims);
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
                let px_per_deg = out.needle_px_per_deg;
                // Each needle frame one device pixel on from the last, sweeping from `lo` to `hi`
                // of capacity: cool (0.1 to 0.75, no bloom) or hot (0.8 to 1.5, the bloom lit and
                // into the redline), at `soak` (0.9 lights the bezel's bloom, from 0.81).
                let needle = |(lo, hi): (f32, f32), soak: f64, i: usize| {
                    let (a, b) = (
                        crate::stats_gauge::tip_deg(lo),
                        crate::stats_gauge::tip_deg(hi),
                    );
                    let step = (i as f64) % ((b - a) * px_per_deg).floor().max(1.0);
                    DeckLive {
                        needle: needle_at(a + step / px_per_deg, soak),
                        ..DeckLive::STILL
                    }
                };
                // The lens pinned on the CPU, hung under the deck in a 700 pt grid: while it is
                // open, the window folds each frame's patched card with it again.
                let lens = crate::stats_lens::lens_geom(
                    StatsBars::Deck,
                    0,
                    g.h,
                    g.pad,
                    g.h,
                    g.w - 2.0 * g.pad,
                    (700.0 * s) as f32,
                    g.scale,
                    g.font_px,
                )
                .map(|lg| {
                    let pinned = crate::stats_lens::Lens {
                        focus: crate::stats_paint::Instrument::Cpu,
                        bar: crate::stats_paint::Bar::Global,
                        how: crate::stats_lens::Opened::Pinned,
                    };
                    let laid =
                        crate::stats_lens::lay_lens(&lg, &input, &pinned, &deck_metrics(), true);
                    let (rgba, pw, ph) = crate::tray_raster::rasterize_tray(
                        &laid.prims,
                        lg.w as u32,
                        lg.h as u32,
                        1.0,
                        [0, 0, 0, 0],
                    );
                    crate::SettingsCard {
                        rgba,
                        pw,
                        ph,
                        dx: lg.dx,
                        dy: lg.dy,
                        fp: 2,
                        geom: 2,
                        raster_fingerprint_once: std::sync::OnceLock::new(),
                        scrim: None,
                    }
                });
                // A reading's grid frames in turn: its own, the two that beat and slide or
                // glide on, and the one that clears them.
                let grid = |i: usize| {
                    let k = (i % 4) as u32;
                    DeckLive {
                        beat: crate::stats_live::beat_phase(k),
                        blend: crate::stats_live::tween_phase(k),
                        ..crate::stats_live::slide_phase(k, 4)
                            .map_or(DeckLive::STILL, |phase| flowing(f64::from(phase)))
                    }
                };
                let pixels = f64::from(pw) * f64::from(ph);
                let mut run = |name: &str,
                               live: &dyn Fn(usize) -> DeckLive,
                               fold: Option<&crate::SettingsCard>| {
                    let mut was = live(0);
                    input.live = was;
                    let (prims, _, _) = deck_paint_kept(&g, &input, &mut still);
                    card.rgba = whole(&prims).0;
                    let (mut times, mut share) = (Vec::with_capacity(RUNS), 0.0);
                    for i in 1..=RUNS {
                        let now = live(i);
                        input.live = now;
                        let t = std::time::Instant::now();
                        let (prims, _, out) = deck_paint_kept(&g, &input, &mut still);
                        assert!(patch_live(&mut card, &prims, &out.rects, &was, &now));
                        if let Some(lens) = fold {
                            let folded = fold_cards(&[&card, lens], i as u64 | 1);
                            assert!(folded.is_some_and(|f| f.pw >= lens.pw));
                        }
                        times.push(t.elapsed().as_secs_f64() * 1e6);
                        share += out
                            .rects
                            .iter()
                            .filter(|(layer, _)| layer.moved(&was, &now))
                            .map(|(_, r)| f64::from(r.2 * r.3))
                            .sum::<f64>()
                            / pixels;
                        was = now;
                    }
                    format!(
                        "{sname}\t{form:?}\t{}\t{name}\t{:.1}\t{:.1}\t{:.3}\n",
                        g.w,
                        pct(&mut times, 0.5),
                        pct(&mut times, 0.95),
                        share / RUNS as f64
                    )
                };
                let mut rows = Vec::new();
                let (cool, hot) = ((0.1, 0.75), (0.8, 1.5));
                // Each row's sweep, soak and the lens it folds with (`None`: no lens fits).
                for (name, sweep, soak, fold) in [
                    ("needle", cool, 0.0, Some(None)),
                    ("needle_soaked", cool, 0.9, Some(None)),
                    ("needle_lens", cool, 0.0, lens.as_ref().map(Some)),
                    ("needle_hot", hot, 0.0, Some(None)),
                ] {
                    let missing =
                        |why: &str| format!("{sname}\t{form:?}\t{}\t{name}\t{why}\t\t\n", g.w);
                    rows.push(match fold {
                        _ if px_per_deg <= 0.0 => missing("no dial"),
                        None => missing("no lens"),
                        Some(fold) => run(name, &|i| needle(sweep, soak, i), fold),
                    });
                }
                rows.push(run("grid", &grid, None));
                // The same frames with the dial steaming, its clock 250 ms on each.
                let steam = |i: usize| DeckLive {
                    steam: Some(10_000 + 250 * i64::try_from(i).unwrap_or(0)),
                    ..grid(i)
                };
                rows.push(run("grid_steam", &steam, None));
                for row in rows {
                    eprint!("{row}");
                    tsv.push_str(&row);
                }
                // A new reading: its words and header read again and the card rastered whole.
                let mut times = Vec::with_capacity(RUNS / 4);
                input.live = DeckLive::STILL;
                for _ in 0..RUNS / 4 {
                    let t = std::time::Instant::now();
                    let mut fresh = DeckStill::default();
                    let (prims, _, _) = deck_paint_kept(&g, &input, &mut fresh);
                    card.rgba = whole(&prims).0;
                    times.push(t.elapsed().as_secs_f64() * 1e6);
                }
                let row = format!(
                    "{sname}\t{form:?}\t{}\treading\t{:.1}\t{:.1}\t1.000\n",
                    g.w,
                    pct(&mut times, 0.5),
                    pct(&mut times, 0.95)
                );
                eprint!("{row}");
                tsv.push_str(&row);
            }
        }
    }
    if let Some(path) = std::env::var_os("ATERM_DECK_COST") {
        std::fs::write(path, tsv).expect("the cost table writes");
    }
}

/// A beat's patch costs its narrow rects, not the card: of the text the whole raster blends, it
/// blends only the glyphs of the texts over its rects (`Canvas::texels_meet_canvas`), so at each
/// window checked a beat frame touches under a sixteenth of the card's glyphs. (The bound was 5%
/// while each row printed a column of figures past its strip; without them the Folded card at
/// 1524 pt holds 146 glyphs, of which the patch blends 8.)
#[test]
fn a_beat_patch_blends_only_the_glyphs_over_its_rects() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut snap = scene(Scene::Calm);
    snap.at_unix_ms = OPEN;
    let was = DeckLive {
        beat: Some(0.0),
        ..DeckLive::STILL
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
        // The glyphs of the texts whose line meets a rect, each glyph granted an em: more than
        // the patch may blend.
        let over: u64 = after
            .iter()
            .filter_map(|p| match p {
                DrawPrim::Text {
                    x, baseline, s, px, ..
                } => Some((*x, *baseline, s, *px)),
                _ => None,
            })
            .filter(|&(x, base, s, px)| {
                let (x1, y0, y1) = (x + px * s.chars().count() as f32, base - px, base + px);
                out.rects
                    .iter()
                    .any(|&(_, (rx, ry, rw, rh))| x < rx + rw && x1 > rx && y0 < ry + rh && y1 > ry)
            })
            .map(|(.., s, _)| s.chars().filter(|c| !c.is_whitespace()).count() as u64)
            .sum();
        assert!(
            patched <= over,
            "{at}: the beat's patch blends {patched} glyphs, the texts over its rects {over}"
        );
        assert!(
            patched * 16 < whole,
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
        beat: Some(0.0),
        blend: Some(0.5),
        needle: needle_at(100.0, 0.9),
        ..flowing(0.5)
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

/// The live edge's frames for a person to read: still, gliding half way through its run (a
/// Folded strip halfway through its slide), beating, and stalled, in both forms and grounds, at
/// 2× and 1×.
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
        ("glide_half", flowing(0.5)),
        (
            "beat",
            DeckLive {
                beat: crate::stats_live::beat_phase(0),
                blend: crate::stats_live::tween_phase(0),
                ..flowing(0.25)
            },
        ),
        ("stalled", DeckLive::STALE),
    ];
    // The mockups' widths, and the owner's default window (80 columns, about 600 pt).
    const WIDTHS: [(f64, f64); 5] = [
        (1119.0, 2.0),
        (550.0, 2.0),
        (1119.0, 1.0),
        (600.0, 2.0),
        (600.0, 1.0),
    ];
    let mut written = 0;
    for (name, live) in frames {
        for (w, s) in WIDTHS {
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
    assert_eq!(written, frames.len() * WIDTHS.len() * 2 * 2);

    // The middle of a needle episode (`stats_needle`): the scene's reading lands on a dial that
    // rested cool at 0.30 of capacity (−72°, no soak), so the tip swings up through the hot
    // bloom's 0.78 (28.8°) and the redline (75°) to the reading while the soak rises, each frame
    // as the window would draw it, and the last frame is the settled dial, the reading itself.
    const MOMENTS_MS: [u64; 9] = [0, 80, 120, 150, 200, 260, 300, 450, 800];
    let t0 = std::time::Instant::now();
    let from = (crate::stats_gauge::tip_deg(0.30), 0.0);
    let to = crate::stats_gauge::Gauge::of(&aterm_vitals::Strain::of(&snap))
        .target()
        .expect("the scene's dial");
    let mut needles = 0;
    for (w, s) in [(1119.0, 2.0), (1119.0, 1.0)] {
        for form in [DeckForm::Full, DeckForm::Folded] {
            let r = if form == DeckForm::Full {
                crate::stats_gauge::R_FACE
            } else {
                crate::stats_plan::FOLD_GAUGE_R
            };
            let view = crate::stats_needle::View {
                px_per_deg: crate::stats_gauge::tip_px_per_deg(r, s),
                interval: std::time::Duration::from_micros(16_667),
            };
            let n = crate::stats_needle::Needle::retarget(
                Some(crate::stats_needle::Needle::at_rest(t0, from)),
                t0,
                Some(to),
                view.px_per_deg,
            )
            .expect("a needle");
            for ms in MOMENTS_MS {
                let at = t0 + std::time::Duration::from_millis(ms);
                let needle = n.drawn(view, n.frame_at(view, at));
                for (ground, inks) in [
                    ("dark", super::tests::dark_inks()),
                    ("light", super::paint_tests::light_inks()),
                ] {
                    let mut input = scene_input(&snap, form);
                    input.inks = inks;
                    input.live = DeckLive {
                        needle,
                        ..DeckLive::STILL
                    };
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
                    let fword = if form == DeckForm::Full {
                        "full"
                    } else {
                        "folded"
                    };
                    let file =
                        dir.join(format!("needle_{ms:03}ms_{w}pt_{s}x_{fword}_{ground}.png"));
                    std::fs::write(&file, png).expect("the PNG writes");
                    needles += 1;
                }
            }
        }
    }
    assert_eq!(needles, MOMENTS_MS.len() * 2 * 2 * 2);

    // Rich showcase frames exercising 1h multi-scale history, Apple M5 Max + 128 GiB + 8 TB
    // hardware identity, live Network state/RTT/per-scope sockets, Sleep gap, Crash line, and
    // hot cores & load dial.
    let mut show = scene(Scene::Calm);
    show.at_unix_ms = OPEN;
    show.facts.cpu_brand = Some("Apple M5 Max".into());
    if let Some(v) = show.volume.as_mut() {
        v.total = 8_000_000_000_000;
    }
    show.resources[Resource::Disk.index()].capacity = Some(8_000_000_000_000.0);
    for (ri, base_heat) in [0.92_f64, 0.24, 0.48, 0.62, 0.31, 0.14]
        .into_iter()
        .enumerate()
    {
        let rv = &mut show.resources[ri];
        let cap = rv.capacity.unwrap_or(100.0).max(1.0);
        rv.heat_now = Some(base_heat as f32);
        rv.now = Some(base_heat * cap);
        rv.ring = (0..360)
            .map(|i| {
                let wave = ((i as f64 * 0.19 + ri as f64).sin() * 0.18 + base_heat * 0.72)
                    .clamp(0.04, 0.98);
                let v = wave * cap;
                aterm_vitals::Cell {
                    mean: Some(v),
                    max: Some(v),
                    over: ri == 0 && i >= 352,
                }
            })
            .collect::<Vec<_>>()
            .into();
        rv.ring_k = OPEN.div_euclid(10_000);
        rv.recent = (0..60_i64)
            .rev()
            .map(|back| {
                let wave = ((back as f64 * 0.35 + ri as f64).cos() * 0.14 + base_heat * 0.85)
                    .clamp(0.05, 0.99);
                aterm_vitals::Reading {
                    at_ms: OPEN - back * 1_000,
                    v: wave * cap,
                    over: ri == 0 && back <= 8,
                }
            })
            .collect();
    }
    show.facts.link = Some(aterm_vitals::LinkSample {
        name: "en0".into(),
        kind: aterm_vitals::LinkKind::Wifi,
        up: true,
        counts: true,
        rate_bps: Some(1_200_000_000),
        ..aterm_vitals::LinkSample::default()
    });
    show.net = Some(aterm_vitals::NetView {
        links: Vec::new(),
        rates: Some(aterm_vitals::LinkRates {
            rx_bps: 14_200_000.0,
            tx_bps: 1_800_000.0,
            rx_pps: 9_800.0,
            tx_pps: 2_100.0,
            rx_err_ps: 0.0,
            tx_err_ps: 0.0,
            rx_drop_ps: 0.0,
            tx_drop_ps: None,
        }),
        tcp: Some(aterm_vitals::TcpCounts {
            established: 24,
            listen: 6,
            time_wait: 3,
            ..aterm_vitals::TcpCounts::default()
        }),
        srtt_ms: Some(12.4),
        state: aterm_vitals::NetState::Online,
        aterm_sockets: Some(18),
        tab_sockets: vec![(2, 6)],
        ..aterm_vitals::NetView::default()
    });
    show.facts.link = Some(aterm_vitals::LinkSample {
        name: "en0".into(),
        kind: aterm_vitals::LinkKind::Wifi,
        up: true,
        counts: true,
        rate_bps: Some(866_000_000),
        ..aterm_vitals::LinkSample::default()
    });
    show.resources[Resource::Network.index()].aterm = aterm_vitals::ScopeFig {
        now: Some(18.0),
        missing: None,
        ..aterm_vitals::ScopeFig::default()
    };
    let tab_net_fig = aterm_vitals::ScopeFig {
        now: Some(6.0),
        missing: None,
        ..aterm_vitals::ScopeFig::default()
    };
    show.resources[Resource::Network.index()].tab = tab_net_fig.clone();
    show.resources[Resource::Network.index()].tabs = vec![(2, tab_net_fig)];
    show.time_gaps = vec![aterm_vitals::TimeGap {
        start_ms: OPEN - 300_000,
        end_ms: OPEN - 180_000,
        kind: aterm_vitals::GapKind::Sleep,
    }];
    show.strip_events = vec![aterm_vitals::StripEvent {
        at_unix_ms: OPEN - 25_000,
        kind: aterm_vitals::StripEventKind::Crash,
        label: "SIGSEGV".into(),
    }];
    for (w, form, ground, inks) in [
        (1119.0, DeckForm::Folded, "dark", super::tests::dark_inks()),
        (1119.0, DeckForm::Full, "dark", super::tests::dark_inks()),
        (
            1119.0,
            DeckForm::Folded,
            "light",
            super::paint_tests::light_inks(),
        ),
        (1424.0, DeckForm::Folded, "dark", super::tests::dark_inks()),
    ] {
        let mut input = scene_input(&show, form);
        input.inks = inks;
        input.live = DeckLive {
            beat: crate::stats_live::beat_phase(0),
            ..flowing(0.25)
        };
        let g = geom_at(w, 2.0, form);
        let (prims, _) = deck_paint(&g, &input);
        let (rgba, pw, ph) =
            crate::tray_raster::rasterize_tray(&prims, g.w as u32, g.h as u32, 1.0, [0, 0, 0, 0]);
        let png = crate::app_introspect::encode_rgba8_png(&rgba, pw, ph).expect("showcase encodes");
        let fword = if form == DeckForm::Full {
            "full"
        } else {
            "folded"
        };
        std::fs::write(
            dir.join(format!("showcase_{w}pt_2x_{fword}_{ground}.png")),
            png,
        )
        .expect("showcase PNG writes");
    }
}

/// The live now box steps through the heat ramp as a blend carries a reading in, settles in the
/// ramp's ink like every other cell, dims when the deck goes stale, and a blend frame patches it
/// to the whole raster's bytes. It draws no halo, fresh, stale or under Differentiate Without
/// Color (the owner, 2026-10-26: "let the other dials be more factual").
#[test]
fn the_live_now_box_steps_through_the_heat_ramp_with_no_halo_and_patches_byte_for_byte() {
    let mut prev = scene(Scene::Calm);
    let mut curr = scene(Scene::Calm);
    prev.resources[Resource::Cpu.index()].heat_now = Some(0.10);
    curr.resources[Resource::Cpu.index()].heat_now = Some(0.95);
    if let Some(cpu) = curr.cpu.as_mut() {
        for c in &mut cpu.cores {
            c.load.level = 0.92;
        }
    }
    for (ground, inks) in [
        ("dark", super::tests::dark_inks()),
        ("light", super::paint_tests::light_inks()),
    ] {
        for form in [DeckForm::Folded, DeckForm::Full] {
            let w = 1119.0;
            let s = 2.0;
            let g = geom_at(w, s, form);
            let d = strips(form, w)[0];
            let (_, live_x, live_w, _) = multi_scale_slot_rects(&d.strip, s, 0.0)
                .into_iter()
                .find(|r| r.3)
                .expect("live now slot");
            let top = px(d.top, s);
            let cell_h = px(d.cell, s);

            let paint_blend = |live: DeckLive, dwc: bool| {
                let mut input = scene_input(&curr, form);
                input.prev_snap = Some(&prev);
                input.inks = inks;
                input.live = live;
                input.dwc = dwc;
                deck_paint_live(&g, &input)
            };

            let (prims_low, _, _) = paint_blend(
                DeckLive {
                    blend: Some(0.20),
                    ..DeckLive::STILL
                },
                false,
            );
            let (prims_mid, _, _) = paint_blend(
                DeckLive {
                    blend: Some(0.60),
                    ..DeckLive::STILL
                },
                false,
            );
            let (prims_done, _, out_done) = paint_blend(DeckLive::STILL, false);
            let (prims_stale, _, _) = paint_blend(DeckLive::STALE, false);
            let (prims_dwc, _, _) = paint_blend(DeckLive::STILL, true);

            let live_fill = |prims: &[DrawPrim]| -> [u8; 4] {
                prims
                    .iter()
                    .find_map(|p| match *p {
                        DrawPrim::Panel {
                            x, y, w, h, fill, ..
                        } if (x - live_x).abs() < 0.01
                            && (w - live_w).abs() < 0.01
                            && y >= top - 0.01
                            && (y + h - (top + cell_h)).abs() < 0.01 =>
                        {
                            Some(fill)
                        }
                        _ => None,
                    })
                    .expect("live now panel")
            };
            let glow_strokes = |prims: &[DrawPrim]| -> usize {
                prims
                    .iter()
                    .filter(|&p| {
                        matches!(
                            *p,
                            DrawPrim::Stroke { x, y, color, .. }
                                if (x - (live_x - 0.5)).abs() < 1.5
                                    && (y - (top - 0.5)).abs() < 1.5
                                    && color[3] < 0xFF
                        )
                    })
                    .count()
            };
            let c_low = live_fill(&prims_low);
            let c_mid = live_fill(&prims_mid);
            let c_done = live_fill(&prims_done);
            let c_stale = live_fill(&prims_stale);
            assert!(
                c_low != c_mid && c_mid != c_done,
                "{ground} {form:?}: the live now box steps across the blend: \
                 {c_low:?} -> {c_mid:?} -> {c_done:?}"
            );
            assert_eq!(
                c_done,
                rgba(inks.heat[usize::from(ink::heat_step(0.95))], 0xFF),
                "{ground} {form:?}: the settled live now box is the heat ramp's step"
            );
            assert_ne!(
                c_stale, c_done,
                "{ground} {form:?}: stale deck dims the live now box"
            );
            for (prims, said) in [
                (&prims_done, "fresh"),
                (&prims_stale, "stale"),
                (&prims_dwc, "dwc"),
            ] {
                assert_eq!(
                    glow_strokes(prims),
                    0,
                    "{ground} {form:?}: a {said} live now box is a plain box, no halo \
                     (owner ruling 2026-10-26)"
                );
            }

            // Verify `patch_live` across `blend` transitions matches a fresh whole raster byte for byte.
            let was = DeckLive {
                blend: Some(0.20),
                ..DeckLive::STILL
            };
            let now = DeckLive::STILL;
            let whole = |prims: &[DrawPrim]| {
                crate::tray_raster::rasterize_tray(prims, g.w as u32, g.h as u32, 1.0, [0, 0, 0, 0])
            };
            let (rgba0, pw, ph) = whole(&prims_low);
            let (want, _, _) = whole(&prims_done);
            let mut card = crate::SettingsCard {
                rgba: rgba0,
                pw,
                ph,
                dx: 0,
                dy: 0,
                fp: 1,
                geom: 1,
                raster_fingerprint_once: std::sync::OnceLock::new(),
                scrim: None,
            };
            assert!(patch_live(
                &mut card,
                &prims_done,
                &out_done.rects,
                &was,
                &now
            ));
            assert!(
                card.rgba == want,
                "{ground} {form:?}: blend patch_live must equal fresh rasterize_tray byte-for-byte"
            );
        }
    }
}

/// The bars under the % figures tween between readings as the bars' column does, in a live
/// rect of their own (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §5): a blend frame patches
/// them to the whole raster's bytes, at 1× and 2×, in both forms.
#[test]
fn a_blend_patches_the_bars_under_the_percent_to_the_whole_rasters_bytes() {
    let mut prev = scene(Scene::Calm);
    let curr = scene(Scene::Calm);
    // The machine's fills were whole a reading ago, so the blend moves each bar's end.
    for res in [
        Resource::Cpu,
        Resource::Gpu,
        Resource::Memory,
        Resource::Disk,
    ] {
        prev.resources[res.index()].heat_now = Some(1.0);
    }
    let was = DeckLive {
        blend: Some(0.20),
        ..DeckLive::STILL
    };
    let now = DeckLive::STILL;
    for (form, w, s) in [
        (DeckForm::Folded, 360.0, 2.0),
        (DeckForm::Folded, 450.0, 1.0),
        (DeckForm::Full, 210.0, 2.0),
        (DeckForm::Full, 210.0, 1.0),
    ] {
        let at = format!("{form:?} {w} pt {s}x");
        let g = geom_at(w, s, form);
        let paint = |live: DeckLive| {
            let mut input = scene_input(&curr, form);
            input.prev_snap = Some(&prev);
            input.live = live;
            deck_paint_live(&g, &input)
        };
        let (before, _, _) = paint(was);
        let (after, _, out) = paint(now);
        let under = match form {
            DeckForm::Full => plan::plan_full(w, &deck_metrics()).bar_under(),
            DeckForm::Folded => plan::plan_folded(w, &deck_metrics()).blocks[0].bar_under(),
        }
        .unwrap_or_else(|| panic!("CONTROL {at}: bars under the %"));
        let x = (under.x * s + 0.5).floor() as f32;
        let bars = |prims: &[DrawPrim]| -> Vec<(Rect, [u8; 4])> {
            prims
                .iter()
                .filter_map(|p| match *p {
                    DrawPrim::Panel {
                        x: px0,
                        y,
                        w,
                        h,
                        fill,
                        ..
                    } if (px0 - x).abs() < 0.01 => Some(((px0, y, w, h), fill)),
                    _ => None,
                })
                .collect()
        };
        assert_ne!(
            bars(&before),
            bars(&after),
            "CONTROL {at}: the bars under the % move"
        );
        let whole = |prims: &[DrawPrim]| {
            crate::tray_raster::rasterize_tray(prims, g.w as u32, g.h as u32, 1.0, [0, 0, 0, 0])
        };
        let (rgba, pw, ph) = whole(&before);
        let (want, _, _) = whole(&after);
        assert_ne!(rgba, want, "CONTROL {at}: the bars move");
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
        assert!(
            patch_live(&mut card, &after, &out.rects, &was, &now),
            "{at}"
        );
        assert!(card.rgba == want, "{at}: the patch is not the whole raster");
    }
}

/// The now box at each strip's live end draws exactly the ink the strip's own cells draw for the
/// same load, the original heat ramp's (979d8c3559; the owner, 2026-10-26: "go back to the
/// orignal color scheme", and "there is a "now" box with green that is out of the color
/// scheme"): at every load, on every built-in theme, under a green accent as under a blue one,
/// with Increase Contrast and without (`docs/DESIGN-stats-deck-owner-2026-10-26.md` §2). The
/// readings behind the newest history box are set to the same load, so the box beside the now
/// box is the strip's cell for it. CONTROL: loads a heat step apart draw different fills, and
/// the two contrasts differ somewhere.
#[test]
fn the_now_box_draws_the_strip_cells_ink_at_every_load_on_every_theme() {
    let loads = [0.0, 0.03, 0.12, 0.3, 0.5, 0.7, 0.88, 0.97, 1.0];
    let (w, s) = (1248.0, 2.0);
    let (mut drawn, mut stepped, mut contrasted) = (0, 0, 0);
    for form in [DeckForm::Folded, DeckForm::Full] {
        let g = geom_at(w, s, form);
        let d = strips(form, w)[0];
        let rects = multi_scale_slot_rects(&d.strip, s, 0.0);
        let (top, cell_h) = (px(d.top, s), px(d.cell, s));
        let fill_at = |prims: &[DrawPrim], (bx, bw): (f32, f32)| {
            prims.iter().find_map(|p| match *p {
                DrawPrim::Panel {
                    x, y, w, h, fill, ..
                } if (x - bx).abs() < 0.01
                    && (w - bw).abs() < 0.01
                    && y >= top - 0.01
                    && (y + h - (top + cell_h)).abs() < 0.01 =>
                {
                    Some(fill)
                }
                _ => None,
            })
        };
        // The now box's fill and the newest history cell's, every CPU reading at `v`.
        let fills = |inks: VitalsInks, contrast: bool, v: f32| -> ([u8; 4], [u8; 4]) {
            let mut snap = scene(Scene::Calm);
            let cpu = &mut snap.resources[Resource::Cpu.index()];
            cpu.heat_now = Some(v);
            let cap = cpu.capacity.expect("the CPU's cores");
            let readings: Vec<aterm_vitals::Reading> = cpu
                .recent
                .iter()
                .map(|r| aterm_vitals::Reading {
                    v: f64::from(v) * cap,
                    over: false,
                    ..*r
                })
                .collect();
            cpu.recent = readings.into();
            let mut input = scene_input(&snap, form);
            input.inks = inks;
            input.contrast = contrast;
            let (prims, _) = deck_paint(&g, &input);
            let live = rects.iter().find(|r| r.3).expect("a live box");
            let live = fill_at(&prims, (live.1, live.2)).expect("the now box's fill");
            let cell = rects
                .iter()
                .filter(|r| !r.3)
                .find_map(|r| fill_at(&prims, (r.1, r.2)))
                .expect("a history cell's fill");
            (live, cell)
        };
        for name in aterm_types::scheme::builtin_names() {
            let (ground, words, _) = super::paint_tests::theme_band(name);
            let green = ink::vitals_inks(ground, words, [0x50, 0xFA, 0x7B]);
            let blue = ink::vitals_inks(ground, words, [0x58, 0xA6, 0xFF]);
            for contrast in [false, true] {
                let mut by_step: Vec<(u8, [u8; 4])> = Vec::new();
                for v in loads {
                    let at = format!("{name} {form:?} at {v}, contrast {contrast}");
                    let (live, cell) = fills(green, contrast, v);
                    assert_eq!(live, cell, "{at}: the now box draws the strip cell's ink");
                    assert_eq!(live, rgba(green.cell_at(v, contrast), 0xFF), "{at}");
                    assert_eq!(
                        fills(blue, contrast, v),
                        (live, cell),
                        "{at}: either accent"
                    );
                    if contrast {
                        contrasted += usize::from(live != fills(green, false, v).0);
                    }
                    by_step.push((ink::heat_step(v), live));
                    drawn += 1;
                }
                for (a, fa) in &by_step {
                    for (b, fb) in &by_step {
                        if a != b && fa != fb {
                            stepped += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(drawn > 300, "CONTROL: {drawn} now boxes");
    assert!(
        stepped > drawn,
        "CONTROL: loads a step apart draw different fills"
    );
    assert!(
        contrasted > 0,
        "CONTROL: Increase Contrast draws its own ramp"
    );
}
