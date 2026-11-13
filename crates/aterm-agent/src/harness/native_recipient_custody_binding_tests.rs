// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn binding_replay_keeps_four_recipient_frames_per_operation() {
    let (_home, paths, _, _) = super::super::super::tests::fixture();
    let key = write_key(&paths).unwrap();
    let maximum =
        (LIMITS.records / 5).min(LIMITS.bytes / (FRAME_BYTES + 4 * recipients::FRAME_BYTES));
    let mut view = View::default();
    for number in 0..=maximum {
        let entry = Entry {
            receipt: Receipt {
                operation: key.operation(&format!("{number:032x}")),
                prepare: [1; 32],
                capability: [2; 32],
            },
            legacy_custody: [3; 32],
            legacy_sequence: 0,
            legacy_time: 1,
            observed: Observed {
                sequence: view.sequence,
                recorded_ms: 1,
            },
        };
        let body = view.encode(&key, &entry);
        let sequence = view.sequence;
        assert_eq!(view.ingest(&key, &body).is_ok(), number < maximum);
        if number == maximum {
            assert_eq!(view.sequence, sequence);
        }
    }
}

#[test]
fn binding_records_authenticate_and_cannot_replace_an_operation() {
    let (_home, paths, _, _) = super::super::super::tests::fixture();
    let key = write_key(&paths).unwrap();
    let mut view = View::default();
    let entry = Entry {
        receipt: Receipt {
            operation: [1; 32],
            prepare: [2; 32],
            capability: [3; 32],
        },
        legacy_custody: [4; 32],
        legacy_sequence: 5,
        legacy_time: 6,
        observed: Observed {
            sequence: 0,
            recorded_ms: 7,
        },
    };
    let valid = view.encode(&key, &entry);
    let mut forged = valid.clone();
    forged[148] ^= 1;
    assert!(view.ingest(&key, &forged).is_err());
    assert_eq!(view.sequence, 0);
    view.ingest(&key, &valid).unwrap();
    assert!(view.ingest(&key, &view.encode(&key, &entry)).is_err());
    assert_eq!(view.sequence, 1);
}
