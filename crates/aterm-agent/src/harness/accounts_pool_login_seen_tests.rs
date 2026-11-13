// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A pooled tab's own sign-in wall and served turn reach its row ([`session_seen`]).

use super::*;

/// A session of `cli` whose process runs on `row`'s own login, standing at `point`.
fn seen_on(
    world: &World,
    cli: Cli,
    row: &accounts::Row,
    point: Point,
    seen: LoginSeen,
) -> Result<bool, String> {
    let env = login_env(cli, row, &world.home).expect("the row's login environment");
    let value = env.value.unwrap_or_default();
    let pairs = [(env.var, value.as_str())];
    let pairs: &[(&str, &str)] = if value.is_empty() { &[] } else { &pairs };
    session_seen(
        &world.seat(),
        &Session {
            cli,
            sid: "s-one",
            supervisor: Supervisor::Own,
            env: pairs,
            seen: &crate::harness::observe::Observation::Unread,
            sample: None,
            point,
            mode: Mode::Off,
            relaunch: (),
        },
        seen,
    )
}

fn row_of(world: &World, cli: Cli, label: &str) -> accounts::Row {
    world
        .seat()
        .bound_roster(cli)
        .expect("roster")
        .row(label)
        .expect("row")
        .clone()
}

/// The row's word as a pick reads it: Codex's under its current credential context.
fn word_of(world: &World, cli: Cli, row: &accounts::Row) -> accounts::Auth {
    let store = world.seat().store(cli);
    let mut state = world.load(cli);
    let roster = world.seat().bound_roster(cli).expect("roster");
    store.hydrate_auth_contexts(&mut state, &roster);
    state.account(&row.label).current_auth(row)
}

fn kinds(world: &World, cli: Cli) -> Vec<(accounts::Kind, String, String)> {
    world
        .seat()
        .store(cli)
        .tail(64)
        .expect("ledger")
        .into_iter()
        .map(|event| (event.kind, event.account, event.why))
        .collect()
}

/// Each pooled CLI's tab that meets its sign-in wall signs its own row out once, in the store's
/// transaction: the word, the ledger's `auth signed-out` row, and a row rotation no longer
/// offers. A served turn there signs it back once. Its supervisor is its own and its pool is
/// off, on purpose: the fact is the account's whatever would be decided about it.
/// CONTROL: the other row is never touched, and a repeat of either observation writes nothing.
#[test]
fn a_pooled_tabs_sign_in_wall_signs_its_row_out_and_its_served_turn_signs_it_back() {
    for cli in [Cli::Grok, Cli::Kimi, Cli::Codex] {
        // A Kimi row's login environment is an overlay home, which aterm makes only where it
        // makes links.
        if !crate::harness::accounts_overlay::links_here(cli) {
            continue;
        }
        let world = World::new("login-seen");
        let work = world.two(cli);
        if cli == Cli::Codex {
            usage_policy::write_context_login(&work, 1);
        }
        world.on_main(cli, &[]);
        let row = row_of(&world, cli, "work");
        let before = world.load(cli);
        assert_ne!(
            word_of(&world, cli, &row),
            accounts::Auth::SignedOut,
            "{cli:?} CONTROL: the row starts on record as usable"
        );
        assert_ne!(
            condition_of(&world, cli, "work"),
            Condition::SignedOut,
            "{cli:?}"
        );

        assert_eq!(
            seen_on(&world, cli, &row, Point::LoginEnded, LoginSeen::Lost),
            Ok(true),
            "{cli:?}: the wall is written"
        );
        let lost = world.load(cli);
        assert_eq!(
            word_of(&world, cli, &row),
            accounts::Auth::SignedOut,
            "{cli:?}"
        );
        assert_eq!(
            condition_of(&world, cli, "work"),
            Condition::SignedOut,
            "{cli:?}: rotation is offered the row no more"
        );
        assert_eq!(
            lost.account("main"),
            before.account("main"),
            "{cli:?} CONTROL: the other row is untouched"
        );
        let ledger = kinds(&world, cli);
        assert_eq!(
            ledger
                .iter()
                .filter(|(kind, account, why)| *kind == accounts::Kind::Auth
                    && account == "work"
                    && why == "signed-out")
                .count(),
            1,
            "{cli:?}: one signed-out row: {ledger:?}"
        );
        assert_eq!(
            seen_on(&world, cli, &row, Point::LoginEnded, LoginSeen::Lost),
            Ok(false),
            "{cli:?} CONTROL: the same wall again writes nothing"
        );
        assert_eq!(kinds(&world, cli), ledger, "{cli:?}");

        assert_eq!(
            seen_on(&world, cli, &row, Point::TurnEnd, LoginSeen::Served),
            Ok(true),
            "{cli:?}: the served turn is written"
        );
        assert_eq!(word_of(&world, cli, &row), accounts::Auth::Ok, "{cli:?}");
        assert_ne!(
            condition_of(&world, cli, "work"),
            Condition::SignedOut,
            "{cli:?}: the row is offered again"
        );
        let back = kinds(&world, cli);
        assert_eq!(
            back.iter()
                .filter(|(kind, account, _)| *kind == accounts::Kind::SignedIn && account == "work")
                .count(),
            1,
            "{cli:?}: one signed-in row: {back:?}"
        );
        assert_eq!(
            seen_on(&world, cli, &row, Point::TurnEnd, LoginSeen::Served),
            Ok(false),
            "{cli:?} CONTROL: a served turn on a row that works writes nothing"
        );
        assert_eq!(kinds(&world, cli), back, "{cli:?}");
    }
}

/// A sign-in wall the session advice is asked about ([`session_plan`]) is written by the same
/// writer before any advice is read from the store.
/// CONTROL: the same session at a plain turn end leaves the row's word alone.
// Its row is Kimi's, whose login environment is an overlay home, which aterm makes only on
// Unix (`accounts_overlay::links_here`).
#[cfg(unix)]
#[test]
fn the_advice_path_writes_the_same_wall() {
    let world = World::new("login-seen-advice");
    world.two(Cli::Kimi);
    world.on_main(Cli::Kimi, &[]);
    let row = row_of(&world, Cli::Kimi, "work");
    let env = login_env(Cli::Kimi, &row, &world.home).expect("env");
    let value = env.value.expect("the row's own home");
    let pairs = [(env.var, value.as_str())];
    let exe = world.exe.as_path();
    let mut s = session(Cli::Kimi, Supervisor::Aterm, Mode::Early, exe);
    s.env = &pairs;
    s.sample = None;
    let _ = session_plan(&world.seat(), &s, |_| {});
    assert_ne!(
        world.load(Cli::Kimi).account("work").auth,
        accounts::Auth::SignedOut,
        "CONTROL: a turn end is no wall"
    );
    s.point = Point::LoginEnded;
    let _ = session_plan(&world.seat(), &s, |_| {});
    assert_eq!(
        world.load(Cli::Kimi).account("work").auth,
        accounts::Auth::SignedOut
    );
}

/// What a process on no row's own login shows is nobody's fact: a login directory the roster
/// does not list, and a row another supervisor holds, write nothing.
#[test]
fn a_session_on_no_rows_own_login_writes_nothing() {
    let world = World::new("login-seen-unlisted");
    world.two(Cli::Grok);
    world.on_main(Cli::Grok, &[]);
    let row = row_of(&world, Cli::Grok, "work");
    let elsewhere = accounts::Row {
        dir: Some(world.root.join("logins/grok/other").display().to_string()),
        ..row.clone()
    };
    std::fs::create_dir_all(world.root.join("logins/grok/other")).expect("dir");
    std::fs::write(world.root.join("logins/grok/other/auth.json"), "{}").expect("login");
    let before = std::fs::read(world.seat().store(Cli::Grok).ledger_path()).ok();
    assert_eq!(
        seen_on(
            &world,
            Cli::Grok,
            &elsewhere,
            Point::LoginEnded,
            LoginSeen::Lost
        ),
        Ok(false)
    );
    assert_eq!(
        std::fs::read(world.seat().store(Cli::Grok).ledger_path()).ok(),
        before
    );
    assert_ne!(
        world.load(Cli::Grok).account("work").auth,
        accounts::Auth::SignedOut
    );
    // CONTROL: the listed row's own login, by the same call, is written.
    assert_eq!(
        seen_on(&world, Cli::Grok, &row, Point::LoginEnded, LoginSeen::Lost),
        Ok(true)
    );
}
