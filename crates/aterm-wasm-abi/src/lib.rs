// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Host boundary and export runtime for aterm's WebAssembly modules.

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
