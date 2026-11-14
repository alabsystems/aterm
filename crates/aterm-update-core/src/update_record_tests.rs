// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

/// Every test that writes through [`UpdateRecord::edit`] holds this, so the one notify hook
/// the binary installs counts one test's writes at a time.
static EDITS: Mutex<()> = Mutex::new(());

fn edits() -> std::sync::MutexGuard<'static, ()> {
    EDITS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

const NOW: u64 = 1_800_000_000;
const PRED: u64 = 99;
const B1: u64 = 100;
const B2: u64 = 101;

fn sha(byte: u8) -> Sha {
    Sha([byte; 32])
}

/// The artifact of each build the conformance lattice uses; any other digest is `OTHER`.
fn sha_of(build: u64) -> Sha {
    sha(u8::try_from(build).unwrap())
}
const OTHER: Sha = Sha([0xee; 32]);

fn failure(build: u64, cause: Cause, witness: Witness) -> Failure {
    Failure {
        build,
        version: format!("0.{build}.0"),
        sha: sha_of(build),
        cause,
        witness,
        why: "why".into(),
    }
}

fn record() -> UpdateRecord {
    UpdateRecord {
        running: PRED,
        proven: true,
        offered: Some(B1),
        ..UpdateRecord::fresh()
    }
}

const H: u64 = 3600;

#[test]
fn each_cause_and_witness_takes_its_schedule() {
    use Cause::*;
    use Witness::*;
    let table: [(Cause, Witness, &[u64]); 8] = [
        (CrashLoop, Headless, &[H, 6 * H, 24 * H, 24 * H]),
        (CrashLoop, Linux, &[H, 6 * H, 24 * H]),
        (CrashLoop, Window, &[4 * H, 24 * H, 24 * H]),
        (Unproven, Headless, &[H, 6 * H, 24 * H]),
        (Unproven, Window, &[4 * H, 24 * H]),
        (LinuxRollback, Linux, &[H, 6 * H, 24 * H]),
        (Stage, Headless, &[900, H, 4 * H, 24 * H, 24 * H]),
        (StartFailed, Headless, &[4 * H, 24 * H, 24 * H]),
    ];
    for (cause, witness, spans) in table {
        for (i, span) in spans.iter().enumerate() {
            let tries = u8::try_from(i + 1).unwrap();
            assert_eq!(
                schedule_span(cause, witness, tries),
                *span,
                "{cause:?} seen by {witness:?}, try {tries}"
            );
            assert_eq!(hold_until(cause, witness, tries, NOW), NOW + span);
        }
        assert_eq!(schedule_span(cause, witness, u8::MAX), HOLD_MAX);
        assert_eq!(
            schedule_span(cause, witness, 0),
            schedule_span(cause, witness, 1),
            "a count of 0 reads as the first failure"
        );
    }
    assert_eq!(
        hold_until(Cause::Stage, Witness::Headless, 4, u64::MAX),
        u64::MAX
    );
}

#[test]
fn a_hold_holds_its_artifact_until_its_time_and_no_further_than_a_day() {
    let mut r = record();
    assert!(r.set_hold(failure(B1, Cause::CrashLoop, Witness::Headless), NOW));
    let hold = r.hold.clone().unwrap();
    assert_eq!((hold.tries, hold.span, hold.until), (1, H, NOW + H));
    assert!(r.holds(B1, &sha_of(B1), NOW));
    assert!(r.holds(B1, &sha_of(B1), NOW + H - 1));
    assert!(!r.holds(B1, &sha_of(B1), NOW + H), "it lapses at its until");
    assert!(
        !r.holds(B1, &OTHER, NOW),
        "another artifact of the build is not held"
    );
    assert!(!r.holds(B2, &sha_of(B1), NOW), "another build is not held");

    let mut ahead = r.clone();
    let stamped = ahead.hold.as_mut().unwrap();
    stamped.at = NOW + 10 * HOLD_MAX;
    stamped.until = stamped.at + H;
    assert!(
        !ahead.holds(B1, &sha_of(B1), NOW),
        "a hold reaching further ahead of the clock than HOLD_MAX holds nothing"
    );
    assert_eq!(ahead.hold.as_ref().unwrap().left(NOW), 0);
}

#[test]
fn a_read_clamps_whatever_was_written_to_the_laws() {
    let mut r = record();
    r.set_hold(failure(B1, Cause::CrashLoop, Witness::Headless), NOW);
    let hold = r.hold.as_mut().unwrap();
    hold.span = 30 * HOLD_MAX;
    hold.until = NOW + 30 * HOLD_MAX;
    hold.why = "é".repeat(WHY_MAX);
    r.trial = Some(TrialNote {
        build: B2,
        sha: sha_of(B2),
        since: NOW,
        witness: Witness::Window,
        misses: 2,
        why: "x".repeat(TRIAL_WHY_MAX + 9),
    });
    let read = UpdateRecord::decode(&r.encode()).unwrap();
    let hold = read.hold.as_ref().unwrap();
    assert_eq!(hold.span, HOLD_MAX);
    assert_eq!(hold.until, NOW + HOLD_MAX);
    assert!(hold.why.len() <= WHY_MAX && hold.why.chars().all(|c| c == 'é'));
    assert_eq!(read.trial.as_ref().unwrap().why.len(), TRIAL_WHY_MAX);
    assert!(read.holds(B1, &sha_of(B1), NOW + HOLD_MAX - 1));
    assert!(!read.holds(B1, &sha_of(B1), NOW + HOLD_MAX));

    let mut lifted = read.clone();
    let hold = lifted.hold.as_mut().unwrap();
    hold.until = hold.at;
    let text = lifted.encode();
    assert_eq!(
        UpdateRecord::decode(&text).unwrap(),
        lifted,
        "a settled record reads back as written"
    );

    let other = text.replacen("\"v\":1", "\"v\":2", 1);
    assert!(
        UpdateRecord::decode(&other).is_err(),
        "another version is unreadable"
    );
    assert!(UpdateRecord::decode("{").is_err());
}

#[test]
fn a_sha_is_64_hex_digits() {
    let s = Sha::parse(&"aB".repeat(32)).unwrap();
    assert_eq!(s, Sha([0xab; 32]));
    assert_eq!(s.hex(), "ab".repeat(32));
    assert_eq!(Sha::parse(&s.hex()), Some(s));
    assert_eq!(Sha::parse(&"ab".repeat(31)), None);
    assert_eq!(Sha::parse(&"zz".repeat(32)), None);
    assert_eq!(Sha::parse(&"é".repeat(32)), None);
}

#[test]
fn a_repeat_of_the_same_artifact_escalates_and_never_shortens() {
    let mut r = record();
    r.set_hold(failure(B1, Cause::CrashLoop, Witness::Window), NOW);
    assert_eq!(r.hold.as_ref().unwrap().span, 4 * H);
    assert!(
        !r.set_hold(failure(B1, Cause::Stage, Witness::Headless), NOW + 1),
        "another report of the artifact while it is held escalates nothing"
    );
    let later = NOW + 5 * H;
    assert!(!r.holds(B1, &sha_of(B1), later), "expired, kept as history");
    r.set_hold(failure(B1, Cause::Stage, Witness::Headless), later);
    let hold = r.hold.as_ref().unwrap();
    assert_eq!(hold.tries, 2);
    assert_eq!(
        hold.span,
        4 * H,
        "a stage failure's second span (1 h) does not shorten the crash's 4 h"
    );
    assert_eq!((hold.at, hold.until), (later, later + 4 * H));
    for _ in 0..300 {
        let lapsed = r.hold.as_ref().unwrap().until;
        assert!(r.set_hold(failure(B1, Cause::CrashLoop, Witness::Headless), lapsed));
    }
    let hold = r.hold.as_ref().unwrap();
    assert_eq!((hold.tries, hold.span), (u8::MAX, HOLD_MAX));

    let mut rebuilt = failure(B1, Cause::CrashLoop, Witness::Headless);
    rebuilt.sha = OTHER;
    assert!(
        r.set_hold(rebuilt, later),
        "another artifact replaces a live hold"
    );
    let hold = r.hold.as_ref().unwrap();
    assert_eq!(
        (hold.tries, hold.span, hold.sha),
        (1, H, OTHER),
        "another artifact of the same build starts its own lineage"
    );
}

#[test]
fn one_slot_a_newer_build_replaces_the_hold_and_an_older_one_changes_nothing() {
    let mut r = record();
    r.set_hold(failure(B1, Cause::CrashLoop, Witness::Headless), NOW);
    assert!(r.set_hold(failure(B2, Cause::Stage, Witness::Headless), NOW + 1));
    assert_eq!(r.hold.as_ref().unwrap().build, B2);
    assert_eq!(r.offered, Some(B2), "a held build was offered");
    assert_eq!(
        r.last,
        Some(Ended {
            build: B1,
            how: EndHow::Newer,
            at: NOW + 1
        })
    );
    let before = r.clone();
    assert!(!r.set_hold(failure(B1, Cause::CrashLoop, Witness::Headless), NOW + 2));
    assert_eq!(r, before, "an older build's failure is superseded");

    let mut offered = record();
    offered.offered = Some(B2);
    assert!(!offered.set_hold(failure(B1, Cause::CrashLoop, Witness::Headless), NOW));
    assert_eq!(
        offered.hold, None,
        "nothing holds a build below the newest offer"
    );
}

#[test]
fn the_endings() {
    let held = || {
        let mut r = record();
        r.set_hold(failure(B1, Cause::CrashLoop, Witness::Headless), NOW);
        r
    };

    let mut r = held();
    assert!(r.end_hold(Ending::Person, NOW + 1));
    assert!(
        !r.holds(B1, &sha_of(B1), NOW + 1),
        "a person's retry admits it"
    );
    assert_eq!(
        r.hold.as_ref().map(|h| (h.tries, h.until)),
        Some((1, NOW + 1)),
        "and keeps the hold as history"
    );
    assert_eq!(r.last.unwrap().how, EndHow::Person);
    assert!(!r.end_hold(Ending::Person, NOW + 2), "nothing left to end");
    r.set_hold(failure(B1, Cause::CrashLoop, Witness::Headless), NOW + 3);
    assert_eq!(
        r.hold.as_ref().map(|h| (h.tries, h.span)),
        Some((2, 6 * H)),
        "a failure after a person's retry escalates"
    );

    let mut r = held();
    assert!(!r.end_hold(Ending::Expired, NOW + H - 1), "still holds");
    assert!(r.end_hold(Ending::Expired, NOW + H));
    assert!(r.hold.is_some(), "an expired hold stays as history");
    assert_eq!(r.last.unwrap().how, EndHow::Expired);
    assert!(!r.end_hold(Ending::Expired, NOW + 2 * H), "noted once");
    r.set_hold(
        failure(B1, Cause::CrashLoop, Witness::Headless),
        NOW + 3 * H,
    );
    assert!(
        r.end_hold(Ending::Expired, NOW + 3 * H + 6 * H),
        "the next lapse is noted again"
    );

    let mut r = held();
    assert!(!r.offer(B1, NOW), "the held build itself is no newer offer");
    assert!(r.hold.is_some());
    assert!(r.offer(B2, NOW + 1));
    assert_eq!(r.hold, None);
    assert_eq!(r.last.unwrap().how, EndHow::Newer);
    assert!(!r.offer(B2, NOW + 2));

    let mut r = held();
    r.arm_trial(B1, sha_of(B1), Witness::Headless, NOW);
    assert!(r.confirm(B1, sha_of(B1), NOW + 1));
    assert_eq!(r.hold, None);
    assert_eq!(r.trial, None);
    assert_eq!(
        (r.running, r.running_sha, r.proven),
        (B1, Some(sha_of(B1)), true)
    );
    assert_eq!(r.last.unwrap().how, EndHow::Started);

    let mut r = held();
    r.offered = Some(B2);
    assert!(r.confirm(B2, sha_of(B2), NOW + 1));
    assert_eq!(r.last.unwrap().how, EndHow::Newer, "a newer build started");

    let mut r = held();
    r.offered = Some(B2);
    r.hold.as_mut().unwrap().build = B2;
    assert!(r.confirm(B1, sha_of(B1), NOW + 1));
    assert!(
        r.hold.is_some(),
        "a hold on a newer build outlives an older start"
    );
}

#[test]
fn peer_evidence_cuts_one_hold_of_an_artifact_short_once() {
    let mut r = record();
    r.set_hold(failure(B1, Cause::CrashLoop, Witness::Headless), NOW);
    let peer = UpdateRecord {
        running: B1,
        running_sha: Some(sha_of(B1)),
        proven: true,
        ..UpdateRecord::fresh()
    };
    for (what, bad) in [
        (
            "unproven",
            UpdateRecord {
                proven: false,
                ..peer.clone()
            },
        ),
        (
            "another build",
            UpdateRecord {
                running: B2,
                ..peer.clone()
            },
        ),
        (
            "another artifact",
            UpdateRecord {
                running_sha: Some(OTHER),
                ..peer.clone()
            },
        ),
        (
            "no digest",
            UpdateRecord {
                running_sha: None,
                ..peer.clone()
            },
        ),
        (
            "another system",
            UpdateRecord {
                os: if Os::here() == Os::Linux {
                    Os::Macos
                } else {
                    Os::Linux
                },
                ..peer.clone()
            },
        ),
        (
            "another processor",
            UpdateRecord {
                arch: if Arch::here() == Arch::X86_64 {
                    Arch::Aarch64
                } else {
                    Arch::X86_64
                },
                ..peer.clone()
            },
        ),
    ] {
        assert!(!r.clone().peer_lift(&bad, NOW + 1), "{what} is no evidence");
    }
    let mut other = r.clone();
    other.os = Os::Other;
    assert!(!other.peer_lift(
        &UpdateRecord {
            os: Os::Other,
            ..peer.clone()
        },
        NOW + 1
    ));

    assert!(r.peer_lift(&peer, NOW + 1));
    assert!(!r.holds(B1, &sha_of(B1), NOW + 1));
    assert_eq!(r.last.unwrap().how, EndHow::Peer);
    let hold = r.hold.clone().unwrap();
    assert!(hold.peer_lifted && hold.until == NOW + 1 && hold.span == H);

    r.set_hold(failure(B1, Cause::CrashLoop, Witness::Headless), NOW + 2);
    let hold = r.hold.clone().unwrap();
    assert!(hold.peer_lifted, "the lineage remembers the lift");
    assert_eq!((hold.tries, hold.span), (2, 6 * H));
    assert!(
        !r.peer_lift(&peer, NOW + 3),
        "a second lift of the artifact: the schedule"
    );
    assert!(r.holds(B1, &sha_of(B1), NOW + 3));

    r.end_hold(Ending::Person, NOW + 4);
    r.set_hold(failure(B1, Cause::CrashLoop, Witness::Headless), NOW + 5);
    assert!(
        !r.peer_lift(&peer, NOW + 6),
        "a person's retry keeps the lineage, and its one lift"
    );
    assert_eq!(r.hold.as_ref().map(|h| h.tries), Some(3));
}

#[test]
fn a_build_kept_unproven_keeps_its_lineage_so_a_window_crash_loop_escalates() {
    // A window re-trial crash-loops and reverts: held on the window's schedule.
    let mut r = record();
    r.arm_trial(B1, sha_of(B1), Witness::Window, NOW);
    r.revert("0.100.0", NOW);
    assert_eq!(r.hold.as_ref().map(|h| (h.tries, h.span)), Some((1, 4 * H)));
    // It lapses; a headless start settles a new trial of it keeping it, unproven.
    let later = NOW + 4 * H;
    r.arm_trial(B1, sha_of(B1), Witness::Headless, later);
    assert!(r.keep_unproven(B1, None));
    assert_eq!(
        (r.running, r.running_sha, r.proven, r.trial.is_none()),
        (B1, Some(sha_of(B1)), false, true)
    );
    assert!(
        r.hold.is_some(),
        "a headless settle proves nothing: the lineage stays"
    );
    // A window launch crash-loops again: the second try, a day.
    r.arm_trial(B1, sha_of(B1), Witness::Window, later + 60);
    r.revert("0.100.0", later + 120);
    assert_eq!(
        r.hold.as_ref().map(|h| (h.tries, h.span)),
        Some((2, HOLD_MAX))
    );
}

#[test]
fn a_build_found_running_is_not_proven_by_being_found() {
    let mut r = record();
    r.set_hold(failure(B1, Cause::CrashLoop, Witness::Headless), NOW);
    assert!(r.observe_running(B1, Some(sha_of(B1)), NOW + 1));
    assert_eq!((r.running, r.proven), (B1, false));
    assert!(r.hold.is_some(), "a hold on the build found running stays");
    assert!(
        !r.observe_running(B1, Some(sha_of(B1)), NOW + 2),
        "already noted"
    );
    r.offered = Some(B2);
    assert!(r.observe_running(B2, Some(sha_of(B2)), NOW + 3));
    assert_eq!(r.hold, None, "a hold on an older build ends");
    assert_eq!(r.last.unwrap().how, EndHow::Newer);
}

#[test]
fn a_trial_carries_its_witness_and_reason_into_the_hold() {
    let mut r = record();
    assert!(r.arm_trial(B1, sha_of(B1), Witness::Headless, NOW));
    assert!(
        !r.arm_trial(B1, sha_of(B1), Witness::Headless, NOW + 1),
        "already armed"
    );
    assert!(r.note_miss("no window answered"));
    assert!(r.arm_trial(B1, sha_of(B1), Witness::Window, NOW + 2));
    assert!(
        !r.arm_trial(B1, sha_of(B1), Witness::Headless, NOW + 3),
        "a window is not lowered"
    );
    let trial = r.trial.clone().unwrap();
    assert_eq!(
        (trial.since, trial.witness, trial.misses, trial.why.as_str()),
        (NOW, Witness::Window, 1, "no window answered")
    );
    assert!(r.note_miss(&"y".repeat(500)));
    assert_eq!(r.trial.as_ref().unwrap().why.len(), TRIAL_WHY_MAX);
    assert!(r.revert("0.100.0", NOW + 4));
    assert_eq!(r.trial, None);
    assert_eq!(r.running, PRED, "the revert restored the build that ran");
    let hold = r.hold.clone().unwrap();
    assert_eq!(
        (
            hold.cause,
            hold.witness,
            hold.span,
            hold.version.as_str(),
            hold.why.len()
        ),
        (
            Cause::CrashLoop,
            Witness::Window,
            4 * H,
            "0.100.0",
            TRIAL_WHY_MAX
        )
    );
    assert!(!r.revert("0.100.0", NOW + 5), "no trial, nothing to revert");
    assert!(!r.note_miss("x"));

    let mut quiet = record();
    quiet.arm_trial(B1, sha_of(B1), Witness::Headless, NOW);
    quiet.revert("0.100.0", NOW);
    assert_eq!(quiet.hold.unwrap().why, CRASH_WHY);

    let mut kept = record();
    kept.arm_trial(B1, sha_of(B1), Witness::Headless, NOW);
    assert!(kept.unproven("0.100.0", NOW));
    assert_eq!((kept.running, kept.proven), (B1, false));
    assert_eq!(kept.hold.unwrap().cause, Cause::Unproven);

    let mut other = record();
    other.arm_trial(B1, sha_of(B1), Witness::Window, NOW);
    other.arm_trial(B2, sha_of(B2), Witness::Headless, NOW + 1);
    let trial = other.trial.unwrap();
    assert_eq!(
        (trial.build, trial.witness, trial.since),
        (B2, Witness::Headless, NOW + 1)
    );
}

#[test]
fn an_edit_is_durable_bumps_n_and_writes_only_a_change() {
    use aterm_durable::seam::Recording;
    let _serial = edits();
    let dir = crate::test_scratch_dir("record-edit");
    let path = dir.path().join("record.json");
    assert_eq!(UpdateRecord::read(&path).unwrap(), None);
    let recording = Recording::start();
    let (r, changed) = UpdateRecord::edit(&path, |r| r.offer(B1, NOW)).unwrap();
    assert!(changed);
    assert_eq!(
        recording.take(),
        vec![aterm_durable::Flush::Durable; 2],
        "the file and its folder"
    );
    assert!(
        r.n >= aterm_time::unix_secs().unwrap() - 60,
        "a new record starts from the clock"
    );
    assert_eq!(UpdateRecord::read(&path).unwrap(), Some(r.clone()));
    let (same, changed) = UpdateRecord::edit(&path, |r| r.offer(B1, NOW)).unwrap();
    assert!(!changed);
    assert_eq!(same, r, "nothing changed, nothing written");
    assert!(recording.take().is_empty());
    let (next, _) = UpdateRecord::edit(&path, |r| r.offer(B2, NOW)).unwrap();
    assert!(next.n > r.n);
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn an_unreadable_record_holds_nothing_and_the_next_edit_replaces_it() {
    let _serial = edits();
    let dir = crate::test_scratch_dir("record-corrupt");
    let path = dir.path().join("record.json");
    let mut held = record();
    held.set_hold(failure(B1, Cause::CrashLoop, Witness::Headless), NOW);
    let text = held.encode();
    std::fs::write(&path, &text[..text.len() / 2]).unwrap();
    assert_eq!(
        UpdateRecord::read(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let loaded = UpdateRecord::load(&path);
    assert_eq!(loaded.hold, None, "fails open");
    let (r, ()) = UpdateRecord::edit(&path, |_| ()).unwrap();
    assert_eq!(r.hold, None);
    assert_eq!(
        UpdateRecord::read(&path).unwrap(),
        Some(r),
        "rewritten though nothing changed"
    );
}

#[test]
fn concurrent_successful_edits_lose_nothing_under_the_lock() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let _serial = edits();
    let dir = crate::test_scratch_dir("record-race");
    let path = dir.path().join("record.json");
    const WRITERS: u64 = 4;
    const EACH: u64 = 25;
    let committed = AtomicU64::new(0);
    std::thread::scope(|s| {
        for _ in 0..WRITERS {
            s.spawn(|| {
                for _ in 0..EACH {
                    match UpdateRecord::edit(&path, |r| r.offer(r.offered.unwrap_or(0) + 1, NOW)) {
                        Ok(_) => {
                            committed.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                        Err(error) => panic!("record edit failed: {error}"),
                    }
                }
            });
        }
    });
    let committed = committed.load(Ordering::Relaxed);
    assert!(committed > 0, "at least one edit must commit");
    let r = UpdateRecord::read(&path).unwrap().unwrap();
    assert_eq!(
        r.offered,
        Some(committed),
        "every successful read-modify-write landed; timed-out lock acquisitions wrote nothing"
    );
}

#[test]
fn a_hold_an_ending_or_a_miss_nudges_the_publisher_and_nothing_else_does() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NUDGES: AtomicUsize = AtomicUsize::new(0);
    let _serial = edits();
    assert!(install_notify(Box::new(|| {
        NUDGES.fetch_add(1, Ordering::SeqCst);
    })));
    assert!(!install_notify(Box::new(|| {})), "the first install wins");
    let dir = crate::test_scratch_dir("record-notify");
    let path = dir.path().join("record.json");
    let nudged = |change: &dyn Fn(&mut UpdateRecord)| {
        let before = NUDGES.load(Ordering::SeqCst);
        UpdateRecord::edit(&path, |r| change(r)).unwrap();
        NUDGES.load(Ordering::SeqCst) - before
    };
    assert_eq!(
        nudged(&|r| {
            r.offer(B1, NOW);
        }),
        0
    );
    assert_eq!(
        nudged(&|r| {
            r.arm_trial(B1, sha_of(B1), Witness::Headless, NOW);
        }),
        0
    );
    assert_eq!(
        nudged(&|r| {
            r.note_miss("no listener");
        }),
        1
    );
    assert_eq!(
        nudged(&|r| {
            r.revert("0.100.0", NOW);
        }),
        1
    );
    assert_eq!(
        nudged(&|r| {
            r.end_hold(Ending::Person, NOW);
        }),
        1
    );
    assert_eq!(nudged(&|_| {}), 0);
}

/// Every concrete state of the conformance lattice: builds PRED (model 0), B1 (1), B2 (2), each
/// artifact `sha_of(build)`, none trialled or held above the offer, a trial only of a build
/// newer than the one running and never of one a hold still holds (the gate refuses it);
/// every hold span the schedules write, of each schedule, every class of the time left,
/// lifted or not.
fn lattice() -> Vec<UpdateRecord> {
    let spans = [900, H, 4 * H, 6 * H, HOLD_MAX];
    let mut holds: Vec<Option<Hold>> = vec![None];
    let kinds = [
        (Cause::CrashLoop, Witness::Headless),
        (Cause::CrashLoop, Witness::Window),
        (Cause::Stage, Witness::Headless),
    ];
    for build in [B1, B2] {
        for (cause, witness) in kinds {
            for tries in 1..=5u8 {
                for span in spans {
                    for elapsed in [
                        0,
                        H,
                        span.saturating_sub(6 * H),
                        span.saturating_sub(1800),
                        span,
                        span + H,
                    ] {
                        for peer_lifted in [false, true] {
                            let at = NOW - elapsed;
                            holds.push(Some(Hold {
                                build,
                                version: format!("0.{build}.0"),
                                sha: sha_of(build),
                                cause,
                                witness,
                                why: String::new(),
                                at,
                                tries,
                                span,
                                until: at + span,
                                peer_lifted,
                                by: 0,
                            }));
                        }
                    }
                }
            }
        }
    }
    let mut trials = vec![None];
    for build in [B1, B2] {
        for witness in [Witness::Headless, Witness::Window] {
            trials.push(Some(TrialNote {
                build,
                sha: sha_of(build),
                since: NOW - 10,
                witness,
                misses: 0,
                why: String::new(),
            }));
        }
    }
    let mut out = Vec::new();
    for offered in [B1, B2] {
        for running in [PRED, B1, B2].into_iter().filter(|b| *b <= offered) {
            for proven in [false, true] {
                let trialled = |t: &&Option<TrialNote>| {
                    t.as_ref()
                        .is_none_or(|t| running < t.build && t.build <= offered)
                };
                for trial in trials.iter().filter(trialled) {
                    let kept = |h: &&Option<Hold>| {
                        h.as_ref().is_none_or(|h| {
                            h.build <= offered
                                && !(h.live(NOW)
                                    && trial.as_ref().is_some_and(|t| t.build == h.build))
                        })
                    };
                    for hold in holds.iter().filter(kept) {
                        out.push(UpdateRecord {
                            running,
                            running_sha: Some(sha_of(running)),
                            proven,
                            offered: Some(offered),
                            trial: trial.clone(),
                            hold: hold.clone(),
                            ..UpdateRecord::fresh()
                        });
                    }
                }
            }
        }
    }
    out
}

fn project(r: &UpdateRecord, probe: Option<(i64, bool)>) -> BTreeMap<&'static str, i64> {
    hold_projection(r, B1, NOW, probe)
}

fn model_class(build: u64) -> i64 {
    project(
        &UpdateRecord {
            running: build,
            ..UpdateRecord::fresh()
        },
        None,
    )["running"]
}

/// What one real call did, as the model's state after it, for the action it performs.
type Step = (&'static str, BTreeMap<&'static str, i64>);

/// Each real writer and reader the model names, called on `r` where the model's action is
/// enabled from `r`'s projection.
fn real_steps(r: &UpdateRecord) -> Vec<Step> {
    let offered = r.offered.unwrap();
    let after = |f: &dyn Fn(&mut UpdateRecord)| {
        let mut next = r.clone();
        f(&mut next);
        project(&next, None)
    };
    let mut steps: Vec<Step> = vec![
        (
            "Stage",
            after(&|n| {
                n.arm_trial(offered, sha_of(offered), Witness::Headless, NOW);
            }),
        ),
        (
            "Revert",
            after(&|n| {
                n.revert("v", NOW);
            }),
        ),
        (
            "Unproven",
            after(&|n| {
                n.unproven("v", NOW);
            }),
        ),
        (
            "StageFail",
            after(&|n| {
                n.set_hold(failure(offered, Cause::Stage, Witness::Headless), NOW);
            }),
        ),
        (
            "OfferNewer",
            after(&|n| {
                n.offer(B2, NOW);
            }),
        ),
        (
            "PersonRetry",
            after(&|n| {
                n.end_hold(Ending::Person, NOW);
            }),
        ),
        (
            "Observe",
            after(&|n| {
                n.observe_running(offered, Some(sha_of(offered)), NOW);
            }),
        ),
        (
            "Probe",
            project(
                r,
                Some((
                    model_class(offered),
                    r.holds(offered, &sha_of(offered), NOW),
                )),
            ),
        ),
        (
            "ProbeOther",
            project(r, Some((3, r.holds(offered, &OTHER, NOW)))),
        ),
    ];
    if let Some(trial) = &r.trial {
        let (build, sha) = (trial.build, trial.sha);
        steps.push((
            "Confirm",
            after(&|n| {
                n.confirm(build, sha, NOW);
            }),
        ));
        steps.push((
            "KeepUnproven",
            after(&|n| {
                n.keep_unproven(build, None);
            }),
        ));
        steps.push((
            "WindowLaunch",
            after(&|n| {
                n.arm_trial(build, sha, Witness::Window, NOW);
            }),
        ));
    }
    if let Some(hold) = &r.hold {
        let peer = UpdateRecord {
            running: hold.build,
            running_sha: Some(hold.sha),
            proven: true,
            ..UpdateRecord::fresh()
        };
        steps.push((
            "PeerProven",
            after(&|n| {
                n.peer_lift(&peer, NOW);
            }),
        ));
    }
    steps
}

/// Tier 1: from every lattice state that keeps the model's laws, each real call the model
/// allows there is one of its action's successors and keeps every law; the arm a window saw
/// is `Stage` then `WindowLaunch`, and an `Expired` ending is a stutter. The lattice's
/// projections cover every state the model reaches between probes.
#[test]
fn the_update_record_refines_native_update_hold() {
    let model = aterm_models_update::native_update_hold_model();
    let lawful = |s: &BTreeMap<&'static str, i64>| {
        model
            .invariants
            .iter()
            .all(|inv| model.check_invariant(inv.name, s))
    };
    let mut covered: BTreeSet<BTreeMap<&'static str, i64>> = BTreeSet::new();
    let mut fired: BTreeMap<&'static str, usize> = BTreeMap::new();
    for r in lattice() {
        let before = project(&r, None);
        if !lawful(&before) {
            continue;
        }
        covered.insert(before.clone());
        for (action, after) in real_steps(&r) {
            let successors = model.successors(action, &before);
            if successors.is_empty() {
                continue;
            }
            assert!(
                successors.contains(&after),
                "{action} from {before:?} (record {r:?}) gave {after:?}, which the model does \
                 not: {successors:?}"
            );
            assert!(lawful(&after), "{action} broke a law: {after:?}");
            *fired.entry(action).or_default() += 1;
        }
        if r.trial.is_none() && !model.successors("Stage", &before).is_empty() {
            let offered = r.offered.unwrap();
            let mut seen = r.clone();
            seen.arm_trial(offered, sha_of(offered), Witness::Window, NOW);
            let after = project(&seen, None);
            let staged = model.successors("Stage", &before);
            assert!(
                staged
                    .iter()
                    .any(|mid| model.successors("WindowLaunch", mid).contains(&after)),
                "a trial a window launched from {before:?} gave {after:?}"
            );
        }
        if r.hold.as_ref().is_some_and(|h| !h.live(NOW)) {
            let mut lapsed = r.clone();
            assert!(lapsed.end_hold(Ending::Expired, NOW));
            assert_eq!(
                project(&lapsed, None),
                before,
                "noting a lapse is a stutter"
            );
        }
    }
    for action in [
        "Stage",
        "WindowLaunch",
        "Confirm",
        "Revert",
        "Unproven",
        "StageFail",
        "OfferNewer",
        "PeerProven",
        "PersonRetry",
        "KeepUnproven",
        "Observe",
        "Probe",
        "ProbeOther",
    ] {
        assert!(
            fired.get(action).copied().unwrap_or(0) > 0,
            "{action} never checked"
        );
    }
    let reachable: BTreeSet<_> = aterm_spec::interp::reachable_states(&model)
        .into_iter()
        .filter(|s| s["pb"] == 0)
        .collect();
    let missed: Vec<_> = reachable.difference(&covered).collect();
    assert!(
        missed.is_empty(),
        "{} reachable model state(s) no lattice state projects to, e.g. {:?}",
        missed.len(),
        missed.first()
    );
}

/// Validate one transition at both tiers, as the label says.
fn admits(
    model: &aterm_spec::derive::Model,
    before: &BTreeMap<&'static str, i64>,
    after: &BTreeMap<&'static str, i64>,
    action: &str,
    label: &str,
) -> bool {
    aterm_spec::verify::validate_transition_tiered(model, &[], before, after, Some(action), label).0
}

/// `state` with the model's ghosts (`fails`, `peak`) and the lift count left out, which a
/// projection of a real record cannot carry apart from what they shadow.
fn observable(state: &BTreeMap<&'static str, i64>) -> BTreeMap<&'static str, i64> {
    let mut s = state.clone();
    s.remove("fails");
    s.remove("peak");
    s.remove("lifts");
    s
}

/// Negative controls: each defect the model's mutants stand for, written into a record, is a
/// transition the healthy model refuses and its mutant performs, breaking the law it names.
#[test]
fn native_update_hold_negative_controls_are_non_vacuous() {
    let model = aterm_models_update::native_update_hold_model();
    let mut held = record();
    held.set_hold(
        failure(B1, Cause::CrashLoop, Witness::Headless),
        NOW - 2 * H,
    );
    let expired = project(&held, None);

    // A gate that ignores `until`: the incident's permanent hold.
    let ignoring_until = project(&held, Some((1, true)));
    assert!(!admits(
        &model,
        &expired,
        &ignoring_until,
        "Probe",
        "hold: gate ignores until"
    ));
    let buggy = aterm_spec::interp::with_buggy(&model, 2);
    assert!(
        buggy
            .successors("Probe", &expired)
            .contains(&ignoring_until)
    );
    assert!(!buggy.check_invariant("SuppressIffLive", &ignoring_until));

    // A newer offer that keeps the hold.
    let mut kept = held.clone();
    kept.offered = Some(B2);
    let kept = project(&kept, None);
    assert!(!admits(
        &model,
        &expired,
        &kept,
        "OfferNewer",
        "hold: newer offer keeps it"
    ));
    let buggy = aterm_spec::interp::with_buggy(&model, 3);
    assert!(buggy.successors("OfferNewer", &expired).contains(&kept));
    assert!(!buggy.check_invariant("NewerEndsHold", &kept));

    // A clean start that keeps the hold.
    let mut trial = held.clone();
    trial.arm_trial(B1, sha_of(B1), Witness::Headless, NOW);
    let trialled = project(&trial, None);
    let mut started = trial.clone();
    started.running = B1;
    started.trial = None;
    let started = project(&started, None);
    assert!(!admits(
        &model,
        &trialled,
        &started,
        "Confirm",
        "hold: start keeps it"
    ));
    let buggy = aterm_spec::interp::with_buggy(&model, 4);
    assert!(buggy.successors("Confirm", &trialled).contains(&started));
    assert!(!buggy.check_invariant("StartEndsHold", &started));

    // A repeat failure that starts the count over.
    let mut reset = trial.clone();
    reset.trial = None;
    let hold = reset.hold.as_mut().unwrap();
    hold.at = NOW;
    hold.until = NOW + hold.span;
    let reset = project(&reset, None);
    assert!(!admits(
        &model,
        &trialled,
        &reset,
        "Revert",
        "hold: repeat resets tries"
    ));
    let buggy = aterm_spec::interp::with_buggy(&model, 5);
    let broken: Vec<_> = buggy
        .successors("Revert", &trialled)
        .into_iter()
        .filter(|s| observable(s) == observable(&reset))
        .collect();
    assert!(!broken.is_empty());
    assert!(
        broken
            .iter()
            .all(|s| !buggy.check_invariant("RetryEscalates", s))
    );

    // Peer evidence that lifts the same artifact's hold a second time: the E2 loop.
    let mut relifted = held.clone();
    let hold = relifted.hold.as_mut().unwrap();
    hold.peer_lifted = true;
    hold.at = NOW;
    hold.until = NOW + hold.span;
    let live_lifted = project(&relifted, None);
    relifted.hold.as_mut().unwrap().until = NOW;
    let lifted_twice = project(&relifted, None);
    assert!(!admits(
        &model,
        &live_lifted,
        &lifted_twice,
        "PeerProven",
        "hold: second lift"
    ));
    let buggy = aterm_spec::interp::with_buggy(&model, 6);
    let broken: Vec<_> = buggy
        .successors("PeerProven", &live_lifted)
        .into_iter()
        .filter(|s| observable(s) == observable(&lifted_twice))
        .collect();
    assert!(!broken.is_empty());
    assert!(
        broken
            .iter()
            .all(|s| !buggy.check_invariant("PeerLiftOnce", s))
    );

    // A retry that forgets the lineage's count, so a repeat failure starts its schedule over.
    let mut live = held.clone();
    let hold = live.hold.as_mut().unwrap();
    hold.at = NOW;
    hold.until = NOW + hold.span;
    let live = project(&live, None);
    let mut forgot = held.clone();
    let hold = forgot.hold.as_mut().unwrap();
    hold.at = NOW;
    hold.until = NOW;
    let mut forgot = project(&forgot, None);
    forgot.insert("tries", 0);
    assert!(!admits(
        &model,
        &live,
        &forgot,
        "PersonRetry",
        "hold: retry forgets tries"
    ));
    let buggy = aterm_spec::interp::with_buggy(&model, 7);
    assert!(buggy.successors("PersonRetry", &live).contains(&forgot));
    assert!(!buggy.check_invariant("OneHold", &forgot));

    // A stored span shorter than its schedule asks at the stored count: the projection itself
    // breaks `RetryEscalates`, so the law is not vacuous on code.
    let mut short = held.clone();
    let hold = short.hold.as_mut().unwrap();
    hold.tries = 2;
    hold.span = H;
    hold.at = NOW;
    hold.until = NOW + H;
    assert!(!model.check_invariant("RetryEscalates", &project(&short, None)));
}

#[test]
fn a_hold_is_named_and_timed_in_a_persons_words() {
    let mut hold = record();
    hold.set_hold(failure(B1, Cause::CrashLoop, Witness::Headless), NOW);
    let mut hold = hold.hold.unwrap();
    assert_eq!(hold.name(), "0.100.0");
    hold.version.clear();
    assert_eq!(hold.name(), "build 100", "never a blank name");
    // 2026-11-12 18:40 UTC; an hour east of UTC it reads 19:40 local.
    let day = 1_794_441_600;
    let at = day + 18 * H + 40 * 60;
    assert_eq!(clock_words_in(at, at - 600, &|_| Some(0)), "18:40");
    assert_eq!(clock_words_in(at, at - 600, &|_| Some(3600)), "19:40");
    assert_eq!(clock_words_in(at, at - 600, &|_| None), "18:40 UTC");
    assert_eq!(
        clock_words_in(day + 24 * H + 9 * H, day + 20 * H, &|_| Some(0)),
        "tomorrow 09:00"
    );
    assert_eq!(left_words(30), "under a minute");
    assert_eq!(left_words(90 * 60), "90 min");
    assert_eq!(left_words(5 * H + 1), "6 h");
}
