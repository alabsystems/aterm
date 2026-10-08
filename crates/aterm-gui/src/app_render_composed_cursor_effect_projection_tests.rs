// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_core::render::{CharFg, FireHaloCell, FirePatch, GlowQuad, RainHalo, TrailCell};

#[test]
fn every_composed_cursor_channel_is_hard_clipped_and_clean_is_a_real_negative_control() {
    let flat = [
        GlowQuad {
            row: 2,
            x: 5,
            y: 25,
            w: 10,
            h: 10,
            color: 0x0012_3456,
            // Additive light (see `GlowQuad::alpha`).
            alpha: 0,
            color2: 0x0012_3456,
            alpha2: 0,
        },
        GlowQuad {
            row: 0,
            x: 1,
            y: 1,
            w: 2,
            h: 2,
            color: 1,
            // Additive light (see `GlowQuad::alpha`).
            alpha: 0,
            color2: 1,
            alpha2: 0,
        },
    ];
    let halos = [
        RainHalo {
            row: 2,
            x: 5,
            y: 25,
            w: 10,
            h: 10,
            cx: 7,
            cy: 27,
            rx: 9,
            ry: 9,
            ..RainHalo::default()
        },
        RainHalo {
            x: 1,
            y: 1,
            w: 2,
            h: 2,
            ..RainHalo::default()
        },
    ];
    let patches = [
        FirePatch {
            row: 2,
            x: 5,
            y: 25,
            w: 10,
            h: 10,
            base_y: 55,
            ..FirePatch::default()
        },
        FirePatch {
            x: 1,
            y: 1,
            w: 2,
            h: 2,
            ..FirePatch::default()
        },
    ];
    let char_fg = [
        CharFg {
            row: 2,
            col: 5,
            fg: 0x00AA_BBCC,
        },
        CharFg {
            row: 1,
            col: 5,
            fg: 1,
        },
    ];
    let fire_halo = [
        FireHaloCell {
            row: 3,
            col: 6,
            strength: 200,
        },
        FireHaloCell {
            row: 4,
            col: 6,
            strength: 1,
        },
    ];
    let trail = [
        TrailCell {
            row: 2,
            col: 6,
            alpha: 220,
        },
        TrailCell {
            row: 2,
            col: 7,
            alpha: 1,
        },
    ];
    let mut input = RenderInput::empty();
    project_focused_composed_cursor_effects(
        &mut input,
        FocusedComposedCursorEffects {
            glow_add: &flat,
            glow_halo: &halos,
            fire_patch: &patches,
            glow_under: &flat,
            char_fg: &char_fg,
            fire_halo: &fire_halo,
            trail: &trail,
            trail_color: 0x00DE_ADBE,
            active_cursor_fill: Some(0x0050_FA7B),
        },
        Some(ComposedCursorEffectClip {
            row0: 2,
            row1: 4,
            col0: 5,
            col1: 7,
            x0: 10,
            y0: 30,
            x1: 20,
            y1: 40,
        }),
        Some(CursorStyle::HollowBlock),
        Some(CursorStyle::Bolt),
    );

    for quad in input.cursor_glow_add.iter().chain(input.glow_under.iter()) {
        assert_eq!((quad.x, quad.y, quad.w, quad.h), (10, 30, 5, 5));
    }
    assert_eq!(input.cursor_glow_add.len(), 1);
    assert_eq!(input.glow_under.len(), 1);
    assert_eq!(input.glow_halo.len(), 1);
    assert_eq!(
        (
            input.glow_halo[0].x,
            input.glow_halo[0].y,
            input.glow_halo[0].w,
            input.glow_halo[0].h,
        ),
        (10, 30, 5, 5)
    );
    assert_eq!(
        (input.glow_halo[0].cx, input.glow_halo[0].cy),
        (7, 27),
        "radial centre remains absolute while its coverage is clipped"
    );
    assert_eq!(input.fire_patch.len(), 1);
    assert_eq!(
        (
            input.fire_patch[0].x,
            input.fire_patch[0].y,
            input.fire_patch[0].w,
            input.fire_patch[0].h,
            input.fire_patch[0].base_y,
        ),
        (10, 30, 5, 5, 55),
        "fire root remains absolute while its coverage is clipped"
    );
    assert_eq!(input.char_fg.as_slice(), &char_fg[..1]);
    assert_eq!(input.fire_halo.as_slice(), &fire_halo[..1]);
    assert_eq!(input.cursor_trail.as_slice(), &trail[..1]);
    assert_eq!(input.cursor_trail_color, 0x00DE_ADBE);
    assert_eq!(
        input.cursor_fill_override,
        Some(INACTIVE_CURSOR_FILL),
        "inactive hollow focus law wins over the active Forge fill"
    );
    assert_ne!(
        input.cursor_style,
        CursorStyle::Bolt,
        "an effect body must never overwrite terminal DECSCUSR"
    );
    assert_eq!(
        input.cursor_effect_style_override,
        Some(CursorStyle::Bolt),
        "the retained effect shape rides its host-owned channel"
    );
    assert_eq!(
        aterm_render::effective_cursor_style(&input, Some(CursorStyle::HollowBlock)),
        CursorStyle::HollowBlock,
        "inactive HollowBlock presentation outranks an effect-owned body style"
    );

    input.clear_overlays();
    assert!(
        input.cursor_glow_add.is_empty()
            && input.glow_halo.is_empty()
            && input.fire_patch.is_empty()
            && input.glow_under.is_empty()
            && input.char_fg.is_empty()
            && input.fire_halo.is_empty()
            && input.cursor_trail.is_empty()
            && input.cursor_effect_style_override.is_none()
            && input.cursor_fill_override.is_none(),
        "clean capture strips every non-vacuously populated cursor-effect channel"
    );
}

#[test]
fn composed_status_owner_follows_the_fill_that_survives_host_projection() {
    use aterm_effects::cursor_glow::{BlockFill, BlockFillOwner};

    let owned = BlockFill {
        owner: BlockFillOwner::Phaser,
        fill: 0x0066_44FF,
        base: Some(0x0050_FA7B),
    };
    let clip = Some(ComposedCursorEffectClip {
        row0: 0,
        row1: 1,
        col0: 0,
        col1: 1,
        x0: 0,
        y0: 0,
        x1: 8,
        y1: 16,
    });
    let project = |input: &mut RenderInput, cursor_override| {
        project_focused_composed_cursor_effects(
            input,
            FocusedComposedCursorEffects {
                glow_add: &[],
                glow_halo: &[],
                fire_patch: &[],
                glow_under: &[],
                char_fg: &[],
                fire_halo: &[],
                trail: &[],
                trail_color: 0,
                active_cursor_fill: Some(owned.fill),
            },
            clip,
            cursor_override,
            None,
        );
    };

    let mut input = RenderInput::empty();
    project(&mut input, None);
    assert_eq!(
        presented_block_fill(None, input.cursor_fill_override, Some(owned)),
        Some(owned),
        "the active effect owns the composed caret it painted"
    );

    project(&mut input, Some(CursorStyle::HollowBlock));
    assert_eq!(input.cursor_fill_override, Some(INACTIVE_CURSOR_FILL));
    assert_eq!(
        presented_block_fill(
            Some(CursorStyle::HollowBlock),
            input.cursor_fill_override,
            Some(owned),
        ),
        None,
        "an inactive composed window is owned by the host HollowBlock"
    );
}
