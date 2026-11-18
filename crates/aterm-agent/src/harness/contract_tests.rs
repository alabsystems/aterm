// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn scoped(holder: &str, words: &[&str]) -> Result<Claim, String> {
    match read(holder, words) {
        Words::Scoped(c) => Ok(c),
        other => Err(format!("{other:?}")),
    }
}

fn refused(holder: &str, words: &[&str]) -> bool {
    matches!(read(holder, words), Words::Refused(_))
}

/// THE HANDSHAKE: aterm serves codex-alab's contract 2, the seed rows' contract 3 and every
/// registry row's contract 4, as a set.
#[test]
fn the_served_contracts_are_two_three_and_four() {
    assert_eq!(served_word(), "2,3,4");
    assert!(SERVED.contains(&SCOPED) && SERVED.contains(&UNIVERSAL));
}

/// grok-alab, muse-alab and kimi-alab may relaunch and upgrade; claude-code-alab may claim every
/// duty. Nothing else is a rostered holder, codex-alab included (its v2 rule stays its own), and
/// neither is claude-code-alab's tool name or its vendor's.
#[test]
fn the_roster_names_each_harness_and_the_duties_it_may_claim() {
    let (row, pid) = holder("grok-alab@4242").expect("rostered");
    assert_eq!(
        (row.id, row.program, row.package, pid),
        ("grok-alab", Program::Grok, "grok-alab", 4242)
    );
    assert_eq!(row.allows, &[Scope::Relaunch, Scope::Upgrade]);
    assert_eq!(row_for(Program::Grok).map(|r| r.id), Some("grok-alab"));
    assert_eq!(row_named("Grok-Alab").map(|r| r.id), Some("grok-alab"));
    let (row, pid) = holder("muse-alab@4243").expect("rostered");
    assert_eq!(
        (row.id, row.program, row.package, row.vendor, pid),
        ("muse-alab", Program::Muse, "muse-alab", "muse", 4243)
    );
    assert_eq!(row.allows, &[Scope::Relaunch, Scope::Upgrade]);
    assert_eq!(row_for(Program::Muse).map(|r| r.id), Some("muse-alab"));
    assert_eq!(row_named("MUSE-ALAB").map(|r| r.id), Some("muse-alab"));
    let (row, pid) = holder("kimi-alab@4244").expect("rostered");
    assert_eq!(
        (row.id, row.program, row.package, row.vendor, pid),
        ("kimi-alab", Program::Kimi, "kimi-alab", "kimi", 4244)
    );
    assert_eq!(row.allows, &[Scope::Relaunch, Scope::Upgrade]);
    assert_eq!(row_for(Program::Kimi).map(|r| r.id), Some("kimi-alab"));
    assert_eq!(row_named("Kimi-Alab").map(|r| r.id), Some("kimi-alab"));
    let (row, pid) = holder("claude-code-alab@4245").expect("rostered");
    assert_eq!(
        (row.id, row.program, row.package, row.vendor, pid),
        (
            "claude-code-alab",
            Program::Claude,
            "claude-code-alab",
            "claude",
            4245
        )
    );
    assert_eq!(row.allows, &Scope::ALL);
    assert_eq!(
        row_for(Program::Claude).map(|r| r.id),
        Some("claude-code-alab")
    );
    assert_eq!(
        row_named("Claude-Code-Alab").map(|r| r.id),
        Some("claude-code-alab")
    );
    // Every row's id reads as its vendor (§8.1 item 5), so presence keeps the vendor's reader.
    for row in super::super::registry::current().rows() {
        assert_eq!(
            aterm_phase::program_of(row.id),
            Some(row.program),
            "{}",
            row.id
        );
    }
    for other in [
        "grok-alab@",
        "grok-alab@1",
        "grok-alab@0",
        "grok-alab@04242",
        "grok-alab@+4242",
        "grok-alab@42x",
        "grok-alab",
        "codex-alab@4242",
        "claude-code-alab@",
        "claude-code-alab@1",
        "claude-code-alab@04242",
        "claude-alab@4242",
        "claude@4242",
        "kimi-alab@",
        "kimi-alab@1",
        "kimi-alab@04242",
        "agy-alab@4242",
        "muse-alab@1",
        "muse@4242",
        "aterm-harness@4242",
        "GROK-ALAB@4242",
    ] {
        assert_eq!(holder(other), None, "{other}");
    }
    for p in [Program::Codex, Program::Agy, Program::Generic] {
        assert_eq!(row_for(p), None, "{p:?}");
    }
}

/// A rostered harness's claim: its scope read in canonical order, its versions kept.
#[test]
fn a_rostered_claim_is_read_with_its_scope_and_versions() {
    let c = scoped(
        "grok-alab@500",
        &[
            "contract=3",
            "scope=upgrade,relaunch",
            "version=0.1.0",
            "vendor=1.0.70",
            "login=work",
        ],
    )
    .expect("a claim");
    assert_eq!(c.pid, 500);
    assert_eq!(c.login.as_deref(), Some("work"));
    assert_eq!(c.scopes.words(), "relaunch,upgrade");
    assert!(c.scopes.contains(Scope::Upgrade) && !c.scopes.contains(Scope::Walls));
    assert_eq!(c.versions(), "grok-alab 0.1.0 (grok 1.0.70)");
    let bare = scoped("grok-alab@500", &["scope=upgrade", "contract=3"]).expect("bare");
    assert_eq!(bare.versions(), "grok-alab unknown (grok unknown)");
}

#[test]
fn reordered_requested_scope_words_are_the_same_claim_under_both_contracts() {
    static ROWS: [Row; 1] = [Row {
        id: "grok-alab",
        program: Program::Grok,
        package: "grok-alab",
        vendor: "grok",
        allows: &[Scope::Relaunch, Scope::Upgrade],
        source: Source::Seed,
    }];
    for contract in ["contract=3", "contract=4"] {
        let parse = |scope: &str| {
            let scope = format!("scope={scope}");
            match read_in(&ROWS, "grok-alab@500", &[contract, &scope, "version=0.3.0"]) {
                Words::Scoped(claim) => claim,
                other => panic!("expected a scoped claim: {other:?}"),
            }
        };
        let first = parse("upgrade,relaunch");
        let reordered = parse("relaunch,upgrade");
        assert_eq!(first, reordered, "{contract}: only word order changed");
        assert!(first.scopes.contains(Scope::Relaunch) && first.scopes.contains(Scope::Upgrade));
        assert_ne!(
            first,
            parse("relaunch"),
            "{contract}: a changed set is new authority"
        );
        assert!(
            matches!(
                read_in(
                    &ROWS,
                    "grok-alab@500",
                    &[contract, "scope=upgrade,relaunch,upgrade", "version=0.3.0"]
                ),
                Words::Refused("a scope word is named twice")
            ),
            "{contract}: canonicalization must not admit duplicate words"
        );
    }
}

#[test]
fn requested_unknown_and_ungranted_words_remain_distinct_from_the_grant() {
    static ROWS: [Row; 1] = [Row {
        id: "grok-alab",
        program: Program::Grok,
        package: "grok-alab",
        vendor: "grok",
        allows: &[Scope::Relaunch, Scope::Upgrade],
        source: Source::Seed,
    }];
    let parse = |scope: &str| {
        let scope = format!("scope={scope}");
        match read_in(
            &ROWS,
            "grok-alab@500",
            &["contract=4", &scope, "version=0.3.0"],
        ) {
            Words::Scoped(claim) => claim,
            other => panic!("expected a scoped claim: {other:?}"),
        }
    };
    let first = parse("walls,future-alpha,relaunch,upgrade");
    assert_eq!(first, parse("upgrade,relaunch,future-alpha,walls"));
    assert_eq!(
        first
            .claimed
            .split(',')
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from(["walls", "future-alpha", "relaunch", "upgrade"])
    );
    assert!(
        !first.scopes.contains(Scope::Walls),
        "the seed row did not grant walls"
    );
    for different in [
        "walls,future-beta,relaunch,upgrade",
        "future-alpha,relaunch,upgrade",
    ] {
        let next = parse(different);
        assert_eq!(
            first.scopes, next.scopes,
            "the effective grant did not change"
        );
        assert_ne!(
            first, next,
            "different requests must survive for later regranting"
        );
    }
    assert!(
        matches!(
            read_in(
                &ROWS,
                "grok-alab@500",
                &[
                    "contract=4",
                    "scope=future-alpha,future-alpha",
                    "version=0.3.0"
                ]
            ),
            Words::Refused("a scope word is named twice")
        ),
        "an unknown word repeated is still refused"
    );
}

/// Every malformed or over-reaching v3 claim is refused whole; a rostered holder never claims
/// under v2; an unrostered holder's v3 claim is an ordinary foreign claim.
#[test]
fn the_scope_parser_refuses_whole_and_admits_only_the_rows_words() {
    let ok = ["contract=3", "scope=relaunch,upgrade"];
    assert!(scoped("grok-alab@500", &ok).is_ok(), "positive control");
    for words in [
        &["scope=relaunch"][..],
        &["contract=3"],
        &["contract=3", "scope="],
        &["contract=2", "scope=relaunch"],
        &["contract=3,2", "scope=relaunch"],
        &["contract=3", "scope=relaunch,walls"],
        &["contract=3", "scope=turn-end"],
        &["contract=3", "scope=relaunch,bogus"],
        &["contract=3", "scope=relaunch,relaunch"],
        &["contract=3", "scope=relaunch,"],
        &["contract=3", "scope=Relaunch"],
        &["contract=3", "contract=3", "scope=relaunch"],
        &["contract=3", "scope=relaunch", "ttl=5000"],
        &["contract=3", "scope=relaunch", "record=/tmp/x"],
        &["contract=3", "scope=relaunch", "loose"],
        &["contract=3", "scope=relaunch", "version=0.1.1"],
        &["contract=3", "scope=relaunch", "version=0.1"],
        &["contract=3", "scope=relaunch", "version=0.1.0-dev.3"],
        &["contract=3", "scope=relaunch", "version=00.1.0"],
        &["contract=3", "scope=relaunch", "version=0.1.0+"],
        &["contract=3", "scope=relaunch", "vendor="],
        &["contract=3", "scope=relaunch", "vendor=1.0.70;rm"],
        &["contract=3", "scope=relaunch", "login=bad/slash"],
        &["contract=3", "scope=relaunch", "login=a", "login=b"],
        &["contract=3", "scope=relaunch,state"],
        &["contract=3", "scope=accounts"],
        &["contract=3", "scope=relaunch", "child=4243:1"],
        &["contract=5", "scope=relaunch"],
        &[],
        &["ttl=5000"],
    ] {
        assert!(refused("grok-alab@500", words), "{words:?}");
    }
    assert_eq!(read("watcher@9", &["ttl=5000"]), Words::V2(&["ttl=5000"]));
    assert_eq!(read("codex-alab@9", &[]), Words::V2(&[]));
    assert_eq!(read("agy-alab@9", &ok), Words::Foreign);
    assert!(scoped("kimi-alab@9", &["contract=3", "scope=relaunch,upgrade"]).is_ok());
    assert!(refused("kimi-alab@9", &["contract=3", "scope=turn-end"]));
    assert!(refused("kimi-alab@9", &["contract=3", "scope=bogus"]));
}

/// claude-code-alab's claim, as its launch makes it and as it widens (§11 D53): `scope=upgrade`,
/// then `relaunch,upgrade`, each with the versions it announces, then the typing duties, one or
/// all of them. Now it is rostered it never claims under v2 (no `contract=`, with or without a
/// lease), and a word that names no duty refuses the claim whole. Its tool's name is no holder:
/// `claude-alab@<pid>` stays an ordinary foreign claim.
#[test]
fn claude_code_alab_claims_under_contract_three_alone_and_any_duty() {
    let me = "claude-code-alab@500";
    let launch = scoped(
        me,
        &[
            "contract=3",
            "scope=upgrade",
            "version=0.2.0+ge8b32af",
            "vendor=2.1.310",
        ],
    )
    .expect("the launch claim");
    assert_eq!((launch.row.program, launch.pid), (Program::Claude, 500));
    assert_eq!(launch.scopes.words(), "upgrade");
    assert!(!launch.scopes.contains(Scope::Relaunch));
    assert_eq!(
        launch.versions(),
        "claude-code-alab 0.2.0+ge8b32af (claude 2.1.310)"
    );
    let widened = scoped(me, &["contract=3", "scope=upgrade,relaunch"]).expect("the widened claim");
    assert_eq!(widened.scopes.words(), "relaunch,upgrade");
    for duty in [
        Scope::TurnEnd,
        Scope::Approvals,
        Scope::Questions,
        Scope::Walls,
    ] {
        let scope = format!("scope=upgrade,{}", duty.word());
        let typing = scoped(me, &["contract=3", &scope]).expect("a typing duty");
        assert!(typing.scopes.contains(duty), "{scope}");
    }
    let every = scoped(
        me,
        &[
            "contract=3",
            "scope=walls,questions,approvals,turn-end,upgrade,relaunch",
            "version=0.3.0",
        ],
    )
    .expect("every duty");
    assert_eq!(
        every.scopes.words(),
        "relaunch,upgrade,turn-end,approvals,questions,walls"
    );
    for words in [
        &[][..],
        &["ttl=5000"],
        &["scope=upgrade"],
        &["contract=2", "scope=upgrade"],
        &["contract=3"],
        &["contract=3", "scope=turn-end,bogus"],
        &["contract=3", "scope=Turn-End"],
        &["contract=3", "scope=turn_end"],
        &["contract=3", "scope=turn-end,turn-end"],
        &["contract=3", "scope=upgrade", "ttl=5000"],
    ] {
        assert!(refused(me, words), "{words:?}");
    }
    assert_eq!(
        read("claude-alab@500", &["contract=3", "scope=upgrade"]),
        Words::Foreign
    );
    assert_eq!(read("claude-alab@500", &[]), Words::V2(&[]));
}

/// The three-part parse (§5): `MAJOR.MINOR.0`, metadata kept for display, a patch refused.
#[test]
fn the_claim_version_is_major_minor_zero() {
    let v = ClaimVersion::parse("0.1.0").expect("0.1.0");
    assert_eq!((v.major, v.minor, v.text.as_str()), (0, 1, "0.1.0"));
    let v = ClaimVersion::parse("12.30.0+g1a2b3c4d5").expect("dev build");
    assert_eq!(
        (v.major, v.minor, v.text.as_str()),
        (12, 30, "12.30.0+g1a2b3c4d5")
    );
    for bad in [
        "0.1.1",
        "0.1",
        "0.1.0.0",
        "v0.1.0",
        "0.01.0",
        "0.1.0-rc.1",
        "0.1.0+",
        "0.1.0+a..b",
        "",
        "a.b.0",
    ] {
        assert_eq!(ClaimVersion::parse(bad), None, "{bad}");
    }
}

fn v4(holder: &str, words: &[&str]) -> Result<Claim, String> {
    scoped(holder, words).and_then(|c| {
        (c.contract == UNIVERSAL)
            .then_some(c)
            .ok_or_else(|| "not contract 4".to_string())
    })
}

/// CONTRACT 4 (`docs/DESIGN-harness-interface-2026-11-12.md` §2.2): `version=` is required, the
/// words this aterm knows are kept and the rest dropped, the row's `allows` is ignored, the seed
/// row's grant is its `allows` and the two words that type nothing, `child=` is
/// `<pid>:<start_us>` in canonical decimals with a pid above 1, and an empty grant stands.
#[test]
fn a_contract_4_claim_keeps_its_known_words_and_is_granted_by_its_rows_source() {
    let me = "kimi-alab@700";
    let c = v4(
        me,
        &[
            "contract=4",
            "scope=walls,relaunch,teleport,state,accounts,upgrade",
            "version=0.3.0",
            "vendor=1.2.0",
            "child=701:1763000000123456",
            "login=work",
        ],
    )
    .expect("a contract 4 claim");
    assert_eq!(c.row.source, Source::Seed, "the index pins no record yet");
    assert_eq!(
        c.scopes.words(),
        "relaunch,upgrade,state,accounts",
        "unknown teleport dropped; walls outside a seed row's grant"
    );
    assert_eq!(
        c.child,
        Some(ChildWord {
            pid: 701,
            start_us: 1_763_000_000_123_456
        })
    );
    assert_eq!(c.login.as_deref(), Some("work"));
    assert_eq!(c.versions(), "kimi-alab 0.3.0 (kimi 1.2.0)");

    let none =
        v4(me, &["contract=4", "scope=walls,teleport", "version=0.3.0"]).expect("granted none");
    assert!(none.scopes.is_empty());
    assert_eq!(none.scopes.words(), "-");
    assert_eq!(none.child, None);

    for words in [
        &["contract=4", "scope=relaunch"][..],
        &["contract=4", "version=0.3.0"],
        &["contract=4", "scope=", "version=0.3.0"],
        &["contract=4", "scope=relaunch,,upgrade", "version=0.3.0"],
        &["contract=4", "scope=relaunch,relaunch", "version=0.3.0"],
        &["contract=4", "scope=teleport,teleport", "version=0.3.0"],
        &["contract=4", "scope=Relaunch", "version=0.3.0"],
        &["contract=4", "scope=-", "version=0.3.0"],
        &["contract=4", "scope=relaunch", "version=0.3.1"],
        &["contract=4", "scope=relaunch", "version=0.3.0", "child=701"],
        &["contract=4", "scope=relaunch", "version=0.3.0", "child=1:5"],
        &[
            "contract=4",
            "scope=relaunch",
            "version=0.3.0",
            "child=0701:5",
        ],
        &[
            "contract=4",
            "scope=relaunch",
            "version=0.3.0",
            "child=701:05",
        ],
        &[
            "contract=4",
            "scope=relaunch",
            "version=0.3.0",
            "child=701:+5",
        ],
        &[
            "contract=4",
            "scope=relaunch",
            "version=0.3.0",
            "child=701:",
        ],
        &["contract=4", "scope=relaunch", "version=0.3.0", "child=a:1"],
        &[
            "contract=4",
            "scope=relaunch",
            "version=0.3.0",
            "child=7:1",
            "child=7:1",
        ],
        &["contract=4", "scope=relaunch", "version=0.3.0", "ttl=5000"],
        &[
            "contract=4",
            "scope=relaunch",
            "version=0.3.0",
            "vendor=1;rm",
        ],
        &["contract=4", "scope=relaunch", "version=0.3.0", "login=Bad"],
        &["contract=4,3", "scope=relaunch", "version=0.3.0"],
        &[
            "contract=4",
            "contract=4",
            "scope=relaunch",
            "version=0.3.0",
        ],
    ] {
        assert!(refused(me, words), "{words:?}");
    }
    let ok = ["contract=4", "scope=relaunch", "version=0.3.0"];
    assert_eq!(read("agy-alab@9", &ok), Words::Unregistered);
    assert_eq!(read("codex-alab@9", &ok), Words::Unregistered);
    assert!(
        refused("agy-alab@9", &["contract=4", "scope=relaunch"]),
        "grammar first"
    );
}

/// The grant by source: a verified record is granted every word it claims, a seed row its
/// `allows` with `state` and `accounts`, a development link every word but `accounts`.
#[test]
fn the_grant_follows_where_the_row_came_from() {
    let row = |source| Row {
        id: "kimi-alab",
        program: Program::Kimi,
        package: "kimi-alab",
        vendor: "kimi",
        allows: &[Scope::Relaunch, Scope::Upgrade],
        source,
    };
    assert_eq!(grant(&row(Source::Signed), &Scope::ALL).words(), {
        Scope::ALL.map(Scope::word).join(",")
    });
    assert_eq!(
        grant(&row(Source::Seed), &Scope::ALL).words(),
        "relaunch,upgrade,state,accounts"
    );
    assert_eq!(
        grant(&row(Source::Linked), &Scope::ALL).words(),
        "relaunch,upgrade,turn-end,approvals,questions,walls,state"
    );
    assert_eq!(grant(&row(Source::Signed), &[]).words(), "-");
    for scope in Scope::ALL {
        assert_eq!(
            scope.since(),
            if matches!(scope, Scope::State | Scope::Accounts) {
                UNIVERSAL
            } else {
                SCOPED
            }
        );
    }
}

/// CONTRACT 3 IS THE SEED IDS' (`docs/HARNESS-CONTRACT.md` §2, until HX-14b deletes it): a row
/// whose id is a seed id is served a contract 3 claim whatever its source (a seed, a development
/// link over one, a verified record over one), so a harness that speaks only contract 3
/// (claude-code-alab) keeps its claim once it is dev-linked or its build carries a record; a row
/// whose id is no seed id is refused contract 3 (`ERR usage`) and claims contract 4, granted by
/// its source. A seed id with no row (its record failed, or its row was refused) is refused
/// unregistered (`ERR unproven`, never stored) under contract 3 and without `contract=`, as under
/// contract 4. NEGATIVE CONTROLS: before HX-05's review fix a linked or signed row over a seed was
/// refused its contract 3 claim, and a seed id whose row was gone was read as a foreign claim (v3)
/// or a v2 claim, either stored to take every duty with nothing proven; a holder no row or seed
/// names stays foreign, a seed id's holder off the pid grammar is no holder, and codex-alab's
/// contract 2 claim stays v2.
#[test]
fn contract_3_is_served_for_seed_ids_whatever_their_rows_source() {
    const RELAUNCH_UPGRADE: &[Scope] = &[Scope::Relaunch, Scope::Upgrade];
    static ROWS: [Row; 5] = [
        Row {
            id: "kimi-alab",
            program: Program::Kimi,
            package: "kimi-alab",
            vendor: "kimi",
            allows: RELAUNCH_UPGRADE,
            source: Source::Seed,
        },
        Row {
            id: "grok-alab",
            program: Program::Grok,
            package: "grok-alab",
            vendor: "grok",
            allows: RELAUNCH_UPGRADE,
            source: Source::Signed,
        },
        Row {
            id: "claude-code-alab",
            program: Program::Claude,
            package: "claude-code-alab",
            vendor: "claude",
            allows: &Scope::ALL,
            source: Source::Linked,
        },
        Row {
            id: "agy-alab",
            program: Program::Agy,
            package: "agy-alab",
            vendor: "agy",
            allows: RELAUNCH_UPGRADE,
            source: Source::Signed,
        },
        Row {
            id: "other-alab",
            program: Program::Kimi,
            package: "other-alab",
            vendor: "kimi",
            allows: RELAUNCH_UPGRADE,
            source: Source::Linked,
        },
    ];
    let v3 = ["contract=3", "scope=relaunch,upgrade", "version=0.2.0"];
    let v4 = [
        "contract=4",
        "scope=relaunch,upgrade,accounts",
        "version=0.2.0",
    ];
    for id in ["kimi-alab", "grok-alab", "claude-code-alab"] {
        let holder = format!("{id}@4242");
        let Words::Scoped(claim) = read_in(&ROWS, &holder, &v3) else {
            panic!("{id}: a seed id's contract 3 claim is read, whatever its row's source");
        };
        assert_eq!(
            (claim.row.id, claim.contract, claim.scopes.words().as_str()),
            (id, SCOPED, "relaunch,upgrade")
        );
        assert!(
            matches!(read_in(&ROWS, &holder, &[]), Words::Refused(_)),
            "{id}: a rostered holder never claims under v2"
        );
    }
    let typing = ["contract=3", "scope=turn-end,walls"];
    assert!(
        matches!(
            read_in(&ROWS, "claude-code-alab@4242", &typing),
            Words::Scoped(_)
        ),
        "a linked claude-code-alab keeps every duty its row allows"
    );
    assert!(
        matches!(read_in(&ROWS, "grok-alab@4242", &typing), Words::Refused(_)),
        "a signed grok-alab's contract 3 claim is still held to its row's allows"
    );
    for id in ["agy-alab", "other-alab"] {
        let holder = format!("{id}@4242");
        assert!(
            matches!(read_in(&ROWS, &holder, &v3), Words::Refused(_)),
            "{id}: no seed id, no contract 3"
        );
        let Words::Scoped(claim) = read_in(&ROWS, &holder, &v4) else {
            panic!("{id}: contract 4 is read");
        };
        assert_eq!(claim.contract, UNIVERSAL);
        assert_eq!(
            claim.scopes.words(),
            if id == "agy-alab" {
                "relaunch,upgrade,accounts"
            } else {
                "relaunch,upgrade"
            },
            "{id}: granted by its source"
        );
    }
    // muse-alab is a seed id with no row here: refused, never foreign, never v2.
    for words in [&v3[..], &[], &["ttl=5000"], &v4] {
        assert_eq!(
            read_in(&ROWS, "muse-alab@4242", words),
            Words::Unregistered,
            "{words:?}"
        );
    }
    assert!(
        matches!(
            read_in(&ROWS, "muse-alab@4242", &["contract=3", "scope=bogus"]),
            Words::Refused(_)
        ),
        "the grammar first"
    );
    assert_eq!(read_in(&ROWS, "muse-alab@04242", &v3), Words::Foreign);
    assert_eq!(read_in(&ROWS, "muse-alab@04242", &[]), Words::V2(&[]));
    assert_eq!(read_in(&ROWS, "watcher-alab@4242", &v3), Words::Foreign);
    assert_eq!(read_in(&ROWS, "codex-alab@4242", &[]), Words::V2(&[]));
    assert_eq!(read_in(&ROWS, "codex-alab@4242", &v3), Words::Foreign);
}
