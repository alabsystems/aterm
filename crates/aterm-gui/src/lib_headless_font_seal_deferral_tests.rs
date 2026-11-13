// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn the_first_pixel_demand_seals_a_deferred_headless_generation_once() {
    let mut app = super::App::headless_for_test();
    app.backend = super::BackendSlot::Ready(super::Backend::Cpu(
        super::Renderer::from_system(super::FONT_PX, app.theme)
            .expect("system font for an unsealed generation"),
    ));
    app.deferred_font_seal = true;
    assert!(
        !sealed(&app),
        "precondition: the published generation is raw"
    );

    app.ensure_pixel_backend();
    assert!(
        sealed(&app),
        "the first pixel demand must settle the cascade"
    );
    assert!(!app.deferred_font_seal, "the intent is spent, not standing");

    app.ensure_pixel_backend();
    assert!(sealed(&app));
}

#[test]
fn the_seal_and_its_chrome_resync_are_timed_apart_and_once() {
    let mut app = super::App::headless_for_test();
    app.backend = super::BackendSlot::Ready(super::Backend::Cpu(
        super::Renderer::from_system(super::FONT_PX, app.theme)
            .expect("system font for an unsealed generation"),
    ));
    app.deferred_font_seal = true;
    let epoch = crate::tray_raster::chrome_font_epoch_for_test();
    let (seal_ms, chrome_sync_ms) = app
        .redeem_deferred_font_seal()
        .expect("an owed seal is redeemed, and both halves are timed");
    assert!(seal_ms.is_finite() && seal_ms >= 0.0, "{seal_ms}");
    assert!(
        chrome_sync_ms.is_finite() && chrome_sync_ms >= 0.0,
        "{chrome_sync_ms}"
    );
    assert!(sealed(&app), "the first half sealed the generation");
    assert_ne!(
        crate::tray_raster::chrome_font_epoch_for_test(),
        epoch,
        "the second half re-synced the chrome faces inside the redemption"
    );
    assert_eq!(
        app.redeem_deferred_font_seal(),
        None,
        "a spent debt times nothing"
    );
}

/// A HEADLESS RUN'S FIRST NATIVE VIEW LAYS OUT IN THE FACES ITS CAPTURE PAINTS WITH: with
/// the font seal deferred, opening Settings redeems it and installs the chrome faces before
/// the page is laid out, so the layout `inspect` reads before any capture is the one read
/// after the capture's pixel demand. NEGATIVE CONTROL: with the faces left to the first
/// capture (main before this), the page first laid out in the fallback face's metrics and
/// the capture re-laid it out (`All Settings` required 93.6 then 64.2; here the layout
/// check fails first, the page's 35 painted nodes 29 after the capture's faces landed).
#[test]
fn a_headless_runs_first_native_view_lays_out_in_its_painted_faces() {
    use crate::native_settings::SettingsRoute;
    let mut app = super::App::headless_for_test();
    app.backend = super::BackendSlot::Ready(super::Backend::Cpu(
        super::Renderer::from_system(super::FONT_PX, app.theme)
            .expect("system font for an unsealed generation"),
    ));
    app.deferred_font_seal = true;
    let wid = crate::WindowId(0);
    assert!(app.open_settings_tab_in_window(wid, SettingsRoute::Home));
    let redeemed = !app.deferred_font_seal && sealed(&app);
    let (_, view) = app.active_native_view(wid).expect("Settings in front");
    // The layout: every audit line but its raster identities, which name the faces' install.
    let audit = |app: &super::App| {
        app.inspect_app(crate::app_control::InspectRequest::View {
            view,
            projection: aterm_types::app_inspection::InspectionProjection::Audit,
        })
        .expect("the page inspects")
        .into_iter()
        .skip(2)
        .map(|line| {
            line.split(' ')
                .filter(|word| !word.contains("fingerprint="))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
    };
    let before = audit(&app);
    app.ensure_pixel_backend();
    app.sync_chrome_fonts();
    assert_eq!(
        audit(&app),
        before,
        "the layout read before the first capture is the painted one"
    );
    assert!(
        redeemed,
        "the first native view redeemed the deferred faces"
    );
}

#[test]
fn a_pixel_demand_with_no_seal_owed_settles_nothing() {
    let mut app = super::App::headless_for_test();
    app.backend = super::BackendSlot::Ready(super::Backend::Cpu(
        super::Renderer::from_system(super::FONT_PX, app.theme)
            .expect("system font for an unsealed generation"),
    ));
    assert!(!app.deferred_font_seal, "a test App owes no seal");
    app.ensure_pixel_backend();
    assert!(
        !sealed(&app),
        "nothing may seal a generation nobody deferred"
    );
}

#[test]
fn a_rebuild_from_admitted_redeems_the_owed_seal_instead_of_failing_soft() {
    let mut app = super::App::headless_for_test();
    app.backend = super::BackendSlot::Ready(super::Backend::Cpu(
        super::Renderer::from_system(super::FONT_PX, app.theme)
            .expect("system font for an unsealed generation"),
    ));
    app.deferred_font_seal = true;
    assert!(
        app.rebuild_backend(),
        "the rebuild must settle the generation it needs, not decline it"
    );
    assert!(sealed(&app), "and the redemption is what settled it");
    assert!(!app.deferred_font_seal);
}

#[test]
fn a_rebuild_from_an_unsealed_generation_with_no_debt_owed_declines() {
    let mut app = super::App::headless_for_test();
    app.backend = super::BackendSlot::Ready(super::Backend::Cpu(
        super::Renderer::from_system(super::FONT_PX, app.theme)
            .expect("system font for an unsealed generation"),
    ));
    assert!(!app.deferred_font_seal);
    assert!(!app.rebuild_backend(), "nothing owed, nothing sealed");
}

fn sealed(app: &super::App) -> bool {
    match &app.backend {
        super::BackendSlot::Ready(backend) => backend.admitted_font_sources_sealed(),
        super::BackendSlot::Pending(_) => panic!("the test App holds a ready backend"),
    }
}
