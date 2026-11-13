// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_window_icon_decodes() {
    assert!(window_icon().is_some());
}
