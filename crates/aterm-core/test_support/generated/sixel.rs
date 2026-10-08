// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Sixel decoder crash-safety and image invariants over generated streams.

use crate::sixel::{SIXEL_MAX_DIMENSION, SixelDecoder};
use aterm_spec::rng::XorShift;

/// A stream of sixel protocol bytes: raster attributes, repeat and colour
/// introducers, carriage return, newline, digit parameters, the separator,
/// and the sixel data characters `?`..`~`.
fn sixel_stream(rng: &mut XorShift, max: usize) -> Vec<u8> {
    const CONTROLS: &[u8] = b"\"!#$-;0123456789";
    (0..rng.range(0, max))
        .map(|_| {
            if rng.next_u64() % 2 == 0 {
                *rng.pick(CONTROLS)
            } else {
                rng.range(0x3F, 0x7F) as u8
            }
        })
        .collect()
}

fn assert_bounded(image: &crate::sixel::SixelImage) {
    assert!(image.width() <= SIXEL_MAX_DIMENSION);
    assert!(image.height() <= SIXEL_MAX_DIMENSION);
    assert_eq!(image.pixels().len(), image.width() * image.height());
}

/// Arbitrary protocol streams never panic, and any image produced is within
/// bounds with exactly width * height pixels.
#[test]
fn sixel_decoder_is_bounded_on_arbitrary_streams() {
    let mut rng = XorShift::new(0x51_8E1);
    for _ in 0..128 {
        let data = sixel_stream(&mut rng, 500);
        let mut decoder = SixelDecoder::new();
        decoder.hook();
        for &byte in &data {
            decoder.put(byte);
        }
        if let Some(image) = decoder.unhook() {
            assert_bounded(&image);
        }

        // Unhook without a prior hook returns None.
        let mut decoder = SixelDecoder::new();
        for &byte in &data {
            decoder.put(byte);
        }
        assert!(decoder.unhook().is_none());
    }
}

/// Multiple hook/unhook cycles on the same decoder must not corrupt state.
#[test]
fn sixel_decoder_reuse_across_cycles() {
    let mut rng = XorShift::new(0xC1C1_E5);
    for _ in 0..128 {
        let cycles = rng.range(1, 6);
        let data = sixel_stream(&mut rng, 200);
        let mut decoder = SixelDecoder::new();
        for _ in 0..cycles {
            decoder.hook();
            for &byte in &data {
                decoder.put(byte);
            }
            if let Some(image) = decoder.unhook() {
                assert_bounded(&image);
            }
        }
    }
}

/// rows_spanned and cols_spanned must not panic for any cell size.
#[test]
fn sixel_image_span_calculations_no_panic() {
    // `@` (0x40 ⇒ bit 1, paints row 0) so unhook() yields an image and the span
    // calculations are actually exercised.
    let mut decoder = SixelDecoder::new();
    decoder.hook();
    decoder.put(b'@');
    let image = decoder
        .unhook()
        .expect("a single painted sixel must yield an image");
    let mut rng = XorShift::new(0x5BA3);
    for size in [0u16, 1, u16::MAX]
        .into_iter()
        .chain((0..256).map(|_| rng.next_u64() as u16))
    {
        let _ = image.rows_spanned(size);
        let _ = image.cols_spanned(size);
    }
}
