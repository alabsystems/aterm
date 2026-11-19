// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn withheld_tui_requires_a_closed_denied_turn_and_its_completed_401() {
    let turn = |denied, completed| {
        Some(turn::Turn {
            thread: "original".into(),
            denied,
            completed,
        })
    };
    let receipts = |accepted, refused, written| SourceResponses {
        accepted,
        refused,
        written,
    };
    let denied = receipts(0, 1, 1);
    assert_eq!(
        denied_turn(turn(true, false), &denied).unwrap(),
        Some(("original".into(), 1))
    );
    assert_eq!(denied_turn(None, &denied).unwrap(), None);
    assert_eq!(denied_turn(turn(false, false), &denied).unwrap(), None);
    assert_eq!(
        denied_turn(turn(true, false), &receipts(0, 0, 0)).unwrap(),
        None
    );
    assert_eq!(
        denied_turn(turn(true, false), &receipts(0, 2, 1)).unwrap(),
        None
    );
    assert_eq!(denied_turn(turn(false, true), &denied).unwrap_err().code, 1);
    assert_eq!(denied_turn(None, &receipts(1, 0, 0)).unwrap_err().code, 1);
}
