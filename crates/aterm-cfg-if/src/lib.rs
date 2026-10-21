// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `cfg-if` — aterm's first-party `cfg_if!` macro.
//!
//! Published into the build under the package name `cfg-if` (`crates/aterm-cfg-if`)
//! so `[patch.crates-io]` redirects third-party consumers to first-party code.
//! One `macro_rules!`, `no_std`, no dependencies.
//!
//! # Invariants
//!
//! `cfg_if!` takes a chain of `if #[cfg(..)] { … } else if #[cfg(..)] { … } else { … }`
//! arms and emits the tokens of the first arm whose predicate holds, in the
//! caller's token context, with nothing wrapped around them:
//!
//! 1. **First-match-wins negates previous arms.** Arm `k` is emitted under
//!    `#[cfg(all(meta_k, not(any(meta_1, …, meta_k-1))))]`. Negating following
//!    arms instead would implement last-match-wins and select the wrong
//!    platform code when multiple arms hold (e.g. `target_os = "linux"` before
//!    `unix` in `parking_lot_core`).
//! 2. **Each arm's tokens route through one `@__group` call.** A `#[cfg]`
//!    written directly in front of `$( $tokens )*` attaches to the first item
//!    only, leaving subsequent items in a multi-item arm unconditional.
//!    Routing the arm through a macro call makes it a single syntax node for
//!    the attribute.
//! 3. **The terminal arm expands to nothing** (not `()`, `;`, or `{}`), so
//!    `cfg_if!` works both as a tail expression and as a statement followed by
//!    more code.
//! 4. **Caller hygiene.** Tokens are re-emitted with the caller's syntax
//!    context, so bindings introduced inside an arm remain visible after the
//!    invocation.
//! 5. **Non-selected arms are never expanded.** `#[cfg]` prevents expansion,
//!    so `compile_error!` guards in unselected fallback arms do not fire.
//!
//! # Differences from upstream `cfg-if`
//!
//! * **`rustc-dep-of-std` is a no-op** rather than pulling
//!   `rustc-std-workspace-core`.
//! * **Internal rule names** are `@__arms` and `@__group` instead of
//!   `@__items` and `@__temp_group`.
//! * **License is Apache-2.0**, with no retained `LICENSE-MIT` (which
//!   `aterm_census::scan_set::redistribution_evidence` treats as redistribution
//!   evidence).
//! * **Edition 2024.** Every fragment specifier is `tt`, which is
//!   edition-independent and accepts opaque `$e:expr` fragments substituted by
//!   outer macros.

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
