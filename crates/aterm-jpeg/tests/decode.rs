// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Each fixture is one 33x21 picture (four coloured quadrants under a
//! gradient and noise, `fixtures/source.ppm`) written by libjpeg-turbo's
//! `cjpeg` in one coding mode, beside `djpeg`'s decode of it as the
//! reference. The odd size crosses MCU edges on both axes, so padding,
//! clipping and the edge rows of the upsamplers are all exercised.
//!
//! Regenerate with `cjpeg -quality 90 [mode flags] -outfile NAME.jpg
//! source.ppm` and `djpeg -pnm -outfile NAME.ppm NAME.jpg`.

use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// A binary PPM (P6) or PGM (P5) as RGB triples.
fn netpbm(bytes: &[u8]) -> (Vec<[u8; 3]>, usize, usize) {
    let mut fields = Vec::new();
    let mut at = 0;
    while fields.len() < 4 {
        while bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while !bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        fields.push(std::str::from_utf8(&bytes[start..at]).unwrap().to_string());
    }
    at += 1;
    let (width, height): (usize, usize) = (fields[1].parse().unwrap(), fields[2].parse().unwrap());
    let pixels = match fields[0].as_str() {
        "P6" => bytes[at..].as_chunks::<3>().0.to_vec(),
        "P5" => bytes[at..].iter().map(|&v| [v, v, v]).collect(),
        other => panic!("not a binary netpbm: {other}"),
    };
    (pixels, width, height)
}

/// Decode `name.jpg` and hold it to `name.ppm` (djpeg's decode): the same
/// size, every channel within `tolerance` levels and the mean error under a
/// fifth of a level. The tolerance covers the float transform against libjpeg's
/// integer one and the rounding of the fixed-point colour conversion.
fn matches_reference(name: &str, tolerance: u8) {
    let (rgba, width, height) =
        aterm_jpeg::decode_rgba8(&fixture(&format!("{name}.jpg")), 8192).expect(name);
    let (reference, ref_w, ref_h) = netpbm(&fixture(&format!("{name}.ppm")));
    assert_eq!((width, height), (ref_w, ref_h), "{name}: size");
    assert_eq!(rgba.len(), width * height * 4, "{name}: buffer");
    let mut worst = 0u8;
    let mut total = 0u64;
    for (pixel, expected) in rgba.as_chunks::<4>().0.iter().zip(&reference) {
        assert_eq!(pixel[3], 255, "{name}: opaque");
        for c in 0..3 {
            let error = pixel[c].abs_diff(expected[c]);
            worst = worst.max(error);
            total += u64::from(error);
        }
    }
    let mean = total as f64 / (width * height * 3) as f64;
    assert!(
        worst <= tolerance && mean < 0.2,
        "{name}: worst {worst}, mean {mean:.3}"
    );
}

#[test]
fn baseline_420_matches_the_reference_decode() {
    matches_reference("b420", 2);
}

#[test]
fn baseline_422_matches_the_reference_decode() {
    matches_reference("b422", 2);
}

#[test]
fn baseline_440_matches_the_reference_decode() {
    matches_reference("b440", 2);
}

#[test]
fn baseline_444_matches_the_reference_decode() {
    matches_reference("b444", 2);
}

/// Luma at 4x2: a factor the triangle filter does not cover, replicated.
#[test]
fn baseline_4x2_matches_the_reference_decode() {
    matches_reference("b4x2", 2);
}

#[test]
fn grayscale_matches_the_reference_decode() {
    matches_reference("gray", 2);
}

#[test]
fn progressive_420_matches_the_reference_decode() {
    matches_reference("p420", 2);
}

#[test]
fn progressive_grayscale_matches_the_reference_decode() {
    matches_reference("pgray", 2);
}

/// A restart marker after every MCU.
#[test]
fn restart_intervals_match_the_reference_decode() {
    matches_reference("r420", 2);
}

/// Progressive, with a restart every two MCUs (every two blocks in the
/// single-component AC scans).
#[test]
fn progressive_restart_intervals_match_the_reference_decode() {
    matches_reference("pr420", 2);
}

/// An Adobe transform-0 file: the samples are RGB, not YCbCr.
#[test]
fn adobe_rgb_matches_the_reference_decode() {
    matches_reference("rgb", 2);
}

/// The quadrants come back in their colours (the reference agreeing is not
/// enough if both read the channels in the wrong order).
#[test]
fn the_quadrants_keep_their_colours() {
    let (rgba, width, _) = aterm_jpeg::decode_rgba8(&fixture("b420.jpg"), 8192).unwrap();
    let at = |x: usize, y: usize| {
        let i = (y * width + x) * 4;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    };
    let dominant = |[r, g, b]: [u8; 3]| {
        if r > g && r > b {
            'r'
        } else if g > r && g > b {
            'g'
        } else {
            'b'
        }
    };
    assert_eq!(dominant(at(4, 4)), 'r');
    assert_eq!(dominant(at(28, 4)), 'g');
    assert_eq!(dominant(at(4, 17)), 'b');
    let yellow = at(28, 17);
    assert!(
        yellow[0] > 150 && yellow[1] > 150 && yellow[2] < 120,
        "{yellow:?}"
    );
}

/// Insert an EXIF APP1 segment carrying `orientation` right after SOI.
fn with_orientation(jpeg: &[u8], orientation: u8) -> Vec<u8> {
    let tiff = [
        b'M',
        b'M',
        0,
        42,
        0,
        0,
        0,
        8,
        0,
        1,
        0x01,
        0x12,
        0,
        3,
        0,
        0,
        0,
        1,
        0,
        orientation,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(&tiff);
    let length = (payload.len() + 2) as u16;
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&payload);
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// EXIF orientation 6 comes back turned upright: the sides swap and the
/// stored left column is the top row.
#[test]
fn the_exif_orientation_turns_the_image_upright() {
    let jpeg = fixture("b444.jpg");
    let (upright, width, height) = aterm_jpeg::decode_rgba8(&jpeg, 8192).unwrap();
    let (turned, turned_w, turned_h) =
        aterm_jpeg::decode_rgba8(&with_orientation(&jpeg, 6), 8192).unwrap();
    assert_eq!((turned_w, turned_h), (height, width));
    for y in 0..turned_h {
        for x in 0..turned_w {
            let (sx, sy) = (y, height - 1 - x);
            let from = (sy * width + sx) * 4;
            let to = (y * turned_w + x) * 4;
            assert_eq!(&turned[to..to + 4], &upright[from..from + 4], "({x},{y})");
        }
    }
    // Orientation 1 changes nothing.
    let (same, ..) = aterm_jpeg::decode_rgba8(&with_orientation(&jpeg, 1), 8192).unwrap();
    assert_eq!(same, upright);
}

/// A frame larger than the bound is refused from its header, before any
/// pixel buffer exists: the header claims 9000 x 9000.
#[test]
fn a_frame_past_the_bound_is_refused_from_its_header() {
    let mut jpeg = fixture("b420.jpg");
    let sof = jpeg
        .windows(2)
        .position(|pair| pair == [0xFF, 0xC0])
        .expect("SOF0");
    jpeg[sof + 5..sof + 7].copy_from_slice(&9000u16.to_be_bytes());
    jpeg[sof + 7..sof + 9].copy_from_slice(&9000u16.to_be_bytes());
    assert!(aterm_jpeg::decode_rgba8(&jpeg, 8192).is_none());
    assert!(
        aterm_jpeg::decode_rgba8(&fixture("b420.jpg"), 32).is_none(),
        "33 > 32"
    );
}

/// Every truncation of every fixture, and a sweep of single-bit flips,
/// decodes or is refused without a panic.
#[test]
fn truncated_and_corrupted_files_never_panic() {
    for name in ["b420", "p420", "pr420", "r420", "gray", "rgb", "b4x2"] {
        let jpeg = fixture(&format!("{name}.jpg"));
        for cut in 0..jpeg.len() {
            let _ = aterm_jpeg::decode_rgba8(&jpeg[..cut], 8192);
        }
        for at in (0..jpeg.len()).step_by(3) {
            for bit in [0x01u8, 0x10, 0x80] {
                let mut flipped = jpeg.clone();
                flipped[at] ^= bit;
                let _ = aterm_jpeg::decode_rgba8(&flipped, 8192);
            }
        }
    }
}

/// A file cut off inside its scan still decodes what it holds, at full size.
#[test]
fn a_file_cut_short_decodes_as_far_as_it_goes() {
    let jpeg = fixture("b444.jpg");
    let (_, width, height) =
        aterm_jpeg::decode_rgba8(&jpeg[..jpeg.len() - 200], 8192).expect("partial decode");
    assert_eq!((width, height), (33, 21));
}
