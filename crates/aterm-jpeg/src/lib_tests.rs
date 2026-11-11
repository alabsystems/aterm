// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::huffman::DECODED;
use super::{MAX_PROGRESSIVE_SCANS, REFINE_EXAMINED, decode_rgba8, exif_orientation};

/// A scan header's `(Ss, Se, Ah << 4 | Al)` and the data after it.
type ScanBytes<'a> = ((u8, u8, u8), &'a [u8]);

/// [`framed`] grey, with no restart interval.
fn generated(progressive: bool, side: u8, symbols: [u8; 2], scans: &[ScanBytes<'_>]) -> Vec<u8> {
    framed(progressive, side, &[0x11], 0, symbols, scans)
}

/// A `side`-square frame of one component per `sampling` entry (its
/// `H << 4 | V`), quantizer 1, a restart every `restart_interval` MCUs (0
/// for none), whose DC and AC tables code `symbols` each as the one-bit
/// code `0`; then one SOS per entry of `scans`, a DC or sequential one
/// over every component and an AC one over the first.
fn framed(
    progressive: bool,
    side: u8,
    sampling: &[u8],
    restart_interval: u8,
    symbols: [u8; 2],
    scans: &[ScanBytes<'_>],
) -> Vec<u8> {
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x43, 0x00];
    jpeg.extend([1u8; 64]);
    let count = sampling.len() as u8;
    let sof = if progressive { 0xC2 } else { 0xC0 };
    jpeg.extend([0xFF, sof, 0x00, 8 + 3 * count, 0x08]);
    jpeg.extend([0x00, side, 0x00, side, count]);
    for (id, &factors) in (1u8..).zip(sampling) {
        jpeg.extend([id, factors, 0x00]);
    }
    for (class, symbol) in [0x00, 0x10].into_iter().zip(symbols) {
        jpeg.extend([0xFF, 0xC4, 0x00, 0x14, class, 0x01]);
        jpeg.extend([0u8; 15]);
        jpeg.push(symbol);
    }
    if restart_interval > 0 {
        jpeg.extend([0xFF, 0xDD, 0x00, 0x04, 0x00, restart_interval]);
    }
    for &((start, end, approximation), data) in scans {
        let members = if start == 0 { count } else { 1 };
        jpeg.extend([0xFF, 0xDA, 0x00, 6 + 2 * members, members]);
        for id in 1..=members {
            jpeg.extend([id, 0x00]);
        }
        jpeg.extend([start, end, approximation]);
        jpeg.extend_from_slice(data);
    }
    jpeg.extend([0xFF, 0xD9]);
    jpeg
}

/// One data byte, then `intervals` restart markers with nothing between.
fn byte_then_restarts(intervals: usize) -> Vec<u8> {
    let mut data = vec![0x00];
    for marker in (0xD0..=0xD7).cycle().take(intervals) {
        data.extend([0xFF, marker]);
    }
    data
}

/// Whether every pixel outside the top-left 8x8 block of a `side`-wide image is mid grey.
fn grey_past_the_first_block(rgba: &[u8], side: usize) -> bool {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .all(|(i, pixel)| (i % side < 8 && i / side < 8) || *pixel == [128, 128, 128, 255])
}

#[test]
fn a_sequential_frame_codes_each_component_in_one_scan() {
    let scan = ((0, 63, 0), &[0x00][..]);
    assert!(decode_rgba8(&generated(false, 16, [0, 0], &[scan]), 64).is_some());
    assert!(
        decode_rgba8(&generated(false, 16, [0, 0], &[scan, scan]), 64).is_none(),
        "a second scan of a coded component"
    );
    // The review's crasher, small: header after header with no data between.
    let empty = vec![((0, 63, 0), &[][..]); 1300];
    assert!(decode_rgba8(&generated(false, 16, [0, 0], &empty), 64).is_none());
}

#[test]
fn a_scan_that_codes_nothing_is_passed_over() {
    // The AC code `0` is a size-8 coefficient, so reading the empty scan's
    // missing bits as zeros would paint every block.
    let dc = ((0, 0, 0), &[0x00][..]);
    let ac = ((1, 63, 0), &[][..]);
    let (without, ..) = decode_rgba8(&generated(true, 16, [0, 0x08], &[dc]), 64).expect("DC alone");
    let (with, ..) = decode_rgba8(&generated(true, 16, [0, 0x08], &[dc, ac]), 64).expect("with it");
    assert!(with == without, "the empty AC scan changed the image");
}

#[test]
fn a_progressive_frame_has_at_most_its_scan_limit() {
    let first = ((0, 0, 0x01), &[0x00][..]);
    let refine = ((0, 0, 0x10), &[0x00][..]);
    let mut scans = vec![first];
    scans.resize(MAX_PROGRESSIVE_SCANS, refine);
    assert!(decode_rgba8(&generated(true, 16, [0, 0], &scans), 64).is_some());
    scans.push(refine);
    assert!(decode_rgba8(&generated(true, 16, [0, 0], &scans), 64).is_none());
}

#[test]
fn a_scan_stops_where_its_data_does() {
    // An AC coefficient takes nine bits, so the two bytes run out in the
    // first block; the other three keep what the DC scan gave them.
    let dc = ((0, 0, 0), &[0x00][..]);
    let ac = ((1, 63, 0), &[0x00, 0x00][..]);
    let (without, ..) = decode_rgba8(&generated(true, 16, [0, 0x08], &[dc]), 64).expect("DC");
    let (with, ..) = decode_rgba8(&generated(true, 16, [0, 0x08], &[dc, ac]), 64).expect("AC");
    assert!(with != without, "the first block took its coefficients");
    let (got, kept) = (with.as_chunks::<4>().0, without.as_chunks::<4>().0);
    for (i, (got, kept)) in got.iter().zip(kept).enumerate() {
        let first_block = i % 16 < 8 && i / 16 < 8;
        assert!(
            first_block || got == kept,
            "pixel {i} past the data changed"
        );
    }
}

#[test]
fn a_sequential_block_no_data_reaches_is_mid_grey() {
    // A DC difference takes nine bits, so the byte runs out in the first block.
    let scan = ((0, 63, 0), &[0x00][..]);
    let (rgba, ..) = decode_rgba8(&generated(false, 16, [0x08, 0], &[scan]), 64).expect("decodes");
    for (i, pixel) in rgba.as_chunks::<4>().0.iter().enumerate() {
        let first_block = i % 16 < 8 && i / 16 < 8;
        assert!(
            first_block || *pixel == [128, 128, 128, 255],
            "pixel {i}: {pixel:?}"
        );
    }
}

#[test]
fn a_restart_interval_without_data_decodes_nothing() {
    // 64 blocks and a restart after each: the AC scan's one byte codes
    // four coefficients (two bits each) of the first block, and the other
    // 63 intervals hold only their markers. Read as zero bits, they would
    // give each block 63 coefficients.
    let data = byte_then_restarts(63);
    let jpeg = framed(true, 64, &[0x11], 1, [0, 0x01], &[((1, 63, 0), &data[..])]);
    DECODED.set(0);
    let (rgba, ..) = decode_rgba8(&jpeg, 64).expect("decodes");
    let decoded = DECODED.get();
    assert!(decoded <= 2 * 64, "{decoded} symbols for 64 blocks");
    assert!(
        grey_past_the_first_block(&rgba, 64),
        "a block past the data changed"
    );
}

#[test]
fn an_interleaved_mcu_stops_where_its_data_does() {
    // 4:2:0 has six blocks an MCU; 16 MCUs and a restart after each. A DC
    // difference takes nine bits, so the one byte runs out in the first
    // block and the other 15 intervals hold only their markers. Read as
    // zero bits, they would give every block a DC difference.
    let data = byte_then_restarts(15);
    let scan = ((0, 0, 0), &data[..]);
    let jpeg = framed(true, 64, &[0x22, 0x11, 0x11], 1, [0x08, 0], &[scan]);
    DECODED.set(0);
    let (rgba, ..) = decode_rgba8(&jpeg, 64).expect("decodes");
    let decoded = DECODED.get();
    assert!(decoded <= 16, "{decoded} symbols for 16 MCUs of 6 blocks");
    let pixels = rgba.as_chunks::<4>().0;
    assert!(
        pixels.iter().all(|pixel| *pixel == [128, 128, 128, 255]),
        "a block past the data changed"
    );
}

#[test]
fn a_refinement_touches_only_the_coefficients_it_codes() {
    // 64 blocks with no nonzero AC coefficient, each refinement an end of
    // band: there is nothing to correct, so nothing to walk.
    let bits = [0x00; 8];
    let mut scans = vec![((0, 0, 0), &bits[..]), ((1, 63, 0x01), &bits[..])];
    scans.extend([((1, 63, 0x10), &bits[..]); 8]);
    REFINE_EXAMINED.set(0);
    assert!(decode_rgba8(&generated(true, 64, [0, 0], &scans), 64).is_some());
    let examined = REFINE_EXAMINED.get();
    assert!(
        examined <= 8 * 64,
        "{examined} coefficients for 8 x 64 blocks"
    );
}

#[test]
fn what_is_not_a_framed_jpeg_is_refused() {
    assert!(decode_rgba8(b"\x89PNG\r\n\x1a\n", 64).is_none());
    assert!(decode_rgba8(&[0xFF, 0xD8, 0xFF, 0xD9], 64).is_none());
    assert!(decode_rgba8(&[], 64).is_none());
}

#[test]
fn the_exif_orientation_is_read_in_both_byte_orders() {
    let little = [
        b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0,
    ];
    assert_eq!(exif_orientation(&little), Some(6));
    let mut wrong_count = little;
    wrong_count[14] = 3;
    assert_eq!(exif_orientation(&wrong_count), None);
    let huge_ifd = [b'I', b'I', 42, 0, 0xFF, 0xFF, 0xFF, 0xFF];
    assert_eq!(exif_orientation(&huge_ifd), None);
    let big = [
        b'M', b'M', 0, 42, 0, 0, 0, 8, 0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 8, 0, 0,
    ];
    assert_eq!(exif_orientation(&big), Some(8));
    assert_eq!(exif_orientation(b"XX"), None);
}
