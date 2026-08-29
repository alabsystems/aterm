// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// PNG DECODE, END TO END.
//
// WHY THIS EXISTS: aterm-png replaced the `png` crate and shipped with no bench
// target, so the decoder that stands behind every inline image in the terminal
// was never measured. The CRC-32 it runs over every byte of every chunk was a
// byte-at-a-time table whose next lookup address depends on the previous
// lookup's result — one serial load-use chain the length of the file — and
// nothing in the tree would have noticed.
//
// THE CORPUS IS ENCODED BY THIS CRATE'S OWN ENCODER, not read from fixtures.
// That is deliberate: the bench then controls the one property that decides how
// much of a decode is checksum rather than inflate — the compression ratio. A
// noisy image barely compresses, so its IDAT is nearly its pixel count and the
// checksum has the whole thing to walk; a flat one compresses hard, so inflate
// dominates and the checksum is a rounding error. Both are real terminal
// inputs, and they bracket the answer.
//
// THE LANES:
//   * `photo_rgb`      — per-pixel noise, ~1:1. Checksum-heavy. The upper bound.
//   * `screenshot_rgba`— flat runs and repeated rows, the shape of a screenshot
//                        or a captured terminal frame. Inflate-heavy. The lower
//                        bound, and the honest common case.
//   * `gradient_rgb`   — smooth ramps, which the PNG filters predict well.
//                        In between.
//
// The decode is set up EXACTLY as aterm-render sets it up (`EXPAND | STRIP_16`
// behind a `Limits`), because that is the only configuration the application
// ever asks for.
//
//   cargo bench -p aterm-png --bench decode

use aterm_png::{BitDepth, ColorType, Decoder, Encoder, Limits, Transformations};
use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

const W: u32 = 1024;
const H: u32 = 768;

/// Deterministic, so the corpus is identical on every box and every run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        // `as u32` keeps the high bits, which are the ones with good period.
        (self.0 >> 33) as u32
    }
}

/// Per-pixel noise: nothing to predict, nothing to run-length, so the encoder
/// emits close to the input size and the decode is dominated by moving and
/// checksumming bytes rather than by Huffman decoding.
fn photo_rgb() -> Vec<u8> {
    let mut lcg = Lcg(0x2545_F491_4F6C_DD1D);
    let mut px = Vec::with_capacity((W * H * 3) as usize);
    for _ in 0..(W * H) {
        let v = lcg.next();
        px.push((v & 0xFF) as u8);
        px.push(((v >> 8) & 0xFF) as u8);
        px.push(((v >> 16) & 0xFF) as u8);
    }
    px
}

/// Flat panels, a few horizontal rules and a repeating column of "text": what a
/// captured terminal frame or a screenshot actually looks like to a compressor.
fn screenshot_rgba() -> Vec<u8> {
    let mut px = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let is_rule = y % 64 == 0;
            let is_glyph = (x / 8 + y / 16) % 7 == 0 && y % 16 < 12;
            let (r, g, b) = if is_rule {
                (0x3A, 0x3A, 0x44)
            } else if is_glyph {
                (0xD0, 0xD4, 0xDC)
            } else {
                (0x1E, 0x1E, 0x26)
            };
            px.extend_from_slice(&[r, g, b, 0xFF]);
        }
    }
    px
}

/// Smooth ramps: the PNG line filters predict these almost exactly, so the
/// residuals are tiny and repetitive.
fn gradient_rgb() -> Vec<u8> {
    let mut px = Vec::with_capacity((W * H * 3) as usize);
    for y in 0..H {
        for x in 0..W {
            // `as u8` is intended truncation — it is what makes the ramp wrap.
            px.push((x * 255 / W) as u8);
            px.push((y * 255 / H) as u8);
            px.push(((x + y) * 255 / (W + H)) as u8);
        }
    }
    px
}

/// An indexed image: icons, emoji strikes and anything exported from a paint
/// program arrive this way, and EXPAND turns every index back into RGBA.
fn indexed() -> Vec<u8> {
    let mut px = Vec::with_capacity((W * H) as usize);
    for y in 0..H {
        for x in 0..W {
            // `as u8` is intended: the index space IS 8 bits.
            px.push(((x / 4 + y / 4) % 256) as u8);
        }
    }
    px
}

fn encode_indexed(indices: &[u8]) -> Vec<u8> {
    let mut palette = Vec::with_capacity(256 * 3);
    for i in 0..256u32 {
        // `as u8` is intended: each channel IS a byte.
        palette.push((i * 7 % 256) as u8);
        palette.push((i * 13 % 256) as u8);
        palette.push((i * 29 % 256) as u8);
    }
    // A tRNS SHORTER than the palette, which is the interesting case: the
    // missing entries are opaque by specification, so the lookup has a fallback
    // and cannot be a bare index.
    let trns: Vec<u8> = (0..128u32).map(|i| (i * 2 % 256) as u8).collect();
    let mut out = Vec::new();
    let mut enc = Encoder::new(&mut out, W, H);
    enc.set_color(ColorType::Indexed);
    enc.set_depth(BitDepth::Eight);
    enc.set_palette(palette);
    enc.set_trns(trns);
    let mut writer = enc.write_header().expect("header writes");
    writer.write_image_data(indices).expect("image data writes");
    writer.finish().expect("stream finishes");
    out
}

fn encode(pixels: &[u8], color: ColorType) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = Encoder::new(&mut out, W, H);
    enc.set_color(color);
    enc.set_depth(BitDepth::Eight);
    let mut writer = enc.write_header().expect("header writes");
    writer.write_image_data(pixels).expect("image data writes");
    writer.finish().expect("stream finishes");
    out
}

/// One decode, configured the way `aterm-render` configures it and no other way.
fn decode(png: &[u8]) -> usize {
    let mut decoder = Decoder::new(png);
    decoder.set_limits(Limits { bytes: 64 << 20 });
    decoder.set_transformations(Transformations::EXPAND | Transformations::STRIP_16);
    let mut reader = decoder.read_info().expect("header parses");
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("frame decodes");
    black_box(&buf);
    info.buffer_size()
}

/// THE REACH GUARD.
///
/// Two-sided on the property that decides whether this bench can see the
/// checksum at all: the ratio. `photo_rgb` must barely compress, so the CRC has
/// most of the file to walk; `screenshot_rgba` must compress hard, so it does
/// not — and if the two ever converge, the lanes have stopped bracketing
/// anything and the bench is measuring one workload twice.
///
/// It also decodes each lane once and checks the pixels come back, so a change
/// that made the decoder fast by making it wrong fails here rather than
/// scoring well.
fn verify_reaches_target(corpus: &[(&str, Vec<u8>, usize)]) {
    for (lane, png, raw) in corpus {
        let ratio = *raw as f64 / png.len() as f64;
        match *lane {
            "photo_rgb" => assert!(
                ratio < 1.3,
                "{lane}: ratio {ratio:.2} — noise is supposed to be incompressible, so \
                 this lane is supposed to be dominated by walking bytes",
            ),
            "screenshot_rgba" => assert!(
                ratio > 8.0,
                "{lane}: ratio {ratio:.2} — a screenshot is supposed to compress hard, \
                 so this lane is supposed to be dominated by inflate",
            ),
            _ => {}
        }
        assert!(decode(png) > 0, "{lane} must decode to a real buffer");
    }
}

fn bench(c: &mut Criterion) {
    let corpus: Vec<(&str, Vec<u8>, usize)> = vec![
        (
            "photo_rgb",
            encode(&photo_rgb(), ColorType::Rgb),
            (W * H * 3) as usize,
        ),
        (
            "screenshot_rgba",
            encode(&screenshot_rgba(), ColorType::Rgba),
            (W * H * 4) as usize,
        ),
        (
            "gradient_rgb",
            encode(&gradient_rgb(), ColorType::Rgb),
            (W * H * 3) as usize,
        ),
        (
            "indexed_expand",
            encode_indexed(&indexed()),
            (W * H) as usize,
        ),
    ];
    verify_reaches_target(&corpus);

    let mut group = c.benchmark_group("png_decode");
    for (lane, png, _) in &corpus {
        // Throughput is the ENCODED size, because that is what the checksum and
        // the bit reader actually walk.
        group.throughput(Throughput::Bytes(png.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(lane), png, |b, png| {
            b.iter(|| black_box(decode(png)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
