// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::{Duration, Instant};

use aterm_window::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase};

use crate::app_workers_tests::person_tab;
use crate::scripted_redraw_fixture::{feed, glass, scripted_app, surface};
use crate::tab_model::ViewId;
use crate::{App, WindowId};

use super::side_inset_tests::{glass_rgba, image_meta};
#[cfg(unix)]
use super::side_inset_tests::{observer, written_before_marker};

const WID: WindowId = WindowId(0);

fn fixture() -> App {
    let mut app = scripted_app();
    hold_motion_latch(&mut app);
    app.redraw_window(WID);
    let ws = &app.windows[&WID];
    let size = app.window_frame_px_for(WID, ws.rows, ws.cols);
    app.windows.get_mut(&WID).unwrap().win_px = Some(size);
    feed(&app, b"dock beside the program\r\nsecond line\x1b[?25l");
    app
}

/// Hold the load-shed latch, so wall-clock present costs on a loaded host cannot move a native
/// compile stamp between a present and the artifact read after it.
fn hold_motion_latch(app: &mut App) {
    app.perf_reduced = true;
    app.perf_flip_at = Some(Instant::now());
    app.perf_shed_dwell = Duration::MAX;
}

fn dock_view(app: &App) -> ViewId {
    app.windows[&WID]
        .dock
        .view
        .expect("the window owns a dock view")
}

fn show(app: &mut App, now: Instant) -> ViewId {
    if !app.first_present_done {
        app.redraw_window(WID);
    }
    app.set_dock_at(WID, 30, now)
        .expect("the fixture's window has room for the dock");
    app.publish_navigators(now);
    app.redraw_window(WID);
    dock_view(app)
}

fn rows_on_screen(app: &App, view: ViewId) -> Vec<String> {
    app.retained_native_pointer_artifact(WID, view)
        .expect("a presented dock")
        .compiled
        .hits
        .iter()
        .filter(|hit| hit.key.as_str().starts_with(crate::navigator::ROW))
        .map(|hit| hit.key.as_str().to_owned())
        .collect()
}

fn row_point(app: &App, view: ViewId, session: u64) -> (f64, f64) {
    let artifact = app
        .retained_native_pointer_artifact(WID, view)
        .expect("a presented dock");
    let key = format!("{}{session}", crate::navigator::ROW);
    let hit = artifact
        .compiled
        .hits
        .iter()
        .find(|hit| hit.key.as_str() == key)
        .expect("a painted conversation row");
    (
        artifact.device_x as f64 + f64::from(hit.rect.x + hit.rect.width * 0.5) * artifact.scale,
        artifact.device_y as f64 + f64::from(hit.rect.y + hit.rect.height * 0.5) * artifact.scale,
    )
}

fn ledger(app: &App, session: u64) -> String {
    app.pool
        .get(session)
        .unwrap()
        .ctx
        .timeline
        .lock()
        .unwrap()
        .resizes()
        .reply(128, None)
}

/// Every resize row program `session`'s ledger has booked, whatever its site: the reply head's
/// `total=`, so a second resize under any of the nine causes is counted.
fn resizes_booked(app: &App, session: u64) -> u64 {
    let reply = ledger(app, session);
    reply
        .lines()
        .next()
        .and_then(|head| {
            head.split_whitespace()
                .find_map(|word| word.strip_prefix("total="))
        })
        .and_then(|total| total.parse().ok())
        .unwrap_or_else(|| panic!("a ledger head with total=: {reply}"))
}

/// The least size window `WID` was last offered: what a platform window was told, recorded for
/// a headless one alike.
fn offered_minimum(app: &App) -> aterm_window::dpi::PhysicalSize<u32> {
    app.windows[&WID]
        .least_held
        .expect("the window was offered a minimum")
}

fn dock_image(app: &mut App, view: ViewId) -> (usize, usize, Vec<u8>) {
    let metadata = std::sync::Arc::new(std::sync::OnceLock::new());
    let (reply, rx) = std::sync::mpsc::channel();
    app.render_native_view_image(
        WID,
        view,
        crate::control::ImageReq {
            handoff: crate::control::ReplyRetentionPermit::from_own_budget(),
            target: crate::control_auth::ConfinedImage::in_memory(),
            clean: false,
            session: None,
            want_bytes: true,
            want_metadata: true,
            frame_metadata: std::sync::Arc::clone(&metadata),
            cancel: crate::control::CaptureCancellation::new(),
            reply,
        },
    );
    let (_, _, png) = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the image worker answers")
        .expect("the dock is captured")
        .value;
    let (rgba, width, height) =
        aterm_render::decode_png_rgba8(&png.expect("PNG bytes")).expect("a PNG");
    let metadata = metadata.get().expect("the view capture publishes metadata");
    assert_eq!(metadata.frame_kind, "native");
    assert_eq!(metadata.view, Some(view.get()));
    assert_eq!(
        metadata.leaves.len(),
        1,
        "the capture inventories its actual native host"
    );
    assert_eq!(metadata.leaves[0].view, view.get());
    assert_eq!(metadata.leaves[0].session, None);
    assert!(
        !metadata.leaves[0].focused,
        "the terminal keeps the keyboard"
    );
    (width, height, rgba)
}

#[test]
fn a_docked_view_paints_and_captures_beside_the_grid() {
    let mut app = fixture();
    let tabs = app.windows[&WID].tab_set.tabs().len();
    let leaves = app.windows[&WID].tab_set.active().unwrap().root.len();
    let view = show(&mut app, Instant::now());
    assert_eq!(app.windows[&WID].tab_set.tabs().len(), tabs);
    assert_eq!(
        app.windows[&WID].tab_set.active().unwrap().root.len(),
        leaves
    );
    assert!(
        app.windows[&WID]
            .tab_set
            .tabs()
            .iter()
            .all(|tab| !tab.root.contains(view)),
        "chrome is never a split leaf"
    );
    let dims = app.window_dims_snapshot().expect("window dimensions");
    assert_eq!(
        dims.dock,
        Some(view.get()),
        "dims names the view a client can inspect and capture"
    );
    assert_eq!(dims.dock_cells, 30);
    let wire = crate::control::serialize_dims(&dims);
    assert!(wire.contains(&format!("dock={} ", view.get())) && wire.contains("dock_cells=30 "));
    let artifact = app
        .retained_native_leaf_artifact(WID, view, true)
        .expect("the dock painted before any route returned");
    let (cw, _) = app.win_cell_size(WID);
    assert_eq!(artifact.width as usize, 30 * cw);
    assert!(artifact.device_x + i64::from(artifact.width) <= app.frame_origin(WID).0);
    assert!(
        artifact
            .compiled
            .hits
            .iter()
            .all(|hit| hit.key.as_str() != crate::navigator::FILTER),
        "pointer-only chrome has no dead filter"
    );
    assert!(
        !rows_on_screen(&app, view).is_empty(),
        "ignoring the dock cannot pass"
    );
    let rect = (
        artifact.device_x as usize,
        artifact.device_y as usize,
        artifact.width as usize,
        artifact.height as usize,
    );
    let expected = glass_rgba(&mut app, rect);
    assert!(
        expected
            .chunks_exact(4)
            .any(|pixel| pixel != &expected[..4]),
        "the list has actual painted content"
    );
    let (width, height, rgba) = dock_image(&mut app, view);
    assert_eq!((width, height), (rect.2, rect.3));
    assert!(
        rgba == expected,
        "the view's image is exactly its own presented pixels"
    );
    let inspected = app
        .inspect_app(crate::app_control::InspectRequest::View {
            view,
            projection: aterm_types::app_inspection::InspectionProjection::Text,
        })
        .expect("the window-owned view answers inspect");
    assert!(inspected.iter().any(|line| line.contains("idle")));
}

#[test]
fn a_click_in_the_dock_reaches_the_view_and_no_pane() {
    #[cfg(unix)]
    let (mut wire, sink) = observer();
    #[cfg(unix)]
    let mut app = crate::scripted_redraw_fixture::scripted_app_with_sink(sink);
    #[cfg(not(unix))]
    let mut app = scripted_app();
    hold_motion_latch(&mut app);
    let first = app.focused_session_id(WID).unwrap();
    let second = person_tab(&mut app);
    app.switch_tab_in(WID, 0);
    feed(&app, b"\x1b[?1000h\x1b[?1006h");
    let view = show(&mut app, Instant::now());
    let point = row_point(&app, view, second);
    app.on_cursor_moved(WID, point.0, point.1);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(
        app.focused_session_id(WID),
        Some(second),
        "the row shows its conversation's tab"
    );
    assert_eq!(dock_view(&app), view, "the dock stays in its window");
    assert!(!app.windows[&WID].selecting);
    assert_eq!(app.windows[&WID].reported_buttons, 0);
    #[cfg(unix)]
    assert_eq!(
        written_before_marker(&mut app, first, &mut wire),
        "",
        "a click in the dock sent no mouse report to its neighbor"
    );
    #[cfg(not(unix))]
    let _ = first;
}

#[test]
fn a_wheel_over_the_dock_scrolls_the_list_and_no_pane() {
    #[cfg(unix)]
    let (mut wire, sink) = observer();
    #[cfg(unix)]
    let mut app = crate::scripted_redraw_fixture::scripted_app_with_sink(sink);
    #[cfg(not(unix))]
    let mut app = scripted_app();
    hold_motion_latch(&mut app);
    let first = app.focused_session_id(WID).unwrap();
    for _ in 0..40 {
        person_tab(&mut app);
    }
    app.switch_tab_in(WID, 0);
    feed(&app, b"\x1b[?1000h\x1b[?1006h");
    let view = show(&mut app, Instant::now());
    let before = rows_on_screen(&app, view);
    let point = row_point(&app, view, first);
    app.on_cursor_moved(WID, point.0, point.1);
    app.on_mouse_wheel_phased(
        WID,
        MouseScrollDelta::LineDelta(0.0, -8.0),
        TouchPhase::Moved,
    );
    app.publish_navigators(Instant::now());
    app.redraw_window(WID);
    assert_ne!(
        rows_on_screen(&app, view),
        before,
        "the wheel changes which conversations are visible"
    );
    assert_eq!(app.focused_session_id(WID), Some(first));
    assert_eq!(app.windows[&WID].reported_buttons, 0);
    #[cfg(unix)]
    {
        assert_eq!(written_before_marker(&mut app, first, &mut wire), "");
        let (cw, ch) = app.win_cell_size(WID);
        let origin = app.frame_origin(WID);
        app.on_cursor_moved(
            WID,
            (origin.0 as usize + app.win_pad(WID) + cw / 2) as f64,
            (origin.1 as usize + app.native_content_origin_y(WID) + ch / 2) as f64,
        );
        app.on_mouse_wheel_phased(
            WID,
            MouseScrollDelta::LineDelta(0.0, 1.0),
            TouchPhase::Moved,
        );
        assert!(
            written_before_marker(&mut app, first, &mut wire).contains("\x1b[<"),
            "negative control: a wheel over the pane reaches its program"
        );
    }
}

#[test]
fn showing_the_dock_inside_a_window_books_one_dock_resize_per_program() {
    let _stats = crate::stats_deck::tests::attach_stats();
    for carries_band in [false, true] {
        let mut app = fixture();
        app.config
            .sys
            .get_or_insert_with(Default::default)
            .stats_bars = Some("deck".into());
        app.windows.get_mut(&WID).unwrap().carries_band = carries_band;
        crate::stats_band::tests::seat(&mut app, WID);
        app.apply_window_scale(WID);
        let first = app.focused_session_id(WID).unwrap();
        let second = person_tab(&mut app);
        app.switch_tab_in(WID, 0);
        let size = app.held_inner_px(WID).unwrap();
        app.windows.get_mut(&WID).unwrap().win_px = Some(size);
        app.on_resize(WID, size);
        let sessions = [first, second];
        let before: Vec<_> = sessions
            .iter()
            .map(|&session| ledger(&app, session))
            .collect();
        let booked: Vec<_> = sessions
            .iter()
            .map(|&session| resizes_booked(&app, session))
            .collect();
        let cols = app.windows[&WID].cols;
        let rows = app.windows[&WID].rows;
        let metrics = app.win_cell_size(WID);
        let now = Instant::now();
        let view = show(&mut app, now);
        let artifact = app
            .retained_native_pointer_artifact(WID, view)
            .expect("a presented dock below the window head");
        // The dock's first row is the grid's first row, beside it and under the head. Main's
        // Disk Custody band grew the deck's head to 300 px, and this fixture's window is then
        // one pixel short of head plus grid, so the frame sits at y = -1: the head is not the
        // floor in that case, the grid's top is.
        let grid_top =
            app.frame_origin(WID).1 + i64::try_from(app.native_content_origin_y(WID)).unwrap();
        assert_eq!(
            artifact.device_y, grid_top,
            "carries_band={carries_band}: the dock starts at the grid's top"
        );
        assert!(
            app.frame_origin(WID).1 < 0
                || artifact.device_y >= i64::try_from(app.win_head(WID)).unwrap(),
            "carries_band={carries_band}: a dock in a frame that fits starts below the head"
        );
        assert_eq!(
            app.win_cell_size(WID),
            metrics,
            "the fixture's scale is settled before Show"
        );
        for (&session, before) in sessions.iter().zip(&before) {
            let term = crate::term_lock(&app.pool.get(session).unwrap().term);
            assert_eq!(
                term.cols(),
                cols - 30,
                "a background program also saw the resize: band={carries_band}, held={size:?}, metrics={metrics:?}"
            );
            assert_eq!(
                term.rows(),
                rows,
                "the resources head stays outside the dock commit"
            );
            drop(term);
            let after = ledger(&app, session);
            assert_eq!(
                after.matches("site=dock").count(),
                before.matches("site=dock").count() + 1
            );
            assert_eq!(
                after.matches("site=window").count(),
                before.matches("site=window").count()
            );
        }
        for (&session, &booked) in sessions.iter().zip(&booked) {
            assert_eq!(
                resizes_booked(&app, session),
                booked + 1,
                "Show and the frame after it resize program {session} once, under any site: band={carries_band}"
            );
        }
        app.set_dock_at(WID, 0, now + Duration::from_millis(400))
            .expect("a hide is never refused");
        // A resize the Hide owed to its next frame is booked by that frame: draw it.
        app.redraw_window(WID);
        for (&session, &booked) in sessions.iter().zip(&booked) {
            assert_eq!(
                resizes_booked(&app, session),
                booked + 2,
                "Hide and the frame after it resize program {session} once more, under any site: band={carries_band}"
            );
        }
        for (&session, before) in sessions.iter().zip(&before) {
            assert_eq!(
                crate::term_lock(&app.pool.get(session).unwrap().term).cols(),
                cols
            );
            assert_eq!(
                crate::term_lock(&app.pool.get(session).unwrap().term).rows(),
                rows
            );
            let after = ledger(&app, session);
            assert_eq!(
                after.matches("site=dock").count(),
                before.matches("site=dock").count() + 2
            );
            assert_eq!(
                after.matches("site=window").count(),
                before.matches("site=window").count()
            );
        }
    }
}

/// A newly supplied headless client wins over the cache of an older rendered window size.
#[test]
fn a_headless_dock_commit_uses_the_recorded_client_not_the_last_rendered_size() {
    let mut app = App::headless_for_test();
    hold_motion_latch(&mut app);
    let session = app.focused_session_id(WID).unwrap();
    let rendered = app.window_frame_px_for(WID, 24, 80);
    let held = app.window_frame_px_for(WID, 24, 140);
    app.on_resize(WID, held);
    app.windows.get_mut(&WID).unwrap().win_px = Some(rendered);
    assert_ne!(
        rendered, held,
        "negative control: the rendered cache is stale"
    );
    assert_eq!(app.held_inner_px(WID), Some(held));
    assert_eq!(
        crate::term_lock(&app.pool.get(session).unwrap().term).cols(),
        140
    );
    let before = ledger(&app, session);
    let booked = resizes_booked(&app, session);

    app.set_dock_at(WID, 30, Instant::now())
        .expect("a 140-column client takes the dock");

    let term = crate::term_lock(&app.pool.get(session).unwrap().term);
    assert_eq!(
        term.cols(),
        110,
        "Show takes 30 cells from the supplied client"
    );
    assert_eq!(term.rows(), 24);
    drop(term);
    assert_eq!(app.held_inner_px(WID), Some(held));
    let after = ledger(&app, session);
    assert_eq!(
        after.matches("site=dock").count(),
        before.matches("site=dock").count() + 1
    );
    assert_eq!(
        after.matches("site=window").count(),
        before.matches("site=window").count()
    );
    assert_eq!(
        resizes_booked(&app, session),
        booked + 1,
        "the program saw one resize, under any site"
    );
}

#[test]
fn a_second_dock_change_within_the_floor_books_nothing() {
    let mut app = fixture();
    let session = app.focused_session_id(WID).unwrap();
    let cols = app.windows[&WID].cols;
    let now = Instant::now();
    show(&mut app, now);
    let shown = ledger(&app, session);
    let booked = resizes_booked(&app, session);
    assert_eq!(app.windows[&WID].cols, cols - 30);
    app.set_dock_at(WID, 0, now + Duration::from_millis(1))
        .expect("a waiting hide is not refused");
    assert_eq!(
        ledger(&app, session),
        shown,
        "a pending Hide books no resize"
    );
    assert_eq!(app.windows[&WID].cols, cols - 30);
    assert!(!app.settle_dock_fall(now + Duration::from_millis(399)));
    assert_eq!(ledger(&app, session), shown);
    assert!(app.settle_dock_fall(now + Duration::from_millis(400)));
    app.redraw_window(WID);
    assert_eq!(app.windows[&WID].cols, cols);
    assert_eq!(
        ledger(&app, session).matches("site=dock").count(),
        shown.matches("site=dock").count() + 1
    );
    assert_eq!(
        resizes_booked(&app, session),
        booked + 1,
        "the settled hide and its frame resize the program once, under any site"
    );
    assert!(!app.settle_dock_fall(now + Duration::from_millis(401)));
    assert_eq!(
        app.dock_fall_due(),
        None,
        "the settle creates no recurring timer"
    );
}

#[test]
fn a_frame_capture_is_the_same_bytes_with_the_dock_shown() {
    let mut app = fixture();
    app.redraw_window(WID);
    let (before, _, _) = image_meta(&mut app, false);

    let size = app.windows[&WID].win_px.unwrap();
    let (cw, _) = app.win_cell_size(WID);
    let view = show(&mut app, Instant::now());
    // Hold the same frame dimensions in a wider client so the capture's exclusion of chrome
    // is tested independently from Show's deliberately smaller terminal grid.
    let wider = aterm_window::dpi::PhysicalSize::new(size.width + (30 * cw) as u32, size.height);
    app.windows.get_mut(&WID).unwrap().win_px = Some(wider);
    app.on_resize(WID, wider);
    app.publish_navigators(Instant::now());
    app.redraw_window(WID);
    assert!(
        !rows_on_screen(&app, view).is_empty(),
        "the comparison includes a painted dock"
    );
    let (after, _, _) = image_meta(&mut app, false);

    assert_eq!(
        (after.0, after.1),
        (before.0, before.1),
        "image keeps the frame dimensions"
    );
    assert!(
        after.2 == before.2,
        "image cuts out exactly the same frame bytes"
    );
    assert!(
        glass(&mut app).0.0 > before.0,
        "the client actually holds extra chrome pixels"
    );
}

#[test]
fn an_idle_or_covered_dock_reads_and_publishes_nothing() {
    let mut app = fixture();
    let session = app.focused_session_id(WID).unwrap();
    let now = Instant::now();
    let view = show(&mut app, now);
    app.publish_navigators(now + Duration::from_secs(2));
    app.redraw_window(WID);
    let work = app.navigator_work();
    let tree = app.navigator_tree().unwrap();
    let paint = app.windows[&WID].leaf_render_cache[&view].native_paint_serial;
    app.publish_navigators(now + Duration::from_secs(10));
    assert_eq!(app.navigator_work(), work);
    assert!(std::sync::Arc::ptr_eq(
        &tree,
        &app.navigator_tree().unwrap()
    ));
    assert_eq!(
        app.windows[&WID].leaf_render_cache[&view].native_paint_serial,
        paint
    );
    assert_eq!(app.dock_fall_due(), None);
    app.on_occlusion_changed(WID, true, std::time::Instant::now());
    app.navigator_mark_row(session);
    app.navigator_mark_words(session);
    app.publish_navigators(now + Duration::from_secs(61));
    assert_eq!(
        app.navigator_work(),
        work,
        "a covered dock reads neither rows nor words"
    );
    assert_eq!(app.navigator_wake(), None);
    assert_eq!(
        app.windows[&WID].leaf_render_cache[&view].native_paint_serial,
        paint
    );
    app.on_occlusion_changed(WID, false, std::time::Instant::now());
    app.publish_navigators(now + Duration::from_secs(62));
    assert!(
        app.navigator_wake().is_some(),
        "negative control: a visible list arms its age wake"
    );
}

#[test]
fn the_dock_paints_before_native_and_heterogeneous_routes_return() {
    let mut app = fixture();
    let size = app.window_frame_px_for(WID, 24, 140);
    app.windows.get_mut(&WID).unwrap().win_px = Some(size);
    app.on_resize(WID, size);
    let session = app.focused_session_id(WID).unwrap();
    assert_eq!(
        crate::term_lock(&app.pool.get(session).unwrap().term).cols(),
        140
    );
    let view = show(&mut app, Instant::now());
    assert_eq!(
        crate::term_lock(&app.pool.get(session).unwrap().term).cols(),
        110
    );
    assert_eq!(app.open_navigator(false), Ok(false));
    app.publish_navigators(Instant::now());
    app.redraw_window(WID);
    assert!(matches!(
        app.active_visible_frame_layout(WID).unwrap().1,
        crate::VisibleContentRoute::Native { .. }
    ));
    assert!(!rows_on_screen(&app, view).is_empty());
    let native = app.active_native_view(WID).unwrap().1;
    assert_ne!(
        native, view,
        "the old tab seat is a separate view of the shared list"
    );
    assert!(
        app.retained_native_leaf_artifact(WID, native, true)
            .is_some()
    );
    app.switch_tab_in(WID, 0);
    assert_eq!(app.open_navigator(true), Ok(true));
    app.publish_navigators(Instant::now());
    app.redraw_window(WID);
    assert_eq!(
        app.active_visible_frame_layout(WID).unwrap().1,
        crate::VisibleContentRoute::Heterogeneous
    );
    assert!(!rows_on_screen(&app, view).is_empty());
    let split = app.active_native_view(WID).unwrap().1;
    assert_ne!(split, view, "the ST-04 left split seat remains available");
    assert!(
        app.retained_native_leaf_artifact(WID, split, true)
            .is_some()
    );
}

#[test]
fn leaving_the_dock_clears_its_hover_and_keeps_the_panes_target() {
    let mut app = fixture();
    let first = app.focused_session_id(WID).unwrap();
    let view = show(&mut app, Instant::now());
    let point = row_point(&app, view, first);
    app.on_cursor_moved(WID, point.0, point.1);
    assert!(
        app.native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .hovered
            .is_some(),
        "a row responds to the pointer"
    );
    let (cw, ch) = app.win_cell_size(WID);
    let origin = app.frame_origin(WID);
    app.on_cursor_moved(
        WID,
        (origin.0 as usize + app.win_pad(WID) + cw / 2) as f64,
        (origin.1 as usize + app.native_content_origin_y(WID) + ch / 2) as f64,
    );
    assert!(
        app.native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .hovered
            .is_none(),
        "the row loses hover when the pointer leaves the dock"
    );
    assert_eq!(app.focused_session_id(WID), Some(first));
    assert!(
        app.active_native_view(WID).is_none(),
        "pointer feedback never takes the keys"
    );
}

#[cfg(unix)]
#[test]
fn a_held_dock_press_and_a_held_program_press_keep_their_owners() {
    let (mut wire, sink) = observer();
    let mut app = crate::scripted_redraw_fixture::scripted_app_with_sink(sink);
    hold_motion_latch(&mut app);
    let first = app.focused_session_id(WID).unwrap();
    let second = person_tab(&mut app);
    app.switch_tab_in(WID, 0);
    feed(&app, b"\x1b[?1000h\x1b[?1006h");
    let view = show(&mut app, Instant::now());
    let dock = row_point(&app, view, second);
    let (cw, ch) = app.win_cell_size(WID);
    let origin = app.frame_origin(WID);
    let pane = (
        (origin.0 as usize + app.win_pad(WID) + cw / 2) as f64,
        (origin.1 as usize + app.native_content_origin_y(WID) + ch / 2) as f64,
    );
    app.on_cursor_moved(WID, dock.0, dock.1);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    assert!(
        app.windows[&WID].hover_pointer,
        "positive control: a native row hand"
    );
    app.on_modifiers_changed(WID, aterm_window::keyboard::ModifiersState::SUPER);
    assert!(
        app.windows[&WID].hover_pointer && app.windows[&WID].link_hover.is_none(),
        "a modifier refresh keeps the held dock native, without probing the last pane cell"
    );
    app.on_modifiers_changed(WID, aterm_window::keyboard::ModifiersState::empty());
    app.on_cursor_moved(WID, pane.0, pane.1);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(
        written_before_marker(&mut app, first, &mut wire),
        "",
        "a dock gesture never turns into program input"
    );
    assert!(!app.windows[&WID].selecting);
    assert_eq!(
        app.focused_session_id(WID),
        Some(first),
        "a dock press released in a pane is cancelled"
    );
    app.on_cursor_moved(WID, pane.0, pane.1);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    assert_ne!(
        app.windows[&WID].reported_buttons, 0,
        "negative control: the program owns its press"
    );
    app.on_cursor_moved(WID, dock.0, dock.1);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(app.windows[&WID].reported_buttons, 0);
    let reports = written_before_marker(&mut app, first, &mut wire);
    assert_eq!(
        reports.matches("\x1b[<").count(),
        2,
        "the program receives its own press and release only"
    );
    assert!(
        reports.ends_with('m'),
        "its held press ends even over the dock"
    );
    assert!(
        app.native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .pressed
            .is_none()
    );
    assert_eq!(
        app.focused_session_id(WID),
        Some(first),
        "a program release cannot activate another conversation"
    );
}

#[cfg(unix)]
#[test]
fn showing_the_dock_keeps_keyboard_input_on_the_terminal() {
    let (mut wire, sink) = observer();
    let mut app = crate::scripted_redraw_fixture::scripted_app_with_sink(sink);
    hold_motion_latch(&mut app);
    let first = app.focused_session_id(WID).unwrap();
    let view = show(&mut app, Instant::now());
    let paint = app.windows[&WID].leaf_render_cache[&view].native_paint_serial;
    app.input(
        WID,
        crate::input::InputEvent::Key {
            key: aterm_types::keyboard::Key::Character('x'),
            mods: aterm_types::keyboard::Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: aterm_types::keyboard::KeyEventType::Press,
        },
        crate::input::Source::Human,
    );
    assert_eq!(written_before_marker(&mut app, first, &mut wire), "x");
    assert_eq!(
        app.windows[&WID].leaf_render_cache[&view].native_paint_serial,
        paint
    );
    assert!(app.active_native_view(WID).is_none());
}

#[test]
fn a_show_cancels_a_queued_hide_without_another_resize() {
    let mut app = fixture();
    let session = app.focused_session_id(WID).unwrap();
    let now = Instant::now();
    let view = show(&mut app, now);
    let shown = ledger(&app, session);
    app.set_dock_at(WID, 0, now + Duration::from_millis(1))
        .expect("a waiting hide is not refused");
    assert!(app.dock_fall_due().is_some());
    app.set_dock_at(WID, 30, now + Duration::from_millis(2))
        .expect("a show that cancels a waiting hide is not refused");
    assert_eq!(app.dock_fall_due(), None);
    assert!(!app.settle_dock_fall(now + Duration::from_secs(1)));
    assert_eq!(ledger(&app, session), shown);
    assert_eq!(dock_view(&app), view);
}

#[test]
fn changing_the_shown_tab_tints_its_dock_row_without_taking_the_keys() {
    let mut app = fixture();
    let first = app.focused_session_id(WID).unwrap();
    let second = person_tab(&mut app);
    app.switch_tab_in(WID, 0);
    let now = Instant::now();
    let view = show(&mut app, now);
    let selected = |app: &App| {
        app.retained_native_leaf_artifact(WID, view, true)
            .unwrap()
            .compiled
            .paint
            .iter()
            .filter_map(|node| {
                let crate::native_ui::UiContent::Button(control) = &node.content else {
                    return None;
                };
                (node.key.as_str().starts_with(crate::navigator::ROW) && control.state.selected)
                    .then(|| {
                        assert!(control.state.muted, "the tinted row never claims the keys");
                        node.key.as_str().to_owned()
                    })
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        selected(&app),
        [format!("{}{first}", crate::navigator::ROW)]
    );
    let before = dock_image(&mut app, view);
    app.switch_tab_in(WID, 1);
    app.publish_navigators(now + Duration::from_secs(1));
    app.redraw_window(WID);
    assert_eq!(
        selected(&app),
        [format!("{}{second}", crate::navigator::ROW)]
    );
    assert_ne!(
        dock_image(&mut app, view),
        before,
        "the tint actually changes the dock's pixels"
    );
    assert!(app.active_native_view(WID).is_none());
    assert_eq!(app.focused_session_id(WID), Some(second));
}

#[test]
fn the_view_menu_has_show_navigator_on_option_command_o() {
    let menu = crate::menu::MENU_MODEL
        .iter()
        .find(|menu| menu.title == "View")
        .expect("the View menu");
    let (_, action, key, mods) = menu
        .entries
        .iter()
        .flat_map(crate::menu::MenuEntry::items)
        .find(|(label, ..)| *label == "Show Navigator")
        .expect("a product menu entry opens the dock");
    assert_eq!(
        action,
        crate::menu::MenuAction::ToggleNavigator,
        "the item shows the dock, not a navigator tab"
    );
    assert_eq!((key, mods), ("o", crate::menu::MenuMods::CommandOption));
    assert!(
        crate::keybinding::MENU_CMD_CHORDS
            .iter()
            .any(|&(chord, label)| chord == "cmd+alt+o" && label == "Show Navigator"),
        "Option-Command-O is the Show Navigator item's chord"
    );
    assert!(
        crate::keybinding::MENU_CMD_CHORDS
            .iter()
            .any(|&(chord, _)| chord == "cmd+ctrl+o"),
        "Overview keeps its chord until DK-5"
    );
}

#[test]
fn the_open_app_dock_form_seats_chrome_and_preserves_the_old_tab_forms() {
    let mut app = fixture();
    let tabs = app.windows[&WID].tab_set.tabs().len();
    let request =
        crate::app_control::parse_open("app navigator dock").expect("the dock form parses");
    app.open_app(request)
        .expect("the product command shows the navigator");
    app.publish_navigators(Instant::now());
    app.redraw_window(WID);
    let view = dock_view(&app);
    assert_eq!(app.windows[&WID].tab_set.tabs().len(), tabs);
    assert!(
        app.windows[&WID]
            .tab_set
            .tabs()
            .iter()
            .all(|tab| !tab.root.contains(view))
    );
    assert!(!rows_on_screen(&app, view).is_empty());
    assert!(crate::app_control::parse_open("app navigator").is_ok());
    assert!(crate::app_control::parse_open("app navigator split").is_ok());
    assert!(crate::app_control::parse_open("app navigator dock split").is_err());
}

#[test]
fn a_program_hold_outranks_a_dock_button_for_the_wheel() {
    #[cfg(unix)]
    let (mut wire, sink) = observer();
    #[cfg(unix)]
    let mut app = crate::scripted_redraw_fixture::scripted_app_with_sink(sink);
    #[cfg(not(unix))]
    let mut app = scripted_app();
    hold_motion_latch(&mut app);
    let first = app.focused_session_id(WID).unwrap();
    for _ in 0..40 {
        person_tab(&mut app);
    }
    app.switch_tab_in(WID, 0);
    feed(&app, b"\x1b[?1000h\x1b[?1006h");
    let view = show(&mut app, Instant::now());
    let dock = row_point(&app, view, first);
    let before = rows_on_screen(&app, view);
    app.on_cursor_moved(WID, dock.0, dock.1);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Right);
    let origin = app.frame_origin(WID);
    let (cw, ch) = app.win_cell_size(WID);
    app.on_cursor_moved(
        WID,
        origin.0 as f64 + (app.win_pad(WID) + cw / 2) as f64,
        origin.1 as f64 + (app.native_content_origin_y(WID) + ch / 2) as f64,
    );
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    app.on_cursor_moved(WID, dock.0, dock.1);
    app.on_mouse_wheel_phased(
        WID,
        MouseScrollDelta::LineDelta(0.0, -8.0),
        TouchPhase::Moved,
    );
    app.publish_navigators(Instant::now());
    app.redraw_window(WID);
    assert_eq!(
        rows_on_screen(&app, view),
        before,
        "the program's left-button gesture keeps its wheel while a dock button is held"
    );
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Right);
    #[cfg(unix)]
    assert!(
        written_before_marker(&mut app, first, &mut wire).contains("<65;"),
        "the program saw its wheel"
    );
}

#[test]
fn a_dock_press_released_after_leaving_the_window_is_cancelled() {
    let mut app = fixture();
    let first = app.focused_session_id(WID).unwrap();
    let second = person_tab(&mut app);
    app.switch_tab_in(WID, 0);
    let view = show(&mut app, Instant::now());
    let dock = row_point(&app, view, second);
    app.on_cursor_moved(WID, dock.0, dock.1);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    app.on_cursor_left(WID);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(
        app.focused_session_id(WID),
        Some(first),
        "a stale inside coordinate cannot activate a row after the pointer left"
    );
    assert!(
        app.native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .pressed
            .is_none()
    );
}

/// A modal layer takes a press or wheel over the dock instead of the list, never as well as it:
/// the press aims at another conversation's row, which would switch tabs if the dock saw it, and
/// the list is long enough to scroll.
#[test]
fn a_modal_layer_claims_a_dock_press_and_wheel_before_the_list() {
    let mut app = fixture();
    hold_motion_latch(&mut app);
    let session = app.focused_session_id(WID).unwrap();
    let other = person_tab(&mut app);
    for _ in 0..40 {
        person_tab(&mut app);
    }
    app.switch_tab_in(WID, 0);
    let view = show(&mut app, Instant::now());
    let at = row_point(&app, view, other);
    let rows = rows_on_screen(&app, view);
    let untouched = |app: &App, layer: &str| {
        let ws = &app.windows[&WID];
        assert_eq!(
            app.native_runtime
                .view_state(view)
                .unwrap()
                .common()
                .pressed,
            None,
            "{layer}: the dock row was not armed under the layer"
        );
        assert_eq!(ws.native_button_owners, [None; 5], "{layer}");
        assert_eq!(ws.page_buttons, 0, "{layer}");
    };
    app.tab_strip_rows = 1;
    app.config.tab_strip_rows = Some(1);
    assert!(app.open_tab_context_menu(WID, 0, 3, false));
    app.on_cursor_moved(WID, at.0, at.1);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    assert!(app.windows[&WID].tab_menu.is_none());
    untouched(&app, "tab menu");
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(app.focused_session_id(WID), Some(session), "tab menu");
    app.open_fact_menu(WID, 0, crate::claude_lights::Fact::Model, (0, 3), false);
    assert!(app.autopilot_menu.is_some());
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    assert!(app.autopilot_menu.is_none());
    untouched(&app, "fact menu");
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(app.focused_session_id(WID), Some(session), "fact menu");
    app.palette_enter();
    let before = app.windows[&WID].palette().unwrap().scroll_extent();
    assert!(
        before.1 > before.2,
        "positive control: the palette can scroll"
    );
    app.on_mouse_wheel(WID, MouseScrollDelta::LineDelta(0.0, -8.0));
    assert!(app.windows[&WID].palette().unwrap().scroll_extent().0 > before.0);
    untouched(&app, "palette");
    app.palette_exit();
    app.publish_navigators(Instant::now());
    app.redraw_window(WID);
    assert_eq!(
        rows_on_screen(&app, view),
        rows,
        "the palette's wheel did not scroll the list as well"
    );
    assert_eq!(app.focused_session_id(WID), Some(session));
    app.on_cursor_moved(WID, at.0, at.1);
    app.on_mouse_wheel_phased(
        WID,
        MouseScrollDelta::LineDelta(0.0, -8.0),
        TouchPhase::Moved,
    );
    app.publish_navigators(Instant::now());
    app.redraw_window(WID);
    assert_ne!(
        rows_on_screen(&app, view),
        rows,
        "positive control: with no layer up the same wheel scrolls the list"
    );
}

#[test]
fn a_clamped_client_keeps_the_dock_beside_the_frame_and_its_pointer() {
    let mut app = fixture();
    let view = show(&mut app, Instant::now());
    let (cw, _) = app.win_cell_size(WID);
    let held = app.windows[&WID].win_px.unwrap();
    // A platform can present a smaller client before its resize has regridded the frame.
    app.windows.get_mut(&WID).unwrap().win_px = Some(aterm_window::dpi::PhysicalSize::new(
        held.width - (10 * cw) as u32,
        held.height,
    ));
    app.redraw_window(WID);
    let host = app.dock_geometry(WID).unwrap();
    assert!(
        host.origin.0 < 0,
        "positive control: the column is clipped at the client edge"
    );
    assert_eq!(
        host.origin.0 + i64::from(host.size.0),
        app.frame_origin(WID).0
    );
    let at = app.cell_centre_px(WID, 0, 0).unwrap();
    assert!(
        at.0 < f64::from(host.size.0),
        "the old zero origin would steal this pane press"
    );
    app.on_cursor_moved(WID, at.0, at.1);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    assert!(app.windows[&WID].selecting);
    assert_eq!(app.windows[&WID].native_button_owners, [None; 5]);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(dock_view(&app), view);
}

#[test]
fn the_pointer_driver_aims_at_the_dock_beside_terminal_and_native_tabs() {
    use crate::app_mouse::{NativePointerTarget, PointerAction, PointerAt};
    struct ReleaseClock;
    impl Drop for ReleaseClock {
        fn drop(&mut self) {
            crate::metrics::pin_host_clock(None);
        }
    }
    // This is a geometry/dispatch fixture. Wall-clock CPU costs on a loaded host must not
    // change the motion policy after its successful present and fence that frame's readback.
    crate::metrics::pin_host_clock(Some(Instant::now()));
    let _clock = ReleaseClock;
    for native_front in [false, true] {
        let mut app = fixture();
        let wanted = person_tab(&mut app);
        if native_front {
            assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::KeyboardInput));
        } else {
            app.switch_tab_in(WID, 0);
        }
        let view = show(&mut app, Instant::now());
        let key = format!("{}{wanted}", crate::navigator::ROW);
        let moved = app
            .pointer_request(PointerAction::MoveNative(NativePointerTarget::Key(
                key.clone(),
            )))
            .expect("the visible dock can be aimed at without taking a tab's keys");
        assert!(
            matches!(moved, PointerAt::Native { view: found, key: Some(ref hit), .. }
            if found == view.get() && hit == &key),
            "native_front={native_front}; wanted dock {view:?} key {key}; reply={moved:?}; host={:?}; presented={:?}",
            app.dock_geometry(WID),
            app.windows[&WID].dock.presented
        );
        app.pointer_request(PointerAction::Click)
            .expect("a real pointer click in the dock");
        assert_eq!(app.focused_session_id(WID), Some(wanted));
        assert_eq!(dock_view(&app), view);
    }
}

#[test]
fn a_dock_release_claimed_by_stats_retires_its_press_without_later_activation() {
    let mut app = fixture();
    let first = app.focused_session_id(WID).unwrap();
    let second = person_tab(&mut app);
    app.switch_tab_in(WID, 0);
    let view = show(&mut app, Instant::now());
    let at = row_point(&app, view, second);
    app.on_cursor_moved(WID, at.0, at.1);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    assert!(
        app.native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .pressed
            .is_some(),
        "positive control: the row is armed"
    );
    {
        let mut ws = app.windows.get_mut(&WID).unwrap();
        ws.stats_lens.open = Some(crate::stats_lens::Lens {
            focus: crate::stats_paint::Instrument::Cpu,
            bar: crate::stats_paint::Bar::Global,
            how: crate::stats_lens::Opened::Pinned,
        });
        ws.stats_lens.rect = Some((0, 0, 2_000, 1_000));
    }
    let origin = app.frame_origin(WID);
    app.on_cursor_moved(WID, origin.0 as f64 + 100.0, origin.1 as f64 + 100.0);
    assert!(app.stats_owns_pointer(WID));
    assert!(
        app.native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .pressed
            .is_some(),
        "moving over Stats leaves the captured press for release to retire"
    );
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(
        app.native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .pressed,
        None,
        "a claimed release cannot leave an armed row without custody"
    );
    assert_eq!(app.windows[&WID].page_buttons, 0);
    assert_eq!(app.windows[&WID].native_button_owners, [None; 5]);
    assert_eq!(app.focused_session_id(WID), Some(first));
    {
        let mut ws = app.windows.get_mut(&WID).unwrap();
        ws.stats_lens.open = None;
        ws.stats_lens.rect = None;
    }
    app.redraw_window(WID);
    app.on_cursor_moved(WID, at.0, at.1);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(
        app.focused_session_id(WID),
        Some(first),
        "an unrelated release cannot fire the old row"
    );
}

/// THE EXPLICIT-SHOW FLOOR: a Show is held to the window's own least size beside the column
/// (`least_window_size` with the inset), so the terminal keeps at least the least window's grid.
/// One pixel short of it, Show is refused in words where each act reports, and nothing changes.
#[test]
fn a_show_the_window_has_no_room_for_is_refused_and_books_nothing() {
    use aterm_window::dpi::PhysicalSize;
    let mut app = fixture();
    let session = app.focused_session_id(WID).unwrap();
    let (cw, _) = app.win_cell_size(WID);
    let least = app.least_window_size(WID);
    let (_, least_cols) = app.grid_dims_for(WID, least);
    let needs = least.width + u32::try_from(30 * cw).unwrap();
    let height = app.window_frame_px_for(WID, 24, 80).height;
    let short = PhysicalSize::new(needs - 1, height);
    app.windows.get_mut(&WID).unwrap().win_px = Some(short);
    app.on_resize(WID, short);
    let cols = app.windows[&WID].cols;
    assert_eq!(
        cols,
        least_cols + 29,
        "CONTROL: one pixel short of the floor holds 29 columns beside the least grid"
    );
    let booked = resizes_booked(&app, session);
    let offered = app.windows[&WID].least_held;
    let now = Instant::now();

    let refusal = app
        .set_dock_at(WID, 30, now)
        .expect_err("no room for the least grid beside the column");
    assert_eq!(
        refusal,
        super::DockRefusal::TooNarrow {
            has: least_cols + 29,
            needs: least_cols + 30,
        }
    );
    assert_eq!(app.windows[&WID].dock.cells, 0);
    assert!(
        app.windows[&WID].dock.view.is_none(),
        "a refused Show makes no view"
    );
    assert_eq!(app.windows[&WID].cols, cols);
    assert_eq!(
        crate::term_lock(&app.pool.get(session).unwrap().term).cols(),
        cols
    );
    assert_eq!(
        resizes_booked(&app, session),
        booked,
        "a refused Show resizes no program"
    );
    assert_eq!(
        app.windows[&WID].least_held, offered,
        "a refused Show offers no minimum"
    );
    assert_eq!(app.dock_fall_due(), None);

    // `aterm ctl open app navigator dock` replies with the refusal ...
    let request = crate::app_control::parse_open("app navigator dock").expect("the dock form");
    let reply = app
        .open_app(request)
        .expect_err("the control reply says the Show was refused");
    assert_eq!(reply, refusal.to_string());
    assert!(
        reply.starts_with("navigator not shown: the window is"),
        "{reply}"
    );
    // ... and the menu act (View > Show Navigator, Option-Command-O) leaves the same words for
    // `invoke`'s reply.
    app.toggle_navigator_dock();
    assert_eq!(
        app.invoke_reply("ToggleNavigator", false),
        Err(format!("ToggleNavigator: {refusal}"))
    );
    assert_eq!(app.windows[&WID].dock.cells, 0);
    assert_eq!(resizes_booked(&app, session), booked);

    // At the floor the Show stands, and the terminal keeps exactly the least window's grid.
    let exact = PhysicalSize::new(needs, height);
    app.windows.get_mut(&WID).unwrap().win_px = Some(exact);
    app.on_resize(WID, exact);
    assert_eq!(app.windows[&WID].cols, least_cols + 30);
    let booked = resizes_booked(&app, session);
    app.set_dock_at(WID, 30, now)
        .expect("a window at the floor takes the dock");
    assert_eq!(
        app.windows[&WID].cols, least_cols,
        "the floor: the least window's grid beside the column"
    );
    assert_eq!(resizes_booked(&app, session), booked + 1);
    let offered = offered_minimum(&app);
    assert!(offered.width <= exact.width && offered.height <= exact.height);
}

/// A client with no area (a minimized Windows window reads 0x0) holds no grid: neither a dock
/// act nor any other re-grid inside the size the window holds shrinks a program to 1x1, and a
/// Hide that waited past its floor is paid once, under `site=dock`, when the client comes back.
#[test]
fn a_window_with_no_client_keeps_every_grid_through_dock_acts_and_regrids() {
    use aterm_window::dpi::PhysicalSize;
    let mut app = fixture();
    let session = app.focused_session_id(WID).unwrap();
    let held = app.held_inner_px(WID).unwrap();
    let (rows, cols) = (app.windows[&WID].rows, app.windows[&WID].cols);
    let gone = PhysicalSize::new(0, 0);
    app.note_headless_px_for(WID, gone, (rows, cols));
    assert_eq!(
        app.held_inner_px(WID),
        Some(gone),
        "CONTROL: the window holds no client"
    );
    let booked = resizes_booked(&app, session);
    let now = Instant::now();

    let shown = app.set_dock_at(WID, 30, now);
    let grid = |app: &App| {
        let term = crate::term_lock(&app.pool.get(session).unwrap().term);
        (
            (app.windows[&WID].rows, app.windows[&WID].cols),
            (term.rows(), term.cols()),
        )
    };
    assert_eq!(
        grid(&app),
        ((rows, cols), (rows, cols)),
        "a Show into a window with no client re-grids no program"
    );
    assert_eq!(resizes_booked(&app, session), booked);
    assert_eq!(shown, Err(super::DockRefusal::NoClient));
    assert_eq!(app.windows[&WID].dock.cells, 0);
    // The other re-grids inside a held size (a chrome row, a font or padding change) enter here.
    app.on_resize(WID, gone);
    assert_eq!(
        grid(&app),
        ((rows, cols), (rows, cols)),
        "a re-grid inside a window with no client moves no program"
    );
    assert_eq!(
        resizes_booked(&app, session),
        booked,
        "no program is resized for a window with no client"
    );

    // The client comes back and takes the dock; its Hide then waits out the floor while the
    // client is gone again.
    app.note_headless_px_for(WID, held, (rows, cols));
    show(&mut app, now);
    let docked = (app.windows[&WID].rows, app.windows[&WID].cols);
    assert_eq!(docked, (rows, cols - 30));
    let shown = ledger(&app, session);
    let shown_booked = resizes_booked(&app, session);
    app.set_dock_at(WID, 0, now + Duration::from_millis(1))
        .expect("a waiting hide is not refused");
    app.note_headless_px_for(WID, gone, docked);
    assert!(
        !app.settle_dock_fall(now + Duration::from_millis(400)),
        "a waiting hide is not paid into a window with no client"
    );
    assert_eq!(
        app.dock_fall_due(),
        None,
        "and arms no deadline while the client is gone"
    );
    assert_eq!(app.windows[&WID].dock.cells, 30);
    assert_eq!((app.windows[&WID].rows, app.windows[&WID].cols), docked);
    assert_eq!(resizes_booked(&app, session), shown_booked);

    app.note_headless_px_for(WID, held, docked);
    assert!(
        app.dock_fall_due().is_some(),
        "the client's return makes the waiting hide due"
    );
    assert!(app.settle_dock_fall(now + Duration::from_millis(400)));
    app.redraw_window(WID);
    assert_eq!(app.windows[&WID].dock.cells, 0);
    assert_eq!(
        (app.windows[&WID].rows, app.windows[&WID].cols),
        (rows, cols)
    );
    assert_eq!(
        resizes_booked(&app, session),
        shown_booked + 1,
        "the hide is paid once"
    );
    assert_eq!(
        ledger(&app, session).matches("site=dock").count(),
        shown.matches("site=dock").count() + 1,
        "under site=dock"
    );
}

/// The initial-frame settle (Linux) holds the attach frame for two seconds. A Show inside it
/// commits inside that same frame, so the settle is rebased to the docked grid: the dock's own
/// re-grid reads as settled, and a late configure to another size is still corrected.
#[test]
fn a_show_inside_the_initial_frame_settle_is_not_corrected_back_to_the_attach_grid() {
    let mut app = fixture();
    let session = app.focused_session_id(WID).unwrap();
    let (rows, cols) = (app.windows[&WID].rows, app.windows[&WID].cols);
    let held = app.held_inner_px(WID).unwrap();
    app.windows.get_mut(&WID).unwrap().initial_frame_settle = Some(crate::InitialFrameSettle {
        rows,
        cols,
        deadline: Instant::now() + Duration::from_secs(60),
        corrections: 3,
        corrected: false,
    });
    assert!(
        !app.settle_initial_frame(WID, held),
        "CONTROL: the attach frame is settled"
    );
    show(&mut app, Instant::now());
    assert_eq!(app.windows[&WID].cols, cols - 30);
    let booked = resizes_booked(&app, session);

    assert!(
        !app.settle_initial_frame(WID, held),
        "the docked grid in the attach frame needs no correction"
    );
    let settle = app.windows[&WID]
        .initial_frame_settle
        .expect("the settle stays armed for a late configure");
    assert_eq!(
        (settle.rows, settle.cols, settle.corrections),
        (rows, cols - 30, 3)
    );
    assert_eq!(
        app.window_frame_px_for(WID, settle.rows, settle.cols),
        held,
        "the rebased settle still asks for the attach frame"
    );
    assert_eq!(app.windows[&WID].cols, cols - 30);
    assert_eq!(resizes_booked(&app, session), booked);

    let (cw, _) = app.win_cell_size(WID);
    let late =
        aterm_window::dpi::PhysicalSize::new(held.width - u32::try_from(cw).unwrap(), held.height);
    assert!(
        app.settle_initial_frame(WID, late),
        "a late configure to another size is still corrected"
    );
    assert_eq!(
        app.windows[&WID]
            .initial_frame_settle
            .expect("armed")
            .corrections,
        2
    );
}

/// Show and Hide offer the inside minimum and record it, headless or not: a client clamped below
/// the nominal height is never told a minimum above it, so an OS that grows a client to its
/// minimum (AppKit, Windows) books no second resize. Growth that does not reach the nominal keeps
/// the cap; an actual client that fits restores the nominal minimum once.
#[test]
fn neither_dock_act_grows_a_height_clamped_client_through_its_minimum() {
    use aterm_window::dpi::PhysicalSize;
    let mut app = fixture();
    let session = app.focused_session_id(WID).unwrap();
    let nominal = app.least_window_size(WID);
    let ch = u32::try_from(app.win_cell_size(WID).1).unwrap();
    let mut held = app.window_frame_px_for(WID, 24, 140);
    held.height = nominal.height.saturating_sub(ch).max(1);
    app.windows.get_mut(&WID).unwrap().win_px = Some(held);
    app.on_resize(WID, held);
    assert_eq!(app.held_inner_px(WID), Some(held));
    let rows = app.windows[&WID].rows;
    let now = Instant::now();
    for (cells, at) in [(30, now), (0, now + Duration::from_millis(400))] {
        app.set_dock_at(WID, cells, at)
            .expect("a 140-column window takes and gives back the dock");
        let grid = (app.windows[&WID].rows, app.windows[&WID].cols);
        assert_eq!(grid.0, rows);
        let nominal = app.least_window_size(WID);
        assert!(
            nominal.height > held.height,
            "positive control: the nominal height would grow this held client"
        );
        let offered = offered_minimum(&app);
        assert!(
            offered.width <= held.width && offered.height <= held.height,
            "dock {cells}: the act offered {offered:?}, above the held client {held:?}"
        );
        assert_eq!(
            offered.width, nominal.width,
            "dock {cells}: the axis the client holds is offered its nominal"
        );
        // AppKit and Windows grow the client to the minimum they are told: deliver that answer.
        let booked = resizes_booked(&app, session);
        let answered = PhysicalSize::new(
            held.width.max(offered.width),
            held.height.max(offered.height),
        );
        app.windows.get_mut(&WID).unwrap().win_px = Some(answered);
        app.on_resize(WID, answered);
        assert_eq!((app.windows[&WID].rows, app.windows[&WID].cols), grid);
        assert_eq!(
            resizes_booked(&app, session),
            booked,
            "neither Show nor Hide may book an OS minimum's second resize"
        );
        assert!(
            !app.minimum_fits_again(WID, PhysicalSize::new(nominal.width, held.height)),
            "width alone cannot restore a height-clamped minimum"
        );
    }

    let cap = offered_minimum(&app);
    let partial = PhysicalSize::new(held.width, held.height + ch / 2);
    assert!(partial.height < app.least_window_size(WID).height);
    app.windows.get_mut(&WID).unwrap().win_px = Some(partial);
    app.on_resize(WID, partial);
    // An offer after the partial growth: a resize short of the nominal makes none of its own,
    // so without one the cap could not move whatever its rule.
    app.push_least_window_size(WID);
    assert_eq!(
        offered_minimum(&app),
        cap,
        "partial growth cannot ratchet the temporary minimum"
    );

    let full = PhysicalSize::new(held.width, app.least_window_size(WID).height);
    assert!(
        app.minimum_fits_again(WID, full),
        "CONTROL: a client that fits meets a minimum still capped"
    );
    app.windows.get_mut(&WID).unwrap().win_px = Some(full);
    app.on_resize(WID, full);
    assert_eq!(
        offered_minimum(&app),
        app.least_window_size(WID),
        "an actual client that fits both axes restores the nominal minimum"
    );
    assert!(
        !app.minimum_fits_again(WID, full),
        "an offered nominal minimum needs no second setter"
    );
    assert_eq!(ledger(&app, session).matches("site=dock").count(), 2);
}

/// AppKit and Windows grow a client to the minimum they are told. Deliver that answer to the
/// minimum window `WID` was last offered and require that it moves no program.
fn answer_the_offered_minimum(app: &mut App, session: u64, step: &str) {
    let client = app.held_inner_px(WID).unwrap();
    let offered = offered_minimum(app);
    assert!(
        offered.width <= client.width && offered.height <= client.height,
        "{step}: offered {offered:?}, above the client {client:?}"
    );
    let grid = (app.windows[&WID].rows, app.windows[&WID].cols);
    let booked = resizes_booked(app, session);
    let answered = aterm_window::dpi::PhysicalSize::new(
        client.width.max(offered.width),
        client.height.max(offered.height),
    );
    app.windows.get_mut(&WID).unwrap().win_px = Some(answered);
    app.on_resize(WID, answered);
    assert_eq!(
        (app.windows[&WID].rows, app.windows[&WID].cols),
        grid,
        "{step}"
    );
    assert_eq!(
        resizes_booked(app, session),
        booked,
        "{step}: the OS's answer to the minimum books no second resize"
    );
}

/// Every offer a docked window makes is the inside one, whoever asks: a tab change's
/// `sync_window` (as a chrome-row, font or scale change) offers no minimum above the client, so
/// the Show's one resize is not followed by another under `site=window`. The cap a dock act set
/// outlives its Hide until a client fits the nominal, and no unrelated offer ratchets it up.
#[test]
fn a_tab_change_in_a_docked_window_offers_no_minimum_above_its_client() {
    use aterm_window::dpi::PhysicalSize;
    let mut app = fixture();
    let session = app.focused_session_id(WID).unwrap();
    person_tab(&mut app);
    app.switch_tab_in(WID, 0);
    let nominal = app.least_window_size(WID);
    let ch = u32::try_from(app.win_cell_size(WID).1).unwrap();
    let mut held = app.window_frame_px_for(WID, 24, 140);
    held.height = nominal.height.saturating_sub(ch).max(1);
    app.windows.get_mut(&WID).unwrap().win_px = Some(held);
    app.on_resize(WID, held);
    assert_eq!(app.held_inner_px(WID), Some(held));
    let now = Instant::now();
    show(&mut app, now);
    assert!(
        app.least_window_size(WID).height > held.height,
        "positive control: the nominal minimum would grow this client"
    );

    app.switch_tab_in(WID, 1);
    answer_the_offered_minimum(&mut app, session, "a tab change after Show");
    app.switch_tab_in(WID, 0);
    answer_the_offered_minimum(&mut app, session, "a tab change back");

    let cap = offered_minimum(&app);
    let partial = PhysicalSize::new(held.width, held.height + ch / 2);
    app.windows.get_mut(&WID).unwrap().win_px = Some(partial);
    app.on_resize(WID, partial);
    app.switch_tab_in(WID, 1);
    assert_eq!(
        offered_minimum(&app),
        cap,
        "an unrelated offer cannot ratchet the cap up to a client that grew short of the nominal"
    );
    answer_the_offered_minimum(&mut app, session, "a tab change after partial growth");

    app.set_dock_at(WID, 0, now + Duration::from_millis(400))
        .expect("a hide is never refused");
    app.switch_tab_in(WID, 0);
    answer_the_offered_minimum(&mut app, session, "a tab change after Hide");

    let full = PhysicalSize::new(held.width, app.least_window_size(WID).height);
    app.windows.get_mut(&WID).unwrap().win_px = Some(full);
    app.on_resize(WID, full);
    assert!(
        !app.windows[&WID].least_inside,
        "a client that fits lifts the cap"
    );
    app.switch_tab_in(WID, 1);
    assert_eq!(
        offered_minimum(&app),
        app.least_window_size(WID),
        "with the cap lifted and no dock, a tab change offers the nominal minimum"
    );
}

/// A FONT STEP HOLDS THE DOCK TO THE EXPLICIT-SHOW FLOOR. The dock is whole cells, so it grows
/// with them: at the least docked width (where a person's drag stops), a step up left the
/// terminal a column or two, and a zoom in any docked window took it to one. The step's one
/// re-grid sets the dock down where the client no longer holds the least window beside it, its
/// record kept, and the step back raises it, each one resize per program. The push before that
/// re-grid is the nominal least, uncapped: AppKit and Windows grow a client to it inside the
/// call, folded into the same re-grid. CONTROL: a window with room keeps its dock.
#[test]
fn a_font_step_keeps_the_terminal_its_least_grid_beside_the_dock() {
    use aterm_window::dpi::PhysicalSize;
    let mut app = fixture();
    let session = app.focused_session_id(WID).unwrap();
    let least_docked = app.least_window_size_with_inset(WID, app.side_inset_px_of(WID, 30));
    let held = PhysicalSize::new(
        least_docked.width,
        app.window_frame_px_for(WID, 24, 80).height,
    );
    app.windows.get_mut(&WID).unwrap().win_px = Some(held);
    app.on_resize(WID, held);
    show(&mut app, Instant::now());
    let docked_cols = app.windows[&WID].cols;
    let base = app.font_size;

    let booked = resizes_booked(&app, session);
    app.set_font_size(base * 1.6);
    let least_cols = app.grid_dims_for(WID, app.least_window_size(WID)).1;
    let cols = app.windows[&WID].cols;
    assert!(
        cols >= least_cols,
        "a font step left the terminal {cols} columns, under the least window's {least_cols}"
    );
    let ws = &app.windows[&WID];
    assert_eq!(ws.dock.cells, 0, "the dock was set down");
    assert_eq!(ws.dock.pref, Some(30), "its record is kept");
    assert_eq!(app.dock_view(WID), None);
    assert_eq!(
        resizes_booked(&app, session),
        booked + 1,
        "the step and the set-down are one resize"
    );

    app.set_font_size(base);
    let ws = &app.windows[&WID];
    assert_eq!(ws.dock.cells, 30, "the step back raised it");
    assert_eq!(ws.cols, docked_cols, "at the grid it had");
    assert!(app.dock_view(WID).is_some());
    assert_eq!(resizes_booked(&app, session), booked + 2);

    // A person's Hide while it is set down: no later step raises it.
    app.set_font_size(base * 1.6);
    assert_eq!(app.windows[&WID].dock.cells, 0, "PRECONDITION: set down");
    app.set_dock_at(WID, 0, Instant::now() + Duration::from_secs(3600))
        .expect("a hide is never refused");
    app.set_font_size(base);
    assert_eq!(app.windows[&WID].dock.cells, 0, "the person's Hide stands");
    assert_eq!(app.windows[&WID].dock.pref, Some(0));

    // CONTROL: a window with room keeps its dock through the step, and the push before the
    // re-grid asks for the nominal least, here above a height-clamped client.
    let mut app = fixture();
    let nominal = app.least_window_size(WID);
    let ch = u32::try_from(app.win_cell_size(WID).1).unwrap();
    let mut held = app.window_frame_px_for(WID, 24, 140);
    held.height = nominal.height.saturating_sub(ch).max(1);
    app.windows.get_mut(&WID).unwrap().win_px = Some(held);
    app.on_resize(WID, held);
    show(&mut app, Instant::now());
    assert!(
        app.windows[&WID].least_inside,
        "PRECONDITION: the Show capped the minimum to the clamped client"
    );
    app.windows.get_mut(&WID).unwrap().offers_for_test.clear();
    let base = app.font_size;
    app.set_font_size(base * 1.2);
    assert_eq!(
        app.windows[&WID].dock.cells, 30,
        "CONTROL: room for the dock"
    );
    let first = app.windows[&WID].offers_for_test.first().copied();
    assert!(
        first.is_some_and(|offer| offer.height > held.height),
        "the push before the step's re-grid is the nominal least: {first:?} against a \
         {held:?} client"
    );
}

/// A point inside each control of the dock's presented frame that `keep` accepts (its centre
/// column, at its top edge or its centre), with the control's key.
fn dock_controls(
    app: &App,
    view: ViewId,
    keep: impl Fn(f64, f64) -> bool,
) -> Vec<(f64, f64, crate::native_ui::UiKey)> {
    let artifact = app
        .retained_native_pointer_artifact(WID, view)
        .expect("a presented dock");
    let (dx, dy, scale) = (
        artifact.device_x as f64,
        artifact.device_y as f64,
        artifact.scale,
    );
    artifact
        .compiled
        .hits
        .iter()
        .filter(|hit| hit.rect.width > 4.0 && hit.rect.height > 4.0)
        .filter_map(|hit| {
            let x = dx + f64::from(hit.rect.x + hit.rect.width * 0.5) * scale;
            [
                dy + f64::from(hit.rect.y + 2.0) * scale,
                dy + f64::from(hit.rect.y + hit.rect.height * 0.5) * scale,
            ]
            .into_iter()
            .find(|&y| keep(x, y))
            .map(|y| (x, y, hit.key.clone()))
        })
        .collect()
}

fn dock_hovered(app: &App, view: ViewId) -> Option<crate::native_ui::UiKey> {
    app.native_runtime
        .view_state(view)
        .unwrap()
        .common()
        .hovered
        .clone()
}

/// THE DOCK REACHES NO PANE: motion over the dock in a split tab whose unfocused pane holds the
/// frame's first column sends that pane's program nothing. (The frame's hit tests clamp a
/// pointer left of the frame onto that column.)
#[cfg(unix)]
#[test]
fn a_dock_hover_in_a_split_tab_reports_no_motion_to_the_pane_beside_it() {
    let (mut left_wire, left_sink) = observer();
    let (mut right_wire, right_sink) = observer();
    let mut app = crate::scripted_redraw_fixture::scripted_app_with_sink(left_sink);
    hold_motion_latch(&mut app);
    let left = app.focused_session_id(WID).unwrap();
    let right =
        app.split_active_stub_tab_dir_with_sink(WID, crate::pane::SplitDir::Vertical, right_sink);
    assert_eq!(
        app.focused_session_id(WID),
        Some(right),
        "fixture: the right pane has focus"
    );
    for session in [left, right] {
        let term = app.pool.get(session).unwrap().term.clone();
        crate::term_lock(&term).process(b"\x1b[?1003h\x1b[?1006h");
    }
    let view = show(&mut app, Instant::now());
    let _ = written_before_marker(&mut app, left, &mut left_wire);
    let _ = written_before_marker(&mut app, right, &mut right_wire);
    let host = app.dock_geometry(WID).unwrap();
    let (_, ch) = app.win_cell_size(WID);
    let x = (host.origin.0 + i64::from(host.size.0) / 2).max(0) as f64;
    let rows = (host.size.1 as usize / ch).min(16);
    assert!(rows >= 4, "fixture: the dock spans several cell rows");
    let mut lit = 0;
    for row in 0..rows {
        app.on_cursor_moved(WID, x, (host.origin.1 as usize + row * ch + ch / 2) as f64);
        lit += usize::from(dock_hovered(&app, view).is_some());
    }
    assert!(
        lit > 0,
        "positive control: the pointer crossed lit dock rows"
    );
    assert_eq!(
        written_before_marker(&mut app, left, &mut left_wire),
        "",
        "the unfocused pane at the frame's first column saw no motion from the dock"
    );
    assert_eq!(written_before_marker(&mut app, right, &mut right_wire), "");
    let (row, col, ..) = app.visible_session_rect(WID, left).unwrap();
    let (px, py) = app.cell_centre_px(WID, row + 2, col + 2).unwrap();
    app.on_cursor_moved(WID, px, py);
    assert!(
        written_before_marker(&mut app, left, &mut left_wire).contains("\x1b[<35;"),
        "negative control: the same motion over the left pane reaches its program"
    );
}

/// A window with the dock shown and a paste question in its band, and a dock control at the
/// band's height (the band lies over the frame's top rows; the dock's top rows share its y).
fn dock_beside_a_paste_question() -> (App, ViewId, (f64, f64), crate::native_ui::UiKey) {
    let mut app = fixture();
    hold_motion_latch(&mut app);
    let first = app.focused_session_id(WID).unwrap();
    person_tab(&mut app);
    app.switch_tab_in(WID, 0);
    let view = show(&mut app, Instant::now());
    let lines: String = (0..12).map(|i| format!("echo {i}\n")).collect();
    app.paste_banner = Some(crate::paste_banner::PendingPaste::new(
        WID,
        Some(first),
        lines,
        crate::input::Source::Human,
        crate::input::PasteFraming::AtDrain,
    ));
    app.redraw_window(WID);
    let mut beside = dock_controls(&app, view, |x, y| app.band_banner_at(WID, x, y).is_some());
    beside.sort_by_key(|(.., key)| !key.as_str().starts_with(crate::navigator::ROW));
    let (x, y, key) = beside
        .into_iter()
        .next()
        .expect("positive control: a dock control at the question's height");
    (app, view, (x, y), key)
}

/// The question's band is the frame's: a dock control at its height lights and shows the hand.
#[test]
fn a_dock_row_beside_a_paste_question_lights() {
    let (mut app, view, (x, y), key) = dock_beside_a_paste_question();
    app.on_cursor_moved(WID, x, y);
    assert_eq!(
        dock_hovered(&app, view),
        Some(key),
        "the dock control lights beside the question"
    );
    assert!(app.windows[&WID].hover_pointer, "the hand over the control");
}

/// A press on a dock control at the question's height is the dock's: it arms the control and
/// the question stands.
#[test]
fn a_dock_press_beside_a_paste_question_leaves_the_question_standing() {
    let (mut app, view, (x, y), key) = dock_beside_a_paste_question();
    app.on_cursor_moved(WID, x, y);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    assert!(
        app.paste_banner.is_some(),
        "a press on the dock does not answer the question"
    );
    assert_eq!(
        app.native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .pressed,
        Some(key),
        "the press arms the dock control"
    );
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert!(app.paste_banner.is_some(), "nor does its release");
}

/// The dock lies inside the side inset: leaving a lit dock row for the inset strip above or
/// below the dock retires its hover, however often the pointer crossed that strip before.
#[test]
fn leaving_a_lit_dock_row_for_the_inset_beside_it_clears_the_row() {
    let mut app = fixture();
    app.tab_strip_rows = 1;
    app.config.tab_strip_rows = Some(1);
    hold_motion_latch(&mut app);
    let first = app.focused_session_id(WID).unwrap();
    let view = show(&mut app, Instant::now());
    let host = app.dock_geometry(WID).unwrap();
    let row = row_point(&app, view, first);
    let height = app.windows[&WID].win_px.unwrap().height;
    let below = host.origin.1 + i64::from(host.size.1);
    let strips: Vec<f64> = [
        (host.origin.1 >= 1).then(|| (host.origin.1 - 1) as f64),
        (below < i64::from(height)).then_some(below as f64),
    ]
    .into_iter()
    .flatten()
    .collect();
    assert!(
        !strips.is_empty(),
        "fixture: the inset holds pixels above or below the dock"
    );
    assert!(app.in_side_inset(WID, row.0));
    for strip in strips {
        app.on_cursor_moved(WID, row.0, strip);
        app.on_cursor_moved(WID, row.0, row.1);
        assert!(
            dock_hovered(&app, view).is_some(),
            "positive control: the row lights"
        );
        app.on_cursor_moved(WID, row.0, strip);
        assert_eq!(
            dock_hovered(&app, view),
            None,
            "the row loses its hover in the inset at y={strip}"
        );
        assert!(!app.windows[&WID].hover_pointer);
    }
}

/// A pane divider lies in the frame: at its height the dock shows its own shape, never the
/// divider's arrows (a press there is the dock's, not a divider drag).
#[test]
fn the_dock_beside_a_pane_divider_shows_no_divider_cursor() {
    let mut app = fixture();
    hold_motion_latch(&mut app);
    let top = app.focused_session_id(WID).unwrap();
    app.split_active_stub_tab_dir_with_sink(
        WID,
        crate::pane::SplitDir::Horizontal,
        std::sync::Arc::new(aterm_session::sink::SinkWriter::new(-1)),
    );
    show(&mut app, Instant::now());
    let (row, col, rows, _) = app.visible_session_rect(WID, top).unwrap();
    let (gx, gy) = app.cell_centre_px(WID, row + rows, col).unwrap();
    app.on_cursor_moved(WID, gx, gy);
    assert!(
        app.windows[&WID].hover_pointer && app.windows[&WID].native_text_cursor,
        "positive control: the divider's own row shows its arrows"
    );
    let host = app.dock_geometry(WID).unwrap();
    assert!(gy >= host.origin.1 as f64 && gy < (host.origin.1 + i64::from(host.size.1)) as f64);
    app.on_cursor_moved(
        WID,
        (host.origin.0 + i64::from(host.size.0) / 2).max(0) as f64,
        gy,
    );
    assert!(
        !app.windows[&WID].native_text_cursor,
        "the dock at the divider's height shows neither the divider's arrows nor an I-beam"
    );
}

/// A Show under a resting pointer takes the terminal's I-beam away from over the list at once,
/// and the dock's first frame gives the lit row its hand without a move.
#[test]
fn showing_the_dock_under_a_resting_pointer_gives_the_list_its_own_cursor() {
    let mut app = fixture();
    hold_motion_latch(&mut app);
    let first = app.focused_session_id(WID).unwrap();
    let now = Instant::now();
    let view = show(&mut app, now);
    let at = row_point(&app, view, first);
    app.set_dock_at(WID, 0, now + Duration::from_secs(1))
        .expect("a hide is never refused");
    app.redraw_window(WID);
    assert_eq!(app.windows[&WID].dock.cells, 0, "fixture: hidden");
    app.on_cursor_moved(WID, at.0, at.1);
    assert!(
        app.windows[&WID].native_text_cursor,
        "positive control: the terminal's I-beam under the pointer"
    );
    app.set_dock_at(WID, 30, now + Duration::from_secs(2))
        .expect("the fixture's window has room for the dock");
    assert!(
        !app.windows[&WID].native_text_cursor,
        "the list under a still pointer never shows the terminal's I-beam"
    );
    app.publish_navigators(now + Duration::from_secs(2));
    app.redraw_window(WID);
    let view = dock_view(&app);
    assert!(
        dock_hovered(&app, view).is_some(),
        "the first frame lights the row under the still pointer"
    );
    assert!(
        app.windows[&WID].hover_pointer,
        "and gives it the hand a move would"
    );
}

/// Present a tab holding a native page beside a terminal, so the page has an artifact to hit.
fn present_both_panes(app: &mut App) {
    assert!(app.prepare_heterogeneous_input_scratch(WID).is_some());
    for cache in app
        .windows
        .get_mut(&WID)
        .unwrap()
        .leaf_render_cache
        .values_mut()
    {
        if let Some(raster) = cache.native.as_mut() {
            raster.presented = true;
        }
    }
}

/// A Settings page split beside a focused terminal, dock never shown, both panes presented; and
/// a window pixel on one of the page's controls.
fn unfocused_native_split() -> (App, ViewId, (f64, f64)) {
    let mut app = crate::App::headless_for_test();
    hold_motion_latch(&mut app);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (_, native) = app.active_native_view(WID).expect("native front");
    let (terminal, _) =
        app.split_active_with_stub_terminal(WID, crate::tab_model::SplitAxis::Horizontal);
    assert_eq!(app.focused_session_id(WID), Some(terminal));
    assert!(app.active_native_view(WID).is_none());
    assert!(app.dock_view(WID).is_none(), "the dock was never shown");
    present_both_panes(&mut app);
    let artifact = app
        .retained_native_leaf_artifact(WID, native, true)
        .expect("the page's presented artifact");
    let at = artifact
        .compiled
        .hits
        .iter()
        .find(|hit| hit.rect.width > 4.0 && hit.rect.height > 4.0)
        .map(|hit| {
            (
                artifact.device_x as f64
                    + f64::from(hit.rect.x + hit.rect.width * 0.5) * artifact.scale,
                artifact.device_y as f64
                    + f64::from(hit.rect.y + hit.rect.height * 0.5) * artifact.scale,
            )
        })
        .expect("a control on the page");
    (app, native, at)
}

fn native_lit(app: &App, view: ViewId) -> Option<crate::native_ui::UiKey> {
    app.native_runtime
        .view_state(view)
        .unwrap()
        .common()
        .hovered
        .clone()
}

/// Press at the pointer to give the page focus, and present both panes again.
fn focus_the_page(app: &mut App, native: ViewId) {
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(
        app.active_native_view(WID).map(|(_, view)| view),
        Some(native),
        "fixture: the press moved focus to the page"
    );
    present_both_panes(app);
}

/// DOCK HIDDEN, MAIN'S HOVER: a native page split beside a focused terminal lights nothing under
/// the pointer's motion (a press there only moves focus), so nothing stays lit when the pointer
/// goes on to the terminal; once the page has focus the same pointer lights its control.
#[test]
fn with_the_dock_hidden_an_unfocused_native_split_pane_does_not_light() {
    let (mut app, native, (x, y)) = unfocused_native_split();
    let terminal = app.focused_session_id(WID).unwrap();
    app.on_cursor_moved(WID, x, y);
    assert_eq!(
        native_lit(&app, native),
        None,
        "an unfocused page does not light on motion"
    );
    let (row, col, ..) = app.visible_session_rect(WID, terminal).unwrap();
    let (tx, ty) = app.cell_centre_px(WID, row + 1, col + 1).unwrap();
    app.on_cursor_moved(WID, tx, ty);
    assert_eq!(
        native_lit(&app, native),
        None,
        "nothing on the page is left lit with the pointer over the terminal"
    );
    app.on_cursor_moved(WID, x, y);
    focus_the_page(&mut app, native);
    app.on_cursor_moved(WID, x + 1.0, y);
    assert!(
        native_lit(&app, native).is_some(),
        "positive control: the focused page lights under the same pointer"
    );
}

/// A single-pane Editor tab, dock never shown, in a window whose 12-point padding holds the
/// edges of the editor's buffer (a page under 440 points tall and 560 wide keeps only 6 points
/// of its own padding), presented; and a window pixel on the buffer inside that padding.
fn editor_buffer_in_the_window_padding() -> (App, ViewId, (f64, f64), aterm_tempfile::TempDir) {
    let mut app = crate::App::headless_for_test();
    hold_motion_latch(&mut app);
    let pad = 12;
    app.backend.set_pad(pad);
    let (cw, ch) = app.win_cell_size(WID);
    if let Some(mut window) = app.windows.get_mut(&WID) {
        window.metrics.pad = pad;
        window.cols = u16::try_from((520 - pad * 2) / cw.max(1)).unwrap();
        window.rows = u16::try_from((400 - pad * 2) / ch.max(1)).unwrap();
    }
    let dir = aterm_tempfile::tempdir().expect("scratch folder");
    let path = dir.path().join("pad.txt");
    std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
    let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
    app.open_document_tab(crate::native_app::AppKind::Editor, &uri)
        .unwrap();
    let (_, view) = app.active_native_view(WID).expect("Editor active");
    assert!(app.dock_view(WID).is_none(), "the dock was never shown");
    assert!(app.prepare_native_input_scratch(WID));
    app.windows
        .get_mut(&WID)
        .unwrap()
        .leaf_render_cache
        .get_mut(&view)
        .and_then(|cache| cache.native.as_mut())
        .expect("retained native raster")
        .presented = true;
    let artifact = app
        .retained_native_leaf_artifact(WID, view, true)
        .expect("the page's presented artifact");
    let buffer = artifact
        .compiled
        .hits
        .iter()
        .find(|hit| hit.key.as_str() == "editor/buffer")
        .expect("the editor's buffer");
    let (left, top) = (
        artifact.device_x as f64 + f64::from(buffer.rect.x) * artifact.scale,
        artifact.device_y as f64 + f64::from(buffer.rect.y) * artifact.scale,
    );
    let (right, bottom) = (
        left + f64::from(buffer.rect.width) * artifact.scale,
        top + f64::from(buffer.rect.height) * artifact.scale,
    );
    let (ox, oy) = app.frame_origin(WID);
    let ws = &app.windows[&WID];
    let grid_left = ox as f64 + pad as f64;
    let grid_right = grid_left + (usize::from(ws.cols) * cw) as f64;
    let grid_top = oy as f64 + (app.win_pad_top(WID) + app.win_head(WID)) as f64;
    let grid_bottom = grid_top + (usize::from(ws.rows) * ch) as f64;
    let in_pad = |x: f64, y: f64| x < grid_left || x >= grid_right || y >= grid_bottom;
    let (mid_x, mid_y) = ((left + right) / 2.0, (top + bottom) / 2.0);
    let at = [
        (left + 1.0, mid_y),
        (right - 1.0, mid_y),
        (mid_x, bottom - 1.0),
    ]
    .into_iter()
    .find(|&(x, y)| in_pad(x, y))
    .expect("positive control: the buffer reaches into the window's padding");
    (app, view, at, dir)
}

/// DOCK HIDDEN, MAIN'S HOVER: the buffer in the window's padding lights under the pointer and
/// stays lit on the next frame.
#[test]
fn with_the_dock_hidden_a_control_in_the_window_padding_stays_lit() {
    let (mut app, view, at, _dir) = editor_buffer_in_the_window_padding();
    let lit = |app: &App| {
        app.native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .hovered
            .as_ref()
            .map(|key| key.as_str().to_owned())
    };
    app.on_cursor_moved(WID, at.0, at.1);
    assert_eq!(
        lit(&app).as_deref(),
        Some("editor/buffer"),
        "the control in the padding lights under the pointer"
    );
    app.windows.get_mut(&WID).unwrap().native_rehover = true;
    app.refresh_native_hover_for_frame(WID);
    assert_eq!(
        lit(&app).as_deref(),
        Some("editor/buffer"),
        "and stays lit on the next frame"
    );
}

/// DOCK HIDDEN, MAIN'S PRESS: the buffer in the window's padding takes a press there.
#[test]
fn with_the_dock_hidden_a_control_in_the_window_padding_takes_its_press() {
    let (mut app, view, at, _dir) = editor_buffer_in_the_window_padding();
    app.on_cursor_moved(WID, at.0, at.1);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    assert_eq!(
        app.native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .pressed
            .as_ref()
            .map(crate::native_ui::UiKey::as_str),
        Some("editor/buffer"),
        "the press in the padding arms the control under it"
    );
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
}

/// View > Show Navigator's act, which `invoke ToggleNavigator` names: Show where hidden, Hide
/// where shown (after the regrow floor), Show again where a Hide still waits.
#[test]
fn the_show_navigator_act_shows_hides_and_cancels_a_waiting_hide() {
    let mut app = fixture();
    let mut palette = crate::palette::PaletteState::new();
    palette.resolve(&app.palette_live());
    assert_eq!(
        palette.action_by_name("ToggleNavigator"),
        Ok(crate::menu::MenuAction::ToggleNavigator)
    );
    let cols = app.windows[&WID].cols;
    app.toggle_navigator_dock();
    assert_eq!(app.pending_action_refusal, None);
    assert!(app.dock_view(WID).is_some(), "the act shows the dock");
    assert_eq!(app.windows[&WID].cols, cols - 30);
    // Hold the regrow floor an hour out, so no wall-clock gap lets the Hide below land at once.
    app.windows
        .get_mut(&WID)
        .unwrap()
        .regrow_floor
        .rose(Instant::now() + Duration::from_secs(3600));
    app.toggle_navigator_dock();
    assert_eq!(
        app.windows[&WID].dock.wanted_cells(),
        0,
        "the act hides a shown dock"
    );
    let due = app
        .dock_fall_due()
        .expect("the hide waits for the regrow floor");
    app.toggle_navigator_dock();
    assert_eq!(
        app.windows[&WID].dock.wanted_cells(),
        30,
        "the act shows a dock whose hide still waits"
    );
    assert_eq!(app.dock_fall_due(), None);
    app.toggle_navigator_dock();
    assert!(app.settle_dock_fall(due + Duration::from_secs(1)));
    assert_eq!(app.windows[&WID].dock.cells, 0);
    assert_eq!(
        app.windows[&WID].cols, cols,
        "the hide gives the columns back"
    );
}

/// The dock's card serial, `None` while no card is held.
fn dock_card_serial(app: &App) -> Option<u64> {
    app.windows[&WID].dock_card.as_ref().map(|card| card.serial)
}

/// A dock re-raster that a compile-stamp input forces with no invalidation of the dock (Low Power
/// Mode turning on here; the load-shed latch and the appearance preferences move the same stamp)
/// reaches glass on the Terminal route: the list keeps a presented artifact, and a click on a row
/// right after it is taken. A terminal's unchanged-frame proof that leaves the dock out returns
/// before that present, and the click finds no presented row.
#[test]
fn a_dock_restamped_beside_an_idle_terminal_reaches_glass_and_keeps_its_clicks() {
    let mut app = fixture();
    let second = person_tab(&mut app);
    app.switch_tab_in(WID, 0);
    let view = show(&mut app, Instant::now());
    app.redraw_window(WID);
    assert!(matches!(
        app.active_visible_frame_layout(WID).unwrap().1,
        crate::VisibleContentRoute::Terminal { .. }
    ));
    let point = row_point(&app, view, second);
    let card = dock_card_serial(&app).expect("a dock card");
    let compiles = crate::app_native::native_ui_compile_count();
    let commits = surface(&mut app).commits();
    app.low_power = !app.low_power;
    app.redraw_window(WID);
    assert!(
        crate::app_native::native_ui_compile_count() > compiles
            && dock_card_serial(&app) != Some(card),
        "positive control: the stamp moved, so the dock compiled and rastered again"
    );
    assert_eq!(
        surface(&mut app).commits(),
        commits + 1,
        "the new dock raster is presented"
    );
    assert!(
        app.native_dock_artifact(WID, true, true).is_some(),
        "the presented dock answers the pointer"
    );
    app.on_cursor_moved(WID, point.0, point.1);
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
    assert_eq!(
        app.focused_session_id(WID),
        Some(second),
        "the click on the row is taken"
    );
}

/// With the dock shown over an idle terminal, nothing asks for a frame, compiles or presents.
#[test]
fn an_idle_dock_asks_for_no_frame_compile_or_present() {
    let mut app = fixture();
    let now = Instant::now();
    let view = show(&mut app, now);
    app.redraw_window(WID);
    let compiles = crate::app_native::native_ui_compile_count();
    let paint = app.windows[&WID].leaf_render_cache[&view].native_paint_serial;
    let card = dock_card_serial(&app).expect("a dock card");
    let asks = crate::frame_asks_for_test(&app);
    let commits = surface(&mut app).commits();
    app.publish_navigators(now);
    for _ in 0..3 {
        app.redraw_window(WID);
    }
    assert_eq!(
        crate::frame_asks_for_test(&app),
        asks,
        "an idle dock asks for no frame"
    );
    assert_eq!(
        crate::app_native::native_ui_compile_count(),
        compiles,
        "nor compiles"
    );
    assert_eq!(surface(&mut app).commits(), commits, "nor presents");
    assert_eq!(
        app.windows[&WID].leaf_render_cache[&view].native_paint_serial,
        paint
    );
    assert_eq!(dock_card_serial(&app), Some(card), "nor mints a card");
}

/// Stamping the dock on each redraw reads the config's revision, never a copy of the config: a
/// docked terminal's frames clone no config snapshot, where each used to copy the config text and
/// every value to read one number.
#[test]
fn stamping_the_dock_each_frame_copies_no_config() {
    let mut app = fixture();
    let now = Instant::now();
    show(&mut app, now);
    app.redraw_window(WID);
    let snapshots = crate::native_config_service::config_snapshots_on_this_thread();
    app.redraw_window(WID);
    feed(&app, b"x");
    app.redraw_window(WID);
    assert_eq!(
        crate::native_config_service::config_snapshots_on_this_thread(),
        snapshots,
        "a docked terminal's frames clone no config"
    );
}

/// A change in the terminal alone beside the dock presents the terminal's rows as damage: the
/// dock's raster, its card and the pixels it holds on glass are left as they are, and the glass
/// equals an independent full copy. A dock change after it presents the whole window with the new
/// dock composited.
#[test]
fn a_terminal_change_beside_the_dock_presents_its_rows_and_leaves_the_dock_alone() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = fixture();
    let second = person_tab(&mut app);
    app.switch_tab_in(WID, 0);
    app.redraw_window(WID);
    feed(&app, b"a");
    app.redraw_window(WID);
    let whole = surface(&mut app).whole_commits();
    let commits = surface(&mut app).commits();
    feed(&app, b"b");
    app.redraw_window(WID);
    let presented = {
        let glass = surface(&mut app);
        (glass.commits(), glass.whole_commits())
    };
    assert_eq!(
        presented,
        (commits + 1, whole),
        "positive control: without the dock a keystroke presents damage"
    );
    let view = show(&mut app, Instant::now());
    app.redraw_window(WID);
    let compiles = crate::app_native::native_ui_compile_count();
    let paint = app.windows[&WID].leaf_render_cache[&view].native_paint_serial;
    let card = dock_card_serial(&app).expect("a dock card");
    let whole = surface(&mut app).whole_commits();
    let commits = surface(&mut app).commits();
    feed(&app, b"c");
    app.redraw_window(WID);
    assert_eq!(
        surface(&mut app).commits(),
        commits + 1,
        "the keystroke presents"
    );
    assert_eq!(
        surface(&mut app).whole_commits(),
        whole,
        "the keystroke presents the terminal's rows, not the whole window with the dock"
    );
    assert_eq!(crate::app_native::native_ui_compile_count(), compiles);
    assert_eq!(
        app.windows[&WID].leaf_render_cache[&view].native_paint_serial,
        paint
    );
    assert_eq!(dock_card_serial(&app), Some(card));
    let after = glass(&mut app);
    app.windows.get_mut(&WID).unwrap().cpu_cache.invalidate();
    app.present_input_scratch_for_test(WID, false, None)
        .unwrap();
    assert!(
        glass(&mut app) == after,
        "the damaged present equals an independent full copy"
    );
    // A dock change, another conversation's word: a new card, composited by a whole present.
    let whole = surface(&mut app).whole_commits();
    mark_working(&mut app, second);
    app.publish_navigators(Instant::now() + Duration::from_secs(1));
    app.redraw_window(WID);
    assert_ne!(dock_card_serial(&app), Some(card), "the word is a new card");
    assert_eq!(
        surface(&mut app).whole_commits(),
        whole + 1,
        "a new dock card is composited by a whole present"
    );
    let marked = glass(&mut app);
    assert!(marked != after, "the new word reached glass");
    app.windows.get_mut(&WID).unwrap().cpu_cache.invalidate();
    app.present_input_scratch_for_test(WID, false, None)
        .unwrap();
    assert!(
        glass(&mut app) == marked,
        "and equals an independent full copy"
    );
}

/// Session `session`'s task turns Working, which changes its row's word in every navigator.
fn mark_working(app: &mut App, session: u64) {
    {
        let store = app.store.read().unwrap_or_else(|p| p.into_inner());
        let handle = store.by_local(session).expect("a live session");
        crate::task_record::mark_session_task(
            &handle.ctx,
            handle.local_id,
            &crate::task_record::TaskMark::new(crate::task_record::TaskState::Working),
        )
        .expect("unfenced");
    }
    app.on_task_changed(session);
}

/// A wheel tick over the dock repaints the dock alone: a native front tab's page keeps its card,
/// so the frame copies and compares no page pixels for a list beside it.
#[test]
fn a_dock_wheel_keeps_a_native_front_tabs_card() {
    let mut app = fixture();
    for _ in 0..40 {
        person_tab(&mut app);
    }
    app.switch_tab_in(WID, 0);
    let view = show(&mut app, Instant::now());
    assert_eq!(app.open_navigator(false), Ok(false));
    app.publish_navigators(Instant::now());
    app.redraw_window(WID);
    assert!(matches!(
        app.active_visible_frame_layout(WID).unwrap().1,
        crate::VisibleContentRoute::Native { .. }
    ));
    let page = app.windows[&WID]
        .settings_card
        .as_ref()
        .expect("the page's card")
        .serial;
    let before = rows_on_screen(&app, view);
    let artifact = app
        .retained_native_pointer_artifact(WID, view)
        .expect("a presented dock");
    let hit = artifact
        .compiled
        .hits
        .iter()
        .find(|hit| hit.key.as_str().starts_with(crate::navigator::ROW))
        .expect("a painted row");
    let point = (
        artifact.device_x as f64 + f64::from(hit.rect.x + hit.rect.width * 0.5) * artifact.scale,
        artifact.device_y as f64 + f64::from(hit.rect.y + hit.rect.height * 0.5) * artifact.scale,
    );
    app.on_cursor_moved(WID, point.0, point.1);
    app.redraw_window(WID);
    let page = app.windows[&WID]
        .settings_card
        .as_ref()
        .map_or(page, |card| card.serial);
    app.on_mouse_wheel_phased(
        WID,
        MouseScrollDelta::LineDelta(0.0, -8.0),
        TouchPhase::Moved,
    );
    app.redraw_window(WID);
    assert_ne!(
        rows_on_screen(&app, view),
        before,
        "positive control: the wheel scrolled the dock"
    );
    assert_eq!(
        app.windows[&WID]
            .settings_card
            .as_ref()
            .map(|card| card.serial),
        Some(page),
        "the page's card is kept"
    );
}

/// One row's change in the dock patches the dock's card where its raster changed: the card keeps
/// its buffer and names the rows it changed since the card a renderer holds, so neither the window
/// nor the renderer copies or compares the rest. The patched card is the raster's copy and is
/// presented.
#[test]
fn one_dock_rows_change_patches_its_card_in_place() {
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let mut app = fixture();
    let second = person_tab(&mut app);
    app.switch_tab_in(WID, 0);
    let now = Instant::now();
    let view = show(&mut app, now);
    app.redraw_window(WID);
    let (serial, buffer) = {
        let card = app.windows[&WID].dock_card.as_ref().expect("a dock card");
        (card.serial, card.rgba.as_ptr())
    };
    mark_working(&mut app, second);
    app.publish_navigators(now + Duration::from_secs(1));
    app.redraw_window(WID);
    let raster = app.windows[&WID].leaf_render_cache[&view]
        .native
        .as_ref()
        .expect("a retained raster");
    let crate::NativeRasterWork::Region { rect, .. } = raster.last_work else {
        panic!(
            "positive control: one row's region rastered, not {:?}",
            raster.last_work
        );
    };
    let card = app.windows[&WID].dock_card.as_ref().expect("a dock card");
    assert_ne!(card.serial, serial, "the patched card is a new card");
    assert_eq!(
        card.rgba.as_ptr(),
        buffer,
        "the card is patched in place, not copied"
    );
    assert_eq!(
        card.delta,
        Some(aterm_gpu::TrayDelta {
            since: serial,
            rows: (rect.y, rect.y + rect.height),
        }),
        "the card names the rows it changed since the card before it"
    );
    assert!(card.rgba == raster.rgba, "the card is the raster's copy");
    assert!(app.native_dock_artifact(WID, true, true).is_some());
}
