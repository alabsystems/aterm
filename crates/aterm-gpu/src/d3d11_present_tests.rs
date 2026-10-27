// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The D3D11 arm's headless presents on their own: a changed input changes
//! the destination, and a removed device drops the present and reaches
//! `device_lost`. The frames themselves are held to their CPU twins
//! (`frame_record_tests`).

use super::replay_fixtures::*;
use super::*;

/// A renderer armed with the D3D11 arm, bloom and shimmer off; COULD NOT RUN
/// when there is no D3D11 device.
fn armed() -> GpuRenderer {
    let ctx = GpuContext::new()
        .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no GPU context: {e}")));
    let cpu = Renderer::from_system_with_family(None, 18.0, Theme::default())
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no system monospace font"));
    let mut gpu = GpuRenderer::from_parts(ctx, cpu, None, Theme::default())
        .expect("the GPU renderer assembles on an acquired device");
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    gpu.arm_d3d11();
    if !gpu.d3d11_arm_ready() {
        aterm_spec::host::could_not_run(format!(
            "no D3D11 device: {}",
            gpu.d3d11_mint_error.as_deref().unwrap_or("unknown")
        ));
    }
    gpu
}

/// The bell inversion, a changed row and the scrim each change the armed
/// D3D11 present, so the record's agreement is the composition's, not two
/// blank destinations'.
#[test]
fn a_changed_present_changes_the_d3d11_destination() {
    let mut armed = armed();
    let mut win = WindowGpu::new();
    let input = fixtures_base_for_cursor();
    let plain = presented(&mut armed, &mut win, &input, false, None, None, None);
    assert!(armed.last_frame_on_d3d11(), "the control presents on D3D11");
    let distinct: std::collections::BTreeSet<u32> = plain.pixels.iter().copied().collect();
    assert!(
        distinct.len() > 2,
        "the plain present draws text, not a flat clear ({} colours)",
        distinct.len()
    );
    let inverted = presented(&mut armed, &mut win, &input, true, None, None, None);
    assert_ne!(
        plain.pixels, inverted.pixels,
        "the bell inversion reaches the destination"
    );
    let card = vec![200u8; 6 * 4 * 4];
    let tray = |scrim| TrayQuad {
        rgba: &card,
        pw: 6,
        ph: 4,
        dx: 2,
        dy: 1,
        scrim,
    };
    let carded = presented(
        &mut armed,
        &mut win,
        &input,
        false,
        None,
        Some(tray(None)),
        None,
    );
    let scrimmed = presented(
        &mut armed,
        &mut win,
        &input,
        false,
        None,
        Some(tray(Some(TrayScrim {
            rgba: [0, 0, 0, 160],
            top: 0,
        }))),
        None,
    );
    assert_ne!(
        carded.pixels, scrimmed.pixels,
        "the scrim reaches the destination"
    );
    let mut changed = input.clone();
    changed.row_rev_lane = 0;
    changed.cells[0][0].ch = 'Q';
    let other = presented(&mut armed, &mut win, &changed, false, None, None, None);
    assert_ne!(
        plain.pixels, other.pixels,
        "a changed cell reaches the destination"
    );
}

/// The device-removed latch through the presents: the injected-loss drill
/// (`ATERM_D3D11_INJECT_LOSS=<n>`) fires after the first armed present, the
/// next frame finds the device removed, that present drops, and the
/// renderer reports the device lost with the drill as its cause, which is
/// what sends the frontend to the CPU renderer (`recover_from_gpu_loss`).
/// The control: the same presents on an undrilled renderer stay on D3D11
/// and the device stays live.
#[test]
fn a_removed_d3d11_device_drops_the_present_and_reaches_device_lost() {
    let input = fixtures_base_for_cursor();
    let mut changed = input.clone();
    changed.row_rev_lane = 0;
    changed.cells[0][0].ch = 'Q';

    let mut control = armed();
    let mut win = WindowGpu::new();
    for i in [&input, &changed] {
        presented(&mut control, &mut win, i, false, None, None, None);
        assert!(
            control.last_frame_on_d3d11(),
            "the control presents on D3D11"
        );
    }
    assert!(!control.device_lost(), "an undrilled device stays live");
    assert_eq!(control.device_loss_reason(), None);

    let mut drilled = armed();
    drilled.d3d11_inject_loss_after_for_test(1);
    let mut win = WindowGpu::new();
    // `present_virtual`, not `presented`: the drill arms the device's next
    // frame, which the helper's snapshot readback would take.
    assert!(
        drilled.present_virtual(&mut win, &input, false, None, None),
        "the first present draws"
    );
    assert!(
        drilled.last_frame_on_d3d11(),
        "the first present draws on D3D11"
    );
    assert!(
        !drilled.device_lost(),
        "the drill fires after the present, not during it"
    );
    // A second window: the lost frame is that window's first, a Full scope.
    let mut win2 = WindowGpu::new();
    assert!(
        !drilled.present_virtual(&mut win2, &changed, false, None, None),
        "the present after the removal drops"
    );
    assert!(
        !drilled.last_frame_on_d3d11(),
        "the frame after the removal did not draw on D3D11"
    );
    assert!(
        drilled.device_lost(),
        "a removed device at a frame's first enter reaches device_lost"
    );
    let reason = drilled.device_loss_reason().unwrap_or_default();
    assert!(reason.contains("injected-loss drill"), "{reason}");
}
