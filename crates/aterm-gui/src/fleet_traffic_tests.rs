// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_ctl::fleet_activity::Record;
use aterm_types::fleet_access::FleetAccess;
use aterm_types::fleet_activity::{Outcome, Plain, Row, WINDOW_MS, Way};

use super::*;
use crate::status_item::blocked_unseen;

const NOW_MS: u64 = 1_800_000_000_000;
const M3: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A scratch state folder, its record, and a traffic with a reader a test looks with by hand.
fn scratch() -> (aterm_tempfile::TempDir, Record) {
    let dir = aterm_tempfile::tempdir().expect("a scratch state folder");
    let record = Record::in_state(dir.path());
    (dir, record)
}

/// `m3` asked this Mac for a new job at `at_ms`, which its lock refused or let through.
fn new_job(at_ms: u64, refused: bool) -> Row {
    Row {
        at_ms,
        way: Way::In,
        node: M3.to_owned(),
        host: Some("m3".to_owned()),
        what: Plain::NewJob,
        outcome: if refused {
            Outcome::Blocked {
                level: FleetAccess::ReadOnly,
                needs: FleetAccess::Full,
            }
        } else {
            Outcome::Admitted
        },
        by: None,
        n: 1,
    }
}

#[test]
fn a_look_adds_the_hour_up_and_answers_only_when_it_changed() {
    let (dir, record) = scratch();
    let traffic = FleetTraffic::over(None, 0);
    let mut reader = Reader::in_state(dir.path(), &traffic);
    assert_eq!(traffic.summary(), None, "nothing looked at yet");
    assert!(reader.look(NOW_MS), "the first look is an answer");
    let empty = traffic.summary().expect("a summary");
    assert_eq!((empty.count(), empty.last_ms), (0, None));
    assert!(
        !reader.look(NOW_MS + 1_000),
        "nothing written, nothing aged"
    );

    record
        .append(&[new_job(NOW_MS + 2_000, false)])
        .expect("written");
    assert!(reader.look(NOW_MS + 3_000));
    let one = traffic.summary().expect("a summary");
    assert_eq!((one.count(), one.peers_in, one.blocked()), (1, 1, 0));
    assert!(!reader.look(NOW_MS + 4_000), "the same hour again");

    // The row leaves the hour by the clock alone: no write, and the answer changes once.
    let later = NOW_MS + 2_000 + WINDOW_MS;
    assert!(reader.look(later));
    let quiet = traffic.summary().expect("a summary");
    assert_eq!((quiet.count(), quiet.last_ms), (0, Some(NOW_MS + 2_000)));
    assert!(!reader.look(later + 1_000));
}

#[test]
fn the_refusal_last_seen_is_kept_on_disk_and_the_next_reader_starts_from_it() {
    let (dir, record) = scratch();
    let traffic = FleetTraffic::over(None, 0);
    let mut reader = Reader::in_state(dir.path(), &traffic);
    assert_eq!(traffic.seen_ms(), 0, "never opened");
    let _ = reader.look(NOW_MS);
    assert_eq!(record.menu_seen(), None, "a look alone writes nothing");

    traffic.menu_opened(NOW_MS + 5_000);
    assert_eq!(traffic.seen_ms(), NOW_MS + 5_000, "known at once");
    assert_eq!(
        record.menu_seen(),
        None,
        "the file is the reader's to write"
    );
    let _ = reader.look(NOW_MS + 6_000);
    assert_eq!(record.menu_seen(), Some(NOW_MS + 5_000));

    // A menu that showed an earlier refusal moves neither the memory nor the file back.
    traffic.menu_opened(NOW_MS + 1_000);
    let _ = reader.look(NOW_MS + 7_000);
    assert_eq!(traffic.seen_ms(), NOW_MS + 5_000);
    assert_eq!(record.menu_seen(), Some(NOW_MS + 5_000));

    // The app starts again: its reader's first look reads it back.
    let next = FleetTraffic::over(None, 0);
    let mut reader = Reader::in_state(dir.path(), &next);
    assert!(reader.look(NOW_MS + 8_000));
    assert_eq!(next.seen_ms(), NOW_MS + 5_000);

    // One stated before the reader's first look is later than the file's, and is the one kept.
    let early = FleetTraffic::over(None, NOW_MS + 9_000);
    let mut reader = Reader::in_state(dir.path(), &early);
    let _ = reader.look(NOW_MS + 10_000);
    assert_eq!(early.seen_ms(), NOW_MS + 9_000);
    assert_eq!(record.menu_seen(), Some(NOW_MS + 9_000));
}

/// The bar's mark of this test's `traffic` at `now_ms`.
fn marked(traffic: &FleetTraffic, now_ms: u64) -> bool {
    blocked_unseen(traffic.summary().as_ref(), traffic.seen_ms(), now_ms)
}

#[test]
fn a_refusal_marks_the_bar_until_a_menu_shows_it_and_stays_cleared_across_a_restart() {
    let (dir, record) = scratch();
    let traffic = FleetTraffic::over(None, 0);
    let mut reader = Reader::in_state(dir.path(), &traffic);
    assert!(!marked(&traffic, NOW_MS), "nothing read");
    record.append(&[new_job(NOW_MS, false)]).expect("written");
    let _ = reader.look(NOW_MS + 1_000);
    assert!(!marked(&traffic, NOW_MS + 1_000), "a request let through");

    record
        .append(&[new_job(NOW_MS + 2_000, true)])
        .expect("written");
    assert!(reader.look(NOW_MS + 3_000));
    assert!(
        marked(&traffic, NOW_MS + 3_000),
        "this Mac's lock refused one"
    );

    // The menu is painted from this summary; a second refusal lands before it opens. The open
    // states what the menu showed, so the refusal the person has not seen keeps the mark.
    let painted = traffic
        .summary()
        .and_then(|summary| summary.last_blocked_in_ms)
        .expect("the refusal the menu shows");
    assert_eq!(painted, NOW_MS + 2_000);
    record
        .append(&[new_job(NOW_MS + 3_500, true)])
        .expect("written");
    assert!(reader.look(NOW_MS + 3_800));
    traffic.menu_opened(painted);
    assert!(
        marked(&traffic, NOW_MS + 4_000),
        "a refusal that came after the paint is not seen"
    );
    // The next open shows both.
    traffic.menu_opened(NOW_MS + 3_500);
    assert!(!marked(&traffic, NOW_MS + 4_000), "the person looked");
    let _ = reader.look(NOW_MS + 4_500);

    let next = FleetTraffic::over(None, 0);
    let mut reader = Reader::in_state(dir.path(), &next);
    assert!(reader.look(NOW_MS + 5_000));
    assert!(
        !marked(&next, NOW_MS + 5_000),
        "a restart does not bring back a refusal already looked at"
    );
    assert_eq!(
        next.summary().map(|s| s.blocked_in),
        Some(2),
        "it is still in the menu"
    );

    record
        .append(&[new_job(NOW_MS + 6_000, true)])
        .expect("written");
    assert!(reader.look(NOW_MS + 7_000));
    assert!(
        marked(&next, NOW_MS + 7_000),
        "a later refusal marks it again"
    );
}

/// TWO ATERMS OF ONE ACCOUNT SHARE THE RECORD AND WHAT WAS SEEN: a menu opened in one clears
/// the mark in the other at its next look, which answers that the bar changed. NEGATIVE
/// CONTROL: a reader that reads the file only when it starts leaves the second bar marked for
/// the hour.
#[test]
fn a_menu_opened_in_another_aterm_clears_the_mark_here_too() {
    let (dir, record) = scratch();
    let (here, there) = (FleetTraffic::over(None, 0), FleetTraffic::over(None, 0));
    let mut reader_here = Reader::in_state(dir.path(), &here);
    let mut reader_there = Reader::in_state(dir.path(), &there);
    record
        .append(&[new_job(NOW_MS + 2_000, true)])
        .expect("written");
    assert!(reader_here.look(NOW_MS + 3_000) && reader_there.look(NOW_MS + 3_000));
    assert!(marked(&here, NOW_MS + 3_000) && marked(&there, NOW_MS + 3_000));

    there.menu_opened(NOW_MS + 2_000);
    let _ = reader_there.look(NOW_MS + 4_000);
    assert!(marked(&here, NOW_MS + 4_000), "not looked yet");
    assert!(
        reader_here.look(NOW_MS + 5_000),
        "what the bar draws from moved, though the hour's sum did not"
    );
    assert!(!marked(&here, NOW_MS + 5_000));
    assert!(!reader_here.look(NOW_MS + 6_000), "and is settled");
}

/// A time seen from before the clock was set back hides no refusal: it is forgotten in memory
/// and on disk, and the refusal marks the bar. One within the clocks' slack stands.
#[test]
fn a_seen_time_after_the_clock_is_forgotten_and_the_mark_shows() {
    let (dir, record) = scratch();
    let ahead = NOW_MS + 30 * 24 * WINDOW_MS;
    record.note_menu_seen(ahead).expect("written");
    record
        .append(&[new_job(NOW_MS + 1_000, true)])
        .expect("written");
    // Known in memory too, as an app that ran through the clock's change knows it.
    let traffic = FleetTraffic::over(None, ahead);
    let mut reader = Reader::in_state(dir.path(), &traffic);
    assert!(reader.look(NOW_MS + 2_000));
    assert_eq!(traffic.seen_ms(), 0);
    assert_eq!(record.menu_seen(), Some(0));
    assert!(marked(&traffic, NOW_MS + 2_000));
    // On disk alone, as an app started since reads it.
    record.note_menu_seen(ahead).expect("written");
    let next = FleetTraffic::over(None, 0);
    let mut reader = Reader::in_state(dir.path(), &next);
    assert!(reader.look(NOW_MS + 2_000));
    assert_eq!((next.seen_ms(), record.menu_seen()), (0, Some(0)));
    assert!(marked(&next, NOW_MS + 2_000));
    // Within the slack between two clocks it is a time like any other.
    let near = NOW_MS + 2_000 + aterm_types::fleet_activity::AHEAD_MS;
    next.menu_opened(near);
    let _ = reader.look(NOW_MS + 2_000);
    assert_eq!((next.seen_ms(), record.menu_seen()), (near, Some(near)));
}
