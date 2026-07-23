// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Visual proof-of-life for SIXEL rendering: feed a small sixel image (top band
// red, bottom band green) through a Terminal, render it on the GPU, and write a
// PNG. Build with the sixel feature so the decoder is compiled in:
//   cargo run -p aterm-gpu --example sixel_shot --features aterm-core/sixel -- /tmp/sixel.png

use aterm_core::terminal::Terminal;
use aterm_gpu::GpuRenderer;
use aterm_render::Theme;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/aterm_sixel_shot.png".to_string());
    let (rows, cols) = (12u16, 40u16);
    let mut term = Terminal::new(rows, cols);

    let mut r = GpuRenderer::new(18.0, Theme::default()).expect("create GpuRenderer");
    let (cw, ch) = r.cell_size();
    // The image footprint is computed from the engine's cell-pixel size; set it so
    // the sixel raster maps onto a whole number of cells.
    term.set_cell_pixel_size(cw as u16, ch as u16);

    // A label, then a sixel image at the home column: 40x12 px, top 6 rows red
    // (#0), bottom 6 rows green (#1). `!40~` = 40 columns of a full 6-px band.
    term.process(b"sixel below:\r\n");
    term.process(b"\x1bPq#0;2;100;0;0#1;2;0;100;0#0!40~-#1!40~\x1b\\");

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
