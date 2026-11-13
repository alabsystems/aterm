// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::App;
use crate::WindowId;
use std::time::Instant;

#[test]
fn disabled_word_decorations_cannot_leak_prior_capture_layers() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.config.cursor_trail = Some(false);
    app.prepare_terminal_capture_grid(wid).unwrap();
    app.sparkle_dirty = false;
    app.sparkle = None;
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        let art = aterm_render::fx::bitmap(
            aterm_core::render::FxTexels::Rgba8Straight,
            1,
            1,
            vec![255; 4],
        )
        .expect("a 1x1 art");
        let tiles = aterm_effects::matrix_rain::RainBaker::default();
        ws.fx.publish_for_test(
            &[aterm_render::sprites::FreeSprite::whole(0, 0, art)],
            (&[], &tiles),
            &mut ws.input_scratch,
        );
        assert_eq!(
            aterm_render::sprites::published(&ws.input_scratch.fx_layers).len(),
            1,
            "fixture: a prior capture left a companion layer"
        );
        let input = &mut ws.input_scratch;
        input.word_decorations.push(aterm_render::WordDecoration {
            row: 0,
            col: 0,
            dx: 0,
            dy: 0,
            glyph: aterm_render::DecoGlyph::Star4,
            blend: aterm_render::DecoBlend::Add,
            color: 0x00FF_FFFF,
            alpha: 255,
        });
        input.ink.push(aterm_render::InkCell {
            row: 0,
            col: 0,
            color: [1, 2, 3],
        });
        input.nova_add.push(aterm_render::GlowQuad::default());
    }

    app.capture_words(wid, Instant::now());

    let input = &app.windows[&wid].input_scratch;
    assert!(input.word_decorations.is_empty());
    assert!(input.ink.is_empty());
    assert!(aterm_render::sprites::published(&input.fx_layers).is_empty());
    assert!(input.nova_add.is_empty());
}

#[test]
fn decoration_refill_restamps_live_default_and_cursor_colors() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.prepare_terminal_capture_grid(wid).unwrap();
    {
        let input = &mut app.windows.get_mut(&wid).unwrap().input_scratch;
        input.default_bg = 0x00AA_BBCC;
        input.cursor_color = 0x00CC_BBAA;
    }
    let terminal = app.front_terminal(wid).unwrap().term.clone();
    crate::term_lock(&terminal).process(b"\x1b]11;#123456\x07\x1b]12;#ABCDEF\x07");

    app.capture_words(wid, Instant::now());

    let input = &app.windows[&wid].input_scratch;
    assert_eq!(input.default_bg, 0x0012_3456);
    assert_eq!(input.cursor_color, 0x00AB_CDEF);

    crate::term_lock(&terminal).process(b"\x1b]21;cursor=\x07\x1b]10;#FEDCBA\x07");
    app.capture_words(wid, Instant::now());
    let input = &app.windows[&wid].input_scratch;
    assert_eq!(
        input.cursor_color, 0x00FE_DCBA,
        "the introspection refill resolves a dynamic cursor from live OSC 10"
    );
}
