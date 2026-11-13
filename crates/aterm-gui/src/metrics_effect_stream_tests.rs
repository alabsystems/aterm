// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Metrics, emit_fx_streams, wire};

#[test]
fn effect_stream_fields_follow_the_final_composed_input() {
    use aterm_core::render::{CharFg, FxBlend, FxContent, FxLayer, FxSlot, FxSpace, InkCell};
    let m = Metrics::new();
    let ctx = wire::ReadCtx::inert(&m, 0);
    let layer = FxLayer::new(
        7,
        std::sync::Arc::new(FxContent::default()),
        FxSlot::Over,
        FxSpace::Grid,
        FxBlend::Exact,
    );
    let input = aterm_render::RenderInput {
        glow_under: vec![aterm_render::GlowQuad::default(); 3],
        nova_add: vec![aterm_render::GlowQuad::default(); 2],
        ink: vec![
            InkCell {
                row: 0,
                col: 0,
                color: [1, 2, 3],
            };
            4
        ],
        char_fg: vec![
            CharFg {
                row: 0,
                col: 0,
                fg: 0
            };
            5
        ],
        fx_layers: vec![layer],
        ..aterm_render::RenderInput::default()
    };
    m.window.redraw.note_fx_streams(&input);
    assert_eq!(
        wire::render_text(|s| emit_fx_streams(&ctx, s)),
        " fx_streams_last=glow_under:3,nova_add:2,ink:4,char_fg:5,fx_layers:1"
    );
    assert_eq!(
        wire::render_json(|s| emit_fx_streams(&ctx, s)),
        "{\"fx_streams_last\":{\"glow_under\":3,\"nova_add\":2,\"ink\":4,\"char_fg\":5,\
         \"fx_layers\":1}}"
    );
    m.window
        .redraw
        .note_fx_streams(&aterm_render::RenderInput::default());
    assert_eq!(
        wire::render_text(|s| emit_fx_streams(&ctx, s)),
        " fx_streams_last=none"
    );
    assert_eq!(
        wire::render_json(|s| emit_fx_streams(&ctx, s)),
        "{\"fx_streams_last\":{}}"
    );
}
