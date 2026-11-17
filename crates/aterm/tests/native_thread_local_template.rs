// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bound the actual composed application, whose CLI dependencies also contribute TLS.

#![cfg(target_os = "macos")]

#[path = "../../aterm-gui/tests/support/thread_local_template.rs"]
mod template;

#[test]
fn the_composed_app_thread_local_template_stays_small() {
    template::assert_binary_template_stays_small("aterm");
}
