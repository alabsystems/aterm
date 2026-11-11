// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{BitReader, HuffmanTable, has_coded_data};

#[test]
fn a_segment_of_markers_and_fill_holds_no_data() {
    let restarts_and_fill = [0xFF, 0xD0, 0xFF, 0xFF, 0xD7, 0xFF, 0xD9];
    assert!(!has_coded_data(&restarts_and_fill, 0));
    assert!(!has_coded_data(&[0xFF, 0xD0, 0xFF], 0));
    assert!(!has_coded_data(&[], 0));
    assert!(has_coded_data(&[0xFF, 0xD0, 0x12, 0xFF, 0xD9], 0));
    assert!(has_coded_data(&[0xFF, 0xFF, 0x00, 0xFF, 0xD9], 0));
}

#[test]
fn the_standard_dc_table_decodes_every_code() {
    let counts = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
    let symbols = [0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
    let table = HuffmanTable::new(&counts, &symbols).expect("table");
    let mut reader = BitReader::new(&[0b0011_1111, 0b1100_0000], 0);
    assert_eq!(reader.decode(&table), Some(0));
    assert_eq!(reader.decode(&table), Some(11));
}

#[test]
fn stuffing_is_undone_and_a_marker_ends_the_segment() {
    let mut reader = BitReader::new(&[0xFF, 0x00, 0xA5, 0xFF, 0xD9], 0);
    assert_eq!(reader.bits(8), 0xFF);
    assert_eq!(reader.bits(8), 0xA5);
    assert_eq!(reader.bits(16), 0);
    assert_eq!(reader.position(), 3, "stopped at the marker");
}

#[test]
fn an_oversubscribed_code_is_refused() {
    let mut counts = [0u8; 16];
    counts[0] = 3;
    assert!(HuffmanTable::new(&counts, &[0, 1, 2]).is_none());
}
