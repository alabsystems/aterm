// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `cfg-if` — aterm's first-party `cfg_if!` macro.
//!
//! This crate is published into the build under the package name `cfg-if` (the
//! directory is `crates/aterm-cfg-if`; see the manifest for why the two names
//! differ) so that `[patch.crates-io]` can redirect every consumer of the
//! crates.io package at code we own. It is one `macro_rules!` and nothing else:
//! no types, no functions, no modules, no consts, no dependencies, `no_std`.
//!
//! # Who asks for it, and why a call-site census misses this crate
//!
//! **No aterm crate uses `cfg_if!` at all.** `grep -rn cfg_if crates/ tools/
//! apps/ --include='*.rs'` returns zero. Every consumer is third-party, which
//! is precisely why the patch table is the only lever — rewriting our own call
//! sites would have rewritten nothing. Measured with
//! `cargo tree --workspace --target <cell> -e normal,build -i cfg-if --depth 1`
//! on all four cells:
//!
//! * **mac-arm and win-msvc**, 7 each — `getrandom 0.2.17`, `half 2.7.1`,
//!   `naga 29.0.3`, `parking_lot_core 0.9.12`, `ring 0.17.14`, `wgpu 29.0.3`,
//!   `wgpu-hal 29.0.3`.
//! * **linux-gnu**, 15 — those seven plus `ahash 0.8.12` (through our winit
//!   fork), `async-io 2.6.0`, `async-process 2.5.0`, `async-signal 0.2.14`,
//!   `getrandom 0.3.4`, `libloading 0.8.9`, `polling 3.11.0` and
//!   `tiny-skia 0.11.4` (through `sctk-adwaita`).
//! * **wasm32**, 8 — `half 2.7.1`, `naga 29.0.3`, `parking_lot_core 0.9.12`,
//!   `ring 0.17.14`, `wasm-bindgen 0.2.108`, `wasm-bindgen-futures 0.4.58`,
//!   `wgpu 29.0.3` and `wgpu-hal 29.0.3` (re-measured 2026-10-15, when
//!   `console_error_panic_hook` left for `aterm-wasm-abi`, wasm-platform WP.3).
//!   Per shipped module (`-p aterm-gpu-web` / `-p aterm-wasm` in place of
//!   `--workspace`), the GPU module reaches all of them but `ring`, and the
//!   CPU module none: its only one was `wasm-bindgen`, which it left at
//!   wasm-platform WP.7 (re-measured 2026-10-16).
//!
//! A patch also redirects DEV and BUILD edges, so `cargo test` compiles more
//! `cfg_if!` call sites than the census above (`-e normal,build,dev` lists
//! them).
//!
//! # Why this replacement is safe
//!
//! The contract is small enough to state completely. `cfg_if!` takes a chain of
//! `if #[cfg(..)] { … } else if #[cfg(..)] { … } else { … }` arms and emits the
//! tokens of the FIRST arm whose predicate holds, in the caller's own token
//! context, with nothing wrapped around them. Three structural facts carry that
//! sentence, and every one of them is a way to get this wrong silently:
//!
//! 1. **First-match-wins is implemented by negating the PREVIOUS arms.** Arm `k`
//!    is emitted under `#[cfg(all(meta_k, not(any(meta_1, …, meta_k-1))))]`. An
//!    implementation that negates the FOLLOWING arms instead is *last*-match-
//!    wins: exactly one arm still compiles, every cell still builds green, and
//!    the wrong platform code ships. That is not hypothetical —
//!    `parking_lot_core`'s `thread_parker/mod.rs:53` opens
//!    `if #[cfg(any(target_os = "linux", target_os = "android"))] … else if
//!    #[cfg(unix)] …`, and on Linux BOTH hold: first-match gives the futex
//!    parker, last-match silently gives the pthread-condvar parker for every
//!    lock in the process. `wgpu-hal`'s `lib.rs:312` and `half`'s
//!    `binary16/arch.rs:19` have the same overlapping shape.
//! 2. **Each arm's tokens are re-emitted through one `@__group` call.** A
//!    `#[cfg]` written directly in front of `$( $tokens )*` attaches to the
//!    FIRST item only; every later item in the arm then compiles
//!    unconditionally. This is upstream's issue #90, and nearly every arm in
//!    aterm's graph is multi-item (`mod pclmulqdq; pub use self::pclmulqdq::
//!    State;` in `crc32fast`, twenty-eight `mod X; pub use X::*;` pairs in
//!    `getrandom`'s backend chain). Routing the arm through a macro call makes
//!    it a single node for the attribute to include or exclude, and the call
//!    expands to the tokens unchanged.
//! 3. **The terminal arm expands to literally nothing.** Not `()`, not `;`, not
//!    an empty block. `cfg_if!` is routinely the whole body of a function whose
//!    value it must produce (`ring`'s `sha2_32.rs:29` on all four cells,
//!    `wgpu`'s `util/mutex.rs:42`, ~90 sites in `tiny-skia`), and it is equally
//!    routinely a statement followed by more code (`nix`'s `addr.rs:584`,
//!    followed by `Ok(())`). Both work only because the selected arm's tokens
//!    land directly in the caller's block with nothing after them.
//!
//! Two more properties fall out of being a `macro_rules!` rather than anything
//! cleverer, and both are relied on:
//!
//! * **Hygiene is the caller's.** The tokens are re-emitted with the caller's
//!   syntax context, so a `let` bound inside an arm is visible *after* the
//!   invocation — `naga`'s `error.rs:114` binds `inner` in every arm and then
//!   writes `Self { inner }` outside the macro. A proc macro that re-spanned
//!   the tokens would break that, and would break shadowing cases silently
//!   first.
//! * **A non-selected arm is never expanded.** `#[cfg]` prevents expansion; it
//!   does not discard output afterwards. Twenty sites in the graph park a
//!   `compile_error!` in a fallback arm as an unsupported-platform guard, and
//!   an implementation that expanded every arm and filtered later would turn
//!   each of those into an unconditional build failure.
//!
//! # What was verified, and the command to re-check it
//!
//! ```text
//! cargo test -p cfg-if
//! ```
//!
//! That runs two independent proofs:
//!
//! * **`tests/differential.rs`** — every chain answered twice: once by this
//!   macro, once by a hand-written `cfg!()` cascade over the identical
//!   predicates, asserting the two agree. `cfg!()` is rustc evaluating the same
//!   predicate this macro hands to `#[cfg]`, so it is the ground truth rather
//!   than a second opinion. Several chains are built from `#[cfg(all())]`
//!   (always true) and `#[cfg(any())]` (always false) so that MULTIPLE
//!   predicates hold at once on every cell — the only shape that can catch a
//!   wrong-arm implementation, since a chain with one true predicate selects
//!   the same arm under every plausible bug. The file also arms its tripwires
//!   before it asserts anything: it defines a deliberately last-match-wins
//!   macro and a deliberately un-grouped one, shows each produces a *different*
//!   answer on the same input, and only then asserts this macro's answer.
//!
//!   The reference is `cfg!()` and NOT a dev-dependency on the upstream crate,
//!   for a measured reason:
//!   `[patch.crates-io]` rewrites dev edges too, so a `cfg-if` dev-dependency
//!   resolves back to THIS crate and the differential silently compares the
//!   shim to itself. The manifest records the `cargo metadata` output that
//!   showed it.
//! * **The doctests below**, including a `compile_fail` control that proves
//!   `compile_error!` really does fire when it lands in a SELECTED arm.
//!
//! # Where this is NOT the crates.io crate
//!
//! Four differences, none reachable from aterm, all stated because an honest
//! divergence is worth more than a claim that does not hold on every cell:
//!
//! * **`rustc-dep-of-std` is a no-op here.** Upstream defines it as
//!   `["core"]`, pulling `rustc-std-workspace-core`; this crate declares the
//!   feature and wires it to nothing, because adding that dependency would put
//!   a package back into the graph. So this shim cannot be used to build `std`
//!   itself. Nothing in aterm's four cells enables the feature (features
//!   resolve to `default` only), and if that ever changes the build breaks
//!   loudly rather than quietly.
//! * **The internal arm names differ** — `@__arms` and `@__group` here,
//!   `@__items` and `@__temp_group` upstream. Verified free: zero matches for
//!   any of those tokens across all twenty-three consuming packages, so nothing
//!   invokes them directly. Anything that started to would get a
//!   no-rules-expected-this-token error, which is loud.
//! * **The licence is Apache-2.0**, not upstream's `MIT OR Apache-2.0`, because
//!   this is aterm's own code rather than a redistribution. It carries no
//!   retained `LICENSE-MIT`/`LICENSE-APACHE`, no `NOTICE` line and no review
//!   row — deliberately, and the classifier depends on it:
//!   `aterm_census::scan_set::redistribution_evidence` treats any `LICENSE*`
//!   file in a patch target's directory as proof of redistribution, and a
//!   `crates/` path showing that is a hard error rather than a reclassification.
//! * **Edition 2024, not upstream's 2018.** Safe here for a specific reason
//!   rather than by luck: the only fragment specifiers in this macro are `tt`,
//!   and `tt` matching is edition-independent. The 2021→2024 change that bites
//!   macros is the `expr` fragment, which this macro does not use — and must
//!   not, because `half`'s `binary16/arch.rs:19` invokes `cfg_if!` from inside
//!   another `macro_rules!` with `$f16c:expr` substituted into an arm body, and
//!   an opaque expression fragment matches `tt` but matches neither `item` nor
//!   `stmt` nor `block`.

#![no_std]
#![warn(missing_docs)]

/// Emit the items of the first arm whose `#[cfg]` predicate holds.
///
/// ```
/// cfg_if::cfg_if! {
///     if #[cfg(unix)] {
///         fn platform() -> &'static str { "unix" }
///     } else if #[cfg(windows)] {
///         fn platform() -> &'static str { "windows" }
///     } else {
///         fn platform() -> &'static str { "other" }
///     }
/// }
/// # fn main() {}
/// ```
///
/// The `else` is optional; when no predicate holds the macro expands to
/// nothing, so it can sit in statement position.
///
/// ```
/// fn f() -> Result<(), ()> {
///     cfg_if::cfg_if! {
///         if #[cfg(any())] {
///             return Err(());
///         }
///     };
///     Ok(())
/// }
/// assert!(f().is_ok());
/// ```
///
/// Arms that are not selected are never expanded, so `compile_error!` works as
/// an unsupported-platform guard:
///
/// ```
/// cfg_if::cfg_if! {
///     if #[cfg(all())] {
///         fn supported() -> bool { true }
///     } else {
///         compile_error!("unsupported platform");
///     }
/// }
/// assert!(supported());
/// ```
///
/// Control: the same `compile_error!` in the selected arm fails the build.
///
/// ```compile_fail
/// cfg_if::cfg_if! {
///     if #[cfg(all())] {
///         compile_error!("this arm is selected, so this must fire");
///     }
/// }
/// ```
#[macro_export]
macro_rules! cfg_if {
    // The only public rule. `tt` for predicate and body: predicates are
    // re-emitted verbatim, and bodies may be opaque `expr` fragments
    // substituted by a caller's own `macro_rules!` (see `half`).
    (
        if #[cfg( $($i_meta:tt)+ )] { $( $i_tokens:tt )* }
        $(
            else if #[cfg( $($ei_meta:tt)+ )] { $( $ei_tokens:tt )* }
        )*
        $(
            else { $( $e_tokens:tt )* }
        )?
    ) => {
        // Normalize into `((predicate) (body))` pairs, the `else` arm with an
        // empty predicate, and recurse with no predicates seen yet.
        $crate::cfg_if! {
            @__arms () ;
            (( $($i_meta)+ ) ( $( $i_tokens )* )),
            $(
                (( $($ei_meta)+ ) ( $( $ei_tokens )* )),
            )*
            $(
                (() ( $( $e_tokens )* )),
            )?
        }
    };

    // Recursion base: expands to nothing (see the crate docs).
    (@__arms ( $( ($($_seen:tt)*) , )* ) ; ) => {};

    // Recursion step: emit one arm, then recurse with that arm's predicate
    // appended to the list every later arm must negate.
    (
        @__arms ( $( ($($no:tt)+) , )* ) ;
        (( $( $($yes:tt)+ )? ) ( $( $tokens:tt )* )),
        $( $rest:tt , )*
    ) => {
        // First match wins: this arm's predicate and the negation of every
        // earlier one. The `else` arm reduces to `not(any(earlier))`.
        #[cfg(all(
            $( $($yes)+ , )?
            not(any( $( $($no)+ ),* ))
        ))]
        // One macro call so the attribute covers the whole multi-item arm; the
        // call expands back to the arm's tokens in the caller's context.
        $crate::cfg_if! { @__group $( $tokens )* }

        $crate::cfg_if! {
            @__arms ( $( ($($no)+) , )* $( ($($yes)+) , )? ) ;
            $( $rest , )*
        }
    };

    // One recursion level per arm: `getrandom`'s 27-arm chain sets no
    // `#![recursion_limit]`.
    (@__group $( $tokens:tt )* ) => {
        $( $tokens )*
    };
}
