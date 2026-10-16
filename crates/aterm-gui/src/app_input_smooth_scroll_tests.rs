// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, WindowId, term_lock};
use std::time::Duration;

/// Seed the window's engine with enough output that scrollback exists.
fn seed_history(
    app: &App,
    wid: WindowId,
) -> std::sync::Arc<std::sync::Mutex<aterm_core::terminal::Terminal>> {
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    {
        let mut t = term_lock(&term);
        for i in 0..60 {
            t.process(format!("line {i}\r\n").as_bytes());
        }
        assert!(
            t.grid().scrollback_lines() >= 10,
            "test needs real scrollback"
        );
    }
    term
}

/// Mixed-DPI authority: wheel easing and the terminal's pixel-cell contract
/// read the target window's metric record even while the shared renderer
/// is activated at another size. The explicit inequality is the negative
/// control for reading `self.cell_size()`.
#[test]
fn wheel_and_terminal_pixel_geometry_use_the_target_windows_cell_size() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let shared = app.cell_size();
    app.windows.get_mut(&wid).unwrap().metrics.font_px += 18.0;
    let owned = app.win_cell_size(wid);
    assert_ne!(
        owned, shared,
        "fixture must keep the shared renderer at another window's geometry"
    );

    let term = seed_history(&app, wid);
    app.scroll_wheel_animated(wid, &term, 3);
    let glide = app.windows[&wid]
        .scroll_glide
        .as_ref()
        .expect("full motion arms the per-window glide");
    assert_eq!(glide.cell_h, owned.1 as i64);
    assert_eq!(glide.glide.target_px(), 3 * owned.1 as i64);
    assert_ne!(
        glide.glide.target_px(),
        3 * shared.1 as i64,
        "the old shared-renderer projection must be observably different"
    );

    // The no-op grid-size case is deliberate: apply_term_resize must refresh
    // the engine's pixel-cell contract even when rows/cols are unchanged.
    let (rows, cols) = {
        let ws = &app.windows[&wid];
        (ws.rows, ws.cols)
    };
    assert!(!app.apply_term_resize(wid, rows, cols));
    assert_eq!(
        term_lock(&term).cell_pixel_size(),
        (owned.0 as u16, owned.1 as u16)
    );
    assert_ne!(
        term_lock(&term).cell_pixel_size(),
        (shared.0 as u16, shared.1 as u16)
    );
}

/// A line-emitting wheel event at a shell prompt takes exactly one terminal
/// acquisition through the real input funnel (the seam's route decision):
/// the viewport scroll consumes the facts that lock read (`WheelViewport`)
/// instead of re-locking, for a chained notch and a fresh one alike.
#[test]
fn a_line_wheel_event_takes_exactly_one_terminal_lock() {
    use crate::input::{InputEvent, PixelOffset, Source};
    use aterm_types::mouse::WheelDir;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let _term = seed_history(&app, wid);
    let wheel = || InputEvent::Wheel {
        dir: WheelDir::Up,
        lines: 1,
        row: 0,
        col: 0,
        mods: 0,
        px_off: PixelOffset::CELL_ORIGIN,
    };
    let acq = crate::term_lock_acquisitions_on_this_thread;
    let before = acq();
    app.input(wid, wheel(), Source::Human);
    assert_eq!(acq() - before, 1, "a fresh notch: the seam's one lock");
    assert!(
        app.windows[&wid].scroll_glide.is_some(),
        "…and it armed the glide from the facts that lock carried"
    );
    let before = acq();
    app.input(wid, wheel(), Source::Human);
    assert_eq!(
        acq() - before,
        1,
        "a chained notch: still the seam's one lock"
    );
    let cell_h = app.win_cell_size(wid).1.max(1) as i64;
    // Two notches. A notch is one row except on Windows, where it is the
    // desktop's lines-per-detent (`input::wheel_platform_lines`: the live
    // `SPI_GETWHEELSCROLLLINES`, 3 by default, so six rows there).
    let notch_rows = i64::from(crate::input::wheel_platform_lines(
        1,
        app.windows[&wid].rows,
    ));
    assert_eq!(
        app.windows[&wid]
            .scroll_glide
            .as_ref()
            .map(|st| st.glide.target_px()),
        Some(2 * notch_rows * cell_h),
        "the chain retargeted to two notches' rows from the carried facts"
    );
}

/// Direct manipulation: a chained precise (trackpad) delta tracks, moving the
/// band by exactly its pixels and turning the glide into a rest-settle to the
/// nearest whole row (`end() == now + TRACK_REST_MS + TRACK_SETTLE_MS`); a
/// chained notch retargets (a fresh `GLIDE_MS` ease). Both land exactly and
/// self-disarm, on one synthetic clock (`scroll_wheel_animated_at`).
#[test]
fn a_precise_chained_delta_tracks_the_finger_and_a_notch_retargets() {
    use crate::scroll_motion::{TRACK_REST_MS, TRACK_SETTLE_MS};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    let cell_h = app.win_cell_size(wid).1.max(1) as i64;
    assert!(cell_h >= 4, "fixture: a cell tall enough to split");
    let state = |app: &App| {
        app.windows[&wid]
            .scroll_glide
            .as_ref()
            .map(|st| (st.glide.end(), st.glide.target_px(), st.engine_row))
            .expect("a glide is armed")
    };

    // Precise chain: a third of a row, then another third.
    let third = (cell_h / 3) as f64;
    let before = std::time::Instant::now();
    app.scroll_wheel_animated_at(wid, &term, 0, None, Some(third), before);
    let (end, target, engine_row) = state(&app);
    let rest = std::time::Duration::from_millis(TRACK_REST_MS + TRACK_SETTLE_MS);
    assert!(
        end >= before + rest,
        "a tracked glide's deadline is the rest window plus the settle"
    );
    assert_eq!(
        target, 0,
        "a third of a row settles back to row 0 (nearest)"
    );
    assert_eq!(
        engine_row, 1,
        "the ceil pairing parked the engine one row deeper…"
    );
    assert_eq!(term_lock(&term).grid().display_offset(), 1);
    assert_eq!(
        i64::from(app.windows[&wid].scroll_frac_px),
        cell_h - third as i64,
        "…and pulled the band back by the remainder: the position IS the pixels"
    );
    // The finger's next delta, one 120 Hz report later (inside the rest window).
    let second = before + Duration::from_millis(8);
    app.scroll_wheel_animated_at(wid, &term, 0, None, Some(third), second);
    let (end_after, target_after, _) = state(&app);
    assert!(end_after >= end, "each delta re-arms the rest window");
    assert_eq!(
        i64::from(app.windows[&wid].scroll_frac_px),
        cell_h - 2 * (third as i64),
        "the second delta moved the band by exactly its pixels — no ease"
    );
    // Two thirds is nearer the next row when cell_h/3*2 > cell_h/2.
    let expect_target = crate::scroll_motion::nearest_row_px(2 * (third as i64), cell_h);
    assert_eq!(
        target_after, expect_target,
        "the settle target is the nearest row"
    );
    // The settle lands on the whole row at end() and disarms.
    app.tick_scroll_glide(wid, end_after + Duration::from_millis(1));
    assert_eq!(
        term_lock(&term).grid().display_offset() as i64,
        expect_target / cell_h,
        "the rest-settle lands on the nearest whole row"
    );
    assert_eq!(app.windows[&wid].scroll_frac_px, 0, "…whole-row at rest");
    assert!(
        app.windows[&wid].scroll_glide.is_none(),
        "…and disarms there"
    );

    // Notch chain: the ease restarts (the deadline moves out with it). The
    // first notch comes after the settle landed; the second 5 ms later.
    let landed = term_lock(&term).grid().display_offset() as i64;
    let notch_at = end_after + Duration::from_millis(2);
    app.scroll_wheel_animated_at(wid, &term, 1, None, None, notch_at);
    let (end, target, _) = state(&app);
    assert_eq!(target, (landed + 1) * cell_h);
    let chained_at = notch_at + Duration::from_millis(5);
    app.scroll_wheel_animated_at(wid, &term, 1, None, None, chained_at);
    let (end_after, target_after, _) = state(&app);
    assert!(
        end_after > end,
        "a notch chain retargets: a fresh 180 ms ease"
    );
    assert_eq!(target_after, (landed + 2) * cell_h);
    app.tick_scroll_glide(wid, end_after + Duration::from_millis(1));
    assert_eq!(term_lock(&term).grid().display_offset() as i64, landed + 2);
    assert!(app.windows[&wid].scroll_glide.is_none());
}

/// Direct manipulation through the winit handler: a sub-row precise delta moves
/// the band before the seam banks a whole row. The engine sits at the ceil row
/// with the remainder in `scroll_frac_px` while the seam's bank keeps the
/// fraction; the gesture start pays at most two locks (route probe, row
/// landing), and a second sub-row delta inside the same row pays none.
#[test]
fn a_sub_row_precise_delta_moves_the_band_before_a_whole_row_banks() {
    use winit::dpi::PhysicalPosition;
    use winit::event::MouseScrollDelta;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    let cell_h = app.win_cell_size(wid).1.max(1) as i64;
    assert!(cell_h >= 4, "fixture: a cell tall enough to split");
    let dy = (cell_h / 3) as f64;
    let acq = crate::term_lock_acquisitions_on_this_thread;
    // One instant for both deltas: the second is the same finger still
    // moving, inside the rest window. On the wall clock `TRACK_REST_MS`
    // could pass between the sends and the rest-settle would move the band.
    let t0 = std::time::Instant::now();
    crate::app_input::pin_wheel_clock(Some(t0));

    let before = acq();
    app.on_mouse_wheel(
        wid,
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, dy)),
    );
    let locks = acq() - before;
    assert!(
        locks <= 2,
        "gesture start: the route probe and the row landing, no more (took {locks})"
    );
    {
        let ws = &app.windows[&wid];
        assert!(
            ws.scroll_residual > 0.0 && ws.scroll_residual < 1.0,
            "the seam's bank still carries the sub-row fraction ({})",
            ws.scroll_residual
        );
        assert!(
            ws.scroll_glide.is_some(),
            "a sub-row precise delta arms the tracked band"
        );
        assert_eq!(
            i64::from(ws.scroll_frac_px),
            cell_h - dy as i64,
            "the band is pulled back by the remainder"
        );
    }
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        1,
        "the ceil pairing parks the engine one row deeper"
    );

    // A second sub-row delta within the same row: no engine lock at all.
    let before = acq();
    app.on_mouse_wheel(
        wid,
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 1.0)),
    );
    assert_eq!(
        acq() - before,
        0,
        "sub-row motion inside a row never touches the engine"
    );
    assert_eq!(
        i64::from(app.windows[&wid].scroll_frac_px),
        cell_h - dy as i64 - 1
    );
    assert_eq!(term_lock(&term).grid().display_offset(), 1);
    // And a finger that rests past the rest window settles: the same second
    // delta sent after `TRACK_REST_MS` finds the band part-way home.
    crate::app_input::pin_wheel_clock(Some(
        t0 + std::time::Duration::from_millis(crate::scroll_motion::TRACK_REST_MS + 60),
    ));
    app.on_mouse_wheel(
        wid,
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 1.0)),
    );
    crate::app_input::pin_wheel_clock(None);
    let rested = i64::from(app.windows[&wid].scroll_frac_px);
    assert!(
        rested > cell_h - dy as i64 - 2 && rested < cell_h,
        "a delta after the rest window tracks from the settling band, not the parked one \
         ({rested})"
    );
}

/// The card owns the wheel, whatever device made it: sub-row trackpad deltas
/// under the tab context menu (answered before the seam) arm no band, shift and
/// scroll nothing, take no engine lock, and leave the card up.
#[test]
fn a_sub_row_precise_delta_under_the_tab_menu_is_swallowed_like_a_notch() {
    use winit::dpi::PhysicalPosition;
    use winit::event::MouseScrollDelta;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    let term = seed_history(&app, wid);
    let cell_h = app.win_cell_size(wid).1.max(1) as i64;
    assert!(cell_h >= 4, "fixture: a cell tall enough to split");
    let dy = (cell_h / 4) as f64;
    let acq = crate::term_lock_acquisitions_on_this_thread;
    let t0 = std::time::Instant::now();
    crate::app_input::pin_wheel_clock(Some(t0));
    assert!(
        app.open_tab_context_menu_at_chip(wid, 0),
        "the card opens at the chip"
    );

    // Two quarter-row deltas: half a row, so no whole row banks and both
    // take the sub-notch half.
    let before = acq();
    for _ in 0..2 {
        app.on_mouse_wheel(
            wid,
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, dy)),
        );
    }
    assert_eq!(
        acq() - before,
        0,
        "the card's delta never probes the engine"
    );
    {
        let ws = &app.windows[&wid];
        assert!(ws.tab_menu.is_some(), "the delta did not dismiss the card");
        assert!(ws.scroll_glide.is_none(), "…nor arm a band under it");
        assert_eq!(ws.scroll_frac_px, 0, "…nor shift the presented band");
        assert!(ws.overscroll.is_none());
    }
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        0,
        "…nor scroll the viewport underneath it"
    );

    // Negative control: the card down, one more quarter row (the seam's
    // bank now holds three quarters, still no whole row) moves the band.
    assert!(app.close_tab_menu(wid));
    app.on_mouse_wheel(
        wid,
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, dy)),
    );
    crate::app_input::pin_wheel_clock(None);
    let ws = &app.windows[&wid];
    assert!(
        ws.scroll_glide.is_some(),
        "the same delta arms the tracked band once the card is gone"
    );
    assert_eq!(i64::from(ws.scroll_frac_px), cell_h - dy as i64);
    assert_eq!(term_lock(&term).grid().display_offset(), 1);
}

#[test]
fn tab_menu_discards_banked_wheel_fractions_at_both_boundaries() {
    use winit::dpi::PhysicalPosition;
    use winit::event::MouseScrollDelta;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    let _term = seed_history(&app, wid);
    let (cell_w, cell_h) = app.win_cell_size(wid);
    let vertical = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, cell_h as f64 / 2.0));
    let horizontal = MouseScrollDelta::PixelDelta(PhysicalPosition::new(cell_w as f64 / 2.0, 0.0));

    // A previous grid gesture must not contribute credit to the card.
    assert!(app.wheel_notches(wid, vertical).is_none());
    assert!(app.wheel_notches(wid, horizontal).is_none());
    assert_eq!(app.windows[&wid].scroll_residual, 0.5);
    assert_eq!(app.windows[&wid].scroll_residual_x, 0.5);
    assert!(app.open_tab_context_menu_at_chip(wid, 0));
    assert_eq!(app.windows[&wid].scroll_residual, 0.0);
    assert_eq!(app.windows[&wid].scroll_residual_x, 0.0);

    // Both swipes are swallowed by the card, including their fractional
    // banks. Closing it must discard that credit before the grid resumes.
    app.on_mouse_wheel(wid, vertical);
    app.on_mouse_wheel(wid, horizontal);
    assert_eq!(app.windows[&wid].scroll_residual, 0.5);
    assert_eq!(app.windows[&wid].scroll_residual_x, 0.5);
    assert!(app.close_tab_menu(wid));
    assert_eq!(app.windows[&wid].scroll_residual, 0.0);
    assert_eq!(app.windows[&wid].scroll_residual_x, 0.0);

    // Negative control: without the close reset, these half-row/cell
    // deltas would pay off the swallowed halves and emit phantom notches.
    assert!(app.wheel_notches(wid, vertical).is_none());
    assert!(app.wheel_notches(wid, horizontal).is_none());
    assert_eq!(app.windows[&wid].scroll_residual, 0.5);
    assert_eq!(app.windows[&wid].scroll_residual_x, 0.5);
}

/// Direct manipulation: a 120 Hz precise stream at 1 row per 100 ms through the
/// winit handler moves the presented position (engine row × cell_h − frac)
/// evenly (`max <= 2 * min`, no zero-motion event after the first) and within a
/// pixel of the finger. The handler reads the real clock, so only a stall longer
/// than `TRACK_REST_MS` between events could start a settle mid-stream.
#[test]
fn a_constant_speed_precise_stream_moves_the_band_evenly() {
    use winit::dpi::PhysicalPosition;
    use winit::event::MouseScrollDelta;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    let cell_h = app.win_cell_size(wid).1.max(1) as i64;
    assert!(
        cell_h >= 12,
        "fixture: dy = cell_h*8.333/100 must be >= 1 px so no rounded step is zero"
    );
    let dy = cell_h as f64 * 8.333 / 100.0; // 1 row per 100 ms at 120 Hz
    let presented = |app: &App| -> i64 {
        let ws = &app.windows[&wid];
        term_lock(&term).grid().display_offset() as i64 * cell_h - i64::from(ws.scroll_frac_px)
    };
    let mut finger = 0.0f64;
    let mut prev = presented(&app);
    let mut vels: Vec<i64> = Vec::new();
    for i in 1..=60u32 {
        app.on_mouse_wheel(
            wid,
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, dy)),
        );
        finger += dy;
        let pos = presented(&app);
        assert!(
            (pos as f64 - finger).abs() <= 1.0,
            "1:1 — presented {pos} vs finger {finger:.2} at event {i}"
        );
        if i > 1 {
            vels.push(pos - prev);
        }
        prev = pos;
    }
    let (mx, mn) = (*vels.iter().max().unwrap(), *vels.iter().min().unwrap());
    assert!(
        mn >= 1 && mx <= 2 * mn,
        "even motion: max {mx} px/event, min {mn} ({vels:?})"
    );
}

/// Gesture end: winit's `TouchPhase::Ended` (usually a zero delta, which
/// the axis guard drops) releases the tracked band, so its settle to the
/// nearest whole row starts now instead of `TRACK_REST_MS` later. A notch
/// glide is untouched by a release.
#[test]
fn a_gesture_end_starts_the_rest_settle_at_once() {
    use crate::scroll_motion::TRACK_SETTLE_MS;
    use winit::dpi::PhysicalPosition;
    use winit::event::{MouseScrollDelta, TouchPhase};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let _term = seed_history(&app, wid);
    let cell_h = app.win_cell_size(wid).1.max(1) as i64;
    let dy = (cell_h / 3) as f64;
    app.on_mouse_wheel_phased(
        wid,
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, dy)),
        TouchPhase::Moved,
    );
    let end_before = app.windows[&wid]
        .scroll_glide
        .as_ref()
        .expect("armed")
        .glide
        .end();
    app.on_mouse_wheel_phased(
        wid,
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 0.0)),
        TouchPhase::Ended,
    );
    let end_after = app.windows[&wid]
        .scroll_glide
        .as_ref()
        .expect("still armed")
        .glide
        .end();
    assert!(
        end_after < end_before,
        "the finger lifted: the settle no longer waits out the rest window"
    );
    assert!(
        end_after <= std::time::Instant::now() + std::time::Duration::from_millis(TRACK_SETTLE_MS),
        "…it is exactly one settle away"
    );
    // …and the loop's armed wake moved with it: one panel period out, not the
    // rest boundary (which would present almost the whole settle in one frame).
    let iv = app.windows[&wid]
        .frame_interval
        .unwrap_or(app.frame_interval);
    let st = app.windows[&wid]
        .scroll_glide
        .as_ref()
        .expect("still armed");
    assert!(
        st.wake_deadline() <= std::time::Instant::now() + iv,
        "the loop's armed wake is one period out, not the old rest boundary"
    );
}

/// The fence the direct-manipulation path must not cross: under a tracking app
/// a precise stream produces exactly the seam's bytes (one wheel report per
/// whole banked row, the engine's own encoding) and moves no band. Sub-row
/// motion exists only on the tracking-off route, which keeps
/// `bytes_human_eq_controller` true on the sub-notch path.
#[cfg(unix)]
#[test]
fn a_precise_stream_under_a_tracking_app_reports_whole_rows_only_and_moves_no_band() {
    use aterm_types::mouse::WheelDir;
    use std::io::Read;
    use std::os::fd::FromRawFd;
    use winit::dpi::PhysicalPosition;
    use winit::event::MouseScrollDelta;
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0, "pipe");
    aterm_pty::set_nonblocking(pipe[0], true).expect("nonblocking pipe reader");
    let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(pipe[1]));
    let mut app = App::headless_for_test_with_sink(sink);
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    let expected = {
        let mut t = term_lock(&term);
        t.process(b"\x1b[?1000h\x1b[?1006h");
        t.encode_mouse_wheel(WheelDir::Up, 0, 0, 0)
            .expect("SGR tracking encodes a wheel report")
    };
    let cell_h = app.win_cell_size(wid).1.max(1) as f64;
    // Ten quarter-row deltas: 2.5 rows, of which exactly 2 whole rows bank.
    for _ in 0..10 {
        app.on_mouse_wheel(
            wid,
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, cell_h / 4.0)),
        );
    }
    let mut reader = unsafe { std::fs::File::from_raw_fd(pipe[0]) };
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    // A hang detector, never a latency budget.
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while std::time::Instant::now() < deadline {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if got.len() >= 2 * expected.len() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(e) => panic!("pipe read: {e}"),
        }
    }
    assert_eq!(
        got,
        expected.repeat(2),
        "exactly two whole-row reports, the engine's own bytes"
    );
    let ws = &app.windows[&wid];
    assert!(
        ws.scroll_glide.is_none(),
        "a tracking app gets no local band"
    );
    assert_eq!(ws.scroll_frac_px, 0);
    assert!(ws.overscroll.is_none());
    assert_eq!(term_lock(&term).grid().display_offset(), 0);
    unsafe {
        libc::close(pipe[1]);
    }
}

/// A band parked on the live bottom, pushed further (a flick's momentum tail),
/// takes no lock: the gesture's first delta probes the route once and parks the
/// band on live with the bounce released, and every further downward delta keeps
/// the bounce (a positive shift) and the position lock-free. Reversing off the
/// bound pans into history with one lock for the row it enters.
#[test]
fn a_precise_push_past_the_live_bottom_takes_no_lock_once_parked() {
    use winit::dpi::PhysicalPosition;
    use winit::event::MouseScrollDelta;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    let c = app.win_cell_size(wid).1.max(1) as f64;
    let precise = |dy: f64| MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, dy));
    let acq = crate::term_lock_acquisitions_on_this_thread;
    // One instant for the whole gesture: the band never reaches its rest
    // window between two deltas of the same finger.
    crate::app_input::pin_wheel_clock(Some(std::time::Instant::now()));

    let before = acq();
    app.on_mouse_wheel(wid, precise(-c / 2.0));
    assert_eq!(acq() - before, 1, "the gesture's first look at the engine");
    assert_eq!(term_lock(&term).grid().display_offset(), 0);
    {
        let ws = &app.windows[&wid];
        assert!(ws.scroll_glide.is_some(), "the band is armed on live");
        assert!(ws.overscroll.is_some(), "the rubber-band is released");
        assert!(ws.scroll_frac_px > 0, "a live-bottom bounce lifts the band");
    }

    // Eight more half-row deltas: four of them bank a whole row.
    let before = acq();
    for _ in 0..8 {
        app.on_mouse_wheel(wid, precise(-c / 2.0));
    }
    assert_eq!(
        acq() - before,
        0,
        "parked on live: no acquisition per momentum delta, banked rows included"
    );
    {
        let ws = &app.windows[&wid];
        assert!(ws.overscroll.is_some(), "still bouncing");
        assert!(ws.scroll_frac_px > 0);
    }
    assert_eq!(term_lock(&term).grid().display_offset(), 0, "still on live");

    // Reversing off the bound enters history: one lock for the row.
    let before = acq();
    app.on_mouse_wheel(wid, precise(c / 2.0));
    assert_eq!(acq() - before, 1, "a row crossing: one lock");
    assert!(
        app.windows[&wid].overscroll.is_none(),
        "off the edge, no bounce"
    );
    assert_eq!(term_lock(&term).grid().display_offset(), 1);
    crate::app_input::pin_wheel_clock(None);
}

/// The twin on the top row of history: a band parked on the top, pushed
/// further up, takes no lock for whole-row deltas, keeps its bounce (down:
/// a negative shift) and stays on the top row.
#[test]
fn a_precise_push_past_the_top_of_history_takes_no_lock_once_parked() {
    use winit::dpi::PhysicalPosition;
    use winit::event::MouseScrollDelta;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    let top = {
        let mut t = term_lock(&term);
        t.scroll_display(i32::MAX);
        t.grid().display_offset()
    };
    assert!(top > 0, "fixture: parked on the top of a real history");
    let c = app.win_cell_size(wid).1.max(1) as f64;
    let precise = |dy: f64| MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, dy));
    let acq = crate::term_lock_acquisitions_on_this_thread;
    crate::app_input::pin_wheel_clock(Some(std::time::Instant::now()));

    let before = acq();
    app.on_mouse_wheel(wid, precise(c / 2.0));
    assert_eq!(acq() - before, 1, "the gesture's first look at the engine");
    assert!(
        app.windows[&wid].scroll_frac_px < 0,
        "a top bounce sags the band"
    );

    let before = acq();
    for _ in 0..8 {
        app.on_mouse_wheel(wid, precise(c / 2.0));
    }
    assert_eq!(
        acq() - before,
        0,
        "parked on the top: no acquisition per momentum delta, banked rows included"
    );
    let ws = &app.windows[&wid];
    assert!(ws.overscroll.is_some(), "still bouncing");
    assert!(ws.scroll_frac_px < 0);
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        top,
        "still on the top"
    );
    crate::app_input::pin_wheel_clock(None);
}

/// A band parked on live when a pager comes up (alt screen + DEC 1007) is no
/// longer aterm's to bounce: the lock-free mode word says so, and each banked
/// row reaches the seam as the pager's arrow key.
#[cfg(unix)]
#[test]
fn a_pager_that_comes_up_under_a_parked_band_still_gets_its_arrows() {
    use std::io::Read;
    use std::os::fd::FromRawFd;
    use winit::dpi::PhysicalPosition;
    use winit::event::MouseScrollDelta;
    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0, "pipe");
    aterm_pty::set_nonblocking(pipe[0], true).expect("nonblocking pipe reader");
    let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(pipe[1]));
    let mut app = App::headless_for_test_with_sink(sink);
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    let c = app.win_cell_size(wid).1.max(1) as f64;
    let precise = |dy: f64| MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, dy));
    crate::app_input::pin_wheel_clock(Some(std::time::Instant::now()));
    // Park the band on live.
    app.on_mouse_wheel(wid, precise(-c / 2.0));
    assert!(
        app.windows[&wid].scroll_glide.is_some(),
        "fixture: the band is armed on live"
    );
    // The pager comes up mid-gesture.
    term_lock(&term).process(b"\x1b[?1007h\x1b[?1049h");
    // Four more half rows down: two whole rows bank.
    for _ in 0..4 {
        app.on_mouse_wheel(wid, precise(-c / 2.0));
    }
    crate::app_input::pin_wheel_clock(None);
    let mut reader = unsafe { std::fs::File::from_raw_fd(pipe[0]) };
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    // A hang detector, never a latency budget.
    let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while std::time::Instant::now() < deadline {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if got.len() >= 2 * b"\x1b[B".len() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(e) => panic!("pipe read: {e}"),
        }
    }
    let arrow =
        b"\x1b[B".repeat(crate::input::wheel_platform_lines(1, app.windows[&wid].rows) as usize);
    assert_eq!(
        got,
        arrow.repeat(2),
        "each banked row is the pager's arrow key, not a bounce"
    );
    unsafe {
        libc::close(pipe[1]);
    }
}

/// The bounce owns the band at a history end: with a tracked glide parked at
/// the top beside a live spring, every served instant presents the spring's
/// displacement. Non-vacuous: at least one pass serves the glide alone.
#[test]
fn a_glide_tick_at_a_history_end_never_snaps_a_live_bounce() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    app.windows.get_mut(&wid).unwrap().frame_interval = Some(Duration::from_micros(8_333));
    term_lock(&term).scroll_display(i32::MAX);
    app.scroll_wheel_animated_with(wid, &term, 0, None, Some(40.0));
    assert!(
        app.windows[&wid].overscroll.is_some() && app.windows[&wid].scroll_glide.is_some(),
        "fixture: a tracked band parked at the top beside a live bounce"
    );
    let mut glide_alone = 0;
    for _ in 0..500 {
        let ws = &app.windows[&wid];
        let Some(sp) = ws.overscroll.as_ref() else {
            break;
        };
        let bounce_at = sp.wake_deadline();
        let glide_at = ws.scroll_glide.as_ref().map(|st| st.wake_deadline());
        let t = glide_at.map_or(bounce_at, |g| g.min(bounce_at));
        if glide_at.is_some_and(|g| g < bounce_at) {
            glide_alone += 1;
        }
        app.service_due_scroll_motion(t);
        let ws = &app.windows[&wid];
        if let Some(sp) = ws.overscroll.as_ref() {
            assert_eq!(
                ws.scroll_frac_px,
                sp.sample(t).0.round() as i32,
                "the live bounce's displacement is what the band presents"
            );
        }
    }
    assert!(
        glide_alone > 0,
        "non-vacuous: some pass served the glide alone"
    );
}

/// The event path obeys the same rule. A momentum tail keeps sending small
/// deltas at the edge after the push that released the bounce: an excess of
/// 1-3 px that resists to a zero impulse, and a sub-pixel delta that rounds
/// onto the edge (no excess), must both leave the bounce live and presented.
#[test]
fn a_small_delta_at_a_history_end_keeps_the_bounce_presented() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    term_lock(&term).scroll_display(i32::MAX);
    let t0 = std::time::Instant::now();
    app.scroll_wheel_animated_at(wid, &term, 0, None, Some(40.0), t0);
    assert!(
        app.windows[&wid].overscroll.is_some(),
        "fixture: the push past the top released a bounce"
    );
    for (ms, dy) in [(1, 2.0), (2, 0.3)] {
        let t = t0 + Duration::from_millis(ms);
        app.scroll_wheel_animated_at(wid, &term, 0, None, Some(dy), t);
        let ws = &app.windows[&wid];
        let sp = ws
            .overscroll
            .as_ref()
            .unwrap_or_else(|| panic!("a {dy} px delta at the edge kept the bounce"));
        let want = sp.sample(t).0.round() as i32;
        assert_ne!(want, 0, "non-vacuous: the bounce is displaced at +{ms} ms");
        assert_eq!(
            ws.scroll_frac_px, want,
            "a {dy} px delta at the edge presents the bounce, not the band at rest"
        );
    }
}

/// …and a tracked band that a partial history drop lands on live presents at
/// rest: its rest target rounds half a cell toward history, so the parked start
/// can sink below live, and must not lift the live screen with no row under it.
#[test]
fn a_partial_drop_that_lands_a_tracked_band_on_live_presents_it_at_rest() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    let cell_h = app.win_cell_size(wid).1.max(1) as f64;
    term_lock(&term).scroll_display(2);
    let t0 = std::time::Instant::now();
    // Park a tracked band half a row below row 2: still offset 2.
    app.scroll_wheel_animated_at(wid, &term, 0, None, Some(-cell_h / 2.0), t0);
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        2,
        "fixture: parked on row 2"
    );
    // History shrinks to one row: the engine's offset drops 2 -> 1.
    term_lock(&term).set_scrollback_line_limit(Some(1));
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        1,
        "fixture: a partial drop"
    );
    // The finger moves on to 0.9 of a row: the rest target rounds to row
    // 1, which the drop shifts onto live, and the start below it.
    let t = t0 + Duration::from_millis(1);
    app.scroll_wheel_animated_at(wid, &term, 0, None, Some(-0.6 * cell_h), t);
    assert_eq!(term_lock(&term).grid().display_offset(), 0);
    assert_eq!(
        app.windows[&wid].scroll_frac_px, 0,
        "no residual lifts the live screen"
    );
}

/// …but a bounce belongs to its own edge: one precise delta that crosses the
/// whole history from a sagging top bounce lands at rest on live, whether it
/// lands exactly or overshoots.
#[test]
fn a_delta_that_crosses_the_history_ends_the_other_edges_bounce() {
    for overshoot in [0.0, 2.0] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = seed_history(&app, wid);
        term_lock(&term).scroll_display(i32::MAX);
        let t0 = std::time::Instant::now();
        app.scroll_wheel_animated_at(wid, &term, 0, None, Some(40.0), t0);
        let max_px = {
            let ws = &app.windows[&wid];
            assert!(
                ws.scroll_frac_px < 0 && ws.overscroll.is_some(),
                "fixture: the push past the top released a sagging bounce"
            );
            let st = ws
                .scroll_glide
                .as_ref()
                .expect("fixture: the band is armed");
            st.max_rows * st.cell_h
        };
        let t = t0 + Duration::from_millis(1);
        let dy = -(max_px as f64) - overshoot;
        app.scroll_wheel_animated_at(wid, &term, 0, None, Some(dy), t);
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            0,
            "fixture: the {dy} px delta crossed to the live bottom"
        );
        let ws = &app.windows[&wid];
        assert!(
            ws.overscroll.is_none(),
            "overshoot {overshoot}: the top bounce ended at the live bottom"
        );
        assert_eq!(
            ws.scroll_frac_px, 0,
            "overshoot {overshoot}: the live bottom presents at rest, not the top's sag"
        );
    }
}

/// Alt over a main-screen tracking app scrolls locally, all of the finger: Alt
/// is the selection-custody override that routes the wheel to the local
/// viewport, so sub-row deltas track too (tier 1 of `track_precise_scroll`
/// exempts Alt as well as Shift). Four quarter-row deltas present one row.
#[test]
fn alt_over_a_main_screen_tracking_app_tracks_every_pixel() {
    use winit::dpi::PhysicalPosition;
    use winit::event::MouseScrollDelta;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    term_lock(&term).process(b"\x1b[?1000h\x1b[?1006h");
    app.windows.get_mut(&wid).unwrap().mods = winit::keyboard::ModifiersState::ALT;
    let cell_h = app.win_cell_size(wid).1.max(1) as i64;
    assert!(cell_h >= 8, "fixture: a cell tall enough to quarter");
    for _ in 0..4 {
        app.on_mouse_wheel(
            wid,
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, (cell_h / 4) as f64)),
        );
    }
    let ws = &app.windows[&wid];
    assert!(
        ws.scroll_glide.is_some(),
        "Alt on the main screen scrolls the local band"
    );
    let presented = i64::try_from(term_lock(&term).grid().display_offset()).unwrap() * cell_h
        - i64::from(ws.scroll_frac_px);
    assert!(
        (presented - 4 * (cell_h / 4)).abs() <= 1,
        "the band moved by every delta's pixels: presented {presented} px of {}",
        4 * (cell_h / 4)
    );
}

/// The other fence: the controller `mouse` verb has no pixel field, so its
/// wheel is a notch: the glide retargets (a fresh `GLIDE_MS` ease per verb),
/// its target is whole-row, and `wheel_precise_px` is never set.
#[test]
fn the_mouse_verb_wheel_is_a_notch_and_never_tracks() {
    use crate::input::Source;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    let cell_h = app.win_cell_size(wid).1.max(1) as i64;
    // The event `mouse wheelup … lines=N` parses into
    // (`control::control_input::parse_mouse`): a whole-row notch.
    let verb = |lines: i32| crate::input::InputEvent::Wheel {
        dir: aterm_types::mouse::WheelDir::Up,
        lines,
        row: 0,
        col: 0,
        mods: 0,
        px_off: crate::input::PixelOffset::CELL_ORIGIN,
    };
    app.input(wid, verb(2), Source::Controller);
    let (end, target) = {
        let st = app.windows[&wid]
            .scroll_glide
            .as_ref()
            .expect("a notch glide");
        (st.glide.end(), st.glide.target_px())
    };
    assert_eq!(target, 2 * cell_h, "whole rows, from the verb's `lines=`");
    assert_eq!(app.windows[&wid].wheel_precise_px, None);
    std::thread::sleep(Duration::from_millis(5));
    app.input(wid, verb(1), Source::Controller);
    {
        let st = app.windows[&wid]
            .scroll_glide
            .as_ref()
            .expect("still a glide");
        assert!(
            st.glide.end() > end,
            "a chained verb RETARGETS (fresh ease)"
        );
        assert_eq!(st.glide.target_px(), 3 * cell_h);
    }
    assert_eq!(app.windows[&wid].wheel_precise_px, None);
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        0,
        "no instant jump"
    );
}

/// Output arriving mid-glide re-pins the viewport
/// (`display_offset += lines_added`) so the reader keeps the content they
/// were looking at; the next glide wake carries that shift instead of
/// scrolling the content back toward live.
#[test]
fn output_mid_glide_shifts_the_glide_instead_of_scrolling_the_reader_back() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    let t0 = std::time::Instant::now();
    app.scroll_wheel_animated_at(wid, &term, 4, None, None, t0);
    let end = app.windows[&wid].scroll_glide.as_ref().unwrap().glide.end();
    // Land the engine on a row mid-ease, below the target row, so the
    // landing wake must move the engine and takes the lock where the
    // machine's motion is discovered; then let output re-pin it.
    app.tick_scroll_glide(wid, end - Duration::from_millis(150));
    let mid = term_lock(&term).grid().display_offset();
    assert!(
        (1..4).contains(&mid),
        "fixture: the ease has moved the engine short of its target ({mid})"
    );
    {
        let mut t = term_lock(&term);
        for i in 0..3 {
            t.process(format!("late output {i}\r\n").as_bytes());
        }
    }
    let repinned = term_lock(&term).grid().display_offset();
    assert_eq!(repinned, mid + 3, "fixture: SCR-1 re-pinned the viewport");
    // The landing wake: the target row is the armed target plus the machine's 3.
    app.tick_scroll_glide(wid, end + Duration::from_millis(1));
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        4 + 3,
        "the glide lands 4 rows into history RELATIVE to the re-pinned content"
    );
    assert!(app.windows[&wid].scroll_glide.is_none());
}

/// Tier-1 binding for `scroll_glide_model::Output`: output that re-pins the
/// viewport under a history-bound glide moves the viewport and the target by the
/// machine's row (`Glide::shift` at the next wake), and the landing is
/// `aim + drift` (`TargetIsAnchored`). Negative control: the Buggy `Output`.
#[test]
fn output_mid_glide_conforms_to_scroll_glide_model() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    let cell_h = app.cell_size().1.max(1) as i64;
    app.scroll_wheel_animated(wid, &term, 2);
    let end = app.windows[&wid].scroll_glide.as_ref().unwrap().glide.end();
    app.tick_scroll_glide(wid, end - Duration::from_millis(150));
    let mid = term_lock(&term).grid().display_offset() as i64;
    assert_eq!(mid, 1, "fixture: mid-ease, one row short of the target");

    let model = aterm_spec::derive::scroll_glide_model();
    let mut before = model.init_state();
    before.insert("pos", mid);
    before.insert("target", 2);
    before.insert("armed", 1);
    before.insert("wakes", 1);
    assert!(model.check_invariant("TargetIsAnchored", &before));
    assert!(model.action_enabled("Output", &before));
    let expected = model
        .successors("Output", &before)
        .into_iter()
        .next()
        .expect("Output has one deterministic successor");

    // One line of machine output: SCR-1 re-pins the reader's content.
    term_lock(&term).process(b"late output\r\n");
    assert_eq!(
        term_lock(&term).grid().display_offset() as i64,
        expected["pos"],
        "the viewport moved with the content, as the model's Output does"
    );
    // The next wake discovers the machine's row and shifts the glide.
    app.tick_scroll_glide(wid, end - Duration::from_millis(100));
    let target_rows = app.windows[&wid]
        .scroll_glide
        .as_ref()
        .expect("still easing")
        .glide
        .target_px()
        / cell_h;
    assert_eq!(target_rows, expected["target"], "the target moved with it");
    assert!(model.check_invariant("TargetIsAnchored", &expected));

    // The landing is the aimed content: aim + drift.
    app.tick_scroll_glide(wid, end + Duration::from_millis(1));
    assert_eq!(
        term_lock(&term).grid().display_offset() as i64,
        expected["aim"] + expected["drift"]
    );
    assert!(app.windows[&wid].scroll_glide.is_none());

    // Negative control: the mutant that re-pins the viewport but leaves the
    // target behind is exactly what the invariant refuses.
    let mut buggy = aterm_spec::derive::scroll_glide_model();
    buggy.consts = vec![("Buggy", 1)];
    let left_behind = buggy
        .successors("Output", &before)
        .into_iter()
        .next()
        .expect("the mutant's Output fires too");
    assert_eq!(left_behind["target"], 2, "the mutant leaves the target");
    assert!(!buggy.check_invariant("TargetIsAnchored", &left_behind));
}

/// A glide bound for live lands on live, not `lines_added` rows above it, with
/// output arriving mid-ease: the re-pin is right for a history target (content),
/// but live is a destination. Covers the landing tick and the settle.
#[test]
fn a_glide_bound_for_live_lands_on_live_while_output_streams() {
    for settle in [false, true] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = seed_history(&app, wid);
        term_lock(&term).scroll_display(3);
        app.scroll_wheel_animated(wid, &term, -3);
        let end = app.windows[&wid].scroll_glide.as_ref().unwrap().glide.end();
        app.tick_scroll_glide(wid, end - Duration::from_millis(150));
        let mid = term_lock(&term).grid().display_offset();
        assert!(
            (1..3).contains(&mid),
            "fixture: the ease has moved the engine toward live but not onto it ({mid})"
        );
        {
            let mut t = term_lock(&term);
            for i in 0..3 {
                t.process(format!("late output {i}\r\n").as_bytes());
            }
        }
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            mid + 3,
            "fixture: SCR-1 re-pinned the viewport while the glide was in flight"
        );
        if settle {
            app.settle_scroll_motion_at_target(wid, end - Duration::from_millis(100));
        } else {
            app.tick_scroll_glide(wid, end + Duration::from_millis(1));
        }
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            0,
            "settle={settle}: a glide bound for live lands ON live, not {} rows above it",
            3
        );
        assert!(app.windows[&wid].scroll_glide.is_none(), "settle={settle}");
    }
}

/// …and an offset that drops under it is followed, not undone. The re-pin
/// only ever raises the offset; a drop (the app clearing its scrollback
/// with `CSI 3 J`, which Ink-based TUIs send on a full redraw) already moved
/// the reader to live, and the glide must not scroll them back up.
#[test]
fn a_live_bound_glide_follows_a_cleared_history_instead_of_climbing_it() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    term_lock(&term).scroll_display(8);
    app.scroll_wheel_animated(wid, &term, -8);
    let end = app.windows[&wid].scroll_glide.as_ref().unwrap().glide.end();
    app.tick_scroll_glide(wid, end - Duration::from_millis(150));
    let mid = term_lock(&term).grid().display_offset();
    assert!(
        mid > 1,
        "fixture: the ease is still well above live ({mid})"
    );
    {
        let mut t = term_lock(&term);
        t.process(b"\x1b[3J");
        assert_eq!(
            t.grid().display_offset(),
            0,
            "fixture: ED 3 dropped the reader to live"
        );
        for i in 0..12 {
            t.process(format!("after the clear {i}\r\n").as_bytes());
        }
        assert!(
            t.grid().scrollback_lines() >= 8,
            "fixture: new history to climb into"
        );
    }
    for ms in [100u64, 50, 0] {
        app.tick_scroll_glide(wid, end - Duration::from_millis(ms));
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            0,
            "{ms} ms before the end: the reader stays at live"
        );
        assert_eq!(
            app.windows[&wid].scroll_frac_px, 0,
            "{ms} ms before the end: no residual lifts the live screen"
        );
    }
    app.tick_scroll_glide(wid, end + Duration::from_millis(1));
    assert_eq!(term_lock(&term).grid().display_offset(), 0);
    assert!(app.windows[&wid].scroll_glide.is_none());
}

/// The anchor: a notch glide's tick is armed once, one panel period after the
/// arm; a chained notch (a fresh `GLIDE_MS` retarget) leaves it where it was;
/// the tick advances phase-locked from the slot that fired (one more than a
/// period late re-phases from `now`); and a precise delta parks the band, so
/// its one wake is the rest-settle start. Every instant is a fixed offset from
/// one origin handed to `scroll_wheel_animated_at` and the tick alike.
#[test]
fn a_chained_notch_leaves_the_anchored_tick_alone_and_a_precise_delta_parks_it() {
    use crate::scroll_motion::{GLIDE_MS, TRACK_REST_MS};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    let interval = Duration::from_micros(8_333); // a 120 Hz panel
    app.windows.get_mut(&wid).unwrap().frame_interval = Some(interval);
    let tick_of = |app: &App| {
        app.windows[&wid]
            .scroll_glide
            .as_ref()
            .map(|st| (st.next_tick, st.wake_deadline(), st.glide.end()))
            .expect("a glide is armed")
    };

    // The test's one clock read: the origin every instant below is built on.
    let t0 = std::time::Instant::now();
    app.scroll_wheel_animated_at(wid, &term, 1, None, None, t0);
    let (tick, deadline, end) = tick_of(&app);
    assert_eq!(
        end,
        t0 + Duration::from_millis(GLIDE_MS),
        "the ease starts at the injected instant"
    );
    assert_eq!(
        tick,
        t0 + interval,
        "the first tick is one panel period after the arm"
    );
    assert_eq!(
        deadline, tick,
        "…and it is the wake the glide owes (its end is far later)"
    );
    // A notch chained 5 ms later, before the tick is due (the order a live loop
    // presents them in).
    let chain = t0 + Duration::from_millis(5);
    assert!(chain < tick, "fixture: the chain precedes the tick");
    app.scroll_wheel_animated_at(wid, &term, 1, None, None, chain);
    let (tick_after, deadline_after, end_after) = tick_of(&app);
    assert_eq!(
        end_after,
        chain + Duration::from_millis(GLIDE_MS),
        "a notch chain retargets: a fresh ease from the chain"
    );
    assert_eq!(
        tick_after, tick,
        "a chained notch must not move the anchored tick"
    );
    assert_eq!(deadline_after, deadline);

    // The tick is served 2 ms late (a redraw stood in front of it): the
    // next slot is measured from the slot that fired, not from `now`.
    app.tick_scroll_glide(wid, tick + Duration::from_millis(2));
    let (next, _, _) = tick_of(&app);
    assert_eq!(
        next,
        tick + interval,
        "phase-locked: the lateness does not accumulate"
    );
    // More than a whole period late: re-phase from `now`.
    let late = next + 2 * interval + Duration::from_millis(1);
    app.tick_scroll_glide(wid, late);
    let (rephased, _, _) = tick_of(&app);
    assert_eq!(
        rephased,
        late + interval,
        "a stalled tick re-phases from now"
    );

    // A precise delta parks the band: its one wake is the rest boundary.
    let precise_at = late + Duration::from_millis(1);
    app.scroll_wheel_animated_at(wid, &term, 0, None, Some(3.0), precise_at);
    let (parked_tick, parked_deadline, _) = tick_of(&app);
    assert_eq!(parked_tick, parked_deadline);
    assert_eq!(
        parked_tick,
        precise_at + Duration::from_millis(TRACK_REST_MS),
        "a parked band owes one wake at its rest-settle start, not a frame cadence"
    );
}

/// The glide ticks at panel cadence through a wake stream denser than one period
/// plus a redraw (the trackpad's `PixelDelta` stream, a PTY burst). Driven as
/// winit does (a wake every 8 ms, a 2 ms redraw, the every-wake
/// `service_due_scroll_motion`, the park's re-read); pins intermediate motion
/// and that a park never arms a past instant. A sliding
/// `end.min(now + interval)` park, ticked only on `ResumeTimeReached`, is
/// computed alongside as the witness that the stream is adversarial: under it
/// only the end ticks.
#[test]
fn a_panel_rate_wake_stream_cannot_starve_the_glide() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    let interval = Duration::from_micros(8_333); // a 120 Hz panel
    app.windows.get_mut(&wid).unwrap().frame_interval = Some(interval);
    let wake_gap = Duration::from_millis(8); // the trackpad's own stream
    let redraw_cost = Duration::from_millis(2); // a dense 144x60 frame

    let t0 = std::time::Instant::now();
    app.scroll_wheel_animated_at(wid, &term, 5, None, None, t0);
    let (end, mut armed) = {
        let st = app.windows[&wid]
            .scroll_glide
            .as_ref()
            .expect("Full policy arms a glide");
        (st.glide.end(), st.wake_deadline())
    };
    let mut next_event = t0 + wake_gap;
    let mut ticks = 0u32;
    let mut last_tick = t0;
    let mut max_gap = Duration::ZERO;
    let mut moved_before_end = false;
    let mut retired_law_could_tick = 0u32;
    let mut turns = 0u32;
    loop {
        turns += 1;
        assert!(turns < 64, "the glide must disarm within its 180 ms");
        // The loop wakes for whichever comes first, the armed timer or the next
        // external event, and judges the anchored tick either way.
        let wake = armed.min(next_event);
        if wake == next_event {
            next_event += wake_gap;
        }
        if !app.service_due_scroll_motion(wake).is_empty() {
            ticks += 1;
            max_gap = max_gap.max(wake - last_tick);
            last_tick = wake;
            if app.windows[&wid].scroll_glide.is_none() {
                break;
            }
            let off = term_lock(&term).grid().display_offset();
            if wake < end && off > 0 && off < 5 {
                moved_before_end = true;
            }
        }
        // The redraw this wake requested, then the park: an overdue slot is
        // served there too, and the fold reads the anchor back.
        let park = wake + redraw_cost;
        if !app.service_due_scroll_motion(park).is_empty() {
            ticks += 1;
            max_gap = max_gap.max(park - last_tick);
            last_tick = park;
            if app.windows[&wid].scroll_glide.is_none() {
                break;
            }
        }
        armed = app.windows[&wid]
            .scroll_glide
            .as_ref()
            .expect("armed until the end")
            .wake_deadline();
        assert!(
            armed > park,
            "a park never arms a past anchor (a past arm feeds the 29/32 streak clamp)"
        );
        // The sliding law: `end.min(park + interval)`, ticked only when the
        // next event does not land first.
        let sliding = end.min(park + interval);
        if next_event >= sliding {
            retired_law_could_tick += 1;
        }
    }
    let expected = u32::try_from(crate::scroll_motion::GLIDE_MS * 1000 / 8_333).unwrap_or(u32::MAX);
    assert!(
        ticks >= expected,
        "a 180 ms ease on a 120 Hz panel owes >= {expected} ticks under a panel-rate \
         wake stream; got {ticks}"
    );
    assert!(
        max_gap <= interval + redraw_cost + Duration::from_millis(1),
        "no tick may wait out an extra period behind the wake stream (max gap {max_gap:?})"
    );
    assert!(
        moved_before_end,
        "non-vacuity: the viewport must move at an intermediate row before the end"
    );
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        5,
        "and still land exactly"
    );
    assert!(
        retired_law_could_tick <= 1,
        "witness: under the retired sliding law this stream admits at most the \
         landing tick (got {retired_law_could_tick}) — the one-jump 'blocky' scroll"
    );
}

/// The every-wake service is a level judge: a wake just before the anchored
/// tick ticks nothing, and one long past it ticks and re-phases the anchor
/// strictly ahead of `now`. The bounce is served the same way.
#[test]
fn the_every_wake_service_ticks_only_due_anchors_and_never_leaves_a_past_one() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    let interval = Duration::from_micros(8_333);
    app.windows.get_mut(&wid).unwrap().frame_interval = Some(interval);
    let t0 = std::time::Instant::now();
    app.scroll_wheel_animated_at(wid, &term, 5, None, None, t0);
    let tick = app.windows[&wid].scroll_glide.as_ref().unwrap().next_tick;
    assert!(
        app.service_due_scroll_motion(tick - Duration::from_micros(1))
            .is_empty(),
        "not due: no tick"
    );
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        0,
        "…and no phantom motion"
    );
    let late = tick + 2 * interval;
    assert_eq!(
        app.service_due_scroll_motion(late),
        vec![wid],
        "overdue: ticked"
    );
    let off = term_lock(&term).grid().display_offset();
    assert!(off > 0 && off < 5, "an intermediate row ({off})");
    let armed = app.windows[&wid]
        .scroll_glide
        .as_ref()
        .unwrap()
        .wake_deadline();
    assert!(
        armed > late,
        "the anchor is re-phased strictly ahead of now"
    );

    // The bounce: parked at the live bottom, a notch toward live releases it
    // with its own anchored tick; the same service advances it.
    app.windows.get_mut(&wid).unwrap().scroll_glide = None;
    term_lock(&term).scroll_to_bottom();
    let bounce_at = late + Duration::from_millis(1);
    app.scroll_wheel_animated_at(wid, &term, -1, None, None, bounce_at);
    let sp_tick = app.windows[&wid]
        .overscroll
        .as_ref()
        .expect("a bounce released at the live bottom")
        .next_tick();
    assert!(
        sp_tick > bounce_at,
        "the bounce's tick is ahead of its release"
    );
    assert!(
        app.service_due_scroll_motion(sp_tick - Duration::from_micros(1))
            .is_empty(),
        "not due: no bounce tick"
    );
    assert_eq!(
        app.service_due_scroll_motion(sp_tick),
        vec![wid],
        "due: the bounce ticked"
    );
    if let Some(sp) = app.windows[&wid].overscroll.as_ref() {
        assert!(sp.wake_deadline() > sp_tick, "…and re-anchored ahead");
    }
}

/// The wheel's tracking-off fallback glides under a Full motion policy: the
/// notch arms a self-disarming ease, chained notches retarget the same
/// ease, the final tick lands exactly on the target row, and the state is
/// dropped (the ty-model `ScrollGlide` discipline on the real seam).
#[test]
fn wheel_scroll_glides_and_lands_exactly() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    let cell_h = app.cell_size().1.max(1) as i64;

    // Default headless policy: auto + OS flag off + focused ⇒ Full ⇒ glide.
    let t0 = std::time::Instant::now();
    app.scroll_wheel_animated_at(wid, &term, 5, None, None, t0);
    {
        let ws = app.windows.get(&wid).unwrap();
        let st = ws.scroll_glide.as_ref().expect("Full policy arms a glide");
        assert_eq!(st.glide.target_px(), 5 * cell_h, "target = 5 rows in px");
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            0,
            "no instant jump — the ease moves the viewport"
        );
    }
    // A chained notch retargets the same ease (2 more rows into history).
    let chained_at = t0 + Duration::from_millis(5);
    app.scroll_wheel_animated_at(wid, &term, 2, None, None, chained_at);
    let end = {
        let ws = app.windows.get(&wid).unwrap();
        let st = ws.scroll_glide.as_ref().expect("still armed");
        assert_eq!(st.glide.target_px(), 7 * cell_h, "retargeted to 7 rows");
        st.glide.end()
    };
    // Mid-flight tick: the viewport may sit anywhere on the path, but the
    // glide is still armed (no premature disarm).
    app.tick_scroll_glide(wid, end - Duration::from_millis(60));
    assert!(
        app.windows.get(&wid).unwrap().scroll_glide.is_some(),
        "mid-flight tick keeps the glide armed"
    );
    // The tick at/after the end lands exactly and self-disarms.
    app.tick_scroll_glide(wid, end + Duration::from_millis(1));
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        7,
        "the ease lands exactly on its target row"
    );
    assert!(
        app.windows.get(&wid).unwrap().scroll_glide.is_none(),
        "self-disarm: the state is dropped at the end (no perpetual wake)"
    );
    // The pill woke with the gesture, and the landing tick kept it awake.
    assert!(
        app.windows
            .get(&wid)
            .unwrap()
            .scroll_pill
            .is_active(end + Duration::from_millis(1), true),
        "scroll activity shows the pill"
    );
}

/// A live Reduce Motion change settles an in-flight Full-policy glide
/// immediately at its target, clears its deadline/residual, and a later
/// wheel notch snaps from that landed row.
#[test]
fn reduced_motion_wheel_snaps_instantly() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);

    // Arm a glide under the default Full policy…
    let t0 = std::time::Instant::now();
    app.scroll_wheel_animated_at(wid, &term, 4, None, None, t0);
    assert!(app.windows.get(&wid).unwrap().scroll_glide.is_some());
    // …then the OS Reduce Motion flag flips (Wake::ReduceMotionChanged).
    assert!(app.apply_system_reduce_motion(true, t0 + Duration::from_millis(20)));
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        4,
        "the policy edge itself lands the retained target"
    );
    assert!(app.windows.get(&wid).unwrap().scroll_glide.is_none());
    assert_eq!(app.windows.get(&wid).unwrap().scroll_frac_px, 0);

    app.scroll_wheel_animated_at(wid, &term, 3, None, None, t0 + Duration::from_millis(40));
    assert!(
        app.windows.get(&wid).unwrap().scroll_glide.is_none(),
        "Reduced motion never re-arms"
    );
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        7,
        "the Reduced notch applies instantly from the settled target"
    );
}

/// Defensive tick convergence: even if a policy fact is installed without
/// its edge reducer, the next due tick jumps to the target and disarms
/// (sampling the old ease would leave an intermediate row and a deadline).
#[test]
fn reduced_motion_tick_defensively_lands_instead_of_sampling() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    let t0 = std::time::Instant::now();
    app.scroll_wheel_animated_at(wid, &term, 4, None, None, t0);
    let end = app.windows[&wid]
        .scroll_glide
        .as_ref()
        .expect("Full motion arms a glide")
        .glide
        .end();

    // Intentionally bypass `apply_system_reduce_motion` to exercise the
    // tick's defensive convergence path in isolation.
    app.system_reduce_motion = true;
    app.tick_scroll_glide(wid, end - Duration::from_millis(150));
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        4,
        "Reduced tick lands at target rather than sampling an early row"
    );
    assert!(app.windows[&wid].scroll_glide.is_none());
    assert_eq!(app.windows[&wid].scroll_frac_px, 0);
}

/// Tier-1 binding for `scroll_glide_model::SetReduced`: a config Full→Reduced
/// edge projects to the model's atomic `{pos=target, armed=0, reduced=1}`.
/// Negative control: cancel-only (dropping the state at the old row) violates
/// `ReducedSettled`.
#[test]
fn reduced_motion_settle_conforms_to_scroll_glide_model() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    app.config.motion = Some("full".into());
    let t0 = std::time::Instant::now();
    app.scroll_wheel_animated_at(wid, &term, 3, None, None, t0);

    let model = aterm_spec::derive::scroll_glide_model();
    let mut before = model.init_state();
    before.insert("pos", term_lock(&term).grid().display_offset() as i64);
    before.insert("target", 3);
    before.insert("armed", 1);
    before.insert("wakes", 0);
    before.insert("reduced", 0);
    assert!(model.action_enabled("SetReduced", &before));
    let expected = model
        .successors("SetReduced", &before)
        .into_iter()
        .next()
        .expect("SetReduced has one deterministic successor");

    let mut cancel_only = before.clone();
    cancel_only.insert("armed", 0);
    cancel_only.insert("reduced", 1);
    assert!(
        !model.check_invariant("ReducedSettled", &cancel_only),
        "negative control: cancel-without-landing must be rejected"
    );

    // The per-window reconciliation `apply_prepared_config_generation_unfenced`
    // runs (through `settle_reduced_scroll_motion`) after config admission.
    app.config.motion = Some("reduced".into());
    assert!(app.settle_scroll_motion_if_reduced(wid, t0 + Duration::from_millis(20)));
    let mut actual = before;
    actual.insert("pos", term_lock(&term).grid().display_offset() as i64);
    actual.insert("armed", i64::from(app.windows[&wid].scroll_glide.is_some()));
    actual.insert("reduced", 1);
    assert_eq!(actual, expected, "shipping reducer must match SetReduced");
    assert!(model.check_invariant("DisarmedAtTarget", &actual));
    assert!(model.check_invariant("ReducedSettled", &actual));
    assert_eq!(app.windows[&wid].scroll_frac_px, 0);
}

/// Typing's snap-to-bottom stays instant and cancels an in-flight glide, so
/// the eased tail cannot scroll the viewport away from the prompt after a
/// keystroke.
#[test]
fn typing_snap_cancels_glide_tail() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    // Scrolled into history with a further glide in flight.
    term_lock(&term).scroll_display(5);
    app.scroll_wheel_animated(wid, &term, 3);
    assert!(app.windows.get(&wid).unwrap().scroll_glide.is_some());

    app.snap_to_bottom(wid);
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        0,
        "keyboard input still snaps instantly"
    );
    assert!(
        app.windows.get(&wid).unwrap().scroll_glide.is_none(),
        "the glide tail is cancelled — nothing re-scrolls after the snap"
    );
}

/// The glide tick banks a sub-row residual (`scroll_frac_px`) so scrolling
/// glides by the pixel: the residual stays in `[0, cell_h)` with the engine at
/// the ceil offset, `offset*cell_h - frac` never retreats, some tick banks a
/// nonzero residual, and the tick at `end` lands whole-row and disarms.
#[test]
fn glide_banks_sub_row_residual_and_lands_whole_row() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    let cell_h = app.cell_size().1.max(1) as i64;

    // Ease 0 → 5 rows into history.
    let t0 = std::time::Instant::now();
    app.scroll_wheel_animated_at(wid, &term, 5, None, None, t0);
    let end = app
        .windows
        .get(&wid)
        .unwrap()
        .scroll_glide
        .as_ref()
        .expect("Full policy arms a glide")
        .glide
        .end();

    let mut saw_frac = false;
    let mut landed = false;
    let mut prev_pos = 0i64;
    // Sample the whole ease on a fine cadence — from before the arm, through
    // the landing at `end`.
    let step = Duration::from_millis(7);
    let mut at = end - Duration::from_millis(200);
    loop {
        app.tick_scroll_glide(wid, at);
        let ws = app.windows.get(&wid).unwrap();
        let frac = i64::from(ws.scroll_frac_px);
        let offset = term_lock(&term).grid().display_offset() as i64;
        assert!(
            (0..cell_h).contains(&frac),
            "residual out of [0,cell_h): {frac}"
        );
        // The shift-up pairing: content position = engine offset shifted up by
        // frac. It must stay a valid eased point and never retreat.
        let pos = offset * cell_h - frac;
        assert!(
            (0..=5 * cell_h).contains(&pos),
            "reconstructed pos {pos} out of range"
        );
        assert!(
            pos >= prev_pos,
            "position must not retreat ({prev_pos} -> {pos})"
        );
        prev_pos = pos;
        if frac > 0 {
            saw_frac = true;
            assert!(offset >= 1, "a nonzero residual sits at the CEIL offset");
        }
        if ws.scroll_glide.is_none() {
            landed = true;
            break;
        }
        if at >= end {
            break;
        }
        at = (at + step).min(end);
    }
    assert!(landed, "the tick at `end` lands the ease and disarms it");
    assert!(
        saw_frac,
        "non-vacuity: the glide must bank a nonzero sub-row residual"
    );
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        5,
        "the ease lands on the whole-row target"
    );
    assert_eq!(
        app.windows.get(&wid).unwrap().scroll_frac_px,
        0,
        "a landed glide clears the residual (whole-row rest)"
    );
}

/// Every instant snap clears the banked residual so the frame is whole-row:
/// Reduced motion (SmoothScroll proven-zero), typing's snap-to-bottom, and
/// the controller/keyboard `ScrollView` verb path all force
/// `scroll_frac_px == 0`.
#[test]
fn instant_snaps_clear_the_sub_row_residual() {
    use crate::input::{InputEvent, ScrollIntent, Source};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);

    // Bank a residual via a mid-glide tick, then a Reduced-motion notch snaps.
    // Each phase arms on the same synthetic timeline its ticks are served on.
    let t0 = std::time::Instant::now();
    app.scroll_wheel_animated_at(wid, &term, 5, None, None, t0);
    let end = app
        .windows
        .get(&wid)
        .unwrap()
        .scroll_glide
        .as_ref()
        .unwrap()
        .glide
        .end();
    app.tick_scroll_glide(wid, end - Duration::from_millis(90));
    app.system_reduce_motion = true;
    app.scroll_wheel_animated_at(wid, &term, 2, None, None, end - Duration::from_millis(80));
    assert_eq!(
        app.windows.get(&wid).unwrap().scroll_frac_px,
        0,
        "Reduced motion snaps whole-row — no residual"
    );

    // Re-bank under Full motion, then typing snaps to bottom.
    app.system_reduce_motion = false;
    let t1 = end;
    app.scroll_wheel_animated_at(wid, &term, 5, None, None, t1);
    let end = app
        .windows
        .get(&wid)
        .unwrap()
        .scroll_glide
        .as_ref()
        .unwrap()
        .glide
        .end();
    app.tick_scroll_glide(wid, end - Duration::from_millis(90));
    app.snap_to_bottom(wid);
    assert_eq!(
        app.windows.get(&wid).unwrap().scroll_frac_px,
        0,
        "typing's snap-to-bottom clears the residual"
    );

    // Re-bank, then an instant ScrollView verb (controller/keyboard) clears it —
    // the applied-offset reply contract stays whole-row (source-blind).
    let t2 = end;
    app.scroll_wheel_animated_at(wid, &term, 5, None, None, t2);
    let end = app
        .windows
        .get(&wid)
        .unwrap()
        .scroll_glide
        .as_ref()
        .unwrap()
        .glide
        .end();
    app.tick_scroll_glide(wid, end - Duration::from_millis(90));
    app.input(
        wid,
        InputEvent::ScrollView(ScrollIntent::Top),
        Source::Human,
    );
    assert_eq!(
        app.windows.get(&wid).unwrap().scroll_frac_px,
        0,
        "an instant ScrollView jump is whole-row (no eased tail, no residual)"
    );
}

/// A notch against a history end (target clamps to the current position)
/// never arms a zero-length glide but still wakes the pill to show the
/// edge, and releases an elastic bounce (see
/// `overscroll_bounce_springs_and_self_disarms`).
#[test]
fn edge_notch_arms_no_glide_but_shows_pill() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);
    // Already at the live bottom; a downward notch has nowhere to go. Notch and
    // pill query share the instant `before`, so no deschedule can cross them.
    let before = std::time::Instant::now();
    app.scroll_wheel_animated_at(wid, &term, -3, None, None, before);
    let ws = app.windows.get(&wid).unwrap();
    assert!(
        ws.scroll_glide.is_none(),
        "a clamped-to-current target must not arm a zero-length glide"
    );
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        0,
        "the bounce is display-only — the engine stays parked at the edge"
    );
    assert!(
        ws.scroll_pill.is_active(before, true),
        "the pill still wakes to show the edge"
    );
}

/// Elastic overscroll: a notch past a history end releases a display-only bounce
/// in the signed `scroll_frac_px` (live bottom: up, positive; scrollback top:
/// down, negative) that frame-paced ticks decay and disarm. Reduced motion never
/// arms one.
#[test]
fn overscroll_bounce_springs_and_self_disarms() {
    use std::time::Instant;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = seed_history(&app, wid);

    // (1) Bottom edge: at the live bottom, a downward notch bounces up
    // (+frac). The release and every tick share one synthetic timeline, so
    // a deschedule cannot shift the ticks against the spring's clock.
    let t0 = Instant::now();
    app.scroll_wheel_animated_at(wid, &term, -4, None, None, t0);
    {
        let ws = app.windows.get(&wid).unwrap();
        assert!(
            ws.overscroll.is_some(),
            "a bottom overscroll releases the spring"
        );
        assert!(
            ws.scroll_frac_px > 0,
            "bottom bounce shifts the band UP (+frac)"
        );
        assert_eq!(term_lock(&term).grid().display_offset(), 0, "engine parked");
    }
    // Frame-paced ticks decay the bounce to rest and disarm.
    let end = app
        .windows
        .get(&wid)
        .unwrap()
        .overscroll
        .as_ref()
        .unwrap()
        .end();
    let mut now = t0;
    let mut settled = false;
    for _ in 0..64 {
        now += Duration::from_millis(16);
        app.tick_overscroll(wid, now);
        if app.windows.get(&wid).unwrap().overscroll.is_none() {
            settled = true;
            break;
        }
    }
    assert!(settled, "the bounce self-disarms within its settle cap");
    assert!(
        now <= end + Duration::from_millis(16),
        "disarms by the settle bound"
    );
    assert_eq!(
        app.windows.get(&wid).unwrap().scroll_frac_px,
        0,
        "a settled bounce rests whole-row (no residual)"
    );

    // (2) Top edge: scrolled to the top, an upward notch bounces down (−frac).
    term_lock(&term).scroll_to_top();
    let top = term_lock(&term).grid().display_offset();
    assert!(top > 0, "test needs the viewport parked in history");
    now += Duration::from_millis(16);
    app.scroll_wheel_animated_at(wid, &term, 4, None, None, now);
    {
        let ws = app.windows.get(&wid).unwrap();
        assert!(
            ws.overscroll.is_some(),
            "a top overscroll releases the spring"
        );
        assert!(
            ws.scroll_frac_px < 0,
            "top bounce shifts the band DOWN (−frac)"
        );
        assert_eq!(
            term_lock(&term).grid().display_offset(),
            top,
            "engine parked at the scrollback top"
        );
    }

    // (3) Reduced motion cancels the bounce and never re-arms (whole-row snap).
    app.system_reduce_motion = true;
    now += Duration::from_millis(16);
    app.scroll_wheel_animated_at(wid, &term, 4, None, None, now);
    let ws = app.windows.get(&wid).unwrap();
    assert!(
        ws.overscroll.is_none(),
        "Reduced motion never arms a bounce"
    );
    assert_eq!(ws.scroll_frac_px, 0, "Reduced motion rests whole-row");
}
