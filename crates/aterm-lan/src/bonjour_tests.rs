// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn txt_records_round_trip_and_bound_their_strings() {
    let strings = vec![b"v=1".to_vec(), Vec::new(), b"k".to_vec()];
    let record = txt_record(&strings).unwrap();
    assert_eq!(record, b"\x03v=1\x00\x01k");
    assert_eq!(txt_strings(&record), strings);
    assert_eq!(
        txt_strings(b"\x09ab"),
        vec![b"ab".to_vec()],
        "a short tail is cut"
    );
    assert!(txt_record(&[vec![0; 256]]).is_err());
}
