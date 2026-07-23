// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration tests for `KittyKeyboardFlags` and `KittyKeyboardState` push/pop stack semantics.
//!
//! Part of #3139: closes keyboard state test coverage gap after extraction to aterm-types.
//! Tests verify protocol-correct behavior through the public API.

use aterm_types::{
    KittyKeyboardFlags, KittyKeyboardState, KittyKeyboardStateSnapshot, ScreenBuffer,
};

// --- KittyKeyboardFlags tests ---

#[test]
fn test_flags_none_is_zero() {
    let f = KittyKeyboardFlags::none();
    assert_eq!(f.bits(), 0);
    assert!(!f.disambiguate());
    assert!(!f.report_events());
    assert!(!f.report_alternates());
    assert!(!f.report_all_keys());
    assert!(!f.report_text());
}

#[test]
fn test_flags_from_bits_masks_invalid() {
    let f = KittyKeyboardFlags::from_bits(0xFF);
    assert_eq!(f.bits(), 0b1_1111);
}

#[test]
fn test_flags_individual_bits() {
    assert!(KittyKeyboardFlags::from_bits(KittyKeyboardFlags::DISAMBIGUATE).disambiguate());
    assert!(KittyKeyboardFlags::from_bits(KittyKeyboardFlags::REPORT_EVENTS).report_events());
    assert!(
        KittyKeyboardFlags::from_bits(KittyKeyboardFlags::REPORT_ALTERNATES).report_alternates()
    );
    assert!(KittyKeyboardFlags::from_bits(KittyKeyboardFlags::REPORT_ALL_KEYS).report_all_keys());
    assert!(KittyKeyboardFlags::from_bits(KittyKeyboardFlags::REPORT_TEXT).report_text());
}

#[test]
fn test_flags_individual_bits_no_cross_contamination() {
    // Each flag bit must activate only its own accessor, not others.
    let flags = [
        (KittyKeyboardFlags::DISAMBIGUATE, "disambiguate"),
        (KittyKeyboardFlags::REPORT_EVENTS, "report_events"),
        (KittyKeyboardFlags::REPORT_ALTERNATES, "report_alternates"),
        (KittyKeyboardFlags::REPORT_ALL_KEYS, "report_all_keys"),
        (KittyKeyboardFlags::REPORT_TEXT, "report_text"),
    ];
    for (i, (bit, name)) in flags.iter().enumerate() {
        let f = KittyKeyboardFlags::from_bits(*bit);
        let results = [
            f.disambiguate(),
            f.report_events(),
            f.report_alternates(),
            f.report_all_keys(),
            f.report_text(),
        ];
        for (j, result) in results.iter().enumerate() {
            if i == j {
                assert!(result, "{name} should be true when its bit is set");
            } else {
                assert!(
                    !result,
                    "{} should be false when only {} is set",
                    flags[j].1, name
                );
            }
        }
    }
}

#[test]
fn test_flags_apply_mode_1_set() {
    let mut f = KittyKeyboardFlags::from_bits(0b1_1111);
    f.apply(0b0_0001, 1);
    assert_eq!(f.bits(), 0b0_0001);
}

#[test]
fn test_flags_apply_mode_2_or() {
    let mut f = KittyKeyboardFlags::from_bits(0b0_0001);
    f.apply(0b0_0010, 2);
    assert_eq!(f.bits(), 0b0_0011);
}

#[test]
fn test_flags_apply_mode_3_and_not() {
    let mut f = KittyKeyboardFlags::from_bits(0b0_0011);
    f.apply(0b0_0010, 3);
    assert_eq!(f.bits(), 0b0_0001);
}

#[test]
fn test_flags_apply_explicit_unknown_mode_is_noop() {
    // kitty's set handler is if/else-if over modes 1/2/3 — an explicit
    // UNDEFINED mode (>= 4) changes nothing, so a program probing a future
    // mode semantic gets "unsupported" rather than "clobber my flags".
    let mut f = KittyKeyboardFlags::from_bits(0b1_1111);
    f.apply(0b0_0001, 4);
    assert_eq!(f.bits(), 0b1_1111);
}

#[test]
fn test_flags_apply_masks_input_bits() {
    let mut f = KittyKeyboardFlags::none();
    f.apply(0xFF, 1);
    assert_eq!(f.bits(), 0b1_1111);
}

// --- KittyKeyboardState push/pop tests ---

#[test]
fn test_state_new_has_no_flags() {
    let state = KittyKeyboardState::new();
    assert_eq!(state.flags().bits(), 0);
    assert_eq!(state.query_flags(), 0);
}

#[test]
fn test_push_set_flags_pop_restores_pre_push_state() {
    // Verify that set_flags() between push and pop doesn't corrupt the stack:
    // pop must restore the value saved at push time, regardless of what
    // set_flags did to current flags afterward.
    let mut state = KittyKeyboardState::new();

    state.set_flags(0b0_0011, 1); // flags = 0b11
    state.push_flags_for_buffer(0b0_0111, ScreenBuffer::Main); // saves 0b11, flags = 0b111
    assert_eq!(state.flags().bits(), 0b0_0111);

    // Mutate flags directly — this should NOT affect what pop restores
    state.set_flags(0b1_1111, 1); // flags = 0b11111
    assert_eq!(state.flags().bits(), 0b1_1111);

    // Fixed #5679: Restores the pre-push value (0b11) from stack[0].
    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0b0_0011);
}

#[test]
fn test_push_pop_basic_lifo() {
    let mut state = KittyKeyboardState::new();

    state.push_flags_for_buffer(0b0_0011, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0b0_0011);

    state.push_flags_for_buffer(0b0_0111, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0b0_0111);

    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0b0_0011);

    // Stack empty → reset to 0
    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0);
}

#[test]
fn test_push_pop_three_levels() {
    let mut state = KittyKeyboardState::new();

    state.push_flags_for_buffer(1, ScreenBuffer::Main);
    state.push_flags_for_buffer(3, ScreenBuffer::Main);
    state.push_flags_for_buffer(7, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 7);

    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 3);

    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 1);

    // Stack empty → reset to 0 (per Kitty spec)
    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0);
}

#[test]
fn test_pop_multi_count() {
    let mut state = KittyKeyboardState::new();

    state.push_flags_for_buffer(1, ScreenBuffer::Main);
    state.push_flags_for_buffer(3, ScreenBuffer::Main);
    state.push_flags_for_buffer(7, ScreenBuffer::Main);

    // Pop 2: skip over 3, restore 1
    state.pop_flags_for_buffer(2, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 1);
}

#[test]
fn test_pop_explicit_count_zero_pops_nothing() {
    // The spec's "defaulting to 1 if unspecified" covers only the OMITTED
    // parameter (resolved by the dispatcher); an EXPLICIT `CSI < 0 u` is
    // specified and kitty's pop loop (`num && i-- > 0`) pops nothing.
    let mut state = KittyKeyboardState::new();

    state.push_flags_for_buffer(1, ScreenBuffer::Main);
    state.push_flags_for_buffer(3, ScreenBuffer::Main);

    state.pop_flags_for_buffer(0, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 3);
}

#[test]
fn test_pop_count_exceeds_stack_resets_to_zero() {
    let mut state = KittyKeyboardState::new();

    state.push_flags_for_buffer(1, ScreenBuffer::Main);
    state.push_flags_for_buffer(3, ScreenBuffer::Main);

    // Pop 10 when only 2 on stack → per Kitty spec, all entries are popped
    // and flags reset to default (0), not to the oldest saved entry (#7482).
    state.pop_flags_for_buffer(10, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0);
}

#[test]
fn test_pop_from_empty_stack_resets_to_zero() {
    let mut state = KittyKeyboardState::new();
    state.set_flags(0b1_1111, 1);

    // Pop from empty stack → per Kitty spec, flags reset to 0 when
    // count > stack depth (#7421, #7482).
    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0);
}

#[test]
fn test_stack_overflow_evicts_oldest() {
    let mut state = KittyKeyboardState::new();

    // Push 8 entries to fill the stack
    for i in 1..=8 {
        state.push_flags_for_buffer(i, ScreenBuffer::Main);
    }
    assert_eq!(state.flags().bits(), 8);

    // Push 9th: evicts oldest
    state.push_flags_for_buffer(9, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 9);

    // Pop through: 8, 7, 6, 5, 4, 3, 2, then reset.
    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 8);

    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 7);

    for expected in (2..=6).rev() {
        state.pop_flags_for_buffer(1, ScreenBuffer::Main);
        assert_eq!(
            state.flags().bits(),
            expected,
            "expected {expected} after pop"
        );
    }

    // A pop that empties an OVERFLOWED stack resets all flags — the base
    // entry was evicted, and restoring the stale evicted-era stack[0] would
    // resurrect a mode the app long since abandoned. Matches kitty's
    // 8-retained-modes array byte-for-byte (7 saved + the live mode): after
    // pushes f1..f9, kitty yields f8..f2 on 7 pops, then 0.
    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0);
}

#[test]
fn test_screen_buffer_isolation() {
    let mut state = KittyKeyboardState::new();

    state.push_flags_for_buffer(1, ScreenBuffer::Main); // saves 0, flags=1
    state.push_flags_for_buffer(3, ScreenBuffer::Alternate); // saves 1, flags=3

    // Pop from main: restores pre-push value (0)
    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0);

    // Fixed #5679: Restores pre-push value (1) from alt stack[0].
    state.pop_flags_for_buffer(1, ScreenBuffer::Alternate);
    assert_eq!(state.flags().bits(), 1);
}

#[test]
fn test_screen_buffer_stacks_independent() {
    let mut state = KittyKeyboardState::new();

    // Push twice on main, once on alternate
    state.push_flags_for_buffer(1, ScreenBuffer::Main);
    state.push_flags_for_buffer(3, ScreenBuffer::Main);
    state.push_flags_for_buffer(7, ScreenBuffer::Alternate);
    assert_eq!(state.flags().bits(), 7);

    // Pop from main: 2 entries, restores saved-before-second-push (flags=1)
    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 1);

    // Fixed #5679: Restores pre-push value (flags=3) from alt stack[0].
    state.pop_flags_for_buffer(1, ScreenBuffer::Alternate);
    assert_eq!(state.flags().bits(), 3);

    // Fixed #5679: Restores pre-first-push value (flags=0) from main stack[0].
    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0);
}

#[test]
fn test_set_flags_direct() {
    let mut state = KittyKeyboardState::new();

    state.set_flags(0b0_0011, 1);
    assert_eq!(state.query_flags(), 0b0_0011);

    state.set_flags(0b0_1000, 2); // OR
    assert_eq!(state.query_flags(), 0b0_1011);

    state.set_flags(0b0_0001, 3); // AND NOT
    assert_eq!(state.query_flags(), 0b0_1010);
}

#[test]
fn test_reset_clears_everything() {
    let mut state = KittyKeyboardState::new();

    state.push_flags_for_buffer(1, ScreenBuffer::Main);
    state.push_flags_for_buffer(3, ScreenBuffer::Alternate);
    state.set_flags(0b1_1111, 1);

    state.reset();
    assert_eq!(state.flags().bits(), 0);

    // Popping after reset restores nothing
    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0);
    state.pop_flags_for_buffer(1, ScreenBuffer::Alternate);
    assert_eq!(state.flags().bits(), 0);
}

// --- switch_screen tests (#5985) ---

#[test]
fn test_switch_screen_basic_round_trip() {
    let mut state = KittyKeyboardState::new();
    state.set_flags(0b0_0101, 1); // flags = 5

    // Enter alt: saves main flags, restores alt (None → 0)
    state.switch_screen(true);
    assert_eq!(state.flags().bits(), 0);

    // Exit alt: saves alt flags (0), restores main (5)
    state.switch_screen(false);
    assert_eq!(state.flags().bits(), 0b0_0101);
}

#[test]
fn test_switch_screen_preserves_alt_flags_across_round_trips() {
    let mut state = KittyKeyboardState::new();
    state.set_flags(1, 1); // main = 1

    state.switch_screen(true); // enter alt, flags = 0
    state.set_flags(4, 1); // alt = 4

    state.switch_screen(false); // back to main, flags = 1
    assert_eq!(state.flags().bits(), 1);

    state.switch_screen(true); // re-enter alt, flags = 4 (preserved)
    assert_eq!(state.flags().bits(), 4);

    state.switch_screen(false); // back to main, flags = 1
    assert_eq!(state.flags().bits(), 1);
}

#[test]
fn test_switch_screen_double_enter_overwrites_saved_main() {
    // Pathological: calling switch_screen(true) twice without intervening false.
    // The second call overwrites main_saved_flags with current (alt) flags.
    let mut state = KittyKeyboardState::new();
    state.set_flags(5, 1); // main = 5

    state.switch_screen(true); // saves main=5, flags=0
    state.set_flags(3, 1); // alt flags = 3

    // Second enter-alt: saves current (3) as main, restores alt_saved (None→0)
    state.switch_screen(true);
    assert_eq!(state.flags().bits(), 0);

    // Exit: restores "main" which is now 3, not the original 5
    state.switch_screen(false);
    assert_eq!(state.flags().bits(), 3);
}

#[test]
fn test_switch_screen_after_reset_no_stale_flags() {
    let mut state = KittyKeyboardState::new();
    state.set_flags(7, 1);

    // Enter alt, saving main=7
    state.switch_screen(true);
    state.set_flags(15, 1);

    // Reset clears everything including saved screen flags
    state.reset();
    assert_eq!(state.flags().bits(), 0);

    // switch_screen after reset must not restore stale flags
    state.switch_screen(true);
    assert_eq!(state.flags().bits(), 0);

    state.switch_screen(false);
    assert_eq!(state.flags().bits(), 0);
}

#[test]
fn test_switch_screen_interleaved_with_push_pop() {
    let mut state = KittyKeyboardState::new();

    // Push on main screen
    state.push_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 1);

    // Enter alt screen
    state.switch_screen(true);
    assert_eq!(state.flags().bits(), 0);

    // Push on alt screen
    state.push_flags_for_buffer(4, ScreenBuffer::Alternate);
    assert_eq!(state.flags().bits(), 4);

    // Return to main
    state.switch_screen(false);
    assert_eq!(state.flags().bits(), 1);

    // Push more on main
    state.push_flags_for_buffer(8, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 8);

    // Re-enter alt — should restore alt flags (4)
    state.switch_screen(true);
    assert_eq!(state.flags().bits(), 4);

    // Pop from alt stack — restores pre-push alt value (0 from first switch)
    state.pop_flags_for_buffer(1, ScreenBuffer::Alternate);
    assert_eq!(state.flags().bits(), 0);

    // Return to main — restores saved main flags (8)
    state.switch_screen(false);
    assert_eq!(state.flags().bits(), 8);

    // Pop from main stack — restores pre-push value (1)
    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 1);
}

#[test]
fn test_flags_apply_mode_0_defaults_to_set() {
    // Mode 0 is a common edge case: CSI = flags ; 0 u
    // Should behave like mode 1 (set) per the default branch.
    let mut f = KittyKeyboardFlags::from_bits(0b1_1111);
    f.apply(0b0_0010, 0);
    assert_eq!(f.bits(), 0b0_0010);
}

#[test]
fn test_alt_stack_overflow_evicts_oldest() {
    let mut state = KittyKeyboardState::new();

    // Push 8 entries to fill the alt stack
    for i in 1..=8 {
        state.push_flags_for_buffer(i, ScreenBuffer::Alternate);
    }
    assert_eq!(state.flags().bits(), 8);

    // Push 9th: evicts oldest
    state.push_flags_for_buffer(9, ScreenBuffer::Alternate);
    assert_eq!(state.flags().bits(), 9);

    // Pop through: 8, 7, 6, 5, 4, 3, 2, then reset
    state.pop_flags_for_buffer(1, ScreenBuffer::Alternate);
    assert_eq!(state.flags().bits(), 8);

    for expected in (2..=7).rev() {
        state.pop_flags_for_buffer(1, ScreenBuffer::Alternate);
        assert_eq!(
            state.flags().bits(),
            expected,
            "expected {expected} after pop"
        );
    }

    // Pop-to-empty after eviction resets all flags (see the main-screen
    // overflow test for the kitty trace this mirrors).
    state.pop_flags_for_buffer(1, ScreenBuffer::Alternate);
    assert_eq!(state.flags().bits(), 0);
}

#[test]
fn test_screen_buffer_from_bool() {
    assert_eq!(ScreenBuffer::from(false), ScreenBuffer::Main);
    assert_eq!(ScreenBuffer::from(true), ScreenBuffer::Alternate);
}

#[test]
fn test_snapshot_roundtrip_restores_full_state() {
    let snapshot = KittyKeyboardStateSnapshot {
        flags: KittyKeyboardFlags::from_bits(0b1_0000),
        main_saved_flags: Some(KittyKeyboardFlags::from_bits(0b0_0001)),
        alt_saved_flags: Some(KittyKeyboardFlags::from_bits(0b0_0100)),
        main_stack: [
            KittyKeyboardFlags::from_bits(0b0_0000),
            KittyKeyboardFlags::from_bits(0b0_0001),
            KittyKeyboardFlags::from_bits(0b0_0011),
            KittyKeyboardFlags::none(),
            KittyKeyboardFlags::none(),
            KittyKeyboardFlags::none(),
            KittyKeyboardFlags::none(),
            KittyKeyboardFlags::none(),
        ],
        alt_stack: [
            KittyKeyboardFlags::from_bits(0b0_0100),
            KittyKeyboardFlags::from_bits(0b0_1000),
            KittyKeyboardFlags::none(),
            KittyKeyboardFlags::none(),
            KittyKeyboardFlags::none(),
            KittyKeyboardFlags::none(),
            KittyKeyboardFlags::none(),
            KittyKeyboardFlags::none(),
        ],
        main_sp: 3,
        alt_sp: 2,
    };

    let mut state = KittyKeyboardState::new();
    state.restore_snapshot(snapshot);

    let roundtrip = state.snapshot();
    assert_eq!(roundtrip, snapshot);

    state.pop_flags_for_buffer(1, ScreenBuffer::Alternate);
    assert_eq!(state.flags().bits(), 0b0_1000);

    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0b0_0011);
}
