// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// GPU rendering proof of life: feed the colourful `diag` demo (plain/inverse/
// blue-bg/box-drawing/CJK/red-grn-blu) through a Terminal, render it on the GPU
// via `GpuRenderer`, and write the read-back pixels to a PNG. The PNG should be
// (near-)identical to `aterm-render`'s `diag` output.
//   cargo run -p aterm-gpu --example gpu_shot -- /tmp/aterm_gpu_shot.png

use aterm_core::terminal::Terminal;
use aterm_gpu::GpuRenderer;
use aterm_render::Theme;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/aterm_gpu_shot.png".to_string());
    let (rows, cols) = (8u16, 40u16);
    let mut term = Terminal::new(rows, cols);
    // Same demo bytes as crates/aterm-render/examples/diag.rs.
    let demo: &[u8] = b"plain ABCabc 0123\r\n\
\x1b[7mINVERSE\x1b[0m then normal\r\n\
\x1b[44m blue-bg \x1b[0m then normal\r\n\
box \xe2\x94\x8c\xe2\x94\x80\xe2\x94\x80\xe2\x94\x90 end\r\n\
wide [\xe6\x97\xa5\xe6\x9c\xac] end\r\n\
red \x1b[31mRR\x1b[0m grn \x1b[32mGG\x1b[0m blu \x1b[34mBB\x1b[0m\r\n";
    term.process(demo);

    let mut r = GpuRenderer::new(18.0, Theme::default()).expect("create GpuRenderer");
    let mut win = aterm_gpu::WindowGpu::new();
    let (name, backend) = r.adapter();
    eprintln!("GPU: {name} (backend {backend})");
    let frame = r.render_input(
        &mut win,
        &term.cell_frame(rows as usize, cols as usize),
        None,
    );
    std::fs::write(&path, frame.to_png()).expect("write png");
    eprintln!("wrote {path} ({}x{})", frame.width, frame.height);
}
