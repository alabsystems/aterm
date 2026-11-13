// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_update_core::update_record::{Cause, Failure, Sha, UpdateRecord, Witness};

const NOW: u64 = 1_800_000_000;

fn held(build: u64, sha: Sha) -> UpdateRecord {
    let mut record = UpdateRecord::fresh();
    record.set_hold(
        Failure {
            build,
            version: "0.42.0".into(),
            sha,
            cause: Cause::CrashLoop,
            witness: Witness::Headless,
            why: "it did not start".into(),
        },
        NOW,
    );
    record
}

fn running(build: u64, sha: Sha, proven: bool) -> UpdateRecord {
    let mut record = UpdateRecord::fresh();
    record.running = build;
    record.running_sha = Some(sha);
    record.proven = proven;
    record
}

#[test]
fn a_peer_running_the_held_artifact_proven_lifts_the_hold_once() {
    let sha = Sha([7; 32]);
    let mut record = held(42, sha);
    let peers = [running(41, sha, true), running(42, sha, true)];
    assert!(super::lift_by(&mut record, &peers, NOW + 10));
    assert!(!record.holds(42, &sha, NOW + 10), "the hold lapsed at once");
    // The same artifact fails again: its hold lasts its schedule, whatever the peers say.
    record.set_hold(
        Failure {
            build: 42,
            version: "0.42.0".into(),
            sha,
            cause: Cause::CrashLoop,
            witness: Witness::Headless,
            why: "again".into(),
        },
        NOW + 20,
    );
    assert!(record.holds(42, &sha, NOW + 20));
    assert!(
        !super::lift_by(&mut record, &peers, NOW + 30),
        "once per artifact"
    );
    assert!(record.holds(42, &sha, NOW + 30));
}

#[test]
fn unproven_or_other_artifacts_lift_nothing() {
    let sha = Sha([7; 32]);
    let mut record = held(42, sha);
    let peers = [running(42, sha, false), running(42, Sha([8; 32]), true)];
    assert!(!super::lift_by(&mut record, &peers, NOW + 10));
    assert!(record.holds(42, &sha, NOW + 10));
}

#[test]
fn with_no_lab_reader_there_are_no_rows() {
    assert!(super::rows().is_none() || super::LAB.get().is_some());
    if super::LAB.get().is_none() {
        assert!(super::peers().is_empty());
    }
}

#[test]
fn this_macs_own_row_is_no_peer() {
    let sha = Sha([7; 32]);
    let mut record = held(42, sha);
    let row = |own| super::LabRow {
        machine: "m".into(),
        record: running(42, sha, true),
        own,
    };
    let peers = super::peer_records(vec![row(true)]);
    assert!(
        peers.is_empty(),
        "a stale own row says this Mac ran it proven"
    );
    assert!(!super::lift_by(&mut record, &peers, NOW + 10));
    assert!(record.holds(42, &sha, NOW + 10));
    assert_eq!(super::peer_records(vec![row(false)]).len(), 1);
}
