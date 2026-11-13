// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_cutoff_round_trips_and_nothing_else_decodes_as_one() {
    let actor = [7; 48];
    for through in [
        None,
        Some(Frontier {
            sequence: 3,
            digest: [9; 32],
        }),
    ] {
        let cut = Cut { actor, through };
        assert_eq!(decode(&encode(&cut)), Some(cut));
    }
    let mut torn = encode(&Cut {
        actor,
        through: None,
    });
    torn[LEN - 1] = 1;
    assert_eq!(decode(&torn), None, "a zero sequence names no digest");
    assert_eq!(decode(br#"{"v":1}"#), None);
    assert_eq!(decode(&torn[..LEN - 1]), None);
}
