// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn render(value: u32) -> String {
    let mut buf = [0u8; U32_MAX_DIGITS];
    let bytes = fmt_u32_decimal(value, &mut buf);
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn decimal_formatter_matches_std_for_many_values() {
    for v in 0u32..2000 {
        assert_eq!(render(v), v.to_string(), "mismatch at {v}");
    }
    for v in [
        0,
        1,
        9,
        10,
        11,
        99,
        100,
        255,
        999,
        1000,
        65_535,
        1_000_000,
        4_294_967_294,
        u32::MAX,
    ] {
        assert_eq!(render(v), v.to_string(), "mismatch at {v}");
    }
}
