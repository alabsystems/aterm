// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pool's plan attributes a session to the row its login file names
//! (`docs/DESIGN-account-discovery-2026-11-09.md`, step 2), not the row its environment does.

use super::*;
use crate::harness::journal::ObservedSrc;
use crate::harness::observe::{Observation, Observed};

const MAIN: &str = "main.pool@proton.example";
const WORK: &str = "work.pool@proton.example";
const ELSE: &str = "someone.else@proton.example";

/// `cli`'s roster of two rows that name their accounts: `main`, the default login, and `work`.
fn named(world: &World, cli: Cli) -> PathBuf {
    let work = world.work_dir(cli);
    let text = format!(
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"{MAIN}\"\n\n\
         [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = {}\nemail = \"{WORK}\"\n",
        toml(&work)
    );
    std::fs::write(world.cfg.join(cli.accounts().roster_file), text).expect("roster");
    work
}

fn seen(cli: Cli, email: &str) -> Observation {
    Observation::Seen(Observed {
        cli: cli.into(),
        email: email.to_owned(),
        key: None,
        region: None,
        src: ObservedSrc::CliLog,
        path: "/logins/somewhere/cli.log".into(),
        mtime: Some(1),
    })
}

fn off(world: &World) {
    aterm_uds::atomic_file::write(
        &(world.cfg.join("aterm.toml")),
        ("[harness]\naccounts = 'off'\n").as_ref(),
        0o600,
    )
    .unwrap();
}

/// THE ATTRIBUTION BUG: an agy tab launched on the default login whose CLI log now names the
/// account row `work` lists. Its session and its usage are `work`'s; nothing is filed under
/// `main`, the row its environment names. Where no row lists the observed account, nothing is
/// filed under any row.
/// NEGATIVE CONTROL: with `session_plan_with` reading `who.me(&roster)`, the sample is the
/// believed row's: `` left: Low right: Unseen `` (and the session's row names `main`).
#[test]
fn a_session_and_its_usage_are_the_observed_rows() {
    let cli = Cli::Agy;
    let plan = |observed: &Observation| {
        let world = World::new("reconciled");
        named(&world, cli);
        world.on_main(cli, &[]);
        off(&world);
        let mut s = session(cli, Supervisor::Aterm, Mode::Off, &world.exe);
        s.seen = observed;
        let _ = session_move(&world.seat(), &s);
        let on = world
            .load(cli)
            .sessions
            .get("s-one")
            .map(|row| row.account.clone());
        (
            condition_of(&world, cli, "main"),
            condition_of(&world, cli, "work"),
            on,
        )
    };
    assert_eq!(
        plan(&Observation::Unread),
        (Condition::Low, Condition::Unseen, Some("main".to_owned())),
        "CONTROL: unread, the belief stands"
    );
    assert_eq!(
        plan(&seen(cli, MAIN)),
        (Condition::Low, Condition::Unseen, Some("main".to_owned())),
        "CONTROL: the file agrees"
    );
    assert_eq!(
        plan(&seen(cli, WORK)),
        (Condition::Unseen, Condition::Low, Some("work".to_owned())),
        "the file names the other row"
    );
    assert_eq!(
        plan(&seen(cli, ELSE)),
        (Condition::Unseen, Condition::Unseen, None),
        "no row lists the account"
    );
}

/// The scoped Kimi plan (`session_login_plan`) files its session under the observed row as the
/// shell plan does: a Kimi tab launched on the default login whose credential now names the
/// account `work` lists is `work`'s session, and no row's where no row lists the account.
/// The `Unread` case is what the scoped plan's caller passed before it read the login file:
/// the session is then the believed row's, whatever the file names.
#[test]
fn a_scoped_kimi_session_is_the_observed_rows() {
    let cli = Cli::Kimi;
    let on = |observed: &Observation| {
        let world = World::new("reconciled-kimi");
        let text = format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"{MAIN}\"\n\
             region = \"mainland-cn\"\n\n\
             [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = {}\n\
             email = \"{WORK}\"\nregion = \"mainland-cn\"\n",
            toml(&world.work_dir(cli))
        );
        std::fs::write(world.cfg.join(cli.accounts().roster_file), text).expect("roster");
        world.on_main(cli, &[]);
        off(&world);
        let mut s = session(cli, Supervisor::Aterm, Mode::Off, &world.exe);
        s.sample = None;
        s.seen = observed;
        let _ = session_login_plan(&world.seat(), &s, |note| note.record());
        world
            .load(cli)
            .sessions
            .get("s-one")
            .map(|row| row.account.clone())
    };
    assert_eq!(
        on(&Observation::Unread),
        Some("main".to_owned()),
        "CONTROL: unread, the belief stands"
    );
    let kimi = |email: &str| {
        let Observation::Seen(mut observed) = seen(cli, email) else {
            unreachable!()
        };
        observed.region = Some(crate::ai::connector::Region::MainlandCn);
        Observation::Seen(observed)
    };
    assert_eq!(on(&kimi(MAIN)), Some("main".to_owned()));
    assert_eq!(on(&kimi(WORK)), Some("work".to_owned()));
    assert_eq!(on(&kimi(ELSE)), None, "no row lists the account");
}

/// A Codex tab launched on `work`'s login directory whose `auth.json` names `main`'s account
/// is low on `main`. The pool does not move it onto `work`: that row's directory holds
/// `main`'s login, so a line onto it would leave the tab on the account it is leaving.
/// NEGATIVE CONTROL: with the displaced row a target, the plan is a move onto it:
/// `` moved onto the row its own directory displaced: Move { to: "work", .. } ``.
#[test]
fn a_move_never_targets_the_row_whose_directory_holds_the_observed_login() {
    let cli = Cli::Codex;
    let world = World::new("reconciled-move");
    named(&world, cli);
    world.on_main(cli, &[]);
    let agreed = session_move(
        &world.seat(),
        &session(cli, Supervisor::Aterm, Mode::Early, &world.exe),
    );
    assert!(
        matches!(&agreed.advice, Advice::Move { to, .. } if to == "work") && agreed.line.is_some(),
        "CONTROL: on the default login, a low `main` moves to `work`: {agreed:?}"
    );

    let world = World::new("reconciled-move-displaced");
    let work = named(&world, cli);
    world.on_main(cli, &[]);
    let auth = work.join("auth.json");
    let auth = auth.to_string_lossy();
    let env = [("CODEX_AUTH_PATH", &*auth)];
    let observed = seen(cli, MAIN);
    let mut s = session(cli, Supervisor::Aterm, Mode::Early, &world.exe);
    s.env = &env;
    s.seen = &observed;
    let moved = session_move(&world.seat(), &s);
    assert_eq!(
        condition_of(&world, cli, "main"),
        Condition::Low,
        "the sample is the observed row's"
    );
    assert_eq!(condition_of(&world, cli, "work"), Condition::Unseen);
    assert!(
        !matches!(&moved.advice, Advice::Move { to, .. } if to == "work"),
        "moved onto the row its own directory displaced: {:?}",
        moved.advice
    );
    assert_eq!(moved.line, None);
    assert!(world.load(cli).rotations.is_empty());
}
