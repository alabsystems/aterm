// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: the reviewer's proofs of the band's MOTION on the host side
//! (design §10.8–§10.12, review 2026-09-24) — the idle law, a hidden window,
//! the frame cadence, what one animation frame costs the host, the still
//! looks, the capture's determinism, the `appstatus` face and the Settings ▸
//! Messages page. Each test drives a headless `App` through the same seams
//! the loop uses (`band_motion_deadline` is what `about_to_wait` folds under
//! `DeadlineOwner::MessageBandMotion`, `band_motion_due` is the timer wake's
//! redraw list, `prepare_band_motion` is the first thing a redraw does).

use std::time::{Duration, Instant};

use aterm_messages::{
    ANIM_FRAME, Amount, ELAPSED_AFTER, Hold, Look, Message, Meter, Pace, Restatement, SHRINK_QUIET,
    STALE_UPDATE, Severity, Unit, tags,
};

use crate::messages_host::message_regrids;
use crate::{App, WindowId, term_lock};

const WID: WindowId = WindowId(0);

/// A headless App whose first window is ON SCREEN (the test seam for a real,
/// unoccluded OS window) and focused, with every motion gate open.
fn on_screen_app() -> App {
    let mut app = App::headless_for_test();
    let ws = app.windows.get_mut(&WID).expect("window 0");
    ws.band_on_screen_for_test = true;
    ws.focused = true;
    app.system_reduce_motion = false;
    app.serious_mode = false;
    app.perf_reduced = false;
    app.config.motion = None;
    app
}

/// A live update download with an amount: a determinate bar with an ETA slot.
fn download(done: u64) -> Message {
    let total = 74_000_000;
    Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.92.0")
        .meter(Meter {
            fill_permille: Some(u16::try_from(done * 1000 / total).unwrap()),
            stats: String::new(),
            amount: Some(Amount {
                series: Amount::series_of("aterm 0.92.0"),
                done,
                total,
                unit: Unit::Bytes,
            }),
            ..Meter::default()
        })
        .hold(Hold::Live {
            stale_after: STALE_UPDATE,
        })
        .key("update.progress")
}

/// Fill the scratch from the terminal and splice the band, as the redraw's
/// compose does (the strip is not drawn headless).
fn compose(app: &mut App) {
    let (rows, cols) = {
        let ws = &app.windows[&WID];
        (ws.rows as usize, ws.cols as usize)
    };
    let terminal = app
        .front_terminal(WID)
        .expect("front terminal")
        .term
        .clone();
    {
        let ws = app.windows.get_mut(&WID).unwrap();
        let mut term = term_lock(&terminal);
        term.cell_frame_into(&mut ws.input_scratch, rows, cols);
    }
    app.splice_message_band(WID, aterm_render::Theme::default());
}

/// The terminal's PTY size as the session holds it.
fn pty_dims(app: &App) -> (u16, u16) {
    let terminal = app
        .front_terminal(WID)
        .expect("front terminal")
        .term
        .clone();
    let term = term_lock(&terminal);
    (term.rows(), term.cols())
}

// ---- (1) THE IDLE LAW -----------------------------------------------------

/// FL-1 AT THE HOST: an App with nothing on the glass arms no state wake and
/// no motion wake, lists no window as due, prepares a zero motion term and
/// folds a zero band term into the RepaintKey — and a RECORD (`Hold::LogOnly`)
/// changes none of that, nor does a live row once it has folded and its echo
/// has ended. On screen and focused throughout: the window that WOULD move.
#[test]
fn an_idle_app_arms_nothing_and_a_record_or_a_folded_row_leaves_it_idle() {
    let mut app = on_screen_app();
    let idle = |app: &mut App, what: &str| {
        let now = Instant::now();
        assert_eq!(app.messages_deadline(), None, "{what}: a state wake");
        assert_eq!(app.band_motion_deadline(now), None, "{what}: a motion wake");
        assert!(app.band_motion_due(now).is_empty(), "{what}: a due window");
        assert_eq!(
            app.prepare_band_motion(WID, now),
            0,
            "{what}: a motion term"
        );
        let ws = &app.windows[&WID];
        assert_eq!(
            crate::message_band::band_fp(
                app.messages.fingerprint(usize::from(ws.cols)),
                ws.band_hover,
                app.band_geometry(WID),
                ws.band_motion_fp,
            ),
            0,
            "{what}: the RepaintKey's band term"
        );
        assert_eq!(app.message_band_rows, 0, "{what}: a row");
    };
    idle(&mut app, "a fresh App");
    // Records: confirmations, FYI, repeats — the log keeps them, the glass
    // never sees them.
    for title in ["ALab tools installed", "Config reloaded", "Update checked"] {
        app.record_message(Message::new(tags::SYSTEM, Severity::Info, title));
    }
    app.record_message(Message::new(tags::CONFIG, Severity::Warn, "Unknown key"));
    idle(&mut app, "after four records");
    // A live row moves; once it resolves, echoes and folds, nothing is armed.
    app.announce_toolchain_pass("installing 10 ALab program(s)", true);
    assert!(
        app.band_motion_deadline(Instant::now()).is_some(),
        "it moves"
    );
    app.toolchain_pass_ended(true);
    let later = Instant::now() + SHRINK_QUIET + Duration::from_secs(2);
    app.settle_messages(later);
    app.settle_messages(later + SHRINK_QUIET + Duration::from_millis(10));
    idle(&mut app, "after the row folded");
}

// ---- (2) A HIDDEN WINDOW ----------------------------------------------------

/// OCCLUDED (minimized, fully covered — what `WindowEvent::Occluded` reports)
/// and HEADLESS windows schedule NO animation frame and no text tick, and are
/// never listed due — even with a prepared frame from before they hid; an
/// UNFOCUSED window on screen keeps only the text ticks (never a frame-rate
/// wake). Folded over a 20 s window of the loop, not one instant.
#[test]
fn a_hidden_window_schedules_no_animation_frames() {
    let mut app = on_screen_app();
    app.announce_toolchain_pass("installing 10 ALab program(s)", true);
    let t0 = Instant::now();
    app.prepare_band_motion(WID, t0);
    let span = |app: &App| {
        let mut wakes = Vec::new();
        let mut t = t0;
        while t < t0 + Duration::from_secs(20) {
            match app.band_motion_deadline(t) {
                Some(d) => {
                    wakes.push(d);
                    t = d;
                }
                None => break,
            }
        }
        wakes
    };
    let moving = span(&app);
    assert!(
        moving.len() > 500,
        "on screen and focused, it animates: {}",
        moving.len()
    );

    app.windows.get_mut(&WID).unwrap().occluded = true;
    assert!(span(&app).is_empty(), "occluded: nothing armed");
    for k in 0..40u32 {
        assert!(
            app.band_motion_due(t0 + ANIM_FRAME * k).is_empty(),
            "occluded: never due"
        );
    }
    app.windows.get_mut(&WID).unwrap().occluded = false;

    app.windows.get_mut(&WID).unwrap().band_on_screen_for_test = false;
    assert!(span(&app).is_empty(), "headless: nothing armed");
    app.windows.get_mut(&WID).unwrap().band_on_screen_for_test = true;

    app.windows.get_mut(&WID).unwrap().focused = false;
    let still = span(&app);
    assert!(!still.is_empty(), "unfocused: the elapsed words still tick");
    let posted = app.messages.on_glass().next().expect("the row").posted_at;
    assert!(
        still[0] >= posted + ELAPSED_AFTER,
        "unfocused: the first wake is the first elapsed word, not a frame"
    );
    for pair in still.windows(2) {
        assert!(
            pair[1] - pair[0] >= Duration::from_millis(900),
            "unfocused: a wake {:?} after the last — a frame-rate wake",
            pair[1] - pair[0]
        );
    }
}

/// A determinate bar's next glint/glide frame searches future cell surfaces.
/// The event sweep and the park may ask again dozens of times in the SAME
/// frame during PTY traffic; those reads must share one search, yet a new
/// reading, width, visual look or visibility edge must keep the first real
/// transition reachable.
#[test]
fn repeated_event_and_park_queries_share_the_next_visible_transition() {
    let mut app = on_screen_app();
    let id = app.post_message(download(10_000_000));
    let t0 = Instant::now();
    app.prepare_band_motion(WID, t0);
    let ws = &app.windows[&WID];
    let expected = app.messages.motion_deadline(
        &ws.band_layout.as_ref().expect("prepared layout").2,
        ws.band_motion.as_ref().expect("prepared frame").at,
        Look::MOVING,
    );
    assert!(expected.is_some(), "the live bar has a future transition");
    assert_eq!(app.band_motion_deadline(t0), expected);
    let first_scan = app.windows[&WID].band_motion_deadline_computations.get();
    assert_eq!(first_scan, 1);
    for event in 0..64 {
        if event % 4 == 0 {
            // Terminal output or a typed key may cause an additional present
            // in this same band frame. It must not force another look-ahead
            // scan merely because terminal content changed.
            app.prepare_band_motion(WID, t0);
        }
        assert_eq!(app.band_motion_deadline(t0), expected);
        assert!(app.band_motion_due(t0).is_empty());
    }
    assert_eq!(
        app.windows[&WID].band_motion_deadline_computations.get(),
        first_scan,
        "same-frame event and park queries reuse the cell-surface scan"
    );
    let due = expected.unwrap();
    assert_eq!(app.band_motion_due(due), vec![WID]);
    app.prepare_band_motion(WID, due);
    assert!(app.band_motion_deadline(due).is_some_and(|next| next > due));
    let after_frame = app.windows[&WID].band_motion_deadline_computations.get();
    assert_eq!(
        after_frame,
        first_scan + 1,
        "a presented frame gets a new answer"
    );

    let next_meter = download(20_000_000).meter;
    assert!(app.messages.restate(
        id,
        Restatement {
            meter: Some(next_meter),
            ..Restatement::default()
        },
        due,
    ));
    let _ = app.band_motion_deadline(due);
    let after_progress = app.windows[&WID].band_motion_deadline_computations.get();
    assert_eq!(
        after_progress,
        after_frame + 1,
        "new progress retires the memo"
    );

    // A visually identical restatement is allowed to re-anchor future ETA
    // timing while leaving the Settings/glass revision alone.
    let revision = app.messages.revision();
    let epoch = app.messages.motion_input_epoch();
    assert!(app.messages.restate(
        id,
        Restatement {
            title: Some("Downloading aterm v0.92.0".into()),
            ..Restatement::default()
        },
        due,
    ));
    assert_eq!(app.messages.revision(), revision);
    assert_ne!(app.messages.motion_input_epoch(), epoch);
    let _ = app.band_motion_deadline(due);
    let after_heartbeat = app.windows[&WID].band_motion_deadline_computations.get();
    assert_eq!(after_heartbeat, after_progress + 1);

    app.windows.get_mut(&WID).unwrap().cols += 1;
    let width_probe = due + Duration::from_millis(7);
    let _ = app.band_motion_deadline(width_probe);
    let after_width = app.windows[&WID].band_motion_deadline_computations.get();
    assert_eq!(
        after_width,
        after_heartbeat + 1,
        "width changes the surface cells"
    );
    assert_eq!(
        app.windows[&WID].band_motion_deadline_last_from.get(),
        Some(width_probe),
        "a layout mismatch starts from now, not the old frame"
    );
    app.windows.get_mut(&WID).unwrap().cols -= 1;
    app.prepare_band_motion(WID, due);
    app.windows.get_mut(&WID).unwrap().occluded = true;
    assert_eq!(app.band_motion_deadline(width_probe), None);
    assert!(app.band_motion_due(width_probe).is_empty());
    assert_eq!(
        app.windows[&WID].band_motion_deadline_computations.get(),
        after_width
    );
    app.windows.get_mut(&WID).unwrap().occluded = false;
    app.windows.get_mut(&WID).unwrap().focused = false;
    let focus_probe = due + Duration::from_millis(11);
    let _ = app.band_motion_deadline(focus_probe);
    assert_eq!(
        app.windows[&WID].band_motion_deadline_computations.get(),
        after_width + 1,
        "unfocus recomputes the next text-only transition"
    );
    assert_eq!(
        app.windows[&WID].band_motion_deadline_last_from.get(),
        Some(focus_probe),
        "a look mismatch starts from now, not the old frame"
    );
}

// ---- (3) THE CADENCE AND THE COST OF ONE FRAME -----------------------------

/// THE CAP: whatever wakes the loop — here a wake every 7 ms for 3 s, as a
/// PTY stream or pointer traffic would — the band is DUE at most once per
/// 33 ms grid step, each prepared frame sits on the grid strictly after the
/// last, and the motion deadline is always on the grid and in the future.
#[test]
fn the_band_is_due_at_most_once_per_frame_whatever_wakes_the_loop() {
    let mut app = on_screen_app();
    app.announce_toolchain_pass("installing 10 ALab program(s)", true);
    let t0 = Instant::now();
    let born = app.messages.frame_instant(t0);
    let mut frames: Vec<Instant> = Vec::new();
    let mut t = t0;
    while t < t0 + Duration::from_secs(3) {
        let d = app
            .band_motion_deadline(t)
            .expect("a live row on screen moves");
        assert!(d > t, "a deadline in the past spins the loop");
        let off = d.saturating_duration_since(born).as_millis() % ANIM_FRAME.as_millis();
        assert_eq!(off, 0, "a deadline off the grid");
        for wid in app.band_motion_due(t) {
            app.prepare_band_motion(wid, t);
            let at = app.windows[&wid].band_motion.as_ref().unwrap().at;
            if let Some(last) = frames.last() {
                assert!(at >= *last + ANIM_FRAME, "two frames in one grid step");
            }
            frames.push(at);
        }
        t += Duration::from_millis(7);
    }
    let cap = 3000 / ANIM_FRAME.as_millis() as usize + 1;
    assert!(
        frames.len() <= cap,
        "{} frames in 3 s (cap {cap})",
        frames.len()
    );
    assert!(
        frames.len() + 2 >= cap,
        "…and it does animate: {}",
        frames.len()
    );
}

/// ONE ANIMATION FRAME COSTS THE BAND ROW AND NOTHING ELSE: across two frame
/// instants the composed input differs in the band row only, so the
/// renderer's row diff (`compute_dirty_rows`, the damage both presents use)
/// repaints exactly that row — the damage a cursor blink costs (its one row);
/// the LAYOUT is reused (a poisoned cached presentation survives
/// `prepare_band_motion`: the width law did not run); a frame changes the
/// row's CHARACTERS only in the glyph cell and the time slots — everything
/// else it moves is the full-row meter's surface under them (rulings 138,
/// 140: one paint per frame, every word floored against the tone under it,
/// so there is no time-free paint to reuse); and no frame re-grids or
/// resizes the PTY.
#[test]
fn a_motion_frame_repaints_only_the_band_row_and_never_reruns_the_layout() {
    let mut app = on_screen_app();
    app.announce_toolchain_pass("installing 10 ALab program(s)", true);
    assert_eq!(app.message_band_rows, 1);
    let regrids = message_regrids();
    let dims = pty_dims(&app);
    let epoch = app
        .messages
        .on_glass()
        .find_map(|l| l.motion_since)
        .expect("on the glass");
    let t1 = epoch + Duration::from_millis(1200);
    app.prepare_band_motion_with(WID, t1, Look::MOVING);
    compose(&mut app);
    let before = app.windows[&WID].input_scratch.clone();
    let fp1 = app.windows[&WID].band_motion_fp;
    let layout = app.windows[&WID].band_layout.as_ref().unwrap().2.rows[0].clone();
    assert!(layout.busy, "the pass is busy work: the comet's row");

    // Poison the cached layout: a frame must read it, never rebuild it.
    {
        let ws = app.windows.get_mut(&WID).unwrap();
        ws.band_layout.as_mut().unwrap().2.rows[0].full_title = "POISON".into();
    }

    let t2 = t1 + ANIM_FRAME * 3;
    app.prepare_band_motion_with(WID, t2, Look::MOVING);
    assert_eq!(
        app.windows[&WID].band_layout.as_ref().unwrap().2.rows[0].full_title,
        "POISON",
        "a motion frame re-ran the width law"
    );
    assert_ne!(app.windows[&WID].band_motion_fp, fp1, "the comet moved");
    compose(&mut app);
    let after = app.windows[&WID].input_scratch.clone();
    let words_may_move = |x: usize| {
        x == aterm_messages::GLYPH_COL
            || layout
                .elapsed
                .is_some_and(|c| (c..c + aterm_messages::ELAPSED_W).contains(&x))
            || layout
                .eta
                .is_some_and(|c| (c..c + layout.eta_width()).contains(&x))
    };
    for (x, (a, b)) in before.cells[0].iter().zip(&after.cells[0]).enumerate() {
        if a.ch != b.ch {
            assert!(
                words_may_move(x),
                "col {x}: a word moved outside the time slots"
            );
        }
    }

    // The renderer's row damage between the two frames: the band row, nothing
    // else.
    let mut dirty = Vec::new();
    let decision =
        aterm_render::compute_dirty_rows(&before, &after, false, None, false, None, 24, &mut dirty);
    let rows_dirty: Vec<usize> = match decision {
        aterm_render::DirtyDecision::Rows(_) => dirty
            .iter()
            .enumerate()
            .filter(|(_, d)| **d)
            .map(|(r, _)| r)
            .collect(),
        aterm_render::DirtyDecision::FullRepaint => panic!("a motion frame forced a full repaint"),
    };
    assert_eq!(rows_dirty, [0], "only the band row is damaged");
    // A cursor blink on the same frame damages its one row too: a motion
    // frame is no more damage than a blink.
    let mut blink = Vec::new();
    let blink_rows = match aterm_render::compute_dirty_rows(
        &before, &before, true, None, false, None, 24, &mut blink,
    ) {
        aterm_render::DirtyDecision::Rows(_) => blink.iter().filter(|d| **d).count(),
        aterm_render::DirtyDecision::FullRepaint => usize::MAX,
    };
    assert!(rows_dirty.len() <= blink_rows.max(1), "blink {blink_rows}");

    // A hundred more frames: no re-grid, no PTY resize.
    for k in 0..100u32 {
        app.prepare_band_motion_with(WID, t2 + ANIM_FRAME * k, Look::MOVING);
        compose(&mut app);
    }
    assert_eq!(message_regrids(), regrids, "an animation never re-grids");
    assert_eq!(pty_dims(&app), dims, "…nor resizes the PTY");
}

// ---- (4) THE STILL LOOKS ------------------------------------------------------

/// SERIOUS MODE, REDUCE MOTION, `motion = "reduced"` AND THE LOAD-SHED LATCH:
/// each draws the still form — its meter does not change with time — and
/// arms no frame-rate deadline, for work with no fraction (the comet's row)
/// and for a determinate download alike: the only wakes are TEXT ticks at
/// most once a second (the download's ETA slot shows its elapsed clock
/// while the estimate is hidden, review round 3, 2026-09-24; the comet's
/// row has no words before 10 s, so it arms nothing here).
#[test]
fn every_still_gate_draws_static_frames_and_arms_no_frame_deadline() {
    type Gate = fn(&mut App, bool);
    let gates: [(&str, Gate); 4] = [
        ("Serious Mode", |a, on| a.serious_mode = on),
        ("Reduce Motion", |a, on| a.system_reduce_motion = on),
        ("motion = reduced", |a, on| {
            a.config.motion = on.then(|| "reduced".to_string());
        }),
        ("the load-shed latch", |a, on| a.perf_reduced = on),
    ];
    for (name, gate) in gates {
        for determinate in [false, true] {
            let mut app = on_screen_app();
            if determinate {
                app.post_message(download(30_000_000));
            } else {
                app.announce_toolchain_pass("installing 10 ALab program(s)", true);
            }
            assert_eq!(app.message_band_rows, 1, "{name}");
            let now = Instant::now();
            assert_eq!(app.band_look(WID).pace, Pace::Moving, "{name}: open");
            gate(&mut app, true);
            assert_eq!(app.band_look(WID).pace, Pace::Still, "{name}");
            let mut wakes = Vec::new();
            let mut t = now;
            while let Some(d) = app.band_motion_deadline(t) {
                if d > now + Duration::from_secs(3) {
                    break;
                }
                wakes.push(d);
                t = d;
            }
            assert!(
                determinate || wakes.is_empty(),
                "{name}: the comet's still row asked {wakes:?}"
            );
            assert!(
                wakes.len() <= 4
                    && wakes
                        .windows(2)
                        .all(|w| w[1] - w[0] >= Duration::from_millis(990)),
                "{name} (determinate {determinate}): a wake faster than a text tick: {:?}",
                wakes.iter().map(|w| *w - now).collect::<Vec<_>>()
            );
            let mut fps = Vec::new();
            let mut meters = Vec::new();
            for k in 0..90u32 {
                fps.push(app.prepare_band_motion(WID, now + ANIM_FRAME * k));
                let m = app.windows[&WID].band_motion.as_ref().expect("prepared");
                meters.push(m.rows.iter().map(|r| r.surface.clone()).collect::<Vec<_>>());
            }
            assert!(
                meters.windows(2).all(|w| w[0] == w[1]),
                "{name} (determinate {determinate}): a still meter moved"
            );
            assert!(
                fps.windows(2).filter(|w| w[0] != w[1]).count() <= 3,
                "{name} (determinate {determinate}): the still frame changed faster than its clock"
            );
            assert_ne!(fps[0], 0, "{name}: the still form is drawn");
            gate(&mut app, false);
            assert_eq!(app.band_look(WID).pace, Pace::Moving, "{name}: reopened");
        }
    }
}

// ---- (6) CAPTURES ---------------------------------------------------------------

/// A PRODUCTION CAPTURE IS STILL AND A PURE FUNCTION OF ITS INSTANT: a
/// headless window (no OS window — what `aterm --headless` and a capture of
/// it run) prepares the still form in its own look, the same instant twice
/// gives the same frame, and instants a few seconds apart give the same
/// meter until the first elapsed word arrives.
#[test]
fn a_headless_capture_is_still_and_a_pure_function_of_its_instant() {
    let mut app = App::headless_for_test();
    app.windows.get_mut(&WID).unwrap().focused = true;
    app.announce_toolchain_pass("installing 10 ALab program(s)", true);
    assert_eq!(app.band_look(WID).pace, Pace::Still, "headless never moves");
    let now = Instant::now();
    let shoot = |app: &mut App, at: Instant| {
        app.prepare_band_motion(WID, at);
        compose(app);
        app.windows[&WID].input_scratch.cells[0].clone()
    };
    let a = shoot(&mut app, now);
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(shoot(&mut app, now), a, "the same instant, the same frame");
    assert_eq!(
        shoot(&mut app, now + Duration::from_secs(4)),
        a,
        "the still form does not move with time"
    );
    // …but its WORDS are the capture's instant's (design ruling 202): a
    // capture after another reads its own elapsed clock, never the last
    // painted one (the live run's `1:59` at 120 s was the request time, not
    // the capture's; re-measured 2026-09-24: every capture read its instant).
    let text = |cells: &[aterm_core::terminal::RenderCell]| -> String {
        cells.iter().map(|c| c.ch).collect()
    };
    let clock = |row: &str| -> u64 {
        let word = row
            .split_whitespace()
            .find(|w| w.len() == 4 && w.as_bytes()[1] == b':')
            .unwrap_or_else(|| panic!("an elapsed clock in {row:?}"));
        word[2..].parse().unwrap()
    };
    // Half a second off the second boundary: the motion's 33 ms frame floor
    // (anchored to the center's birth, and 1 s is not a whole number of
    // frames) and the post-to-`now` gap can never carry a word across one.
    let at20 = text(&shoot(&mut app, now + Duration::from_millis(20_500)));
    let at21 = text(&shoot(&mut app, now + Duration::from_millis(21_500)));
    assert_eq!(clock(&at20), 20, "{at20:?}");
    assert_eq!(clock(&at21), 21, "{at21:?}");
    assert_eq!(
        text(&shoot(&mut app, now + Duration::from_millis(20_500))),
        at20,
        "back to an earlier instant, its words again"
    );
}

// ---- (7) THE FACES THAT MUST NOT MOVE ---------------------------------------

/// `appstatus` carries no motion: a live row's `phase=live` line is the same
/// bytes 30 s apart (no elapsed, no ETA, no frame state leaks into it), and
/// animating the band between the two reads changes nothing.
#[test]
fn appstatus_carries_no_motion_words() {
    let mut app = on_screen_app();
    app.post_message(download(30_000_000));
    app.announce_toolchain_pass("installing 10 ALab program(s)", true);
    let read = |app: &App, at: Instant| {
        aterm_messages::wire::activity_rows_compat(&app.messages, at, &crate::control::pct_encode)
    };
    let now = Instant::now();
    let first = read(&app, now);
    assert_eq!(first.len(), 2, "{first:?}");
    for k in 0..60u32 {
        app.prepare_band_motion(WID, now + ANIM_FRAME * k);
    }
    assert_eq!(read(&app, now + Duration::from_secs(30)), first);
}

/// THE PAGE STILL LISTS THE RECORDS: a `Hold::LogOnly` message never reaches
/// the glass, arms nothing, and is on Settings ▸ Messages' projection as
/// `recorded`, worded "recorded, never on the band", newest first.
#[test]
fn the_messages_page_lists_log_only_records() {
    let mut app = on_screen_app();
    let first = app.record_message(Message::new(
        tags::TOOLCHAIN,
        Severity::Info,
        "ALab tools installed",
    ));
    let second = app.record_message(
        Message::new(tags::UPDATE, Severity::Warn, "aterm update failed")
            .line("could not verify the download"),
    );
    assert_eq!(app.message_band_rows, 0, "never on the glass");
    assert_eq!(app.messages_deadline(), None);
    let state = app.messages_state();
    let ids: Vec<u64> = state.entries.iter().map(|e| e.id).collect();
    assert_eq!(ids, [second.raw(), first.raw()], "newest first");
    for id in [first, second] {
        let e = state.entry(id.raw()).expect("listed");
        assert_eq!(e.state, "recorded");
        assert_eq!(e.state_words(), "recorded", "one word (ruling 149)");
    }
    assert_eq!(
        state.entry(second.raw()).unwrap().detail,
        vec!["could not verify the download"],
        "its detail rides the page"
    );
}

/// MEASUREMENT (not a gate; run with `--ignored --nocapture`): what one
/// animation frame adds to the host's main thread — `prepare_band_motion`
/// (the motion layer at the frame instant) plus the band's splice (one paint
/// of the band's rows over the frame's surface) — beside
/// the compose every present already pays (the terminal's cell frame), which
/// is what a cursor-blink frame costs before the renderer. Debug-build
/// numbers: read them as a ratio, not as the release budget.
#[test]
#[ignore = "measurement: prints per-frame costs"]
fn measure_the_host_cost_of_one_motion_frame() {
    let mut app = on_screen_app();
    app.announce_toolchain_pass("installing 10 ALab program(s)", true);
    let t0 = Instant::now();
    let frames = 2000u32;
    compose(&mut app);
    let start = Instant::now();
    for k in 0..frames {
        app.prepare_band_motion_with(WID, t0 + ANIM_FRAME * k, Look::MOVING);
        app.splice_message_band(WID, aterm_render::Theme::default());
    }
    let band = start.elapsed() / frames;
    let (rows, cols) = {
        let ws = &app.windows[&WID];
        (ws.rows as usize, ws.cols as usize)
    };
    let terminal = app.front_terminal(WID).expect("front").term.clone();
    let start = Instant::now();
    for _ in 0..frames {
        let ws = app.windows.get_mut(&WID).unwrap();
        let mut term = term_lock(&terminal);
        term.cell_frame_into(&mut ws.input_scratch, rows, cols);
    }
    let blink = start.elapsed() / frames;
    let start = Instant::now();
    for k in 0..frames {
        let _ = app.band_motion_deadline(t0 + ANIM_FRAME * k);
    }
    let deadline = start.elapsed() / frames;
    println!(
        "per frame, {cols}x{rows}: band motion + splice {band:?}; the terminal's cell frame \
         (a blink's compose) {blink:?}; band_motion_deadline {deadline:?}"
    );
}
