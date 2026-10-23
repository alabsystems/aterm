// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Headless tests for the `key|send as=person` development seam.

#![cfg(unix)]

use crate::headless_boot::{self, field};

/// The session's `(human_ms, human_seq)` off its `status` record.
fn person(inst: &headless_boot::Instance, sid: &str) -> (String, u64) {
    let status = headless_boot::status(inst, sid);
    let ms = field(&status, "human_ms")
        .unwrap_or_else(|| panic!("status names human_ms: {status}"))
        .to_string();
    let seq = field(&status, "human_seq")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("status names human_seq: {status}"));
    (ms, seq)
}

#[test]
fn as_person_stamps_the_person_and_a_plain_write_never_does() {
    let inst = headless_boot::boot("atps", &[]);
    let (_, sid) = headless_boot::boot_session(&inst);
    let at = format!("@{sid}");

    let (ms0, seq0) = person(&inst, &sid);
    assert_eq!(ms0, "-", "no person has keyed a fresh session");

    headless_boot::ctl_ok(&inst, &[&at, "key", "space"]);
    headless_boot::ctl_ok(&inst, &[&at, "send", "x"]);
    assert_eq!(
        person(&inst, &sid),
        (ms0, seq0),
        "a plain write is a controller's"
    );

    headless_boot::ctl_ok(&inst, &[&at, "key", "as=person", "space"]);
    let (ms1, seq1) = person(&inst, &sid);
    assert!(
        ms1.parse::<u64>().is_ok(),
        "a person's press stamps human_ms: {ms1}"
    );
    assert!(seq1 > seq0, "a person's press is counted: {seq0} -> {seq1}");
    headless_boot::ctl_ok(&inst, &[&at, "send", "as=person", "y"]);
    let (_, seq2) = person(&inst, &sid);
    assert!(seq2 > seq1, "a person's text is counted: {seq1} -> {seq2}");

    let fenced = headless_boot::said(&headless_boot::ctl(
        &inst,
        &[&at, "key", "as=person", "if=.", "space"],
    ));
    assert!(
        fenced.contains("ERR usage: as=person takes no fence"),
        "a fenced as=person is refused: {fenced}"
    );
    assert_eq!(
        person(&inst, &sid).1,
        seq2,
        "a refused write counts no gesture"
    );
}
