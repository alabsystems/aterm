// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Phase-1 GPU proof of life: acquire a GPU, clear an offscreen texture, read it
// back, write a PNG. Confirms wgpu/Metal works headless on this machine.
//   cargo run -p aterm-gpu --example gpu_probe -- /tmp/aterm_gpu_probe.png

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/aterm_gpu_probe.png".into());
    let ctx = aterm_gpu::GpuContext::new().expect("acquire GPU");
    eprintln!("GPU: {} (backend {})", ctx.adapter_name, ctx.backend);
    // clear to aterm's calm-dark theme background so it matches the CPU renderer
    let frame = ctx.clear_to_frame(320, 120, 0x0011_1318);
    std::fs::write(&path, frame.to_png()).expect("write png");
    // sanity: report the corner pixel actually read back from the GPU
    eprintln!(
        "wrote {path} ({}x{}); pixel[0]=0x{:06X}",
        frame.width, frame.height, frame.pixels[0]
    );
}
