// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

use super::*;

#[test]
fn text_round_trips_on_both_sides_of_the_inline_limit() {
    for text in ["", "a", "é", "\u{1F600}", "abcdefghijklmnopqrstuv", "abcdefghijklmnopqrstuvw"] {
        let s = SmolStr::new(text);
        assert_eq!(s.as_str(), text);
        assert_eq!(s.clone(), s);
        assert_eq!(format!("{s:?}"), format!("{text:?}"));
    }
    assert!(matches!(SmolStr::new("abcdefghijklmnopqrstuv").0, Repr::Inline { .. }));
    assert!(matches!(SmolStr::new("abcdefghijklmnopqrstuvw").0, Repr::Shared(_)));
    #[cfg(target_pointer_width = "64")]
    assert_eq!(std::mem::size_of::<SmolStr>(), 24);
}
