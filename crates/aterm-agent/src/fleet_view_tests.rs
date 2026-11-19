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

/// What a Mac whose log crossed the legacy bound, in format 1, says a build needs.
const CROSSED: Needs = Needs {
    crossed: true,
    format: FormatFloor::Every,
    own: None,
};

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

fn survey_fixture() -> (Fleet, Vec<Peer>, aterm_ctl::FleetRead) {
    let (seats, book) = (seats(), book());
    let fleet = compose(&Facts {
        seats: Some(&seats),
        records: Some((4_169, true)),
        format: FormatFloor::Every,
        own: None,
        view: Err("no cached presence".to_owned()),
        book: &book,
        now_ms: NOW,
    });
    let peer = aterm_ctl::PeerSurvey {
        authenticated: Some(aterm_ctl::AuthenticatedPeer { node: identity(5).node().to_owned(), key: key(5) }),
        // A matching name is neither needed nor sufficient: only m5's key is bound here.
        label: "m1".to_owned(), node: Some(identity(5).node().to_owned()),
        answer: Ok(vec![
            format!("instance node={} version=0.197.0 sessions=3 window=1 window_last=- label=m1 ssh_host_key=- home_free_mb=128000 accounts=1", identity(5).node()),
            "account pool=claude id=fixture condition=usable enrolled=1 pct5=27 pct7=60 week_reset=- walled_until=- last_used=-".to_owned(),
        ]),
        fault: aterm_ctl::PeerFault::Other, unproven: false, addr: None, reason: None,
    };
    (
        fleet,
        book,
        aterm_ctl::FleetRead {
            peers: vec![peer],
            note: None,
            alone: None,
        },
    )
}

#[test]
fn proved_surveys_supply_current_metrics_without_relabelling_unknown_seats() {
    let (fleet, book, read) = survey_fixture();
    let fleet = observe(fleet, &book, Ok(&read), NOW, CROSSED);
    let json = to_json(&fleet);
    assert_eq!(json["up"], Value::from(1_u64));
    assert_eq!(json["unknown"], Value::from(5_u64));
    let row = &json["seats"][4];
    assert_eq!(row["build"], Value::from("0.197.0"));
    assert_eq!(row["build_source"], Value::from("survey"));
    assert_eq!(row["node"], Value::from(identity(5).node()));
    assert_eq!(row["observed_at_ms"], Value::from(NOW));
    assert_eq!(row["home_free_mb"], Value::from(128_000_u64));
    assert_eq!(row["sessions"], Value::from(3_u64));
    assert_eq!(row["accounts"][0]["pct5"], Value::from(27_u64));
    assert_eq!(row["accounts"][0]["pct7"], Value::from(60_u64));
    assert_eq!(row["needs_update"], Value::from(false));
    assert_eq!(json["seats"][0]["up"], Value::Null);
    assert_eq!(json["seats"][0]["sessions"], Value::Null);
}

/// THE FLEET VIEW PRINTS EACH SEAT'S TOTAL, FREE AND BUILD BYTES: a member whose survey says its
/// home volume's size, free space and build output shows them under TOTAL_MB, FREE_MB and
/// BUILD_MB, the build output with `+` where its sizing was cut short and that sizing's age at
/// the member's answer, told on the member's own clock: `(2 h)` whether that clock runs 3 h
/// behind this Mac's or 3 h ahead, `(0 s)` for a sizing that clock puts after the answer, and no
/// age from an answer that says no time. `aterm fleet --json` gives each figure under its key,
/// with the answer's own time (`said_at_ms`). A survey from an aterm older than the keys shows
/// `-` and null for each, never 0; a measured 0 is `0`; a seat with no survey shows `-`.
/// NEGATIVE CONTROL: with the total cell read from the free space, TOTAL_MB says 128000; with
/// the partial word dropped, BUILD_MB says `84992 (2 h)`; with the age told on this Mac's clock
/// (the observation's `at_ms`), the member 3 h behind says `(5 h)` and the one 3 h ahead `(0 s)`;
/// with an unknown figure written through `unwrap_or(0)`, the older survey's TOTAL_MB says 0 and
/// its JSON 0.
#[test]
fn the_fleet_view_prints_each_seats_total_free_and_build_bytes() {
    // The member's clock runs 3 h behind this Mac's, and it sized its build output 2 h before it
    // answered.
    const SAID_AT: u64 = NOW - 10_800_000;
    const SIZED_AT: u64 = SAID_AT - 7_200_000;
    let surveyed = |tokens: &str| {
        let (fleet, book, mut read) = survey_fixture();
        let lines = read.peers[0].answer.as_mut().expect("the fixture's answer");
        let with = lines[0].replacen(
            " home_free_mb=128000 ",
            &format!(" home_free_mb=128000{tokens} "),
            1,
        );
        assert_ne!(with, lines[0], "{tokens}");
        lines[0] = with;
        observe(fleet, &book, Ok(&read), NOW, CROSSED)
    };
    // The cell of `label`'s row under `column`, placed by the head's own columns.
    let cell = |text: &str, label: &str, column: &str| -> String {
        let head = text
            .lines()
            .find(|line| line.starts_with("LABEL "))
            .expect("the head");
        let row = text
            .lines()
            .find(|line| line.starts_with(&format!("{label} ")))
            .unwrap_or_else(|| panic!("{label}'s row: {text}"));
        let from = head
            .find(&format!("  {column} "))
            .unwrap_or_else(|| panic!("{column}: {head}"))
            + 2;
        let named = from + column.len();
        let to = head[named..]
            .find(|c: char| c != ' ')
            .map_or(head.len(), |gap| named + gap);
        row.get(from..to.min(row.len()))
            .unwrap_or_default()
            .trim()
            .to_owned()
    };

    // m5's BUILD_MB cell where its answer's header adds `tokens`.
    let build_cell = |tokens: &str| cell(&render(&surveyed(tokens), Some(NOW)), "m5", "BUILD_MB");

    let fleet = surveyed(&format!(
        " at_ms={SAID_AT} home_total_mb=953000 home_build_mb=84992 \
         home_build_at_ms={SIZED_AT} home_build_partial=1"
    ));
    let text = render(&fleet, Some(NOW));
    assert_eq!(
        ["TOTAL_MB", "FREE_MB", "BUILD_MB"].map(|column| cell(&text, "m5", column)),
        ["953000", "128000", "84992+ (2 h)"],
        "{text}"
    );
    assert_eq!(
        ["TOTAL_MB", "FREE_MB", "BUILD_MB"].map(|column| cell(&text, "m1", column)),
        ["-", "-", "-"],
        "a seat with no survey: {text}"
    );
    let json = to_json(&fleet);
    let row = &json["seats"][4];
    assert_eq!(row["label"], Value::from("m5"));
    assert_eq!(row["observed_at_ms"], Value::from(NOW));
    assert_eq!(row["said_at_ms"], Value::from(SAID_AT));
    assert_eq!(row["home_total_mb"], Value::from(953_000_u64));
    assert_eq!(row["home_free_mb"], Value::from(128_000_u64));
    assert_eq!(row["home_build_mb"], Value::from(84_992_u64));
    assert_eq!(row["home_build_at_ms"], Value::from(SIZED_AT));
    assert_eq!(row["home_build_partial"], Value::from(true));

    // A member 3 h ahead of this Mac: the same age, on its own clock.
    let ahead = NOW + 10_800_000;
    assert_eq!(
        build_cell(&format!(
            " at_ms={ahead} home_build_mb=84992 home_build_at_ms={} home_build_partial=1",
            ahead - 7_200_000
        )),
        "84992+ (2 h)"
    );
    // A sizing its own clock puts after its answer (a clock set back since): 0 s, never
    // negative.
    assert_eq!(
        build_cell(&format!(
            " at_ms={SAID_AT} home_build_mb=84992 home_build_at_ms={} home_build_partial=1",
            SAID_AT + 5_000
        )),
        "84992+ (0 s)"
    );
    // An answer that says no time: the figure, and no age told on another clock.
    let untimed = format!(" home_build_mb=84992 home_build_at_ms={SIZED_AT} home_build_partial=1");
    assert_eq!(build_cell(&untimed), "84992+");
    assert_eq!(
        to_json(&surveyed(&untimed))["seats"][4]["said_at_ms"],
        Value::Null
    );

    // An aterm older than the keys: unknown, never 0.
    let (fleet, book, read) = survey_fixture();
    let fleet = observe(fleet, &book, Ok(&read), NOW, CROSSED);
    let text = render(&fleet, Some(NOW));
    assert_eq!(
        ["TOTAL_MB", "FREE_MB", "BUILD_MB"].map(|column| cell(&text, "m5", column)),
        ["-", "128000", "-"],
        "{text}"
    );
    let json = to_json(&fleet);
    let row = &json["seats"][4];
    for key in [
        "home_total_mb",
        "home_build_mb",
        "home_build_at_ms",
        "home_build_partial",
    ] {
        assert_eq!(row[key], Value::Null, "{key}");
    }
    assert_eq!(row["home_free_mb"], Value::from(128_000_u64));

    // A measured 0, from a sizing that ran whole.
    let fleet = surveyed(&format!(
        " at_ms={SAID_AT} home_total_mb=953000 home_build_mb=0 home_build_at_ms={SIZED_AT} \
         home_build_partial=0"
    ));
    assert_eq!(
        cell(&render(&fleet, Some(NOW)), "m5", "BUILD_MB"),
        "0 (2 h)"
    );
    let json = to_json(&fleet);
    assert_eq!(json["seats"][4]["home_build_mb"], Value::from(0_u64));
    assert_eq!(json["seats"][4]["home_build_partial"], Value::from(false));
}

/// THE FLEET VIEW NAMES EACH SEAT'S FULLEST STORE: a member whose survey says which of the
/// stores aterm keeps there is nearest the bound it refuses writes at shows that store's fixed
/// word and its share under STORE, between BUILD_MB and SESSIONS, and `aterm fleet --json`
/// gives them as `store_worst` and `store_worst_pct`. A survey that says none (an aterm older
/// than the keys, or no such store sized) and a seat with no survey show `-` and null, never 0%.
/// NEGATIVE CONTROL: with the cell left out of the row, the head has no STORE column and a Mac
/// whose vault log is at 83 % of its bound reads like every other in `aterm fleet`.
#[test]
fn the_fleet_view_names_each_seats_fullest_store_and_its_share() {
    let surveyed = |tokens: &str| {
        let (fleet, book, mut read) = survey_fixture();
        let lines = read.peers[0].answer.as_mut().expect("the fixture's answer");
        let with = lines[0].replacen(
            " home_free_mb=128000 ",
            &format!(" home_free_mb=128000{tokens} "),
            1,
        );
        assert_ne!(with, lines[0], "{tokens}");
        lines[0] = with;
        observe(fleet, &book, Ok(&read), NOW, CROSSED)
    };
    // The words of `label`'s row between the head's STORE and SESSIONS columns.
    let store_cell = |text: &str, label: &str| -> String {
        let head = text
            .lines()
            .find(|line| line.starts_with("LABEL "))
            .expect("the head");
        let row = text
            .lines()
            .find(|line| line.starts_with(&format!("{label} ")))
            .unwrap_or_else(|| panic!("{label}'s row: {text}"));
        let from = head.find("  STORE ").expect("the STORE column") + 2;
        let to = head.find("  SESSIONS").expect("the SESSIONS column");
        assert!(head.find("  BUILD_MB ").is_some_and(|build| build < from));
        row.get(from..to.min(row.len()))
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let fleet = surveyed(" store_worst=vault-records store_worst_pct=83");
    let text = render(&fleet, Some(NOW));
    assert_eq!(store_cell(&text, "m5"), "vault-records 83%", "{text}");
    assert_eq!(
        store_cell(&text, "m1"),
        "-",
        "a seat with no survey: {text}"
    );
    let json = to_json(&fleet);
    assert_eq!(json["seats"][4]["label"], Value::from("m5"));
    assert_eq!(
        json["seats"][4]["store_worst"],
        Value::from("vault-records")
    );
    assert_eq!(json["seats"][4]["store_worst_pct"], Value::from(83_u64));
    assert_eq!(json["seats"][0]["store_worst"], Value::Null);

    // A store past its bound says so, and a measured 0 is 0.
    for (pct, said) in [(104_u64, "journal-outbox 104%"), (0, "journal-outbox 0%")] {
        let fleet = surveyed(&format!(
            " store_worst=journal-outbox store_worst_pct={pct}"
        ));
        assert_eq!(store_cell(&render(&fleet, Some(NOW)), "m5"), said);
        assert_eq!(
            to_json(&fleet)["seats"][4]["store_worst_pct"],
            Value::from(pct)
        );
    }
    // None said, one key alone, or a name that is no fixed word: unknown, never 0%.
    for tokens in [
        "",
        " store_worst=- store_worst_pct=-",
        " store_worst=vault-records",
        " store_worst=a%40b.example store_worst_pct=83",
    ] {
        let fleet = if tokens.is_empty() {
            let (fleet, book, read) = survey_fixture();
            observe(fleet, &book, Ok(&read), NOW, CROSSED)
        } else {
            surveyed(tokens)
        };
        assert_eq!(
            store_cell(&render(&fleet, Some(NOW)), "m5"),
            "-",
            "{tokens}"
        );
        let json = to_json(&fleet);
        assert_eq!(json["seats"][4]["store_worst"], Value::Null, "{tokens}");
        assert_eq!(json["seats"][4]["store_worst_pct"], Value::Null, "{tokens}");
    }
}

#[test]
fn surveys_require_full_bound_keys_consistent_nodes_and_complete_facts() {
    for fault in [
        "unproven",
        "missing-identity",
        "wrong-proved-key",
        "wrong-node",
        "unsigned",
        "conflicting-book",
        "short-key",
        "ambiguous",
        "cut",
    ] {
        let (mut fleet, mut book, mut read) = survey_fixture();
        match fault {
            "unproven" => read.peers[0].unproven = true,
            "missing-identity" => read.peers[0].authenticated = None,
            "wrong-proved-key" => read.peers[0].authenticated.as_mut().unwrap().key[31] ^= 1,
            "wrong-node" => {
                let lines = read.peers[0].answer.as_mut().unwrap();
                lines[0] = lines[0].replace(identity(5).node(), identity(4).node());
            }
            "unsigned" => book[0].signed = None,
            "conflicting-book" => book.push(Peer::own(
                &NodeIdentity::from_seed(identity(5).node(), &[4; 32]).unwrap(),
                ("other.local", 7100, "0.197.0"),
                NOW,
            )),
            "short-key" => fleet.rows[4].node_key.as_mut().unwrap()[31] ^= 1,
            "ambiguous" => read.peers.push(read.peers[0].clone()),
            "cut" => {
                read.peers[0].answer.as_mut().unwrap().pop();
            }
            _ => unreachable!(),
        }
        let fleet = observe(fleet, &book, Ok(&read), NOW, CROSSED);
        assert_eq!(fleet.seats.up, Some(0), "{fault}");
        assert!(fleet.observations.is_empty(), "{fault}");
        assert!(
            fleet.rows.iter().all(|row| row.presence.is_none()),
            "{fault}"
        );
    }
}

#[test]
fn a_proved_identity_can_answer_a_peer_requested_by_name_only() {
    let (fleet, book, mut read) = survey_fixture();
    read.peers[0].node = None;
    let fleet = observe(fleet, &book, Ok(&read), NOW, CROSSED);
    assert_eq!(fleet.seats.up, Some(1));
    assert_eq!(fleet.rows[4].node.as_deref(), Some(identity(5).node()));
}

#[test]
fn a_local_network_failure_is_unknown_and_a_missed_peer_has_no_current_metrics() {
    for (fault, expected) in [
        (aterm_ctl::PeerFault::NoNetwork, None),
        (aterm_ctl::PeerFault::NotPermitted, None),
        (aterm_ctl::PeerFault::NoRoute, Some(false)),
    ] {
        let (fleet, book, mut read) = survey_fixture();
        read.peers[0].answer = Err("unavailable".to_owned());
        read.peers[0].fault = fault;
        let fleet = observe(fleet, &book, Ok(&read), NOW, CROSSED);
        let json = to_json(&fleet);
        let row = &json["seats"][4];
        assert_eq!(row["up"], expected.map_or(Value::Null, Value::from));
        assert_eq!(row["build_source"], Value::from("book"));
        assert_eq!(row["sessions"], Value::Null);
        assert_eq!(row["accounts"], Value::Null);
    }
    let (fleet, book, mut read) = survey_fixture();
    let lines = read.peers[0].answer.as_mut().unwrap();
    lines[0] = lines[0].replace(" accounts=1", "");
    lines.pop();
    let fleet = observe(fleet, &book, Ok(&read), NOW, CROSSED);
    assert_eq!(fleet.seats.up, Some(1));
    assert_eq!(to_json(&fleet)["seats"][4]["accounts"], Value::Null);
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
        format: FormatFloor::Every,
        own: None,
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
    let json = to_json(&fleet);
    assert_eq!(json["up"], Value::from(3_u64));
    assert_eq!(json["seats"][4]["needs_update"], Value::from(true));

    // Before this Mac's log crossed the legacy bound, an old build can still hold it: only the
    // member that publishes no presence row needs an update.
    let uncrossed = compose(&facts(false));
    assert_eq!(
        uncrossed.seats.needs_update,
        vec![("m5".to_owned(), "0.147.0".to_owned())]
    );
}

/// A MEMBER THAT CANNOT READ THE VAULT'S RECORDS FORMAT SHOWS `needs update` (tracker FL-148E;
/// `docs/DESIGN-vault-log-2026-11-14.md` step 3): once this Mac's format mark says its log is in
/// format 2, each member whose build reads as a release before the first that reads format 2
/// needs an update, by the view's build, the book's or a survey's, and the status line counts
/// them; a member at that release or later does not. While the mark says format 1, or does not
/// read, the format names no one. A Mac whose own build names no release that reads its log's
/// format (it was taken back to an older aterm) shows its own row so and claims nothing of the
/// others.
///
/// NEGATIVE CONTROL: with `Needs::update`'s `From` arm answering `false`, the format-2 compose
/// names m5 alone (`left: [("m5", "0.147.0")]`); with its `Unread` arm answering `false`, the
/// taken-back Mac's own row is not named (`this Mac's own row needs an update`); with
/// `FormatFloor::of` reading a mark that does not read as format 2, the unread mark names a
/// floor (`left: From((0, 159))`).
#[test]
fn a_member_that_cannot_read_the_vaults_records_format_needs_an_update() {
    let (seats, view, book) = (seats(), view(), book());
    let facts = |format| Facts {
        seats: Some(&seats),
        records: Some((1_200, false)),
        format,
        own: None,
        view: Ok(&view),
        book: &book,
        now_ms: NOW,
    };
    let named = |fleet: &Fleet| fleet.seats.needs_update.clone();
    let name = |label: &str, build: &str| (label.to_owned(), build.to_owned());

    // The mark, and the first release this build says reads format 2.
    let reads = Some((0, 159));
    assert_eq!(FormatFloor::of(Some(1), reads), FormatFloor::Every);
    assert_eq!(FormatFloor::of(Some(2), reads), FormatFloor::From((0, 159)));
    assert_eq!(FormatFloor::of(Some(2), None), FormatFloor::Unread);
    assert_eq!(FormatFloor::of(Some(3), reads), FormatFloor::Unread);
    assert_eq!(FormatFloor::of(None, reads), FormatFloor::Every);

    // Format 1: only the member that publishes no presence row, as before.
    let one = compose(&facts(FormatFloor::Every));
    assert_eq!(named(&one), vec![name("m5", "0.147.0")]);

    // Format 2, read from 0.159: m3 (up, 0.158) and m4 (silent, 0.150) cannot read it; m1 and
    // m2, at 0.159, can.
    let two = compose(&facts(FormatFloor::From((0, 159))));
    assert_eq!(
        named(&two),
        vec![
            name("m3", "0.158.0"),
            name("m4", "0.150.0"),
            name("m5", "0.147.0")
        ]
    );
    assert_eq!(
        words::status_line(&two.seats),
        "6 members · 3 up · 3 need update (m3 0.158, m4 0.150, m5 0.147) · tables in step with \
         1 of 2, slowest 40 s behind · log 1,200 of 16,384"
    );
    assert!(render(&two, Some(NOW)).contains("needs update (0.158.0)"));

    // A Mac whose own build reads no such format: its own row, and no claim about the others.
    let unread = compose(&facts(FormatFloor::Unread));
    let own = unread.rows.iter().find(|row| row.me).expect("this Mac");
    assert!(own.needs_update, "this Mac's own row needs an update");
    assert_eq!(
        named(&unread),
        vec![name("m1", "0.159.0+g4ef9cd6144"), name("m5", "0.147.0")]
    );
    assert!(own_line(&unread, Some(NOW)).contains(", needs update"));

    // A survey's build is held to the same floor: m5 answers at 0.197.0.
    let (fleet, book, read) = survey_fixture();
    let needs = |format| Needs {
        crossed: false,
        format,
        own: None,
    };
    let row = |fleet: &Fleet| {
        fleet
            .rows
            .iter()
            .find(|row| row.label == "m5")
            .expect("m5")
            .needs_update
    };
    let at = |format| observe(fleet.clone(), &book, Ok(&read), NOW, needs(format));
    assert!(row(&at(FormatFloor::From((0, 198)))));
    assert!(!row(&at(FormatFloor::From((0, 197)))));
    assert!(!row(&at(FormatFloor::Every)));
    assert!(!row(&at(FormatFloor::Unread)));
}

/// `aterm fleet` NAMES THIS MAC'S OWN ROW FOR A FORMAT ITS BUILD DOES NOT READ (tracker
/// FL-148E). The command composes with no node's view (its presence comes from the survey), so
/// no view says which seat is this Mac's: the seat that bound this Mac's own node key
/// ([`Facts::own`]) is the row that needs an update, in the text and the JSON, and a survey's
/// answer for that seat keeps it. The key names a row for that alone: with a format every
/// build reads it marks no one, it shows no row as this Mac's, and a seat that bound no key is
/// never the one a missing key names.
///
/// NEGATIVE CONTROLS, each run from the test's first line and seen to fail:
/// - `Mutant::OwnByViewAlone` (a row is this Mac's by the view alone, as before the slice B
///   review): ``assertion `left == right` failed: this Mac's own row, by its node key``,
///   `left: []`, `right: [("m1", "-")]`.
/// - `Needs::own` without its `key.is_some()` (a missing key names the seat that bound none):
///   ``assertion `left == right` failed: no key names no row``, `left: [("m6", "-")]`,
///   `right: []`.
#[test]
fn aterm_fleet_names_this_macs_own_row_for_a_format_its_build_does_not_read() {
    let (seats, book) = (seats(), book());
    // As `main_entry` composes: no view, then the survey.
    let composed = |format, own| {
        compose(&Facts {
            seats: Some(&seats),
            records: Some((1_200, false)),
            format,
            own,
            view: Err("fleet survey not read".to_owned()),
            book: &book,
            now_ms: NOW,
        })
    };
    let name = |label: &str, build: &str| (label.to_owned(), build.to_owned());

    let fleet = composed(FormatFloor::Unread, Some(key(1)));
    assert_eq!(
        fleet.seats.needs_update,
        vec![name("m1", "-")],
        "this Mac's own row, by its node key"
    );
    assert!(
        fleet.rows.iter().all(|row| !row.me),
        "the key shows no row as this Mac's: only the view does"
    );
    let text = render(&fleet, Some(NOW));
    let own = text
        .lines()
        .find(|line| line.starts_with("m1 "))
        .expect("m1");
    assert!(own.contains("needs update"), "{text}");
    assert_eq!(
        to_json(&fleet)["seats"][0]["needs_update"],
        Value::from(true)
    );

    // The survey answers for m5 alone, at a build that names no floor: m1 stays named.
    let (_, book, read) = survey_fixture();
    let needs = Needs {
        crossed: false,
        format: FormatFloor::Unread,
        own: Some(key(1)),
    };
    let surveyed = observe(fleet, &book, Ok(&read), NOW, needs);
    assert_eq!(surveyed.seats.needs_update, vec![name("m1", "-")]);
    // And where the survey's answer is this Mac's own seat's, its build is named with it.
    let own = Needs {
        own: Some(key(5)),
        ..needs
    };
    let surveyed = observe(
        composed(FormatFloor::Unread, Some(key(5))),
        &book,
        Ok(&read),
        NOW,
        own,
    );
    assert_eq!(surveyed.seats.needs_update, vec![name("m5", "0.197.0")]);

    // No key read: no row, and never the seat that bound no key (m6).
    assert_eq!(
        composed(FormatFloor::Unread, None).seats.needs_update,
        Vec::new(),
        "no key names no row"
    );
    // A format every build reads: the key names no one.
    assert_eq!(
        composed(FormatFloor::Every, Some(key(1)))
            .seats
            .needs_update,
        Vec::new()
    );
}

/// THE FLOOR IS READ OFF THIS MAC'S OWN FORMAT MARK ([`gather_here`], [`format_floor`]): what
/// `aterm fleet`, `aterm status` and the glance hand [`compose`]. With no vault, no mark, or a
/// mark that does not read, every build reads the log's format and this Mac's node key is not
/// read; with a mark that names format 2 the floor is what this build's `READS_FORMAT` says
/// reads it ([`FormatFloor::of`]: `Unread` while it names no release), and only for `Unread` is
/// this Mac's own node key read, never minted, to name its row.
///
/// NEGATIVE CONTROL, run from the test's first line and seen to fail: `Mutant::MarkUnread`
/// (`format_floor` reads no mark) with ``assertion `left == right` failed: a mark of format
/// 2``, `left: Every`, `right: Unread`.
#[cfg(unix)]
#[test]
fn the_floor_is_read_off_this_macs_own_format_mark() {
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};

    let home = aterm_tempfile::tempdir().expect("a home");
    let state = home.path().join("state");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&state)
        .expect("the state directory");
    let paths = aterm_secrets::client::Paths::locate_for_home(home.path());
    let needs = |paths: Option<&aterm_secrets::client::Paths>| {
        gather_here(&state, paths, Err("fleet survey not read".to_owned())).2
    };
    let reads = aterm_secrets::commons::hearing::READS_FORMAT
        .map(|release| (u64::from(release.0), u64::from(release.1)));
    let none = Needs {
        crossed: false,
        format: FormatFloor::Every,
        own: None,
    };

    assert_eq!(needs(None), none, "no vault");
    assert_eq!(needs(Some(&paths)), none, "no mark");

    let mark = paths.records_file().with_file_name("records.format");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(mark.parent().expect("the vault's folder"))
        .expect("the vault's folder");
    let write = |text: &str| {
        let _ = std::fs::remove_file(&mark);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&mark)
            .expect("the format mark");
        std::io::Write::write_all(&mut file, text.as_bytes()).expect("its text");
    };
    write("aterm-secrets records format 2\n");
    let floor = FormatFloor::of(Some(2), reads);
    assert_ne!(floor, FormatFloor::Every);
    assert_eq!(needs(Some(&paths)).format, floor, "a mark of format 2");
    // No node key is minted to name a row: none is held, so none is read.
    assert_eq!(needs(Some(&paths)).own, None);
    assert!(
        aterm_net::fleet_member::read_node_identity(&state)
            .expect("the state directory")
            .is_none(),
        "a display mints no node key"
    );

    let me = aterm_net::fleet_member::node_identity(&state).expect("this Mac's node");
    let own = (floor == FormatFloor::Unread).then(|| *me.key().bytes());
    assert_eq!(
        needs(Some(&paths)),
        Needs {
            crossed: false,
            format: floor,
            own,
        }
    );

    // A mark that does not read claims nothing, and a Mac with no mark reads no key.
    write("garbage");
    assert_eq!(needs(Some(&paths)), none, "a mark that does not read");
    std::fs::remove_file(&mark).expect("the mark removed");
    assert_eq!(needs(Some(&paths)), none, "no mark, a node key held");
}

/// A MAC ITS VAULT'S SOURCE REFUSED FOR A LATER FORMAT NAMES ITS OWN ROW ([`format_floor`]):
/// with no format mark, the note a records pass keeps where the source refused it for a later
/// format than its build reads (`aterm_secrets::client::told_later_format`) reads as
/// [`FormatFloor::Unread`], so this Mac's own row needs an update and no other row is named;
/// with the note gone the floor is `Every` again. A mark, where one is stored, says the floor
/// as it did, the note beside it read for nothing.
///
/// NEGATIVE CONTROL: `format_floor` reading no note (as before), run against this test and seen
/// to fail with ``assertion `left == right` failed: a source's refusal names this Mac's own
/// row``, `left: Every`, `right: Unread`.
#[cfg(unix)]
#[test]
fn a_mac_its_source_refused_for_a_later_format_names_its_own_row() {
    use std::os::unix::fs::DirBuilderExt as _;

    let home = aterm_tempfile::tempdir().expect("a home");
    let paths = aterm_secrets::client::Paths::locate_for_home(home.path());
    let note = paths.records_file().with_file_name("records.later-format");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(note.parent().expect("the vault's folder"))
        .expect("the vault's folder");
    assert_eq!(format_floor(Some(&paths)), FormatFloor::Every, "no note");
    std::fs::write(&note, b"aterm-secrets records refused for a later format\n").expect("the note");
    assert_eq!(
        format_floor(Some(&paths)),
        FormatFloor::Unread,
        "a source's refusal names this Mac's own row"
    );
    let mark = paths.records_file().with_file_name("records.format");
    std::fs::write(&mark, b"aterm-secrets records format 2\n").expect("the mark");
    let reads = aterm_secrets::commons::hearing::READS_FORMAT
        .map(|release| (u64::from(release.0), u64::from(release.1)));
    assert_eq!(
        format_floor(Some(&paths)),
        FormatFloor::of(Some(2), reads),
        "a mark says the floor"
    );
    std::fs::remove_file(&mark).expect("the mark removed");
    std::fs::remove_file(&note).expect("the note removed");
    assert_eq!(
        format_floor(Some(&paths)),
        FormatFloor::Every,
        "the note gone"
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
        format: FormatFloor::Every,
        own: None,
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
        format: FormatFloor::Every,
        own: None,
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
