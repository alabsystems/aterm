// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn only_a_headless_launch_that_may_use_a_gpu_defers_the_device() {
    assert!(super::defer_gpu_build(true, true), "headless GPU launch");
    assert!(!super::defer_gpu_build(true, false), "windowed GPU launch");
    assert!(!super::defer_gpu_build(false, true), "headless --cpu");
    assert!(!super::defer_gpu_build(false, false), "windowed --cpu");
}

/// A headless boot's `warm_config` claims no first-frame Metal warm, so its first pixel
/// demand mints without waiting on a warm thread.
#[cfg(target_os = "macos")]
#[test]
fn a_headless_boot_claims_no_first_frame_warm() {
    if !crate::test_alone::alone() {
        return;
    }
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    crate::test_env::scoped("XDG_CONFIG_HOME", dir.path(), || {
        let warm = super::warm_config(
            super::WarmLaunch::Headless,
            crate::app_config::CarriedZoom::default(),
        );
        let _ = warm.backend_handle.join().expect("the backend worker");
    });
    assert!(
        !aterm_gpu::first_frame_warm_began(),
        "a headless boot claimed the first-frame Metal warm"
    );
}

#[test]
fn a_pixel_demand_without_a_deferred_intent_builds_nothing() {
    let mut app = super::App::headless_for_test();
    assert!(!app.backend_kind_undecided(), "no intent at construction");
    for _ in 0..3 {
        app.ensure_pixel_backend();
        assert!(!app.use_gpu, "no intent, so no device may appear");
        assert!(!app.backend.is_gpu());
        assert!(!app.backend_kind_undecided());
    }
}

fn configured_gpu_intent() -> (super::App, aterm_render::DrawnFaces) {
    let mut app = super::App::headless_for_test();
    app.font_family = Some("monospace".to_owned());
    app.font_px = super::FONT_PX + 3.0;
    app.theme = super::Theme {
        bg: 0x0018_2028,
        cursor: 0x0055_CCAA,
        ..super::Theme::default()
    };
    let mut renderer = super::Renderer::from_system_with_family(
        app.font_family.as_deref(),
        app.font_px,
        app.theme,
    )
    .expect("the configured generic monospace font");
    renderer.set_line_height(super::Config::default().line_height_or_default());
    renderer.seal_admitted_font_sources();
    let faces = renderer.drawn_faces().expect("sealed drawn faces").clone();
    assert!(faces.regular.is_some(), "the prepared face is known");
    app.backend = super::BackendSlot::Ready(super::Backend::Cpu(renderer));
    app.set_deferred_gpu(true);
    (app, faces)
}

#[test]
fn a_headless_gpu_redemption_refuses_without_a_window_fallback_or_second_acquisition() {
    let (mut app, faces) = configured_gpu_intent();
    let family = app.font_family.clone();
    let theme = app.theme;
    let mut acquisitions = 0;

    app.ensure_pixel_backend_with(|policy| match policy {
        aterm_gpu::DevicePolicy::Headless => {
            acquisitions += 1;
            Err("no enumerated headless device".to_owned())
        }
        aterm_gpu::DevicePolicy::Windowed => {
            panic!("headless redemption requested a window device")
        }
    });

    assert_eq!(acquisitions, 1, "the actual redemption reached acquisition");
    assert!(!app.backend_kind_undecided(), "the refused intent is spent");
    assert!(!app.use_gpu);
    assert_eq!(app.font_family, family);
    assert_eq!(app.theme, theme);
    let super::BackendSlot::Ready(super::Backend::Cpu(renderer)) = &app.backend else {
        panic!("refusal must retain the original CPU renderer")
    };
    assert!(renderer.admitted_font_sources_sealed());
    assert_eq!(renderer.drawn_faces(), Some(&faces));
    assert_eq!(renderer.theme(), theme);

    app.ensure_pixel_backend_with(|_| panic!("a spent intent acquired another device"));
    assert!(!app.backend.is_gpu());
    assert_eq!(app.backend.drawn_faces(), Some(&faces));
    assert_eq!(app.font_family, family);
}

#[test]
fn a_headless_gpu_redemption_assembles_the_admitted_configured_font() {
    let context = aterm_gpu::GpuContext::new_headless().unwrap_or_else(|error| {
        aterm_spec::host::could_not_run(format!("no permitted headless GPU: {error}"))
    });
    assert_eq!(context.device_policy(), aterm_gpu::DevicePolicy::Headless);
    let (mut app, faces) = configured_gpu_intent();
    let family = app.font_family.clone();
    let theme = app.theme;
    let mut acquisitions = 0;
    app.ensure_pixel_backend_with(|policy| {
        assert_eq!(policy, aterm_gpu::DevicePolicy::Headless);
        acquisitions += 1;
        Ok(context)
    });
    assert_eq!(acquisitions, 1);
    assert!(
        app.backend.is_gpu(),
        "the common redemption assembled the GPU"
    );
    assert!(app.use_gpu);
    assert!(!app.backend_kind_undecided());
    assert!(app.backend.ready().admitted_font_sources_sealed());
    assert_eq!(app.backend.drawn_faces(), Some(&faces));
    assert_eq!(app.font_family, family);
    let prepared = app
        .backend
        .cpu_renderer_from_admitted(app.font_px, app.theme)
        .expect("the installed GPU retains the admitted font sources");
    assert_eq!(prepared.drawn_faces(), Some(&faces));
    assert_eq!(prepared.theme(), theme);
    app.ensure_pixel_backend_with(|_| panic!("a redeemed GPU intent acquired another device"));
}

#[test]
fn the_settings_capability_read_follows_the_intent_the_app_holds() {
    let mut app = super::App::headless_for_test();
    let wid = super::WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorMotion));
    let (_, view) = app.active_native_view(wid).expect("the Settings view");
    let read = |app: &super::App| {
        (
            app.backend_kind_undecided(),
            app.native_view_motion_cx(wid, view).backend_gpu,
        )
    };
    assert_eq!(read(&app), (false, false), "a test App carries no intent");
    app.set_deferred_gpu(true);
    assert_eq!(
        read(&app),
        (true, true),
        "an unredeemed intent reads as GPU-capable in the views too"
    );
    app.set_deferred_gpu(false);
    assert_eq!(
        read(&app),
        (false, false),
        "a spent intent is an honest CPU run again"
    );
}

#[test]
fn a_declined_redemption_restores_the_cpu_only_warnings_boot_withheld() {
    if !crate::message_inbox::lane_zero_alone() {
        return;
    }
    let mut app = super::App::headless_for_test();
    let _ = crate::message_inbox::take_queued();
    app.render_knobs.background_opacity = 0.5;
    app.set_deferred_gpu(true);
    app.pin_backend_render_config_core();
    let withheld = crate::message_inbox::take_queued();
    let undecided = (
        app.backend_kind_undecided(),
        backend_background_opacity(&app),
    );

    app.render_knobs.background_opacity = 0.25;
    app.backend = super::BackendSlot::Ready(super::Backend::Cpu(
        super::Renderer::from_system(super::FONT_PX, app.theme)
            .expect("system font for an unsealed generation"),
    ));
    app.ensure_pixel_backend();
    let settled = (
        app.backend_kind_undecided(),
        app.backend.is_gpu(),
        backend_background_opacity(&app),
    );
    let _ = crate::message_inbox::take_queued();

    assert_eq!(
        undecided,
        (true, 0.5),
        "boot pins the knob onto the live CPU renderer either way"
    );
    assert!(
        withheld.is_empty(),
        "an unredeemed intent withholds the CPU-only warning: {withheld:?}"
    );
    assert_eq!(
        settled,
        (false, false, 0.25),
        "a declined redemption re-pins against a backend that is now, finally, the answer"
    );
}

fn backend_background_opacity(app: &super::App) -> f32 {
    match &app.backend {
        super::BackendSlot::Ready(super::Backend::Cpu(renderer)) => renderer.background_opacity(),
        _ => panic!("the headless test App holds a ready CPU renderer"),
    }
}
