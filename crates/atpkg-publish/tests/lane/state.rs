// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The lanes' durable state, holds and reports, over typed data: the floors, the
//! attempts, the lock, the once-a-day rule, the coherence hold.

use std::collections::BTreeMap;

use atpkg_publish::lane::LaneKind;
use atpkg_publish::lane::hold::{Holds, of_members};
use atpkg_publish::lane::report::{self, Recorded, Transcript};
use atpkg_publish::lane::state::{LaneState, Lock};
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
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 86_400);
    std::fs::File::options()
        .write(true)
        .open(&file)
        .and_then(|f| f.set_modified(old))
        .expect("age the counter");
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs(),
    )
    .expect("i64");
    s.prune(now);
    assert!(!file.exists());
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

#[test]
fn a_notice_is_shown_once_a_utc_day() {
    let tmp = scratch("state-notify");
    let seen = Recorded::default();
    let day = 86_400;
    report::notify_once(&seen, "t", tmp.path(), NOW, false, "a cut is owed");
    report::notify_once(&seen, "t", tmp.path(), NOW + 3600, false, "a cut is owed");
    report::notify_once(&seen, "t", tmp.path(), NOW, false, "something else");
    report::notify_once(&seen, "t", tmp.path(), NOW, true, "a dry run says nothing");
    assert_eq!(seen.shown(), ["a cut is owed", "something else"]);
    report::notify_once(&seen, "t", tmp.path(), NOW + day, false, "a cut is owed");
    assert_eq!(seen.shown().len(), 3, "the next day, again");
    let files: Vec<String> = std::fs::read_dir(tmp.path())
        .expect("dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files, [format!("notified-{}", report::utc_day(NOW + day))]);
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
