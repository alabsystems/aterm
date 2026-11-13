// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;

use super::*;
use crate::native_ui::{GroupSpec, SemanticRole, UiTree};

/// The companions a preview frame draws: its sprite layers, read back.
fn sprites(input: &RenderInput) -> Vec<aterm_render::sprites::FreeSprite> {
    aterm_render::sprites::published(&input.fx_layers)
}

const VIEWPORT: LogicalRect = LogicalRect::new(0.0, 0.0, 480.0, 210.0);
const NORD: PreviewTerminalTheme = PreviewTerminalTheme::new(
    0xD8_DEE9,
    0x2E_3440,
    0x88_C0D0,
    0x43_4C5E,
    [
        0x3B4252, 0xBF616A, 0xA3BE8C, 0xEBCB8B, 0x81A1C1, 0xB48EAD, 0x88C0D0, 0xE5E9F0, 0x596377,
        0xBF616A, 0xA3BE8C, 0xEBCB8B, 0x81A1C1, 0xB48EAD, 0x8FBCBB, 0xECEFF4,
    ],
);

fn played(spec: &SettingsPreviewSpec, theme: Theme) -> RenderInput {
    let spec = spec.normalized();
    let (base, _) = build_terminal_specimen_input(&spec, theme);
    CursorScene::new(&spec, theme, None).decorate(&base, (8, 17))
}

fn has_effect_light(input: &RenderInput) -> bool {
    !input.cursor_glow_add.is_empty()
        || !input.glow_under.is_empty()
        || !input.glow_halo.is_empty()
        || !input.fire_patch.is_empty()
        || !input.cursor_trail.is_empty()
}

fn compiled(spec: SettingsPreviewSpec) -> crate::native_ui::CompiledUi {
    compiled_with_font(spec, |_| {})
}

fn compiled_with_font(
    mut spec: SettingsPreviewSpec,
    perturb: impl FnOnce(&mut crate::tray_raster::PreparedSemanticFont),
) -> crate::native_ui::CompiledUi {
    let _ = crate::native_appearance::install_preferences(
        crate::native_appearance::AppearancePreferences::default(),
    );
    thread_local! {
        static INSTALLED_EPOCH: Cell<Option<u64>> = const { Cell::new(None) };
    }
    INSTALLED_EPOCH.with(|installed| {
        let current = crate::tray_raster::chrome_font_epoch_for_test();
        if installed.get() != Some(current) {
            let mut renderer = aterm_render::Renderer::from_bytes(
                aterm_render::embedded_font(),
                14.0,
                Theme::default(),
            )
            .expect("embedded semantic renderer");
            renderer.set_runtime_font_discovery(false);
            renderer.prepare_semantic_typography("Bold Italic != => 你好 ∑✓♥ 😀 🚀 👩‍💻");
            let epoch = crate::tray_raster::install_settled_chrome_fonts_for_test(renderer);
            installed.set(Some(epoch));
        }
    });
    let mut prepared =
        crate::tray_raster::prepared_semantic_font_for_direct_view_test(&spec.font_candidate);
    perturb(&mut prepared);
    spec = spec.with_prepared_font(prepared);
    let preview = preview_node("preview", spec, 190.0);
    UiTree::new(
        UiNode::new(
            "root",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
        )
        .layout(Layout::column().padding(Insets::all(10.0)))
        .children(vec![preview]),
    )
    .compile(VIEWPORT)
    .expect("preview compiles")
}

#[test]
fn the_demo_shadow_ring_paints_what_the_full_shadow_shows() {
    let rect = LogicalRect::new(40.0, 30.0, 300.0, 180.0);
    for radius in [DemoFrame::Dots.radius(), DemoFrame::Windows.radius()] {
        let body = DrawPrim::Panel {
            x: rect.x,
            y: rect.y,
            w: rect.width,
            h: rect.height,
            radius,
            fill: rgba([30, 34, 40], 255),
        };
        let raster = |ring: bool, light: bool, with_body: bool, scale: f32| {
            let mut prims = Vec::new();
            push_demo_shadow(&mut prims, rect, light, ring, radius);
            if with_body {
                prims.push(body.clone());
            }
            crate::tray_raster::rasterize_tray(&prims, 400, 260, scale, [236, 236, 236, 255]).0
        };
        for light in [true, false] {
            for scale in [1.0, 2.0] {
                assert!(
                    raster(true, light, true, scale) == raster(false, light, true, scale),
                    "radius {radius}, light {light} at {scale}x"
                );
            }
        }
        assert!(raster(true, true, false, 1.0) != raster(false, true, false, 1.0));
    }
}

fn pixels(spec: SettingsPreviewSpec) -> Vec<u8> {
    raster_of(&compiled(spec).tray(Theme::default(), 13.0).prims)
}

fn solid_wallpaper(rgb: [u8; 3], dim: f32, tint: bool) -> crate::widget::SpecimenWallpaper {
    crate::widget::SpecimenWallpaper {
        rgba: Arc::from(
            std::iter::repeat_n([rgb[0], rgb[1], rgb[2], 255], 8)
                .flatten()
                .collect::<Vec<u8>>(),
        ),
        w: 4,
        h: 2,
        fp: u64::from(u32::from_be_bytes([0, rgb[0], rgb[1], rgb[2]])) ^ 0x5157,
        dim,
        tint,
    }
}

fn wallpaper_spec(wallpaper: Option<crate::widget::SpecimenWallpaper>) -> SettingsPreviewSpec {
    SettingsPreviewSpec {
        scene: PreviewScene::Wallpaper,
        wallpaper,
        terminal_theme: Some(PreviewTerminalTheme::from_scheme(
            &aterm_types::ColorScheme::default(),
        )),
        focused_key: crate::prefs::EDIT_WALLPAPER.to_string(),
        focused_value: "wall.png".to_string(),
        ..SettingsPreviewSpec::appearance(14.0)
    }
}

fn texel(raster: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = (y * VIEWPORT.width as usize + x) * 4;
    [raster[i], raster[i + 1], raster[i + 2]]
}

#[test]
fn the_wallpaper_preview_draws_the_picture_under_the_terminal() {
    let bg = crate::settings::u32_rgb(Theme::default().bg);
    let red = [0xE0, 0x20, 0x20];
    let (x, y) = (
        (VIEWPORT.width - 10.0 - 10.0 - 6.0) as usize,
        (10.0 + 190.0 - 10.0 - 6.0) as usize,
    );
    let at = |wallpaper| texel(&pixels(wallpaper_spec(wallpaper)), x, y);
    assert_eq!(at(None), bg, "no wallpaper: the theme background");
    assert_eq!(at(Some(solid_wallpaper(red, 0.0, false))), red, "dim 0");
    assert_eq!(at(Some(solid_wallpaper(red, 1.0, false))), bg, "dim 1");
    let mixed = at(Some(solid_wallpaper(red, 0.3, false)));
    for channel in 0..3 {
        let want = f32::from(red[channel]) * 0.7 + f32::from(bg[channel]) * 0.3;
        assert!(
            (f32::from(mixed[channel]) - want).abs() <= 2.0,
            "dim 0.3: {mixed:?} against {want} on channel {channel}"
        );
    }
    let tinted = |tint| pixels(wallpaper_spec(Some(solid_wallpaper(red, 0.0, tint))));
    assert_ne!(tinted(true), tinted(false), "the tint moves the text's ink");
    assert_ne!(
        wallpaper_spec(Some(solid_wallpaper(red, 0.3, false))).paint_fingerprint(),
        wallpaper_spec(Some(solid_wallpaper(red, 0.4, false))).paint_fingerprint()
    );
}

fn raster_of(prims: &[DrawPrim]) -> Vec<u8> {
    crate::tray_raster::rasterize_tray(
        prims,
        VIEWPORT.width as u32,
        VIEWPORT.height as u32,
        1.0,
        [0, 0, 0, 0],
    )
    .0
}

fn pixels_recomputed(spec: SettingsPreviewSpec) -> Vec<u8> {
    let parking_spec = if spec.scene == PreviewScene::Appearance {
        SettingsPreviewSpec::cursor(CursorPreviewSpec::default())
    } else {
        SettingsPreviewSpec::appearance(14.0)
    };
    let (_, parked) = build_terminal_specimen_input(&parking_spec, Theme::default());

    thread_local! {
        static RECOMPUTE_NONCE: Cell<u64> = const { Cell::new(0) };
    }
    let prims = compiled_with_font(spec, |prepared| {
        let nonce = RECOMPUTE_NONCE.with(|slot| {
            let next = slot.get().wrapping_add(1);
            slot.set(next);
            next
        });
        prepared.snapshot.ready_epoch = prepared.snapshot.ready_epoch.wrapping_add(nonce);
    })
    .tray(Theme::default(), 13.0)
    .prims;

    let fingerprint = prims.iter().find_map(|primitive| match primitive {
        DrawPrim::TerminalSpecimen { spec, .. } => Some(spec.input_fingerprint),
        _ => None,
    });
    assert!(
        fingerprint.is_some_and(|resolved| resolved != parked),
        "the scene must paint a terminal specimen, and this paint must rebuild \
         its own engine snapshot instead of inheriting the parked one"
    );
    raster_of(&prims)
}

fn differences(left: &[u8], right: &[u8]) -> usize {
    differences_up_to(left, right, usize::MAX)
}

fn differences_up_to(left: &[u8], right: &[u8], cap: usize) -> usize {
    const BLOCK: usize = 256;
    let common = left.len().min(right.len());
    let mut counted = 0usize;
    for (a, b) in left[..common]
        .chunks(BLOCK)
        .zip(right[..common].chunks(BLOCK))
    {
        if a != b {
            counted += a.iter().zip(b).filter(|(x, y)| x != y).count();
            if counted >= cap {
                return cap;
            }
        }
    }
    counted
}

fn panel_fill(prims: &[DrawPrim], wanted_radius: f32) -> [u8; 4] {
    prims
        .iter()
        .find_map(|primitive| match primitive {
            DrawPrim::Panel { radius, fill, .. }
                if (*radius - wanted_radius).abs() < f32::EPSILON =>
            {
                Some(*fill)
            }
            _ => None,
        })
        .expect("preview panel exists")
}

fn test_relative_luminance(color: [u8; 3]) -> f32 {
    let linear = |channel: u8| {
        let value = f32::from(channel) / 255.0;
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color[0]) + 0.7152 * linear(color[1]) + 0.0722 * linear(color[2])
}

fn test_contrast_ratio(left: [u8; 3], right: [u8; 3]) -> f32 {
    let left = test_relative_luminance(left);
    let right = test_relative_luminance(right);
    (left.max(right) + 0.05) / (left.min(right) + 0.05)
}

#[test]
fn phone_preview_header_keeps_scene_and_live_state_disjoint() {
    let roles = Roles::from_theme(Theme::default());
    let rect = LogicalRect::new(16.0, 150.0, 254.5, 120.0);
    for spec in [
        SettingsPreviewSpec::appearance(24.0),
        SettingsPreviewSpec::typography(18.0),
        SettingsPreviewSpec::default().with_phase(780),
    ] {
        let mut prims = Vec::new();
        spec.paint(&mut prims, rect, Theme::default(), roles);
        let headers = prims
            .iter()
            .filter_map(|primitive| match primitive {
                DrawPrim::Text { x, s, px, face, .. } if *face == TextFace::UiBold => {
                    Some((*x, s.as_str(), *px))
                }
                _ => None,
            })
            .take(2)
            .collect::<Vec<_>>();
        assert!(
            !headers.is_empty(),
            "phone preview retains a candidate glance label"
        );
        let (right_x, right, right_px) = *headers.last().unwrap();
        assert!(
            !right.contains('='),
            "header exposes a raw config assignment: {right}"
        );
        assert!(
            !right.contains('_'),
            "header exposes a raw config key: {right}"
        );
        if headers.len() == 2 {
            let (left_x, left, left_px) = headers[0];
            let left_right =
                left_x + crate::tray_raster::ui_text_width_for(TextFace::UiBold, left, left_px);
            assert!(
                left_right + 7.99 <= right_x,
                "preview header overlaps: {left:?} ends at {left_right:.1}, {right:?} starts at {right_x:.1}"
            );
        }
        assert!(
            right_x + crate::tray_raster::ui_text_width_for(TextFace::UiBold, right, right_px)
                <= rect.right() - 10.0 + 0.01,
            "preview state stays inside its right inset"
        );
    }
}

#[test]
fn the_header_state_never_runs_under_the_pause_control() {
    use crate::native_appearance::{AppearancePreferences, install_preferences};
    let roles = Roles::from_theme(Theme::default());
    let specs = [
        SettingsPreviewSpec::default().with_phase(300),
        SettingsPreviewSpec::default()
            .with_phase(300)
            .with_paused(true),
        SettingsPreviewSpec::default()
            .with_phase(900)
            .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "sparkle")
            .with_style_trial(Some(StyleTrial::Keyboard)),
    ];
    let mut checked = 0;
    for scale in [1.0_f32, 2.0] {
        let _ = install_preferences(AppearancePreferences {
            text_scale: scale,
            ..AppearancePreferences::default()
        });
        for width in [254.5_f32, 420.0, 720.0] {
            let rect = LogicalRect::new(16.0, 40.0, width, 220.0);
            for spec in &specs {
                assert!(spec.pause_control(), "fixture: a looping preview");
                let header_bottom = spec.terminal_rect(rect).y;
                let mut prims = Vec::new();
                spec.paint(&mut prims, rect, Theme::default(), roles);
                let slot = rect.right() - PREVIEW_INSET - PAUSE_GLYPH_W;
                let glyph_left = prims
                    .iter()
                    .filter_map(|prim| match prim {
                        DrawPrim::Line {
                            x1,
                            x2,
                            y1,
                            y2,
                            width,
                            ..
                        } if y1.max(*y2) <= header_bottom && x1.min(*x2) >= slot - 0.01 => {
                            Some(x1.min(*x2) - width * 0.5)
                        }
                        _ => None,
                    })
                    .fold(f32::MAX, f32::min);
                assert!(glyph_left < f32::MAX, "the preview draws its control");
                let (x, state, px) = prims
                    .iter()
                    .filter_map(|prim| match prim {
                        DrawPrim::Text {
                            x,
                            baseline,
                            s,
                            px,
                            face,
                            ..
                        } if *face == TextFace::UiBold && *baseline <= header_bottom => {
                            Some((*x, s.clone(), *px))
                        }
                        _ => None,
                    })
                    .next_back()
                    .expect("the header says its state");
                let state_width =
                    crate::tray_raster::ui_text_width_for(TextFace::UiBold, &state, px);
                assert!(
                    x + state_width + 2.0 <= glyph_left,
                    "{scale}x, {width}pt: {state:?} ends at {:.1}, the control starts at \
                     {glyph_left:.1}",
                    x + state_width
                );
                let unreserved = header_state_x(rect, PREVIEW_INSET, 0.0, state_width);
                assert!(
                    unreserved + state_width > glyph_left,
                    "control: {state:?} with no room kept for the control must run under it"
                );
                checked += 1;
            }
        }
    }
    let _ = install_preferences(AppearancePreferences::default());
    assert_eq!(checked, 18);
}

#[test]
fn a_preview_header_paints_every_word_it_chooses_at_every_width() {
    use crate::native_appearance::{AppearancePreferences, install_preferences};
    let _ = compiled(SettingsPreviewSpec::default());
    let roles = Roles::from_theme(Theme::default());
    let comet = CursorPreviewSpec {
        trail_style: PreviewTrailStyle::Comet,
        trail_style_raw: Arc::from("comet"),
        ..CursorPreviewSpec::default()
    };
    let specs: Vec<(&str, SettingsPreviewSpec)> = vec![
        (
            "Rainbow kitty",
            SettingsPreviewSpec::cursor(rainbow_cursor("rainbow"))
                .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "rainbow"),
        ),
        (
            "Rainbow kitty",
            SettingsPreviewSpec::cursor(rainbow_cursor("rainbow"))
                .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "rainbow")
                .with_style_trial(Some(StyleTrial::Pointer)),
        ),
        (
            "Comet",
            SettingsPreviewSpec::cursor(comet.clone())
                .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "comet"),
        ),
        (
            "1 cell",
            SettingsPreviewSpec::cursor(CursorPreviewSpec { length: 1, ..comet })
                .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_LENGTH, "1")
                .with_focus_label("Trail length"),
        ),
        (
            "Automatic (SF Mono)",
            SettingsPreviewSpec::typography(13.0)
                .with_focus(crate::prefs::EDIT_FONT_FAMILY, "Automatic (SF Mono)"),
        ),
        (
            "Catppuccin Mocha",
            SettingsPreviewSpec::appearance(13.0)
                .with_focus(crate::prefs::EDIT_THEME, "Catppuccin Mocha")
                .with_theme_sample(true),
        ),
        (
            "Off",
            SettingsPreviewSpec::appearance(13.0)
                .with_focus(crate::prefs::EDIT_SELECTION_INACTIVE, "false")
                .with_focus_label("Dim selection when unfocused"),
        ),
    ];
    let squash = |text: &str| text.replace("-", "").split_whitespace().collect::<String>();
    let mut checked = 0;
    for scale in [1.0_f32, 1.5, 2.0] {
        let _ = install_preferences(AppearancePreferences {
            text_scale: scale,
            ..AppearancePreferences::default()
        });
        for (value, spec) in &specs {
            for width in [
                96.0_f32, 122.0, 146.0, 150.0, 206.0, 254.0, 320.0, 420.0, 720.0,
            ] {
                let spec = spec.clone().with_measure_width(width);
                let height = spec.minimum_height(48.0);
                let rect = LogicalRect::new(10.0, 10.0, width, height);
                let mut prims = Vec::new();
                spec.paint(&mut prims, rect, Theme::default(), roles);
                let header_bottom = spec.terminal_rect(rect).y;
                let words = prims
                    .iter()
                    .filter_map(|prim| match prim {
                        DrawPrim::Text {
                            s,
                            x,
                            px,
                            face,
                            baseline,
                            ..
                        } if *baseline <= header_bottom => Some((s.clone(), *x, *px, *face)),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let context = format!("{value} at {width}pt, {scale}x");
                assert!(!words.is_empty(), "{context}: the header says something");
                for (text, x, px, face) in &words {
                    assert!(!text.contains('…'), "{context}: {text:?} is cut");
                    let right = x + crate::tray_raster::ui_text_width_for(*face, text, *px);
                    assert!(
                        right <= rect.right() - PREVIEW_INSET + 0.5,
                        "{context}: {text:?} runs to {right:.1}"
                    );
                }
                let said = squash(
                    &words
                        .iter()
                        .map(|(text, ..)| text.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                );
                assert!(
                    said.contains(&squash(value)),
                    "{context}: the value is painted whole in {words:?}"
                );
                let preview = preview_node("preview", spec, height);
                let tree = UiTree::new(
                    UiNode::new(
                        "root",
                        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
                    )
                    .layout(Layout::column().padding(Insets::all(10.0)))
                    .children(vec![
                        preview.layout(
                            Layout::default()
                                .width(Length::Fixed(width))
                                .height(Length::Fixed(height))
                                .clipped(),
                        ),
                    ]),
                )
                .compile(LogicalRect::new(0.0, 0.0, width + 40.0, height + 40.0))
                .expect("compiles");
                assert!(
                    tree.fit_failures().is_empty(),
                    "{context}: {:?}",
                    tree.fit_failures()
                );
                checked += 1;
            }
        }
    }
    let _ = install_preferences(AppearancePreferences::default());
    assert_eq!(checked, 3 * 7 * 9);

    let long = "Pneumonoultramicroscopicsilicovolcanoconiosis".repeat(4);
    let pathological = SettingsPreviewSpec::cursor(CursorPreviewSpec::default())
        .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, long.as_str());
    let header = pathological.header_in(LogicalRect::new(0.0, 0.0, 122.0, 44.0));
    assert!(
        header.lines.is_empty(),
        "too short for its lines: {header:?}"
    );
    let tree = UiTree::new(
        UiNode::new(
            "root",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
        )
        .layout(Layout::column())
        .children(vec![
            preview_node("preview", pathological, 44.0).layout(
                Layout::default()
                    .width(Length::Fixed(122.0))
                    .height(Length::Fixed(44.0))
                    .clipped(),
            ),
        ]),
    )
    .compile(LogicalRect::new(0.0, 0.0, 160.0, 100.0))
    .expect("compiles");
    assert!(tree.fit_failures().is_empty(), "{:?}", tree.fit_failures());
}

#[test]
fn a_preview_header_never_takes_its_specimens_least_row() {
    use crate::native_appearance::{AppearancePreferences, install_preferences};
    let _ = compiled(SettingsPreviewSpec::default());
    let prepared =
        crate::tray_raster::prepare_semantic_font(&crate::widget::SemanticFontCandidate::default());
    let roles = Roles::from_theme(Theme::default());
    let specs = [
        SettingsPreviewSpec::cursor(rainbow_cursor("rainbow"))
            .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "rainbow"),
        SettingsPreviewSpec::typography(12.0)
            .with_focus(crate::prefs::EDIT_FONT_FAMILY, "Automatic (SF Mono)"),
    ];
    let mut squeezed = 0;
    for scale in [1.0_f32, 2.0] {
        let _ = install_preferences(AppearancePreferences {
            text_scale: scale,
            ..AppearancePreferences::default()
        });
        for spec in &specs {
            for width in [122.0_f32, 150.0, 206.0] {
                let spec = spec
                    .clone()
                    .with_prepared_font(prepared.clone())
                    .with_measure_width(width);
                let full = spec.normalized().header_layout(width);
                let band = HEADER_BAND;
                let body = spec.normalized().least_body();
                let least = (band + body).ceil();
                let most = spec.minimum_height(0.0).max(full.height + body).ceil();
                let mut height = least;
                while height <= most {
                    let rect = LogicalRect::new(10.0, 10.0, width, height);
                    let context =
                        format!("{} at {width}x{height}, {scale}x", spec.semantic_label());
                    let mut prims = Vec::new();
                    spec.paint(&mut prims, rect, Theme::default(), roles);
                    let rows = prims
                        .iter()
                        .find_map(|prim| match prim {
                            DrawPrim::TerminalSpecimen { spec, .. } => Some(spec.input.rows),
                            _ => None,
                        })
                        .unwrap_or(0);
                    assert!(rows >= 1, "{context}: the specimen paints a row");
                    let header_bottom = spec.terminal_rect(rect).y;
                    let words: Vec<String> = prims
                        .iter()
                        .filter_map(|prim| match prim {
                            DrawPrim::Text { s, baseline, .. }
                                if *baseline <= header_bottom && s != spec.scene.badge_label() =>
                            {
                                Some(s.clone())
                            }
                            _ => None,
                        })
                        .collect();
                    assert!(
                        words.iter().all(|word| !word.contains('…')),
                        "{context}: {words:?}"
                    );
                    if full.height + body > height + 0.01 {
                        assert!(words.is_empty(), "{context}: {words:?}");
                        squeezed += 1;
                    }
                    if height >= spec.minimum_height(0.0) {
                        assert_eq!(
                            words.join(" "),
                            full.lines.join(" "),
                            "{context}: CONTROL, the whole statement"
                        );
                    }
                    height += 4.0;
                }
            }
        }
    }
    let _ = install_preferences(AppearancePreferences::default());
    assert!(squeezed > 0, "some card holds the lines but not the row");
}

/// A card too short for its header's lines drops the words rather than cutting the last
/// line it has room for ("Comet ·…"): a header says a whole statement or nothing.
#[test]
fn a_short_card_drops_its_header_words_and_never_cuts_them() {
    use crate::native_appearance::{AppearancePreferences, install_preferences};
    let _ = compiled(SettingsPreviewSpec::default());
    let roles = Roles::from_theme(Theme::default());
    let comet = CursorPreviewSpec {
        trail_style: PreviewTrailStyle::Comet,
        trail_style_raw: Arc::from("comet"),
        ..CursorPreviewSpec::default()
    };
    let long = "Pneumonoultramicroscopicsilicovolcanoconiosis".repeat(2);
    let specs = [
        SettingsPreviewSpec::cursor(comet.clone())
            .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "comet"),
        SettingsPreviewSpec::cursor(comet)
            .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "comet")
            .with_style_trial(Some(StyleTrial::Pointer)),
        SettingsPreviewSpec::cursor(rainbow_cursor("rainbow"))
            .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "rainbow"),
        SettingsPreviewSpec::cursor(CursorPreviewSpec::default())
            .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, long.as_str()),
        SettingsPreviewSpec::typography(13.0)
            .with_focus(crate::prefs::EDIT_FONT_FAMILY, "Automatic (SF Mono)"),
    ];
    let squash = |text: &str| text.replace('-', "").split_whitespace().collect::<String>();
    let (mut dropped, mut kept) = (0, 0);
    for scale in [1.0_f32, 1.5, 2.0] {
        let _ = install_preferences(AppearancePreferences {
            text_scale: scale,
            ..AppearancePreferences::default()
        });
        for spec in &specs {
            for width in [96.0_f32, 122.0, 150.0, 206.0] {
                let spec = spec.clone().with_measure_width(width);
                let rungs = spec.normalized().header_rungs();
                let most = spec.minimum_height(0.0).ceil() as i32 + 8;
                for height in (24..=most).step_by(2) {
                    let rect = LogicalRect::new(10.0, 10.0, width, height as f32);
                    let context = format!(
                        "{} at {width}x{height}, {scale}x",
                        spec.normalized().focused_value
                    );
                    let header = spec.normalized().header_in(rect);
                    let said = header.lines.join(" ");
                    assert!(!said.contains('\u{2026}'), "{context}: {header:?}");
                    if header.lines.is_empty() {
                        assert!(!header.scene && header.says.is_empty(), "{context}");
                        dropped += 1;
                        continue;
                    }
                    assert!(
                        rungs.contains(&header.says) && squash(&said) == squash(&header.says),
                        "{context}: the lines say one whole statement: {header:?}"
                    );
                    let mut prims = Vec::new();
                    spec.paint(&mut prims, rect, Theme::default(), roles);
                    for prim in &prims {
                        if let DrawPrim::Text { s, .. } = prim {
                            assert!(!s.contains('\u{2026}'), "{context}: paints {s:?}");
                        }
                    }
                    kept += 1;
                }
            }
        }
    }
    let _ = install_preferences(AppearancePreferences::default());
    assert!(dropped > 0 && kept > 0, "{dropped} dropped, {kept} kept");
}

/// A header's first line holds its caption at every text size: the band grows with the
/// type for every scene (not only the text specimen), and so does the Themes demo's strip.
#[test]
fn a_preview_headers_first_line_follows_the_text_scale() {
    use crate::native_appearance::{AppearancePreferences, install_preferences};
    let _ = compiled(SettingsPreviewSpec::default());
    let specs = [
        SettingsPreviewSpec::cursor(CursorPreviewSpec::default())
            .with_focus(crate::prefs::EDIT_CURSOR_STYLE, "block"),
        SettingsPreviewSpec::appearance(13.0)
            .with_focus(crate::prefs::EDIT_THEME, "Catppuccin Mocha"),
        wallpaper_spec(None),
        SettingsPreviewSpec::typography(13.0)
            .with_focus(crate::prefs::EDIT_FONT_FAMILY, "Automatic (SF Mono)"),
    ];
    for scale in [1.0_f32, 1.5, 2.0] {
        let _ = install_preferences(AppearancePreferences {
            text_scale: scale,
            ..AppearancePreferences::default()
        });
        let px = header_caption().get();
        for spec in &specs {
            let header = spec.normalized().header_layout(720.0);
            assert_eq!(header.lines.len(), 1, "{scale}x: {header:?}");
            assert!(
                header.height + 0.01 >= px + 8.0,
                "{scale}x {:?}: a {px}px caption in a {}pt band",
                spec.scene,
                header.height
            );
        }
        for frame in [DemoFrame::Dots, DemoFrame::Windows] {
            let rect = LogicalRect::new(0.0, 0.0, 420.0, 260.0);
            let strip = DemoStripLayout::of(rect, frame);
            assert!(
                strip.tab_h(rect) + 0.01 >= strip.title_px + 8.0,
                "{scale}x {frame:?}: a {}px title in a {}pt tab",
                strip.title_px,
                strip.tab_h(rect)
            );
        }
    }
    let _ = install_preferences(AppearancePreferences::default());
}

#[test]
fn a_shortened_header_keeps_the_value_whole() {
    let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec::default())
        .with_focus(crate::prefs::EDIT_LOAD_ADAPTIVE_MOTION, "true")
        .with_focus_label("Reduce motion under heavy load");
    let wide = spec.header_layout(720.0);
    assert_eq!(wide.lines.len(), 1);
    assert!(wide.lines[0].starts_with("Reduce motion under heavy load: On"));
    let mut saw = (false, false);
    for width in (80..=720).step_by(8) {
        let header = spec.header_layout(width as f32);
        let said = header.lines.join(" ");
        assert!(!said.contains('…'), "{width}: {said}");
        assert!(
            said.ends_with("On") || said.contains("On ·"),
            "{width}: {said}"
        );
        saw.0 |= header.says == "On";
        saw.1 |= header.says == "Reduce motion under heavy load: On";
    }
    assert!(
        saw.0 && saw.1,
        "the ladder reaches the value alone: {saw:?}"
    );
}

#[test]
fn a_wide_preview_header_says_the_whole_badge() {
    let family = "Iosevka Term Extended Heavy Oblique";
    let spec = SettingsPreviewSpec::typography(12.0)
        .with_focus(crate::prefs::EDIT_FONT_FAMILY, family)
        .with_font_candidate(SemanticFontCandidate {
            regular: Some(family.to_string()),
            ..SemanticFontCandidate::default()
        });
    let header = |width: f32| {
        let mut prims = Vec::new();
        spec.paint(
            &mut prims,
            LogicalRect::new(0.0, 0.0, width, 132.0),
            Theme::default(),
            Roles::from_theme(Theme::default()),
        );
        prims
            .iter()
            .find_map(|prim| match prim {
                DrawPrim::Text { s, baseline, .. }
                    if *baseline < 27.0 && s != PreviewScene::Typography.badge_label() =>
                {
                    Some(s.clone())
                }
                _ => None,
            })
            .expect("the header names the font")
    };
    assert_eq!(
        header(720.0),
        "Font: Iosevka Term Extended Heavy Oblique · Text sample"
    );
    let narrow = spec.header_layout(320.0);
    let said = narrow.lines.join(" ");
    assert!(said.len() < header(720.0).len(), "{said}");
    assert!(said.contains(family) && !said.contains('…'), "{said}");
}

#[test]
fn an_automatic_font_size_reads_with_its_unit_on_the_number() {
    let unset = SettingsPreviewSpec::typography(12.0)
        .with_focus(crate::prefs::EDIT_FONT_PX, "12 (automatic)");
    assert_eq!(unset.candidate_badge(), "Size: 12 px (automatic)");
    let set = SettingsPreviewSpec::typography(14.0).with_focus(crate::prefs::EDIT_FONT_PX, "14");
    assert_eq!(set.candidate_badge(), "Size: 14 px");
}

#[test]
fn narrow_large_type_font_candidate_pill_contains_its_unicode_text() {
    let _ = compiled(SettingsPreviewSpec::default());
    let family = "Aurora 👩‍💻 Observatory élan 🇺🇳 — deliberately long candidate family";
    let mut spec =
        SettingsPreviewSpec::typography(14.0).with_focus(crate::prefs::EDIT_FONT_FAMILY, family);
    spec.font_status = "unavailable; fallback active".to_string();
    let terminal = LogicalRect::new(12.0, 20.0, 112.0, 72.0);
    let mut prims = Vec::new();
    paint_font_candidate_pill_at_scale(
        &mut prims,
        terminal,
        &spec,
        Roles::from_theme(Theme::default()),
        2.0,
    );

    let panel = prims
        .iter()
        .find_map(|prim| match prim {
            DrawPrim::Panel { x, y, w, h, .. } => Some((*x, *y, *w, *h)),
            _ => None,
        })
        .expect("font candidate pill panel");
    let texts = prims
        .iter()
        .filter_map(|prim| match prim {
            DrawPrim::Text {
                x,
                baseline,
                s,
                px,
                face,
                ..
            } => Some((*x, *baseline, s.as_str(), *px, *face)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(!texts.is_empty(), "font candidate pill text");
    assert!(panel.0 >= terminal.x);
    assert!(panel.1 >= terminal.y);
    assert!(panel.0 + panel.2 <= terminal.right() + 0.01);
    assert!(panel.1 + panel.3 <= terminal.bottom() + 0.01);
    for (x, baseline, text, px, face) in texts {
        assert!(!text.contains('…'), "{text:?}");
        assert!(std::str::from_utf8(text.as_bytes()).is_ok());
        let text_right = x + crate::tray_raster::ui_text_width_for(face, text, px);
        assert!(
            text_right <= panel.0 + panel.2 - 8.0 + 0.01,
            "text {text:?} ends at {text_right:.2} outside pill {panel:?}"
        );
        assert!(
            baseline > panel.1 && baseline < panel.1 + panel.3,
            "{text:?}"
        );
    }
}

#[test]
fn a_narrow_static_header_names_its_candidate_once() {
    let spec = SettingsPreviewSpec::appearance(24.0).with_focus(crate::prefs::EDIT_THEME, "Nord");
    assert_eq!(spec.animation(), PreviewAnimation::None);
    for rung in spec.header_rungs() {
        assert_eq!(rung.matches("Nord").count(), 1, "{rung}");
    }
    for width in [140.0_f32, 220.0, 420.0] {
        let said = spec.header_layout(width).lines.join(" ");
        assert_eq!(said.matches("Nord").count(), 1, "{width}: {said}");
    }
}

#[test]
fn every_trail_style_is_visible_on_light_and_dark_themes() {
    let _ = compiled(SettingsPreviewSpec::default());
    let prepared = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
        &SemanticFontCandidate::default(),
    );
    let themes = [
        (
            "GitHub Light",
            PreviewTerminalTheme::from_scheme(
                &aterm_types::scheme::builtin("GitHub Light").expect("built-in light theme"),
            ),
        ),
        (
            "Default",
            PreviewTerminalTheme::from_scheme(&aterm_types::ColorScheme::default()),
        ),
    ];
    let mut nonce = 0x5EED_0000_u64;
    let mut frame = |spec: &crate::widget::TerminalSpecimenSpec, input: RenderInput| {
        nonce += 1;
        let spec = crate::widget::TerminalSpecimenSpec {
            input: Arc::new(input),
            input_fingerprint: nonce,
            motion: None,
            ..spec.clone()
        };
        crate::tray_raster::rasterize_tray(
            &[DrawPrim::TerminalSpecimen {
                x: 0.0,
                y: 0.0,
                spec: Box::new(spec),
            }],
            300,
            120,
            1.0,
            [0, 0, 0, 255],
        )
        .0
    };
    for (name, theme) in themes {
        for &style in crate::prefs::CURSOR_TRAIL_STYLES {
            if style == "off" {
                continue;
            }
            let mut best = 0;
            for phase in [300, 900, 1_500, 2_100] {
                let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
                    blink: false,
                    trail_style: PreviewTrailStyle::parse(style),
                    trail_style_raw: Arc::from(style),
                    ..CursorPreviewSpec::default()
                })
                .with_terminal_theme(theme)
                .with_phase(phase)
                .with_prepared_font(prepared.clone())
                .normalized();
                let specimen = spec.terminal_specimen(theme.into_theme(), Some(5));
                let cell = crate::tray_raster::specimen_cell_size_for_test(&specimen, 1.0)
                    .expect("the fixture renderer forks");
                let scene = specimen.motion.clone().expect("the cursor scene");
                let lit = scene.decorate(&specimen.input, cell);
                let mut bare = lit.clone();
                bare.cursor_glow_add.clear();
                bare.glow_under.clear();
                bare.glow_halo.clear();
                bare.fire_patch.clear();
                bare.char_fg.clear();
                bare.fire_halo.clear();
                bare.cursor_trail.clear();
                let shown = differences(&frame(&specimen, lit), &frame(&specimen, bare)) / 4;
                best = best.max(shown);
            }
            assert!(
                best > 40,
                "{style} leaves only {best} visible pixels on {name}"
            );
        }
    }
}

#[test]
fn every_listed_cursor_style_is_a_look_of_its_own_at_every_card_size() {
    let _ = compiled(SettingsPreviewSpec::default());
    let prepared = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
        &SemanticFontCandidate::default(),
    );
    let themes = [
        (
            "GitHub Light",
            PreviewTerminalTheme::from_scheme(
                &aterm_types::scheme::builtin("GitHub Light").expect("built-in light theme"),
            ),
        ),
        (
            "Default",
            PreviewTerminalTheme::from_scheme(&aterm_types::ColorScheme::default()),
        ),
    ];
    const PHASES: [u64; 4] = [300, 900, 1_500, 2_100];
    let render = |style: &str, theme: PreviewTerminalTheme, rows: usize, phase: u64| {
        let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            blink: false,
            trail_style: PreviewTrailStyle::parse(style),
            trail_style_raw: Arc::from(style),
            ..CursorPreviewSpec::default()
        })
        .with_terminal_theme(theme)
        .with_phase(phase)
        .with_prepared_font(prepared.clone())
        .normalized();
        let specimen = spec.terminal_specimen(theme.into_theme(), Some(rows));
        crate::tray_raster::rasterize_tray(
            &[DrawPrim::TerminalSpecimen {
                x: 0.0,
                y: 0.0,
                spec: Box::new(specimen),
            }],
            360,
            160,
            1.0,
            [0, 0, 0, 255],
        )
        .0
    };
    let styles: Vec<&str> = crate::native_settings::listed_cursor_style_values().collect();
    for (name, theme) in themes {
        for rows in [2, 5] {
            let frames: Vec<Vec<Vec<u8>>> = styles
                .iter()
                .map(|style| {
                    PHASES
                        .iter()
                        .map(|&phase| render(style, theme, rows, phase))
                        .collect()
                })
                .collect();
            for (i, a) in styles.iter().enumerate() {
                for (j, b) in styles.iter().enumerate().skip(i + 1) {
                    let apart = (0..PHASES.len())
                        .map(|p| differences_up_to(&frames[i][p], &frames[j][p], 4 * 41) / 4)
                        .max()
                        .unwrap_or(0);
                    assert!(
                        apart > 40,
                        "{a} and {b} paint only {apart} pixels apart on {name} at {rows} rows"
                    );
                }
            }
            let default = styles
                .iter()
                .position(|style| *style == crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE)
                .expect("the default is listed");
            for (p, &phase) in PHASES.iter().enumerate() {
                assert_eq!(
                    differences(&render("rainbow", theme, rows, phase), &frames[default][p]),
                    0,
                    "an alias paints as the style it names ({name}, {rows} rows)"
                );
            }
        }
    }
}

#[test]
fn preview_summaries_name_the_setting_in_plain_words() {
    let cursor = |style, trail: &'static str| {
        SettingsPreviewSpec::cursor(CursorPreviewSpec {
            style,
            blink: false,
            trail_style: PreviewTrailStyle::parse(trail),
            trail_style_raw: Arc::from(trail),
            ..CursorPreviewSpec::default()
        })
        .with_phase(900)
        .semantic_value()
    };
    let bar = cursor(PreviewCursorStyle::Bar, "lumen");
    assert_ne!(bar, cursor(PreviewCursorStyle::Block, "lumen"), "{bar}");
    assert_ne!(bar, cursor(PreviewCursorStyle::Bar, "comet"), "{bar}");

    let window = SettingsPreviewSpec::appearance(14.0)
        .with_terminal_theme(NORD)
        .with_terminal_theme_identity(PreviewThemeIdentity {
            label: "Nord".to_string(),
            light: false,
        })
        .with_appearance(AppearancePreviewSpec {
            window_theme: "light".to_string(),
            ..AppearancePreviewSpec::default()
        })
        .with_focus(crate::prefs::EDIT_WINDOW_THEME, "light")
        .semantic_value();
    assert!(
        window.contains("Nord, dark") && window.contains("Light window appearance"),
        "{window}"
    );

    let pair = SettingsPreviewSpec::appearance(14.0)
        .with_terminal_theme(NORD)
        .with_terminal_theme_identity(PreviewThemeIdentity {
            label: "Nord".to_string(),
            light: false,
        })
        .with_focus(crate::prefs::EDIT_THEME, "dark:Nord,light:GitHub Light");
    assert_eq!(pair.candidate_badge(), "Theme: Nord");
    let sample = pair.clone().with_theme_sample(true);
    assert_eq!(sample.candidate_badge(), "Sample: Nord");
    assert_ne!(sample.paint_fingerprint(), pair.paint_fingerprint());

    let legacy_name = SettingsPreviewSpec::appearance(14.0)
        .with_terminal_theme_identity(PreviewThemeIdentity {
            label: "aterm Night".to_string(),
            light: false,
        })
        .with_focus(crate::prefs::EDIT_THEME, "Aterm Night");
    assert_eq!(legacy_name.candidate_badge(), "Theme: aterm Night");
    assert_eq!(
        legacy_name.with_theme_sample(true).candidate_badge(),
        "Sample: aterm Night"
    );

    let ligatures = SettingsPreviewSpec::typography(12.0)
        .with_focus(crate::prefs::EDIT_LIGATURES, "true")
        .with_focus_label("Ligatures");
    for text in [ligatures.candidate_badge(), ligatures.semantic_value()] {
        assert!(!text.to_lowercase().contains("true"), "{text}");
    }
    let line_height = SettingsPreviewSpec::typography(12.0)
        .with_focus(crate::prefs::EDIT_LINE_HEIGHT, "1.6")
        .with_focus_label("Line height");
    for text in [line_height.candidate_badge(), line_height.semantic_value()] {
        assert!(text.to_lowercase().contains("line height"), "{text}");
    }
    let family = SettingsPreviewSpec::typography(12.0)
        .with_font_candidate(SemanticFontCandidate {
            regular: Some("Berkeley Mono".to_string()),
            ..SemanticFontCandidate::default()
        })
        .with_focus(crate::prefs::EDIT_FONT_FAMILY, "Berkeley Mono");
    assert!(family.semantic_value().contains("Berkeley Mono"));
    let discovered = SettingsPreviewSpec::typography(12.0)
        .with_focus(crate::prefs::EDIT_FONT_FAMILY_BOLD, "auto (discovered)")
        .with_focus_label("Bold font family");
    for text in [discovered.candidate_badge(), discovered.semantic_value()] {
        assert!(!text.contains("discovered"), "{text}");
    }
}

fn unpack(color: u32) -> [u8; 3] {
    let rgb = packed_rgb(color);
    [rgb.r, rgb.g, rgb.b]
}

#[test]
fn spoken_palette_names_the_theme_its_side_and_measured_contrast() {
    let identity = PreviewThemeIdentity {
        label: "Arctic".to_string(),
        light: false,
    };
    let text = test_contrast_ratio(unpack(NORD.fg), unpack(NORD.bg));
    let illegible = NORD
        .ansi
        .iter()
        .filter(|&&color| test_contrast_ratio(unpack(color), unpack(NORD.bg)) < 3.0)
        .count();
    assert!(illegible > 0, "fixture has a dim slot");
    let words = if illegible == 1 { "colour" } else { "colours" };
    assert_eq!(
        spoken_palette(NORD, Some(&identity)),
        format!(
            "Arctic, dark, text contrast {}:1, {illegible} {words} below 3:1",
            theme_card::contrast_figure(f64::from(text))
        )
    );
    let light = PreviewThemeIdentity {
        light: true,
        ..identity
    };
    assert!(spoken_palette(NORD, Some(&light)).starts_with("Arctic, light,"));
}

#[test]
fn spoken_palette_counts_exactly_the_ansi_slots_below_three_to_one() {
    let legible = PreviewTerminalTheme {
        ansi: [NORD.fg; 16],
        ..NORD
    };
    assert!(spoken_palette(legible, None).ends_with(", 0 colours below 3:1"));
    let invisible = PreviewTerminalTheme {
        ansi: [NORD.bg; 16],
        ..NORD
    };
    assert!(spoken_palette(invisible, None).ends_with(", 16 colours below 3:1"));
    let mut one = legible;
    one.ansi[5] = NORD.bg;
    assert!(spoken_palette(one, None).ends_with(", 1 colour below 3:1"));
    let paper = PreviewTerminalTheme {
        fg: 0x20_2020,
        bg: 0xFA_FAFA,
        ..NORD
    };
    assert!(spoken_palette(paper, None).contains(", light, "));
    assert!(spoken_palette(NORD, None).contains(", dark, "));
}

#[test]
fn a_tried_style_badges_and_speaks_as_unsaved() {
    let spec = |trial: bool| {
        SettingsPreviewSpec::cursor(rainbow_cursor("rainbow"))
            .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "rainbow")
            .with_style_trial(trial.then_some(StyleTrial::Keyboard))
    };
    let tried = spec(true);
    let badge = tried.with_state_badge(tried.candidate_badge());
    assert!(badge.starts_with("Trying: Rainbow kitty"), "{badge}");
    assert!(
        tried
            .header_rungs()
            .iter()
            .all(|rung| rung.starts_with("Trying:"))
    );
    for width in [90.0_f32, 160.0, 260.0, 520.0] {
        let header = tried.header_layout(width);
        assert!(
            header.lines[0].starts_with("Trying:"),
            "{width}: {header:?}"
        );
    }
    assert!(tried.semantic_value().contains("not saved"));
    let saved = spec(false);
    let badge = saved.with_state_badge(saved.candidate_badge());
    assert!(badge.starts_with("Style: Rainbow kitty"), "{badge}");
    assert!(!badge.contains("Trying") && !saved.semantic_value().contains("not saved"));
}

#[test]
fn an_idle_focused_row_is_never_badged_live() {
    let comet = || CursorPreviewSpec {
        blink: false,
        trail_style: PreviewTrailStyle::Comet,
        trail_style_raw: Arc::from("comet"),
        ..CursorPreviewSpec::default()
    };
    let spec = SettingsPreviewSpec::cursor(comet())
        .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_BLOOM, "true")
        .with_focus_label("Bloom")
        .with_focused_effect(Some("Inactive: dark themes only".to_string()));
    let badge = spec.with_state_badge(spec.candidate_badge());
    assert_eq!(badge, "Bloom: On · Inactive: dark themes only");
    assert!(!spec.reads_live());
    assert!(
        spec.header_rungs()
            .iter()
            .all(|rung| !rung.contains("Live"))
    );
    assert!(spec.semantic_value().contains("Inactive: dark themes only"));
    let live = SettingsPreviewSpec::cursor(comet())
        .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_BLOOM, "true")
        .with_focus_label("Bloom");
    assert_eq!(
        live.with_state_badge(live.candidate_badge()),
        "Bloom: On · Live trail"
    );
    assert!(live.reads_live());
    let paused = spec.with_reduced_motion(true);
    assert!(
        !paused.live_state().contains("Inactive"),
        "{}",
        paused.live_state()
    );
}

#[test]
fn theme_stage_speaks_and_badges_the_theme_as_painted() {
    let theme = |value: &str, sample: bool| {
        SettingsPreviewSpec::appearance(14.0)
            .with_terminal_theme(NORD)
            .with_terminal_theme_identity(PreviewThemeIdentity {
                label: value.to_string(),
                light: false,
            })
            .with_focus(crate::prefs::EDIT_THEME, value)
            .with_theme_sample(sample)
    };
    let unset = "Default".to_string();
    let configured = theme(&unset, false);
    let badge = configured.with_state_badge(configured.candidate_badge());
    assert_eq!(badge, format!("Theme: {unset}"));
    let spoken = configured.semantic_value();
    assert!(
        spoken.starts_with(&format!("Color theme: {unset}, dark,")),
        "{spoken}"
    );
    assert!(!spoken.to_lowercase().contains("sample"), "{spoken}");

    let hearth = theme("Hearth", true);
    assert_eq!(
        hearth.with_state_badge(hearth.candidate_badge()),
        "Sample: Hearth"
    );
    let spoken = hearth.semantic_value();
    assert!(
        spoken.starts_with("Color theme sample: Hearth, dark,"),
        "{spoken}"
    );

    let window = theme(&unset, false).with_focus(crate::prefs::EDIT_WINDOW_THEME, "light");
    let badge = window.with_state_badge(window.candidate_badge());
    assert!(
        badge.ends_with(&format!(" · {}", window.state_badge())),
        "{badge}"
    );
}

#[test]
fn a_spec_without_a_candidate_draws_the_engine_default_ansi() {
    let host = Theme::default();
    let fallback = PreviewTerminalTheme::over_engine_palette(host);
    assert_eq!(
        fallback.ansi,
        PreviewTerminalTheme::from_scheme(&aterm_types::ColorScheme::default()).ansi,
        "the specimen's engine table and the Default scheme agree"
    );
    assert_eq!(
        (fallback.fg, fallback.bg),
        (host.fg & 0xff_ffff, host.bg & 0xff_ffff)
    );
}

#[test]
fn candidate_theme_changes_only_terminal_paint_and_declared_semantics() {
    let host = SettingsPreviewSpec::appearance(14.0);
    let nord = host.clone().with_terminal_theme(NORD);

    assert!(differences(&pixels(host.clone()), &pixels(nord.clone())) > 120);
    assert_ne!(host.paint_fingerprint(), nord.paint_fingerprint());

    let host_ui = compiled(host);
    let nord_ui = compiled(nord);
    assert_eq!(host_ui.bounds, nord_ui.bounds);
    assert_eq!(host_ui.hits, nord_ui.hits);
    assert_eq!(host_ui.focus_order, nord_ui.focus_order);

    let key = crate::native_ui::UiKey::new("preview");
    let host_semantic = host_ui.semantic(&key).expect("host preview semantics");
    let nord_semantic = nord_ui.semantic(&key).expect("Nord preview semantics");
    assert_eq!(host_semantic.key, nord_semantic.key);
    assert_eq!(host_semantic.parent, nord_semantic.parent);
    assert_eq!(host_semantic.rect, nord_semantic.rect);
    assert_eq!(host_semantic.role, nord_semantic.role);
    assert_eq!(host_semantic.label, nord_semantic.label);
    assert_eq!(host_semantic.state, nord_semantic.state);
    assert_eq!(host_semantic.action, nord_semantic.action);
    assert_eq!(host_semantic.audit_id, nord_semantic.audit_id);
    assert_ne!(host_semantic.value, nord_semantic.value);
    let crate::native_ui::SemanticValue::Text(value) = &nord_semantic.value else {
        panic!("preview exposes its candidate theme as text")
    };
    assert!(value.contains(&spoken_palette(NORD, None)));

    let host_prims = host_ui.tray(Theme::default(), 13.0).prims;
    let nord_prims = nord_ui.tray(Theme::default(), 13.0).prims;
    assert_eq!(
        panel_fill(&host_prims, 11.0),
        panel_fill(&nord_prims, 11.0),
        "candidate colors must not leak into host Settings chrome"
    );
    assert_ne!(panel_fill(&host_prims, 8.0), panel_fill(&nord_prims, 8.0));
    assert_eq!(
        panel_fill(&nord_prims, 8.0),
        rgba(crate::settings::u32_rgb(NORD.bg), 255)
    );
}

#[test]
fn automatic_window_samples_follow_the_terminal_palette() {
    let paper = PreviewTerminalTheme {
        fg: 0x33_3333,
        bg: 0xFA_FAFA,
        cursor: 0x35_84E4,
        selection: 0xD0_D7DE,
        ..NORD
    };
    let host = Theme::default();
    let dark_palette = SettingsPreviewSpec::appearance(14.0).with_terminal_theme(NORD);
    let light_palette = dark_palette.clone().with_terminal_theme(paper);
    assert!(!dark_palette.window_sample_is_light(host));
    assert!(light_palette.window_sample_is_light(host));
    let side_words = "matches the terminal theme, currently Light";
    assert!(
        light_palette.semantic_value().contains(side_words),
        "{}",
        light_palette.semantic_value()
    );
    assert!(
        differences(
            &pixels(dark_palette.clone()),
            &pixels(light_palette.clone())
        ) > 40
    );
    let demo = SettingsPreviewSpec {
        terminal_theme: Some(paper),
        ..SettingsPreviewSpec::theme_demo(14.0, 0)
    };
    assert_eq!(
        demo.window_sample_is_light(host),
        light_palette.window_sample_is_light(host)
    );
    let explicit = light_palette
        .clone()
        .with_appearance(AppearancePreviewSpec {
            window_theme: "dark".to_string(),
            ..AppearancePreviewSpec::default()
        });
    assert!(!explicit.window_sample_is_light(host));
    assert!(differences(&pixels(explicit), &pixels(light_palette)) > 40);
}

#[test]
fn window_sample_title_and_edge_hold_contrast_on_both_sides() {
    let white = PreviewTerminalTheme {
        fg: 0x24_292F,
        bg: 0xFF_FFFF,
        ..NORD
    };
    let one_dark = PreviewTerminalTheme {
        fg: 0xAB_B2BF,
        bg: 0x21_252B,
        ..NORD
    };
    for (window_theme, theme, rimmed) in [
        ("light", white, true),
        ("dark", one_dark, true),
        ("light", NORD, false),
        ("dark", white, false),
    ] {
        let context = format!("{window_theme} over #{:06X}", theme.bg);
        let spec = SettingsPreviewSpec::appearance(14.0)
            .with_terminal_theme(theme)
            .with_appearance(AppearancePreviewSpec {
                window_theme: window_theme.to_string(),
                ..AppearancePreviewSpec::default()
            });
        let prims = compiled(spec).tray(Theme::default(), 13.0).prims;
        let (bar, fill) = prims
            .iter()
            .find_map(|prim| match prim {
                DrawPrim::Panel {
                    x,
                    y,
                    w,
                    h,
                    radius,
                    fill,
                } if *radius == 0.0 && (*h - APPEARANCE_WINDOW_SAMPLE_HEIGHT).abs() < 0.01 => {
                    Some((
                        LogicalRect::new(*x, *y, *w, *h),
                        [fill[0], fill[1], fill[2]],
                    ))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("{context}: the window sample's bar"));
        let raster = raster_of(&prims);
        let rgb_at = |x: usize, y: usize| {
            let at = (y * VIEWPORT.width as usize + x) * 4;
            [raster[at], raster[at + 1], raster[at + 2]]
        };
        let page = crate::settings::u32_rgb(theme.bg);
        let line = (bar.y as usize + 8..bar.y as usize + 12)
            .flat_map(|y| (bar.x as usize + 50..bar.right() as usize - 12).map(move |x| (x, y)))
            .map(|(x, y)| rgb_at(x, y))
            .max_by(|a, b| test_contrast_ratio(*a, fill).total_cmp(&test_contrast_ratio(*b, fill)))
            .expect("the title line");
        assert!(
            test_contrast_ratio(line, fill) >= 4.5,
            "{context}: title line {line:?} on {fill:?} is {:.2}:1",
            test_contrast_ratio(line, fill)
        );
        let rim = rgb_at(bar.x as usize, bar.y as usize + 9);
        if rimmed {
            assert!(
                test_contrast_ratio(rim, page) >= 3.0,
                "{context}: rim {rim:?} on {page:?} is {:.2}:1",
                test_contrast_ratio(rim, page)
            );
        }
    }
    let old = vec![
        DrawPrim::Panel {
            x: 0.0,
            y: 0.0,
            w: 40.0,
            h: 19.0,
            radius: 0.0,
            fill: rgba([0xF1, 0xF2, 0xF5], 255),
        },
        DrawPrim::Line {
            x1: 0.0,
            y1: 10.0,
            x2: 40.0,
            y2: 10.0,
            width: 2.0,
            color: rgba(window_chrome_text_color(true), 210),
        },
    ];
    let raster = raster_of(&old);
    let worst = (0..40)
        .map(|x| {
            let at = (10 * VIEWPORT.width as usize + x) * 4;
            [raster[at], raster[at + 1], raster[at + 2]]
        })
        .map(|ink| test_contrast_ratio(ink, [0xF1, 0xF2, 0xF5]))
        .fold(0.0_f32, f32::max);
    assert!(worst < 3.0, "the old line read {worst:.2}:1");
}

fn window_sample_tones(spec: SettingsPreviewSpec) -> ([u8; 3], Option<[u8; 3]>, [u8; 3]) {
    let mut prims = Vec::new();
    spec.paint(
        &mut prims,
        LogicalRect::new(0.0, 0.0, 360.0, 156.0),
        Theme::default(),
        Roles::from_theme(Theme::default()),
    );
    let rgb = |c: [u8; 4]| [c[0], c[1], c[2]];
    let bar = |h: f32| (h - APPEARANCE_WINDOW_SAMPLE_HEIGHT).abs() < 0.01;
    let fill = prims.iter().find_map(|p| match p {
        DrawPrim::Panel {
            h, radius, fill, ..
        } if *radius == 0.0 && bar(*h) => Some(rgb(*fill)),
        _ => None,
    });
    let rim = prims.iter().find_map(|p| match p {
        DrawPrim::Stroke {
            h, width, color, ..
        } if bar(*h) && (*width - 1.0).abs() < 0.01 => Some(rgb(*color)),
        _ => None,
    });
    let line = prims.iter().find_map(|p| match p {
        DrawPrim::Line { width, color, .. } if (*width - 2.0).abs() < 0.01 => Some(rgb(*color)),
        _ => None,
    });
    (
        fill.expect("the sample's bar"),
        rim,
        line.expect("the sample's title line"),
    )
}

#[test]
fn the_demo_selected_tab_wears_the_strips_outline() {
    let rect = LogicalRect::new(0.0, 0.0, 520.0, 300.0);
    let layout = DemoStripLayout::of(rect, DemoFrame::Windows);
    let rimmed = |name: &str| {
        let candidate = PreviewTerminalTheme::from_scheme(
            &aterm_types::scheme::builtin(name).expect("a built-in theme"),
        );
        let strip = crate::tab_bar::strip_sample(crate::chrome_band::ChromePalette {
            theme: candidate.into_theme(),
            ansi: Some(demo_ansi_hues(&candidate)),
            authored: aterm_messages::tokens::AuthoredChrome::NONE,
        });
        let mut prims = Vec::new();
        paint_demo_windows_strip(
            &mut prims,
            rect,
            &layout,
            &strip,
            crate::settings::u32_rgb(candidate.bg),
            crate::settings::u32_rgb(candidate.fg),
            TypeStep::Caption.px(13.75),
        );
        let rim = prims.iter().any(|prim| {
            matches!(prim, DrawPrim::Stroke { width, color, radius, .. }
                if *width == 1.0
                    && *radius == DEMO_TAB_RADIUS - 0.5
                    && color[..3] == strip.seam)
        });
        (rim, strip)
    };
    let (light, strip) = rimmed("Default Light");
    assert!(light, "Default Light's tab is outlined: {strip:?}");
    assert!(aterm_messages::ink::contrast(strip.active, strip.band) >= 1.15);
    assert!(!rimmed("Default").0, "Default's tab stands off its band");
    for name in aterm_types::scheme::builtin_names() {
        let (rim, strip) = rimmed(name);
        assert_eq!(
            rim,
            crate::tab_bar::windows_tab_wears_rim(strip.active, strip.band),
            "{name}"
        );
        if crate::tab_bar::bg_is_light(strip.band) {
            assert!(rim, "{name}: every light band's selected tab is outlined");
        }
    }
}

#[test]
fn the_window_sample_wears_the_caption_tone_with_a_rim_on_windows() {
    let theme = |name: &str| {
        PreviewTerminalTheme::from_scheme(
            &aterm_types::scheme::builtin(name).expect("a built-in theme"),
        )
    };
    let contrast = aterm_messages::ink::contrast;
    let (light, dark) = (theme("Default Light"), theme("Default"));
    let band = |candidate: PreviewTerminalTheme| {
        crate::tab_bar::strip_sample(crate::chrome_band::ChromePalette {
            theme: candidate.into_theme(),
            ansi: Some(demo_ansi_hues(&candidate)),
            authored: aterm_messages::tokens::AuthoredChrome::NONE,
        })
    };
    let page = |candidate: PreviewTerminalTheme| crate::settings::u32_rgb(candidate.bg);
    let stock_light = [0xF1, 0xF2, 0xF5];
    assert!(
        contrast(stock_light, page(light)) < 1.1,
        "control: the stock fill vanishes"
    );
    for (candidate, rimmed) in [(light, true), (dark, false)] {
        let spec = SettingsPreviewSpec::appearance(14.0).with_terminal_theme(candidate);
        let (fill, rim, line) = window_sample_tones(spec);
        assert!(contrast(line, fill) >= 4.5, "{line:?} on {fill:?}");
        let strip = band(candidate);
        if cfg!(windows) {
            assert_eq!(fill, strip.band);
            assert_eq!(rim, rimmed.then_some(strip.seam));
            assert!(contrast(fill, page(candidate)) > 1.1, "{fill:?}");
        } else {
            assert_ne!(fill, strip.band);
            assert_eq!(
                rim.is_some(),
                contrast(fill, page(candidate)) < 1.3,
                "{fill:?}"
            );
            if let Some(rim) = rim {
                assert!(contrast(rim, page(candidate)) >= 3.0, "{rim:?}");
            }
        }
        if let Some(rim) = rim {
            assert!(contrast(rim, fill) >= 1.5, "{rim:?} on {fill:?}");
            assert!(contrast(rim, page(candidate)) >= 1.5, "{rim:?}");
        }
    }
    if cfg!(windows) {
        let plain = SettingsPreviewSpec::appearance(14.0).with_terminal_theme(light);
        let red = [0xFF, 0x00, 0x00];
        let authored = plain
            .clone()
            .with_authored_chrome(aterm_messages::tokens::AuthoredChrome {
                border: Some(red),
                ..aterm_messages::tokens::AuthoredChrome::NONE
            });
        assert_eq!(window_sample_tones(authored.clone()).1, Some(red));
        assert_ne!(plain.paint_fingerprint(), authored.paint_fingerprint());
    }
    for (candidate, forced, stock) in [
        (light, "light", stock_light),
        (dark, "dark", [0x20, 0x22, 0x2A]),
    ] {
        let spec = SettingsPreviewSpec::appearance(14.0)
            .with_terminal_theme(candidate)
            .with_appearance(AppearancePreviewSpec {
                window_theme: forced.to_string(),
                ..AppearancePreviewSpec::default()
            });
        let (fill, rim, line) = window_sample_tones(spec);
        assert_eq!(fill, stock);
        assert!(contrast(line, fill) >= 4.5);
        assert_eq!(
            rim.is_some(),
            contrast(fill, page(candidate)) < 1.3,
            "{forced}"
        );
        if let Some(rim) = rim {
            assert!(contrast(rim, fill) >= 1.5, "{rim:?} on {fill:?}");
        }
    }
}

#[test]
fn small_top_theme_card_always_shows_text_and_the_candidate_ansi_palette() {
    let candidate = PreviewTerminalTheme {
        ansi: [
            0x101010, 0xAA0000, 0x00AA00, 0xAAAA00, 0x0000AA, 0xAA00AA, 0x00AAAA, 0xAAAAAA,
            0x303030, 0xFF3030, 0x30FF30, 0xFFFF30, 0x3030FF, 0xFF30FF, 0x30FFFF, 0xFFFFFF,
        ],
        ..NORD
    };
    let spec = SettingsPreviewSpec::appearance(14.0)
        .with_terminal_theme(candidate)
        .with_focus(crate::prefs::EDIT_THEME, "Nord");
    let rect = LogicalRect::new(0.0, 0.0, 360.0, 132.0);
    let mut prims = Vec::new();
    spec.paint(
        &mut prims,
        rect,
        Theme::default(),
        Roles::from_theme(Theme::default()),
    );
    assert!(prims.iter().any(|primitive| matches!(
        primitive,
        DrawPrim::Text { s, face: TextFace::Mono, .. } if s == "$ aterm"
    )));
    let swatches = prims
        .iter()
        .filter_map(|primitive| match primitive {
            DrawPrim::Panel {
                x,
                y,
                w,
                h,
                radius,
                fill,
                ..
            } if (*radius - 2.0).abs() < f32::EPSILON && (*h - 6.0).abs() < f32::EPSILON => {
                Some((*x, *y, *w, *fill))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(swatches.len(), 16, "all sixteen ANSI colours are visible");
    for (index, (x, y, width, fill)) in swatches.iter().enumerate() {
        assert_eq!(
            *fill,
            rgba(crate::settings::u32_rgb(candidate.ansi[index]), 255)
        );
        assert!(*x >= rect.x && *x + *width <= rect.right() + 0.01);
        assert!(*y >= rect.y && *y + 6.0 <= rect.bottom() + 0.01);
    }
}

#[test]
fn a_preview_in_a_fading_strip_fades_as_one_picture() {
    let _ = compiled(SettingsPreviewSpec::appearance(14.0));
    let prepared =
        crate::tray_raster::prepare_semantic_font(&crate::widget::SemanticFontCandidate::default());
    assert!(prepared.is_ready());
    let roles = Roles::from_theme(Theme::default());
    let ground = [0x28, 0x1d, 0x4e, 255];
    let lut = |c: u8| {
        let c = f32::from(c) / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let encode = |l: f32| {
        let l = l.clamp(0.0, 1.0);
        let e = if l <= 0.003_130_8 {
            l * 12.92
        } else {
            1.055 * l.powf(1.0 / 2.4) - 0.055
        };
        (e * 255.0).round() as i32
    };
    for scene in ["cursor", "appearance"] {
        for scale in [1.0_f32, 2.0] {
            let spec = match scene {
                "cursor" => {
                    SettingsPreviewSpec::cursor(CursorPreviewSpec::default()).with_phase(780)
                }
                _ => SettingsPreviewSpec::appearance(14.0),
            }
            .with_prepared_font(prepared.clone());
            let rect = LogicalRect::new(20.0, 10.0, 420.0, 156.0);
            let (top, bottom) = (rect.y + 60.0, rect.bottom());
            let prims = |fade: bool| {
                let mut prims = vec![
                    DrawPrim::Panel {
                        x: 0.0,
                        y: 0.0,
                        w: 480.0,
                        h: 180.0,
                        radius: 0.0,
                        fill: ground,
                    },
                    DrawPrim::ClipPush {
                        x: rect.x,
                        y: rect.y,
                        w: rect.width,
                        h: rect.height,
                    },
                ];
                if fade {
                    prims.push(DrawPrim::FadePush {
                        y: top,
                        h: bottom - top,
                        from: 1.0,
                        to: 0.0,
                    });
                }
                spec.paint(&mut prims, rect, Theme::default(), roles);
                if fade {
                    prims.push(DrawPrim::FadePop);
                }
                prims.push(DrawPrim::ClipPop);
                prims
            };
            let (whole, pw, _) =
                crate::tray_raster::rasterize_tray(&prims(false), 480, 180, scale, [0; 4]);
            let (faded, _, _) =
                crate::tray_raster::rasterize_tray(&prims(true), 480, 180, scale, [0; 4]);
            let (y0, y1) = ((top * scale).ceil() as u32, (bottom * scale) as u32);
            let (x0, x1) = ((rect.x * scale) as u32, (rect.right() * scale) as u32);
            let mut worst = (0, 0, 0);
            for y in y0..y1 {
                let t = ((y as f32 + 0.5 - top * scale) / ((bottom - top) * scale)).clamp(0.0, 1.0);
                let opacity = (1.0 - t) * (1.0 - t);
                for x in x0..x1 {
                    let i = ((y * pw + x) * 4) as usize;
                    for k in 0..3 {
                        let want =
                            encode(lut(ground[k]) * (1.0 - opacity) + lut(whole[i + k]) * opacity);
                        let off = (i32::from(faded[i + k]) - want).abs();
                        if off > worst.0 {
                            worst = (off, x, y);
                        }
                    }
                }
            }
            assert!(
                worst.0 <= 2,
                "{scene} at {scale}x: the band is off its fade by {} at {:?}",
                worst.0,
                (worst.1, worst.2)
            );
        }
    }
}

#[test]
fn a_region_under_an_opaque_specimen_rasters_the_same_bytes_drawing_less() {
    let _ = compiled(SettingsPreviewSpec::appearance(14.0));
    let prepared =
        crate::tray_raster::prepare_semantic_font(&crate::widget::SemanticFontCandidate::default());
    assert!(prepared.is_ready());
    let roles = Roles::from_theme(Theme::default());
    for scene in ["cursor", "appearance"] {
        for scale in [1.0_f32, 2.0] {
            let context = format!("{scene} at {scale}x");
            let spec = match scene {
                "cursor" => {
                    SettingsPreviewSpec::cursor(CursorPreviewSpec::default()).with_phase(780)
                }
                _ => SettingsPreviewSpec::appearance(14.0),
            }
            .with_prepared_font(prepared.clone());
            let mut prims = vec![
                DrawPrim::Panel {
                    x: 0.0,
                    y: 0.0,
                    w: 480.0,
                    h: 220.0,
                    radius: 0.0,
                    fill: rgba([0x22, 0x26, 0x30], 255),
                },
                DrawPrim::Panel {
                    x: 12.0,
                    y: 16.0,
                    w: 456.0,
                    h: 196.0,
                    radius: 12.0,
                    fill: rgba([0x40, 0x46, 0x52], 200),
                },
            ];
            let rect = LogicalRect::new(20.0, 30.0, 420.0, 156.0);
            spec.paint(&mut prims, rect, Theme::default(), roles);
            let (full, pw, _) =
                crate::tray_raster::rasterize_tray(&prims, 480, 220, scale, [0, 0, 0, 0]);
            let crop = |region: crate::tray_raster::RasterRect| {
                let mut out = Vec::new();
                for row in region.y..region.y + region.height {
                    let start = ((row * pw + region.x) * 4) as usize;
                    out.extend_from_slice(&full[start..start + region.width as usize * 4]);
                }
                out
            };
            let tick = spec.tick_rect(rect);
            let crossing = crate::tray_raster::RasterRect {
                x: (tick.x * scale).floor() as u32,
                y: (tick.y * scale).floor() as u32,
                width: ((tick.right() * scale).ceil() - (tick.x * scale).floor()) as u32,
                height: ((tick.bottom() * scale).ceil() - (tick.y * scale).floor()) as u32,
            };
            let covered = crate::tray_raster::occluded_rect_for_test(&prims, scale, crossing)
                .unwrap_or_else(|| panic!("{context}: the specimen covers its tick region"));
            let exact = crate::tray_raster::RasterRect {
                x: covered.0 as u32,
                y: covered.1 as u32,
                width: (covered.2 - covered.0) as u32,
                height: (covered.3 - covered.1) as u32,
            };
            for (name, region) in [("exact", exact), ("crossing", crossing)] {
                crate::tray_raster::DRAWN_PRIMS.with(|drawn| drawn.set(0));
                let bytes =
                    crate::tray_raster::rasterize_tray_region(&prims, scale, [0, 0, 0, 0], region);
                assert!(
                    bytes == crop(region),
                    "{context} {name}: the culled bytes differ"
                );
                if name == "exact" {
                    let drawn = crate::tray_raster::DRAWN_PRIMS.with(std::cell::Cell::get);
                    let drawable = prims
                        .iter()
                        .filter(|prim| {
                            !matches!(
                                prim,
                                DrawPrim::ClipPush { .. }
                                    | DrawPrim::ClipPop
                                    | DrawPrim::FadePush { .. }
                                    | DrawPrim::FadePop
                            )
                        })
                        .count() as u64;
                    assert!(
                        drawn < drawable,
                        "{context}: {drawn} of {drawable} prims drawn under the frame"
                    );
                }
            }
        }
    }

    let spec = SettingsPreviewSpec::appearance(14.0).with_prepared_font(prepared.clone());
    let mut sample = Vec::new();
    spec.paint(
        &mut sample,
        LogicalRect::new(0.0, 0.0, 420.0, 156.0),
        Theme::default(),
        roles,
    );
    let specimen = sample
        .into_iter()
        .find_map(|prim| match prim {
            DrawPrim::TerminalSpecimen { spec, .. } => Some(DrawPrim::TerminalSpecimen {
                x: 0.0,
                y: 0.0,
                spec,
            }),
            _ => None,
        })
        .expect("the prepared specimen");
    let backdrop = [0x22, 0x26, 0x30, 255];
    let prims = vec![
        DrawPrim::Panel {
            x: 0.0,
            y: 0.0,
            w: 32.0,
            h: 16.0,
            radius: 0.0,
            fill: backdrop,
        },
        DrawPrim::ClipPush {
            x: 0.1,
            y: 0.0,
            w: 10.3,
            h: 8.0,
        },
        specimen,
        DrawPrim::ClipPop,
    ];
    let scale = 1.25;
    let (full, stride, _) = crate::tray_raster::rasterize_tray(&prims, 32, 16, scale, [0, 0, 0, 0]);
    let region = crate::tray_raster::RasterRect {
        x: 0,
        y: 0,
        width: 14,
        height: 10,
    };
    assert!(
        crate::tray_raster::occluded_rect_for_test(&prims, scale, region).is_some(),
        "the fractional region selects the culling path"
    );
    let patch = crate::tray_raster::rasterize_tray_region(&prims, scale, [0, 0, 0, 0], region);
    for row in 0..region.height as usize {
        let from = row * stride as usize * 4;
        let at = row * region.width as usize * 4;
        assert_eq!(
            &patch[at..at + region.width as usize * 4],
            &full[from..from + region.width as usize * 4],
            "fractional clip row {row} agrees with the full raster"
        );
        assert_eq!(
            &patch[at + 13 * 4..at + 14 * 4],
            &backdrop,
            "the backdrop outside the specimen's actual clip survives"
        );
    }
}

#[test]
fn pinned_specimens_fill_their_card_with_whole_rows() {
    let _ = compiled(SettingsPreviewSpec::appearance(14.0));
    let prepared =
        crate::tray_raster::prepare_semantic_font(&crate::widget::SemanticFontCandidate::default());
    assert!(prepared.is_ready());
    let mut checked = 0;
    for (scene, most) in [
        ("typography", TYPOGRAPHY_SPECIMEN_ROWS),
        ("cursor", CURSOR_SPECIMEN_ROWS),
        ("appearance", APPEARANCE_SPECIMEN_ROWS),
    ] {
        for height in [108.0, 132.0, 156.0] {
            for font_px in [10.0, 12.0, 14.0] {
                for line_height in [1.0, 1.2] {
                    for dpi in [1.0, 2.0] {
                        let mut spec = match scene {
                            "typography" => SettingsPreviewSpec::typography(font_px)
                                .with_focus(crate::prefs::EDIT_FONT_FAMILY, "default"),
                            "cursor" => SettingsPreviewSpec {
                                font_px,
                                ..SettingsPreviewSpec::cursor(CursorPreviewSpec::default())
                            },
                            _ => SettingsPreviewSpec::appearance(font_px),
                        }
                        .with_prepared_font(prepared.clone());
                        spec.line_height = line_height;
                        spec.terminal_scale = dpi;
                        spec.font_status = "committed renderer font cascade ready".to_string();
                        let context =
                            format!("{scene} {height}pt {font_px}px lh {line_height} {dpi}x");
                        let mut prims = Vec::new();
                        spec.paint(
                            &mut prims,
                            LogicalRect::new(0.0, 0.0, 720.0, height),
                            Theme::default(),
                            Roles::from_theme(Theme::default()),
                        );
                        let Some(terminal) = prims.iter().find_map(|prim| match prim {
                            DrawPrim::Panel {
                                x, y, w, h, radius, ..
                            } if *radius == 8.0 => Some(LogicalRect::new(*x, *y, *w, *h)),
                            _ => None,
                        }) else {
                            continue;
                        };
                        let Some((y, specimen)) = prims.iter().find_map(|prim| match prim {
                            DrawPrim::TerminalSpecimen { y, spec, .. } => Some((*y, spec)),
                            _ => None,
                        }) else {
                            continue;
                        };
                        let normalized = spec.normalized();
                        let pitch = normalized.row_height_at(specimen.font_px);
                        let rows = specimen.input.rows.min(most);
                        let shown = if scene == "cursor" {
                            ((terminal.height / pitch).floor() as usize).min(most)
                        } else {
                            rows
                        };
                        let bottom = match scene {
                            "typography" => terminal.bottom() - 4.0,
                            "appearance" => appearance_specimen_clip(terminal).bottom(),
                            _ => terminal.bottom(),
                        };
                        let band = bottom - (y + shown as f32 * pitch);
                        assert!(band >= -0.01, "{context}: rows past the body by {band}");
                        if shown < most {
                            assert!(
                                band < pitch,
                                "{context}: {band}pt of empty terminal under {shown} rows (pitch {pitch})"
                            );
                        }
                        checked += 1;
                    }
                }
            }
        }
    }
    assert!(checked > 50, "{checked}");
    let text = SettingsPreviewSpec::typography(12.0)
        .with_focus(crate::prefs::EDIT_FONT_FAMILY, "default")
        .with_prepared_font(prepared.clone());
    let cell = prepared.specimen_cell_height(12.0, 1.0, &[]).unwrap() as f32;
    assert_eq!(text.normalized().row_height_at(12.0), cell);
    let mut prims = Vec::new();
    text.paint(
        &mut prims,
        LogicalRect::new(0.0, 0.0, 720.0, 156.0),
        Theme::default(),
        Roles::from_theme(Theme::default()),
    );
    let rows = prims
        .iter()
        .find_map(|prim| match prim {
            DrawPrim::TerminalSpecimen { spec, .. } => Some(spec.input.rows),
            _ => None,
        })
        .expect("the text specimen");
    assert!(rows >= 7, "a 156pt text card shows {rows} rows");
}

#[test]
fn color_specimen_shows_fewer_whole_rows_never_cut_ones() {
    let _ = compiled(SettingsPreviewSpec::appearance(14.0));
    let prepared =
        crate::tray_raster::prepare_semantic_font(&crate::widget::SemanticFontCandidate::default());
    assert!(prepared.is_ready());
    let paint = |spec: &SettingsPreviewSpec, rect: LogicalRect| {
        let mut prims = Vec::new();
        spec.paint(
            &mut prims,
            rect,
            Theme::default(),
            Roles::from_theme(Theme::default()),
        );
        prims
    };
    let swatches = |prims: &[DrawPrim], swatch_y: f32| {
        prims
            .iter()
            .filter(|primitive| {
                matches!(
                    primitive,
                    DrawPrim::Panel { y, h, .. }
                        if (*y - swatch_y).abs() < 0.01
                            && (*h - APPEARANCE_SWATCH_HEIGHT).abs() < 0.01
                )
            })
            .count()
    };
    for font_px in [12.0, 24.0] {
        let spec = SettingsPreviewSpec::appearance(font_px).with_prepared_font(prepared.clone());
        let floor = spec.minimum_height(48.0);
        let mut shown = Vec::new();
        for height in [floor, 96.0_f32.max(floor), 132.0_f32.max(floor)] {
            let rect = LogicalRect::new(0.0, 0.0, 360.0, height);
            let prims = paint(&spec, rect);
            let specimen = prims
                .iter()
                .find_map(|primitive| match primitive {
                    DrawPrim::TerminalSpecimen { spec, .. } => Some(spec.as_ref()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{font_px}px at {height}pt paints its specimen"));
            assert!(specimen.font_px <= font_px, "{font_px}px at {height}pt");
            let row = spec.normalized().row_height_at(specimen.font_px);
            let terminal = LogicalRect::new(10.0, 32.0, 340.0, height - 42.0);
            let specimen_clip = appearance_specimen_clip(terminal);
            let rows = specimen.input.cells.len();
            assert!(rows >= 1, "{font_px}px at {height}pt keeps a whole row");
            assert_eq!(
                rows,
                ((specimen_clip.height / row).floor() as usize).min(APPEARANCE_SPECIMEN_ROWS)
            );
            assert!(rows as f32 * row <= specimen_clip.height + 0.01);
            let first: String = specimen.input.cells[0].iter().map(|cell| cell.ch).collect();
            assert!(first.starts_with("ANSI 4"), "the first rows stay: {first}");
            shown.push(rows);

            let swatch_y = appearance_swatch_y(terminal);
            assert!(specimen_clip.bottom() + APPEARANCE_SPECIMEN_SWATCH_GAP <= swatch_y + 0.01);
            assert!(swatch_y + APPEARANCE_SWATCH_HEIGHT <= terminal.bottom() + 0.01);
            assert_eq!(
                swatches(&prims, swatch_y),
                16,
                "the whole palette band shows"
            );
            assert!(prims.iter().all(|primitive| !matches!(
                primitive,
                DrawPrim::Text { s, .. } if s == "$ aterm" || s == "ready"
            )));
            assert!(prims.iter().any(|primitive| matches!(
                primitive,
                DrawPrim::ClipPush { x, y, w, h }
                    if (*x - specimen_clip.x).abs() < 0.01
                        && (*y - specimen_clip.y).abs() < 0.01
                        && (*w - specimen_clip.width).abs() < 0.01
                        && (*h - specimen_clip.height).abs() < 0.01
            )));
        }
        assert!(
            shown.windows(2).all(|pair| pair[0] <= pair[1]),
            "a taller card never shows fewer rows: {shown:?}"
        );
        let sliver = paint(&spec, LogicalRect::new(0.0, 0.0, 360.0, floor - 1.0));
        assert!(sliver.iter().all(|primitive| match primitive {
            DrawPrim::TerminalSpecimen { .. } => false,
            DrawPrim::Panel { h, radius, .. } => {
                (*h - APPEARANCE_SWATCH_HEIGHT).abs() > 0.01 && (*radius - 8.0).abs() > 0.01
            }
            _ => true,
        }));
    }
    assert!(
        SettingsPreviewSpec::appearance(12.0)
            .with_prepared_font(prepared)
            .minimum_height(48.0)
            <= 72.0
    );
}

#[test]
fn a_color_sample_fits_its_tile_at_every_theme_type_size() {
    let _ = compiled(SettingsPreviewSpec::appearance(14.0));
    let prepared =
        crate::tray_raster::prepare_semantic_font(&crate::widget::SemanticFontCandidate::default());
    assert!(prepared.is_ready());
    let mut cells = std::collections::HashMap::new();
    let mut cell_of = |specimen: &TerminalSpecimenSpec, font_px: f32| {
        *cells
            .entry((font_px.to_bits(), specimen.line_height.to_bits()))
            .or_insert_with(|| {
                let mut at = specimen.clone();
                at.font_px = font_px;
                let (w, h) = crate::tray_raster::specimen_cell_size_for_test(&at, 1.0)
                    .expect("a prepared font measures");
                (w as f32, h as f32)
            })
    };
    let inked = |input: &RenderInput| {
        input
            .cells
            .iter()
            .enumerate()
            .map(|(row, cells)| {
                let text = cells
                    .iter()
                    .rposition(|cell| cell.ch != ' ')
                    .map_or(0, |last| last + 1);
                if input.cursor_visible && input.cursor_row == row {
                    text.max(input.cursor_col + 1)
                } else {
                    text
                }
            })
            .collect::<Vec<_>>()
    };
    let overruns = |columns: &[usize],
                    (cell_w, cell_h): (f32, f32),
                    body: (f32, f32),
                    bar: Option<(f32, f32)>| {
        let (left, right) = body;
        columns
            .iter()
            .enumerate()
            .filter(|&(row, &cols)| {
                let end = left + cols as f32 * cell_w;
                let top = row as f32 * cell_h;
                end > right + 0.01
                    || bar.is_some_and(|(bar_left, bar_bottom)| {
                        top < bar_bottom && end > bar_left - APPEARANCE_WINDOW_SAMPLE_GAP + 0.01
                    })
            })
            .count()
    };
    let host = Theme::default();
    let (mut cases, mut narrowed, mut kept, mut below_bar) = (0, 0, 0, 0);
    for width in [240.0_f32, 300.0, 360.0, 480.0] {
        for height in [150.0_f32, 260.0] {
            for base in [6.0_f32, 9.0, 12.0, 14.0, 18.0, 24.0, 32.0] {
                for size_scale in [0.8_f32, 1.0, 1.15, 1.25] {
                    for line_height in [0.8_f32, 1.0, 1.4, 2.0] {
                        let font_px = base * size_scale;
                        let mut spec = SettingsPreviewSpec::appearance(font_px)
                            .with_prepared_font(prepared.clone());
                        spec.line_height = line_height;
                        let font_px = spec.normalized().font_px;
                        let mut prims = Vec::new();
                        spec.paint(
                            &mut prims,
                            LogicalRect::new(0.0, 0.0, width, height),
                            host,
                            Roles::from_theme(host),
                        );
                        let Some((x, specimen)) = prims.iter().find_map(|p| match p {
                            DrawPrim::TerminalSpecimen { x, spec, .. } => Some((*x, spec.as_ref())),
                            _ => None,
                        }) else {
                            continue;
                        };
                        let body = prims
                            .iter()
                            .find_map(|p| match p {
                                DrawPrim::Panel { x, w, radius, .. }
                                    if (*radius - 8.0).abs() < 0.01 =>
                                {
                                    Some((*x, *x + *w))
                                }
                                _ => None,
                            })
                            .expect("the terminal body is drawn");
                        let top = prims
                            .iter()
                            .find_map(|p| match p {
                                DrawPrim::TerminalSpecimen { y, .. } => Some(*y),
                                _ => None,
                            })
                            .unwrap();
                        let bar = prims.iter().find_map(|p| match p {
                            DrawPrim::Panel {
                                x, y, h, radius, ..
                            } if *radius == 0.0
                                && (*h - APPEARANCE_WINDOW_SAMPLE_HEIGHT).abs() < 0.01 =>
                            {
                                Some((*x, *y + *h - top))
                            }
                            _ => None,
                        });
                        assert_eq!(x, body.0, "the specimen starts at the body's edge");
                        if bar.is_some_and(|(_, bottom)| bottom <= 0.0) {
                            below_bar += 1;
                        }
                        let case =
                            format!("{width}x{height} at {font_px}px, line height {line_height}");
                        assert!(specimen.font_px <= font_px, "never grows: {case}");
                        let columns = inked(&specimen.input);
                        let fitted = cell_of(specimen, specimen.font_px);
                        assert_eq!(
                            overruns(&columns, fitted, body, bar),
                            0,
                            "every shown row fits: {case} drew {}px",
                            specimen.font_px
                        );
                        cases += 1;
                        if specimen.font_px < font_px {
                            narrowed += 1;
                            let authored = cell_of(specimen, font_px);
                            assert!(
                                overruns(&columns, authored, body, bar) > 0,
                                "the authored size overran: {case}"
                            );
                        } else {
                            kept += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(
        narrowed > 0 && kept > 0 && below_bar > 0,
        "{cases}: {narrowed} narrowed, {kept} kept, {below_bar} below the bar"
    );
}

#[test]
fn typography_sample_is_unicode_real_and_column_safe() {
    let prims = compiled(SettingsPreviewSpec::typography(14.0))
        .tray(Theme::default(), 13.0)
        .prims;
    let specimen = prims
        .iter()
        .find_map(|primitive| match primitive {
            DrawPrim::TerminalSpecimen { spec, .. } => Some(spec),
            _ => None,
        })
        .expect("real terminal specimen primitive");
    let cells = specimen.input.cells.iter().flatten().collect::<Vec<_>>();
    assert!(cells.iter().any(|cell| cell.bold && !cell.italic));
    assert!(cells.iter().any(|cell| !cell.bold && cell.italic));
    assert!(cells.iter().any(|cell| cell.bold && cell.italic));
    assert!(
        cells
            .iter()
            .any(|cell| cell.underline != aterm_core::terminal::UnderlineStyle::None)
    );
    let terminal_text = cells.iter().map(|cell| cell.ch).collect::<String>();
    for sample in ['你', '好', '∑', '✓', '♥', '🚀', '😀', '👩'] {
        assert!(
            terminal_text.contains(sample),
            "missing fallback sample {sample}"
        );
    }
    assert_eq!(specimen.input.cursor_row, 1);
    let row: String = specimen.input.cells[1].iter().map(|cell| cell.ch).collect();
    assert!(row[..specimen.input.cursor_col].ends_with("cursor"));
    assert!(specimen.input.selection.has_selection());
    let breaking = SettingsPreviewSpec::typography(14.0)
        .with_focus(crate::prefs::EDIT_CURSOR_BREAK_LIGATURES, "true");
    let (input, _) = build_terminal_specimen_input(&breaking.normalized(), Theme::default());
    assert_eq!(input.cursor_col, 1);
}

#[test]
fn visual_settings_change_real_pixels_without_changing_semantic_bounds() {
    let base = SettingsPreviewSpec::default().with_phase(780);
    let style = SettingsPreviewSpec {
        cursor: CursorPreviewSpec {
            trail_style: PreviewTrailStyle::Water,
            ..base.cursor.clone()
        },
        ..base.clone()
    };
    let font = SettingsPreviewSpec {
        font_px: 22.0,
        ..base.clone()
    };
    let reduced = base.clone().with_reduced_motion(true);

    let base_pixels = pixels(base.clone());
    assert!(differences(&base_pixels, &pixels(style.clone())) > 120);
    assert!(differences(&base_pixels, &pixels(font.clone())) > 120);
    assert!(differences(&base_pixels, &pixels(reduced.clone())) > 120);

    let bounds = |spec: SettingsPreviewSpec| {
        let control = spec.pause_control();
        let compiled = compiled(spec);
        let node = compiled
            .semantic(&crate::native_ui::UiKey::new("preview"))
            .expect("semantic preview");
        assert_eq!(
            node.role,
            if control {
                SemanticRole::Button
            } else {
                SemanticRole::Group
            }
        );
        assert!(node.label.contains("preview"));
        assert!(matches!(
            node.value,
            crate::native_ui::SemanticValue::Text(_)
        ));
        assert!(
            compiled
                .hits
                .iter()
                .all(|hit| hit.action.as_str() == PREVIEW_PAUSE_ACTION),
            "a demonstration is not a fake control: its only activation is the real Pause/Play"
        );
        node.rect
    };
    assert_eq!(bounds(base.clone()), bounds(style));
    assert_eq!(bounds(base.clone()), bounds(font));
    assert_eq!(bounds(base), bounds(reduced));
}

#[test]
fn injected_phase_is_deterministic_and_reduced_motion_is_stable() {
    let first = SettingsPreviewSpec::default().with_phase(420);
    let later = SettingsPreviewSpec::default().with_phase(980);
    assert_eq!(pixels(first.clone()), pixels(first));
    assert!(
        differences(
            &pixels(later.clone()),
            &pixels(SettingsPreviewSpec::default().with_phase(420))
        ) > 80
    );

    let reduced_first = SettingsPreviewSpec::default()
        .with_phase(420)
        .with_reduced_motion(true);
    let reduced_later = later.with_reduced_motion(true);
    assert_eq!(
        pixels(reduced_first),
        pixels(reduced_later),
        "reduced motion freezes both the trail and cursor blink"
    );

    let live_first = compiled(SettingsPreviewSpec::default().with_phase(420));
    let live_later = compiled(SettingsPreviewSpec::default().with_phase(980));
    assert_ne!(live_first.fingerprint(), live_later.fingerprint());
    let reduced_first = compiled(
        SettingsPreviewSpec::default()
            .with_phase(420)
            .with_reduced_motion(true),
    );
    let reduced_later = compiled(
        SettingsPreviewSpec::default()
            .with_phase(980)
            .with_reduced_motion(true),
    );
    assert_eq!(reduced_first.fingerprint(), reduced_later.fingerprint());
}

#[test]
fn non_finite_glow_inputs_fail_off_like_the_live_renderer() {
    let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
        trail_style: PreviewTrailStyle::Comet,
        duration_ms: u64::MAX,
        length: usize::MAX,
        intensity: f32::INFINITY,
        radius: f32::NAN,
        momentum_glow: false,
        ..CursorPreviewSpec::default()
    })
    .with_phase(900)
    .normalized();
    assert_eq!(spec.cursor.duration_ms, MAX_DURATION_MS);
    assert_eq!(spec.cursor.length, MAX_LENGTH);
    assert_eq!(spec.cursor.intensity, 0.0);
    assert_eq!(spec.cursor.radius, 0.0);

    assert!(!has_effect_light(&played(&spec, Theme::default())));
}

/// The rainbow beside the pet `pet` names (`cat` or `dog`).
fn beside(pet: &'static str) -> CursorPreviewSpec {
    CursorPreviewSpec {
        pet: aterm_effects::cursor_pet::species(pet),
        ..rainbow_cursor("rainbow")
    }
}

fn rainbow_cursor(raw: &'static str) -> CursorPreviewSpec {
    CursorPreviewSpec {
        blink: false,
        trail_style: PreviewTrailStyle::RainbowKitty,
        trail_style_raw: Arc::from(raw),
        ..CursorPreviewSpec::default()
    }
}

#[test]
fn the_preview_s_rainbow_draws_its_band() {
    let theme = Theme::default();
    let rainbow = rainbow_cursor("rainbow");
    let cfg = rainbow.glow_config(theme, 0.5, true);
    assert!(cfg.ribbon_tall && !cfg.ribbon_flat);
    assert_eq!(rainbow.effective_trail_style_raw(), "rainbow");
    // A retired name is no style: the card draws the default, the same band.
    let retired = rainbow_cursor("rainbow kitty underline");
    let retired = retired.glow_config(theme, 0.5, true);
    assert!(retired.ribbon_tall && !retired.ribbon_flat);
}

/// The rainbow kitty looks the light fork's companion pins preview.
#[cfg(unix)]
const RAINBOW_LOOKS: [&str; 1] = ["rainbow"];

/// Each assert of main's rule the preview's glow config `cfg` on the ground `(fg, bg)`
/// breaks, by name: the host pin's config asserts
/// ([`crate::light_fork_pin_tests::config_breaks`]), and its light, which no dark theme
/// takes (§15.2 1.11).
#[cfg(unix)]
fn preview_breaks(cfg: &crate::cursor_glow::GlowConfig, ground: (u32, u32)) -> Vec<&'static str> {
    let mut out = crate::light_fork_pin_tests::config_breaks(cfg, ground);
    if cfg.light != crate::cursor_glow::Light::OFF {
        out.push("light");
    }
    out
}

/// The host unreachability pin's companion (`crate::light_fork_pin_tests`,
/// `docs/design/PRISM-GLASS.md` §15.1 P0.9): the preview's producer draws every dark theme,
/// in each rainbow kitty look, by main's rule, its light [`crate::cursor_glow::Light::OFF`]
/// (§15.2 1.11); the ground that names no theme previews on the renderer's default theme.
#[cfg(unix)]
#[test]
fn the_preview_draws_every_dark_theme_by_mains_rule() {
    use crate::light_fork_pin_tests::Ground;

    let grounds = Ground::dark();
    let mut drawn = 0;
    for ground in &grounds {
        let theme = ground.scheme().map_or_else(Theme::default, |scheme| {
            PreviewTerminalTheme::from_scheme(&scheme).into_theme()
        });
        assert!(aterm_render::theme_is_dark(theme.bg), "{ground} is dark");
        for raw in RAINBOW_LOOKS {
            let cfg = rainbow_cursor(raw).glow_config(theme, 0.5, true);
            let broke = preview_breaks(&cfg, (theme.fg, theme.bg));
            assert!(broke.is_empty(), "{ground}, {raw}: {broke:?}");
            drawn += 1;
        }
    }
    assert_eq!(drawn, grounds.len() * RAINBOW_LOOKS.len());
}

/// The preview's light (§15.2 1.11): every light built-in and pack theme lights the
/// preview's first frame at `prism_glass::LIVE`, with no latch to wait on, and that light
/// breaks the companion pin's light assert as a dark theme's would (the assert's negative
/// control). A light page whose ink is not 3:1 darker than it is no light ground, so its
/// light stays [`crate::cursor_glow::Light::OFF`].
#[cfg(unix)]
#[test]
fn the_preview_lights_every_light_theme_from_its_first_frame() {
    use crate::cursor_glow::Light;
    use aterm_effects::rainbow_kitty::prism_glass::LIVE;

    let builtins = aterm_types::scheme::builtin_names()
        .into_iter()
        .filter_map(|name| Some((name, aterm_types::scheme::builtin(name)?)));
    let packs = aterm_theme_pack::themes().iter().filter_map(|theme| {
        Some((
            theme.id,
            aterm_types::scheme::parse_scheme_str(theme.text).ok()?,
        ))
    });
    let mut lit = Vec::new();
    for (name, scheme) in builtins.chain(packs) {
        let theme = PreviewTerminalTheme::from_scheme(&scheme).into_theme();
        if aterm_render::theme_is_dark(theme.bg) {
            continue;
        }
        for raw in RAINBOW_LOOKS {
            let cfg = rainbow_cursor(raw).glow_config(theme, 0.5, true);
            assert_eq!(cfg.light, Light::at(LIVE), "{name}, {raw}");
            assert_eq!(
                preview_breaks(&cfg, (theme.fg, theme.bg)),
                ["light"],
                "{name}, {raw}"
            );
        }
        lit.push(name);
    }
    assert!(lit.contains(&"Default Light"), "the sweep met {lit:?}");

    let paper = PreviewTerminalTheme::from_scheme(
        &aterm_types::scheme::builtin("Default Light").expect("built-in light theme"),
    )
    .into_theme();
    let pale = Theme {
        fg: 0x00B8_B8B8,
        ..paper
    };
    assert_eq!(
        rainbow_cursor("rainbow").glow_config(pale, 0.5, true).light,
        Light::OFF,
        "ink under 3:1 on a light page"
    );
}

/// The preview's light keeps the window's page law (§15.2 1.7, `Light::on_page`): at the
/// glass level a card whose page takes no glass lights a sheer light, so its bed draws
/// main's paper table, where one whose page takes it draws the glass (the negative control);
/// each scene reads its wallpaper off its spec. Its face's part of the law is
/// `the_cards_letters_read_under_every_light`'s (§15.8 amendment 54).
#[cfg(unix)]
#[test]
fn the_preview_s_glass_keeps_the_page_law() {
    use aterm_effects::cursor_glow::Live;
    use aterm_effects::rainbow_kitty::{self as rk, ribbon::BedInkLut};

    let paper = PreviewTerminalTheme::from_scheme(
        &aterm_types::scheme::builtin("Default Light").expect("built-in light theme"),
    )
    .into_theme();
    let cursor = rainbow_cursor("rainbow");
    let bed = |clear: bool| {
        let cfg = cursor.glow_config_at(paper, 0.5, clear, Live::Glass);
        let lut = BedInkLut::synced(&rk::Config::from_glow(&cfg, false));
        (cfg.light, lut.glass_lit())
    };
    let (light, glass) = bed(true);
    assert!(
        light.glass() && glass,
        "control: a clear card draws the glass: {light:?}"
    );
    let (light, glass) = bed(false);
    assert!(
        light.on() && light.sheer() && !light.glass() && !glass,
        "a card whose page takes no glass draws main's paper table: {light:?}"
    );
    let wallpaper = |wallpaper| {
        let spec = SettingsPreviewSpec {
            wallpaper,
            ..SettingsPreviewSpec::cursor(cursor.clone())
        };
        CursorScene::new(&spec, paper, None).wallpaper
    };
    assert!(!wallpaper(None), "no wallpaper: a clear page");
    assert!(
        wallpaper(Some(solid_wallpaper([200, 30, 30], 0.0, false))),
        "a wallpaper under the card"
    );
}

#[test]
fn preview_cache_identity_preserves_case_visible_style_text() {
    let lower = SettingsPreviewSpec::cursor(rainbow_cursor("rainbow"));
    let authored = SettingsPreviewSpec::cursor(rainbow_cursor("Rainbow"));
    assert_ne!(
        lower.paint_fingerprint(),
        authored.paint_fingerprint(),
        "case-visible badge text cannot alias in the retained preview cache"
    );
}

/// The pet in the preview is the pet setting's, beside the rainbow and beside any other style.
#[test]
fn the_preview_draws_the_pet_the_pet_setting_names() {
    use aterm_effects::kitty_pet::PetSpecies;
    use std::hash::{Hash, Hasher};

    let beside = |pet: Option<PetSpecies>| CursorPreviewSpec {
        pet,
        ..rainbow_cursor("rainbow")
    };
    for (pet, expected) in [
        (None, PreviewTrailCompanion::None),
        (
            Some(PetSpecies::Cat),
            PreviewTrailCompanion::Pet(PetSpecies::Cat),
        ),
        (
            Some(PetSpecies::Dog),
            PreviewTrailCompanion::Pet(PetSpecies::Dog),
        ),
    ] {
        let cursor = beside(pet);
        assert_eq!(cursor.trail_companion(), expected, "{pet:?}");
        assert_eq!(cursor.effective_trail_style_raw(), "rainbow", "{pet:?}");
    }
    assert_eq!(
        CursorPreviewSpec::default().pet,
        Some(PetSpecies::Cat),
        "a spec that names no pet draws the default"
    );

    let body = |pet: PetSpecies| {
        let input = played(
            &SettingsPreviewSpec::cursor(beside(Some(pet))).with_phase(900),
            Theme::default(),
        );
        let sprite = sprites(&input).first().cloned().expect("a drawn companion");
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        sprite.art.data().hash(&mut hash);
        hash.finish()
    };
    assert_ne!(
        body(PetSpecies::Cat),
        body(PetSpecies::Dog),
        "species must change pixels"
    );
    assert_ne!(
        SettingsPreviewSpec::cursor(beside(Some(PetSpecies::Cat))).paint_fingerprint(),
        SettingsPreviewSpec::cursor(beside(None)).paint_fingerprint(),
        "the pet is part of the preview's identity"
    );
}

#[test]
fn the_cursor_scene_draws_one_caret_on_the_typed_line() {
    for style in [PreviewCursorStyle::Block, PreviewCursorStyle::Bar] {
        let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            style,
            blink: false,
            ..CursorPreviewSpec::default()
        })
        .with_phase(900);
        let mut prims = Vec::new();
        spec.paint(
            &mut prims,
            LogicalRect::new(0.0, 0.0, 480.0, 156.0),
            Theme::default(),
            Roles::from_theme(Theme::default()),
        );
        assert!(
            prims.iter().all(|primitive| !matches!(
                primitive,
                DrawPrim::Panel { radius, .. } if *radius != 11.0 && *radius != 8.0
            )),
            "only the card and the terminal canvas are panels"
        );
        let (base, _) = build_terminal_specimen_input(&spec.normalized(), Theme::default());
        assert!(
            !base.cursor_visible,
            "the snapshot carries no caret of its own"
        );
        let input = played(&spec, Theme::default());
        assert!(input.cursor_visible);
        assert_eq!(
            input.cursor_row,
            input.rows - 2,
            "{style:?} types on the prompt above the free row"
        );
        let row = &input.cells[input.cursor_row];
        assert_eq!(
            row[input.cursor_col].ch, ' ',
            "the caret leads the typed text"
        );
        assert_ne!(
            row[input.cursor_col - 1].ch,
            ' ',
            "right after the last typed key"
        );
    }
}

#[test]
fn reduced_motion_preview_keeps_the_resident_pet() {
    use aterm_effects::kitty_pet::PetSpecies;
    let reduced_input = |pet: Option<PetSpecies>| {
        played(
            &SettingsPreviewSpec::cursor(CursorPreviewSpec {
                pet,
                ..rainbow_cursor("rainbow")
            })
            .with_reduced_motion(true),
            Theme::default(),
        )
    };
    for pet in [PetSpecies::Cat, PetSpecies::Dog] {
        assert!(
            !sprites(&reduced_input(Some(pet))).is_empty(),
            "{pet:?} remains a static resident under Reduced Motion"
        );
    }
    assert!(
        sprites(&reduced_input(None)).is_empty(),
        "negative control: with the pet off no animal is drawn"
    );
}

/// The cells a sprite's drawn texels cover, `(row, col)`, at `cell` px.
fn inked_cells(
    sprite: &aterm_render::sprites::FreeSprite,
    (cell_w, cell_h): (usize, usize),
) -> std::collections::BTreeSet<(usize, usize)> {
    use aterm_core::render::FxTexels;

    let art = &sprite.art;
    let (aw, ah) = (usize::from(art.w()), usize::from(art.h()));
    let (dw, dh) = (usize::from(sprite.w), usize::from(sprite.h));
    let mut cells = std::collections::BTreeSet::new();
    if aw == 0 || ah == 0 || dw == 0 || dh == 0 {
        return cells;
    }
    for dy in 0..dh {
        for dx in 0..dw {
            let (x, y) = (sprite.x + dx as i32, sprite.y + dy as i32);
            let clip = sprite.clip;
            if x < clip.x0.max(0) || x >= clip.x1 || y < clip.y0.max(0) || y >= clip.y1 {
                continue;
            }
            let tx = dx * aw / dw;
            let tx = if sprite.flip_x { aw - 1 - tx } else { tx };
            let at = (dy * ah / dh) * aw + tx;
            let inked = match art.texels() {
                FxTexels::Index8 => art.data()[at] != 0,
                FxTexels::Rgba8Premul | FxTexels::Rgba8Straight => art.data()[at * 4 + 3] != 0,
            };
            if inked {
                cells.insert((y as usize / cell_h, x as usize / cell_w));
            }
        }
    }
    cells
}

/// The resident pet keeps off the scene's letters as the terminal's head law
/// keeps it (`kitty_pet`'s `clear_of_letters_above`): no frame of the loop lays
/// a drawn texel of it on a lettered cell or the caret's, in every companion
/// style on dark and light, in a roomy card and a short one; and it is there
/// every frame, whole (no dip and return), its pose held more than a frame. Set
/// down at the station beside the caret whatever lies over it (the negative
/// control), its head lay on the command the line above holds.
#[test]
fn the_resident_pet_keeps_off_the_scenes_letters() {
    let light = PreviewTerminalTheme::from_scheme(
        &aterm_types::scheme::builtin("GitHub Light").expect("built-in light theme"),
    );
    let pets: Vec<&str> = crate::prefs::CURSOR_TRAIL_STYLES
        .iter()
        .copied()
        .filter(|raw| {
            matches!(
                CursorPreviewSpec {
                    trail_style: PreviewTrailStyle::parse(raw),
                    trail_style_raw: Arc::from(*raw),
                    ..CursorPreviewSpec::default()
                }
                .trail_companion(),
                PreviewTrailCompanion::Pet(_)
            )
        })
        .collect();
    assert!(pets.len() >= 3, "the pet styles: {pets:?}");
    let cell = (8, 17);
    let grounds = [("dark", None), ("light", Some(light))];
    let lays_on_letters =
        |seat_law: bool, pets: &[&str], grounds: &[(&str, Option<PreviewTerminalTheme>)]| {
            SCENE_CONTROLS.with(|controls| {
                controls.set(SceneControls {
                    pet_seat_law: seat_law,
                    ..controls.get()
                });
            });
            let mut on_letters = Vec::new();
            for &raw in pets {
                for &(side, ground) in grounds {
                    for rows in [None, Some(4)] {
                        let mut poses = Vec::new();
                        for phase in (0..CURSOR_SCENE_CYCLE_MS).step_by(MOTION_STEP_MS as usize) {
                            let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
                                blink: false,
                                trail_style: PreviewTrailStyle::parse(raw),
                                trail_style_raw: Arc::from(raw),
                                ..CursorPreviewSpec::default()
                            });
                            let spec = match ground {
                                Some(theme) => spec.with_terminal_theme(theme),
                                None => spec,
                            }
                            .with_phase(phase)
                            .normalized();
                            let theme = ground
                                .map_or_else(Theme::default, PreviewTerminalTheme::into_theme);
                            let (base, _) = build_terminal_specimen_input_rows(&spec, theme, rows);
                            let input = CursorScene::new(&spec, theme, rows).decorate(&base, cell);
                            let at = format!("{raw} {side} rows {rows:?} at {phase} ms");
                            let pet = sprites(&input)
                                .into_iter()
                                .max_by_key(|sprite| u32::from(sprite.w) * u32::from(sprite.h))
                                .unwrap_or_else(|| panic!("{at}: the pet is drawn"));
                            assert_eq!(pet.alpha, 255, "{at}: the pet is drawn whole");
                            poses.push((pet.art.id(), pet.flip_x));
                            let caret = (input.cursor_row, input.cursor_col);
                            let lettered: Vec<(usize, usize)> = inked_cells(&pet, cell)
                                .into_iter()
                                .filter(|&(row, col)| {
                                    (row, col) == caret
                                        || input
                                            .cells
                                            .get(row)
                                            .and_then(|cells| cells.get(col))
                                            .is_some_and(|cell| !matches!(cell.ch, ' ' | '\0'))
                                })
                                .collect();
                            if !lettered.is_empty() {
                                on_letters.push(format!("{at}: on {lettered:?}"));
                            }
                        }
                        if seat_law {
                            let flash = poses
                                .windows(3)
                                .position(|three| three[0] == three[2] && three[1] != three[0]);
                            assert_eq!(
                                flash, None,
                                "{raw} {side} rows {rows:?}: a pose shown for one frame"
                            );
                        }
                    }
                }
            }
            on_letters
        };
    let control = lays_on_letters(false, &pets[..1], &grounds[..1]);
    let law = lays_on_letters(true, &pets, &grounds);
    assert!(
        law.is_empty(),
        "{} frames: {:#?}",
        law.len(),
        &law[..law.len().min(8)]
    );
    assert!(
        !control.is_empty(),
        "negative control: the pet at the station beside the caret lies on the letters"
    );
}

/// review9's finding 10, the preview's half: THE COMPANION KEEPS OFF A LETTERED ROW ABOVE
/// THE PROMPT ([`scene_pet_seat`]) when the row above is written across, so no seat along
/// the prompt's row clears it (descenders, `gjpqy`, and a sentence): the resident cat's and
/// dog's drawn texels lie on no lettered cell and not on the caret's at any phase of the
/// scene. Negative control (`pet_seat_law` off): stood at the station beside the caret, its
/// head lies on the row above's letters.
#[test]
fn the_preview_companion_keeps_off_a_row_above_written_across() {
    let cell = (8, 17);
    let on_letters = |seat_law: bool| {
        SCENE_CONTROLS.with(|controls| {
            controls.set(SceneControls {
                pet_seat_law: seat_law,
                ..controls.get()
            });
        });
        let mut on = Vec::new();
        for raw in ["cat", "dog"] {
            for above in ["gjpqy ", "the quick brown fox jumps over the lazy dog "] {
                for phase in [0_u64, 300, 700, 1_100, 1_500, 1_900, 2_300] {
                    let spec = SettingsPreviewSpec::cursor(beside(raw))
                        .with_phase(phase)
                        .normalized();
                    let (base, _) = build_terminal_specimen_input(&spec, Theme::default());
                    let mut base = RenderInput::clone(&base);
                    let row = CursorScene::new(&spec, Theme::default(), None)
                        .decorate(&base, cell)
                        .cursor_row;
                    assert!(row >= 1, "{raw}: the prompt has a row above it");
                    let letters: Vec<char> = above.chars().collect();
                    for (i, c) in base.cells[row - 1].iter_mut().enumerate() {
                        c.ch = letters[i % letters.len()];
                    }
                    let input =
                        CursorScene::new(&spec, Theme::default(), None).decorate(&base, cell);
                    let pet = sprites(&input)
                        .into_iter()
                        .max_by_key(|sprite| u32::from(sprite.w) * u32::from(sprite.h))
                        .unwrap_or_else(|| panic!("{raw} at {phase} ms draws its companion"));
                    let caret = (input.cursor_row, input.cursor_col);
                    let lettered: Vec<(usize, usize)> = inked_cells(&pet, cell)
                        .into_iter()
                        .filter(|&(r, c)| {
                            (r, c) == caret
                                || input
                                    .cells
                                    .get(r)
                                    .and_then(|cells| cells.get(c))
                                    .is_some_and(|cell| !matches!(cell.ch, ' ' | '\0'))
                        })
                        .collect();
                    if !lettered.is_empty() {
                        on.push(format!("{raw} under {above:?} at {phase} ms: {lettered:?}"));
                    }
                }
            }
        }
        on
    };
    let control = on_letters(false);
    let law = on_letters(true);
    assert!(law.is_empty(), "{law:#?}");
    assert!(
        !control.is_empty(),
        "negative control: stood at the station, the pet never met the row above's letters"
    );
}

#[test]
fn a_one_row_card_holds_every_companion_whole() {
    let cell = (8, 17);
    for (raw, phases) in [
        ("cat", &[0_u64, 700, 1_500, 2_300][..]),
        ("dog", &[0, 700, 1_500, 2_300][..]),
    ] {
        for &phase in phases {
            let spec = SettingsPreviewSpec::cursor(beside(raw))
                .with_phase(phase)
                .normalized();
            let body = |rows: Option<usize>| {
                let (base, _) = build_terminal_specimen_input_rows(&spec, Theme::default(), rows);
                let input = CursorScene::new(&spec, Theme::default(), rows).decorate(&base, cell);
                let sprite = sprites(&input)
                    .into_iter()
                    .max_by_key(|sprite| u32::from(sprite.w) * u32::from(sprite.h))
                    .unwrap_or_else(|| panic!("{raw} at {phase} ms is drawn"));
                (input.rows, sprite)
            };
            let at = format!("{raw} at {phase} ms");
            let (rows, short) = body(Some(1));
            assert_eq!(rows, 2, "{at}: the prompt row and the blank row below it");
            assert!(
                short.y >= 0 && short.y + i32::from(short.h) <= 17,
                "{at}: whole in one row: y {} h {}",
                short.y,
                short.h
            );
            assert!(short.h >= 16, "{at}: not shrunk to a speck: h {}", short.h);
            let (rows, roomy) = body(None);
            assert!(rows > 2, "{at}");
            assert!(
                roomy.h > 17,
                "{at}: with room, it keeps its own size: {}",
                roomy.h
            );
        }
    }
}

fn scene_streams(input: &RenderInput) -> String {
    format!(
        "{:?}{:?}{:?}{:?}{:?}{:?}{:?}{:?}{:?}",
        input.cursor_glow_add,
        input.glow_under,
        input.glow_halo,
        input.fire_patch,
        input.char_fg,
        input.fire_halo,
        input.cursor_trail,
        input.cursor_fill_override,
        sprites(input),
    )
}

#[test]
fn a_scene_frame_is_the_same_however_the_frames_arrive() {
    for raw in ["fire", "rainbow", "comet"] {
        let at = |phase: u64| {
            played(
                &SettingsPreviewSpec::cursor(rainbow_cursor(raw)).with_phase(phase),
                Theme::default(),
            )
        };
        let alone = scene_streams(&at(2_340));
        let _ = played(
            &SettingsPreviewSpec::cursor(rainbow_cursor("water")).with_phase(100),
            Theme::default(),
        );
        let _ = at(900);
        let _ = at(1_716);
        assert_eq!(scene_streams(&at(2_340)), alone, "{raw}");
    }
}

#[test]
fn the_fire_preview_shows_the_sustained_blaze() {
    let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
        blink: false,
        trail_style: PreviewTrailStyle::Fire,
        trail_style_raw: Arc::from("fire"),
        ..CursorPreviewSpec::default()
    });
    let heat = |input: &RenderInput| {
        input
            .fire_patch
            .iter()
            .map(|patch| (patch.temp, patch.peak_h))
            .max()
            .unwrap_or((0, 0))
    };
    let warm: Vec<(u8, u16)> = (0..CURSOR_SCENE_CYCLE_MS)
        .step_by(KEY_MS as usize)
        .map(|phase| heat(&played(&spec.clone().with_phase(phase), Theme::default())))
        .collect();
    let spec = spec.normalized();
    let (base, _) = build_terminal_specimen_input(&spec, Theme::default());
    let scene = CursorScene::new(&spec, Theme::default(), None);
    let row = prompt_row(&base);
    let base = scroll_submitted_line(&base, row);
    let geom = Geom {
        cw: 8,
        ch: 17,
        rows: base.rows,
        cols: base.cols,
        origin_x: 0,
        origin_y: 0,
        win_w: (base.cols * 8) as u16,
        win_h: (base.rows * 17) as u16,
        head: 0,
    };
    let stage = SceneStage {
        base: &base,
        scrolled: &base,
        row,
        line: TypedLine::of(&base.cells[row]),
        geom,
        cursor_style: base.cursor_style,
        ink: None,
        renderer: None,
        without_body_under: std::cell::RefCell::new(None),
    };
    let mut player = ScenePlayer::new(0, spec.post_fx.companion_look);
    let mut cold = Vec::new();
    for at in (0..CURSOR_SCENE_CYCLE_MS).step_by(MOTION_STEP_MS as usize) {
        scene.step(&mut player, &stage, at, true);
        if at.is_multiple_of(KEY_MS) {
            cold.push(
                player
                    .step
                    .fire_patch
                    .iter()
                    .map(|patch| (patch.temp, patch.peak_h))
                    .max()
                    .unwrap_or((0, 0)),
            );
        }
    }
    // The typed keys' blaze, as the checks below read it: at the last key the cursor's
    // lead cell can stand past the end of the line above, and a flame there rises into
    // the air, whose floor is as hot from a cold start (the PowerShell line, a column
    // shorter than the Mac's, burns 183 there both ways).
    let keys = CURSOR_SCENE_KEYS as usize;
    let hottest = |heat: &[(u8, u16)]| heat[1..keys].iter().map(|(temp, _)| *temp).max();
    // Up to the Tab the loop types into a forge it has kept hot; the Tab's leap is a
    // meteor whose strike heats even a cold forge for a moment, so the sustained blaze is
    // read on the keys before it.
    let typed = |heat: &[(u8, u16)]| {
        heat[1..=CURSOR_SCENE_TAB.0]
            .iter()
            .map(|(temp, _)| *temp)
            .max()
    };
    let (warmest, coldest_typing) = (typed(&warm).unwrap_or(0), typed(&cold).unwrap_or(0));
    assert!(
        hottest(&warm).unwrap_or(0) >= 120,
        "the blaze reaches orange metal: {warm:?}"
    );
    assert!(
        warmest > coldest_typing + 10,
        "the typed line burns warmer than a cold start's: {warm:?} against {cold:?}"
    );
    assert!(
        (1..keys).all(|key| warm[key].0 + 8 >= cold[key].0),
        "{warm:?} against {cold:?}"
    );
    assert!(
        warm[1..keys]
            .iter()
            .zip(&cold[1..keys])
            .any(|(w, c)| w.1 > c.1)
    );
}

/// The light the card's scene lays behind its head at a Trail duration of `ms`, `raw`'s
/// trail on Nord, summed over `phases`: each phase decorated as the card's raster decorates
/// it (`CursorScene::decorate_with_renderer`) and drawn by the same renderer beside its bare
/// twin (the same cells with every light cleared, `settings card clean`'s), the summed
/// channel distance of every pixel some channel of which lies more than 12 levels off the
/// twin's, outside the head's box: the caret's cell and two cells either side of it, a row
/// above and below (`trail_duration_read`'s measure in the terminal). `scaled` off is the
/// negative control (`SceneControls::typed_life_scaled`).
fn card_trail_light(raw: &str, ms: u64, scaled: bool, phases: &[u64]) -> u64 {
    const HEAD_CELLS: usize = 2;
    let kept = SCENE_CONTROLS.with(std::cell::Cell::get);
    SCENE_CONTROLS.with(|controls| {
        controls.set(SceneControls {
            typed_life_scaled: scaled,
            ..kept
        });
    });
    let theme = NORD.into_theme();
    let mut spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
        blink: false,
        trail_style: PreviewTrailStyle::parse(raw),
        trail_style_raw: Arc::from(raw),
        duration_ms: ms,
        // The trail's light alone: the pet beside it is the same at every duration.
        pet: None,
        ..CursorPreviewSpec::default()
    });
    spec.terminal_theme = Some(NORD);
    let mut rend = aterm_render::Renderer::from_bytes(aterm_render::embedded_font(), 14.0, theme)
        .expect("embedded renderer");
    rend.set_runtime_font_discovery(false);
    let (cw, ch) = rend.cell_size();
    let mut total = 0;
    for &phase in phases {
        let spec = spec.clone().with_phase(phase).normalized();
        let (base, _) = build_terminal_specimen_input(&spec, theme);
        let lit = CursorScene::new(&spec, theme, None).decorate_with_renderer(&base, &mut rend);
        let mut bare = lit.clone();
        bare.clear_overlays();
        let (on, off) = (rend.render_input(&lit), rend.render_input(&bare));
        let place = rend.fx_frame(on.width, on.height, lit.rows);
        let (cx0, cy0) = (
            place.pad + lit.cursor_col * cw,
            place.grid_top + lit.cursor_row * ch,
        );
        let (x0, x1) = (
            cx0.saturating_sub(HEAD_CELLS * cw),
            cx0 + (HEAD_CELLS + 1) * cw,
        );
        let (y0, y1) = (cy0.saturating_sub(ch), cy0 + 2 * ch);
        total += on
            .pixels
            .iter()
            .zip(&off.pixels)
            .enumerate()
            .filter(|&(i, _)| {
                let (x, y) = (i % on.width, i / on.width);
                !((x0..x1).contains(&x) && (y0..y1).contains(&y))
            })
            .map(|(_, (&p, &c))| {
                let d = [16u32, 8, 0].map(|sh| ((p >> sh) & 0xff).abs_diff((c >> sh) & 0xff));
                if d.iter().any(|&v| v > 12) {
                    u64::from(d.iter().sum::<u32>())
                } else {
                    0
                }
            })
            .sum::<u64>();
    }
    SCENE_CONTROLS.with(|controls| controls.set(kept));
    total
}

/// TRAIL DURATION SHOWS ON THE CARD (access r3's must_fix, r14 PV7: "a knob whose change
/// is near-invisible reads as broken"): the Cursor card's scene on Nord, summed over its
/// motion from 300 ms to 2100 ms as access measured it, lays at least a fifth more light
/// behind its head at a Trail duration of 1200 ms than at the default 260 ms, and at least
/// a twentieth less at 60 ms, for every style that reads the knob (`TrailKnob::Duration`:
/// all but the Rainbow Kitty's, whose row says its ribbon follows the typing; Lumen lays
/// nothing behind its head at 260 ms or 60 ms there, and its 1200 ms trail behind it).
/// Before, a chained key's light lived as long as the key gap needed, whatever the
/// duration, and Lumen, Phaser and Sparkle laid within 5% at both. Negative control
/// (`SceneControls::typed_life_scaled` off, the lives before): one of Lumen, Phaser and
/// Sparkle lays under a fifth more at 1200 ms.
#[test]
fn the_card_shows_trail_duration() {
    use aterm_effects::cursor_glow::{KnobContext, TrailKnob};
    let phases: Vec<u64> = (1..=7).map(|k| k * 300).collect();
    let styles: Vec<&str> = crate::prefs::CURSOR_TRAIL_STYLES
        .iter()
        .copied()
        .filter(|raw| {
            crate::app_config::resolve_trail_style(raw)
                .style
                .is_some_and(|glow| {
                    glow.reads_knob(TrailKnob::Duration, KnobContext::widest(false))
                })
        })
        .collect();
    assert!(
        styles.len() >= 9,
        "the styles that read Trail duration: {styles:?}"
    );
    let read: Vec<(&str, [u64; 3])> = aterm_spec::check::each(&styles, |&raw| {
        (
            raw,
            [60, 260, 1_200].map(|ms| card_trail_light(raw, ms, true, &phases)),
        )
    });
    let old: Vec<(&str, u64, u64)> =
        aterm_spec::check::each(&["lumen", "phaser", "sparkle"], |&raw| {
            (
                raw,
                card_trail_light(raw, 260, false, &phases),
                card_trail_light(raw, 1_200, false, &phases),
            )
        });
    eprintln!("the card's trail light at 60, 260 and 1200 ms: {read:?}; before: {old:?}");
    for &(raw, [short, default, long]) in &read {
        assert!(
            long > 0,
            "fixture: {raw} lights the card behind its head at 1200 ms"
        );
        assert!(
            long * 5 >= default * 6,
            "{raw}: the card lays {long} of trail light at 1200 ms against {default} at \
             260 ms, under a fifth more"
        );
        assert!(
            short * 20 <= default * 19,
            "{raw}: the card lays {short} of trail light at 60 ms against {default} at \
             260 ms, under a twentieth less"
        );
    }
    assert!(
        old.iter().any(|&(_, default, long)| long * 5 < default * 6),
        "negative control: the old lives lay a fifth more at 1200 ms in each: {old:?}"
    );
}

fn card_pixels(spec: SettingsPreviewSpec, height: f32) -> Vec<u8> {
    let _ = compiled(SettingsPreviewSpec::default());
    let prepared =
        crate::tray_raster::prepared_semantic_font_for_direct_view_test(&spec.font_candidate);
    let tree = UiTree::new(
        UiNode::new(
            "root",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
        )
        .layout(Layout::column().padding(Insets::all(10.0)))
        .children(vec![preview_node(
            "preview",
            spec.with_prepared_font(prepared),
            height,
        )]),
    )
    .compile(VIEWPORT)
    .expect("preview compiles");
    raster_of(&tree.tray(Theme::default(), 13.0).prims)
}

/// How many pixels of two rasters differ by more than `levels` in some channel. A run of
/// pixels alike in both is passed over by one slice compare (a memcmp, quick even in a debug
/// build), so only the few pixels a knob changes are read one by one: read pixel by pixel,
/// two whole cards cost more than rendering them.
fn apart_by(left: &[u8], right: &[u8], levels: u8) -> usize {
    const RUN: usize = 64 * 4;
    left.chunks(RUN)
        .zip(right.chunks(RUN))
        .filter(|(a, b)| a != b)
        .map(|(a, b)| {
            a.as_chunks::<4>()
                .0
                .iter()
                .zip(b.as_chunks::<4>().0)
                .filter(|(x, y)| x.iter().zip(y.iter()).any(|(p, q)| p.abs_diff(*q) > levels))
                .count()
        })
        .sum()
}

/// THE SCENE'S JUMPS SHOW EVERY JUMP KNOB THE TERMINAL WOULD (audit r14 PV7, r14b PV2-1,
/// r17 CA4): on a dark and a light ground, for every style, Trail length (2 against a length
/// past the scene's reach), Ring on jumps and Phaser's accent paint the card visibly apart on
/// the frames after the Return or the Tab exactly where the engine says the scene's jumps
/// draw them (`GlowStyle::jump_reach`, `GlowStyle::reads_knob`), a ring as a mark of its own,
/// alike while only typing, and a knob that shows on the Tab shows on the frame a pause
/// holds. Before, the scene's only completion was a 6-cell hop over letters, which Fire's
/// flame leaves dark (its Length never showed) and the header's reach ignored the Return;
/// and on dark the landing light of Lumen, Comet, Sparkle, Phaser and Laser was a veil
/// alone, which a person toggling Ring on jumps could not see.
#[test]
fn the_scenes_jumps_show_every_jump_knob() {
    use aterm_effects::cursor_glow::TrailKnob;

    /// The least of the card a landing ring changes by more than 32 levels: twice the
    /// most a veil alone changed (19 to 30 px). Classic's paper ring, whole and off the
    /// letters since looks10 L12, hugs the caret inside its own row after the Return
    /// (83 px in the 190pt card), where laid over the typed line it changed 160 or more.
    const RING_SHOWS: usize = 64;

    let light = PreviewTerminalTheme::from_scheme(
        &aterm_types::scheme::builtin("GitHub Light").expect("built-in light theme"),
    );
    let tab_ms = (CURSOR_SCENE_TAB.0 as u64 + 1) * KEY_MS;
    // The frames after the Return and after the Tab, then those while only typing: each
    // knob setting's frames are rendered in this, the scene's own order, so the scene's one
    // player plays on from the frame before rather than from the scene's start, and every
    // frame is the one the card draws at that phase. Each jump's run past the ring a Fire
    // meteor strikes on landing, some 160 ms after the hop (`METEOR_STRIKE_FRAC`): in the
    // one-row card the Return's caret leaves from a line scrolled off the card's top, so
    // Fire's ring there is the Tab's.
    let after: Vec<u64> = [
        MOTION_STEP_MS,
        2 * MOTION_STEP_MS,
        4 * MOTION_STEP_MS,
        5 * MOTION_STEP_MS,
    ]
    .into_iter()
    .chain((0..5).map(|step| tab_ms + step * MOTION_STEP_MS))
    .collect();
    let typing_only: Vec<u64> = vec![6 * KEY_MS, CURSOR_SCENE_TAB.0 as u64 * KEY_MS];
    let mut phases: Vec<u64> = after.iter().chain(&typing_only).copied().collect();
    phases.sort_unstable();
    assert!(after.is_sorted() && phases.windows(2).all(|pair| pair[0] < pair[1]));
    let one_row = {
        let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec::default()).normalized();
        spec.header_layout(VIEWPORT.width - 20.0).height + 15.0 + spec.typography_row_height()
    };
    // Each style on each ground is one item: the scene's player, the card's renderers and its
    // fonts are each thread's own, so a worker plays an item's knob settings in the order the
    // lone loop did.
    let items: Vec<(&str, Option<PreviewTerminalTheme>)> = crate::prefs::CURSOR_TRAIL_STYLES
        .iter()
        .flat_map(|&raw| [(raw, None), (raw, Some(light))])
        .collect();
    let checked: usize = aterm_spec::check::each(&items, |&(raw, ground)| {
        let style = PreviewTrailStyle::parse(raw);
        let Some(glow) = crate::app_config::resolve_trail_style(raw).style else {
            return 0;
        };
        let heights = match ground {
            None => vec![190.0_f32, one_row],
            Some(_) => vec![190.0],
        };
        let mut checked = 0_usize;
        let base = CursorPreviewSpec {
            blink: false,
            trail_style: style,
            trail_style_raw: Arc::from(raw),
            ..CursorPreviewSpec::default()
        };
        let on_ground = |cursor: CursorPreviewSpec| {
            let spec = SettingsPreviewSpec::cursor(cursor);
            match ground {
                Some(theme) => spec.with_terminal_theme(theme),
                None => spec,
            }
        };
        let scene = on_ground(base.clone());
        let ctx = scene.scene_knob_context();
        let reach = scene.scene_length_reach();
        let long = reach.map_or(512, |reach| reach.ceil() as usize + 8);
        let knobs = [
            (
                TrailKnob::Length,
                reach.is_some(),
                CursorPreviewSpec {
                    length: 2,
                    ..base.clone()
                },
                CursorPreviewSpec {
                    length: long,
                    ..base.clone()
                },
            ),
            (
                TrailKnob::Ring,
                glow.reads_knob(TrailKnob::Ring, ctx),
                CursorPreviewSpec {
                    ring: true,
                    ..base.clone()
                },
                CursorPreviewSpec {
                    ring: false,
                    ..base.clone()
                },
            ),
            (
                TrailKnob::Accent,
                glow.reads_knob(TrailKnob::Accent, ctx),
                CursorPreviewSpec {
                    accent: Some(0x00FF_0000),
                    ..base.clone()
                },
                CursorPreviewSpec {
                    accent: Some(0x0000_FFFF),
                    ..base.clone()
                },
            ),
        ];
        for (knob, law, a, b) in knobs {
            if knob == TrailKnob::Accent && glow != GlowStyle::Phaser {
                continue;
            }
            for &height in &heights {
                // One knob setting's frames at `phases`, played in order.
                let frames = |cursor: &CursorPreviewSpec| {
                    phases
                        .iter()
                        .map(|&phase| {
                            let shown = on_ground(cursor.clone()).with_phase(phase);
                            (phase, card_pixels(shown, height))
                        })
                        .collect::<std::collections::BTreeMap<u64, Vec<u8>>>()
                };
                let (on_a, on_b) = (frames(&a), frames(&b));
                let pairs: Vec<(&Vec<u8>, &Vec<u8>)> = after
                    .iter()
                    .map(|phase| (&on_a[phase], &on_b[phase]))
                    .collect();
                let apart = pairs
                    .iter()
                    .map(|(x, y)| apart_by(x, y, 32))
                    .max()
                    .unwrap_or(0);
                let apart16 = pairs
                    .iter()
                    .map(|(x, y)| apart_by(x, y, 16))
                    .max()
                    .unwrap_or(0);
                let side = if ground.is_some() { "light" } else { "dark" };
                if law {
                    assert!(
                        apart >= 12 && apart16 >= 40,
                        "{raw} {side}: {knob:?} shows after a jump in a {height}pt card: \
                         {apart} px more than 32 levels apart, {apart16} more than 16"
                    );
                    // A ring is a mark of its own, not a tint: Classic's square and
                    // Water's ripples change some 160 to 270 px of the card (Classic's
                    // paper square, kept off the typed line, 83), Fire's flash 117,
                    // where the veil alone of Lumen, Comet, Sparkle, Phaser and Laser
                    // on dark changed 19 to 30 and the knob read as broken.
                    // Those five's dark rim shows whole or not at all, kept off the
                    // letters the frame holds (the letter guard's `RIM_SHOWN_SHARE`):
                    // the Return lands among the letters of what the shell printed, where
                    // the terminal draws no rim either (the trail gallery's
                    // `enter-bottom`), and the Tab lands with room, under the printed
                    // line's end, where it draws. The floor holds in every card, on both
                    // grounds. The shortest card's three rows lose the Return's line to
                    // the scroll, and from an origin off the grid the trail draws no
                    // landing (`note_scroll`, the terminal's own law), so its one landing
                    // is the Tab's, the least ring a jump draws (impact 1.0): every style
                    // changes 118 to 272 px there, and Water's ripple, the thinnest ring,
                    // 96 by more than 32 levels and 147 by more than 16, so Water's least
                    // ring is held to the floor by 16 levels.
                    let thin = glow == aterm_effects::cursor_glow::GlowStyle::Water;
                    assert!(
                        knob != TrailKnob::Ring
                            || apart >= RING_SHOWS
                            || (thin && apart16 >= RING_SHOWS),
                        "{raw} {side}: Ring on jumps draws a ring in a {height}pt card: \
                         {apart} px more than 32 levels apart ({apart16} more than 16), \
                         under {RING_SHOWS}"
                    );
                    for phase in &typing_only {
                        assert_eq!(
                            apart_by(&on_a[phase], &on_b[phase], 32),
                            0,
                            "{raw} {side}: {knob:?} is invisible while only typing \
                             ({phase} ms)"
                        );
                    }
                } else {
                    assert!(
                        apart < 12,
                        "{raw} {side}: {knob:?} the engine says the scene cannot show \
                         paints {apart} px apart"
                    );
                }
                checked += 1;
            }
        }
        // What the Tab shows, the frame a pause holds shows.
        let held = |cursor: CursorPreviewSpec| {
            card_pixels(on_ground(cursor).with_phase(300).with_paused(true), 190.0)
        };
        let tab = scene_jumps()[0];
        let (cell_w, cell_h) = scene.demo_cell_size();
        let tab_reach = glow.jump_reach(
            tab.rows,
            tab.cols,
            cell_h / cell_w.max(1.0),
            aterm_effects::cursor_glow::KnobContext {
                over_text: tab.over_letters,
                ..ctx
            },
        );
        if tab_reach.is_some_and(|reach| reach > 2.0) {
            let (short, long) = (
                held(CursorPreviewSpec {
                    length: 2,
                    ..base.clone()
                }),
                held(CursorPreviewSpec {
                    length: long,
                    ..base.clone()
                }),
            );
            let (apart, apart16) = (apart_by(&short, &long, 32), apart_by(&short, &long, 16));
            // The held frame is the Tab's next step, where the longer trail's smoke lies
            // over the `m` and the `d` the Tab completed: the letter guard dims it there
            // to keep each letter's core, as the card's face lays it, at its bar
            // (`CursorGlow::observe_glyph_ink`). Fire's dark smoke then changes 67 px
            // by more than 16 levels and 4 by more than 32 (22 with every glyph read
            // as its colour whole), so the held frame is held to either of the two
            // floors the Tab's frames meet.
            assert!(
                apart >= 12 || apart16 >= 40,
                "{raw}: Trail length shows on the paused frame: {apart} px more than 32 \
                 levels apart, {apart16} more than 16"
            );
        }
        checked
    })
    .into_iter()
    .sum();
    assert!(checked >= 40, "{checked} knobs checked");
}

#[test]
fn the_scenes_return_scrolls_the_line_up_with_its_light() {
    let cw = 8_i32;
    // The light's reach along `row`, its dust's motes (halos narrower than a cell) left out
    // when `motes` is false: a mote of the submitted line's dust drifting down past the
    // line's end is no sweep along the row (Comet's dark dust, looks8 round 3's K4).
    let reach_with = |input: &RenderInput, row: usize, motes: bool| -> i32 {
        let quads = input
            .cursor_glow_add
            .iter()
            .chain(&input.glow_under)
            .filter(|quad| usize::from(quad.row) == row && quad.w > 0 && quad.h > 0)
            .map(|quad| i32::from(quad.x) + i32::from(quad.w));
        let halos = input
            .glow_halo
            .iter()
            .filter(|halo| usize::from(halo.row) == row && halo.w > 0)
            .filter(|halo| motes || 2 * i32::from(halo.rx) >= cw)
            .map(|halo| i32::from(halo.x) + i32::from(halo.w));
        let trail = input
            .cursor_trail
            .iter()
            .filter(|cell| cell.row == row && cell.alpha > 0)
            .map(|cell| (cell.col as i32 + 1) * cw);
        quads.chain(halos).chain(trail).max().unwrap_or(0)
    };
    let reach = |input: &RenderInput, row: usize| reach_with(input, row, true);
    let text = |cells: &[aterm_core::terminal::RenderCell]| -> String {
        cells.iter().map(|cell| cell.ch).collect()
    };
    for raw in ["rainbow", "comet"] {
        let cursor = CursorPreviewSpec {
            blink: false,
            trail_style: PreviewTrailStyle::parse(raw),
            trail_style_raw: Arc::from(raw),
            ..CursorPreviewSpec::default()
        };
        let at = |phase: u64| {
            played(
                &SettingsPreviewSpec::cursor(cursor.clone()).with_phase(phase),
                Theme::default(),
            )
        };
        let (before, after) = (
            at(CURSOR_SCENE_CYCLE_MS - MOTION_STEP_MS),
            at(MOTION_STEP_MS),
        );
        let row = after.cursor_row;
        let up = usize::from(CURSOR_SCENE_RETURN_ROWS);
        assert!(
            row >= up && before.cursor_row == row,
            "{raw}: one prompt row"
        );
        assert!(
            text(&after.cells[row - up]).contains(CURSOR_SCENE_COMMAND),
            "{raw}: the line just typed is over what the shell printed: {:?}",
            text(&after.cells[row - up])
        );
        assert!(
            text(&after.cells[row - 1]).starts_with(CURSOR_SCENE_OUTPUT),
            "{raw}: the shell's line is over the fresh prompt: {:?}",
            text(&after.cells[row - 1])
        );
        let line_end = before.cursor_col as i32 * cw - 2 * cw;
        let caret_x = after.cursor_col as i32 * cw;
        assert!(
            reach(&before, row) >= line_end && reach(&before, row + 1) >= line_end,
            "{raw}, control: the light reaches the typed line's end before the Return"
        );
        if raw == "comet" {
            assert!(
                reach(&after, row - up) >= line_end - 4 * cw,
                "comet: the Return's streak leaves the line's end, where it scrolled: {} px",
                reach(&after, row - up)
            );
            assert!(
                reach_with(&after, row, false) < line_end,
                "comet: no sweep back along the prompt's row: {} px",
                reach_with(&after, row, false)
            );
        } else {
            assert!(
                reach(&after, row + 1) <= caret_x + 4 * cw,
                "{raw}: under the fresh prompt light reaches {} px, the caret at {caret_x}",
                reach(&after, row + 1)
            );
        }
    }
}

#[test]
fn the_preview_blooms_with_the_live_kernel_below_the_prompt_too() {
    let _ = compiled(SettingsPreviewSpec::default());
    let prepared = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
        &SemanticFontCandidate::default(),
    );
    let theme = PreviewTerminalTheme::from_scheme(&aterm_types::ColorScheme::default());
    let render = |bloom: bool| {
        let mut spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            blink: false,
            trail_style: PreviewTrailStyle::Water,
            trail_style_raw: Arc::from("water"),
            ..CursorPreviewSpec::default()
        })
        .with_terminal_theme(theme)
        .with_phase(2_340)
        .with_prepared_font(prepared.clone());
        spec.post_fx.bloom = bloom;
        spec.post_fx.bloom_strength = aterm_render::bloom::BLOOM_STRENGTH;
        spec.post_fx.sdr_boost = 0.0;
        let spec = spec.normalized();
        let specimen = spec.terminal_specimen(theme.into_theme(), Some(2));
        let cell = crate::tray_raster::specimen_cell_size_for_test(&specimen, 1.0)
            .expect("the fixture renderer forks");
        let pixels = crate::tray_raster::rasterize_tray(
            &[DrawPrim::TerminalSpecimen {
                x: 0.0,
                y: 0.0,
                spec: Box::new(specimen),
            }],
            400,
            80,
            1.0,
            [0, 0, 0, 255],
        )
        .0;
        (pixels, cell)
    };
    let ((lit, (_, cell_h)), (bare, _)) = (render(true), render(false));
    assert_ne!(lit, bare, "the bloom adds light");
    let gain_below: u32 = lit
        .chunks(400 * 4)
        .zip(bare.chunks(400 * 4))
        .skip(2 * cell_h)
        .take(cell_h)
        .flat_map(|(l, b)| l.iter().zip(b))
        .map(|(l, b)| u32::from(l.saturating_sub(*b)))
        .sum();
    assert!(gain_below > 0, "light below the prompt row");
    let (off_a, _) = render(false);
    assert_eq!(off_a, bare);
}

/// Composed r2 B1: the Cursor preview's Bloom size lights more of the card the larger it
/// is, and its Bloom strength changes the card, under the gate that spares the letters:
/// the bloom keeps off the letters at its composite (`bloom::LetterMask`), so the card's
/// dense lines leave it a source to blur at every radius. NEGATIVE CONTROL
/// (`bloom::with_source_cut_by_reach`): the source cut a whole blur's reach off the
/// letters, as looks10 kept it, lit the card no more at a larger size.
#[test]
fn the_preview_s_bloom_size_and_strength_change_the_card() {
    let _ = compiled(SettingsPreviewSpec::default());
    let prepared = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
        &SemanticFontCandidate::default(),
    );
    let theme = PreviewTerminalTheme::from_scheme(&aterm_types::ColorScheme::default());
    let (w, h) = (640usize, 160usize);
    let render = |bloom: bool, radius: f32, strength: f32, phase: u64| {
        let mut spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            blink: false,
            ..CursorPreviewSpec::default()
        })
        .with_terminal_theme(theme)
        .with_phase(phase)
        .with_prepared_font(prepared.clone());
        spec.post_fx.bloom = bloom;
        spec.post_fx.bloom_strength = strength;
        spec.post_fx.bloom_radius = radius;
        spec.post_fx.sdr_boost = 0.0;
        let spec = spec.normalized();
        let specimen = spec.terminal_specimen(theme.into_theme(), None);
        crate::tray_raster::rasterize_tray(
            &[DrawPrim::TerminalSpecimen {
                x: 0.0,
                y: 0.0,
                spec: Box::new(specimen),
            }],
            w as u32,
            h as u32,
            1.0,
            [0, 0, 0, 255],
        )
        .0
    };
    let phases = [1_400_u64, 2_340, 3_100];
    let lit_at = |radius: f32| -> usize {
        phases
            .iter()
            .map(|&phase| {
                let (lit, bare) = (
                    render(true, radius, aterm_render::bloom::BLOOM_STRENGTH, phase),
                    render(false, radius, aterm_render::bloom::BLOOM_STRENGTH, phase),
                );
                lit.chunks(4)
                    .zip(bare.chunks(4))
                    .filter(|(l, b)| l[..3].iter().zip(&b[..3]).any(|(l, b)| l > b))
                    .count()
            })
            .sum()
    };
    let radii = [0.5f32, aterm_render::bloom::BLOOM_RADIUS, 8.0];
    let lit: Vec<usize> = radii.iter().map(|&r| lit_at(r)).collect();
    assert!(
        lit[0] > 0 && lit[0] < lit[1] && lit[1] < lit[2],
        "the card's lit area grows with the Bloom size: {lit:?} at {radii:?}"
    );
    let r = aterm_render::bloom::BLOOM_RADIUS;
    assert_ne!(
        render(true, r, 0.4, phases[1]),
        render(true, r, 1.2, phases[1]),
        "the Bloom strength changes the card"
    );
    let cut: Vec<usize> = aterm_render::bloom::with_source_cut_by_reach(|| {
        radii.iter().map(|&r| lit_at(r)).collect()
    });
    assert!(
        !(cut[0] < cut[1] && cut[1] < cut[2]),
        "control: the source cut by the blur's reach lights the card no more at a larger size: {cut:?}"
    );
}

#[test]
fn the_water_preview_draws_its_wake_behind_the_drop() {
    let light = PreviewTerminalTheme::from_scheme(
        &aterm_types::scheme::builtin("GitHub Light").expect("built-in light theme"),
    );
    for (name, theme) in [
        ("Default", Theme::default()),
        ("GitHub Light", light.into_theme()),
    ] {
        let reach = [2_652_u64, 2_756, 2_964, 3_172]
            .into_iter()
            .map(|phase| {
                let input = played(
                    &SettingsPreviewSpec::cursor(CursorPreviewSpec {
                        blink: false,
                        trail_style: PreviewTrailStyle::Water,
                        trail_style_raw: Arc::from("water"),
                        ..CursorPreviewSpec::default()
                    })
                    .with_phase(phase),
                    theme,
                );
                let row = input.cursor_row as u16;
                let caret_x = input.cursor_col as i32 * 8;
                input
                    .glow_under
                    .iter()
                    .chain(&input.cursor_glow_add)
                    .filter(|quad| quad.row == row)
                    .filter(|quad| {
                        let c = quad.color;
                        ((c >> 16) & 0xff).max((c >> 8) & 0xff).max(c & 0xff) >= 24
                            || quad.alpha >= 24
                    })
                    .map(|quad| caret_x - i32::from(quad.x))
                    .max()
                    .unwrap_or(0)
            })
            .max()
            .unwrap_or(0);
        assert!(
            reach >= 3 * 8,
            "{name}: the wake reaches {reach} px behind the drop"
        );
    }
}

#[test]
fn a_trail_length_the_scenes_jumps_cannot_show_says_so() {
    let focused = |raw: &'static str, length: usize| {
        SettingsPreviewSpec::cursor(CursorPreviewSpec {
            trail_style: PreviewTrailStyle::parse(raw),
            trail_style_raw: Arc::from(raw),
            length,
            ..CursorPreviewSpec::default()
        })
        .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_LENGTH, length.to_string())
        .with_phase(900)
        .normalized()
    };
    // Comet's Return lays the whole line it leaves, a cell a column; Fire flies the Tab's
    // leap as a meteor, and the Return's, steep, shorter; Phaser lays nothing off its row,
    // so only the Tab shows its length, short of the cell it leaves.
    let line = CURSOR_SCENE_COMMAND.len();
    let tab = CURSOR_SCENE_TAB.1 - CURSOR_SCENE_TAB.0;
    for (raw, reach) in [("comet", line), ("fire", tab), ("phaser", tab - 1)] {
        let long = focused(raw, reach);
        let says = format!("Preview shows up to {reach} cells");
        assert_eq!(long.live_state(), says, "{raw}");
        let spoken = long
            .clone()
            .with_focus_label("Trail length")
            .semantic_value();
        assert!(
            spoken.contains(&format!("Trail length · {says}.")),
            "{raw}: {spoken}"
        );
        assert!(
            long.header_rungs()
                .iter()
                .all(|rung| !rung.contains("Live")),
            "{raw}"
        );
        assert!(!long.reads_live(), "{raw}");
        let cut = focused(raw, reach - 1);
        assert!(cut.reads_live(), "{raw}: {}", cut.live_state());
        let spoken = cut
            .clone()
            .with_focus_label("Trail length")
            .semantic_value();
        assert!(!spoken.contains("Preview shows up to"), "{raw}: {spoken}");
    }
    let one = focused("comet", 1).with_focus_label("Trail length");
    assert!(
        one.header_rungs()[0].starts_with("Trail length: 1 cell ·"),
        "{:?}",
        one.header_rungs()
    );
}

/// The heat shimmer is the GPU's refraction over the flames, which the CPU-drawn preview
/// cannot draw: focused, the header says so instead of calling the trail live (it said
/// "Live trail" while turning the shimmer on or off changed nothing it drew).
#[test]
fn a_focused_heat_shimmer_says_the_preview_cannot_show_it() {
    let focused = SettingsPreviewSpec::cursor(CursorPreviewSpec {
        trail_style: PreviewTrailStyle::Fire,
        trail_style_raw: Arc::from("fire"),
        ..CursorPreviewSpec::default()
    })
    .with_focus(crate::prefs::EDIT_CURSOR_FIRE_SHIMMER, "true")
    .with_focus_label("Heat shimmer")
    .with_phase(900)
    .normalized();
    assert_eq!(focused.live_state(), SHIMMER_PREVIEW_CAVEAT);
    assert!(!focused.reads_live());
    assert!(
        focused
            .header_rungs()
            .iter()
            .all(|rung| !rung.contains("Live")),
        "{:?}",
        focused.header_rungs()
    );
    assert!(
        focused
            .semantic_value()
            .contains(&format!("Heat shimmer · {SHIMMER_PREVIEW_CAVEAT}.")),
        "{}",
        focused.semantic_value()
    );
}

#[test]
fn a_reduced_preview_keeps_the_looping_previews_floor() {
    use crate::native_appearance::{AppearancePreferences, install_preferences};
    let looping = SettingsPreviewSpec::cursor(rainbow_cursor("rainbow"))
        .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "rainbow")
        .with_phase(300);
    assert!(looping.pause_control(), "fixture: a looping preview");
    let mut wrapped = 0;
    for scale in [1.0_f32, 1.25, 2.0] {
        let _ = install_preferences(AppearancePreferences {
            text_scale: scale,
            ..AppearancePreferences::default()
        });
        for step in 0..=180_u16 {
            let width = 60.0 + f32::from(step) * 2.0;
            let playing = looping.clone().with_measure_width(width);
            let reduced = playing.clone().with_reduced_motion(true);
            assert!(
                !reduced.pause_control(),
                "fixture: reduced, nothing to pause"
            );
            assert_eq!(
                playing.header_layout(width).height,
                reduced.header_layout(width).height,
                "{scale}x, {width}pt: {:?} against {:?}",
                playing.header_layout(width).lines,
                reduced.header_layout(width).lines
            );
            assert_eq!(
                playing.minimum_height(48.0),
                reduced.minimum_height(48.0),
                "{scale}x, {width}pt"
            );
            wrapped += usize::from(playing.header_layout(width).lines.len() > 1);
        }
    }
    let _ = install_preferences(AppearancePreferences::default());
    assert!(
        wrapped > 0,
        "control: the sweep reaches widths where the header wraps"
    );
}

#[test]
fn a_faces_landing_is_the_same_demonstration() {
    let probe = SettingsPreviewSpec::cursor(rainbow_cursor("rainbow")).with_phase(300);
    let mut painted = probe.clone();
    painted.font_ready_epoch = 7;
    assert!(
        probe.loop_identity().is_some(),
        "fixture: a looping preview"
    );
    assert_eq!(probe.loop_identity(), painted.loop_identity());
    assert_ne!(probe.paint_fingerprint(), painted.paint_fingerprint());
    let other = SettingsPreviewSpec::cursor(beside("dog")).with_phase(300);
    assert_ne!(probe.loop_identity(), other.loop_identity());
}

#[test]
fn the_sprite_control_previews_its_kitty_and_no_style_flies_one_in() {
    let at = |pet, phase: u64| {
        played(
            &SettingsPreviewSpec::cursor(CursorPreviewSpec {
                pet,
                ..rainbow_cursor("rainbow")
            })
            .with_phase(phase),
            Theme::default(),
        )
    };
    for phase in [0, 700, 2_300] {
        assert!(
            sprites(&at(None, phase)).is_empty(),
            "with the pet off no kitty flies in at {phase} ms"
        );
        assert!(
            !sprites(&at(Some(aterm_effects::kitty_pet::PetSpecies::Cat), phase)).is_empty(),
            "negative control: the walking cat is there at {phase} ms"
        );
    }
    let diagnostic = played(
        &SettingsPreviewSpec::cursor(CursorPreviewSpec {
            pet: None,
            ..rainbow_cursor("rainbow")
        })
        .with_focus(crate::prefs::EDIT_CURSOR_NYAN_SPRITE, "")
        .with_phase(0),
        Theme::default(),
    );
    assert!(
        !sprites(&diagnostic).is_empty(),
        "the sprite control previews its kitty at once"
    );
}

#[test]
fn the_glow_is_lifted_by_hdr_glow_or_sdr_glow_boost_never_both() {
    let halo = |display_edr: bool, hdr_glow: bool, sdr_boost: f32| {
        let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            blink: false,
            trail_style: PreviewTrailStyle::Phaser,
            ..CursorPreviewSpec::default()
        })
        .with_post_fx(CursorPostFxSpec {
            display_edr,
            hdr_glow,
            sdr_boost,
            ..CursorPostFxSpec::default()
        })
        .normalized();
        let theme = Theme::default();
        let (base, _) = build_terminal_specimen_input(&spec, theme);
        let scene = CursorScene::new(&spec, theme, None);
        let input = scene.decorate(&base, (8, 17));
        let mut frame = aterm_render::Frame {
            width: 8 * input.cols,
            height: 17 * input.rows,
            pixels: vec![theme.bg; 8 * input.cols * 17 * input.rows],
        };
        let grid = aterm_render::bloom::LetterGrid {
            pad: 0,
            grid_top: 0,
            cell_w: 8,
            cell_h: 17,
        };
        scene.post_process(&mut frame, &input, grid).unwrap();
        frame.pixels
    };
    assert!(
        halo(true, true, 0.25) != halo(false, false, 0.0),
        "premise: a crown to compare"
    );
    assert_ne!(
        halo(true, true, 0.25),
        halo(true, false, 0.25),
        "EDR: HDR glow"
    );
    assert_eq!(
        halo(true, true, 0.1),
        halo(true, true, 0.9),
        "EDR: not SDR boost"
    );
    assert_eq!(
        halo(false, true, 0.5),
        halo(false, false, 0.5),
        "SDR: not HDR glow"
    );
    assert_ne!(
        halo(false, true, 0.1),
        halo(false, true, 0.9),
        "SDR: SDR boost"
    );
}

#[test]
fn resolved_custom_kitty_asset_is_drawn_even_while_the_style_is_dormant() {
    let rgba: Arc<[u8]> = Arc::from(
        [
            255, 40, 80, 255, 20, 240, 255, 255, 255, 220, 30, 255, 80, 30, 255, 255,
        ]
        .as_slice(),
    );
    let custom_asset = crate::app_config::KittySpriteAsset::Ready {
        source_id: Arc::from("test-cat.png"),
        w: 2,
        h: 2,
        rgba: Arc::clone(&rgba),
        fp: 0xCA7C_A7C0_1234_5678,
    };
    let custom = SettingsPreviewSpec::cursor(CursorPreviewSpec {
        trail_style: PreviewTrailStyle::Phaser,
        ..CursorPreviewSpec::default()
    })
    .with_focus(crate::prefs::EDIT_CURSOR_NYAN_SPRITE, "test-cat.png")
    .with_post_fx(CursorPostFxSpec {
        kitty_sprite: "test-cat.png".to_string(),
        kitty_asset: custom_asset,
        ..CursorPostFxSpec::default()
    });
    assert_eq!(custom.cursor.trail_style, PreviewTrailStyle::Phaser);
    assert!(
        custom
            .audit_value()
            .contains("shown independently in this specimen")
    );
    assert!(
        custom
            .audit_value()
            .contains("flies once Pet is Flying cat beside the Rainbow kitty trail")
    );
    let crate::app_config::KittySpriteAsset::Ready { rgba: carried, .. } =
        &custom.post_fx.kitty_asset
    else {
        unreachable!()
    };
    assert!(Arc::ptr_eq(carried, &rgba));

    let input = played(&custom, Theme::default());
    assert!(!sprites(&input).is_empty());
    assert!(
        differences(
            &pixels(custom),
            &pixels(
                SettingsPreviewSpec::cursor(CursorPreviewSpec {
                    trail_style: PreviewTrailStyle::Phaser,
                    ..CursorPreviewSpec::default()
                })
                .with_focus(crate::prefs::EDIT_CURSOR_NYAN_SPRITE, "built-in CatBaker"),
            ),
        ) > 20,
        "chosen custom sprite changes exact specimen pixels",
    );
}

#[test]
fn unknown_style_previews_the_default_trail_and_companion() {
    let cursor = CursorPreviewSpec {
        blink: false,
        trail_style: PreviewTrailStyle::parse("phasr"),
        trail_style_raw: Arc::from("phasr"),
        ..CursorPreviewSpec::default()
    };
    let default_cursor = rainbow_cursor(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE);
    assert_eq!(cursor.trail_style, default_cursor.trail_style);
    assert_eq!(cursor.trail_companion(), default_cursor.trail_companion());
    assert_eq!(
        cursor.effective_trail_style_token(),
        crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE
    );
    assert_eq!(
        cursor.effective_trail_style_raw(),
        "phasr",
        "the cache keys on the authored spelling"
    );
    assert_eq!(cursor.trail_style_label(), "Rainbow kitty");
    assert_eq!(cursor.unknown_trail_spelling(), Some("phasr"));

    let spec = SettingsPreviewSpec::cursor(cursor.clone()).with_phase(900);
    assert_ne!(spec.animation(), PreviewAnimation::None);
    assert!(!spec.audit_value().contains("fails closed"));
    assert!(has_effect_light(&played(&spec, Theme::default())));
    let spoken = spec.semantic_value();
    assert!(
        spoken.contains("Rainbow kitty trail, since aterm.toml's \"phasr\" is not a style"),
        "{spoken}"
    );
    assert!(!spoken.contains("Phasr"), "{spoken}");
    let focused = SettingsPreviewSpec::cursor(cursor)
        .with_phase(900)
        .with_focus(crate::prefs::EDIT_CURSOR_TRAIL_STYLE, "phasr");
    assert_eq!(
        focused.candidate_badge(),
        "Style: Rainbow kitty (unknown \"phasr\")"
    );

    for (alias, name) in [("comet", "Comet"), ("rainbow", "Rainbow kitty")] {
        let known = rainbow_cursor(alias);
        let known = CursorPreviewSpec {
            trail_style: PreviewTrailStyle::parse(alias),
            trail_style_raw: Arc::from(alias),
            ..known
        };
        assert_eq!(known.trail_style_label(), name, "{alias}");
        assert_eq!(known.unknown_trail_spelling(), None, "{alias}");
    }
}

#[test]
fn unresolved_font_candidate_is_acknowledged_inside_the_specimen() {
    let family = "No Such Font Family 7F31";
    let spec = SettingsPreviewSpec::typography(14.0)
        .with_font_candidate(SemanticFontCandidate {
            regular: Some(family.to_string()),
            ..SemanticFontCandidate::default()
        })
        .with_focus(crate::prefs::EDIT_FONT_FAMILY, family);
    let compiled = compiled(spec);
    let preview = compiled
        .semantic(&crate::native_ui::UiKey::new("preview"))
        .expect("preview semantics");
    let prims = compiled.tray(Theme::default(), 13.0).prims;
    let pill = prims.iter().find_map(|prim| match prim {
        DrawPrim::Text { baseline, s, .. } if s.contains("Regular:") && s.contains(family) => {
            Some((*baseline, s))
        }
        _ => None,
    });
    let (baseline, label) = pill.expect("candidate status capsule text");
    assert!(
        baseline > preview.rect.y + 33.0,
        "pill is inside the specimen body"
    );
    assert!(
        label.contains("Preparing") || label.contains("Unavailable"),
        "candidate is never misrepresented as live: {label}",
    );
    let crate::native_ui::SemanticValue::Text(value) = &preview.value else {
        panic!("preview semantic value is textual")
    };
    assert!(value.contains(family));
}

#[test]
fn a_failed_font_status_names_the_slot_that_failed() {
    let mut spec = SettingsPreviewSpec::typography(14.0)
        .with_focus(crate::prefs::EDIT_FONT_FAMILY, "")
        .with_font_candidate(SemanticFontCandidate {
            emoji: Some("NoSuchEmoji".to_string()),
            ..SemanticFontCandidate::default()
        });
    spec.font_status = "candidate fallback active: unresolved emoji \"NoSuchEmoji\": \
                        \"NoSuchEmoji\" does not resolve to a font file; requested emoji \
                        \"NoSuchEmoji\""
        .to_string();
    assert_eq!(
        spec.font_candidate_pill(),
        Some(("Not found · Emoji: NoSuchEmoji".to_string(), true))
    );
    let mut missing =
        SettingsPreviewSpec::typography(14.0).with_font_candidate(SemanticFontCandidate {
            regular: Some("NoSuchFontXYZ".to_string()),
            ..SemanticFontCandidate::default()
        });
    missing.font_status = "candidate fallback active: unresolved regular \"NoSuchFontXYZ\": \
                           \"NoSuchFontXYZ\" does not resolve to a font file; requested \
                           regular \"NoSuchFontXYZ\""
        .to_string();
    assert!(
        missing.semantic_value().starts_with(
            "Text sample in the current font, because \u{201c}NoSuchFontXYZ\u{201d} is not found"
        ),
        "{}",
        missing.semantic_value()
    );
    let missing = missing.with_focus(crate::prefs::EDIT_FONT_FAMILY, "NoSuchFontXYZ");
    assert_eq!(missing.candidate_badge(), "Font: Current font");
    let mut found = missing.clone();
    found.font_status = "candidate renderer font cascade ready".to_string();
    assert_eq!(found.candidate_badge(), "Font: NoSuchFontXYZ");
}

#[test]
fn a_ready_font_has_no_status_pill_and_a_loading_one_does_not_repeat_its_name() {
    let mut spec =
        SettingsPreviewSpec::typography(14.0).with_focus(crate::prefs::EDIT_FONT_FAMILY, "default");
    spec.font_status = "committed renderer font cascade ready".to_string();
    assert_eq!(spec.font_candidate_pill(), None);
    assert!(spec.candidate_badge().contains("Automatic"));

    spec.font_status = "candidate loading".to_string();
    assert_eq!(
        spec.font_candidate_pill().map(|(_, warning)| warning),
        Some(false),
        "the settled host font is no warning capsule"
    );
}

#[test]
fn typography_reserves_whole_candidate_rows_above_its_status_band() {
    let _ = compiled(SettingsPreviewSpec::typography(12.0));
    let prepared = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
        &SemanticFontCandidate::default(),
    );
    let theme = Theme::default();
    for (status, has_pill) in [
        ("committed renderer font cascade ready", false),
        ("candidate loading", true),
    ] {
        for (width, dpi) in [(254.0, 1.0), (592.0, 1.25), (592.0, 2.0), (900.0, 3.0)] {
            for font_px in [12.0, 24.0, 36.0, 72.0] {
                for line_height in [0.8, 1.0, 2.0] {
                    let mut spec = SettingsPreviewSpec::typography(font_px)
                        .with_focus(crate::prefs::EDIT_FONT_FAMILY, "default")
                        .with_prepared_font(prepared.clone());
                    spec.line_height = line_height;
                    spec.terminal_scale = dpi;
                    spec.font_status = status.to_string();
                    let full_height = spec.preferred_height(108.0);
                    for height in [108.0, full_height] {
                        let rect = LogicalRect::new(0.0, 0.0, width, height);
                        let mut prims = Vec::new();
                        spec.paint(&mut prims, rect, theme, Roles::from_theme(theme));
                        let pill_y = prims.iter().find_map(|prim| match prim {
                            DrawPrim::Panel { y, radius, .. } if *radius == 6.0 => Some(*y),
                            _ => None,
                        });
                        let specimen = prims.iter().find_map(|prim| match prim {
                            DrawPrim::TerminalSpecimen { y, spec, .. } => Some((*y, spec)),
                            _ => None,
                        });
                        let body = height - spec.header_height() - 15.0;
                        let row = spec.normalized().typography_row_height();
                        let Some(pill_y) = pill_y else {
                            if body - 4.0 >= row {
                                assert!(
                                    specimen.is_some_and(|(_, specimen)| specimen.input.rows >= 1),
                                    "{width}px/{height}px: {font_px}px drops its status for no row"
                                );
                            } else {
                                assert!(specimen.is_none(), "no cut row under the header");
                            }
                            continue;
                        };
                        assert!(has_pill, "a ready font paints no status pill");
                        if height == full_height {
                            assert!(
                                specimen.as_ref().unwrap().1.input.rows >= TYPOGRAPHY_KEPT_ROWS,
                                "all five intended rows fit {width}px / {font_px}px / {line_height}"
                            );
                        }
                        if let Some((y, specimen)) = specimen {
                            assert_eq!(
                                specimen.font_px,
                                spec.normalized().font_px,
                                "never shrink the authored type"
                            );
                            assert_eq!(specimen.line_height, line_height);
                            assert!(specimen.input.rows <= TYPOGRAPHY_SPECIMEN_ROWS);
                            let first: String =
                                specimen.input.cells[0].iter().map(|cell| cell.ch).collect();
                            assert!(
                                first.starts_with("ANSI 4"),
                                "a short specimen keeps the first rows, not parser-scroll tail: {first}"
                            );
                            let dpi = spec.terminal_scale.max(1.0);
                            let row_h = prepared
                                .specimen_cell_height(specimen.font_px * dpi, line_height, &[])
                                .unwrap() as f32
                                / dpi;
                            assert!(
                                y + specimen.input.rows as f32 * row_h <= pill_y - 4.0,
                                "{width}px/{height}px: {font_px}px at {dpi}dpi overlaps the badge"
                            );
                        }
                    }
                }
            }
        }
    }
    let old_terminal_h = 108.0 - 27.0 - 15.0;
    let row_h = prepared.specimen_cell_height(48.0, 1.0, &[]).unwrap() as f32 / 2.0;
    assert!(row_h * TYPOGRAPHY_SPECIMEN_ROWS as f32 > old_terminal_h - 24.0);
}

#[test]
fn a_sliver_text_preview_is_its_header_alone() {
    let _ = compiled(SettingsPreviewSpec::typography(12.0));
    let prepared = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
        &SemanticFontCandidate::default(),
    );
    let theme = Theme::default();
    let mut spec = SettingsPreviewSpec::typography(12.0)
        .with_focus(crate::prefs::EDIT_FONT_FAMILY, "default")
        .with_prepared_font(prepared);
    spec.font_status = "candidate loading".to_string();
    let paint = |height: f32| {
        let mut prims = Vec::new();
        spec.paint(
            &mut prims,
            LogicalRect::new(0.0, 0.0, 552.0, height),
            theme,
            Roles::from_theme(theme),
        );
        prims
    };
    let sliver = paint(48.0);
    assert!(sliver.iter().all(|prim| match prim {
        DrawPrim::TerminalSpecimen { .. } => false,
        DrawPrim::Panel { radius, .. } => *radius > 10.0,
        DrawPrim::Text { s, .. } => !s.contains("Loading font"),
        _ => true,
    }));
    let floor = paint(spec.minimum_height(48.0));
    assert!(
        floor
            .iter()
            .any(|prim| matches!(prim, DrawPrim::TerminalSpecimen { .. }))
    );
    assert!(floor.iter().any(|prim| matches!(
        prim,
        DrawPrim::Text { s, .. } if s == "Loading font"
    )));
}

#[test]
fn exact_candidate_font_size_and_cursor_shapes_reach_the_draw_ir() {
    let exact = SettingsPreviewSpec::typography(21.5);
    let prims = compiled(exact).tray(Theme::default(), 13.0).prims;
    let sizes = prims
        .iter()
        .filter_map(|primitive| match primitive {
            DrawPrim::TerminalSpecimen { spec, .. } => Some(spec.font_px),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(!sizes.is_empty());
    assert!(sizes.iter().all(|size| *size == 21.5));

    let minimum = compiled(SettingsPreviewSpec::typography(1.0))
        .tray(Theme::default(), 13.0)
        .prims
        .into_iter()
        .filter_map(|primitive| match primitive {
            DrawPrim::TerminalSpecimen { spec, .. } => Some(spec.font_px),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(!minimum.is_empty());
    assert!(minimum.iter().all(|size| *size == 6.0));

    let shape = |style| {
        pixels(SettingsPreviewSpec::cursor(CursorPreviewSpec {
            style,
            blink: false,
            trail_enabled: false,
            trail_style: PreviewTrailStyle::Off,
            ..CursorPreviewSpec::default()
        }))
    };
    let block = shape(PreviewCursorStyle::Block);
    let bar = shape(PreviewCursorStyle::Bar);
    assert!(differences(&block, &bar) > 40);
}

#[test]
fn appearance_is_truthfully_static_at_every_phase() {
    let early = SettingsPreviewSpec::appearance(14.0).with_phase(100);
    let late = SettingsPreviewSpec::appearance(14.0).with_phase(1_700);
    assert_eq!(early.animation(), PreviewAnimation::None);
    assert_eq!(late.animation(), PreviewAnimation::None);
    assert_eq!(early.paint_fingerprint(), late.paint_fingerprint());
    assert_eq!(pixels(early), pixels(late));
}

#[test]
fn only_pixel_visible_subjects_request_animation() {
    let zero_intensity = SettingsPreviewSpec::cursor(CursorPreviewSpec {
        blink: false,
        trail_enabled: true,
        trail_style: PreviewTrailStyle::Phaser,
        intensity: 0.0,
        momentum_glow: false,
        ..CursorPreviewSpec::default()
    });
    assert_eq!(zero_intensity.animation(), PreviewAnimation::None);
    assert_eq!(
        pixels(zero_intensity.clone().with_phase(100)),
        pixels(zero_intensity.with_phase(900)),
        "zero intensity must really be pixel-static, not only timer-static"
    );

    let moving = SettingsPreviewSpec::cursor(CursorPreviewSpec {
        blink: false,
        trail_enabled: true,
        trail_style: PreviewTrailStyle::Phaser,
        intensity: 0.05,
        ..CursorPreviewSpec::default()
    });
    assert_eq!(moving.animation(), PreviewAnimation::Continuous);
}

#[test]
fn blink_arms_only_visual_edges_and_quantizes_paint_identity() {
    let blink = |phase_ms| {
        SettingsPreviewSpec::cursor(CursorPreviewSpec {
            blink: true,
            trail_enabled: false,
            trail_style: PreviewTrailStyle::Off,
            ..CursorPreviewSpec::default()
        })
        .with_phase(phase_ms)
    };
    assert_eq!(
        blink(100).animation(),
        PreviewAnimation::BlinkEdge { after_ms: 430 }
    );
    assert_eq!(
        blink(529).animation(),
        PreviewAnimation::BlinkEdge { after_ms: 1 }
    );
    assert_eq!(
        blink(530).animation(),
        PreviewAnimation::BlinkEdge { after_ms: 530 }
    );
    assert_eq!(
        blink(CURSOR_SCENE_CYCLE_MS - 200).animation(),
        PreviewAnimation::BlinkEdge { after_ms: 730 },
        "the cycle's phase reset is not a visual blink edge"
    );

    assert_eq!(
        blink(100).paint_fingerprint(),
        blink(500).paint_fingerprint()
    );
    assert_eq!(pixels(blink(100)), pixels(blink(500)));
    assert_ne!(
        blink(500).paint_fingerprint(),
        blink(600).paint_fingerprint()
    );
    assert_ne!(pixels(blink(500)), pixels(blink(600)));
}

#[test]
fn preview_trail_colours_follow_the_cursor_like_a_window() {
    let theme = PreviewTerminalTheme {
        cursor: 0x50_FA7B,
        ..NORD
    };
    use aterm_effects::cursor_fx::TrailColor;
    let drawn = |style: &'static str, color: TrailColor, pinned: bool| {
        let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            blink: false,
            trail_style: PreviewTrailStyle::parse(style),
            trail_style_raw: Arc::from(style),
            color,
            cursor_color_pinned: pinned,
            ..CursorPreviewSpec::default()
        })
        .with_terminal_theme(theme)
        .with_phase(900);
        let lit = played(&spec, theme.into_theme());
        let mut colors = Vec::new();
        colors.extend(lit.cursor_fill_override);
        if !lit.cursor_trail.is_empty() {
            colors.push(lit.cursor_trail_color);
        }
        assert!(!colors.is_empty(), "{style} lights the caret or a trail");
        colors
    };
    let channels = |rgb: u32| ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
    let green = |rgb: u32| {
        let (r, g, b) = channels(rgb);
        g > b && g > r
    };
    for style in ["comet", "beam", "sparkle"] {
        for rgb in drawn(style, TrailColor::Automatic, false) {
            assert!(!green(rgb), "{style} keeps its own hue: {rgb:06x}");
        }
        for rgb in drawn(style, TrailColor::Automatic, true) {
            assert!(
                green(rgb),
                "{style} wears the pinned cursor colour: {rgb:06x}"
            );
        }
        for rgb in drawn(style, TrailColor::Pinned(0x00E0_218A), false) {
            let (r, g, b) = channels(rgb);
            assert!(r > g && r > b, "{style} draws its set colour: {rgb:06x}");
        }
    }
    for pinned in [false, true] {
        for rgb in drawn("laser", TrailColor::Automatic, pinned) {
            let (r, g, b) = channels(rgb);
            assert!(b > g && r > g, "Laser keeps its violet: {rgb:06x}");
        }
    }
    for style in ["laser", "comet"] {
        for rgb in drawn(style, TrailColor::Cursor, false) {
            assert!(
                green(rgb),
                "{style} in the cursor's colour, chosen: {rgb:06x}"
            );
        }
    }
}

#[test]
fn the_off_preview_types_while_the_typing_glow_is_on() {
    let off = |momentum_glow: bool| {
        SettingsPreviewSpec::cursor(CursorPreviewSpec {
            blink: false,
            trail_enabled: true,
            trail_style: PreviewTrailStyle::Off,
            trail_style_raw: Arc::from("off"),
            momentum_glow,
            ..CursorPreviewSpec::default()
        })
    };
    assert_eq!(off(true).animation(), PreviewAnimation::Continuous);
    assert_eq!(off(true).state_badge(), "Live typing glow");
    let said = off(true).semantic_value();
    assert!(said.contains("glowing as you type"));
    // Typing is said once (composed r1's access judge, D2 b: "no trail, glowing as you
    // type, shown while typing").
    assert_eq!(said.matches("typ").count(), 1, "{said}");
    assert_eq!(off(false).animation(), PreviewAnimation::None);
    assert!(!off(false).semantic_value().contains("glowing"));
    // Control: a trail still says when it is shown.
    let comet = SettingsPreviewSpec::cursor(CursorPreviewSpec {
        blink: false,
        trail_enabled: true,
        trail_style: PreviewTrailStyle::Comet,
        trail_style_raw: Arc::from("comet"),
        momentum_glow: true,
        ..CursorPreviewSpec::default()
    });
    assert!(
        comet.semantic_value().contains("shown while typing"),
        "{}",
        comet.semantic_value()
    );
}

#[test]
fn the_off_preview_glows_on_every_terminal() {
    let off = |style: PreviewCursorStyle, bg: u32, glow: bool| {
        let mut spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            style,
            blink: false,
            trail_enabled: true,
            trail_style: PreviewTrailStyle::Off,
            trail_style_raw: Arc::from("off"),
            momentum_glow: glow,
            ..CursorPreviewSpec::default()
        });
        spec.terminal_theme = Some(PreviewTerminalTheme::new(
            0x001F_2328,
            bg,
            0x0009_69DA,
            0x0080_8080,
            [0; 16],
        ));
        spec.with_phase(1_500)
    };
    let dark = off(PreviewCursorStyle::Block, 0x0011_1111, true);
    assert_eq!(dark.state_badge(), "Live typing glow");
    assert!(dark.semantic_value().contains("glowing as you type"));
    for style in [PreviewCursorStyle::Block, PreviewCursorStyle::Bar] {
        let light = off(style, 0x00FF_FFFF, true);
        assert_eq!(light.animation(), PreviewAnimation::Continuous);
        assert_eq!(light.state_badge(), "Live typing glow", "{style:?}");
        assert!(light.semantic_value().contains("glowing as you type"));
        let paper = light.terminal_theme.expect("a light terminal").into_theme();
        let input = played(&light, paper);
        assert!(
            input.glow_under.iter().any(|q| q.alpha > 0),
            "{style:?}: the glow inks the paper"
        );
        let unlit = played(&off(style, 0x00FF_FFFF, false), paper);
        assert!(unlit.glow_under.is_empty(), "{style:?}: no glow, no ink");
    }
}

/// The card draws a bar caret in the colour the effects lifted for it, as the terminal
/// does (composed r2: the card took the block's fill alone, and the gallery read
/// GitHub Light's #0969da bar at 2.58 on Comet's disc): wherever Comet's light lies on
/// the bar during the card's loop, the bar is drawn darker on the paper in its own hue,
/// and never lighter. Control: the block's fill alone draws the theme's colour throughout.
#[test]
fn the_card_draws_a_bar_caret_in_its_lifted_colour() {
    const BAR: u32 = 0x0009_69DA;
    let lifted_phases = || -> Vec<(u64, u32)> {
        let mut spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
            style: PreviewCursorStyle::Bar,
            blink: false,
            trail_enabled: true,
            trail_style: PreviewTrailStyle::Comet,
            trail_style_raw: Arc::from("comet"),
            momentum_glow: true,
            ..CursorPreviewSpec::default()
        });
        spec.terminal_theme = Some(PreviewTerminalTheme::new(
            0x001F_2328,
            0x00FF_FFFF,
            BAR,
            0x0080_8080,
            [0; 16],
        ));
        let paper = spec.terminal_theme.expect("a light terminal").into_theme();
        (1..=24u64)
            .filter_map(|k| {
                let input = played(&spec.clone().with_phase(k * 100), paper);
                let fill = input.cursor_fill_override.unwrap_or(BAR) & 0x00FF_FFFF;
                (fill != BAR).then_some((k * 100, fill))
            })
            .collect()
    };
    let hue = |c: u32| {
        let [_, r, g, b] = c.to_be_bytes();
        let (r, g, b) = (f32::from(r), f32::from(g), f32::from(b));
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let h = if max == r {
            (g - b) / (max - min)
        } else if max == g {
            2.0 + (b - r) / (max - min)
        } else {
            4.0 + (r - g) / (max - min)
        };
        (h * 60.0).rem_euclid(360.0)
    };
    let paper = 0x00FF_FFFF;
    let lifted = lifted_phases();
    assert!(
        !lifted.is_empty(),
        "Comet's light lies on the bar somewhere in the card's loop, and the bar is lifted there"
    );
    for &(phase, fill) in &lifted {
        let (theirs, ours) = (
            aterm_effects::color_math::contrast_ratio(BAR, paper),
            aterm_effects::color_math::contrast_ratio(fill, paper),
        );
        assert!(
            ours > theirs,
            "at {phase} ms the bar is drawn #{fill:06x}, lighter on the paper than the theme's"
        );
        let gap = (hue(fill) - hue(BAR)).abs();
        assert!(
            gap.min(360.0 - gap) <= 4.0,
            "at {phase} ms the bar #{fill:06x} keeps the theme's hue"
        );
    }
    let kept = SCENE_CONTROLS.with(std::cell::Cell::get);
    SCENE_CONTROLS.with(|controls| {
        controls.set(SceneControls {
            thin_caret_fill: false,
            ..kept
        });
    });
    let unlifted = lifted_phases();
    SCENE_CONTROLS.with(|controls| controls.set(kept));
    assert!(
        unlifted.is_empty(),
        "negative control: the block's fill alone draws the theme's bar: {unlifted:?}"
    );
}

fn sample_rows(source: &str) -> Vec<String> {
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    term.process(source.as_bytes());
    term.cell_frame(24, 80)
        .cells
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| cell.ch)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

#[test]
fn the_shell_samples_speak_this_platforms_shell() {
    assert_eq!(APPEARANCE_SOURCE == APPEARANCE_SOURCE_PWSH, cfg!(windows));
    assert_eq!(
        CURSOR_SPECIMEN_LINES == CURSOR_SPECIMEN_LINES_PWSH,
        cfg!(windows)
    );
    let color = sample_rows(APPEARANCE_SOURCE_PWSH);
    let cursor = sample_rows(&CURSOR_SPECIMEN_LINES_PWSH.join("\r\n"));
    for (rows, line) in [
        (&color, "PS C:\\src> git status --short"),
        (&color, "PS C:\\src> cargo test"),
        (&cursor, "PS C:\\> cat README.md"),
        (&cursor, "PS C:\\> git add .\\README.md; git commit"),
    ] {
        assert!(rows.iter().any(|row| row == line), "{line}: {rows:#?}");
    }
    for row in color.iter().chain(&cursor) {
        for mark in ["$ ", "~/", "build.sh", "make "] {
            assert!(!row.contains(mark), "{mark} in a PowerShell sample: {row}");
        }
    }
    let sh = sample_rows(APPEARANCE_SOURCE_SH);
    assert!(sh.iter().any(|row| row == "$ git status --short"));
    assert_eq!(
        sample_rows(&CURSOR_SPECIMEN_LINES_SH.join("\r\n"))[4],
        "~/demo $ cat README.md"
    );
    for row in color.iter().chain(&sh) {
        assert!(row.chars().count() <= SPECIMEN_COLS, "{row}");
    }
    let station = aterm_effects::kitty_pet::art_cols(8, 17).ceil() as usize + 2;
    for row in cursor
        .iter()
        .chain(&sample_rows(&CURSOR_SPECIMEN_LINES_SH.join("\r\n")))
    {
        assert!(row.chars().count() + station <= CURSOR_SCENE_COLS, "{row}");
    }
    assert_eq!(
        APPEARANCE_SOURCE_PWSH.split("\r\n").count(),
        APPEARANCE_SPECIMEN_ROWS
    );
    for (lines, prompt) in [
        (CURSOR_SPECIMEN_LINES_PWSH, "PS C:\\> "),
        (CURSOR_SPECIMEN_LINES_SH, "~/demo $ "),
    ] {
        let mut term = aterm_core::terminal::Terminal::new(2, CURSOR_SCENE_COLS as u16);
        term.process(lines[lines.len() - 1].as_bytes());
        let input = term.cell_frame(2, CURSOR_SCENE_COLS);
        let line = TypedLine::of(&input.cells[0]);
        assert_eq!(
            (line.start, line.end),
            (prompt.len(), prompt.len() + 31),
            "{prompt}"
        );
        let typed: String = input.cells[0][line.start..line.end]
            .iter()
            .map(|cell| cell.ch)
            .collect();
        let command = if prompt.starts_with("PS") {
            "git add .\\README.md; git commit"
        } else {
            "git add README.md && git commit"
        };
        assert_eq!(typed, command, "{prompt}");
    }
}

#[test]
fn terminal_samples_hold_theme_pairs_to_the_tabs_floor() {
    let mut ansi = [0x0020_2020; 16];
    ansi[4] = 0x00F0_F0F0;
    let candidate =
        PreviewTerminalTheme::new(0x0020_2020, 0x00F0_F0F0, 0x0020_2020, 0x00C0_C0C0, ansi);
    let sample = |floor: f32| {
        let spec = SettingsPreviewSpec::appearance(14.0)
            .with_terminal_theme(candidate)
            .with_appearance(AppearancePreviewSpec {
                indexed_minimum_contrast: floor,
                ..AppearancePreviewSpec::default()
            });
        build_terminal_specimen_input_rows(&spec, candidate.into_theme(), Some(1))
    };
    let (off, off_key) = sample(1.0);
    let (on, on_key) = sample(crate::app_config::DEFAULT_INDEXED_MINIMUM_CONTRAST.max(4.5));
    assert_ne!(off_key, on_key, "the floor is part of the sample's key");
    let ratio = |input: &RenderInput| {
        let cell = &input.cells[0][0];
        assert_eq!(cell.ch, 'A');
        crate::native_appearance::contrast_ratio(cell.fg, cell.bg)
    };
    assert!(ratio(&off) < 1.1, "unfloored: {}", ratio(&off));
    assert!(ratio(&on) >= 3.0, "floored: {}", ratio(&on));
    assert_eq!(
        AppearancePreviewSpec::default().indexed_minimum_contrast,
        crate::app_config::DEFAULT_INDEXED_MINIMUM_CONTRAST,
        "a sample's floor is the tab's default"
    );
}

fn style_scene(style: PreviewTrailStyle, raw: &'static str) -> CursorPreviewSpec {
    CursorPreviewSpec {
        blink: false,
        trail_style: style,
        trail_style_raw: Arc::from(raw),
        momentum_glow: false,
        ..CursorPreviewSpec::default()
    }
}

#[test]
fn a_looping_preview_is_a_focusable_button_in_the_semantic_tree() {
    let key = crate::native_ui::UiKey::new("preview");
    let tree = |spec: SettingsPreviewSpec, focus: Option<&str>, visible: bool| {
        let prepared =
            crate::tray_raster::prepared_semantic_font_for_direct_view_test(&spec.font_candidate);
        let spec = spec.with_prepared_font(prepared);
        let mut tree = UiTree::new(
            UiNode::new(
                "root",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(Layout::column().padding(Insets::all(10.0)))
            .children(vec![
                preview_node("preview", spec, 190.0),
                UiNode::new(
                    "other",
                    UiContent::Button(crate::native_ui::Control::new(
                        crate::native_ui::ButtonSpec::new("Other"),
                        crate::native_ui::ActionId::new("other"),
                    )),
                ),
            ]),
        );
        let focus = focus.map(crate::native_ui::UiKey::new);
        tree.apply_interaction(focus.as_ref(), None, None, visible);
        tree.compile(VIEWPORT).expect("preview compiles")
    };
    let _ = compiled(SettingsPreviewSpec::default());
    let looping = SettingsPreviewSpec::default().with_phase(300);
    assert!(looping.pause_control());
    let ringed = |compiled: &crate::native_ui::CompiledUi| {
        compiled.paint.iter().any(|node| match &node.content {
            UiContent::SettingsPreview(spec) => spec.focus_visible,
            _ => false,
        })
    };

    let focused = tree(looping.clone(), Some("preview"), true);
    let focused_nodes: Vec<_> = focused
        .semantics
        .iter()
        .filter(|node| node.state.is_some_and(|state| state.focused))
        .map(|node| node.key.clone())
        .collect();
    assert_eq!(focused_nodes, vec![key.clone()], "exactly one focused node");
    let node = focused.semantic(&key).expect("the preview");
    assert_eq!(node.role, SemanticRole::Button);
    let state = node.state.expect("a control states itself");
    assert!(state.enabled && state.focused && state.focus_visible);
    assert!(focused.focus_order.contains(&key));
    assert!(ringed(&focused), "keyboard focus rings the card");

    let pointer = tree(looping.clone(), Some("preview"), false);
    let state = pointer.semantic(&key).and_then(|node| node.state);
    assert!(state.is_some_and(|state| state.focused && !state.focus_visible));
    assert!(!ringed(&pointer), "a pointer's focus rings nothing");

    let elsewhere = tree(looping, Some("other"), true);
    let state = elsewhere.semantic(&key).and_then(|node| node.state);
    assert!(state.is_some_and(|state| state.enabled && !state.focused));
    assert!(!ringed(&elsewhere));

    let still = SettingsPreviewSpec::default()
        .with_phase(300)
        .with_reduced_motion(true);
    assert!(!still.pause_control());
    let still = tree(still, Some("preview"), true);
    let node = still.semantic(&key).expect("the preview");
    assert_eq!(node.role, SemanticRole::Group);
    assert!(node.state.is_none() && node.action.is_none());
    assert!(!still.focus_order.contains(&key));
    assert!(!ringed(&still));
}

#[test]
fn pause_control_stills_the_looping_preview() {
    let live = SettingsPreviewSpec::default().with_phase(300);
    assert!(live.pause_control());
    assert_eq!(live.animation(), PreviewAnimation::Continuous);
    let paused = |phase_ms| {
        SettingsPreviewSpec::default()
            .with_phase(phase_ms)
            .with_paused(true)
    };
    assert_eq!(paused(300).animation(), PreviewAnimation::None);
    assert_eq!(
        paused(300).paint_fingerprint(),
        paused(1_900).paint_fingerprint()
    );
    assert_eq!(pixels(paused(300)), pixels_recomputed(paused(1_900)));
    assert_ne!(
        paused(300).state_badge(),
        live.state_badge(),
        "the header says the loop is held"
    );
    assert_ne!(
        paused(300).semantic_value(),
        live.semantic_value(),
        "the accessible value says the loop is held"
    );
    assert!(
        differences(
            &pixels(live),
            &pixels(SettingsPreviewSpec::default().with_phase(1_900))
        ) > 80,
        "negative control: playing, the phase moves pixels"
    );

    let held = compiled(paused(300));
    let key = crate::native_ui::UiKey::new("preview");
    let node = held.semantic(&key).expect("preview semantics");
    assert_eq!(
        node.action.as_ref().map(crate::native_ui::ActionId::as_str),
        Some(PREVIEW_PAUSE_ACTION)
    );
    assert!(held.focus_order.contains(&key), "keyboard reachable");
    let held_gesture = node
        .description
        .clone()
        .expect("a control describes what activating it does");
    let hit = held
        .hits
        .iter()
        .find(|hit| hit.key == key)
        .expect("a hit target");
    assert!(
        hit.rect.width >= 96.0 && hit.rect.height >= 96.0,
        "{:?}",
        hit.rect
    );
    let playing = compiled(SettingsPreviewSpec::default().with_phase(300));
    let playing_gesture = playing
        .semantic(&key)
        .and_then(|node| node.description.clone())
        .expect("a control describes what activating it does");
    assert_ne!(
        held_gesture, playing_gesture,
        "the gesture flips with the state: play when held, pause when playing"
    );

    let blink_only = SettingsPreviewSpec::cursor(CursorPreviewSpec {
        trail_enabled: false,
        trail_style: PreviewTrailStyle::Off,
        ..CursorPreviewSpec::default()
    })
    .with_phase(100)
    .with_paused(true);
    assert!(!blink_only.pause_control());
    assert!(matches!(
        blink_only.animation(),
        PreviewAnimation::BlinkEdge { .. }
    ));
    assert!(
        !SettingsPreviewSpec::default()
            .with_reduced_motion(true)
            .pause_control()
    );
}

/// The card stands still for one cause at a time, and its header names that cause: no two
/// causes share the header's words, nor the spoken value's. Round 2's header read "Paused"
/// for the background, load, rest and the viewer's own Pause alike, so a sighted reader
/// could not tell a held demo from a backgrounded one while the spoken value could.
#[test]
fn each_cause_of_a_still_card_reads_its_own_header() {
    let causes = [
        PreviewPause::Unspecified,
        PreviewPause::SeriousMode,
        PreviewPause::Load,
        PreviewPause::Unfocused,
        PreviewPause::MotionSetting,
        PreviewPause::SystemReduceMotion,
        PreviewPause::Resting,
        PreviewPause::Viewer,
    ];
    let still = |cause: PreviewPause| {
        let spec = SettingsPreviewSpec::default().with_phase(300);
        if cause == PreviewPause::Viewer {
            spec.with_paused(true)
        } else {
            spec.with_reduced_motion(true).with_pause(cause)
        }
    };
    let live = SettingsPreviewSpec::default().with_phase(300);
    let mut headers = std::collections::BTreeMap::new();
    let mut spoken = std::collections::BTreeMap::new();
    for cause in causes {
        let spec = still(cause);
        assert_eq!(
            spec.animation(),
            PreviewAnimation::None,
            "{cause:?} stands still"
        );
        assert_ne!(spec.state_badge(), live.state_badge(), "{cause:?}");
        if let Some(twin) = headers.insert(spec.state_badge(), cause) {
            panic!(
                "{twin:?} and {cause:?} both read {:?} in the header",
                spec.state_badge()
            );
        }
        if let Some(twin) = spoken.insert(spec.semantic_value(), cause) {
            panic!("{twin:?} and {cause:?} are spoken alike");
        }
    }
}

#[test]
fn a_paused_preview_holds_the_trail_mid_line() {
    let spec = |phase| {
        SettingsPreviewSpec::cursor(style_scene(PreviewTrailStyle::Comet, "comet"))
            .with_phase(phase)
            .with_paused(true)
    };
    let held = played(&spec(300), Theme::default());
    assert_eq!(
        scene_streams(&held),
        scene_streams(&played(&spec(3_900), Theme::default())),
        "one frame at every phase"
    );
    assert!(has_effect_light(&held), "the held frame shows the trail");
    let (base, _) = build_terminal_specimen_input(&spec(300).normalized(), Theme::default());
    let line = TypedLine::of(&base.cells[prompt_row(&base)]);
    assert!(
        held.cursor_col > line.start && held.cursor_col < line.end,
        "the command is part-typed at the hold: caret {} in {}..{}",
        held.cursor_col,
        line.start,
        line.end
    );
    let still = played(
        &SettingsPreviewSpec::cursor(style_scene(PreviewTrailStyle::Comet, "comet"))
            .with_phase(300)
            .with_reduced_motion(true),
        Theme::default(),
    );
    assert!(
        !has_effect_light(&still),
        "control: the still line carries no light"
    );
}

#[test]
fn laser_preview_caret_is_the_bolt() {
    let at = |style, raw| {
        played(
            &SettingsPreviewSpec::cursor(style_scene(style, raw)).with_phase(900),
            Theme::default(),
        )
    };
    let laser = at(PreviewTrailStyle::Laser, "laser");
    assert_eq!(
        laser.cursor_effect_style_override,
        Some(aterm_core::terminal::CursorStyle::Bolt)
    );
    assert!(laser.cursor_visible);
    assert_ne!(
        at(PreviewTrailStyle::Lumen, "lumen").cursor_effect_style_override,
        Some(aterm_core::terminal::CursorStyle::Bolt),
        "negative control: only the laser owns the bolt"
    );
}

#[test]
fn fire_preview_caret_is_the_forge_fill() {
    let fire = played(
        &SettingsPreviewSpec::cursor(style_scene(PreviewTrailStyle::Fire, "fire"))
            .with_phase(1_200),
        Theme::default(),
    );
    let fill = fire.cursor_fill_override.expect("fire owns the caret body");
    assert_ne!(fill, Theme::default().cursor & 0x00FF_FFFF);
    let (r, b) = ((fill >> 16) & 0xFF, fill & 0xFF);
    assert!(r > b, "the forge caret is warm metal: {fill:#08x}");
    assert_eq!(
        played(
            &SettingsPreviewSpec::cursor(style_scene(PreviewTrailStyle::Classic, "classic"))
                .with_phase(1_200),
            Theme::default(),
        )
        .cursor_fill_override,
        None,
        "negative control: a style with no caret body leaves the caret alone"
    );
}

#[test]
fn light_theme_rainbow_pet_preview_shows_its_ribbon() {
    let github_light = PreviewTerminalTheme::from_scheme(
        &aterm_types::scheme::builtin("GitHub Light").expect("built-in GitHub Light"),
    );
    let at = |cursor| {
        let spec = SettingsPreviewSpec::cursor(cursor)
            .with_phase(1_100)
            .with_terminal_theme(github_light);
        played(&spec, github_light.into_theme())
    };
    let ink = |input: &RenderInput| {
        input
            .glow_under
            .iter()
            .chain(&input.cursor_glow_add)
            .filter(|quad| quad.alpha > 0)
            .count()
    };
    let rainbow = at(CursorPreviewSpec {
        momentum_glow: false,
        ..rainbow_cursor("rainbow")
    });
    assert!(ink(&rainbow) > 0, "the light ribbon is ink on the page");
    let off = at(style_scene(PreviewTrailStyle::Off, "off"));
    assert!(!has_effect_light(&off), "control: Off lays no light");
}

#[test]
fn preview_light_is_the_renderer_frame_at_fractional_scales() {
    let specimen_at = |phase_ms| {
        compiled(SettingsPreviewSpec::cursor(rainbow_cursor("rainbow")).with_phase(phase_ms))
            .tray(Theme::default(), 13.0)
            .prims
            .iter()
            .find_map(|primitive| match primitive {
                DrawPrim::TerminalSpecimen { spec, .. } => Some(spec.as_ref().clone()),
                _ => None,
            })
            .expect("cursor specimen")
    };
    let earlier = specimen_at(312);
    let specimen = specimen_at(1_196);
    for scale in [1.25_f32, 1.5] {
        let frame = crate::tray_raster::render_specimen_frame_for_test(&specimen, scale)
            .expect("the specimen renders");
        let logical_w = (frame.width as f32 / scale).ceil() as u32 + 1;
        let logical_h = (frame.height as f32 / scale).ceil() as u32 + 1;
        let blit = |specimen: &crate::widget::TerminalSpecimenSpec| {
            crate::tray_raster::rasterize_tray(
                &[DrawPrim::TerminalSpecimen {
                    x: 0.0,
                    y: 0.0,
                    spec: Box::new(specimen.clone()),
                }],
                logical_w,
                logical_h,
                scale,
                [0, 0, 0, 0],
            )
        };
        let _ = blit(&earlier);
        let (raster, raster_w, _) = blit(&specimen);
        let still = crate::tray_raster::render_specimen_frame_for_test(
            &crate::widget::TerminalSpecimenSpec {
                motion: None,
                ..specimen.clone()
            },
            scale,
        )
        .expect("the still specimen renders");
        assert_ne!(
            still.pixels, frame.pixels,
            "{scale}x: the scene reaches the renderer's frame"
        );
        for (row, pixels) in frame.pixels.chunks(frame.width).enumerate() {
            for (col, pixel) in pixels.iter().enumerate() {
                let i = (row * raster_w as usize + col) * 4;
                let rgb = [
                    ((pixel >> 16) & 0xff) as u8,
                    ((pixel >> 8) & 0xff) as u8,
                    (pixel & 0xff) as u8,
                ];
                assert_eq!(raster[i..i + 3], rgb, "{scale}x pixel ({col},{row})");
            }
        }
    }
}

/// The pixel at `(x, y)` of `frame`.
fn frame_rgb(frame: &aterm_render::Frame, x: usize, y: usize) -> [u8; 3] {
    crate::settings::u32_rgb(frame.pixels[y * frame.width + x])
}

use aterm_render::letter_judge::judged_letters;

/// The least a letter reads under a light (WCAG AA).
const LEGIBLE: f32 = 4.5;

/// The share of its own bare reading a letter that reads under [`LEGIBLE`] on the bare
/// card keeps under the lights, as the letter guard keeps one
/// (`cursor_glow::ink_ledger::letter_guard`'s `OWN_SHARE`): GitHub Light's bold green
/// `build.sh` reads 4.25:1 bare, so no light can leave it at 4.5.
const OWN_SHARE: f32 = 0.97;

/// Of [`judged_letters`], the one furthest under its bar (`(reading, bar, row, col)`):
/// [`LEGIBLE`], or [`OWN_SHARE`] of its bare reading where that is lower.
fn worst_lit_letter(
    lit: &aterm_render::Frame,
    twin: &aterm_render::Frame,
    input: &RenderInput,
    cell: (usize, usize),
) -> Option<(f32, f32, usize, usize)> {
    judged_letters(lit, twin, input, cell)
        .into_iter()
        .map(|(reading, bare, row, col)| (reading, LEGIBLE.min(OWN_SHARE * bare), row, col))
        .min_by(|a, b| (a.0 / a.1).total_cmp(&(b.0 / b.1)))
}

/// What [`lit_card`]'s twin of the lit card leaves out.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Twin {
    /// The lights alone: the caret, the letters and the companion kept.
    Bare,
    /// Every overlay (`RenderInput::clear_overlays`, as `image clean` captures a
    /// terminal's twin): the letters and the caret kept.
    Clean,
}

struct LitCard {
    lit: aterm_render::Frame,
    twin: aterm_render::Frame,
    input: RenderInput,
    cell: (usize, usize),
    without_body: Option<aterm_render::Frame>,
}

/// The card's terminal at `rows` rows playing `raw` on `theme` at `phase`, lit and as
/// its `twin`, with the bloom or without and the companion in `look` (`None`: the
/// default), drawn at display `scale` (1, or a Retina display's 2): the frames, the lit
/// input and the cell size.
fn lit_card(
    raw: &str,
    (theme, bloom, look): (
        PreviewTerminalTheme,
        bool,
        Option<aterm_effects::kitty_registry::KittyLook>,
    ),
    (phase, rows, twin): (u64, usize, Twin),
    (prepared, scale): (&crate::tray_raster::PreparedSemanticFont, f32),
) -> LitCard {
    let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
        blink: false,
        trail_style: PreviewTrailStyle::parse(raw),
        trail_style_raw: Arc::from(raw),
        ..CursorPreviewSpec::default()
    })
    .with_post_fx(CursorPostFxSpec {
        bloom,
        companion_look: look.unwrap_or(CursorPostFxSpec::default().companion_look),
        ..CursorPostFxSpec::default()
    })
    .with_terminal_theme(theme)
    .with_phase(phase)
    .with_prepared_font(prepared.clone())
    .normalized();
    let kept = SCENE_CONTROLS.get();
    SCENE_CONTROLS.set(SceneControls {
        observe_body: raw == "rainbow" && scale == 2.0 && twin == Twin::Clean,
        ..kept
    });
    let specimen = spec.terminal_specimen(theme.into_theme(), Some(rows));
    SCENE_CONTROLS.set(kept);
    let crate::tray_raster::ObservedSpecimen {
        frame: lit,
        input: lit_input,
        cell,
        without_body,
    } = crate::tray_raster::render_specimen_observed_for_test(&specimen, scale)
        .expect("the lit card and its actual published input");
    if raw == "rainbow" && scale == 2.0 && twin == Twin::Clean {
        assert!(
            without_body.is_some(),
            "the retained step supplies its body-only twin"
        );
    }
    let mut twin_input = lit_input.clone();
    match twin {
        Twin::Bare => {
            twin_input.cursor_glow_add.clear();
            twin_input.glow_under.clear();
            twin_input.glow_halo.clear();
            twin_input.fire_patch.clear();
            twin_input.char_fg.clear();
            twin_input.fire_halo.clear();
            twin_input.cursor_trail.clear();
        }
        Twin::Clean => twin_input.clear_overlays(),
    }
    let twin = crate::tray_raster::render_specimen_frame_for_test(
        &crate::widget::TerminalSpecimenSpec {
            input: Arc::new(twin_input),
            input_fingerprint: specimen.input_fingerprint ^ 0x0BA2_E000 ^ phase,
            motion: None,
            ..specimen.clone()
        },
        scale,
    )
    .expect("the card's twin");
    LitCard {
        lit,
        twin,
        input: lit_input,
        cell,
        without_body,
    }
}

/// A card of style `raw` on `theme` at `phase` in the face `prepared`, drawn at scale 1, with
/// the input it published.
fn observed_card(
    raw: &str,
    theme: PreviewTerminalTheme,
    phase: u64,
    prepared: &crate::tray_raster::PreparedSemanticFont,
) -> crate::tray_raster::ObservedSpecimen {
    let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
        blink: false,
        trail_style: PreviewTrailStyle::parse(raw),
        trail_style_raw: Arc::from(raw),
        ..CursorPreviewSpec::default()
    })
    .with_terminal_theme(theme)
    .with_phase(phase)
    .with_prepared_font(prepared.clone())
    .normalized();
    let specimen = spec.terminal_specimen(theme.into_theme(), Some(7));
    crate::tray_raster::render_specimen_observed_for_test(&specimen, 1.0)
        .expect("the lit card and its published input")
}

/// Whether a card's guard reads the steps it does not keep (`guarded`), for a test to compare.
fn set_unseen_steps_guarded(guarded: bool) {
    SCENE_CONTROLS.with(|controls| {
        controls.set(SceneControls {
            unseen_steps_unguarded: !guarded,
            ..controls.get()
        });
    });
}

/// A CARD IS THE FRAME IT WOULD BE WITH EVERY STEP BEFORE IT GUARDED: the letter guard spends
/// nothing on a step the card does not keep (`CursorGlow::set_frame_unseen`), and on every
/// listed style, on dark and on paper, at two phases of one replay, the kept frame's pixels and
/// lights are the very ones a replay guarding each step gives.
/// NEGATIVE CONTROL (2026-11-05): with the guard also passing by the kept step this fired "the
/// kept frame moved: rainbow kitty pet on 111318 at 4472 ms"; with its frame memo answering
/// whatever the frame's lights, "the kept frame moved: off on 111318 at 2262 ms".
#[test]
fn a_card_is_the_frame_it_would_be_with_every_step_before_it_guarded() {
    let _ = compiled(SettingsPreviewSpec::default());
    let prepared = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
        &SemanticFontCandidate::default(),
    );
    let themes = [
        PreviewTerminalTheme::from_scheme(&aterm_types::ColorScheme::default()),
        PreviewTerminalTheme::from_scheme(
            &aterm_types::scheme::builtin("GitHub Light").expect("built-in light theme"),
        ),
    ];
    let phases = [
        CURSOR_SCENE_CYCLE_MS / 2,
        CURSOR_SCENE_CYCLE_MS - MOTION_STEP_MS,
    ];
    let mut judged = 0;
    for raw in crate::native_settings::listed_cursor_style_values() {
        for theme in themes {
            // One replay a side, each phase playing on from the one before as a card's does.
            let kept = phases.map(|phase| observed_card(raw, theme, phase, &prepared));
            set_unseen_steps_guarded(true);
            let whole = phases.map(|phase| observed_card(raw, theme, phase, &prepared));
            set_unseen_steps_guarded(false);
            for ((kept, whole), phase) in kept.iter().zip(&whole).zip(phases) {
                let at = format!("{raw} on {:06x} at {phase} ms", theme.into_theme().bg);
                assert!(
                    kept.frame.pixels == whole.frame.pixels,
                    "the kept frame moved: {at}"
                );
                assert_eq!(
                    (&kept.input.cursor_glow_add, &kept.input.glow_under),
                    (&whole.input.cursor_glow_add, &whole.input.glow_under),
                    "the kept frame's lights moved: {at}"
                );
                judged += usize::from(
                    !(kept.input.cursor_glow_add.is_empty() && kept.input.glow_under.is_empty()),
                );
            }
        }
    }
    assert!(judged > 0, "some frame holds a light to judge");
}

/// The same in the host's own face, comet on Solarized Light, a card playing on at five motion
/// steps a frame: a share the guard lowers on a frame it reads (`CursorGlow::verify_cuts`) stays
/// that frame's, so a replay that read more frames carries nothing over into the one shown.
/// NEGATIVE CONTROL (2026-11-05): `verify_cuts` writing its lowered share back over the kept
/// verdict, as it did from 39ba2aa319, fired "the kept frame moved at 3640 ms". The wider sweep
/// this case came from (the host's face and the embedded one, at scales 1 and 2; five themes,
/// every style, three companion looks, six strides; 13,750 frames a face and scale, kept, whole
/// and judged afresh) moved no frame once that write was gone. It was run once and not kept.
#[cfg(target_os = "macos")]
#[test]
fn a_card_in_the_host_s_face_carries_nothing_over_from_a_step_it_guarded() {
    let _ = compiled(SettingsPreviewSpec::default());
    let prepared = host_card_face();
    let theme = PreviewTerminalTheme::from_scheme(
        &aterm_types::scheme::builtin("Solarized Light").expect("a built-in theme"),
    );
    let phases: Vec<u64> = (0..=3640).step_by((5 * MOTION_STEP_MS) as usize).collect();
    let replay = |phases: &[u64]| -> Vec<_> {
        phases
            .iter()
            .map(|&phase| observed_card("comet", theme, phase, &prepared))
            .collect()
    };
    let kept = replay(&phases);
    set_unseen_steps_guarded(true);
    let whole = replay(&phases);
    set_unseen_steps_guarded(false);
    for ((kept, whole), phase) in kept.iter().zip(&whole).zip(&phases) {
        assert!(
            kept.frame.pixels == whole.frame.pixels,
            "the kept frame moved at {phase} ms"
        );
        assert_eq!(
            (&kept.input.cursor_glow_add, &kept.input.glow_under),
            (&whole.input.cursor_glow_add, &whole.input.glow_under),
            "the kept frame's lights moved at {phase} ms"
        );
    }
}

/// How far toward the page the darkest pixel of the thinnest glyph of the Cursor card's face
/// lies on `theme` at display `scale` ([`aterm_render::GlyphInk::thin_share`]), as a window's
/// page law reads its own face (`host::PageFacts::thin_ink`).
fn card_thin_ink(
    theme: PreviewTerminalTheme,
    prepared: &crate::tray_raster::PreparedSemanticFont,
    scale: f32,
) -> f32 {
    let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec::default())
        .with_terminal_theme(theme)
        .with_prepared_font(prepared.clone())
        .normalized();
    let theme = theme.into_theme();
    let specimen = spec.terminal_specimen(theme, Some(7));
    let ink =
        crate::tray_raster::specimen_glyph_ink_for_test(&specimen, scale).expect("the card's face");
    let lum = |c: u32| aterm_render::linear_luminance(c & 0x00FF_FFFF);
    ink.thin_share(lum(theme.fg), lum(theme.bg))
}

/// `settings card` names the card a capture painted as the terminal names a frame: its grid
/// at the card's place in the window (the native page's origin plus where its raster blitted
/// the card), the caret, each row's text, and the companion's body in window pixels; `settings
/// card clean` is the same cells with no light, and inside that body it differs from the lit
/// card (the companion is gone). A capture of a frame with no native page (the negative
/// control) keeps no card, and the verb says so rather than answer a stale one.
#[test]
fn settings_card_names_the_painted_card_as_a_terminal_frame() {
    let _ = compiled(SettingsPreviewSpec::default());
    let prepared = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
        &SemanticFontCandidate::default(),
    );
    let theme = PreviewTerminalTheme::from_scheme(&aterm_types::ColorScheme::default());
    let raw = "rainbow";
    let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
        blink: false,
        trail_style: PreviewTrailStyle::parse(raw),
        trail_style_raw: Arc::from(raw),
        ..CursorPreviewSpec::default()
    })
    .with_terminal_theme(theme)
    .with_phase(1500)
    .with_prepared_font(prepared.clone())
    .normalized();
    let specimen = spec.terminal_specimen(theme.into_theme(), Some(7));
    let (cw, ch) =
        crate::tray_raster::specimen_cell_size_for_test(&specimen, 1.0).expect("the card's cell");
    let lit =
        crate::tray_raster::render_specimen_frame_for_test(&specimen, 1.0).expect("the lit card");
    // The raster blits the card 30, 40 into the page, the page at 3, 4 in the window.
    note_scene_card_painted(&specimen, 1.0, (30, 40));
    stash_captured_scene_card(Some((3, 4)));
    let lines = captured_scene_card_reply(false).expect("the card");
    let (rows, cols) = (specimen.input.rows, specimen.input.cols);
    assert_eq!(
        lines[0],
        format!(
            "dims OK {rows} {cols} {} {} cell_w={cw} cell_h={ch} pad=33 pad_top=44 head=0",
            cols * cw,
            rows * ch
        )
    );
    assert!(lines[1].starts_with("cursor OK 5 "), "{lines:?}");
    let text: Vec<&str> = lines
        .iter()
        .filter_map(|line| line.strip_prefix("text "))
        .collect();
    assert_eq!(text.len(), rows, "{lines:?}");
    // The card speaks this platform's shell (`CURSOR_SPECIMEN_LINES`).
    let typing = if cfg!(windows) {
        "PS C:\\> git add"
    } else {
        "~/demo $ git add"
    };
    assert!(text.iter().any(|row| row.starts_with(typing)), "{text:?}");
    let body = lines[2]
        .split_whitespace()
        .find_map(|word| word.strip_prefix("pet_body="))
        .expect("the companion's body");
    let body: Vec<usize> = body
        .split(',')
        .map(|v| v.parse::<usize>().expect("a window pixel"))
        .collect();
    let (x0, x1, y0, y1) = (body[0] - 33, body[1] - 33, body[2] - 44, body[3] - 44);
    let clean = captured_scene_card_reply(true).expect("the bare card");
    let words: Vec<&str> = clean[0].split_whitespace().collect();
    assert_eq!(&words[..3], ["clean", "33", "44"], "{:?}", &words[..5]);
    let png = aterm_codec::base64::decode(words[6]).expect("the card's PNG");
    let (bare, w, h) = aterm_png::decode_rgba8(&png, 4096).expect("the PNG decodes");
    assert_eq!((w, h), (lit.width, lit.pixels.len() / lit.width));
    let changed = |x: usize, y: usize| {
        let at = (y * w + x) * 4;
        frame_rgb(&lit, x, y) != [bare[at], bare[at + 1], bare[at + 2]]
    };
    let in_body = (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| (x, y)))
        .filter(|&(x, y)| changed(x, y))
        .count();
    assert!(in_body > 0, "the bare card still shows the companion");
    // Both ways a page's raster paints the card note it: the frame of a specimen that
    // covers most of the page, prepared once for the whole raster (the Cursor page's card
    // in a captured window; the trail gallery's every settings frame missed it so), and a
    // card a larger page paints in passing.
    let card = (cols * cw, rows * ch);
    for page in [(card.0 + 40, card.1 + 50), (4 * card.0, 4 * card.1)] {
        PAINTED_CARD.set(None);
        let prims = [crate::widget::DrawPrim::TerminalSpecimen {
            x: 30.0,
            y: 40.0,
            spec: Box::new(specimen.clone()),
        }];
        let w = u32::try_from(page.0).expect("a page's width");
        let h = u32::try_from(page.1).expect("a page's height");
        let _ = crate::tray_raster::rasterize_tray(&prims, w, h, 1.0, [0, 0, 0, 255]);
        stash_captured_scene_card(Some((3, 4)));
        let painted = captured_scene_card_reply(false).expect("the painted card");
        assert_eq!(painted[0], lines[0], "a {page:?} page");
    }
    stash_captured_scene_card(None);
    assert!(captured_scene_card_reply(false).is_err());
}

/// The light the card's edges cut in `rows` rows playing `raw` on `theme`, one line a frame,
/// at every frame of the loop (the bloom left out, whose soft spill is no mark). Round the
/// sides and the bottom every pixel is the bare page's. On the top edge a light may stand
/// on the top row from its row's top, kept in its row (Beam's leap bed after the Return
/// lights the row's top pixel in every card, the same pixels a row lower in a taller card);
/// it is cut where it rises past the row: where the card one line taller, the same scene
/// with one more line over it, lights the pixel over that same row in that column.
fn edge_cuts(
    raw: &str,
    (side, theme): (&str, PreviewTerminalTheme),
    rows: usize,
    (prepared, scale): (&crate::tray_raster::PreparedSemanticFont, f32),
) -> Vec<String> {
    let lit_at = |lit: &aterm_render::Frame, bare: &aterm_render::Frame, x, y| {
        let (a, b) = (frame_rgb(lit, x, y), frame_rgb(bare, x, y));
        a.iter().zip(&b).any(|(p, q)| p.abs_diff(*q) > 12)
    };
    // Each card walks the loop whole, one after the other: its scene plays on from the frame
    // before, where turning between the two at every lit frame replays its warm cycles.
    let mut lit_edges = Vec::new();
    for phase in (0..CURSOR_SCENE_CYCLE_MS).step_by(MOTION_STEP_MS as usize) {
        let LitCard {
            lit, twin: bare, ..
        } = lit_card(
            raw,
            (theme, false, None),
            (phase, rows, Twin::Bare),
            (prepared, scale),
        );
        let (w, h) = (lit.width, lit.height);
        let sides = (0..w)
            .map(|x| (x, h - 1))
            .chain((0..h).flat_map(|y| [(0, y), (w - 1, y)]))
            .filter(|&(x, y)| lit_at(&lit, &bare, x, y))
            .count();
        let top: Vec<usize> = (0..w).filter(|&x| lit_at(&lit, &bare, x, 0)).collect();
        if sides > 0 || !top.is_empty() {
            lit_edges.push((phase, sides, top));
        }
    }
    let mut cuts = Vec::new();
    for (phase, sides, top) in lit_edges {
        let risen = if top.is_empty() {
            0
        } else {
            let LitCard {
                lit: tall,
                twin: tall_bare,
                cell: (_, cell_h),
                ..
            } = lit_card(
                raw,
                (theme, false, None),
                (phase, rows + 1, Twin::Bare),
                (prepared, scale),
            );
            top.iter()
                .filter(|&&x| lit_at(&tall, &tall_bare, x, cell_h - 1))
                .count()
        };
        if sides + risen > 0 {
            cuts.push(format!(
                "{raw} {side} in {rows} rows at {scale}x at {phase} ms: {sides} px at the \
                 sides and bottom, {risen} risen past the top"
            ));
        }
    }
    cuts
}

/// No mark the scene draws stands cut at the card's edge ([`edge_cuts`]), every style on
/// dark and light, in cards of three, four and five rows, at display scale 1 and 2, and on
/// macOS on light in the host's face at scale 2, whose card the page law gives the glass
/// (§15.8 amendment 54): the picker host's preview shrinks to three, a Settings window under
/// 760 px wide shows four. A particle the
/// edge would cut is left out whole (`CursorGlow::emit_particles`, the rainbow kitty's paper
/// stars), and a light rising past the card's top is laid shorter inside it, its soft top
/// whole ([`aterm_effects::cursor_glow`]'s rise ceiling): the card gives its grid no head
/// band (`head` 0), where a terminal window's top row rises into its own. The negative
/// controls: drawn cut, Water's drops lay flat along the card's bottom edge; cut by the
/// box's clip, the rainbow band's exit in a four-row card (the Return sets the submitted
/// line on the top row) and Fire's dark cores over the prompt in a three-row card stood
/// flat against the card's top edge (settings round 2). Fire's paper tongues there were
/// cut too. A taller card that reads the frame's text holds them under its submitted line's
/// letters, where the top edge cannot see them rise, so only the blind three-row card shows
/// them here; the engine's own test holds them with the text read
/// (`a_blaze_under_the_grids_top_keeps_inside_the_box`).
///
/// Only the four-row items play as the terminal does: that card and its taller five-row twin
/// read the frame's text. The three- and five-row items play blind to it, their taller twins
/// too, where the guard cuts nothing and more light reaches the edge: a frame production never
/// draws, so a failure seen only blind may be a false positive, and a mark drawn only because of
/// the frame's text is judged by the four-row items alone. Each control plays its row count as
/// the sweep does.
#[test]
fn no_light_stands_cut_at_the_cards_edge() {
    fn reads_frame_text(rows: usize) -> bool {
        rows == 4
    }
    let light = PreviewTerminalTheme::from_scheme(
        &aterm_types::scheme::builtin("GitHub Light").expect("built-in light theme"),
    );
    let dark = PreviewTerminalTheme::from_scheme(&aterm_types::ColorScheme::default());
    // One card: its style, its named ground, its rows, its scale, and whether it lays the
    // host's face.
    type Card<'a> = (&'a str, (&'a str, PreviewTerminalTheme), usize, f32, bool);
    let mut items: Vec<Card<'_>> = crate::prefs::CURSOR_TRAIL_STYLES
        .iter()
        .flat_map(|&raw| {
            [3, 4, 5].into_iter().flat_map(move |rows| {
                [1.0, 2.0].into_iter().flat_map(move |scale| {
                    [
                        (raw, ("dark", dark), rows, scale, false),
                        (raw, ("light", light), rows, scale, false),
                    ]
                })
            })
        })
        .collect();
    if cfg!(target_os = "macos") {
        for &raw in crate::prefs::CURSOR_TRAIL_STYLES {
            for rows in [3, 4, 5] {
                items.push((raw, ("light, the host's face,", light), rows, 2.0, true));
            }
        }
    }
    // Each item plays on its own thread, with that thread's fonts and scene.
    let found: Vec<String> =
        aterm_spec::check::each(&items, |&(raw, ground, rows, scale, host)| {
            // Set on each item: a worker keeps its thread-local between items.
            SCENE_CONTROLS.with(|controls| {
                let mut sight = controls.get();
                sight.frame_text = reads_frame_text(rows);
                controls.set(sight);
            });
            let _ = compiled(SettingsPreviewSpec::default());
            #[cfg(target_os = "macos")]
            if host {
                return edge_cuts(raw, ground, rows, (&host_card_face(), scale));
            }
            assert!(!host, "the host's face is macOS's");
            let prepared = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
                &SemanticFontCandidate::default(),
            );
            edge_cuts(raw, ground, rows, (&prepared, scale))
        })
        .into_iter()
        .flatten()
        .collect();
    assert!(found.is_empty(), "lit at the card's edge: {found:#?}");

    let _ = compiled(SettingsPreviewSpec::default());
    let prepared = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
        &SemanticFontCandidate::default(),
    );
    // A control plays its row count as the sweep does, with one law off.
    let with_law_off = |rows: usize, off: &dyn Fn(&mut SceneControls), raw, ground| {
        let kept = SCENE_CONTROLS.with(std::cell::Cell::get);
        let mut changed = kept;
        changed.frame_text = reads_frame_text(rows);
        off(&mut changed);
        SCENE_CONTROLS.with(|controls| controls.set(changed));
        let cuts = edge_cuts(raw, ground, rows, (&prepared, 1.0));
        SCENE_CONTROLS.with(|controls| controls.set(kept));
        cuts
    };
    let drops = with_law_off(5, &|c| c.motes_whole = false, "water", ("dark", dark));
    assert!(
        !drops.is_empty(),
        "negative control: Water's drops drawn cut at the edge"
    );
    for (raw, ground, rows) in [
        ("rainbow", ("dark", dark), 4),
        ("rainbow", ("light", light), 4),
        ("fire", ("dark", dark), 3),
        ("fire", ("light", light), 3),
    ] {
        let cut = with_law_off(rows, &|c| c.rises_kept = false, raw, ground);
        assert!(
            !cut.is_empty(),
            "negative control: {raw} on {} in {rows} rows cut by the box's clip",
            ground.0
        );
    }
}

/// The host's own system face at the card's 14 px, as the Settings window draws its card on
/// macOS (SF Mono, 9 x 16 cells at 1x).
#[cfg(target_os = "macos")]
fn host_card_face() -> crate::tray_raster::PreparedSemanticFont {
    let mut host =
        aterm_render::Renderer::from_system_with_family(None, 14.0, aterm_render::Theme::default())
            .expect("the host's system face");
    host.prepare_semantic_typography("Bold Italic != => ");
    crate::tray_raster::prepared_semantic_font_from_renderer_for_test(host)
}

/// The styles whose scene draws a companion, resident or flying.
fn companion_styles() -> Vec<&'static str> {
    // The pet stands beside any style: each of its looks is judged beside the rainbow, beside
    // one other trail and beside the plain cursor.
    vec!["off", "rainbow", "comet"]
}

/// The companion looks the card is judged in: the default (an adult) and the smallest
/// and largest ages, whose heads differ in height by two fifths.
fn judged_looks() -> [aterm_effects::kitty_registry::KittyLook; 3] {
    use aterm_effects::genome::CatAge;
    let look = aterm_effects::kitty_registry::KittyLook::DEFAULT;
    [
        look,
        aterm_effects::kitty_registry::KittyLook {
            age: CatAge::Kitten,
            ..look
        },
        aterm_effects::kitty_registry::KittyLook {
            age: CatAge::Elder,
            ..look
        },
    ]
}

/// The styles, each on its ground, whose cards run the real blur when
/// [`the_cards_letters_read_under_every_light`] judges every style: whichever frames light the
/// mask [`aterm_render::bloom::LetterMask`] reads the cells and the caret alone, not the style, so
/// these hold it for the rest.
const REAL_BLUR: [(&str, &str); 6] = [
    ("comet", "dark"),
    ("comet", "light"),
    ("rainbow", "dark"),
    ("rainbow", "light"),
    ("laser", "dark"),
    ("fire", "light"),
];

/// Every letter a trail or companion lies on in the card keeps 4.5:1, judged as the
/// trail lab's `trail-contrast` judges one ([`judged_letters`]: the lower of the ring round
/// its strokes and its core against its bed, on the card's clean twin's pixels, no light and
/// no companion drawn), in every style
/// on dark and light at every frame of the scene's loop,
/// and the companions in every look [`judged_looks`] names, in the embedded face and, on
/// macOS, in the host's own system face at the card's 14 px (SF Mono, 9 x 16 cells: the
/// face the settings window draws its card in, which the trail gallery photographs): the
/// scene hands its trail the frame's text and what its face lays of each glyph as the
/// terminal's host does (`pipeline_native::step_cursor_family`,
/// `CursorGlow::observe_glyph_ink`), so the letter guard, the dust's air and the dark
/// rim's law read the letters, and its companions keep off them. Classic's dark face is
/// v0.28's, byte for byte: its spectrum streak runs over the letter it
/// leaves. The negative controls: without the frame's text, Laser's bolt on dark lay on
/// letters at 1:1; with every glyph's ink read as its colour whole, SF Mono's one-column
/// stems showed the light laid under them, and Sparkle's landing on GitHub Light left the
/// `m` of `README.md` at 4.30:1 (the gallery's 4.17, looks9's settings round 2); kept by the
/// ring alone, the `.` beside it read 3.43 of its own 3.90 under Fire's landing, whose flame
/// stands in the cell's upper rows clear of the dot's few solid pixels. Under the
/// page law the card draws the glass where its face lays its letters full
/// (`host::PageFacts::glass`, `docs/design/PRISM-GLASS.md` §15.8 amendment 54): on macOS the
/// host's face at scale 2, so at `prism_glass::LIVE` Glass every frame of that card is judged
/// on the glass. The page law's own bar is the glass's, held by aterm-render's
/// `the_glass_draws_where_the_windows_face_lays_its_letters_full` and its control: judged by
/// the ring round their strokes, the card's letters keep 4.5:1 on a glass laid whatever the
/// face lays as well (every style, both faces, both scales, round 27), which the card's
/// control under the whole cell's median ground had denied. Every look, style and frame is
/// judged at display scale 1 and 2. The judged letters under the control print with
/// `--nocapture`.
///
/// A frame whose gate holds the bloom or spares its letters lays no blur on a pixel this judge
/// reads, so only the styles in [`REAL_BLUR`] run it (each asserted to light a frame through it);
/// a card whose gate opens runs it from that frame on, whatever its style.
#[test]
fn the_cards_letters_read_under_every_light() {
    let _ = compiled(SettingsPreviewSpec::default());
    let embedded = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
        &SemanticFontCandidate::default(),
    );
    let faces = vec![("the embedded face", embedded)];
    #[cfg(target_os = "macos")]
    let faces = {
        let mut faces = faces;
        faces.push(("the host's system face", host_card_face()));
        faces
    };
    let light = PreviewTerminalTheme::from_scheme(
        &aterm_types::scheme::builtin("GitHub Light").expect("built-in light theme"),
    );
    let dark = PreviewTerminalTheme::from_scheme(&aterm_types::ColorScheme::default());
    let under = |raw: &str,
                 (side, theme): (&str, PreviewTerminalTheme),
                 look: Option<aterm_effects::kitty_registry::KittyLook>,
                 (face, prepared, scale): (
        &str,
        &crate::tray_raster::PreparedSemanticFont,
        f32,
    )| {
        let mut under = Vec::new();
        let mut glass_frames = 0;
        let mut judged_glass_frames = 0;
        let mut retained_word_frames = 0;
        let mut blooms = REAL_BLUR.contains(&(raw, side));
        let mut blurred_frames = 0;
        for phase in (0..CURSOR_SCENE_CYCLE_MS).step_by(MOTION_STEP_MS as usize) {
            let _ = aterm_render::test_counters::take_glass_slabs();
            let _ = aterm_render::test_counters::take_bloom_pixels();
            let mut card = lit_card(
                raw,
                (theme, blooms, look),
                (phase, 7, Twin::Clean),
                (prepared, scale),
            );
            let gate = card.input.bloom_gate;
            if !blooms && !(gate.held() || gate.spares_letters()) {
                blooms = true;
                card = lit_card(
                    raw,
                    (theme, true, look),
                    (phase, 7, Twin::Clean),
                    (prepared, scale),
                );
            }
            let LitCard {
                lit,
                twin,
                input: lit_input,
                cell,
                without_body,
            } = card;
            blurred_frames += usize::from(aterm_render::test_counters::take_bloom_pixels() > 0);
            let glass = aterm_render::test_counters::take_glass_slabs() > 0;
            glass_frames += usize::from(glass);
            if glass && let Some(without_body) = without_body {
                assert_eq!(
                    (lit.width, lit.height),
                    (without_body.width, without_body.height),
                    "the body-only twin has the actual card's geometry"
                );
                let mut final_glass = Vec::new();
                lit_input
                    .glow_under_ramps
                    .expand_into(&lit_input.glow_under, &mut final_glass);
                let retained = judged_letters(&lit, &twin, &lit_input, cell)
                    .into_iter()
                    .any(|(reading, bare, row, col)| {
                        let bar = LEGIBLE.min(OWN_SHARE * bare);
                        reading.is_finite()
                            && bar.is_finite()
                            && bar > 0.0
                            && reading >= bar
                            && lit_input.cells[row][col].ch.is_ascii_alphabetic()
                            && final_glass.iter().any(|q| {
                                if usize::from(q.row) >= lit_input.rows {
                                    return false;
                                }
                                let (x0, y0) = (col * cell.0, row * cell.1);
                                let lo = x0.max(usize::from(q.x));
                                let hi = (x0 + cell.0)
                                    .min(lit.width)
                                    .min(usize::from(q.x) + usize::from(q.w));
                                let top = y0.max(usize::from(q.y));
                                let bottom = (y0 + cell.1)
                                    .min(lit.height)
                                    .min(usize::from(q.y) + usize::from(q.h));
                                (lo..hi).any(|x| {
                                    aterm_render::glow_lerp(
                                        u32::from(q.alpha),
                                        u32::from(q.alpha2),
                                        (x - usize::from(q.x)) as u32,
                                        u32::from(q.w),
                                    ) > 0
                                        && (top..bottom).any(|y| {
                                            frame_rgb(&lit, x, y) != frame_rgb(&without_body, x, y)
                                        })
                                })
                            })
                    });
                retained_word_frames += usize::from(retained);
            }
            if let Some((reading, bar, row, col)) = worst_lit_letter(&lit, &twin, &lit_input, cell)
            {
                assert!(reading.is_finite() && bar.is_finite() && bar > 0.0);
                judged_glass_frames += usize::from(glass);
                if reading < bar {
                    let age = look.map(|look| look.age);
                    under.push(format!(
                        "{raw} {side} {age:?} in {face} at {scale}x at {phase} ms: the letter \
                             at row {row} col {col} reads {reading:.2}:1, under its {bar:.2}"
                    ));
                }
            }
        }
        if raw == "rainbow" && side == "light" && scale == 2.0 {
            let page = aterm_effects::host::PageFacts {
                thin_ink: card_thin_ink(theme, prepared, scale),
                ..aterm_effects::host::PageFacts::OPAQUE
            };
            if page.glass() {
                assert!(
                    glass_frames > 0 && judged_glass_frames > 0 && retained_word_frames > 0,
                    "{face} at {scale}x: an admitted page must retain nonzero final glass beneath an ordinary word and pass its finite unchanged judge in that frame"
                );
            }
        }
        (under, blurred_frames)
    };
    let companions = companion_styles();
    assert!(
        companions
            .iter()
            .all(|raw| crate::prefs::CURSOR_TRAIL_STYLES.contains(raw)),
        "the companion styles: {companions:?}"
    );
    let mut items = Vec::new();
    for (at, (face, _)) in faces.iter().enumerate() {
        for scale in [1.0, 2.0] {
            for &raw in crate::prefs::CURSOR_TRAIL_STYLES {
                for ground in [("dark", dark), ("light", light)] {
                    if raw == "classic" && ground.0 == "dark" {
                        continue;
                    }
                    if companions.contains(&raw) {
                        for look in judged_looks() {
                            items.push((*face, at == 1, scale, raw, ground, Some(look)));
                        }
                    } else {
                        items.push((*face, at == 1, scale, raw, ground, None));
                    }
                }
            }
        }
    }
    // Each item plays on its own thread, with that thread's fonts and scene.
    let judged = aterm_spec::check::each(&items, |&(face, host, scale, raw, ground, look)| {
        let _ = compiled(SettingsPreviewSpec::default());
        #[cfg(target_os = "macos")]
        if host {
            return under(raw, ground, look, (face, &host_card_face(), scale));
        }
        assert!(!host, "the host's face is macOS's");
        let prepared = crate::tray_raster::prepared_semantic_font_for_direct_view_test(
            &SemanticFontCandidate::default(),
        );
        under(raw, ground, look, (face, &prepared, scale))
    });
    let found: Vec<&String> = judged.iter().flat_map(|(under, _)| under).collect();
    assert!(
        found.is_empty(),
        "{} frames hold a letter under its bar: {:#?}",
        found.len(),
        &found[..found.len().min(24)]
    );
    // The list above is names: a renamed style would end its blur here in silence.
    for (raw, side) in REAL_BLUR {
        assert!(
            crate::prefs::CURSOR_TRAIL_STYLES.contains(&raw),
            "{raw} is no style of the card, so its blur holds no mask"
        );
        for (face, _) in &faces {
            let blurred: usize = items
                .iter()
                .zip(&judged)
                .filter(|((f, _, _, r, ground, _), _)| *f == *face && *r == raw && ground.0 == side)
                .map(|(_, (_, blurred))| blurred)
                .sum();
            assert!(
                blurred > 0,
                "{raw} on {side} in {face} blurred no frame, so its blur holds no mask"
            );
        }
    }
    let set = |change: &dyn Fn(&mut SceneControls)| {
        SCENE_CONTROLS.with(|controls| {
            let mut next = controls.get();
            change(&mut next);
            controls.set(next);
        });
    };
    set(&|c| c.frame_text = false);
    let (face, prepared) = &faces[0];
    let (unread, _) = under("laser", ("dark", dark), None, (face, prepared, 1.0));
    set(&|c| c.frame_text = true);
    assert!(
        !unread.is_empty(),
        "negative control: blind to the frame's letters, Laser's bolt lies on one"
    );
    let page_takes = |prepared, scale| {
        let thin_ink = card_thin_ink(light, prepared, scale);
        let page = aterm_effects::host::PageFacts {
            thin_ink,
            ..aterm_effects::host::PageFacts::OPAQUE
        };
        (page.glass(), thin_ink)
    };
    let (face, prepared) = &faces[0];
    let (glass, thin_ink) = page_takes(prepared, 1.0);
    assert!(
        !glass,
        "{face} at 1x: the page law takes no glass ({thin_ink:.4})"
    );
    #[cfg(target_os = "macos")]
    {
        let (face, prepared) = &faces[1];
        let (glass, thin_ink) = page_takes(prepared, 2.0);
        assert!(
            glass,
            "{face} at 2x: the page law takes the glass, so at `LIVE` Glass the card's judge \
             reads that card on it ({thin_ink:.4})"
        );
        set(&|c| c.glyph_ink = false);
        let (solid, _) = under("sparkle", ("light", light), None, (face, prepared, 1.0));
        set(&|c| c.glyph_ink = true);
        assert!(
            !solid.is_empty(),
            "negative control: each glyph's ink read as its colour whole, Sparkle's landing \
             leaves a letter of the host's face under its bar"
        );
        set(&|c| c.letter_whole = false);
        let (ringed, _) = under("fire", ("light", light), None, (face, prepared, 1.0));
        set(&|c| c.letter_whole = true);
        assert!(
            !ringed.is_empty(),
            "negative control: kept by the ring round its strokes alone, the `.` of \
             `README.md` under Fire's landing keeps its reading on GitHub Light"
        );
    }
}

/// W10 (the after-land forward round), BEAM'S BODY KEEPS A DIM CARD'S LETTERS: on Solarized
/// Dark, a dim page whose text reads under 4.5, Beam's body under its letters is a page shade
/// (`cursor_glow::bed::page_lit_body_ink`), which the letter guard holds to the letters it
/// reaches (`CursorGlow::hold_page_shades`). Every frame of Beam's card loop there, in the
/// embedded face and, on macOS, the host's, at display scale 1 and 2, keeps every letter at
/// its bar ([`LEGIBLE`], or [`OWN_SHARE`] of its own reading where that is lower), judged as
/// [`the_cards_letters_read_under_every_light`] judges one ([`worst_lit_letter`]). Kept whole,
/// the body's edge put the page's green and its own blue in one ring, whose per-channel
/// medians read a ground brighter than either: the trail gallery's card read the `&&` of
/// `git add README.md &&` (4.220 of its own, bar 4.093) at 4.03 to 4.04 on the landed
/// after-land tree, and reads 4.16 held, three captures of three (macOS, 2026-11-11).
/// Negative control (`SceneControls::shades_held`): each page shade kept whole, the `&&` falls
/// under its bar at display scale 1 in each face, 15 frames of the loop: in the host's face to
/// 4.020 to 4.092 against 4.095, as the gallery read it, and in the embedded face to 3.862
/// against 3.934. At scale 2 none falls either way (macOS, 2026-11-11).
#[test]
fn beam_s_body_keeps_a_dim_card_s_letters() {
    let _ = compiled(SettingsPreviewSpec::default());
    let dim = PreviewTerminalTheme::from_scheme(
        &aterm_types::scheme::builtin("Solarized Dark").expect("built-in dim theme"),
    );
    // Every frame of Beam's loop on Solarized Dark in `prepared` at `scale` whose worst letter
    // reads under its bar.
    let under =
        |(face, prepared, scale): (&str, &crate::tray_raster::PreparedSemanticFont, f32)| {
            let mut blooms = false;
            let mut under = Vec::new();
            for phase in (0..CURSOR_SCENE_CYCLE_MS).step_by(MOTION_STEP_MS as usize) {
                let at = (phase, 7, Twin::Clean);
                let mut card = lit_card("beam", (dim, blooms, None), at, (prepared, scale));
                let gate = card.input.bloom_gate;
                if !blooms && !(gate.held() || gate.spares_letters()) {
                    blooms = true;
                    card = lit_card("beam", (dim, true, None), at, (prepared, scale));
                }
                if let Some((reading, bar, row, col)) =
                    worst_lit_letter(&card.lit, &card.twin, &card.input, card.cell)
                    && reading < bar
                {
                    under.push(format!(
                        "{face} at {scale}x at {phase} ms: the letter at row {row} col {col} \
                     reads {reading:.3}:1, under its {bar:.3}"
                    ));
                }
            }
            under
        };
    let face = |host: bool| {
        #[cfg(target_os = "macos")]
        if host {
            return ("the host's system face", host_card_face());
        }
        assert!(!host, "the host's face is macOS's");
        (
            "the embedded face",
            crate::tray_raster::prepared_semantic_font_for_direct_view_test(
                &SemanticFontCandidate::default(),
            ),
        )
    };
    let mut items = vec![(false, 1.0), (false, 2.0)];
    if cfg!(target_os = "macos") {
        items.extend([(true, 1.0), (true, 2.0)]);
    }
    // Each item plays on its own thread, with that thread's fonts and scene.
    let found: Vec<String> = aterm_spec::check::each(&items, |&(host, scale)| {
        let _ = compiled(SettingsPreviewSpec::default());
        let (name, prepared) = face(host);
        under((name, &prepared, scale))
    })
    .into_iter()
    .flatten()
    .collect();
    assert!(
        found.is_empty(),
        "{} frames of Beam's card on Solarized Dark hold a letter under its bar: {:#?}",
        found.len(),
        &found[..found.len().min(24)]
    );
    let set = |held: bool| {
        SCENE_CONTROLS.with(|controls| {
            controls.set(SceneControls {
                shades_held: held,
                ..controls.get()
            });
        });
    };
    for &(host, scale) in items.iter().filter(|&&(_, scale)| scale == 1.0) {
        set(false);
        let (name, prepared) = face(host);
        let whole = under((name, &prepared, scale));
        set(true);
        assert!(
            !whole.is_empty(),
            "negative control: each page shade kept whole, Beam's body on Solarized Dark keeps \
             every letter of {name} at {scale}x at its bar"
        );
    }
}

/// The rainbow beside the flier (`cursor_pet = "flying cat"`): no walker, and a head its
/// momentum flies in.
fn flying_cat() -> CursorPreviewSpec {
    CursorPreviewSpec {
        pet: None,
        pet_flies: true,
        ..rainbow_cursor("rainbow")
    }
}

/// The flier is a pet of its own: the preview draws it beside the rainbow, the one trail
/// whose momentum flies it in, and beside no other style.
#[test]
fn the_flying_cat_is_the_preview_s_companion_beside_the_rainbow_alone() {
    assert_eq!(
        flying_cat().trail_companion(),
        PreviewTrailCompanion::FlyingKitty
    );
    let beside_fire = CursorPreviewSpec {
        trail_style: PreviewTrailStyle::parse("fire"),
        trail_style_raw: Arc::from("fire"),
        ..flying_cat()
    };
    assert_eq!(beside_fire.trail_companion(), PreviewTrailCompanion::None);
    assert_ne!(
        SettingsPreviewSpec::cursor(flying_cat()).paint_fingerprint(),
        SettingsPreviewSpec::cursor(CursorPreviewSpec {
            pet: None,
            ..rainbow_cursor("rainbow")
        })
        .paint_fingerprint(),
        "the flier cannot alias the pet off in the retained preview cache"
    );
}

#[test]
fn the_flying_cat_rides_beside_the_caret_on_its_row() {
    let (cell_w, cell_h) = (8_i32, 17_i32);
    let raw = "the flying cat";
    for &phase in &[1_500, 2_300] {
        let input = played(
            &SettingsPreviewSpec::cursor(flying_cat()).with_phase(phase),
            Theme::default(),
        );
        let row = input.cursor_row as i32;
        assert_eq!(
            input.cursor_row,
            prompt_row(&input),
            "the caret types on the prompt"
        );
        let caret_x = input.cursor_col as i32 * cell_w;
        let sprite = sprites(&input)
            .into_iter()
            .max_by_key(|sprite| u32::from(sprite.w) * u32::from(sprite.h))
            .unwrap_or_else(|| panic!("{raw} at {phase} ms draws its companion"));
        assert!(
            sprite.x >= caret_x && sprite.x <= caret_x + 2 * cell_w,
            "{raw} at {phase} ms starts {} px from the caret",
            sprite.x - caret_x
        );
        assert!(
            sprite.y >= (row - 1) * cell_h && sprite.y + i32::from(sprite.h) <= (row + 2) * cell_h,
            "{raw} at {phase} ms stays on the caret's row: y {}..{}",
            sprite.y,
            sprite.y + i32::from(sprite.h)
        );
    }
}

#[test]
fn reduced_motion_preview_suppresses_the_flying_cat_s_flight() {
    let reduced = played(
        &SettingsPreviewSpec::cursor(flying_cat()).with_reduced_motion(true),
        Theme::default(),
    );
    assert!(
        sprites(&reduced).is_empty(),
        "ordinary flying motion stays suppressed"
    );
    let live = played(
        &SettingsPreviewSpec::cursor(flying_cat()).with_phase(2_300),
        Theme::default(),
    );
    assert!(
        !sprites(&live).is_empty(),
        "negative control: the flier is drawable outside Reduced Motion"
    );
}

#[test]
fn the_flying_cat_arrives_partway_through_the_typing() {
    let at = |cursor: CursorPreviewSpec, phase: u64| {
        played(
            &SettingsPreviewSpec::cursor(cursor).with_phase(phase),
            Theme::default(),
        )
    };
    for phase in [0, 700] {
        let early = at(flying_cat(), phase);
        assert!(
            sprites(&early).is_empty(),
            "no flier yet at {phase} ms: {} sprites",
            sprites(&early).len()
        );
        assert!(
            !sprites(&at(rainbow_cursor("rainbow"), phase)).is_empty(),
            "negative control: the walking cat is there at {phase} ms"
        );
    }
    assert!(!sprites(&at(flying_cat(), 2_300)).is_empty());
}

/// The flying cat keeps off the scene's letters: no frame of the loop lays a drawn texel
/// of its head on a lettered cell or the caret's, on dark and light, in every look
/// [`judged_looks`] names, in a roomy card and a short one. The head is taller than its
/// row and centred on it, so seated by its row's letters alone
/// (`rainbow_kitty::companion::placement` before it read the rows beside its own: the
/// negative control) its ears lay on the letters of the line above, the card's `git c`
/// at 1.25:1 (looks9's settings round 1).
#[test]
fn the_flying_cat_keeps_off_the_scenes_letters() {
    let light = PreviewTerminalTheme::from_scheme(
        &aterm_types::scheme::builtin("GitHub Light").expect("built-in light theme"),
    );
    let cell = (8, 17);
    let grounds = [("dark", None), ("light", Some(light))];
    let on_letters = |unread: bool| {
        aterm_effects::rainbow_kitty::companion::set_rows_beside_unread(unread);
        let mut on_letters = Vec::new();
        let mut drawn = 0_usize;
        for &(side, ground) in &grounds {
            for look in judged_looks() {
                for rows in [None, Some(4)] {
                    for phase in (0..CURSOR_SCENE_CYCLE_MS).step_by(MOTION_STEP_MS as usize) {
                        let spec = SettingsPreviewSpec::cursor(CursorPreviewSpec {
                            blink: false,
                            ..flying_cat()
                        })
                        .with_post_fx(CursorPostFxSpec {
                            companion_look: look,
                            ..CursorPostFxSpec::default()
                        });
                        let spec = match ground {
                            Some(theme) => spec.with_terminal_theme(theme),
                            None => spec,
                        }
                        .with_phase(phase)
                        .normalized();
                        let theme =
                            ground.map_or_else(Theme::default, PreviewTerminalTheme::into_theme);
                        let (base, _) = build_terminal_specimen_input_rows(&spec, theme, rows);
                        let input = CursorScene::new(&spec, theme, rows).decorate(&base, cell);
                        let Some(head) = sprites(&input)
                            .into_iter()
                            .filter(|sprite| sprite.alpha > 0)
                            .max_by_key(|sprite| u32::from(sprite.w) * u32::from(sprite.h))
                        else {
                            continue;
                        };
                        drawn += 1;
                        let caret = (input.cursor_row, input.cursor_col);
                        let lettered: Vec<(usize, usize)> = inked_cells(&head, cell)
                            .into_iter()
                            .filter(|&(row, col)| {
                                (row, col) == caret
                                    || input
                                        .cells
                                        .get(row)
                                        .and_then(|cells| cells.get(col))
                                        .is_some_and(|cell| !matches!(cell.ch, ' ' | '\0'))
                            })
                            .collect();
                        if !lettered.is_empty() {
                            on_letters.push(format!(
                                "{side} {:?} rows {rows:?} at {phase} ms: on {lettered:?}",
                                look.age
                            ));
                        }
                    }
                }
            }
        }
        aterm_effects::rainbow_kitty::companion::set_rows_beside_unread(false);
        (on_letters, drawn)
    };
    let (control, _) = on_letters(true);
    let (law, drawn) = on_letters(false);
    assert!(drawn >= 100, "the flying cat is drawn: {drawn} frames");
    assert!(
        law.is_empty(),
        "{} frames: {:#?}",
        law.len(),
        &law[..law.len().min(8)]
    );
    assert!(
        !control.is_empty(),
        "negative control: seated by its row's letters alone, its ears lie on the line above"
    );
}
