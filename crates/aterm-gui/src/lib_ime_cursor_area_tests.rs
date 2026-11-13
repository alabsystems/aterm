// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{App, WindowId};
use aterm_window::dpi::PhysicalSize;

#[test]
fn same_cell_republishes_after_metric_padding_and_frame_origin_changes() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.backend.set_pad(12);
    app.backend.set_pad_top(2);
    app.windows.get_mut(&wid).unwrap().metrics.pad = 12;
    app.windows.get_mut(&wid).unwrap().metrics.pad_top = 2;
    let cell = (3, 5);
    app.report_ime_cursor_area(wid, cell, (0, 0), true);
    let initial = app.windows[&wid].last_ime_rect.expect("initial IME rect");

    app.windows.get_mut(&wid).unwrap().metrics.font_px *= 1.5;
    app.report_ime_cursor_area(wid, cell, (0, 0), true);
    let metric = app.windows[&wid].last_ime_rect.expect("metric IME rect");
    assert_ne!(
        metric, initial,
        "same cell must resize/reposition with DPI metrics"
    );
    assert_ne!((metric.2, metric.3), (initial.2, initial.3));

    app.windows.get_mut(&wid).unwrap().metrics.pad_top += 7;
    app.report_ime_cursor_area(wid, cell, (0, 0), true);
    let padded = app.windows[&wid].last_ime_rect.expect("padded IME rect");
    assert_eq!(padded.1, metric.1 + 7);

    let (cw, ch) = app.win_cell_size(wid);
    let (rows, cols, pad, pad_top) = {
        let ws = &app.windows[&wid];
        (
            ws.rows as usize,
            ws.cols as usize,
            ws.metrics.pad,
            ws.metrics.pad_top,
        )
    };
    app.windows.get_mut(&wid).unwrap().win_px = Some(PhysicalSize::new(
        (cols * cw + 2 * pad + cw.saturating_sub(1).max(2)) as u32,
        (rows * ch + pad_top + pad + ch.saturating_sub(1).max(2)) as u32,
    ));
    let origin = app.frame_origin(wid);
    assert!(origin.0 > 0, "fixture needs a horizontal frame remainder");
    if cfg!(target_os = "linux") {
        // Top-pinned vertical placement: the remainder lands below the
        // frame, so the IME rect's Y is origin-invariant by construction
        // and the X axis carries the origin-shift assertion.
        assert_eq!(origin.1, 0, "Linux pins the frame top");
    } else {
        assert!(origin.1 > 0, "fixture needs a vertical frame remainder");
    }
    app.report_ime_cursor_area(wid, cell, (0, 0), true);
    let shifted = app.windows[&wid].last_ime_rect.expect("shifted IME rect");
    assert_eq!(shifted.0, padded.0 + i32::try_from(origin.0).unwrap());
    assert_eq!(shifted.1, padded.1 + i32::try_from(origin.1).unwrap());
}

#[test]
fn re_enabling_ime_forgets_the_rect_memo_so_the_next_frame_re_sends_it() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let cell = (4, 9);
    app.report_ime_cursor_area(wid, cell, (0, 0), true);
    let parked = app.windows[&wid]
        .last_ime_rect
        .expect("caret rect reported");

    app.report_ime_cursor_area(wid, cell, (0, 0), true);
    assert_eq!(
        app.windows[&wid].last_ime_rect,
        Some(parked),
        "an unmoved caret keeps the memo (that is why it exists)"
    );

    app.on_ime_enablement_changed(wid);
    assert_eq!(
        app.windows[&wid].last_ime_rect, None,
        "Ime::Disabled must forget the rect"
    );

    app.on_ime_enablement_changed(wid);
    assert_eq!(
        app.windows[&wid].last_ime_rect, None,
        "Ime::Enabled must forget the rect"
    );
    app.report_ime_cursor_area(wid, cell, (0, 0), true);
    assert_eq!(
        app.windows[&wid].last_ime_rect,
        Some(parked),
        "the re-send resolves to the same rect the caret always had — before \
         the fix this call short-circuited on the stale memo and the \
         compositor kept no rectangle at all"
    );
}

#[test]
fn re_enabling_ime_also_ends_an_interrupted_composition() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.on_ime_preedit(wid, "日本".to_string(), Some((3, 3)));
    assert!(!app.windows[&wid].preedit.is_empty());

    app.on_ime_enablement_changed(wid);
    assert!(
        app.windows[&wid].preedit.is_empty(),
        "a focus cycle mid-compose must not leave the suppress gate stuck on"
    );
    assert_eq!(app.windows[&wid].preedit_caret, None);
}
