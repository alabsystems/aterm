// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn push_hex4_matches_format() {
    for v in [0u16, 1, 0xf, 0x10, 0xabc, 0x1234, 0xffff] {
        let mut s = String::new();
        push_hex4(&mut s, v);
        assert_eq!(s, format!("{v:04x}"));
    }
}
