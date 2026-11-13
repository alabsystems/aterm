// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::paths::Staging;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn receipt_for(s: &Staging, build: u64, digest: &str) {
    crate::manifest::InstalledReceipt::record(
        &s.installed_receipt(),
        build,
        COMMIT,
        digest,
        aterm_durable::Flush::Durable,
    )
    .unwrap();
}

/// A START FAILURE NAMES EXACT BYTES AND READS CLOSED WHEN UNREADABLE: a record of `(11, d)`
/// names `d` in any case and neither another digest of 11 nor `d` under another build; a second
/// record replaces the one slot; a withdraw removes only the bytes it names; an installed build
/// is keyed by the receipt's digest for that build, or nothing is written or replaced, and a
/// withdraw of one the receipt does not name is a plain no (a person's own bundle, which no
/// record can name, is no error to warn of on every proven switch); a file that is not a record
/// reads as none, and one the read cannot get through is an error.
/// NEGATIVE CONTROL: matching by build alone makes `names(11, e)` hold; keying an installed
/// build by anything but its receipt fails the `Installed` arms; answering an unreceipted
/// activation's withdraw with an error fails "THE UNRECEIPTED ACTIVATION".
#[test]
fn a_start_failure_names_exact_bytes_and_reads_closed_when_unreadable() {
    let s = Staging::scratch("start-failure-key");
    let (d, e) = ("ab".repeat(32), "cd".repeat(32));
    let now = crate::install::unix_now_secs();
    assert_eq!(read(&s), Ok(None), "nothing recorded yet");
    record(
        &s,
        10,
        FailedStartBytes::Download {
            build: 11,
            dmg_sha256: &d,
        },
        "unexplained/stalled",
        now,
    )
    .unwrap();
    let on_record = read(&s).unwrap().expect("the record");
    assert_eq!(
        (
            on_record.build,
            on_record.dmg_sha256.as_str(),
            on_record.by_build
        ),
        (11, d.as_str(), 10)
    );
    assert_eq!(on_record.signature, "unexplained/stalled");
    assert_eq!(names(&s, 11, &d), Ok(true));
    assert_eq!(
        names(&s, 11, &d.to_ascii_uppercase()),
        Ok(true),
        "as a window spells it"
    );
    assert_eq!(names(&s, 11, &e), Ok(false), "another artifact of 11");
    assert_eq!(
        names(&s, 12, &d),
        Ok(false),
        "the digest under another build"
    );

    record(
        &s,
        10,
        FailedStartBytes::Download {
            build: 12,
            dmg_sha256: &e,
        },
        "exited-73",
        now,
    )
    .unwrap();
    assert_eq!(names(&s, 11, &d), Ok(false), "one slot: replaced");
    assert_eq!(names(&s, 12, &e), Ok(true));
    assert_eq!(
        withdraw(
            &s,
            FailedStartBytes::Download {
                build: 12,
                dmg_sha256: &d,
            }
        ),
        Ok(false),
        "other bytes withdraw nothing"
    );
    assert_eq!(names(&s, 12, &e), Ok(true));
    assert_eq!(
        withdraw(
            &s,
            FailedStartBytes::Download {
                build: 12,
                dmg_sha256: &e.to_ascii_uppercase(),
            }
        ),
        Ok(true)
    );
    assert_eq!(read(&s), Ok(None), "the hold is ended");

    receipt_for(&s, 11, &d);
    record(
        &s,
        10,
        FailedStartBytes::Installed { build: 11 },
        "stalled",
        now,
    )
    .unwrap();
    assert_eq!(
        read(&s).unwrap().map(|r| (r.build, r.dmg_sha256)),
        Some((11, d.clone())),
        "an activation is keyed by the receipt's digest, not one made up for it"
    );
    receipt_for(&s, 12, &e);
    assert!(
        record(
            &s,
            10,
            FailedStartBytes::Installed { build: 11 },
            "stalled",
            now
        )
        .is_err(),
        "the receipt names no artifact of 11"
    );
    assert_eq!(
        withdraw(&s, FailedStartBytes::Installed { build: 11 }),
        Ok(false),
        "a bundle the receipt does not name has no bytes on record: nothing is withdrawn, and \
         nothing went wrong (THE UNRECEIPTED ACTIVATION)"
    );
    assert_eq!(names(&s, 11, &d), Ok(true), "the prior record stands");
    std::fs::remove_file(s.installed_receipt()).unwrap();
    assert_eq!(
        withdraw(&s, FailedStartBytes::Installed { build: 11 }),
        Ok(false),
        "no receipt at all: the same (THE UNRECEIPTED ACTIVATION)"
    );
    assert_eq!(names(&s, 11, &d), Ok(true), "the prior record stands");

    std::fs::write(s.record(), "not = [a record").unwrap();
    assert_eq!(read(&s), Ok(None), "an unparseable file is no record");
    std::fs::remove_file(s.record()).unwrap();
    std::os::unix::fs::symlink(s.record(), s.record()).unwrap();
    assert!(read(&s).is_err(), "a file the read cannot open is an error");
    assert!(names(&s, 11, &d).is_err());
}

/// A START FAILURE IS AN EXPIRING HOLD OF THE WINDOWLESS LANES ONLY: the record keeps it beside
/// the one hold, as `start_failure` ([`Cause::StartFailed`], window witness, the window's build
/// as `by`). WINDOWS: the gate every lane asks does not hold it, the windowless gate does.
/// LAPSE: after the window's first span (4 h) it holds nothing, though the record still names
/// the bytes as evidence; a failure of them again is the next try, held a day. APART: a hold on
/// other bytes, live or lapsed, keeps its lineage and does not keep the booking out, and a
/// newer offer, a failed stage of a newer build, a newer build observed running and a peer's
/// proof of the failed bytes leave the booking and its evidence in place. RECOVERY: a person's
/// retry cuts it short, a crash-loop revert of the bytes (whose hold holds every lane) and a
/// window's proven start of them end it.
/// NEGATIVE CONTROLS: a `holds` that reads the start failure fails WINDOWS; a windowless gate
/// that ignores `until` fails LAPSE; a start failure kept in the hold slot fails APART.
#[test]
fn a_start_failure_is_an_expiring_hold_of_the_windowless_lanes_only() {
    use aterm_update_core::update_record::{
        CRASH_WINDOW_SPANS, Cause, Ending, Failure, Os, UpdateRecord, Witness,
    };
    let s = Staging::scratch("start-failure-hold");
    let d = "ab".repeat(32);
    let sha = crate::hold::sha(&d).unwrap();
    let now = crate::install::unix_now_secs();
    let bytes = FailedStartBytes::Download {
        build: 11,
        dmg_sha256: &d,
    };
    let on_record = |s: &Staging| UpdateRecord::read(&s.record()).unwrap().unwrap();

    record(&s, 10, bytes, "stalled", now).unwrap();
    let failed = on_record(&s).start_failure.unwrap();
    assert_eq!(
        (failed.cause, failed.witness, failed.by, failed.tries),
        (Cause::StartFailed, Witness::Window, 10, 1)
    );
    assert_eq!(failed.until, now + CRASH_WINDOW_SPANS[0]);
    assert_eq!(on_record(&s).hold, None, "APART: not the hold slot");
    assert!(
        !on_record(&s).holds(11, &sha, now),
        "WINDOWS: a window may try them"
    );
    assert!(on_record(&s).holds_windowless(11, &sha, now));
    assert_eq!(holds(&s, 11, &d), Ok(true));
    assert!(
        crate::hold::held(&s, 11, &d, now).is_none(),
        "WINDOWS: no gate of every lane"
    );
    assert_eq!(
        crate::HoldView::of(&on_record(&s), 10, now),
        None,
        "WINDOWS: no surface says the update is held while a window may install it"
    );

    let s2 = Staging::scratch("start-failure-hold-lapse");
    let lapsed_at = now - CRASH_WINDOW_SPANS[0] - 1;
    record(&s2, 10, bytes, "stalled", lapsed_at).unwrap();
    assert_eq!(holds(&s2, 11, &d), Ok(false), "LAPSE: it holds nothing");
    assert_eq!(names(&s2, 11, &d), Ok(true), "LAPSE: still the evidence");
    record(&s2, 10, bytes, "stalled again", now).unwrap();
    let again = on_record(&s2).start_failure.unwrap();
    assert_eq!(
        (again.tries, again.until),
        (2, now + CRASH_WINDOW_SPANS[1]),
        "LAPSE: the next try is held a day"
    );
    assert_eq!(holds(&s2, 11, &d), Ok(true));

    let failure = |build: u64, sha, cause| Failure {
        build,
        version: String::new(),
        sha,
        cause,
        witness: Witness::Headless,
        why: "failed".into(),
    };
    // A newer release appears while the failed bytes are still the stage: its offer, its
    // failed stage, its running elsewhere on this record and a peer's proof of the failed
    // bytes say nothing about why they failed to start here.
    let newer = crate::hold::sha(&"ef".repeat(32)).unwrap();
    UpdateRecord::edit(&s.record(), |r| {
        r.offer(12, now);
        r.set_hold(failure(12, newer, Cause::Stage), now);
        let mut peer = r.clone();
        peer.os = Os::here();
        peer.running = 11;
        peer.running_sha = Some(sha);
        peer.proven = true;
        r.peer_lift(&peer, now);
    })
    .unwrap();
    assert!(
        on_record(&s).holds_windowless(11, &sha, now),
        "APART: a newer offer, its failed stage and a peer's proof leave the booking"
    );
    assert_eq!(holds(&s, 11, &d), Ok(true));
    assert_eq!(names(&s, 11, &d), Ok(true), "APART: and its evidence");
    assert_eq!(on_record(&s).hold.unwrap().build, 12);
    UpdateRecord::edit(&s.record(), |r| r.observe_running(12, Some(newer), now)).unwrap();
    assert_eq!(names(&s, 11, &d), Ok(true), "APART: a newer build running");

    // A newer build's lapsed, escalated hold keeps its lineage when older bytes a window
    // launched fail to start, and the booking is made all the same.
    let s3 = Staging::scratch("start-failure-hold-apart");
    let long_ago = now - 2 * aterm_update_core::update_record::HOLD_MAX;
    UpdateRecord::edit(&s3.record(), |r| {
        for k in 0..3 {
            r.set_hold(
                failure(12, newer, Cause::CrashLoop),
                long_ago + k * aterm_update_core::update_record::HOLD_MAX / 2,
            );
        }
    })
    .unwrap();
    let lineage = on_record(&s3).hold.unwrap();
    assert_eq!(lineage.tries, 3);
    record(&s3, 10, bytes, "stalled", now).unwrap();
    assert_eq!(
        on_record(&s3).hold,
        Some(lineage.clone()),
        "APART: the newer build's lineage stands"
    );
    assert_eq!(
        holds(&s3, 11, &d),
        Ok(true),
        "APART: and the booking is made"
    );
    UpdateRecord::edit(&s3.record(), |r| {
        r.set_hold(failure(12, newer, Cause::CrashLoop), now)
    })
    .unwrap();
    assert_eq!(
        on_record(&s3).hold.unwrap().tries,
        4,
        "APART: its next failure still escalates"
    );
    // A live hold of every lane on other bytes does not keep a booking out either.
    let s4 = Staging::scratch("start-failure-hold-live-other");
    UpdateRecord::edit(&s4.record(), |r| {
        r.set_hold(failure(12, newer, Cause::Stage), now)
    })
    .unwrap();
    record(&s4, 10, bytes, "stalled", now).unwrap();
    assert_eq!(holds(&s4, 11, &d), Ok(true));
    assert!(on_record(&s4).holds(12, &newer, now));

    UpdateRecord::edit(&s2.record(), |r| r.end_hold(Ending::Person, now + 1)).unwrap();
    assert!(
        !on_record(&s2).holds_windowless(11, &sha, now + 1),
        "RECOVERY: a person's retry cuts it short"
    );
    assert_eq!(names(&s2, 11, &d), Ok(true), "RECOVERY: kept as history");

    let s5 = Staging::scratch("start-failure-hold-revert");
    record(&s5, 10, bytes, "stalled", now).unwrap();
    UpdateRecord::edit(&s5.record(), |r| {
        r.arm_trial(11, sha, Witness::Window, now);
        r.revert("v", now);
    })
    .unwrap();
    assert_eq!(
        read(&s5),
        Ok(None),
        "RECOVERY: the crash-loop revert's hold replaces it"
    );
    assert!(on_record(&s5).holds(11, &sha, now), "and holds every lane");

    UpdateRecord::edit(&s.record(), |r| r.confirm(11, sha, now)).unwrap();
    assert_eq!(
        read(&s),
        Ok(None),
        "RECOVERY: a proven start of them ends it"
    );
}
