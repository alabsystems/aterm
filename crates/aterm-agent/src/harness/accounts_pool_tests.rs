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

// Existing selection tests compare the chosen row. Refusal tests below inspect the
// production Result directly, including the distinction from an unmanaged launch.
fn launch_env(seat: &Seat<'_>, cli: Cli, read: ReadUsage<'_>) -> Option<LoginEnv> {
    super::launch_env(seat, cli, read).ok().flatten()
}

#[path = "accounts_pool_launch_tests.rs"]
mod launch_admission;

#[path = "accounts_pool_launch_policy_tests.rs"]
mod launch_policy;

#[path = "accounts_pool_usage_policy_tests.rs"]
mod usage_policy;

#[path = "accounts_sample_context_tests.rs"]
mod sample_context;

#[path = "accounts_pool_move_policy_tests.rs"]
mod move_policy;

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
            home: None,
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
    assert_eq!(
        line,
        format!(
            " '/usr/bin/env' -u 'CODEX_API_KEY' -u 'CODEX_ACCESS_TOKEN' 'CODEX_HOME={}' \
             '/opt/codex-old/bin/codex' 'resume' '019a-rollout-id'",
            work.display()
        )
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

/// A Grok Build row's line names the row's auth file, removes `GROK_AUTH` (read ahead of that
/// file) and `GROK_LEADER_SOCKET`, and names the leader socket of the row's label and auth file in
/// the grok home; the machine's default row removes the file's variable and `GROK_AUTH` and adds
/// no socket.
#[test]
fn a_grok_line_names_the_rows_auth_file_and_leader_and_unsets_grok_auth() {
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
        home: Some(Path::new("/Users//u/.grok")),
    };
    let row = roster.row("work").expect("work");
    let env = login_env(Cli::Grok, row, &world.home).expect("grok env");
    let auth = work.join("auth.json").display().to_string();
    assert_eq!(
        (env.var, env.value.clone()),
        ("GROK_AUTH_PATH", Some(auth.clone()))
    );
    let socket = relaunch::grok_leader_socket(Path::new("/Users//u/.grok"), "work", &auth)
        .expect("socket")
        .display()
        .to_string();
    let tag = socket
        .strip_prefix("/Users//u/.grok/leader-work-")
        .and_then(|rest| rest.strip_suffix(".sock"))
        .unwrap_or_default();
    assert!(
        tag.len() == 8 && tag.bytes().all(|b| b.is_ascii_hexdigit()),
        "{socket}"
    );
    let line = relaunch::pool_line(&how, Cli::Grok, &env).expect("line");
    assert_eq!(
        line,
        format!(
            " '/usr/bin/env' -u 'GROK_AUTH' -u 'GROK_LEADER_SOCKET' 'GROK_AUTH_PATH={auth}' \
             '/opt/grok/bin/grok' '--leader-socket' '{socket}' '--resume' 'abc'"
        )
    );
    assert!(
        !work.join("sessions").exists(),
        "a login-file seam makes no overlay"
    );

    // Negative: the default row adds no socket and still removes `GROK_AUTH`; `GROK_HOME` stays
    // shared and `XAI_API_KEY`, a fallback behind the stored login, stays in place.
    let main =
        login_env(Cli::Grok, roster.row("main").expect("main"), &world.home).expect("default env");
    assert_eq!(main.value, None);
    let line = relaunch::pool_line(&how, Cli::Grok, &main).expect("default line");
    assert_eq!(
        line,
        " '/usr/bin/env' -u 'GROK_AUTH_PATH' -u 'GROK_AUTH' '/opt/grok/bin/grok' '--resume' 'abc'"
    );
    let unread = PoolRelaunch { home: None, ..how };
    assert_eq!(
        relaunch::pool_line(&unread, Cli::Grok, &main).as_deref(),
        Ok(line.as_str()),
        "the default row needs no grok home"
    );
    assert!(
        relaunch::pool_line(&unread, Cli::Grok, &env).is_err(),
        "no home, no socket"
    );
}

/// A carried `--leader-socket`, in either spelling, may name the leader of the login the session
/// ran on: a row of its own drops every one and names its own after the other carried flags, and
/// the default row drops one that names a row's socket and keeps a person's own.
#[test]
fn a_grok_line_drops_a_carried_leader_socket() {
    let flags: Vec<String> = [
        "-m",
        "grok-4",
        "--leader-socket",
        "/Users//u/.grok/leader-old.sock",
        "--leader-socket=/Users//u/.grok/leader-older.sock",
        "--leader-socket",
        "/Users//u/dev/leader.sock",
        "--always-approve",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let how = PoolRelaunch {
        dialect: Dialect::Bash,
        cd: Some("/w"),
        exe: Path::new("/opt/grok/bin/grok"),
        flags: &flags,
        resume: Some("abc"),
        home: Some(Path::new("/Users//u/.grok")),
    };
    let work = LoginEnv {
        label: "work".to_string(),
        var: "GROK_AUTH_PATH",
        value: Some("/l/work/auth.json".to_string()),
    };
    assert_eq!(
        relaunch::pool_line(&how, Cli::Grok, &work).expect("row line"),
        format!(
            " {}( cd -- '/w' && exec '/usr/bin/env' -u 'GROK_AUTH' -u 'GROK_LEADER_SOCKET' \
             'GROK_AUTH_PATH=/l/work/auth.json' '/opt/grok/bin/grok' '-m' 'grok-4' \
             '--always-approve' '--leader-socket' '/Users//u/.grok/leader-work-99068163.sock' \
             '--resume' 'abc' )",
            crate::harness::upgrade::BASH_LINE_OPENS
        )
    );
    let main = LoginEnv {
        value: None,
        label: "main".to_string(),
        ..work
    };
    assert_eq!(
        relaunch::pool_line(&how, Cli::Grok, &main).expect("default line"),
        format!(
            " {}( cd -- '/w' && exec '/usr/bin/env' -u 'GROK_AUTH_PATH' -u 'GROK_AUTH' \
             '/opt/grok/bin/grok' '-m' 'grok-4' '--leader-socket' '/Users//u/dev/leader.sock' \
             '--always-approve' '--resume' 'abc' )",
            crate::harness::upgrade::BASH_LINE_OPENS
        )
    );

    // Negative: another CLI's flags ride as they are; a Kimi Code line removes only the
    // variables kimi 2.3.1 reads in place of the home's login (its environment model and custom
    // headers) and drops none of the Grok Build sockets it carries.
    let kimi = LoginEnv {
        label: "work".to_string(),
        var: "KIMI_CODE_HOME",
        value: Some("/k/work".to_string()),
    };
    let how = PoolRelaunch {
        cd: None,
        exe: Path::new("/opt/kimi/bin/kimi"),
        ..how
    };
    assert_eq!(
        relaunch::pool_line(&how, Cli::Kimi, &kimi).expect("kimi line"),
        " '/usr/bin/env' -u 'KIMI_MODEL_NAME' -u 'KIMI_MODEL_API_KEY' \
         -u 'KIMI_CODE_CUSTOM_HEADERS' 'KIMI_CODE_HOME=/k/work' '/opt/kimi/bin/kimi' '-m' \
         'grok-4' '--leader-socket' '/Users//u/.grok/leader-old.sock' \
         '--leader-socket=/Users//u/.grok/leader-older.sock' '--leader-socket' \
         '/Users//u/dev/leader.sock' '--always-approve' '--session' 'abc'"
    );
}

/// A Codex line removes Codex's own credential variables and leaves `OPENAI_API_KEY`, which an
/// agent's tool reads and codex-old 0.164.0's text never names as read ahead of `auth.json`; it
/// resumes its thread after the flags it carries (the usage begins
/// `codex resume [OPTIONS] [SESSION_ID]`).
#[test]
fn a_codex_default_line_unsets_its_home_and_codex_credentials_alone() {
    let flags = ["--no-alt-screen".to_string()];
    let how = PoolRelaunch {
        dialect: Dialect::Fish,
        cd: None,
        exe: Path::new("/opt/codex-old/bin/codex"),
        flags: &flags,
        resume: Some("019a"),
        home: None,
    };
    let main = LoginEnv {
        label: "main".to_string(),
        var: "CODEX_HOME",
        value: None,
    };
    let line = relaunch::pool_line(&how, Cli::Codex, &main).expect("default line");
    assert_eq!(
        line,
        " '/usr/bin/env' -u 'CODEX_HOME' -u 'CODEX_API_KEY' -u 'CODEX_ACCESS_TOKEN' \
         '/opt/codex-old/bin/codex' 'resume' '--no-alt-screen' '019a'"
    );
    assert!(!line.contains("OPENAI_API_KEY"), "{line}");
}

/// A Codex line carries only the flags codex-alab's relaunch keeps: `-i`/`--image` and its
/// files go, since a value list ahead of the thread would read the thread as a file, and a
/// flag that cannot resume in place refuses the line.
#[test]
fn a_codex_line_drops_an_image_list_ahead_of_its_thread() {
    let flags: Vec<String> = ["-i", "a.png", "b.png", "--no-alt-screen", "--image=c.png"]
        .map(str::to_string)
        .into();
    let how = PoolRelaunch {
        dialect: Dialect::Zsh,
        cd: None,
        exe: Path::new("/opt/codex-old/bin/codex"),
        flags: &flags,
        resume: Some("019a"),
        home: None,
    };
    let work = LoginEnv {
        label: "work".to_string(),
        var: "CODEX_HOME",
        value: Some("/c/work".to_string()),
    };
    let line = relaunch::pool_line(&how, Cli::Codex, &work).expect("a row's line");
    assert!(
        line.ends_with(" '/opt/codex-old/bin/codex' 'resume' '--no-alt-screen' '019a'"),
        "{line}"
    );
    assert!(!line.contains("png"), "{line}");

    // Negative: a flag the relaunch refuses refuses the line.
    let remote = ["--remote".to_string(), "ws://h".to_string()];
    let how = PoolRelaunch {
        flags: &remote,
        ..how
    };
    let err = relaunch::pool_line(&how, Cli::Codex, &work).expect_err("a remote session");
    assert!(err.contains("not-resumable --remote"), "{err}");
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

    // Negative: one row is nothing to pick between.
    let one = World::new("launch-one");
    std::fs::write(
        one.cfg.join("accounts-codex.toml"),
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n",
    )
    .expect("roster");
    one.on_main(Cli::Codex, &[]);
    assert_eq!(launch_env(&one.seat(), Cli::Codex, &no_usage), None);
}

#[test]
fn a_kimi_launch_takes_its_next_row_in_an_overlay_linked_to_the_default_home() {
    let world = World::new("launch-kimi");
    let work = world.two(Cli::Kimi);
    world.on_main(Cli::Kimi, &[]);
    let pick = launch_env(&world.seat(), Cli::Kimi, &no_usage).expect("a pick");
    assert_eq!((pick.label.as_str(), pick.value), ("main", None));
    wall(&world, Cli::Kimi, "main", NOW + HOUR);
    let pick = launch_env(&world.seat(), Cli::Kimi, &no_usage).expect("next");
    assert_eq!(
        (pick.label.as_str(), pick.var, pick.value),
        ("work", "KIMI_CODE_HOME", Some(work.display().to_string()))
    );
    let default_home = world.home.join(".kimi-code");
    for shared in ["sessions", "locks", "workspace-trust"] {
        assert_eq!(
            std::fs::read_link(work.join(shared)).expect(shared),
            default_home.join(shared),
            "{shared}"
        );
    }
    assert!(
        work.join("credentials/kimi-code.json").is_file(),
        "the row's login stays its own"
    );

    // Negative: an overlay that holds its own `locks` (a session there would not see another
    // home's turn) is not started on.
    std::fs::remove_file(work.join("locks")).expect("unlink");
    std::fs::create_dir(work.join("locks")).expect("own locks");
    assert_eq!(launch_env(&world.seat(), Cli::Kimi, &no_usage), None);
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
        line.contains(&format!(" 'CODEX_HOME={}' ", staging.display())),
        "the move names the renewed home: {line}"
    );

    // Negative: with explicitly empty bindings, the configured row has no login here, so it
    // is neither pinned nor picked. A lost snapshot is unknown, not an empty binding set.
    let bare = World::new("renewed-none");
    renewed_work(&bare);
    bare.seat()
        .store(Cli::Codex)
        .save_logins(&accounts::Logins::default())
        .expect("known empty bindings");
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
fn a_superseded_session_observation_cannot_prove_the_current_login() {
    let world = World::new("superseded-observation");
    let old = renewed_work(&world);
    let store = world.seat().store(Cli::Codex);
    let renewed = world.root.join("logins/codex/work+2");
    std::fs::create_dir_all(&renewed).unwrap();
    std::fs::write(renewed.join("auth.json"), "{}").unwrap();
    store
        .transact_logins(NOW, &mut None, |state, logins| {
            let events = accounts::note_renewed(
                state,
                logins,
                "work",
                old.to_str(),
                renewed.to_str().unwrap(),
                None,
                NOW,
            );
            state.active = Some("work".into());
            state.proven = false;
            accounts::note_auth(state, "work", accounts::Auth::SignedOut, NOW);
            Ok(events)
        })
        .unwrap();
    let exe = Path::new("/opt/codex-old/bin/codex");
    let old_text = old.to_string_lossy();
    let old_env = [("CODEX_HOME", old_text.as_ref())];
    let mut s = session(Cli::Codex, Supervisor::Aterm, Mode::Off, exe);
    s.env = &old_env;
    s.sample = None;
    let _ = session_move(&world.seat(), &s);
    let state = world.load(Cli::Codex);
    assert!(
        !state.proven,
        "the old login cannot release the new login's canary"
    );
    assert_eq!(state.account("work").auth, accounts::Auth::SignedOut);

    let new_text = renewed.to_string_lossy();
    let new_env = [("CODEX_HOME", new_text.as_ref())];
    s.env = &new_env;
    let _ = session_move(&world.seat(), &s);
    assert!(
        world.load(Cli::Codex).proven,
        "a current login still proves the canary"
    );
}

#[test]
fn a_renewal_after_observation_retains_usage_without_proving_the_new_login() {
    let world = World::new("observation-usage-race");
    world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let seat = world.seat();
    let captured = seat
        .bound_roster(Cli::Codex)
        .unwrap()
        .row("main")
        .unwrap()
        .clone();
    let who = accounts::ProcessLogin::of_env_for(Cli::Codex, []);
    let renewed = world.root.join("new-main");
    seat.store(Cli::Codex)
        .transact_logins(NOW, &mut None, |state, logins| {
            let events = accounts::note_renewed(
                state,
                logins,
                "main",
                None,
                renewed.to_str().unwrap(),
                None,
                NOW,
            );
            state.proven = false;
            Ok(events)
        })
        .unwrap();
    let exe = Path::new("/opt/codex-old/bin/codex");
    let s = session(Cli::Codex, Supervisor::Aterm, Mode::Off, exe);
    let current = record_session(&seat, &s, &who, &captured).unwrap();
    assert_eq!(
        current.row("main").unwrap().dir.as_deref(),
        renewed.to_str()
    );
    assert_eq!(
        who.me(&current).left,
        Some("main"),
        "advice receives the updated binding"
    );
    let state = world.load(Cli::Codex);
    assert!(!state.proven);
    let input = s.sample.clone().unwrap();
    let expected = Sample {
        session_read_at: Some(input.at),
        week_read_at: Some(input.at),
        by: state.machine.clone(),
        ..input
    };
    assert_eq!(
        state.account("main").sample,
        Some(expected),
        "the old login still consumed the account's budget"
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

#[test]
fn a_drifted_unpinned_overlay_row_falls_through_to_a_clean_one_and_a_pinned_one_holds() {
    let world = World::new("drift-fallthrough");
    let work = world.work_dir(Cli::Codex);
    let backup = world.root.join("logins/codex/backup");
    std::fs::create_dir_all(&backup).expect("backup dir");
    std::fs::write(backup.join("auth.json"), "{}").expect("backup login");
    let roster_text = format!(
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
         [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = \"{}\"\n\n\
         [[account]]\nlabel = \"backup\"\nclass = \"dedicated\"\ndir = \"{}\"\n",
        work.display(),
        backup.display()
    );
    std::fs::write(world.cfg.join("accounts-codex.toml"), roster_text).expect("roster");
    let default_home = world.home.join(".codex");
    std::fs::create_dir_all(&default_home).expect("default home");
    std::fs::write(default_home.join("state_5.sqlite"), "shared").expect("shared db");
    std::fs::write(work.join("state_5.sqlite"), "own").expect("drifted work db");

    world.on_main(Cli::Codex, &[]);
    wall(&world, Cli::Codex, "main", NOW + 2 * HOUR);

    let pick = launch_env(&world.seat(), Cli::Codex, &no_usage).expect("launch skips drifted work");
    assert_eq!(pick.label, "backup");
    assert_eq!(pick.value, Some(backup.display().to_string()));

    let exe = Path::new("/opt/codex-old/bin/codex");
    let mut s = session(Cli::Codex, Supervisor::Aterm, Mode::Early, exe);
    s.point = Point::Wall {
        window: accounts::SESSION_LIMIT.to_string(),
        at: Some(NOW),
    };
    let moved = session_move(&world.seat(), &s);
    assert!(
        matches!(&moved.advice, Advice::Move { to, rotates: true, .. } if to == "backup"),
        "expected rotation past drifted work to backup, got {:?}",
        moved.advice
    );
    assert!(moved.line.is_some());
    assert_eq!(world.load(Cli::Codex).active.as_deref(), Some("backup"));

    // Negative: when the owner pins drifted `work`, neither launch_env nor session_move ignores
    // the pin or starts on the drifted overlay.
    let mut note = None;
    world
        .seat()
        .store(Cli::Codex)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |state| {
            state.active = Some("work".to_string());
            state.pinned = true;
            state.proven = true;
            Ok(Vec::new())
        })
        .expect("pin drifted work");
    assert_eq!(launch_env(&world.seat(), Cli::Codex, &no_usage), None);
    let pinned_move = session_move(&world.seat(), &s);
    assert_eq!(pinned_move, Moved::stay(StayWhy::NoLine));
}

#[test]
fn record_usage_reads_the_default_home_before_linking_overlays_so_startup_sqlite_never_drifts() {
    let world = World::new("usage-startup-sqlite");
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let default_home = world.home.join(".codex");
    let full = Usage {
        sample: Sample {
            session_pct: Some(100),
            session_reset: Some(NOW + 2 * HOUR),
            at: NOW,
            ..Sample::default()
        },
        wall: Some((accounts::SESSION_LIMIT, Some(NOW + 2 * HOUR))),
    };
    let read = |cli: Cli, home: &Path, _: i64| {
        assert_eq!(cli, Cli::Codex);
        std::fs::create_dir_all(home).expect("home dir");
        let db = home.join("state_5.sqlite");
        if std::fs::symlink_metadata(&db).is_err() {
            std::fs::write(&db, "created-at-startup").expect("startup sqlite");
        }
        (home == default_home).then(|| full.clone())
    };
    let pick = launch_env(&world.seat(), Cli::Codex, &read).expect("launch on work");
    assert_eq!(pick.label, "work");
    assert!(
        work.join("state_5.sqlite").is_symlink(),
        "default home was read before work's overlay was linked"
    );

    // Negative: a pre-existing real sqlite file in work's overlay still refuses to start there.
    let pre_drifted = World::new("usage-pre-drifted");
    let bad_work = pre_drifted.two(Cli::Codex);
    pre_drifted.on_main(Cli::Codex, &[]);
    std::fs::write(bad_work.join("state_5.sqlite"), "already-own").expect("own db");
    let bad_default = pre_drifted.home.join(".codex");
    let read_bad = |_: Cli, home: &Path, _: i64| {
        std::fs::create_dir_all(home).expect("home");
        let db = home.join("state_5.sqlite");
        if std::fs::symlink_metadata(&db).is_err() {
            std::fs::write(&db, "shared").expect("shared db");
        }
        (home == bad_default).then(|| full.clone())
    };
    assert_eq!(
        launch_env(&pre_drifted.seat(), Cli::Codex, &read_bad),
        None,
        "pre-drifted work is still refused when main is full"
    );
}

#[test]
fn fleet_synced_single_enrolled_row_and_lazy_backup_measure_usage_and_launch_until_walled() {
    // 1. Fleet-synced roster (`codex-1` + `codex-2`, both `dir = Some(...)`, no default row)
    //    where `upkeep_pools` enrolled only `codex-1` so far: `launch_env` places launches on
    //    `codex-1`, reads its usage, records its weekly wall when full, and rolls over to
    //    `codex-2` once `codex-2` is bound.
    let world = World::new("fleet-single-enrolled");
    let c1 = world.root.join("logins/codex/codex-1");
    let c2 = world.root.join("logins/codex/codex-2");
    std::fs::create_dir_all(&c1).expect("c1 dir");
    std::fs::create_dir_all(&c2).expect("c2 dir");
    std::fs::write(c1.join("auth.json"), "{}").expect("c1 enrolled");
    std::fs::write(
        world.cfg.join("accounts-codex.toml"),
        format!(
            "[[account]]\nlabel = \"codex-1\"\nclass = \"dedicated\"\ndir = \"{}\"\n\
             email = \"one@example.com\"\n\n\
             [[account]]\nlabel = \"codex-2\"\nclass = \"dedicated\"\ndir = \"{}\"\n\
             email = \"two@example.com\"\n",
            c1.display(),
            c2.display(),
        ),
    )
    .expect("roster");

    let ok_usage = Usage {
        sample: Sample {
            session_pct: Some(25),
            session_reset: Some(NOW + 2 * HOUR),
            week_pct: Some(40),
            week_reset: Some(NOW + 3 * 24 * HOUR),
            at: NOW,
            ..Sample::default()
        },
        wall: None,
    };
    let pick = launch_env(&world.seat(), Cli::Codex, &|_, _, _| Some(ok_usage.clone()))
        .expect("single enrolled dedicated row must launch");
    assert_eq!(pick.label, "codex-1");
    assert_eq!(pick.value, Some(c1.display().to_string()));

    // Advance past USAGE_FRESH_S so `codex-1` is read again and hits its 7d weekly wall.
    let later = Seat {
        now: NOW + USAGE_FRESH_S + 10,
        ..world.seat()
    };
    let week_walled = Usage {
        sample: Sample {
            week_pct: Some(100),
            week_reset: Some(later.now + 24 * HOUR),
            at: later.now,
            ..Sample::default()
        },
        wall: Some((accounts::WEEK_LIMIT, Some(later.now + 24 * HOUR))),
    };
    assert_eq!(
        launch_env(&later, Cli::Codex, &|_, _, _| Some(week_walled.clone())),
        None,
        "codex-1 is now walled and codex-2 is not yet enrolled"
    );
    let roster = later.roster(Cli::Codex).expect("roster");
    let (s_after_wall, _) = later.store(Cli::Codex).load(later.now).expect("store");
    assert!(
        matches!(
            accounts::condition(
                roster.row("codex-1").expect("c1"),
                &s_after_wall,
                true,
                later.now
            ),
            Condition::Limited { .. }
        ),
        "codex-1's weekly wall was persisted in state.json so upkeep can auto-enroll codex-2"
    );

    // Bind `codex-2+1` as `upkeep_pools` does on seeing `codex-1` Limited; next launch takes it.
    let c2_staged = world.root.join("logins/codex/codex-2+1");
    std::fs::create_dir_all(&c2_staged).expect("c2 staged");
    std::fs::write(c2_staged.join("auth.json"), "{}").expect("c2 staged auth");
    let mut note = None;
    later
        .store(Cli::Codex)
        .transact_logins(later.now, &mut note, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                "codex-2",
                Some(c2.to_str().expect("utf8")),
                c2_staged.to_str().expect("utf8"),
                Some("two@example.com"),
                later.now,
            ))
        })
        .expect("bind codex-2");
    let pick2 = launch_env(&later, Cli::Codex, &no_usage).expect("rolls over to codex-2");
    assert_eq!(pick2.label, "codex-2");
    assert_eq!(pick2.value, Some(c2_staged.display().to_string()));

    // 2. Adopted default row + lazy unenrolled backup (`[main, backup]`): `launch_env` reads
    //    `main`'s usage and persists its wall even while `backup` is still `NotEnrolled`.
    let lazy = World::new("default-plus-lazy");
    let backup = lazy.root.join("logins/codex/backup");
    std::fs::create_dir_all(&backup).expect("backup dir");
    std::fs::write(
        lazy.cfg.join("accounts-codex.toml"),
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
             [[account]]\nlabel = \"backup\"\nclass = \"dedicated\"\ndir = \"{}\"\n\
             email = \"backup@example.com\"\n",
            backup.display()
        ),
    )
    .expect("lazy roster");
    assert_eq!(
        launch_env(&lazy.seat(), Cli::Codex, &|_, _, now| {
            Some(Usage {
                sample: Sample {
                    at: now,
                    ..week_walled.sample.clone()
                },
                ..week_walled.clone()
            })
        }),
        None
    );
    assert!(matches!(
        condition_of(&lazy, Cli::Codex, "main"),
        Condition::Limited { .. }
    ));

    // 3. CONTROL: an unpinned lone default row (`[main]`) neither reads usage nor rewrites launch
    //    env, whereas pinning that lone default row honours the owner's pin.
    let lone = World::new("lone-default-pin");
    std::fs::write(
        lone.cfg.join("accounts-codex.toml"),
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n",
    )
    .expect("lone roster");
    let panic_read = |_: Cli, _: &Path, _: i64| -> Option<Usage> {
        panic!("unpinned lone default row must not read usage")
    };
    assert_eq!(launch_env(&lone.seat(), Cli::Codex, &panic_read), None);
    pin(&lone, Cli::Codex, "main").expect("pin main");
    let pinned_pick =
        launch_env(&lone.seat(), Cli::Codex, &no_usage).expect("pinned lone default row");
    assert_eq!(pinned_pick.label, "main");
    assert_eq!(pinned_pick.value, None);
}
