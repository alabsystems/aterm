// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const C: &str = "cccccccccccccccccccccccccccccccc";
const SID: &str = "s-0123456789abcdef0123";

const BLOCKED: Outcome = Outcome::Blocked {
    level: FleetAccess::ReadOnly,
    needs: FleetAccess::Full,
};

fn row(at_ms: u64, way: Way, node: &str, kind: DriveKind, outcome: Outcome) -> Row {
    Row::of(at_ms, way, (node, Some("m86")), kind, Some(SID), outcome).expect("an agent request")
}

/// Every kind has its plain type or is named as no agent request: the table, kind by kind. A new
/// kind fails here until the table names it, as `Plain::of` does not compile until it does.
#[test]
fn every_drive_kind_has_its_plain_type_or_is_a_service() {
    use DriveKind as K;
    let table = [
        (K::Read, Some(Plain::Read)),
        (K::ShellRead, Some(Plain::Read)),
        (K::Vault, None),
        (K::ServiceRepair, None),
        (K::AccountsRenew, None),
        (K::Post, Some(Plain::Message)),
        (K::MailSeen, Some(Plain::Message)),
        (K::Turn, Some(Plain::Typed)),
        (K::Key, Some(Plain::Typed)),
        (K::Send, Some(Plain::Typed)),
        (K::Hold, Some(Plain::Hold)),
        (K::Close, Some(Plain::Close)),
        (K::Claim, Some(Plain::Hold)),
        (K::BusLog, None),
        (K::Spawn, Some(Plain::NewJob)),
    ];
    assert_eq!(table.map(|(kind, _)| kind), DriveKind::ALL);
    for (kind, plain) in table {
        assert_eq!(Plain::of(kind), plain, "{kind:?}");
    }
    // Every plain type is some kind's, and its word reads back.
    for plain in Plain::ALL {
        assert!(table.iter().any(|(_, of)| *of == Some(plain)), "{plain:?}");
        assert_eq!(Plain::from_word(plain.word()), Some(plain));
    }
    for way in Way::ALL {
        assert_eq!(Way::from_word(way.word()), Some(way));
    }
}

/// A read counts only where a session asked for it; a service's kind never; anything else
/// whoever asked.
#[test]
fn a_poll_is_not_counted_and_a_sessions_read_is() {
    for kind in DriveKind::ALL {
        let asked = counted(kind, Some(SID));
        let unasked = counted(kind, None);
        assert_eq!(asked, Plain::of(kind), "{kind:?}");
        match Plain::of(kind) {
            Some(Plain::Read) => assert_eq!(unasked, None, "{kind:?} with no session is a poll"),
            plain => assert_eq!(unasked, plain, "{kind:?}"),
        }
        assert_eq!(
            Row::of(1, Way::In, (A, None), kind, None, Outcome::Admitted).is_some(),
            unasked.is_some(),
            "{kind:?}"
        );
    }
    // A requester that is no session aterm minted names nobody: its read is a poll too.
    assert!(
        Row::of(
            1,
            Way::In,
            (A, None),
            DriveKind::Read,
            Some("me"),
            Outcome::Admitted
        )
        .is_none()
    );
    // No node, no row.
    assert!(
        Row::of(
            1,
            Way::In,
            ("", None),
            DriveKind::Turn,
            None,
            Outcome::Admitted
        )
        .is_none()
    );
}

#[test]
fn a_row_reads_back_from_its_line() {
    let rows = [
        row(1_000, Way::In, A, DriveKind::Turn, Outcome::Admitted),
        row(2_000, Way::Out, B, DriveKind::Spawn, BLOCKED),
        Row::of(
            3_000,
            Way::In,
            (C, Some("Ana's Mac mini")),
            DriveKind::Post,
            None,
            Outcome::Blocked {
                level: FleetAccess::ReadOnly,
                needs: FleetAccess::ReadWrite,
            },
        )
        .expect("mail"),
        Row {
            n: 61,
            host: None,
            ..row(4_000, Way::In, A, DriveKind::Read, Outcome::Admitted)
        },
    ];
    for row in rows {
        let line = row.line();
        assert!(!line.contains('\n'), "{line:?}");
        assert_eq!(Row::parse(&line), Some(row), "{line}");
    }
    // A later build's extra word is passed over; a line short of a row's words is no row.
    let line = format!("{} via=mesh", rows_line());
    assert_eq!(Row::parse(&line), Row::parse(&rows_line()));
    // A mesh node's id is its key's principal.
    let mesh = Row::of(
        5_000,
        Way::Out,
        ("n-0123456789abcdef", Some("m86")),
        DriveKind::Turn,
        Some(SID),
        Outcome::Admitted,
    )
    .expect("a mesh node");
    assert_eq!(Row::parse(&mesh.line()), Some(mesh));
    for no_node in [
        "",
        "aa",
        "n-0123",
        &A.to_uppercase(),
        &format!("{A}a"),
        "a/b",
    ] {
        assert_eq!(
            Row::of(
                1,
                Way::In,
                (no_node, None),
                DriveKind::Turn,
                None,
                Outcome::Admitted
            ),
            None,
            "{no_node:?}"
        );
    }
    let good = format!("at=1 way=in node={A} host=- type=read n=1 out=admitted");
    assert!(Row::parse(&good).is_some(), "{good}");
    for bad in [
        String::new(),
        "at=1 way=in".to_owned(),
        good.replace("at=1", "at=x"),
        good.replace("way=in", "way=up"),
        good.replace("type=read", "type=wrote"),
        good.replace("n=1", "n=0"),
        good.replace("out=admitted", "out=blocked"),
        good.replace("out=admitted", "out=blocked level=read needs=most"),
        good.replace(A, "a/b"),
        good.replace(A, "aa"),
    ] {
        assert_eq!(Row::parse(&bad), None, "{bad:?}");
    }
}

fn rows_line() -> String {
    row(1_000, Way::In, A, DriveKind::Turn, Outcome::Admitted).line()
}

/// A row is built from the node, the name, the type, the lock's verdict and the asking session:
/// there is nowhere in it for what was typed, sent or run.
#[test]
fn a_row_holds_only_its_named_words() {
    let line = row(1_000, Way::In, A, DriveKind::Send, BLOCKED).line();
    let keys: Vec<&str> = line
        .split(' ')
        .map(|word| word.split_once('=').expect("k=v").0)
        .collect();
    assert_eq!(
        keys,
        [
            "at", "way", "node", "host", "type", "n", "out", "level", "needs", "by"
        ]
    );
}

#[test]
fn only_admitted_reads_of_one_mac_and_minute_fold() {
    let read = |at, way, node| row(at, way, node, DriveKind::Read, Outcome::Admitted);
    let first = read(60_000, Way::In, A);
    assert!(first.folds(&read(119_999, Way::In, A)));
    assert!(!first.folds(&read(120_000, Way::In, A)), "the next minute");
    assert!(!first.folds(&read(60_001, Way::Out, A)), "the other way");
    assert!(!first.folds(&read(60_001, Way::In, B)), "another Mac");
    let typed = row(60_001, Way::In, A, DriveKind::Turn, Outcome::Admitted);
    assert!(!first.folds(&typed) && !typed.folds(&typed));
    let blocked = row(60_001, Way::In, A, DriveKind::Spawn, BLOCKED);
    assert!(!blocked.folds(&blocked), "a reader shows every block");
    let folded = first
        .with(&read(90_000, Way::In, A))
        .with(&read(70_000, Way::In, A));
    assert_eq!((folded.n, folded.at_ms), (3, 90_000));
}

/// What a record that outgrew its size keeps: each minute's like requests as one counted row,
/// a block among them, so the requests add up as before and the rows are few. A block of
/// another session, another level or another Mac keeps its own row.
#[test]
fn a_record_past_its_size_folds_every_like_request_and_loses_no_count() {
    let minute = 600_000;
    let other = Row::of(
        minute + 7,
        Way::In,
        (A, Some("m86")),
        DriveKind::Spawn,
        Some("s-fedcba98765432100123"),
        BLOCKED,
    )
    .expect("a request");
    let mut rows = Vec::new();
    for at in 0..500 {
        rows.push(row(minute + at, Way::In, A, DriveKind::Spawn, BLOCKED));
        rows.push(row(
            minute + at,
            Way::In,
            A,
            DriveKind::Turn,
            Outcome::Admitted,
        ));
    }
    rows.push(other.clone());
    rows.push(row(minute + 9, Way::In, B, DriveKind::Spawn, BLOCKED));
    rows.push(row(minute + FOLD_MS, Way::In, A, DriveKind::Spawn, BLOCKED));
    let total: u64 = rows.iter().map(|row| u64::from(row.n)).sum();
    let blocks = |rows: &[Row]| -> u64 {
        rows.iter()
            .filter(|row| row.blocked())
            .map(|row| u64::from(row.n))
            .sum()
    };
    let blocked = blocks(&rows);
    // A reader folds no block and no write: every one of them has its row.
    assert_eq!(folded(rows.clone(), false).len(), rows.len());
    let kept = folded(rows, true);
    assert_eq!(kept.len(), 5, "{kept:?}");
    assert_eq!(kept.iter().map(|row| u64::from(row.n)).sum::<u64>(), total);
    assert_eq!(blocks(&kept), blocked);
    assert!(kept.contains(&other), "another session's block is its own");
    assert!(
        kept.windows(2).all(|pair| pair[0].at_ms <= pair[1].at_ms),
        "oldest first"
    );
    // The summary of the folded rows is the summary of the rows.
    let now = minute + FOLD_MS;
    let whole = summarize(&kept, now, WINDOW_MS);
    assert_eq!((whole.blocked_in, whole.count()), (blocked, total));
    assert_eq!(whole.last_blocked_in_ms, Some(now));
}

/// A row stamped after now was written before the clock was set back: it reads as written now,
/// so it leaves the hour and the record's week as any row does. A stamp within the clocks'
/// slack stays.
#[test]
fn a_row_stamped_ahead_of_the_clock_reads_as_written_now() {
    let now = 10 * WINDOW_MS;
    let mut rows = vec![
        row(now - 5, Way::In, A, DriveKind::Turn, Outcome::Admitted),
        row(
            now + AHEAD_MS,
            Way::In,
            A,
            DriveKind::Turn,
            Outcome::Admitted,
        ),
        row(now + 30 * WINDOW_MS, Way::In, B, DriveKind::Spawn, BLOCKED),
    ];
    assert!(settled(&mut rows, now));
    let stamps: Vec<u64> = rows.iter().map(|row| row.at_ms).collect();
    assert_eq!(stamps, [now - 5, now + AHEAD_MS, now]);
    assert!(!settled(&mut rows, now), "settled once");
    // An hour on, the settled block has left the window; unsettled it would be in it for a
    // month.
    let later = now + WINDOW_MS + 1;
    assert_eq!(summarize(&rows, later, WINDOW_MS).blocked_in, 0);
}

/// A summary of a record that dropped rows inside the window names the span it covers.
#[test]
fn a_summary_of_a_cut_record_names_the_time_it_covers() {
    let now = 10 * WINDOW_MS;
    let rows = [row(now - 1, Way::In, A, DriveKind::Turn, Outcome::Admitted)];
    let whole = summarize(&rows, now, WINDOW_MS);
    assert_eq!(whole.cut_ms, None);
    let hour = whole.headline(now);
    let cut = now - 20 * 60_000;
    let since = whole.clone().since(Some(cut), now);
    assert_eq!(since.cut_ms, Some(cut));
    assert_eq!(
        since.headline(now),
        hour.replacen("Last hour", "Last 20m", 1)
    );
    assert_ne!(since.headline(now), hour);
    // A cut before the window's start hides nothing of it.
    let old = whole.clone().since(Some(now - WINDOW_MS), now);
    assert_eq!((old.cut_ms, old.headline(now)), (None, hour));
}

#[test]
fn a_drives_answer_says_what_the_lock_there_decided() {
    let refusal = FleetAccess::ReadOnly
        .refusal(DriveKind::Spawn)
        .expect("refused");
    assert_eq!(Outcome::of_reply(&refusal), Some(BLOCKED));
    assert_eq!(
        Outcome::of_reply("OK s-0123456789abcdef0123"),
        Some(Outcome::Admitted)
    );
    assert_eq!(Outcome::of_reply("OK"), Some(Outcome::Admitted));
    for other in [
        "",
        "OKAY",
        "ERR denied: a drive names a session",
        "ERR busy: ask again",
    ] {
        assert_eq!(Outcome::of_reply(other), None, "{other:?}");
    }
}

/// The window's edges: a row is in it while it is younger than the window, and one stamped
/// after now is new.
#[test]
fn the_window_takes_rows_younger_than_it() {
    let now = 10 * WINDOW_MS;
    let at = |age: u64| row(now - age, Way::In, A, DriveKind::Turn, Outcome::Admitted);
    let counted = |rows: &[Row]| summarize(rows, now, WINDOW_MS).count();
    assert_eq!(counted(&[at(0)]), 1);
    assert_eq!(counted(&[at(WINDOW_MS - 1)]), 1);
    assert_eq!(counted(&[at(WINDOW_MS)]), 0);
    assert_eq!(counted(&[at(WINDOW_MS + 1)]), 0);
    let ahead = row(now + 5_000, Way::In, A, DriveKind::Turn, Outcome::Admitted);
    assert_eq!(counted(&[ahead]), 1);
    // A shorter window takes less of the same rows.
    let rows = [at(0), at(30_000), at(90_000)];
    assert_eq!(summarize(&rows, now, 60_000).count(), 2);
    // Out of the window, the latest row still says how long it has been quiet.
    let quiet = summarize(&[at(3 * WINDOW_MS)], now, WINDOW_MS);
    assert_eq!((quiet.count(), quiet.pairs.len()), (0, 0));
    assert_eq!(quiet.last_ms, Some(now - 3 * WINDOW_MS));
    assert_eq!(
        summarize(&[], now, WINDOW_MS),
        Summary {
            window_ms: WINDOW_MS,
            ..Summary::default()
        }
    );
}

#[test]
fn a_summary_counts_macs_types_and_blocks_and_orders_the_busiest_first() {
    let now = 10 * WINDOW_MS;
    let ago = |mins: u64| now - mins * 60_000;
    let rows = vec![
        // A in: 3 typed. B in: 3 requests too, the later: it comes first.
        row(ago(50), Way::In, A, DriveKind::Turn, Outcome::Admitted),
        row(ago(40), Way::In, A, DriveKind::Key, Outcome::Admitted),
        row(ago(30), Way::In, A, DriveKind::Send, Outcome::Admitted),
        row(ago(45), Way::In, B, DriveKind::Spawn, BLOCKED),
        row(ago(20), Way::In, B, DriveKind::Spawn, BLOCKED),
        Row {
            host: Some("m18".to_owned()),
            ..row(ago(10), Way::In, B, DriveKind::Post, Outcome::Admitted)
        },
        // A out: 62 reads in two rows, the busiest pair.
        Row {
            n: 61,
            ..row(ago(5), Way::Out, A, DriveKind::Read, Outcome::Admitted)
        },
        row(ago(4), Way::Out, A, DriveKind::Read, Outcome::Admitted),
        // C out: one new job another Mac's lock refused.
        row(ago(1), Way::Out, C, DriveKind::Spawn, BLOCKED),
        // Out of the window: counted nowhere but the record's latest.
        row(ago(90), Way::In, C, DriveKind::Close, Outcome::Admitted),
    ];
    let summary = summarize(&rows, now, WINDOW_MS);
    assert_eq!((summary.peers_in, summary.peers_out), (2, 2));
    assert_eq!(summary.count(), 3 + 3 + 62 + 1);
    assert_eq!(summary.types.of(Plain::Read), 62);
    assert_eq!(summary.types.of(Plain::Typed), 3);
    assert_eq!(summary.types.of(Plain::NewJob), 3);
    assert_eq!(summary.types.of(Plain::Message), 1);
    assert_eq!(summary.types.of(Plain::Close), 0);
    assert_eq!((summary.blocked_in, summary.blocked_out), (2, 1));
    assert_eq!(summary.blocked(), 3);
    assert_eq!(summary.last_blocked_in_ms, Some(ago(20)));
    assert_eq!(summary.last_ms, Some(ago(1)));
    let order: Vec<(&str, Way, u64)> = summary
        .pairs
        .iter()
        .map(|pair| (pair.node.as_str(), pair.way, pair.count()))
        .collect();
    assert_eq!(
        order,
        [
            (A, Way::Out, 62),
            (B, Way::In, 3),
            (A, Way::In, 3),
            (C, Way::Out, 1),
        ]
    );
    let b_in = &summary.pairs[1];
    assert_eq!(b_in.host.as_deref(), Some("m18"), "the latest row's name");
    assert_eq!(b_in.dominant(), Plain::NewJob);
    assert_eq!(b_in.blocked.of(Plain::NewJob), 2);
    assert_eq!(b_in.last_ms, ago(10));
    assert_eq!(summary.pairs[0].dominant(), Plain::Read);
    assert_eq!(summary.pairs[2].dominant(), Plain::Typed);
    // The same rows in another order summarize the same.
    let mut shuffled = rows.clone();
    shuffled.reverse();
    assert_eq!(summarize(&shuffled, now, WINDOW_MS), summary);
}

/// A menu has rows for the busiest five pairs that are more than a few admitted reads; the
/// summary's totals count the rest all the same.
#[test]
fn a_menu_has_rows_for_the_busiest_pairs_that_are_not_a_few_reads() {
    let now = 10 * WINDOW_MS;
    let reads = |node: &str, way: Way, n: u32| Row {
        n,
        ..row(now - 1_000, way, node, DriveKind::Read, Outcome::Admitted)
    };
    let few = u32::try_from(ROUTINE_READS).expect("a small number");
    let rows = [
        reads(A, Way::In, few),
        reads(A, Way::Out, few + 1),
        // One read this Mac refused is no routine read, and one typed line is no read.
        row(now - 2_000, Way::In, B, DriveKind::Read, BLOCKED),
        row(now - 3_000, Way::Out, B, DriveKind::Turn, Outcome::Admitted),
        reads(C, Way::Out, 1),
    ];
    let summary = summarize(&rows, now, WINDOW_MS);
    assert_eq!(summary.pairs.len(), 5);
    assert_eq!((summary.peers_in, summary.peers_out), (2, 3));
    let shown: Vec<(&str, Way)> = summary
        .shown()
        .map(|pair| (pair.node.as_str(), pair.way))
        .collect();
    assert_eq!(shown, [(A, Way::Out), (B, Way::In), (B, Way::Out)]);
    // More pairs than a menu holds: the busiest are kept.
    let nodes: Vec<String> = (0..8).map(|at| format!("{at:032x}")).collect();
    let many: Vec<Row> = nodes
        .iter()
        .zip(1..)
        .map(|(node, n)| Row {
            n,
            ..row(
                now - 1_000,
                Way::In,
                node,
                DriveKind::Turn,
                Outcome::Admitted,
            )
        })
        .collect();
    let summary = summarize(&many, now, WINDOW_MS);
    let counts: Vec<u64> = summary.shown().map(Pair::count).collect();
    assert_eq!(counts, [8, 7, 6, 5, 4]);
    assert_eq!(counts.len(), SHOWN_PAIRS);
}

/// A tie between two types of one pair goes to the heavier, and a pair with a new job leads
/// with it.
#[test]
fn a_tie_goes_to_the_heavier_type() {
    let rows = [
        row(1, Way::In, A, DriveKind::Read, Outcome::Admitted),
        row(2, Way::In, A, DriveKind::Turn, Outcome::Admitted),
        row(3, Way::In, B, DriveKind::Read, Outcome::Admitted),
        row(4, Way::In, B, DriveKind::Read, Outcome::Admitted),
        row(5, Way::In, B, DriveKind::Spawn, Outcome::Admitted),
    ];
    let summary = summarize(&rows, 10, WINDOW_MS);
    let pair = |node: &str| {
        summary
            .pairs
            .iter()
            .find(|pair| pair.node == node)
            .expect("a pair")
    };
    assert_eq!(pair(A).dominant(), Plain::Typed);
    assert_eq!(
        (pair(B).dominant(), pair(B).lead()),
        (Plain::Read, Plain::NewJob)
    );
}

/// The design's own examples of a row's words.
#[test]
fn a_pair_is_said_in_a_few_words() {
    let pair = |rows: &[Row]| summarize(rows, 10, WINDOW_MS).pairs.remove(0);
    let n = |n: u32, kind, outcome| Row {
        n,
        ..row(1, Way::In, A, kind, outcome)
    };
    let ok = Outcome::Admitted;
    assert_eq!(pair(&[n(61, DriveKind::Read, ok)]).phrase(), "reads");
    assert_eq!(
        pair(&[n(1, DriveKind::Read, ok), n(141, DriveKind::Turn, ok)]).phrase(),
        "mostly typing"
    );
    assert_eq!(
        pair(&[
            n(9, DriveKind::Read, ok),
            n(2, DriveKind::Spawn, ok),
            n(1, DriveKind::Spawn, BLOCKED)
        ])
        .phrase(),
        "3 new jobs, 1 blocked"
    );
    assert_eq!(
        pair(&[n(7, DriveKind::Read, ok), n(2, DriveKind::Spawn, BLOCKED)]).phrase(),
        "2 new jobs, both blocked"
    );
}

#[test]
fn a_summary_has_one_line() {
    let now = 10 * WINDOW_MS;
    let rows = [
        row(now - 1, Way::In, A, DriveKind::Turn, Outcome::Admitted),
        row(now - 2, Way::In, B, DriveKind::Spawn, BLOCKED),
        row(now - 3, Way::Out, C, DriveKind::Turn, Outcome::Admitted),
    ];
    let busy = summarize(&rows, now, WINDOW_MS).headline(now);
    assert!(
        busy.contains("in from 2 Macs") && busy.contains("out to 1"),
        "{busy}"
    );
    assert!(busy.contains("1 blocked"), "{busy}");
    let calm = summarize(&rows[..1], now, WINDOW_MS).headline(now);
    assert!(!calm.contains("blocked"), "{calm}");
    // Nothing in the window: how long since the record's latest, or that it is empty.
    let later = now + 3 * WINDOW_MS;
    assert_eq!(
        summarize(&rows, later, WINDOW_MS).headline(later),
        "Quiet for 3h"
    );
    assert_eq!(summarize(&[], now, WINDOW_MS).headline(now), "Quiet");
    assert_eq!(ago(now, now - 59_999), "now");
    assert_eq!(ago(now, now - 4 * 60_000), "4m");
}

/// The kind a sender reads off its own line, for each form a drive serves.
#[test]
fn a_senders_line_names_its_kind() {
    use DriveKind as K;
    for (selected, verb, rest, kind) in [
        (true, "status", "", Some(K::Read)),
        (true, "result", "", Some(K::Read)),
        (true, "inbox", "", Some(K::Read)),
        (true, "inbox", "seen 3", Some(K::MailSeen)),
        (true, "await", "idle", Some(K::ShellRead)),
        (true, "post", "kind=report", Some(K::Post)),
        (true, "turn", "hello", Some(K::Turn)),
        (true, "key", "Enter", Some(K::Key)),
        (true, "send", "ls", Some(K::Send)),
        (true, "close", "", Some(K::Close)),
        (true, "feed-bin", "", None),
        (false, "survey", "", Some(K::Read)),
        (false, "board", "", Some(K::Read)),
        (false, "await", "task @s-1", Some(K::Read)),
        (false, "hold", "s-1 on", Some(K::Hold)),
        (false, "spawn", "agent=claude -", Some(K::Spawn)),
        (false, "request", "claim path=/tmp/x", Some(K::Claim)),
        (false, "request", "show r-1", Some(K::Read)),
        (false, "setup", "", Some(K::Read)),
        (false, "update", "status", Some(K::Read)),
        (false, "update", " holds ", Some(K::Read)),
        (false, "update", "", None),
        (false, "update", "check", None),
        (false, "update", "apply", None),
        (false, "update", "retry", None),
        (false, "update", "status check", None),
        (false, "update", "status\napply", None),
        (false, "update", "unread=ok check", None),
        (true, "update", "status", None),
        // A fleet service's line is its own client's: no kind, so no row, for its act or its
        // read.
        (false, "vault", "status", None),
        (false, "vault", "approve x", None),
        (false, "accounts-renew", "start=0f", None),
        (false, "accounts-renew", "poll=0f", None),
        (false, "service-repair", "{}", None),
        (false, "paste", "", None),
    ] {
        assert_eq!(kind_of_request(selected, verb, rest), kind, "{verb} {rest}");
        let line = if selected {
            format!("@{SID} {verb} {rest}\n")
        } else {
            format!("{verb} {rest}\n")
        };
        assert_eq!(kind_of_line(&line), kind, "{line:?}");
    }
    assert_eq!(kind_of_line(""), None);
    assert_eq!(kind_of_line("@s-1"), None);
}

/// A survey a session asked for names it in one word, which comes off again whole; a line
/// without the word, or with a word that names no session, is a poll.
#[test]
fn a_survey_line_names_the_session_that_asked() {
    for line in ["survey", "survey facts", "survey holder=1.2.3"] {
        let rest = line.strip_prefix("survey").expect("a survey");
        assert_eq!(survey_asker(rest), None, "{line:?} is a poll");
        assert_eq!(survey_unmarked(line), None);
        let marked = survey_marked(line, SID);
        let rest = marked.strip_prefix("survey").expect("a survey");
        assert_eq!(survey_asker(rest), Some(SID), "{marked:?}");
        assert_eq!(survey_unmarked(&marked).as_deref(), Some(line));
    }
    assert_eq!(survey_asker("by=me"), None);
    assert_eq!(survey_asker("by="), None);
    // A word that names no session is left for whoever serves the line to refuse.
    assert_eq!(survey_unmarked("survey by=me"), None);
}
