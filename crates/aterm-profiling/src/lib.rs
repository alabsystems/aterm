// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `profiling` — aterm's first-party replacement for the upstream `profiling`
//! crate, patched in (`[patch.crates-io]`) under wgpu, wgpu-core and wgpu-hal.
//! Every macro expands to nothing.
//!
//! # Why a no-op is exact
//!
//! No `profiling` feature is enabled anywhere in the graph (all three
//! consumers take it `default-features = false`; re-check per cell with
//! `cargo tree --target <cell> -e features -i profiling`), so upstream compiles
//! `src/empty_impl.rs`, whose transcribers are empty. The macros below match
//! that module in matcher and transcriber: the compiled program is unchanged.
//!
//! The consumers use only `profiling::scope!` (198 sites, 1 or 2 arguments,
//! all in statement position). The other macros exist for parity with
//! upstream's empty backend.
//!
//! # Arguments are never evaluated
//!
//! This is load-bearing, not an optimisation:
//!
//! * **Borrowck.** `wgpu-core/src/command/render.rs` and `compute.rs` pass
//!   `pass.base.label.as_deref().unwrap_or("")`, then mutably borrow
//!   `pass.base` lines later. A guard that stored `$data` until end of scope
//!   fails with E0502 there.
//! * **Allocation.** `wgpu-core/src/instance.rs` passes
//!   `&*alloc::format!("{_backend:?}")`; touching `$data` would allocate per
//!   backend on every `enumerate_adapters`.
//!
//! `$name` and `$data` are independent fields, never a format string and its
//! argument (`format!("unmap", "Buffer")` would not compile).
//!
//! Empty rather than `{}` (as `aterm-tracing` uses): every site is a
//! statement, and `{}` could draw `unused_braces` in a consumer.
//!
//! # Not equivalent
//!
//! * **No `procmacros` attributes** (`function`, `all_functions`, `skip`), and
//!   `default` is empty rather than `["procmacros"]`. Nothing in the graph uses
//!   them; the day something does, it fails to resolve at compile time. Fix with
//!   a pass-through proc-macro crate
//!   or by dropping the patch row.
//! * **`profile-with-*` and `type-check` are accepted and ignored.** To get real
//!   profiler output, drop the `profiling` row from the root
//!   `[patch.crates-io]`; do not grow a backend here.
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
// `no_std` as upstream: all three consumers are `#![no_std]`, and a `std`
// requirement would go unnoticed on aterm's cells (std exists on all of them).

// Macros live at the crate root: consumers call the absolute path
// `profiling::scope!`, which `#[macro_export]` provides, as upstream's
// `pub use empty_impl::*` does.

/// Opens a scope. Two variants:
///  - `profiling::scope!(name: &str)` — opens a scope with the given name
///  - `profiling::scope!(name: &str, data: &str)` — opens a scope with the
///    given name and an extra data field
///
/// `$name:expr`, not a `$($tt:tt)*` catch-all, so this accepts exactly what
/// upstream accepts: a three-argument call fails here as it does there.
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
///  - `profiling::function_scope!()` — a scope named for the current function
///  - `profiling::function_scope!(data: &str)` — the same, with a data field
///
/// Unused in aterm's graph; present for parity.
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

/// Registers a thread with the profiler API(s) — usually setting its name. Two
/// variants:
///  - `profiling::register_thread!()` — uses the thread's name, or its id
///  - `profiling::register_thread!(name: &str)` — uses the given name
///
/// Unused in aterm's graph; present for parity.
#[macro_export]
macro_rules! register_thread {
    () => {};
    ($name:expr) => {};
}

/// Finishes the frame — the frame boundary a sampling profiler groups scopes
/// by.
///
/// Unused in aterm's graph; present for parity.
#[macro_export]
macro_rules! finish_frame {
    () => {};
}
