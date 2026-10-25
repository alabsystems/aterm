// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The wallpaper in Settings: Modified's row names the image as the
//! Wallpaper page does.

use super::*;

/// A Settings state with `path` as the wallpaper, decoded as `asset`.
fn wallpaper_state(path: &str, asset: crate::app_config::WallpaperAsset) -> SettingsViewState {
    let mut state = SettingsViewState::new(&Config::default());
    state
        .raw_values
        .insert(prefs::EDIT_WALLPAPER.to_string(), path.to_string());
    state.assets = std::sync::Arc::new(crate::app_config::ConfigAssetCatalog {
        wallpaper: asset,
        ..crate::app_config::ConfigAssetCatalog::default()
    });
    state
}

/// The words the button keyed `key` paints.
fn button_words(compiled: &crate::native_ui::CompiledUi, key: &str) -> String {
    compiled
        .paint
        .iter()
        .find_map(|node| match &node.content {
            UiContent::Button(control) if node.key.as_str() == key => Some(
                control
                    .spec
                    .visual_label
                    .clone()
                    .unwrap_or_else(|| control.spec.label.clone()),
            ),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{key} paints a button"))
}

/// A loaded `w`×`h` image of one colour.
fn ready(path: &str, w: u32, h: u32) -> crate::app_config::WallpaperAsset {
    crate::app_config::WallpaperAsset::Ready {
        source_id: path.into(),
        w,
        h,
        rgba: std::sync::Arc::from(vec![0x80_u8; (w * h * 4) as usize]),
        fp: 7,
    }
}

/// Modified names the wallpaper by its file name and dimensions across window widths and text scales.
#[test]
fn modified_names_the_wallpaper_by_its_file() {
    at_each_text_scale(&[1.0, 1.25, 1.5, 2.0], "wallpaper words", |scale| {
        wallpaper_words_at_scale(scale);
    });
}

fn wallpaper_words_at_scale(scale: f32) {
    let path = "/Users//someone/Library/Mobile Documents/com~apple~CloudDocs/Pictures/Backdrops/2026/sunset-over-the-bay.png";
    let key = "settings/control/wallpaper";
    for (width, height) in [
        (178.0_f32, 568.0_f32),
        (234.0, 568.0),
        (320.0, 568.0),
        (744.0, 420.0),
        (1_200.0, 820.0),
    ] {
        let mut state = wallpaper_state(path, ready(path, 1074, 686));
        state.navigate(SettingsRoute::Modified);
        state.reveal_control(UiKey::new(key));
        let compiled = settings_route_at(&state, &view_cx_at(width, height));
        let at = format!("{scale}\u{d7} {width}×{height}");
        let node = compiled
            .semantic(&UiKey::new(key))
            .unwrap_or_else(|| panic!("{at}: the row"));
        assert_eq!(node.role, SemanticRole::Button, "{at}");
        assert_eq!(
            node.action.as_ref().map(ActionId::as_str),
            Some("settings/route/wallpaper"),
            "{at}"
        );
        assert_eq!(node.value, SemanticValue::Text(path.to_string()), "{at}");
        assert_eq!(
            node.label,
            format!(
                "sunset-over-the-bay.png \u{b7} 1074 \u{d7} 686: Wallpaper image {path}, \
                 1074 by 686 pixels; opens Wallpaper"
            ),
            "{at}"
        );
        let painted = button_words(&compiled, key);
        let letters = |text: &str| text.replace(['\n', '-'], "");
        assert!(
            letters(&painted).contains(&letters("sunset-over-the-bay.png")),
            "{at}: the file by its whole name: {painted:?}"
        );
        assert!(
            !painted.contains('\u{2026}'),
            "{at}: never cut: {painted:?}"
        );
        if width >= 744.0 && scale <= 1.0 {
            assert_eq!(painted, "sunset-over-the-bay.png · 1074 × 686", "{at}");
        }
        let cut: Vec<_> = compiled
            .paint_audit_lines()
            .into_iter()
            .filter(|line| {
                line.contains(key)
                    && (line.contains("overflow=true") || line.contains("clip-truncated=true"))
            })
            .collect();
        assert!(cut.is_empty(), "{at}: {cut:?}");
    }
    let mut state = wallpaper_state(
        path,
        crate::app_config::WallpaperAsset::Invalid {
            source_id: path.into(),
            bounded_reason: "not a decodable image".into(),
        },
    );
    state.navigate(SettingsRoute::Modified);
    state.reveal_control(UiKey::new(key));
    let compiled = settings_route_at(&state, &view_cx_at(1_200.0, 820.0));
    assert_eq!(button_words(&compiled, key), "sunset-over-the-bay.png");
}

/// The Wallpaper preview node's spec in `compiled`, if it is drawn.
fn wallpaper_preview_of(
    compiled: &crate::native_ui::CompiledUi,
) -> Option<crate::settings_preview::SettingsPreviewSpec> {
    compiled.paint.iter().find_map(|node| match &node.content {
        UiContent::SettingsPreview(spec) if node.key.as_str() == "settings/preview/wallpaper" => {
            Some((**spec).clone())
        }
        _ => None,
    })
}

/// The Wallpaper page and Appearance color sample draw over a loaded wallpaper image.
#[test]
fn the_wallpaper_page_shows_the_picture() {
    let path = "/tmp/wall.png";
    for (width, height) in [
        (584.0_f32, 348.0_f32),
        (738.0, 420.0),
        (1_074.0, 684.0),
        (1_200.0, 820.0),
    ] {
        let at = format!("{width}×{height}");
        let mut state = wallpaper_state(path, ready(path, 1074, 686));
        state.navigate(SettingsRoute::Wallpaper);
        let compiled = settings_route_at(&state, &view_cx_at(width, height));
        assert_eq!(
            compiled
                .semantic(&UiKey::new("settings/wallpaper/current"))
                .map(|status| status.label.as_str()),
            Some("Wallpaper image \u{b7} Changed: /tmp/wall.png, 1074 by 686 pixels"),
            "{at}: the status says the image once"
        );
        let spec = wallpaper_preview_of(&compiled).unwrap_or_else(|| panic!("{at}: the preview"));
        assert_eq!(
            spec.scene,
            crate::settings_preview::PreviewScene::Wallpaper,
            "{at}"
        );
        let wallpaper = spec.wallpaper.as_ref().expect("the picture");
        assert_eq!(
            wallpaper.identity(),
            (7, 1074, 686, 0.3_f32.to_bits(), true),
            "{at}"
        );
        let preview = compiled
            .paint
            .iter()
            .find(|node| node.key.as_str() == "settings/preview/wallpaper")
            .map(|node| node.rect)
            .expect("painted");
        let list = compiled
            .paint
            .iter()
            .find(|node| node.key.as_str() == "settings/wallpaper/list")
            .map(|node| node.rect)
            .expect("the page's list");
        assert!(
            preview.height <= list.height + 0.5,
            "{at}: {preview:?} in {list:?}"
        );
    }
    let mut state = wallpaper_state(path, ready(path, 1074, 686));
    state.navigate(SettingsRoute::Wallpaper);
    state.field_inputs.insert(
        prefs::EDIT_WALLPAPER_TEXT_TINT.to_string(),
        crate::native_text_input::TextInputState::new("false".to_string()),
    );
    state.field_inputs.insert(
        prefs::EDIT_WALLPAPER_DIM.to_string(),
        crate::native_text_input::TextInputState::new("0.6".to_string()),
    );
    let spec = wallpaper_preview_of(&settings_route_at(&state, &view_cx_at(1_200.0, 820.0)))
        .expect("the preview");
    let wallpaper = spec.wallpaper.expect("the picture");
    assert_eq!((wallpaper.dim, wallpaper.tint), (0.6, false));
    let mut state = wallpaper_state(path, ready(path, 1074, 686));
    state.navigate(SettingsRoute::Appearance);
    let appearance = renderer_preview_spec_for_key_with_font(
        &state,
        prefs::EDIT_THEME,
        0,
        crate::native_app::ViewMotionCx::default(),
        14.0,
        1.0,
        None,
    )
    .expect("the color sample");
    assert_eq!(
        appearance.wallpaper.map(|wallpaper| wallpaper.identity().0),
        Some(7)
    );
    for asset in [
        crate::app_config::WallpaperAsset::None,
        crate::app_config::WallpaperAsset::Invalid {
            source_id: path.into(),
            bounded_reason: "not a decodable image".into(),
        },
    ] {
        let mut state = wallpaper_state(path, asset);
        state.navigate(SettingsRoute::Wallpaper);
        assert!(
            wallpaper_preview_of(&settings_route_at(&state, &view_cx_at(1_200.0, 820.0))).is_none()
        );
    }
}
