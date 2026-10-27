// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The D3D11 arm's device-removed latch where a frame writes resident state
//! before it draws (docs/AUDIT-third-party-2026-10-15.md GW.5): a removed
//! device refuses those writes, and the frame must fail by name and reach
//! `device_lost`, which sends the frontend to the CPU renderer, never panic:
//! the failed frame has no other arm.

use super::replay_fixtures::*;
use super::*;

/// A renderer armed on the D3D11 arm, without the wall-clock layers; COULD
/// NOT RUN with no device.
fn armed() -> GpuRenderer {
    let ctx = GpuContext::new()
        .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no GPU context: {e}")));
    let cpu = Renderer::from_system_with_family(None, 18.0, Theme::default())
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no system monospace font"));
    let mut gpu = GpuRenderer::from_parts(ctx, cpu, None, Theme::default())
        .expect("the GPU renderer assembles on an acquired device");
    gpu.arm_d3d11();
    if !gpu.d3d11_arm_ready() {
        aterm_spec::host::could_not_run(format!(
            "no D3D11 device: {}",
            gpu.d3d11_mint_error.as_deref().unwrap_or("unknown")
        ));
    }
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    gpu
}

/// `input` with one more Glow layer whose sprite is an art the fx atlas has
/// not seen, so the frame appends it to the resident atlas.
fn with_fresh_art(input: &RenderInput) -> RenderInput {
    use aterm_core::render::{
        FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxSlot, FxSpace, FxSprite, FxTexels,
        PxRect,
    };
    let art = Arc::new(
        FxArt::new(
            4,
            4,
            FxTexels::Index8,
            vec![2, 1, 0, 1, 1, 2, 1, 0, 0, 1, 2, 1, 1, 0, 1, 2],
        )
        .expect("sized"),
    );
    let mut out = input.clone();
    out.fx_layers.push(FxLayer::new(
        99,
        Arc::new(FxContent::new(
            vec![FxItem::Sprite(FxSprite {
                x: 10,
                y: 10,
                dw: 12,
                dh: 12,
                flip_x: false,
                art,
                ink: 0,
                clip: PxRect::EVERYWHERE,
            })],
            vec![
                FxInk::add(0x0040_2010),
                FxInk::over(0x0030_4020, 0xa0),
                FxInk::over(0x0010_2030, 0xff),
            ],
        )),
        FxSlot::Glow,
        FxSpace::Window,
        FxBlend::Exact,
    ));
    out
}

/// An fx atlas that grows on a removed device fails the frame, not the
/// process: the second frame appends an art the atlas has not seen, the
/// removed device refuses the upload, the frame does not draw on D3D11, and
/// the renderer reports the device lost with the drill as its cause. The
/// control: the same two frames on a live device draw on D3D11 and the
/// second uploads the new art's bytes, so the drilled frame reaches the
/// upload. Before the upload was fallible, the drilled frame panicked with
/// `device layer: d3d11: the device was removed`.
#[test]
fn an_fx_atlas_growing_on_a_removed_device_reaches_device_lost() {
    let mut control = armed();
    let (_, input, _) = fx_fixture(&control);
    let grown = with_fresh_art(&input);
    let mut win = WindowGpu::new();
    control
        .try_render_input(&mut win, &input, None)
        .expect("the control's first frame");
    assert!(control.last_frame_on_d3d11(), "the control draws on D3D11");
    control
        .try_render_input(&mut win, &grown, None)
        .expect("the control's second frame");
    assert!(
        control.last_frame_on_d3d11(),
        "the control's grown frame draws on D3D11"
    );
    assert!(
        control.fx.atlas_bytes > 0,
        "the grown frame appends the new art to the atlas"
    );
    assert!(!control.device_lost());

    let mut drilled = armed();
    let mut win = WindowGpu::new();
    drilled
        .try_render_input(&mut win, &input, None)
        .expect("the drilled renderer's first frame");
    assert!(drilled.last_frame_on_d3d11(), "the first frame is on D3D11");
    drilled
        .d3d11_arm
        .as_ref()
        .expect("minted")
        .dev
        .shared()
        .inject_device_loss();
    // `Err` (no other arm draws it), and no panic.
    let after = drilled.try_render_input(&mut win, &grown, None);
    assert!(
        !drilled.last_frame_on_d3d11(),
        "the grown frame on a removed device does not draw on D3D11 ({:?})",
        after.as_ref().map(|_| ())
    );
    assert!(
        drilled.device_lost(),
        "a removed device reaches device_lost"
    );
    let reason = drilled.device_loss_reason().unwrap_or_default();
    assert!(reason.contains("injected-loss drill"), "{reason}");
}
