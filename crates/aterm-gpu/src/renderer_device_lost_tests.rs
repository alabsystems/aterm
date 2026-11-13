// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(target_os = "macos")]
#[test]
fn a_wired_metal_loss_latch_feeds_device_lost() {
    use crate::metal::loss::{CbOutcome, LossLatch};
    let gpu = crate::renderer::gpu_renderer_for_test(None, 18.0, aterm_render::Theme::default());
    assert!(!gpu.device_lost(), "a fresh context is healthy");
    let latch = gpu.ctx.metal_loss_latch_or_wire();
    assert!(
        !gpu.device_lost(),
        "a wired but healthy latch reports nothing"
    );
    latch.record(&CbOutcome::Lost {
        code: None,
        name: "injected PageFault stand-in",
    });
    assert!(
        gpu.device_lost(),
        "the injected Metal loss must reach the frontend's existing poll"
    );
    assert!(
        !gpu.ctx
            .wire_metal_loss_latch(std::sync::Arc::new(LossLatch::new())),
        "a second wire refuses — one loss domain per context lifetime"
    );
}
