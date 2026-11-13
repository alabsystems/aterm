// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Instant;

use super::App;
use crate::WindowId;
use aterm_window::dpi::PhysicalSize;

#[test]
fn coherent_dims_snapshot_is_nonblocking_when_the_grid_is_busy() {
    let app = App::headless_for_test();
    let term = app.front_terminal(WindowId(0)).unwrap().term.clone();
    let snapshot = app.try_dims_snapshot(0, &term).unwrap();
    assert_eq!((snapshot.rows, snapshot.cols), (24, 80));

    let _held = term.lock().unwrap();
    assert_eq!(
        app.try_dims_snapshot(0, &term).unwrap_err(),
        "terminal busy; retry dims"
    );
}

#[test]
fn detached_dims_reports_the_scale_its_own_geometry_came_from() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let metrics = crate::MetricsView::for_scale(2.0);
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.scale = 2.0;
        ws.metrics = metrics;
    }
    app.font_px = metrics.font_px;
    app.backend.activate_px(metrics.font_px);
    app.backend.set_pad(metrics.pad);
    app.backend.set_pad_top(metrics.pad_top);
    app.backend.set_head(metrics.head);

    let attached = app.dims_snapshot(0, 24, 80);
    let detached = app.dims_snapshot(u64::MAX, 24, 80);
    assert_eq!(
        (detached.geometry, detached.window),
        ("detached", None),
        "no window contains this session"
    );
    assert!(
        (detached.scale - 2.0).abs() < f64::EPSILON,
        "the detached record reports the 2x it was derived from, not 1.0 \
         (scale={})",
        detached.scale
    );
    assert_eq!(
        (detached.scale, detached.font_px, detached.pad),
        (attached.scale, attached.font_px, attached.pad),
        "one backend, one scale: detaching a session cannot change the DPI \
         the same pixels were rasterized at"
    );
}

fn panes_rows_matching_the_engines(app: &App, panes: usize) -> Vec<(u64, u16, u16)> {
    let lines = app.read_pane_layout(None);
    assert_eq!(lines.len(), panes + 1, "a header and the panes: {lines:?}");
    assert!(
        lines[0].starts_with(&format!("layout tab=0 panes={panes} ")),
        "{}",
        lines[0]
    );
    let mut focused = 0;
    let mut rows_out = Vec::new();
    for line in &lines[1..] {
        let field = |key: &str| {
            line.split_whitespace()
                .find_map(|tok| tok.strip_prefix(key))
                .unwrap_or_else(|| panic!("{key} in {line}"))
                .to_string()
        };
        let sid: u64 = field("session=").parse().expect("session=<sid>");
        let rect = field("rect=");
        let (_, size) = rect
            .rsplit_once(',')
            .expect("rect=<row_off>,<col_off>,<rows>x<cols>");
        let (rows, cols) = size.split_once('x').expect("<rows>x<cols>");
        let (rows, cols): (u16, u16) = (rows.parse().unwrap(), cols.parse().unwrap());
        focused += usize::from(field("focused=") == "true");
        let term = app.pool.get(sid).expect("pane session").term.clone();
        let t = crate::term_lock(&term);
        assert_eq!(
            (t.rows(), t.cols()),
            (rows, cols),
            "`panes` must report the grid `dims` reads for this pane: {line}"
        );
        rows_out.push((sid, rows, cols));
    }
    assert_eq!(focused, 1, "exactly one focused pane: {lines:?}");
    rows_out
}

#[test]
fn panes_agrees_with_the_engines_where_the_tree_layout_does_not() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.split_active_stub_tab_dir(wid, crate::pane::SplitDir::Horizontal);
    app.split_active_stub_tab_dir(wid, crate::pane::SplitDir::Horizontal);
    let cols = app.windows[&wid].cols;
    app.windows.get_mut(&wid).expect("window 0").rows = 6;
    app.resize_panes(wid);

    let reported = panes_rows_matching_the_engines(&app, 3);

    let tree = app
        .active_tree(wid)
        .expect("an all-terminal tab has a tree");
    let old = tree.compute_layout(6, cols);
    assert!(
        old.iter().any(|rect| {
            reported
                .iter()
                .find(|(sid, ..)| *sid == rect.session)
                .is_some_and(|&(_, rows, cols)| (rows, cols) != (rect.rows, rect.cols))
        }),
        "negative control: the tree's layout must disagree with the engines \
         here, or this test proves nothing ({old:?} vs {reported:?})"
    );
}

#[test]
fn panes_reports_a_co_viewed_sessions_shared_grid_in_every_viewer() {
    let mut app = App::headless_for_test(); // window 0: 24x80, session 0
    let big = app
        .open_active_session_in_new_window_logical()
        .expect("share session 0 into a second window");
    assert_eq!(app.pool.views(0), Some(2));
    {
        let ws = app.windows.get_mut(&big).expect("the second window");
        ws.rows = 60;
        ws.cols = 200;
    }
    app.resize_panes(big);
    app.resize_panes(WindowId(0));

    let plan = app
        .active_visible_leaf_plan(big)
        .expect("the big window's plan");
    assert_eq!(
        App::leaf_grid_cells(&plan.leaves[0].rect),
        (60, 200),
        "negative control: the larger viewer's leaf is bigger than the shared grid"
    );
    for wid in [WindowId(0), big] {
        app.frontmost_window = Some(wid);
        assert_eq!(
            panes_rows_matching_the_engines(&app, 1),
            vec![(0, 24, 80)],
            "window {wid:?} reports the shared minimum"
        );
    }
}

#[test]
fn dims_snapshot_tracks_live_zoom_and_raw_surface_remainder() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.backend.set_pad(12);
    app.backend.set_pad_top(2);
    app.windows.get_mut(&wid).unwrap().metrics.pad = 12;
    app.windows.get_mut(&wid).unwrap().metrics.pad_top = 2;
    let initial = app.dims_snapshot(0, 24, 80);
    assert_eq!((initial.pad_top, initial.pad_bottom), (2, 12));
    assert_eq!(
        initial.frame_h,
        initial
            .composed_rows
            .saturating_mul(initial.cell_h)
            .saturating_add(initial.head)
            .saturating_add(initial.pad_top)
            .saturating_add(initial.pad_bottom),
        "dims exposes the same explicit top/bottom frame law as presentation",
    );
    assert_ne!(
        initial.frame_h,
        initial
            .composed_rows
            .saturating_mul(initial.cell_h)
            .saturating_add(initial.head)
            .saturating_add(initial.pad.saturating_mul(2)),
        "negative control: dims must not retain the old conserved-height law",
    );
    assert_eq!(initial.present_retry_state, "ready");
    app.windows
        .get_mut(&wid)
        .unwrap()
        .present_retry
        .on_recovery_redraw_requested();
    assert_eq!(app.dims_snapshot(0, 24, 80).present_retry_state, "redraw");
    let _ = app
        .windows
        .get_mut(&wid)
        .unwrap()
        .present_retry
        .on_external_stimulus();

    app.windows.get_mut(&wid).unwrap().present_retry.on_drop(
        crate::metrics::PresentDropReason::GpuTimeout,
        Instant::now(),
    );
    let backing_off = app.dims_snapshot(0, 24, 80);
    assert_eq!(backing_off.present_retry_state, "backoff");
    assert_eq!(backing_off.present_retry_count, 1);
    assert!(backing_off.present_retry_in_ms.is_some());
    let _ = app
        .windows
        .get_mut(&wid)
        .unwrap()
        .present_retry
        .on_external_stimulus();

    let surface = PhysicalSize::new(initial.frame_w + 11, initial.frame_h + 28);
    app.windows.get_mut(&wid).unwrap().win_px = Some(surface);
    let banded = app.dims_snapshot(0, 24, 80);
    assert_eq!(banded.geometry, "window");
    assert_eq!(
        (banded.surface_w, banded.surface_h),
        (surface.width, surface.height)
    );
    assert_eq!(
        banded.band_top + banded.band_bottom,
        banded.surface_h - banded.frame_h,
    );

    let old_font_size = app.font_size;
    app.windows.get_mut(&wid).unwrap().present_retry.on_drop(
        crate::metrics::PresentDropReason::GpuOccluded,
        Instant::now(),
    );
    let parked = app.dims_snapshot(0, 24, 80);
    assert_eq!(parked.present_retry_state, "parked");
    assert_eq!(parked.present_retry_in_ms, None);
    app.set_font_size(old_font_size + 8.0);
    let zoomed = app.dims_snapshot(0, 24, 80);
    assert_eq!(
        zoomed.present_retry_state, "ready",
        "font rebuild is an explicit recovery stimulus"
    );
    assert!(zoomed.cell_w > banded.cell_w && zoomed.cell_h > banded.cell_h);
    assert!(
        zoomed.frame_w <= banded.frame_w && zoomed.frame_h <= banded.frame_h,
        "a zoom re-grids the window inside the size it holds"
    );
    // A grid the surface does not hold (as before its window re-grids): the frame overhangs
    // the raw surface and dims crops it.
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.rows = 24;
        ws.cols = 80;
    }
    let zoomed = app.dims_snapshot(0, 24, 80);
    assert!(zoomed.frame_w > banded.frame_w && zoomed.frame_h > banded.frame_h);
    assert_eq!(
        (zoomed.surface_w, zoomed.surface_h),
        (surface.width, surface.height),
        "the raw surface is an independent live fact"
    );
    assert_eq!(
        zoomed.crop_left + zoomed.crop_right,
        zoomed.frame_w.saturating_sub(zoomed.surface_w)
    );
    assert_eq!(
        zoomed.crop_top + zoomed.crop_bottom,
        zoomed.frame_h.saturating_sub(zoomed.surface_h)
    );
}

/// `presented_seq=` is the grid's content clock in the frame the window's last own present
/// drew: a dropped present leaves it behind the grid, the retry that reaches glass brings it to
/// the grid's, a present a capture forces does not move it, another grid (the alternate
/// screen, and each new one a `?1049h` builds) reads `none` until a present draws it, and a
/// grid `?47` parks and puts back is still the one the frame drew.
#[test]
fn presented_seq_moves_only_with_the_windows_own_present_that_reached_glass() {
    use crate::present::scripted::ScriptedOutcome;
    use crate::scripted_redraw_fixture::{feed, scripted_app, surface};

    let wid = WindowId(0);
    let mut app = scripted_app();
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let grid = || crate::term_lock(&term).content_seq();
    let presented = |app: &App| app.try_dims_snapshot(0, &term).expect("dims").presented_seq;
    assert_eq!(presented(&app), None, "nothing presented yet");
    app.redraw_window(wid);
    assert_eq!(
        presented(&app),
        Some(grid()),
        "the first present drew the grid"
    );

    feed(&app, b"echo");
    let echoed = grid();
    surface(&mut app).fail_next(ScriptedOutcome::FailCommit);
    app.redraw_window(wid);
    let dropped = presented(&app);
    assert!(
        dropped.is_some_and(|seq| seq < echoed),
        "a dropped present drew nothing: {dropped:?} against the grid's {echoed}"
    );

    let ws = app.windows.get_mut(&wid).expect("window");
    let fired = match ws.present_retry.deadline {
        Some(deadline) => crate::service_due_presentation_clocks(
            false,
            &mut ws.present_retry,
            &mut ws.redraw_pending,
            ws.last_present_at,
            std::time::Duration::from_millis(8),
            deadline,
        ),
        None => ws.present_retry.on_external_stimulus(),
    };
    assert!(fired, "the retry fires");
    app.redraw_window(wid);
    assert_eq!(surface(&mut app).commits(), 2, "the retry presented");
    assert_eq!(presented(&app), Some(echoed), "the retry drew the echo");

    feed(&app, b" more");
    let later = grid();
    app.capture_present_budget = super::CapturePresentBudget::default();
    app.present_before_capture(wid, None, super::PresentTake::Model)
        .expect("the capture's present");
    assert_eq!(
        presented(&app),
        Some(echoed),
        "a capture's forced present leaves the window's own record"
    );
    feed(&app, b"!");
    let last = grid();
    assert!(last > later);
    app.redraw_window(wid);
    assert_eq!(
        presented(&app),
        Some(last),
        "the window's own present drew it"
    );

    feed(&app, b"\x1b[?1049h");
    assert_eq!(
        presented(&app),
        None,
        "the alternate screen is another grid, with its own clock"
    );
    app.redraw_window(wid);
    assert_eq!(presented(&app), Some(grid()), "its own present drew it");

    feed(&app, b"\x1b[?1049l\x1b[?1049hnew");
    assert_eq!(
        presented(&app),
        None,
        "a new alternate screen's clock starts over, so the last one's frame is not its"
    );
    app.redraw_window(wid);
    assert_eq!(presented(&app), Some(grid()), "its own present drew it");

    let drawn = presented(&app);
    feed(&app, b"\x1b[?47l\x1b[?47h");
    assert_eq!(
        presented(&app),
        drawn,
        "a grid parked and put back is the grid that frame drew, clock and all"
    );
}
