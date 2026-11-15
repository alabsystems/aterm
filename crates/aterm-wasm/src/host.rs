// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! The JS functions this module's Rust side calls: the console the panic
//! hook reports to (`aterm_wasm_abi::panic_hook`) and the two browser clocks
//! under `aterm_time` (`aterm_time::wasm_clock_host!`), all through
//! aterm-wasm-abi's raw `aterm_host` imports (`aterm_wasm_abi::host`), which
//! the glue that `verify glue` writes provides.
//!
//! `aterm-gpu-web` makes the same calls the same way in its own `src/host.rs`.

// The clocks aterm_time reads on wasm32-unknown-unknown: link-time symbols
// this module must define, or it does not link.
#[cfg(target_arch = "wasm32")]
aterm_time::wasm_clock_host!(
    aterm_wasm_abi::host::performance_now,
    aterm_wasm_abi::host::date_now
);

/// `console.error` and `new Error().stack`, the two calls the panic hook
/// makes.
#[cfg(target_arch = "wasm32")]
pub(crate) use aterm_wasm_abi::host::CONSOLE;
