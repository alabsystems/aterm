// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Settings page's frame, timed: the native compile a frame of a preview's
//! clock runs (`App::compiled_native_ui`, which renders the page and lays it
//! out), on the headless App, at a short window whose page sheds its chrome
//! and at a shipped one whose page keeps it. MEASURED by the
//! `settings_frame_cost` bench; that a frame of the clock decides no chrome
//! is ASSERTED by `a_short_windows_chrome_is_decided_once_per_layout_input`.
//! Compiled under the `bench-support` feature only.

use super::*;

use std::time::Instant;

/// One case: µs per frame at p50, p90 and p99 over `frames` frames.
pub(crate) struct FrameCost {
    pub(crate) what: &'static str,
    pub(crate) p50_us: f64,
    pub(crate) p90_us: f64,
    pub(crate) p99_us: f64,
}

/// `frames` frames of `route` at each window: the page compiled again with
/// nothing changed but the clock, each frame timed.
pub(crate) fn settings_frame_cost(frames: usize) -> Vec<FrameCost> {
    let cases: [(
        &'static str,
        crate::native_settings::SettingsRoute,
        (u32, u32),
    ); 4] = [
        (
            "Top Settings 150x24",
            crate::native_settings::SettingsRoute::Home,
            (1_078, 360),
        ),
        (
            "Top Settings 150x48",
            crate::native_settings::SettingsRoute::Home,
            (1_078, 700),
        ),
        (
            "Cursor & Motion 167x20",
            crate::native_settings::SettingsRoute::CursorMotion,
            (1_197, 312),
        ),
        (
            "Cursor & Motion 150x48",
            crate::native_settings::SettingsRoute::CursorMotion,
            (1_078, 700),
        ),
    ];
    let mut out = Vec::with_capacity(cases.len());
    for (what, route, (width, height)) in cases {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.on_resize(wid, aterm_window::dpi::PhysicalSize::new(width, height));
        assert!(app.open_settings_tab(route), "{what}: Settings opens");
        for _ in 0..16 {
            let _ = app.compiled_native_ui(wid).expect("a page");
        }
        let mut micros = Vec::with_capacity(frames);
        for _ in 0..frames {
            let t = Instant::now();
            let _ = std::hint::black_box(app.compiled_native_ui(wid).expect("a page"));
            micros.push(t.elapsed().as_secs_f64() * 1e6);
        }
        micros.sort_by(f64::total_cmp);
        let at = |q: f64| micros[((micros.len() - 1) as f64 * q) as usize];
        out.push(FrameCost {
            what,
            p50_us: at(0.5),
            p90_us: at(0.9),
            p99_us: at(0.99),
        });
    }
    out
}
