// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The deck painter's live edge (`docs/DESIGN-stats-deck-live-2026-10-24.md`): the now marker at
//! each strip's right end, the strips' glide, the grey of a stalled deck, and the region patch
//! that draws a live frame. The clock and the schedule are `stats_live_tests.rs`.

use super::paint_tests::{AT, Scene, scene, scene_input};
use super::*;
use crate::stats_live::{BEAT_REACH, DOT_R, DOT_RISE, LINE_DX, glide_step};
use aterm_vitals::SlotGrid;

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
            slide: None,
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

/// Every live frame draws every cell on a whole device pixel (review of 2026-10-25): a Full
/// strip's glide when the reading landed between two of the glide's steps (the clock behind it
/// counts as the reading's moment) and at any instant after, and a Folded strip's slide at every
/// phase its frames draw. Unrounded, those shifts fall between pixels (the control), and the cell
/// edges blur.
#[test]
fn every_live_frame_draws_its_cells_on_whole_pixels() {
    use crate::stats_live::open_frac;
    let mut fractional = 0;
    let at = OPEN + 3_457;
    let mut snap = scene(Scene::Calm);
    snap.at_unix_ms = at;
    for w in [1424.0, 1119.0, 800.0, 550.0] {
        let d = strips(DeckForm::Full, w)[0];
        for s in [1.0, 2.0] {
            let pitch = px(d.strip.pitch, s);
            for glide_ms in [OPEN + 3_000, at, OPEN + 4_321, OPEN + 7_777, OPEN + 12_345] {
                let live = DeckLive {
                    glide_ms: Some(glide_ms),
                    slide: None,
                    beat: None,
                    fresh: true,
                };
                let prims = paint(&snap, DeckForm::Full, w, s, live);
                let xs = row_cells(&prims, &d, s);
                assert!(!xs.is_empty(), "CONTROL Full {w} pt {s}x");
                assert!(
                    xs.iter().all(|x| x.fract() == 0.0),
                    "Full {w} pt {s}x at {glide_ms}: {xs:?}"
                );
                let f = open_frac(at, glide_ms, d.strip.period_s);
                fractional += usize::from(((1.0 - f) as f32 * pitch).fract() != 0.0);
            }
        }
    }
    for w in [600.0, 800.0, 1119.0] {
        let d = strips(DeckForm::Folded, w)[0];
        for s in [1.0, 2.0] {
            let pitch = px(d.strip.pitch, s);
            for slide in [0.25, 1.0 / 3.0, 0.5, 2.0 / 3.0, 0.75] {
                let live = DeckLive {
                    slide: Some(slide),
                    ..DeckLive::STILL
                };
                let prims = paint(&snap, DeckForm::Folded, w, s, live);
                let xs = row_cells(&prims, &d, s);
                assert!(!xs.is_empty(), "CONTROL Folded {w} pt {s}x");
                assert!(
                    xs.iter().all(|x| x.fract() == 0.0),
                    "Folded {w} pt {s}x at {slide}: {xs:?}"
                );
                fractional += usize::from(((1.0 - slide) * pitch).fract() != 0.0);
            }
        }
    }
    assert!(
        fractional >= 10,
        "CONTROL: unrounded, {fractional} of these frames fall between pixels"
    );
}

/// The shift, `-max..=max` device px, that best lines `b`'s pixel row `y` up with `a`'s over
/// `x0..x1` (`b[x]` against `a[x + shift]`, so a positive shift is content gone left), and the
/// mean channel difference left there and with no shift at all.
fn best_shift(
    a: &[u8],
    b: &[u8],
    pw: u32,
    y: u32,
    (x0, x1): (u32, u32),
    max: i32,
) -> (i32, f64, f64) {
    let at = |img: &[u8], x: i64| -> [u8; 4] {
        let i = (y as usize * pw as usize + x as usize) * 4;
        img.get(i..i + 4)
            .and_then(|p| <[u8; 4]>::try_from(p).ok())
            .unwrap_or_default()
    };
    let cost = |d: i32| -> f64 {
        let (lo, hi) = (
            i64::from(x0) + i64::from(max),
            i64::from(x1) - i64::from(max),
        );
        let mut sum = 0.0;
        for x in lo..hi {
            let (p, q) = (at(b, x), at(a, x + i64::from(d)));
            sum += p
                .iter()
                .zip(q)
                .map(|(&u, v)| f64::from(u.abs_diff(v)))
                .sum::<f64>();
        }
        sum / (hi - lo).max(1) as f64
    };
    let best = (-max..=max)
        .min_by(|&p, &q| cost(p).total_cmp(&cost(q)))
        .unwrap_or(0);
    (best, cost(best), cost(0))
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

/// The owner's ask, "I want to see a shifting to the left animation" (2026-10-24), where he
/// looks: the default window, Folded at about 600 pt and focused. Each cell is a reading's
/// second, so each reading slides the strip one pitch left (3 pt: 3 device px at 1x, 6 at 2x), as
/// every Folded width's do (3 to 5 pt), in whole-pixel moves under half a pitch, over the beat's
/// frames; and on the raster two readings carry the strip's pixels two pitches left, measured as
/// two screenshots are: by the shift that best lines the later frame's strip up with the
/// earlier's. The label over it still names what the strip spans
/// (`the_time_scale_marks_how_far_back_at_every_width`).
#[test]
fn the_default_window_is_seen_sliding_left() {
    use crate::stats_live::{slide_frames, slide_phase};
    let m = deck_metrics();
    for w in (300..=2560).map(f64::from) {
        let p = plan::plan_folded(w, &m);
        if p.ground_only {
            continue;
        }
        for b in p.blocks {
            assert_eq!(b.strip.period_s, plan::FOLD_SLOT_S, "Folded {w} pt");
            assert!(
                b.strip.pitch >= 3.0 && b.strip.pt_per_s() >= 3.0,
                "Folded {w} pt slides {} pt a reading: {:?}",
                b.strip.pitch,
                b.strip
            );
        }
    }
    // NEGATIVE CONTROL: a Full strip keeps its ten minutes, and at 1x glides under a pixel a
    // second (Full is drawn only where it is chosen).
    let full = plan::plan_full(1119.0, &m).strip;
    assert_eq!(full.span_s(), 600);
    assert!(full.pt_per_s() < 1.0, "{full:?}");

    let (form, w) = (DeckForm::Folded, 600.0);
    let d = strips(form, w)[0];
    assert_eq!(
        (d.strip.period_s, d.strip.span_s(), d.strip.pitch),
        (1, 20, 3.0),
        "the default window's strip spans 20 s, a reading's second on each 3 pt cell"
    );
    let at0 = OPEN + 300;
    let snap_at = |at: i64| with_cpu_readings(scene(Scene::Calm), at, varied);
    let slid = |phase: Option<f32>| DeckLive {
        slide: phase,
        ..DeckLive::STILL
    };
    for s in [1.0, 2.0] {
        let g = geom_at(w, s, form);
        let pitch = px(d.strip.pitch, s);
        let (_, _, out) = deck_paint_live(&g, &scene_input(&snap_at(at0), form));
        assert_eq!(
            (out.slide_pitch_px, out.glide_px_per_s),
            (f64::from(pitch), 0.0),
            "{s}x: a Folded card slides and never glides: {out:?}"
        );
        // The prims through one reading's slide: the first row's cells stand a shift right of
        // their slots, which runs from a whole pitch (the reading before's strip, read in the new
        // one's slots) down to none, each frame a whole pixel or more and under half a pitch.
        let frames = slide_frames(out.slide_pitch_px);
        assert!(frames >= 3, "{s}x: {frames} frames");
        let x0 = px(d.strip.x, s);
        let shift_of = |cells: &[f32]| -> f32 {
            let shifts: Vec<f32> = cells.iter().map(|x| (x - x0).rem_euclid(pitch)).collect();
            assert!(
                shifts.windows(2).all(|w| (w[0] - w[1]).abs() < 0.01),
                "{s}x: the cells moved as one: {cells:?}"
            );
            shifts.first().copied().unwrap_or_default()
        };
        let next = snap_at(at0 + 1_000);
        let mut shifts = vec![pitch];
        for k in 0..frames {
            let cells = row_cells(
                &paint(&next, form, w, s, slid(slide_phase(k, frames))),
                &d,
                s,
            );
            assert!(cells.len() >= d.strip.cols as usize, "{s}x: {cells:?}");
            shifts.push(shift_of(&cells));
        }
        assert_eq!(shifts.last(), Some(&0.0), "{s}x: it comes in: {shifts:?}");
        for m in shifts.windows(2).map(|w| w[0] - w[1]) {
            assert!(
                m >= 1.0 && m < pitch / 2.0 && m.fract() == 0.0,
                "{s}x: a frame moved {m} px: {shifts:?}"
            );
        }
        assert_eq!(
            row_cells(&paint(&next, form, w, s, slid(None)), &d, s),
            row_cells(&paint(&snap_at(at0), form, w, s, slid(None)), &d, s),
            "{s}x: come in, a reading's strip stands in the slots the last one's did"
        );
        // The raster, read as two screenshots are: two readings on, the strip's pixels stand
        // two pitches left.
        let raster = |snap: &Snapshot| {
            let mut input = scene_input(snap, form);
            input.live = DeckLive::STILL;
            let (prims, _) = deck_paint(&g, &input);
            crate::tray_raster::rasterize_tray(&prims, g.w as u32, g.h as u32, 1.0, [0; 4]).0
        };
        let (a, b) = (raster(&snap_at(at0)), raster(&snap_at(at0 + 2_000)));
        let y = (px(d.top, s) + px(d.cell, s) / 2.0) as u32;
        let span = (px(d.strip.x, s) as u32, px(d.strip.end(), s) as u32);
        let max = (3.0 * pitch) as i32;
        let (shift, left, unmoved) = best_shift(&a, &b, g.w as u32, y, span, max);
        assert_eq!(
            shift as f32,
            2.0 * pitch,
            "{s}x: two readings moved the strip {shift} px"
        );
        assert!(left < 1.0, "{s}x: {left} left after lining up");
        assert!(
            unmoved > 10.0,
            "CONTROL {s}x: the frames differ ({unmoved})"
        );
    }
}

/// A Folded strip draws each second's reading, the newest at its right end: with the CPU hot for
/// its last 8 seconds and cold for the minute before, the default window's 20 cells are 12 cold
/// then 8 hot, and the other way round 12 hot then 8 cold.
#[test]
fn a_folded_strip_draws_each_seconds_reading() {
    let (form, w) = (DeckForm::Folded, 600.0);
    let d = strips(form, w)[0];
    let at = OPEN + 300;
    let cells_of = |snap: &Snapshot, s: f64| -> Vec<[u8; 4]> {
        let prims = paint(snap, form, w, s, DeckLive::STILL);
        let xs = row_cells(&prims, &d, s);
        let mut cells: Vec<(f32, [u8; 4])> = prims
            .iter()
            .filter_map(|p| match *p {
                DrawPrim::Panel {
                    x, y, w, h, fill, ..
                } if xs.contains(&x)
                    && (w - px(d.strip.cell, s)).abs() < 0.01
                    && (y + h - (px(d.top, s) + px(d.cell, s))).abs() < 0.01 =>
                {
                    Some((x, fill))
                }
                _ => None,
            })
            .collect();
        cells.sort_by(|a, b| a.0.total_cmp(&b.0));
        cells.into_iter().map(|(_, fill)| fill).collect()
    };
    let painted = |hot_last: bool, s: f64| -> Vec<[u8; 4]> {
        let last8 = at.div_euclid(1000) - 8;
        let snap = with_cpu_readings(scene(Scene::Calm), at, |t| {
            if (t > last8) == hot_last { 1.0 } else { 0.0 }
        });
        cells_of(&snap, s)
    };
    let inks = super::tests::dark_inks();
    let (hot, cold) = (
        ink(inks.heat[usize::from(ink::heat_step(1.0))], 0xFF),
        ink(inks.heat[usize::from(ink::heat_step(0.0))], 0xFF),
    );
    assert_ne!(hot, cold, "CONTROL: the ramp's ends differ");
    for s in [1.0, 2.0] {
        let newest = painted(true, s);
        assert_eq!(newest.len(), d.strip.cols as usize, "{s}x: {newest:?}");
        assert!(newest[..12].iter().all(|&f| f == cold), "{s}x: {newest:?}");
        assert!(newest[12..].iter().all(|&f| f == hot), "{s}x: {newest:?}");
        let oldest = painted(false, s);
        assert_eq!(oldest.len(), d.strip.cols as usize, "{s}x");
        assert!(oldest[..12].iter().all(|&f| f == hot), "{s}x: {oldest:?}");
        assert!(oldest[12..].iter().all(|&f| f == cold), "{s}x: {oldest:?}");
    }
    // Readings a second apart that straddle the wall clock's second by 30 ms either way, hot
    // and cold in turn: on the snapshot's grid (its edges half a second in, where the model puts
    // them for such readings) each stands in its own slot and the strip alternates as they do;
    // on the wall clock's own seconds two share a slot and the next is spoken for by a third.
    let straddling = |grid: SlotGrid| {
        let mut snap = scene(Scene::Calm);
        let view = &mut snap.resources[Resource::Cpu.index()];
        let cap = view.capacity.expect("the scene's CPU capacity");
        view.recent = (0..60_i64)
            .map(|k| aterm_vitals::Reading {
                at_ms: OPEN - 59_000 + k * 1000 + if k % 2 == 0 { -30 } else { 30 },
                v: if k % 2 == 0 { cap } else { 0.0 },
                over: false,
            })
            .collect();
        snap.at_unix_ms = OPEN + 30;
        snap.slot_grid = grid;
        snap
    };
    let grid = SlotGrid {
        origin_ms: 500,
        ..SlotGrid::SECONDS
    };
    for s in [1.0, 2.0] {
        let cells = cells_of(&straddling(grid), s);
        assert_eq!(cells.len(), d.strip.cols as usize, "{s}x: {cells:?}");
        assert_eq!(cells.last(), Some(&cold), "{s}x: the newest reading's");
        assert!(
            cells.windows(2).all(|p| p[0] != p[1]) && cells.iter().all(|c| [hot, cold].contains(c)),
            "{s}x: a reading a cell: {cells:?}"
        );
        let seconds = cells_of(&straddling(SlotGrid::SECONDS), s);
        assert!(
            !seconds.windows(2).all(|p| p[0] != p[1]),
            "CONTROL {s}x: on the wall clock's seconds the readings pair up: {seconds:?}"
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
        glide_ms: None,
        slide: None,
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

/// The now marker's line as rastered: the contrast of its pixels with the ground beside it
/// (the best and the median along its run), and how many breaks the run has.
fn line_as_drawn(rgba: &[u8], pw: u32, d: &Drawn, s: f64) -> (f64, f64, usize) {
    let lw = (0.75 * s).round().max(1.0);
    let cx = (d.strip.end() + LINE_DX) * s;
    let x = (cx - lw / 2.0).round() as usize;
    let ground_x = x + lw as usize + 1;
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

/// Every frame the live edge asks for draws something new (review of 2026-10-24): with
/// readings landing each second, each glide frame between them moves the card's pixels, at the
/// slowest widths too; past the open column's end the glide holds and asks none.
#[test]
fn every_glide_frame_moves_the_cards_pixels() {
    use crate::stats_live::{Flow, LiveFacts, glide_step, open_start_ms, slide_frames};
    use std::time::{Duration, Instant};
    let t0 = Instant::now();
    let ms0 = OPEN + 1_234;
    let wall = |t: Instant| ms0 + i64::try_from((t - t0).as_millis()).unwrap_or(0);
    let mut asked = 0;
    for (form, w, s) in [
        (DeckForm::Folded, 600.0, 2.0),
        (DeckForm::Folded, 1119.0, 1.0),
        (DeckForm::Folded, 1119.0, 2.0),
        (DeckForm::Full, 1119.0, 1.0),
        (DeckForm::Full, 1119.0, 2.0),
    ] {
        let g = geom_at(w, s, form);
        let snap_at = |at_ms: i64| {
            let mut snap = scene(Scene::Calm);
            snap.at_unix_ms = at_ms;
            snap
        };
        let first = snap_at(ms0);
        let (_, _, out) = deck_paint_live(&g, &scene_input(&first, form));
        let folded = form == DeckForm::Folded;
        assert!(
            if folded {
                out.slide_pitch_px >= 3.0 && out.glide_px_per_s == 0.0
            } else {
                out.glide_px_per_s > 0.0 && out.glide_period_s > 0
            },
            "CONTROL: a Folded card slides, a Full one glides and its strips share one period \
             (else nothing holds): {out:?}"
        );
        let raster = |snap: &Snapshot, live: DeckLive| {
            let mut input = scene_input(snap, form);
            input.live = live;
            let (prims, _) = deck_paint(&g, &input);
            crate::tray_raster::rasterize_tray(&prims, g.w as u32, g.h as u32, 1.0, [0; 4]).0
        };
        let period = out.glide_period_s;
        for r in 0..8_u32 {
            let landed = t0 + Duration::from_secs(u64::from(r));
            let at_ms = wall(landed);
            let snap = snap_at(at_ms);
            // A reading a second, each opening its own second of a Folded strip.
            let flow = if folded {
                Flow::Slide {
                    frames: slide_frames(out.slide_pitch_px),
                }
            } else {
                Flow::Glide {
                    step: glide_step(out.glide_px_per_s),
                    end_ms: Some(open_start_ms(at_ms, period) + i64::from(period) * 1000),
                }
            };
            let facts = LiveFacts {
                landed: Some(landed),
                stale_after: Duration::from_secs(3),
                motion: true,
                beat: false,
                flow,
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
    snap.at_unix_ms = OPEN;
    let still = DeckLive::STILL;
    let gliding = |ms| DeckLive {
        glide_ms: Some(ms),
        slide: None,
        beat: Some(0.0),
        fresh: true,
    };
    let sliding = |slide| DeckLive {
        slide,
        beat: Some(0.0),
        ..DeckLive::STILL
    };
    // Full's 5 pt pitch over 10 s glides half a pixel a second at 1x: its glide steps every 2 s
    // ([`glide_step`]), a whole pixel each.
    let cases = [
        (
            DeckForm::Full,
            1424.0,
            1.0,
            gliding(OPEN + 2_000),
            gliding(OPEN + 2_000 + glide_step(0.5).as_millis() as i64),
        ),
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
        if was.glide_ms == now.glide_ms && was.slide == now.slide {
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
        slide: None,
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
        slide: Some(0.5),
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

/// The live edge's frames for a person to read: still, gliding half a period (a Folded strip
/// halfway through its slide), beating, and stalled, in both forms and grounds, at 2× and 1×.
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
                slide: Some(0.5),
                beat: None,
                fresh: true,
            },
        ),
        (
            "beat",
            DeckLive {
                glide_ms: Some(OPEN + 1_000),
                slide: Some(0.25),
                beat: crate::stats_live::beat_phase(0),
                fresh: true,
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
}
