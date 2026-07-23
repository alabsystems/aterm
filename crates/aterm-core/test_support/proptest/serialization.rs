// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Serialization roundtrip and checkpoint header property tests.

use proptest::prelude::*;

// =============================================================================
// Serialization roundtrip property tests (#1920)
// =============================================================================

proptest! {
    /// SelectionState serialize/deserialize roundtrip: for any valid field
    /// values, deserialize(serialize(state)) recovers the original state.
    #[test]
    fn selection_state_roundtrip(
        start_row in proptest::num::u32::ANY,
        start_col in proptest::num::u32::ANY,
        end_row in proptest::num::u32::ANY,
        end_col in proptest::num::u32::ANY,
        mode_u8 in 0u8..3u8,
    ) {
        use crate::session::{SelectionState, SelectionMode};
        let mode = SelectionMode::from_u8(mode_u8).unwrap();
        let state = SelectionState {
            start_row,
            start_col,
            end_row,
            end_col,
            mode,
        };
        let bytes = state.serialize();
        prop_assert_eq!(bytes.len(), 17);
        let recovered = SelectionState::deserialize(&bytes)
            .expect("deserialize should succeed on serialized data");
        prop_assert_eq!(recovered.start_row, start_row);
        prop_assert_eq!(recovered.start_col, start_col);
        prop_assert_eq!(recovered.end_row, end_row);
        prop_assert_eq!(recovered.end_col, end_col);
        prop_assert_eq!(recovered.mode, mode);
    }

    /// SelectionState::deserialize rejects short buffers.
    #[test]
    fn selection_state_short_buffer_rejected(buf_len in 0usize..17usize) {
        use crate::session::SelectionState;
        use std::io::ErrorKind;
        let buf = vec![0u8; buf_len];
        let result = SelectionState::deserialize(&buf);
        prop_assert!(
            matches!(result, Err(err) if err.kind() == ErrorKind::InvalidData),
            "short buffer length {} should fail with InvalidData",
            buf_len
        );
    }

    /// SelectionMode::from_u8 returns None for out-of-range values.
    #[test]
    fn selection_mode_from_u8_invalid(val in 3u8..=255u8) {
        use crate::session::SelectionMode;
        prop_assert!(SelectionMode::from_u8(val).is_none());
    }

    /// SelectionState::deserialize maps invalid mode bytes to Character (default).
    ///
    /// This documents that `deserialize` uses `unwrap_or_default()` on the mode
    /// byte — corrupted mode bytes are silently accepted as `Character` rather
    /// than rejected. This is a deliberate design choice for backwards
    /// compatibility, but means the mode field does NOT roundtrip faithfully
    /// through arbitrary byte corruption.
    #[test]
    fn selection_state_invalid_mode_defaults_to_character(
        start_row in proptest::num::u32::ANY,
        start_col in proptest::num::u32::ANY,
        end_row in proptest::num::u32::ANY,
        end_col in proptest::num::u32::ANY,
        mode_byte in 3u8..=255u8,
    ) {
        use crate::session::{SelectionState, SelectionMode};
        let mut buf = [0u8; 17];
        buf[0..4].copy_from_slice(&start_row.to_le_bytes());
        buf[4..8].copy_from_slice(&start_col.to_le_bytes());
        buf[8..12].copy_from_slice(&end_row.to_le_bytes());
        buf[12..16].copy_from_slice(&end_col.to_le_bytes());
        buf[16] = mode_byte;
        let state = SelectionState::deserialize(&buf)
            .expect("deserialize should accept 17-byte buffer regardless of mode byte");
        prop_assert_eq!(state.mode, SelectionMode::Character,
            "invalid mode byte {} should default to Character", mode_byte);
        prop_assert_eq!(state.start_row, start_row);
        prop_assert_eq!(state.start_col, start_col);
        prop_assert_eq!(state.end_row, end_row);
        prop_assert_eq!(state.end_col, end_col);
    }
}

