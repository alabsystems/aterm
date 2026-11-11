// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn answer_projection_validates_discarded_notes_before_admission() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = crate::record::dir(root.path());
    aterm_uds::private_file::ensure_private_dir(&dir).unwrap();
    let answer = "a fleet 0123456789abcdef - - - - log 900 1000\n";
    let overlong = "x".repeat(MAX_ENDPOINT + 1);
    for (note, passed) in [
        ("".to_owned(), 0),
        ("\n".to_owned(), 0),
        ("r fleet 1 900 1000\n".to_owned(), 0),
        ("r commons 0 900 1000 2\n".to_owned(), 0),
        ("i ok api-probe 1000 example.com\n".to_owned(), 0),
        ("i down path 1000 -\n".to_owned(), 0),
        ("r unknown 1 900 1000 0\n".to_owned(), 1),
        ("r fleet x 900 1000 0\n".to_owned(), 1),
        ("r fleet 1 x 1000 0\n".to_owned(), 1),
        ("r fleet 1 900 x 0\n".to_owned(), 1),
        ("r fleet 1 900 1000 x\n".to_owned(), 1),
        ("r fleet 1 900 1000 0 extra\n".to_owned(), 1),
        ("i maybe api-probe 1000 host\n".to_owned(), 1),
        ("i ok unknown 1000 host\n".to_owned(), 1),
        ("i ok api-probe x host\n".to_owned(), 1),
        ("i ok api-probe 1000 \n".to_owned(), 1),
        ("i ok api-probe 1000 host\x07\n".to_owned(), 1),
        (format!("i ok api-probe 1000 {overlong}\n"), 1),
        ("i ok api-probe 1000 host extra\n".to_owned(), 1),
        ("r fleet 1 900 1000 0".to_owned(), 1),
        ("i ok api-probe 1000 host\r\n".to_owned(), 1),
        ("unknown note\n".to_owned(), 1),
        ("r fleet 1 900 1000 0\ni bad path 1000 host\n".to_owned(), 1),
    ] {
        let text = format!("{answer}{note}");
        let (all, full_passed) = parse_counted(&text);
        let (projected, projected_passed) = parse_projected::<false>(&text);
        assert_eq!(full_passed, passed, "{note:?}");
        assert_eq!(projected_passed, passed, "{note:?}");
        assert_eq!(projected.answers, all.answers);
        assert!(projected.rounds.is_empty() && projected.internet.is_empty());
        aterm_uds::private_file::write_private(&dir, FILE, text.as_bytes()).unwrap();
        let got = crate::proof::read_with(root.path(), Default::default(), 1000);
        assert_eq!(
            got.answers,
            if passed == 0 { all.answers } else { Vec::new() }
        );
    }
}

#[test]
fn answer_projection_matches_full_parser_on_line_mutations() {
    for seed in [
        "a fleet 0123456789abcdef 192.168.4.7 192.168.4.31 en0 n0123456789abcdef log 900 1000\n",
        "r fleet 1 900 1000 0\n",
        "i ok api-probe 1000 example.com\n",
    ] {
        for index in 0..seed.len() {
            for replacement in [b' ', b'\n', b'\r', b'\t', b'0', b'X', b'-', 0x7f] {
                let mut bytes = seed.as_bytes().to_vec();
                bytes[index] = replacement;
                let text = String::from_utf8(bytes).unwrap();
                let (all, passed) = parse_counted(&text);
                let (projected, projected_passed) = parse_projected::<false>(&text);
                assert_eq!(projected_passed, passed, "{text:?}");
                assert_eq!(projected.answers, all.answers, "{text:?}");
                assert!(projected.rounds.is_empty() && projected.internet.is_empty());
            }
        }
    }
}

#[test]
fn direct_observations_do_not_replace_or_delay_admission_evidence() {
    let root = aterm_tempfile::tempdir().expect("scratch state");
    let dir = root.path();
    let first = answer("m1", 1_000);
    add(
        dir,
        Evidence {
            answers: vec![first.clone()],
            ..Evidence::default()
        },
    )
    .unwrap();
    let scan = answer("m1", 2_000);
    note_observations(dir, vec![scan]);
    assert_eq!(read_counted(dir).0.answers, [first]);
    let displayed = read_for_place(dir);
    assert_eq!(displayed.answers.len(), 1);
    assert_eq!(displayed.answers[0].at_ms, 2_000);
    assert!(!displayed.answers[0].logged);
    assert_eq!(displayed.answers[0].network, None);

    let heartbeat = answer("m1", 2_001);
    note_answers(dir, vec![heartbeat.clone()]);
    assert_eq!(read_counted(dir).0.answers, [heartbeat]);
    assert_eq!(read_for_place(dir).answers[0].at_ms, 2_001);
}

/// A write that fails holds only the notes it was given for the next: what the file
/// already holds is not written a second time when the next write goes through.
/// NEGATIVE CONTROL: holding the whole compacted evidence on failure (0d9f794e84) holds the
/// file's round too, which the next write adds again: `assertion `left == right` failed`
/// with both rounds held.
#[test]
fn a_failed_write_holds_only_its_own_notes() {
    let root = aterm_tempfile::tempdir().expect("scratch state");
    let dir = root.path();
    let round = |at_ms| Round {
        lane: Lane::Fleet,
        answered: 1,
        blocked: 0,
        began_ms: at_ms - 100,
        at_ms,
    };
    let on_file = Evidence {
        rounds: vec![round(1_000)],
        ..Evidence::default()
    };
    add(dir, on_file).unwrap();
    let new = Evidence {
        rounds: vec![round(2_000)],
        ..Evidence::default()
    };
    let (_, held) = add_or_hold(dir, new.clone(), |_, _, _| Err(io::Error::other("full")))
        .expect_err("the write fails");
    assert_eq!(held, new);
    add(dir, held).unwrap();
    assert_eq!(read_counted(dir).0.rounds.len(), 2);
}

fn answer(member: &str, at_ms: u64) -> Answer {
    Answer {
        lane: Lane::Fleet,
        member: member_key(Lane::Fleet, member),
        addr: Some(IpAddr::V4(Ipv4Addr::new(192, 168, 4, 7))),
        via: Some(Ipv4Addr::new(192, 168, 4, 31)),
        iface: Some("en0".to_owned()),
        network: NetworkId::parse("n00112233445566ff"),
        logged: true,
        began_ms: at_ms - 100,
        at_ms,
    }
}

/// The evidence file is bounded and keeps the newest answer per member and lane: many
/// answers from one member are one line, the newest, and a flood of members stops at the
/// line bound with the newest kept.
/// NEGATIVE CONTROL: dropping the per-member `retain` in `compact` keeps every answer of
/// `m0`, and this test fails.
#[test]
fn the_evidence_file_is_bounded_and_keeps_the_newest_answer_per_member() {
    let mut evidence = Evidence::default();
    for k in 0..10 {
        evidence.answers.push(answer("m0", 1_000 + k));
    }
    let kept = compact(evidence.clone());
    assert_eq!(kept.answers.len(), 1);
    assert_eq!(kept.answers[0].at_ms, 1_009);
    for k in 0..2 * MAX_LINES as u64 {
        evidence.answers.push(answer(&format!("x{k}"), 10_000 + k));
    }
    let kept = compact(evidence);
    assert_eq!(kept.answers.len(), MAX_LINES);
    assert!(
        kept.answers
            .iter()
            .all(|a| a.at_ms >= 10_000 + MAX_LINES as u64)
    );
    assert_eq!(parse(&format(&kept)), {
        let mut sorted = kept.clone();
        sorted.answers.sort_by_key(|a| a.at_ms);
        sorted
    });
}

/// An answer line keeps the interface its route took, the network it was heard on (`-`
/// where unread) and whether its round ran under the vault's membership log, and a line whose
/// interface word is no interface name, whose network word is no network id, whose log word
/// is neither, or one of an earlier build's shape (no interface, no network, no log word), is
/// no answer: a member counts only with the interface and network it came over.
/// NEGATIVE CONTROLS: `answer_line` leaving the interface word out writes a line `parse`
/// passes over, and "the interface, network and log read back" fails; leaving the network
/// word out fails it too; writing `-` for every log word reads the logged answer back
/// unlogged and fails it.
#[test]
fn an_answers_interface_network_and_log_read_back() {
    let unread = Answer {
        iface: None,
        network: None,
        logged: false,
        ..answer("m1", 2_000)
    };
    let evidence = Evidence {
        answers: vec![answer("m0", 1_000), unread.clone()],
        ..Evidence::default()
    };
    assert_eq!(
        parse(&format(&evidence)).answers,
        evidence.answers,
        "the interface, network and log read back"
    );
    let member = member_key(Lane::Fleet, "m2");
    for line in [
        format!("a fleet {member} 192.168.4.7 192.168.4.31 en/0 - log 900 1000\n"),
        format!("a fleet {member} 192.168.4.7 192.168.4.31 en0 home log 900 1000\n"),
        format!("a fleet {member} 192.168.4.7 192.168.4.31 en0 - yes 900 1000\n"),
        format!("a fleet {member} 192.168.4.7 192.168.4.31 en0 - 900 1000\n"),
        format!("a fleet {member} 192.168.4.7 192.168.4.31 en0 900 1000\n"),
        format!("a fleet {member} 192.168.4.7 192.168.4.31 900 1000\n"),
    ] {
        assert!(parse(&line).answers.is_empty(), "{line}");
    }
}

/// A noted answer is booked on the network its round began on only where this Mac's traffic
/// still leaves on that network when it is noted: a round that began on another network (this
/// Mac moved under it), one that began before the gateway was read, and a note made while
/// the gateway is unread book it on none.
/// NEGATIVE CONTROL: `note_answer` booking the answer on `began_on` as given (no comparison
/// with `network_now`) books the round begun on the other network there, and "a round begun
/// elsewhere is booked on none" fails.
#[test]
fn an_answer_is_booked_on_the_network_its_round_began_and_ended_on() {
    let root = aterm_tempfile::tempdir().expect("a scratch directory");
    let dir = crate::record::dir(root.path());
    let route = |hw: Option<Vec<u8>>| crate::place::RouteFacts {
        interface: "en0".to_owned(),
        address: Ipv4Addr::new(192, 168, 4, 31),
        prefix: 22,
        gateway: Some(Ipv4Addr::new(192, 168, 4, 1)),
        gateway_hw: hw,
    };
    let here = route(Some(vec![0, 1, 2, 3, 4, 5]));
    let there = route(Some(vec![0, 1, 2, 3, 4, 9]));
    let (here_id, there_id) = (NetworkId::of(&here), NetworkId::of(&there));
    let script = |route: crate::place::RouteFacts| crate::facts::Script {
        facts: crate::place::Facts {
            route: Some(route),
            ..crate::place::Facts::default()
        },
        via: Vec::new(),
    };
    let noted = |member: &str, facts: crate::place::RouteFacts, began_on: Option<&NetworkId>| {
        crate::facts::script_here(script(facts), || {
            note_answer(
                &dir,
                (Lane::Fleet, member),
                Some(IpAddr::V4(Ipv4Addr::new(192, 168, 4, 9))),
                (began_on, true),
                (1_000, 1_500),
            );
        });
        read(&dir)
            .answers
            .into_iter()
            .find(|answer| answer.member == member_key(Lane::Fleet, member))
            .expect("noted")
            .network
    };
    assert_eq!(noted("m0", here.clone(), here_id.as_ref()), here_id, "here");
    assert_eq!(
        noted("m1", here.clone(), there_id.as_ref()),
        None,
        "a round begun elsewhere is booked on none"
    );
    assert_eq!(noted("m2", here, None), None, "a round begun unread");
    assert_eq!(
        noted("m3", route(None), here_id.as_ref()),
        None,
        "a note made unread"
    );
}

/// Every process can add a note while the keeper runs: the lock is held only for one
/// read-modify-rename, two writers in turn both land, and a writer that finds the lock held
/// past its wait keeps its note and lands it with its next.
/// NEGATIVE CONTROL: guarding `add` with `keeper.lock` (which a keeper holds for its whole
/// life) in place of `heard.lock` makes the first add fail while the keeper's lock is held,
/// and this test fails.
#[test]
fn every_process_can_flush_a_note_while_a_keeper_holds_its_lock() {
    let root = aterm_tempfile::tempdir().expect("a scratch directory");
    let dir = crate::record::dir(root.path());
    aterm_uds::private_file::ensure_private_dir(&dir).expect("the dir");
    let _keeper = aterm_uds::private_file::PrivateLock::take(
        &dir,
        crate::keeper::LOCK,
        Duration::ZERO,
        "held",
    )
    .expect("the keeper's lock");
    add(
        &dir,
        Evidence {
            answers: vec![answer("m1", 1_000)],
            ..Evidence::default()
        },
    )
    .expect("a note while the keeper runs");
    add(
        &dir,
        Evidence {
            answers: vec![answer("m2", 2_000)],
            ..Evidence::default()
        },
    )
    .expect("a second writer");
    assert_eq!(read(&dir).answers.len(), 2);
    let held = aterm_uds::private_file::PrivateLock::take(&dir, LOCK, Duration::ZERO, "held")
        .expect("the evidence lock");
    flush(
        &dir,
        Evidence {
            answers: vec![answer("m3", 3_000)],
            ..Evidence::default()
        },
        Vec::new(),
        3_000,
    );
    assert_eq!(read(&dir).answers.len(), 2, "held back, not lost");
    drop(held);
    flush(
        &dir,
        Evidence {
            answers: vec![answer("m4", 4_000)],
            ..Evidence::default()
        },
        Vec::new(),
        4_000,
    );
    assert_eq!(
        read(&dir).answers.len(),
        4,
        "the held note landed with the next"
    );
}

/// One process notes a member once a minute within a stay, but the first answer after a
/// move at once, whatever came a moment before it.
/// NEGATIVE CONTROL: dropping the stay test from `due` holds the answer right after the move
/// back for the minute, and this test fails.
#[test]
fn the_first_answer_after_a_move_is_noted_at_once_and_later_ones_coalesce() {
    let root = aterm_tempfile::tempdir().expect("a scratch directory");
    let dir = crate::record::dir(root.path());
    let addr = Some(IpAddr::V4(Ipv4Addr::new(10, 9, 8, 7)));
    let t = 1_800_000_000_000;
    note_answer(&dir, (Lane::Commons, "peer-a"), addr, (None, false), (t, t));
    note_answer(
        &dir,
        (Lane::Commons, "peer-a"),
        addr,
        (None, false),
        (t + 1_000, t + 1_000),
    );
    let lines = |dir: &Path| {
        read(dir)
            .answers
            .iter()
            .map(|a| a.at_ms)
            .collect::<Vec<_>>()
    };
    assert_eq!(lines(&dir), [t], "the second within the minute coalesced");
    let moved = crate::record::Written {
        record: crate::place::Record {
            stays: vec![crate::place::Stay {
                start_ms: t + 2_000,
                end_ms: None,
                at: crate::place::StayAt::Checking,
                network: None,
                subnet: None,
                gateway: None,
                from_start: false,
            }],
            ..crate::place::Record::default()
        },
        ..crate::record::Written::default()
    };
    crate::record::write(&dir, &moved).expect("the record");
    note_answer(
        &dir,
        (Lane::Commons, "peer-a"),
        addr,
        (None, false),
        (t + 3_000, t + 3_000),
    );
    assert_eq!(
        lines(&dir),
        [t + 3_000],
        "the first after the move, at once"
    );
    note_answer(
        &dir,
        (Lane::Commons, "peer-a"),
        addr,
        (None, false),
        (t + 70_000, t + 70_000),
    );
    assert_eq!(lines(&dir), [t + 70_000], "a minute on");
}

/// A round's blocked count is written and read back, and a round line with none (the first
/// build's) reads as none blocked, so an older file never reads a refused round as silence
/// it did not record.
/// NEGATIVE CONTROL: writing the round line without its blocked count reads the refused
/// round back as silence, and "the blocked count reads back" fails.
#[test]
fn a_rounds_blocked_count_reads_back() {
    let round = Round {
        lane: Lane::Fleet,
        answered: 0,
        blocked: 2,
        began_ms: 1_000,
        at_ms: 2_000,
    };
    let evidence = Evidence {
        rounds: vec![round.clone()],
        ..Evidence::default()
    };
    assert_eq!(
        parse(&format(&evidence)).rounds,
        [round],
        "the blocked count reads back"
    );
    assert_eq!(parse("r fleet 0 1000 2000\n").rounds[0].blocked, 0);
    assert!(parse("r fleet 0 1000 2000 x\n").rounds.is_empty());
}

/// The fleet book snapshot reads back, member keys hashed, with addresses and answer times,
/// each answer with the network it was booked on: a new answer takes the network this Mac is
/// on as it writes, and one the snapshot held keeps its own across a write on another
/// network.
/// NEGATIVE CONTROL: stamping every answer with the network of the write (`booked` always
/// `answered_ms.and(network.cloned())`) moves node-a's home answer onto the hotel, and "an
/// answer keeps the network it came on" fails.
#[test]
fn the_book_snapshot_reads_back() {
    let root = aterm_tempfile::tempdir().expect("a scratch directory");
    let dir = crate::record::dir(root.path());
    assert_eq!(read_book(&dir), None);
    let addr = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 4, 9)));
    let home = NetworkId::parse("n00112233445566aa").expect("an id");
    let hotel = NetworkId::parse("n66778899aabbccdd").expect("an id");
    write_book(
        &dir,
        [
            ("node-a".to_owned(), addr, Some(5)),
            ("node-b".to_owned(), None, None),
        ],
        Some(&home),
    )
    .expect("written");
    assert_eq!(
        read_book(&dir).expect("a book").members[0].network,
        Some(home.clone())
    );
    write_book(
        &dir,
        [
            ("node-a".to_owned(), addr, Some(5)),
            ("node-c".to_owned(), addr, Some(9)),
            ("node-b".to_owned(), None, None),
        ],
        Some(&hotel),
    )
    .expect("written");
    let networks: Vec<Option<NetworkId>> = read_book(&dir)
        .expect("a book")
        .members
        .into_iter()
        .map(|row| row.network)
        .collect();
    assert_eq!(
        networks,
        [Some(home), Some(hotel), None],
        "an answer keeps the network it came on"
    );
    write_book(
        &dir,
        [
            ("node-a".to_owned(), addr, Some(5)),
            ("node-b".to_owned(), None, None),
        ],
        None,
    )
    .expect("written");
    let book = read_book(&dir).expect("a book");
    assert_eq!(book.members.len(), 2);
    assert_eq!(book.members[0].member, member_key(Lane::Fleet, "node-a"));
    assert_eq!(
        (book.members[0].addr, book.members[0].answered_ms),
        (addr, Some(5))
    );
    assert_eq!(
        (book.members[1].addr, book.members[1].answered_ms),
        (None, None)
    );
}

/// Torn internet lines (empty, control-character, or oversize endpoints) count as unread in
/// `parse_counted`, and `parse_book` ignores rows whose member word is not a 16-hex-digit hash.
/// NEGATIVE CONTROL: accepting `*endpoint` unchecked parses `"i ok api-probe 1000 "` with
/// `passed == 0`, and skipping the 16-hex check in `parse_book` inflates `book.members`.
#[test]
fn torn_internet_endpoints_and_malformed_book_members_are_passed_over() {
    let overlong = "x".repeat(MAX_ENDPOINT + 1);
    for bad in [
        "i ok api-probe 1000 \n".to_owned(),
        "i ok api-probe 1000 host\x07\n".to_owned(),
        format!("i ok api-probe 1000 {overlong}\n"),
    ] {
        let (ev, passed) = parse_counted(&bad);
        assert!(ev.internet.is_empty(), "{bad:?}");
        assert_eq!(passed, 1, "{bad:?}");
    }
    let valid_member = member_key(Lane::Fleet, "node-a");
    let book = parse_book(&format!(
        "m {valid_member} - - -\n\
         m  - - -\n\
         m nothex0123456789 - - -\n\
         m abcd - - -\n"
    ));
    assert_eq!(book.members.len(), 1);
    assert_eq!(book.members[0].member, valid_member);
}

/// Unterminated trailing lines, uppercase hex member keys, and unreadable/loose evidence files
/// count as unread in `parse_counted` / `read_counted` so `proof::read_with` refuses to treat
/// partial or untrusted files as clean silence.
/// NEGATIVE CONTROL: iterating `text.lines()` in `parse_counted` accepts a truncated trailing
/// line without `'\n'`, `is_ascii_hexdigit` admits uppercase member keys, and `unwrap_or_default`
/// in `read_counted` returns `passed == 0` when `heard` has mode `0o644` or non-UTF-8 bytes.
#[test]
fn unterminated_lines_uppercase_members_and_unreadable_ledgers_count_as_unread() {
    let valid_member = member_key(Lane::Fleet, "node-a");
    let upper_member = valid_member.to_ascii_uppercase();
    let truncated = format!("a fleet {valid_member} 192.168.4.7 192.168.4.31 en0 - log 900 10");
    let (ev_trunc, passed_trunc) = parse_counted(&truncated);
    assert!(ev_trunc.answers.is_empty());
    assert_eq!(passed_trunc, 1);

    let upper_line =
        format!("a fleet {upper_member} 192.168.4.7 192.168.4.31 en0 - log 900 1000\n");
    let (ev_upper, passed_upper) = parse_counted(&upper_line);
    assert!(ev_upper.answers.is_empty());
    assert_eq!(passed_upper, 1);
    assert!(
        parse_book(&format!("m {upper_member} - - -\n"))
            .members
            .is_empty()
    );

    let root = aterm_tempfile::tempdir().expect("scratch state");
    let dir = crate::record::dir(root.path());
    assert_eq!(read_counted(&dir), (Evidence::default(), 0));
    aterm_uds::private_file::ensure_private_dir(&dir).expect("private dir");
    let file = dir.join(FILE);
    std::fs::write(&file, [0xff, 0xfe, b'\n']).expect("write non-utf8");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert_eq!(read_counted(&dir), (Evidence::default(), 1));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::write(&file, "r fleet 0 1000 2000 0\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(read_counted(&dir), (Evidence::default(), 1));
    }
}

/// A strong network id of the home-day tests, `n` its last hex digit.
fn net(n: u8) -> NetworkId {
    NetworkId::parse(&format!("n00112233445566{n:02x}")).expect("a strong id")
}

/// `member`'s answer on network `on` (strong, or `None`) at `at_ms`, logged or not.
fn heard_on(member: &str, on: Option<NetworkId>, at_ms: u64, logged: bool) -> Answer {
    Answer {
        network: on,
        logged,
        ..answer(member, at_ms)
    }
}

const DAY0: u64 = 20_400 * DAY_MS;

/// The ledger books one day per member per network per UTC day: only a logged fleet answer
/// booked on a strong network books one (an unlogged one, a Commons contact, one booked on no
/// network or a weak one books none), two answers of a member on one day book that day once,
/// and the next day is another day. What it wrote reads back whole.
/// NEGATIVE CONTROL: dropping `answer.logged` from `note_days`' filter books the unlogged
/// answer's member, and "an unlogged answer books nothing" fails.
#[test]
fn only_logged_fleet_answers_on_a_strong_network_book_their_day() {
    let root = aterm_tempfile::tempdir().expect("scratch state");
    let dir = crate::record::dir(root.path());
    let home = net(1);
    note_days(
        &dir,
        &[
            heard_on("m1", Some(home.clone()), DAY0 + 1_000, true),
            heard_on("m1", Some(home.clone()), DAY0 + 60_000, true),
            heard_on("m2", Some(home.clone()), DAY0 + 2_000, true),
        ],
    );
    note_days(
        &dir,
        &[
            heard_on("m3", Some(home.clone()), DAY0 + 3_000, false),
            Answer {
                lane: Lane::Commons,
                ..heard_on("m4", Some(home.clone()), DAY0 + 4_000, true)
            },
            heard_on("m5", None, DAY0 + 5_000, true),
            heard_on(
                "m6",
                NetworkId::parse("w00112233445566ff"),
                DAY0 + 6_000,
                true,
            ),
        ],
    );
    let (days, passed) = read_days(&dir);
    assert_eq!(passed, 0);
    let entry = days.of(&home).expect("the home network's entry");
    assert_eq!(entry.days.len(), 1, "one day: {entry:?}");
    let mut members = vec![member_key(Lane::Fleet, "m1"), member_key(Lane::Fleet, "m2")];
    members.sort();
    assert_eq!(
        entry.days[0].members, members,
        "an unlogged answer books nothing, nor a Commons contact"
    );
    assert_eq!(days.networks.len(), 1, "no weak or missing network books");

    note_days(
        &dir,
        &[heard_on("m1", Some(home.clone()), DAY0 + DAY_MS, true)],
    );
    let (days, _) = read_days(&dir);
    let entry = days.of(&home).expect("the home network's entry");
    assert_eq!(
        entry.days.iter().map(|day| day.day).collect::<Vec<_>>(),
        [DAY0 / DAY_MS, DAY0 / DAY_MS + 1]
    );
    assert_eq!(entry.heard_by(2).collect::<Vec<_>>(), [DAY0 / DAY_MS]);
    assert_eq!(entry.heard_by(1).count(), 2);
    assert_eq!(
        parse_days(&format_days(&days)),
        (days, 0),
        "reads back whole"
    );
}

/// THE SEED IS ONCE: the first seed credits each strong network remembered as the ALab LAN
/// (a weak one never is) and marks the ledger seeded; a second, handed a network remembered
/// since, credits nothing and leaves the first credit as it was. Days booked before the seed
/// stay beside its credit.
/// NEGATIVE CONTROL: dropping `seed_days`' `seeded_ms` check credits the network remembered
/// since on the second start, and "a second start credits nothing" fails.
#[test]
fn the_seed_credits_each_remembered_network_once() {
    let root = aterm_tempfile::tempdir().expect("scratch state");
    let dir = crate::record::dir(root.path());
    let known = |network: NetworkId| Known {
        network,
        learned_ms: DAY0 - 90 * DAY_MS,
        confirmed_ms: DAY0 - DAY_MS,
    };
    note_days(&dir, &[heard_on("m1", Some(net(1)), DAY0 - DAY_MS, true)]);
    let first = seed_days(
        &dir,
        &[
            known(net(1)),
            known(NetworkId::parse("w00112233445566ff").expect("a weak id")),
        ],
        7,
        DAY0,
    )
    .expect("the first seed");
    assert_eq!(first, Seeding::Now(vec![net(1)]));
    let (days, _) = read_days(&dir);
    assert_eq!(days.seeded_ms, Some(DAY0));
    let home = days.of(&net(1)).expect("the remembered network");
    assert_eq!((home.credited, home.days.len()), (7, 1));

    let second = seed_days(&dir, &[known(net(1)), known(net(2))], 7, DAY0 + DAY_MS)
        .expect("the second seed");
    assert_eq!(
        second,
        Seeding::Before(DAY0),
        "a second start credits nothing"
    );
    let (after, _) = read_days(&dir);
    assert_eq!(after, days);
    assert!(after.of(&net(2)).is_none());
}

/// A ledger with a line this build cannot read is read as passed over, and the seed, which
/// cannot tell from it whether it ran, credits nothing; the note that next writes the ledger
/// whole, reading no mark of the seed, counts it as run, so the damage credits no network.
///
/// NEGATIVE CONTROL: `seed_days` ignoring `passed` seeds the torn ledger, and "a torn ledger
/// seeds nothing" fails; `note_days` leaving `seeded_ms` unset lets the next seed credit
/// `net(1)`, and "the damage reopens no seed" fails.
#[test]
fn a_torn_ledger_never_reopens_the_seed() {
    let root = aterm_tempfile::tempdir().expect("scratch state");
    let dir = crate::record::dir(root.path());
    aterm_uds::private_file::ensure_private_dir(&dir).expect("private dir");
    let line = format!(
        "d {} 3 {}\n",
        net(1).as_str(),
        member_key(Lane::Fleet, "m1")
    );
    aterm_uds::private_file::write_private(
        &dir,
        DAYS_FILE,
        format!("{line}{}", &line[..line.len() / 2]).as_bytes(),
    )
    .expect("a torn ledger");
    assert_eq!(read_days(&dir).1, 1);
    let known = Known {
        network: net(1),
        learned_ms: 0,
        confirmed_ms: 0,
    };
    let refused = seed_days(&dir, std::slice::from_ref(&known), 7, DAY0)
        .expect_err("a torn ledger seeds nothing");
    assert_eq!(refused.kind(), io::ErrorKind::InvalidData);
    assert_eq!(read_days(&dir).0.seeded_ms, None);
    note_days(&dir, &[heard_on("m2", Some(net(1)), DAY0 + 5, true)]);
    let (whole, passed) = read_days(&dir);
    assert_eq!(passed, 0, "a note writes it whole");
    assert_eq!(whole.seeded_ms, Some(DAY0 + 5));
    assert_eq!(
        whole.of(&net(1)).map(|entry| entry.days.len()),
        Some(2),
        "the day it read is kept"
    );
    assert_eq!(
        seed_days(&dir, &[known], 7, DAY0 + 6).expect("the seed"),
        Seeding::Before(DAY0 + 5),
        "the damage reopens no seed"
    );
    assert_eq!(
        read_days(&dir).0.of(&net(1)).map(|entry| entry.credited),
        Some(0)
    );
}

/// Every bound holds as the ledger is written, and none starves what the condition counts: a
/// day keeps at most [`DAY_MEMBERS`] members; a network keeps its newest day (the one a note
/// is booking), then the days most members were heard on; the ledger keeps its credited
/// networks (most credit and days first) and apart from them the networks still earning
/// their days, most recently heard first, so a new home earns its days among old networks.
///
/// NEGATIVE CONTROL: `compact_days` with the newest-day key dropped loses day 35, and "the
/// newest day is kept" fails; ranking the earning networks by days first drops `net(50)`, and
/// "a new network keeps its place" fails; one ranking for both kinds (credited first) drops
/// `net(50)` too; the credited ranking by id alone drops `net(116)`, and "the most credited"
/// fails.
#[test]
fn every_bound_holds_and_none_starves_a_new_home() {
    let days_up_to = |last: u64| -> Vec<HeardDay> {
        (0..=last)
            .map(|day| HeardDay {
                day,
                members: (0..if day >= 32 { 1 } else { DAY_MEMBERS + 2 })
                    .map(|m| member_key(Lane::Fleet, &format!("m{m}")))
                    .collect(),
            })
            .collect()
    };
    let bound = u8::try_from(DAYS_NETWORKS).expect("a small bound");
    let mut networks: Vec<NetworkDays> = (0..bound)
        .map(|n| NetworkDays {
            network: net(n),
            credited: 0,
            days: days_up_to(35),
        })
        .collect();
    // A network first heard after all of them, on one day.
    networks.push(NetworkDays {
        network: net(50),
        credited: 0,
        days: vec![HeardDay {
            day: 40,
            members: vec![member_key(Lane::Fleet, "m0")],
        }],
    });
    // One more credited network than the bound, each with as many days as its place.
    networks.extend((0..=bound).map(|n| {
        NetworkDays {
            network: net(100 + n),
            credited: 7,
            days: (0..u64::from(n))
                .map(|day| HeardDay {
                    day,
                    members: vec![member_key(Lane::Fleet, "m0")],
                })
                .collect(),
        }
    }));
    let bounded = compact_days(HomeDays {
        seeded_ms: Some(DAY0),
        networks,
    });
    let (credited, earning): (Vec<&NetworkDays>, Vec<&NetworkDays>) = bounded
        .networks
        .iter()
        .partition(|entry| entry.credited > 0);
    assert_eq!(
        (credited.len(), earning.len()),
        (DAYS_NETWORKS, DAYS_NETWORKS)
    );
    assert!(
        bounded.of(&net(50)).is_some(),
        "a new network keeps its place"
    );
    assert!(bounded.of(&net(100 + bound)).is_some(), "the most credited");
    assert!(bounded.of(&net(100)).is_none(), "the least credited goes");
    for entry in earning.iter().filter(|entry| entry.network != net(50)) {
        let kept: Vec<u64> = entry.days.iter().map(|day| day.day).collect();
        assert_eq!(kept.len(), DAYS_KEPT);
        assert_eq!(kept.last(), Some(&35), "the newest day is kept");
        assert_eq!(kept[0], 1, "then the days most members were heard on");
        assert!(
            !kept.contains(&32) && !kept.contains(&34),
            "the days one member was heard on go first"
        );
        assert!(
            entry.days[..DAYS_KEPT - 1]
                .iter()
                .all(|day| day.members.len() == DAY_MEMBERS)
        );
    }
    assert_eq!(parse_days(&format_days(&bounded)), (bounded, 0));

    // Through the ledger: a network with 32 days of two members gathers a new day's three,
    // one note at a time.
    let root = aterm_tempfile::tempdir().expect("scratch state");
    let dir = crate::record::dir(root.path());
    for day in 0..32u64 {
        for member in ["m1", "m2"] {
            note_days(
                &dir,
                &[heard_on(member, Some(net(7)), DAY0 + day * DAY_MS, true)],
            );
        }
    }
    for member in ["m1", "m2", "m3"] {
        note_days(
            &dir,
            &[heard_on(member, Some(net(7)), DAY0 + 40 * DAY_MS, true)],
        );
    }
    let (days, _) = read_days(&dir);
    assert!(
        days.of(&net(7))
            .is_some_and(|entry| entry.heard_by(3).any(|day| day == DAY0 / DAY_MS + 40)),
        "the newest day gathers its members"
    );
}
