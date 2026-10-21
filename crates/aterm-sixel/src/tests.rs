// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unit tests for the sixel decoder: raster correctness, palette, bounds.

use super::*;

/// Feed a whole sixel data body (without DCS framing) through a fresh decoder.
fn decode(body: &[u8]) -> Option<SixelImage> {
    let mut d = SixelDecoder::new();
    d.hook();
    for &b in body {
        d.put(b);
    }
    d.unhook()
}

#[test]
fn unhook_without_hook_is_none() {
    let mut d = SixelDecoder::new();
    d.put(b'~');
    assert!(d.unhook().is_none());
}

#[test]
fn empty_sequence_is_none() {
    assert!(decode(b"").is_none());
}

#[test]
fn width_growth_is_amortized_and_preserves_the_height_fast_path() {
    // Growing the raster width one column at a time (the per-byte sixel
    // paint path) must over-allocate the stride geometrically — O(log W) reallocations
    // instead of a realloc+whole-raster copy on every byte (Theta(H*W^2) DoS). And a
    // height-only growth must still take the in-place resize fast-path.
    let mut d = SixelDecoder::new();
    d.hook();

    d.ensure_capacity(0, 0);
    let w1 = d.alloc_width;
    d.ensure_capacity(1, 0); // need_w = 2 > alloc_width ⇒ width must grow
    assert!(
        d.alloc_width >= 2 * w1.max(1),
        "width growth must be geometric (doubled), got {} from {w1}",
        d.alloc_width
    );

    // Advance one column at a time; the stride always covers the need and grows in
    // doubling steps, so it exceeds need_w between reallocations (amortized O(area)).
    for x in 2..64usize {
        d.ensure_capacity(x, 0);
        assert!(d.alloc_width > x, "stride must cover the needed width {x}");
    }
    let wide = d.alloc_width;
    assert!(wide >= 64, "stride covers 64 columns, got {wide}");

    // Height-only growth must NOT change the stride (fast-path).
    d.ensure_capacity(0, 100);
    assert_eq!(
        d.alloc_width, wide,
        "height-only growth must keep the stride so the in-place resize fast-path fires"
    );
    assert!(d.alloc_height >= 101);
}

#[test]
fn tall_raster_width_growth_stays_amortized_near_the_pixel_cap() {
    // On a tall raster, geometric doubling eventually overflows
    // SIXEL_MAX_PIXELS while alloc_width is still moderate (2*alloc_width*new_h > cap).
    // The fallback must grab the widest cap-fitting stride in one realloc — not the
    // exact needed width (= alloc+1 on a column-at-a-time paint), which would realloc +
    // copy the whole raster on every byte.
    let mut d = SixelDecoder::new();
    d.hook();

    // new_h = 1025 ⇒ cap/new_h ≈ 4092 columns fit; doubling past ~2046 columns
    // overflows the cap and hits the fallback. All columns 0..3000 fit the cap
    // (3000*1025 < 4 Mi), so the stride must cover each and the grow is never refused.
    let tall_y = 1024usize; // need_h = 1025
    let mut reallocs = 0usize;
    let mut prev_w = d.alloc_width;
    for x in 0..3000usize {
        d.ensure_capacity(x, tall_y);
        assert!(
            d.alloc_width > x,
            "stride {} must cover the needed column {x} (all fit under the cap)",
            d.alloc_width
        );
        if d.alloc_width != prev_w {
            reallocs += 1;
            prev_w = d.alloc_width;
        }
    }
    // O(log W) reallocations (~13 across this loop), not one per column.
    assert!(
        reallocs <= 20,
        "tall-raster column-at-a-time width growth must stay amortized (O(log W) \
         reallocations); got {reallocs} — the per-byte whole-raster-copy DoS regressed"
    );
    // The fallback grabbed the wide cap-fitting stride (≈4092) rather than crawling
    // up by +1: it covers thousands of columns from a single realloc.
    assert!(
        d.alloc_width >= 4000,
        "the fallback must grab the wide cap-fitting stride, got {}",
        d.alloc_width
    );
    // Never over the pixel cap (fail-closed preserved).
    assert!(
        d.alloc_width * d.alloc_height <= SIXEL_MAX_PIXELS,
        "the amortized stride must still respect SIXEL_MAX_PIXELS"
    );
}

/// Small valid bodies decode to their raster geometry, every painted pixel
/// the colour register 1 defines (RGB% 100;0;0, opaque red): one full `~`
/// column, four columns in one band, two bands split by a graphics newline
/// `-`, and a DECGRI `!5` repeat.
#[test]
fn small_bodies_decode_to_their_geometry_in_the_defined_colour() {
    for (body, width, height) in [
        (&b"\"1;1;1;6#1;2;100;0;0#1~"[..], 1usize, 6usize),
        (b"\"1;1;4;6#0;2;0;0;0#1;2;100;0;0#1~~~~$-", 4, 6),
        (b"\"1;1;1;12#1;2;100;0;0#1~-~", 1, 12),
        (b"\"1;1;5;6#1;2;100;0;0#1!5~", 5, 6),
    ] {
        let what = String::from_utf8_lossy(body);
        let img = decode(body).expect("a painted body produces an image");
        assert_eq!((img.width(), img.height()), (width, height), "{what}");
        assert_eq!(img.pixels().len(), width * height, "{what}");
        assert!(
            img.pixels().iter().all(|&p| p == 0xFFFF_0000),
            "{what}: every pixel opaque red"
        );
    }
}

#[test]
fn oversized_raster_declaration_is_refused_without_allocating() {
    // REGRESSION: a ~16-byte DECGRA declaring a 4096×4096 raster (16.7 M cells)
    // must NOT force the ~150 MB raster/mask/compose allocation. The total-pixel
    // cap (SIXEL_MAX_PIXELS) refuses the over-cap raster: ensure_capacity does
    // not allocate and unhook refuses to compose (the image would be rejected
    // downstream anyway).
    let mut d = SixelDecoder::new();
    d.hook();
    // `"Pan;Pad;Ph;Pv` = 4096×4096, then one data byte `~` to trigger the
    // deferred apply_raster (and thus ensure_capacity).
    for &b in b"\"1;1;4096;4096~" {
        d.put(b);
    }
    // ensure_capacity refused the 4096×4096 declaration, so only the tiny painted
    // region is allocated — NOT the ~67 MB (4096·4096·4) raster a missing cap
    // would have produced. Stay well under 1 MiB.
    assert!(
        d.pixel_alloc_bytes() < 1024 * 1024,
        "an over-cap raster must not allocate the full 4096x4096 buffers (got {} bytes)",
        d.pixel_alloc_bytes()
    );
    assert!(
        d.unhook().is_none(),
        "an over-cap image must be dropped, not composed"
    );
}

#[test]
fn capped_raster_at_the_pixel_limit_still_decodes() {
    // A raster exactly at the total-pixel cap (2048×2048 = 4 Mi pixels) must
    // still decode — the cap is a ceiling, not an off-by-one rejection of
    // legitimate large images.
    let mut d = SixelDecoder::new();
    d.hook();
    for &b in b"\"1;1;2048;2048#1;2;100;0;0#1~" {
        d.put(b);
    }
    let img = d
        .unhook()
        .expect("a raster at the pixel cap must still compose");
    assert_eq!(img.width(), 2048);
    assert_eq!(img.height(), 2048);
}

#[test]
fn partial_column_sets_only_low_bits() {
    // `?` = 0x3F → value 0 → no pixels. `A` = 0x41 → value 2 → bit 1 set (row 1).
    let img = decode(b"\"1;1;1;6#1;2;0;100;0#1A").expect("image");
    assert_eq!(img.width(), 1);
    assert_eq!(img.height(), 6);
    // Only row 1 (second from top) is painted green; others transparent.
    assert_eq!(img.pixels()[0], 0, "row 0 transparent");
    assert_eq!(
        img.pixels()[1] & 0xFF00_FF00,
        0xFF00_FF00,
        "row 1 opaque green"
    );
    assert_eq!(img.pixels()[2], 0, "row 2 transparent");
}

#[test]
fn dimensions_are_clamped() {
    // A hostile DECGRI cannot exceed SIXEL_MAX_DIMENSION on width.
    let body = b"#1;2;100;0;0#1!4294967295~";
    let img = decode(body).expect("image");
    assert!(img.width() <= SIXEL_MAX_DIMENSION, "width clamped");
    assert!(img.height() <= SIXEL_MAX_DIMENSION, "height clamped");
    assert_eq!(img.pixels().len(), img.width() * img.height());
}

#[test]
fn register_select_out_of_range_is_clamped_no_panic() {
    // Selecting register 99999 must clamp, not panic or grow the palette.
    let img = decode(b"\"1;1;1;6#99999~").expect("image");
    assert_eq!(img.width(), 1);
    assert_eq!(img.height(), 6);
}

#[test]
fn span_helpers_round_up() {
    let img = decode(b"\"1;1;4;6#1;2;100;0;0#1~~~~").expect("image");
    // 4px wide / 8px cell → 1 col; 6px tall / 16px cell → 1 row.
    assert_eq!(img.cols_spanned(8), 1);
    assert_eq!(img.rows_spanned(16), 1);
    // 4px / 2px cell → 2 cols; 6px / 4px cell → 2 rows.
    assert_eq!(img.cols_spanned(2), 2);
    assert_eq!(img.rows_spanned(4), 2);
    // A zero cell size divides by one, not zero; the largest is one cell.
    assert_eq!((img.cols_spanned(0), img.rows_spanned(0)), (4, 6));
    assert_eq!(
        (img.cols_spanned(u16::MAX), img.rows_spanned(u16::MAX)),
        (1, 1)
    );
}

#[test]
fn reuse_across_cycles_resets_state() {
    let mut d = SixelDecoder::new();
    d.hook();
    for &b in b"\"1;1;4;6#1;2;100;0;0#1~~~~" {
        d.put(b);
    }
    let a = d.unhook().expect("first image");
    assert_eq!(a.width(), 4);

    // Second cycle: a smaller image must not inherit the first's geometry.
    d.hook();
    for &b in b"\"1;1;1;6#1;2;0;100;0#1~" {
        d.put(b);
    }
    let bimg = d.unhook().expect("second image");
    assert_eq!(bimg.width(), 1, "geometry reset between cycles");
    assert_eq!(bimg.height(), 6);
    assert_eq!(
        bimg.pixels()[0] & 0x00FF_FF00,
        0x0000_FF00,
        "green, not red"
    );
}

#[test]
fn abort_frees_and_yields_no_image() {
    let mut d = SixelDecoder::new();
    d.hook();
    for &b in b"\"1;1;4;6#1~~~~" {
        d.put(b);
    }
    assert!(d.pixel_alloc_bytes() > 0, "buffer allocated during decode");
    d.abort();
    assert_eq!(d.pixel_alloc_bytes(), 0, "abort frees the buffer");
    assert!(d.unhook().is_none(), "aborted decode yields nothing");
}

#[test]
fn repeat_does_not_survive_band_control() {
    // DECGRI `!Pn` applies ONLY to the immediately-following sixel data byte. A
    // `$` (graphics-CR) or `-` (graphics-NL) between `!3` and the data byte must
    // cancel the pending repeat — otherwise the next band is wrongly widened.
    let cr = decode(b"#1;2;100;0;0#1!3$~").expect("image");
    assert_eq!(
        cr.width(),
        1,
        "`!3` then `$` then `~` must NOT repeat (width 1)"
    );
    let nl = decode(b"#1;2;100;0;0#1!3-~").expect("image");
    assert_eq!(
        nl.width(),
        1,
        "`!3` then `-` then `~` must NOT repeat (width 1)"
    );
    // Control: a repeat IMMEDIATELY followed by its data byte still repeats.
    let ok = decode(b"#1;2;100;0;0#1!3~").expect("image");
    assert_eq!(
        ok.width(),
        3,
        "`!3~` must repeat the data byte 3x (width 3)"
    );
}

#[test]
fn decgra_only_oversized_yields_none_without_transient() {
    // A DECGRA declaration of the maximal raster with NO following data byte:
    // `apply_raster` is deferred to `unhook`. The decoder must NOT eagerly
    // allocate the declared box during `put`,
    // and `unhook` must reject the over-cap geometry before composing the
    // output `pixels` Vec — so no multi-MiB transient is ever materialized.
    let mut d = SixelDecoder::new();
    d.hook();
    for &b in b"\"1;1;4096;4096" {
        d.put(b);
    }
    assert_eq!(
        d.pixel_alloc_bytes(),
        0,
        "a DECGRA-only declaration must not pre-size the raster"
    );
    assert!(
        d.unhook().is_none(),
        "over-cap declared geometry (4096*4096*4 > SIXEL_MAX_IMAGE_BYTES) must yield None"
    );
    // 4096*4096*4 = 64 MiB really does exceed the 16 MiB image cap.
    const _: () = assert!(4096usize * 4096 * 4 > SIXEL_MAX_IMAGE_BYTES);
}

#[test]
fn decgra_only_within_cap_uses_declared_geometry() {
    // A small DECGRA-only declaration still yields a transparent image of the
    // declared size (the declared-geometry fallback).
    let img = decode(b"\"1;1;4;6").expect("in-cap declared geometry");
    assert_eq!(img.width(), 4);
    assert_eq!(img.height(), 6);
    assert_eq!(img.pixels().len(), 24);
    for &p in img.pixels() {
        assert_eq!(p, 0, "no data painted → fully transparent");
    }
}

#[test]
fn compose_pads_declared_edges_transparent() {
    // Declared geometry larger than the painted extent exercises BOTH padding
    // paths of the row-wise compose: the right edge (width 5 > stride 2, the
    // painted columns) and the bottom rows (height 13 > alloc_height 6, one
    // painted band). Every pixel must match the per-pixel reference: painted
    // region opaque red, all padding fully transparent.
    let img = decode(b"\"1;1;5;13#1;2;100;0;0#1~~").expect("image");
    assert_eq!(img.width(), 5, "declared width wins over painted extent");
    assert_eq!(img.height(), 13, "declared height wins over painted extent");
    assert_eq!(img.pixels().len(), 5 * 13);
    for y in 0..13 {
        for x in 0..5 {
            let p = img.pixels()[y * 5 + x];
            if x < 2 && y < 6 {
                assert_eq!(p, 0xFFFF_0000, "painted pixel ({x},{y}) opaque red");
            } else {
                assert_eq!(p, 0, "padding pixel ({x},{y}) must be transparent");
            }
        }
    }
}

#[test]
fn semicolon_flood_keeps_params_bounded() {
    // A long `;`-separator run after an introducer must not grow `params`
    // unbounded — it is capped at SIXEL_MAX_PARAMS regardless of stream length.
    let mut d = SixelDecoder::new();
    d.hook();
    d.put(b'#');
    for _ in 0..100_000 {
        d.put(b';');
    }
    assert!(
        d.params.len() <= SIXEL_MAX_PARAMS,
        "params capped at {SIXEL_MAX_PARAMS}, got {}",
        d.params.len()
    );
}

// The richest valid introducer uses 5 params (`#Pc;Pu;Px;Py;Pz`): lowering the
// cap below 5 would truncate a colour define (params-bound, second clause).
const _: () = assert!(
    SIXEL_MAX_PARAMS >= 5,
    "cap must admit the 5-param color define"
);

/// xorshift64: a few lines, so the generated-stream test below needs no
/// dev-dependency. Fixed seeds, so a failure reproduces from its case index.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform-enough in `0..n` for a stream generator.
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// A stream of sixel protocol bytes: raster attributes, repeat and colour
/// introducers, carriage return, newline, digit parameters, the separator,
/// and the sixel data characters `?`..`~`.
fn sixel_stream(rng: &mut XorShift, max: usize) -> Vec<u8> {
    const CONTROLS: &[u8] = b"\"!#$-;0123456789";
    (0..rng.below(max))
        .map(|_| {
            if rng.next().is_multiple_of(2) {
                CONTROLS[rng.below(CONTROLS.len())]
            } else {
                0x3F + rng.below(0x40) as u8
            }
        })
        .collect()
}

/// Arbitrary protocol streams never panic, and any image produced is within
/// bounds with exactly width * height pixels; without a hook there is none.
#[test]
fn decoder_is_bounded_on_arbitrary_streams() {
    let mut rng = XorShift(0x51_8E1);
    for case in 0..128 {
        let data = sixel_stream(&mut rng, 500);
        if let Some(image) = decode(&data) {
            assert!(image.width() <= SIXEL_MAX_DIMENSION, "case {case}");
            assert!(image.height() <= SIXEL_MAX_DIMENSION, "case {case}");
            assert_eq!(
                image.pixels().len(),
                image.width() * image.height(),
                "case {case}"
            );
        }
        let mut unhooked = SixelDecoder::new();
        for &byte in &data {
            unhooked.put(byte);
        }
        assert!(unhooked.unhook().is_none(), "case {case}");
    }
}

#[test]
fn sixel_const_helpers_round_trip() {
    assert!(!is_sixel_data_byte(0x3E));
    assert!(is_sixel_data_byte(0x3F));
    assert!(is_sixel_data_byte(0x7E));
    assert!(!is_sixel_data_byte(0x7F));
    assert_eq!(sixel_data_bits(0x3E), None);
    for bits in 0..=63u8 {
        let b = encode_sixel_data_byte(bits);
        assert!(is_sixel_data_byte(b));
        assert_eq!(sixel_data_bits(b), Some(bits));
    }
    for row in 0..6usize {
        let mask = sixel_row_mask(row);
        assert_eq!(mask, 1u8 << row);
        assert!(sixel_bit_is_set(mask, row));
        assert!(!sixel_bit_is_set(0, row));
    }
    assert_eq!(sixel_row_mask(6), 0);
    assert!(!sixel_bit_is_set(0x3F, 6));
    assert_eq!(clamp_color_register(0), 0);
    assert_eq!(clamp_color_register(99_999), MAX_COLOR_REGISTERS - 1);
    assert_eq!(clamp_sixel_dimension(100), 100);
    assert_eq!(clamp_sixel_dimension(99_999), SIXEL_MAX_DIMENSION);
    assert_eq!(checked_sixel_pixel_count(0, 10), None);
    assert_eq!(
        checked_sixel_pixel_count(2048, 2048),
        Some(SIXEL_MAX_PIXELS)
    );
    assert_eq!(checked_sixel_pixel_count(4096, 4096), None);
    assert_eq!(
        checked_sixel_image_bytes(2048, 2048),
        Some(SIXEL_MAX_IMAGE_BYTES)
    );
    assert_eq!(checked_sixel_image_bytes(4096, 4096), None);
    let px = pack_argb_pixel(0x12, 0x34, 0x56);
    assert_eq!(unpack_argb_channels(px), (0xFF, 0x12, 0x34, 0x56));
    assert_eq!(decode_argb_pixel_le(encode_argb_pixel_le(px)), px);
    assert_eq!(scale_pct(0), 0);
    assert_eq!(scale_pct(100), 255);
    assert_eq!(scale_pct(200), 255);
    assert_eq!(rgb_percent(100, 0, 100), 0x00FF_00FF);

    let mut d = SixelDecoder::new();
    assert!(!d.is_active());
    d.hook();
    assert!(d.is_active());
    for &b in b"\"1;1;4;6#2~~" {
        d.put(b);
    }
    assert_eq!(d.current_color(), 2);
    assert_eq!(d.cursor_pos(), (2, 0));
    assert_eq!(d.painted_extent(), (2, 6));
    assert_eq!(d.declared_dimensions(), (4, 6));
    let hdr = encode_sixel_header_le(640, 480, 15, 3);
    assert_eq!(decode_sixel_header_le(hdr), (640, 480, 15, 3));
}
