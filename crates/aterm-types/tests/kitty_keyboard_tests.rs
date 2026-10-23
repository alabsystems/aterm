// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration tests for `KittyKeyboardFlags` and `KittyKeyboardState` push/pop stack semantics.

use aterm_types::{
    KittyKeyboardFlags, KittyKeyboardState, KittyKeyboardStateSnapshot, ScreenBuffer,
};

#[test]
fn test_flags_from_bits_masks_invalid() {
    let f = KittyKeyboardFlags::from_bits(0xFF);
    assert_eq!(f.bits(), 0b1_1111);
}

#[test]
fn test_flags_individual_bits_no_cross_contamination() {
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

/// `CSI = flags ; mode u`: mode 0/1 sets, 2 ORs, 3 AND-NOTs, and >=4 is a no-op.
#[test]
fn test_flags_apply_modes() {
    for (start, input, mode, want) in [
        (0b1_1111, 0b0_0001, 1, 0b0_0001),
        (0b0_0001, 0b0_0010, 2, 0b0_0011),
        (0b0_0011, 0b0_0010, 3, 0b0_0001),
        (0b1_1111, 0b0_0010, 0, 0b0_0010),
        (0b1_1111, 0b0_0001, 4, 0b1_1111),
        (0, 0xFF, 1, 0b1_1111),
    ] {
        let mut f = KittyKeyboardFlags::from_bits(start);
        f.apply(input, mode);
        assert_eq!(f.bits(), want, "{start:#b} apply {input:#b} mode {mode}");
    }
}

#[test]
fn test_push_set_flags_pop_restores_pre_push_state() {
    let mut state = KittyKeyboardState::new();

    state.set_flags(0b0_0011, 1); // flags = 0b11
    state.push_flags_for_buffer(0b0_0111, ScreenBuffer::Main); // saves 0b11, flags = 0b111
    assert_eq!(state.flags().bits(), 0b0_0111);

    state.set_flags(0b1_1111, 1); // flags = 0b11111
    assert_eq!(state.flags().bits(), 0b1_1111);

    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0b0_0011);
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

    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0);
}

#[test]
fn test_pop_multi_count() {
    let mut state = KittyKeyboardState::new();

    state.push_flags_for_buffer(1, ScreenBuffer::Main);
    state.push_flags_for_buffer(3, ScreenBuffer::Main);
    state.push_flags_for_buffer(7, ScreenBuffer::Main);

    state.pop_flags_for_buffer(2, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 1);
}

#[test]
fn test_pop_explicit_count_zero_pops_nothing() {
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

    state.pop_flags_for_buffer(10, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0);
}

#[test]
fn test_pop_from_empty_stack_resets_to_zero() {
    let mut state = KittyKeyboardState::new();
    state.set_flags(0b1_1111, 1);

    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0);
}

#[test]
fn test_stack_overflow_evicts_oldest() {
    let mut state = KittyKeyboardState::new();

    for i in 1..=8 {
        state.push_flags_for_buffer(i, ScreenBuffer::Main);
    }
    assert_eq!(state.flags().bits(), 8);

    state.push_flags_for_buffer(9, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 9);

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

    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0);
}

#[test]
fn test_screen_buffer_stacks_independent() {
    let mut state = KittyKeyboardState::new();

    state.push_flags_for_buffer(1, ScreenBuffer::Main);
    state.push_flags_for_buffer(3, ScreenBuffer::Main);
    state.push_flags_for_buffer(7, ScreenBuffer::Alternate);
    assert_eq!(state.flags().bits(), 7);

    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 1);

    state.pop_flags_for_buffer(1, ScreenBuffer::Alternate);
    assert_eq!(state.flags().bits(), 3);

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

    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 0);
    state.pop_flags_for_buffer(1, ScreenBuffer::Alternate);
    assert_eq!(state.flags().bits(), 0);
}

#[test]
fn test_switch_screen_basic_round_trip() {
    let mut state = KittyKeyboardState::new();
    state.set_flags(0b0_0101, 1); // flags = 5

    state.switch_screen(true);
    assert_eq!(state.flags().bits(), 0);

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
    let mut state = KittyKeyboardState::new();
    state.set_flags(5, 1); // main = 5

    state.switch_screen(true); // saves main=5, flags=0
    state.set_flags(3, 1); // alt flags = 3

    state.switch_screen(true);
    assert_eq!(state.flags().bits(), 0);

    state.switch_screen(false);
    assert_eq!(state.flags().bits(), 3);
}

#[test]
fn test_switch_screen_after_reset_no_stale_flags() {
    let mut state = KittyKeyboardState::new();
    state.set_flags(7, 1);

    state.switch_screen(true);
    state.set_flags(15, 1);

    state.reset();
    assert_eq!(state.flags().bits(), 0);

    state.switch_screen(true);
    assert_eq!(state.flags().bits(), 0);

    state.switch_screen(false);
    assert_eq!(state.flags().bits(), 0);
}

#[test]
fn test_switch_screen_interleaved_with_push_pop() {
    let mut state = KittyKeyboardState::new();

    state.push_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 1);

    state.switch_screen(true);
    assert_eq!(state.flags().bits(), 0);

    state.push_flags_for_buffer(4, ScreenBuffer::Alternate);
    assert_eq!(state.flags().bits(), 4);

    state.switch_screen(false);
    assert_eq!(state.flags().bits(), 1);

    state.push_flags_for_buffer(8, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 8);

    state.switch_screen(true);
    assert_eq!(state.flags().bits(), 4);

    state.pop_flags_for_buffer(1, ScreenBuffer::Alternate);
    assert_eq!(state.flags().bits(), 0);

    state.switch_screen(false);
    assert_eq!(state.flags().bits(), 8);

    state.pop_flags_for_buffer(1, ScreenBuffer::Main);
    assert_eq!(state.flags().bits(), 1);
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
