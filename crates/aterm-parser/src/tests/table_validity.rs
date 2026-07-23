// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Transition table validity tests: exhaustive entry validation and
//! static-vs-generated table consistency.

use super::super::table::{ActionType, TRANSITIONS};
use super::super::*;

/// Exhaustively verify the transition table has valid entries.
///
/// This is a runtime equivalent of the Kani proof `transition_table_lookup_safe`.
/// It checks all 14 × 256 = 3584 entries to ensure:
/// - All action values are valid ActionType variants (0-15)
/// - All next_state values are valid State variants (0-13)
#[test]
fn transition_table_all_entries_valid() {
    const ACTION_TYPE_MAX: u8 = ActionType::ApcEnd as u8; // 15
    const STATE_MAX: usize = State::COUNT; // 14

    let mut action_counts = [0usize; ActionType::ApcEnd as usize + 1];
    let mut state_counts = [0usize; State::COUNT];

    for state_idx in 0..State::COUNT {
        for byte in 0..256usize {
            let transition = TRANSITIONS[state_idx][byte];

            // Verify action is valid
            let action_u8 = transition.action as u8;
            assert!(
                action_u8 <= ACTION_TYPE_MAX,
                "Invalid action {} at state {} byte {}: action exceeds ActionType::ApcEnd ({})",
                action_u8,
                state_idx,
                byte,
                ACTION_TYPE_MAX
            );

            // Verify next_state is valid
            let next_state_idx = transition.next_state as usize;
            assert!(
                next_state_idx < STATE_MAX,
                "Invalid next_state {} at state {} byte {}: exceeds State::COUNT ({})",
                next_state_idx,
                state_idx,
                byte,
                STATE_MAX
            );

            // Count for statistics
            action_counts[action_u8 as usize] += 1;
            state_counts[next_state_idx] += 1;
        }
    }

    // Verify we checked all entries
    let total_entries: usize = action_counts.iter().sum();
    assert_eq!(
        total_entries,
        State::COUNT * 256,
        "Should check all {} entries",
        State::COUNT * 256
    );
}

/// Verify that the transition table constant matches the generated table.
///
/// This ensures TRANSITIONS static hasn't been corrupted or improperly initialized.
#[test]
fn transition_table_matches_generated() {
    use super::super::table::generate_table;

    let generated = generate_table();

    for state_idx in 0..State::COUNT {
        for byte in 0..256usize {
            let static_entry = TRANSITIONS[state_idx][byte];
            let generated_entry = generated[state_idx][byte];

            assert_eq!(
                static_entry.action, generated_entry.action,
                "Action mismatch at state {} byte {}: static={:?} generated={:?}",
                state_idx, byte, static_entry.action, generated_entry.action
            );
            assert_eq!(
                static_entry.next_state, generated_entry.next_state,
                "Next state mismatch at state {} byte {}: static={:?} generated={:?}",
                state_idx, byte, static_entry.next_state, generated_entry.next_state
            );
        }
    }
}
