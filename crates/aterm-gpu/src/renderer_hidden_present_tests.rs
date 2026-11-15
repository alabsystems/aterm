// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_core::terminal::Terminal;
use aterm_render::Theme;

/// A window surface no swapchain was attached to, for a test of the present path in front of the
/// swapchain: the Metal arm refuses it as `Validation` once a present gets that far.
fn unattached_surface() -> GpuSurface {
    GpuSurface {
        acquire_id: next_surface_acquire_id(),
        neutral: NeutralSurfaceConfig {
            width: 64,
            height: 64,
            format: crate::device_layer::TexelFormat::Bgra8Unorm,
        },
        supports_f16: false,
        post_mult: true,
        copyable: true,
        metal: None,
    }
}

#[test]
fn the_metal_arm_refuses_a_hidden_windows_present_before_any_work() {
    let mut gpu = gpu_renderer_for_test(None, 18.0, Theme::default());
    gpu.arm_metal_for_test();
    let (rows, cols) = (4usize, 10usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"hidden");
    let input = term.cell_frame(rows, cols);
    let mut surf = unattached_surface();
    let mut win = WindowGpu::new();
    // The arm itself, behind `present_input`'s refusal of a headless context, which
    // `gpu_renderer_for_test` is.
    let mut present = |gpu: &mut GpuRenderer, win: &mut WindowGpu| {
        gpu.metal_present_input_with_crop(win, &mut surf, &input, false, None, None, None, 0)
    };
    assert_eq!(
        present(&mut gpu, &mut win),
        Err(SurfacePresentFailure::Validation),
        "CONTROL: a visible window's present reaches the arm, which finds no swapchain"
    );
    win.set_occluded_hint(true);
    assert_eq!(
        present(&mut gpu, &mut win),
        Err(SurfacePresentFailure::Occluded),
        "a hidden window's present is refused first"
    );
    win.set_frame_awaited(true);
    assert_eq!(
        present(&mut gpu, &mut win),
        Err(SurfacePresentFailure::Validation),
        "a frame the frontend waits on reaches the arm"
    );
    win.set_frame_awaited(false);
    win.set_capture_present(true);
    assert_eq!(
        present(&mut gpu, &mut win),
        Err(SurfacePresentFailure::Validation),
        "and so does a capture's"
    );
}
