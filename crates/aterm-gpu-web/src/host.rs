// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! The JS functions this module's Rust side calls, bound in one place: the
//! console the panic hook reports to (`aterm_wasm_abi::panic_hook`) and the two
//! browser clocks under `aterm_time` (`aterm_time::wasm_clock_host!`).
//!
//! Bound with wasm-bindgen, which this module still uses for its exports.
//! `aterm-wasm` makes the same calls through aterm-wasm-abi's raw
//! `aterm_host` imports (its `src/host.rs`); this module follows when it
//! leaves wasm-bindgen (docs/AUDIT-third-party-2026-10-15.md, wasm-platform
//! WP.8).
//!
//! Debug builds also export what `crates/aterm-verify/perf/wasm/smoke.mjs`
//! drives under node (`verify wasm-smoke`); a release module has none of it.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::wasm_bindgen;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
unsafe extern "C" {
    /// `console.error(msg)`.
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn console_error(msg: &str);

    /// The JS `Error` class, for the stack at a panic.
    type Error;

    #[wasm_bindgen(constructor)]
    fn new() -> Error;

    #[wasm_bindgen(structural, method, getter)]
    fn stack(this: &Error) -> String;

    /// `performance.now()`, resolved at call time, so one build works in a
    /// window, a worker and a worklet.
    #[wasm_bindgen(js_namespace = performance, js_name = now)]
    fn performance_now() -> f64;

    /// `Date.now()`.
    #[wasm_bindgen(js_namespace = Date, js_name = now)]
    fn date_now() -> f64;
}

// The clocks aterm_time reads on wasm32-unknown-unknown: link-time symbols
// this module must define, or it does not link.
#[cfg(target_arch = "wasm32")]
aterm_time::wasm_clock_host!(performance_now, date_now);

/// `console.error` and `new Error().stack`, the two calls the panic hook
/// makes.
#[cfg(target_arch = "wasm32")]
pub(crate) const CONSOLE: aterm_wasm_abi::panic_hook::Console =
    aterm_wasm_abi::panic_hook::Console {
        error: console_error,
        stack: || Error::new().stack(),
    };

/// Debug builds only: panic with `msg`, so the node smoke can read what the
/// hook a constructor installed hands `console.error`.
#[cfg(all(target_arch = "wasm32", debug_assertions))]
#[wasm_bindgen(js_name = atermSmokePanic)]
pub fn smoke_panic(msg: &str) {
    panic!("{msg}");
}

/// Debug builds only: milliseconds since the first call by
/// `aterm_time::Instant`, then milliseconds since the Unix epoch by
/// `aterm_time::SystemTime`, so the node smoke can hold both clocks to JS's.
#[cfg(all(target_arch = "wasm32", debug_assertions))]
#[wasm_bindgen(js_name = atermSmokeClocks)]
pub fn smoke_clocks() -> Vec<f64> {
    static FIRST: std::sync::OnceLock<aterm_time::Instant> = std::sync::OnceLock::new();
    let first = *FIRST.get_or_init(aterm_time::Instant::now);
    let wall = aterm_time::since_epoch().unwrap_or_default();
    vec![
        first.elapsed().as_secs_f64() * 1e3,
        wall.as_secs_f64() * 1e3,
    ]
}
