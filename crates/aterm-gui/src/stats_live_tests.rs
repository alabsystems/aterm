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

/// 10:42:10 on the mockups' day, unix ms: a second (and a 10 s column) opens.
const OPEN: i64 = 1_792_000_800_000 + 42 * 60_000 + 10_000;

/// Full's glide runs with the wall clock for a slot of the snapshot's grid from the reading,
/// wherever in its slot the reading fell, and holds there.
#[test]
fn the_glide_runs_a_slot_from_its_reading() {
    let secs = SlotGrid::SECONDS;
    assert_eq!(glide_end_ms(OPEN + 500, secs), OPEN + 1_500);
    assert_eq!(glide_end_ms(OPEN + 900, secs), OPEN + 1_900);
    assert_eq!(glide_end_ms(OPEN, secs), OPEN + 1_000);
    // On the snapshot's grid, its slots starting half a second in: a slot's length still.
    let half = SlotGrid {
        origin_ms: 500,
        ..secs
    };
    assert_eq!(glide_end_ms(OPEN + 900, half), OPEN + 1_900);
    // From a reading half a second into its slot: nothing of it has run, then half, then all.
    let (at, end) = (OPEN + 500, OPEN + 1_500);
    assert!(glide_frac(at, at, end).abs() < EPS);
    assert!((glide_frac(at, at + 500, end) - 0.5).abs() < EPS);
    assert!((glide_frac(at, end, end) - 1.0).abs() < EPS);
    // Past its end with no reading to open the next: the band waits in its slots.
    assert!((glide_frac(at, end + 15_000, end) - 1.0).abs() < EPS);
    // A clock behind the reading counts as the reading's, so the band never glides backward.
    assert!(glide_frac(at, at - 300, end).abs() < EPS);
    // A reading with nothing of its slot left has run out.
    assert!((glide_frac(end, end, end) - 1.0).abs() < EPS);
}

/// A reading moves the fine band by the time it opened (§3.4 of the now-left design): a 1 s
/// reading on 2 s boxes moves half a box, two seconds a whole one, and more never more than
/// one. What it still lags is the part not yet run in whole pixels, rounded up: all of it as the
/// move starts, none only once it has run.
#[test]
fn a_reading_moves_the_fine_band_by_the_time_it_opened() {
    let band = FineBand {
        box_px: 8.0,
        period_ms: 2_000,
    };
    assert_eq!(band.step_px(1_000), 4.0);
    assert_eq!(band.step_px(2_000), 8.0);
    assert_eq!(band.step_px(5_000), 8.0);
    assert_eq!(band.step_px(0), 0.0);
    assert_eq!(band.step_px(-1_000), 0.0);
    // A pitch as laid out, before rounding: the step is whole pixels.
    let laid = FineBand {
        box_px: 4.6,
        period_ms: 2_000,
    };
    assert_eq!(laid.step_px(1_000), 2.0);
    assert_eq!(FineBand::default().step_px(1_000), 0.0);
    // The faster of two bands sets a card's frames.
    let slow = FineBand {
        box_px: 8.0,
        period_ms: 4_000,
    };
    assert_eq!(band.faster(slow), band);
    assert_eq!(slow.faster(band), band);
    assert_eq!(FineBand::default().faster(band), band);
    // What it lags.
    assert_eq!(lag_px(4.0, 0.0), 4.0);
    assert_eq!(lag_px(4.0, 0.25), 3.0);
    assert_eq!(lag_px(4.0, 0.3), 3.0);
    assert_eq!(lag_px(4.0, 0.99), 1.0);
    assert_eq!(lag_px(4.0, 1.0), 0.0);
    assert_eq!(lag_px(4.0, 7.0), 0.0);
    assert_eq!(lag_px(4.0, -1.0), 4.0);
    for step in [0.0, -2.0, f64::NAN] {
        assert_eq!(lag_px(step, 0.5), 0.0, "{step}");
    }
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
    // A band gliding 2 px a second: a whole pixel every 500 ms, every other frame of the grid;
    // 1 px a second: a pixel a second.
    assert_eq!(glide_step(2.0), Duration::from_millis(500));
    assert_eq!(glide_step(1.0), Duration::from_secs(1));
    assert_eq!(glide_step(4.0), FRAME);
    // A slow band (2 px over the 15 s of a slot) still steps a whole pixel, never a fraction of
    // one.
    let slim = 2.0 / 15.0;
    assert!(
        (glide_step(slim).as_secs_f64() * slim - 1.0).abs() < 1e-3,
        "{:?}",
        glide_step(slim)
    );
    // Whole milliseconds, rounded up: a band gliding 3 px a second steps every 334 ms, never
    // 333, so no step of its clock moves it under a pixel (a 299 ms step of a 3 px glide once
    // asked a frame that drew nothing new, the review of 2026-10-26).
    assert_eq!(glide_step(3.0), Duration::from_millis(334));
    for tenths in 1..=400_u32 {
        let px_per_s = f64::from(tenths) / 10.0;
        let step = glide_step(px_per_s);
        assert_eq!(step.subsec_nanos() % 1_000_000, 0, "{px_per_s}: {step:?}");
        assert!(
            step == GLIDE_STEP_MIN
                || step == GLIDE_STEP_MAX
                || step.as_secs_f64() * px_per_s >= GLIDE_QUANTUM_PX,
            "{px_per_s} px/s steps {step:?}"
        );
    }
    // Fast strips are held to the grid's four frames a second, slow ones to one each 10 s.
    assert_eq!(glide_step(100.0), GLIDE_STEP_MIN);
    assert_eq!(GLIDE_STEP_MIN, FRAME);
    assert_eq!(glide_step(0.01), GLIDE_STEP_MAX);
    // No strip on the card: the least wait, never a zero or a panic.
    for px in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert_eq!(glide_step(px), GLIDE_STEP_MIN, "{px}");
    }
}

/// A reading that landed at `landed`, whose Full strips glide a second's step from `OPEN` to
/// `OPEN + 4 s` in 250 ms steps.
fn facts(landed: Instant, motion: bool, beat: bool) -> LiveFacts {
    LiveFacts {
        landed: Some(landed),
        stale_after: Duration::from_secs(3),
        motion,
        beat,
        tween: false,
        needle: None,
        drawn: None,
        flow: Flow::Glide {
            opened_ms: 1_000,
            from_ms: OPEN,
            end_ms: OPEN + 4_000,
            step: Duration::from_millis(250),
        },
        steam: false,
    }
}

/// How far a frame's glide or slide has run, `None` with every box in its slot.
fn ran(live: DeckLive) -> Option<f64> {
    live.flow.map(|flow| flow.ran)
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
    assert_eq!(ran(at), Some(0.25), "quantized to the glide's step");
    assert_eq!(at.flow.map(|flow| flow.opened_ms), Some(1_000));
    assert_eq!(at.beat, beat_phase(0));
    assert!(at.fresh);
    // Inside a frame of the grid nothing moves: the edge stands as at the frame's moment.
    let ms = Duration::from_millis;
    assert_eq!(f.live(t0 + ms(130), ms0 + 130), at);
    // Each frame of the grid beats wider and fainter, and the glide moves with it.
    let one = f.live(t0 + FRAME, ms0 + 250);
    assert_eq!(one.beat, beat_phase(1));
    assert_eq!(ran(one), Some(0.3125));
    assert!(beat_phase(1) > beat_phase(0) && beat_phase(2) > beat_phase(1));
    // The beat rings for its frames and ends; the strips glide on.
    assert_eq!(FRAME * BEAT_FRAMES, ms(750));
    let after = f.live(t0 + FRAME * BEAT_FRAMES, ms0 + 750);
    assert_eq!(after.beat, None);
    assert!(after.fresh && ran(after) == Some(0.4375));
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

/// Once the glide has run its slot from the reading, every box stands in its slot until a reading
/// opens the next: the glide holds and asks no frame that would draw the same strips. Its steps
/// count from the reading, which landed 150 ms after it was read.
#[test]
fn the_glide_holds_at_its_end() {
    let t0 = Instant::now();
    let (from, ms0) = (OPEN + 9_250, OPEN + 9_400);
    let end = glide_end_ms(from, SlotGrid::SECONDS);
    let f = LiveFacts {
        flow: Flow::Glide {
            opened_ms: 1_000,
            from_ms: from,
            end_ms: end,
            step: FRAME,
        },
        ..facts(t0, true, false)
    };
    let ms = Duration::from_millis;
    assert_eq!(ran(f.live(t0, ms0)), Some(0.0));
    assert_eq!(ran(f.live(t0 + ms(500), ms0 + 500)), Some(0.5));
    assert_eq!(ran(f.live(t0 + ms(750), ms0 + 750)), Some(0.75));
    assert_eq!(ran(f.live(t0 + ms(1_000), ms0 + 1_000)), None);
    assert_eq!(ran(f.live(t0 + ms(1_250), ms0 + 1_250)), None);
    // Four frames to the end, then only the grey.
    assert_eq!(f.next_change(t0, ms0), Some(t0 + FRAME));
    assert_eq!(
        f.next_change(t0 + FRAME * 3, ms0 + 750),
        Some(t0 + FRAME * 4)
    );
    assert_eq!(
        f.next_change(t0 + FRAME * 4, ms0 + 1_000),
        Some(t0 + f.stale_after),
        "past the glide's end nothing glides"
    );
    // CONTROL: further off, the same clock would glide on.
    let on = LiveFacts {
        flow: Flow::Glide {
            opened_ms: 1_000,
            from_ms: from,
            end_ms: end + 1_000,
            step: FRAME,
        },
        ..f
    };
    assert_eq!(
        on.next_change(t0 + FRAME * 4, ms0 + 1_000),
        Some(t0 + FRAME * 5)
    );
}

/// The most a frame moves a Full fine band of `box_px` device px on boxes of `period_ms`, under
/// readings landing each `cadence_ms` (each opening as much), the first `into_ms` into its slot,
/// when each reading's glide is `flow_of` it: the band a frame draws is the steps landed so far
/// less the lag left of the glide; a reading moves it what its predecessor's glide had left.
/// Panics on a frame that moves it left.
fn biggest_glide_move(
    (box_px, period_ms): (f64, i64),
    cadence_ms: i64,
    into_ms: i64,
    flow_of: impl Fn(FineBand, Opened) -> Flow,
) -> f64 {
    let band = FineBand { box_px, period_ms };
    let t0 = Instant::now();
    let ms0 = OPEN + into_ms;
    let cadence = Duration::from_millis(u64::try_from(cadence_ms).unwrap_or(0));
    let (mut landed_px, mut drawn, mut biggest) = (0.0, 0.0, 0.0_f64);
    for r in 0..4_u32 {
        let landed = t0 + cadence * r;
        let opened = Opened {
            at_ms: wall(t0, ms0, landed),
            ms: cadence_ms,
            landed,
        };
        let f = LiveFacts {
            stale_after: Duration::from_secs(10),
            flow: flow_of(band, opened),
            ..facts(landed, true, false)
        };
        landed_px += band.step_px(cadence_ms);
        let at = |t: Instant| {
            let lag = f
                .live(t, wall(t0, ms0, t))
                .flow
                .map_or(0.0, |flow| lag_px(band.step_px(flow.opened_ms), flow.ran));
            landed_px - lag
        };
        let mut now = landed;
        let mut frames = vec![landed];
        while let Some(next) = f.next_change(now, wall(t0, ms0, now)) {
            if next >= landed + cadence {
                break;
            }
            frames.push(next);
            now = next;
        }
        for t in frames {
            let x = at(t);
            assert!(
                x >= drawn,
                "{box_px} px, {cadence_ms} ms, {into_ms} ms in: left at {:?}",
                t - t0
            );
            biggest = biggest.max(x - drawn);
            drawn = x;
        }
    }
    biggest
}

/// Full's glide spreads each reading's step over a slot's length (the review of 2026-10-26):
/// wherever in its second a reading lands, no frame moves the band half a box or more, which the
/// eye reads either way, or backward, on the 1 s boxes the owner's blocks lay (a whole box a
/// reading a second) as on the 2 s boxes before them (half a box). Gliding to the reading's
/// slot's end instead packed the step into what was left of the slot: for a reading 900 ms into
/// its second, half a box or more in a frame.
#[test]
fn no_glide_frame_moves_the_band_half_a_box() {
    let to_slot_end = |band: FineBand, opened: Opened| {
        let secs = SlotGrid::SECONDS;
        let end = (secs.slot(opened.at_ms) + 1) * secs.slot_ms;
        let step = band.step_px(opened.ms);
        let run_s = (end - opened.at_ms) as f64 / 1000.0;
        Flow::Glide {
            opened_ms: opened.ms,
            from_ms: opened.at_ms,
            end_ms: end,
            step: glide_step(step / run_s),
        }
    };
    let mut checked = 0;
    for period_ms in [1_000, 2_000] {
        for box_px in [3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 12.0, 16.0] {
            let boxes = (box_px, period_ms);
            for cadence_ms in [1_000, 2_000] {
                for into_ms in [100, 300, 500, 700, 900] {
                    let biggest = biggest_glide_move(boxes, cadence_ms, into_ms, |band, opened| {
                        Flow::of((band, FineBand::default()), opened, SlotGrid::SECONDS)
                    });
                    assert!(
                        biggest > 0.0 && biggest < box_px / 2.0,
                        "{box_px} px boxes of {period_ms} ms, a reading each {cadence_ms} ms, \
                         {into_ms} ms into its second: a frame moved {biggest} px"
                    );
                    checked += 1;
                }
            }
            // CONTROL: the glide to the slot's end moves a late reading's step in one frame.
            assert!(
                biggest_glide_move(boxes, 1_000, 900, to_slot_end) >= box_px / 2.0 - 0.5,
                "{box_px} px"
            );
            assert!(
                biggest_glide_move(boxes, 2_000, 900, to_slot_end) >= box_px,
                "{box_px} px"
            );
        }
    }
    assert_eq!(checked, 160);
}

/// A reading that opens a new second slides a Folded strip's fine band its step right over the
/// frames of the grid after it, the beat's own: the reading's frame carries it a quarter of the
/// way, and 750 ms on it has come in and nothing moves until the next reading.
#[test]
fn a_reading_that_opens_a_second_slides_the_strip_in_on_the_beats_frames() {
    let t0 = Instant::now();
    let ms0 = OPEN + 1_234;
    let slide = |frames, beat| LiveFacts {
        flow: Flow::Slide {
            opened_ms: 1_000,
            landed: t0,
            frames,
        },
        ..facts(t0, true, beat)
    };
    let f = slide(slide_frames(6.0), true);
    let at = |k: u32| f.live(t0 + FRAME * k, ms0 + 250 * i64::from(k));
    assert_eq!(
        [0, 1, 2, 3, 4].map(|k| ran(at(k))),
        [Some(0.25), Some(0.5), Some(0.75), None, None]
    );
    assert!(
        (0..3).all(|k| at(k).beat == beat_phase(k)),
        "on the beat's frames"
    );
    assert!(
        (0..3).all(|k| at(k).flow.is_some_and(|flow| flow.opened_ms == 1_000)),
        "the reading's step"
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
    // A band whose reading moved it nothing: nothing slides, nothing is asked but the grey.
    let none = LiveFacts {
        flow: Flow::None,
        ..quiet
    };
    assert!((0..5).all(|k| none.live(t0 + FRAME * k, ms0).flow.is_none()));
    assert_eq!(none.next_change(t0, ms0), Some(t0 + none.stale_after));
    // Without motion the strip stands in its slots.
    let still = LiveFacts { motion: false, ..f };
    assert_eq!(still.live(t0, ms0), DeckLive::STILL);
}

/// Each frame of a slide moves the fine band at least a whole device pixel and, on boxes of 3 px
/// or more, under half a box, so it reads as a step right even over a strip of one heat step,
/// where a move of half a box or more reads either way; and it takes at most the grid's four
/// frames, whatever time the reading opened.
#[test]
fn every_frame_of_a_slide_moves_a_pixel_and_under_half_a_box() {
    for box_px in 2..=24_u32 {
        let b = f64::from(box_px);
        let band = FineBand {
            box_px: b,
            period_ms: 2_000,
        };
        for opened in [1_000, 2_000, 3_000] {
            let step = band.step_px(opened);
            let frames = slide_frames(step);
            let at = format!("{box_px} px boxes, {opened} ms opened, a {step} px step");
            assert!(
                (1..=SLIDE_FRAMES).contains(&frames) && f64::from(frames) <= step,
                "{at}: {frames}"
            );
            // The lag before the reading (the last reading's band stood at rest, a step right of
            // where the new reading's slots put the same time) and at each of the slide's frames.
            let mut lags = vec![step];
            lags.extend(
                (0..frames).map(|k| lag_px(step, slide_phase(k, frames).map_or(1.0, f64::from))),
            );
            assert_eq!(lags.last(), Some(&0.0), "{at}: it comes in: {lags:?}");
            for w in lags.windows(2) {
                let moved = w[0] - w[1];
                assert!(
                    moved >= 1.0 && moved.fract() == 0.0,
                    "{at}: a frame moved {moved}: {lags:?}"
                );
                if box_px >= 3 {
                    assert!(moved < b / 2.0, "{at}: a frame moved {moved}: {lags:?}");
                }
            }
        }
    }
    for p in [f64::NAN, 0.0, -3.0, 0.5] {
        assert_eq!(slide_frames(p), 1, "{p}");
    }
}

#[test]
fn a_reading_opens_its_slots_once() {
    let secs = SlotGrid::SECONDS;
    assert_eq!(
        opened_ms(None, OPEN, secs),
        0,
        "the first reading moves nothing"
    );
    assert_eq!(opened_ms(Some(OPEN + 100), OPEN + 900, secs), 0);
    assert_eq!(opened_ms(Some(OPEN + 900), OPEN + 1_100, secs), 1_000);
    assert_eq!(opened_ms(Some(OPEN - 5_000), OPEN, secs), 5_000);
    assert_eq!(
        opened_ms(Some(OPEN + 2_000), OPEN + 1_000, secs),
        0,
        "a clock set back"
    );
    // On the snapshot's grid, its slots starting half a second in: readings at 900 and 1100 ms
    // share one, 400 and 600 do not.
    let half = SlotGrid {
        origin_ms: 500,
        ..secs
    };
    assert_eq!(opened_ms(Some(OPEN + 900), OPEN + 1_100, half), 0);
    assert_eq!(opened_ms(Some(OPEN + 400), OPEN + 600, half), 1_000);
    // The strip's slot is the grid's.
    assert_eq!(
        i64::from(crate::stats_plan::FOLD_SLOT_S) * 1000,
        aterm_vitals::ring::SLOT_MS
    );
}

/// A reading that opens no slot (the lens's second in a second) leaves the strips' fine band to
/// the reading before it, whose slide or glide runs on to its end; with the lens open the second
/// reading of each slot once snapped the band to rest with the rest of its step undrawn (the
/// review of 2026-10-26). The first reading, and one after a reading that opened nothing either,
/// moves nothing.
#[test]
fn a_reading_that_opens_no_slot_lets_the_last_ones_move_run_on() {
    let secs = SlotGrid::SECONDS;
    let t0 = Instant::now();
    let ms = Duration::from_millis;
    let first = Opened::after(None, None, OPEN + 300, secs, t0);
    assert_eq!(
        first,
        Opened {
            at_ms: OPEN + 300,
            ms: 0,
            landed: t0
        }
    );
    let opener = Opened::after(
        Some(first),
        Some(OPEN + 300),
        OPEN + 1_250,
        secs,
        t0 + ms(950),
    );
    assert_eq!((opener.at_ms, opener.ms), (OPEN + 1_250, 1_000));
    // Half a second on, in the same slot: the opener still moves the band.
    let second = t0 + ms(1_450);
    let same = Opened::after(Some(opener), Some(OPEN + 1_250), OPEN + 1_750, secs, second);
    assert_eq!(same, opener);
    // The next slot's reading takes over.
    let next = Opened::after(
        Some(same),
        Some(OPEN + 1_750),
        OPEN + 2_250,
        secs,
        t0 + ms(1_950),
    );
    assert_eq!((next.at_ms, next.ms), (OPEN + 2_250, 1_000));
    // A clock set back opens nothing either: the band's last move has long run out.
    assert_eq!(
        Opened::after(Some(next), Some(OPEN + 2_250), OPEN + 1_000, secs, t0),
        next
    );

    // A Full glide runs on through the second reading: its frames keep coming and each moves.
    let band = FineBand {
        box_px: 20.0,
        period_ms: 2_000,
    };
    let glide = |landed: Instant| LiveFacts {
        landed: Some(landed),
        stale_after: STALE_FLOOR,
        motion: true,
        beat: true,
        tween: false,
        flow: Flow::of((band, FineBand::default()), opener, secs),
        needle: None,
        drawn: None,
        steam: false,
    };
    let (by_opener, by_second) = (glide(opener.landed), glide(second));
    let wall = |t: Instant| {
        OPEN + 1_250 + 50 + i64::try_from((t - opener.landed).as_millis()).unwrap_or(0)
    };
    let at = second + FRAME;
    assert_eq!(
        by_second.live(at, wall(at)).flow,
        by_opener.live(at, wall(at)).flow,
        "the second reading draws the opener's glide"
    );
    assert!(by_second.live(second, wall(second)).flow.is_some());
    // CONTROL: without the carry the second reading's own step is nothing, and the band stood.
    let own = Opened::after(None, Some(OPEN + 1_250), OPEN + 1_750, secs, second);
    assert_eq!(Flow::of((band, FineBand::default()), own, secs), Flow::None);

    // A Folded slide runs on through it too, on the second reading's grid.
    let slide = |landed: Instant| LiveFacts {
        flow: Flow::of((FineBand::default(), band), opener, secs),
        ..glide(landed)
    };
    let sliding = slide(second);
    assert_eq!(
        ran(sliding.live(second, OPEN)),
        Some(0.75),
        "half a second in, the slide's third frame"
    );
    assert_eq!(ran(sliding.live(second + FRAME, OPEN)), None);
    assert_eq!(sliding.next_change(second, OPEN), Some(second + FRAME));
}

/// How the strips move in [`frames_over`]: a Full glide or a Folded slide of a fine band whose
/// boxes are `px` wide on 2 s, as its card's layout says ([`Flow::of`]).
#[derive(Clone, Copy, Debug)]
enum Moves {
    Glide(f64),
    Slide(f64),
}

/// A needle each reading swings across most of the dial, so it is in motion on every grid frame
/// (Full's dial at 2×, at a 60 Hz window's effect interval).
fn swinging(landed: Instant, r: u32) -> (Needle, View) {
    let view = View {
        px_per_deg: crate::stats_gauge::tip_px_per_deg(crate::stats_gauge::R_FACE, 2.0),
        interval: Duration::from_micros(33_333),
    };
    let (from, to) = if r.is_multiple_of(2) {
        (-120.0, 120.0)
    } else {
        (120.0, -120.0)
    };
    let rest = Needle::at_rest(landed, (from, 0.0));
    let n = Needle::retarget(Some(rest), landed, Some((to, 0.0)), view.px_per_deg);
    (n.expect("a needle"), view)
}

/// Frames the live edge's grid asks of one window over `secs` seconds of readings every
/// `cadence`, its strips moving as `moves` says (and, with `needle`, its dial swinging at every
/// reading; with `steam`, its hot dial steaming), counting each reading's own frame, and the
/// frames that would have drawn nothing new.
fn frames_over(
    cadence: Duration,
    moves: Moves,
    (beat, needle, steam): (bool, bool, bool),
    secs: u32,
) -> (usize, Vec<String>) {
    let t0 = Instant::now();
    let ms0 = OPEN + 3_456;
    let readings =
        u32::try_from(Duration::from_secs(u64::from(secs)).as_nanos() / cadence.as_nanos())
            .unwrap_or(0);
    let band = |px| FineBand {
        box_px: px,
        period_ms: 2_000,
    };
    let bands = match moves {
        Moves::Glide(px) => (band(px), FineBand::default()),
        Moves::Slide(px) => (FineBand::default(), band(px)),
    };
    let (mut frames, mut idle, mut was) = (0, Vec::new(), None);
    for r in 0..readings {
        let landed = t0 + cadence * r;
        let at_ms = wall(t0, ms0, landed);
        let prev_ms = r.checked_sub(1).map(|p| wall(t0, ms0, t0 + cadence * p));
        let opened = Opened::after(was, prev_ms, at_ms, SlotGrid::SECONDS, landed);
        was = Some(opened);
        let flow = Flow::of(bands, opened, SlotGrid::SECONDS);
        let f = LiveFacts {
            landed: Some(landed),
            stale_after: stale_after(cadence, None),
            motion: true,
            beat,
            tween: false,
            needle: needle.then(|| swinging(landed, r)),
            drawn: None,
            flow,
            steam,
        };
        frames += 1;
        let until = landed + cadence;
        let mut now = landed;
        while let Some(next) = f.next_change(now, wall(t0, ms0, now)) {
            if next >= until {
                break;
            }
            if f.grid(now, wall(t0, ms0, now)) == f.grid(next, wall(t0, ms0, next)) {
                idle.push(format!("reading {r}: {:?}", next - landed));
            }
            frames += 1;
            now = next;
        }
    }
    (frames, idle)
}

/// The cost bound (§6): a focused, live deck asks at most four frames a second on the grid,
/// whatever its strips' speed and the readings' cadence, and none that draws nothing new; with
/// the beat it still moves on most of them, so it looks live. A needle in motion asks its own
/// frames (`the_needle_asks_its_own_frames_and_none_once_settled`) and none of the grid's. A hot
/// dial's steam (`docs/DESIGN-stats-deck-owner-2026-10-26.md` §5) asks none the beat does not,
/// and without the beat still at most four a second.
#[test]
fn the_live_edge_asks_at_most_four_frames_a_second() {
    let ms = Duration::from_millis;
    for cadence in [ms(1_000), ms(500), ms(2_000)] {
        for moves in [
            Moves::Glide(2.0),
            Moves::Glide(4.6),
            Moves::Glide(8.3),
            Moves::Glide(16.6),
            Moves::Glide(80.0),
            Moves::Slide(3.0),
            Moves::Slide(4.0),
            Moves::Slide(8.0),
            Moves::Slide(16.0),
        ] {
            for beat in [true, false] {
                let secs = 30;
                let (frames, idle) = frames_over(cadence, moves, (beat, false, false), secs);
                let per_s = frames as f64 / f64::from(secs);
                let what = format!("{cadence:?} readings, {moves:?}, beat {beat}");
                assert!(per_s <= 4.0, "{what}: {per_s} frames a second");
                assert!(idle.is_empty(), "{what}: {idle:?}");
                let swung = frames_over(cadence, moves, (beat, true, false), secs);
                assert_eq!(
                    swung,
                    (frames, idle.clone()),
                    "{what}: a swinging needle moves the grid"
                );
                let (steamed, steam_idle) = frames_over(cadence, moves, (beat, false, true), secs);
                let steam_per_s = steamed as f64 / f64::from(secs);
                assert!(
                    steam_per_s <= 4.0,
                    "{what}, steaming: {steam_per_s} frames a second"
                );
                assert!(steam_idle.is_empty(), "{what}, steaming: {steam_idle:?}");
                if beat {
                    assert_eq!(
                        steamed, frames,
                        "{what}: the steam asks no frame the beat does not"
                    );
                }
                if beat && cadence == ms(1_000) {
                    assert!(per_s >= 3.0, "CONTROL {what}: the beat draws ({per_s})");
                }
            }
        }
    }
}

/// The needle's own bound (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §4.3): over half a
/// minute of readings at the deck's and the lens's cadences, each retargeted from where the
/// needle stands, every frame it asks draws a needle the tip of which stands a whole device pixel
/// from the last, each reading's episode asks at most its settle time over the window's interval,
/// and a settled needle asks for nothing. A quiet machine, whose readings drift under a pixel at
/// the tip, asks no needle frame at all; without motion or once readings stop, none either.
#[test]
fn the_needle_asks_its_own_frames_and_none_once_settled() {
    let ms = Duration::from_millis;
    let px_per_deg = crate::stats_gauge::tip_px_per_deg(crate::stats_gauge::R_FACE, 2.0);
    let view = View {
        px_per_deg,
        interval: Duration::from_micros(16_667),
    };
    // A load that climbs, holds, plunges and wanders, as busy % then the redline, degrees.
    let busy = |i: u32| {
        let v = [0.1, 0.35, 0.8, 0.82, 1.4, 0.6, 0.62, 0.3, 0.05, 1.9][i as usize % 10];
        crate::stats_gauge::tip_deg(v)
    };
    let quiet = |i: u32| 10.0 + 0.4 / px_per_deg * f64::from(i % 3);
    for (cadence, target, wants_frames) in [
        (ms(1_000), &busy as &dyn Fn(u32) -> f64, true),
        (ms(500), &busy, true),
        (ms(1_000), &quiet, false),
    ] {
        let t0 = Instant::now();
        let mut needle: Option<Needle> = None;
        let mut asked = 0;
        for r in 0..30_u32 {
            let landed = t0 + cadence * r;
            needle = Needle::retarget(needle, landed, Some((target(r), 0.0)), px_per_deg);
            let f = LiveFacts {
                landed: Some(landed),
                stale_after: stale_after(cadence, None),
                motion: true,
                beat: true,
                tween: false,
                flow: Flow::None,
                needle: needle.map(|n| (n, view)),
                drawn: None,
                steam: false,
            };
            let at = format!("{cadence:?}, reading {r}");
            let (mut now, mut episode) = (landed, 0_u32);
            let mut last = None;
            while let Some(next) = f.next_needle(now) {
                if next >= landed + cadence {
                    break;
                }
                let (was, drawn) = (f.live(now, OPEN), f.live(next, OPEN));
                let deg = |l: &DeckLive| {
                    l.needle.map_or(target(r), |(a, _)| {
                        f64::from(a) / crate::stats_needle::ANGLE_PER_DEG
                    })
                };
                let px = (deg(&drawn) - deg(&was)).abs() * px_per_deg;
                assert!(
                    px >= 1.0 - px_per_deg / crate::stats_needle::ANGLE_PER_DEG,
                    "{at}: a needle frame at {:?} moves the tip {px} px",
                    next - landed
                );
                if let Some(last) = last {
                    assert!(next - last >= view.interval, "{at}: {:?}", next - last);
                }
                last = Some(next);
                now = next;
                episode += 1;
                assert!(episode < 200, "{at}: the needle never settles");
            }
            // Settled before the next reading: it draws the reading and asks nothing.
            if f.next_needle(now).is_none() {
                assert_eq!(
                    f.live(now, OPEN).needle,
                    None,
                    "{at}: settled on the reading"
                );
            }
            let bound = (0.8 / view.interval.as_secs_f64()).ceil() as u32 + 1;
            assert!(
                episode <= bound,
                "{at}: {episode} needle frames, at most {bound}"
            );
            asked += episode;
        }
        assert_eq!(
            asked > 0,
            wants_frames,
            "{cadence:?}: {asked} needle frames"
        );
    }
    // Without motion the needle draws the reading and asks for nothing; once readings stop, the
    // grey draws it at rest.
    let t0 = Instant::now();
    let rest = Needle::at_rest(t0, (-120.0, 0.0));
    let n = Needle::retarget(Some(rest), t0, Some((120.0, 0.0)), px_per_deg);
    let f = LiveFacts {
        needle: n.map(|n| (n, view)),
        ..facts(t0, true, true)
    };
    assert!(f.live(t0, OPEN).needle.is_some(), "CONTROL: it moves");
    assert!(f.next_needle(t0).is_some(), "CONTROL: it asks");
    assert_eq!(
        f.next_change(t0, OPEN),
        facts(t0, true, true).next_change(t0, OPEN)
    );
    let still = LiveFacts { motion: false, ..f };
    assert_eq!(still.live(t0, OPEN), DeckLive::STILL);
    assert_eq!(still.next_needle(t0), None);
    let stale = t0 + f.stale_after;
    assert_eq!(f.live(stale, OPEN), DeckLive::STALE);
    assert_eq!(f.next_needle(stale), None);
    let late = LiveFacts {
        stale_after: ms(40),
        ..f
    };
    assert!(
        late.next_needle(t0 + ms(39))
            .is_none_or(|at| at < t0 + ms(40)),
        "no needle frame past the grey"
    );
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
        flow: Some(FlowAt {
            opened_ms: 1_000,
            ran: 0.3125,
        }),
        ..still
    };
    let slide = DeckLive {
        flow: Some(FlowAt {
            opened_ms: 1_000,
            ran: 0.5,
        }),
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
    totals: &[],
};

fn head_of(form: DeckForm, w: f64, m: &FaceMetrics) -> Option<(HeaderPlan, Strip)> {
    match form {
        DeckForm::Full => {
            let p = plan::plan_full(w, m);
            (!p.ground_only).then(|| (plan::header_full(&p, TEXT, None, m), p.strip))
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

/// The seconds a scale's words name, read as a person reads them: `1h ago`, `–15m`, `–10s`,
/// `10 min ago`, `–2.5 min`, `–80 s`; `None` for anything else.
fn read_age(words: &str) -> Option<f64> {
    let words = words.strip_suffix(" ago").unwrap_or(words);
    let words = words.strip_prefix('\u{2013}').unwrap_or(words);
    if let Some((n, unit)) = words.split_once(' ') {
        let n: f64 = n.parse().ok()?;
        return match unit {
            "h" => Some(n * 3_600.0),
            "min" | "m" => Some(n * 60.0),
            "s" => Some(n),
            _ => None,
        };
    }
    if let Some(n) = words.strip_suffix('h') {
        n.parse::<f64>().ok().map(|v| v * 3_600.0)
    } else if let Some(n) = words.strip_suffix('m') {
        n.parse::<f64>().ok().map(|v| v * 60.0)
    } else if let Some(n) = words.strip_suffix('s') {
        n.parse::<f64>().ok()
    } else {
        None
    }
}

/// How far back the strip reaches as drawn, seconds: its multi-scale horizon.
fn drawn_span(strip: Strip) -> f64 {
    f64::from(strip.span_s())
}

#[test]
fn the_scales_words_read_as_their_ages() {
    // NEGATIVE CONTROL for `read_age`: it reads what a person reads, and nothing else.
    assert_eq!(read_age("1h ago"), Some(3_600.0));
    assert_eq!(read_age("10 min ago"), Some(600.0));
    assert_eq!(read_age("\u{2013}15m"), Some(900.0));
    assert_eq!(read_age("\u{2013}2.5 min"), Some(150.0));
    assert_eq!(read_age("\u{2013}80 s"), Some(80.0));
    assert_eq!(read_age("\u{2013}10s"), Some(10.0));
    assert_eq!(read_age("now"), None);
    assert_eq!(read_age("10 minutes"), None);
    for age in [10, 20, 30, 40, 60, 80, 120, 150, 300, 600, 900, 3_600] {
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
    // The label stands over the strip's right (old) end, right-aligned to it, or beside it:
    // starting STRIP_LEAD past that end, which a tick then marks.
    let hi = if (time.x + time_w - strip.end()).abs() < EPS {
        if head.time_tick.is_some() {
            why.push("a tick at the strip's end under a label over it".into());
        }
        time.x - 6.0
    } else {
        if (time.x - (strip.end() + plan::STRIP_LEAD)).abs() > EPS {
            why.push(format!(
                "the label beside the strip spans {}..{}",
                time.x,
                time.x + time_w
            ));
        }
        if head.time_tick != Some(strip.end()) {
            why.push(format!("the strip's end tick is {:?}", head.time_tick));
        }
        strip.end()
    };
    if head.ruler.is_empty()
        && let Some(could) = scale_could_go(head, strip, m)
    {
        why.push(format!(
            "no tick says how far back the strip reaches: {could}"
        ));
    }
    // The now marker's room at the strip's left end stays clear.
    let lo = strip.x + MARKER_ROOM;
    for (i, t) in head.ruler.iter().enumerate() {
        let want = strip
            .x_at_age_ms(i64::from(t.age_s) * 1_000)
            .unwrap_or(strip.x);
        if (t.x - want).abs() > 1e-4 {
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
/// tick's words fit under their middle half before either label over the strip's old end, and
/// with the label beside the strip every tick that fits there costs the key "over max" by rule
/// K's own search ([`plan::place_key`]), which the scale never does.
fn scale_could_go(head: &HeaderPlan, strip: Strip, m: &FaceMetrics) -> Option<String> {
    let lo = strip.x + MARKER_ROOM;
    let tick = |age: u32| -> Option<(f64, f64)> {
        let x = strip.x_at_age_ms(i64::from(age) * 1_000)?;
        Some((x, header_pt_words(m, &plan::tick_words(age))))
    };
    let ages = plan::tick_ages(strip);
    let labels = plan::time_words(strip);
    for label in &labels {
        let hi = strip.end() - header_pt_words(m, label) - 6.0;
        for &age in &ages {
            let Some((x, w)) = tick(age) else { continue };
            if lo.max(x - 0.75 * w) <= (hi - w).min(x - 0.25 * w) {
                return Some(format!("{} fits before {label:?}", plan::tick_words(age)));
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
    let lead = strip.end() + plan::STRIP_LEAD;
    let is_lead = |s: &plan::KeySpan| !s.strip && s.x0 >= strip.end() - EPS && s.x0 <= lead + EPS;
    let on_strip =
        |s: &plan::KeySpan| s.strip && s.x0 >= strip.x - EPS && s.x1 <= strip.end() + EPS;
    let free = head.key.spans.iter().copied().find(is_lead)?;
    for label in &labels {
        let lw = header_pt_words(m, label);
        if free.x1 < lead + lw {
            continue;
        }
        for &age in &ages {
            let Some((tx, w)) = tick(age) else { continue };
            let (least, most) = (lo.max(tx - 0.75 * w), (strip.end() - w).min(tx - 0.25 * w));
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
                (lead + lw + plan::KEY_GAP, free.x1, false),
                (lo, left - plan::KEY_GAP, true),
                (left + w + plan::KEY_GAP, lead - plan::KEY_GAP, true),
            ] {
                if x1 > x0 {
                    spans.push(plan::KeySpan { x0, x1, strip });
                }
            }
            let trial = kept(spans);
            if now
                .iter()
                .all(|item| *item != plan::KeyItem::Over || trial.contains(item))
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
/// words, clear of the now marker at the strip's left end. With now at the left every deck
/// width's strip holds the label over its old end and a tick, so none needs the label beside
/// it (that placement is pinned on a strip too short for both, below); the scale is missing
/// only where every placement would cost the key "over max" ([`scale_could_go`]),
/// which on the Mac's face (Helvetica Neue's metrics) is nowhere.
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
        beside < drew / 2,
        "{beside} of {drew} labels stand beside their strip"
    );
    assert_eq!(bare_hn, 0, "headers without a scale on the Mac's face");
    assert!(failed.is_empty(), "{}", failed.join("\n"));

    // A strip too short for its label over it and a tick (41 pt of the 1-minute scale) sets the
    // label beside it, past its old end, where the scope key leaves a free span; with none
    // there the label stands over it alone.
    for (face, m) in &faces {
        let strip = Strip::folded(100.0, 14, 3.0);
        let free = plan::KeySpan {
            x0: strip.end() + plan::STRIP_LEAD,
            x1: strip.end() + 160.0,
            strip: false,
        };
        let head = plan::header_of_strip(strip, &[free], m);
        assert_eq!(
            head.time_tick,
            Some(strip.end()),
            "{face}: {:?}",
            head.labels
        );
        assert!(!head.ruler.is_empty(), "{face}: a scale beside it");
        let faults = ruler_faults(&head, strip, m);
        assert!(faults.is_empty(), "{face}: {}", faults.join("\n"));
        let alone = plan::header_of_strip(strip, &[], m);
        assert_eq!(
            (alone.time_tick, alone.ruler.len()),
            (None, 0),
            "{face}: no room beside it"
        );
        assert!(ruler_faults(&alone, strip, m).is_empty(), "{face}");
    }
}

/// The scale at the widths the mockups draw and the owner's default window, and at each
/// strip's period: the label over the strip's old end, and the ticks, by age. The strip is
/// each row's widest element (the owner, 2026-10-26: "the time series is too short"), so from
/// Full 550 and Folded 1119 it reads six hours (Full 550's narrow header keeps its key's three
/// items and the hour's tick); Folded 800's and 600's narrower blocks read 4 hours, the label
/// taking the room the coarser ticks would.
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
            (10, "6h ago".into(), vec![10, 60, 600, 3600]),
            (10, "6h ago".into(), vec![10, 60, 600, 3600]),
            (10, "6h ago".into(), vec![10, 60, 600, 3600]),
            (10, "6h ago".into(), vec![10, 60, 600, 3600]),
            (10, "6h ago".into(), vec![3600]),
            (1, "6h ago".into(), vec![10, 60, 600, 3600]),
            (1, "4h ago".into(), vec![60, 600]),
            (1, "4h ago".into(), vec![60, 600]),
        ]
    );
    // The owner's default window (Folded at about 600 pt): its 4h multi-scale strip carries the
    // "–1m" tick past the fine real-time region, with the tick under the middle half of its words.
    let (head, _strip) = head_of(DeckForm::Folded, 600.0, &m).expect("the table is drawn");
    let mid = &head.ruler[0];
    let w = header_pt_words(&m, &mid.text);
    assert!(
        (mid.left + w / 2.0 - mid.x).abs() <= w / 4.0 + EPS,
        "{mid:?}"
    );
}

/// A headless App whose one focused window draws the v2 deck, holding a reading that landed at
/// `landed`, a second after the reading before it.
#[cfg(unix)]
fn live_app(landed: Instant) -> (crate::App, crate::WindowId) {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("deck".into());
    let snap = crate::stats_deck::paint_tests::scene(crate::stats_deck::paint_tests::Scene::Calm);
    app.vitals.landed_open = Some(Opened {
        at_ms: snap.at_unix_ms,
        ms: 1_000,
        landed,
    });
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
    assert!(live.fresh && live.flow.is_none());
    assert_eq!(live.beat, beat_phase(0));
    // A Folded strip of 12 px boxes on 2 s, after a reading that opened a second: its fine band
    // slides 6 px on the beat's frames.
    let at_ms = app.vitals.snapshot.as_ref().expect("a snapshot").at_unix_ms;
    let fine = |box_px| FineBand {
        box_px,
        period_ms: 2_000,
    };
    let ws = app.windows.get_mut(&wid).expect("the window");
    ws.deck_glide.set((FineBand::default(), fine(12.0)));
    let facts = app.live_facts(wid).expect("the deck draws");
    assert_eq!(
        facts.flow,
        Flow::Slide {
            opened_ms: 1_000,
            landed: t0,
            frames: 4
        }
    );
    let live = facts.live(t0, OPEN);
    assert!(live.fresh && ran(live) == Some(0.25));
    // The band moves for the reading the host says opened a slot (`Opened::after`): one that
    // opened none moves nothing.
    app.vitals.landed_open = Some(Opened {
        at_ms,
        ms: 0,
        landed: t0,
    });
    let facts = app.live_facts(wid).expect("the deck draws");
    assert_eq!(facts.flow, Flow::None);
    assert_eq!(facts.live(t0, OPEN).flow, None);
    app.vitals.landed_open = None;
    assert_eq!(
        app.live_facts(wid).map(|f| f.flow),
        Some(Flow::None),
        "nothing landed that opened a slot"
    );
    app.vitals.landed_open = Some(Opened {
        at_ms,
        ms: 1_000,
        landed: t0,
    });
    // A Full strip glides on the wall clock instead, from the reading for a slot.
    let ws = app.windows.get_mut(&wid).expect("the window");
    ws.deck_glide.set((fine(8.0), FineBand::default()));
    let facts = app.live_facts(wid).expect("the deck draws");
    assert!(
        matches!(facts.flow, Flow::Glide { from_ms, end_ms, .. } if from_ms == at_ms && end_ms == at_ms + 1_000),
        "{facts:?}"
    );
    let live = facts.live(t0, at_ms + 100);
    assert!(live.fresh && live.flow.is_some());
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

/// Lays `wid`'s deck out to its form, so its card is the height that form draws.
#[cfg(unix)]
fn fit_deck(app: &mut crate::App, wid: crate::WindowId) {
    let (_, cell) = app.win_cell_size(wid);
    let ws = app.windows.get_mut(&wid).expect("the window");
    ws.metrics.head = crate::stats_deck::reserved_deck_px(
        crate::stats_deck::StatsBars::Deck,
        ws.deck_form,
        ws.scale,
        cell,
    );
}

/// A reading adopted at `at` whose CPU and GPU stand at `share` of their capacity, the needle
/// retargeted to it: the frames its episode owes the windows.
#[cfg(unix)]
fn adopt(app: &mut crate::App, share: f64, at: Instant) -> u64 {
    let mut snap = (**app.vitals.snapshot.as_ref().expect("a reading")).clone();
    snap.seq += 1;
    for res in [aterm_vitals::Resource::Cpu, aterm_vitals::Resource::Gpu] {
        let row = &mut snap.resources[res.index()];
        row.now = row.capacity.map(|cap| cap * share);
    }
    app.vitals.snapshot = Some(std::sync::Arc::new(snap));
    app.vitals.landed_at = Some(at);
    app.retarget_needle(at)
}

/// [`adopt`]'s needle.
#[cfg(unix)]
fn busier(app: &mut crate::App, share: f64, at: Instant) -> crate::stats_needle::Needle {
    adopt(app, share, at);
    app.vitals.needle.get().expect("the reading's needle")
}

/// Follows the loop's needle deadlines from `from` while they come before `until`, drawing each
/// of `wids` at each as the splice does: the deadlines it took.
#[cfg(unix)]
fn needle_deadlines(
    app: &crate::App,
    wids: &[crate::WindowId],
    from: Instant,
    until: Instant,
) -> u64 {
    for &wid in wids {
        draw_needle_at(app, wid, from);
    }
    let (mut at, mut fired) = (from, 0);
    while let Some(next) = app.stats_needle_deadline(at).filter(|&next| next < until) {
        at = next;
        for &wid in wids {
            draw_needle_at(app, wid, at);
        }
        fired += 1;
        assert!(fired <= 1_000, "{fired} deadlines and counting");
    }
    fired
}

/// `live_app` with the deck laid out wide in `wids` (window 0 and any it adds), each focused.
#[cfg(unix)]
fn needle_app(t0: Instant, wids: &[crate::WindowId]) -> crate::App {
    let (mut app, _) = live_app(t0);
    app.vitals.landed_period = Some(Duration::from_secs(3600));
    for &wid in wids {
        if !app.windows.contains_key(&wid) {
            app.install_window_state(wid, crate::stub_session(90 + wid.0), 24, 80);
        }
        let ws = app.windows.get_mut(&wid).expect("the window");
        ws.cols = 160;
        ws.focused = true;
        fit_deck(&mut app, wid);
        let _ = app.stats_deck_fp(wid);
    }
    app
}

/// A reading's episode owes its window exactly the needle deadlines the loop then fires for it
/// (`needle_frames_owed`, by which the `measure` spin row bounds the `stats_needle` owner):
/// following the loop's own deadlines from the reading to rest, drawing each frame as the splice
/// does, fires that many, and so does a reading that lands mid-sweep, walked from what the window
/// drew. Two windows that draw the dial are owed a window's frames each, and the loop fires
/// no more than both. The first reading, the same reading again and a reading no window animates
/// owe none.
#[cfg(unix)]
#[test]
fn a_readings_owed_frames_are_the_needle_deadlines_its_windows_fire() {
    let _attached = crate::stats_deck::tests::attach_stats();
    let one = [crate::WindowId(0)];
    let t0 = Instant::now();
    let far = t0 + Duration::from_secs(3600);
    let mut app = needle_app(t0, &one);
    assert_eq!(app.retarget_needle(t0), 0, "the first reading rests");
    let t1 = t0 + Duration::from_secs(1);
    let owed = adopt(&mut app, 0.95, t1);
    assert!(owed > 0, "CONTROL: a swing owes frames");
    assert_eq!(needle_deadlines(&app, &one, t1, far), owed, "owed as fired");
    let t2 = t1 + Duration::from_secs(1);
    let back = adopt(&mut app, 0.10, t2);
    let mid = t2 + Duration::from_millis(150);
    let drawn = needle_deadlines(&app, &one, t2, mid);
    assert!(
        drawn > 0 && drawn < back,
        "CONTROL: {drawn} of {back} drawn before the next reading"
    );
    let again = adopt(&mut app, 0.95, mid);
    assert!(again > 0, "CONTROL: it sweeps on from mid-sweep");
    assert_eq!(
        needle_deadlines(&app, &one, mid, far),
        again,
        "owed as fired, mid-sweep"
    );
    let t3 = mid + Duration::from_secs(1);
    assert_eq!(adopt(&mut app, 0.95, t3), 0, "the same reading again");
    app.config.motion = Some("reduced".into());
    assert_eq!(
        adopt(&mut app, 0.10, t3 + Duration::from_secs(1)),
        0,
        "no window animates the dial"
    );
    let two = [crate::WindowId(0), crate::WindowId(1)];
    let mut both = needle_app(t0, &two);
    assert_eq!(both.retarget_needle(t0), 0);
    assert_eq!(
        adopt(&mut both, 0.95, t1),
        2 * owed,
        "a window's frames each"
    );
    let fired = needle_deadlines(&both, &two, t1, far);
    assert!(
        (owed..=2 * owed).contains(&fired),
        "{fired} deadlines for two windows owed {owed} each"
    );
}

/// One needle for the app, retargeted where a reading is adopted: the first reading rests it on
/// the dial, a busier one sweeps it on its own deadline (`stats_needle`, serious mode or not) from
/// where it stood; a window without motion draws the reading and asks nothing, and a reading no
/// window animates rests it; a layout that adds, drops or resizes the dial rests it too.
#[cfg(unix)]
#[test]
fn the_app_retargets_one_needle_and_rests_it_where_it_must() {
    use crate::stats_needle::Needle;
    let _attached = crate::stats_deck::tests::attach_stats();
    let t0 = Instant::now();
    let (mut app, wid) = live_app(t0);
    app.vitals.landed_period = Some(Duration::from_secs(3600));
    app.windows.get_mut(&wid).expect("the window").cols = 160;
    fit_deck(&mut app, wid);
    let _ = app.stats_deck_fp(wid);
    let px = app.windows[&wid].deck_needle_px.get();
    assert!(px > 0.0, "CONTROL: the deck draws a dial");
    app.retarget_needle(t0);
    let first = app.vitals.needle.get().expect("the reading's needle");
    assert_eq!(
        first,
        Needle::at_rest(t0, first.target()),
        "the first reading rests"
    );
    assert_eq!(app.stats_needle_deadline(t0), None);
    // A busier reading sweeps it from where it stood, on its own deadline.
    let t1 = t0 + Duration::from_secs(1);
    let hot = busier(&mut app, 0.95, t1);
    assert!(
        (hot.target().0 - first.target().0).abs() > 10.0,
        "CONTROL: {first:?} then {hot:?}"
    );
    assert_eq!((hot.tip.x0, hot.tip.v0), (first.tip.target, 0.0));
    let next = app
        .stats_needle_deadline(t1)
        .expect("the needle asks a frame");
    let interval = app.windows[&wid].effect_present_interval();
    assert_eq!(
        next,
        t1 + interval,
        "a large swing runs at the window's rate"
    );
    let facts = app.live_facts(wid).expect("the deck draws");
    assert!(facts.live(t1, OPEN).needle.is_some());
    assert_eq!(
        app.stats_live_deadline(t1),
        facts.next_change(t1, now_unix_ms()),
        "CONTROL: the grid keeps its own deadline"
    );
    app.serious_mode = true;
    assert_eq!(
        app.stats_needle_deadline(t1),
        Some(next),
        "serious mode keeps the needle"
    );
    app.serious_mode = false;
    // Without motion the window draws the reading and asks nothing, and a reading rests it.
    app.config.motion = Some("reduced".into());
    assert_eq!(app.stats_needle_deadline(t1), None);
    let reduced = app.live_facts(wid).expect("the deck draws");
    assert_eq!(reduced.live(t1, OPEN).needle, None);
    let t2 = t1 + Duration::from_secs(1);
    let calm = busier(&mut app, 0.10, t2);
    assert_eq!(
        calm,
        Needle::at_rest(t2, calm.target()),
        "no window animates: at rest"
    );
    app.config.motion = None;
    let t3 = t2 + Duration::from_secs(1);
    let swing = busier(&mut app, 0.95, t3);
    assert!(
        app.stats_needle_deadline(t3).is_some(),
        "CONTROL: it sweeps"
    );
    // The other form's dial is another size: the needle rests on its reading at once.
    let ws = app.windows.get_mut(&wid).expect("the window");
    ws.deck_form = match ws.deck_form {
        DeckForm::Full => DeckForm::Folded,
        DeckForm::Folded => DeckForm::Full,
    };
    fit_deck(&mut app, wid);
    let _ = app.stats_deck_fp(wid);
    let resized = app.windows[&wid].deck_needle_px.get();
    assert!(
        resized > 0.0 && resized != px,
        "CONTROL: {px} then {resized}"
    );
    let rested = app.vitals.needle.get().expect("the needle");
    assert_eq!(rested.target(), swing.target());
    assert_eq!((rested.tip.x0, rested.tip.v0), (swing.target().0, 0.0));
    assert_eq!(
        app.stats_needle_deadline(t3),
        None,
        "a resized dial draws the reading"
    );
}

/// What window `wid` draws of the needle at `at`, kept as what it drew as the splice keeps it
/// (`WindowState::deck_needle_drawn`): its angle, degrees, `None` for the reading itself.
#[cfg(unix)]
fn draw_needle_at(app: &crate::App, wid: crate::WindowId, at: Instant) -> Option<f64> {
    let facts = app.live_facts(wid).expect("the deck draws");
    let needle = facts.live(at, OPEN).needle;
    app.windows[&wid].deck_needle_drawn.set(facts.needle_at(at));
    needle.map(|(a, _)| f64::from(a) / crate::stats_needle::ANGLE_PER_DEG)
}

/// A window never draws the needle back where it already stood
/// (`docs/DESIGN-stats-deck-now-left-2026-10-26.md` §4.2). One whose motion goes mid-sweep
/// (`motion = "reduced"`, the focus moving to another window) draws the reading, and keeps it
/// when its motion comes back before the next reading; one whose effect interval changes
/// mid-sweep moves the needle on from the frame it drew. The splice keeps what each layout drew.
/// A window that kept nothing of what it drew would draw the needle back mid-sweep (the CONTROL).
#[cfg(unix)]
#[test]
fn a_window_never_draws_the_needle_back_where_it_stood() {
    let _attached = crate::stats_deck::tests::attach_stats();
    let t0 = Instant::now();
    let (mut app, wid) = live_app(t0);
    app.vitals.landed_period = Some(Duration::from_secs(3600));
    app.windows.get_mut(&wid).expect("the window").cols = 160;
    fit_deck(&mut app, wid);
    let _ = app.stats_deck_fp(wid);
    assert!(
        app.windows[&wid].deck_needle_px.get() > 0.0,
        "CONTROL: the deck draws a dial"
    );
    app.retarget_needle(t0);
    let mut at = t0;
    let mut share = 0.95;
    for lose in ["reduced", "unfocused"] {
        at += Duration::from_secs(1);
        let n = busier(&mut app, share, at);
        share = 1.05 - share;
        let target = n.target().0;
        assert!(
            (n.tip.x0 - target).abs() > 10.0,
            "CONTROL {lose}: it sweeps"
        );
        // The splice keeps what its layout drew of this sweep.
        let _ = app.stats_deck_fp(wid);
        assert_eq!(
            app.windows[&wid].deck_needle_drawn.get().map(|d| d.t0),
            Some(n.t0),
            "{lose}: the layout keeps what it drew"
        );
        let ms = |m: u32| at + Duration::from_millis(u64::from(m));
        let mut nearest = f64::MAX;
        for m in 0..80 {
            let drawn = draw_needle_at(&app, wid, ms(m)).map_or(0.0, |a| (a - target).abs());
            assert!(drawn <= nearest, "{lose}: back at {m} ms");
            nearest = drawn;
        }
        assert!(nearest > 1.0, "CONTROL {lose}: mid-sweep at 80 ms");
        let set_motion = |app: &mut crate::App, on: bool| match lose {
            "reduced" => app.config.motion = (!on).then(|| "reduced".into()),
            _ => app.windows.get_mut(&wid).expect("the window").focused = on,
        };
        set_motion(&mut app, false);
        assert_eq!(
            draw_needle_at(&app, wid, ms(90)),
            None,
            "{lose}: the reading"
        );
        set_motion(&mut app, true);
        let forgot = LiveFacts {
            drawn: None,
            ..app.live_facts(wid).expect("the deck draws")
        };
        assert!(
            forgot.live(ms(200), OPEN).needle.is_some(),
            "CONTROL {lose}: a window that kept nothing draws the needle back mid-sweep"
        );
        for m in (100..1000).step_by(10) {
            assert_eq!(
                draw_needle_at(&app, wid, ms(m)),
                None,
                "{lose}: motion back at {m} ms draws the reading still"
            );
            assert_eq!(app.stats_needle_deadline(ms(m)), None, "{lose}: {m} ms");
        }
    }
    // A sweep whose window's effect interval halves, then doubles back, mid-way.
    at += Duration::from_secs(1);
    let n = busier(&mut app, share, at);
    let target = n.target().0;
    let set_rate = |app: &mut crate::App, us: u64| {
        let ws = app.windows.get_mut(&wid).expect("the window");
        ws.frame_interval = Some(Duration::from_micros(us));
        ws.effect_present_interval()
    };
    let fast = set_rate(&mut app, 8_333);
    let mut nearest = f64::MAX;
    for m in 0..1500_u32 {
        if m == 55 {
            assert_ne!(
                set_rate(&mut app, 16_667),
                fast,
                "CONTROL: the interval moved"
            );
        }
        if m == 140 {
            set_rate(&mut app, 8_333);
        }
        let now = at + Duration::from_millis(u64::from(m));
        let drawn = draw_needle_at(&app, wid, now).map_or(0.0, |a| (a - target).abs());
        assert!(
            drawn <= nearest,
            "back at {m} ms: {drawn}° from the reading after {nearest}°"
        );
        nearest = drawn;
    }
    assert_eq!(nearest, 0.0, "the sweep ends on the reading");
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
        ws.metrics.head = crate::stats_deck::reserved_deck_px(
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
    // A reading that opened a slot a moment ago: its strips move on the wall clock.
    app.vitals.landed_open = Some(Opened {
        at_ms: now_unix_ms(),
        ms: 1_000,
        landed: Instant::now(),
    });
    let work = split(&mut app, &|app| app.vitals.landed_at = None);
    whole(&app, "a frame whose edge moved after its fingerprint");
    assert!(
        shows(&app).fresh && shows(&app).flow.is_some(),
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
    ws.metrics.head = crate::stats_deck::reserved_deck_px(
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

#[test]
fn tween_interpolates_exponentially_settles_cleanly_and_snaps_under_reduce_motion() {
    assert_eq!(exp_blend(Duration::ZERO, TWEEN_TAU), Some(0.0));
    let mut prev = 0.0_f32;
    for ms in [60, 120, 240, 480] {
        let cur = exp_blend(Duration::from_millis(ms), TWEEN_TAU).expect("still settling");
        assert!(
            cur >= prev && cur < TWEEN_SETTLE,
            "{ms} ms: {prev} -> {cur}"
        );
        prev = cur;
    }
    assert_eq!(exp_blend(Duration::from_secs(2), TWEEN_TAU), None);
    assert_eq!(lerp_opt(Some(0.2), Some(0.8), Some(0.5)), Some(0.5));
    assert_eq!(lerp_opt(Some(0.2), Some(0.8), None), Some(0.8));
    assert_eq!(lerp_opt(None, Some(0.8), Some(0.5)), Some(0.8));

    let phases: Vec<Option<f32>> = (0..=BEAT_FRAMES).map(tween_phase).collect();
    assert!(phases[..BEAT_FRAMES as usize].iter().all(Option::is_some));
    assert_eq!(phases[BEAT_FRAMES as usize], None);
    for pair in phases[..BEAT_FRAMES as usize].windows(2) {
        assert!(pair[0] < pair[1], "{phases:?}");
    }

    let t0 = Instant::now();
    let f = LiveFacts {
        landed: Some(t0),
        stale_after: Duration::from_secs(5),
        motion: true,
        beat: false,
        tween: true,
        needle: None,
        drawn: None,
        flow: Flow::None,
        steam: false,
    };
    let mut now = t0;
    let mut steps = Vec::new();
    while let Some(next) = f.next_change(now, wall(t0, OPEN, now)) {
        steps.push(next - t0);
        now = next;
    }
    assert_eq!(
        steps,
        [FRAME, FRAME * 2, FRAME * 3, f.stale_after],
        "tween runs its 3 frames and then schedules zero idle frames before stale_after"
    );
    let reduced = LiveFacts { motion: false, ..f };
    assert_eq!(reduced.live(t0, OPEN), DeckLive::STILL);
    assert_eq!(reduced.next_change(t0, OPEN), Some(t0 + f.stale_after));

    let blending = DeckLive {
        blend: Some(0.5),
        ..DeckLive::STILL
    };
    let beating = DeckLive {
        beat: Some(0.25),
        ..DeckLive::STILL
    };
    assert!(LiveLayer::Values.moved(&DeckLive::STILL, &blending));
    assert!(LiveLayer::NowBox.moved(&DeckLive::STILL, &blending));
    assert!(!LiveLayer::Glide.moved(&DeckLive::STILL, &blending));
    assert!(!LiveLayer::NowBox.moved(&DeckLive::STILL, &beating));
}

/// The steam's frames (`docs/DESIGN-stats-deck-owner-2026-10-26.md` §5): a hot dial's steam
/// clock moves on the beat's own frames, the reading's and the next [`BEAT_FRAMES`], 250 ms on a
/// frame, and holds after them; the next reading carries it on from the moment it lands, so a
/// puff rises on across readings; without motion, once readings stop or while the dial is cool
/// there is no steam, and a dial that steams with nothing else moving asks only those frames.
#[test]
fn the_steam_rides_the_beats_frames_and_holds_between_them() {
    let t0 = Instant::now();
    let ms0 = OPEN + 1_234;
    let hot = LiveFacts {
        steam: true,
        flow: Flow::None,
        ..facts(t0, true, false)
    };
    let clock: Vec<Option<i64>> = (0..8)
        .map(|k| hot.live(t0 + FRAME * k, ms0).steam)
        .collect();
    let first = clock[0].expect("a hot dial steams");
    for (k, at) in clock.iter().enumerate() {
        let held = i64::try_from(k)
            .unwrap_or(i64::MAX)
            .min(i64::from(BEAT_FRAMES));
        assert_eq!(*at, Some(first + 250 * held), "frame {k}: {clock:?}");
    }
    let next = LiveFacts {
        landed: Some(t0 + Duration::from_secs(1)),
        ..hot
    };
    assert_eq!(
        next.live(t0 + Duration::from_secs(1), ms0 + 1_000).steam,
        Some(first + 1_000),
        "the next reading carries the steam on"
    );
    // Its frames: the beat's three after the reading's own, then none until the grey.
    let mut asks = Vec::new();
    let mut now = t0;
    while let Some(at) = hot.next_change(now, ms0) {
        asks.push(at - t0);
        now = at;
    }
    assert_eq!(
        asks,
        [FRAME, FRAME * 2, FRAME * 3, hot.stale_after],
        "the steam's frames, then the grey"
    );
    assert_eq!(
        hot.live(t0 + hot.stale_after, ms0),
        DeckLive::STALE,
        "once readings stop, no steam"
    );
    for (f, said) in [
        (
            LiveFacts {
                steam: false,
                ..hot
            },
            "a cool dial",
        ),
        (
            LiveFacts {
                motion: false,
                ..hot
            },
            "without motion",
        ),
    ] {
        assert_eq!(f.live(t0 + FRAME, ms0).steam, None, "{said}");
        assert_eq!(
            f.next_change(t0, ms0),
            Some(t0 + f.stale_after),
            "{said}: only the grey is scheduled"
        );
    }
    // One clock for every window, from a fixed moment: a second on is 1000 ms on.
    let a = crate::stats_live::steam_ms(t0);
    assert_eq!(
        crate::stats_live::steam_ms(t0 + Duration::from_secs(1)),
        a + 1_000
    );
    assert!(crate::stats_live::steam_ms(t0 + Duration::from_millis(1)) >= a);
}

/// The window's steam: it rides the grid only while the deck's last layout laid a hot dial's
/// steam rect, outside serious mode, with the card presented alone and with motion.
#[cfg(unix)]
#[test]
fn the_app_steams_only_a_hot_dial_presented_alone_where_motion_is_allowed() {
    let _attached = crate::stats_deck::tests::attach_stats();
    let t0 = Instant::now();
    let (mut app, wid) = live_app(t0);
    let steams = |app: &crate::App| app.live_facts(wid).is_some_and(|f| f.steam);
    assert!(!steams(&app), "before a layout laid a steaming dial, none");
    let slot = (20.0, 8.0, 108.0, 108.0);
    let rects = |app: &mut crate::App, layers: &[LiveLayer]| {
        let ws = app.windows.get_mut(&wid).expect("the window");
        *ws.deck_live_rects.borrow_mut() = layers.iter().map(|&l| (l, slot)).collect();
    };
    rects(&mut app, &[LiveLayer::Gauge]);
    assert!(!steams(&app), "a cool dial lays its gauge's slot alone");
    rects(&mut app, &[LiveLayer::Gauge, LiveLayer::Steam]);
    let facts = app.live_facts(wid).expect("the deck draws");
    assert!(facts.steam, "{facts:?}");
    assert!(facts.live(t0, OPEN).steam.is_some());
    assert!(
        app.stats_live_deadline(t0)
            .is_some_and(|at| at > t0 && at <= t0 + FRAME),
        "the steam's next frame is the grid's"
    );

    app.serious_mode = true;
    let serious = app.live_facts(wid).expect("the deck draws");
    assert!(
        serious.motion && !serious.steam,
        "serious mode takes the steam"
    );
    app.serious_mode = false;

    let ws = app.windows.get_mut(&wid).expect("the window");
    ws.lens_card = Some(crate::SettingsCard {
        rgba: vec![0; 4],
        pw: 1,
        ph: 1,
        dx: 0,
        dy: 0,
        fp: 1,
        geom: 1,
        raster_fingerprint_once: std::sync::OnceLock::new(),
        scrim: None,
    });
    assert!(!steams(&app), "a card over the deck takes the steam");
    app.windows.get_mut(&wid).expect("the window").lens_card = None;

    app.config.motion = Some("reduced".into());
    let reduced = app.live_facts(wid).expect("the deck draws");
    assert_eq!(
        reduced.live(t0, OPEN),
        DeckLive::STILL,
        "no steam without motion"
    );
    app.config.motion = None;
    app.system_reduce_motion = true;
    let reduced = app.live_facts(wid).expect("the deck draws");
    assert_eq!(
        reduced.live(t0, OPEN).steam,
        None,
        "no steam under Reduce Motion"
    );
    app.system_reduce_motion = false;
    assert!(steams(&app), "CONTROL: it steams again");
}
