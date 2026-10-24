// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the pooled CLIs' launch pick and session moves.

use std::path::{Path, PathBuf};

use super::*;
use crate::harness::accounts::{Condition, State};
use crate::harness::upgrade::Dialect;

const NOW: i64 = 1_790_000_000;
const MIN: i64 = 60;
const HOUR: i64 = 3600;

struct World {
    _dir: aterm_tempfile::TempDir,
    root: PathBuf,
    cfg: PathBuf,
    state: PathBuf,
    home: PathBuf,
}

impl World {
    fn new(tag: &str) -> World {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-accounts-pool-{tag}-"))
            .tempdir()
            .expect("temp dir");
        let root = dir.path().canonicalize().expect("canonical temp dir");
        for sub in ["cfg", "state", "home"] {
            std::fs::create_dir_all(root.join(sub)).expect("world dir");
        }
        World {
            _dir: dir,
            cfg: root.join("cfg"),
            state: root.join("state"),
            home: root.join("home"),
            root,
        }
    }

    fn seat(&self) -> Seat<'_> {
        Seat {
            config_dir: &self.cfg,
            state: &self.state,
            home: &self.home,
            now: NOW,
        }
    }

    /// The login directory of `cli`'s row `work`, holding the file its CLI keeps a login in.
    fn work_dir(&self, cli: Cli) -> PathBuf {
        let dir = self.root.join("logins").join(cli.id()).join("work");
        let file = cli
            .connector()
            .stores
            .first()
            .copied()
            .unwrap_or("auth.json");
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("login dir");
        std::fs::write(&path, "{}").expect("stand-in login file");
        dir
    }

    /// `cli`'s roster of two: `main`, the machine's default login, and `work`.
    fn two(&self, cli: Cli) -> PathBuf {
        let work = self.work_dir(cli);
        let text = format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n[[account]]\n\
             label = \"work\"\nclass = \"dedicated\"\ndir = \"{}\"\n",
            work.display()
        );
        std::fs::write(self.cfg.join(cli.accounts().roster_file), text).expect("roster");
        work
    }

    /// `cli`'s store with `main` active and proven, and `rotations` committed at their instants.
    fn on_main(&self, cli: Cli, rotations: &[i64]) {
        let store = self.seat().store(cli);
        let mut note = None;
        store
            .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |state| {
                state.active = Some("main".to_string());
                state.proven = true;
                state.rotations = rotations.to_vec();
                Ok(Vec::new())
            })
            .expect("seed the store");
    }

    fn load(&self, cli: Cli) -> State {
        self.seat().store(cli).load(NOW).expect("store").0
    }
}

/// The share of a five-hour window a session near its wall has used.
fn near_wall() -> Sample {
    Sample {
        session_pct: Some(95),
        session_reset: Some(NOW + 3 * HOUR),
        week_pct: Some(20),
        week_reset: Some(NOW + 3 * 24 * HOUR),
        at: NOW - MIN,
        by: String::new(),
        ..Sample::default()
    }
}

const NO_FLAGS: &[String] = &[];

/// A [`ReadUsage`] that reads nothing.
fn no_usage(_: Cli, _: &Path, _: i64) -> Option<Usage> {
    None
}

/// A wall on `cli`'s `label` until `until`.
fn wall(world: &World, cli: Cli, label: &str, until: i64) {
    let mut note = None;
    world
        .seat()
        .store(cli)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |state| {
            Ok(accounts::note_wall(
                state,
                label,
                "five_hour",
                Some(until),
                NOW - MIN,
                NOW,
            ))
        })
        .expect("a wall");
}

fn session<'a>(cli: Cli, supervisor: Supervisor, mode: Mode, exe: &'a Path) -> Session<'a> {
    Session {
        cli,
        sid: "s-one",
        supervisor,
        env: &[],
        sample: Some(near_wall()),
        point: Point::TurnEnd,
        mode,
        relaunch: PoolRelaunch {
            dialect: Dialect::Zsh,
            cd: None,
            exe,
            flags: NO_FLAGS,
            resume: Some("019a-rollout-id"),
        },
    }
}

fn condition_of(world: &World, cli: Cli, label: &str) -> Condition {
    let roster = world.seat().roster(cli).expect("roster");
    let row = roster.row(label).expect("row");
    accounts::condition(row, &world.load(cli), true, NOW)
}

#[test]
fn a_low_codex_session_moves_onto_its_overlay_home_and_resumes_its_rollout() {
    let world = World::new("codex-move");
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let exe = Path::new("/opt/codex-old/bin/codex");
    let moved = session_move(
        &world.seat(),
        &session(Cli::Codex, Supervisor::Aterm, Mode::Early, exe),
    );
    assert_eq!(
        moved.advice,
        Advice::Move {
            to: "work".to_string(),
            ahead: true,
            rotates: true,
            takes_canary: false,
        }
    );
    let line = moved.line.expect("a move writes its line");
    assert!(
        line.contains(&format!(" CODEX_HOME={} ", work.display())),
        "{line}"
    );
    assert!(
        line.contains("'/opt/codex-old/bin/codex' 'resume' '019a-rollout-id'"),
        "{line}"
    );
    assert!(!line.contains("CLAUDE"), "{line}");
    assert!(
        work.join("sessions").is_symlink(),
        "the overlay home shares the default home's rollouts"
    );
    let state = world.load(Cli::Codex);
    assert_eq!(state.active.as_deref(), Some("work"));
    assert_eq!(
        state.rotations,
        vec![NOW],
        "the rotation is the codex store's"
    );

    // Negative: a self-claimed codex-alab answers its own walls; nothing is read or written.
    let other = World::new("codex-self");
    other.two(Cli::Codex);
    let moved = session_move(
        &other.seat(),
        &session(Cli::Codex, Supervisor::Own, Mode::Early, exe),
    );
    assert_eq!(moved.advice, Advice::Stay(StayWhy::OtherSupervisor));
    assert_eq!(StayWhy::OtherSupervisor.as_str(), "other-supervisor");
    assert_eq!(moved.line, None, "no line is written");
    assert!(!other.seat().store(Cli::Codex).state_file().exists());
}

#[test]
fn a_grok_line_names_the_rows_auth_file_and_the_default_rows_line_unsets_it() {
    let world = World::new("grok-line");
    let work = world.two(Cli::Grok);
    let roster = world.seat().roster(Cli::Grok).expect("roster");
    let exe = Path::new("/opt/grok/bin/grok");
    let how = PoolRelaunch {
        dialect: Dialect::Zsh,
        cd: None,
        exe,
        flags: NO_FLAGS,
        resume: Some("abc"),
    };
    let row = roster.row("work").expect("work");
    let env = login_env(Cli::Grok, row, &world.home).expect("grok env");
    assert_eq!(
        (env.var, env.value.clone()),
        (
            "GROK_AUTH_PATH",
            Some(work.join("auth.json").display().to_string())
        )
    );
    let line = relaunch::pool_line(&how, Cli::Grok, &env).expect("line");
    assert!(
        line.contains(&format!(
            " GROK_AUTH_PATH={} ",
            work.join("auth.json").display()
        )),
        "{line}"
    );
    assert!(
        line.ends_with("'/opt/grok/bin/grok' '--resume' 'abc'"),
        "{line}"
    );
    assert!(
        !line.contains("GROK_HOME"),
        "GROK_HOME stays shared: {line}"
    );
    assert!(
        !work.join("sessions").exists(),
        "a login-file seam makes no overlay"
    );

    // Negative: nothing of Claude Code's seam, and the default row unsets the file's variable.
    assert!(!line.contains("CLAUDE_SECURESTORAGE_CONFIG_DIR"), "{line}");
    assert!(!line.contains("CLAUDE_CONFIG_DIR"), "{line}");
    let main =
        login_env(Cli::Grok, roster.row("main").expect("main"), &world.home).expect("default env");
    assert_eq!(main.value, None);
    let line = relaunch::pool_line(&how, Cli::Grok, &main).expect("default line");
    assert!(line.contains("-u 'GROK_AUTH_PATH'"), "{line}");
    assert!(!line.contains("GROK_AUTH_PATH="), "{line}");
}

#[test]
fn each_cli_resumes_by_its_own_flag() {
    assert_eq!(relaunch::resume_argv(Cli::Codex, "i"), ["resume", "i"]);
    assert_eq!(relaunch::resume_argv(Cli::Grok, "i"), ["--resume", "i"]);
    assert_eq!(relaunch::resume_argv(Cli::Kimi, "i"), ["--session", "i"]);
    assert_eq!(relaunch::resume_argv(Cli::Claude, "i"), ["--resume", "i"]);
}

#[test]
fn mode_off_records_every_clis_limits_and_moves_none() {
    let exe = Path::new("/opt/bin/agent");
    for cli in Cli::ALL {
        let world = World::new(&format!("off-{}", cli.id()));
        world.two(cli);
        world.on_main(cli, &[]);
        let moved = session_move(
            &world.seat(),
            &session(cli, Supervisor::Aterm, Mode::Off, exe),
        );
        assert_eq!(moved.advice, Advice::Stay(StayWhy::Off), "{cli:?}");
        assert_eq!(moved.line, None, "{cli:?}");
        assert_eq!(
            condition_of(&world, cli, "main"),
            Condition::Low,
            "{cli:?}: the sample is recorded under off"
        );
        assert!(world.load(cli).rotations.is_empty(), "{cli:?}");
    }

    // Negative: mode early moves a CLI whose walls are measured, and holds one whose walls are
    // not (its advice is a launch's).
    for (cli, moves) in [
        (Cli::Codex, true),
        (Cli::Claude, true),
        (Cli::Grok, false),
        (Cli::Kimi, false),
    ] {
        let world = World::new(&format!("early-{}", cli.id()));
        world.two(cli);
        world.on_main(cli, &[]);
        let moved = session_move(
            &world.seat(),
            &session(cli, Supervisor::Aterm, Mode::Early, exe),
        );
        if moves {
            assert!(
                matches!(&moved.advice, Advice::Move { to, .. } if to == "work"),
                "{cli:?}: {:?}",
                moved.advice
            );
            assert!(moved.line.is_some(), "{cli:?}");
        } else {
            assert_eq!(moved.advice, Advice::Stay(StayWhy::Unmeasured), "{cli:?}");
            assert_eq!(moved.line, None, "{cli:?}");
        }
    }
}

#[test]
fn the_breaker_is_spent_in_one_clis_store_and_no_other() {
    let world = World::new("breaker");
    world.two(Cli::Codex);
    world.two(Cli::Claude);
    let spent: Vec<i64> = (1..=4).map(|i| NOW - i * 5 * MIN).collect();
    world.on_main(Cli::Codex, &spent);
    world.on_main(Cli::Claude, &[]);
    let exe = Path::new("/opt/bin/agent");
    let codex = session_move(
        &world.seat(),
        &session(Cli::Codex, Supervisor::Aterm, Mode::Early, exe),
    );
    assert!(
        matches!(codex.advice, Advice::Stay(StayWhy::Breaker { .. })),
        "2 x a roster of 2 in the hour: {:?}",
        codex.advice
    );
    assert_eq!(codex.line, None);

    // Negative: Claude Code's store, beside it under the same state root, is not spent.
    let claude = session_move(
        &world.seat(),
        &session(Cli::Claude, Supervisor::Aterm, Mode::Early, exe),
    );
    assert!(
        matches!(&claude.advice, Advice::Move { to, .. } if to == "work"),
        "{:?}",
        claude.advice
    );
    assert_eq!(world.load(Cli::Codex).rotations.len(), 4);
    assert_eq!(world.load(Cli::Claude).rotations, vec![NOW]);
}

#[test]
fn a_launch_starts_under_the_pin_or_a_fresh_active_else_next() {
    let world = World::new("launch");
    let work = world.two(Cli::Codex);
    let seat = world.seat();
    world.on_main(Cli::Codex, &[]);
    let pick = launch_env(&seat, Cli::Codex, &no_usage).expect("a pick");
    assert_eq!((pick.label.as_str(), pick.value), ("main", None));
    let mut note = None;
    seat.store(Cli::Codex)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |state| {
            Ok(accounts::note_wall(
                state,
                "main",
                "five_hour",
                Some(NOW + HOUR),
                NOW - MIN,
                NOW,
            ))
        })
        .expect("a wall");
    let pick = launch_env(&seat, Cli::Codex, &no_usage).expect("next");
    assert_eq!(
        (pick.label.as_str(), pick.value),
        ("work", Some(work.display().to_string()))
    );
    assert_eq!(pick.var, "CODEX_HOME");

    // Negative: one row is nothing to pick between, and an unmeasured seam is never launched on.
    let one = World::new("launch-one");
    std::fs::write(
        one.cfg.join("accounts-codex.toml"),
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n",
    )
    .expect("roster");
    one.on_main(Cli::Codex, &[]);
    assert_eq!(launch_env(&one.seat(), Cli::Codex, &no_usage), None);
    let kimi = World::new("launch-kimi");
    kimi.two(Cli::Kimi);
    kimi.on_main(Cli::Kimi, &[]);
    assert_eq!(launch_env(&kimi.seat(), Cli::Kimi, &no_usage), None);
}

/// A codex roster of `main` (the default login) and `work`, whose roster dir is what `accounts
/// add --cli codex` leaves (no login in it), and whose login `accounts renew work --cli codex`
/// signed in to a staging home it bound in `logins.json`. Returns the staging home.
fn renewed_work(world: &World) -> PathBuf {
    let row_dir = world.root.join("logins/codex/work");
    std::fs::create_dir_all(&row_dir).expect("row dir");
    std::fs::write(
        world.cfg.join("accounts-codex.toml"),
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n[[account]]\n\
             label = \"work\"\nclass = \"dedicated\"\ndir = \"{}\"\n",
            row_dir.display()
        ),
    )
    .expect("roster");
    let staging = world.root.join("logins/codex/work+1");
    std::fs::create_dir_all(&staging).expect("staging");
    std::fs::write(staging.join("auth.json"), "{}").expect("stand-in login");
    let store = world.seat().store(Cli::Codex);
    let mut note = None;
    store
        .transact_logins(NOW, &mut note, |state, logins| {
            let ev = accounts::note_renewed(
                state,
                logins,
                "work",
                Some(row_dir.to_str().expect("utf8")),
                staging.to_str().expect("utf8"),
                None,
                NOW - HOUR,
            );
            state.active = Some("main".to_string());
            state.proven = true;
            Ok(ev)
        })
        .expect("renewal bound");
    staging
}

fn pin(world: &World, cli: Cli, label: &str) -> Result<(), String> {
    let seat = world.seat();
    let roster = seat.bound_roster(cli).expect("roster");
    let enrolled = accounts::enrolled_for(cli, &roster);
    let mut note = None;
    seat.store(cli)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |state| {
            accounts::pin(state, &roster, &enrolled, label, NOW).map(|e| vec![e])
        })
        .map(|_| ())
}

#[test]
fn a_launch_and_a_move_read_a_renewed_rows_login_where_logins_json_binds_it() {
    let world = World::new("renewed");
    let staging = renewed_work(&world);
    pin(&world, Cli::Codex, "work").expect("work is signed in here, as `use` reads it");
    let pick = launch_env(&world.seat(), Cli::Codex, &no_usage).map(|p| (p.label, p.value));
    assert_eq!(
        pick,
        Some(("work".to_string(), Some(staging.display().to_string()))),
        "the owner's pin on the renewed row starts the launch under its renewed home"
    );

    let moving = World::new("renewed-move");
    let staging = renewed_work(&moving);
    let exe = Path::new("/opt/codex-old/bin/codex");
    let moved = session_move(
        &moving.seat(),
        &session(Cli::Codex, Supervisor::Aterm, Mode::Early, exe),
    );
    assert!(
        matches!(&moved.advice, Advice::Move { to, .. } if to == "work"),
        "main is low and work is renewed: {:?}",
        moved.advice
    );
    let line = moved.line.unwrap_or_default();
    assert!(
        line.contains(&format!("CODEX_HOME='{}'", staging.display())),
        "the move names the renewed home: {line}"
    );

    // Negative: a row no renewal bound has no login here, so it is neither pinned nor picked.
    let bare = World::new("renewed-none");
    renewed_work(&bare);
    std::fs::remove_file(bare.seat().store(Cli::Codex).dir.join("logins.json"))
        .expect("no bindings");
    assert!(pin(&bare, Cli::Codex, "work").is_err(), "work has no login");
    wall(&bare, Cli::Codex, "main", NOW + HOUR);
    assert_eq!(launch_env(&bare.seat(), Cli::Codex, &no_usage), None);
}

#[test]
fn a_launch_keeps_a_low_active_row_when_every_other_row_is_walled() {
    let world = World::new("launch-low");
    let work = world.two(Cli::Codex);
    let seat = world.seat();
    let mut note = None;
    seat.store(Cli::Codex)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |state| {
            state.active = Some("work".to_string());
            state.proven = true;
            Ok(accounts::note_sample(state, "work", near_wall(), NOW))
        })
        .expect("seed");
    wall(&world, Cli::Codex, "main", NOW + 2 * HOUR);
    assert_eq!(condition_of(&world, Cli::Codex, "work"), Condition::Low);
    let pick = launch_env(&seat, Cli::Codex, &no_usage).map(|p| (p.label, p.value));
    assert_eq!(
        pick,
        Some(("work".to_string(), Some(work.display().to_string()))),
        "the low active row still runs; the default login, main, is walled"
    );

    // Negative: with the active row walled too, no row is usable.
    wall(&world, Cli::Codex, "work", NOW + HOUR);
    assert_eq!(launch_env(&seat, Cli::Codex, &no_usage), None);
}

#[test]
fn accounts_off_places_a_launch_on_the_owners_pin_alone() {
    let world = World::new("launch-off");
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    wall(&world, Cli::Codex, "main", NOW + HOUR);
    std::fs::write(
        world.cfg.join("aterm.toml"),
        "[harness]\naccounts = \"off\"\n",
    )
    .expect("aterm.toml");
    assert_eq!(world.seat().mode(), Mode::Off);
    let read = |_: Cli, _: &Path, _: i64| -> Option<Usage> { panic!("off reads no usage") };
    assert_eq!(
        launch_env(&world.seat(), Cli::Codex, &read),
        None,
        "off leaves the worker on its own login"
    );
    pin(&world, Cli::Codex, "work").expect("pin");
    let pick = launch_env(&world.seat(), Cli::Codex, &read).map(|p| (p.label, p.value));
    assert_eq!(
        pick,
        Some(("work".to_string(), Some(work.display().to_string()))),
        "the owner's own pin holds under off"
    );

    // Negative: under early, the wall moves the launch with no pin.
    let early = World::new("launch-early");
    let work = early.two(Cli::Codex);
    early.on_main(Cli::Codex, &[]);
    wall(&early, Cli::Codex, "main", NOW + HOUR);
    let pick = launch_env(&early.seat(), Cli::Codex, &no_usage).map(|p| (p.label, p.value));
    assert_eq!(
        pick,
        Some(("work".to_string(), Some(work.display().to_string())))
    );
}

#[test]
fn a_keyring_store_holds_the_codex_launch_and_move() {
    for (store, held) in [
        ("keyring", true),
        ("auto", true),
        ("ephemeral", true),
        ("file", false),
    ] {
        let world = World::new(&format!("keyring-{store}"));
        world.two(Cli::Codex);
        world.on_main(Cli::Codex, &[]);
        wall(&world, Cli::Codex, "main", NOW + HOUR);
        let codex = world.home.join(".codex");
        std::fs::create_dir_all(&codex).expect("default home");
        std::fs::write(
            codex.join("config.toml"),
            format!("cli_auth_credentials_store = \"{store}\"\n"),
        )
        .expect("config");
        let pick = launch_env(&world.seat(), Cli::Codex, &no_usage).map(|p| p.label);
        assert_eq!(pick.is_none(), held, "{store}: {pick:?}");
        let exe = Path::new("/opt/codex-old/bin/codex");
        let moved = session_move(
            &world.seat(),
            &session(Cli::Codex, Supervisor::Aterm, Mode::Early, exe),
        );
        assert_eq!(
            moved.advice == Advice::Stay(StayWhy::KeyringStore),
            held,
            "{store}: {:?}",
            moved.advice
        );
    }
    assert_eq!(StayWhy::KeyringStore.as_str(), "keyring-store");
}

#[test]
fn an_overlay_home_with_shared_entries_of_its_own_is_never_started_on() {
    let world = World::new("drift");
    let default_home = world.home.join(".codex");
    std::fs::create_dir_all(&default_home).expect("default home");
    std::fs::write(default_home.join("state_5.sqlite"), "shared").expect("shared db");
    // What `codex app-server` (0.164.0) made in an empty staging home (lab: state_5.sqlite,
    // installation_id, skills/).
    let staging = world.root.join("logins/codex/work+1");
    std::fs::create_dir_all(staging.join("skills/.system")).expect("staging");
    std::fs::write(staging.join("state_5.sqlite"), "own").expect("own db");
    std::fs::write(staging.join("auth.json"), "{}").expect("login");
    let row = accounts::Row {
        label: "work".into(),
        dir: Some(staging.display().to_string()),
        ..accounts::Row::default()
    };
    let refused = login_env(Cli::Codex, &row, &world.home);
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(
        std::fs::read_to_string(staging.join("state_5.sqlite")).expect("left"),
        "own",
        "its own database is left as it is"
    );

    // Negative: a home whose shared entries are all links (as `ensure` makes them) is used.
    let clean = world.root.join("logins/codex/work+2");
    std::fs::create_dir_all(&clean).expect("clean");
    std::fs::write(clean.join("auth.json"), "{}").expect("login");
    let row = accounts::Row {
        dir: Some(clean.display().to_string()),
        ..row
    };
    let env = login_env(Cli::Codex, &row, &world.home).expect("an overlay");
    assert_eq!(env.value, Some(clean.display().to_string()));
    assert!(clean.join("state_5.sqlite").is_symlink());
}

#[test]
fn a_moved_session_that_served_its_turn_proves_its_row_and_can_leave_early_again() {
    let world = World::new("canary");
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let exe = Path::new("/opt/codex-old/bin/codex");
    let first = session_move(
        &world.seat(),
        &session(Cli::Codex, Supervisor::Aterm, Mode::Early, exe),
    );
    assert!(matches!(&first.advice, Advice::Move { to, rotates: true, .. } if to == "work"));
    assert!(
        !world.load(Cli::Codex).proven,
        "the move's new row is the canary"
    );
    let later = NOW + 4 * HOUR;
    let seat = Seat {
        config_dir: &world.cfg,
        state: &world.state,
        home: &world.home,
        now: later,
    };
    let work_text = work.display().to_string();
    let env = [("CODEX_HOME", work_text.as_str())];
    let low_later = Sample {
        session_reset: Some(later + 3 * HOUR),
        week_reset: Some(later + 3 * 24 * HOUR),
        at: later - MIN,
        ..near_wall()
    };

    // Negative: a turn served on another row (main's own login) proves nothing of work's.
    let mut s = session(Cli::Codex, Supervisor::Aterm, Mode::Early, exe);
    s.sample = None;
    let _ = session_move(&seat, &s);
    assert!(!seat.store(Cli::Codex).load(later).expect("store").0.proven);

    let mut s = session(Cli::Codex, Supervisor::Aterm, Mode::Early, exe);
    s.env = &env;
    s.sample = Some(low_later);
    let second = session_move(&seat, &s);
    let state = seat.store(Cli::Codex).load(later).expect("store").0;
    assert!(state.proven || state.active.as_deref() == Some("main"));
    assert!(
        matches!(&second.advice, Advice::Move { to, .. } if to == "main"),
        "the served turn proved work, so its low session leaves early: {:?}",
        second.advice
    );
}

#[test]
fn a_low_session_of_a_cli_with_no_usage_probe_is_never_told_to_probe() {
    let exe = Path::new("/opt/codex-old/bin/codex");
    let world = World::new("probe");
    world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let mut s = session(Cli::Codex, Supervisor::Aterm, Mode::Early, exe);
    s.sample = Some(Sample {
        at: NOW - 11 * MIN,
        ..near_wall()
    });
    let moved = session_move(&world.seat(), &s);
    assert!(
        matches!(&moved.advice, Advice::Move { to, .. } if to == "work"),
        "codex has no /usage probe; got {:?}",
        moved.advice
    );

    // Negative: the same reading of a Claude Code session is due a `/usage` probe.
    let claude = World::new("probe-claude");
    claude.two(Cli::Claude);
    claude.on_main(Cli::Claude, &[]);
    let roster = claude.seat().bound_roster(Cli::Claude).expect("roster");
    let enrolled = accounts::enrolled_for(Cli::Claude, &roster);
    let mut state = claude.load(Cli::Claude);
    let _ = accounts::note_sample(
        &mut state,
        "main",
        Sample {
            at: NOW - 11 * MIN,
            ..near_wall()
        },
        NOW,
    );
    let me = accounts::Me {
        account: Some("main"),
        left: None,
        default_home: true,
        stored_login: true,
    };
    assert_eq!(
        accounts::advise_for(
            Cli::Claude,
            &roster,
            &state,
            &enrolled,
            &me,
            &Point::TurnEnd,
            Mode::Early,
            NOW
        ),
        Advice::Probe
    );
}

#[test]
fn a_move_whose_line_cannot_be_written_commits_no_rotation() {
    let world = World::new("line-fails");
    world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    // The overlay cannot be linked: the default home is not a directory.
    std::fs::write(world.home.join(".codex"), "not a dir").expect("a file at ~/.codex");
    let exe = Path::new("/opt/codex-old/bin/codex");
    let moved = session_move(
        &world.seat(),
        &session(Cli::Codex, Supervisor::Aterm, Mode::Early, exe),
    );
    let state = world.load(Cli::Codex);
    assert_eq!(moved, Moved::stay(StayWhy::NoLine));
    assert_eq!(state.active.as_deref(), Some("main"));
    assert!(state.rotations.is_empty() && state.canary.is_none());

    // Negative: with a default home to link, the same move writes its line and rotates.
    let ok = World::new("line-ok");
    ok.two(Cli::Codex);
    ok.on_main(Cli::Codex, &[]);
    let moved = session_move(
        &ok.seat(),
        &session(Cli::Codex, Supervisor::Aterm, Mode::Early, exe),
    );
    assert!(moved.line.is_some());
    assert_eq!(ok.load(Cli::Codex).rotations, vec![NOW]);
}

#[test]
fn a_launch_reads_each_rows_usage_in_its_own_home_and_leaves_a_full_one() {
    let world = World::new("usage");
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let default_home = world.home.join(".codex");
    let homes = std::sync::Mutex::new(Vec::new());
    let full = |reset: i64| Usage {
        sample: Sample {
            session_pct: Some(100),
            session_reset: Some(reset),
            at: NOW,
            ..Sample::default()
        },
        wall: Some((accounts::SESSION_LIMIT, Some(reset))),
    };
    let read = |cli: Cli, home: &Path, _: i64| {
        assert_eq!(cli, Cli::Codex);
        homes.lock().expect("homes").push(home.to_path_buf());
        (home == default_home).then(|| full(NOW + 2 * HOUR))
    };
    let pick = launch_env(&world.seat(), Cli::Codex, &read).map(|p| (p.label, p.value));
    assert_eq!(
        pick,
        Some(("work".to_string(), Some(work.display().to_string()))),
        "main's own home reads full, so the launch starts on work"
    );
    let mut read_in = homes.lock().expect("homes").clone();
    read_in.sort();
    let mut want = vec![default_home.clone(), work.clone()];
    want.sort();
    assert_eq!(read_in, want, "each row read in its own home");
    assert!(
        work.join("sessions").is_symlink(),
        "work's overlay was linked before its read"
    );
    assert!(matches!(
        condition_of(&world, Cli::Codex, "main"),
        Condition::Limited { .. }
    ));

    // A reading younger than USAGE_FRESH_S is not read again.
    homes.lock().expect("homes").clear();
    let _ = launch_env(&world.seat(), Cli::Codex, &read);
    assert_eq!(
        homes.lock().expect("homes").as_slice(),
        std::slice::from_ref(&work),
        "main's reading is fresh; work read nothing, so it is read again"
    );

    // Negative: with nothing read, the launch stays on the fresh active row.
    let blind = World::new("usage-none");
    blind.two(Cli::Codex);
    blind.on_main(Cli::Codex, &[]);
    let pick = launch_env(&blind.seat(), Cli::Codex, &no_usage).map(|p| p.label);
    assert_eq!(pick.as_deref(), Some("main"));
}
