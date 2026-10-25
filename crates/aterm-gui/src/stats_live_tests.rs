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
fn the_glide_steps_a_quarter_pixel_within_its_bounds() {
    // 10 px a 10 s column: a quarter pixel every 250 ms.
    assert_eq!(glide_step(1.0), Duration::from_millis(250));
    // Fast strips are held to eight frames a second, slow ones to one each 2 s.
    assert_eq!(glide_step(100.0), GLIDE_STEP_MIN);
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
        step: Duration::from_millis(250),
    }
}

#[test]
fn a_fresh_reading_beats_once_and_the_strips_glide() {
    let t0 = Instant::now();
    let f = facts(t0, true, true);
    let at = f.live(t0, OPEN + 1_234);
    assert_eq!(
        at.glide_ms,
        Some(OPEN + 1_000),
        "quantized to the glide's step"
    );
    assert_eq!(at.beat, Some(0.0));
    assert!(at.fresh);
    // The beat moves in whole frames and ends.
    let mid = f.live(t0 + Duration::from_millis(130), OPEN);
    assert_eq!(mid.beat, Some(120.0 / 840.0));
    let after = f.live(t0 + BEAT, OPEN);
    assert_eq!(after.beat, None);
    assert!(after.fresh && after.glide_ms.is_some());
    // The next frame is the beat's next, then the glide's step.
    assert_eq!(
        f.next_change(t0, OPEN + 1_100),
        Some(t0 + BEAT_FRAME),
        "the beat frames first"
    );
    assert_eq!(
        f.next_change(t0 + BEAT, OPEN + 1_100),
        Some(t0 + BEAT + Duration::from_millis(150)),
        "then the glide's step"
    );
    // Without the beat (serious mode, a card over the deck) the glide alone.
    let quiet = facts(t0, true, false);
    assert_eq!(quiet.live(t0, OPEN).beat, None);
    assert_eq!(
        quiet.next_change(t0, OPEN + 1_100),
        Some(t0 + Duration::from_millis(150))
    );
    // Every frame the schedule names changes what the deck draws.
    let mut now = t0;
    let mut ms = OPEN + 1_100;
    for _ in 0..40 {
        let Some(next) = f.next_change(now, ms) else {
            break;
        };
        let dt = next - now;
        let ms_next = ms + i64::try_from(dt.as_millis()).unwrap_or(0);
        assert_ne!(
            f.live(now, ms),
            f.live(next, ms_next),
            "a frame at {dt:?} moved nothing"
        );
        now = next;
        ms = ms_next;
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
    let beat = DeckLive {
        beat: Some(0.5),
        ..still
    };
    let keys = [still, DeckLive::STALE, glide, beat].map(|l| l.key());
    for (i, a) in keys.iter().enumerate() {
        assert_ne!(*a, 0);
        for b in keys.iter().skip(i + 1) {
            assert_ne!(a, b);
        }
    }
    assert!(LiveLayer::Glide.moved(&still, &glide));
    assert!(!LiveLayer::Marker.moved(&still, &glide));
    assert!(LiveLayer::Marker.moved(&still, &beat));
    assert!(!LiveLayer::Glide.moved(&still, &beat));
    assert!(LiveLayer::Marker.moved(&still, &DeckLive::STALE));
    assert!(!LiveLayer::Glide.moved(&still, &DeckLive::STALE));
}

const TEXT: HeaderText<'static> = HeaderText {
    tab: "Tab 2",
    machine: "this Mac",
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

/// What is wrong with `head`'s time scale over `strip`.
fn ruler_faults(head: &HeaderPlan, strip: Strip, m: &FaceMetrics) -> Vec<String> {
    let mut why = Vec::new();
    let Some(time) = head.labels.iter().find(|l| l.label == Label::Time) else {
        return vec!["no time label".into()];
    };
    if !["10 min ago", "\u{2013}10 min"].contains(&time.text.as_str()) {
        why.push(format!("the time label says {:?}", time.text));
    }
    // A tick fits when its words can cover its point inside what the short label and the
    // marker leave of the strip: then the scale must not be empty.
    let short_lo = strip.x + header_pt_words(m, "\u{2013}10 min") + 6.0;
    let room = strip.end() - MARKER_ROOM;
    let fits = [300_u32, 60, 120, 30].iter().any(|&age| {
        let x = strip.end() - f64::from(age) / f64::from(strip.period_s) * strip.pitch;
        let w = header_pt_words(m, &plan::tick_words(age));
        short_lo.max(x - w) <= (room - w).min(x)
    });
    if head.ruler.is_empty() && fits {
        why.push("a tick fits and none says how far back the strip reaches".into());
    }
    if !head.ruler.is_empty() && time.text != "10 min ago" && !fits {
        why.push("the short label with no tick that fits".into());
    }
    let lo = strip.x + header_pt_words(m, &time.text);
    let hi = strip.end() - MARKER_ROOM;
    for (i, t) in head.ruler.iter().enumerate() {
        let want = strip.end() - f64::from(t.age_s) / f64::from(strip.period_s) * strip.pitch;
        if (t.x - want).abs() > EPS {
            why.push(format!("{} at {}, not {want}", t.text, t.x));
        }
        if t.text != plan::tick_words(t.age_s) || t.age_s >= 600 {
            why.push(format!("{} for {} s", t.text, t.age_s));
        }
        let w = header_pt_words(m, &t.text);
        if t.left > t.x + EPS || t.left + w < t.x - EPS {
            why.push(format!(
                "{} at {}..{} leaves its tick {}",
                t.text,
                t.left,
                t.left + w,
                t.x
            ));
        }
        if t.left < lo || t.left + w > hi + EPS {
            why.push(format!(
                "{} at {}..{} leaves {lo}..{hi}",
                t.text,
                t.left,
                t.left + w
            ));
        }
        if let Some(next) = head.ruler.get(i + 1)
            && t.left + w > next.left
        {
            why.push(format!("{} meets {}", t.text, next.text));
        }
    }
    why
}

/// T-G4's widths, both faces, both forms: wherever the header carries the time label, a scale
/// says how far back the strip reaches, each tick at its age from now and its words over it.
#[test]
fn the_time_scale_marks_how_far_back_at_every_width() {
    let faces = [FaceMetrics::helvetica_neue(), FaceMetrics::test_stack()];
    let mut failed = Vec::new();
    let (mut drew, mut scaled) = (0, 0);
    for m in &faces {
        for form in [DeckForm::Full, DeckForm::Folded] {
            for w in (160..=5120).step_by(3).map(f64::from) {
                let Some((head, strip)) = head_of(form, w, m) else {
                    continue;
                };
                if !head.labels.iter().any(|l| l.label == Label::Time) {
                    continue;
                }
                drew += 1;
                scaled += usize::from(!head.ruler.is_empty());
                for why in ruler_faults(&head, strip, m) {
                    if failed.len() < 30 {
                        failed.push(format!("{form:?} {w} pt: {why}"));
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
        scaled * 10 > drew * 9,
        "CONTROL: {scaled} of {drew} headers carry a scale"
    );
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// The scale at the widths the mockups draw, and at each strip's period: the ticks, left to
/// right, by age.
#[test]
fn the_time_scale_at_the_mockups_widths() {
    let m = FaceMetrics::helvetica_neue();
    let ages = |form, w| -> (u32, Vec<u32>) {
        let (head, strip) = head_of(form, w, &m).expect("the table is drawn");
        let mut ages: Vec<u32> = head.ruler.iter().map(|t: &RulerTick| t.age_s).collect();
        ages.sort_unstable();
        (strip.period_s, ages)
    };
    let got = [
        (DeckForm::Full, 2560.0),
        (DeckForm::Full, 1424.0),
        (DeckForm::Full, 1119.0),
        (DeckForm::Full, 800.0),
        (DeckForm::Full, 550.0),
        (DeckForm::Folded, 1119.0),
    ]
    .map(|(form, w)| ages(form, w));
    assert_eq!(
        got,
        [
            (10, vec![60, 120, 300]),
            (10, vec![60, 120, 300]),
            (10, vec![60, 300]),
            (20, vec![300]),
            (30, vec![60]),
            (20, vec![60, 300]),
        ]
    );
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
    let facts = app.live_facts(wid).expect("the deck draws");
    assert!(facts.motion && facts.beat, "{facts:?}");
    assert_eq!(facts.landed, Some(t0));
    let live = facts.live(t0, OPEN);
    assert!(live.fresh && live.glide_ms.is_some() && live.beat == Some(0.0));
    // The beat's next frame, or the glide's next step on the wall clock if that comes first.
    let next = app.stats_live_deadline(t0);
    assert!(
        next.is_some_and(|at| at > t0 && at <= t0 + BEAT_FRAME),
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
