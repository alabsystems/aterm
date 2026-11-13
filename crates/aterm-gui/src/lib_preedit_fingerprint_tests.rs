// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// NEGATIVE CONTROL: empty preedit is `None` (unconditionally disjoint from every non-empty `Some(...)`),
/// and both `text_len` and `caret` (including `None` vs `Some(usize::MAX)`) are stored structurally.
#[test]
fn empty_is_zero_and_text_and_caret_both_move_it() {
    assert_eq!(super::preedit_fingerprint("", None), None);
    assert_eq!(
        super::preedit_fingerprint("", Some(3)),
        None,
        "no text, no overlay"
    );
    let a = super::preedit_fingerprint("日本", None);
    let b = super::preedit_fingerprint("日本語", None);
    let c = super::preedit_fingerprint("日本", Some(3));
    let d = super::preedit_fingerprint("日本", Some(usize::MAX));
    assert_ne!(a, None);
    assert_ne!(a, b, "text length moves the key");
    assert_ne!(a, c, "the caret alone moves the key");
    assert_ne!(a, d, "None and Some(usize::MAX) never alias");
}
