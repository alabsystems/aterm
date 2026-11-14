// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{update_record_fields, update_record_line};
use aterm_update_core::update_record::{Cause, Failure, Sha, UpdateRecord, Witness};

fn status() -> aterm_update::UpdateStatus {
    aterm_update::UpdateStatus {
        enabled: true,
        installable: true,
        current_build: 10,
        staged_build: None,
        staged_version: None,
        staged_commit: None,
        staged_dmg_sha256: None,
        changelog: None,
        outcome: "behind: …".into(),
        updated_at: String::new(),
        failing_checks: 0,
        failing_kind: String::new(),
        failing_applies: 0,
        failing_since: String::new(),
        failing_persistent: false,
        failing_checks_kind: String::new(),
        channel_unreadable: false,
        hold: None,
        start_failure: None,
        last: None,
        offered: None,
        behind: false,
        linux: None,
    }
}

/// `update status` CARRIES THE RECORD: the newer offer, `behind=true`, and the hold's version,
/// build, cause word, lapse time, try count, whether it holds now, and its reason pct-encoded.
/// NEGATIVE CONTROL: a copy with no offer and no hold adds nothing to the line.
#[test]
fn the_status_line_names_the_offer_and_the_hold_by_field() {
    assert_eq!(update_record_fields(&status()), "");
    let mut st = status();
    st.offered = Some(11);
    st.behind = true;
    st.hold = Some(aterm_update::HoldView {
        build: 11,
        version: "0.11.0".into(),
        cause: Cause::CrashLoop,
        witness: Witness::Window,
        why: "no window answered".into(),
        tries: 2,
        until: 1_789_000_000,
        live: true,
        peer_lifted: false,
    });
    assert_eq!(
        update_record_fields(&st),
        " offered=11 behind=true hold=0.11.0 hold_build=11 cause=crash-loop until=1789000000 \
         tries=2 hold_live=true hold_why=no%20window%20answered"
    );
    st.offered = Some(10);
    st.behind = false;
    assert!(
        update_record_fields(&st).starts_with(" hold=0.11.0"),
        "an offer of the running build is no offer"
    );
}

/// `update holds` says one record per line: who, what it runs, whether proven, what it was
/// offered, and its hold, which reads as lapsed once its time has passed.
#[test]
fn a_holds_line_names_the_machine_its_build_and_its_hold() {
    let mut record = UpdateRecord {
        running: 10,
        proven: true,
        ..UpdateRecord::default()
    };
    assert_eq!(
        update_record_line("self", &record, 0),
        "machine=self running=10 proven=true offered=-"
    );
    record.offered = Some(11);
    assert!(record.set_hold(
        Failure {
            build: 11,
            version: String::new(),
            sha: Sha::parse(&"ab".repeat(32)).expect("a digest"),
            cause: Cause::Stage,
            witness: Witness::Headless,
            why: String::new(),
        },
        1_000,
    ));
    let until = record.hold.as_ref().expect("held").until;
    assert_eq!(
        update_record_line("a b", &record, 1_000),
        format!(
            "machine=a%20b running=10 proven=true offered=11 hold=- hold_build=11 cause=stage \
             until={until} tries=1 hold_live=true"
        )
    );
    assert!(
        update_record_line("a b", &record, until).ends_with("hold_live=false"),
        "a hold reads as lapsed at its time"
    );
}
