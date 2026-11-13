// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{App, image_req_for_test};
use crate::{Backend, MetricsView, WindowId, control_auth};
use aterm_core::terminal::CursorStyle;

#[derive(Debug, PartialEq)]
struct BoundContext {
    font_px: f32,
    cell_size: (usize, usize),
    pad: usize,
    pad_top: usize,
    head: usize,
    blink_phase: bool,
    cursor_style: Option<CursorStyle>,
    selection_inactive: bool,
}

fn bound_context(app: &App) -> BoundContext {
    let Backend::Cpu(renderer) = app.backend.ready() else {
        panic!("headless test backend is CPU")
    };
    BoundContext {
        font_px: app.font_px,
        cell_size: renderer.cell_size(),
        pad: renderer.pad(),
        pad_top: renderer.pad_top(),
        head: renderer.head(),
        blink_phase: renderer.cursor_blink_phase(),
        cursor_style: renderer.cursor_style_override(),
        selection_inactive: renderer.selection_inactive(),
    }
}

fn expected_frame_size(app: &App, wid: WindowId) -> (u32, u32) {
    let window = app.windows.get(&wid).expect("capture window");
    let metrics = window.metrics;
    let (cell_w, cell_h) = app.win_cell_size(wid);
    (
        u32::try_from(usize::from(window.cols) * cell_w + 2 * metrics.pad).unwrap(),
        u32::try_from(
            usize::from(window.rows) * cell_h + metrics.pad + metrics.pad_top + metrics.head,
        )
        .unwrap(),
    )
}

fn capture(app: &mut App, wid: WindowId, dir: &std::path::Path, name: &str) -> (u32, u32) {
    app.frontmost_window = Some(wid);
    let (tx, rx) = std::sync::mpsc::channel();
    app.render_image(image_req_for_test(
        control_auth::ConfinedImage::for_test(dir, name),
        false,
        false,
        None,
        tx,
    ));
    let mut retained = rx
        .recv_timeout(crate::app_introspect::WORKER_PATIENCE)
        .expect("image worker reply")
        .expect("image capture succeeds");
    retained
        .retention
        .as_mut()
        .expect("file capture retains its exact path")
        .prepare_write()
        .expect("capture identity survives to the simulated wire edge");
    let (width, height, _) = retained.value;
    (width, height)
}

#[test]
fn two_window_capture_rebinds_unequal_complete_render_contexts() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-window-render-context-{}",
        std::process::id()
    ));
    control_auth::ensure_private_dir(&dir).unwrap();
    let mut app = App::headless_for_test();
    app.render_knobs.selection_inactive = true;

    let first = WindowId(0);
    let session = app.next_session_id;
    let second = app.insert_logical_window(crate::stub_session(session), 24, 80);
    let mut first_metrics = MetricsView::for_scale(1.0);
    first_metrics.head = 3;
    let mut second_metrics = MetricsView::for_scale(2.0);
    second_metrics.head = 11;
    assert_ne!(
        first_metrics, second_metrics,
        "the regression must exercise genuinely unequal render contexts"
    );
    {
        let window = app.windows.get_mut(&first).unwrap();
        window.scale = 1.0;
        window.metrics = first_metrics;
        window.blink_phase = false;
        window.focused = false;
    }
    {
        let window = app.windows.get_mut(&second).unwrap();
        window.scale = 2.0;
        window.metrics = second_metrics;
        window.blink_phase = true;
        window.focused = true;
    }

    app.font_px = second_metrics.font_px;
    app.backend.activate_px(second_metrics.font_px);
    app.backend.set_pad(second_metrics.pad);
    app.backend.set_pad_top(second_metrics.pad_top);
    app.backend.set_head(second_metrics.head);
    app.backend.set_cursor_blink_phase(true);
    app.backend
        .set_cursor_style_override(Some(CursorStyle::HollowBlock));
    app.backend.set_selection_inactive(false);

    assert_eq!(
        capture(&mut app, first, &dir, "first.png"),
        expected_frame_size(&app, first),
        "A's capture dimensions come from A's 1x metrics"
    );
    assert_eq!(
        bound_context(&app),
        BoundContext {
            font_px: first_metrics.font_px,
            cell_size: app.win_cell_size(first),
            pad: first_metrics.pad,
            pad_top: first_metrics.pad_top,
            head: first_metrics.head,
            blink_phase: false,
            cursor_style: None,
            selection_inactive: true,
        },
        "A owns every renderer-global channel at its raster seam"
    );

    assert_eq!(
        capture(&mut app, second, &dir, "second.png"),
        expected_frame_size(&app, second),
        "B's capture dimensions come from B's 2x metrics"
    );
    assert_eq!(
        bound_context(&app),
        BoundContext {
            font_px: second_metrics.font_px,
            cell_size: app.win_cell_size(second),
            pad: second_metrics.pad,
            pad_top: second_metrics.pad_top,
            head: second_metrics.head,
            blink_phase: true,
            cursor_style: None,
            selection_inactive: false,
        },
        "B cannot inherit A's typography or cursor/selection globals"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
