// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{video_empty_ledger_note, video_keys_reply_tokens, video_keys_status_tokens};

#[test]
fn status_and_reply_tokens_state_both_counts_only_for_a_keys_take() {
    assert_eq!(
        video_keys_status_tokens(true, 0, 5),
        " keys=true inputs=0 unlogged=5"
    );
    assert_eq!(video_keys_status_tokens(false, 0, 0), " keys=false");
    assert_eq!(
        video_keys_reply_tokens(true, 0, 3),
        " inputs=0 unlogged_inputs=3"
    );
    assert_eq!(video_keys_reply_tokens(false, 0, 0), "");
}

#[test]
fn empty_ledger_note_separates_measured_zero_from_unmeasurable() {
    let notes = [
        video_empty_ledger_note(false, 0, 0),
        video_empty_ledger_note(true, 4, 0),
        video_empty_ledger_note(true, 3, 3),
        video_empty_ledger_note(true, 5, 2),
        video_empty_ledger_note(true, 0, 0),
    ];
    for (i, note) in notes.iter().enumerate() {
        for other in &notes[i + 1..] {
            assert_ne!(note, other, "two situations share one note");
        }
        assert!(
            !note.contains('"') && !note.contains('\\'),
            "a note must stay JSON-safe unescaped: {note}"
        );
    }
}
