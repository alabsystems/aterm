// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn oscillation_is_bounded_and_does_not_hide_the_terminal_record() {
    let mut progress = Progress::default();
    let mut record = Record {
        phase: Phase::Waiting,
        control_pending: true,
        control_failed: false,
        restore_pending: true,
        manifest_windows: 1,
        pending_adoptions: 0,
        first_present_pending: true,
        unpainted_windows: 1,
    };
    let mut emitted = Vec::new();
    for n in 0..1000 {
        record.unpainted_windows = n % 2;
        progress.note(record, &mut |r| emitted.push(r));
    }
    assert_eq!(emitted.len(), usize::from(Progress::LIMIT));
    record.phase = Phase::Rejected;
    progress.note(record, &mut |r| emitted.push(r));
    progress.note(record, &mut |r| emitted.push(r));
    assert_eq!(emitted.len(), usize::from(Progress::LIMIT) + 1);
    assert_eq!(emitted.last().unwrap().phase, Phase::Rejected);
    progress.clear();
    record.phase = Phase::Waiting;
    progress.note(record, &mut |r| emitted.push(r));
    assert_eq!(emitted.len(), usize::from(Progress::LIMIT) + 2);
}
