// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::input::PixelOffset;
use crate::{App, WindowId, term_lock};

/// The engine's headless placeholder cell box (`Terminal::set_cell_pixel_size`
/// default); a live window reporting it is a defect.
const PLACEHOLDER: (u16, u16) = (8, 16);

/// Stage a headless window whose resolved cell box is not the 8x16
/// placeholder, without resizing, and return that cell box.
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

/// `apply_window_scale` runs at every raster seam, so a never-resized window
/// reports the real font's cell box (DEC 1016, OSC 1337) from its first frame.
#[test]
fn a_never_resized_window_reports_its_real_cell_box_to_every_session() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let cell = window_with_a_big_cell(&mut app, wid);

    // A second tab, also never resized: a stub engine born at the placeholder.
    let second = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(second));

    for id in [0, second] {
        assert_eq!(
            term_lock(&app.pool.get(id).expect("pooled session").term).cell_pixel_size(),
            PLACEHOLDER,
            "session {id} must start at the placeholder, or this test proves nothing"
        );
    }

    // The first raster seam. No `on_resize`, no `apply_term_resize`, no drag.
    app.apply_window_scale(wid);

    for id in [0, second] {
        assert_eq!(
            term_lock(&app.pool.get(id).expect("pooled session").term).cell_pixel_size(),
            cell,
            "session {id} must report the window's real cell box"
        );
    }
}

/// DEC 1016: a press in cell (3, 2) at sub-cell offset (4, 7) on a fresh tab
/// resolves to `col*cell_w + 4`, `row*cell_h + 7` from the real font.
#[test]
fn dec_1016_on_a_fresh_tab_reports_pixels_from_the_real_font() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let (cw, ch) = window_with_a_big_cell(&mut app, wid);
    app.apply_window_scale(wid);

    let term = app.pool.get(0).expect("session 0").term.clone();
    {
        let mut t = term_lock(&term);
        t.process(b"\x1b[?1000h"); // normal mouse tracking
        t.process(b"\x1b[?1016h"); // SGR-pixel (DEC 1016)
    }
    let t = term_lock(&term);
    let off = PixelOffset { x: 4, y: 7 };
    assert_eq!(
        crate::input::report_coords(&t, 3, 2, off),
        (3 * cw + 4, 2 * ch + 7),
        "a 1016 report on a never-resized tab must carry real device pixels"
    );
    assert_ne!(
        crate::input::report_coords(&t, 3, 2, off),
        (3 * PLACEHOLDER.0 + 4, 2 * PLACEHOLDER.1 + 7),
        "...and must not be the 8x16 placeholder's answer"
    );
}

/// OSC 1337: an image sized in pixels on a fresh tab occupies the footprint
/// the real font implies (`ceil(px / cell)`); sized at 4x3 real cells.
#[test]
fn osc_1337_pixel_sizing_on_a_fresh_tab_uses_the_real_cell_box() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let (cw, ch) = window_with_a_big_cell(&mut app, wid);
    app.apply_window_scale(wid);

    let (want_cols, want_rows) = (4u16, 3u16);
    let (px_w, px_h) = (u32::from(cw) * 4, u32::from(ch) * 3);
    // A pixel-sized request never decodes the payload (only Auto sizing reads the PNG
    // header), so any non-empty body exercises the same math.
    let b64 = aterm_codec::base64::encode(b"cell-px-1").expect("base64");
    let mut seq = format!("\x1b]1337;File=inline=1;width={px_w}px;height={px_h}px:").into_bytes();
    seq.extend_from_slice(b64.as_bytes());
    seq.extend_from_slice(b"\x1b\\");

    let term = app.pool.get(0).expect("session 0").term.clone();
    let mut t = term_lock(&term);
    t.process(&seq);
    assert_eq!(
        t.images_row(0).len(),
        usize::from(want_cols),
        "the image must be {want_cols} REAL cells wide ({px_w}px / {cw}px per cell)"
    );
    assert_eq!(
        t.images_row(usize::from(want_rows) - 1).len(),
        usize::from(want_cols),
        "...and its last row must be full width"
    );
    assert!(
        t.images_row(usize::from(want_rows)).is_empty(),
        "...and exactly {want_rows} real cells tall ({px_h}px / {ch}px per cell)"
    );
    assert_ne!(
        usize::from(want_cols),
        (px_w as usize).div_ceil(usize::from(PLACEHOLDER.0)),
        "the placeholder must imply a DIFFERENT footprint, or this proves nothing"
    );
}

/// A session is seeded with the cell box at construction (`spawn_session`),
/// before any frame is drawn.
#[test]
fn a_newborn_live_engine_carries_the_owning_windows_cell_box() {
    let build = |cell_px| {
        crate::spawn::new_live_terminal(24, 80, None, aterm_types::Appearance::Dark, cell_px)
    };
    assert_eq!(build(Some((11, 23))).cell_pixel_size(), (11, 23));
    // `None` (the boot session, whose backend is still building) keeps the
    // placeholder — `sync_cell_pixel_size` corrects it at the first raster seam.
    assert_eq!(build(None).cell_pixel_size(), PLACEHOLDER);
    // A zero axis would be a divide-by-zero in the OSC 1337 footprint math.
    assert_eq!(build(Some((0, 0))).cell_pixel_size(), (1, 1));
}

/// The boot session is spawned before the backend build is joined, so it cannot be seeded at
/// construction; the join corrects it with no frame or resize in between.
#[test]
fn the_backend_join_corrects_the_boot_sessions_placeholder() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    window_with_a_big_cell(&mut app, wid);

    // Windowed cold-launch shape: the boot engine at the placeholder, nothing
    // reported to this window yet, and the backend build still in flight.
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

    // A `Pending` slot has no metrics to give (asking it for any panics), so nothing is pushed.
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
        // SAFETY: `winsize` is four integers; zeroed is valid, and
        // `TIOCGWINSZ` fills it for an open pty master.
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

    // The windowed cold-launch seed: `spawn_session(.., cell_px: None, ..)`.
    aterm_pty::resize_with_cell_px(master, 24, 80, None);
    let seeded = winsize_of(master);
    assert_eq!(
        (seeded.ws_xpixel, seeded.ws_ypixel),
        (0, 0),
        "the boot seed really does open at 0x0 — without this the test could \
         pass on a pty that was never wrong"
    );

    // The correction seam the backend join reaches.
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
        // SAFETY: `winsize` is four integers; zeroed is valid, and
        // `TIOCGWINSZ` fills it for an open pty fd.
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
    // The boot seed, with no cell box yet.
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

    // The backend join, mid-hold.
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

    // Negative control: with no hold standing, the join writes the PTY.
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

/// `spawn_session`'s `cell_px` accepts `None`, which keeps the 8x16 placeholder. Only the boot
/// session (spawned before the backend build is joined) may pass it; every other spawn hands
/// over its window's real cell box. Read from the sources: a headless harness cannot fork the
/// PTYs these sites need.
#[test]
fn every_non_boot_spawn_site_hands_over_a_real_cell_box() {
    let sources = [
        ("app_tabs.rs", include_str!("app_tabs.rs")),
        ("app_window.rs", include_str!("app_window.rs")),
        ("app_restore.rs", include_str!("app_restore.rs")),
        ("lib.rs", include_str!("lib.rs")),
        // Every tab and split spawns here, on the spawner thread.
        ("tab_spawn.rs", include_str!("tab_spawn.rs")),
    ];
    // The one justification a bare `None` may carry, written at the call.
    const BOOT_MARKER: &str = "CELL-PX-1: the BOOT session has no metrics to be seeded from";
    let mut wired = Vec::new();
    let mut exempt = Vec::new();
    let mut unwired = Vec::new();
    for (file, src) in sources {
        let lines: Vec<&str> = src.lines().collect();
        for (n, line) in lines.iter().enumerate() {
            if !line.trim_end().ends_with("spawn_session(") {
                continue; // a mention in prose, not a call
            }
            // The argument list: `id, window, rows, cols, <cell_px>, ...`.
            let args = lines[n..(n + 14).min(lines.len())].join("\n");
            let site = format!("{file}:{}", n + 1);
            // Both wired spellings: a hoisted `let cell_px` and `self.spawn_cell_px(wid)`.
            if args.contains("cell_px") {
                wired.push(site);
            } else if args.contains(BOOT_MARKER) {
                exempt.push(site);
            } else {
                unwired.push(site);
            }
        }
    }
    assert!(
        unwired.is_empty(),
        "a `spawn_session` call passes no `cell_px` and claims no boot \
         exemption, so its newborn keeps the 8x16 placeholder: {unwired:?}"
    );
    assert!(
        wired.len() >= 5,
        "every tab / split / window / restore spawn must pass the owning \
         window's cell box; only these do: {wired:?}"
    );
    // Exactly one bare `None`: the boot spawn in `main_entry`.
    assert_eq!(
        exempt.len(),
        1,
        "only the BOOT session may keep the placeholder (its backend is still \
         building); these sites claim that exemption: {exempt:?}"
    );
    assert!(
        exempt[0].starts_with("lib.rs:"),
        "the boot exemption belongs to `main_entry` in lib.rs, not {:?}",
        exempt[0]
    );
}

/// The per-window memo avoids locks and never gates correctness: it keys on the resolved
/// cell size (so a face swap at an unchanged `font_px` still pushes), and an unchanged second
/// call is a no-op.
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

    // Zoom back down: a different resolved box must be pushed again.
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

    // An unchanged box is a memo hit: poison the engine's value and check the
    // second call leaves it alone while a real change still lands.
    term_lock(&app.pool.get(0).expect("session 0").term).set_cell_pixel_size(3, 5);
    app.apply_window_scale(wid);
    assert_eq!(
        term_lock(&app.pool.get(0).expect("session 0").term).cell_pixel_size(),
        (3, 5),
        "an unchanged resolved cell box must take no engine lock at all"
    );
}

/// A metrics change (`refresh_all_window_metrics`: font zoom, `font_size` /
/// `font_family` reload) reaches a background window's engines immediately,
/// with no redraw, resize or `apply_window_scale` after it.
#[test]
fn a_font_change_reaches_an_unpainted_windows_engines_at_the_change() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    // Seed at the default face through one raster seam, so the memo is warm
    // and the engine already carries the pre-change box.
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

    // The font change (the pinned-px shape a zoom or config reload takes), through the
    // metrics seam only.
    app.font_px = 24.0;
    app.font_px_explicit = true;
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
