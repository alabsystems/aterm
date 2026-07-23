// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Headless screenshot: feed bytes (or a built-in colourful demo) into the engine
// and write the rendered screen to a PNG. This is aterm's own `read_image` used
// as a screen-capturing env — no display required.
//
//   cargo run -p aterm-render --example shot -- /tmp/aterm_shot.png

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme};

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/aterm_shot.png".to_string());
    let (rows, cols) = (16u16, 72u16);

    let mut term = Terminal::new(rows, cols);
    // A colourful, mixed demo: prompt, a coloured `ls`, attributes, box drawing.
    let demo: &[u8] = b"\x1b[1;32muser@aterm\x1b[0m:\x1b[1;34m~/dev/aterm\x1b[0m$ ls --color\r\n\
\x1b[1;34msrc\x1b[0m  \x1b[1;34mdocs\x1b[0m  \x1b[0;32mCargo.toml\x1b[0m  README.md  \x1b[0;36materm.png\x1b[0m\r\n\
\x1b[31mERROR\x1b[0m text, \x1b[1mbold\x1b[0m, \x1b[3mitalic\x1b[0m, \x1b[4munderline\x1b[0m, \x1b[7minverse\x1b[0m\r\n\
\x1b[38;5;208m256-color orange\x1b[0m and \x1b[38;2;120;200;255mtrue-color sky\x1b[0m\r\n\
\xe2\x94\x8c\xe2\x94\x80\xe2\x94\x80 a 2026 terminal \xe2\x94\x80\xe2\x94\x80\xe2\x94\x90\r\n\
\xe2\x94\x82 \xe6\x97\xa5\xe6\x9c\xac\xe8\xaa\x9e wide \xe2\x94\x82\r\n\
\xe2\x94\x94\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x98\r\n\
$ ";
    term.process(demo);

    let mut r =
        Renderer::from_system(18.0, Theme::default()).expect("no system monospace font found");
    let frame = r.render_input(&term.cell_frame(rows as usize, cols as usize));
    std::fs::write(&path, frame.to_png()).expect("write png");
    eprintln!("wrote {path} ({}x{} px)", frame.width, frame.height);
}
