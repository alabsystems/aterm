// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::TerminalCapabilities;

#[test]
fn kitty_graphics_advertised_after_kitty_core() {
    let feats = TerminalCapabilities::aterm_capabilities().feature_list_string();
    assert!(
        feats.contains("kitty-graphics"),
        "KITTY-CORE landed: kitty graphics must be advertised; got {feats:?}"
    );
}

#[test]
fn soft_fonts_not_advertised() {
    let feats = TerminalCapabilities::aterm_capabilities().feature_list_string();
    assert!(!feats.contains("soft-fonts"), "soft fonts unimplemented");
}
