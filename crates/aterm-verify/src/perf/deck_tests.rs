// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_gated_keys_are_the_busy_2238_rows_as_frames_per_second() {
    let mut tsv = String::from("scene\tform\tdevice_px\tframe\tp50_us\tp95_us\tpatched_share\n");
    for form in ["Folded", "Full"] {
        for (frame, p50) in [("needle", 200.0), ("grid", 500.0), ("reading", 1250.0)] {
            tsv.push_str(&format!("busy\t{form}\t2238\t{frame}\t{p50}\t1.0\t0.1\n"));
            tsv.push_str(&format!("worst\t{form}\t2238\t{frame}\t9999\t1.0\t0.1\n"));
            tsv.push_str(&format!("busy\t{form}\t700\t{frame}\t9999\t1.0\t0.1\n"));
        }
    }
    let got = report_of(&tsv).expect("every row");
    assert_eq!(got.len(), DECK_KEYS.len());
    assert_eq!(got[0], ("folded_needle_fps", 5000.0));
    assert_eq!(got[2], ("folded_reading_fps", 800.0));
    assert_eq!(got[4], ("full_grid_fps", 2000.0));
    assert!(DECK_KEYS.iter().zip(&got).all(|(k, (g, _))| k == g));
    let short = tsv.replace("busy\tFull\t2238\tgrid", "busy\tFull\t2238\tgrids");
    assert!(report_of(&short).is_err());
}
