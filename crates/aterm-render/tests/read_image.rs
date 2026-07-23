// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// read_image (ATERM_DESIGN §8): an intelligence reads the ACTUAL rendered pixels
// of the terminal, as a PNG, headless. feed → render → encode → it's a valid,
// decodable image of the right size.

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme};

#[test]
fn read_image_encodes_the_rendered_screen_as_valid_png() {
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font found");
        return;
    };
    let mut term = Terminal::new(4, 8);
    term.process(b"\x1b[31mhi\x1b[0m");
    let frame = r.render_input(&term.cell_frame(4, 8));

    let bytes = frame.to_png();

    // valid PNG signature
    assert_eq!(
        &bytes[..8],
        &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
        "PNG magic"
    );
    assert!(bytes.len() > 100, "non-trivial PNG");

    // round-trips: decodes back to the exact rendered dimensions
    let decoder = png::Decoder::new(std::io::Cursor::new(&bytes));
    let reader = decoder.read_info().expect("decode header");
    let info = reader.info();
    assert_eq!(info.width as usize, frame.width);
    assert_eq!(info.height as usize, frame.height);
}
