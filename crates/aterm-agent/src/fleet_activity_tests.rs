// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_types::fleet_access::{DriveKind, FleetAccess};
use aterm_types::fleet_activity::Way;

const A: &str = "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a";
const B: &str = "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b";
const SID: &str = "s-0123456789abcdef0123";

/// 2026-11-12T10:00:00Z, in Unix milliseconds.
const NOW: u64 = 1_794_477_600_000;

const BLOCKED: Outcome = Outcome::Blocked {
    level: FleetAccess::ReadOnly,
    needs: FleetAccess::Full,
};

fn row(ago_s: u64, way: Way, node: &str, host: &str, kind: DriveKind, outcome: Outcome) -> Row {
    Row::of(
        NOW - ago_s * 1000,
        way,
        (node, Some(host)),
        kind,
        Some(SID),
        outcome,
    )
    .expect("an agent request")
}

/// A state folder whose record holds: m85 asked for two new jobs this Mac blocked and typed one
/// line (20, 10 and 5 minutes ago); this Mac read m18 61 times in one minute (4 minutes ago);
/// and a close m18 asked for three hours ago.
fn scratch() -> aterm_tempfile::TempDir {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let record = Record::in_state(dir.path());
    for row in [
        row(
            3 * 3_600,
            Way::In,
            A,
            "m18",
            DriveKind::Close,
            Outcome::Admitted,
        ),
        row(20 * 60, Way::In, B, "m85", DriveKind::Spawn, BLOCKED),
        row(10 * 60, Way::In, B, "m85", DriveKind::Spawn, BLOCKED),
        row(
            5 * 60,
            Way::In,
            B,
            "m85",
            DriveKind::Turn,
            Outcome::Admitted,
        ),
        Row {
            n: 61,
            by: None,
            ..row(
                4 * 60,
                Way::Out,
                A,
                "m18",
                DriveKind::Read,
                Outcome::Admitted,
            )
        },
    ] {
        record.append(&[row]).expect("written");
    }
    dir
}

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_owned).collect()
}

/// The text: the summary's line, a line for each Mac and way (busiest first), a blank line, and
/// the window's rows, newest first. Every part is the model's own words
/// (`aterm_types::fleet_activity`), laid out in columns.
#[test]
fn the_text_is_the_headline_then_each_mac_and_way_then_the_rows_newest_first() {
    let dir = scratch();
    let text = answer(dir.path(), &[], NOW).expect("answered");
    assert_eq!(
        text,
        format!(
            "Last hour \u{b7} in from 1 Mac \u{b7} out to 1 \u{b7} 2 blocked\n\
             m18  \u{2192}  61  reads                     4m\n\
             m85  \u{2190}  3   2 new jobs, both blocked  5m\n\
             \n\
             2026-11-12T09:56:00Z  \u{2192}  m18  read x61  admitted\n\
             2026-11-12T09:55:00Z  \u{2190}  m85  typed     admitted                     {SID}\n\
             2026-11-12T09:50:00Z  \u{2190}  m85  new-job   blocked at read, needs full  {SID}\n\
             2026-11-12T09:40:00Z  \u{2190}  m85  new-job   blocked at read, needs full  {SID}\n"
        )
    );
    // A longer window takes the close in, and a shorter one leaves the first new job out.
    let day = answer(dir.path(), &words("--since 1d"), NOW).expect("answered");
    assert!(day.starts_with("Last 1d \u{b7} in from 2 Macs \u{b7} out to 1 \u{b7} 2 blocked\n"));
    assert!(
        day.ends_with(&format!(
            "2026-11-12T07:00:00Z  \u{2190}  m18  close     admitted                     {SID}\n"
        )),
        "{day}"
    );
    let short = answer(dir.path(), &words("--since=15m"), NOW).expect("answered");
    assert!(
        short.starts_with("Last 15m \u{b7} in from 1 Mac \u{b7} out to 1 \u{b7} 1 blocked\n"),
        "{short}"
    );
    assert_eq!(short.matches("new-job").count(), 1, "{short}");
    // A window with nothing in it says how long the record has been quiet, and names no row.
    let quiet = answer(dir.path(), &words("--since 2m"), NOW).expect("answered");
    assert_eq!(quiet, "Quiet for 4m\n");
    let empty = aterm_tempfile::tempdir().expect("scratch");
    assert_eq!(answer(empty.path(), &[], NOW).expect("answered"), "Quiet\n");
}

#[test]
fn the_json_holds_the_summary_the_pairs_and_the_rows() {
    let dir = scratch();
    let text = answer(dir.path(), &words("--json"), NOW).expect("answered");
    assert!(text.ends_with('\n') && text.matches('\n').count() == 1);
    let json: Value = aterm_json::from_str(&text).expect("JSON");
    assert_eq!(
        json["headline"],
        Value::from("Last hour \u{b7} in from 1 Mac \u{b7} out to 1 \u{b7} 2 blocked")
    );
    for (key, want) in [
        ("now_ms", NOW),
        ("window_ms", WINDOW_MS),
        ("requests", 64),
        ("macs_in", 1),
        ("macs_out", 1),
        ("blocked_in", 2),
        ("blocked_out", 0),
        ("last_blocked_in_ms", NOW - 600_000),
        ("last_ms", NOW - 240_000),
    ] {
        assert_eq!(json[key], Value::from(want), "{key}");
    }
    assert_eq!(json["window"], Value::from("1h"));
    assert_eq!(json["types"]["read"], Value::from(61_u64));
    assert_eq!(json["types"]["new-job"], Value::from(2_u64));
    assert_eq!(json["types"]["close"], Value::from(0_u64));
    let Value::Array(pairs) = &json["pairs"] else {
        panic!("pairs: {text}");
    };
    assert_eq!(pairs.len(), 2);
    assert_eq!(
        pairs[1],
        json!({
            "node": B,
            "host": "m85",
            "way": "in",
            "count": 3_u64,
            "types": {"read": 0_u64, "message": 0_u64, "typed": 1_u64, "hold": 0_u64,
                      "close": 0_u64, "new-job": 2_u64},
            "blocked": {"read": 0_u64, "message": 0_u64, "typed": 0_u64, "hold": 0_u64,
                        "close": 0_u64, "new-job": 2_u64},
            "phrase": "2 new jobs, both blocked",
            "last_ms": NOW - 300_000,
            "routine": false,
        })
    );
    let Value::Array(rows) = &json["rows"] else {
        panic!("rows: {text}");
    };
    assert_eq!(rows.len(), 4, "the window's rows");
    assert_eq!(
        rows[0],
        json!({
            "at_ms": NOW - 240_000,
            "at": "2026-11-12T09:56:00Z",
            "way": "out",
            "node": A,
            "host": "m18",
            "type": "read",
            "n": 61_u64,
            "outcome": "admitted",
            "level": Value::Null,
            "needs": Value::Null,
            "by": Value::Null,
        })
    );
    assert_eq!(
        rows[3],
        json!({
            "at_ms": NOW - 1_200_000,
            "at": "2026-11-12T09:40:00Z",
            "way": "in",
            "node": B,
            "host": "m85",
            "type": "new-job",
            "n": 1_u64,
            "outcome": "blocked",
            "level": "read",
            "needs": "full",
            "by": SID,
        })
    );
}

#[test]
fn a_word_that_is_no_option_is_a_usage_error() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    for bad in [
        "extra",
        "--json --json",
        "--since",
        "--since soon",
        "--since 5s",
        "--since 8d",
        "--since 1h --since 2h",
        "--since=",
    ] {
        let refused = answer(dir.path(), &words(bad), NOW).expect_err(bad);
        assert!(
            refused.contains("--since") || refused.contains("unexpected"),
            "{bad}: {refused}"
        );
    }
    assert_eq!(parse(&[]), Ok((false, WINDOW_MS)));
    assert_eq!(parse(&words("--since 90s --json")), Ok((true, 90_000)));
    assert_eq!(
        parse(&words("--since 7d")),
        Ok((false, aterm_ctl::fleet_activity::MAX_AGE_MS))
    );
}

/// A Mac is named as the book names its node now: a row written under an address, or under a
/// name the Mac has since changed, reads with the book's name; a node the book does not hold
/// keeps the name its row has, and one with neither is named by its node's head.
#[test]
fn a_reader_names_each_mac_as_the_book_names_its_node_now() {
    let names = Names(vec![(A.to_owned(), "m18".to_owned())]);
    let rows = names.naming(vec![
        row(
            60,
            Way::In,
            A,
            "192.168.1.172",
            DriveKind::Turn,
            Outcome::Admitted,
        ),
        row(50, Way::In, B, "m85", DriveKind::Turn, Outcome::Admitted),
        Row {
            host: None,
            ..row(40, Way::Out, A, "x", DriveKind::Turn, Outcome::Admitted)
        },
    ]);
    let hosts: Vec<Option<&str>> = rows.iter().map(|row| row.host.as_deref()).collect();
    assert_eq!(hosts, [Some("m18"), Some("m85"), Some("m18")]);
    assert_eq!(mac(None, B), "0b0b0b0b");
    assert_eq!(mac(Some("m85"), B), "m85");
}

/// The book's names are the ones its members' keys bind: a member's primary name, read from
/// the state folder's book.
#[test]
fn the_names_are_the_books_bound_members() {
    use aterm_net::fleet::{FleetSecret, NodeIdentity};
    use aterm_net::fleet_member::{self as member, Began, Peer};
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let state = dir.path().join("state");
    aterm_uds::private_file::ensure_private_dir(&state).expect("a private folder");
    assert_eq!(Names::of(&state), Names::default(), "no book, no names");
    let named = NodeIdentity::from_seed(A, &[11; 32]).expect("an identity");
    let unnamed = NodeIdentity::from_seed(B, &[12; 32]).expect("an identity");
    let secret = [4; member::SECRET_LEN];
    let mark = FleetSecret::new(&secret).expect("a secret").mark();
    let mut first = Peer::own(&named, ("192.168.1.11", 7100, "0.150.0"), 1)
        .with_primary_name(&named, Some("m18"));
    first.fleet = Some(mark);
    let mut second = Peer::own(&unnamed, ("192.168.1.12", 7100, "0.150.0"), 1);
    second.fleet = Some(mark);
    member::write_held(&state, Began::Here, |book| {
        book.write_key(&secret).map_err(|e| e.to_string())?;
        book.update(|peers| {
            peers.extend([first, second]);
            Ok(())
        })
    })
    .expect("the book's lock")
    .expect("written");
    let names = Names::of(&state);
    assert_eq!(names.of_node(named.node()), Some("m18"));
    assert_eq!(names.of_node(unnamed.node()), Some("192.168.1.12"));
    assert_eq!(names.of_node("0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c"), None);
}

/// A reader that looks again and again reads the file only when it was written since, and adds
/// the rows it holds up again at each look: a row leaves the window by the clock alone.
#[test]
fn a_watch_reads_again_what_was_written_and_ages_what_it_holds() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let record = Record::in_state(dir.path());
    let mut watch = Watch::in_state(dir.path());
    assert_eq!(watch.summary(NOW, WINDOW_MS).headline(NOW), "Quiet");
    record
        .append(&[row(60, Way::In, B, "m85", DriveKind::Spawn, BLOCKED)])
        .expect("written");
    let summary = watch.summary(NOW, WINDOW_MS);
    assert_eq!((summary.count(), summary.blocked_in), (1, 1));
    assert_eq!(summary.last_blocked_in_ms, Some(NOW - 60_000));
    assert_eq!(watch.rows().len(), 1);
    // Nothing written since: the same rows, an hour on, are out of the window.
    let later = NOW + WINDOW_MS;
    let summary = watch.summary(later, WINDOW_MS);
    assert_eq!((summary.count(), summary.blocked_in), (0, 0));
    assert_eq!(summary.headline(later), "Quiet for 1h");
    // A row written since is read at the next look.
    record
        .append(&[Row {
            at_ms: later,
            ..row(0, Way::Out, A, "m18", DriveKind::Turn, Outcome::Admitted)
        }])
        .expect("written");
    let summary = watch.summary(later, WINDOW_MS);
    assert_eq!((summary.count(), summary.peers_out), (1, 1));
    assert_eq!(watch.rows().len(), 2);
}

/// A MAC IS NAMED AT THE NEXT LOOK AFTER THE BOOK NAMES IT, though nothing wrote the record
/// since; and a row the record no longer keeps at its age leaves what the watch holds too, as
/// it would leave a fresh read. NEGATIVE CONTROL: a watch that reads the names only with the
/// record keeps the address, and one that never drops a held row keeps it past its week.
#[test]
fn a_watch_names_a_mac_once_the_book_does_and_drops_a_row_past_its_age() {
    use aterm_net::fleet::{FleetSecret, NodeIdentity};
    use aterm_net::fleet_member::{self as member, Began, Peer};
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let state = dir.path().join("state");
    aterm_uds::private_file::ensure_private_dir(&state).expect("a private folder");
    let named = NodeIdentity::from_seed(A, &[11; 32]).expect("an identity");
    let record = Record::in_state(&state);
    let mut watch = Watch::in_state(&state);
    let turn = Row {
        node: named.node().to_owned(),
        ..row(
            60,
            Way::In,
            A,
            "192.168.1.11",
            DriveKind::Turn,
            Outcome::Admitted,
        )
    };
    record.append(&[turn]).expect("written");
    let summary = watch.summary(NOW, WINDOW_MS);
    assert_eq!(summary.pairs[0].name(), "192.168.1.11");
    let secret = [4; member::SECRET_LEN];
    let mut peer = Peer::own(&named, ("192.168.1.11", 7100, "0.150.0"), 1)
        .with_primary_name(&named, Some("m18"));
    peer.fleet = Some(FleetSecret::new(&secret).expect("a secret").mark());
    member::write_held(&state, Began::Here, |book| {
        book.write_key(&secret).map_err(|e| e.to_string())?;
        book.update(|peers| {
            peers.push(peer);
            Ok(())
        })
    })
    .expect("the book's lock")
    .expect("written");
    let summary = watch.summary(NOW, WINDOW_MS);
    assert_eq!(summary.pairs[0].name(), "m18");
    assert_eq!(watch.rows()[0].host.as_deref(), Some("m18"));
    assert_eq!(read(&state, NOW, WINDOW_MS).1, summary, "as a fresh read");
    // A week and a minute on, with nothing written: the row is gone from the watch, as from
    // the record.
    let later = NOW + aterm_ctl::fleet_activity::MAX_AGE_MS;
    let summary = watch.summary(later, WINDOW_MS);
    assert_eq!(watch.rows(), []);
    assert_eq!(summary.last_ms, None);
    assert_eq!(read(&state, later, WINDOW_MS).1, summary, "as a fresh read");
}

/// A row stamped after the clock (the clock was set back) is read as written at the look that
/// first sees it, once: it then leaves the hour like any other row, where its own stamp would
/// keep it, and a block's mark, for as long as the clock had been ahead.
#[test]
fn a_watch_settles_a_row_stamped_after_the_clock_and_lets_it_age() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let record = Record::in_state(dir.path());
    let mut watch = Watch::in_state(dir.path());
    let ahead = NOW + 30 * 24 * WINDOW_MS;
    record
        .append(&[Row {
            at_ms: ahead,
            ..row(0, Way::In, B, "m85", DriveKind::Spawn, BLOCKED)
        }])
        .expect("written");
    let summary = watch.summary(NOW, WINDOW_MS);
    assert_eq!(summary.last_blocked_in_ms, Some(NOW));
    let later = NOW + WINDOW_MS + 1;
    let summary = watch.summary(later, WINDOW_MS);
    assert_eq!((summary.blocked_in, summary.last_blocked_in_ms), (0, None));
    assert_eq!(watch.rows()[0].at_ms, NOW, "settled once");
}

/// A record that dropped rows inside the window says so: the summary covers the time since,
/// from a watch and from a fresh read alike, and `--json` carries it.
#[test]
fn a_cut_record_is_summarized_since_its_cut() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let record = Record::in_state(dir.path());
    let turn = row(60, Way::In, B, "m85", DriveKind::Turn, Outcome::Admitted);
    record.append(std::slice::from_ref(&turn)).expect("written");
    let cut = NOW - 20 * 60_000;
    std::fs::write(record.path(), format!("cut={cut}\n{}\n", turn.line())).expect("written");
    let mut watch = Watch::in_state(dir.path());
    let summary = watch.summary(NOW, WINDOW_MS);
    assert_eq!(summary.cut_ms, Some(cut));
    let (rows, fresh) = read(dir.path(), NOW, WINDOW_MS);
    assert_eq!(fresh, summary);
    assert_eq!(rows, [turn]);
    assert_eq!(to_json(&rows, &summary, NOW)["cut_ms"], json!(cut));
    // Two hours on the cut is out of the window, which is whole again.
    let later = NOW + 2 * WINDOW_MS;
    assert_eq!(watch.summary(later, WINDOW_MS).cut_ms, None);
}
