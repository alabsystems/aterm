// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Reference whole-image filter pass used as an oracle for `RowFilter`.
fn whole_image_filter(data: &[u8], stride: usize, bpp: usize) -> Vec<u8> {
    let rows = data.len() / stride.max(1);
    let mut out = Vec::with_capacity(rows * (stride + 1));
    let mut candidate = vec![0u8; stride];
    let mut best = vec![0u8; stride];
    let zero = vec![0u8; stride];
    for row in 0..rows {
        let cur = &data[row * stride..(row + 1) * stride];
        let prev: &[u8] = if row == 0 {
            &zero
        } else {
            &data[(row - 1) * stride..row * stride]
        };
        let mut best_kind = 0u8;
        let mut best_score = u64::MAX;
        for kind in 0..5u8 {
            apply_filter(kind, cur, prev, bpp, &mut candidate);
            let score: u64 = candidate
                .iter()
                .map(|&b| u64::from((b as i8).unsigned_abs()))
                .sum();
            if score < best_score {
                best_score = score;
                best_kind = kind;
                best.copy_from_slice(&candidate);
            }
        }
        out.push(best_kind);
        out.extend_from_slice(&best);
    }
    out
}

/// Every chunk of a PNG this encoder wrote, as (type, payload).
fn chunks(png: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut rest = &png[SIGNATURE.len()..];
    let mut out = Vec::new();
    while !rest.is_empty() {
        let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        let kind = [rest[4], rest[5], rest[6], rest[7]];
        out.push((kind, &rest[8..8 + len]));
        rest = &rest[8 + len + 4..];
    }
    out
}

/// Verify streamed `IDAT` payloads match the one-shot compression of
/// `whole_image_filter` across gradient, noise, and flat images at 1, 3,
/// and 4 bytes per pixel.
#[test]
fn the_streamed_idat_is_the_whole_image_encoders_stream() {
    let mut state = 0x1234_5678u32;
    let mut multi_chunk = false;
    for (samples, color) in [
        (1usize, ColorType::Grayscale),
        (3, ColorType::Rgb),
        (4, ColorType::Rgba),
    ] {
        for (shape, w, h) in [
            ("gradient", 61u32, 47u32),
            ("noise", 211, 160),
            ("flat", 300, 2),
        ] {
            let stride = w as usize * samples;
            let data: Vec<u8> = (0..stride * h as usize)
                .map(|i| match shape {
                    "gradient" => ((i % stride) * 3 + i / stride) as u8,
                    "noise" => {
                        state ^= state << 13;
                        state ^= state >> 17;
                        state ^= state << 5;
                        (state >> 24) as u8
                    }
                    _ => 7,
                })
                .collect();
            let mut png = Vec::new();
            let mut encoder = Encoder::new(&mut png, w, h);
            encoder.set_color(color);
            let mut writer = encoder.write_header().expect("header");
            writer.write_image_data(&data).expect("data");
            writer.finish().expect("finish");

            let idats: Vec<&[u8]> = chunks(&png)
                .into_iter()
                .filter(|(kind, _)| kind == b"IDAT")
                .map(|(_, payload)| payload)
                .collect();
            let (last, full) = idats.split_last().expect("one IDAT at least");
            assert!(
                full.iter().all(|chunk| chunk.len() == IDAT_CHUNK)
                    && !last.is_empty()
                    && last.len() <= IDAT_CHUNK,
                "{shape}/{samples}: IDATs are full chunks and one remainder"
            );
            multi_chunk |= !full.is_empty();
            let oracle = deflate::zlib_compress(&whole_image_filter(&data, stride, samples));
            assert_eq!(idats.concat(), oracle, "{shape}/{samples}: the zlib stream");
        }
    }
    assert!(multi_chunk, "the noise images span more than one IDAT");
}
