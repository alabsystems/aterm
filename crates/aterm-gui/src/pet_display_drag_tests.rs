// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A window dragged from a 2x display to a 1x one under a sleeping pet, through the live path: the
//! headless App on a scripted CPU surface, drawn by the live present
//! ([`App::presented_frame_at`]) on a pinned host clock, the drag as macOS delivers it (the
//! window's metrics at the new scale, then a resize to its size in points at that scale,
//! `aterm-window`'s `handle_scale_factor_changed`), so the grid's columns change with its cells.

use crate::{App, WindowId};
use std::time::{Duration, Instant};

fn pinned<R>(t: Instant, f: impl FnOnce() -> R) -> R {
    crate::metrics::pin_host_clock(Some(t));
    let r = f();
    crate::metrics::pin_host_clock(None);
    r
}

/// A focused 12 by 100 window laid out at 2x before its first frame, presenting onto a scripted
/// CPU surface, its caret on `caret`.
fn window_at_2x(caret: (u16, u16), t0: Instant) -> App {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let _ = app.apply_term_resize(wid, 12, 100);
    crate::scripted_redraw_fixture::at_scale(&mut app, wid, 2.0);
    app.windows.get_mut(&wid).expect("window").present = Some(crate::PresentTarget::Cpu {
        surface: crate::present::CpuSurface::Scripted(Default::default()),
    });
    pinned(t0, || app.on_focus(wid, true));
    let at = t0 + Duration::from_millis(20);
    let (term, session) = {
        let m = app.front_terminal(wid).expect("terminal");
        (m.term.clone(), m.session)
    };
    crate::term_lock(&term).process_at(
        format!("\x1b[2J\x1b[{};{}H", caret.0 + 1, caret.1 + 1).as_bytes(),
        aterm_core::terminal::ClockReading {
            monotonic: at,
            wall_ms: None,
        },
    );
    pinned(at, || app.on_output_wake(session, at));
    app
}

/// Draws window 0 at `now`; the pet's hit rect `(x0, x1, y0, y1)` in window pixels when it is
/// drawn, and its action.
fn present(app: &mut App, now: Instant) -> (Option<(i32, i32, i32, i32)>, String) {
    let wid = WindowId(0);
    let _ = pinned(now, || app.presented_frame_at(wid, now)).expect("scripted surface");
    let c = app.windows[&wid].fx.companion();
    (c.hit_rect(), format!("{:?}", c.brain().action()))
}

/// The drag to a 1x display at `at`: the window's metrics at 1x (its font at 1x's device px, its
/// pads and head scaled), then its frame resized to its size in points at 1x, half its pixels.
fn drag_to_1x(app: &mut App, at: Instant) {
    let wid = WindowId(0);
    pinned(at, || {
        let (rows, cols) = {
            let ws = &app.windows[&wid];
            (ws.rows, ws.cols)
        };
        let px = app.window_frame_px_for(wid, rows, cols);
        let ws = app.windows.get_mut(&wid).expect("window");
        let (from, m) = (ws.scale, ws.metrics);
        let size = crate::app_window::font_size_of_device_px(m.font_px, from);
        let device = |px: usize| crate::logical_to_device_px(px as f32 / from as f32, 1.0);
        ws.scale = 1.0;
        ws.metrics = crate::MetricsView::applied(
            crate::font_px_for_scale(size, 1.0),
            device(m.pad),
            device(m.pad_top),
            device(m.head),
        );
        let half = aterm_window::dpi::PhysicalSize::new(
            (f64::from(px.width) / from).round() as u32,
            (f64::from(px.height) / from).round() as u32,
        );
        let _ = app.resize_window_to_px(wid, half);
    });
}

/// A WINDOW DRAGGED FROM A 2X DISPLAY TO A 1X ONE KEEPS ITS SLEEPING CAT ON ITS STATION
/// (aterm-effects' `PetBrain::keep_place_on_restation`, SPECS `regrid-keeps-rest`). The window
/// keeps its size in points, so its 15x28 px cells become 7x14 ones and its 100 columns 107, the
/// caret on its cell. The art is 5.23 columns wide on the 2x cells and 5.61 on the 1x ones, so
/// the station by a caret near the right edge (the body's right edge a cell short of the caret)
/// moves 0.37 columns; the one by a caret with room on its right (the body's left edge a cell
/// past the caret) does not. The cat's anchor stays within a pixel of its column on the first
/// 1x frame, asleep, and the 300 frames after draw it where that frame does. Before, the cat by
/// the right edge came down 2 px right of its station and glided onto it over 252 frames.
#[test]
fn a_window_dragged_to_a_1x_display_keeps_its_sleeping_cat_on_its_station() {
    let wid = WindowId(0);
    for (caret, right_anchor) in [((6u16, 40u16), false), ((6, 96), true)] {
        let t0 = crate::metrics::instant_at_us(crate::metrics::us_at(Instant::now()) + 1)
            .expect("the host clock's anchor");
        let mut app = window_at_2x(caret, t0);
        let mut t = t0 + Duration::from_millis(20);
        let end = t + Duration::from_secs(29);
        let mut before = (None, String::new());
        while t < end {
            t += Duration::from_millis(if end - t > Duration::from_secs(1) {
                50
            } else {
                16
            });
            before = present(&mut app, t);
        }
        let cell = app.win_cell_size(wid);
        let origin = app.frame_origin(wid).0;
        let (Some(was), "Sleep") = (before.0, before.1.as_str()) else {
            panic!("fixture: caret {caret:?}: a cat asleep on glass at 2x, got {before:?}");
        };
        t += Duration::from_millis(8);
        drag_to_1x(&mut app, t);
        let frames: Vec<_> = (0..300)
            .map(|_| {
                t += Duration::from_millis(16);
                present(&mut app, t)
            })
            .collect();
        let (cell1, origin1) = (app.win_cell_size(wid), app.frame_origin(wid).0);
        let ws = &app.windows[&wid];
        assert_eq!(
            (cell, cell1, (ws.rows, ws.cols)),
            ((15, 28), (7, 14), (12, 107)),
            "fixture: caret {caret:?}: the drag lays 15x28 px cells out as 7x14 ones, 100 \
             columns as 107"
        );
        let Some(now) = frames[0].0 else {
            panic!("caret {caret:?}: the cat is drawn on the first 1x frame");
        };
        let column =
            |x: i32, origin: i64, cell: usize| (i64::from(x) - origin) as f32 / cell as f32;
        let (anchor_was, anchor_now) = if right_anchor {
            (
                column(was.1, origin, cell.0),
                column(now.1, origin1, cell1.0),
            )
        } else {
            (
                column(was.0, origin, cell.0),
                column(now.0, origin1, cell1.0),
            )
        };
        assert!(
            (anchor_now - anchor_was).abs() * cell1.0 as f32 <= 1.0,
            "caret {caret:?}: the cat's anchor moved {:.3} columns at the drag ({was:?} at 2x, \
             {now:?} at 1x)",
            anchor_now - anchor_was
        );
        for (i, (hit, action)) in frames.iter().enumerate() {
            assert!(
                *hit == Some(now) && action == "Sleep",
                "caret {caret:?}: frame {i} after the drag draws {action} at {hit:?}, the first \
                 drew the sleeping cat at {now:?}"
            );
        }
    }
}
