// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unit tests for the sixel decoder: raster correctness, palette, bounds.

use super::*;

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

    for x in 2..64usize {
        d.ensure_capacity(x, 0);
        assert!(d.alloc_width > x, "stride must cover the needed width {x}");
    }
    let wide = d.alloc_width;
    assert!(wide >= 64, "stride covers 64 columns, got {wide}");

    d.ensure_capacity(0, 100);
    assert_eq!(
        d.alloc_width, wide,
        "height-only growth must keep the stride so the in-place resize fast-path fires"
    );
    assert!(d.alloc_height >= 101);
}

#[test]
fn tall_raster_width_growth_stays_amortized_near_the_pixel_cap() {
    let mut d = SixelDecoder::new();
    d.hook();

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
    assert!(
        reallocs <= 20,
        "tall-raster column-at-a-time width growth must stay amortized (O(log W) \
         reallocations); got {reallocs} — the per-byte whole-raster-copy DoS regressed"
    );
    assert!(
        d.alloc_width >= 4000,
        "the fallback must grab the wide cap-fitting stride, got {}",
        d.alloc_width
    );
    assert!(
        d.alloc_width * d.alloc_height <= SIXEL_MAX_PIXELS,
        "the amortized stride must still respect SIXEL_MAX_PIXELS"
    );
}

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
    let mut d = SixelDecoder::new();
    d.hook();
    for &b in b"\"1;1;4096;4096~" {
        d.put(b);
    }
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
fn partial_last_band_at_pixel_cap_decodes_without_band_rounding_refusal() {
    let mut body = vec![b'-'; 681];
    body.extend_from_slice(b"#1;2;100;0;0#1!1026A");
    let img = decode(&body).expect(
        "1026x4088 = 4,194,288 <= SIXEL_MAX_PIXELS must not fail closed to band height 4092",
    );
    assert_eq!((img.width(), img.height()), (1026, 4088));
    let last_row = &img.pixels()[4087 * 1026..4088 * 1026];
    assert!(last_row.iter().all(|&px| px == 0xFFFF_0000));
    assert!(img.pixels()[..4087 * 1026].iter().all(|&px| px == 0));
}

#[test]
fn partial_column_sets_only_low_bits() {
    let img = decode(b"\"1;1;1;6#1;2;0;100;0#1A").expect("image");
    assert_eq!(img.width(), 1);
    assert_eq!(img.height(), 6);
    for row in 0..6usize {
        if row == 1 {
            assert_eq!(img.pixels()[row], 0xFF00_FF00, "row 1 opaque green");
        } else {
            assert_eq!(img.pixels()[row], 0, "row {row} transparent");
        }
    }
}

#[test]
fn dimensions_are_clamped() {
    let body = b"#1;2;100;0;0#1!4294967295~";
    let img = decode(body).expect("image");
    assert_eq!(img.width(), SIXEL_MAX_DIMENSION, "width clamped to 4096");
    assert_eq!(img.height(), 6, "single band height");
    assert_eq!(img.pixels().len(), SIXEL_MAX_DIMENSION * 6);
}

#[test]
fn register_select_out_of_range_is_clamped_no_panic() {
    let mut d = SixelDecoder::new();
    d.hook();
    for &b in b"\"1;1;1;6#99999~" {
        d.put(b);
    }
    assert_eq!(d.palette.len(), MAX_COLOR_REGISTERS);
    assert_eq!(d.current_color, MAX_COLOR_REGISTERS - 1);
    let img = d.unhook().expect("image");
    assert_eq!((img.width(), img.height()), (1, 6));
    assert!(img.pixels().iter().all(|&px| px == 0xFF00_0000));
}

#[test]
fn span_helpers_round_up() {
    let img = decode(b"\"1;1;4;6#1;2;100;0;0#1~~~~").expect("image");
    assert_eq!(img.cols_spanned(8), 1);
    assert_eq!(img.rows_spanned(16), 1);
    assert_eq!(img.cols_spanned(2), 2);
    assert_eq!(img.rows_spanned(4), 2);
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
    let ok = decode(b"#1;2;100;0;0#1!3~").expect("image");
    assert_eq!(
        ok.width(),
        3,
        "`!3~` must repeat the data byte 3x (width 3)"
    );
}

#[test]
fn decgra_only_oversized_yields_none_without_transient() {
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
        "over-cap declared geometry (4096*4096 > SIXEL_MAX_PIXELS) must yield None"
    );
    const _: () = assert!(4096usize * 4096 > SIXEL_MAX_PIXELS);
}

#[test]
fn decgra_only_within_cap_uses_declared_geometry() {
    let img = decode(b"\"1;1;4;6").expect("in-cap declared geometry");
    assert_eq!(img.width(), 4);
    assert_eq!(img.height(), 6);
    assert_eq!(img.pixels().len(), 24);
    for &p in img.pixels() {
        assert_eq!(p, 0, "no data painted → fully transparent");
    }

    let omitted_pan = decode(b"\";1;4;6").expect("omitted Pan keeps Ph/Pv in slots 2/3");
    assert_eq!((omitted_pan.width(), omitted_pan.height()), (4, 6));
    let omitted_both = decode(b"\";;5;7").expect("omitted Pan and Pad keep Ph/Pv in slots 2/3");
    assert_eq!((omitted_both.width(), omitted_both.height()), (5, 7));
}

#[test]
fn compose_pads_declared_edges_transparent() {
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
    let mut d = SixelDecoder::new();
    d.hook();
    for &b in b"99;88" {
        d.put(b);
    }
    assert!(
        d.params.is_empty(),
        "digits and semicolons outside an introducer must not populate params"
    );
    d.put(b'#');
    for _ in 0..100_000 {
        d.put(b';');
    }
    assert!(
        d.params.len() <= SIXEL_MAX_PARAMS,
        "params capped at {SIXEL_MAX_PARAMS}, got {}",
        d.params.len()
    );
    for &b in b"999" {
        d.put(b);
    }
    assert_eq!(
        d.params.last().copied(),
        Some(0),
        "digits past SIXEL_MAX_PARAMS must not mutate the last kept slot"
    );
}

const _: () = assert!(
    SIXEL_MAX_PARAMS >= 5,
    "cap must admit the 5-param color define"
);

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

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

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
fn rgb_percent_scales_and_clamps_each_component() {
    assert_eq!(scale_pct(0), 0);
    assert_eq!(scale_pct(100), 255);
    assert_eq!(scale_pct(200), 255);
    assert_eq!(rgb_percent(100, 0, 100), 0x00FF_00FF);
}

#[test]
fn hls_to_rgb_follows_dec_shifted_hsl() {
    // DEC HLS places blue at 0°, red at 120°, and green at 240° (+240° hue shift onto HSL).
    assert_eq!(hls_to_rgb(120, 50, 100), 0x00FF_0000);
    assert_eq!(hls_to_rgb(240, 50, 100), 0x0000_FF00);
    assert_eq!(hls_to_rgb(0, 50, 100), 0x0000_00FF);
    assert_eq!(hls_to_rgb(360, 50, 100), 0x0000_00FF);
    // Secondary hues at 60° steps: yellow (180°), cyan (300°), magenta (60°).
    assert_eq!(hls_to_rgb(180, 50, 100), 0x00FF_FF00);
    assert_eq!(hls_to_rgb(300, 50, 100), 0x0000_FFFF);
    assert_eq!(hls_to_rgb(60, 50, 100), 0x00FF_00FF);
    // Lightness/saturation extremes and out-of-range clamping.
    assert_eq!(hls_to_rgb(120, 0, 100), 0x0000_0000);
    assert_eq!(hls_to_rgb(120, 100, 100), 0x00FF_FFFF);
    assert_eq!(hls_to_rgb(120, 50, 0), 0x0080_8080);
    assert_eq!(hls_to_rgb(480, 250, 250), 0x00FF_FFFF);

    let img = decode(b"\"1;1;1;6#1;1;120;50;100#1~").expect("HLS red image");
    assert!(img.pixels().iter().all(|&px| px == 0xFFFF_0000));
}
