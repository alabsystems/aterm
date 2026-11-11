// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Frame;

fn alpha_fixture() -> Frame {
    Frame {
        width: 3,
        height: 1,
        pixels: vec![0x0011_2233, 0x7f44_5566, 0xff77_8899],
    }
}

#[test]
fn rgba_bytes_inverts_packed_transmittance() {
    assert_eq!(
        alpha_fixture().rgba_bytes(),
        [
            0x11, 0x22, 0x33, 0xff, // opaque
            0x44, 0x55, 0x66, 0x80, // half-transmittance
            0x77, 0x88, 0x99, 0x00, // fully transparent
        ]
    );
}

#[test]
fn png_preserves_converted_frame_alpha() {
    let frame = alpha_fixture();
    let png = frame.to_png();
    let mut reader = aterm_png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .expect("decode header");
    assert_eq!(reader.info().color_type, aterm_png::ColorType::Rgba);
    assert_eq!(
        reader.info().srgb,
        Some(aterm_png::SrgbRenderingIntent::Perceptual),
        "renderer PNGs carry explicit sRGB metadata"
    );
    let mut decoded = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut decoded).expect("decode pixels");
    assert_eq!(
        &decoded[..info.buffer_size()],
        frame.rgba_bytes().as_slice()
    );
}
