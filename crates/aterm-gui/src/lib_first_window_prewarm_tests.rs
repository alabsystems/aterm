// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_render::StyleBits;

const FONT: &[u8] = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");

fn sealed_backend_at_base() -> Backend {
    let mut backend = Backend::Cpu(
        Renderer::from_bytes(FONT, FONT_PX, Theme::default()).expect("fixture font parses"),
    );
    backend.seal_admitted_font_sources();
    backend
}

fn prompt_lookups_added(backend: &mut Backend) -> usize {
    let Backend::Cpu(r) = backend else {
        unreachable!("CPU fixture")
    };
    let before = r.glyph_cache_len();
    for ch in ['$', '>', 'a', 'Z', '~'] {
        let regular = r.glyph_key(ch);
        let bold = r.glyph_key_styled(ch, StyleBits::BOLD);
        let _ = r.glyph_image(regular);
        let _ = r.glyph_image(bold);
    }
    r.glyph_cache_len() - before
}

fn active_px(backend: &Backend) -> f32 {
    let Backend::Cpu(r) = backend else {
        unreachable!("CPU fixture")
    };
    r.px()
}

#[test]
fn the_post_seal_warm_is_resident_at_the_first_windows_predicted_px() {
    let retina = font_px_for_scale(FONT_PX, 2.0);
    assert_eq!(retina, (FONT_PX * 2.0).round());
    assert_ne!(
        retina, FONT_PX,
        "a 2x target equal to the base would make the precondition below vacuous"
    );

    let mut base_warm = sealed_backend_at_base();
    base_warm.prewarm_ascii();
    base_warm.activate_px(retina);
    assert!(
        prompt_lookups_added(&mut base_warm) > 0,
        "precondition: a base-px warm is invisible at the Retina target"
    );

    let mut predicted = sealed_backend_at_base();
    predicted.prewarm_ascii_for_first_window(retina, FONT_PX);
    assert_eq!(
        active_px(&predicted),
        active_px(&sealed_backend_at_base()),
        "the helper hands the renderer back at the size it was given"
    );
    predicted.activate_px(retina);
    assert_eq!(
        prompt_lookups_added(&mut predicted),
        0,
        "the prompt's regular and BOLD keys at the first window's px must be cache hits"
    );
}

#[test]
fn the_warm_survives_the_joins_re_pin_of_a_non_default_typography() {
    let retina = font_px_for_scale(FONT_PX, 2.0);
    let defaults = app_config::GlyphRasterKnobs::from_config(&Config::default());
    let deltas: [(&str, app_config::GlyphRasterKnobs); 4] = [
        (
            "line_height",
            app_config::GlyphRasterKnobs {
                line_height: 1.2,
                ..defaults.clone()
            },
        ),
        (
            "font_thicken",
            app_config::GlyphRasterKnobs {
                font_thicken: true,
                ..defaults.clone()
            },
        ),
        (
            "stem_gamma",
            app_config::GlyphRasterKnobs {
                stem_gamma: 0.85,
                ..defaults.clone()
            },
        ),
        (
            "adjust_baseline",
            app_config::GlyphRasterKnobs {
                adjust_baseline: 2,
                ..defaults.clone()
            },
        ),
    ];
    for (knob, knobs) in &deltas {
        let mut join_only = sealed_backend_at_base();
        join_only.prewarm_ascii_for_first_window(retina, FONT_PX);
        join_only.pin_glyph_raster_knobs(knobs);
        join_only.activate_px(retina);
        assert!(
            prompt_lookups_added(&mut join_only) > 0,
            "precondition: a {knob} pinned first at the join discards the warm"
        );

        let mut worker_pinned = sealed_backend_at_base();
        worker_pinned.pin_glyph_raster_knobs(knobs);
        worker_pinned.prewarm_ascii_for_first_window(retina, FONT_PX);
        worker_pinned.pin_glyph_raster_knobs(knobs);
        worker_pinned.activate_px(retina);
        assert_eq!(
            prompt_lookups_added(&mut worker_pinned),
            0,
            "{knob}: the join's re-pin of the worker's values must keep the warm"
        );
    }
}
