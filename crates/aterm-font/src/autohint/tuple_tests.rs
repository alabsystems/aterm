// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from read-fonts 0.43.2 src/tables/gvar.rs, src/tables/variations.rs and
// generated/generated_gvar.rs, generated/generated_variations.rs, and modified by the aterm
// project in 2026: the `gvar` glyph variation data as FreeType's 16.16 loader reads it (tuple
// headers, shared and private packed point numbers, packed deltas, the fixed-point tuple
// scalar), specialized to point deltas in 16.16, with every read checked.
// See ../../LICENSE-FONTATIONS for the upstream license.

use super::*;

fn decode_points(bytes: &[u8]) -> Option<Vec<u16>> {
    let packed = PackedPointNumbers { data: bytes };
    if packed.count() == 0 {
        None
    } else {
        Some(packed.iter().collect())
    }
}

#[test]
fn packed_points() {
    assert_eq!(decode_points(&[0]), None);
    assert_eq!(decode_points(&[0x80, 0]), None);
    assert_eq!(decode_points(&[0x02, 0x01, 0x09, 0x06]), Some(vec![9, 15]));
    assert_eq!(
        decode_points(&[0x02, 0x81, 0xbe, 0xef, 0x10, 0x01]),
        Some(vec![0xbeef, 0xcef0])
    );
    assert_eq!(
        decode_points(&[0x05, 0x01, 0x01, 0x01, 0x02, 0x05, 0x08, 0x03]),
        Some(vec![1, 2, 7, 15, 18])
    );
    assert_eq!(
        decode_points(&[0x03, 0x82, 0xbe, 0xef, 0x10, 0x01, 0x00, 0x02]),
        Some(vec![0xbeef, 0xcef0, 0xcef2])
    );
}

#[test]
fn packed_point_byte_len() {
    fn count_bytes(bytes: &[u8]) -> usize {
        PackedPointNumbers { data: bytes }.total_len()
    }
    static CASES: &[&[u8]] = &[
        &[0],
        &[0x80, 0],
        &[0x02, 0x01, 0x09, 0x06],
        &[0x02, 0x81, 0xbe, 0xef, 0x10, 0x01],
        &[0x05, 0x01, 0x01, 0x01, 0x02, 0x05, 0x08, 0x03],
    ];
    for case in CASES {
        assert_eq!(count_bytes(case), case.len(), "{case:?}");
    }
}

#[test]
fn packed_deltas() {
    static INPUT: &[u8] = &[0x83, 0x40, 0x01, 0x02, 0x01, 0x81, 0x80];
    let deltas = PackedDeltas {
        data: INPUT,
        count: None,
    };
    assert_eq!(deltas.count_or_compute(), 7);
    assert_eq!(
        DeltaRunIter::new(Cursor::new(INPUT), None).collect::<Vec<_>>(),
        &[0, 0, 0, 0, 258, -127, -128]
    );
    assert_eq!(
        DeltaRunIter::new(Cursor::new(&[0x81]), None).collect::<Vec<_>>(),
        &[0, 0]
    );
}

#[test]
fn packed_deltas_spec() {
    static INPUT: &[u8] = &[
        0x03, 0x0A, 0x97, 0x00, 0xC6, 0x87, 0x41, 0x10, 0x22, 0xFB, 0x34,
    ];
    static EXPECTED: &[i32] = &[10, -105, 0, -58, 0, 0, 0, 0, 0, 0, 0, 0, 4130, -1228];
    let deltas = PackedDeltas {
        data: INPUT,
        count: None,
    };
    assert_eq!(deltas.count_or_compute(), EXPECTED.len());
    assert_eq!(
        DeltaRunIter::new(Cursor::new(INPUT), None).collect::<Vec<_>>(),
        EXPECTED
    );
}

#[test]
fn packed_point_split() {
    static INPUT: &[u8] = &[2, 1, 1, 2, 1, 205, 143, 1, 8, 0, 1, 202, 59, 1, 255, 0];
    let (points, data) = PackedPointNumbers::split_off_front(INPUT);
    assert_eq!(points.count(), 2);
    assert_eq!(points.iter().collect::<Vec<_>>(), &[1, 3]);
    assert_eq!(points.total_len(), 4);
    assert_eq!(data.len(), INPUT.len() - 4);
}

#[test]
fn packed_points_dont_panic() {
    static ALL_POINTS: &[u8] = &[0];
    let (all_points, _) = PackedPointNumbers::split_off_front(ALL_POINTS);
    assert_eq!(all_points.iter().count(), u16::MAX as usize);
}

#[test]
fn packed_point_numbers_avoid_overflow() {
    let buf = vec![0xFF; 0xFFFF];
    let iter = PackedPointNumbersIter {
        count: 0xFFFF,
        seen: 0,
        last_val: 0,
        current_run: PointRunIter {
            remaining: 0,
            two_bytes: false,
            cursor: Cursor::new(&buf),
        },
    };
    let _ = iter.count();
}

/// A walk ends at its limit, not at the tuple's highest point number:
/// the phantom points of an empty glyph whose tuple names point 65,534
/// are four turns away, not 65,535.
#[test]
fn a_delta_walk_stops_at_its_limit() {
    // One private point number, 65,534 (a run of one word), then its
    // deltas, (+5, +5).
    let data = [0x01, 0x80, 0xFF, 0xFE, 0x00, 5, 0x00, 5];
    let tuple = TupleVariation {
        header: TupleVariationHeader {
            data: &[0, 8, 0xA0, 0x00, 0, 0],
            axis_count: 1,
        },
        serialized_data: &data,
        shared_point_numbers: None,
    };
    let mut walk = tuple.deltas(4);
    assert_eq!(walk.next(), None);
    assert_eq!(walk.cur, 4);
    assert_eq!(
        tuple.deltas(usize::MAX).collect::<Vec<_>>(),
        [GlyphDelta {
            position: 65_534,
            x_delta: 5,
            y_delta: 5
        }]
    );
}

#[test]
fn accumulate_dense_and_sparse() {
    let header = [0, 0, 0x80, 0x00, 0x40, 0x00];
    let header = TupleVariationHeader {
        data: &header,
        axis_count: 1,
    };
    let dense = [0x00, 0x03, 1, 2, 3, 4, 0x03, 5, 6, 7, 8];
    let tuple = TupleVariation {
        header: TupleVariationHeader {
            data: &[0, 0, 0xA0, 0x00, 0x40, 0x00],
            ..header
        },
        serialized_data: &dense,
        shared_point_numbers: None,
    };
    assert!(tuple.has_deltas_for_all_points());
    let mut deltas = [Point::default(); 4];
    let half = Fixed::from_bits(0x8000);
    tuple.accumulate_dense_deltas(&mut deltas, half).unwrap();
    let expected: Vec<_> = [(1, 5), (2, 6), (3, 7), (4, 8)]
        .iter()
        .map(|&(x, y)| Point::new(Fixed::from_i32(x) * half, Fixed::from_i32(y) * half))
        .collect();
    assert_eq!(&deltas[..], &expected[..]);
    let sparse = [0x02, 0x01, 0x01, 0x02, 0x01, 10, 20, 0x01, 30, 40];
    let tuple = TupleVariation {
        serialized_data: &sparse,
        ..tuple
    };
    assert!(!tuple.has_deltas_for_all_points());
    let mut deltas = [Point::default(); 4];
    let mut flags = [PointFlags::default(); 4];
    tuple
        .accumulate_sparse_deltas(&mut deltas, &mut flags, Fixed::ONE)
        .unwrap();
    assert_eq!(
        deltas[1],
        Point::new(Fixed::from_i32(10), Fixed::from_i32(30))
    );
    assert_eq!(
        deltas[3],
        Point::new(Fixed::from_i32(20), Fixed::from_i32(40))
    );
    let marked: Vec<bool> = flags
        .iter()
        .map(|f| f.has_marker(PointMarker::HAS_DELTA))
        .collect();
    assert_eq!(marked, [false, true, false, true]);
    let explicit: Vec<_> = tuple.deltas(4).collect();
    assert_eq!(
        explicit,
        [
            GlyphDelta {
                position: 1,
                x_delta: 10,
                y_delta: 30
            },
            GlyphDelta {
                position: 3,
                x_delta: 20,
                y_delta: 40
            }
        ]
    );
}
