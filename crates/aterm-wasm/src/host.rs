// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! The JS functions this module's Rust side calls: the console the panic
//! hook reports to (`aterm_wasm_abi::panic_hook`) and the two browser clocks
//! under `aterm_time` (`aterm_time::wasm_clock_host!`), all through
//! aterm-wasm-abi's raw `aterm_host` imports (`aterm_wasm_abi::host`), which
//! the glue that `verify glue` writes provides.
//!
//! `aterm-gpu-web` makes the same calls the same way in its own
//! `src/host.rs` (docs/AUDIT-third-party-2026-10-15.md, wasm-platform WP.8).
//!
//! Debug builds also export what `crates/aterm-verify/perf/wasm/smoke.mjs` drives in
//! a browser (`verify wasm-smoke`); a release module has none of it.

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

/// Debug builds only: panic with `msg`, so the smoke can read what the
/// hook a constructor installed hands `console.error`.
#[cfg(all(target_arch = "wasm32", debug_assertions))]
#[aterm_wasm_abi::export(js_name = atermSmokePanic)]
pub fn smoke_panic(msg: &str) {
    panic!("{msg}");
}

/// Debug builds only: milliseconds since the first call by
/// `aterm_time::Instant`, then milliseconds since the Unix epoch by
/// `aterm_time::SystemTime`, so the smoke can hold both clocks to JS's.
#[cfg(all(target_arch = "wasm32", debug_assertions))]
#[aterm_wasm_abi::export(js_name = atermSmokeClocks)]
pub fn smoke_clocks() -> Vec<f64> {
    static FIRST: std::sync::OnceLock<aterm_time::Instant> = std::sync::OnceLock::new();
    let first = *FIRST.get_or_init(aterm_time::Instant::now);
    let wall = aterm_time::since_epoch().unwrap_or_default();
    vec![
        first.elapsed().as_secs_f64() * 1e3,
        wall.as_secs_f64() * 1e3,
    ]
}
