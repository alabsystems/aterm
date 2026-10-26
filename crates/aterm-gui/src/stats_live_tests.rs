// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The live edge's clock, its frames and the time scale's ticks
//! (`docs/DESIGN-stats-deck-live-2026-10-24.md`). The painter's half is
//! `stats_live_paint_tests.rs`.

use super::*;
use crate::deck_face::FaceMetrics;
use crate::stats_plan::{
    self as plan, DeckForm, HeaderPlan, HeaderText, Label, MARKER_ROOM, RulerTick, Strip,
};

const EPS: f64 = 1e-9;

/// 10:42:10 on the mockups' day, unix ms: a 10 s column opens.
const OPEN: i64 = 1_792_000_800_000 + 42 * 60_000 + 10_000;

#[test]
fn the_open_column_runs_with_the_wall_clock() {
    assert_eq!(open_start_ms(OPEN + 7_000, 10), OPEN);
    assert_eq!(open_start_ms(OPEN, 10), OPEN);
    // A reading on the column's first instant: nothing of it has run, then half, then all.
    assert!(open_frac(OPEN, OPEN, 10).abs() < EPS);
    assert!((open_frac(OPEN, OPEN + 5_000, 10) - 0.5).abs() < EPS);
    assert!((open_frac(OPEN, OPEN + 10_000, 10) - 1.0).abs() < EPS);
    // Past its end with no reading to open the next: the strip waits full, it does not run on.
    assert!((open_frac(OPEN, OPEN + 25_000, 10) - 1.0).abs() < EPS);
    // A reading 7 s into its column already holds 0.7 of it; a clock behind the reading counts
    // as the reading's, so the strips never glide backward.
    assert!((open_frac(OPEN + 7_000, OPEN + 7_000, 10) - 0.7).abs() < EPS);
    assert!((open_frac(OPEN + 7_000, OPEN, 10) - 0.7).abs() < EPS);
    // The other periods align to the wall clock too: 10:42:10 is 10 s into a 20 s column.
    assert!((open_frac(OPEN, OPEN, 20) - 0.5).abs() < EPS);
    assert!((open_frac(OPEN, OPEN + 5_000, 30) - 0.5).abs() < EPS);
}

#[test]
fn columns_slide_right_of_their_slots_by_the_part_not_yet_run() {
    // Five 3-wide cells on a 4 pitch from x = 10: the strip ends at 29, which is now.
    let (x0, pitch, cell, n) = (10.0, 4.0, 3.0, 5);
    // Run out: every column in its slot, the one before the oldest outside.
    assert_eq!(
        column_xs(x0, pitch, cell, n, 1.0),
        vec![
            None,
            Some(10.0),
            Some(14.0),
            Some(18.0),
            Some(22.0),
            Some(26.0)
        ]
    );
    // Half run: half a pitch right; the open column shows its first half up to now, and the
    // one before the oldest slides in at the left.
    assert_eq!(
        column_xs(x0, pitch, cell, n, 0.5),
        vec![
            Some(8.0),
            Some(12.0),
            Some(16.0),
            Some(20.0),
            Some(24.0),
            Some(28.0)
        ]
    );
    // Just opened: a whole pitch right, so the open column is not on the strip yet and the
    // newest closed one stands in the last slot.
    assert_eq!(
        column_xs(x0, pitch, cell, n, 0.0),
        vec![
            Some(10.0),
            Some(14.0),
            Some(18.0),
            Some(22.0),
            Some(26.0),
            None
        ]
    );
    // Out-of-range glides stand in the nearest end.
    assert_eq!(
        column_xs(x0, pitch, cell, n, 7.0),
        column_xs(x0, pitch, cell, n, 1.0)
    );
    assert_eq!(
        column_xs(x0, pitch, cell, n, -1.0),
        column_xs(x0, pitch, cell, n, 0.0)
    );
}

#[test]
fn the_gridlines_stand_at_wall_clock_minutes() {
    // At 10:42:10 the open 10 s column is the 11th of n = 60 + 1; 10:42:00 opened one column
    // earlier and 10:41:00 seven earlier.
    let minutes = minute_columns(OPEN, 10, 60);
    assert!(minutes.contains(&59), "{minutes:?}");
    assert!(minutes.contains(&53), "{minutes:?}");
    assert!(minutes.windows(2).all(|w| w[1] - w[0] == 6), "{minutes:?}");
    assert_eq!(minutes.len(), 10, "{minutes:?}");
    // A column opening on the minute is itself a minute.
    assert_eq!(minute_columns(OPEN - 10_000, 10, 60).last(), Some(&60));
    // 30 s columns: every other one.
    assert!(
        minute_columns(OPEN - 10_000, 30, 20)
            .iter()
            .all(|k| k.is_multiple_of(2))
    );
}

#[test]
fn a_beat_widens_then_fades_to_nothing() {
    let (r0, a0) = beat_ring(0.0);
    assert!(r0.abs() < EPS);
    assert!((a0 - BEAT_ALPHA).abs() < 1e-6);
    let (r1, a1) = beat_ring(1.0);
    assert!((r1 - BEAT_REACH).abs() < EPS);
    assert!(a1.abs() < 1e-6);
    let mut last = beat_ring(0.0);
    for i in 1..=20 {
        let now = beat_ring(i as f32 / 20.0);
        assert!(
            now.0 >= last.0 && now.1 <= last.1,
            "{i}: {last:?} then {now:?}"
        );
        last = now;
    }
    assert_eq!(beat_ring(-1.0), beat_ring(0.0));
    assert_eq!(beat_ring(2.0), beat_ring(1.0));
}

#[test]
fn a_reading_speaks_for_three_of_the_slower_cadence() {
    let s = Duration::from_secs;
    assert_eq!(stale_after(s(1), None), s(3));
    assert_eq!(stale_after(s(5), None), s(15));
    // Coming into focus does not grey the marker before the first quick reading.
    assert_eq!(stale_after(s(1), Some(s(5))), s(15));
    assert_eq!(stale_after(s(5), Some(s(1))), s(15));
    // The lens's half second still waits the floor.
    assert_eq!(
        stale_after(Duration::from_millis(500), Some(Duration::from_millis(500))),
        STALE_FLOOR
    );
}

#[test]
fn the_glide_steps_a_whole_pixel_within_its_bounds() {
    // A Full strip at 1119 pt 2x, 2 px a second: a whole pixel every 500 ms, every other frame
    // of the grid; at 1x, 1 px a second: a pixel a second.
    assert_eq!(glide_step(2.0), Duration::from_millis(500));
    assert_eq!(glide_step(1.0), Duration::from_secs(1));
    assert_eq!(glide_step(4.0), FRAME);
    // The slowest strip drawn (Full slim at 1x: 20 cells of 4 pt over ten minutes) still steps
    // a whole pixel, never a fraction of one.
    let slim = 20.0 * 4.0 / 600.0;
    assert!(
        (glide_step(slim).as_secs_f64() * slim - 1.0).abs() < 1e-3,
        "{:?}",
        glide_step(slim)
    );
    // Fast strips are held to the grid's four frames a second, slow ones to one each 10 s.
    assert_eq!(glide_step(100.0), GLIDE_STEP_MIN);
    assert_eq!(GLIDE_STEP_MIN, FRAME);
    assert_eq!(glide_step(0.01), GLIDE_STEP_MAX);
    // No strip on the card: the least wait, never a zero or a panic.
    for px in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert_eq!(glide_step(px), GLIDE_STEP_MIN, "{px}");
    }
}

fn facts(landed: Instant, motion: bool, beat: bool) -> LiveFacts {
    LiveFacts {
        landed: Some(landed),
        stale_after: Duration::from_secs(3),
        motion,
        beat,
        flow: Flow::Glide {
            step: Duration::from_millis(250),
            end_ms: None,
        },
    }
}

/// `t` on the wall clock when `t0` is `ms0`.
fn wall(t0: Instant, ms0: i64, t: Instant) -> i64 {
    ms0 + i64::try_from(t.saturating_duration_since(t0).as_millis()).unwrap_or(i64::MAX)
}

#[test]
fn a_fresh_reading_beats_once_and_the_strips_glide() {
    let t0 = Instant::now();
    let ms0 = OPEN + 1_234;
    let f = facts(t0, true, true);
    let at = f.live(t0, ms0);
    assert_eq!(
        at.glide_ms,
        Some(OPEN + 1_000),
        "quantized to the glide's step"
    );
    assert_eq!(at.beat, beat_phase(0));
    assert!(at.fresh);
    // Inside a frame of the grid nothing moves: the edge stands as at the frame's moment.
    let ms = Duration::from_millis;
    assert_eq!(f.live(t0 + ms(130), ms0 + 130), at);
    // Each frame of the grid beats wider and fainter, and the glide moves with it.
    let one = f.live(t0 + FRAME, ms0 + 250);
    assert_eq!(one.beat, beat_phase(1));
    assert_eq!(one.glide_ms, Some(OPEN + 1_250));
    assert!(beat_phase(1) > beat_phase(0) && beat_phase(2) > beat_phase(1));
    // The beat rings for its frames and ends; the strips glide on.
    assert_eq!(FRAME * BEAT_FRAMES, ms(750));
    let after = f.live(t0 + FRAME * BEAT_FRAMES, ms0 + 750);
    assert_eq!(after.beat, None);
    assert!(after.fresh && after.glide_ms == Some(OPEN + 1_750));
    // The next frame is the grid's next.
    assert_eq!(f.next_change(t0, ms0), Some(t0 + FRAME));
    assert_eq!(f.next_change(t0 + ms(10), ms0 + 10), Some(t0 + FRAME));
    // Without the beat (serious mode, a card over the deck) the glide alone, on the grid.
    let quiet = facts(t0, true, false);
    assert_eq!(quiet.live(t0, ms0).beat, None);
    assert_eq!(quiet.next_change(t0, ms0 - 134), Some(t0 + FRAME));
    // Every frame the schedule names lies on the grid (or is the grey) and changes what the
    // deck draws.
    let mut now = t0;
    let mut frames = 0;
    while let Some(next) = f.next_change(now, wall(t0, ms0, now)) {
        let on_grid = (next - t0).as_nanos().is_multiple_of(FRAME.as_nanos());
        assert!(on_grid || next == t0 + f.stale_after, "{:?}", next - t0);
        assert_ne!(
            f.live(now, wall(t0, ms0, now)),
            f.live(next, wall(t0, ms0, next)),
            "a frame at {:?} moved nothing",
            next - t0
        );
        now = next;
        frames += 1;
        assert!(frames < 100, "the schedule never ends");
    }
    assert_eq!(f.live(now, wall(t0, ms0, now)), DeckLive::STALE);
}

/// Once the open column has run out, every column stands in its slot until a reading opens the
/// next: the glide holds and asks no frame that would draw the same strips.
#[test]
fn the_glide_holds_at_the_open_columns_end() {
    let t0 = Instant::now();
    let ms0 = OPEN + 9_400;
    let end = OPEN + 10_000;
    let f = LiveFacts {
        flow: Flow::Glide {
            step: FRAME,
            end_ms: Some(end),
        },
        ..facts(t0, true, false)
    };
    let ms = Duration::from_millis;
    assert_eq!(f.live(t0, ms0).glide_ms, Some(OPEN + 9_250));
    assert_eq!(f.live(t0 + ms(500), ms0 + 500).glide_ms, Some(OPEN + 9_750));
    assert_eq!(f.live(t0 + ms(750), ms0 + 750).glide_ms, Some(end));
    assert_eq!(f.live(t0 + ms(1_000), ms0 + 1_000).glide_ms, Some(end));
    assert!(
        (open_frac(OPEN, end, 10) - 1.0).abs() < EPS,
        "CONTROL: run out"
    );
    // Three frames to the end, then only the grey.
    assert_eq!(f.next_change(t0, ms0), Some(t0 + FRAME));
    assert_eq!(
        f.next_change(t0 + FRAME * 2, ms0 + 500),
        Some(t0 + FRAME * 3)
    );
    assert_eq!(
        f.next_change(t0 + FRAME * 3, ms0 + 750),
        Some(t0 + f.stale_after),
        "past the column's end nothing glides"
    );
    // Unbounded, the same clock would glide on.
    let on = LiveFacts {
        flow: Flow::Glide {
            step: FRAME,
            end_ms: None,
        },
        ..f
    };
    assert_eq!(
        on.next_change(t0 + FRAME * 3, ms0 + 750),
        Some(t0 + FRAME * 4)
    );
}

/// A reading that opens a new second slides a Folded strip one pitch left over the frames of
/// the grid after it, the beat's own: the reading's frame brings the newest slot a quarter in,
/// and 750 ms on it has come in and nothing moves until the next reading. A reading that opens
/// no new second (the lens's half-second cadence) moves nothing.
#[test]
fn a_reading_that_opens_a_second_slides_the_strip_in_on_the_beats_frames() {
    let t0 = Instant::now();
    let ms0 = OPEN + 1_234;
    let slide = |frames, beat| LiveFacts {
        flow: Flow::Slide { frames },
        ..facts(t0, true, beat)
    };
    let f = slide(slide_frames(6.0), true);
    let at = |k: u32| f.live(t0 + FRAME * k, ms0 + 250 * i64::from(k));
    assert_eq!(
        [0, 1, 2, 3, 4].map(|k| at(k).slide),
        [Some(0.25), Some(0.5), Some(0.75), None, None]
    );
    assert!(
        (0..3).all(|k| at(k).beat == beat_phase(k)),
        "on the beat's frames"
    );
    assert!(
        (0..5).all(|k| at(k).glide_ms.is_none()),
        "a Folded strip never glides with the clock"
    );
    // Its frames are the beat's: four, then only the grey.
    let mut now = t0;
    let mut frames = Vec::new();
    while let Some(next) = f.next_change(now, wall(t0, ms0, now)) {
        frames.push(next - t0);
        now = next;
    }
    assert_eq!(
        frames,
        [FRAME, FRAME * 2, FRAME * 3, f.stale_after],
        "the slide's frames, then the grey"
    );
    // Without the beat (serious mode, a lens over the deck) the slide still asks its frames.
    let quiet = slide(slide_frames(6.0), false);
    assert_eq!(quiet.next_change(t0, ms0), Some(t0 + FRAME));
    assert_eq!(
        quiet.next_change(t0 + FRAME * 3, ms0 + 750),
        Some(t0 + quiet.stale_after)
    );
    // A reading in the same second as the last: nothing slides, nothing is asked but the grey.
    let none = slide(0, false);
    assert!((0..5).all(|k| none.live(t0 + FRAME * k, ms0).slide.is_none()));
    assert_eq!(none.next_change(t0, ms0), Some(t0 + none.stale_after));
    // Without motion the strip stands in its slots.
    let still = LiveFacts { motion: false, ..f };
    assert_eq!(still.live(t0, ms0), DeckLive::STILL);
}

/// Each frame of a slide moves the strip at least a whole device pixel and under half a pitch,
/// so it reads as a step left even over a strip of one heat step, where a move of half a pitch
/// or more reads either way; and it takes at most the grid's four frames.
#[test]
fn every_frame_of_a_slide_moves_a_pixel_and_under_half_a_pitch() {
    for pitch in 1..=24_u32 {
        let p = f64::from(pitch);
        let frames = slide_frames(p);
        assert!(
            (1..=SLIDE_FRAMES).contains(&frames) && frames <= pitch,
            "{pitch} px: {frames}"
        );
        // The shift, device px, before the reading (the last reading's strip, one pitch right
        // of the new one's slots) and at each of the slide's frames, as `column_xs` rounds it.
        let mut shifts = vec![p];
        shifts.extend((0..frames).map(|k| {
            let f = slide_phase(k, frames).map_or(1.0, f64::from);
            ((1.0 - f) * p).round()
        }));
        assert_eq!(shifts.last(), Some(&0.0), "{pitch} px: it comes in");
        for w in shifts.windows(2) {
            let moved = w[0] - w[1];
            assert!(
                moved >= 1.0,
                "{pitch} px: a frame moved {moved}: {shifts:?}"
            );
            if pitch >= 3 {
                assert!(
                    moved < p / 2.0,
                    "{pitch} px: a frame moved {moved}: {shifts:?}"
                );
            }
        }
    }
    // The column positions the painter draws are the same whole pixels.
    for f in [0.25, 0.5, 0.75] {
        let xs = column_xs(10.0, 6.0, 4.0, 5, f);
        assert!(xs.iter().flatten().all(|x| x.fract() == 0.0), "{f}: {xs:?}");
    }
    for p in [f64::NAN, 0.0, -3.0, 0.5] {
        assert_eq!(slide_frames(p), 1, "{p}");
    }
}

#[test]
fn a_reading_opens_a_second_once() {
    let secs = SlotGrid::SECONDS;
    assert!(
        !opened_slot(None, OPEN, secs),
        "the first reading slides nothing"
    );
    assert!(!opened_slot(Some(OPEN + 100), OPEN + 900, secs));
    assert!(opened_slot(Some(OPEN + 900), OPEN + 1_100, secs));
    assert!(opened_slot(Some(OPEN - 5_000), OPEN, secs));
    assert!(
        !opened_slot(Some(OPEN + 2_000), OPEN + 1_000, secs),
        "a clock set back"
    );
    // On the snapshot's grid, its slots starting half a second in: readings at 900 and 1100 ms
    // share one, 400 and 600 do not.
    let half = SlotGrid {
        origin_ms: 500,
        ..secs
    };
    assert!(!opened_slot(Some(OPEN + 900), OPEN + 1_100, half));
    assert!(opened_slot(Some(OPEN + 400), OPEN + 600, half));
    // The strip's slot is the grid's.
    assert_eq!(
        i64::from(crate::stats_plan::FOLD_SLOT_S) * 1000,
        aterm_vitals::ring::SLOT_MS
    );
}

/// How the strips move in [`frames_over`]: a Full glide of `px/s` on `period` s columns, or a
/// Folded slide of a pitch of `px`.
#[derive(Clone, Copy, Debug)]
enum Moves {
    Glide(f64, u32),
    Slide(f64),
}

/// Frames the live edge asks of one window over `secs` seconds of readings every `cadence`,
/// its strips moving as `moves` says, counting each reading's own frame, and the frames that
/// would have drawn nothing new.
fn frames_over(cadence: Duration, moves: Moves, beat: bool, secs: u32) -> (usize, Vec<String>) {
    let t0 = Instant::now();
    let ms0 = OPEN + 3_456;
    let readings =
        u32::try_from(Duration::from_secs(u64::from(secs)).as_nanos() / cadence.as_nanos())
            .unwrap_or(0);
    let (mut frames, mut idle) = (0, Vec::new());
    for r in 0..readings {
        let landed = t0 + cadence * r;
        let at_ms = wall(t0, ms0, landed);
        let prev_ms = r.checked_sub(1).map(|p| wall(t0, ms0, t0 + cadence * p));
        let flow = match moves {
            Moves::Glide(px_per_s, period_s) => Flow::Glide {
                step: glide_step(px_per_s),
                end_ms: Some(open_start_ms(at_ms, period_s) + i64::from(period_s) * 1000),
            },
            Moves::Slide(pitch_px) => Flow::Slide {
                frames: if opened_slot(prev_ms, at_ms, SlotGrid::SECONDS) {
                    slide_frames(pitch_px)
                } else {
                    0
                },
            },
        };
        let f = LiveFacts {
            landed: Some(landed),
            stale_after: stale_after(cadence, None),
            motion: true,
            beat,
            flow,
        };
        frames += 1;
        let until = landed + cadence;
        let mut now = landed;
        while let Some(next) = f.next_change(now, wall(t0, ms0, now)) {
            if next >= until {
                break;
            }
            if f.live(now, wall(t0, ms0, now)) == f.live(next, wall(t0, ms0, next)) {
                idle.push(format!("reading {r}: {:?}", next - landed));
            }
            frames += 1;
            now = next;
        }
    }
    (frames, idle)
}

/// The cost bound (§6): a focused, live deck asks at most four frames a second, whatever its
/// strips' speed and the readings' cadence, and none that draws nothing new; with the beat it
/// still moves on most of them, so it looks live.
#[test]
fn the_live_edge_asks_at_most_four_frames_a_second() {
    let ms = Duration::from_millis;
    for cadence in [ms(1_000), ms(500), ms(2_000)] {
        for moves in [
            Moves::Glide(0.2, 30),
            Moves::Glide(0.27, 30),
            Moves::Glide(0.5, 20),
            Moves::Glide(1.2, 10),
            Moves::Glide(2.0, 10),
            Moves::Glide(40.0, 10),
            Moves::Slide(3.0),
            Moves::Slide(6.0),
            Moves::Slide(10.0),
        ] {
            for beat in [true, false] {
                let secs = 30;
                let (frames, idle) = frames_over(cadence, moves, beat, secs);
                let per_s = frames as f64 / f64::from(secs);
                let what = format!("{cadence:?} readings, {moves:?}, beat {beat}");
                assert!(per_s <= 4.0, "{what}: {per_s} frames a second");
                assert!(idle.is_empty(), "{what}: {idle:?}");
                if beat && cadence == ms(1_000) {
                    assert!(per_s >= 3.0, "CONTROL {what}: the beat draws ({per_s})");
                }
            }
        }
    }
}

#[test]
fn without_motion_nothing_moves_but_the_grey_still_comes() {
    let t0 = Instant::now();
    let f = facts(t0, false, true);
    assert_eq!(f.live(t0, OPEN), DeckLive::STILL);
    assert_eq!(
        f.live(t0 + Duration::from_millis(100), OPEN + 100),
        DeckLive::STILL
    );
    // The one change left is the grey, three cadences on.
    assert_eq!(f.next_change(t0, OPEN), Some(t0 + Duration::from_secs(3)));
    assert_eq!(f.live(t0 + Duration::from_secs(3), OPEN), DeckLive::STALE);
}

#[test]
fn a_stalled_deck_never_looks_live() {
    let t0 = Instant::now();
    let f = facts(t0, true, true);
    let stale = t0 + Duration::from_secs(3);
    assert_eq!(f.live(stale, OPEN), DeckLive::STALE);
    assert_eq!(
        f.live(stale + Duration::from_secs(60), OPEN),
        DeckLive::STALE
    );
    assert_eq!(
        f.next_change(stale, OPEN),
        None,
        "a stalled deck schedules no frame"
    );
    assert_eq!(f.next_change(stale + Duration::from_secs(1), OPEN), None);
    // The last frame before is the grey itself.
    let just_before = stale - Duration::from_millis(10);
    assert!(
        f.next_change(just_before, OPEN + 1)
            .is_some_and(|t| t <= stale)
    );
    // Before any reading: grey, and nothing scheduled.
    let none = LiveFacts { landed: None, ..f };
    assert_eq!(none.live(t0, OPEN), DeckLive::STALE);
    assert_eq!(none.next_change(t0, OPEN), None);
}

#[test]
fn the_card_key_and_the_layers_follow_what_moved() {
    let still = DeckLive::STILL;
    let glide = DeckLive {
        glide_ms: Some(OPEN),
        ..still
    };
    let slide = DeckLive {
        slide: Some(0.5),
        ..still
    };
    let beat = DeckLive {
        beat: Some(0.5),
        ..still
    };
    let keys = [still, DeckLive::STALE, glide, beat, slide].map(|l| l.key());
    for (i, a) in keys.iter().enumerate() {
        assert_ne!(*a, 0);
        for b in keys.iter().skip(i + 1) {
            assert_ne!(a, b);
        }
    }
    assert!(LiveLayer::Glide.moved(&still, &glide));
    assert!(!LiveLayer::Marker.moved(&still, &glide));
    assert!(LiveLayer::Glide.moved(&still, &slide));
    assert!(!LiveLayer::Marker.moved(&still, &slide));
    assert!(LiveLayer::Marker.moved(&still, &beat));
    assert!(!LiveLayer::Glide.moved(&still, &beat));
    assert!(LiveLayer::Marker.moved(&still, &DeckLive::STALE));
    assert!(!LiveLayer::Glide.moved(&still, &DeckLive::STALE));
}

const TEXT: HeaderText<'static> = HeaderText {
    tab: "Tab 2",
    machine: "this Mac",
    over: true,
};

fn head_of(form: DeckForm, w: f64, m: &FaceMetrics) -> Option<(HeaderPlan, Strip)> {
    match form {
        DeckForm::Full => {
            let p = plan::plan_full(w, m);
            (!p.ground_only).then(|| (plan::header_full(&p, TEXT, m), p.strip))
        }
        DeckForm::Folded => {
            let p = plan::plan_folded(w, m);
            (!p.ground_only).then(|| {
                (
                    plan::header_folded(&p, TEXT, [None, None], m),
                    p.blocks[0].strip,
                )
            })
        }
    }
}

fn header_pt_words(m: &FaceMetrics, text: &str) -> f64 {
    m.width(text, plan::HEADER_PT, false)
}

/// The seconds a scale's words name, read as a person reads them: `10 min ago`, `–2.5 min`,
/// `–80 s`; `None` for anything else.
fn read_age(words: &str) -> Option<f64> {
    let words = words.strip_suffix(" ago").unwrap_or(words);
    let words = words.strip_prefix('\u{2013}').unwrap_or(words);
    let (n, unit) = words.split_once(' ')?;
    let n: f64 = n.parse().ok()?;
    match unit {
        "min" => Some(n * 60.0),
        "s" => Some(n),
        _ => None,
    }
}

/// How far back the strip reaches as drawn, seconds: its width in pitches, a period each.
fn drawn_span(strip: Strip) -> f64 {
    (strip.end() + (strip.pitch - strip.cell) - strip.x) / strip.pitch * f64::from(strip.period_s)
}

#[test]
fn the_scales_words_read_as_their_ages() {
    // NEGATIVE CONTROL for `read_age`: it reads what a person reads, and nothing else.
    assert_eq!(read_age("10 min ago"), Some(600.0));
    assert_eq!(read_age("\u{2013}2.5 min"), Some(150.0));
    assert_eq!(read_age("\u{2013}80 s"), Some(80.0));
    assert_eq!(read_age("now"), None);
    assert_eq!(read_age("10 minutes"), None);
    for age in [30, 40, 60, 80, 120, 150, 300, 600] {
        assert_eq!(
            read_age(&plan::tick_words(age)),
            Some(f64::from(age)),
            "{age}"
        );
    }
}

/// What is wrong with `head`'s time scale over `strip`.
fn ruler_faults(head: &HeaderPlan, strip: Strip, m: &FaceMetrics) -> Vec<String> {
    let mut why = Vec::new();
    let Some(time) = head.labels.iter().find(|l| l.label == Label::Time) else {
        return vec!["no time label".into()];
    };
    // The label names how far back the strip reaches as drawn.
    if !plan::time_words(strip).contains(&time.text)
        || read_age(&time.text).is_none_or(|age| (age - drawn_span(strip)).abs() > EPS)
    {
        why.push(format!(
            "the time label says {:?} over {} s of strip",
            time.text,
            drawn_span(strip)
        ));
    }
    let time_w = header_pt_words(m, &time.text);
    // The label stands over the strip's left end, or beside it: ending STRIP_LEAD short of
    // the strip, whose left end a tick then marks.
    let lo = if (time.x - strip.x).abs() < EPS {
        if head.time_tick.is_some() {
            why.push("a tick at the strip's end under a label over it".into());
        }
        strip.x + time_w + 6.0
    } else {
        if (time.x + time_w - (strip.x - plan::STRIP_LEAD)).abs() > EPS {
            why.push(format!(
                "the label beside the strip spans {}..{}",
                time.x,
                time.x + time_w
            ));
        }
        if head.time_tick != Some(strip.x) {
            why.push(format!("the strip's end tick is {:?}", head.time_tick));
        }
        strip.x
    };
    if head.ruler.is_empty()
        && let Some(could) = scale_could_go(head, strip, m)
    {
        why.push(format!(
            "no tick says how far back the strip reaches: {could}"
        ));
    }
    let hi = strip.end() - MARKER_ROOM;
    for (i, t) in head.ruler.iter().enumerate() {
        let want = strip.end() - f64::from(t.age_s) / f64::from(strip.period_s) * strip.pitch;
        if (t.x - want).abs() > EPS {
            why.push(format!("{} at {}, not {want}", t.text, t.x));
        }
        if read_age(&t.text) != Some(f64::from(t.age_s)) || f64::from(t.age_s) >= drawn_span(strip)
        {
            why.push(format!("{} for {} s", t.text, t.age_s));
        }
        let w = header_pt_words(m, &t.text);
        // The tick stands under the middle half of its words.
        let off = (t.left + w / 2.0 - t.x).abs();
        if off > w / 4.0 + EPS {
            why.push(format!(
                "{} at {}..{} stands {off} pt off its tick {}",
                t.text,
                t.left,
                t.left + w,
                t.x
            ));
        }
        if t.left < lo - EPS || t.left + w > hi + EPS {
            why.push(format!(
                "{} at {}..{} leaves {lo}..{hi}",
                t.text,
                t.left,
                t.left + w
            ));
        }
        if let Some(next) = head.ruler.get(i + 1)
            && t.left + w + 6.0 > next.left + EPS
        {
            why.push(format!("{} meets {}", t.text, next.text));
        }
    }
    why
}

/// Why a header that carries no scale could have carried one, or `None` when it could not: no
/// tick's words fit under their middle half beside either label over the strip, and with the
/// label beside the strip every tick that fits there costs the key an item other than "not
/// read" by rule K's own search ([`plan::place_key`]), which the scale never does.
fn scale_could_go(head: &HeaderPlan, strip: Strip, m: &FaceMetrics) -> Option<String> {
    let hi = strip.end() - MARKER_ROOM;
    let tick = |age: u32| {
        let x = strip.end() - f64::from(age) / f64::from(strip.period_s) * strip.pitch;
        (x, header_pt_words(m, &plan::tick_words(age)))
    };
    let ages = plan::tick_ages(strip);
    let labels = plan::time_words(strip);
    for label in &labels {
        let lo = strip.x + header_pt_words(m, label) + 6.0;
        for &age in &ages {
            let (x, w) = tick(age);
            if lo.max(x - 0.75 * w) <= (hi - w).min(x - 0.25 * w) {
                return Some(format!("{} fits beside {label:?}", plan::tick_words(age)));
            }
        }
    }
    let kept = |spans: Vec<plan::KeySpan>| -> Vec<plan::KeyItem> {
        plan::place_key(spans, head.keys, m)
            .placed
            .iter()
            .map(|&(item, _)| item)
            .collect()
    };
    let now = kept(head.key.spans.clone());
    let lead = strip.x - plan::STRIP_LEAD;
    let is_lead = |s: &plan::KeySpan| !s.strip && (s.x1 - lead).abs() < EPS;
    let on_strip =
        |s: &plan::KeySpan| s.strip && s.x0 >= strip.x - EPS && s.x1 <= strip.end() + EPS;
    let name = head.key.spans.iter().copied().find(is_lead)?;
    for label in &labels {
        let x = lead - header_pt_words(m, label);
        if name.x0 > x {
            continue;
        }
        for &age in &ages {
            let (tx, w) = tick(age);
            let (least, most) = (strip.x.max(tx - 0.75 * w), (hi - w).min(tx - 0.25 * w));
            if least > most {
                continue;
            }
            let left = (tx - w / 2.0).clamp(least, most);
            let mut spans: Vec<plan::KeySpan> = head
                .key
                .spans
                .iter()
                .copied()
                .filter(|s| !is_lead(s) && !on_strip(s))
                .collect();
            for (x0, x1, strip) in [
                (name.x0, x - plan::KEY_GAP, false),
                (lead + plan::KEY_GAP, left - plan::KEY_GAP, true),
                (left + w + plan::KEY_GAP, hi, true),
            ] {
                if x1 > x0 {
                    spans.push(plan::KeySpan { x0, x1, strip });
                }
            }
            let trial = kept(spans);
            if now
                .iter()
                .all(|item| *item == plan::KeyItem::NotRead || trial.contains(item))
            {
                return Some(format!(
                    "{label:?} beside the strip with {} keeps the key",
                    plan::tick_words(age)
                ));
            }
        }
    }
    None
}

/// T-G4's widths, both faces, both forms: wherever the header carries the time label, a scale
/// says how far back the strip reaches, each tick at its age from now under the middle of its
/// words. Where the strip is too short for the label and a tick, the label stands beside it;
/// the scale is missing only where every placement would cost the key "over max" or the ramp
/// ([`scale_could_go`]), which on the Mac's face (Helvetica Neue's metrics) is nowhere.
#[test]
fn the_time_scale_marks_how_far_back_at_every_width() {
    let faces = [
        ("Helvetica Neue", FaceMetrics::helvetica_neue()),
        ("the test stack", FaceMetrics::test_stack()),
    ];
    let mut failed = Vec::new();
    let (mut drew, mut beside, mut bare_hn) = (0, 0, 0);
    for (face, m) in &faces {
        for form in [DeckForm::Full, DeckForm::Folded] {
            for w in (160..=5120).step_by(3).map(f64::from) {
                let Some((head, strip)) = head_of(form, w, m) else {
                    continue;
                };
                if !head.labels.iter().any(|l| l.label == Label::Time) {
                    continue;
                }
                drew += 1;
                beside += usize::from(head.time_tick.is_some());
                bare_hn += usize::from(head.ruler.is_empty() && *face == "Helvetica Neue");
                for why in ruler_faults(&head, strip, m) {
                    if failed.len() < 30 {
                        failed.push(format!("{face}, {form:?} {w} pt: {why}"));
                    }
                }
            }
        }
    }
    assert!(
        drew > 4_000,
        "CONTROL: {drew} headers carried the time label"
    );
    assert!(
        beside > 0 && beside < drew / 2,
        "CONTROL: {beside} of {drew} labels stand beside their strip"
    );
    assert_eq!(bare_hn, 0, "headers without a scale on the Mac's face");
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// The scale at the widths the mockups draw and the owner's default window, and at each
/// strip's period: the ticks, by age. Where "–1 min" stands near the strip's end (Full 550,
/// Folded 1119 and 800) its words cannot keep the tick under their middle half and clear the
/// marker, so "–2 min" stands instead (with words free to slide until their edge sits on the
/// tick, "–1 min" stood there).
#[test]
fn the_time_scale_at_the_mockups_widths() {
    let m = FaceMetrics::helvetica_neue();
    let ages = |form, w| -> (u32, String, Vec<u32>) {
        let (head, strip) = head_of(form, w, &m).expect("the table is drawn");
        let mut ages: Vec<u32> = head.ruler.iter().map(|t: &RulerTick| t.age_s).collect();
        ages.sort_unstable();
        let label = head
            .labels
            .iter()
            .find(|l| l.label == Label::Time)
            .map(|l| l.text.clone())
            .unwrap_or_default();
        (strip.period_s, label, ages)
    };
    let got = [
        (DeckForm::Full, 2560.0),
        (DeckForm::Full, 1424.0),
        (DeckForm::Full, 1119.0),
        (DeckForm::Full, 800.0),
        (DeckForm::Full, 550.0),
        (DeckForm::Folded, 1119.0),
        (DeckForm::Folded, 800.0),
        (DeckForm::Folded, 600.0),
    ]
    .map(|(form, w)| ages(form, w));
    assert_eq!(
        got,
        [
            (10, "10 min ago".into(), vec![60, 120, 300]),
            (10, "10 min ago".into(), vec![60, 120, 300]),
            (10, "10 min ago".into(), vec![60, 300]),
            (10, "10 min ago".into(), vec![60, 300]),
            (30, "10 min ago".into(), vec![120]),
            (1, "30 s ago".into(), vec![10, 20]),
            (1, "–20 s".into(), vec![10]),
            (1, "20 s ago".into(), vec![10]),
        ]
    );
    // The owner's default window (Folded at about 600 pt): the label stands beside block A's
    // strip and the strip's whole header is the scale's, "–10 s" centred over its middle.
    let (head, strip) = head_of(DeckForm::Folded, 600.0, &m).expect("the table is drawn");
    assert_eq!(head.time_tick, Some(strip.x));
    let mid = &head.ruler[0];
    let w = header_pt_words(&m, &mid.text);
    assert!((mid.left + w / 2.0 - mid.x).abs() < EPS, "{mid:?}");
}

/// A headless App whose one focused window draws the v2 deck, holding a reading that landed at
/// `landed`.
#[cfg(unix)]
fn live_app(landed: Instant) -> (crate::App, crate::WindowId) {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("deck".into());
    let snap = crate::stats_deck::paint_tests::scene(crate::stats_deck::paint_tests::Scene::Calm);
    app.vitals.snapshot = Some(std::sync::Arc::new(snap));
    app.vitals.landed_at = Some(landed);
    app.windows.get_mut(&wid).expect("the window").focused = true;
    (app, wid)
}

/// The window's own facts: a focused deck glides and beats; Reduce Motion, `motion =
/// "reduced"`, an unfocused window and serious mode take what they must and leave the grey; a
/// stalled reading greys and schedules nothing; no deck, no live edge.
#[cfg(unix)]
#[test]
fn the_app_moves_the_live_edge_only_where_motion_is_allowed() {
    let _attached = crate::stats_deck::tests::attach_stats();
    let t0 = Instant::now();
    let (mut app, wid) = live_app(t0);
    // Before the deck is laid out it has said nothing of its strips: the beat rings, nothing
    // moves.
    let facts = app.live_facts(wid).expect("the deck draws");
    assert!(facts.motion && facts.beat, "{facts:?}");
    assert_eq!(facts.landed, Some(t0));
    assert_eq!(facts.flow, Flow::None);
    let live = facts.live(t0, OPEN);
    assert!(live.fresh && live.glide_ms.is_none() && live.slide.is_none());
    assert_eq!(live.beat, beat_phase(0));
    // A Folded strip of 6 px a slot, after a reading that opened a second: it slides on the
    // beat's frames.
    let at_ms = app.vitals.snapshot.as_ref().expect("a snapshot").at_unix_ms;
    app.vitals.landed_prev_at_ms = Some(at_ms - 1000);
    let ws = app.windows.get_mut(&wid).expect("the window");
    ws.deck_glide.set((0.0, 0, 6.0));
    let facts = app.live_facts(wid).expect("the deck draws");
    assert_eq!(facts.flow, Flow::Slide { frames: 4 });
    let live = facts.live(t0, OPEN);
    assert!(live.fresh && live.glide_ms.is_none() && live.slide == Some(0.25));
    // A reading inside the second the last one opened (the lens reads every half second) opens
    // none: the beat rings, the strip stands.
    let mut half = (**app.vitals.snapshot.as_ref().expect("a snapshot")).clone();
    half.at_unix_ms = at_ms + 600;
    app.vitals.snapshot = Some(std::sync::Arc::new(half));
    app.vitals.landed_prev_at_ms = Some(at_ms + 100);
    let facts = app.live_facts(wid).expect("the deck draws");
    assert_eq!(facts.flow, Flow::Slide { frames: 0 });
    assert_eq!(facts.live(t0, OPEN).slide, None);
    // Slots are the snapshot's grid's, not the wall clock's seconds: with its edges half a
    // second in, readings at 900 and 1100 ms share a slot.
    let mut edged = (**app.vitals.snapshot.as_ref().expect("a snapshot")).clone();
    edged.at_unix_ms = at_ms + 1_100;
    edged.slot_grid.origin_ms = 500;
    app.vitals.snapshot = Some(std::sync::Arc::new(edged));
    app.vitals.landed_prev_at_ms = Some(at_ms + 900);
    let facts = app.live_facts(wid).expect("the deck draws");
    assert_eq!(facts.flow, Flow::Slide { frames: 0 });
    app.vitals.landed_prev_at_ms = Some(at_ms + 400);
    let facts = app.live_facts(wid).expect("the deck draws");
    assert_eq!(facts.flow, Flow::Slide { frames: 4 }, "CONTROL");
    // A Full strip glides on the wall clock instead.
    let ws = app.windows.get_mut(&wid).expect("the window");
    ws.deck_glide.set((0.6, 10, 0.0));
    let facts = app.live_facts(wid).expect("the deck draws");
    assert!(matches!(facts.flow, Flow::Glide { .. }), "{facts:?}");
    let live = facts.live(t0, OPEN);
    assert!(live.fresh && live.glide_ms.is_some() && live.slide.is_none());
    assert_eq!(live.beat, beat_phase(0));
    // The grid's next frame, which beats.
    let next = app.stats_live_deadline(t0);
    assert!(
        next.is_some_and(|at| at > t0 && at <= t0 + FRAME),
        "{next:?}"
    );

    app.serious_mode = true;
    let serious = app.live_facts(wid).expect("the deck draws");
    assert!(
        serious.motion && !serious.beat,
        "serious mode takes the beat only"
    );
    app.serious_mode = false;

    app.config.motion = Some("reduced".into());
    let reduced = app.live_facts(wid).expect("the deck draws");
    assert_eq!(reduced.live(t0, OPEN), DeckLive::STILL);
    assert_eq!(
        app.stats_live_deadline(t0),
        Some(t0 + reduced.stale_after),
        "only the grey is scheduled"
    );
    app.config.motion = None;

    app.system_reduce_motion = true;
    assert_eq!(
        app.live_facts(wid).map(|f| f.live(t0, OPEN)),
        Some(DeckLive::STILL)
    );
    app.system_reduce_motion = false;

    app.windows.get_mut(&wid).expect("the window").focused = false;
    assert_eq!(
        app.live_facts(wid).map(|f| f.live(t0, OPEN)),
        Some(DeckLive::STILL)
    );
    app.windows.get_mut(&wid).expect("the window").focused = true;

    let long_ago = t0
        .checked_sub(Duration::from_secs(60))
        .expect("an instant a minute back");
    app.vitals.landed_at = Some(long_ago);
    assert_eq!(
        app.live_facts(wid).map(|f| f.live(t0, OPEN)),
        Some(DeckLive::STALE)
    );
    assert_eq!(
        app.stats_live_deadline(t0),
        None,
        "a stalled deck schedules nothing"
    );
    app.vitals.landed_at = Some(t0);

    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("off".into());
    assert!(app.live_facts(wid).is_none());
    assert_eq!(app.stats_live_deadline(t0), None);
    assert_eq!(app.deck_live(wid), DeckLive::STILL);
}

/// A frame that moves only the live edge lays the deck out once, for its fingerprint and its
/// raster both, over the words and the header line the last layout read: it measures no words and
/// plans no header line. A new reading, a form, a width or a tab of its own reads them again. Every
/// card is what a layout of its frame from nothing rasters whole, at the live edge it says it
/// shows, also when that edge moved between the fingerprint's layout and the raster.
#[cfg(unix)]
#[test]
fn a_live_frame_lays_the_deck_out_once_over_what_it_kept() {
    let _attached = crate::stats_deck::tests::attach_stats();
    let (mut app, wid) = live_app(Instant::now());
    // Read at an hour's cadence, the reading stays fresh for the whole test.
    app.vitals.landed_period = Some(Duration::from_secs(3600));
    let fit = |app: &mut crate::App| {
        let (_, cell) = app.win_cell_size(wid);
        let ws = app.windows.get_mut(&wid).expect("the window");
        ws.metrics.head = crate::stats_deck::deck_px(
            crate::stats_deck::StatsBars::Deck,
            ws.deck_form,
            ws.scale,
            cell,
        );
    };
    fit(&mut app);
    // The frame's fingerprint, then `between`, then its raster.
    let split = |app: &mut crate::App, between: &dyn Fn(&mut crate::App)| {
        let was = crate::stats_deck::layout_work();
        let _ = app.stats_deck_fp(wid);
        between(app);
        app.splice_stats_deck(wid);
        let now = crate::stats_deck::layout_work();
        [0, 1, 2].map(|i| now[i] - was[i])
    };
    let frame = |app: &mut crate::App| split(app, &|_| {});
    let card = |app: &crate::App| {
        app.windows[&wid]
            .deck_card
            .as_ref()
            .map(|c| (c.geom, c.rgba.clone()))
            .expect("a card")
    };
    // The card's pixels are a fresh layout's of the frame's inputs, at the live edge the card
    // records (the next patch's `was`).
    let whole = |app: &crate::App, at: &str| {
        let mut f = app.deck_frame(wid).expect("the deck draws");
        f.live = app.windows[&wid].deck_live_card.get().1;
        let prims = crate::stats_deck::deck_prims(&f.geom, &app.deck_input(&f));
        let (rgba, ..) = crate::tray_raster::rasterize_tray(
            &prims,
            f.geom.w as u32,
            f.geom.h as u32,
            1.0,
            [0, 0, 0, 0],
        );
        assert!(
            card(app).1 == rgba,
            "{at}: the card is not what a fresh layout rasters"
        );
    };
    let shows = |app: &crate::App| app.windows[&wid].deck_live_card.get().1;
    let work = frame(&mut app);
    whole(&app, "the first frame");
    assert_eq!(work, [1, 1, 1], "CONTROL: the first frame reads all");
    let first = card(&app);
    // The reading's landing forgotten: the marker greys and the strips stand still, and nothing
    // else the deck draws moved.
    app.vitals.landed_at = None;
    let work = frame(&mut app);
    whole(&app, "a live frame");
    assert_eq!(shows(&app), DeckLive::STALE);
    assert_eq!(work, [1, 0, 0], "a live frame reads nothing again");
    let grey = card(&app);
    assert_ne!(
        grey.0, first.0,
        "CONTROL: the live edge moved the card's key"
    );
    assert_ne!(
        grey.1, first.1,
        "CONTROL: the live edge moved the card's pixels"
    );
    // Under the same reading, what else the layout reads reads the words and the header again.
    type Change = fn(&mut crate::App, crate::WindowId);
    let changes: [(&str, Change); 3] = [
        ("the other form", |app, wid| {
            let ws = app.windows.get_mut(&wid).expect("the window");
            ws.deck_form = match ws.deck_form {
                DeckForm::Full => DeckForm::Folded,
                DeckForm::Folded => DeckForm::Full,
            };
        }),
        ("a wider window", |app, wid| {
            app.windows.get_mut(&wid).expect("the window").cols += 40;
        }),
        ("a second tab, in front", |app, wid| {
            let id = app.tab_ids.allocate().expect("a tab id");
            let ws = app.windows.get_mut(&wid).expect("the window");
            let mut tab = ws.tab_set.tabs()[0].clone();
            tab.id = id;
            ws.tab_set.push(tab).expect("a fresh tab id");
        }),
    ];
    for (at, change) in changes {
        let was = card(&app);
        change(&mut app, wid);
        fit(&mut app);
        let work = frame(&mut app);
        whole(&app, at);
        assert_ne!(card(&app).1, was.1, "CONTROL {at}: the card's pixels moved");
        assert_eq!(work, [1, 1, 1], "{at} reads all again");
    }
    // The live edge moves after the fingerprint laid the deck out: the raster draws that layout
    // and records its edge, and the next frame patches from there.
    app.config.motion = Some("reduced".into());
    app.vitals.landed_at = Some(Instant::now());
    let work = frame(&mut app);
    whole(&app, "a still frame");
    assert_eq!(shows(&app), DeckLive::STILL);
    assert_eq!(work, [1, 0, 0]);
    app.config.motion = None;
    let work = split(&mut app, &|app| app.vitals.landed_at = None);
    whole(&app, "a frame whose edge moved after its fingerprint");
    assert!(
        shows(&app).fresh && shows(&app).glide_ms.is_some(),
        "the raster draws the fingerprint's gliding edge, not {:?}",
        shows(&app)
    );
    assert_eq!(work, [1, 0, 0]);
    let work = frame(&mut app);
    whole(&app, "the frame after it");
    assert_eq!(shows(&app), DeckLive::STALE);
    assert_eq!(work, [1, 0, 0]);
    let mut next = app.vitals.snapshot.as_deref().expect("a reading").clone();
    next.seq += 1;
    app.vitals.snapshot = Some(std::sync::Arc::new(next));
    let work = frame(&mut app);
    whole(&app, "a new reading");
    assert_eq!(work, [1, 1, 1], "a new reading reads all again");
}

/// A deck turned off and on again lays its card out again, so its hits and the live edge it drew
/// come back with it, even when a layout made for the fingerprint alone (a pointer's hit test)
/// was never rastered.
#[cfg(unix)]
#[test]
fn a_deck_turned_off_and_on_lays_its_card_out_again() {
    let _attached = crate::stats_deck::tests::attach_stats();
    let (mut app, wid) = live_app(Instant::now());
    app.vitals.landed_period = Some(Duration::from_secs(3600));
    let (_, cell) = app.win_cell_size(wid);
    let ws = app.windows.get_mut(&wid).expect("the window");
    ws.metrics.head = crate::stats_deck::deck_px(
        crate::stats_deck::StatsBars::Deck,
        ws.deck_form,
        ws.scale,
        cell,
    );
    let _ = app.stats_deck_fp(wid);
    app.splice_stats_deck(wid);
    // The edge moves and only a hit test lays the deck out for it.
    app.vitals.landed_at = None;
    let _ = app.stats_deck_fp(wid);
    let set = |app: &mut crate::App, bars: &str| {
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some(bars.into());
    };
    set(&mut app, "off");
    let _ = app.stats_deck_fp(wid);
    app.splice_stats_deck(wid);
    let ws = &app.windows[&wid];
    assert!(
        ws.deck_card.is_none() && ws.stats_hits.borrow().is_empty(),
        "CONTROL: the deck off holds no card and no hits"
    );
    set(&mut app, "deck");
    let _ = app.stats_deck_fp(wid);
    app.splice_stats_deck(wid);
    let ws = &app.windows[&wid];
    assert!(ws.deck_card.is_some(), "CONTROL: the card is back");
    assert!(
        !ws.stats_hits.borrow().is_empty(),
        "the card's hits are back"
    );
    assert_eq!(
        ws.deck_live_drawn.get(),
        Some(DeckLive::STALE.key()),
        "the live edge it drew is known"
    );
}
