// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `profiling` — aterm's first-party replacement for the upstream `profiling`
//! crate, patched in (`[patch.crates-io]`) under wgpu, wgpu-core and wgpu-hal.
//! Every macro expands to nothing.
//!
//! # Examples
//!
//! ```
//! // A scope, and a scope with a data field. Neither argument is evaluated.
//! fn expensive() -> &'static str { unreachable!("scope! evaluates nothing") }
//! profiling::scope!("aterm::Renderer::frame");
//! profiling::scope!("aterm::Renderer::frame", expensive());
//!
//! // The parity macros no consumer calls yet.
//! profiling::register_thread!();
//! profiling::register_thread!("aterm-gpu");
//! profiling::function_scope!();
//! profiling::finish_frame!();
//! ```

#![no_std]

/// Opens a scope. Two variants:
///  - `profiling::scope!(name: &str)` — opens a scope with the given name
///  - `profiling::scope!(name: &str, data: &str)` — opens a scope with the
///    given name and an extra data field
///
/// ```
/// profiling::scope!("outer");
/// for _i in 0..10 {
///     profiling::scope!("inner", format!("iteration {_i}").as_str());
/// }
/// ```
#[macro_export]
macro_rules! scope {
    ($name:expr) => {};
    ($name:expr, $data:expr) => {};
}

/// Opens a scope automatically named after the current function.
///
/// ```
/// fn function_a() {
///     profiling::function_scope!();
/// }
/// fn function_b(iteration: u32) {
///     profiling::function_scope!(format!("iteration {iteration}").as_str());
/// }
/// ```
#[macro_export]
macro_rules! function_scope {
    () => {};
    ($data:expr) => {};
}

/// No-op thread registration macro (`profiling::register_thread!()` or `profiling::register_thread!(name)`).
#[macro_export]
macro_rules! register_thread {
    () => {};
    ($name:expr) => {};
}

/// No-op frame boundary marker (`profiling::finish_frame!()`).
#[macro_export]
macro_rules! finish_frame {
    () => {};
}
