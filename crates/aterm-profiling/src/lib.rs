// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `profiling` — aterm's first-party replacement for the upstream `profiling`
//! crate. Every macro in it expands to nothing.
//!
//! This crate is published into the build under the package name `profiling`
//! (the directory is `crates/aterm-profiling`; see the manifest for why the two
//! names differ) so that `[patch.crates-io]` can redirect the three third-party
//! consumers in aterm's graph — `wgpu 29.0.3`, `wgpu-core 29.0.3` and
//! `wgpu-hal 29.0.3` — at code we own. It replaces 558 lines across 8 source
//! files with four macro definitions that discard their arguments.
//!
//! # Why a no-op is *correct*, and why that is checked rather than argued
//!
//! The claim is not "a no-op is good enough". It is that this crate emits
//! **the same tokens** as the `profiling` that ships today, at all 198 call
//! sites, on every cell. Three facts, each re-checkable:
//!
//! 1. **No feature of `profiling` is enabled anywhere in aterm's graph.** All
//!    three consumers declare it `default-features = false` (wgpu
//!    `Cargo.toml:178-180`, wgpu-core `:162-164`, wgpu-hal `:223-226`), and no
//!    package forwards a `profiling/*` feature. Re-check on each cell:
//!
//!    ```text
//!    cargo tree --workspace --target <cell> -e features -i profiling | grep -c 'profiling feature'   # => 0
//!    ```
//!
//!    Measured 0 on aarch64-apple-darwin, x86_64-pc-windows-msvc,
//!    x86_64-unknown-linux-gnu and wasm32-unknown-unknown. The corollary is in
//!    the lockfile: `grep -cE '^name = "profiling-procmacros"$' Cargo.lock`
//!    is 0, so upstream's `default = ["procmacros"]` never activates.
//! 2. **With no feature on, upstream compiles `src/empty_impl.rs`.** Its
//!    `lib.rs` selects that module under `#[cfg(not(any(profile-with-puffin,
//!    profile-with-optick, profile-with-superluminal, profile-with-tracing,
//!    profile-with-tracy, type-check)))]` — the exact condition fact (1)
//!    establishes. Every macro in that module has an **empty transcriber**:
//!    `($name:expr) => {};` expands to no tokens at all.
//! 3. **Therefore the crate aterm ships today is already a pure no-op**, and
//!    the macros below are byte-identical in matcher and transcriber to the
//!    ones it would have used. Nothing about the compiled program changes; only
//!    the package leaves the graph.
//!
//! That is a stronger footing than the sibling `crates/aterm-tracing` shim
//! stands on. There the argument runs through a runtime premise (no subscriber
//! is ever installed, so every callsite is disabled). Here there is no premise:
//! the upstream code that would have been compiled is right there in the
//! registry, and it is empty.
//!
//! # The surface, and the fact that it is exactly one macro
//!
//! A name histogram over every `profiling::` occurrence in the three consumer
//! trees finds `profiling::scope!` and **nothing else** — 198 invocations
//! (wgpu 1, wgpu-core 137, wgpu-hal 60), zero `function_scope!`, zero
//! `register_thread!`, zero `finish_frame!`, zero `#[profiling::function]`,
//! zero `use profiling::…`, and nothing outside `src/` (no build script,
//! bench, example or test). Re-derive it with
//!
//! ```text
//! grep -rho 'profiling::[a-zA-Z_]*' <registry>/{wgpu,wgpu-core,wgpu-hal}-29.0.3 --include='*.rs' | sort | uniq -c
//! ```
//!
//! A paren-balancing scan of those 198 sites (not an eyeball) closes the arity
//! set at `{1: 194, 2: 4}` with no trailing commas and no three-argument form,
//! in four structural shapes: 1-arg single-line 193, 1-arg multi-line 1, 2-arg
//! single-line 2, 2-arg multi-line 2. `tests/consumer_forms.rs` carries all
//! four verbatim.
//!
//! [`function_scope!`], [`register_thread!`] and [`finish_frame!`] are here for
//! parity only — upstream's empty backend exports them, and a consumer bump
//! that starts using one should find it present rather than find a build break.
//!
//! # Arguments are never evaluated, and that is load-bearing twice
//!
//! Discarding the token trees is not an optimisation, it is the behaviour being
//! reproduced — and two real call sites turn any deviation into a bug:
//!
//! * **Borrowck.** `wgpu-core/src/command/render.rs:1820` and
//!   `src/command/compute.rs:473` hand the macro
//!   `pass.base.label.as_deref().unwrap_or("")` — a shared borrow of
//!   `pass.base` — and then take that same field mutably eleven lines later
//!   (`let base = pass.base.take();`, render.rs:1830 / compute.rs:483). How
//!   long the expansion keeps that borrow alive decides whether wgpu-core
//!   still compiles, and the answer is less obvious than it looks, so it was
//!   MEASURED: four transcribers, each compiled against
//!   `tests/consumer_forms.rs`, which reproduces both functions statement for
//!   statement.
//!
//!   | expansion of the 2-argument arm | result |
//!   |---|---|
//!   | nothing (this crate, and upstream's empty backend) | compiles |
//!   | `let _: &str = $data;` (upstream's `type-check` backend) | compiles |
//!   | `let _data = $data;` (a plain named binding) | compiles |
//!   | a guard that STORES `$data` and has a `Drop` impl | **E0502** at both sites |
//!
//!   The middle two compile because NLL ends a borrow at its last USE, and a
//!   binding nothing reads again has none: "a named binding holds it to the end
//!   of the block" is a pre-NLL reflex, and it is wrong here. What does break —
//!   `error[E0502]: cannot borrow pass.base as mutable because it is also
//!   borrowed as immutable` — is a guard that keeps the `&str` and drops at
//!   the end of the enclosing block. Upstream's tracy backend sidesteps it by
//!   copying the text out on the spot (`_tracy_span.emit_text($data);`,
//!   `tracy_impl.rs`) rather than storing it. Expanding to nothing inherits the
//!   property without having to be careful about it, and the counterfactual is
//!   re-runnable — `consumer_forms.rs` documents how it was armed.
//! * **Allocation.** `wgpu-core/src/instance.rs:469` passes
//!   `&*alloc::format!("{_backend:?}")`. Any shim that touches `$data`
//!   allocates a `String` per backend on every `Global::enumerate_adapters`
//!   call, on a path upstream deliberately made free.
//!
//! And the `{}` in `"CommandEncoder::run_render_pass {}"` **is not a format
//! hole**: `$name` and `$data` are two independent fields to every upstream
//! backend, never substituted into one another. A shim that "helpfully" wrote
//! `format!($name, $data)` would allocate, rename the scope, and fail to
//! compile at `device/global.rs:2111` (`format!("unmap", "Buffer")` — argument
//! never used).
//!
//! # Why this expands to nothing where `aterm-tracing` expands to `{}`
//!
//! The sibling shim's macros expand to `{}` because winit writes them in
//! EXPRESSION position (`None => info!("…")`, a match arm), where an empty
//! expansion is a parse error. `profiling` is the mirror image: a scan of all
//! 198 sites found the character after every closing paren is `;` — every one
//! is a statement, none is a block's trailing expression, and none sits inside
//! a `macro_rules!` transcriber (brace-matched in the three consumer files that
//! contain both). Both expansions would compile here, so the tie is broken by
//! fidelity: upstream's empty backend emits nothing, and `{}` is a block
//! expression that can draw `unused_braces` / `clippy::no_effect` inside a
//! consumer crate. Registry crates are not linted today, which is exactly why
//! that divergence would be silent.
//!
//! # Where this is NOT equivalent, stated plainly
//!
//! Two divergences, both outside anything aterm's graph reaches, neither of
//! them silent if it ever starts to matter:
//!
//! * **The `procmacros` attributes are absent.** Upstream re-exports
//!   `function`, `all_functions` and `skip` from `profiling-procmacros` under
//!   its default features; this crate has no proc-macro half, and its `default`
//!   feature is empty rather than `["procmacros"]` (the manifest says why).
//!   Nothing in the graph writes `#[profiling::function]` and
//!   `profiling-procmacros` is not in the lockfile, so today the difference is
//!   unobservable. The day a consumer writes one, it will fail to *resolve* —
//!   loudly, at compile time, not silently — and the fix is either a
//!   pass-through proc-macro crate (`crates/aterm-tracing-attributes` is the
//!   precedent: a `#[proc_macro_attribute]` that returns its item unchanged,
//!   no `syn`) or retiring the patch entry.
//! * **The `profile-with-*` features are accepted and ignored.** Upstream
//!   switches to a real backend and re-exports the profiler crate
//!   (`pub use tracy_client;` and friends). Here they are empty names that
//!   exist so a resolve cannot fail. If anyone ever genuinely wants Tracy or
//!   puffin output, the fix is to DROP the `profiling` row from the root
//!   `[patch.crates-io]` table so the real crate resolves again — not to grow
//!   this shim a backend. `type-check` is in the same list and is the one to
//!   watch: upstream's `type_check_impl.rs` expands to `let _: &str = $name;`,
//!   which type-checks the argument by EVALUATING it — harmless for the 194
//!   literal sites, but at `instance.rs:469` it runs the `alloc::format!` and
//!   allocates. Turning that feature on here changes nothing, which is a
//!   divergence in aterm's favour but a divergence all the same.
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
// `no_std` unconditionally, exactly as upstream (`profiling` lib.rs:8), and it
// is not decorative: all three consumers are themselves `#![no_std]` (wgpu
// lib.rs:75, wgpu-core lib.rs:9, wgpu-hal lib.rs:205; wgpu-core reaches
// `alloc::format!` at instance.rs:469). A shim that quietly required `std`
// would still link on all four of aterm's cells, because std exists on every
// one of them — so the regression would never show up here and would only bite
// a downstream no_std consumer. Cheap to hold: this crate has no items at all,
// only macros.

// The macros are defined at the crate ROOT, not in a module.
//
// All 198 call sites use the absolute path `profiling::scope!` and there is not
// one `use profiling::…` in the three consumer trees. That path resolves
// because `#[macro_export]` hoists a macro to the crate root — upstream defines
// these in `src/empty_impl.rs` and re-exports with `pub use empty_impl::*;`,
// which lands them in the same place. Defining them in a module here and
// `pub use`-ing them would change `$crate` hygiene and macro-name resolution
// for no gain.

/// Opens a scope. Two variants:
///  - `profiling::scope!(name: &str)` — opens a scope with the given name
///  - `profiling::scope!(name: &str, data: &str)` — opens a scope with the
///    given name and an extra data field
///
/// THE ONLY item any consumer uses: 198 invocations across wgpu, wgpu-core and
/// wgpu-hal, 194 of them one-argument and 4 two-argument.
///
/// The matcher is `$name:expr` rather than a `$($tt:tt)*` catch-all so this
/// crate accepts exactly what upstream accepts and no more — a three-argument
/// call is a compile error here as it is there. The transcribers are empty, so
/// neither argument is parsed into a value, evaluated, borrowed or moved.
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
/// Zero uses in aterm's graph; present because upstream's empty backend exports
/// it and parity costs four lines.
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
/// Zero uses in aterm's graph; present for parity.
#[macro_export]
macro_rules! register_thread {
    () => {};
    ($name:expr) => {};
}

/// Finishes the frame — the frame boundary a sampling profiler groups scopes
/// by.
///
/// Zero uses in aterm's graph; present for parity.
#[macro_export]
macro_rules! finish_frame {
    () => {};
}
