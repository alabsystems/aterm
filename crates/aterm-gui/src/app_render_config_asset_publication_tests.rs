// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use crate::{App, WindowId};

fn catalog(
    kitty_sprite: crate::app_config::KittySpriteAsset,
    themes: Arc<crate::app_config::ThemeCatalog>,
) -> Arc<crate::app_config::ConfigAssetCatalog> {
    Arc::new(crate::app_config::ConfigAssetCatalog {
        trail_style: crate::app_config::TrailStyleCache::empty(),
        kitty_sprite,
        wallpaper: crate::app_config::WallpaperAsset::None,
        themes,
        sparkle_spec_consumers: Default::default(),
    })
}

fn assert_exact_catalog_on_every_window(
    app: &App,
    expected: &Arc<crate::app_config::ConfigAssetCatalog>,
) {
    assert!(Arc::ptr_eq(&app.config_assets, expected));
    assert!(app.windows.values().all(|window| {
        window
            .installed_config_assets
            .as_ref()
            .is_some_and(|installed| Arc::ptr_eq(installed, expected))
    }));
}

#[test]
fn authoritative_publication_installs_ready_invalid_and_builtin_before_effect_paths() {
    let mut app = App::headless_for_test();
    let sid = app.next_session_id;
    let _second = app.insert_logical_window(crate::stub_session(sid), 24, 80);

    let rgba: Arc<[u8]> = Arc::from([0x12, 0x34, 0x56, 0xff]);
    let ready = catalog(
        crate::app_config::KittySpriteAsset::Ready {
            source_id: Arc::from("same.png"),
            w: 1,
            h: 1,
            rgba: Arc::clone(&rgba),
            fp: 11,
        },
        crate::app_config::ThemeCatalog::empty(),
    );
    assert_eq!(app.publish_config_assets(Arc::clone(&ready)), 2);
    assert_exact_catalog_on_every_window(&app, &ready);
    assert!(
        app.windows
            .values()
            .all(|window| { window.fx.decos().kitty_sprite_source_fingerprint() == Some(11) })
    );
    assert!(!app.install_window_config_assets(WindowId(0)));

    let themes = crate::app_config::ThemeCatalog::from_schemes([(
        "Publication Test".to_string(),
        aterm_types::scheme::builtin("Dracula").expect("builtin theme"),
    )]);
    let theme_only = catalog(ready.kitty_sprite.clone(), themes);
    assert_eq!(app.publish_config_assets(Arc::clone(&theme_only)), 2);
    assert_exact_catalog_on_every_window(&app, &theme_only);

    let invalid = catalog(
        crate::app_config::KittySpriteAsset::Invalid {
            source_id: Arc::from("same.png"),
            bounded_reason: Arc::from("replacement is invalid"),
        },
        crate::app_config::ThemeCatalog::empty(),
    );
    assert_eq!(app.publish_config_assets(Arc::clone(&invalid)), 2);
    assert_exact_catalog_on_every_window(&app, &invalid);
    assert!(app.windows.values().all(|window| {
        window
            .fx
            .decos()
            .kitty_sprite_source_fingerprint()
            .is_none()
    }));

    let builtin = catalog(
        crate::app_config::KittySpriteAsset::BuiltIn,
        crate::app_config::ThemeCatalog::empty(),
    );
    assert_eq!(app.publish_config_assets(Arc::clone(&builtin)), 2);
    assert_exact_catalog_on_every_window(&app, &builtin);
    assert!(
        app.windows
            .values()
            .all(|window| { window.fx.decos().kitty_sprite_source_fingerprint() == Some(0) })
    );
    assert_eq!(
        app.publish_config_assets(Arc::clone(&builtin)),
        0,
        "re-publishing the identical outer Arc is a complete no-op"
    );
}
