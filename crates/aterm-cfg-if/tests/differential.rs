// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Differential tests comparing `cfg_if!` arm selection against `cfg!()`.

#![allow(dead_code)]

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

case! {
    case_three_true ;
    [ "arm1-false", any() ]
    [ "arm2-true",  all() ]
    [ "arm3-true",  all() ]
    else "else"
}

case! {
    case_only_else ;
    [ "arm1", any() ]
    [ "arm2", any() ]
    else "else"
}

case! {
    case_first_wins ;
    [ "arm1", all() ]
    [ "arm2", all() ]
    [ "arm3", all() ]
    else "else"
}

case! {
    case_parking_lot_core_shape ;
    [ "linux.rs",       any(target_os = "linux", target_os = "android") ]
    [ "unix.rs",        unix ]
    [ "windows/mod.rs", windows ]
    [ "redox.rs",       target_os = "redox" ]
    else "generic.rs"
}

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
    assert_eq!(leak_correct::leaked(), 99);
    assert_eq!(leak_correct::selected(), 2);
}

/// `nix-0.29.0/src/sys/socket/addr.rs:584` in miniature: a `cfg_if!` whose arms
/// all fail, in statement position, followed by the function's real value.
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
    let expected = if cfg!(any()) {
        0
    } else if cfg!(all()) {
        42
    } else {
        1
    };
    assert_eq!(tail_value(), expected);
}

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
