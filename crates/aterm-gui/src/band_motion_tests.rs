// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-side message band motion, cadence, damage, and visibility tests.

use std::time::{Duration, Instant};

use aterm_messages::{
    ANIM_FRAME, Amount, ELAPSED_AFTER, Hold, Look, Message, Meter, Pace, Restatement, SHRINK_QUIET,
    STALE_UPDATE, Severity, Unit, tags,
};

use crate::messages_host::message_regrids;
use crate::{App, WindowId, term_lock};

const WID: WindowId = WindowId(0);

/// Headless `App` with window 0 on screen, focused, and every motion gate open.
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

fn pty_dims(app: &App) -> (u16, u16) {
    let terminal = app
        .front_terminal(WID)
        .expect("front terminal")
        .term
        .clone();
    let term = term_lock(&terminal);
    (term.rows(), term.cols())
}

/// An idle `App` arms no state or motion wake and contributes zero to `RepaintKey`.
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
                ws.band.motion_fp(),
            ),
            0,
            "{what}: the RepaintKey's band term"
        );
        assert_eq!(app.message_band_rows, 0, "{what}: a row");
    };
    idle(&mut app, "a fresh App");
    for title in ["ALab tools installed", "Config reloaded", "Update checked"] {
        app.record_message(Message::new(tags::SYSTEM, Severity::Info, title));
    }
    app.record_message(Message::new(tags::CONFIG, Severity::Warn, "Unknown key"));
    idle(&mut app, "after four records");
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

/// Hidden windows schedule no wakes; unfocused on-screen windows schedule only elapsed ticks.
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

/// Same-frame queries reuse the deadline scan until progress, layout, or focus changes.
#[test]
fn repeated_event_and_park_queries_share_the_next_visible_transition() {
    let mut app = on_screen_app();
    let id = app.post_message(download(10_000_000));
    let t0 = Instant::now();
    app.prepare_band_motion(WID, t0);
    let ws = &app.windows[&WID];
    let expected = app.messages.motion_deadline(
        ws.band.layout().expect("prepared layout"),
        ws.band.motion().expect("prepared frame").at,
        Look::MOVING,
    );
    assert!(expected.is_some(), "the live bar has a future transition");
    assert_eq!(app.band_motion_deadline(t0), expected);
    let first_scan = app.windows[&WID].band.deadline_computations();
    assert_eq!(first_scan, 1);
    for event in 0..64 {
        if event % 4 == 0 {
            app.prepare_band_motion(WID, t0);
        }
        assert_eq!(app.band_motion_deadline(t0), expected);
        assert!(app.band_motion_due(t0).is_empty());
    }
    assert_eq!(
        app.windows[&WID].band.deadline_computations(),
        first_scan,
        "same-frame event and park queries reuse the cell-surface scan"
    );
    let due = expected.unwrap();
    assert_eq!(app.band_motion_due(due), vec![WID]);
    app.prepare_band_motion(WID, due);
    assert!(app.band_motion_deadline(due).is_some_and(|next| next > due));
    let after_frame = app.windows[&WID].band.deadline_computations();
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
    let after_progress = app.windows[&WID].band.deadline_computations();
    assert_eq!(
        after_progress,
        after_frame + 1,
        "new progress retires the memo"
    );

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
    let after_heartbeat = app.windows[&WID].band.deadline_computations();
    assert_eq!(after_heartbeat, after_progress + 1);

    app.windows.get_mut(&WID).unwrap().cols += 1;
    let width_probe = due + Duration::from_millis(7);
    let _ = app.band_motion_deadline(width_probe);
    let after_width = app.windows[&WID].band.deadline_computations();
    assert_eq!(
        after_width,
        after_heartbeat + 1,
        "width changes the surface cells"
    );
    assert_eq!(
        app.windows[&WID].band.last_from(),
        Some(width_probe),
        "a layout mismatch starts from now, not the old frame"
    );
    app.windows.get_mut(&WID).unwrap().cols -= 1;
    app.prepare_band_motion(WID, due);
    app.windows.get_mut(&WID).unwrap().occluded = true;
    assert_eq!(app.band_motion_deadline(width_probe), None);
    assert!(app.band_motion_due(width_probe).is_empty());
    assert_eq!(app.windows[&WID].band.deadline_computations(), after_width);
    app.windows.get_mut(&WID).unwrap().occluded = false;
    app.windows.get_mut(&WID).unwrap().focused = false;
    let focus_probe = due + Duration::from_millis(11);
    let _ = app.band_motion_deadline(focus_probe);
    assert_eq!(
        app.windows[&WID].band.deadline_computations(),
        after_width + 1,
        "unfocus recomputes the next text-only transition"
    );
    assert_eq!(
        app.windows[&WID].band.last_from(),
        Some(focus_probe),
        "a look mismatch starts from now, not the old frame"
    );
}

/// Frequent loop wakeups advance the band at most once per `ANIM_FRAME` grid step.
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
            let at = app.windows[&wid].band.motion().unwrap().at;
            if let Some(last) = frames.last() {
                assert!(at >= *last + ANIM_FRAME, "two frames in one grid step");
            }
            frames.push(at);
        }
        t += Duration::from_millis(7);
    }
    // The first prepared frame may sit up to one grid step before `t0`.
    let first = *frames.first().expect("the band animates");
    let span = (t0 + Duration::from_secs(3)).saturating_duration_since(first);
    let cap = (span.as_millis() / ANIM_FRAME.as_millis()) as usize + 1;
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

/// Motion frames reuse cached layout, damage only the band row, and never resize the PTY.
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
    let fp1 = app.windows[&WID].band.motion_fp();
    let layout = app.windows[&WID].band.layout().unwrap().rows[0].clone();
    assert!(layout.busy, "the pass is busy work: the comet's row");

    let built = crate::message_band::BAND_LAYOUTS_BUILT.with(std::cell::Cell::get);

    let t2 = t1 + ANIM_FRAME * 3;
    app.prepare_band_motion_with(WID, t2, Look::MOVING);
    assert_eq!(
        crate::message_band::BAND_LAYOUTS_BUILT.with(std::cell::Cell::get),
        built,
        "a motion frame re-ran the width law"
    );
    assert_ne!(app.windows[&WID].band.motion_fp(), fp1, "the comet moved");
    compose(&mut app);
    let after = app.windows[&WID].input_scratch.clone();
    let words_may_move = |x: usize| {
        x == aterm_messages::GLYPH_COL
            || layout
                .elapsed
                .is_some_and(|c| (c..c + layout.elapsed_width()).contains(&x))
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

    let mut dirty = Vec::new();
    let decision = aterm_render::compute_dirty_rows(
        &before,
        &after,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, 24),
        &mut dirty,
    );
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
    let mut blink = Vec::new();
    let blink_rows = match aterm_render::compute_dirty_rows(
        &before,
        &before,
        true,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, 24),
        &mut blink,
    ) {
        aterm_render::DirtyDecision::Rows(_) => blink.iter().filter(|d| **d).count(),
        aterm_render::DirtyDecision::FullRepaint => usize::MAX,
    };
    assert!(rows_dirty.len() <= blink_rows.max(1), "blink {blink_rows}");

    for k in 0..100u32 {
        app.prepare_band_motion_with(WID, t2 + ANIM_FRAME * k, Look::MOVING);
        compose(&mut app);
    }
    assert_eq!(message_regrids(), regrids, "an animation never re-grids");
    assert_eq!(pty_dims(&app), dims, "…nor resizes the PTY");
}

/// Still-motion gates draw static meters and arm at most 1 Hz text ticks.
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
                let m = app.windows[&WID].band.motion().expect("prepared");
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

/// Headless captures render the still form as a pure function of the capture instant.
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
    assert_eq!(shoot(&mut app, now), a, "the same instant, the same frame");
    assert_eq!(
        shoot(&mut app, now + Duration::from_secs(4)),
        a,
        "the still form does not move with time"
    );
    let text = |cells: &[aterm_core::terminal::RenderCell]| -> String {
        cells.iter().map(|c| c.ch).collect()
    };
    let clock = |row: &str| -> u64 {
        let words: Vec<&str> = row.split_whitespace().collect();
        let at = words
            .windows(3)
            .position(|w| w[0] == "for" && w[2] == "s")
            .unwrap_or_else(|| panic!("the elapsed words in {row:?}"));
        words[at + 1].parse().unwrap()
    };
    // Sample 500 ms past each second boundary to avoid 33 ms frame-floor rounding.
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

/// `appstatus` output is invariant across time and band animation frames.
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

/// `Hold::LogOnly` messages stay off the glass and appear in Settings ▸ Messages newest first.
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
    }
    assert_eq!(
        state.entry(second.raw()).unwrap().detail,
        vec!["could not verify the download"],
        "its detail rides the page"
    );
}
