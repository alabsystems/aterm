// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_signed_cli_withheld_control_requires_a_failed_original_turn_and_a_completed_provider_401() {
    let event = |kind: &str| J::obj([("type", J::s(kind))]);
    let failed = [event("turn.started"), event("turn.failed")];
    let receipts = |accepted, refused, written| SourceResponses {
        accepted,
        refused,
        written,
    };
    assert!(judge(Some(1), &failed, true, &receipts(0, 1, 1)).is_ok());
    for (identified, receipts, code) in [
        (false, receipts(0, 1, 1), 3),
        (true, receipts(0, 0, 0), 3),
        (true, receipts(0, 2, 1), 3),
        (true, receipts(1, 1, 1), 1),
    ] {
        assert_eq!(
            judge(Some(1), &failed, identified, &receipts)
                .unwrap_err()
                .code,
            code
        );
    }
    for exit in [None, Some(0)] {
        assert!(judge(exit, &failed, true, &receipts(0, 1, 1)).is_err());
    }
    let completed = [
        event("turn.started"),
        event("turn.failed"),
        event("turn.completed"),
    ];
    assert!(judge(Some(1), &completed, true, &receipts(0, 1, 1)).is_err());
}
