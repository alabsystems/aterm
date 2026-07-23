// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Property-based tests for Sixel decoder crash-safety and image invariants.

use crate::sixel::{SIXEL_MAX_DIMENSION, SixelDecoder};
use proptest::prelude::*;

fn sixel_protocol_byte() -> impl Strategy<Value = u8> {
    prop_oneof![
        Just(b'"'),        // raster attributes
        Just(b'!'),        // repeat introducer
        Just(b'#'),        // color introducer
        Just(b'$'),        // graphics carriage return
        Just(b'-'),        // graphics newline
        (0x3Fu8..=0x7Eu8), // sixel data characters
        (b'0'..=b'9'),     // digit parameters
        Just(b';'),        // parameter separator
    ]
}

fn sixel_stream_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(sixel_protocol_byte(), 0..500)
}

fn hook_params_strategy() -> impl Strategy<Value = Vec<u16>> {
    prop::collection::vec(0u16..=20u16, 0..=3)
}

// Sixel decoder tests: 128 cases is sufficient for crash-safety and
// dimension-bound checking on structured protocol byte streams.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Arbitrary byte sequences through the decoder must never panic.
    #[test]
    fn sixel_decoder_no_panic_arbitrary_input(
        params in hook_params_strategy(),
        data in sixel_stream_strategy(),
        cursor_row in any::<u16>(),
        cursor_col in any::<u16>(),
    ) {
        let mut decoder = SixelDecoder::new();
        decoder.hook(&params, cursor_row, cursor_col);
        for &byte in &data {
            decoder.put(byte);
        }
        let _ = decoder.unhook();
    }

    /// If an image is produced, its dimensions must be within bounds
    /// and its pixel buffer must have exactly width * height elements.
    #[test]
    fn sixel_image_dimensions_bounded(
        params in hook_params_strategy(),
        data in sixel_stream_strategy(),
    ) {
        let mut decoder = SixelDecoder::new();
        decoder.hook(&params, 0, 0);
        for &byte in &data {
            decoder.put(byte);
        }
        if let Some(image) = decoder.unhook() {
            prop_assert!(
                image.width() <= SIXEL_MAX_DIMENSION,
                "width {} exceeds max {}",
                image.width(), SIXEL_MAX_DIMENSION
            );
            prop_assert!(
                image.height() <= SIXEL_MAX_DIMENSION,
                "height {} exceeds max {}",
                image.height(), SIXEL_MAX_DIMENSION
            );
            prop_assert_eq!(
                image.pixels().len(),
                image.width() * image.height(),
                "pixel buffer length must equal width * height"
            );
        }
    }

    /// Unhook without prior hook returns None.
    #[test]
    fn sixel_unhook_without_hook_returns_none(
        data in sixel_stream_strategy(),
    ) {
        let mut decoder = SixelDecoder::new();
        // Feed data without calling hook first
        for &byte in &data {
            decoder.put(byte);
        }
        prop_assert!(decoder.unhook().is_none());
    }

    /// Multiple hook/unhook cycles on the same decoder must not corrupt state.
    #[test]
    fn sixel_decoder_reuse_across_cycles(
        cycle_count in 1usize..=5usize,
        params in hook_params_strategy(),
        data in prop::collection::vec(sixel_protocol_byte(), 0..200),
    ) {
        let mut decoder = SixelDecoder::new();
        for _ in 0..cycle_count {
            decoder.hook(&params, 0, 0);
            for &byte in &data {
                decoder.put(byte);
            }
            let result = decoder.unhook();
            if let Some(image) = result {
                prop_assert!(image.width() <= SIXEL_MAX_DIMENSION);
                prop_assert!(image.height() <= SIXEL_MAX_DIMENSION);
                prop_assert_eq!(image.pixels().len(), image.width() * image.height());
            }
        }
    }

    /// rows_spanned and cols_spanned must not panic for any cell size.
    #[test]
    fn sixel_image_span_calculations_no_panic(
        cell_height in any::<u16>(),
        cell_width in any::<u16>(),
    ) {
        // Produce a minimal NON-EMPTY image: `@` (0x40 ⇒ bit 1, paints row 0) so
        // unhook() yields Some and the span calculations are ACTUALLY exercised.
        // (`?` (0x3F) paints nothing ⇒ unhook() returns None ⇒ the `if let Some`
        // body never ran and this test verified nothing.)
        let mut decoder = SixelDecoder::new();
        decoder.hook(&[0], 0, 0);
        decoder.put(b'@');
        let image = decoder
            .unhook()
            .expect("a single painted sixel must yield an image");
        // Must not panic for ANY cell size, including 0.
        let _ = image.rows_spanned(cell_height);
        let _ = image.cols_spanned(cell_width);
    }
}
