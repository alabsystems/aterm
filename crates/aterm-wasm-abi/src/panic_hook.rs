// Copyright (c) 2018 Nick Fitzgerald
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from console_error_panic_hook 0.1.7 src/lib.rs (`hook_impl` and
// `set_once`) and modified by the aterm project in 2026: the two JS calls are
// the caller's, passed in as a [`Console`], so this file needs no wasm-bindgen.
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

//! A panic hook that reports through the host's `console.error`.

use std::fmt::Display;
use std::sync::Once;

/// The host's half of the hook: the two JS calls it makes.
#[derive(Clone, Copy, Debug)]
pub struct Console {
    /// `console.error(msg)`.
    pub error: fn(&str),
    /// `new Error().stack` callback returning the JS stack with wasm frames.
    pub stack: fn() -> String,
}

/// Formats `info` and `stack` as `"{info}\n\nStack:\n\n{stack}\n\n"`.
#[must_use]
pub fn message(info: &dyn Display, stack: &str) -> String {
    let mut msg = info.to_string();
    msg.push_str("\n\nStack:\n\n");
    msg.push_str(stack);
    msg.push_str("\n\n");
    msg
}

/// Installs the `console.error` panic hook on the first call.
pub fn set_once(console: Console) {
    static SET_HOOK: Once = Once::new();
    SET_HOOK.call_once(|| {
        std::panic::set_hook(Box::new(move |info| {
            (console.error)(&message(info, &(console.stack)()));
        }));
    });
}

#[cfg(test)]
mod tests {
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
}
