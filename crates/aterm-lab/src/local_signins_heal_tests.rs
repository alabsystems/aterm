// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_the_stand_in_claudes_runs_are_counted() {
    assert_eq!(claude_runs(""), 0);
    assert_eq!(
        claude_runs("codex --version\nclaude auth status\nagy -p /usage\nclaude --version\n"),
        2
    );
    assert_eq!(claude_runs("claudex auth\n claude auth\n"), 0);
}

#[test]
fn only_probe_records_are_counted() {
    let spool = "{\"k\":\"probe\",\"c\":\"agy\",\"why\":\"asked\"}\n\
                 {\"k\":\"usage\",\"c\":\"claude\"}\n\
                 not json\n\
                 {\"k\":\"probe\",\"c\":\"claude\",\"why\":\"launching\"}\n";
    assert_eq!(probe_records(spool), 2);
    assert_eq!(probe_records(""), 0);
}

#[test]
fn the_profile_is_signed_in_as_the_address_and_keeps_its_other_fields() {
    let laid =
        signed_in_profile("{\"hasCompletedOnboarding\":true}", "ac-def@example.test").unwrap();
    let value: aterm_json::Value = aterm_json::from_str(&laid).unwrap();
    assert_eq!(
        value["oauthAccount"]["emailAddress"].as_str(),
        Some("ac-def@example.test")
    );
    assert_eq!(value["hasCompletedOnboarding"].as_bool(), Some(true));
    assert!(signed_in_profile("[]", "x@example.test").is_err());
}

/// The stored login [`lay`] writes is one the agent's own reader holds as signed in, and an
/// emptied one as signed out, so the world's two answers are the reader's.
#[test]
fn the_stored_login_reads_as_held_by_the_agents_own_reader() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let claude = Cli::Claude.connector();
    let read = |text: &str| {
        std::fs::write(dir.path().join(".credentials.json"), text).unwrap();
        aterm_agent::ai::connector::stored_login_held(claude, dir.path())
    };
    assert_eq!(read(&stored_login("ac-def")), Some(true));
    assert_eq!(
        read("{\"claudeAiOauth\":{\"accessToken\":\"\",\"refreshToken\":\"\"}}"),
        Some(false),
        "CONTROL: a login holding no token is not held"
    );
}

/// A world laid as the lane lays it (one dedicated Claude row already on its roster) and then
/// [`lay`]: the heal is due, and once it runs (the agent's own heal with its live check, the one
/// the window's upkeep makes) a stored login heals the row where the default login is that file,
/// while a keychain-held one, and on macOS a stored file too, leaves its sign-out standing;
/// none runs anything or notes a probe, and the judge passes each. Both platforms run on any OS
/// (`test_on_login_platform`). NEGATIVE CONTROLS: before the heal the judge fails the world the
/// heal heals, and a stand-in run or a probe record in the world fails a judge that otherwise
/// passes.
#[test]
fn a_heal_due_world_heals_from_its_stored_login_and_runs_nothing() {
    for (in_file, held) in [
        (true, Held::File),
        (true, Held::Keychain),
        (false, Held::File),
        (false, Held::Keychain),
    ] {
        aterm_agent::ai::connector::test_on_login_platform(in_file, || {
            heal_due_world(held, in_file);
        });
    }
}

fn heal_due_world(held: Held, in_file: bool) {
    let world = World::new(crate::instance::CONFIG_OFF).unwrap();
    let now = crate::util::now_secs();
    put(
        &roster_path(&world),
        "[[account]]\nlabel = \"ac-in\"\nclass = \"dedicated\"\ndir = \"/nonexistent/ac-in\"\n\n",
    )
    .unwrap();
    let label = lay(&world, 'a', held, now).unwrap();
    assert_eq!(label, "ac-def");
    assert_eq!(
        aterm_agent::harness::accounts_sync::default_login_heal_due_for_lab(
            &world.home,
            &harness_state(&world),
            &roster_path(&world),
            now,
        ),
        Some(label.clone()),
        "{held:?}: the laid world is due its heal"
    );
    let before = judge(&world, &label, now, 0).unwrap();
    assert!(
        before.due && before.auth == Auth::SignedOut,
        "{held:?}: {before:?}"
    );
    assert_eq!(
        heals(held),
        held == Held::File && in_file,
        "{held:?} {in_file}"
    );
    if heals(held) {
        assert!(
            !before.pass(held),
            "CONTROL: the unhealed world fails the judge: {before:?}"
        );
    }

    let (healed, probes) = aterm_agent::harness::provider_read::captured(|| {
        aterm_agent::harness::accounts_sync::heal_default_login_for_lab(
            &world.home,
            &harness_state(&world),
            &roster_path(&world),
            now + 1,
        )
    });
    let (healed, line) = healed.expect("the heal was due");
    assert_eq!(healed, heals(held), "{held:?} {in_file}: {line}");
    assert!(probes.is_empty(), "{held:?}: {probes:?}");
    let after = judge(&world, &label, now + 1, 0).unwrap();
    assert!(after.pass(held), "{held:?}: {}", after.words());

    put(&world.dir.join(RAN), "claude auth status --json\n").unwrap();
    assert!(
        !judge(&world, &label, now + 1, 0).unwrap().pass(held),
        "CONTROL: a stand-in run fails the judge"
    );
    std::fs::remove_file(world.dir.join(RAN)).unwrap();
    put(
        &world.state().join("alab/spool.jsonl"),
        "{\"k\":\"probe\",\"c\":\"claude\",\"why\":\"launching\"}\n",
    )
    .unwrap();
    assert!(
        !judge(&world, &label, now + 1, 0).unwrap().pass(held),
        "CONTROL: a probe record fails the judge"
    );
}
