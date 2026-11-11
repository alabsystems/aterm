// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{FILL_GLIDE, FILL_GLIDE_MAX};

#[test]
fn easings_hit_their_ends() {
    assert_eq!(smooth(0), 0);
    assert_eq!(smooth(65_536), 65_536);
    assert_eq!(breath(0), 0);
    assert_eq!(breath(65_536), 65_536);
    assert_eq!(ease_out_quad(0), 0);
    assert_eq!(ease_out_quad(65_536), 65_536);
    assert_eq!(ease_out_quad(32_768), 49_152, "three quarters at half time");
    let span = glide_span(100, 900);
    assert_eq!(span, FILL_GLIDE_MAX, "80 points: 180 + 560 ms, capped");
    assert_eq!(glide(100, 900, Duration::ZERO), 100);
    assert_eq!(glide(100, 900, span), 900);
    assert!(glide(100, 900, span / 2) > 500, "eased out");
    assert_eq!(glide(900, 100, span), 100, "down too");
    assert_eq!(glide_span(500, 500), FILL_GLIDE);
    assert!(glide_span(0, 100) < glide_span(0, 500));
    assert_eq!(glide_span(600, 100), glide_span(0, 500), "both ways");
    assert_eq!(glide_span(0, 1000), FILL_GLIDE_MAX);
    let mut prev = 0u32;
    for p in (1024..=65_536).step_by(1024) {
        let v = breath(p);
        let step = v - prev;
        assert!(
            step >= 1024 * 70 / 100 && step <= 1024 * 115 / 100,
            "{p}: {step}"
        );
        prev = v;
    }
}

/// The integral is exact: a span's mean over a linear ramp is the ramp at
/// its middle; a span straddling a hard edge is the covered fraction; a
/// zero-width span is the point tone; an empty surface is the track.
#[test]
fn a_span_is_the_mean_of_the_profile_over_it() {
    let ramp = Surface {
        edge: None,
        stops: vec![
            Stop {
                at: 0,
                tone: Tone::TRACK,
            },
            Stop {
                at: ROW,
                tone: Tone::FULL,
            },
        ],
        flat: false,
    };
    assert_eq!(ramp.span(0, 1000, 1000).fill, 128);
    assert_eq!(ramp.span(0, 500, 1000).fill, 64);
    assert_eq!(ramp.span(250, 250, 1000).fill, 64);
    let edge = bar(250, None, true);
    assert_eq!(edge.span(200, 300, 1000).fill, 128, "half covered");
    assert_eq!(Surface::default().span(0, 10, 10), Tone::TRACK);
    assert_eq!(
        edge.cells(4),
        [Tone::FULL, Tone::TRACK, Tone::TRACK, Tone::TRACK]
    );
}

/// The stalled bar is dim and unlit by any glint; a still bar at its
/// data is exactly the moving one without its glint.
#[test]
fn a_stalled_bar_is_dim_and_never_glints() {
    let s = stalled_bar(620, true);
    assert!(s.stops.iter().any(|p| p.tone == Tone::STALLED));
    assert!(s.stops.iter().all(|p| p.tone.lift == 0));
    assert_eq!(
        bar(620, None, true),
        bar_in(permille_q16(620), None, true, Tone::FULL, 0)
    );
    assert!(!bar_glints(10) && bar_glints(15));
    let mut flat = s.clone();
    flat.flat = true;
    let _ = flat.span(u64::MAX / 2, u64::MAX / 2, u64::MAX);
}

/// Every echo covers the whole row: a Complete's wipe ends at the window's
/// right edge; a bar's Fault washes what the fill covered and leaves the
/// track; a busy row's Fault freezes its comet and drains it, with no warn.
#[test]
fn echoes_cover_the_whole_row() {
    let mut e = Echo {
        id: crate::MessageId::FIRST,
        msg: crate::Message::new(crate::tags::UPDATE, crate::Severity::Info, "t"),
        kind: EchoKind::Complete,
        from_permille: 300,
        indeterminate: false,
        comet_since: None,
        elapsed: Duration::ZERO,
        started: Instant::now(),
        until: Instant::now(),
        slot: 0,
        load: None,
        load_slot: crate::model::Loads::NONE,
    };
    let (s, fade) = echo(&e, glide_span(300, 1000), Look::MOVING, 80);
    assert_eq!(fade, 0);
    assert!(s.cells(80).iter().all(|t| t.fill == 255), "{s:?}");
    e.kind = EchoKind::Fault;
    let (s, _) = echo(&e, ECHO_FAULT_FLASH, Look::MOVING, 80);
    let cells = s.cells(80);
    assert!(
        cells[..20]
            .iter()
            .all(|t| t.warn == 255 && t.fill == 0 && t.lift == 0),
        "the fill has handed its coverage to warn"
    );
    assert!(
        cells[30..].iter().all(|t| *t == Tone::TRACK),
        "the track keeps its tone"
    );
    for k in 0..=12u64 {
        let (s, _) = echo(&e, Duration::from_millis(k * 10), Look::MOVING, 80);
        for p in &s.stops {
            assert!(
                u16::from(p.tone.fill) + u16::from(p.tone.warn) <= 255,
                "{k}: {p:?}"
            );
        }
    }
    e.indeterminate = true;
    e.comet_since = Some(Duration::from_millis(1500));
    let (s, _) = echo(&e, Duration::ZERO, Look::MOVING, 80);
    assert_eq!(
        s,
        comet(Duration::from_millis(1500), true),
        "frozen where it failed"
    );
    let (s, _) = echo(&e, ECHO_DRAIN, Look::MOVING, 80);
    assert!(
        s.cells(80).iter().all(|t| *t == Tone::TRACK),
        "drained to the track, no warn anywhere"
    );
}

/// The tail's tones are the `i64` clamp form's at every stop a `u8` holds: `ceil(255·u²/n²)`
/// fill and the lift `150·hot²/half²` rounded, the head's tone from `TAIL_STOPS` on.
#[test]
fn the_tail_tones_are_the_clamped_squares() {
    let (n, half) = (i64::from(TAIL_STOPS), i64::from(TAIL_STOPS / 2));
    for stop in 0..=u8::MAX {
        let u = i64::from(stop).clamp(0, n);
        let hot = (u - half).max(0);
        let fill = u8::try_from((u * u * 255 + n * n - 1) / (n * n)).unwrap();
        let lift = u8::try_from(
            (i64::from(COMET_HEAD_LIFT) * hot * hot + half * half / 2) / (half * half),
        )
        .unwrap();
        let want = Tone {
            fill,
            lift,
            warn: 0,
        };
        assert_eq!(tail_tone(stop), want, "stop {stop}");
    }
    assert_eq!(tail_tone(0), Tone::TRACK, "the tail's end is the track");
    assert_eq!(tail_tone(32), Tone::HEAD, "its last stop is the head");
    assert_eq!(
        tail_tone(u8::MAX),
        Tone::HEAD,
        "and so is every stop past it"
    );
}
