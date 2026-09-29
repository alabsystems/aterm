// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ONE BAND DRIVER, tested where it lives (design ruling 338): the step's
//! order and its freeze, the afford arithmetic, the idle law, the memo's key
//! and cost law, the past guard, the look-change due, and the paint key —
//! including the High Contrast toggle the macOS host's old theme-only key
//! missed (ruling 337, F1).

use crate::drive::{self, Commit, Lay, LookIn, View};
use crate::ink::{self, BandInks, BarBase, ForcedPalette, ThemeInks};
use crate::paint::{Geometry, Hover, HoverTarget};
use crate::wire::WireGate;
use crate::{
    ANIM_FRAME, Duration, ELAPSED_AFTER, Hold, Instant, Links, Look, MAX_ROWS, Message,
    MessageCenter, MessageLog, Meter, PROGRESS_GRACE, Pace, Restatement, SHRINK_QUIET,
    STALE_TAILED, Severity, WallStamp, tags,
};

const STAMP: WallStamp = WallStamp {
    unix_ms: 1_790_000_000_000,
};

const STILL: Look = Look {
    pace: Pace::Still,
    graded: true,
};

const DARK: ThemeInks = ThemeInks {
    bg: [0x11, 0x13, 0x18],
    fg: [0xd0, 0xd0, 0xd0],
    cursor: [0x50, 0xfa, 0x7b],
};

const HC: ForcedPalette = ForcedPalette {
    window: [0x00, 0x00, 0x00],
    window_text: [0xff, 0xff, 0xff],
    highlight: [0x1a, 0xeb, 0xff],
    highlight_text: [0x00, 0x00, 0x00],
    btn_face: [0x00, 0x00, 0x00],
};

fn lay(cols: usize) -> Lay {
    Lay {
        cols,
        links: Links::Withheld,
        home: || None,
    }
}

fn geom(cols: usize) -> Geometry {
    Geometry {
        win_w: cols * 10 + 12,
        cells_x: 6,
        cell_w: 10,
    }
}

fn inks() -> BandInks {
    BandInks::derive(DARK, None, BarBase::Blend)
}

fn warn(title: &str, secs: u64) -> Message {
    Message::new(tags::CONFIG, Severity::Warn, title).hold(Hold::For(Duration::from_secs(secs)))
}

fn bar(permille: u16) -> Message {
    Message::new(tags::UPDATE, Severity::Info, "Downloading assets")
        .meter(Meter {
            fill_permille: Some(permille),
            ..Meter::default()
        })
        .hold(Hold::Live {
            stale_after: STALE_TAILED,
        })
        .key("p")
}

fn busy() -> Message {
    Message::new(tags::TOOLCHAIN, Severity::Info, "Indexing the tree")
        .meter(Meter {
            busy: true,
            ..Meter::default()
        })
        .hold(Hold::Live {
            stale_after: STALE_TAILED,
        })
        .key("b")
}

/// One host step with nothing to spare but the band's own ceiling.
fn step(center: &mut MessageCenter, gate: &mut WireGate, now: Instant) -> drive::Committed {
    let reserved = center.committed_rows();
    let c = drive::settle_and_commit(
        center,
        Commit {
            now,
            frozen: false,
            afford: MAX_ROWS,
            reserved,
        },
    );
    let _ = drive::drain_and_pace(center, gate, now, true);
    c
}

#[test]
fn the_step_commits_after_its_settle_and_a_freeze_holds_the_rows_and_the_log() {
    let t0 = Instant::now();
    let mut center = MessageCenter::new(MessageLog::default(), t0);
    let mut gate = WireGate::default();
    let _ = center.post(warn("Disk nearly full", 2), STAMP, t0);
    let c = step(&mut center, &mut gate, t0);
    assert_eq!(c.regrid, Some(1), "the row is committed in the same step");
    // A freeze past the hold: nothing folds, nothing commits, nothing drains.
    let t = t0 + Duration::from_secs(3);
    let _ = center.post(warn("Deploy failed", 2), STAMP, t);
    let frozen = drive::settle_and_commit(
        &mut center,
        Commit {
            now: t,
            frozen: true,
            afford: MAX_ROWS,
            reserved: 1,
        },
    );
    assert_eq!(frozen.regrid, None, "a freeze never re-grids");
    assert_eq!(center.committed_rows(), 1);
    assert_eq!(center.on_glass().count(), 1, "the hold is suspended");
    let (lines, _) = drive::drain_and_pace(&mut center, &mut gate, t, false);
    assert!(lines.is_empty(), "a frozen successor writes nothing");
    let (lines, _) = drive::drain_and_pace(&mut center, &mut gate, t, true);
    assert!(!lines.is_empty(), "the lines waited for the persist");
    // Lifted: the expired hold folds; the second row takes the slot.
    let c = step(&mut center, &mut gate, t);
    assert!(c.glass_changed, "the held row folded");
    // A host that reserves fewer rows than the center committed converges
    // even with no move (the handoff successor's case).
    let c = drive::settle_and_commit(
        &mut center,
        Commit {
            now: t,
            frozen: false,
            afford: MAX_ROWS,
            reserved: 0,
        },
    );
    assert_eq!(c.regrid, Some(center.committed_rows()));
    assert!(c.regrid.is_some_and(|r| r > 0));
    // …and a shrink waits its quiet before the count moves.
    let t = t + Duration::from_secs(3);
    let c = step(&mut center, &mut gate, t);
    assert!(c.glass_changed);
    assert_eq!(c.regrid, None, "the shrink waits");
    let c = step(&mut center, &mut gate, t + SHRINK_QUIET);
    assert_eq!(c.regrid, Some(0));
    assert_eq!(drive::state_deadline(&center, &gate, false), None, "idle");
}

#[test]
fn afford_keeps_every_grid_its_last_row() {
    assert_eq!(
        drive::afford([], 2),
        MAX_ROWS,
        "no grid: the band's own cap"
    );
    assert_eq!(drive::afford([24], 0), 23);
    assert_eq!(drive::afford([24, 3], 1), 3, "the smallest decides");
    assert_eq!(drive::afford([1], 0), 0, "one row: the band takes none");
    assert_eq!(drive::afford([0], 0), 0, "saturates at zero");
    assert_eq!(drive::afford([u16::MAX], 3), u16::MAX - 1, "saturates up");
}

#[test]
fn the_look_moves_only_on_screen_unfrozen_and_allowed() {
    let on = LookIn {
        motion_allowed: true,
        on_screen: true,
        frozen: false,
        forced: false,
    };
    assert_eq!(drive::look(on), Look::MOVING);
    for off in [
        LookIn {
            motion_allowed: false,
            ..on
        },
        LookIn {
            on_screen: false,
            ..on
        },
        LookIn { frozen: true, ..on },
    ] {
        assert_eq!(drive::look(off), STILL, "{off:?}");
    }
    let forced = drive::look(LookIn { forced: true, ..on });
    assert_eq!((forced.pace, forced.graded), (Pace::Moving, false));
}

#[test]
fn an_idle_band_arms_nothing_a_held_row_only_its_hold_and_a_still_look_only_ticks() {
    let t0 = Instant::now();
    let mut center = MessageCenter::new(MessageLog::default(), t0);
    let mut gate = WireGate::default();
    let mut view = View::default();
    let l = lay(80);
    // Idle.
    let _ = step(&mut center, &mut gate, t0);
    assert_eq!(drive::state_deadline(&center, &gate, false), None);
    assert_eq!(view.prepare(&center, &l, t0, Look::MOVING), 0);
    let over = |center: &MessageCenter, view: &View, now: Instant, look: Look| {
        drive::motion_deadline_over(
            center,
            center.committed_rows(),
            false,
            now,
            [(view, l, look)],
        )
    };
    assert_eq!(over(&center, &view, t0, Look::MOVING), None);
    assert!(drive::due_views(&center, 0, false, t0, [(0u8, &view, l, Look::MOVING)]).is_empty());
    assert_eq!(
        view.paint(&center, 80, None, geom(80), false, &|| panic!(
            "idle derives no inks"
        )),
        None
    );
    assert_eq!(view.band_fp(&center, 80, None, geom(80)), 0);
    // A held row: its hold's end, and no motion.
    let _ = center.post(warn("Disk nearly full", 5), STAMP, t0);
    let _ = step(&mut center, &mut gate, t0);
    assert_eq!(
        drive::state_deadline(&center, &gate, false),
        Some(t0 + Duration::from_secs(5))
    );
    assert_eq!(
        drive::state_deadline(&center, &gate, true),
        None,
        "a freeze suspends the hold's wake"
    );
    assert_eq!(view.prepare(&center, &l, t0, Look::MOVING), 0);
    assert_eq!(over(&center, &view, t0, Look::MOVING), None);
    // A busy row: frames while moving, only its words' ticks while still.
    let mut center = MessageCenter::new(MessageLog::default(), t0);
    let _ = center.post(busy(), STAMP, t0);
    let at = t0 + PROGRESS_GRACE + Duration::from_millis(100);
    let _ = step(&mut center, &mut gate, at);
    assert_eq!(center.committed_rows(), 1);
    let mut view = View::default();
    assert_ne!(view.prepare(&center, &l, at, Look::MOVING), 0);
    let moving = over(&center, &view, at, Look::MOVING).expect("a frame");
    assert!(moving <= at + ANIM_FRAME, "{:?}", moving - at);
    let _ = view.prepare(&center, &l, at, STILL);
    let still = over(&center, &view, at, STILL).expect("a tick");
    assert!(still >= t0 + ELAPSED_AFTER, "{:?}", still - t0);
    assert_eq!(
        drive::motion_deadline_over(&center, 1, true, at, [(&view, l, Look::MOVING)]),
        None,
        "frozen: nothing armed"
    );
    assert_eq!(
        drive::motion_deadline_over(&center, 0, false, at, [(&view, l, Look::MOVING)]),
        None,
        "no row reserved: nothing armed"
    );
}

#[test]
fn the_next_frame_memo_hits_on_its_key_and_misses_on_each_term() {
    let t0 = Instant::now();
    let mut center = MessageCenter::new(MessageLog::default(), t0);
    let mut gate = WireGate::default();
    let posted = center.post(bar(135), STAMP, t0);
    let at = t0 + PROGRESS_GRACE + Duration::from_millis(100);
    let _ = step(&mut center, &mut gate, at);
    let l = lay(80);
    let mut view = View::default();
    let _ = view.prepare(&center, &l, at, Look::MOVING);
    let frame_at = view.motion().expect("prepared").at;
    let want = center.motion_deadline(view.layout().expect("laid"), frame_at, Look::MOVING);
    assert!(want.is_some(), "a live bar has a next change");
    assert_eq!(view.motion_deadline(&center, &l, at, Look::MOVING), want);
    assert_eq!(view.deadline_computations(), 1);
    for _ in 0..16 {
        assert_eq!(view.motion_deadline(&center, &l, at, Look::MOVING), want);
    }
    assert_eq!(view.deadline_computations(), 1, "one scan per frame");
    // The epoch: a restatement that moves no painted word still re-anchors
    // the estimator, so the memo misses while the layout stays.
    let fp = center.fingerprint(80);
    assert!(center.restate(
        posted.id,
        Restatement {
            title: Some("Downloading assets".into()),
            ..Restatement::default()
        },
        at,
    ));
    assert_eq!(center.fingerprint(80), fp, "the layout is still current");
    let _ = view.next_frame(&center, &l, at, Look::MOVING);
    assert_eq!(view.deadline_computations(), 2, "a new epoch misses");
    let _ = view.next_frame(&center, &l, at, Look::MOVING);
    assert_eq!(view.deadline_computations(), 2, "and is memoised again");
    // The width: unprepared, from now, unmemoised.
    let probe = at + Duration::from_millis(7);
    let _ = view.next_frame(&center, &lay(81), probe, Look::MOVING);
    let _ = view.next_frame(&center, &lay(81), probe, Look::MOVING);
    assert_eq!(view.deadline_computations(), 4);
    assert_eq!(
        view.last_from(),
        Some(probe),
        "a layout mismatch reads from now"
    );
    // The look: likewise, and the frame on glass is due at once.
    let _ = view.next_frame(&center, &l, probe, STILL);
    assert_eq!(view.deadline_computations(), 5);
    assert_eq!(view.last_from(), Some(probe));
    assert!(view.due(&center, &l, probe, STILL), "another look: due now");
    assert!(
        !view.due(&center, &l, at, Look::MOVING),
        "its own look: not yet"
    );
    // A new frame: a new origin.
    let next = view
        .motion_deadline(&center, &l, at, Look::MOVING)
        .expect("next");
    let before = view.deadline_computations();
    assert!(view.due(&center, &l, next, Look::MOVING));
    let _ = view.prepare(&center, &l, next, Look::MOVING);
    let _ = view.next_frame(&center, &l, next, Look::MOVING);
    assert_eq!(
        view.deadline_computations(),
        before + 1,
        "a new frame misses"
    );
}

#[test]
fn a_past_answer_is_re_read_from_now() {
    let t0 = Instant::now();
    let mut center = MessageCenter::new(MessageLog::default(), t0);
    let mut gate = WireGate::default();
    let _ = center.post(busy(), STAMP, t0);
    let at = t0 + PROGRESS_GRACE + Duration::from_millis(100);
    let _ = step(&mut center, &mut gate, at);
    let l = lay(80);
    let mut view = View::default();
    let _ = view.prepare(&center, &l, at, Look::MOVING);
    let late = at + Duration::from_millis(500);
    let stale = view
        .next_frame(&center, &l, late, Look::MOVING)
        .expect("next");
    assert!(stale <= late, "the frame on glass is behind");
    let d = view
        .motion_deadline(&center, &l, late, Look::MOVING)
        .expect("next");
    assert!(d > late, "never a past deadline");
    assert_eq!(view.last_from(), Some(late));
    // The web's rule reads the kept layout from where it is told.
    assert_eq!(view.deadline_from(&center, late, Look::MOVING), Some(d));
}

#[test]
fn the_paint_key_moves_with_every_term_it_paints_from() {
    let t0 = Instant::now();
    let mut center = MessageCenter::new(MessageLog::default(), t0);
    let mut gate = WireGate::default();
    let _ = center.post(warn("Disk nearly full", 30), STAMP, t0);
    let _ = step(&mut center, &mut gate, t0);
    let l = lay(80);
    let g = geom(80);
    let mut view = View::default();
    let _ = view.prepare(&center, &l, t0, Look::MOVING);
    let dark = inks();
    let rows = view
        .paint(&center, 80, None, g, false, &|| dark)
        .expect("painted");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        view.paint(&center, 80, None, g, false, &|| dark),
        None,
        "a hit"
    );
    let hover = Some(Hover {
        row: 0,
        target: HoverTarget::Body,
    });
    assert!(
        view.paint(&center, 80, hover, g, false, &|| dark).is_some(),
        "hover"
    );
    assert!(
        view.paint(&center, 80, None, g, false, &|| dark).is_some(),
        "unhover"
    );
    let wide = Geometry {
        win_w: g.win_w + 5,
        ..g
    };
    assert!(
        view.paint(&center, 80, None, wide, false, &|| dark)
            .is_some(),
        "geometry"
    );
    let light = BandInks::derive(
        ThemeInks {
            bg: [0xff; 3],
            fg: [0x1f, 0x23, 0x28],
            cursor: [0x09, 0x69, 0xda],
        },
        None,
        BarBase::Blend,
    );
    assert!(
        view.paint(&center, 80, None, wide, false, &|| light)
            .is_some(),
        "inks"
    );
    // THE HIGH CONTRAST TOGGLE (ruling 337, F1): the palette the host derives
    // from the SAME theme becomes the OS's, and the rows must follow. The old
    // macOS key hashed the theme — equal across the toggle — so it kept the
    // stale rows; this key hashes what the rows are painted with.
    let before = view
        .paint(&center, 80, None, g, false, &|| dark)
        .expect("painted");
    let hc = BandInks::forced(HC);
    let after = view
        .paint(&center, 80, None, g, true, &|| hc)
        .expect("repainted");
    assert_ne!(before, after, "the rows differ under the forced palette");
    let fresh = ink::paint_band(
        view.layout().expect("laid"),
        None,
        g,
        view.motion().expect("prepared"),
        true,
        &hc,
    );
    assert_eq!(after, fresh, "the drive paints what a fresh paint draws");
    // NEGATIVE CONTROL: the old key — the theme's bg/fg/cursor in place of
    // the inks — is the same number on both sides of the toggle, so a cache
    // on it would have presented `before`, which is not `fresh`.
    let theme_key = |t: ThemeInks| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        t.bg.hash(&mut h);
        t.fg.hash(&mut h);
        t.cursor.hash(&mut h);
        h.finish()
    };
    let old_key = |_forced: bool| {
        (
            center.fingerprint(80),
            80usize,
            theme_key(DARK),
            None::<Hover>,
            g,
            view.motion_fp(),
        )
    };
    assert_eq!(
        old_key(false),
        old_key(true),
        "the old key misses the toggle"
    );
    assert_ne!(before, fresh, "and the stale rows are not the forced ones");
    // Motion: a busy row's comet moves the key frame to frame.
    let mut center = MessageCenter::new(MessageLog::default(), t0);
    let _ = center.post(busy(), STAMP, t0);
    let at = t0 + PROGRESS_GRACE + Duration::from_millis(100);
    let _ = step(&mut center, &mut gate, at);
    let mut view = View::default();
    let _ = view.prepare(&center, &l, at, Look::MOVING);
    assert!(view.paint(&center, 80, None, g, false, &|| dark).is_some());
    let mfp = view.motion_fp();
    let _ = view.prepare(&center, &l, at + ANIM_FRAME * 5, Look::MOVING);
    assert_ne!(view.motion_fp(), mfp);
    assert!(
        view.paint(&center, 80, None, g, false, &|| dark).is_some(),
        "motion"
    );
    // The row leaves: one empty paint, then nothing; the view forgets it.
    let mut center = MessageCenter::new(MessageLog::default(), at);
    let _ = step(&mut center, &mut gate, at);
    assert_eq!(view.prepare(&center, &l, at, Look::MOVING), 0);
    assert!(view.layout().is_none() && view.motion().is_none() && view.look().is_none());
    assert_eq!(
        view.paint(&center, 80, None, g, false, &|| dark),
        Some(Vec::new())
    );
    assert_eq!(view.paint(&center, 80, None, g, false, &|| dark), None);
    view.invalidate();
    assert_eq!(
        view.paint(&center, 80, None, g, false, &|| dark),
        Some(Vec::new()),
        "an invalidated view repaints, even empty"
    );
    assert_eq!(view.paint(&center, 80, None, g, false, &|| dark), None);
}
