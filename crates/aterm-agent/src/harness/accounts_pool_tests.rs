// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the pooled CLIs' launch pick and session moves.

use std::path::{Path, PathBuf};

use super::*;

#[path = "accounts_pool_quota_tests.rs"]
mod quota;

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

// Pool-only fixtures have no concurrently replaced GUI process. Runtime callers
// must instead consume the note while holding their local process context guard.
fn session_move(seat: &Seat<'_>, s: &Session<'_>) -> Moved {
    super::session_move(seat, s, |note| note.record())
}

fn session_move_using(
    seat: &Seat<'_>,
    s: &Session<'_>,
    prepare: impl FnMut(&Row) -> Option<String>,
) -> Moved {
    super::session_move_using(seat, s, prepare, |note| note.record())
}

fn record_session(
    seat: &Seat<'_>,
    s: &Session<'_>,
    who: &accounts::ProcessLogin,
    row: &Row,
) -> Result<Roster, String> {
    super::record_session(seat, s, who, row, |note| note.record())
}

#[path = "accounts_pool_launch_tests.rs"]
mod launch_admission;

#[path = "accounts_pool_recovery_tests.rs"]
mod recovery;

#[path = "accounts_pool_commit_tests.rs"]
mod commit;

#[path = "accounts_pool_launch_policy_tests.rs"]
mod launch_policy;

#[path = "accounts_pool_usage_policy_tests.rs"]
mod usage_policy;

#[path = "accounts_sample_context_tests.rs"]
mod sample_context;

#[path = "accounts_pool_move_policy_tests.rs"]
mod move_policy;

#[path = "accounts_pool_stored_identity_tests.rs"]
mod stored_identity;

#[path = "accounts_pool_identity_tests.rs"]
mod identity;

#[path = "accounts_pool_preparation_tests.rs"]
mod preparation;

#[path = "accounts_pool_preparation_race_tests.rs"]
mod preparation_races;

#[path = "accounts_pool_planning_tests.rs"]
mod planning;

#[path = "accounts_pool_canary_tests.rs"]
mod canary;

#[path = "accounts_pool_cold_resume_tests.rs"]
mod cold_resume_capability;

#[path = "accounts_pool_claims_tests.rs"]
mod claims;

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
        let world = World {
            _dir: dir,
            cfg: root.join("cfg"),
            state: root.join("state"),
            home: root.join("home"),
            root,
        };
        // Each pooled CLI's default login is signed in on this machine, as a roster's default
        // row (`main`) is enrolled only while it is ([`accounts::enrolled_in`]).
        for cli in [Cli::Codex, Cli::Grok, Cli::Kimi] {
            world.default_login(cli);
        }
        world
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

    /// The file `cli` keeps the machine's default login in, made in its default home.
    fn default_login(&self, cli: Cli) -> PathBuf {
        let file = cli
            .connector()
            .stores
            .first()
            .copied()
            .unwrap_or("auth.json");
        let path = self.home.join(cli.accounts().default_home).join(file);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("default home");
        std::fs::write(&path, "{}").expect("stand-in default login");
        path
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
fn no_usage(_: Cli, _: &Path, _: Option<&str>, _: i64) -> Option<Usage> {
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
            " '/usr/bin/env' -u 'CODEX_API_KEY' -u 'CODEX_ACCESS_TOKEN' 'CODEX_AUTH_PATH={}' \
             '/opt/codex-old/bin/codex' 'resume' '019a-rollout-id'",
            work.join("auth.json").display()
        )
    );
    assert!(!line.contains("CLAUDE"), "{line}");
    assert!(
        !work.join("sessions").exists(),
        "a login-file seam leaves the default home's rollouts shared without an overlay"
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
        var: "CODEX_AUTH_PATH",
        value: None,
    };
    let line = relaunch::pool_line(&how, Cli::Codex, &main).expect("default line");
    assert_eq!(
        line,
        " '/usr/bin/env' -u 'CODEX_AUTH_PATH' -u 'CODEX_API_KEY' -u 'CODEX_ACCESS_TOKEN' \
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
        var: "CODEX_AUTH_PATH",
        value: Some("/c/work/auth.json".to_string()),
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
        std::fs::write(
            world.cfg.join("aterm.toml"),
            "[harness]\naccounts = 'off'\n",
        )
        .unwrap();
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
        ("work", Some(work.join("auth.json").display().to_string()))
    );
    assert_eq!(pick.var, "CODEX_AUTH_PATH");

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
        Some((
            "work".to_string(),
            Some(staging.join("auth.json").display().to_string())
        )),
        "the owner's pin on the renewed row starts the launch under its renewed auth file"
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
        line.contains(&format!(
            " 'CODEX_AUTH_PATH={}' ",
            staging.join("auth.json").display()
        )),
        "the move names the renewed auth file: {line}"
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
        Some((
            "work".to_string(),
            Some(work.join("auth.json").display().to_string())
        )),
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
    let read = |_: Cli, _: &Path, _: Option<&str>, _: i64| -> Option<Usage> {
        panic!("off reads no usage")
    };
    assert_eq!(
        launch_env(&world.seat(), Cli::Codex, &read),
        None,
        "off leaves the worker on its own login"
    );
    pin(&world, Cli::Codex, "work").expect("pin");
    let pick = launch_env(&world.seat(), Cli::Codex, &read).map(|p| (p.label, p.value));
    assert_eq!(
        pick,
        Some((
            "work".to_string(),
            Some(work.join("auth.json").display().to_string())
        )),
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
        Some((
            "work".to_string(),
            Some(work.join("auth.json").display().to_string())
        ))
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
    let default_home = world.home.join(".kimi-code");
    std::fs::create_dir_all(&default_home).expect("default home");
    std::fs::write(default_home.join("mcp.json"), "shared").expect("shared mcp");
    let staging = world.root.join("logins/kimi/work+1");
    std::fs::create_dir_all(staging.join("credentials")).expect("staging");
    std::fs::write(staging.join("mcp.json"), "own").expect("own mcp");
    std::fs::write(staging.join("credentials/kimi-code.json"), "{}").expect("login");
    let row = accounts::Row {
        label: "work".into(),
        dir: Some(staging.display().to_string()),
        ..accounts::Row::default()
    };
    let refused = login_env(Cli::Kimi, &row, &world.home);
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(
        std::fs::read_to_string(staging.join("mcp.json")).expect("left"),
        "own",
        "its own shared entry is left as it is"
    );

    // Negative: a home whose shared entries are all links (as `ensure` makes them) is used.
    let clean = world.root.join("logins/kimi/work+2");
    std::fs::create_dir_all(clean.join("credentials")).expect("clean");
    std::fs::write(clean.join("credentials/kimi-code.json"), "{}").expect("login");
    let row = accounts::Row {
        dir: Some(clean.display().to_string()),
        ..row
    };
    let env = login_env(Cli::Kimi, &row, &world.home).expect("an overlay");
    assert_eq!(env.value, Some(clean.display().to_string()));
    assert!(clean.join("mcp.json").is_symlink());
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
    let work_auth = work.join("auth.json").display().to_string();
    let env = [("CODEX_AUTH_PATH", work_auth.as_str())];
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
    let old_text = old.join("auth.json").to_string_lossy().into_owned();
    let old_env = [("CODEX_AUTH_PATH", old_text.as_str())];
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

    let new_text = renewed.join("auth.json").to_string_lossy().into_owned();
    let new_env = [("CODEX_AUTH_PATH", new_text.as_str())];
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
    let exe = Path::new("/opt/codex-old/bin/codex");
    let remote = ["--remote".to_string(), "ws://h".to_string()];
    let mut unresumable = session(Cli::Codex, Supervisor::Aterm, Mode::Early, exe);
    unresumable.relaunch.flags = &remote;
    let moved = session_move(&world.seat(), &unresumable);
    let state = world.load(Cli::Codex);
    assert_eq!(moved, Moved::stay(StayWhy::NoLine));
    assert_eq!(state.active.as_deref(), Some("main"));
    assert!(state.rotations.is_empty() && state.canary.is_none());

    // Negative: with resumable flags, the same move writes its line and rotates.
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
        walls: vec![(accounts::SESSION_LIMIT, Some(reset))],
    };
    let read = |cli: Cli, home: &Path, _: Option<&str>, _: i64| {
        assert_eq!(cli, Cli::Codex);
        homes.lock().expect("homes").push(home.to_path_buf());
        (home == default_home).then(|| full(NOW + 2 * HOUR))
    };
    let pick = launch_env(&world.seat(), Cli::Codex, &read).map(|p| (p.label, p.value));
    assert_eq!(
        pick,
        Some((
            "work".to_string(),
            Some(work.join("auth.json").display().to_string())
        )),
        "main's own home reads full, so the launch starts on work"
    );
    let mut read_in = homes.lock().expect("homes").clone();
    read_in.sort();
    let mut want = vec![default_home.clone(), work.clone()];
    want.sort();
    assert_eq!(read_in, want, "each row read in its own home");
    assert!(
        !work.join("sessions").exists(),
        "a login-file seam creates no overlay symlinks"
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
    let work = world.work_dir(Cli::Kimi);
    let backup = world.root.join("logins/kimi/backup");
    std::fs::create_dir_all(backup.join("credentials")).expect("backup dir");
    std::fs::write(backup.join("credentials/kimi-code.json"), "{}").expect("backup login");
    let roster_text = format!(
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
         [[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = \"{}\"\n\n\
         [[account]]\nlabel = \"backup\"\nclass = \"dedicated\"\ndir = \"{}\"\n",
        work.display(),
        backup.display()
    );
    std::fs::write(world.cfg.join("accounts-kimi.toml"), roster_text).expect("roster");
    let default_home = world.home.join(".kimi-code");
    std::fs::create_dir_all(&default_home).expect("default home");
    std::fs::write(default_home.join("mcp.json"), "shared").expect("shared mcp");
    std::fs::write(work.join("mcp.json"), "own").expect("drifted work mcp");

    world.on_main(Cli::Kimi, &[]);
    wall(&world, Cli::Kimi, "main", NOW + 2 * HOUR);

    let pick = launch_env(&world.seat(), Cli::Kimi, &no_usage).expect("launch skips drifted work");
    assert_eq!(pick.label, "backup");
    assert_eq!(pick.value, Some(backup.display().to_string()));

    // Negative: when the owner pins drifted `work`, launch_env neither ignores the pin nor starts
    // on the drifted overlay.
    let mut note = None;
    world
        .seat()
        .store(Cli::Kimi)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |state| {
            state.active = Some("work".to_string());
            state.pinned = true;
            state.proven = true;
            Ok(Vec::new())
        })
        .expect("pin drifted work");
    assert_eq!(launch_env(&world.seat(), Cli::Kimi, &no_usage), None);
}

#[test]
fn record_usage_reads_the_default_home_before_linking_overlays_so_startup_files_never_drift() {
    let world = World::new("usage-startup-overlay");
    let work = world.two(Cli::Kimi);
    world.on_main(Cli::Kimi, &[]);
    let default_home = world.home.join(".kimi-code");
    let full = Usage {
        sample: Sample {
            session_pct: Some(100),
            session_reset: Some(NOW + 2 * HOUR),
            at: NOW,
            ..Sample::default()
        },
        walls: vec![(accounts::SESSION_LIMIT, Some(NOW + 2 * HOUR))],
    };
    let read = |cli: Cli, home: &Path, _: Option<&str>, _: i64| {
        assert_eq!(cli, Cli::Kimi);
        std::fs::create_dir_all(home).expect("home dir");
        let agents = home.join("AGENTS.md");
        if std::fs::symlink_metadata(&agents).is_err() {
            std::fs::write(&agents, "created-at-startup").expect("startup file");
        }
        (home == default_home).then(|| full.clone())
    };
    let pick = launch_env(&world.seat(), Cli::Kimi, &read).expect("launch on work");
    assert_eq!(pick.label, "work");
    assert!(
        work.join("AGENTS.md").is_symlink(),
        "default home was read before work's overlay was linked"
    );

    // Negative: a pre-existing real shared file in work's overlay still refuses to start there.
    let pre_drifted = World::new("usage-pre-drifted");
    let bad_work = pre_drifted.two(Cli::Kimi);
    pre_drifted.on_main(Cli::Kimi, &[]);
    std::fs::write(bad_work.join("AGENTS.md"), "already-own").expect("own file");
    let bad_default = pre_drifted.home.join(".kimi-code");
    let read_bad = |_: Cli, home: &Path, _: Option<&str>, _: i64| {
        std::fs::create_dir_all(home).expect("home");
        let agents = home.join("AGENTS.md");
        if std::fs::symlink_metadata(&agents).is_err() {
            std::fs::write(&agents, "shared").expect("shared file");
        }
        (home == bad_default).then(|| full.clone())
    };
    assert_eq!(
        launch_env(&pre_drifted.seat(), Cli::Kimi, &read_bad),
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
        walls: Vec::new(),
    };
    let pick = launch_env(
        &world.seat(),
        Cli::Codex,
        &|_: Cli, _: &Path, _: Option<&str>, _: i64| Some(ok_usage.clone()),
    )
    .expect("single enrolled dedicated row must launch");
    assert_eq!(pick.label, "codex-1");
    assert_eq!(pick.value, Some(c1.join("auth.json").display().to_string()));

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
        walls: vec![(accounts::WEEK_LIMIT, Some(later.now + 24 * HOUR))],
    };
    assert_eq!(
        launch_env(
            &later,
            Cli::Codex,
            &|_: Cli, _: &Path, _: Option<&str>, _: i64| Some(week_walled.clone())
        ),
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
    assert_eq!(
        pick2.value,
        Some(c2_staged.join("auth.json").display().to_string())
    );

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
        launch_env(
            &lazy.seat(),
            Cli::Codex,
            &|_: Cli, _: &Path, _: Option<&str>, now: i64| {
                Some(Usage {
                    sample: Sample {
                        at: now,
                        ..week_walled.sample.clone()
                    },
                    ..week_walled.clone()
                })
            }
        ),
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
    let panic_read = |_: Cli, _: &Path, _: Option<&str>, _: i64| -> Option<Usage> {
        panic!("unpinned lone default row must not read usage")
    };
    assert_eq!(launch_env(&lone.seat(), Cli::Codex, &panic_read), None);
    pin(&lone, Cli::Codex, "main").expect("pin main");
    let pinned_pick =
        launch_env(&lone.seat(), Cli::Codex, &no_usage).expect("pinned lone default row");
    assert_eq!(pinned_pick.label, "main");
    assert_eq!(pinned_pick.value, None);
}

/// A [`UsageReader`] answering each home its own [`Reading`], and nothing for any other.
struct Readings(Vec<(PathBuf, Reading)>);

impl UsageReader for Readings {
    fn read(&self, _: Cli, home: &Path, _: Option<&str>, _: i64) -> Reading {
        self.0
            .iter()
            .find(|(at, _)| at == home)
            .map_or(Reading::Unread, |(_, reading)| reading.clone())
    }
}

#[test]
fn a_login_the_provider_refused_is_signed_out_and_launches_leave_it_until_it_reads_again() {
    let world = World::new("refused-login");
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    wall(&world, Cli::Codex, "main", NOW + HOUR);
    let refused = Readings(vec![(work.clone(), Reading::SignedOut)]);
    let launch = super::launch_env(&world.seat(), Cli::Codex, &refused);
    assert!(
        launch
            .as_ref()
            .is_err_and(|why| why.contains("no usable account")),
        "{launch:?}"
    );
    assert_eq!(
        condition_of(&world, Cli::Codex, "work"),
        Condition::SignedOut
    );
    // Every row is shut, and only `main`'s wall ends by waiting.
    let Condition::Limited { until } = condition_of(&world, Cli::Codex, "main") else {
        panic!("main is at its limit");
    };
    assert_eq!(launch_opening(&world.seat(), Cli::Codex, None), Some(until));
    // With rotation switched off the launch starts on its own login: it waits on no row.
    std::fs::write(
        world.cfg.join("aterm.toml"),
        "[harness]\naccounts = \"off\"\n",
    )
    .expect("aterm.toml");
    assert_eq!(launch_opening(&world.seat(), Cli::Codex, None), None, "off");
    std::fs::remove_file(world.cfg.join("aterm.toml")).expect("rotation on again");

    // A reading of its usage again (a sign-in by hand in its home) signs it in again.
    let served = Readings(vec![(
        work.clone(),
        Reading::Usage(Usage {
            sample: Sample {
                session_pct: Some(10),
                session_reset: Some(NOW + 4 * HOUR),
                at: NOW,
                ..Sample::default()
            },
            walls: Vec::new(),
        }),
    )]);
    let pick = launch_env(&world.seat(), Cli::Codex, &served).map(|pick| pick.label);
    assert_eq!(pick.as_deref(), Some("work"));
    assert_eq!(condition_of(&world, Cli::Codex, "work"), Condition::Usable);
    assert_eq!(
        launch_opening(&world.seat(), Cli::Codex, None),
        None,
        "a row serves"
    );
}

#[test]
fn a_default_row_whose_login_file_is_gone_is_not_signed_in_here() {
    let world = World::new("default-gone");
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let pick = launch_env(&world.seat(), Cli::Codex, &no_usage).map(|pick| pick.label);
    assert_eq!(pick.as_deref(), Some("main"), "PRECONDITION: main serves");
    std::fs::remove_file(world.home.join(".codex/auth.json")).expect("sign main out");
    let pick = launch_env(&world.seat(), Cli::Codex, &no_usage).map(|pick| pick.value);
    assert_eq!(
        pick,
        Some(Some(work.join("auth.json").display().to_string())),
        "the active default row with no login file is passed over"
    );
    let roster = world.seat().roster(Cli::Codex).expect("roster");
    assert_eq!(
        accounts::enrolled_in(Cli::Codex, &roster, &world.home),
        ["work".to_string()].into_iter().collect()
    );
    assert!(!accounts::default_login_made(Cli::Codex, &world.home));
}

#[test]
fn a_cold_relaunch_leaves_a_row_that_no_longer_serves_and_keeps_one_that_does() {
    let world = World::new("cold");
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let work_auth = work.join("auth.json").display().to_string();
    let on_work = [("CODEX_AUTH_PATH", work_auth.as_str())];
    // `work` serves: the relaunch stays on it.
    assert_eq!(
        cold_launch_env(&world.seat(), Cli::Codex, &on_work, &no_usage, None),
        None
    );
    // `work` is at its limit: the relaunch starts on `main`, the default login.
    wall(&world, Cli::Codex, "work", NOW + HOUR);
    let moved = cold_launch_env(&world.seat(), Cli::Codex, &on_work, &no_usage, None)
        .map(|pick| (pick.label, pick.value));
    assert_eq!(moved, Some(("main".to_string(), None)));
    // CONTROL: a conversation on a home of its own is left alone, as is one whose pool has
    // no row serving.
    assert_eq!(
        cold_launch_env(
            &world.seat(),
            Cli::Codex,
            &[("CODEX_HOME", "/scratch/codex")],
            &no_usage,
            None
        ),
        None
    );
    wall(&world, Cli::Codex, "main", NOW + HOUR);
    assert_eq!(
        cold_launch_env(&world.seat(), Cli::Codex, &on_work, &no_usage, None),
        None
    );
}

/// ON A CODEX PROGRAM THAT KEEPS NO ROW'S LOGIN ITS OWN, A REFUSED LAUNCH WAITS FOR THE DEFAULT
/// ROW'S WALL, NOT FOR ANOTHER ROW'S: with `work` usable, and with `work` walled to an earlier
/// reset, the opening is the instant the refusal names, `main`'s. Under the owner's pin on
/// `work`, which that build refuses whatever the wait, nothing opens by waiting, nor with `main`
/// signed out. CONTROL: a program that keeps the row's login waits on no row while `work`
/// serves, and on `work`'s earlier reset once it is walled.
#[test]
fn a_refused_launch_on_a_program_that_ignores_codex_auth_path_waits_on_the_default_row_alone() {
    let world = World::new("opening-unread");
    world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let old = crate::harness::codex_alab::lay_package(&world.root.join("store/old"), false);
    let new = crate::harness::codex_alab::lay_package(&world.root.join("store/new"), true);
    let opening = |program: &Path| launch_opening(&world.seat(), Cli::Codex, Some(program));
    wall(&world, Cli::Codex, "main", NOW + 2 * HOUR);
    let Condition::Limited { until: main_until } = condition_of(&world, Cli::Codex, "main") else {
        panic!("`main` is at its limit");
    };
    let refused = super::launch_env_for(&world.seat(), Cli::Codex, &no_usage, Some(&old))
        .expect_err("that build runs no row but `main`");
    let named = crate::harness::usage::rfc3339_utc(main_until);
    assert!(refused.contains(&format!("until {named}")), "{refused}");
    assert_eq!(opening(&old), Some(main_until));
    assert_eq!(opening(&new), None, "CONTROL: `work` serves");

    wall(&world, Cli::Codex, "work", NOW + HOUR);
    let Condition::Limited { until: work_until } = condition_of(&world, Cli::Codex, "work") else {
        panic!("`work` is at its limit");
    };
    assert!(work_until < main_until, "{work_until} {main_until}");
    assert_eq!(
        opening(&old),
        Some(main_until),
        "never `work`'s earlier reset"
    );
    assert_eq!(
        opening(&new),
        Some(work_until),
        "CONTROL: the first row to open"
    );

    pin(&world, Cli::Codex, "work").expect("the owner pins `work`");
    assert!(
        super::launch_env_for(&world.seat(), Cli::Codex, &no_usage, Some(&old)).is_err(),
        "a pin on a row of its own does not run on that build"
    );
    assert_eq!(opening(&old), None, "the pin's refusal ends by no wait");
    let mut note = None;
    world
        .seat()
        .store(Cli::Codex)
        .transact_within(&accounts::LOCK_LADDER, NOW, &mut note, |state| {
            state.pinned = false;
            Ok(accounts::note_auth(
                state,
                "main",
                accounts::Auth::SignedOut,
                NOW,
            ))
        })
        .expect("unpin, and sign `main` out");
    assert_eq!(opening(&old), None, "a signed-out `main` opens by no wait");
}

/// ON SUCH A PROGRAM A COLD RELAUNCH MOVES A CONVERSATION ONLY ONTO THIS MACHINE'S OWN LOGIN:
/// one that ran on the default login while `main` is walled stays on it, since that build runs
/// no other row (its relaunch then needs no row of its own's login), and one on `work` at its
/// limit moves onto `main`. CONTROL: a program that keeps `CODEX_AUTH_PATH`'s login moves the
/// first onto `work`.
#[test]
fn a_cold_relaunch_on_a_program_that_ignores_codex_auth_path_moves_only_onto_the_default_row() {
    let world = World::new("cold-unread");
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let old = crate::harness::codex_alab::lay_package(&world.root.join("store/old"), false);
    let new = crate::harness::codex_alab::lay_package(&world.root.join("store/new"), true);
    let work_auth = work.join("auth.json").display().to_string();
    wall(&world, Cli::Codex, "main", NOW + HOUR);
    let on_main = |program: &Path| {
        cold_launch_env(&world.seat(), Cli::Codex, &[], &no_usage, Some(program))
            .map(|pick| (pick.label, pick.value))
    };
    assert_eq!(on_main(&old), None, "it stays on the default login");
    assert_eq!(
        on_main(&new),
        Some(("work".to_string(), Some(work_auth.clone()))),
        "CONTROL"
    );

    let world = World::new("cold-unread-work");
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let work_auth = work.join("auth.json").display().to_string();
    wall(&world, Cli::Codex, "work", NOW + HOUR);
    let moved = cold_launch_env(
        &world.seat(),
        Cli::Codex,
        &[("CODEX_AUTH_PATH", work_auth.as_str())],
        &no_usage,
        Some(&old),
    )
    .map(|pick| (pick.label, pick.value));
    assert_eq!(moved, Some(("main".to_string(), None)));
}

/// ON SUCH A PROGRAM A DEFAULT ROW A SIGN-IN ALREADY MOVED ONTO A LOGIN OF ITS OWN IS NAMED SO,
/// never as a roster with no row on this machine's own login whose fix is `adopt-default`
/// (which refuses while the roster file's row names no `dir`), and the moved row is refused
/// like any row of its own there. The refusal names rotation off, under which that launch takes
/// no row and runs on this machine's own login, and what else that switch turns off, which holds:
/// the accounts' sync and aterm's own sign-ins of every pool. CONTROL: a roster that never listed
/// that login is offered `adopt-default`.
#[test]
fn a_default_row_a_sign_in_moved_is_named_as_moved_on_a_program_that_ignores_codex_auth_path() {
    let world = World::new("moved-default");
    world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let old = crate::harness::codex_alab::lay_package(&world.root.join("store/old"), false);
    let staging = world.root.join("logins/codex/main+1");
    std::fs::create_dir_all(&staging).expect("staging");
    std::fs::write(staging.join("auth.json"), "{}").expect("staged login");
    let mut note = None;
    world
        .seat()
        .store(Cli::Codex)
        .transact_logins(NOW, &mut note, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                "main",
                None,
                &staging.display().to_string(),
                None,
                NOW,
            ))
        })
        .expect("a sign-in binds `main` to its own login");
    let refused = super::launch_env_for(&world.seat(), Cli::Codex, &no_usage, Some(&old))
        .expect_err("no row is on the default login now");
    assert!(
        refused.contains(
            "`main`, the row that login was listed as, was signed in to a login of its own"
        ),
        "{refused}"
    );
    assert!(!refused.contains("adopt-default"), "{refused}");
    let switch = refused
        .split_once("`[harness] accounts = \"off\"`")
        .map(|(_, after)| after)
        .expect("the refusal names rotation off's switch");
    assert!(
        switch.contains("sync") && switch.contains("sign-ins"),
        "it says the switch also stops the sync and aterm's own sign-ins: {refused}"
    );
    // The way back it names: with rotation off and no pin, the launch takes no row, so Codex
    // runs on this machine's own login; and that switch does what the words say besides.
    let config = world.cfg.join("aterm.toml");
    std::fs::write(&config, "[harness]\naccounts = \"off\"\n").expect("aterm.toml");
    assert_eq!(
        super::launch_env_for(&world.seat(), Cli::Codex, &no_usage, Some(&old)),
        Ok(None),
        "rotation off runs the launch on the default login"
    );
    assert!(crate::harness::accounts_sync::accounts_sync_off(Some(
        &config
    )));
    assert!(crate::harness::accounts_sync::sign_ins_off(&config).is_some());

    let world = World::new("unlisted-default");
    let work = world.work_dir(Cli::Codex);
    std::fs::write(
        world.cfg.join(Cli::Codex.accounts().roster_file),
        format!(
            "[[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = \"{}\"\n",
            work.display()
        ),
    )
    .expect("roster");
    let refused = super::launch_env_for(&world.seat(), Cli::Codex, &no_usage, Some(&old))
        .expect_err("CONTROL: no row is on the default login");
    assert!(refused.contains("adopt-default"), "{refused}");
}

#[test]
fn accounts_off_moves_no_cold_relaunch_onto_the_owners_pin() {
    let world = World::new("cold-off");
    let program = crate::harness::codex_alab::lay_package(&world.root.join("codex"), true);
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let work_auth = work.join("auth.json").display().to_string();
    let on_work = [("CODEX_AUTH_PATH", work_auth.as_str())];
    pin(&world, Cli::Codex, "main").expect("pin");
    wall(&world, Cli::Codex, "work", NOW + HOUR);
    // PRECONDITION: with rotation on, the limited row's tab starts on the pinned one.
    let moved = cold_launch_env(&world.seat(), Cli::Codex, &on_work, &no_usage, None)
        .map(|pick| pick.label);
    assert_eq!(moved.as_deref(), Some("main"));
    std::fs::write(
        world.cfg.join("aterm.toml"),
        "[harness]\naccounts = \"off\"\n",
    )
    .expect("aterm.toml");
    assert_eq!(world.seat().mode(), Mode::Off);
    assert_eq!(
        cold_launch_env(&world.seat(), Cli::Codex, &on_work, &no_usage, None),
        None,
        "off moves no session, a cold one included"
    );
    assert_eq!(
        cold_resume_env(&world.seat(), Cli::Codex, &on_work, &program).unwrap(),
        None,
        "off moves no held resume line onto the owner's pin"
    );
}

#[test]
fn cold_resume_env_routes_off_recorded_unusable_rows_without_locking_or_writing() {
    let world = World::new("cold-resume");
    let work = world.two(Cli::Codex);
    world.on_main(Cli::Codex, &[]);
    let work_auth = work.join("auth.json").display().to_string();
    let on_work = [("CODEX_AUTH_PATH", work_auth.as_str())];
    let program = crate::harness::codex_alab::lay_package(&world.root.join("codex"), true);

    // While `work` still serves, its resume line stays on `work`.
    assert_eq!(
        cold_resume_env(&world.seat(), Cli::Codex, &on_work, &program).unwrap(),
        None
    );

    // Once `work` is recorded limited, `cold_resume_env` routes onto `main` (`value: None`)
    // even while another caller holds the store's writer lock, and writes nothing to `logins.json`.
    wall(&world, Cli::Codex, "work", NOW + HOUR);
    let logins_before = world.seat().store(Cli::Codex).load_logins(NOW).unwrap().0;
    let held = world
        .seat()
        .store(Cli::Codex)
        .acquire_within(&[])
        .expect("lock")
        .expect("acquired");
    assert_eq!(
        cold_resume_env(&world.seat(), Cli::Codex, &on_work, &program).unwrap(),
        Some(LoginEnv {
            label: "main".to_string(),
            var: "CODEX_AUTH_PATH",
            value: None,
        }),
        "lock-free main-thread read routes off limited `work` onto `main`"
    );
    drop(held);
    let logins_after = world.seat().store(Cli::Codex).load_logins(NOW).unwrap().0;
    assert_eq!(
        logins_after, logins_before,
        "offering a resume line never mutates served_at or bindings"
    );

    // Reverse direction: a held tab on `main` (default login, empty env) whose `~/.codex/auth.json`
    // was removed (`NotEnrolled`) routes onto `work` once `work`'s wall has passed.
    std::fs::remove_file(world.home.join(".codex/auth.json")).expect("remove default login");
    let later = Seat {
        now: NOW + 2 * HOUR,
        ..world.seat()
    };
    assert_eq!(
        cold_resume_env(&later, Cli::Codex, &[], &program).unwrap(),
        Some(LoginEnv {
            label: "work".to_string(),
            var: "CODEX_AUTH_PATH",
            value: Some(work_auth.clone()),
        }),
        "held default-row tab whose login file is gone routes onto serving `work`"
    );

    // Negative control: a conversation on a custom `CODEX_HOME` of its own is never moved.
    assert_eq!(
        cold_resume_env(
            &world.seat(),
            Cli::Codex,
            &[
                ("CODEX_HOME", "/scratch/codex"),
                ("CODEX_AUTH_PATH", work_auth.as_str()),
            ],
            &program,
        )
        .unwrap(),
        None
    );
}

/// A CODEX DEFAULT ROW THIS MAC CANNOT CLAIM SAYS WHY IN WORDS: where this Mac cannot record its
/// claims and is not the row's home ([`accounts::WhyNot::Unclaimed`]), [`default_serving`]'s
/// reason, which `aterm ai login codex` and a refused launch quote, says so, never the slug
/// `unclaimed`. CONTROL: on the row's home Mac it serves.
#[test]
fn an_unclaimed_codex_default_row_says_its_claims_cannot_be_recorded() {
    let roster = Roster {
        rows: vec![Row {
            label: "main".to_string(),
            ..Row::default()
        }],
    };
    let mut state = State {
        unrecorded: true,
        ..State::default()
    };
    let enrolled = std::collections::BTreeSet::from(["main".to_string()]);
    match default_serving(&roster, &state, &enrolled, NOW) {
        Err(crate::harness::codex_alab::DefaultRow::Other { label, why }) => {
            assert_eq!(label, "main");
            assert!(why.contains("cannot record its claims"), "{why}");
            assert!(!why.contains(accounts::WhyNot::Unclaimed.as_str()), "{why}");
        }
        other => panic!("{other:?}"),
    }
    state.home_accounts.insert("main".to_string());
    assert!(
        default_serving(&roster, &state, &enrolled, NOW).is_ok(),
        "CONTROL"
    );
}
