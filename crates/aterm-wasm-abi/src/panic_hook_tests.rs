// Copyright (c) 2018 Nick Fitzgerald
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from console_error_panic_hook 0.1.7 src/lib.rs (`hook_impl` and
// `set_once`) and modified by the aterm project in 2026: the two JS calls are
// the caller's, passed in as a [`Console`], so this file needs no wasm-bindgen.
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

use super::message;

#[test]
fn the_message_is_upstreams_format() {
    for (info, stack) in [
        (
            "panicked at src/lib.rs:5:9:\nboom",
            "Error\n    at f (wasm://wasm/1:1)",
        ),
        ("panicked at a.rs:1:1:\nline one\nline two", ""),
        ("", "Error"),
    ] {
        assert_eq!(
            message(&info, stack),
            format!("{}\n\nStack:\n\n{}\n\n", info, stack)
        );
    }
}
