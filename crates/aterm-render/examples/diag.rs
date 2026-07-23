// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Controlled rendering diagnostic — isolate specific cases to verify by eye.
//   cargo run -p aterm-render --example diag -- /tmp/aterm_diag.png

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme};

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/aterm_diag.png".to_string());
    let (rows, cols) = (8u16, 40u16);
    let mut term = Terminal::new(rows, cols);
    let demo: &[u8] = b"plain ABCabc 0123\r\n\
\x1b[7mINVERSE\x1b[0m then normal\r\n\
\x1b[44m blue-bg \x1b[0m then normal\r\n\
box \xe2\x94\x8c\xe2\x94\x80\xe2\x94\x80\xe2\x94\x90 end\r\n\
wide [\xe6\x97\xa5\xe6\x9c\xac] end\r\n\
red \x1b[31mRR\x1b[0m grn \x1b[32mGG\x1b[0m blu \x1b[34mBB\x1b[0m\r\n";
    term.process(demo);
    let mut r = Renderer::from_system(18.0, Theme::default()).expect("font");
    let frame = r.render_input(&term.cell_frame(rows as usize, cols as usize));
    std::fs::write(&path, frame.to_png()).expect("write");
    eprintln!("wrote {path} ({}x{})", frame.width, frame.height);
}
