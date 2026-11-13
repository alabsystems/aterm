// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::image_req_for_test;
use crate::native_settings::SettingsRoute;
use crate::settings_preview::{PreviewAnimation, SettingsPreviewSpec};
use crate::{App, WindowId, control_auth};
use std::time::{Duration, Instant};

const WID: WindowId = WindowId(0);

fn image_at(app: &mut App, elapsed_ms: u64) -> Vec<u8> {
    let now = app.lat_epoch + Duration::from_millis(elapsed_ms);
    crate::app_native::with_native_preview_instant_for_test(now, || {
        let (tx, rx) = std::sync::mpsc::channel();
        let target = control_auth::ConfinedImage::in_memory();
        app.render_image(image_req_for_test(target, false, true, None, tx));
        let (_, _, png) = rx
            .recv_timeout(crate::app_introspect::WORKER_PATIENCE)
            .expect("image worker reply")
            .expect("headless image succeeds")
            .value;
        png.expect("PNG bytes")
    })
}

fn drawn_preview(app: &App) -> SettingsPreviewSpec {
    let (_, view) = app.active_native_view(WID).expect("the Settings view");
    app.windows[&WID].leaf_render_cache[&view]
        .native
        .as_ref()
        .expect("a staged native raster")
        .compiled
        .paint
        .iter()
        .find_map(|paint| match &paint.content {
            crate::native_ui::UiContent::SettingsPreview(spec) => Some((**spec).clone()),
            _ => None,
        })
        .expect("the page paints its cursor preview")
}

#[test]
fn a_headless_capture_samples_a_live_settings_preview_at_its_instant() {
    let mut renderer = aterm_render::Renderer::from_bytes(
        aterm_render::embedded_font(),
        14.0,
        aterm_render::Theme::default(),
    )
    .expect("the embedded face");
    renderer.set_runtime_font_discovery(false);
    crate::tray_raster::install_settled_chrome_fonts_for_test(renderer);
    let mut app = App::headless_for_test();
    // The cursor preview loops while a trail draws, which Windows keeps
    // off until one is chosen.
    app.native_config_service = crate::native_config_service::VersionedConfigService::new(
        crate::app_config::trail_drawn(""),
    )
    .unwrap();
    app.lat_epoch = Instant::now();
    {
        let window = app.windows.get_mut(&WID).unwrap();
        window.focused = true;
        window.rows = 50;
        window.cols = 120;
    }
    crate::app_native::with_native_preview_instant_for_test(app.lat_epoch, || {
        assert!(app.open_settings_tab(SettingsRoute::CursorMotion));
    });

    let early_png = image_at(&mut app, 420);
    let early = drawn_preview(&app);
    assert_eq!(
        early.animation(),
        PreviewAnimation::Continuous,
        "the page's cursor preview loops"
    );
    assert_eq!(
        app.native_settings_preview_for_tick(WID, 700, false),
        None,
        "the park never ticks an unwatched headless preview"
    );
    let late_png = image_at(&mut app, 980);
    let late = drawn_preview(&app);
    assert_eq!((early.phase_ms, late.phase_ms), (420, 980));
    assert_ne!(early_png, late_png, "the preview moved between captures");

    app.windows.get_mut(&WID).unwrap().focused = false;
    let still_png = image_at(&mut app, 1_400);
    let still = drawn_preview(&app);
    assert_ne!(still.animation(), PreviewAnimation::Continuous);
    assert_eq!(
        image_at(&mut app, 2_600),
        still_png,
        "a still preview is the same frame at every capture"
    );
    assert_eq!(drawn_preview(&app).phase_ms, still.phase_ms);
}
