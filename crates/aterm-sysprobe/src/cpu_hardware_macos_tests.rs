// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
fn rows(states: &[u32]) -> Vec<u8> {
    states
        .iter()
        .flat_map(|f| f.to_le_bytes().into_iter().chain([0; 4]))
        .collect()
}

#[test]
fn dvfs_table_rejects_missing_partial_or_nonmonotonic_data() {
    assert_eq!(decode_table(&[]), None);
    assert_eq!(decode_table(&[0; 7]), None);
    assert_eq!(decode_table(&[0; 8]), None);
    let table = rows(&[1_020_000_000, 2_000_000_000]);
    assert_eq!(decode_table(&table), Some(vec![1020.0, 2000.0]));
    let mut wrong = table.clone();
    wrong.extend_from_slice(&table[..8]);
    let out_of_order = decode_table(&wrong).expect("decodes");
    assert_eq!(ordinal(out_of_order), None);
    assert_eq!(
        ordinal(decode_table(&table).expect("decodes")),
        Some(vec![1020.0, 2000.0])
    );
}

/// A table in Hz (M1–M3) and one in kHz (M4, M5) read the same MHz, and the range takes each
/// table's lowest and highest state whatever their order: M5's Super table (N1) repeats a
/// lower state near its top. CONTROL: its ordinal map refuses that table.
#[test]
fn the_range_reads_hz_or_khz_tables_in_any_order() {
    assert_eq!(
        decode_table(&rows(&[972_000, 3_048_000])),
        decode_table(&rows(&[972_000_000, 3_048_000_000]))
    );
    let m5_super = decode_table(&rows(&[
        1_308_000, 1_608_000, 1_932_000, 2_220_000, 2_508_000, 2_820_000, 3_072_000, 3_300_000,
        3_516_000, 3_720_000, 3_876_000, 4_020_000, 4_176_000, 4_248_000, 4_272_000, 4_332_000,
        4_428_000, 4_416_000, 4_464_000,
    ]))
    .expect("decodes");
    assert_eq!(span(&m5_super), (1308.0, 4464.0));
    assert_eq!(ordinal(m5_super), None, "control");
}

/// This Mac's power manager gives both tables, each a range a core clocks within.
#[test]
fn this_macs_dvfs_range_reads_or_says_why() {
    match dvfs_range() {
        Ok(r) => {
            for (lo, hi) in [r.e_mhz, r.p_mhz] {
                assert!((100.0..hi).contains(&lo) && hi < 10_000.0, "{r:?}");
            }
        }
        // An Intel Mac has no pmgr tables.
        Err(why) => assert!(why.starts_with("pmgr"), "{why}"),
    }
}
