// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn generation_and_queued_revision_have_one_bounded_canonical_shape() {
    for generation in [[0; 16], [255; 16], [9; 16]] {
        let line = request(generation);
        let rest = line.strip_prefix("allocation ").unwrap();
        assert_eq!(parse_attach(rest), Some(generation));
    }
    for rest in [
        "attach",
        "attach generation=a",
        "attach generation=0000000000000000000000000000000G",
        "attach generation=00000000000000000000000000000000 extra",
        "other generation=00000000000000000000000000000000",
    ] {
        assert!(parse_attach(rest).is_none(), "{rest}");
    }
    assert_eq!(parse_queued(queued(u64::MAX).trim_end()), Some(u64::MAX));
    for value in [
        "0",
        "01",
        "+1",
        "-1",
        "1 extra",
        "18446744073709551616",
        "1\n",
    ] {
        assert!(parse_queued(&format!("{QUEUED}{value}")).is_none());
    }
}
