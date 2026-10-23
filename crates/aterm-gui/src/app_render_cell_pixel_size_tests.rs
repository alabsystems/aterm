// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, WindowId, term_lock};

const PLACEHOLDER: (u16, u16) = (8, 16);

fn window_with_a_big_cell(app: &mut App, wid: WindowId) -> (u16, u16) {
    app.windows
        .get_mut(&wid)
        .expect("headless window 0")
        .metrics
        .font_px = 24.0;
    let (cw, ch) = app.win_cell_size(wid);
    let cell = (cw as u16, ch as u16);
    assert!(
        cell.0 > PLACEHOLDER.0 && cell.1 > PLACEHOLDER.1,
        "a 24px face must resolve past the 8x16 placeholder for this test to \
         mean anything; got {cell:?}"
    );
    cell
}

#[test]
fn a_never_resized_window_reports_its_real_cell_box_to_every_session() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let cell = window_with_a_big_cell(&mut app, wid);

    let second = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(second));

    for id in [0, second] {
        assert_eq!(
            term_lock(&app.pool.get(id).expect("pooled session").term).cell_pixel_size(),
            PLACEHOLDER,
            "session {id} must start at the placeholder, or this test proves nothing"
        );
    }

    app.apply_window_scale(wid);

    for id in [0, second] {
        assert_eq!(
            term_lock(&app.pool.get(id).expect("pooled session").term).cell_pixel_size(),
            cell,
            "session {id} must report the window's real cell box"
        );
    }
}

#[test]
fn a_newborn_live_engine_carries_the_owning_windows_cell_box() {
    let build = |cell_px| crate::spawn::new_live_terminal(24, 80, None, cell_px);
    assert_eq!(build(Some((11, 23))).cell_pixel_size(), (11, 23));
    assert_eq!(build(None).cell_pixel_size(), PLACEHOLDER);
    assert_eq!(build(Some((0, 0))).cell_pixel_size(), (1, 1));
}

#[test]
fn the_backend_join_corrects_the_boot_sessions_placeholder() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    window_with_a_big_cell(&mut app, wid);

    term_lock(&app.pool.get(0).expect("session 0").term)
        .set_cell_pixel_size(PLACEHOLDER.0, PLACEHOLDER.1);
    app.windows
        .get_mut(&wid)
        .expect("window 0")
        .cell_px_reported = None;
    let theme = app.theme;
    let handle = std::thread::spawn(move || -> (crate::Backend, bool, Option<String>) {
        let mut r = aterm_render::Renderer::from_system(crate::FONT_PX, theme)
            .expect("system font for the test build");
        r.seal_admitted_font_sources();
        (crate::Backend::Cpu(r), false, None)
    });
    app.backend = crate::BackendSlot::Pending(Some(handle));

    assert_eq!(app.spawn_cell_px(wid), None);

    app.finalize_backend();

    let real = app
        .spawn_cell_px(wid)
        .expect("a joined backend has metrics");
    assert_ne!(
        real, PLACEHOLDER,
        "the 24px fixture face must differ from the placeholder"
    );
    assert_eq!(
        term_lock(&app.pool.get(0).expect("session 0").term).cell_pixel_size(),
        real,
        "the join must correct the boot session before any frame"
    );
}

/// The backend join also writes the cell box into the boot PTY's winsize, so `TIOCGWINSZ`
/// reports non-zero `ws_xpixel`/`ws_ypixel`. Driven on a real pty, first asserting the 0x0 seed.
#[cfg(unix)]
#[test]
fn the_backend_join_teaches_the_boot_ptys_winsize_its_cell_box() {
    fn winsize_of(master: i32) -> libc::winsize {
        // SAFETY: `winsize` is four integers; zeroed is valid, and `TIOCGWINSZ` fills it for an
        // open pty master.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::ioctl(master, libc::TIOCGWINSZ, &mut ws) },
            0,
            "TIOCGWINSZ"
        );
        ws
    }

    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    window_with_a_big_cell(&mut app, wid);
    app.pool
        .sessions
        .get_mut(&0)
        .expect("session 0")
        .session
        .master = master;

    aterm_pty::resize_with_cell_px(master, 24, 80, None);
    let seeded = winsize_of(master);
    assert_eq!(
        (seeded.ws_xpixel, seeded.ws_ypixel),
        (0, 0),
        "the boot seed really does open at 0x0 — without this the test could \
         pass on a pty that was never wrong"
    );

    app.windows
        .get_mut(&wid)
        .expect("window 0")
        .cell_px_reported = None;
    app.sync_cell_pixel_size(wid);

    let px = app
        .spawn_cell_px(wid)
        .expect("a joined backend has metrics");
    let corrected = winsize_of(master);
    assert_eq!(
        (corrected.ws_xpixel, corrected.ws_ypixel),
        (
            px.0.saturating_mul(corrected.ws_col),
            px.1.saturating_mul(corrected.ws_row)
        ),
        "the pty's pixel fields must be the cell box times the grid the \
         engine is on"
    );

    // SAFETY: both fds were opened above and are still open.
    unsafe {
        libc::close(master);
        libc::close(slave);
    }
}

/// The backend join does not end an adoption pulse hold early: a successor holds each
/// repaint-adopted PTY at `rows-1` so a size-comparing program (Node) sees the change, the
/// join leaves the PTY alone while `adopt_pulse_hold` stands, and the pulse's second half
/// writes the grid with the join's cell box. On a real pty.
#[cfg(unix)]
#[test]
fn the_backend_join_does_not_end_an_adoption_pulse_hold_early() {
    fn winsize_of(fd: i32) -> (u16, u16, u16, u16) {
        // SAFETY: `winsize` is four integers; zeroed is valid, and `TIOCGWINSZ` fills it for an
        // open pty fd.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) },
            0,
            "TIOCGWINSZ"
        );
        (ws.ws_row, ws.ws_col, ws.ws_xpixel, ws.ws_ypixel)
    }

    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    window_with_a_big_cell(&mut app, wid);
    let (term, holding) = {
        let entry = app.pool.sessions.get_mut(&0).expect("session 0");
        entry.session.master = master;
        (
            std::sync::Arc::clone(&entry.session.term),
            std::sync::Arc::clone(&entry.session.adopt_pulse_hold),
        )
    };
    let (rows, cols) = {
        let engine = term_lock(&term);
        (engine.rows(), engine.cols())
    };
    aterm_pty::resize_with_cell_px(master, rows, cols, None);

    let mut held = None;
    crate::spawn::pulse_adopted_pty_with(master, &term, &holding, None, |restore| {
        held = Some(restore);
    });
    assert!(
        holding.load(std::sync::atomic::Ordering::Acquire),
        "the hold stands between the halves"
    );
    assert_eq!(
        winsize_of(slave).0,
        rows - 1,
        "the program reads the transient"
    );

    app.windows
        .get_mut(&wid)
        .expect("window 0")
        .cell_px_reported = None;
    app.sync_cell_pixel_size(wid);
    let px = app
        .spawn_cell_px(wid)
        .expect("a joined backend has metrics");
    assert_eq!(
        winsize_of(slave).0,
        rows - 1,
        "the join's same-size write must not end the hold early"
    );
    assert_eq!(
        term_lock(&term).cell_pixel_size(),
        px,
        "the engine still learns the cell box"
    );

    held.take().expect("the second half is held").run();
    assert!(
        !holding.load(std::sync::atomic::Ordering::Acquire),
        "the hold ends"
    );
    assert_eq!(
        winsize_of(slave),
        (
            rows,
            cols,
            px.0.saturating_mul(cols),
            px.1.saturating_mul(rows)
        ),
        "the second half ends on the engine's grid with the join's cell box"
    );

    aterm_pty::resize_with_cell_px(master, rows, cols, None);
    app.windows
        .get_mut(&wid)
        .expect("window 0")
        .cell_px_reported = None;
    app.sync_cell_pixel_size(wid);
    assert_eq!(
        (winsize_of(slave).2, winsize_of(slave).3),
        (px.0.saturating_mul(cols), px.1.saturating_mul(rows)),
        "outside a hold the join still teaches the PTY its cell box"
    );

    // SAFETY: both fds were opened above and are still open.
    unsafe {
        libc::close(master);
        libc::close(slave);
    }
}

#[test]
fn the_cell_box_memo_re_pushes_whenever_the_resolved_size_moves() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let first = window_with_a_big_cell(&mut app, wid);
    app.apply_window_scale(wid);
    assert_eq!(
        app.windows.get(&wid).expect("window 0").cell_px_reported,
        Some(first)
    );

    app.windows.get_mut(&wid).expect("window 0").metrics.font_px = 10.0;
    let second = {
        let (cw, ch) = app.win_cell_size(wid);
        (cw as u16, ch as u16)
    };
    assert_ne!(first, second, "the two font sizes must resolve differently");
    app.apply_window_scale(wid);
    assert_eq!(
        term_lock(&app.pool.get(0).expect("session 0").term).cell_pixel_size(),
        second,
        "a font change with no re-grid must still reach the engine"
    );

    term_lock(&app.pool.get(0).expect("session 0").term).set_cell_pixel_size(3, 5);
    app.apply_window_scale(wid);
    assert_eq!(
        term_lock(&app.pool.get(0).expect("session 0").term).cell_pixel_size(),
        (3, 5),
        "an unchanged resolved cell box must take no engine lock at all"
    );
}

fn session_reporting_its_size(
    app: &App,
) -> std::sync::Arc<std::sync::Mutex<aterm_core::terminal::Terminal>> {
    let term = std::sync::Arc::clone(&app.pool.get(0).expect("session 0").term);
    {
        let mut t = term_lock(&term);
        t.process(b"\x1b[?1049h\x1b[?2048h");
        let _ = t.take_reports();
        assert!(t.modes().in_band_size_reports);
    }
    term
}

#[test]
fn a_new_cell_box_sends_its_size_report_in_the_same_pass() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let cell = window_with_a_big_cell(&mut app, wid);
    let term = session_reporting_its_size(&app);
    app.apply_window_scale(wid);
    {
        let mut t = term_lock(&term);
        assert_eq!(
            t.cell_pixel_size(),
            cell,
            "the box moved, or this proves nothing"
        );
        assert_eq!(
            t.take_reports(),
            None,
            "the raster seam's report waits for no reader"
        );
    }

    app.resize_panes(wid);
    let grid = {
        let mut t = term_lock(&term);
        let _ = t.take_reports();
        (t.rows(), t.cols())
    };
    app.windows.get_mut(&wid).expect("window 0").metrics.font_px = 10.0;
    app.resize_panes(wid);
    let mut t = term_lock(&term);
    assert_eq!(
        (t.rows(), t.cols()),
        grid,
        "the same grid: the early return"
    );
    assert_ne!(t.cell_pixel_size(), cell, "a new box again");
    assert_eq!(
        t.take_reports(),
        None,
        "the pane pass's report waits for no reader"
    );
}

#[test]
fn a_font_change_reaches_an_unpainted_windows_engines_at_the_change() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.apply_window_scale(wid);
    let before = app
        .windows
        .get(&wid)
        .expect("window 0")
        .cell_px_reported
        .expect("the raster seam reported a cell box");
    assert_eq!(
        term_lock(&app.pool.get(0).expect("session 0").term).cell_pixel_size(),
        before,
        "the seed must have reached the engine, or this test proves nothing"
    );

    app.font_size = 24.0;
    app.font_px = 24.0;
    app.refresh_all_window_metrics();

    let after = {
        let (cw, ch) = app.win_cell_size(wid);
        (cw as u16, ch as u16)
    };
    assert_ne!(
        before, after,
        "the 24px face must resolve past the seeded one for this to mean anything"
    );
    assert_eq!(
        term_lock(&app.pool.get(0).expect("session 0").term).cell_pixel_size(),
        after,
        "the metrics change itself must reach the engine, with no frame in between"
    );
}
