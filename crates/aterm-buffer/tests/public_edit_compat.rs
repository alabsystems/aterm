// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Source-compatibility guard for the frozen public Edit algebra.

use aterm_buffer::{Edit, LineId};

fn require_string(value: String) -> usize {
    value.len()
}

fn payload_len(edit: Edit) -> usize {
    // Exhaustive on purpose: the CLOSED algebra and its String payloads are the
    // public source contract, not merely an internal representation detail.
    match edit {
        Edit::AppendLine(text) => require_string(text),
        Edit::SetLine(_, text) => require_string(text),
        Edit::ClearLine(_) => 0,
    }
}

#[test]
fn direct_string_variant_constructors_and_exhaustive_match_remain_source_compatible() {
    let append = String::from("append");
    let replace = String::from("replace");

    assert_eq!(payload_len(Edit::AppendLine(append)), 6);
    assert_eq!(payload_len(Edit::SetLine(LineId(7), replace)), 7);
    assert_eq!(payload_len(Edit::ClearLine(LineId(7))), 0);
}
