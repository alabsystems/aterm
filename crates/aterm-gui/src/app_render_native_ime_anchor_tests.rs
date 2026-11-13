// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_app::{ActionInvocation, AppEvent};

#[test]
fn a_presented_native_field_replaces_the_terminal_ime_anchor_at_its_painted_caret() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
    app.dispatch_native_event(
        wid,
        AppEvent::Action(ActionInvocation {
            id: crate::native_ui::ActionId::new("settings/search"),
            value: None,
        }),
    )
    .unwrap();
    let (instance, view) = app.active_native_view(wid).unwrap();
    let stale = (3, 5, 7, 11);
    app.windows.get_mut(&wid).unwrap().last_ime_rect = Some(stale);
    let mut previous_x = None;
    for caret in [0, 3, 6] {
        app.on_ime_preedit(wid, "日本語".to_string(), Some((caret, caret)));
        let (plan, route) = app.active_visible_frame_layout(wid).unwrap();
        app.prepare_layout_coordinate_space_from_plan(wid, route, &plan);
        assert!(app.prepare_native_input_scratch_from_plan(wid, &plan));
        let before = app.windows[&wid].last_ime_rect;
        app.report_native_ime_cursor_area(wid, &plan);
        assert_eq!(app.windows[&wid].last_ime_rect, before);
        let stamp = app.windows[&wid]
            .native_ui_compiled
            .as_ref()
            .map(|frame| frame.stamp);
        app.first_present_done = true;
        app.finalize_successful_present(
            wid,
            crate::metrics::StartupPresentTiming::collapsed(Instant::now()),
            0,
            None,
            SuccessfulPresentRoute::Native {
                view,
                presented_stamp: stamp,
            },
            crate::VisibleContentRoute::Native { instance, view },
            HostVisualState::default(),
            None,
            &plan,
        );
        let (x, y, w, h) = app.windows[&wid].last_ime_rect.expect("native IME anchor");
        assert_ne!(
            (x, y, w, h),
            stale,
            "the parked terminal cannot own this field"
        );
        if let Some(previous_x) = previous_x {
            assert!(
                x > previous_x,
                "a middle IME caret moves the candidate anchor"
            );
        }
        previous_x = Some(x);

        let (artifact, lx, ly) = app
            .retained_native_leaf_at_pointer(wid, f64::from(x), f64::from(y))
            .expect("anchor lies on the retained native leaf");
        assert_eq!(artifact.view, view);
        let caret = artifact.compiled.ime_caret_rect().unwrap();
        assert!((lx - caret.x).abs() <= 1.0 / artifact.scale as f32);
        assert!((ly - caret.y).abs() <= 1.0 / artifact.scale as f32);
    }

    let plan = app.active_visible_leaf_plan(wid).unwrap();
    app.windows.get_mut(&wid).unwrap().win_px =
        Some(aterm_window::dpi::PhysicalSize::new(100, 100));
    let (caret, scale) = {
        let artifact = app
            .retained_native_leaf_artifact_from_plan(wid, view, true, &plan)
            .unwrap();
        (artifact.ime_caret.unwrap(), artifact.scale)
    };
    let (frame_x, frame_y) = app.frame_origin(wid);
    if cfg!(target_os = "linux") {
        assert_eq!(frame_y, 0, "Linux pins the frame to the window's top");
        return;
    }
    for (top, visible) in [
        (-1, true),
        (-(f64::from(caret.height) * scale).ceil() as i32 - 2, false),
    ] {
        let window = app.windows.get_mut(&wid).unwrap();
        let card = window.settings_card.as_mut().unwrap();
        let caret_x = frame_x + (f64::from(caret.x) * scale).floor() as i64;
        card.dx = u32::try_from(caret_x.max(20) - caret_x).unwrap();
        card.dy =
            u32::try_from(i64::from(top) - frame_y - (f64::from(caret.y) * scale).floor() as i64)
                .unwrap();
        window.last_ime_rect = Some(stale);
        assert!(
            app.retained_native_leaf_artifact_from_plan(wid, view, true, &plan)
                .is_some(),
            "the retained leaf still intersects the window"
        );
        app.report_native_ime_cursor_area(wid, &plan);
        if visible {
            let (x, y, w, h) = app.windows[&wid].last_ime_rect.unwrap();
            assert_eq!(y, 0);
            assert!(x >= 0 && x as u32 + w <= 100 && h <= 100);
            assert!((h as f64) < f64::from(caret.height) * scale + 1.0);
        } else {
            assert_eq!(
                app.windows[&wid].last_ime_rect,
                Some(stale),
                "fully cropped caret cannot publish an offscreen anchor"
            );
        }
    }
}
