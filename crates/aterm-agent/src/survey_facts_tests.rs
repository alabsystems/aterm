// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn sample_facts() -> SwarmSurveyFacts {
    SwarmSurveyFacts {
        operation_lines: Vec::new(),
        node: Some("f1e2d3c4".to_owned()),
        label: Some("m22".to_owned()),
        roster_unread: false,
        version: "0.146.0".to_owned(),
        window: true,
        window_last: Some(1_793_160_000),
        sessions: 4,
        ssh_host_key: Some("SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU".to_owned()),
        moves: Some(MoveFacts {
            proven_7d: 2,
            tried_7d: 3,
            last: Some(1_793_159_000),
        }),
        accounts: vec![
            SurveyAccountFact {
                pool: "claude".to_owned(),
                id: "c01".to_owned(),
                email: None,
                condition: "usable".to_owned(),
                enrolled: true,
                usage_5h_pct: Some(34),
                usage_7d_pct: Some(61),
                week_reset: Some(1_793_500_000),
                walled_until: None,
                last_used: Some(1_793_159_900),
                read_at: None,
                launch: Some(true),
                token: None,
            },
            SurveyAccountFact {
                pool: "codex".to_owned(),
                id: "x02".to_owned(),
                email: None,
                condition: "limited".to_owned(),
                enrolled: true,
                usage_5h_pct: Some(100),
                usage_7d_pct: Some(88),
                week_reset: Some(1_793_600_000),
                walled_until: Some(1_793_170_000),
                last_used: Some(1_793_158_000),
                read_at: None,
                launch: Some(false),
                token: None,
            },
            SurveyAccountFact {
                pool: "claude".to_owned(),
                id: "c03".to_owned(),
                email: None,
                condition: "not-enrolled".to_owned(),
                enrolled: false,
                usage_5h_pct: None,
                usage_7d_pct: None,
                week_reset: None,
                walled_until: None,
                last_used: None,
                read_at: None,
                launch: None,
                token: None,
            },
        ],
        accounts_unread: vec!["grok".to_owned(), "kimi".to_owned()],
        session_accounts: BTreeMap::new(),
        server_heard: vec![ServerHeardFact {
            vault: "alab".to_owned(),
            subject: "age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p".to_owned(),
            at: 1_793_159_995,
        }],
        home: Some("/Users//a yates".to_owned()),
        home_free_mb: Some(812_345),
        repos: Some(HomeRepos {
            read_ms: 1_793_159_000_000,
            repos: vec![
                HomeRepo {
                    name: "aterm".to_owned(),
                    path: "/Users//a yates/aterm".into(),
                    head: None,
                    branch: None,
                    main_ref: None,
                    behind: None,
                    fetched_ms: None,
                    dirty: None,
                },
                HomeRepo {
                    name: "trust".to_owned(),
                    path: "/Users//a yates/trust".into(),
                    head: Some("0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c".to_owned()),
                    branch: Some("main".to_owned()),
                    main_ref: Some("origin/main".to_owned()),
                    behind: Some(3),
                    fetched_ms: Some(1_793_150_000_000),
                    dirty: Some(true),
                },
            ],
            cut: 0,
        }),
        update: None,
    }
}

/// THE HOME'S CHECKOUTS CROSS THE WIRE AT CONTENT AND ARE READ BACK: `home=`,
/// the `repos=` count with its read's time and cut, and one `repo` line each.
/// A reply at facts carries free-space counts, but none of these locations. An aterm
/// that says nothing of them, and one that read none yet (`repos=-`), read as unknown
/// (`None`), never as a home with no checkout; a body whose `repo` lines disagree with the
/// count is refused.
#[test]
fn the_homes_checkouts_cross_the_wire_and_are_unknown_where_unsaid() {
    let facts = sample_facts();
    let wire = facts.to_survey_reply_at(
        true,
        1,
        Some("m22.local"),
        1_793_160_000_000,
        1,
        (aterm_types::fleet_access::FleetAccess::ReadOnly, false),
    );
    assert!(wire.contains(" level=content "), "{wire}");
    let at_facts = facts.to_survey_reply(
        1,
        Some("m22.local"),
        1_793_160_000_000,
        1,
        (aterm_types::fleet_access::FleetAccess::ReadOnly, false),
    );
    assert!(
        !at_facts.contains(" home=")
            && !at_facts.contains(" repos=")
            && !at_facts.contains("\nrepo ")
            && !at_facts.contains("yates"),
        "{at_facts}"
    );
    let parsed = SwarmSurveyFacts::from_survey_reply(&at_facts).expect("valid");
    assert_eq!(
        (parsed.home, parsed.home_free_mb, parsed.repos),
        (None, Some(812_345), None)
    );
    let head = wire.lines().nth(1).expect("the instance line");
    for token in [
        " home=/Users//a%20yates ",
        " home_free_mb=812345 ",
        " repos=2 ",
        " repos_at=1793159000000 ",
        " repos_cut=0 ",
    ] {
        assert!(head.contains(token), "{token}: {head}");
    }
    assert!(wire.contains(
        "\nrepo name=trust path=/Users//a%20yates/trust \
         head=0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c branch=main main=origin/main behind=3 \
         fetched_at=1793150000000 dirty=1\n"
    ));
    assert!(wire.contains(
        "\nrepo name=aterm path=/Users//a%20yates/aterm head=- branch=- main=- behind=- \
         fetched_at=- dirty=-\n"
    ));
    let parsed = SwarmSurveyFacts::from_survey_reply(&wire).expect("valid");
    assert_eq!(parsed.repos, facts.repos);

    let unread = SwarmSurveyFacts {
        repos: None,
        ..facts.clone()
    };
    let wire_unread = unread.to_survey_reply_at(
        true,
        1,
        None,
        0,
        1,
        (aterm_types::fleet_access::FleetAccess::ReadOnly, false),
    );
    assert!(wire_unread.contains(" repos=- repos_at=- repos_cut=- "));
    let parsed = SwarmSurveyFacts::from_survey_reply(&wire_unread).expect("valid");
    assert_eq!(
        (parsed.repos, parsed.home.as_deref()),
        (None, Some("/Users//a yates"))
    );

    let mut older = wire.clone();
    for token in [
        " home=/Users//a%20yates",
        " home_free_mb=812345",
        " repos=2",
        " repos_at=1793159000000",
        " repos_cut=0",
    ] {
        older = older.replacen(token, "", 1);
    }
    let older: String = older
        .lines()
        .filter(|line| !line.starts_with("repo "))
        .map(|line| format!("{line}\n"))
        .collect();
    let parsed = SwarmSurveyFacts::from_survey_reply(&older).expect("an older aterm's");
    assert_eq!(
        (parsed.home, parsed.home_free_mb, parsed.repos),
        (None, None, None)
    );
    assert_eq!(
        SwarmSurveyFacts::from_survey_reply(&wire)
            .expect("valid")
            .moves,
        Some(MoveFacts {
            proven_7d: 2,
            tried_7d: 3,
            last: Some(1_793_159_000),
        }),
        "CONTROL: what it says goes both ways"
    );
    let predates = wire
        .replacen(" moves_7d=2", "", 1)
        .replacen(" move_tries_7d=3", "", 1)
        .replacen(" last_move=1793159000", "", 1);
    assert_eq!(
        SwarmSurveyFacts::from_survey_reply(&predates)
            .expect("an older aterm's")
            .moves,
        None,
        "unknown, never none"
    );
    let cannot_say = wire.replacen(" moves_7d=2", " moves_7d=-", 1).replacen(
        " last_move=1793159000",
        " last_move=-",
        1,
    );
    assert_eq!(
        SwarmSurveyFacts::from_survey_reply(&cannot_say)
            .expect("valid")
            .moves,
        None
    );

    let dropped: String = wire
        .lines()
        .filter(|line| !line.starts_with("repo name=trust "))
        .map(|line| format!("{line}\n"))
        .collect();
    assert!(SwarmSurveyFacts::from_survey_reply(&dropped).is_err());
    for broken in [
        wire.replacen(" repos_at=1793159000000", "", 1),
        wire.replacen(" dirty=1", " dirty=2", 1),
        wire.replacen(" repos=2 ", " ", 1),
    ] {
        assert!(
            SwarmSurveyFacts::from_survey_reply(&broken).is_err(),
            "{broken}"
        );
    }
}

#[test]
fn facts_round_trip_and_missing_field_negative_controls() {
    let expected = sample_facts();
    let content = expected.to_survey_reply_at(
        true,
        4242,
        Some("m22.local"),
        1_793_160_000_000,
        1,
        (aterm_types::fleet_access::FleetAccess::ReadOnly, false),
    );
    let parsed = SwarmSurveyFacts::from_survey_reply(&content).expect("valid C16 wire reply");
    assert_eq!(parsed, expected);
    let wire = expected.to_survey_reply(
        4242,
        Some("m22.local"),
        1_793_160_000_000,
        1,
        (aterm_types::fleet_access::FleetAccess::ReadOnly, false),
    );
    let parsed = SwarmSurveyFacts::from_survey_reply(&wire).expect("valid C16 wire reply");
    assert_eq!(
        parsed,
        SwarmSurveyFacts {
            home: None,
            repos: None,
            ..expected.clone()
        },
        "a facts reply keeps home paths and checkouts private"
    );

    // Negative controls: omitting any required C16 field on `instance`, `account`, or
    // `server_heard` fails validation instead of silently inventing defaults.
    for missing_token in [
        " window=1",
        " window_last=1793160000",
        " label=m22",
        " ssh_host_key=SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU",
        " pct5=34",
        " last_used=1793159900",
        " at=1793159995",
    ] {
        let broken = wire.replacen(missing_token, "", 1);
        assert_ne!(broken, wire, "token {missing_token} must appear in wire");
        assert!(
            SwarmSurveyFacts::from_survey_reply(&broken).is_err(),
            "removing `{missing_token}` must be rejected: {broken}"
        );
    }
    let plus_time = wire.replacen(" last_used=1793159900", " last_used=+1793159900", 1);
    assert!(SwarmSurveyFacts::from_survey_reply(&plus_time).is_err());

    // Truncated survey replies whose body holds fewer `account` lines than `accounts=` on
    // `instance` declares must be rejected rather than publishing partial swarm facts.
    let truncated = wire
        .lines()
        .filter(|line| !line.starts_with("account pool=codex "))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert!(SwarmSurveyFacts::from_survey_reply(&truncated).is_err());
    let signed_accounts = wire.replacen(" accounts=3 ", " accounts=+3 ", 1);
    assert!(SwarmSurveyFacts::from_survey_reply(&signed_accounts).is_err());
}

#[test]
fn release_process_observations_cross_the_fleet_read() {
    let mut facts = sample_facts();
    facts.operation_lines = vec![
        "release_line alive=1 data=%7B%7D".into(),
        "github_rate hour=123 status=unrecorded".into(),
    ];
    let wire = facts.to_survey_reply(
        1,
        None,
        0,
        1,
        (aterm_types::fleet_access::FleetAccess::ReadOnly, false),
    );
    let parsed = SwarmSurveyFacts::from_survey_reply(&wire).unwrap();
    assert_eq!(parsed.operation_lines, facts.operation_lines);
    let removed = wire
        .lines()
        .filter(|line| !line.starts_with("release_line ") && !line.starts_with("github_rate "))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        SwarmSurveyFacts::from_survey_reply(&removed)
            .unwrap()
            .operation_lines
            .is_empty(),
        "an older or unread peer supplies no process evidence"
    );
}

/// A Mac's moves of the last seven days, proved and tried, and the newest proved however old.
#[test]
fn moves_count_the_last_seven_days_and_keep_the_newest_proved() {
    const DAY: i64 = 24 * 60 * 60;
    let now = 1_800_000_000;
    let moves = PoolMoves {
        tried: vec![
            now - 8 * DAY,
            now - 7 * DAY,
            now - 7 * DAY + 1,
            now - 20,
            now - 5,
        ],
        proven: vec![now - 8 * DAY, now - 7 * DAY + 2, now - 10],
    };
    assert_eq!(
        MoveFacts::of(&moves, now),
        MoveFacts {
            proven_7d: 2,
            tried_7d: 3,
            last: Some(now - 10),
        }
    );
    let old = PoolMoves {
        tried: vec![now - 30 * DAY, now - 3],
        proven: vec![now - 30 * DAY],
    };
    assert_eq!(
        MoveFacts::of(&old, now),
        MoveFacts {
            proven_7d: 0,
            tried_7d: 1,
            last: Some(now - 30 * DAY),
        },
        "an attempt is no last move"
    );
    assert_eq!(
        MoveFacts::of(&PoolMoves::default(), now),
        MoveFacts::default()
    );
}

/// The header counts the account lines and names the pools that could not be read, which an
/// aterm whose facts predate the field leaves out. Each row carries its label whole and its
/// address (`email=`), which a line from an older member leaves out.
/// NEGATIVE CONTROL: with the writer masking `id=` again, the id reads `ab%E2%80%A6@…`; with
/// no `email=` written, the address reads back `None`.
#[test]
fn the_header_counts_the_rows_and_names_each_unread_pool() {
    let facts = sample_facts();
    let wire = facts.to_survey_reply(
        4242,
        Some("m22.local"),
        1_793_160_000_000,
        1,
        (aterm_types::fleet_access::FleetAccess::ReadOnly, false),
    );
    let head = wire.lines().nth(1).expect("the instance line");
    assert!(head.contains(" accounts=3 "), "{head}");
    let unread = |wire: &str| {
        SwarmSurveyFacts::from_survey_reply(wire)
            .expect("valid C16 wire reply")
            .accounts_unread
    };
    assert_eq!(unread(&wire), ["grok", "kimi"]);
    let none = SwarmSurveyFacts {
        accounts_unread: Vec::new(),
        ..facts.clone()
    };
    let wire = none.to_survey_reply(
        4242,
        None,
        0,
        1,
        (aterm_types::fleet_access::FleetAccess::ReadOnly, false),
    );
    assert!(wire.contains(" accounts_unread=- "), "{wire}");
    assert!(unread(&wire).is_empty());
    assert!(unread(&wire.replacen(" accounts_unread=-", "", 1)).is_empty());

    let mut emailed = facts;
    emailed.accounts[0].id = "m22".to_owned();
    emailed.accounts[0].email = Some("abcdef@example.com".to_owned());
    let wire = emailed.to_survey_reply(
        4242,
        None,
        0,
        1,
        (aterm_types::fleet_access::FleetAccess::ReadOnly, false),
    );
    assert!(wire.contains(" id=m22 email=abcdef@example.com "), "{wire}");
    let read = SwarmSurveyFacts::from_survey_reply(&wire).expect("valid C16 wire reply");
    assert_eq!(
        read.accounts[0].email.as_deref(),
        Some("abcdef@example.com")
    );
    assert_eq!(read.accounts[1].email, None, "`email=-`");
    let older = wire.replacen(" email=abcdef@example.com", "", 1);
    let read = SwarmSurveyFacts::from_survey_reply(&older).expect("an older member's line");
    assert_eq!(read.accounts[0].id, "m22");
    assert_eq!(read.accounts[0].email, None);
}

/// Each pool's read makes its rows, or names it unread: a pool that could not be read never
/// reads as one holding no row, and the others' rows still count.
#[test]
fn a_pool_that_could_not_be_read_is_named_and_the_others_still_list() {
    use crate::harness::accounts::{Class, Condition};
    use crate::harness::cli::SurveyAccount;
    let row = |label: &str, condition: Condition| SurveyAccount {
        label: label.to_owned(),
        email: None,
        class: Class::Preferred,
        enrolled: condition != Condition::NotEnrolled,
        condition,
        five_hour_pct: Some(10),
        five_hour_reset: Some(1_793_140_000),
        five_hour_reset_named: true,
        seven_day_pct: Some(42),
        week_reset: Some(1_793_500_000),
        week_reset_named: true,
        used_at: Some(1_793_130_000),
        read_at: Some(1_793_130_000),
        launch: false,
        pk: None,
        plan: None,
        plan_tier: None,
    };
    let claude = SurveyPool {
        moves: Some(PoolMoves {
            tried: vec![1_793_000_000],
            proven: vec![1_793_000_050],
        }),
        rows: vec![
            row("main", Condition::Usable),
            row(
                "work-2",
                Condition::Limited {
                    until: 1_793_140_000,
                },
            ),
        ],
        sessions: BTreeMap::from([("s-a1".to_owned(), "work-2".to_owned())]),
    };
    let (_, _, _, moved) = account_facts_of([
        (Cli::Claude, Ok(claude.clone())),
        (
            Cli::Kimi,
            Ok(SurveyPool {
                moves: Some(PoolMoves {
                    tried: vec![1_793_000_100],
                    proven: Vec::new(),
                }),
                ..SurveyPool::default()
            }),
        ),
    ]);
    assert_eq!(
        moved,
        Some(PoolMoves {
            tried: vec![1_793_000_000, 1_793_000_100],
            proven: vec![1_793_000_050],
        }),
        "a Mac's moves across its pools"
    );
    let (_, _, _, moved) = account_facts_of([
        (Cli::Claude, Ok(claude.clone())),
        (Cli::Kimi, Ok(SurveyPool::default())),
    ]);
    assert_eq!(moved, None, "a ledger it could not read is unknown");
    let (rows, unread, sessions, moved) = account_facts_of([
        (Cli::Claude, Ok(claude)),
        (Cli::Codex, Err("its roster could not be read".to_owned())),
        (
            Cli::Grok,
            Ok(SurveyPool {
                moves: Some(PoolMoves::default()),
                ..SurveyPool::default()
            }),
        ),
    ]);
    assert_eq!(unread, ["codex"]);
    assert_eq!(moved, None, "a pool it could not read is unknown");
    assert_eq!(
        rows.iter()
            .map(|row| (row.pool.as_str(), row.id.as_str()))
            .collect::<Vec<_>>(),
        [("claude", "main"), ("claude", "work-2")]
    );
    assert_eq!(
        (rows[0].usage_7d_pct, rows[0].week_reset, rows[0].last_used),
        (Some(42), Some(1_793_500_000), Some(1_793_130_000))
    );
    assert_eq!(
        [rows[0].walled_until, rows[1].walled_until],
        [None, Some(1_793_140_000)]
    );
    assert_eq!(rows[1].condition, "limited");
    assert_eq!(
        sessions.get(&("claude".to_owned(), "s-a1".to_owned())),
        Some(&"work-2".to_owned())
    );
}

#[test]
fn ssh_host_key_fingerprint_reads_ed25519_in_pure_rust() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    assert_eq!(ssh_host_key_fingerprint_in(dir.path()), None);

    let blob = [0x42u8; 51];
    let b64 = aterm_codec::base64::encode(&blob).expect("b64");
    std::fs::write(
        dir.path().join("ssh_host_ed25519_key.pub"),
        format!("ssh-ed25519 {b64} root@m22\n"),
    )
    .expect("write pubkey");

    let expected_digest = aterm_digest::Sha256::digest(blob);
    let expected_fp = format!(
        "SHA256:{}",
        aterm_codec::base64::encode(&expected_digest)
            .expect("b64")
            .trim_end_matches('=')
    );
    assert_eq!(ssh_host_key_fingerprint_in(dir.path()), Some(expected_fp));

    // Negative control: corrupt base64 or non-SSH prefix is rejected.
    let bad_dir = aterm_tempfile::tempdir().expect("tempdir");
    std::fs::write(
        bad_dir.path().join("ssh_host_ed25519_key.pub"),
        "not-ssh $$$invalid$$$\n",
    )
    .expect("write corrupt");
    assert_eq!(ssh_host_key_fingerprint_in(bad_dir.path()), None);
}

#[test]
fn window_last_reads_do_not_publish_and_the_heartbeat_persists_for_headless_readers() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    assert_eq!(read_window_last(&state, false, 1_793_160_000), None);
    assert_eq!(
        read_window_last(&state, true, 1_793_160_000),
        Some(1_793_160_000)
    );
    assert!(!state.exists(), "a live survey must not create state");
    let flushes = aterm_durable::seam::Recording::start();
    record_window_last(&state, 1_793_160_000).expect("heartbeat");
    assert!(flushes.take().is_empty(), "an advisory hint needs no flush");
    assert_eq!(
        read_window_last(&state, false, 1_793_160_300),
        Some(1_793_160_000)
    );
    assert_eq!(read_window_last(&state, false, 1_793_159_999), None);
}

/// When a row's figures were read crosses the wire, and an older member, which sends no such
/// field, still parses.
#[test]
fn an_account_rows_read_time_crosses_the_wire_and_an_older_member_sends_none() {
    let line = |read_at: &str| {
        let mut fields: Vec<(String, String)> = [
            ("pool", "codex"),
            ("id", "main"),
            ("condition", "unseen"),
            ("enrolled", "1"),
            ("pct5", "-"),
            ("pct7", "1"),
            ("week_reset", "1793916839"),
            ("walled_until", "-"),
            ("last_used", "-"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
        if !read_at.is_empty() {
            fields.push(("read_at".to_owned(), read_at.to_owned()));
        }
        parse_account_line(&fields)
    };
    assert_eq!(line("1793000000").unwrap().read_at, Some(1_793_000_000));
    assert_eq!(line("-").unwrap().read_at, None);
    assert_eq!(line("").unwrap().read_at, None, "an older member");
    assert!(line("soon").is_err(), "CONTROL: a bad figure is no figure");
}

/// THE MACHINE LABEL IS READ FROM `aterm.toml` BY THE RULE EVERY READER OF IT HOLDS
/// ([`config_label`]): a file group or others can write names none, so a machine with no
/// vault has no label. CONTROL: the same file, private or only readable by others, names it.
#[cfg(unix)]
#[test]
fn a_machine_label_is_not_read_from_an_aterm_toml_others_may_write() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    let config = dir.path().join("aterm.toml");
    std::fs::create_dir_all(&state).unwrap();
    crate::test_config::write(&config, "[ai]\nmachine = \"m7\"\n").unwrap();
    for (mode, label) in [
        (0o600, Some("m7")),
        (0o644, Some("m7")),
        (0o620, None),
        (0o602, None),
        (0o664, None),
    ] {
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(mode)).unwrap();
        let facts = SwarmSurveyFacts::collect_at(
            &state,
            None,
            Some(&config),
            0,
            false,
            None,
            1_793_160_000,
        );
        assert_eq!(facts.label.as_deref(), label, "{mode:o}");
        assert!(!facts.roster_unread, "{mode:o}");
    }
}

/// Which row a launch there would start on crosses the wire as one field, on every row of a
/// pool, and an older member, which sends no such field, still parses as saying nothing.
#[test]
fn an_account_rows_launch_crosses_the_wire_and_an_older_member_sends_none() {
    use crate::harness::accounts::{Class, Condition};
    use crate::harness::cli::SurveyAccount;
    let line = |launch: &str| {
        let mut fields: Vec<(String, String)> = [
            ("pool", "codex"),
            ("id", "main"),
            ("condition", "usable"),
            ("enrolled", "1"),
            ("pct5", "-"),
            ("pct7", "1"),
            ("week_reset", "-"),
            ("walled_until", "-"),
            ("last_used", "-"),
            ("read_at", "-"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
        if !launch.is_empty() {
            fields.push(("launch".to_owned(), launch.to_owned()));
        }
        parse_account_line(&fields)
    };
    assert_eq!(line("1").unwrap().launch, Some(true));
    assert_eq!(line("0").unwrap().launch, Some(false));
    assert_eq!(line("-").unwrap().launch, None);
    assert_eq!(line("").unwrap().launch, None, "an older member");
    assert!(line("maybe").is_err(), "CONTROL: a bad word is no word");

    let row = |label: &str, launch: bool| SurveyAccount {
        label: label.to_owned(),
        email: None,
        class: Class::Preferred,
        enrolled: true,
        condition: Condition::Usable,
        five_hour_pct: None,
        five_hour_reset: None,
        five_hour_reset_named: false,
        seven_day_pct: None,
        week_reset: None,
        week_reset_named: false,
        used_at: None,
        read_at: None,
        launch,
        pk: None,
        plan: None,
        plan_tier: None,
    };
    let (rows, _, _, _) = account_facts_of([(
        Cli::Codex,
        Ok(SurveyPool {
            rows: vec![row("main", true), row("work", false)],
            ..SurveyPool::default()
        }),
    )]);
    let launches: Vec<_> = rows.iter().map(|row| row.launch).collect();
    assert_eq!(launches, [Some(true), Some(false)]);
}

/// A Claude Code row's vault token state crosses the wire as `token=` (§5.10 of the
/// setup-token design), and an older member's line, which has none, still parses as saying
/// nothing; the extra line a member writes reads back as the same fact.
#[test]
fn an_account_rows_token_state_crosses_the_wire_and_an_older_member_sends_none() {
    let line = |token: Option<&str>| {
        let mut fields: Vec<(String, String)> = [
            ("pool", "claude"),
            ("id", "work"),
            ("condition", "usable"),
            ("enrolled", "1"),
            ("pct5", "-"),
            ("pct7", "-"),
            ("week_reset", "-"),
            ("walled_until", "-"),
            ("last_used", "-"),
            ("read_at", "-"),
            ("launch", "1"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
        if let Some(token) = token {
            fields.push(("token".to_owned(), token.to_owned()));
        }
        parse_account_line(&fields).expect("the line parses")
    };
    assert_eq!(line(Some("soon")).token.as_deref(), Some("soon"));
    assert_eq!(line(Some("-")).token, None);
    assert_eq!(line(None).token, None, "an older member");
    let mut facts = sample_facts();
    facts.accounts[0].token = Some("active".to_owned());
    let lines = facts.extra_lines(false);
    let written = lines
        .iter()
        .find(|line| line.starts_with("account pool=claude id=c01 "))
        .expect("the claude row's line");
    assert!(written.ends_with(" token=active"), "{written}");
    let other = lines
        .iter()
        .find(|line| line.starts_with("account pool=codex "))
        .expect("the codex row's line");
    assert!(other.ends_with(" token=-"), "{other}");
}

/// A MAC WITH NO VAULT CONFIGURED SAYS NOTHING OF TOKENS ON ITS ACCOUNT LINES (§5.10 of the
/// setup-token design): its Claude Code row with an email has no token state (`token=-`), as
/// `ctl @self status` and the doctor say nothing there, never `unknown`, which says a vault
/// that could not be read. CONTROL: the same row read through that unconfigured vault reads
/// `unknown`.
///
/// NEGATIVE CONTROL (2026-11-06, restored with cp from a backup plus touch, confirmed with
/// cmp): with `claude_token_states` reading the unconfigured vault again, the row reads
/// `{"work": "unknown"}`.
#[cfg(unix)]
#[test]
fn a_mac_with_no_vault_says_no_token_state_on_its_account_lines() {
    use crate::harness::accounts_token_view as view;
    fn keyer(_realm: &str, _email: &str) -> Option<String> {
        Some("ac-fedcba9876543210".to_owned())
    }
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    let state = dir.path().join("state");
    let config = dir.path().join("config/aterm.toml");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let roster = dir.path().join("config/accounts.toml");
    std::fs::write(
        &roster,
        "[[account]]\nlabel = \"work\"\nclass = \"dedicated\"\nemail = \"person@example.com\"\n",
    )
    .unwrap();
    let env = Env {
        state: state.clone(),
        aterm_state: None,
        cwd: dir.path().to_path_buf(),
        home: Some(home.clone()),
        claude_dir: None,
        claude_config_unusable: None,
        claude_profile: None,
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        codex_home: None,
        xdg_config_home: None,
        codex_login: None,
        agy_conversation: None,
        now: 1_800_000_000,
        sid: String::new(),
        sock: None,
        config: Some(config),
        claude_tmp: None,
        claude_running: None,
        tz_s: 0,
    };
    let states = crate::harness::journal::with_keyer(keyer, || claude_token_states(&env));
    assert!(states.is_empty(), "{states:?}");
    let store = accounts::Store::at(&state);
    let unread = crate::harness::journal::with_keyer(keyer, || {
        view::states_by_label(
            &view::Places {
                store: &store,
                roster: Some(&roster),
                logins: &[],
                now: env.now,
                offers: view::OFFERED_HERE,
                enrolled: &accounts::enrolled,
            },
            &crate::harness::accounts_tokens::Vault(aterm_secrets::client::Paths::for_home(&home)),
        )
    });
    assert_eq!(
        unread.get("work").map(String::as_str),
        Some("unknown"),
        "CONTROL"
    );
}

#[cfg(unix)]
#[test]
fn resolve_machine_label_enforces_roster_deny_lists_and_rejects_non_canonical_vault_host() {
    use aterm_secrets::swarm_roster::{ROSTER_ENTRY, SwarmMember, SwarmRoster};

    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    let state = dir.path().join("state");
    let config = dir.path().join("aterm.toml");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&state).unwrap();

    let srv_dir = aterm_tempfile::tempdir().expect("server dir");
    let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let running = aterm_secrets::server::start(
        socket,
        &aterm_secrets::server::Options {
            listen: "127.0.0.1:0".parse().unwrap(),
            store: srv_dir.path().to_path_buf(),
            advertise: None,
            pins: Vec::new(),
            hold: std::time::Duration::from_millis(50),
            keep_awake: false,
        },
    )
    .expect("start server");
    let url = format!("http://{}/v1/test", running.addr);
    let paths = aterm_secrets::client::Paths::for_home(&home);
    aterm_secrets::client::create(&paths, &url, None, Some("mac-mini-m4")).expect("create vault");
    let rec = aterm_secrets::client::machine_identity(&paths)
        .expect("identity")
        .recipient()
        .to_bech32();

    // 1. When a SwarmRoster exists and this recipient is not yet listed (and `cfg.host` is
    //    `"Mac-mini-M4"`), `resolve_vault_host_label` must NOT return `"Mac-mini-M4"`, and
    //    a denied `[swarm] label = "m4"` in `aterm.toml` must also be rejected.
    let roster = SwarmRoster {
        members: vec![SwarmMember {
            recipient: "age1other".to_owned(),
            vault_name: "m22".to_owned(),
            label: "m22".to_owned(),
            aliases: Vec::new(),
            hint: None,
            hw_hint: None,
            admitted_ms: 1,
            claimed_label: None,
            claimed_at_ms: None,
            node_id: None,
            node_key: None,
        }],
        deny_labels: vec!["m4".to_owned()],
        deny_nodes: Vec::new(),
    };
    let adm = aterm_secrets::client::admin_open(&paths).expect("admin open");
    let mut doc = adm.doc.clone();
    doc.set_entry(
        &aterm_secrets::text::Audience::Everyone,
        ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .expect("set roster");
    aterm_secrets::client::admin_publish(&paths, &adm, &doc).expect("publish roster");
    aterm_secrets::client::sync(&paths, None).expect("sync local copy");

    crate::test_config::write(&config, "[swarm]\nlabel = \"m4\"\n").unwrap();
    let facts_denied = SwarmSurveyFacts::collect_at(
        &state,
        Some(&home),
        Some(&config),
        0,
        false,
        None,
        1_793_160_000,
    );
    assert_eq!(facts_denied.label, None);

    // 2. When this recipient IS in the roster with a denied node_key, even a valid `[swarm] label = "m100"`
    //    in `aterm.toml` is rejected.
    let adm2 = aterm_secrets::client::admin_open(&paths).expect("admin open");
    let mut doc2 = adm2.doc.clone();
    let roster_denied_node = SwarmRoster {
        members: vec![SwarmMember {
            recipient: rec.clone(),
            vault_name: "m100".to_owned(),
            label: "m100".to_owned(),
            aliases: Vec::new(),
            hint: None,
            hw_hint: None,
            admitted_ms: 1,
            claimed_label: None,
            claimed_at_ms: None,
            node_id: None,
            node_key: Some("ab".repeat(32)),
        }],
        deny_labels: Vec::new(),
        deny_nodes: vec!["ab".repeat(32)],
    };
    doc2.set_entry(
        &aterm_secrets::text::Audience::Everyone,
        ROSTER_ENTRY,
        roster_denied_node.to_toml().as_bytes(),
    )
    .expect("set roster");
    aterm_secrets::client::admin_publish(&paths, &adm2, &doc2).expect("publish roster");
    aterm_secrets::client::sync(&paths, None).expect("sync local copy");
    crate::test_config::write(&config, "[swarm]\nlabel = \"m100\"\n").unwrap();
    let facts_denied_node = SwarmSurveyFacts::collect_at(
        &state,
        Some(&home),
        Some(&config),
        0,
        false,
        None,
        1_793_160_000,
    );
    assert_eq!(facts_denied_node.label, None);

    // 3. Once the node_key is not denied, `m100` resolves cleanly even without `aterm.toml`.
    let _ = std::fs::remove_file(&config);
    let adm3 = aterm_secrets::client::admin_open(&paths).expect("admin open");
    let mut doc3 = adm3.doc.clone();
    let mut roster_ok = roster_denied_node;
    roster_ok.deny_nodes.clear();
    doc3.set_entry(
        &aterm_secrets::text::Audience::Everyone,
        ROSTER_ENTRY,
        roster_ok.to_toml().as_bytes(),
    )
    .expect("set roster");
    aterm_secrets::client::admin_publish(&paths, &adm3, &doc3).expect("publish roster");
    aterm_secrets::client::sync(&paths, None).expect("sync local copy");
    let facts_ok =
        SwarmSurveyFacts::collect_at(&state, Some(&home), None, 0, false, None, 1_793_160_000);
    assert_eq!(facts_ok.label.as_deref(), Some("m100"));
    let writer = aterm_secrets::client::test_hold_state_lock(&paths).unwrap();
    let busy =
        SwarmSurveyFacts::collect_at(&state, Some(&home), None, 0, false, None, 1_793_160_000);
    assert!(
        busy.roster_unread,
        "a writer is visible as an unread roster"
    );
    assert_eq!(busy.label, None);
    let wire = busy.to_survey_reply(
        1,
        None,
        0,
        1,
        (aterm_types::fleet_access::FleetAccess::ReadOnly, false),
    );
    assert!(
        SwarmSurveyFacts::from_survey_reply(&wire)
            .unwrap()
            .roster_unread
    );
    drop(writer);
    let parsed = SwarmSurveyFacts::from_survey_reply(
        "OK 1\ninstance version=0.148.0 sessions=0 window=0 window_last=- label=- ssh_host_key=- accounts_unread=claude,%20codex%20,\n",
    )
    .expect("parse survey reply");
    assert_eq!(parsed.accounts_unread, ["claude", "codex"]);

    running.stop();
}

/// SURVEY FACTS UPDATE (ATERM-KI-1111-23): where the updater stands crosses the wire as the
/// `update*=` words on the `instance` header, at facts as at content, and reads back whole, so the
/// node job's re-serialized push of a window's facts keeps it. An aterm older than the words sends
/// none and reads as unknown (`None`); a word outside its grammar reads as nothing alone, and the
/// rest of the facts still read.
/// NEGATIVE CONTROL: a strict parse (an `Err` for a malformed `update*=` word) fails "the rest of
/// the facts still read"; a header that drops the words fails the round trip.
#[test]
fn the_updaters_standing_crosses_the_wire_and_an_older_member_sends_none() {
    use aterm_types::update_standing::{UpdateFact, UpdateStanding};
    let held = UpdateFact {
        checked_at: Some(1_793_159_400),
        to: Some("0.199.0".to_owned()),
        until: Some(1_793_170_000),
        why: Some("crash-loop"),
        tries: Some(2),
        checks: Some(true),
        // The reply's own `at_ms=`, which a reader takes the fact's clock from.
        said_at: Some(1_793_160_000),
        ..UpdateFact::of(UpdateStanding::Held)
    };
    let facts = SwarmSurveyFacts {
        update: Some(held.clone()),
        ..sample_facts()
    };
    let access = (aterm_types::fleet_access::FleetAccess::ReadOnly, false);
    for content in [false, true] {
        let wire = facts.to_survey_reply_at(content, 1, Some("m22"), 1_793_160_000_000, 1, access);
        let header = wire.lines().nth(1).expect("an instance header");
        assert!(
            header.contains(" update=held update_at=1793159400 update_to=0.199.0 "),
            "{header}"
        );
        let parsed = SwarmSurveyFacts::from_survey_reply(&wire).expect("valid facts");
        assert_eq!(parsed.update.as_ref(), Some(&held), "content={content}");
    }
    let wire = facts.to_survey_reply(1, Some("m22"), 1_793_160_000_000, 1, access);
    let older = wire
        .split(' ')
        .filter(|word| !word.starts_with("update"))
        .collect::<Vec<_>>()
        .join(" ");
    assert_ne!(older, wire);
    let parsed = SwarmSurveyFacts::from_survey_reply(&older).expect("an older member's facts");
    assert_eq!(parsed.update, None, "an older member sends none");
    assert_eq!(parsed.version, facts.version);

    let soon = wire
        .replacen(" update_at=1793159400 ", " update_at=soon ", 1)
        .replacen(" update_why=crash-loop ", " update_why=%2FUsers%2Fx ", 1);
    let parsed =
        SwarmSurveyFacts::from_survey_reply(&soon).expect("the rest of the facts still read");
    let update = parsed.update.expect("the standing still reads");
    assert_eq!((update.checked_at, update.why), (None, None));
    assert_eq!((update.until, update.tries), (held.until, held.tries));
    assert_eq!(parsed.accounts, facts.accounts);
}
