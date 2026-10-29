// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! JPEG decoder conformance tests against `djpeg` reference outputs across 33x21 MCU-crossing fixtures.

use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

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

#[test]
fn restart_intervals_match_the_reference_decode() {
    matches_reference("r420", 2);
}

#[test]
fn progressive_restart_intervals_match_the_reference_decode() {
    matches_reference("pr420", 2);
}

#[test]
fn adobe_rgb_matches_the_reference_decode() {
    matches_reference("rgb", 2);
}

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
    let (same, ..) = aterm_jpeg::decode_rgba8(&with_orientation(&jpeg, 1), 8192).unwrap();
    assert_eq!(same, upright);
}

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

#[test]
fn a_file_cut_short_decodes_as_far_as_it_goes() {
    let jpeg = fixture("b444.jpg");
    let (_, width, height) =
        aterm_jpeg::decode_rgba8(&jpeg[..jpeg.len() - 200], 8192).expect("partial decode");
    assert_eq!((width, height), (33, 21));
}

#[test]
fn malformed_marker_segments_are_refused() {
    let base = fixture("b420.jpg");

    // DRI segment whose payload is 3 bytes instead of 2 (total segment length 5).
    let mut bad_dri = base[..2].to_vec();
    bad_dri.extend_from_slice(&[0xFF, 0xDD, 0x00, 0x05, 0x00, 0x04, 0x00]);
    bad_dri.extend_from_slice(&base[2..]);
    assert!(aterm_jpeg::decode_rgba8(&bad_dri, 8192).is_none());

    // DQT table with precision nibble > 1.
    let mut bad_dqt = base.clone();
    let dqt = bad_dqt
        .windows(2)
        .position(|pair| pair == [0xFF, 0xDB])
        .expect("DQT");
    bad_dqt[dqt + 4] = (2 << 4) | (bad_dqt[dqt + 4] & 0x0F);
    assert!(aterm_jpeg::decode_rgba8(&bad_dqt, 8192).is_none());

    // SOF0 with duplicate component IDs.
    let mut dup_sof = base.clone();
    let sof = dup_sof
        .windows(2)
        .position(|pair| pair == [0xFF, 0xC0])
        .expect("SOF0");
    dup_sof[sof + 13] = dup_sof[sof + 10];
    assert!(aterm_jpeg::decode_rgba8(&dup_sof, 8192).is_none());

    // SOF0 with trailing garbage byte inside the declared segment length.
    let sof_len = u16::from_be_bytes([base[sof + 2], base[sof + 3]]) as usize;
    let mut long_sof = base[..sof + 2].to_vec();
    long_sof.extend_from_slice(&((sof_len + 1) as u16).to_be_bytes());
    long_sof.extend_from_slice(&base[sof + 4..sof + 2 + sof_len]);
    long_sof.push(0);
    long_sof.extend_from_slice(&base[sof + 2 + sof_len..]);
    assert!(aterm_jpeg::decode_rgba8(&long_sof, 8192).is_none());

    // SOS with duplicate scan component selector.
    let mut dup_sos = base.clone();
    let sos = dup_sos
        .windows(2)
        .position(|pair| pair == [0xFF, 0xDA])
        .expect("SOS");
    dup_sos[sos + 7] = dup_sos[sos + 5];
    assert!(aterm_jpeg::decode_rgba8(&dup_sos, 8192).is_none());

    // Progressive SOS with invalid successive approximation (Ah = 3, Al = 1 -> Ah != Al + 1).
    let prog = fixture("p420.jpg");
    let psos = prog
        .windows(2)
        .position(|pair| pair == [0xFF, 0xDA])
        .expect("progressive SOS");
    let psos_len = u16::from_be_bytes([prog[psos + 2], prog[psos + 3]]) as usize;
    let mut bad_approx = prog.clone();
    bad_approx[psos + 2 + psos_len - 1] = 0x31;
    assert!(aterm_jpeg::decode_rgba8(&bad_approx, 8192).is_none());
}
