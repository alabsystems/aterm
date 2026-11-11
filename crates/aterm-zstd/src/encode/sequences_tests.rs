// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{LL_CODES, MIN_MATCH, ML_CODES, ll_code, ml_code, offset_value};
use crate::decode::sequences::resolve_offset;

/// The shortcut code functions agree with the code tables at every length a block can
/// hold, and each value sits inside its code's range.
#[test]
fn length_codes_cover_every_value() {
    // A block holds 128 KiB: a sequence carries fewer literals than that (it ends in a
    // match), and a match up to the whole block.
    for v in 0..(1u32 << 17) - MIN_MATCH {
        let c = ll_code(v);
        let (base, bits) = LL_CODES[usize::from(c)];
        assert!(
            base <= v && v - base < 1 << bits,
            "literal length {v} as code {c}"
        );
        let ml = v + MIN_MATCH;
        let c = ml_code(ml);
        let (base, bits) = ML_CODES[usize::from(c)];
        assert!(
            base <= ml && ml - base < 1 << bits,
            "match length {ml} as code {c}"
        );
    }
}

/// Every distance, after literals or not, resolves through a decoder's repeat-offset
/// rules to itself, the history moving as the decoder's does.
#[test]
fn offset_values_resolve_to_their_distance() {
    let mut reps: [u32; 3] = [1, 4, 8];
    let mut x = 99u64;
    for i in 0..20_000u32 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let distance = match x % 5 {
            0 => reps[0],
            1 => reps[1],
            2 => reps[2],
            3 => reps[0].saturating_sub(1).max(1),
            _ => 1 + (x >> 8) as u32 % 100_000,
        };
        let lit_len = (i % 3) * ((x >> 40) as u32 % 4);
        let ov = offset_value(distance, lit_len, &reps);
        assert_eq!(resolve_offset(ov, lit_len as usize, &mut reps), distance);
    }
}

/// The negative control: an offset value resolved with the literal length it was not
/// made for names another distance.
#[test]
fn a_repeat_code_under_the_other_literal_rule_differs() {
    let reps = [10, 20, 30];
    let ov = offset_value(20, 0, &reps);
    assert_eq!(ov, 1);
    assert_ne!(resolve_offset(ov, 5, &mut reps.clone()), 20);
}
