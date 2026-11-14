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

/// THE HANDSHAKE: aterm serves codex-alab's contract 2 and the roster's contract 3, as a set.
#[test]
fn the_served_contracts_are_two_and_three() {
    assert_eq!(served_word(), "2,3");
    assert!(SERVED.contains(&SCOPED));
}

/// grok-alab, muse-alab, kimi-alab and claude-code-alab may relaunch and upgrade. Nothing else is
/// a rostered holder, codex-alab included (its v2 rule stays its own), and neither is
/// claude-code-alab's tool name or its vendor's.
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
    assert_eq!(row.allows, &[Scope::Relaunch, Scope::Upgrade]);
    assert_eq!(
        row_for(Program::Claude).map(|r| r.id),
        Some("claude-code-alab")
    );
    assert_eq!(
        row_named("Claude-Code-Alab").map(|r| r.id),
        Some("claude-code-alab")
    );
    // Every row's id reads as its vendor (§8.1 item 5), so presence keeps the vendor's reader.
    for row in ROSTER {
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
/// then `relaunch,upgrade`, each with the versions it announces. Now it is rostered it never
/// claims under v2 (no `contract=`, with or without a lease), and a duty outside its row
/// (`turn-end` and the rest stay the built-in engine's) refuses the claim whole. Its tool's name
/// is no holder: `claude-alab@<pid>` stays an ordinary foreign claim.
#[test]
fn claude_code_alab_claims_under_contract_three_alone_and_only_relaunch_and_upgrade() {
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
    for words in [
        &[][..],
        &["ttl=5000"],
        &["scope=upgrade"],
        &["contract=2", "scope=upgrade"],
        &["contract=3"],
        &["contract=3", "scope=upgrade,turn-end"],
        &["contract=3", "scope=approvals"],
        &["contract=3", "scope=questions"],
        &["contract=3", "scope=walls"],
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
