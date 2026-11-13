// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{InkCell, InkWalk};

#[test]
fn merge_walk_equals_binary_search_probe() {
    for subset in 0u16..256 {
        let slice: Vec<InkCell> = (0u8..8)
            .filter(|col| subset >> col & 1 == 1)
            .map(|col| InkCell {
                row: 0,
                col: u16::from(col),
                color: [col, 0x40 | col, 0x80 | col],
            })
            .collect();
        for a in 0..=8u16 {
            for b in a..=8 {
                for c in b..=8 {
                    let mut walk = InkWalk::new(&slice);
                    for col in [a, b, c] {
                        let probe = slice
                            .binary_search_by(|cell| cell.col.cmp(&col))
                            .ok()
                            .map(|i| slice[i].color);
                        assert_eq!(
                            walk.at(col),
                            probe,
                            "slice {subset:08b}, queries {a} {b} {c}, at {col}"
                        );
                    }
                }
            }
        }
    }
}
