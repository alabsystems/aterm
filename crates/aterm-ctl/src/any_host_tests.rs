// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const AT: u64 = 1_793_130_000_000;

/// A row's `read_at=`, in seconds, as a current member sends it: a little before [`AT`].
const READ_AT: u64 = AT / 1000 - 100;

fn row(program: &str, label: &str, seven_day: Option<u64>, five_hour: Option<u64>) -> Row {
    Row {
        program: program.to_owned(),
        label: label.to_owned(),
        enrolled: true,
        condition: "usable".to_owned(),
        five_hour,
        seven_day,
        week_reset_ms: None,
        walled_until_ms: None,
        read_at: Some(READ_AT),
    }
}

/// What `--host any` says of its choice names how far the row's figures can be trusted: an `unseen`
/// row's figures are old ones, however low, and a `low` row's near its wall. CONTROL: a row read
/// working says only its figures, and one with none says its word.
#[test]
fn the_choice_says_when_its_figures_are_old_or_near_a_wall() {
    let said = |condition: &str, seven_day| {
        let mut read = row("codex", "main", seven_day, None);
        read.condition = condition.to_owned();
        let members = [member("m8", vec![read])];
        let chosen = pick("codex", &[], &members).expect("a member qualifies");
        why("codex", &[], &chosen)
    };
    assert!(
        said("unseen", Some(1)).contains("7d 1% (unseen),"),
        "{}",
        said("unseen", Some(1))
    );
    assert!(
        said("low", Some(98)).contains("7d 98% (low),"),
        "{}",
        said("low", Some(98))
    );
    assert!(
        said("usable", Some(1)).contains("7d 1%,"),
        "{}",
        said("usable", Some(1))
    );
    assert!(
        said("unseen", None).contains(" unseen,"),
        "{}",
        said("unseen", None)
    );
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
        home: None,
        free_mb: None,
        repos: None,
    }
}

fn chosen<'a>(program: &str, members: &'a [Member]) -> &'a str {
    pick(program, &[], members)
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
    let chosen = pick("claude", &[], &members).expect("a member qualifies");
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
    let chosen = pick("claude", &[], &members).expect("a member qualifies");
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
        pick("claude", &[], &members),
        Err(Counts {
            answered: 1,
            drivable: 1,
            silent: 0,
            unread: 0,
            ..Counts::default()
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
        pick("claude", &[], &members),
        Err(Counts {
            answered: 3,
            drivable: 0,
            silent: 0,
            unread: 0,
            ..Counts::default()
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
    let counts =
        pick("claude", &[], &[silent, unread, walled, ungranted]).expect_err("none qualifies");
    assert_eq!(
        counts,
        Counts {
            answered: 4,
            drivable: 3,
            silent: 1,
            unread: 1,
            ..Counts::default()
        }
    );
    assert_eq!(
        none_able("claude", &[], &counts, None),
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
        pick("claude", &[], &[unread_codex])
            .expect_err("none qualifies")
            .unread,
        0,
        "another pool unread is no unread claude pool"
    );
    assert_eq!(
        none_able("codex", &[], &Counts::default(), Some("no fleet key here")),
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
         walled_until=- last_used=- read_at=1793129900",
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
    let chosen = pick("claude", &[], &members).expect("m7 qualifies");
    assert_eq!(
        why("claude", &[], &chosen),
        "claude account main 7d 12% 5h 3%, 1 busy session; the best of 1 that can take it"
    );
    let plus = vec![
        "instance pid=9 host=m7.local version=0.146.0 sessions=0 level=facts \
         at_ms=+1 accounts=1 accounts_unread=- drive=1"
            .to_owned(),
        "account pool=claude id=main condition=usable enrolled=1 pct5=+3 pct7=+12 week_reset=- \
         walled_until=- last_used=- read_at=+1793129900"
            .to_owned(),
    ];
    let m = &members_of(&[peer(Ok(plus))])[0];
    assert_eq!(m.at_ms, None);
    assert_eq!(
        m.accounts,
        Some(vec![Row {
            read_at: None,
            ..row("claude", "main", None, None)
        }])
    );
}

/// An `unseen` row has no figures, which would rank it as the least used of all, though its login
/// may stand at a sign-in screen where a worker stalls. Negative control: with nothing else, it
/// still takes the worker.
#[test]
fn a_row_read_working_goes_before_an_unseen_one() {
    let unseen = |host: &str| {
        member(
            host,
            vec![Row {
                condition: "unseen".to_owned(),
                ..row("claude", "new", None, None)
            }],
        )
    };
    let read = member("m22", vec![row("claude", "used", Some(90), Some(40))]);
    let members = [unseen("m1"), read.clone()];
    assert_eq!(chosen("claude", &members), "m22.local");
    let both = Member {
        accounts: Some(vec![
            Row {
                condition: "unseen".to_owned(),
                ..row("claude", "a-new", None, None)
            },
            row("claude", "z-used", Some(90), Some(40)),
        ]),
        ..member("m8", Vec::new())
    };
    assert_eq!(
        both.best("claude").map(|row| row.label.as_str()),
        Some("z-used")
    );
    assert_eq!(chosen("claude", &[unseen("m1")]), "m1.local");
}

#[test]
fn an_expired_week_reset_ranks_and_prints_as_unused_on_the_members_clock() {
    let reset_row = Row {
        condition: "limited".to_owned(),
        week_reset_ms: Some(AT),
        walled_until_ms: Some(AT),
        ..row("claude", "reset-week", Some(100), None)
    };
    let active_row = Row {
        week_reset_ms: Some(AT + 1_000),
        ..row("claude", "active-week", Some(50), Some(10))
    };
    let members = [
        member("m2", vec![active_row.clone()]),
        member("m1", vec![reset_row]),
    ];
    let picked = pick("claude", &[], &members).expect("expired week reset qualifies as 0%");
    assert_eq!(picked.member.host(), "m1.local");
    assert_eq!(
        why("claude", &[], &picked),
        "claude account reset-week its wall ended, 0 busy sessions; the best of 2 that can take it"
    );

    // Negative control: before `week_reset_ms`, `seven_day` still stands.
    let unreset = Member {
        at_ms: Some(AT - 1_000),
        ..member(
            "m3",
            vec![Row {
                week_reset_ms: Some(AT),
                ..row("claude", "almost", Some(95), None)
            }],
        )
    };
    assert_eq!(
        chosen("claude", &[unreset, member("m2", vec![active_row])]),
        "m2.local"
    );
}

/// A `low` row goes after an `unseen` one, as in the harness's own order: a worker on a nearly full
/// window stops mid-task, where a wall on an unseen row costs a current member one launch, which
/// stalls by the startup deadline and shows the row `limited` (a member older than `read_at=` has
/// no such deadline, and its worker can sit there: `Row::rank`). A row read lately with room still
/// goes first, so an old 1% that says `unseen` does not beat it. m21's old 1% said `usable`, which
/// rank alone would put first: [`a_usable_row_that_names_no_read_takes_no_worker`].
#[test]
fn a_low_row_goes_after_an_unseen_one_and_a_read_row_before_both() {
    let of = |host: &str, condition: &str, week: Option<u64>| {
        member(
            host,
            vec![Row {
                condition: condition.to_owned(),
                ..row("codex", "main", week, None)
            }],
        )
    };
    let low = of("m1", "low", Some(98));
    let old = of("m2", "unseen", Some(3));
    let read = of("m3", "usable", Some(90));
    assert_eq!(chosen("codex", &[low.clone(), old.clone()]), "m2.local");
    assert_eq!(chosen("codex", &[old, read.clone()]), "m3.local");
    assert_eq!(chosen("codex", &[low.clone(), read]), "m3.local");
    assert_eq!(
        chosen("codex", &[low]),
        "m1.local",
        "CONTROL: with nothing else, it still takes the worker"
    );
}

/// A ROW THAT SAYS `usable` AND NOT WHEN IT WAS READ TAKES NO WORKER, however little its figures
/// say it has used: only an aterm older than `read_at=` sends one (`survey_condition` keeps
/// `usable` only with a read), so m21's stale 1% does not beat a fresh 90%, and a worker sent there
/// would sit `working` for days at a wall its member cannot see. Its refusal names the fix.
/// CONTROLS: the same row with a read takes the worker, and an `unseen` row that names no read
/// still does, since refusing it would leave an idle fleet with nowhere to start.
#[test]
fn a_usable_row_that_names_no_read_takes_no_worker() {
    let m21 = member(
        "m21",
        vec![Row {
            read_at: None,
            ..row("codex", "main", Some(1), None)
        }],
    );
    let m3 = member("m3", vec![row("codex", "main", Some(90), None)]);
    assert_eq!(chosen("codex", &[m21.clone(), m3.clone()]), "m3.local");

    let counts = pick("codex", &[], std::slice::from_ref(&m21)).expect_err("m21 alone");
    assert_eq!(
        counts,
        Counts {
            answered: 1,
            drivable: 1,
            undated: 1,
            ..Counts::default()
        }
    );
    assert_eq!(
        none_able("codex", &[], &counts, None),
        "--host any: no Mac can take agent=codex: 1 members answered, 1 of them let this Mac \
         drive them (`aterm fleet allow drive --from <this Mac>` there), and none of those has \
         an enrolled, unwalled codex account (1 of those run an aterm too old to say when it read \
         its codex account: update it, or name it with `--host <node>`)"
    );
    let needs = [Need::Home];
    let homed_m21 = Member {
        home: Some("/Users//m21".to_owned()),
        ..m21.clone()
    };
    let counts = pick("codex", &needs, std::slice::from_ref(&homed_m21)).expect_err("m21 alone");
    assert_eq!(counts.undated, 1, "it meets the needs");
    assert!(
        none_able("codex", &needs, &counts, None).contains("update it, or name it with"),
        "{counts:?}"
    );

    // The aside is for what a newer aterm would mend, so a row it would refuse anyway adds none.
    let refused_anyway = [
        Row {
            read_at: None,
            enrolled: false,
            ..row("codex", "main", Some(1), None)
        },
        Row {
            read_at: None,
            ..row("codex", "main", Some(100), None)
        },
        Row {
            read_at: None,
            ..row("claude", "main", Some(1), None)
        },
    ];
    for refused in refused_anyway {
        let members = [member("m21", vec![refused.clone()])];
        let counts = pick("codex", &[], &members).expect_err("no row takes it");
        assert_eq!(counts.undated, 0, "{refused:?}");
        assert!(
            !none_able("codex", &[], &counts, None).contains("too old"),
            "{refused:?}"
        );
    }

    let unseen = member(
        "m4",
        vec![Row {
            condition: "unseen".to_owned(),
            read_at: None,
            ..row("codex", "main", None, None)
        }],
    );
    assert_eq!(
        chosen("codex", std::slice::from_ref(&unseen)),
        "m4.local",
        "CONTROL: an idle fleet still starts"
    );
    assert_eq!(chosen("codex", &[m21, unseen]), "m4.local");
    assert_eq!(chosen("codex", std::slice::from_ref(&m3)), "m3.local");

    // A read's `-` and an older aterm's nothing read alike, and either is refused for `usable`.
    let peer = |row: &str| PeerSurvey {
        label: "m21.local at 192.168.1.21".to_owned(),
        answer: Ok(vec![
            "instance pid=9 host=m21.local version=0.146.0 sessions=0 level=facts \
             at_ms=1793130000000 accounts=1 accounts_unread=- drive=1"
                .to_owned(),
            row.to_owned(),
        ]),
        fault: survey::PeerFault::Other,
        unproven: false,
        node: Some("ab".repeat(16)),
        addr: Some("192.168.1.21".to_owned()),
    };
    let line = "account pool=codex id=main condition=usable enrolled=1 pct5=- pct7=1 week_reset=- \
                walled_until=- last_used=-";
    let absent = members_of(&[peer(line)]);
    let dashed = members_of(&[peer(&format!("{line} read_at=-"))]);
    assert_eq!(absent, dashed);
    let read = |members: &[Member]| members[0].accounts.as_ref().expect("its rows")[0].read_at;
    assert_eq!(read(&absent), None);
    assert!(pick("codex", &[], &absent).is_err());
    assert!(pick("codex", &[], &dashed).is_err());
    let named = members_of(&[peer(&format!("{line} read_at=1793129900"))]);
    assert_eq!(read(&named), Some(1_793_129_900));
    assert_eq!(
        chosen("codex", &named),
        "m21.local",
        "CONTROL: the same row, read"
    );
    let unseen_dashed = members_of(&[peer(
        &format!("{line} read_at=-").replace("condition=usable", "condition=unseen"),
    )]);
    assert_eq!(
        chosen("codex", &unseen_dashed),
        "m21.local",
        "CONTROL: `unseen` with `read_at=-` still takes the worker"
    );
}

/// A ROW WITH A WINDOW FULL TAKES NO WORKER, whatever its condition word, since a member older than
/// the survey's own rule still sends `usable` or `low` for it: `--host any` goes elsewhere, and
/// says none can take it where none can. CONTROL: 99% takes one, and a week that has reset says
/// nothing of the row.
#[test]
fn a_row_with_a_full_window_is_never_picked() {
    let walled = |week, five| member("m21", vec![row("codex", "main", week, five)]);
    let free = member("m99", vec![row("codex", "main", Some(60), Some(50))]);
    for spent in [walled(Some(100), None), walled(None, Some(100))] {
        assert_eq!(chosen("codex", &[spent.clone(), free.clone()]), "m99.local");
        assert!(pick("codex", &[], std::slice::from_ref(&spent)).is_err());
    }
    let low = member(
        "m21",
        vec![Row {
            condition: "low".to_owned(),
            ..row("codex", "main", Some(100), None)
        }],
    );
    assert!(pick("codex", &[], std::slice::from_ref(&low)).is_err());
    assert_eq!(
        chosen("codex", &[walled(Some(99), None)]),
        "m21.local",
        "CONTROL: a window with room"
    );
    let reset = member(
        "m21",
        vec![Row {
            week_reset_ms: Some(AT),
            ..row("codex", "main", Some(100), None)
        }],
    );
    assert_eq!(
        chosen("codex", &[reset]),
        "m21.local",
        "CONTROL: it has reset"
    );
}

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_owned).collect()
}

/// A member whose last read of its home, `age_ms` before its answer, found `repos` (each with a
/// `HEAD`), with `free_gib` free on the home's volume.
fn homed(host: &str, rows: Vec<Row>, repos: &[&str], age_ms: u64, free_gib: u64) -> Member {
    Member {
        home: Some(format!("/Users/{host}")),
        free_mb: Some(free_gib * 1024),
        repos: Some(Repos {
            read_ms: Some(AT - age_ms),
            rows: repos
                .iter()
                .map(|name| Repo {
                    name: (*name).to_owned(),
                    head: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
                    behind: Some(2),
                })
                .collect(),
            cut: 0,
        }),
        ..member(host, rows)
    }
}

#[test]
fn needs_read_in_binary_units_each_once() {
    let needs = needs_of(&words(
        "need=repo:trust,free:150g need=free:2t,repo:trust,free:512M",
    ))
    .expect("needs");
    assert_eq!(
        needs,
        vec![
            Need::Repo("trust".to_owned()),
            Need::Free {
                mb: 150 * 1024,
                said: "free:150g".to_owned()
            },
            Need::Free {
                mb: 2 * 1024 * 1024,
                said: "free:2t".to_owned()
            },
            Need::Free {
                mb: 512,
                said: "free:512m".to_owned()
            },
        ]
    );
    assert_eq!(needs_of(&[]), Ok(Vec::new()));
    for bad in [
        "need=",
        "need=repo:",
        "need=repo:.git",
        "need=repo:a/b",
        "need=repo:trust,",
        "need=free:150",
        "need=free:g",
        "need=free:-1g",
        "need=free:1.5g",
        "need=free:1gb",
        "need=free:5\u{e9}",
        "need=free:99999999999999999999t",
        "need=free:18014398509481984t",
        "need=gpu:1",
        "need=home",
    ] {
        let why = needs_of(&words(bad)).expect_err(bad);
        assert!(why.starts_with("need= "), "{bad}: {why}");
    }
}

/// A NEED IS MET ONLY BY FACTS THAT SAY SO: an aterm too old to say, a read older than
/// [`REPOS_FRESH_MS`] or dated after its answer, a checkout listed with no `HEAD`, or one a read
/// that did not reach every clone does not list, cannot say; a fresh read that lists every clone
/// and not this one says it lacks it.
#[test]
fn a_need_is_met_only_by_facts_that_say_so() {
    let trust = Need::Repo("trust".to_owned());
    let room = Need::Free {
        mb: 150 * 1024,
        said: "free:150g".to_owned(),
    };
    let fresh = homed("m1", Vec::new(), &["trust"], REPOS_FRESH_MS, 150);
    assert_eq!(fresh.judge(&trust), Judged::Met);
    assert_eq!(fresh.judge(&Need::Repo("TRUST".to_owned())), Judged::Met);
    assert_eq!(fresh.judge(&room), Judged::Met, "exactly 150 GiB");
    assert_eq!(fresh.judge(&Need::Home), Judged::Met);
    assert_eq!(fresh.judge(&Need::Repo("aterm".to_owned())), Judged::Lacks);

    let stale = homed("m1", Vec::new(), &["trust"], REPOS_FRESH_MS + 1, 149);
    assert_eq!(stale.judge(&trust), Judged::Unknown, "a read too old");
    assert_eq!(
        stale.judge(&Need::Repo("aterm".to_owned())),
        Judged::Unknown
    );
    assert_eq!(stale.judge(&room), Judged::Lacks, "149 GiB");

    let mut ahead = homed("m1", Vec::new(), &["trust"], 0, 1);
    if let Some(repos) = ahead.repos.as_mut() {
        repos.read_ms = Some(AT + 1);
    }
    assert_eq!(ahead.judge(&trust), Judged::Unknown, "a read dated after");
    let undated = Member {
        at_ms: None,
        ..homed("m1", Vec::new(), &["trust"], 0, 1)
    };
    assert_eq!(undated.judge(&trust), Judged::Unknown, "no clock");

    let mut headless = homed("m1", Vec::new(), &["trust"], 0, 1);
    if let Some(repos) = headless.repos.as_mut() {
        repos.rows[0].head = None;
    }
    assert_eq!(headless.judge(&trust), Judged::Unknown, "no HEAD read");

    let mut cut = homed("m1", Vec::new(), &["aterm"], 0, 1);
    if let Some(repos) = cut.repos.as_mut() {
        repos.cut = 1;
    }
    assert_eq!(
        cut.judge(&trust),
        Judged::Unknown,
        "among the clones not read"
    );
    assert_eq!(cut.judge(&Need::Repo("aterm".to_owned())), Judged::Met);

    let older = member("m1", Vec::new());
    for need in [&trust, &room, &Need::Home] {
        assert_eq!(older.judge(need), Judged::Unknown, "{need:?}");
    }
}

/// THE ACCEPT: a package that needs a trust checkout and 150 GiB free is never placed on a Mac
/// without them, though its accounts rank best; nor on one whose facts cannot say. Negative
/// control: with no needs, the best accounts win.
#[test]
fn a_package_that_needs_trust_and_150g_is_never_placed_on_a_mac_without_them() {
    let best = || vec![row("claude", "best", Some(1), Some(1))];
    let worst = vec![row("claude", "worst", Some(90), Some(90))];
    let no_trust = homed("m1", best(), &["aterm"], 0, 800);
    let small = homed("m2", best(), &["trust"], 0, 149);
    let older = member("m3", best());
    let stale = homed("m4", best(), &["trust"], REPOS_FRESH_MS + 1, 800);
    let fit = homed("m5", worst, &["aterm", "trust"], 60_000, 150);
    let needs = needs_of(&words("need=repo:trust,free:150g")).expect("needs");
    let members = [
        no_trust.clone(),
        small.clone(),
        older.clone(),
        stale.clone(),
        fit,
    ];
    let chosen = pick("claude", &needs, &members).expect("m5 fits");
    assert_eq!(chosen.member.host(), "m5.local");
    assert_eq!(chosen.able, 1);
    assert_eq!(
        why("claude", &needs, &chosen),
        "claude account worst 7d 90% 5h 90%, 0 busy sessions; trust 2 behind, 150 GiB free; \
         the best of 1 that can take it"
    );
    assert_eq!(
        chosen_of(pick("claude", &[], &members)),
        "m1.local",
        "with no needs, the best accounts"
    );

    let members = [no_trust, small, older, stale];
    let counts = pick("claude", &needs, &members).expect_err("none fits");
    assert_eq!(
        counts,
        Counts {
            answered: 4,
            drivable: 4,
            no_repo: 1,
            no_room: 1,
            unknown: 2,
            silent: 0,
            unread: 0,
            undated: 0,
        }
    );
    assert_eq!(
        none_able("claude", &needs, &counts, None),
        "--host any: no Mac can take agent=claude need=repo:trust,free:150g: 4 members answered, \
         4 of them let this Mac drive them (`aterm fleet allow drive --from <this Mac>` there); \
         of those, 1 lack a checkout it needs (repo:trust), 1 have less free disk than it needs \
         (free:150g), 2 cannot say whether they meet its needs (an aterm too old to report its \
         home, checkouts and disk, or no read of its checkouts within 30 min), so none meets them"
    );
}

fn chosen_of(picked: Result<Chosen<'_>, Counts>) -> &str {
    picked.expect("a member qualifies").member.host()
}

/// A Mac that meets the needs but has no account to run the worker on is counted among those
/// that meet them, and a lack of a checkout outranks too little room in the counts.
#[test]
fn a_refusal_with_needs_counts_those_that_meet_them_apart() {
    let needs = vec![
        Need::Repo("trust".to_owned()),
        Need::Free {
            mb: 1024,
            said: "free:1g".to_owned(),
        },
        Need::Home,
    ];
    let walled = homed(
        "m1",
        vec![Row {
            condition: "limited".to_owned(),
            ..row("claude", "w", Some(100), None)
        }],
        &["trust"],
        0,
        2,
    );
    let silent = Member {
        accounts: None,
        ..homed("m2", Vec::new(), &["trust"], 0, 2)
    };
    let both_short = homed("m3", vec![row("claude", "a", Some(1), None)], &[], 0, 0);
    let counts = pick("claude", &needs, &[walled, silent, both_short]).expect_err("none");
    assert_eq!(
        counts,
        Counts {
            answered: 3,
            drivable: 3,
            no_repo: 1,
            no_room: 0,
            unknown: 0,
            silent: 1,
            unread: 0,
            undated: 0,
        }
    );
    assert_eq!(
        none_able("claude", &needs, &counts, None),
        "--host any: no Mac can take agent=claude need=repo:trust,free:1g cwd=~/\u{2026}: 3 \
         members answered, 3 of them let this Mac drive them (`aterm fleet allow drive --from \
         <this Mac>` there); of those, 1 lack a checkout it needs (repo:trust), and none of the 2 \
         that meet them has an enrolled, unwalled claude account (1 of those run an aterm too old \
         to say its accounts)"
    );
    assert_eq!(
        none_able(
            "claude",
            &needs,
            &Counts::default(),
            Some("no fleet key here")
        ),
        "--host any: no Mac can take agent=claude need=repo:trust,free:1g cwd=~/\u{2026}: no \
         fleet member answered (no fleet key here)"
    );
}

/// A member's home, free space and checkouts come from its reply; a reply whose `repo` lines do
/// not all come, or that says `repos=-`, says nothing of its checkouts, and an older aterm says
/// nothing of any of them.
#[test]
fn a_survey_reply_carries_the_members_home_and_checkouts() {
    let peer = |lines: &[&str]| PeerSurvey {
        label: "m7.local at 192.168.1.7".to_owned(),
        answer: Ok(lines.iter().map(|l| (*l).to_owned()).collect()),
        fault: survey::PeerFault::Other,
        unproven: false,
        node: Some("ab".repeat(16)),
        addr: Some("192.168.1.7".to_owned()),
    };
    let head = "instance pid=9 host=m7.local version=0.147.0 sessions=0 level=facts \
                at_ms=1793130000000 home=/Users//a%20b home_free_mb=153600 accounts=0 \
                accounts_unread=- drive=1";
    let trust = "repo name=trust path=/Users//a%20b/trust \
                 head=0123456789abcdef0123456789abcdef01234567 branch=main main=origin/main \
                 behind=3 fetched_at=1793120000000 dirty=1";
    let aterm = "repo name=aterm path=/Users//a%20b/aterm head=- branch=- main=- behind=- \
                 fetched_at=- dirty=-";
    let full = format!("{head} repos=2 repos_at=1793129000000 repos_cut=0");
    let members = members_of(&[
        peer(&[&full, trust, aterm]),
        peer(&[&full, trust]),
        peer(&[&format!("{head} repos=- repos_at=- repos_cut=-")]),
        peer(&[&format!("{head} repos=1")]),
        peer(&["instance pid=9 host=m8.local version=0.146.0 sessions=0 at_ms=1 drive=1"]),
    ]);
    assert_eq!(members[0].home.as_deref(), Some("/Users//a b"));
    assert_eq!(members[0].free_mb, Some(150 * 1024));
    assert_eq!(
        members[0].repos,
        Some(Repos {
            read_ms: Some(1_793_129_000_000),
            rows: vec![
                Repo {
                    name: "trust".to_owned(),
                    head: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
                    behind: Some(3),
                },
                Repo {
                    name: "aterm".to_owned(),
                    head: None,
                    behind: None,
                },
            ],
            cut: 0,
        })
    );
    assert_eq!(
        members[0].judge(&Need::Repo("trust".to_owned())),
        Judged::Met
    );
    assert_eq!(members[1].repos, None, "a row that did not come");
    assert_eq!(members[2].repos, None, "no read yet");
    assert_eq!(members[3].repos, None, "the count's row did not come");
    let older = &members[4];
    assert_eq!(
        (&older.home, older.free_mb, &older.repos),
        (&None, None, &None),
        "an older aterm"
    );
    let extra = members_of(&[peer(&[&format!("{head} repos=0"), trust])]);
    assert_eq!(extra[0].repos, None, "a row past the count");
    let undated = members_of(&[peer(&[&format!("{head} repos=1 repos_cut=0"), trust])]);
    assert_eq!(undated[0].repos.as_ref().map(|r| r.read_ms), Some(None));
    assert_eq!(
        undated[0].judge(&Need::Repo("trust".to_owned())),
        Judged::Unknown,
        "a read that does not say when it ran"
    );
    let uncut = members_of(&[peer(&[
        &format!("{head} repos=1 repos_at=1793129000000"),
        trust,
    ])]);
    assert_eq!(
        uncut[0].repos.as_ref().map(|r| r.cut),
        Some(u64::MAX),
        "a read that does not say how many it left counts as leaving some"
    );
    assert_eq!(
        uncut[0].judge(&Need::Repo("aterm".to_owned())),
        Judged::Unknown
    );
}
