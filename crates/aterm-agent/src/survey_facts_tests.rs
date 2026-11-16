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
        home_dir: None,
        home_free_mb: Some(812_345),
        home_total_mb: Some(1_907_000),
        home_build: Some(HomeBuild {
            mb: 84_992,
            at_ms: 1_793_152_800_000,
            partial: true,
        }),
        store_worst: StoreWorst::new("vault-records", 83),
        machine: Some(sample_machine()),
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

/// A machine that read every figure, each a different value, so a key read into another's
/// field shows; its free space and its size are [`sample_facts`]' `home_free_mb` and
/// `home_total_mb`.
fn sample_machine() -> MachineFacts {
    MachineFacts {
        look: MachineLook {
            at_unix_ms: Some(1_793_159_998_500),
            cores: Some(12),
            load_x100: Some([345, 280, 199]),
            cpu_busy_x100: Some(1130),
            rate_win_ms: Some(60_250),
            mem_total_mb: Some(65_536),
            mem_used_mb: Some(40_100),
            mem_avail_mb: Some(21_900),
            mem_pressure: Some(MemoryLevel::Warn),
            mem_psi60_x100: Some(1234),
            swap_used_mb: Some(2048),
            swap_total_mb: Some(4096),
            disk_total_mb: Some(1_907_000),
            disk_free_mb: Some(812_345),
            gpu_cores: Some(30),
            gpu_busy_pct: Some(97),
            net_link_kbps: Some(1_000_000),
            net_rx_kbps: Some(5120),
            net_tx_kbps: Some(640),
            net_other_kbps: Some(17),
            limits: Limits {
                cpu: Some(false),
                mem: Some(true),
                disk: Some(false),
                gpu: Some(true),
            },
            load: Some(LoadScore {
                level_x100: 88,
                over_x100: Some(21),
            }),
        },
        up_since_ms: Some(1_793_100_000_000),
        os: Some("macos"),
        target: Some("aarch64-apple-darwin".to_owned()),
    }
}

/// [`sample_machine`]'s header tokens, in wire order.
const SAMPLE_MACHINE_TOKENS: &str = " os=macos target=aarch64-apple-darwin \
     m_at_ms=1793159998500 cores=12 load1_x100=345 load5_x100=280 load15_x100=199 \
     cpu_busy_x100=1130 rate_win_ms=60250 mem_total_mb=65536 mem_used_mb=40100 \
     mem_avail_mb=21900 mem_pressure=warn mem_psi60_x100=1234 swap_used_mb=2048 \
     swap_total_mb=4096 gpu_cores=30 gpu_busy_pct=97 net_link_kbps=1000000 \
     net_rx_kbps=5120 net_tx_kbps=640 net_other_kbps=17 up_since_ms=1793100000000 cpu_lim=0 \
     mem_lim=1 disk_lim=0 gpu_lim=1 load=88:21";

/// [`sample_facts`]' home volume tokens, in wire order, right after `ssh_host_key=`: its free
/// space, its size and its build output with that sizing's time and floor word.
const SAMPLE_DISK_TOKENS: &str = " home_free_mb=812345 home_total_mb=1907000 \
     home_build_mb=84992 home_build_at_ms=1793152800000 home_build_partial=1";

/// What a machine's facts read with one key of the look unsent or unreadable: that figure
/// unread and every other as it was. `m_at_ms` is not among them: its absence is an older
/// member's header, its damage a look with no time. Nor are the volume's free space and size,
/// which are the header's own keys ([`SAMPLE_DISK_TOKENS`]).
type Unread = fn(&mut MachineFacts);
const LOOK_KEYS: [(&str, Unread); 27] = [
    ("os", |m| m.os = None),
    ("target", |m| m.target = None),
    ("cores", |m| m.look.cores = None),
    ("load1_x100", |m| m.look.load_x100 = None),
    ("load5_x100", |m| m.look.load_x100 = None),
    ("load15_x100", |m| m.look.load_x100 = None),
    ("cpu_busy_x100", |m| m.look.cpu_busy_x100 = None),
    ("rate_win_ms", |m| m.look.rate_win_ms = None),
    ("mem_total_mb", |m| m.look.mem_total_mb = None),
    ("mem_used_mb", |m| m.look.mem_used_mb = None),
    ("mem_avail_mb", |m| m.look.mem_avail_mb = None),
    ("mem_pressure", |m| m.look.mem_pressure = None),
    ("mem_psi60_x100", |m| m.look.mem_psi60_x100 = None),
    ("swap_used_mb", |m| m.look.swap_used_mb = None),
    ("swap_total_mb", |m| m.look.swap_total_mb = None),
    ("gpu_cores", |m| m.look.gpu_cores = None),
    ("gpu_busy_pct", |m| m.look.gpu_busy_pct = None),
    ("net_link_kbps", |m| m.look.net_link_kbps = None),
    ("net_rx_kbps", |m| m.look.net_rx_kbps = None),
    ("net_tx_kbps", |m| m.look.net_tx_kbps = None),
    ("net_other_kbps", |m| m.look.net_other_kbps = None),
    ("up_since_ms", |m| m.up_since_ms = None),
    ("cpu_lim", |m| m.look.limits.cpu = None),
    ("mem_lim", |m| m.look.limits.mem = None),
    ("disk_lim", |m| m.look.limits.disk = None),
    ("gpu_lim", |m| m.look.limits.gpu = None),
    ("load", |m| m.look.load = None),
];

/// The sample's facts reply and, of its header, the token that carries `key`.
fn sample_wire() -> String {
    sample_facts().to_survey_reply(
        4242,
        Some("m22.local"),
        1_793_160_000_000,
        1,
        (aterm_types::fleet_access::FleetAccess::Full, true),
    )
}

fn sample_token(key: &str) -> String {
    SAMPLE_MACHINE_TOKENS
        .split(' ')
        .find(|token| token.split_once('=').is_some_and(|(k, _)| k == key))
        .map(|token| format!(" {token}"))
        .unwrap_or_else(|| panic!("the sample sends {key}"))
}

/// A MACHINE'S LOOK CROSSES THE WIRE: the header carries every key after `home_build_partial=`,
/// at facts as at content (a resource is no location), still ends with its access and drive
/// words, and reads back figure for figure. An older member's header, which has none of the
/// keys, reads as no look at all, and one key left out reads as that figure unread with the rest
/// as they were.
/// NEGATIVE CONTROL (2026-11-12): with a missing key read as 0, the older member's look reads
/// as a machine with no cores and no memory, and each one-key case fails on its figure.
#[test]
fn a_machines_look_crosses_the_wire_and_an_older_member_sends_none() {
    let facts = sample_facts();
    let machine = facts.machine.clone().expect("the sample carries a look");
    assert_eq!(machine.header_tokens(), SAMPLE_MACHINE_TOKENS);
    assert_eq!(facts.machine_tokens(), SAMPLE_MACHINE_TOKENS);
    let wire = sample_wire();
    let head = wire.lines().nth(1).expect("the instance line");
    assert!(
        head.contains(&format!(
            "{SAMPLE_DISK_TOKENS}{SAMPLE_MACHINE_TOKENS} accounts=3 "
        )),
        "{head}"
    );
    assert!(
        head.ends_with(
            " roster_unread=0 store_worst=vault-records store_worst_pct=83 access=full drive=1"
        ),
        "{head}"
    );
    let parsed = SwarmSurveyFacts::from_survey_reply(&wire).expect("valid");
    assert_eq!(parsed.machine.as_ref(), Some(&machine));
    assert_eq!(parsed.home_free_mb, Some(812_345));

    let content = facts.to_survey_reply_at(
        true,
        4242,
        None,
        1_793_160_000_000,
        1,
        (aterm_types::fleet_access::FleetAccess::ReadOnly, false),
    );
    let head = content.lines().nth(1).expect("the instance line");
    assert!(
        head.contains(&format!(
            "{SAMPLE_DISK_TOKENS}{SAMPLE_MACHINE_TOKENS} home=/Users//a%20yates "
        )),
        "{head}"
    );
    assert!(head.ends_with(" access=read"), "{head}");

    let older = wire.replacen(SAMPLE_MACHINE_TOKENS, "", 1);
    assert_ne!(older, wire);
    let parsed = SwarmSurveyFacts::from_survey_reply(&older).expect("an older aterm's");
    assert_eq!(parsed.machine, None, "no look, never a look of zeros");
    assert_eq!(
        parsed,
        SwarmSurveyFacts {
            machine: None,
            home: None,
            repos: None,
            ..facts.clone()
        },
        "and the rest as it sent it, its free space among it"
    );
    let no_look = SwarmSurveyFacts {
        machine: None,
        ..facts.clone()
    };
    assert_eq!(no_look.machine_tokens(), "");
    assert_eq!(
        no_look.to_survey_reply(
            4242,
            Some("m22.local"),
            1_793_160_000_000,
            1,
            (aterm_types::fleet_access::FleetAccess::Full, true),
        ),
        older,
        "facts with no look say none of its keys"
    );

    for (key, unread) in LOOK_KEYS {
        let without = wire.replacen(&sample_token(key), "", 1);
        assert_ne!(without, wire, "{key}");
        let mut expected = machine.clone();
        unread(&mut expected);
        assert_ne!(expected, machine, "{key}: the sample reads it");
        assert_eq!(
            SwarmSurveyFacts::from_survey_reply(&without)
                .unwrap_or_else(|why| panic!("{key}: {why}"))
                .machine,
            Some(expected),
            "{key}"
        );
    }
    // The free space is the header's own older key: without it the look has none.
    let parsed = SwarmSurveyFacts::from_survey_reply(&wire.replacen(" home_free_mb=812345", "", 1))
        .expect("valid");
    assert_eq!(
        parsed.machine,
        Some(MachineFacts {
            look: MachineLook {
                disk_free_mb: None,
                ..machine.look.clone()
            },
            ..machine.clone()
        })
    );
    // So is the size: without it neither the facts nor the look say one.
    let sizeless = wire.replacen(" home_total_mb=1907000", "", 1);
    assert_ne!(sizeless, wire);
    let parsed = SwarmSurveyFacts::from_survey_reply(&sizeless).expect("valid");
    assert_eq!(parsed.home_total_mb, None);
    assert_eq!(
        parsed.machine,
        Some(MachineFacts {
            look: MachineLook {
                disk_total_mb: None,
                ..machine.look.clone()
            },
            ..machine.clone()
        })
    );
}

/// AN UNREAD FIGURE IS A DASH ON THE WIRE: a look that read nothing says `-` under every key
/// and reads back as a look that read nothing, and a verdict of "within its limit" (`0`) is
/// not that dash. The up-since instant of an OS that says none is unread, never the epoch.
/// NEGATIVE CONTROL (2026-11-12): with a token written through `unwrap_or(0)`, its key says
/// `=0` here.
#[test]
fn an_unread_figure_is_a_dash_on_the_wire() {
    let unread = MachineFacts {
        look: MachineLook::UNREAD,
        up_since_ms: None,
        os: None,
        target: None,
    };
    let tokens = unread.header_tokens();
    let said: Vec<(&str, &str)> = tokens
        .split(' ')
        .filter_map(|token| token.split_once('='))
        .collect();
    assert_eq!(
        said.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
        SAMPLE_MACHINE_TOKENS
            .split(' ')
            .filter_map(|token| Some(token.split_once('=')?.0))
            .collect::<Vec<_>>(),
        "every key, in the sample's order"
    );
    for (key, value) in said {
        assert_eq!(value, "-", "{key}");
    }
    let facts = SwarmSurveyFacts {
        machine: Some(unread.clone()),
        home_free_mb: None,
        home_total_mb: None,
        home_build: None,
        store_worst: None,
        ..sample_facts()
    };
    let wire = facts.to_survey_reply(
        1,
        None,
        0,
        1,
        (aterm_types::fleet_access::FleetAccess::ReadOnly, false),
    );
    assert!(
        wire.contains(
            " home_free_mb=- home_total_mb=- home_build_mb=- home_build_at_ms=- \
             home_build_partial=- os=- "
        ),
        "{wire}"
    );
    assert_eq!(
        SwarmSurveyFacts::from_survey_reply(&wire)
            .expect("valid")
            .machine,
        Some(unread),
        "a look that read nothing, not no look"
    );

    let within = MachineFacts {
        look: MachineLook {
            limits: Limits {
                cpu: Some(false),
                mem: Some(false),
                disk: Some(false),
                gpu: Some(false),
            },
            gpu_busy_pct: Some(0),
            net_other_kbps: Some(0),
            ..MachineLook::UNREAD
        },
        up_since_ms: None,
        os: None,
        target: None,
    };
    let tokens = within.header_tokens();
    for token in [
        " cpu_lim=0 mem_lim=0 disk_lim=0 gpu_lim=0",
        " gpu_busy_pct=0 ",
        " net_other_kbps=0 ",
    ] {
        assert!(tokens.contains(token), "a read zero is said: {tokens}");
    }
    assert_eq!(up_since_ms(0), None);
    assert_eq!(up_since_ms(1_793_100_000), Some(1_793_100_000_000));
    assert_eq!(up_since_ms(u64::MAX), None, "no wrapped instant");
}

/// A MALFORMED MACHINE FIGURE IS UNREAD AND THE REPLY STILL PARSES: a key whose value is not a
/// whole number in its range, or not a word of its closed set, reads as that one figure unread;
/// the look's other figures, the accounts and the rest of the reply read as they were sent. A
/// reader that uses none of these figures does not lose the machine to one of them.
/// CONTROL: the same damage to `home_free_mb`, an older key, still refuses the reply.
/// NEGATIVE CONTROL (2026-11-12): with a new key parsed by `?`, each case is an `Err`.
#[test]
fn a_malformed_machine_figure_is_unread_and_the_reply_still_parses() {
    let facts = sample_facts();
    let machine = facts.machine.clone().expect("the sample carries a look");
    let wire = sample_wire();
    let read = |wire: &str, why: &str| {
        SwarmSurveyFacts::from_survey_reply(wire).unwrap_or_else(|err| panic!("{why}: {err}"))
    };
    let rest = SwarmSurveyFacts {
        home: None,
        repos: None,
        ..facts.clone()
    };
    for (key, unread) in LOOK_KEYS {
        let mut expected = machine.clone();
        unread(&mut expected);
        for bad in [
            "x",
            "",
            "-1",
            "+3",
            "1.5",
            "0x10",
            "1e3",
            "99999999999999999999999",
            "%20",
        ] {
            if (key, bad) == ("target", "x") {
                // One lowercase word has a target's shape; the cases below damage it.
                continue;
            }
            let broken = wire.replacen(&sample_token(key), &format!(" {key}={bad}"), 1);
            assert_ne!(broken, wire, "{key}={bad}");
            assert_eq!(
                read(&broken, &format!("{key}={bad}")),
                SwarmSurveyFacts {
                    machine: Some(expected.clone()),
                    ..rest.clone()
                },
                "{key}={bad}"
            );
        }
    }
    // Its own time damaged: a look with no time, never no look and never time 0.
    let timeless = wire.replacen(&sample_token("m_at_ms"), " m_at_ms=soon", 1);
    assert_eq!(
        read(&timeless, "m_at_ms").machine,
        Some(MachineFacts {
            look: MachineLook {
                at_unix_ms: None,
                ..machine.look.clone()
            },
            ..machine.clone()
        })
    );
    // In range for a number and out of range for its figure.
    let long_target = format!("a{}", "b".repeat(48));
    let ranged: [(&str, &str, Unread); 11] = [
        ("cores", "0", |m| m.look.cores = None),
        ("cores", "4294967296", |m| m.look.cores = None),
        ("mem_total_mb", "0", |m| m.look.mem_total_mb = None),
        ("gpu_busy_pct", "101", |m| m.look.gpu_busy_pct = None),
        ("gpu_busy_pct", "256", |m| m.look.gpu_busy_pct = None),
        ("mem_pressure", "urgent", |m| m.look.mem_pressure = None),
        ("cpu_lim", "2", |m| m.look.limits.cpu = None),
        ("up_since_ms", "0", |m| m.up_since_ms = None),
        ("os", "plan9", |m| m.os = None),
        ("target", &long_target, |m| m.target = None),
        ("load", "101:0", |m| m.look.load = None),
    ];
    for (key, bad, unread) in ranged {
        let mut expected = machine.clone();
        unread(&mut expected);
        let broken = wire.replacen(&sample_token(key), &format!(" {key}={bad}"), 1);
        assert_eq!(read(&broken, key).machine, Some(expected), "{key}={bad}");
    }
    for words in ["a%20b", "Aarch64-apple-darwin", "9front", "a/b"] {
        let broken = wire.replacen(&sample_token("target"), &format!(" target={words}"), 1);
        assert_eq!(
            read(&broken, words).machine.and_then(|m| m.target),
            None,
            "{words}"
        );
    }
    // A target this build does not know is a newer aterm's, and reads.
    let newer = wire.replacen(
        &sample_token("target"),
        " target=riscv64gc-unknown-linux-gnu",
        1,
    );
    assert_eq!(
        read(&newer, "a newer target")
            .machine
            .and_then(|m| m.target)
            .as_deref(),
        Some("riscv64gc-unknown-linux-gnu")
    );
    assert!(
        SwarmSurveyFacts::from_survey_reply(&wire.replacen(
            " home_free_mb=812345",
            " home_free_mb=x",
            1
        ))
        .is_err(),
        "CONTROL: an older key keeps its rule"
    );
}

/// THE LOAD SCORE CROSSES THE WIRE: the deck dial's level and over, in hundredths, go as one
/// `load=<level>:<over>` with the machine's other keys, after its four verdicts and before the
/// header's `access=`, and read back equal: the sample's, both ends of the range, a machine at
/// rest (a read `0:0`) and one whose over is unread (`<level>:-`, never `<level>:0`).
/// NEGATIVE CONTROL (2026-11-13): with the two parts read back in each other's place, or the
/// key left unread by the parser, the score does not read back equal.
#[test]
fn the_load_score_crosses_the_wire_and_reads_back_equal() {
    let wire = sample_wire();
    let head = wire.lines().nth(1).expect("the instance line");
    assert!(head.contains(" gpu_lim=1 load=88:21 accounts=3 "), "{head}");
    assert!(head.ends_with(" access=full drive=1"), "{head}");
    for (level_x100, over_x100, said) in [
        (88, Some(21), "88:21"),
        (0, Some(0), "0:0"),
        (100, Some(100), "100:100"),
        (100, Some(12), "100:12"),
        (7, Some(0), "7:0"),
        (100, None, "100:-"),
    ] {
        let load = Some(LoadScore {
            level_x100,
            over_x100,
        });
        let mut facts = sample_facts();
        facts.machine.as_mut().expect("a look").look.load = load;
        let wire = facts.to_survey_reply(
            4242,
            Some("m22.local"),
            1_793_160_000_000,
            1,
            (aterm_types::fleet_access::FleetAccess::ReadOnly, false),
        );
        assert!(wire.contains(&format!(" load={said} ")), "{wire}");
        let parsed = SwarmSurveyFacts::from_survey_reply(&wire).expect("valid");
        assert_eq!(parsed.machine.map(|m| m.look.load), Some(load));
    }
}

/// AN UNREAD LOAD LEVEL IS A DASH AND READS BACK UNREAD: a machine whose dial has no level (its
/// CPU's busy share unread) says `load=-`, never `load=0:0`, and a reader reads it as no score,
/// beside the figures that were read. CONTROL: a machine at rest says `load=0:0`, a score.
/// NEGATIVE CONTROL (2026-11-13): with a writer that prints `0:0` for an unread score, the
/// header says `load=0:0` and reads back as a machine at rest.
#[test]
fn an_unread_load_level_is_a_dash_and_reads_back_unread() {
    let mut facts = sample_facts();
    facts.machine.as_mut().expect("a look").look.load = None;
    let wire = facts.to_survey_reply(
        4242,
        Some("m22.local"),
        1_793_160_000_000,
        1,
        (aterm_types::fleet_access::FleetAccess::Full, true),
    );
    let head = wire.lines().nth(1).expect("the instance line");
    assert!(head.contains(" gpu_lim=1 load=- accounts=3 "), "{head}");
    let parsed = SwarmSurveyFacts::from_survey_reply(&wire).expect("valid");
    let machine = parsed.machine.expect("a look");
    assert_eq!(machine.look.load, None, "unread, never 0");
    assert_eq!(machine, facts.machine.clone().expect("a look"));

    let at_rest = Some(LoadScore {
        level_x100: 0,
        over_x100: Some(0),
    });
    facts.machine.as_mut().expect("a look").look.load = at_rest;
    let tokens = facts.machine_tokens();
    assert!(tokens.ends_with(" load=0:0"), "CONTROL: {tokens}");
}

/// A MALFORMED LOAD READS UNREAD AND THE REST STILL READS: `load=x:y`, one part alone, a third
/// part, a part past 100, a sign or a decimal is no score, while the machine's verdicts (its
/// `cpu_lim` among them), its other figures and the rest of the reply read as they were sent.
/// NEGATIVE CONTROLS (2026-11-13): with a malformed part read as 0 the score reads; with a
/// malformed score failing the machine's look, `cpu_lim` reads nothing.
#[test]
fn a_malformed_load_reads_unread_while_cpu_lim_still_reads() {
    let facts = sample_facts();
    let mut expected = facts.machine.clone().expect("a look");
    expected.look.load = None;
    let wire = sample_wire();
    for bad in [
        "x:y", "x:21", "88:y", "88", "88:", ":21", ":", "88:21:3", "101:21", "88:101", "-:21",
        "-:-", "88:--", "+8:21", "8.5:21", "300:0",
    ] {
        let broken = wire.replacen(" load=88:21", &format!(" load={bad}"), 1);
        assert_ne!(broken, wire, "{bad}");
        let parsed = SwarmSurveyFacts::from_survey_reply(&broken)
            .unwrap_or_else(|why| panic!("{bad}: {why}"));
        let machine = parsed.machine.as_ref().expect("a look");
        assert_eq!(machine.look.load, None, "{bad}");
        assert_eq!(machine.look.limits.cpu, Some(false), "{bad}: cpu_lim reads");
        assert_eq!(machine, &expected, "{bad}");
        assert_eq!(parsed.accounts, facts.accounts, "{bad}");
    }
}

/// THIS MACHINE SAYS ITS OWN LOOK, AND ONLY IN AN ANSWER: the look carries the OS, the target
/// atpkg selects by and the up-since instant. Collected facts carry none (the 3-minute facts
/// push sends them as collected, and a look is taken only inside an answer), with the one
/// volume read they always made; an answer takes the look, and the free space and the size are
/// then the look's own, of the volume holding the home the facts were collected for, read from
/// its path even where the path is no UTF-8 that `home` could spell. Facts a listener kept take
/// the look again. CONTROL: facts with no home read no volume.
#[test]
fn only_an_answer_takes_this_machines_look_and_it_reads_the_homes_volume() {
    let now = MachineFacts::now(None);
    assert_eq!(now.os, aterm_types::look_words::os_of(std::env::consts::OS));
    if cfg!(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "windows"
    )) {
        assert!(now.os.is_some());
        let target = now.target.as_deref().expect("a target atpkg knows");
        assert!(atpkg::TARGETS.contains(&target), "{target}");
    }
    assert!(now.look.cores.is_some(), "{now:?}");
    assert_eq!(
        (now.look.disk_total_mb, now.look.disk_free_mb),
        (None, None),
        "no home, no volume read"
    );
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        let up = now.up_since_ms.expect("the OS says since when it is up");
        let at = now.look.at_unix_ms.expect("a clock after the epoch");
        assert!(up < at, "up since {up}, looked at {at}");
    }

    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let facts =
        SwarmSurveyFacts::collect_at(&state, Some(&home), None, 0, false, None, 1_793_160_000);
    assert_eq!(facts.machine, None, "collected facts take no look");
    assert!(
        facts.home_free_mb.is_some() && facts.home_total_mb.is_some(),
        "the one volume read collected facts make: {facts:?}"
    );
    assert_eq!(facts.home_dir.as_deref(), Some(home.as_path()));
    assert!(
        !facts
            .to_survey_reply(
                1,
                Some("m22.local"),
                1_793_160_000_000,
                1,
                (aterm_types::fleet_access::FleetAccess::ReadOnly, false)
            )
            .contains(" m_at_ms="),
        "the push sends no look"
    );
    let mut answered = facts.clone();
    answered.take_latest_look();
    let machine = answered.machine.clone().expect("an answer takes a look");
    assert!(machine.look.disk_total_mb.is_some(), "{machine:?}");
    assert_eq!(answered.home_free_mb, machine.look.disk_free_mb);
    assert_eq!(answered.home_total_mb, machine.look.disk_total_mb);

    let mut kept = SwarmSurveyFacts {
        machine: Some(MachineFacts {
            look: MachineLook {
                at_unix_ms: Some(1),
                cores: Some(4096),
                ..MachineLook::UNREAD
            },
            ..sample_machine()
        }),
        home_free_mb: Some(7),
        ..facts.clone()
    };
    kept.take_latest_look();
    let taken = kept.machine.clone().expect("a look");
    assert!(
        taken.look.at_unix_ms >= machine.look.at_unix_ms,
        "{taken:?}"
    );
    assert_eq!(taken.look.cores, machine.look.cores);
    assert_eq!(taken.target, machine.target);
    assert_eq!(kept.home_free_mb, taken.look.disk_free_mb);
    assert!(taken.look.disk_total_mb.is_some(), "the kept home's volume");
    assert_eq!(kept.home_total_mb, taken.look.disk_total_mb);

    // A home whose path is no UTF-8 has no `home` words; its volume is read from its path.
    let mut unspelled = SwarmSurveyFacts {
        home: None,
        home_free_mb: Some(7),
        home_total_mb: Some(9),
        ..facts.clone()
    };
    unspelled.take_latest_look();
    let look = unspelled.machine.clone().expect("a look").look;
    assert!(look.disk_total_mb.is_some(), "{look:?}");
    assert_eq!(unspelled.home_free_mb, look.disk_free_mb);
    assert_eq!(unspelled.home_total_mb, look.disk_total_mb);

    let mut homeless = SwarmSurveyFacts {
        home: None,
        home_dir: None,
        home_free_mb: Some(7),
        home_total_mb: Some(9),
        ..facts
    };
    homeless.take_latest_look();
    let look = homeless.machine.expect("a look").look;
    assert_eq!(
        (
            look.disk_total_mb,
            homeless.home_free_mb,
            homeless.home_total_mb
        ),
        (None, None, None),
        "CONTROL: no home, no volume"
    );
}

/// A SURVEY HEADER CARRIES THE HOME VOLUME'S TOTAL, FREE AND BUILD BYTES: collected facts say
/// the size and the free space of the home's volume from their one volume read, and its build
/// output as the disk watch's record of its last sizing of that volume keeps it (the bytes, when
/// that sizing ended, whether it was cut short), never from a walk of the home. The header says
/// all five right after `ssh_host_key=`, at facts as at content, with no path and no volume key,
/// and they read back. An answer's look makes the size its own, and its header still says it
/// once. A sizing cut short is said as a floor. A record of another volume, one whose sizing
/// ended at no time after the epoch, and no record say no build output (`-`), and the size needs
/// no record.
/// NEGATIVE CONTROL: with the build output sized from the home (the bytes under its one marked
/// build folder) instead of read from the record, it says 1 MiB, not 5120; with the record's
/// volume unchecked, the other volume's 9216 is said; with the record's partial word dropped,
/// the floor reads as whole; with its time of 0 read as a time, a sizing at the epoch is said;
/// with the size left among the look's tokens too, the answer's header says `home_total_mb=`
/// twice.
#[test]
fn a_survey_header_carries_the_home_volumes_total_free_and_build_bytes() {
    use crate::harness::disk::{self, Rebuildable, budget};
    use aterm_types::fleet_access::FleetAccess;

    const NOW: i64 = 1_793_160_000;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    let home = dir.path().join("home");
    // A build folder in the home, marked as cargo marks one, which a survey that walked the home
    // would count.
    let build = home.join("proj/target/debug");
    std::fs::create_dir_all(&build).expect("a build folder");
    std::fs::write(build.join("blob"), vec![0_u8; 1 << 20]).expect("a build file");
    std::fs::write(
        build.parent().expect("its target").join(disk::RUSTC_INFO),
        "{}",
    )
    .expect("the build folder's marker");
    std::fs::create_dir_all(&state).expect("a state folder");
    let volume =
        disk::volume_of(&home, &aterm_sysprobe::volumes::table()).expect("the home's volume reads");
    let spelled = volume.key.spelled();
    let harness = state.join("harness").join(crate::harness::cli::HARNESS);
    // The disk watch's record, kept as its own sizing keeps one: claimed, then kept.
    let seed = |began: i64, key: String, sized: Rebuildable, sized_unix: i64| {
        assert!(
            matches!(
                budget::claim(&harness, began),
                Ok(budget::Claim::Claimed(_))
            ),
            "a sizing is due at {began}"
        );
        budget::keep(
            &harness,
            began,
            budget::Sizing {
                volume: key,
                total: volume.total,
                used: volume.total.saturating_sub(volume.free),
                sized,
                sized_unix,
            },
        )
        .expect("the record is kept");
    };
    let sized = Rebuildable {
        bytes: 5 << 30,
        partial: false,
    };
    seed(NOW, spelled.clone(), sized, NOW - 7200);
    let collect =
        |state: &Path| SwarmSurveyFacts::collect_at(state, Some(&home), None, 0, false, None, NOW);
    let facts = collect(&state);
    let total = volume.total >> 20;
    assert!(total > 0, "{volume:?}");
    assert_eq!(facts.home_total_mb, Some(total), "{facts:?}");
    let free = facts
        .home_free_mb
        .expect("the free space, read with the size");
    assert!(free <= total, "{free} MiB free of {total}");
    assert_eq!(
        facts.home_build,
        Some(HomeBuild {
            mb: 5120,
            at_ms: 1_793_152_800_000,
            partial: false,
        }),
        "the record's figure, never a walk's"
    );
    let said = format!(
        " home_free_mb={free} home_total_mb={total} home_build_mb=5120 \
         home_build_at_ms=1793152800000 home_build_partial=0 "
    );
    let root = dir.path().to_str().expect("a scratch path in UTF-8");
    let access = (FleetAccess::ReadOnly, false);
    for content in [false, true] {
        let wire =
            facts.to_survey_reply_at(content, 1, Some("m22.local"), 1_793_160_000_000, 1, access);
        let head = wire.lines().nth(1).expect("the instance line");
        let key_at = head.find(" ssh_host_key=").expect("its host key");
        let after = &head[key_at + 1..];
        let after = &after[after.find(' ').expect("more after the host key")..];
        assert!(after.starts_with(&said), "content={content}: {head}");
        assert!(!head.contains(&spelled), "no volume key: {head}");
        assert_eq!(
            head.matches(aterm_codec::pct::encode(root).as_str())
                .count(),
            usize::from(content),
            "no path but the home's own `home=` at content: {head}"
        );
        let parsed = SwarmSurveyFacts::from_survey_reply(&wire).expect("valid");
        assert_eq!(
            (parsed.home_free_mb, parsed.home_total_mb, parsed.home_build),
            (facts.home_free_mb, facts.home_total_mb, facts.home_build),
            "content={content}"
        );
    }

    // An answer's look makes the size its own, and says it once.
    let mut answered = facts.clone();
    answered.take_latest_look();
    let look = &answered
        .machine
        .as_ref()
        .expect("an answer takes a look")
        .look;
    assert_eq!(answered.home_total_mb, look.disk_total_mb);
    assert_eq!(answered.home_total_mb, Some(total), "the one volume's size");
    assert_eq!(
        answered.home_build, facts.home_build,
        "the record, as collected"
    );
    let wire = answered.to_survey_reply(1, None, 1_793_160_000_000, 1, access);
    let head = wire.lines().nth(1).expect("the instance line");
    assert!(head.contains(" m_at_ms="), "{head}");
    assert_eq!(head.matches(" home_total_mb=").count(), 1, "{head}");

    // Another volume's sizing says nothing of this one.
    seed(
        NOW + budget::EVERY_S,
        format!("{spelled}-elsewhere"),
        Rebuildable {
            bytes: 9 << 30,
            partial: true,
        },
        NOW + budget::EVERY_S,
    );
    let elsewhere = collect(&state);
    assert_eq!(elsewhere.home_build, None, "{elsewhere:?}");
    assert_eq!(elsewhere.home_total_mb, Some(total));
    assert!(
        elsewhere
            .to_survey_reply(1, None, 0, 1, access)
            .contains(" home_build_mb=- home_build_at_ms=- home_build_partial=- "),
        "unknown, never 0"
    );

    // A sizing cut short is a floor, said as one.
    seed(
        NOW + 2 * budget::EVERY_S,
        spelled.clone(),
        Rebuildable {
            bytes: 3 << 20,
            partial: true,
        },
        NOW,
    );
    assert_eq!(
        collect(&state).home_build,
        Some(HomeBuild {
            mb: 3,
            at_ms: 1_793_160_000_000,
            partial: true,
        }),
        "a floor"
    );
    // A sizing whose end does not read as a time after the epoch says none.
    seed(NOW + 3 * budget::EVERY_S, spelled, sized, 0);
    assert_eq!(collect(&state).home_build, None, "a sizing at 0 is none");

    // A state with no record: no build output, and the size needs none.
    let fresh = dir.path().join("fresh");
    std::fs::create_dir_all(&fresh).expect("a fresh state folder");
    let sizeless = collect(&fresh);
    assert_eq!(
        (sizeless.home_build, sizeless.home_total_mb),
        (None, Some(total))
    );
}

/// AN OLDER HEADER WITHOUT TOTAL OR BUILD BYTES READS UNKNOWN, NEVER ZERO: a fleet runs mixed
/// versions during an update. A header from an aterm before the size and the build keys (and
/// before the look) reads no size, no build output and no look, and is said again with `-` under
/// each new key, never 0. One from the aterm that said the size among the look's keys still
/// reads its size, into the facts and the look alike, and no build output. A reader of that
/// aterm finds every key it knows in a newer header, with its value, the size once and the free
/// space where it stood; the newer header adds the three build keys and nothing else. A new key
/// that is damaged or unsent reads as unknown alone, and the reply still parses; a size of 0 is
/// no size, a sizing at time 0 none, and build output of 0 a measured 0.
/// CONTROL: the same damage to `home_free_mb`, an older key, still refuses the reply.
/// NEGATIVE CONTROL: with a new figure written or read through `unwrap_or(0)`, the older
/// header's facts are said again with `=0`; with a new key parsed by `?`, each damaged case is
/// an `Err`; with the size still among the look's tokens, the newer header says it twice.
#[test]
fn an_older_header_without_total_or_build_bytes_reads_unknown_never_zero() {
    use aterm_types::fleet_access::FleetAccess;

    /// The header of [`sample_wire`] as the aterm before these keys wrote it, frozen: the size
    /// among the look's keys, after `swap_total_mb=`, and no build output.
    const SIZED_IN_THE_LOOK: &str = "instance pid=4242 host=m22.local version=0.146.0 sessions=4 \
         level=facts cut=0 at_ms=1793160000000 node=f1e2d3c4 instances=1 label=m22 window=1 \
         window_last=1793160000 ssh_host_key=SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU \
         home_free_mb=812345 os=macos target=aarch64-apple-darwin m_at_ms=1793159998500 \
         cores=12 load1_x100=345 load5_x100=280 load15_x100=199 cpu_busy_x100=1130 \
         rate_win_ms=60250 mem_total_mb=65536 mem_used_mb=40100 mem_avail_mb=21900 \
         mem_pressure=warn mem_psi60_x100=1234 swap_used_mb=2048 swap_total_mb=4096 \
         home_total_mb=1907000 gpu_cores=30 gpu_busy_pct=97 net_link_kbps=1000000 \
         net_rx_kbps=5120 net_tx_kbps=640 net_other_kbps=17 up_since_ms=1793100000000 \
         cpu_lim=0 mem_lim=1 disk_lim=0 gpu_lim=1 load=88:21 accounts=3 \
         accounts_unread=grok,kimi moves_7d=2 move_tries_7d=3 last_move=1793159000 \
         roster_unread=0 access=full drive=1";

    let facts = sample_facts();
    let machine = facts.machine.clone().expect("the sample carries a look");
    let wire = sample_wire();
    let read = |wire: &str, why: &str| {
        SwarmSurveyFacts::from_survey_reply(wire).unwrap_or_else(|err| panic!("{why}: {err}"))
    };
    let rest = SwarmSurveyFacts {
        home: None,
        repos: None,
        ..facts.clone()
    };

    // (a) An aterm before the new keys and before the look.
    let mut older = wire.replacen(SAMPLE_MACHINE_TOKENS, "", 1);
    for token in [
        " home_total_mb=1907000",
        " home_build_mb=84992",
        " home_build_at_ms=1793152800000",
        " home_build_partial=1",
        " store_worst=vault-records",
        " store_worst_pct=83",
    ] {
        let without = older.replacen(token, "", 1);
        assert_ne!(without, older, "{token}");
        older = without;
    }
    let parsed = read(&older, "an older aterm's");
    assert_eq!(
        parsed,
        SwarmSurveyFacts {
            home_total_mb: None,
            home_build: None,
            store_worst: None,
            machine: None,
            ..rest.clone()
        },
        "unknown, never 0, and the rest as it sent it"
    );
    let again = parsed.to_survey_reply(
        4242,
        Some("m22.local"),
        1_793_160_000_000,
        1,
        (FleetAccess::Full, true),
    );
    let head = again.lines().nth(1).expect("the instance line");
    assert!(
        head.contains(
            " home_free_mb=812345 home_total_mb=- home_build_mb=- home_build_at_ms=- \
             home_build_partial=- accounts=3 "
        ),
        "{head}"
    );

    // (b) The aterm before this one: its size reads, in the facts and the look alike.
    let new_head = wire.lines().nth(1).expect("the instance line");
    let sized_in_look = wire.replacen(new_head, SIZED_IN_THE_LOOK, 1);
    assert_ne!(sized_in_look, wire);
    let parsed = read(&sized_in_look, "the aterm before this one's");
    assert_eq!(
        parsed,
        SwarmSurveyFacts {
            home_build: None,
            store_worst: None,
            ..rest.clone()
        },
        "its size and its look as it sent them, and no build output"
    );
    assert_eq!(
        parsed.machine.and_then(|m| m.look.disk_total_mb),
        Some(1_907_000)
    );

    // (c) A reader of that aterm, reading a newer header by key.
    let keyed = |line: &str| -> Vec<(String, String)> {
        line.split(' ')
            .skip(1)
            .filter_map(|token| token.split_once('='))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect()
    };
    let before = keyed(SIZED_IN_THE_LOOK);
    let after = keyed(new_head);
    for (key, value) in &before {
        assert_eq!(
            after.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            Some(value),
            "{key}: {new_head}"
        );
    }
    assert_eq!(
        after
            .iter()
            .filter(|(key, _)| key == "home_total_mb")
            .count(),
        1,
        "the size once: {new_head}"
    );
    let added: Vec<&str> = after
        .iter()
        .map(|(key, _)| key.as_str())
        .filter(|key| before.iter().all(|(known, _)| known.as_str() != *key))
        .collect();
    assert_eq!(
        added,
        [
            "home_build_mb",
            "home_build_at_ms",
            "home_build_partial",
            "store_worst",
            "store_worst_pct"
        ]
    );
    let host_key = after
        .iter()
        .position(|(key, _)| key == "ssh_host_key")
        .expect("its host key");
    assert_eq!(
        after[host_key + 1].0,
        "home_free_mb",
        "where it stood: {new_head}"
    );

    // (d) A new key damaged or unsent reads as unknown alone, and the reply still parses.
    let sizeless = SwarmSurveyFacts {
        home_total_mb: None,
        machine: Some(MachineFacts {
            look: MachineLook {
                disk_total_mb: None,
                ..machine.look.clone()
            },
            ..machine.clone()
        }),
        ..rest.clone()
    };
    let unbuilt = SwarmSurveyFacts {
        home_build: None,
        ..rest.clone()
    };
    for (key, sent, expected) in [
        ("home_total_mb", "1907000", &sizeless),
        ("home_build_mb", "84992", &unbuilt),
        ("home_build_at_ms", "1793152800000", &unbuilt),
        ("home_build_partial", "1", &unbuilt),
    ] {
        let token = format!(" {key}={sent} ");
        for bad in ["x", "", "-1", "1.5", "99999999999999999999999"] {
            let broken = wire.replacen(&token, &format!(" {key}={bad} "), 1);
            assert_ne!(broken, wire, "{key}={bad}");
            assert_eq!(
                &read(&broken, &format!("{key}={bad}")),
                expected,
                "{key}={bad}"
            );
        }
        let unsent = wire.replacen(&token, " ", 1);
        assert_ne!(unsent, wire, "{key}");
        assert_eq!(&read(&unsent, key), expected, "{key} unsent");
    }
    let zero = wire.replacen(" home_total_mb=1907000 ", " home_total_mb=0 ", 1);
    assert_eq!(
        read(&zero, "a size of 0"),
        sizeless,
        "a size of 0 is no size"
    );
    let untimed = wire.replacen(
        " home_build_at_ms=1793152800000 ",
        " home_build_at_ms=0 ",
        1,
    );
    assert_eq!(
        read(&untimed, "a time of 0"),
        unbuilt,
        "a sizing at 0 is none"
    );
    let none_built = wire.replacen(" home_build_mb=84992 ", " home_build_mb=0 ", 1);
    assert_eq!(
        read(&none_built, "a measured 0").home_build,
        Some(HomeBuild {
            mb: 0,
            ..facts.home_build.expect("the sample's build output")
        }),
        "a measured 0 is 0"
    );

    // (e) CONTROL: an older key keeps its rule.
    let free_broken = wire.replacen(" home_free_mb=812345 ", " home_free_mb=x ", 1);
    assert_ne!(free_broken, wire);
    assert!(SwarmSurveyFacts::from_survey_reply(&free_broken).is_err());
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
    // Every key the look writes is read back into its own figure: the sample leaves none
    // unread and no two alike, so a key written and not read (or read into another's field)
    // differs here.
    let machine = expected
        .machine
        .as_ref()
        .expect("the sample carries a look");
    assert_eq!(parsed.machine.as_ref(), Some(machine));
    for (key, forget) in LOOK_KEYS {
        let mut without = machine.clone();
        forget(&mut without);
        assert_ne!(&without, machine, "{key}: the sample holds a figure for it");
    }
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
        lifts: Vec::new(),
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
        lifts: Vec::new(),
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
    // R38: the release lease's checked form refuses an unnamed Mac at once, saying why.
    let at_once = std::time::Instant::now();
    let unnamed = super::machine_label_within(
        &super::env_at(&state, Some(&home), Some(&config), 1_793_160_000),
        super::RELEASE_ROSTER_WAIT,
    );
    assert!(
        matches!(
            unnamed,
            Err(super::NoLabel::Denied | super::NoLabel::Unnamed)
        ),
        "{unnamed:?}"
    );
    assert!(
        at_once.elapsed() < std::time::Duration::from_secs(5),
        "no wait for an answer"
    );

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
    // R38 NEGATIVE CONTROL: a denied recipient is still refused at once under the release's wait.
    let at_once = std::time::Instant::now();
    let denied = super::machine_label_within(
        &super::env_at(&state, Some(&home), Some(&config), 1_793_160_000),
        super::RELEASE_ROSTER_WAIT,
    );
    assert_eq!(denied, Err(super::NoLabel::Denied));
    assert!(
        at_once.elapsed() < std::time::Duration::from_secs(5),
        "no wait for a denial"
    );
    assert!(
        denied
            .unwrap_err()
            .to_string()
            .contains("denies this Mac's vault recipient"),
        "the lease says why"
    );

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
    // R38: a writer held past a wait reads as unread with its reason and the wait, never as "no
    // label"; under the release's wait the same writer, released after 800 ms, is waited out.
    // Before, the lease read through the survey's 250 ms and ended the release on this writer.
    let env = super::env_at(&state, Some(&home), None, 1_793_160_000);
    let short = std::time::Duration::from_millis(300);
    match super::machine_label_within(&env, short) {
        Err(super::NoLabel::Unread { why, waited }) => {
            assert_eq!(waited, short);
            assert!(why.contains("busy"), "{why}");
        }
        other => panic!("a writer held past the wait is an unread roster, not {other:?}"),
    }
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(800));
        drop(writer);
    });
    assert_eq!(
        super::machine_label_within(&env, super::RELEASE_ROSTER_WAIT).as_deref(),
        Ok("m100"),
        "the release waits a vault writer out"
    );
    release.join().unwrap();
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

/// THE FULLEST STORE CROSSES THE WIRE as a fixed word and a share, the header's last keys before
/// its access words, and reads back as sent; a header without them, or with either unreadable
/// or a name that is no fixed word's shape, reads as none and the rest of the reply still
/// reads. A share of 0 is a measurement, and one past 100 is a store past its bound.
/// NEGATIVE CONTROL: with the name taken as sent, `store_worst=a%40b.example` reads as a store
/// and `aterm fleet` shows an address in its STORE column.
#[test]
fn the_fullest_store_crosses_the_wire_as_a_fixed_word_and_a_share_or_not_at_all() {
    let facts = sample_facts();
    let wire = sample_wire();
    let sent = " store_worst=vault-records store_worst_pct=83 ";
    assert!(wire.contains(sent), "{wire}");
    let read = |wire: &str| SwarmSurveyFacts::from_survey_reply(wire).expect("the reply reads");
    assert_eq!(
        read(&wire).store_worst,
        StoreWorst::new("vault-records", 83)
    );
    let unknown = SwarmSurveyFacts {
        store_worst: None,
        home: None,
        repos: None,
        ..facts.clone()
    };
    for bad in [
        " store_worst=- store_worst_pct=- ",
        " store_worst=vault-records store_worst_pct=- ",
        " store_worst=vault-records store_worst_pct=x ",
        " store_worst=vault-records ",
        " store_worst_pct=83 ",
        " store_worst=a%40b.example store_worst_pct=83 ",
        " store_worst=Vault store_worst_pct=83 ",
        " store_worst=--- store_worst_pct=83 ",
        " store_worst=abcdefghijklmnopqrstuvwxyzabcdefg store_worst_pct=83 ",
        " ",
    ] {
        let broken = wire.replacen(sent, bad, 1);
        assert_ne!(broken, wire, "{bad}");
        assert_eq!(read(&broken), unknown, "{bad}");
    }
    for (pct, said) in [(0, "0"), (100, "100"), (104, "104")] {
        let worst = StoreWorst::new("journal-outbox", pct);
        let facts = SwarmSurveyFacts {
            store_worst: worst.clone(),
            ..facts.clone()
        };
        let head = facts.header_tokens(false);
        assert!(
            head.ends_with(&format!(
                " store_worst=journal-outbox store_worst_pct={said}"
            )),
            "{head}"
        );
    }
    // Collected facts carry none until the answering window or node job sizes the stores: `-`,
    // never 0.
    let unread = SwarmSurveyFacts {
        store_worst: None,
        ..facts
    };
    assert!(
        unread
            .header_tokens(false)
            .ends_with(" store_worst=- store_worst_pct=-"),
    );
    assert_eq!(StoreWorst::new("", 1), None);
    assert_eq!(StoreWorst::new("commons-table", 1).map(|w| w.pct), Some(1));
}
