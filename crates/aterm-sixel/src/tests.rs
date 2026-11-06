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

/// `growth-amortized` (f): the pixels one image's re-strides copy, summed.
const RESTRIDE_WORK_BOUND: usize = 32 * SIXEL_MAX_PIXELS;

/// Feeds `body` a byte at a time, adding the raster's size at each stride change.
fn restride_work(body: &[u8]) -> (usize, Option<SixelImage>) {
    let mut d = SixelDecoder::new();
    d.hook();
    let (mut work, mut stride) = (0usize, 0usize);
    for &b in body {
        d.put(b);
        if d.alloc_width != stride {
            stride = d.alloc_width;
            work += d.raster.len();
            assert!(
                work <= RESTRIDE_WORK_BOUND,
                "re-strides copied {work} pixels by byte {b:?}"
            );
        }
    }
    (work, d.unhook())
}

#[test]
fn growth_past_an_over_wide_stride_copies_the_raster_a_bounded_number_of_times() {
    // A second colour one column wider doubles the stride to 2000, so from row 2098 on
    // the raster must re-stride to grow; it grows one band per 2 bytes after that.
    let mut tall = b"#1;2;100;0;0#1!1000~$!1001~".to_vec();
    for _ in 0..682 {
        tall.extend_from_slice(b"-~");
    }
    let (_, img) = restride_work(&tall);
    let img = img.expect("1001x4096 fits under SIXEL_MAX_PIXELS");
    assert_eq!((img.width(), img.height()), (1001, 4096));
    assert!((0..1001).all(|x| img.pixel(x, 0) == 0xFFFF_0000));
    assert!((0..4096).all(|y| img.pixel(0, y) == 0xFFFF_0000));

    // Each band one column wider than every band above it, alternating width and
    // height growth: 460 bands stay under the cap.
    let mut widening = b"#1;2;100;0;0#1!1000~$!1001~".to_vec();
    for k in 0..460 {
        widening.extend_from_slice(format!("-!{}?~", 1001 + k).as_bytes());
    }
    let (_, img) = restride_work(&widening);
    let img = img.expect("1461x2766 fits under SIXEL_MAX_PIXELS");
    assert_eq!((img.width(), img.height()), (1461, 2766));
    assert_eq!(img.pixel(1460, 2765), 0xFFFF_0000);

    // Each step makes the cheaper of the two growths that force a re-stride: one column
    // past the stride, or one row past the height the stride holds.
    let mut d = SixelDecoder::new();
    d.hook();
    let (mut w, mut h, mut work) = (1usize, 1usize, 0usize);
    d.ensure_capacity(0, 0);
    d.max_x = 1;
    loop {
        let aw = d.alloc_width;
        let wider = (aw + 1, h);
        let taller = (w, (SIXEL_MAX_PIXELS / aw).max(h) + 1);
        let Some((nw, nh)) = [wider, taller]
            .into_iter()
            .filter(|&(x, y)| {
                x <= SIXEL_MAX_DIMENSION && y <= SIXEL_MAX_DIMENSION && x * y <= SIXEL_MAX_PIXELS
            })
            .min_by_key(|&(x, y)| x * y)
        else {
            break;
        };
        (w, h) = (nw, nh);
        d.ensure_capacity(w - 1, h - 1);
        d.max_x = w;
        assert!(
            d.alloc_width >= w && d.alloc_height >= h,
            "{w}x{h} fits under the cap, so it must be granted"
        );
        if d.alloc_width != aw {
            work += d.raster.len();
            assert!(
                work <= RESTRIDE_WORK_BOUND,
                "re-strides copied {work} pixels by {w}x{h}"
            );
        }
    }
    assert!(
        w * h > SIXEL_MAX_PIXELS / 2,
        "the walk reaches the cap: {w}x{h}"
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
        assert_eq!(
            (img.raster_width(), img.raster_height()),
            (width, height),
            "{what}: painted throughout"
        );
        assert_eq!(img.rgba().len(), 4 * width * height, "{what}");
        assert!(
            img.rgba()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|&p| p == [0xFF, 0, 0, 0xFF]),
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

    // An undeclared multi-band paint whose combined painted box exceeds `SIXEL_MAX_PIXELS`
    // (`2000 * 3606 > 4,194,304`) must also return `None` rather than silently truncating to band 0.
    let mut undeclared = vec![b'~'];
    undeclared.extend(core::iter::repeat_n(b'-', 600));
    undeclared.extend_from_slice(b"!2000~");
    assert!(
        decode(&undeclared).is_none(),
        "undeclared over-cap multi-band paint must yield None"
    );

    let mut declared_small_then_overcap = b"\"1;1;10;10".to_vec();
    declared_small_then_overcap.extend(core::iter::repeat_n(b'-', 600));
    declared_small_then_overcap.extend_from_slice(b"!2000~");
    assert!(
        decode(&declared_small_then_overcap).is_none(),
        "over-cap paint past a small DECGRA declaration must yield None"
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
    assert_eq!((img.raster_width(), img.raster_height()), (1026, 4088));
    assert!((0..1026).all(|x| img.pixel(x, 4087) == 0xFFFF_0000));
    assert!(img.rgba()[..4 * 4087 * 1026].iter().all(|&byte| byte == 0));

    // Geometric width doubling (`1024 -> 2048` on column 1025) must compact back within
    // `SIXEL_MAX_PIXELS / new_h` when height later grows to 4088 (`1025 * 4088 <= SIXEL_MAX_PIXELS`).
    let mut over_strided = b"#1;2;100;0;0#1!1024~~".to_vec();
    over_strided.extend(core::iter::repeat_n(b'-', 681));
    over_strided.extend_from_slice(b"!1025A");
    let compacted = decode(&over_strided)
        .expect("1025x4088 fits under SIXEL_MAX_PIXELS even after geometric stride doubling");
    assert_eq!((compacted.width(), compacted.height()), (1025, 4088));
    assert_eq!(compacted.rgba().len(), 4 * 1025 * 4088);
    assert!((0..1025).all(|x| compacted.pixel(x, 0) == 0xFFFF_0000));
    assert!((0..1025).all(|x| compacted.pixel(x, 4087) == 0xFFFF_0000));
}

#[test]
fn partial_column_sets_only_low_bits() {
    let img = decode(b"\"1;1;1;6#1;2;0;100;0#1A").expect("image");
    assert_eq!(img.width(), 1);
    assert_eq!(img.height(), 6);
    for row in 0..6usize {
        if row == 1 {
            assert_eq!(img.pixel(0, row), 0xFF00_FF00, "row 1 opaque green");
        } else {
            assert_eq!(img.pixel(0, row), 0, "row {row} transparent");
        }
    }
}

#[test]
fn dimensions_are_clamped() {
    let body = b"#1;2;100;0;0#1!4294967295~";
    let img = decode(body).expect("image");
    assert_eq!(img.width(), SIXEL_MAX_DIMENSION, "width clamped to 4096");
    assert_eq!(img.height(), 6, "single band height");
    assert_eq!(img.rgba().len(), 4 * SIXEL_MAX_DIMENSION * 6);
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
    assert!((0..6).all(|y| img.pixel(0, y) == 0xFF00_0000));
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
        bimg.pixel(0, 0) & 0x00FF_FF00,
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
    assert_eq!(
        (img.raster_width(), img.raster_height(), img.rgba().len()),
        (0, 0, 0),
        "no data painted → no pixel bytes"
    );
    assert!((0..6).all(|y| (0..4).all(|x| img.pixel(x, y) == 0)));

    let omitted_pan = decode(b"\";1;4;6").expect("omitted Pan keeps Ph/Pv in slots 2/3");
    assert_eq!((omitted_pan.width(), omitted_pan.height()), (4, 6));
    let omitted_both = decode(b"\";;5;7").expect("omitted Pan and Pad keep Ph/Pv in slots 2/3");
    assert_eq!((omitted_both.width(), omitted_both.height()), (5, 7));
}

/// A DECGRA declaration reserves footprint, not memory: a 4096x1024 declaration holds no pixel
/// bytes until data paints, and then only the painted extent's.
#[test]
fn a_declared_raster_holds_bytes_only_where_data_paints() {
    let declared = decode(b"\"1;1;4096;1024").expect("in-cap declared geometry");
    assert_eq!((declared.width(), declared.height()), (4096, 1024));
    assert!(declared.rgba().is_empty());
    let dotted = decode(b"\"1;1;4096;1024#1;2;100;0;0#1~").expect("one painted column");
    assert_eq!((dotted.width(), dotted.height()), (4096, 1024));
    assert_eq!((dotted.raster_width(), dotted.raster_height()), (1, 6));
    assert_eq!(dotted.into_rgba(), [0xFF, 0, 0, 0xFF].repeat(6));
}

#[test]
fn declared_edges_past_the_paint_are_transparent_and_hold_no_bytes() {
    let img = decode(b"\"1;1;5;13#1;2;100;0;0#1~~").expect("image");
    assert_eq!(img.width(), 5, "declared width wins over painted extent");
    assert_eq!(img.height(), 13, "declared height wins over painted extent");
    assert_eq!((img.raster_width(), img.raster_height()), (2, 6));
    assert_eq!(
        img.rgba().len(),
        4 * 2 * 6,
        "only the painted 2x6 holds bytes"
    );
    for y in 0..13 {
        for x in 0..5 {
            let p = img.pixel(x, y);
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
            assert!(image.raster_width() <= image.width(), "case {case}");
            assert!(image.raster_height() <= image.height(), "case {case}");
            assert_eq!(
                image.rgba().len(),
                4 * image.raster_width() * image.raster_height(),
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
    assert!((0..6).all(|y| img.pixel(0, y) == 0xFFFF_0000));
}
