// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const AT: u64 = 1_793_130_000_000;

fn row(program: &str, label: &str, seven_day: Option<u64>, five_hour: Option<u64>) -> Row {
    Row {
        program: program.to_owned(),
        label: label.to_owned(),
        enrolled: true,
        condition: "usable".to_owned(),
        five_hour,
        seven_day,
        walled_until_ms: None,
    }
}

fn member(host: &str, rows: Vec<Row>) -> Member {
    Member {
        node: Some(format!("{host:0>32}")),
        proven: true,
        label: format!("{host}.local"),
        host: Some(format!("{host}.local")),
        addr: Some("192.168.1.9".to_owned()),
        drive: true,
        at_ms: Some(AT),
        accounts: Some(rows),
        unread: Vec::new(),
        busy: 0,
    }
}

fn chosen<'a>(program: &str, members: &'a [Member]) -> &'a str {
    pick(program, members)
        .expect("a member qualifies")
        .member
        .host()
}

#[test]
fn the_least_used_week_wins_then_five_hours_then_fewest_busy_then_the_host() {
    let members = [
        member("m7", vec![row("claude", "a", Some(95), Some(10))]),
        member("m16", vec![row("claude", "b", Some(30), Some(80))]),
        member("m18", vec![row("claude", "c", Some(60), Some(0))]),
    ];
    assert_eq!(chosen("claude", &members), "m16.local", "the week first");

    let members = [
        member("m16", vec![row("claude", "b", Some(30), Some(80))]),
        member("m18", vec![row("claude", "c", Some(30), Some(5))]),
    ];
    assert_eq!(chosen("claude", &members), "m18.local", "then five hours");

    let busy = |host: &str, busy: usize| Member {
        busy,
        ..member(host, vec![row("claude", "a", Some(30), Some(5))])
    };
    let members = [busy("m1", 3), busy("m19", 1), busy("m2", 2)];
    assert_eq!(chosen("claude", &members), "m19.local", "then fewest busy");
    let members = [busy("m19", 1), busy("M10", 1), busy("m1", 1)];
    assert_eq!(chosen("claude", &members), "m1.local", "then the host");
    let chosen = pick("claude", &members).expect("a member qualifies");
    assert_eq!(chosen.able, 3);
}

#[test]
fn a_macs_best_row_is_the_one_a_worker_would_run_on() {
    let members = [
        member(
            "m7",
            vec![
                row("claude", "worn", Some(90), Some(1)),
                row("claude", "fresh", Some(10), Some(70)),
            ],
        ),
        member("m8", vec![row("claude", "even", Some(20), Some(0))]),
    ];
    let chosen = pick("claude", &members).expect("a member qualifies");
    assert_eq!(chosen.member.host(), "m7.local");
    assert_eq!(chosen.row.label, "fresh");
}

/// A member sends no figure for a window that reset or was never read: it ranks as unused.
#[test]
fn a_window_with_no_figure_ranks_as_unused() {
    let members = [
        member("m2", vec![row("claude", "worn", Some(97), Some(40))]),
        member("m1", vec![row("claude", "reset", None, None)]),
    ];
    assert_eq!(chosen("claude", &members), "m1.local", "its week reset");
    let members = [
        member("m2", vec![row("claude", "fresh", Some(0), Some(30))]),
        member("m1", vec![row("claude", "reset", None, Some(31))]),
    ];
    assert_eq!(
        chosen("claude", &members),
        "m2.local",
        "as 0%, then five hours"
    );
}

#[test]
fn only_an_enrolled_unwalled_row_of_the_program_takes_the_worker() {
    let walled_till = |until: u64| Row {
        condition: "limited".to_owned(),
        walled_until_ms: Some(until),
        ..row("claude", "w", Some(1), Some(1))
    };
    let taking = [
        Row {
            condition: "low".to_owned(),
            ..row("claude", "low", Some(98), None)
        },
        Row {
            condition: "unseen".to_owned(),
            ..row("claude", "unseen", None, None)
        },
        walled_till(AT),
    ];
    for row in taking {
        assert!(row.takes("claude", Some(AT)), "{row:?}");
    }
    let refused = [
        Row {
            enrolled: false,
            ..row("claude", "x", Some(1), Some(1))
        },
        Row {
            condition: "limited".to_owned(),
            ..row("claude", "x", Some(1), Some(1))
        },
        Row {
            condition: "signed-out".to_owned(),
            ..row("claude", "x", Some(1), Some(1))
        },
        Row {
            condition: "not-enrolled".to_owned(),
            ..row("claude", "x", Some(1), Some(1))
        },
        walled_till(AT + 1),
        row("codex", "other", Some(1), Some(1)),
    ];
    for row in &refused {
        assert!(!row.takes("claude", Some(AT)), "{row:?}");
    }
    assert!(
        !walled_till(AT).takes("claude", None),
        "a wall with no clock to judge it by stands"
    );
    let members = [member("m7", refused.to_vec())];
    assert_eq!(
        pick("claude", &members),
        Err(Counts {
            answered: 1,
            drivable: 1,
            silent: 0,
            unread: 0,
        })
    );
    assert_eq!(
        chosen("codex", &members),
        "m7.local",
        "its codex row takes codex"
    );
}

#[test]
fn a_mac_that_does_not_let_this_one_drive_it_is_never_chosen() {
    let best = vec![row("claude", "best", Some(0), Some(0))];
    let ungranted = Member {
        drive: false,
        ..member("m1", best.clone())
    };
    let unproven = Member {
        proven: false,
        ..member("m2", best.clone())
    };
    let nodeless = Member {
        node: None,
        ..member("m3", best)
    };
    let worse = member("m40", vec![row("claude", "worse", Some(90), Some(90))]);
    let members = [ungranted.clone(), unproven.clone(), nodeless.clone(), worse];
    assert_eq!(chosen("claude", &members), "m40.local");
    let members = [ungranted, unproven, nodeless];
    assert_eq!(
        pick("claude", &members),
        Err(Counts {
            answered: 3,
            drivable: 0,
            silent: 0,
            unread: 0,
        })
    );
}

#[test]
fn a_refusal_counts_how_far_the_members_got() {
    let silent = Member {
        accounts: None,
        ..member("m3", Vec::new())
    };
    let unread = Member {
        unread: vec!["grok".to_owned(), "claude".to_owned()],
        ..member("m4", vec![row("codex", "x", Some(1), None)])
    };
    let walled = member(
        "m7",
        vec![Row {
            condition: "limited".to_owned(),
            ..row("claude", "w", Some(100), None)
        }],
    );
    let ungranted = Member {
        drive: false,
        ..member("m1", vec![row("claude", "a", Some(1), None)])
    };
    let counts = pick("claude", &[silent, unread, walled, ungranted]).expect_err("none qualifies");
    assert_eq!(
        counts,
        Counts {
            answered: 4,
            drivable: 3,
            silent: 1,
            unread: 1,
        }
    );
    assert_eq!(
        none_able("claude", &counts, None),
        "--host any: no Mac can take agent=claude: 4 members answered, 3 of them let this Mac \
         drive them (`aterm fleet allow drive --from <this Mac>` there), and none of those has \
         an enrolled, unwalled claude account (1 of those run an aterm too old to say its \
         accounts; 1 of those could not read their claude account records)"
    );
    let unread_codex = Member {
        unread: vec!["codex".to_owned()],
        ..member("m4", Vec::new())
    };
    assert_eq!(
        pick("claude", &[unread_codex])
            .expect_err("none qualifies")
            .unread,
        0,
        "another pool unread is no unread claude pool"
    );
    assert_eq!(
        none_able("codex", &Counts::default(), Some("no fleet key here")),
        "--host any: no Mac can take agent=codex: no fleet member answered (no fleet key here)"
    );
}

#[test]
fn a_survey_reply_reads_as_a_member() {
    let peer = |answer: Result<Vec<String>, String>| PeerSurvey {
        label: "m7.local at 192.168.1.7".to_owned(),
        answer,
        fault: survey::PeerFault::Other,
        unproven: false,
        node: Some("ab".repeat(16)),
        addr: Some("192.168.1.7".to_owned()),
    };
    let lines = [
        "instance pid=9 host=m7-mini.local version=0.146.0 sessions=2 level=facts \
         at_ms=1793130000000 accounts=1 accounts_unread=- drive=1",
        "account pool=claude id=main condition=usable enrolled=1 pct5=3 pct7=12 week_reset=- \
         walled_until=- last_used=-",
        "session local=1 sid=s-a1 state=running program=claude program_kind=agent agent=busy",
        "session local=2 sid=s-a2 state=running program=zsh program_kind=shell agent=-",
    ]
    .map(str::to_owned)
    .to_vec();
    let older = vec![
        "instance pid=9 host=m8.local version=0.145.0 sessions=0 level=facts at_ms=1".to_owned(),
    ];
    let unread = vec![
        "instance pid=9 host=m9.local version=0.146.0 sessions=0 level=facts at_ms=1 \
         accounts=0 accounts_unread=claude,codex"
            .to_owned(),
    ];
    let members = members_of(&[
        peer(Ok(lines)),
        peer(Ok(older)),
        peer(Ok(unread)),
        peer(Err("no route".to_owned())),
        peer(Ok(vec!["session local=1".to_owned()])),
    ]);
    assert_eq!(
        members.len(),
        3,
        "an unanswered or headless reply is no member"
    );
    let m7 = &members[0];
    assert_eq!(
        (m7.host(), m7.addr.as_deref(), m7.drive, m7.busy),
        ("m7-mini.local", Some("192.168.1.7"), true, 1)
    );
    assert_eq!(m7.at_ms, Some(AT));
    assert_eq!(
        m7.accounts,
        Some(vec![row("claude", "main", Some(12), Some(3))])
    );
    assert_eq!(
        (members[1].drive, &members[1].accounts),
        (false, &None),
        "an older aterm says nothing of its accounts"
    );
    assert_eq!(
        (&members[2].accounts, &members[2].unread),
        (
            &Some(Vec::new()),
            &vec!["claude".to_owned(), "codex".to_owned()]
        ),
        "a newer one that could not read them"
    );
    assert!(m7.unread.is_empty());
    let chosen = pick("claude", &members).expect("m7 qualifies");
    assert_eq!(
        why("claude", &chosen),
        "claude account main 7d 12% 5h 3%, 1 busy session; the best of 1 that can take it"
    );
}
