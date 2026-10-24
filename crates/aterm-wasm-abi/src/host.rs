// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The JS calls a module makes, as raw `aterm_host` imports the generated glue provides: the two
//! the panic hook needs, browser clocks and entropy, and `object_drop`.

use crate::panic_hook::Console;

#[link(wasm_import_module = "aterm_host")]
unsafe extern "C" {
    #[link_name = "console_error"]
    fn host_console_error(ptr: *const u8, len: usize);
    #[link_name = "error_stack"]
    fn host_error_stack(out: *mut [usize; 2]);
    #[link_name = "performance_now"]
    safe fn host_performance_now() -> f64;
    #[link_name = "date_now"]
    safe fn host_date_now() -> f64;
    #[link_name = "random_fill"]
    fn host_random_fill(ptr: *mut u8, len: usize) -> u32;
    #[link_name = "object_drop"]
    safe fn host_object_drop(index: u32);
}

/// `console.error(text)`.
pub fn console_error(text: &str) {
    // SAFETY: the glue reads `len` bytes at `ptr` before returning.
    unsafe { host_console_error(text.as_ptr(), text.len()) }
}

/// `new Error().stack`: the JS stack here, wasm frames included.
#[must_use]
pub fn error_stack() -> String {
    let mut out = [0usize; 2];
    // SAFETY: the glue writes a pointer and length to the two words at `out`: a UTF-8 string it
    // allocated with `aterm_alloc`/`aterm_realloc` at exactly that length, now this module's.
    unsafe {
        host_error_stack(&mut out);
        <String as crate::marshal::FromAbi>::from_abi(out[0], out[1])
    }
    .unwrap_or_default()
}

/// `performance.now()`, resolved at call time, so one module works in a window, a worker and a
/// worklet.
#[must_use]
pub fn performance_now() -> f64 {
    host_performance_now()
}

/// `Date.now()`.
#[must_use]
pub fn date_now() -> f64 {
    host_date_now()
}

/// Fill bytes using the browser CSPRNG. Missing Web Crypto, invalid bounds or a
/// host exception returns false; callers must discard the entire buffer on failure.
#[must_use]
pub fn random_fill(bytes: &mut [u8]) -> bool {
    // SAFETY: `bytes` is writable for its full length until the synchronous host returns;
    // the glue bounds-checks the view and writes only that range of this module's memory.
    unsafe { host_random_fill(bytes.as_mut_ptr(), bytes.len()) == 1 }
}

/// Release slot `index` of the glue's object table (a dropped [`crate::js::JsObject`]).
pub fn object_drop(index: u32) {
    host_object_drop(index);
}

/// The panic hook's console through these imports (`panic_hook::set_once(host::CONSOLE)`).
pub const CONSOLE: Console = Console {
    error: console_error,
    stack: error_stack,
};
