// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE DIFFERENTIAL ORACLE: every chain answered twice, and the two answers
//! must agree.
//!
//! # What it is differential against, and why not the upstream crate
//!
//! The obvious reference is a `[dev-dependencies]` on the real crates.io
//! `cfg-if`. It does not work here, and the way it fails is the worst kind. `[patch.crates-io]` rewrites EVERY crates-io edge in the workspace,
//! dev edges included, so once the patch entry lands a `cfg-if` dev-dependency
//! resolves to this very crate. Measured with the patch in place:
//!
//! ```text
//! cargo metadata --format-version 1 | …
//! NODE path+…/crates/aterm-cfg-if#cfg-if@1.0.4
//!    -> cfg_if path+…/crates/aterm-cfg-if#cfg-if@1.0.4
//! ```
//!
//! The differential would compare the shim to itself, pass forever, and prove
//! nothing — with no error anywhere to notice.
//!
//! So the reference is `cfg!()`: rustc's own evaluation of the same predicate
//! this macro hands to `#[cfg]`. That is not a workaround, it is the better
//! reference. Upstream `cfg-if` would only tell us "another macro agrees";
//! `cfg!()` tells us "the compiler's own answer to `is this predicate true?`
//! agrees, arm by arm". And nothing in a patch table can redirect it.
//!
//! # The upstream comparison was still run, once, out of tree
//!
//! Dropping the dev-dependency does not mean nobody checked. The comparison was
//! done in a throwaway workspace that has NO patch table, so the dependency
//! really did resolve to the registry copy, and it was ARMED first: the probe
//!
//! ```text
//! upstream_cfg_if::cfg_if! { @__temp_group pub fn is_upstream() -> bool { true } }
//! ```
//!
//! only compiles against upstream, whose internal arm is spelled
//! `@__temp_group`; this crate spells it `@__group` and has no rule that
//! matches, so a dependency that had silently become the shim would have failed
//! to build rather than passing vacuously. With that armed, four chains — the
//! `parking_lot_core` overlap, a three-true-arm `all()`/`any()` chain, `half`'s
//! f16c/fp16/std cascade and a multi-item dead arm — gave identical answers
//! through both macros (`unix.rs`, `b`, `fp16`, `live` on mac-arm).
//!
//! To repeat it: copy this crate into an empty workspace, add
//! `upstream-cfg-if = { package = "cfg-if", version = "1.0.4" }` to
//! `[dev-dependencies]`, and keep the `@__temp_group` probe in the test. Do NOT
//! do it inside aterm's workspace, where the patch table makes the answer a
//! foregone conclusion.
//!
//! # Why the chains look artificial
//!
//! Most of them are built out of `#[cfg(all())]` — always true, no predicates
//! to satisfy — and `#[cfg(any())]` — always false. That is deliberate. The
//! headline failure this file exists to catch is **wrong-arm selection**, and
//! it is invisible unless SEVERAL predicates hold at once: with exactly one
//! true predicate, first-match-wins, last-match-wins and "no negation at all"
//! all pick the same arm and all compile green. `all()`/`any()` let a chain
//! have three simultaneously-true arms on every cell, mac to wasm, with no
//! `#[cfg]` juggling and no host dependence.
//!
//! Real overlapping predicates are covered too — `case_parking_lot_core_shape`
//! is `parking_lot_core`'s actual `linux/android` vs `unix` overlap, the one
//! whose two answers are the futex parker and the pthread-condvar parker.

// Verbatim consumer predicates are not in play here; these chains use only
// well-known cfg keys. The allows below cover the scaffolding, not the subject.
#![allow(dead_code)]

// ONE chain in, two answers out.
//
// The chain is written ONCE, at the `case!` call site, and spliced into the
// real macro and into the `cfg!()` cascade. That
// matters: a hand-copied second version of a chain is a place for a typo to
// make the differential pass for the wrong reason.
//
// Passing the arms through this wrapper as `tt` is also, for free, coverage of
// shape 12 from the census — `half`'s `binary16/arch.rs:19` invokes `cfg_if!`
// from inside its own `macro_rules!` with fragments substituted into the arms.
macro_rules! case {
    (
        $name:ident ;
        [ $l0:literal , $($p0:tt)+ ]
        $( [ $l:literal , $($p:tt)+ ] )*
        else $le:literal
    ) => {
        mod $name {
            /// This crate's macro.
            pub mod real {
                cfg_if::cfg_if! {
                    if #[cfg( $($p0)+ )] { pub fn pick() -> &'static str { $l0 } }
                    $( else if #[cfg( $($p)+ )] { pub fn pick() -> &'static str { $l } } )*
                    else { pub fn pick() -> &'static str { $le } }
                }
            }

            /// THE REFERENCE: rustc's own answer, no macro from this crate
            /// anywhere in it.
            pub fn expected() -> &'static str {
                if cfg!( $($p0)+ ) { $l0 }
                $( else if cfg!( $($p)+ ) { $l } )*
                else { $le }
            }
        }
    };
}

// Three arms hold at once. Correct answer: the FIRST of them.
case! {
    case_three_true ;
    [ "arm1-false", any() ]
    [ "arm2-true",  all() ]
    [ "arm3-true",  all() ]
    else "else"
}

// Nothing holds; only the `else` is left.
case! {
    case_only_else ;
    [ "arm1", any() ]
    [ "arm2", any() ]
    else "else"
}

// The very first arm wins even though everything after it also holds.
case! {
    case_first_wins ;
    [ "arm1", all() ]
    [ "arm2", all() ]
    [ "arm3", all() ]
    else "else"
}

// `parking_lot_core-0.9.12/src/thread_parker/mod.rs:53`, predicates verbatim.
// On Linux BOTH of the first two hold and the answer is the difference between
// the futex parker and the pthread-condvar parker; on mac only the second does;
// on Windows only the third. Host-dependent by design — `expected()` tracks it.
case! {
    case_parking_lot_core_shape ;
    [ "linux.rs",       any(target_os = "linux", target_os = "android") ]
    [ "unix.rs",        unix ]
    [ "windows/mod.rs", windows ]
    [ "redox.rs",       target_os = "redox" ]
    else "generic.rs"
}

// `wgpu-hal-29.0.3/src/lib.rs:312` — same overlap shape, and NO final else, so
// the whole thing must be able to expand to nothing. (Written here with an else
// so `pick()` always exists; the no-else form is `no_arm_matches_expands_to_nothing`.)
case! {
    case_target_family_overlap ;
    [ "unix-family",    target_family = "unix" ]
    [ "any-target",     all() ]
    else "else"
}

#[test]
fn real_macro_matches_rustc_on_every_chain() {
    assert_eq!(case_three_true::real::pick(), case_three_true::expected());
    assert_eq!(case_three_true::real::pick(), "arm2-true");

    assert_eq!(case_only_else::real::pick(), case_only_else::expected());
    assert_eq!(case_only_else::real::pick(), "else");

    assert_eq!(case_first_wins::real::pick(), case_first_wins::expected());
    assert_eq!(case_first_wins::real::pick(), "arm1");

    assert_eq!(
        case_parking_lot_core_shape::real::pick(),
        case_parking_lot_core_shape::expected()
    );
    assert_eq!(
        case_target_family_overlap::real::pick(),
        case_target_family_overlap::expected()
    );
}

// THE MULTI-ITEM LEAK (upstream issue #90).
//
// A NON-selected arm holding two items, and a selected `else` holding one.
// Under a correct macro the whole first arm disappears; a macro that put the
// `#[cfg]` straight on the arm's tokens would bind it to `type _Ignored` only
// and emit `leaked()` anyway.
//
// How absence is proved, since you cannot call a function that does not exist:
// the module also defines its OWN `leaked()`. If the macro had emitted one, the
// module would have two and would not compile. The module compiles AND
// `leaked()` returns the module's own value, so the macro emitted nothing.
mod leak_correct {
    cfg_if::cfg_if! {
        if #[cfg(any())] {
            type _Ignored = u8;
            pub fn leaked() -> u8 { 1 }
        } else {
            pub fn selected() -> u8 { 2 }
        }
    }

    /// Would collide with a leaked definition; it compiles, so nothing leaked.
    pub fn leaked() -> u8 {
        99
    }
}

#[test]
fn a_dead_arm_contributes_none_of_its_items() {
    // `leaked()` is the module's own, so the dead arm's second item was not
    // emitted (upstream issue #90 would have emitted it and failed to build).
    assert_eq!(leak_correct::leaked(), 99);
    assert_eq!(leak_correct::selected(), 2);
}

// EMPTY EXPANSION, in the position that makes it matter.

/// `nix-0.29.0/src/sys/socket/addr.rs:584` in miniature: a `cfg_if!` whose arms
/// all fail, in statement position, followed by the function's real value. An
/// expansion of `()` or `;` here would either change the value or trip an
/// unused-expression lint under the workspace's `-D warnings`.
fn no_arm_matches() -> Result<u32, ()> {
    cfg_if::cfg_if! {
        if #[cfg(any())] {
            return Err(());
        } else if #[cfg(any())] {
            return Err(());
        }
    };
    Ok(7)
}

#[test]
fn no_arm_matches_expands_to_nothing() {
    assert_eq!(no_arm_matches(), Ok(7));
}

/// Tail position: the macro IS the block's value, on every cell.
/// `ring-0.17.14/src/digest/sha2/sha2_32.rs:29` is this shape.
fn tail_value() -> u32 {
    cfg_if::cfg_if! {
        if #[cfg(any())] {
            0
        } else if #[cfg(all())] {
            42
        } else {
            1
        }
    }
}

#[test]
fn tail_expression_produces_the_selected_arms_value() {
    assert_eq!(tail_value(), 42);
    // Same predicate list, answered by rustc directly.
    let expected = if cfg!(any()) {
        0
    } else if cfg!(all()) {
        42
    } else {
        1
    };
    assert_eq!(tail_value(), expected);
}

// RECURSION DEPTH.
//
// `getrandom-0.3.4/src/backends.rs:10` is 27 else-if arms plus an else and sets
// no `#![recursion_limit]`, so it runs on the default 128. This crate spends one
// nesting level per arm; a design spending four or five would work everywhere
// except there, and would fail with an error pointing at getrandom's lib.rs
// rather than at the macro. 40 arms here, with no `recursion_limit` attribute on
// this file, so the margin is measured rather than assumed.
fn deep_chain() -> u32 {
    cfg_if::cfg_if! {
        if #[cfg(any())] { 1 }
        else if #[cfg(any())] { 2 }
        else if #[cfg(any())] { 3 }
        else if #[cfg(any())] { 4 }
        else if #[cfg(any())] { 5 }
        else if #[cfg(any())] { 6 }
        else if #[cfg(any())] { 7 }
        else if #[cfg(any())] { 8 }
        else if #[cfg(any())] { 9 }
        else if #[cfg(any())] { 10 }
        else if #[cfg(any())] { 11 }
        else if #[cfg(any())] { 12 }
        else if #[cfg(any())] { 13 }
        else if #[cfg(any())] { 14 }
        else if #[cfg(any())] { 15 }
        else if #[cfg(any())] { 16 }
        else if #[cfg(any())] { 17 }
        else if #[cfg(any())] { 18 }
        else if #[cfg(any())] { 19 }
        else if #[cfg(any())] { 20 }
        else if #[cfg(any())] { 21 }
        else if #[cfg(any())] { 22 }
        else if #[cfg(any())] { 23 }
        else if #[cfg(any())] { 24 }
        else if #[cfg(any())] { 25 }
        else if #[cfg(any())] { 26 }
        else if #[cfg(any())] { 27 }
        else if #[cfg(any())] { 28 }
        else if #[cfg(any())] { 29 }
        else if #[cfg(any())] { 30 }
        else if #[cfg(any())] { 31 }
        else if #[cfg(any())] { 32 }
        else if #[cfg(any())] { 33 }
        else if #[cfg(any())] { 34 }
        else if #[cfg(any())] { 35 }
        else if #[cfg(any())] { 36 }
        else if #[cfg(any())] { 37 }
        else if #[cfg(any())] { 38 }
        else if #[cfg(all())] { 39 }
        else { 40 }
    }
}

#[test]
fn forty_arms_stay_inside_the_default_recursion_limit() {
    assert_eq!(deep_chain(), 39);
}
