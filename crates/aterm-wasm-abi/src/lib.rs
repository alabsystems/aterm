// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The host boundary of aterm's two wasm modules (`aterm-wasm`, the CPU
//! canvas path, and `aterm-gpu-web`, the WebGL path), with no third-party
//! dependency: the layer that replaces wasm-bindgen
//! (docs/AUDIT-third-party-2026-10-15.md, wasm-platform WP.3 and WP.6).
//! `aterm-wasm` (WP.7) and `aterm-gpu-web` (WP.8) export through it and
//! import through [`host`]; aterm-gpu's WebGL2 arm imports its GL calls
//! beside it (`aterm_gl`) and takes its canvas as a [`js`] object.
//!
//! * [`export`] (from `aterm-wasm-abi-macros`) marks what JS sees: a
//!   `struct` becomes a class, the `pub fn`s of its `impl` its methods
//!   (`#[export(getter)]`, `#[export(constructor)]`), a module-level
//!   `pub fn` a function. It keeps the source shape wasm-bindgen had, down to
//!   `#[cfg_attr(target_arch = "wasm32", export(getter))]` on a method.
//! * [`export_runtime!`], once per module, exports the allocator and return
//!   slot the glue calls ([`mem`]). An export always returns: a throw (an
//!   `Err`, a misused handle) is left in the return slot for the glue to
//!   throw, never thrown through an import.
//! * [`marshal`] is how each type crosses; [`handle`] is how a class's value
//!   lives behind a JS object (null, aliasing and post-trap checks); [`js`]
//!   is how a JS object the host hands in (a canvas) lives in the module: as
//!   its slot in the glue's object table.
//! * [`descriptor`] is what each export tells the glue generator,
//!   `verify glue`, in the module's `aterm_abi` custom section.
//! * [`host`] is the JS the module calls (`console.error`,
//!   `new Error().stack`, the clocks), as raw `aterm_host` imports, and
//!   [`panic_hook`] reports a panic through it.
//!
//! Everything but the imports and the `#[no_mangle]` exports is plain Rust
//! over words and memory, so it builds and is tested natively; the wasm32
//! arms are thin.

#![deny(unsafe_code)]
#![warn(missing_docs)]

#[cfg(feature = "export")]
pub use aterm_wasm_abi_macros::export;

#[cfg(feature = "export")]
#[doc(hidden)]
pub use aterm_wasm_abi_macros::__export_method;

pub mod descriptor;
#[allow(
    unsafe_code,
    reason = "a handle is a raw pointer JS holds; the borrow flag and class tag are the checks"
)]
pub mod handle;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
#[allow(
    unsafe_code,
    reason = "the aterm_host imports and the buffers they hand over"
)]
pub mod host;
#[allow(
    unsafe_code,
    reason = "a JS object's slot crosses as a word the glue vouches for"
)]
pub mod js;
#[allow(
    unsafe_code,
    reason = "buffers cross the boundary as pointers and lengths the glue allocates"
)]
pub mod marshal;
#[allow(unsafe_code, reason = "the allocator the glue writes arguments into")]
pub mod mem;
pub mod panic_hook;

#[doc(hidden)]
#[path = "rt.rs"]
#[allow(
    unsafe_code,
    reason = "realloc and a class's free pass their callers' contracts on"
)]
pub mod __rt;
