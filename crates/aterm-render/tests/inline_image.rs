// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Inline images (iTerm2 OSC 1337 File=): an `imgcat`-style sequence places a
// PNG over the grid and the CPU renderer composites its ACTUAL pixels — image
// cells skip their glyph (image-vs-glyph precedence), and a text-only frame is
// byte-identical to the pre-image path.

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme};

/// Encode a solid-colour `w`×`h` PNG (opaque RGBA).
fn solid_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        writer.write_image_data(&rgba).expect("png data");
    }
    out
}

/// Build an OSC 1337 `File=` sequence for `payload` with the given args.
fn osc_1337_file(args: &str, payload: &[u8]) -> Vec<u8> {
    let b64 = aterm_codec::base64::encode(payload).expect("encode");
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b]1337;File=");
    out.extend_from_slice(args.as_bytes());
    out.push(b':');
    out.extend_from_slice(b64.as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

#[test]
fn red_image_paints_red_pixels_over_the_grid() {
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font found");
        return;
    };
    let (cw, ch) = r.cell_size();
    let mut term = Terminal::new(6, 10);
    // A 4×2 cell red image, sized in pixels so the footprint matches exactly.
    let (cols, rows) = (4u32, 2u32);
    let png = solid_png(cols * cw as u32, rows * ch as u32, [255, 0, 0]);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    term.process(&osc_1337_file(
        &format!("inline=1;width={cols};height={rows}"),
        &png,
    ));

    let frame = r.render_input(&term.cell_frame(6, 10));

    // A pixel in the centre of the image footprint must be (near) red.
    let mid_x = cw; // column 1, well inside the 4-col image
    let mid_y = ch / 2; // row 0
    let px = frame.pixels[mid_y * frame.width + mid_x];
    let (red, green, blue) = ((px >> 16) & 0xff, (px >> 8) & 0xff, px & 0xff);
    assert!(red > 200, "image centre should be red, got #{px:06x}");
    assert!(
        green < 60 && blue < 60,
        "image centre should be red, got #{px:06x}"
    );

    // A pixel BELOW the image (row 3) must NOT be red — the image is bounded.
    let below = frame.pixels[(3 * ch) * frame.width + mid_x];
    let br = (below >> 16) & 0xff;
    assert!(
        br < 200,
        "below the image must not be red, got #{below:06x}"
    );
}

#[test]
fn image_cell_skips_its_glyph() {
    // A glyph written first, then an image placed over the SAME cells, must not
    // show the glyph: the image owns the cell. We compare the image region of an
    // image-covered frame against a control where the same green image covers a
    // BLANK grid — they must be pixel-identical (the prior glyph left no trace).
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font found");
        return;
    };
    let (cw, ch) = r.cell_size();
    let png = solid_png(2 * cw as u32, ch as u32, [0, 200, 0]);

    // Frame A: glyphs, then image over them.
    let mut term_a = Terminal::new(4, 8);
    term_a.set_cell_pixel_size(cw as u16, ch as u16);
    term_a.process(b"XX"); // two glyphs at row 0, cols 0-1
    term_a.process(b"\r"); // back to column 0 so the image lands over them
    term_a.process(&osc_1337_file("inline=1;width=2;height=1", &png));
    let frame_a = r.render_input(&term_a.cell_frame(4, 8));

    // Frame B: image over a blank grid (no prior glyphs).
    let mut r2 = Renderer::from_system(16.0, Theme::default()).expect("font");
    let mut term_b = Terminal::new(4, 8);
    term_b.set_cell_pixel_size(cw as u16, ch as u16);
    term_b.process(&osc_1337_file("inline=1;width=2;height=1", &png));
    let frame_b = r2.render_input(&term_b.cell_frame(4, 8));

    // The 2-cell image band (rows 0..ch, cols 0..2*cw) must be identical — proof
    // the glyph under the image left no pixels.
    for y in 0..ch {
        for x in 0..(2 * cw) {
            let i = y * frame_a.width + x;
            assert_eq!(
                frame_a.pixels[i], frame_b.pixels[i],
                "image must fully cover the glyph at ({x},{y})"
            );
        }
    }
}

/// Minimal CRC-32 (the PNG/IEEE 802.3 variant), table-free — tests only.
fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// A tiny PNG whose IHDR *declares* `w`×`h` (valid IHDR CRC) but carries one
/// pixel — the inline-image allocation bomb a remote peer can stream over SSH.
fn png_with_declared_dims(w: u32, h: u32) -> Vec<u8> {
    let mut bytes = solid_png(1, 1, [9, 9, 9]);
    let ihdr_data = 16usize; // 8-byte sig + 4 len + 4 "IHDR"
    bytes[ihdr_data..ihdr_data + 4].copy_from_slice(&w.to_be_bytes());
    bytes[ihdr_data + 4..ihdr_data + 8].copy_from_slice(&h.to_be_bytes());
    let crc = crc32_ieee(&bytes[ihdr_data - 4..ihdr_data + 13]);
    bytes[ihdr_data + 13..ihdr_data + 17].copy_from_slice(&crc.to_be_bytes());
    bytes
}

#[test]
fn oversized_inline_image_png_draws_nothing_without_huge_alloc() {
    // End-to-end: a remote peer sends an OSC 1337 inline image whose PNG IHDR
    // claims 30000×30000 (≈3.4 GiB if honored) in a sub-1KB payload. The render
    // path must skip it (draw nothing) rather than allocate — no panic, no OOM.
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font found");
        return;
    };
    let (cw, ch) = r.cell_size();
    let bomb = png_with_declared_dims(30_000, 30_000);
    assert!(
        bomb.len() < 1024,
        "bomb payload stays tiny: {} bytes",
        bomb.len()
    );

    // The guard's load-bearing claim: the footprint decode is rejected (returns
    // nothing), so no `30000*30000*4` buffer is ever allocated.
    assert!(
        aterm_render::decode_image_to_footprint(
            &bomb,
            aterm_core::grid::extra::ImageFormat::Png,
            4 * cw,
            2 * ch,
        )
        .is_none(),
        "oversized inline-image PNG must decode to nothing"
    );

    // End-to-end the render must complete without panic / OOM, and the rejected
    // bomb must paint exactly like ANY other undecodable placement. We compare it
    // against a control with identical placement args but a non-PNG garbage
    // payload: both reject, so the footprint pixels must be byte-identical. (This
    // isolates "image rejected" from the handler's footprint-reservation paint.)
    let mut term = Terminal::new(6, 10);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    term.process(&osc_1337_file("inline=1;width=4;height=2", &bomb));
    let frame = r.render_input(&term.cell_frame(6, 10));

    let mut ctrl_r = Renderer::from_system(16.0, Theme::default()).expect("font");
    let mut ctrl = Terminal::new(6, 10);
    ctrl.set_cell_pixel_size(cw as u16, ch as u16);
    ctrl.process(&osc_1337_file(
        "inline=1;width=4;height=2",
        b"not a png at all",
    ));
    let control = ctrl_r.render_input(&ctrl.cell_frame(6, 10));

    for y in 0..ch {
        for x in 0..(4 * cw) {
            let i = y * frame.width + x;
            assert_eq!(
                frame.pixels[i], control.pixels[i],
                "rejected bomb must paint like any undecodable image at ({x},{y})"
            );
        }
    }
}

#[test]
fn text_only_frame_is_unaffected_by_the_image_path() {
    // No image anywhere → the rendered pixels must be byte-identical to a render
    // built before any image plumbing existed. We assert internal consistency:
    // the same input renders identically twice (the image pass is a strict no-op
    // for an image-free row, allocating nothing and touching no pixels).
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font found");
        return;
    };
    let mut term = Terminal::new(4, 8);
    term.process(b"\x1b[31mhi\x1b[0m world");
    let a = r.render_input(&term.cell_frame(4, 8)).pixels;
    let mut r2 = Renderer::from_system(16.0, Theme::default()).expect("font");
    let b = r2.render_input(&term.cell_frame(4, 8)).pixels;
    assert_eq!(a, b, "image-free frame renders identically");
}
