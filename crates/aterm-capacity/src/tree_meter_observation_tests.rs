// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_unavailable_attempt_never_reuses_a_successful_measurement() {
    let mut tally = Tally {
        group: Some(9),
        within: vec![(42, 7)],
        ..Tally::default()
    };
    let member = Member {
        pid: 42,
        start: 7,
        footprint: 96 << 20,
        peak: 96 << 20,
    };
    tally.observe(Some(&[member]), true);
    assert_eq!(tally.observation.sequence, 1);
    assert_eq!(tally.observation.members, [(member, true)]);
    tally.observe(None, false);
    assert_eq!(tally.observation.sequence, 2);
    assert!(!tally.observation.available);
    assert!(tally.observation.members.is_empty());
    assert_eq!(tally.within, [(42, 7)], "diagnosis does not change policy");
    tally.observe(Some(&[Member { start: 8, ..member }]), true);
    assert!(
        !tally.observation.members[0].1,
        "a reused PID is not eligible"
    );
}
