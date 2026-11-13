// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The lanes' durable state, holds and reports, over typed data: the floors, the
//! attempts, the lock, the once-a-day rule, the coherence hold.

use std::collections::BTreeMap;

use atpkg_publish::lane::LaneKind;
use atpkg_publish::lane::hold::{Holds, of_members};
use atpkg_publish::lane::report::{self, Recorded, Transcript};
use atpkg_publish::lane::state::{EVIDENCE_KEEP_SECS, LaneState, Lock, PRUNE_AFTER_SECS};
use atpkg_publish::testkit::NOW;
use atpkg_publish::triple::{
    AARCH64_APPLE_DARWIN, AARCH64_UNKNOWN_LINUX_GNU, X86_64_APPLE_DARWIN, X86_64_UNKNOWN_LINUX_GNU,
};

use super::super::shared::scratch;

fn pins(rows: &[(&str, u64)]) -> BTreeMap<String, u64> {
    rows.iter().map(|(p, b)| ((*p).to_string(), *b)).collect()
}

#[test]
fn each_lane_keeps_its_floors_where_its_shell_lane_did() {
    let root = std::path::Path::new("/s");
    let a = LaneState::of(LaneKind::Alab, root);
    assert_eq!(a.index_seen, root.join("alab/index-seen"));
    assert_eq!(a.lock, root.join("alab.lockf"));
    let l = LaneState::of(LaneKind::Linux, root);
    assert_eq!(l.index_seen, root.join("atpkg-index-seen"));
    assert_eq!(l.pins_seen, root.join("atpkg-pins-seen"));
    assert_eq!(l.lock, root.join("atpkg.lock"));
}

#[test]
fn ready_notification_handoff_waits_for_the_stager_and_rejects_malformed_state() {
    let tmp = scratch("ready-handoff");
    let state = LaneState::of(LaneKind::Linux, tmp.path());
    state.ensure().unwrap();
    let Lock::Held(guard) = state.try_lock().unwrap() else {
        panic!("own lock")
    };
    state.notify_ready().unwrap();
    let waiter = state.clone();
    let thread = std::thread::spawn(move || {
        let Lock::Held(_guard) = waiter.queued_lock().unwrap() else {
            panic!("staging handoff")
        };
        assert!(waiter.ready_pending().unwrap());
        waiter.clear_ready().unwrap();
    });
    assert!(matches!(state.try_lock().unwrap(), Lock::Busy));
    drop(guard);
    thread.join().unwrap();
    assert!(!state.ready_pending().unwrap());
    state.notify_ready().unwrap();
    std::fs::write(state.ready_dir().join("pending"), b"not a queue\n").unwrap();
    assert!(state.ready_pending().is_err());
    #[cfg(unix)]
    {
        std::fs::remove_file(state.ready_dir().join("pending")).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("absent"), state.ready_dir().join("pending"))
            .unwrap();
        assert!(state.ready_pending().is_err());
    }
}

#[test]
fn a_floor_that_cannot_be_read_is_never_read_as_zero() {
    let tmp = scratch("state-floor");
    let s = LaneState::of(LaneKind::Alab, tmp.path());
    s.ensure().expect("state");
    assert_eq!(s.index_seen().expect("absent is 0"), 0);
    std::fs::write(&s.index_seen, "six\n").expect("write");
    let e = s.index_seen().expect_err("garbage refuses");
    assert_eq!(e.exit_code(), 1);
    assert!(s.check_index(9).is_err());
}

#[test]
fn the_pins_floor_refuses_a_fallen_pin_and_warns_of_a_vanished_one() {
    let tmp = scratch("state-pins");
    let s = LaneState::of(LaneKind::Linux, tmp.path());
    s.ensure().expect("state");
    let targets = [X86_64_UNKNOWN_LINUX_GNU, AARCH64_UNKNOWN_LINUX_GNU];
    s.record(7, &targets, &pins(&[("ay", 10), ("clean", 20)]))
        .expect("record");
    let tx = Transcript::memory("t");
    s.check_pins(&targets, &pins(&[("ay", 11), ("clean", 20)]), 8, &tx)
        .expect("a rise passes");
    s.check_pins(&targets, &pins(&[("ay", 11)]), 8, &tx)
        .expect("a vanished program warns");
    assert!(
        tx.text().contains(
            "program clean was pinned when this lane last ran and is ABSENT from index-8"
        )
    );
    let e = s
        .check_pins(&targets, &pins(&[("ay", 9), ("clean", 20)]), 8, &tx)
        .expect_err("a fall refuses");
    assert!(
        e.to_string()
            .contains("PIN DOWNGRADE on the channel: ay 10 -> 9 in index-8"),
        "{e}"
    );
    let tx = Transcript::memory("t");
    s.check_pins(&[X86_64_UNKNOWN_LINUX_GNU], &pins(&[("ay", 1)]), 8, &tx)
        .expect("restamped, not believed");
    assert!(
        tx.text()
            .contains("atpkg-pins-seen was not recorded for x86_64-unknown-linux-gnu")
    );
    for malformed in [
        "# pins-resolved-for x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu\nay\n",
        "# pins-resolved-for x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu\nay 10 extra\n",
    ] {
        std::fs::write(&s.pins_seen, malformed).expect("write malformed pins_seen");
        let err = s
            .check_pins(&targets, &pins(&[("ay", 11)]), 8, &tx)
            .expect_err("truncated or trailing-token pin line must refuse");
        assert!(err.to_string().contains("unreadable line"), "{err}");
    }
}

#[test]
fn the_shell_apple_lanes_unstamped_pins_still_floor_the_apple_pins() {
    let tmp = scratch("state-shell-pins");
    let s = LaneState::of(LaneKind::Alab, tmp.path());
    s.ensure().expect("state");
    std::fs::write(&s.pins_seen, "ay 3\nclean 20\n").expect("write");
    let tx = Transcript::memory("t");
    let e = s
        .check_pins(
            atpkg_publish::lane::alab::TARGETS,
            &pins(&[("ay", 2), ("clean", 20)]),
            9,
            &tx,
        )
        .expect_err("the first Rust run keeps the floor");
    assert!(
        e.to_string()
            .contains("PIN DOWNGRADE on the channel: ay 3 -> 2 in index-9"),
        "{e}"
    );
    assert!(!tx.text().contains("was not recorded"), "{}", tx.text());
    let l = LaneState::of(LaneKind::Linux, tmp.path());
    std::fs::write(&l.pins_seen, "ay 3\n").expect("write");
    let targets = [X86_64_UNKNOWN_LINUX_GNU];
    l.check_pins(&targets, &pins(&[("ay", 2)]), 9, &tx)
        .expect("retired, not believed");
}

#[test]
fn attempts_count_up_clear_on_success_and_age_out() {
    let tmp = scratch("state-attempts");
    let s = LaneState::of(LaneKind::Alab, tmp.path());
    s.ensure().expect("state");
    assert_eq!(s.bump_attempts("ay", 4), 1);
    assert_eq!(s.bump_attempts("ay", 4), 2);
    s.clear_attempts("ay", 4);
    assert_eq!(s.bump_attempts("ay", 4), 1);
    let file = s.attempts_file("ay", 4);
    let set_mtime = |unix_secs: i64| {
        let stamp = std::time::UNIX_EPOCH
            + std::time::Duration::from_secs(u64::try_from(unix_secs).expect("non-negative"));
        std::fs::File::options()
            .write(true)
            .open(&file)
            .and_then(|f| f.set_modified(stamp))
            .expect("age the counter");
    };
    set_mtime(NOW - PRUNE_AFTER_SECS);
    s.prune(NOW);
    assert!(
        file.exists(),
        "an entry aged exactly PRUNE_AFTER_SECS survives"
    );
    set_mtime(NOW - PRUNE_AFTER_SECS - 1);
    s.prune(NOW);
    assert!(
        !file.exists(),
        "an entry one second past PRUNE_AFTER_SECS is pruned"
    );
}

#[test]
fn raising_the_floors_never_lowers_what_another_writer_left() {
    let tmp = scratch("state-raise");
    let s = LaneState::of(LaneKind::Tip, tmp.path());
    s.ensure().expect("state");
    let arm = [AARCH64_APPLE_DARWIN];
    let seen = |s: &LaneState| {
        std::fs::read_to_string(&s.pins_seen)
            .expect("pins")
            .lines()
            .skip(1)
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    // The dispatcher published index 134 and moved ay while a pass that read index 133 ran.
    s.raise(134, &arm, &pins(&[("ay", 15), ("nn", 3)]))
        .expect("the dispatcher's floors");
    // The pass ends and records the older head it read: nothing falls.
    s.raise(133, &arm, &pins(&[("ay", 14), ("nn", 3), ("ty", 9)]))
        .expect("the pass's floors");
    assert_eq!(s.index_seen().expect("a floor"), 134);
    assert_eq!(seen(&s), ["ay 15", "nn 3", "ty 9"]);
    // So the gates still refuse what the dispatcher's floors refuse.
    let tx = Transcript::memory("t");
    assert!(s.check_index(133).is_err());
    assert!(
        s.check_pins(&arm, &pins(&[("ay", 14), ("nn", 3), ("ty", 9)]), 134, &tx)
            .is_err()
    );
    // A newer head raises them.
    s.raise(135, &arm, &pins(&[("ay", 16), ("nn", 3), ("ty", 9)]))
        .expect("a newer head");
    assert_eq!(s.index_seen().expect("a floor"), 135);
    assert_eq!(seen(&s), ["ay 16", "nn 3", "ty 9"]);
    // Floors recorded for another target set say nothing of this one: they are replaced.
    s.raise(135, &[X86_64_APPLE_DARWIN], &pins(&[("ay", 12)]))
        .expect("another target's floors");
    assert_eq!(seen(&s), ["ay 12"]);
}

#[test]
fn a_builds_evidence_ages_out_unless_a_latch_still_names_it() {
    let tmp = scratch("state-evidence");
    let s = LaneState::of(LaneKind::Tip, tmp.path());
    s.ensure().expect("state");
    let kept = |rel: &str| s.dir.join(rel).exists();
    for build in ["ay-5", "ay-6"] {
        std::fs::create_dir_all(s.dir.join(format!("logs/{build}/attempt-1"))).expect("logs");
        std::fs::write(
            s.dir.join(format!("logs/{build}/attempt-1/unit.stdout")),
            "raw\n",
        )
        .expect("a log");
        std::fs::create_dir_all(s.dir.join("receipts")).expect("receipts");
        std::fs::write(s.dir.join(format!("receipts/{build}.json")), "{}").expect("a receipt");
        std::fs::create_dir_all(s.dir.join("retired")).expect("retired");
        std::fs::write(
            s.dir.join(format!("retired/{build}.ready.toml.refused")),
            "",
        )
        .expect("a retired record");
    }
    // ay-6 is latched: a person is still pointed at what it left.
    std::fs::write(s.blocked_file("ay", 6), "# latched\n").expect("a latch");
    let all = [
        "logs/ay-5",
        "receipts/ay-5.json",
        "retired/ay-5.ready.toml.refused",
        "logs/ay-6/attempt-1/unit.stdout",
        "receipts/ay-6.json",
        "retired/ay-6.ready.toml.refused",
    ];
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after the epoch")
            .as_secs(),
    )
    .expect("a time");
    // Within the keep nothing goes.
    s.prune_evidence(now + EVIDENCE_KEEP_SECS - 3600);
    for rel in all {
        assert!(kept(rel), "{rel} was swept early");
    }
    // Past it the unlatched build's evidence goes and the latched build's stays.
    s.prune_evidence(now + EVIDENCE_KEEP_SECS + 3600);
    for rel in &all[..3] {
        assert!(!kept(rel), "{rel} outlived its keep");
    }
    for rel in &all[3..] {
        assert!(kept(rel), "{rel} is what a latch points at");
    }
    // The latch deleted, its build's evidence ages out like any other.
    std::fs::remove_file(s.blocked_file("ay", 6)).expect("unlatch");
    s.prune_evidence(now + EVIDENCE_KEEP_SECS + 3600);
    for rel in &all[3..] {
        assert!(!kept(rel), "{rel} outlived its latch");
    }
}

#[test]
fn a_dry_run_counts_an_unreadable_head_without_writing_it() {
    let tmp = scratch("state-head");
    let s = LaneState::of(LaneKind::Alab, tmp.path());
    s.ensure().expect("state");
    assert_eq!(s.bump_head_failures(true), 1);
    assert_eq!(s.bump_head_failures(true), 1, "a dry run records nothing");
    assert_eq!(s.bump_head_failures(false), 1);
    assert_eq!(s.bump_head_failures(false), 2);
    s.clear_head_failures();
    assert_eq!(s.bump_head_failures(false), 1);
}

#[test]
fn a_held_lock_is_busy_and_a_holder_past_twelve_hours_has_stalled() {
    let tmp = scratch("state-lock");
    let s = LaneState::of(LaneKind::Linux, tmp.path());
    s.ensure().expect("state");
    let held = s.try_lock().expect("lock");
    assert!(matches!(held, Lock::Held(_)));
    assert!(matches!(s.try_lock().expect("lock"), Lock::Busy));
    s.mark_started(NOW - 3600);
    assert_eq!(s.stalled_since(NOW), None);
    s.mark_started(NOW - 13 * 3600);
    assert_eq!(s.stalled_since(NOW), Some(NOW - 13 * 3600));
    drop(held);
    assert!(matches!(s.try_lock().expect("lock"), Lock::Held(_)));
}

/// The day is the civil date's whole year, month and day. NEGATIVE CONTROL: the epoch, a day in
/// 2026, and the last instant of 9999 read as their dates.
#[test]
fn a_utc_day_names_its_whole_year() {
    assert_eq!(report::utc_day(0), "19700101");
    assert_eq!(
        report::utc_day(-1),
        "19700101",
        "before the epoch reads as its day"
    );
    assert_eq!(report::utc_day(1_792_022_399), "20261014");
    assert_eq!(report::utc_day(253_402_300_799), "99991231");
    assert_eq!(report::utc_day(253_402_300_800), "100000101");
    assert_eq!(report::utc_day(i64::MAX), "2922770265961204");
}

#[test]
fn a_notice_is_recorded_once_a_utc_day() {
    let tmp = scratch("state-notify");
    let seen = Recorded::default();
    let day = 86_400;
    report::record_once(&seen, "t", tmp.path(), NOW, false, "a cut is owed");
    report::record_once(&seen, "t", tmp.path(), NOW + 3600, false, "a cut is owed");
    report::record_once(&seen, "t", tmp.path(), NOW, false, "something else");
    report::record_once(&seen, "t", tmp.path(), NOW, true, "a dry run says nothing");
    assert_eq!(seen.entries(), ["a cut is owed", "something else"]);
    report::record_once(&seen, "t", tmp.path(), NOW + day, false, "a cut is owed");
    assert_eq!(seen.entries().len(), 3, "the next day, again");
    let files: Vec<String> = std::fs::read_dir(tmp.path())
        .expect("dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files, [format!("notified-{}", report::utc_day(NOW + day))]);
    report::record_once(&seen, "t", tmp.path(), NOW + day, false, "carriage\r");
    report::record_once(&seen, "t", tmp.path(), NOW + day + 60, false, "carriage\r");
    assert_eq!(
        seen.entries().len(),
        4,
        "a message ending in CR is still recorded at most once a UTC day"
    );
    report::record_once(&seen, "t", tmp.path(), NOW + day, false, "carriage");
    assert_eq!(
        seen.entries().len(),
        5,
        "NEGATIVE CONTROL: without the CR it is another"
    );

    let transcript = Transcript::memory("publication");
    report::record_once(
        &report::Journal(&transcript),
        "lane",
        &tmp.path().join("journal"),
        NOW,
        false,
        "deferred work",
    );
    assert!(transcript.text().contains("lane: deferred work"));
    let blocking_dir = tmp.path().join("dir-target");
    std::fs::create_dir(&blocking_dir).expect("mkdir blocking_dir");
    assert!(report::write_atomic(&blocking_dir, "body").is_err());
    let leftovers: Vec<String> = std::fs::read_dir(tmp.path())
        .expect("dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn a_group_holds_where_a_member_is_not_ready_and_nowhere_else() {
    let mut h = Holds::new();
    h.not_ready("rustc", Some(AARCH64_UNKNOWN_LINUX_GNU), "trust");
    assert!(h.is_held("rustc", Some(AARCH64_UNKNOWN_LINUX_GNU)));
    assert!(
        !h.is_held("rustc", Some(X86_64_UNKNOWN_LINUX_GNU)),
        "per target"
    );
    assert!(!h.is_held("other", Some(AARCH64_UNKNOWN_LINUX_GNU)));
    let whole = of_members([
        ("rustc", "trust", None, false),
        ("rustc", "trust-ir", None, true),
    ]);
    assert!(whole.is_held("rustc", None));
    assert!(whole.is_held("rustc", Some(AARCH64_APPLE_DARWIN)));
    assert_eq!(
        whole
            .holding("rustc", Some(X86_64_APPLE_DARWIN))
            .expect("held"),
        ["trust".to_string()].into_iter().collect()
    );
    assert!(of_members([("rustc", "trust", None, true)]).is_empty());
}
