// Copyright (c) 2018 Nick Fitzgerald
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from console_error_panic_hook 0.1.7 src/lib.rs (`hook_impl` and
// `set_once`) and modified by the aterm project in 2026: the two JS calls are
// the caller's, passed in as a [`Console`], so this file needs no wasm-bindgen.
// See ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

//! A panic hook that reports through the host's `console.error`.
//!
//! On `wasm32-unknown-unknown` a panic aborts as a bare `RuntimeError:
//! unreachable`, and std's default hook writes the message to a stderr that
//! goes nowhere. [`set_once`] installs a hook that hands the message to the
//! host instead, in upstream's format ([`message`]): the panic, then the JS
//! stack at the panic, which carries the wasm frames.
//!
//! A module passes the two JS calls in a [`Console`]: `host::CONSOLE` (the
//! raw `aterm_host` imports) for one on this crate's export layer, as both
//! web modules are, or its own bindings.

use std::fmt::Display;
use std::sync::Once;

/// The host's half of the hook: the two JS calls it makes.
#[derive(Clone, Copy, Debug)]
pub struct Console {
    /// `console.error(msg)`.
    pub error: fn(&str),
    /// `new Error().stack`: the JS stack where the hook runs, which includes
    /// the wasm frames of the panic.
    pub stack: fn() -> String,
}

/// The text the hook logs for a panic `info` and a JS `stack`: upstream's
/// format, `"{info}\n\nStack:\n\n{stack}\n\n"`.
#[must_use]
pub fn message(info: &dyn Display, stack: &str) -> String {
    let mut msg = info.to_string();

    // Add the error stack to our message.
    //
    // This ensures that even if the `console` implementation doesn't include
    // stacks for `console.error`, the stack is still available for the user.
    // Additionally, Firefox's console tries to clean up stack traces, and
    // ruins Rust symbols in the process
    // (https://bugzilla.mozilla.org/show_bug.cgi?id=1519569) but since it only
    // touches the logged message's associated stack, and not the message's
    // contents, by including the stack in the message contents we make sure it
    // is available to the user.
    msg.push_str("\n\nStack:\n\n");
    msg.push_str(stack);

    // Safari's devtools, on the other hand, _do_ mess with logged messages'
    // contents, so we attempt to break their heuristics for doing that by
    // appending some whitespace.
    // https://github.com/rustwasm/console_error_panic_hook/issues/7
    msg.push_str("\n\n");
    msg
}

/// Install the hook the first time this is called, replacing std's; later
/// calls do nothing, whatever `console` they pass.
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

    /// The message is upstream's format string, byte for byte, including a
    /// multi-line panic and an empty stack.
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
