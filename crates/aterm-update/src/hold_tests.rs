// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_update_core::update_record::{Cause, HOLD_MAX, STAGE_SPANS, UpdateRecord};

use crate::paths::Staging;

const SHA: &str = "abababababababababababababababababababababababababababababababab";
const NOW: u64 = 1_800_000_000;

#[test]
fn a_failed_stage_holds_its_artifact_on_the_stage_schedule_and_lapses() {
    let s = Staging::scratch("hold-stage");
    let hold = super::stage_failed(&s, 42, "0.42.0", SHA, "bytes did not verify", NOW)
        .expect("the stage failure holds the artifact");
    assert_eq!(hold.cause, Cause::Stage);
    assert_eq!(hold.until, NOW + STAGE_SPANS[0]);
    assert!(super::held(&s, 42, SHA, NOW).is_some());
    assert!(
        super::held(&s, 42, &"cd".repeat(32), NOW).is_none(),
        "another digest of the build is another artifact"
    );
    assert!(super::held(&s, 43, SHA, NOW).is_none());
    assert!(
        super::held(&s, 42, SHA, NOW + STAGE_SPANS[0]).is_none(),
        "the hold lapses at its until"
    );
    let again = super::stage_failed(&s, 42, "0.42.0", SHA, "again", NOW + STAGE_SPANS[0])
        .expect("a repeat after the lapse holds again");
    assert_eq!(again.tries, 2);
    assert_eq!(again.until, NOW + STAGE_SPANS[0] + STAGE_SPANS[1]);
    assert!(super::refusal(&again).contains("try 2"));
}

#[test]
fn no_hold_lasts_past_a_day() {
    let s = Staging::scratch("hold-day");
    let mut at = NOW;
    for _ in 0..8 {
        let hold = super::stage_failed(&s, 42, "0.42.0", SHA, "again", at).expect("held");
        assert!(hold.until - at <= HOLD_MAX);
        at = hold.until;
    }
}

#[test]
fn an_unreadable_record_holds_nothing_and_the_next_write_replaces_it() {
    let s = Staging::scratch("hold-unreadable");
    std::fs::write(s.record(), b"{ not a record").unwrap();
    assert!(super::held(&s, 42, SHA, NOW).is_none(), "fails open");
    super::stage_failed(&s, 42, "0.42.0", SHA, "why", NOW).expect("held");
    assert!(UpdateRecord::read(&s.record()).unwrap().is_some());
}

#[test]
fn a_digest_that_is_not_a_sha256_holds_nothing() {
    let s = Staging::scratch("hold-bad-digest");
    assert!(super::stage_failed(&s, 42, "0.42.0", "nope", "why", NOW).is_none());
    assert!(super::held(&s, 42, "nope", NOW).is_none());
    assert!(!s.record().exists());
}
