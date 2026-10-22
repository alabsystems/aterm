// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    asymmetric_pad_cells, clamp_visible_pad_top, crop_visible_frame, gpu_visible_crop_top,
    project_visible_pad_crop, try_shift_window_absolute_effects_y, visible_frame_height,
};
use aterm_core::render::FreeSprite;
use aterm_render::{FirePatch, Frame, GlowQuad, RainHalo, RenderInput};

#[test]
fn asymmetric_row_fit_is_the_inverse_of_visible_frame_height() {
    for cell in 1..=31 {
        for pad in 0..=20 {
            for pad_top in 0..=pad {
                for cells in 1..=40 {
                    let exact = cells * cell + pad_top + pad;
                    assert_eq!(asymmetric_pad_cells(exact, pad_top, pad, cell), cells);
                    for remainder in 0..cell {
                        assert_eq!(
                            asymmetric_pad_cells(exact + remainder, pad_top, pad, cell,),
                            cells
                        );
                    }
                }
            }
        }
    }

    // Negative control: the symmetric `pad_split` inverse loses a row at this height.
    let exact = 24 * 14 + 2 + 12;
    assert_eq!(asymmetric_pad_cells(exact, 2, 12, 14), 24);
    assert_eq!(aterm_render::pad_split(exact, 12, 14).cells, 23);
}

#[test]
fn gpu_crop_alignment_is_exact_for_every_parity_and_crop_direction() {
    for pad in 0..=20 {
        for pad_top in 0..=pad {
            let removed = pad - pad_top;
            let (head, grid_height) = (7usize, 13 * 17);
            let raw_height = head + grid_height + 2 * pad;
            let visible_height = visible_frame_height(raw_height, pad, pad_top);
            for destination_height in 1..=(visible_height + 2 * 17) {
                let crop_top = gpu_visible_crop_top(raw_height, destination_height, pad, pad_top);
                let crop_bottom = removed - crop_top;
                let raw_pad_top = pad_top + crop_top;

                assert_eq!(
                    aterm_render::band_offset_y(destination_height, raw_height)
                        + i64::try_from(head + raw_pad_top).unwrap(),
                    aterm_render::band_offset_y(destination_height, visible_height)
                        + i64::try_from(head + pad_top).unwrap(),
                    "GPU and visible grid origins diverged: pad={pad} top={pad_top} dest={destination_height}",
                );
                let raw_bottom = raw_height - head - grid_height - raw_pad_top;
                assert_eq!(raw_bottom - crop_bottom, pad);
            }
        }
    }

    // Odd removed padding + odd row remainder is where a fixed ceil(delta/2)
    // compensation is one pixel wrong.
    let (pad, pad_top, grid_height) = (11, 2, 100);
    let raw_height = grid_height + 2 * pad;
    let visible_height = visible_frame_height(raw_height, pad, pad_top);
    let destination_height = visible_height + 5;
    assert_eq!(
        gpu_visible_crop_top(raw_height, destination_height, pad, pad_top),
        if cfg!(target_os = "linux") { 0 } else { 4 }
    );
    assert_eq!((pad - pad_top).div_ceil(2), 5);
}

/// Drives the real clamp and crop helpers over the model's whole bounded lattice and validates
/// one Crop transition through the tiered conformance validator.
#[test]
fn visible_pad_crop_shipping_helpers_conform_to_model() {
    let model = aterm_models_render::visible_pad_crop_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let choices = model.successors("ChooseGeometry", &model.init_state());
    assert_eq!(choices.len(), 4 * 5 * 4 * 3, "complete bounded lattice");
    // The projected `pad_top` is this renderer's clamp: the CPU backend's own, and the one
    // the GPU renderer's `pad_top`/`set_pad_top` delegate to.
    let Some(mut renderer) =
        aterm_render::Renderer::from_system(16.0, aterm_render::Theme::default())
    else {
        aterm_spec::host::could_not_run("no system monospace font on this host");
    };

    let mut validated_fixture = false;
    let mut seam_fixture = false;
    for picked in choices {
        let pad = picked["pad"] as usize;
        let request = picked["request"] as usize;
        let grid = picked["grid"] as usize;
        let head = picked["head"] as usize;
        let top = clamp_visible_pad_top(pad, request);
        renderer.set_pad(pad);
        renderer.set_pad_top(top);
        let rendered_top = renderer.pad_top();
        let raw_height = grid + head + 2 * pad;

        // The CPU present crops with `Backend::pad_top`, the renderer's own.
        // Two columns, each pixel its row number: a crop that strode by rows
        // instead of rows × width would land the wrong row at an edge.
        let rows =
            |height: usize| -> Vec<u32> { (0..height as u32).flat_map(|row| [row, row]).collect() };
        let mut cpu = Frame {
            width: 2,
            height: raw_height,
            pixels: rows(raw_height),
        };
        crop_visible_frame(&mut cpu, 0, pad, rendered_top);
        let projected = project_visible_pad_crop(
            pad,
            request,
            rendered_top,
            top,
            grid,
            head,
            raw_height,
            cpu.height,
        );
        let expected = model.successors("Crop", &picked);
        assert_eq!(expected.len(), 1);
        assert_eq!(projected, expected[0], "CPU crop drift: {picked:?}");
        assert_eq!(cpu.pixels.first().copied(), Some(0));
        assert_eq!(cpu.pixels.last().copied(), Some((cpu.height - 1) as u32));

        // The GPU may crop removed rows above the raw source to align a centred
        // destination, but its exposed height and edge algebra match the CPU's
        // for every parity.
        let destination_height = cpu.height.saturating_add((grid + head) % 3);
        let crop_top = gpu_visible_crop_top(raw_height, destination_height, pad, top);
        let mut gpu = Frame {
            width: 2,
            height: raw_height,
            pixels: rows(raw_height),
        };
        crop_visible_frame(&mut gpu, crop_top, pad, top);
        assert_eq!(gpu.height, cpu.height);
        assert_eq!(
            gpu.pixels.len(),
            2 * gpu.height,
            "the crop keeps whole rows"
        );
        assert_eq!(gpu.pixels.first().copied(), Some(crop_top as u32));
        assert_eq!(
            gpu.pixels.last().copied(),
            Some((crop_top + gpu.height - 1) as u32)
        );

        if pad == 3 && request == 1 && grid == 4 && head == 2 {
            let (ok, evidence) = aterm_spec::verify::validate_transition_tiered(
                &model,
                &[],
                &picked,
                &projected,
                Some("Crop"),
                "GUI visible-padding crop conformance",
            );
            assert!(ok, "real Crop transition must conform\n{evidence}");

            // Negative control: the uncropped raw frame leaves a `2*pad - top` bottom.
            let exposed_raw = project_visible_pad_crop(
                pad,
                request,
                rendered_top,
                top,
                grid,
                head,
                raw_height,
                raw_height,
            );
            assert_eq!(exposed_raw["visible_pad_bottom"], 5);
            assert!(!model.check_invariant("VisibleBottomIsBasePad", &exposed_raw));
            let (mutant_ok, _) = aterm_spec::verify::validate_transition_tiered(
                &model,
                &[],
                &picked,
                &exposed_raw,
                Some("Crop"),
                "GUI visible-padding uncropped negative control",
            );
            assert!(!mutant_ok, "uncropped raw transport must not conform");
            validated_fixture = true;
        }

        // Negative control for `VisibleTopMatchesRendererTop` (the `Buggy=1` Crop): an
        // unclamped top through the real GPU path, where the renderer still clamps.
        if pad == 2 && request == 4 && grid == 1 && head == 0 {
            let slipped_top = request;
            renderer.set_pad_top(slipped_top);
            let slipped_rendered = renderer.pad_top();
            let crop_top = gpu_visible_crop_top(
                raw_height,
                visible_frame_height(raw_height, pad, slipped_top),
                pad,
                slipped_top,
            );
            let mut unclamped = Frame {
                width: 1,
                height: raw_height,
                pixels: (0..raw_height as u32).collect(),
            };
            crop_visible_frame(&mut unclamped, crop_top, pad, slipped_top);
            let seam_defect = project_visible_pad_crop(
                pad,
                request,
                slipped_rendered,
                slipped_top,
                grid,
                head,
                raw_height,
                unclamped.height,
            );
            assert_eq!(
                (seam_defect["pad_top"], seam_defect["visible_pad_top"]),
                (2, 4),
                "the renderer clamps while the visible authority keeps the request"
            );
            assert_eq!(buggy.successors("Crop", &picked), vec![seam_defect.clone()]);
            assert!(!model.check_invariant("VisibleTopMatchesRendererTop", &seam_defect));
            let (mutant_ok, _) = aterm_spec::verify::validate_transition_tiered(
                &model,
                &[],
                &picked,
                &seam_defect,
                Some("Crop"),
                "GUI visible-padding unclamped-seam negative control",
            );
            assert!(!mutant_ok, "an unclamped visible top must not conform");
            seam_fixture = true;
        }
    }
    assert!(validated_fixture, "tiered fixture must be reachable");
    assert!(seam_fixture, "the unclamped-seam fixture must be reachable");
}

#[test]
fn gpu_effect_shift_covers_only_absolute_streams_and_restores_exactly() {
    let mut input = RenderInput::empty();
    input.cursor_glow_add = vec![GlowQuad {
        y: 20,
        alpha: 0,
        ..GlowQuad::default()
    }];
    input.glow_under = vec![GlowQuad {
        y: 21,
        alpha: 0,
        ..GlowQuad::default()
    }];
    input.glow_halo = vec![RainHalo {
        y: 22,
        cy: 23,
        ..RainHalo::default()
    }];
    input.fire_patch = vec![FirePatch {
        y: 24,
        base_y: 25,
        ..FirePatch::default()
    }];
    // Renderer-offset/grid-relative streams must not be double-shifted.
    input.nova_add = vec![GlowQuad {
        y: 30,
        alpha: 0,
        ..GlowQuad::default()
    }];
    input.rain_add = vec![RainHalo {
        y: 31,
        cy: 32,
        ..RainHalo::default()
    }];
    input.free_sprites = vec![FreeSprite {
        y: 34,
        ..FreeSprite::default()
    }];
    let original = input.clone();

    assert!(try_shift_window_absolute_effects_y(&mut input, 7));
    assert_eq!(input.cursor_glow_add[0].y, 27);
    assert_eq!(input.glow_under[0].y, 28);
    assert_eq!((input.glow_halo[0].y, input.glow_halo[0].cy), (29, 30));
    assert_eq!(
        (input.fire_patch[0].y, input.fire_patch[0].base_y),
        (31, 32)
    );
    assert_eq!(input.nova_add[0].y, 30);
    assert_eq!((input.rain_add[0].y, input.rain_add[0].cy), (31, 32));
    assert_eq!(input.free_sprites[0].y, 34);

    assert!(try_shift_window_absolute_effects_y(&mut input, -7));
    assert_eq!(input, original, "+crop/-crop must restore every byte");

    // An impossible source shift degrades without moving effects, and does
    // so before mutation, so restoration can never corrupt coordinates.
    let mut invalid = RenderInput::empty();
    invalid.cursor_glow_add = vec![GlowQuad {
        y: u16::MAX,
        alpha: 0,
        ..GlowQuad::default()
    }];
    let before = invalid.clone();
    assert!(!try_shift_window_absolute_effects_y(&mut invalid, 1));
    assert_eq!(
        invalid, before,
        "preflight rejects without partial mutation"
    );
}

/// An fx layer of one rect, the ramp `c0 → c1` across it.
fn fx_rect_layer(
    id: u64,
    (x, y, w, h): (i32, i32, u16, u16),
    space: aterm_core::render::FxSpace,
) -> aterm_core::render::FxLayer {
    use aterm_core::render::{FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot};
    FxLayer::new(
        id,
        std::sync::Arc::new(FxContent::new(
            vec![FxItem::Rect(FxRect { x, y, w, h, ink: 0 })],
            vec![
                FxInk::over(0x0040_2010, 0x90),
                FxInk::over(0x0010_3040, 0x40),
            ],
        )),
        FxSlot::Over,
        space,
        FxBlend::Exact,
    )
}

/// THE TRANSPORT SHIFT MOVES THE FX THAT ARE WINDOW PIXELS (design §4.4): a
/// Window-space layer's offset and clip and every occluder move by the GPU
/// source shift, unbounded edges stay unbounded, a Grid-space layer (which
/// follows `grid_top`) stays put, and `+crop` then `-crop` restores every
/// byte. A shift that would push a bounded edge onto an unbounded sentinel
/// (and so not reverse exactly) is refused before any mutation, the u16
/// streams included.
#[test]
fn gpu_effect_shift_moves_window_fx_and_occluders_and_restores_exactly() {
    use aterm_core::render::{FxSpace, PxRect};
    let mut input = RenderInput::empty();
    input.cursor_glow_add = vec![GlowQuad {
        y: 20,
        alpha: 0,
        ..GlowQuad::default()
    }];
    let mut window = fx_rect_layer(1, (0, 0, 30, 9), FxSpace::Window);
    window.offset = (3, 5);
    window.clip = PxRect::new(0, 10, 100, 60);
    let open = fx_rect_layer(2, (4, 4, 10, 10), FxSpace::Window);
    let mut grid = fx_rect_layer(3, (0, 0, 30, 9), FxSpace::Grid);
    grid.offset = (1, 2);
    grid.clip = PxRect::new(0, 0, 50, 40);
    input.fx_layers = vec![window, open, grid];
    input.fx_occluders = vec![
        PxRect::new(0, 8, 40, 20),
        PxRect::new(i32::MIN, i32::MIN, i32::MAX, 30),
    ];
    let original = input.clone();

    assert!(try_shift_window_absolute_effects_y(&mut input, 7));
    assert_eq!(input.fx_layers[0].offset, (3, 12));
    assert_eq!(input.fx_layers[0].clip, PxRect::new(0, 17, 100, 67));
    assert_eq!(input.fx_layers[1].offset, (0, 7));
    assert_eq!(
        input.fx_layers[1].clip,
        PxRect::EVERYWHERE,
        "unbounded stays unbounded"
    );
    assert_eq!(
        input.fx_layers[2], original.fx_layers[2],
        "Grid follows grid_top"
    );
    assert_eq!(
        input.fx_occluders,
        vec![
            PxRect::new(0, 15, 40, 27),
            PxRect::new(i32::MIN, i32::MIN, i32::MAX, 37),
        ]
    );
    assert!(try_shift_window_absolute_effects_y(&mut input, -7));
    assert_eq!(input, original, "+crop/-crop must restore every byte");

    // A bounded edge that would saturate cannot reverse: refused, unmoved.
    let mut edge = original.clone();
    edge.fx_occluders[0].y1 = i32::MAX - 3;
    let before = edge.clone();
    assert!(!try_shift_window_absolute_effects_y(&mut edge, 5));
    assert_eq!(edge, before, "preflight rejects without partial mutation");
}

/// THE TRANSPORT KEEPS WINDOW FX REGISTERED WITH GRID FX under a pad crop:
/// the CPU renderer in the raw geometry `configure_visible_y` gives the GPU
/// (`pad_top` raised by the crop, so the grid sits `crop` rows lower), a
/// Window-space layer authored in visible pixels over the same grid pixels as
/// a Grid-space one, and an occluder authored in visible pixels cutting both:
/// after the transport shift the two frames are byte-identical.
///
/// CONTROLS: without the shift the Window layer lands `crop` rows high, and
/// an occluder left unshifted cuts other pixels of the Grid layer.
#[test]
fn gpu_transport_keeps_window_fx_registered_with_grid_fx() {
    use aterm_core::render::{FxSpace, PxRect};
    let mut r = aterm_render::Renderer::from_bytes(
        aterm_render::embedded_font(),
        14.0,
        aterm_render::Theme::default(),
    )
    .expect("bundled font");
    let (pad, visible_top, crop, head) = (12usize, 4usize, 5usize, 9usize);
    r.set_pad(pad);
    r.set_head(head);
    r.set_pad_top(visible_top + crop);
    assert_eq!(r.grid_top(), visible_top + crop + head, "the raw geometry");
    let (cw, ch) = r.cell_size();
    let (cwi, chi) = (cw as i32, ch as i32);
    let mut t = aterm_core::terminal::Terminal::new(4, 12);
    t.process(b"\x1b[?25lfx over text\r\nsecond row");
    let base = t.cell_frame(4, 12);
    // The visible frame's grid origin, where the host authors window pixels.
    let (gx, gy) = (pad as i32, (visible_top + head) as i32);
    let item = (0, 0, (6 * cw) as u16, (2 * ch) as u16);
    let mut grid = fx_rect_layer(1, item, FxSpace::Grid);
    grid.offset = (cwi, chi / 2);
    grid.clip = PxRect::new(0, 0, 5 * cwi, 3 * chi);
    let mut window = fx_rect_layer(1, item, FxSpace::Window);
    window.offset = (gx + cwi, gy + chi / 2);
    window.clip = grid.clip.translate(gx, gy);
    let occluder = PxRect::new(gx + 2 * cwi, gy + chi, gx + 4 * cwi, gy + 2 * chi);
    let render =
        |r: &mut aterm_render::Renderer, layer: &aterm_core::render::FxLayer, shift: bool| {
            let mut input = base.clone();
            input.fx_layers = vec![layer.clone()];
            input.fx_occluders = vec![occluder];
            let original = input.clone();
            if shift {
                assert!(try_shift_window_absolute_effects_y(&mut input, crop as i32));
            }
            let pixels = r.render_input(&input).pixels;
            if shift {
                assert!(try_shift_window_absolute_effects_y(
                    &mut input,
                    -(crop as i32)
                ));
                assert_eq!(input, original, "the shift reverses exactly");
            }
            pixels
        };
    let from_grid = render(&mut r, &grid, true);
    assert!(
        from_grid == render(&mut r, &window, true),
        "the shifted Window layer registers with the Grid layer"
    );
    assert!(
        from_grid != render(&mut r, &window, false),
        "CONTROL: unshifted, the Window layer lands crop rows high"
    );
    assert!(
        from_grid != render(&mut r, &grid, false),
        "CONTROL: an unshifted occluder cuts other pixels"
    );
}
