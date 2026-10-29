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

#[test]
fn statement_arm_preserves_caller_hygiene_for_let_bindings() {
    cfg_if::cfg_if! {
        if #[cfg(any())] {
            let bound = 10u32;
        } else if #[cfg(all())] {
            let bound = 29u32;
        } else {
            let bound = 0u32;
        }
    }
    assert_eq!(bound, 29);
}

#[allow(clippy::too_many_arguments)]
fn expansion_state(
    p1: i64,
    p2: i64,
    p3: i64,
    has_else: i64,
    phase: i64,
    seen_true: i64,
    selected: i64,
    emitted_arms: i64,
    leaked_dead_items: i64,
    depth: i64,
) -> aterm_spec::interp::State {
    std::collections::BTreeMap::from([
        ("p1", p1),
        ("p2", p2),
        ("p3", p3),
        ("has_else", has_else),
        ("phase", phase),
        ("seen_true", seen_true),
        ("selected", selected),
        ("emitted_arms", emitted_arms),
        ("leaked_dead_items", leaked_dead_items),
        ("depth", depth),
    ])
}

mod ungrouped_negative_control {
    macro_rules! buggy_ungrouped {
        (if #[cfg($($p:tt)+)] { $( $it:item )+ }) => {
            #[cfg($($p)+)]
            $( $it )+
        };
    }

    buggy_ungrouped! {
        if #[cfg(any())] {
            pub fn first_stripped() -> i64 { 1 }
            pub fn second_leaked_from_dead_arm() -> i64 { 1 }
        }
    }
}

#[test]
fn cfg_if_expansion_conformance_and_tripwire_negative_controls() {
    let m = aterm_models_core::cfg_if_expansion_model();
    assert!(aterm_spec::interp::prove_and_catch(&m) > 1);
    let check_step = |prev: &aterm_spec::interp::State,
                      next: &aterm_spec::interp::State,
                      expected: &'static str| {
        assert_eq!(
            aterm_spec::interp::admits(&m, prev, next),
            Some(expected),
            "step {expected} from {prev:?} to {next:?} must refine CfgIfExpansion"
        );
        for inv in &m.invariants {
            assert!(
                m.check_invariant(inv.name, next),
                "invariant {} violated at {next:?}",
                inv.name
            );
        }
    };

    // 1. Conformance trace for overlapping true arms (`all()`, `all()`, `else`):
    //    `case_first_wins::real::pick()` selects arm 1 ("arm1") with 0 leaked dead items.
    let sel_first = match case_first_wins::real::pick() {
        "arm1" => 1,
        "arm2" => 2,
        _ => 3,
    };
    assert_eq!(sel_first, 1);
    let s0 = m.init_state();
    assert_eq!(s0, expansion_state(0, 0, 0, 1, 0, 0, 0, 0, 0, 0));
    let s1 = expansion_state(1, 0, 0, 1, 0, 0, 0, 0, 0, 0);
    check_step(&s0, &s1, "SetP1");
    let s2 = expansion_state(1, 1, 0, 1, 0, 0, 0, 0, 0, 0);
    check_step(&s1, &s2, "SetP2");
    let s3 = expansion_state(1, 1, 0, 1, 1, 0, 0, 0, 0, 1);
    check_step(&s2, &s3, "Enter");
    let s4 = expansion_state(1, 1, 0, 1, 2, 1, sel_first, 1, 0, 2);
    check_step(&s3, &s4, "StepArm1");
    let s5 = expansion_state(1, 1, 0, 1, 3, 1, sel_first, 1, 0, 3);
    check_step(&s4, &s5, "StepArm2");
    let s6 = expansion_state(1, 1, 0, 1, 4, 1, sel_first, 1, 0, 4);
    check_step(&s5, &s6, "StepArm3");
    let s7 = expansion_state(1, 1, 0, 1, 5, 1, sel_first, 1, 0, 6);
    check_step(&s6, &s7, "BaseEmpty");

    // 2. Conformance trace for an else-less all-false chain (`no_arm_matches` -> 0 arms emitted).
    fn all_false_elseless() -> i64 {
        cfg_if::cfg_if! {
            if #[cfg(any())] {
                return 1;
            } else if #[cfg(any())] {
                return 2;
            } else if #[cfg(any())] {
                return 3;
            }
        }
        0
    }
    let emitted_none = all_false_elseless();
    assert_eq!(emitted_none, 0);
    let n1 = expansion_state(0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
    check_step(&s0, &n1, "OmitElse");
    let n2 = expansion_state(0, 0, 0, 0, 1, 0, 0, 0, 0, 1);
    check_step(&n1, &n2, "Enter");
    let n3 = expansion_state(0, 0, 0, 0, 2, 0, 0, 0, 0, 2);
    check_step(&n2, &n3, "StepArm1");
    let n4 = expansion_state(0, 0, 0, 0, 3, 0, 0, 0, 0, 3);
    check_step(&n3, &n4, "StepArm2");
    let n5 = expansion_state(0, 0, 0, 0, 4, 0, 0, 0, 0, 4);
    check_step(&n4, &n5, "StepArm3");
    let n6 = expansion_state(0, 0, 0, 0, 5, 0, 0, emitted_none, 0, 6);
    check_step(&n5, &n6, "BaseEmpty");

    // 3. Negative control 1 (last-match-wins / missing `not(any(seen))`):
    //    omitting `not(any(..))` lets arm 2 fire after arm 1 already matched.
    macro_rules! buggy_last_wins {
        ($hits:ident; $( [$idx:expr, $($p:tt)+] )+) => {{
            $(
                #[cfg($($p)+)]
                {
                    $hits.push($idx);
                }
            )+
        }};
    }
    let mut bug1_hits = Vec::<i64>::new();
    buggy_last_wins!(bug1_hits; [1, all()] [2, all()] [3, all()]);
    assert_eq!(bug1_hits, vec![1, 2, 3]);
    let bug1_sel = *bug1_hits.last().expect("three arms fired");
    let bug1_emitted = bug1_hits.len() as i64;
    let bug1_step2 = expansion_state(1, 1, 0, 1, 3, 1, 2, 2, 0, 3);
    assert_eq!(aterm_spec::interp::admits(&m, &s4, &bug1_step2), None);
    assert_eq!(
        aterm_spec::interp::admits(&aterm_spec::interp::with_buggy(&m, 1), &s4, &bug1_step2),
        Some("StepArm2")
    );
    let bug1_final = expansion_state(1, 1, 1, 1, 5, 1, bug1_sel, bug1_emitted, 0, 6);
    assert!(!m.check_invariant("FirstMatchSelected", &bug1_final));

    // 4. Negative control 2 (un-grouped multi-item arm, upstream issue #90):
    //    without `@__group`, `#[cfg(any())]` strips only item 1 and leaks item 2.
    let leaked = ungrouped_negative_control::second_leaked_from_dead_arm();
    assert_eq!(leaked, 1);
    let bug2_enter = expansion_state(0, 0, 0, 1, 1, 0, 0, 0, 0, 1);
    let bug2_step1 = expansion_state(0, 0, 0, 1, 2, 0, 0, 0, leaked, 2);
    assert_eq!(
        aterm_spec::interp::admits(&m, &bug2_enter, &bug2_step1),
        None
    );
    assert_eq!(
        aterm_spec::interp::admits(
            &aterm_spec::interp::with_buggy(&m, 2),
            &bug2_enter,
            &bug2_step1
        ),
        Some("StepArm1")
    );
    assert!(!m.check_invariant("DeadArmFullyAbsent", &bug2_step1));
}
