// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(target_os = "macos")]
#[test]
fn an_alt_window_is_both_option_keys_and_a_composing_one_neither() {
    use aterm_window::platform::macos::OptionAsAlt;
    assert_eq!(super::option_as_alt_for(true), OptionAsAlt::Both);
    assert_eq!(super::option_as_alt_for(false), OptionAsAlt::None);
}

#[test]
fn only_a_macos_option_that_is_no_alt_composes() {
    assert!(!super::option_composes(true));
    assert_eq!(super::option_composes(false), cfg!(target_os = "macos"));
}
