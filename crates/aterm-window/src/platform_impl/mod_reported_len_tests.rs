// Copyright The winit contributors
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/mod.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

/// An OS-reported output length never passes the buffer it was given.
#[test]
fn an_os_reported_length_never_passes_its_buffer() {
    assert_eq!(super::reported_len(300, 255), 255);
    assert_eq!(super::reported_len(usize::MAX, 16), 16);
    assert_eq!(super::reported_len(16, 16), 16);
    assert_eq!(super::reported_len(4, 255), 4, "control: a length inside the buffer stands");
    assert_eq!(super::reported_len(0, 255), 0);
}
