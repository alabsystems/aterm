// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fleet`'s rows and status line, composed from fixture facts: a presence view whose seats
//! run old builds, a book that heard a member on the old path, a membership log's seats.

use super::*;
use aterm_mesh::presence::{PresenceRow, Seat};
use aterm_net::fleet::NodeIdentity;
use std::time::Duration;

/// The fixture's clock: the view's time and the compose's now (Unix ms).
const NOW: u64 = 1_800_000_000_000;

/// A node identity whose key is fixed by `n`.
fn identity(n: u8) -> NodeIdentity {
    NodeIdentity::from_seed(&format!("{n:02x}").repeat(16), &[n; 32]).expect("a node identity")
}

fn key(n: u8) -> [u8; 32] {
    *identity(n).key().bytes()
}

fn log_seat(label: &str, name: &str, node: Option<u8>) -> LogSeat {
    LogSeat {
        label: label.to_owned(),
        name: name.to_owned(),
        node_key: node.map(key),
    }
}

/// The view's seat of node `n`: `standing`, `behind`, and a row naming `build` where it has one.
fn view_seat(n: u8, me: bool, standing: Presence, behind: Behind, build: Option<&str>) -> Seat {
    Seat {
        principal: principal_of(&key(n)),
        key: key(n),
        me,
        standing,
        behind,
        row: build.map(|build| {
            (
                7,
                PresenceRow {
                    at_ms: NOW - 1_000,
                    build: Build::new(build).expect("a build word"),
                    addresses: Vec::new(),
                    membership: None,
                    tables: None,
                    topics: Vec::new(),
                },
            )
        }),
    }
}

/// Six seats: m1 is this Mac; m2 and m3 are up, m3 40 s behind on an older build; m4 has been
/// silent 3 h on 0.150; m5's node publishes no presence row while this Mac's book heard it on the
/// old path two minutes ago at 0.147; m6 bound no node key.
fn seats() -> Vec<LogSeat> {
    vec![
        log_seat("m1", "m1-mini", Some(1)),
        log_seat("m2", "m2-studio", Some(2)),
        log_seat("m3", "m3-air", Some(3)),
        log_seat("m4", "m4-pro", Some(4)),
        log_seat("m5", "m5-old", Some(5)),
        log_seat("m6", "m6-new", None),
    ]
}

fn view() -> View {
    let silent_since = NOW - 3 * 3_600_000;
    View {
        me: principal_of(&key(1)),
        at_ms: NOW,
        standings: true,
        seats: vec![
            view_seat(
                1,
                true,
                Presence::Up,
                Behind::InStep,
                Some("0.159.0+g4ef9cd6144"),
            ),
            view_seat(
                2,
                false,
                Presence::Up,
                Behind::InStep,
                Some("0.159.0+g4ef9cd6144"),
            ),
            view_seat(
                3,
                false,
                Presence::Up,
                Behind::Behind {
                    at_least: Duration::from_secs(40),
                },
                Some("0.158.0"),
            ),
            view_seat(
                4,
                false,
                Presence::Silent {
                    last_seen_ms: Some(silent_since),
                },
                Behind::Unknown,
                Some("0.150.0"),
            ),
            view_seat(
                5,
                false,
                Presence::Silent { last_seen_ms: None },
                Behind::Unknown,
                None,
            ),
        ],
        needs: None,
        activity: None,
    }
}

/// m5's own signed entry in this Mac's book, at 0.147.0, heard two minutes ago.
fn book() -> Vec<Peer> {
    vec![Peer::own(
        &identity(5),
        ("m5-old.local", 7100, "0.147.0"),
        NOW - 120_000,
    )]
}

/// A table line's cells: the text between runs of two or more spaces.
fn cells(line: &str) -> Vec<&str> {
    line.split("  ")
        .map(str::trim)
        .filter(|cell| !cell.is_empty())
        .collect()
}

/// Up and silent, builds, the members that need an update and tables in step come from the
/// view; the old-path member's build from its signed book entry; the log's fill from this Mac's
/// records.
///
/// NEGATIVE CONTROL: drop `crossed &&` from `compose`'s `needs_update`: the uncrossed compose
/// names m4 too. Drop its book-heard arm: the uncrossed compose names no one.
#[test]
fn the_view_names_the_old_builds_and_reads_each_seat_from_presence() {
    let (seats, view, book) = (seats(), view(), book());
    let facts = |crossed| Facts {
        seats: Some(&seats),
        records: Some((4_169, crossed)),
        view: Ok(&view),
        book: &book,
        now_ms: NOW,
    };
    let fleet = compose(&facts(true));
    assert_eq!(
        words::status_line(&fleet.seats),
        "6 members · 3 up · 2 need update (m4 0.150, m5 0.147) · tables in step with 1 of 2, \
         slowest 40 s behind · log 4,169 of 16,384"
    );
    assert_eq!(fleet.unserved, None);
    let text = render(&fleet, Some(NOW));
    let lines: Vec<Vec<&str>> = text.lines().skip(1).map(cells).collect();
    let n1 = principal_of(&key(1));
    let n3 = principal_of(&key(3));
    let n4 = principal_of(&key(4));
    let n5 = principal_of(&key(5));
    assert_eq!(
        lines,
        vec![
            vec!["LABEL", "NAME", "NODE", "UP", "BUILD", "TABLES"],
            vec![
                "m1",
                "m1-mini (this Mac)",
                &n1,
                "up",
                "0.159.0+g4ef9cd6144",
                "-"
            ],
            vec![
                "m2",
                "m2-studio",
                &principal_of(&key(2)),
                "up",
                "0.159.0+g4ef9cd6144",
                "in step"
            ],
            vec!["m3", "m3-air", &n3, "up", "0.158.0", "40 s behind"],
            vec![
                "m4",
                "m4-pro",
                &n4,
                "silent 3 h",
                "needs update (0.150.0)",
                "-"
            ],
            vec![
                "m5",
                "m5-old",
                &n5,
                "silent, never seen up",
                "needs update (0.147.0)",
                "-"
            ],
            vec!["m6", "m6-new", "-", "silent, never seen up", "-", "-"],
        ],
        "{text}"
    );
    let json = to_json(&fleet);
    assert_eq!(json["up"], Value::from(3_u64));
    assert_eq!(json["seats"][4]["needs_update"], Value::from(true));
    assert_eq!(json["seats"][2]["behind_ms"], Value::from(40_000_u64));

    // Before this Mac's log crossed the legacy bound, an old build can still hold it: only the
    // member that publishes no presence row needs an update.
    let uncrossed = compose(&facts(false));
    assert_eq!(
        uncrossed.seats.needs_update,
        vec![("m5".to_owned(), "0.147.0".to_owned())]
    );
}

/// With no node serving presence, nothing is read as up or silent and nothing as in step; the
/// view says why, and a build comes from the book alone.
#[test]
fn with_no_node_the_view_says_why_and_reads_no_one_up() {
    let (seats, book) = (seats(), book());
    let why = "no node service serves presence here (`aterm node status`)".to_owned();
    let fleet = compose(&Facts {
        seats: Some(&seats),
        records: Some((4_169, true)),
        view: Err(why.clone()),
        book: &book,
        now_ms: NOW,
    });
    assert_eq!(
        words::status_line(&fleet.seats),
        "6 members · up not read · 1 needs update \
         (m5 0.147) · log 4,169 of 16,384"
    );
    assert!(
        fleet
            .rows
            .iter()
            .all(|row| row.presence.is_none() && row.behind.is_none())
    );
    let text = render(&fleet, None);
    assert_eq!(
        text.lines().nth(1),
        Some(format!("presence: {why}").as_str())
    );
    assert_eq!(
        own_line(&fleet, None),
        format!("6 members; presence not read: {why}"),
        "with no view no seat reads as this Mac"
    );
}

/// A seat the node holds that this Mac's log does not (a log read behind the node's) is listed
/// under no label and counted a member, so no up seat goes unseen and no more are up than are
/// members; `aterm status`'s line is this Mac's own row.
#[test]
fn a_seat_only_the_view_holds_is_listed_and_the_own_line_is_this_macs_row() {
    let seats = seats();
    let mut view = view();
    view.seats.push(view_seat(
        9,
        false,
        Presence::Up,
        Behind::InStep,
        Some("0.159.0"),
    ));
    let fleet = compose(&Facts {
        seats: Some(&seats),
        records: None,
        view: Ok(&view),
        book: &[],
        now_ms: NOW,
    });
    let last = fleet.rows.last().expect("rows");
    assert_eq!(
        (last.label.as_str(), last.node.clone()),
        ("-", Some(principal_of(&key(9))))
    );
    assert_eq!((fleet.seats.up, fleet.seats.members), (Some(4), 7));
    assert_eq!(
        fleet.seats.in_step.map(|step| (step.with, step.of)),
        Some((2, 3))
    );
    assert_eq!(
        own_line(&fleet, Some(NOW)),
        format!(
            "m1 {} up at 0.159.0+g4ef9cd6144; 4 up of 7 members",
            principal_of(&key(1))
        )
    );
}

/// `compose_here` composes the view its caller asked with this Mac's own facts, and asks no node
/// itself: a view handed in is read (its seats listed, presence served), and a why handed in is
/// the line's, though no node serves the state directory.
#[test]
fn compose_here_reads_the_view_it_is_handed() {
    let state = aterm_tempfile::tempdir().unwrap();
    let view = view();
    let fleet = compose_here(state.path(), None, Ok(&view));
    assert_eq!(fleet.unserved, None);
    assert_eq!(
        fleet.rows.len(),
        view.seats.len(),
        "with no log or book, the view's seats are the rows"
    );
    assert!(fleet.rows.iter().all(|row| row.presence.is_some()));
    let why = "the node has not read the lab's membership standings yet".to_owned();
    let unread = compose_here(state.path(), None, Err(why.clone()));
    assert_eq!(unread.unserved, Some(why));
    assert!(unread.rows.is_empty());
}
