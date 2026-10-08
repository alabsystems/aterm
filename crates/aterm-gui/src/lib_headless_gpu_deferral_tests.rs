// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// The deferral needs both halves: a windowed GPU launch still builds at boot,
/// and a headless `--cpu` launch has nothing to defer (so `ensure_pixel_backend`
/// never resurrects a GPU the user refused).
#[test]
fn only_a_headless_launch_that_may_use_a_gpu_defers_the_device() {
    assert!(super::defer_gpu_build(true, true), "headless GPU launch");
    assert!(!super::defer_gpu_build(true, false), "windowed GPU launch");
    assert!(!super::defer_gpu_build(false, true), "headless --cpu");
    assert!(!super::defer_gpu_build(false, false), "windowed --cpu");
}

/// The redemption's log line says what each leg cost and how the device
/// question settled, for every shape the redemption can take.
#[test]
fn the_redemption_line_names_every_leg_and_the_device_verdict() {
    use super::{PixelBackendRedemptionLegs, pixel_backend_redemption_line};
    // Illustrative legs, not a measurement: here the device leg's time is in
    // the font thread, which is what the split exists to show.
    let built = PixelBackendRedemptionLegs {
        seal_ms: Some(91.4),
        chrome_sync_ms: Some(40.0),
        fork_ms: Some(12.0),
        device: Some((9012.0, Ok("Intel(R) HD Graphics 630 (Metal)".into()))),
        font_thread_ms: Some(8990.4),
        font_join_ms: Some(8980.0),
        install_ms: Some(8.2),
    };
    assert_eq!(
        pixel_backend_redemption_line(&built, 9163.6),
        "headless pixel backend redeemed on the main thread in 9164 ms: font seal 91 ms, \
         chrome re-sync 40 ms, face fork 12 ms, GPU device 9012 ms (font thread 8990 ms, \
         join wait 8980 ms; \
         Intel(R) HD Graphics 630 (Metal)), install 8 ms — the first pixel demand of a \
         headless run pays this once"
    );
    // A build that was not the process's first leaves the split unread:
    // the device clause is the adapter alone.
    let unsplit = PixelBackendRedemptionLegs {
        font_thread_ms: None,
        font_join_ms: None,
        ..built.clone()
    };
    let line = pixel_backend_redemption_line(&unsplit, 9163.6);
    assert!(
        line.contains("GPU device 9012 ms (Intel(R) HD Graphics 630 (Metal)), install"),
        "{line}"
    );
    // The context failed before the join: the font thread ran (and may
    // have landed its slot) but was never joined, so only its half prints.
    let unavailable = PixelBackendRedemptionLegs {
        seal_ms: Some(90.0),
        chrome_sync_ms: Some(30.0),
        fork_ms: Some(11.0),
        device: Some((5.0, Err("no Metal device on this machine".into()))),
        font_thread_ms: Some(4.0),
        font_join_ms: None,
        install_ms: None,
    };
    let line = pixel_backend_redemption_line(&unavailable, 136.0);
    assert!(
        line.contains(
            "GPU device 5 ms (font thread 4 ms; unavailable: no Metal device on this \
             machine; the CPU renderer keeps serving)"
        ),
        "{line}"
    );
    assert!(!line.contains("install"), "{line}");
    let fork_failed = PixelBackendRedemptionLegs {
        seal_ms: None,
        chrome_sync_ms: None,
        fork_ms: Some(3.0),
        device: None,
        font_thread_ms: None,
        font_join_ms: None,
        install_ms: None,
    };
    let line = pixel_backend_redemption_line(&fork_failed, 3.0);
    assert!(
        line.contains("no device built (the face fork failed"),
        "{line}"
    );
    assert!(
        !line.contains("font seal") && !line.contains("chrome re-sync"),
        "no seal owed, neither half printed: {line}"
    );
    // A `--cpu` headless run: the seal and the chrome re-sync after it are
    // two legs, never one `font seal` figure that hides the re-sync.
    let cpu_only = PixelBackendRedemptionLegs {
        seal_ms: Some(80.0),
        chrome_sync_ms: Some(38.0),
        ..PixelBackendRedemptionLegs::default()
    };
    let line = pixel_backend_redemption_line(&cpu_only, 118.0);
    assert!(
        line.contains("font seal 80 ms, chrome re-sync 38 ms, no GPU intent"),
        "{line}"
    );
}

/// A pixel demand with no deferred intent constructs nothing: a CPU run stays a
/// CPU run however many captures it serves, and the repeat shows the no-op comes
/// from the absent intent, not a once-only latch.
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

/// The capability read follows the intent, not the live renderer: an unredeemed
/// intent reads as GPU-capable (`ViewMotionCx::backend_gpu`) and a spent one as
/// CPU again, the same answer `backend_kind_undecided` gives.
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

/// A failed pixel-backend redemption stays on CPU, so the render config is
/// re-pinned and the "no CPU consumer" warn-onces stop being suppressed. The
/// failure is simulated with an unsealed font generation, which
/// `cpu_renderer_from_admitted` refuses without touching a device.
#[test]
fn a_declined_redemption_restores_the_cpu_only_warnings_boot_withheld() {
    let _lane = crate::message_inbox::lane_test_guard();
    let mut app = super::App::headless_for_test();
    let _ = crate::message_inbox::take_queued();
    // A launch that asked for translucent glass, on a boot that has not yet
    // decided whether it has a GPU to composite it with.
    app.render_knobs.background_opacity = 0.5;
    app.set_deferred_gpu(true);
    app.pin_backend_render_config_core();
    let withheld = crate::message_inbox::take_queued();
    let undecided = (
        app.backend_kind_undecided(),
        backend_background_opacity(&app),
    );

    // Every pin write is idempotent, so move the knob without pinning it: only
    // a second pin can carry it to the renderer.
    app.render_knobs.background_opacity = 0.25;
    // A generation the redemption cannot fork => the first `Err` arm.
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
    // Whatever the process-wide `Once` did with it, this test does not leave
    // a message behind for the inbox-drain tests to find.
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

/// The background opacity the CPU renderer is carrying — separates "the failure
/// path re-pinned" from "the failure path returned".
fn backend_background_opacity(app: &super::App) -> f32 {
    match &app.backend {
        super::BackendSlot::Ready(super::Backend::Cpu(renderer)) => renderer.background_opacity(),
        _ => panic!("the headless test App holds a ready CPU renderer"),
    }
}
