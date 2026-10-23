// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `profiling` — aterm's first-party replacement for the upstream `profiling`
//! crate, patched in (`[patch.crates-io]`) under wgpu, wgpu-core and wgpu-hal.

#![no_std]

/// Opens a scope. Two variants:
///  - `profiling::scope!(name: &str)` — opens a scope with the given name
///  - `profiling::scope!(name: &str, data: &str)` — opens a scope with the
///    given name and an extra data field
#[macro_export]
macro_rules! scope {
    ($name:expr) => {};
    ($name:expr, $data:expr) => {};
}

/// Opens a scope automatically named after the current function.
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
