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
//! * **wasm32**, 10 — the seven plus `console_error_panic_hook 0.1.7`,
//!   `wasm-bindgen 0.2.108` and `wasm-bindgen-futures 0.4.58`.
//!
//! A patch also redirects DEV and BUILD edges, so `cargo test` additionally
//! compiles `cfg_if!` call sites in `crc32fast 1.5.0`, `filetime 0.2.29`,
//! `getrandom 0.4.2`, `nix 0.29.0`, `polling 3.11.0` and `sha2 0.10.9` —
//! twenty-three packages and 258 invocation sites in total. `sha2` is worth
//! naming twice: it is `aterm-digest`'s differential oracle, so a `cfg_if!`
//! that mis-expands there breaks a *proof* rather than a build.
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
//! That runs three independent proofs:
//!
//! * **`tests/consumer_forms.rs`** — every distinct invocation SHAPE the
//!   twenty-three consumers write, copied verbatim with a comment naming the
//!   crate, file and line it came from. Most of those crates cannot be built on
//!   this macOS box, so compiling this file is what stands in for compiling
//!   them.
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
//!   The reference is `cfg!()` and NOT a dev-dependency on the upstream crate
//!   — which is what this wave's other shims do — for a measured reason:
//!   `[patch.crates-io]` rewrites dev edges too, so a `cfg-if` dev-dependency
//!   resolves back to THIS crate and the differential silently compares the
//!   shim to itself. The manifest records the `cargo metadata` output that
//!   showed it.
//! * **The doctests below**, including a `compile_fail` control that proves
//!   `compile_error!` really does fire when it lands in a SELECTED arm — so
//!   "the `compile_error!`s in `tests/consumer_forms.rs` never fired" is
//!   evidence rather than a tautology.
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
//!
//! # If a consumer ever bumps past what this accepts
//!
//! Paste the new invocation into `tests/consumer_forms.rs` first and watch it
//! fail to compile. That file is the specification; this one is the
//! implementation.

#![no_std]
#![deny(missing_docs)]

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
/// The `else` is optional, and so is the whole chain: when no predicate holds
/// the macro expands to nothing at all, which is what lets it sit in statement
/// position without changing the value of the block it is in.
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
/// Arms that are not selected are never expanded — `#[cfg]` prevents expansion
/// rather than discarding output afterwards — which is what makes a
/// `compile_error!` usable as an unsupported-platform guard:
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
/// THE CONTROL FOR THAT CLAIM. The same `compile_error!` in the arm that IS
/// selected does fail the build, so the test above is evidence and not a
/// tautology:
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
    // THE ONLY PUBLIC RULE. `tt` on both the predicate and the body, not
    // `meta`/`item`/`stmt`: the predicate has to survive being re-emitted
    // verbatim (some consumers write build-script cfgs the macro must treat as
    // opaque), and the body has to accept an opaque `expr` fragment
    // substituted in by a caller's own `macro_rules!` — see `half`.
    (
        if #[cfg( $($i_meta:tt)+ )] { $( $i_tokens:tt )* }
        $(
            else if #[cfg( $($ei_meta:tt)+ )] { $( $ei_tokens:tt )* }
        )*
        $(
            else { $( $e_tokens:tt )* }
        )?
    ) => {
        // Normalize the chain into a flat list of `((predicate) (body))` pairs,
        // the `else` arm carrying an EMPTY predicate, and hand it to the
        // recursion with an empty list of already-seen predicates.
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

    // Recursion base: every arm consumed. Expands to LITERALLY NOTHING, which
    // is load-bearing — see the "terminal arm" paragraph in the crate docs.
    (@__arms ( $( ($($_seen:tt)*) , )* ) ; ) => {};

    // Recursion step: emit one arm, then recurse with that arm's predicate
    // appended to the list every later arm must negate.
    (
        @__arms ( $( ($($no:tt)+) , )* ) ;
        (( $( $($yes:tt)+ )? ) ( $( $tokens:tt )* )),
        $( $rest:tt , )*
    ) => {
        // FIRST-MATCH-WINS lives on this one line: require this arm's own
        // predicate AND the negation of every predicate before it. Negating the
        // FOLLOWING arms instead would compile just as green and select the
        // LAST match — the failure this crate's differential test exists to
        // catch. The `else` arm contributes no `$yes`, so it reduces to
        // `not(any(everything before it))`.
        #[cfg(all(
            $( $($yes)+ , )?
            not(any( $( $($no)+ ),* ))
        ))]
        // ONE macro call, not `$( $tokens )*` directly: an attribute in front
        // of a token sequence binds to the first item only, so a multi-item arm
        // would leak its second item and onward into an unconditional
        // compilation. Wrapping the arm in a call gives the attribute a single
        // node to include or exclude; if it is included, the call expands right
        // back to the arm's tokens, unchanged and in the caller's context.
        $crate::cfg_if! { @__group $( $tokens )* }

        $crate::cfg_if! {
            @__arms ( $( ($($no)+) , )* $( ($($yes)+) , )? ) ;
            $( $rest , )*
        }
    };

    // The grouping indirection from the step above. Costs one recursion level
    // per arm, which matters: `getrandom`'s backend chain is 27 arms deep and
    // sets no `#![recursion_limit]`, so an implementation spending four or five
    // levels per arm would work everywhere except there.
    (@__group $( $tokens:tt )* ) => {
        $( $tokens )*
    };
}
