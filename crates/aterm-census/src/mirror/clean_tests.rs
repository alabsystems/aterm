// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn a_header_names_its_items() {
    let harness = "#![allow(dead_code)]\n// D-2(b) rows.\n\n\
        // aterm: crates/aterm-log/src/lib.rs `MAX`, `Level` (derives dropped: a `Clone`\n\
        // reads through a pointer), the `use` of its variants, crates/x/src/a.rs\n\
        // `<A as From<bool>>::from`, the body of `g` (rewritten), `Level::f`. The `x`.\n\
        //\n// `ignored`\n";
    assert_eq!(
        header_items(harness),
        Some(vec![
            "crates/aterm-log/src/lib.rs#MAX".to_string(),
            "crates/aterm-log/src/lib.rs#Level".to_string(),
            "crates/x/src/a.rs#<A as From<bool>>::from".to_string(),
            "crates/x/src/a.rs#g".to_string(),
            "crates/aterm-log/src/lib.rs#Level::f".to_string(),
        ])
    );
    assert_eq!(header_items("// no header\n"), None);
    assert!(covers("f#MASK_*", "f#MASK_A") && !covers("f#MASK_*", "g#MASK_A"));
}

#[test]
fn proved_rows_are_aterm_rows_graded_proved() {
    let text = "# blob\trow\n\
                bv_x\tx\t0\tsym\tproved (calculus)\t{}\taterm-x src/lib.rs f\n\
                aterm\ty\t1\tsym\tproved (calculus)\n\
                wrong_x\tx_wrong\tunsafe\t`x` panics\n\
                collision\tc\t0\tsym\tproved (calculus)\n";
    assert_eq!(
        proved_rows(text),
        BTreeSet::from(["x".to_string(), "y".to_string()])
    );
}
