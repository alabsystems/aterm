// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// 2026-11-05T00:00:00Z, the wall clock's start in every case here.
const T0_MS: u64 = 1_793_836_800_000;

const S: Duration = Duration::from_secs(1);

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).expect("a test's time fits")
}

fn row(at_ms: u64) -> PresenceRow {
    PresenceRow {
        at_ms,
        build: Build::new("0.155.0+g0123abcd").expect("a build"),
        addresses: vec![
            "192.168.1.22:7480".parse().expect("an address"),
            "[fd00::22]:7480".parse().expect("an address"),
        ],
        membership: Some(Held::membership(&BTreeSet::from([[1; 32], [2; 32]]))),
        tables: Some(Held::tables(
            &BTreeMap::from([(
                [9; 32],
                Held {
                    digest: [3; 32],
                    count: 12,
                },
            )]),
            &BTreeMap::new(),
        )),
        topics: vec!["build".to_owned(), "deploy".to_owned()],
    }
}

/// A board and the times it is fed: the links' clock from zero and the wall clock from
/// [`T0_MS`], moved together.
struct At {
    board: Board,
    now: Duration,
}

impl At {
    fn new() -> Self {
        Self {
            board: Board::new(BTreeMap::new()),
            now: Duration::ZERO,
        }
    }

    fn wall(&self) -> u64 {
        T0_MS + ms(self.now)
    }

    fn pass(&mut self, by: Duration) {
        self.now += by;
    }

    /// `who`'s row `seq`, written now by its own clock, arriving live now.
    fn live(&mut self, who: &str, seq: u64) -> Took {
        let wall = self.wall();
        self.board.offer(who, seq, row(wall), true, self.now, wall)
    }

    fn up(&self, who: &str) -> bool {
        self.board.is_up(who, self.now)
    }
}

const X: &str = "n-00000000000000aa";

/// §10 "Presence": a Mac whose rows stop reads down once 90 seconds of this Mac's clock pass
/// after its last advance, with the time it was last seen up; nothing but the clock moved.
#[test]
fn a_mac_whose_rows_stop_reads_down_after_90_seconds() {
    let mut at = At::new();
    assert_eq!(at.live(X, 1), Took::Advanced, "a live, fresh first row");
    at.pass(30 * S);
    assert_eq!(at.live(X, 2), Took::Advanced);
    let last = at.wall();
    at.pass(89 * S);
    assert!(at.up(X), "89 s after its last row it is still up");
    at.pass(S);
    assert_eq!(
        at.board.standing(X, at.now),
        Standing::Silent {
            last_seen_ms: Some(last)
        },
        "90 s after its last row it reads silent, last seen at that row"
    );
}

/// §10 "Presence": a Mac whose row advances every 30 seconds never reads down, at any moment
/// between its rows.
#[test]
fn a_mac_whose_row_advances_never_reads_down() {
    let mut at = At::new();
    for seq in 1..=200 {
        assert_eq!(at.live(X, seq), Took::Advanced);
        for _ in 0..30 {
            assert!(at.up(X), "up {} s in", ms(at.now) / 1000);
            at.pass(S);
        }
    }
    assert!(at.up(X), "30 s after its last row it is up");
}

/// §10 "Presence": a restarted Mac's first row wins at once: its number is in a new
/// incarnation, above every number of its last life, so it advances the seat and replaces
/// the row held.
#[test]
fn a_restarted_macs_first_row_wins() {
    let mut at = At::new();
    let old = astream_broker::incarnation_base(7).expect("an incarnation") + 40;
    assert_eq!(at.live(X, old), Took::Advanced);
    at.pass(5 * 60 * S);
    assert!(!at.up(X), "silent while it restarts");
    let first = astream_broker::incarnation_base(8).expect("an incarnation");
    let wall = at.wall();
    let mut restarted = row(wall);
    restarted.build = Build::new("0.156.0+g89abcdef").expect("a build");
    assert_eq!(
        at.board
            .offer(X, first, restarted.clone(), true, at.now, wall),
        Took::Advanced,
        "its first row wins"
    );
    assert!(at.up(X), "up at once");
    assert_eq!(at.board.row(X), Some((first, &restarted)));
}

/// §10 "Presence": a late, older row never replaces a newer one, and never keeps a seat up.
#[test]
fn a_late_older_row_never_replaces_a_newer_one() {
    let mut at = At::new();
    assert_eq!(at.live(X, 10), Took::Advanced);
    let newest = at.board.row(X).map(|(_, row)| row.clone());
    at.pass(80 * S);
    let wall = at.wall();
    assert_eq!(
        at.board.offer(X, 9, row(wall), true, at.now, wall),
        Took::Kept,
        "a lower number is kept out"
    );
    assert_eq!(
        at.board.offer(X, 10, row(wall), true, at.now, wall),
        Took::Kept,
        "the same number is kept out"
    );
    assert_eq!(at.board.row(X).map(|(_, row)| row.clone()), newest);
    at.pass(10 * S);
    assert!(
        !at.up(X),
        "90 s after its newest row it reads silent: the late row refreshed nothing"
    );
}

/// A row that was in the log before the tail started is held but advances nothing, and the
/// first live row above it advances the seat.
#[test]
fn a_row_held_before_the_tail_started_advances_nothing() {
    let mut at = At::new();
    let wall = at.wall();
    assert_eq!(
        at.board.offer(X, 5, row(wall), false, at.now, wall),
        Took::Held
    );
    assert!(!at.up(X), "a row read at the start says nothing about now");
    assert_eq!(at.board.last_seen(X), None);
    assert_eq!(at.live(X, 6), Took::Advanced);
    assert!(at.up(X));
}

/// The freshness rule: rows a Mac receives in a burst on its return (a resumed cursor, the
/// newest rows of a new connection) that their origin wrote more than 10 minutes ago are
/// held, newest last, but they advance nothing, so a Mac that went silent while this one
/// slept reads silent, not up. Answers what the burst's last row did, and whether X then
/// reads up and was ever seen up.
fn backlog_reads(stale_rows_up: bool) -> (Took, bool, Option<u64>) {
    STALE_ROWS_UP.with(|on| on.set(stale_rows_up));
    let mut at = At::new();
    STALE_ROWS_UP.with(|on| on.set(false));
    at.pass(3 * 24 * 60 * 60 * S);
    let wall = at.wall();
    // X's row of 2 days ago, then its last, 11 minutes ago.
    let two_days = ms(2 * 24 * 60 * 60 * S);
    assert_eq!(
        at.board
            .offer(X, 1, row(wall - two_days), true, at.now, wall),
        if stale_rows_up {
            Took::Advanced
        } else {
            Took::Held
        }
    );
    let took = at
        .board
        .offer(X, 2, row(wall - ms(11 * 60 * S)), true, at.now, wall);
    (took, at.up(X), at.board.last_seen(X))
}

#[test]
fn a_backlog_advances_nothing() {
    assert_eq!(backlog_reads(false), (Took::Held, false, None));
    let mut at = At::new();
    at.pass(60 * 60 * S);
    let wall = at.wall();
    assert_eq!(
        at.board
            .offer(X, 1, row(wall - ms(9 * 60 * S)), true, at.now, wall),
        Took::Advanced,
        "a row written 9 minutes ago is no backlog"
    );
}

#[test]
fn an_implausibly_future_row_does_not_refresh_presence() {
    let mut at = At::new();
    let wall = at.wall();
    let edge = wall + ms(FRESH);
    assert_eq!(
        at.board.offer(X, 1, row(edge), true, at.now, wall),
        Took::Advanced,
        "the permitted clock-skew boundary is inclusive"
    );
    let seen = at.board.last_seen(X);
    at.pass(UP_FOR);
    let wall = at.wall();
    assert_eq!(
        at.board
            .offer(X, 2, row(wall + ms(FRESH) + 1), true, at.now, wall),
        Took::Held
    );
    assert!(!at.up(X), "a future-stamped replay cannot renew liveness");
    assert_eq!(at.board.last_seen(X), seen);
    assert_eq!(at.board.row(X).map(|(seq, _)| seq), Some(2));
}

/// NEGATIVE CONTROL (`STALE_ROWS_UP`): with the freshness rule off, the backlog's last row
/// advances the seat, so a Mac silent for 11 minutes reads up.
#[test]
fn with_stale_rows_up_a_backlog_reads_up() {
    let (took, up, seen) = backlog_reads(true);
    assert_eq!(took, Took::Advanced);
    assert!(up, "the control restores receipt alone");
    assert!(seen.is_some());
}

/// Last seen is written when it changed and at most once per 10 minutes of the links'
/// clock, the first at once; only the seats kept stay; and a board made from what was
/// written reads each seat silent since that time.
#[test]
fn last_seen_is_written_at_most_once_per_10_minutes_and_read_back() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("ap")
        .tempdir_in("/tmp")
        .expect("a scratch dir");
    aterm_uds::private_file::ensure_private_dir(dir.path()).expect("private");
    let y = "n-00000000000000bb";
    let gone = "n-00000000000000cc";
    let keep = BTreeSet::from([X.to_owned(), y.to_owned()]);
    let mut at = At::new();
    let mut writes = Vec::new();
    for seq in 1..=(60 * 2) {
        for who in [X, y, gone] {
            at.live(who, seq);
        }
        if let Some(text) = at.board.due(at.now, Some(&keep)) {
            aterm_uds::private_file::write_private(dir.path(), SEEN_FILE, text.as_bytes())
                .expect("written");
            at.board.wrote(at.now);
            writes.push(at.now);
        }
        at.pass(30 * S);
    }
    assert_eq!(
        writes,
        [0, 10, 20, 30, 40, 50].map(|m| m * 60 * S),
        "the first at once, then once per 10 minutes"
    );
    let held = read_seen(dir.path());
    assert_eq!(
        held.keys().map(String::as_str).collect::<Vec<_>>(),
        [X, y],
        "a seat no longer kept goes"
    );
    let x_seen = held[X];
    assert_eq!(x_seen, T0_MS + ms(50 * 60 * S), "the last write's time");
    // A restart: no row yet, every seat silent since what was written, days later.
    let board = Board::new(held);
    let later = Duration::from_secs(3 * 24 * 60 * 60);
    assert_eq!(
        board.standing(X, later),
        Standing::Silent {
            last_seen_ms: Some(x_seen)
        }
    );
    assert_eq!(
        board.standing(gone, later),
        Standing::Silent { last_seen_ms: None }
    );
}

#[test]
fn a_damaged_seen_file_reads_each_good_line() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("ap")
        .tempdir_in("/tmp")
        .expect("a scratch dir");
    aterm_uds::private_file::ensure_private_dir(dir.path()).expect("private");
    aterm_uds::private_file::write_private(
        dir.path(),
        SEEN_FILE,
        format!("{X} 17\nn-zz 3\nnot a line\nn-00000000000000bb x\n").as_bytes(),
    )
    .expect("written");
    assert_eq!(read_seen(dir.path()), BTreeMap::from([(X.to_owned(), 17)]));
    assert!(read_seen(&dir.path().join("none")).is_empty());
}

#[test]
fn a_row_reads_back_as_itself() {
    let mut full = row(T0_MS);
    full.addresses = (0..ADDRESSES_MAX)
        .map(|n| SocketAddr::from(([10, 0, 0, u8::try_from(n).expect("small")], 7480)))
        .collect();
    for r in [
        row(T0_MS),
        full,
        PresenceRow {
            at_ms: 0,
            build: Build::new("u").expect("a build"),
            addresses: Vec::new(),
            membership: None,
            tables: None,
            topics: Vec::new(),
        },
    ] {
        assert_eq!(PresenceRow::decode(&r.encode()), Ok(r));
    }
    let size = row(T0_MS).encode().len();
    assert!(
        size < 300,
        "a presence row is about 300 bytes at most: {size}"
    );
}

/// Past its bounds a row is written to them: its first addresses, no zone.
#[test]
fn a_writer_writes_within_the_bounds() {
    let mut many = row(T0_MS);
    many.addresses = (0..40)
        .map(|n| SocketAddr::from(([10, 0, 0, n], 7480)))
        .collect();
    let read = PresenceRow::decode(&many.encode()).expect("a row");
    assert_eq!(read.addresses, many.addresses[..ADDRESSES_MAX]);
    let zoned: SocketAddr = "[fe80::1%4]:7480".parse().expect("an address");
    assert_eq!(
        travelling(zoned),
        "[fe80::1]:7480".parse::<SocketAddr>().expect("an address")
    );
}

#[test]
fn a_row_is_read_strictly() {
    let good = row(T0_MS).encode();
    let mut trailing = good.clone();
    trailing.push(0);
    let mut kind = good.clone();
    kind[0] = b'Q';
    let mut version = good.clone();
    version[1] = VERSION + 1;
    // The varint of `at_ms` spelled one byte longer.
    let mut long = vec![KIND, VERSION, 0x80, 0x00];
    long.extend_from_slice(&good[2 + varint_len(T0_MS)..]);
    let mut tag = PresenceRow {
        membership: None,
        tables: None,
        topics: Vec::new(),
        ..row(T0_MS)
    }
    .encode();
    // The tables' flag, before the topics' count.
    let at = tag.len() - 2;
    tag[at] = 2;
    for (bytes, refused) in [
        (&good[..good.len() - 1], RowError::Truncated),
        (&trailing[..], RowError::Trailing),
        (&kind[..], RowError::Kind(b'Q')),
        (&version[..], RowError::Version(VERSION + 1)),
        (&long[..], RowError::Varint),
    ] {
        assert_eq!(PresenceRow::decode(bytes), Err(refused));
    }
    assert_eq!(PresenceRow::decode(&tag), Err(RowError::Tag), "a flag of 2");
    let mut bad_build = vec![KIND, VERSION, 0, 3];
    bad_build.extend_from_slice(b"a/b");
    assert_eq!(PresenceRow::decode(&bad_build), Err(RowError::Build));
    let mut many = vec![KIND, VERSION, 0, 1, b'u'];
    put_varint(&mut many, u64::try_from(ADDRESSES_MAX + 1).expect("small"));
    assert_eq!(PresenceRow::decode(&many), Err(RowError::TooMany));
    let family = [KIND, VERSION, 0, 1, b'u', 1, 5];
    assert_eq!(PresenceRow::decode(&family), Err(RowError::Family(5)));
}

/// A row names the topics held each once, in order, in their grammar, its first
/// [`TOPICS_MAX`]; a reader refuses any other list rather than misread whom to push to.
#[test]
fn a_rows_topics_are_written_in_order_and_read_strictly() {
    let held: Vec<String> = ["deploy", "build", "deploy", "Not A Topic", ""]
        .iter()
        .map(|t| (*t).to_owned())
        .chain((0..40).map(|n| format!("t{n:02}")))
        .collect();
    let named = row_topics(&held);
    assert_eq!(named.len(), TOPICS_MAX);
    assert_eq!(&named[..2], ["build", "deploy"]);
    assert!(named.windows(2).all(|w| w[0] < w[1]), "{named:?}");
    let written = PresenceRow {
        topics: held,
        ..row(T0_MS)
    };
    let read = PresenceRow::decode(&written.encode()).expect("a row");
    assert_eq!(read.topics, named, "the row names what row_topics names");

    // A row with no topics ends in a zero count; each case below replaces it.
    let mut bare = PresenceRow {
        topics: Vec::new(),
        ..row(T0_MS)
    }
    .encode();
    assert_eq!(bare.pop(), Some(0));
    let with = |topics: &[&str], count: usize| {
        let mut bytes = bare.clone();
        put_varint(&mut bytes, u64::try_from(count).expect("small"));
        for t in topics {
            bytes.push(u8::try_from(t.len()).expect("short"));
            bytes.extend_from_slice(t.as_bytes());
        }
        bytes
    };
    // The control: in order, once each, in the grammar, reads.
    let good = PresenceRow::decode(&with(&["a", "b.c"], 2)).expect("a row");
    assert_eq!(good.topics, ["a", "b.c"]);
    let past: Vec<String> = (0..=TOPICS_MAX).map(|n| format!("t{n:02}")).collect();
    let past: Vec<&str> = past.iter().map(String::as_str).collect();
    for (topics, why) in [
        (&["b", "a"][..], "out of order"),
        (&["a", "a"][..], "twice"),
        (&["A"][..], "outside the grammar"),
        (&[""][..], "empty"),
        (&past[..], "past the bound"),
    ] {
        assert_eq!(
            PresenceRow::decode(&with(topics, topics.len())),
            Err(RowError::Topic),
            "{why}"
        );
    }
    let mut not_utf8 = bare.clone();
    not_utf8.extend_from_slice(&[1, 1, 0xff]);
    assert_eq!(PresenceRow::decode(&not_utf8), Err(RowError::Topic));
    assert_eq!(
        PresenceRow::decode(&with(&["a"], 2)),
        Err(RowError::Truncated),
        "a count past the topics"
    );
}

fn varint_len(v: u64) -> usize {
    let mut out = Vec::new();
    put_varint(&mut out, v);
    out.len()
}

#[test]
fn a_build_is_a_word_not_a_path() {
    for good in ["0.155.0", "0.155.0+g0123abcd", "0.155.0-dev.1_x"] {
        assert!(Build::new(good).is_some(), "{good}");
    }
    for bad in [
        "",
        "-x",
        "/usr/bin",
        "a b",
        "x@y",
        &"9".repeat(BUILD_MAX + 1),
    ] {
        assert!(Build::new(bad).is_none(), "{bad}");
    }
    assert_eq!(
        Build::running().as_str(),
        aterm_stamp::build_id(),
        "this build's id is a build word"
    );
}

/// The two digests name what is held, not how it was gathered: the same ids give the same
/// digest whatever order they arrive in, one more id gives another, and a table's own
/// summaries feed the lab's, its conflict records apart from its operations.
#[test]
fn a_digest_names_what_is_held() {
    let ids: BTreeSet<[u8; 32]> = (0..50u8).map(|n| [n; 32]).collect();
    let reversed: BTreeSet<[u8; 32]> = (0..50u8).rev().map(|n| [n; 32]).collect();
    let one = Held::membership(&ids);
    assert_eq!(one, Held::membership(&reversed));
    assert_eq!(one.count, 50);
    let mut more = ids.clone();
    more.insert([99; 32]);
    assert_ne!(one.digest, Held::membership(&more).digest);
    let table = |count, digest| Held { digest, count };
    let none = BTreeMap::new();
    let lab = BTreeMap::from([([1; 32], table(3, [7; 32])), ([2; 32], table(4, [8; 32]))]);
    let tables = Held::tables(&lab, &none);
    assert_eq!(tables.count, 7);
    let mut moved = lab.clone();
    moved.insert([2; 32], table(4, [6; 32]));
    assert_ne!(tables.digest, Held::tables(&moved, &none).digest);
    assert_ne!(
        Held::tables(&none, &none).digest,
        Held::membership(&BTreeSet::new()).digest,
        "the two domains differ"
    );
    // With no conflict record the digest is the one the operations alone give, as on a build
    // that counted none: the domain, the number of tables, each one's identity, count, digest.
    let mut alone = Sha256::new();
    alone.update(b"aterm/mesh/presence/tables/1\0");
    alone.update(2u64.to_le_bytes());
    for (id, count, digest) in [([1; 32], 3u64, [7; 32]), ([2; 32], 4, [8; 32])] {
        alone.update(id);
        alone.update(count.to_le_bytes());
        alone.update(digest);
    }
    assert_eq!(tables.digest, alone.finalize());
    // A conflict record moves the digest and the count; the same records give the same one;
    // and one table's summary read as conflicts is not read as operations.
    let certificates = Held::conflicts(&BTreeSet::from([[5; 32]]));
    assert_eq!(
        certificates,
        Held::conflicts(&BTreeSet::from([[5; 32]])),
        "one set, one digest"
    );
    assert_ne!(
        certificates.digest,
        Held::conflicts(&BTreeSet::from([[6; 32]])).digest
    );
    let fork = BTreeMap::from([([2; 32], certificates)]);
    let with_fork = Held::tables(&lab, &fork);
    assert_eq!(with_fork.count, 8, "operations and conflict records");
    assert_ne!(
        with_fork.digest, tables.digest,
        "a conflict record moves it"
    );
    let one_table = BTreeMap::from([([1; 32], table(3, [7; 32]))]);
    assert_ne!(
        Held::tables(&one_table, &none).digest,
        Held::tables(&none, &one_table).digest,
        "operations and conflict records read apart"
    );
    let (a, mut b) = (row(T0_MS), row(T0_MS + 1));
    assert!(a.in_step_with(&b), "in step: the same digests");
    b.tables = None;
    assert!(
        !a.in_step_with(&b),
        "a table digest not known is not in step"
    );
    b.tables = Some(Held::tables(&moved, &none));
    assert!(!a.in_step_with(&b));
}

#[test]
fn a_row_names_only_addresses_a_lan_peer_can_dial() {
    let ip = |text: &str| text.parse::<IpAddr>().expect("an address");
    for yes in [
        "192.168.1.20",
        "10.0.0.7",
        "172.16.4.1",
        "169.254.3.9",
        "fd12:3456::1",
    ] {
        assert!(dialable(ip(yes)), "{yes} is dialable");
    }
    for no in ["127.0.0.1", "::1", "fe80::1", "8.8.8.8", "2001:db8::1"] {
        assert!(!dialable(ip(no)), "{no} is not");
    }
}

/// Holdings that differ by `n`: a membership log of `n` records and no tables.
fn holds(n: u8) -> Holds {
    Holds {
        membership: Some(Held::membership(&(0..n).map(|i| [i; 32]).collect())),
        tables: Some(Held::tables(&BTreeMap::new(), &BTreeMap::new())),
    }
}

/// A row of `who`'s that holds `held`, offered live now.
fn holding(at: &mut At, who: &str, seq: u64, held: Holds) {
    let wall = at.wall();
    let row = PresenceRow {
        membership: held.membership,
        tables: held.tables,
        ..row(wall)
    };
    at.board.offer(who, seq, row, true, at.now, wall);
}

/// §4.12 "slowest": a seat whose row holds what this Mac held before it moved on reads
/// behind by at least how long ago this Mac's own rows first held more; one that holds what
/// this Mac holds now reads in step; one that holds what none of this Mac's remembered rows
/// held reads apart, and one whose row names no digest reads unknown. A repeat of this Mac's
/// holdings is no change, and only the last [`OWN_KEPT`] changes are remembered.
/// NEGATIVE CONTROL: the same seat read before this Mac moved on is in step.
#[test]
fn a_seat_reads_behind_by_when_this_mac_moved_past_what_it_holds() {
    const Y: &str = "n-00000000000000bb";
    let mut at = At::new();
    at.board.note_own(holds(1), at.now);
    holding(&mut at, X, 1, holds(1));
    assert_eq!(at.board.behind(X, at.now), Behind::InStep, "the control");
    at.pass(30 * S);
    at.board.note_own(holds(2), at.now);
    at.pass(30 * S);
    at.board.note_own(holds(2), at.now);
    at.board.note_own(holds(3), at.now);
    at.pass(30 * S);
    assert_eq!(
        at.board.behind(X, at.now),
        Behind::Behind { at_least: 60 * S },
        "this Mac first held more 60 s ago"
    );
    holding(&mut at, Y, 1, holds(2));
    assert_eq!(
        at.board.behind(Y, at.now),
        Behind::Behind { at_least: 30 * S },
        "a repeat of the same holdings did not move the time"
    );
    holding(&mut at, Y, 2, holds(3));
    assert_eq!(at.board.behind(Y, at.now), Behind::InStep);
    // A row of this Mac's whose membership did not read, between holding 3 and holding
    // more, is no row that held more: the lag runs from the one that did.
    let gap = Holds {
        membership: None,
        ..holds(3)
    };
    at.pass(10 * S);
    at.board.note_own(gap, at.now);
    at.pass(10 * S);
    at.board.note_own(holds(4), at.now);
    at.pass(10 * S);
    assert_eq!(
        at.board.behind(Y, at.now),
        Behind::Behind { at_least: 10 * S },
        "an unread store's row is not where this Mac first held more"
    );
    at.board.note_own(holds(3), at.now);
    holding(&mut at, Y, 3, holds(9));
    assert_eq!(
        at.board.behind(Y, at.now),
        Behind::Apart,
        "records this Mac lacks"
    );
    holding(
        &mut at,
        Y,
        4,
        Holds {
            tables: None,
            ..holds(3)
        },
    );
    assert_eq!(
        at.board.behind(Y, at.now),
        Behind::Unknown,
        "no tables digest"
    );
    assert_eq!(
        at.board.behind("n-00000000000000cc", at.now),
        Behind::Unknown,
        "no row"
    );
    for n in 4..4 + u8::try_from(OWN_KEPT).expect("a small bound") {
        at.pass(S);
        at.board.note_own(holds(n), at.now);
    }
    assert_eq!(
        at.board.behind(X, at.now),
        Behind::Apart,
        "a lag older than the board remembers"
    );
}

/// A build's release is the major and minor of its version, build metadata or none; any
/// other text is no release.
#[test]
fn a_builds_release_is_its_major_and_minor() {
    let release = |text: &str| Build::new(text).and_then(|build| build.release());
    assert_eq!(release("0.147.0+g1a2b3c4d"), Some((0, 147)));
    assert_eq!(release("0.159.0"), Some((0, 159)));
    assert_eq!(release("1.2.3+dev.4-x"), Some((1, 2)));
    for no in [
        "0.147",
        "0.147.0.1",
        "00.147.0",
        "0.147.0+",
        "v0.147.0",
        "dev-build",
    ] {
        assert_eq!(release(no), None, "{no}");
    }
}

#[test]
fn a_record_file_reads_as_what_it_holds() {
    let dir = aterm_tempfile::tempdir().expect("a scratch dir");
    let file = dir.path().join("records");
    assert_eq!(membership_of_records(&file), None, "no file");
    std::fs::write(&file, b"not records").expect("written");
    assert_eq!(
        membership_of_records(&file),
        None,
        "a file that does not decode"
    );
}
