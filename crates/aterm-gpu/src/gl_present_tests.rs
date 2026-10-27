// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GL arm's headless presents on their own: a changed input changes the
//! destination, and a lost context drops the present and reaches
//! `device_lost`. The frames themselves are held to their CPU twins
//! (`frame_record_tests`).

use super::replay_fixtures::*;
use super::*;

/// A renderer armed with the GL arm, bloom and shimmer off.
fn armed() -> GpuRenderer {
    let ctx = GpuContext::new()
        .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no GPU context: {e}")));
    let cpu = Renderer::from_system_with_family(None, 18.0, Theme::default())
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no system monospace font"));
    let mut gpu = GpuRenderer::from_parts(ctx, cpu, None, Theme::default())
        .expect("the GPU renderer assembles on an acquired device");
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    gpu.arm_gl();
    gpu
}

/// The bell inversion and a changed row each change the armed GL present, so
/// the record's agreement is the composition's, not two blank destinations'.
#[test]
fn a_changed_present_changes_the_gl_destination() {
    let mut armed = armed();
    let mut win = WindowGpu::new();
    let input = fixtures_base_for_cursor();
    let plain = presented(&mut armed, &mut win, &input, false, None, None, None);
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
    let mut changed = input.clone();
    changed.row_rev_lane = 0;
    changed.cells[0][0].ch = 'Q';
    let other = presented(&mut armed, &mut win, &changed, false, None, None, None);
    assert_ne!(
        plain.pixels, other.pixels,
        "a changed cell reaches the destination"
    );
}

/// The lost-context latch through the presents: the injected-loss drill
/// (`ATERM_GL_INJECT_LOSS=<n>`) fires after the first armed present, the next
/// frame's `eglMakeCurrent` fails as a lost context's does, that present drops,
/// and the renderer reports the device lost. The control: the same presents on
/// an undrilled renderer stay on GL and the device stays live.
#[test]
fn a_lost_gl_context_reaches_device_lost() {
    let input = fixtures_base_for_cursor();
    let mut changed = input.clone();
    changed.row_rev_lane = 0;
    changed.cells[0][0].ch = 'Q';

    let mut control = armed();
    let mut win = WindowGpu::new();
    for i in [&input, &changed] {
        presented(&mut control, &mut win, i, false, None, None, None);
        assert!(control.last_frame_on_gl(), "the control presents on GL");
    }
    assert!(!control.device_lost(), "an undrilled device stays live");
    assert_eq!(control.device_loss_reason(), None);

    let mut drilled = armed();
    drilled.gl_inject_loss_after_for_test(1);
    let mut win = WindowGpu::new();
    assert!(
        drilled.present_virtual(&mut win, &input, false, None, None),
        "the first present draws"
    );
    assert!(drilled.last_frame_on_gl(), "the first present draws on GL");
    assert!(
        !drilled.device_lost(),
        "the drill fires after the present, not during it"
    );
    let mut win2 = WindowGpu::new();
    assert!(
        !drilled.present_virtual(&mut win2, &changed, false, None, None),
        "the present after the loss drops"
    );
    assert!(
        !drilled.last_frame_on_gl(),
        "the lost frame did not draw on GL"
    );
    assert!(
        drilled.device_lost(),
        "a lost context at a frame's first eglMakeCurrent reaches device_lost"
    );
    let reason = drilled.device_loss_reason().unwrap_or_default();
    assert!(reason.contains("EGL_CONTEXT_LOST"), "{reason}");
}
