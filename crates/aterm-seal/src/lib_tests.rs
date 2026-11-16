// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// The seal is `aterm_hash`'s FNV-1a of the text with its digits zeroed, and its value does not
/// move: every generated file's head holds one (`verify census` reads them).
#[test]
fn a_seals_digest_is_aterm_hashs_fold_and_a_stored_seal_does_not_move() {
    assert_eq!(digest("", &(0..0)), aterm_hash::FNV64_OFFSET);
    assert_eq!(digest("foobar", &(0..0)), aterm_hash::fnv1a_64(b"foobar"));
    assert_eq!(digest("foobar", &(0..0)), 0x8594_4171_f739_67e8);
    assert_eq!(digest("ab123c", &(2..5)), aterm_hash::fnv1a_64(b"ab000c"));
}

fn generated() -> String {
    format!(
        "// Copyright 2026 Andrew Yates\n// SPDX-License-Identifier: Apache-2.0\n{}\n\n\
         // What the table holds.\npub const N: u32 = 1;\n",
        line("//", "`gen tables`")
    )
}

#[test]
fn a_stamped_file_holds_and_any_edit_breaks_it() {
    let stamped = stamp(&generated()).expect("the head carries a seal line");
    assert_eq!(check(&stamped), Seal::Holds { line: 3 });
    assert_eq!(stamp(&stamped).as_deref(), Ok(stamped.as_str()));
    let pruned = stamped.replace("// What the table holds.\n", "");
    assert!(matches!(check(&pruned), Seal::Broken { line: 3, .. }));
    let retitled = stamped.replace("`gen tables`", "`gen`");
    assert!(matches!(check(&retitled), Seal::Broken { line: 3, .. }));
    assert!(matches!(check(&generated()), Seal::Broken { line: 3, .. }));
}

#[test]
fn a_seal_is_read_only_from_the_head_and_only_beside_the_mark() {
    assert_eq!(check("fn main() {}\n"), Seal::Absent);
    assert_eq!(check("// seal 0000000000000000\n"), Seal::Absent);
    assert_eq!(
        check("// @generated seal 00000000000000000\n"),
        Seal::Absent
    );
    let late = format!("{}{}\n", "//\n".repeat(HEAD_LINES), line("//", "x"));
    assert_eq!(check(&late), Seal::Absent);
    assert!(stamp(&late).is_err());
}
