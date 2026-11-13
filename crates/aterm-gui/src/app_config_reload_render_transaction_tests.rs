// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Config, FontConfig, ReloadRenderSnapshot, RenderKnobs};

/// The rollback writer ([`ReloadRenderSnapshot::rollback`]) reinstalls the colours it
/// captured, so a window's light latch, keyed by the configured scheme, sees the scheme
/// change on the frame after a candidate's colours and again on the frame after the
/// rollback, which re-enters at once (`docs/design/PRISM-GLASS.md` §15.2 1.3).
#[cfg(unix)]
#[test]
fn a_rollback_writes_the_scheme_back_and_raises_scheme_changed() {
    use crate::trail_rig::TrailRig;
    use std::time::{Duration, Instant};
    let font =
        aterm_tempfile::manifest_dir().join("../aterm-render/tests/fixtures/jetbrains-mono.ttf");
    let mut rig = TrailRig::new(&font).expect("the rig");
    let t0 = rig.t0();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    rig.configure(
        "trail_sounds = false\ncursor_trail_style = \"rainbow\"\nmotion = \"full\"\n\
         theme = \"Default Light\"\n",
        t0,
    )
    .expect("the config admits");
    rig.grid(8, 40, t0).expect("the grid");
    rig.pty(b"\x1b[4;1H$ ", t0);
    let latch = |rig: &mut TrailRig, now: Instant| {
        rig.frame(now).expect("a presented frame");
        rig.with_app(now, |app, wid| app.windows[&wid].trail_ground.last())
    };
    assert!(latch(&mut rig, at(1)).on, "Default Light admits");
    let previous = rig.with_app(at(50), |app, _| {
        let previous = ReloadRenderSnapshot::capture(app);
        // A candidate's colours, installed before its rebuild fails.
        app.config.theme = Some("Dracula".to_string());
        app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
        previous
    });
    let candidate = latch(&mut rig, at(66));
    assert!(
        candidate.scheme_changed && !candidate.on && candidate.fell,
        "{candidate:?}"
    );
    rig.with_app(at(100), |app, _| previous.rollback(app));
    let back = latch(&mut rig, at(116));
    assert!(back.scheme_changed && back.on, "{back:?}");
    let steady = latch(&mut rig, at(150));
    assert!(steady.on && !steady.scheme_changed, "{steady:?}");
}

#[test]
fn failed_backend_rebuild_rolls_back_complete_render_slice_only() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let old_config = app.config.clone();
    let old_default_font_size = app.default_font_size;
    let old_font_size = app.font_size;
    let old_font_px = app.font_px;
    let old_theme = app.theme;
    let old_family = app.font_family.clone();
    let old_shaping = app.text_shaping.clone();
    let old_blending = app.text_blending;
    let old_thicken = app.font_thicken;
    let old_stem_gamma = app.stem_gamma;
    let old_variations = app.font_variations.clone();
    let old_dark_nudge = app.font_weight_dark_nudge;
    let old_knobs = app.render_knobs;
    let old_font_config = app.font_config.clone();
    let old_typography = app.typography.clone();
    let old_size_scale = app.font_size_scale_milli;
    let old_metrics = app.windows[&wid].metrics;
    let previous = ReloadRenderSnapshot::capture(&app);

    let candidate: Config = aterm_toml::from_str(
        r##"
font_px = 23.0
theme = "Dracula"
foreground = "#f0f1f2"
background = "#101112"
cursor_color = "#aabbcc"
selection_color = "#334455"
palette = ["#010203", "#040506"]
font_family = "Menlo"
font_family_bold = "Menlo Bold"
font_family_italic = "Menlo Italic"
font_family_bold_italic = "Menlo Bold Italic"
font_synthetic_style = false
fallback_fonts = ["Menlo"]
symbol_font = "Menlo"
emoji_font = "Menlo"
ligatures = false
merged_ligatures = true
font_features = ["ss01"]
text_blending = "linear"
font_thicken = true
font_variation = ["wght=550"]
font_weight = 600
font_weight_dark_nudge = 40.0
stem_gamma = 0.7
line_height = 1.4
font_size_scale = 1.1
adjust_baseline = 3
adjust_underline_position = 2
adjust_underline_thickness = 1
underline_skip_descenders = false
minimum_contrast = 7.0
selection_foreground = "#eeeeee"
selection_inactive = true
cursor_break_ligatures = true
background_opacity = 0.8
background_material = "hud"
window_padding = 31.0
window_padding_top = 7.0
copy_on_select = true
"##,
    )
    .expect("candidate config");
    app.config = candidate.clone();
    app.default_font_size = 23.0;
    app.font_size = 23.0;
    app.font_px = 23.0;
    app.theme = candidate.theme_for(app.os_appearance);
    app.font_family = Some("Menlo".to_string());
    app.text_shaping = candidate.text_shaping();
    app.text_blending = candidate.text_blending_or_default();
    app.font_thicken = candidate.font_thicken_or_default();
    app.stem_gamma = candidate.stem_gamma_or_default();
    let (variations, _) = candidate.font_variation_requests();
    app.font_variations = variations;
    app.font_weight_dark_nudge = candidate.font_weight_dark_nudge_or_default();
    app.render_knobs = RenderKnobs::from_config(
        &candidate,
        &candidate.resolved_colors(app.os_appearance, &app.config_assets.themes, None),
    );
    app.font_config = FontConfig::from_config(&candidate).0;
    app.typography = crate::resolved_typography::ResolvedTypography {
        id: "Dracula".to_string(),
        applied: aterm_types::scheme::parse_theme_str(
            "fg = #ffffff\nbg = #000000\nline_height = 1.5\n",
        )
        .expect("a typography theme")
        .typography,
        ..crate::resolved_typography::ResolvedTypography::none(app.os_appearance)
    };
    app.font_size_scale_milli = candidate.font_size_scale_milli_or_default();
    assert_ne!(
        app.typography, old_typography,
        "candidate typography is live"
    );
    app.refresh_all_window_metrics();
    assert_ne!(app.windows[&wid].metrics, old_metrics, "candidate is live");

    assert!(
        !app.finish_reload_render_transaction_with(previous, |candidate_app| {
            assert_eq!(candidate_app.font_px, 23.0, "failure sees candidate");
            assert_eq!(candidate_app.render_knobs.line_height, 1.4);
            false
        })
    );

    let mut expected = old_config.clone();
    expected.copy_on_select = Some(true);
    assert!(
        app.config == expected,
        "config authority rolls back the render slice only"
    );
    assert_eq!(app.default_font_size, old_default_font_size);
    assert_eq!(app.font_size, old_font_size);
    assert_eq!(app.font_px, old_font_px);
    assert_eq!(app.theme, old_theme);
    assert_eq!(app.font_family, old_family);
    assert_eq!(app.text_shaping, old_shaping);
    assert_eq!(app.text_blending, old_blending);
    assert_eq!(app.font_thicken, old_thicken);
    assert_eq!(app.stem_gamma, old_stem_gamma);
    assert_eq!(app.font_variations, old_variations);
    assert_eq!(app.font_weight_dark_nudge, old_dark_nudge);
    assert_eq!(app.render_knobs, old_knobs);
    assert_eq!(app.font_config, old_font_config);
    assert_eq!(app.typography, old_typography);
    assert_eq!(app.font_size_scale_milli, old_size_scale);
    assert_eq!(app.windows[&wid].metrics, old_metrics);
}

#[test]
fn a_same_cell_box_face_rebuild_repaints_the_cpu_window() {
    use aterm_render::DamageOutcome;

    fn present_cpu(
        app: &mut crate::App,
        input: &aterm_render::RenderInput,
    ) -> (Vec<u32>, DamageOutcome) {
        let crate::App {
            backend, windows, ..
        } = app;
        let crate::Backend::Cpu(renderer) = backend.ready_mut() else {
            panic!("headless test backend is CPU")
        };
        let ws = windows
            .get_mut(&crate::WindowId(0))
            .expect("headless fixture window");
        let _ = renderer.render_input_cached(&mut ws.cpu_cache, input);
        (
            ws.cpu_cache.presented_pixels().to_vec(),
            ws.cpu_cache.last_damage(),
        )
    }

    let mut app = crate::App::headless_for_test();
    let mut term = aterm_core::terminal::Terminal::new(3, 8);
    term.process(b"\x1b[?25lABC");
    let input = term.cell_frame(3, 8);
    let _ = present_cpu(&mut app, &input);
    assert_eq!(
        present_cpu(&mut app, &input).1,
        DamageOutcome::GateHit,
        "negative control: an identical frame really is cached"
    );
    let cell = app.backend.cell_size();

    let mut prepared = aterm_render::Renderer::from_system(app.font_px, app.theme)
        .expect("system font for the prepared generation");
    let _ = prepared.set_font_variations(&[(u32::from_be_bytes(*b"wght"), 800.0)], 0.0);
    let _ = prepared.seal_admitted_font_sources();
    let chrome = crate::tray_raster::prepare_chrome_fonts(None, None, None);
    assert!(app.rebuild_backend_with_prepared(Some((prepared, chrome))));
    assert_eq!(
        app.backend.cell_size(),
        cell,
        "the case needs an unchanged cell box"
    );

    let (pixels, damage) = present_cpu(&mut app, &input);
    assert_eq!(damage, DamageOutcome::Full, "the face rebuild repaints");
    let crate::Backend::Cpu(renderer) = app.backend.ready_mut() else {
        panic!("headless test backend is CPU")
    };
    assert!(
        pixels == renderer.render_input(&input).pixels,
        "the window shows what the new face renders"
    );
}
