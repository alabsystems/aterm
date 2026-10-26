// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

use std::cell::RefCell;
use std::path::PathBuf;

use aterm_phase::Program;

use super::super::{
    Admitted, Dialect, Fresh, Readiness, Refused, Starter, WORKERS, admit, ensure_program,
    starter_of, worker_cli,
};

/// [`super::super::line`] for `starter` taken as admitted: these tests check the line's form.
fn line(
    cli: &super::super::WorkerCli,
    dialect: Dialect,
    starter: &Starter,
    fresh: &Fresh<'_>,
) -> Result<String, String> {
    super::super::line(cli, dialect, &Admitted::assumed(starter.clone()), fresh)
}

fn words(argv: &[&str]) -> Vec<String> {
    argv.iter().map(|a| (*a).to_string()).collect()
}

fn grok() -> &'static super::super::WorkerCli {
    worker_cli(Program::Grok).expect("a Grok Build worker row")
}

/// A GROK BUILD STARTER LENDS ITS WORKER ONLY ITS LEADER SOCKET (how its sessions reach xAI),
/// spelled either way, with its value; its model, posture, session, folders and prompt stay its
/// own, and an option value that looks like a flag or a subcommand word is read as the value.
/// A starter running a subcommand or a one-shot prompt lends nothing; an option 1.0.70's
/// `--help` does not list, or a switch given a value, is refused as typed.
#[test]
fn a_grok_starter_lends_its_worker_only_its_leader_socket() {
    let argv = words(&[
        "/p/agents/grok",
        "--model",
        "--leader-socket",
        "--always-approve",
        "--leader-socket",
        "/Users//u/.grok/leader-work-0123abcd.sock",
        "--session-id",
        "0b5c6c33-1f7e-4a59-9d2a-6f1e2d3c4b5a",
        "--resume",
        "--sandbox=strict",
        "--cwd",
        "update",
        "the first prompt",
        "--",
        "--leader-socket",
        "/not/an/option",
    ]);
    assert_eq!(
        config_flags(&argv),
        Ok(words(&[
            "--leader-socket",
            "/Users//u/.grok/leader-work-0123abcd.sock"
        ]))
    );
    assert_eq!(
        config_flags(&words(&[
            "grok",
            "--leader-socket=/s/leader-a.sock",
            "-m",
            "x"
        ])),
        Ok(words(&["--leader-socket=/s/leader-a.sock"]))
    );
    for lends_nothing in [
        &["grok", "--leader-socket", "/s", "sessions", "list"][..],
        &["grok", "--leader-socket", "/s", "-p", "hi"],
        &["grok", "--leader-socket", "/s", "--prompt-file", "/t"],
        &["grok", "--leader-socket", "/s", "--version"],
        &["grok", "a prompt", "update"],
    ] {
        assert_eq!(
            config_flags(&words(lends_nothing)),
            Ok(Vec::new()),
            "{lends_nothing:?}"
        );
    }
    assert_eq!(
        config_flags(&words(&["grok", "--brand-new", "--leader-socket", "/s"])),
        Err("--brand-new".to_string())
    );
    assert_eq!(
        config_flags(&words(&["grok", "--always-approve=yes"])),
        Err("--always-approve=yes".to_string())
    );
    assert_eq!(
        config_flags(&words(&["grok", "-mx"])),
        Err("-mx".to_string()),
        "a short option is never read attached"
    );
    assert_eq!(config_flags(&words(&["grok"])), Ok(Vec::new()));
}

/// A GROK BUILD STARTER'S HOMES RIDE ITS WORKER: its own `GROK_HOME` and pooled
/// `GROK_AUTH_PATH`; with no `GROK_HOME`, another `HOME`'s `.grok`; nothing for the user's own
/// home or a relative one; never the value of `GROK_AUTH` (a credential is no line's).
#[test]
fn a_grok_starters_homes_ride_and_no_credential_does() {
    fn env<'a>(
        pairs: &'a [(&'static str, &'static str)],
    ) -> impl Fn(&str) -> Option<&'static str> + 'a {
        move |name| pairs.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
    }
    let user = Path::new("/Users//u");
    let both = [
        ("GROK_HOME", "/g"),
        ("GROK_AUTH_PATH", "/state/accounts-grok/work/auth.json"),
        ("GROK_AUTH", "{\"secret\":1}"),
        ("HOME", "/Users//other"),
    ];
    assert_eq!(
        relaunch_env(env(&both), user),
        [
            ("GROK_HOME".to_string(), "/g".to_string()),
            (
                "GROK_AUTH_PATH".to_string(),
                "/state/accounts-grok/work/auth.json".to_string()
            ),
        ]
    );
    let other = Path::new("/Users//other").join(DEFAULT_HOME);
    assert_eq!(
        relaunch_env(env(&[("HOME", "/Users//other"), ("GROK_HOME", "")]), user),
        [(
            "GROK_HOME".to_string(),
            other.to_str().expect("UTF-8").to_string()
        )]
    );
    assert!(relaunch_env(env(&[("HOME", "/Users//u")]), user).is_empty());
    assert!(relaunch_env(env(&[("HOME", "rel")]), user).is_empty());
    let args = atpkg::caller_shell::ProcArgs {
        exec_path: "/usr/local/bin/grok".to_string(),
        argv: words(&["grok"]),
        env: vec!["GROK_HOME=rel/home".to_string()],
    };
    let s = starter_of(
        grok(),
        Some(Path::new("/usr/local/bin/grok")),
        &args,
        user,
        || None,
    )
    .expect("a Grok Build starter");
    let err = line(grok(), Dialect::Zsh, &s, &Fresh::default()).expect_err("relative refused");
    assert!(err.contains("GROK_HOME"), "{err}");
}

/// A STORE BUILD OF GROK BUILD is `…/store/grok/<build>/bin/grok`, and nothing else is.
#[test]
fn a_grok_store_build_is_named_by_the_store() {
    assert!(store_build(Path::new(
        "/p/store/grok/1000070000000000001/bin/grok"
    )));
    for not in [
        "/usr/local/bin/grok",
        "/p/store/muse/1/bin/grok",
        "/p/store/grok/1/bin/muse",
        "/p/store/grok//bin/grok",
        "/p/agents/grok",
    ] {
        assert!(!store_build(Path::new(not)), "{not}");
    }
}

/// NO GROK BUILD WORKER STARTS WITHOUT A SIGN-IN FILE: signed out, Grok Build opens xAI's sign-in
/// page in the browser by itself. The file read is the one the line would start on: a pooled
/// `GROK_AUTH_PATH` first, else `<GROK_HOME or ~/.grok>/auth.json`. A missing, empty, refused or
/// unnameable file refuses. A non-empty one grok has not refused passes, whether or not its
/// login has expired: this is a floor (D40). A row with no such hazard (Claude Code) never
/// refuses on this.
#[test]
fn no_grok_worker_starts_without_a_sign_in_file() {
    let d = aterm_tempfile::Builder::new()
        .prefix("aterm-grok-signin")
        .tempdir()
        .expect("scratch dir");
    let user = d.path().join("home");
    let default_home = user.join(DEFAULT_HOME);
    std::fs::create_dir_all(&default_home).expect("~/.grok");
    let starter = |env: &[(&str, &Path)]| Starter {
        exe: PathBuf::from(PROGRAM),
        env: env
            .iter()
            .map(|(n, v)| ((*n).to_string(), v.to_str().expect("UTF-8").to_string()))
            .collect(),
        args: Ok(Vec::new()),
        unset: Vec::new(),
    };
    let row = grok();
    let refused = row
        .signed_out(&starter(&[]), Some(&user))
        .expect("no auth.json: refused");
    assert!(
        refused.contains(&default_home.join(AUTH_FILE).display().to_string()),
        "{refused}"
    );
    assert!(refused.contains("browser"), "{refused}");
    std::fs::write(default_home.join(AUTH_FILE), "").expect("an empty auth.json");
    assert!(
        row.signed_out(&starter(&[]), Some(&user)).is_some(),
        "empty"
    );
    std::fs::write(default_home.join(AUTH_FILE), "{\"token\":\"t\"}").expect("a sign-in");
    assert_eq!(row.signed_out(&starter(&[]), Some(&user)), None);
    // A login grok refused (aterm's `auth.json.refused`, at least as new) is signed out until a
    // newer login replaces it: the gate's own reading (`grok_signin::login_accepted`).
    let auth = default_home.join(AUTH_FILE);
    let marker = crate::harness::grok_signin::refused_marker(&auth);
    std::fs::write(&marker, "").expect("a refusal marker");
    let marked = std::fs::metadata(&marker)
        .and_then(|m| m.modified())
        .expect("marker time");
    let set = |at: std::time::SystemTime| {
        std::fs::File::options()
            .write(true)
            .open(&auth)
            .and_then(|f| f.set_modified(at))
            .expect("login time");
    };
    set(marked - std::time::Duration::from_secs(5));
    let refused = row
        .signed_out(&starter(&[]), Some(&user))
        .expect("a refused login: refused");
    assert!(refused.contains("refused"), "{refused}");
    set(marked + std::time::Duration::from_secs(5));
    assert_eq!(
        row.signed_out(&starter(&[]), Some(&user)),
        None,
        "a newer login"
    );
    std::fs::remove_file(&marker).expect("marker gone");
    let other = d.path().join("other-grok");
    std::fs::create_dir_all(&other).expect("another home");
    assert!(
        row.signed_out(&starter(&[(HOME_ENV, &other)]), Some(&user))
            .is_some(),
        "the carried GROK_HOME is the one read"
    );
    let pooled = d.path().join("accounts-grok-work.json");
    assert!(
        row.signed_out(&starter(&[(AUTH_PATH_ENV, &pooled)]), Some(&user))
            .is_some(),
        "the pooled login is read ahead of the home's"
    );
    std::fs::write(&pooled, "{\"token\":\"p\"}").expect("a pooled sign-in");
    assert_eq!(
        row.signed_out(
            &starter(&[(HOME_ENV, &other), (AUTH_PATH_ENV, &pooled)]),
            Some(&user)
        ),
        None
    );
    assert!(
        row.signed_out(&starter(&[]), None).is_some(),
        "no home known: refused"
    );
    assert!(
        row.signed_out(&starter(&[(HOME_ENV, Path::new("rel"))]), Some(&user))
            .is_some(),
        "a relative home names no file"
    );
    for cli in WORKERS.iter().filter(|c| c.agent != Program::Grok) {
        assert_eq!(
            cli.signed_out(&starter(&[]), None),
            None,
            "{:?}: no sign-in hazard of its own",
            cli.agent
        );
    }
}

/// GROK BUILD'S WORKER LINE: its model, the task after `--` as its `[PROMPT]`, and its own
/// updater off for the run; it approves nothing on its own (no `--always-approve`).
#[test]
fn a_grok_workers_line_keeps_its_updater_off() {
    let typed = line(
        grok(),
        Dialect::Zsh,
        &grok().plain(),
        &Fresh {
            model: Some("grok-code-fast-1"),
            ..Fresh::default()
        },
    )
    .expect("a line");
    assert!(typed.contains("GROK_DISABLE_AUTOUPDATER=1"), "{typed}");
    assert!(
        typed.contains("'grok' '--model' 'grok-code-fast-1' -- "),
        "{typed}"
    );
    assert!(
        typed.ends_with(super::super::upgrade::TASK_OPERAND),
        "{typed}"
    );
    assert!(!typed.contains("--always-approve"), "{typed}");
}

/// NO WORKER IS STARTED WHOSE END NOTHING WOULD SEE: Grok Build's reader reads no turn end yet
/// (`Program::supervisable` is false), so its row says why; every supervised agent's row, and
/// codex-alab's (which carries itself, though aterm's policy acts on no Codex screen), says
/// nothing.
#[test]
fn only_a_worker_whose_end_nothing_sees_is_refused() {
    for cli in &WORKERS {
        let unseen = cli.end_unseen();
        let expected = !cli.agent.supervisable() && cli.agent != Program::Codex;
        assert_eq!(unseen.is_some(), expected, "{:?}: {unseen:?}", cli.agent);
        if let Some(why) = unseen {
            assert!(why.contains(cli.agent.product()), "{why}");
        }
    }
    assert!(
        !Program::Codex.supervisable(),
        "PRECONDITION: codex-alab is let through by its own claim, not by aterm's policy"
    );
}

/// A GROK BUILD WORKER IS REFUSED TODAY, WHATEVER FLIPS. The refusal is all that keeps Grok
/// workers from starting, and it lifts by itself when `Program::supervisable(Grok)` turns true.
/// The sign-in floor behind `WorkerCli::signed_out` then stands alone, and it passes an expired
/// login grok has not refused yet. So this test does not follow `supervisable`: the change that
/// flips it rewrites this test on purpose (`docs/DESIGN-harness-repos-2026-10-24.md` D40).
#[test]
fn a_grok_worker_stays_refused_whatever_flips() {
    let why = grok().end_unseen().expect(
        "a Grok Build worker is refused: lifting this is a deliberate change that reads \
         Grok Build's turn end and keeps WorkerCli::signed_out on the sign-in gate (D40)",
    );
    assert!(why.contains("Grok Build"), "{why}");
    assert!(why.contains("await task"), "{why}");
}

/// ONLY A ROW THE DEFAULT SET DOES NOT KEEP IS INSTALLED ON DEMAND: Grok Build's; the rows the
/// default set keeps (Claude Code, Codex, agy, Muse Code) are its update pass's to install.
#[test]
fn only_a_row_outside_the_default_set_installs_on_demand() {
    for cli in &WORKERS {
        let want = atpkg::vendor_direct::spec(cli.word).is_some_and(|spec| !spec.default_set);
        assert_eq!(cli.on_demand().is_some(), want, "{:?}", cli.agent);
    }
    assert_eq!(grok().on_demand(), Some(PROGRAM));
    assert_eq!(
        worker_cli(Program::Claude).and_then(super::super::WorkerCli::on_demand),
        None
    );
}

/// What a fake installer was asked, and what it does.
struct FakeInstaller {
    asked: RefCell<Vec<String>>,
    lays: bool,
    fails: Option<&'static str>,
    installed: RefCell<bool>,
}

impl FakeInstaller {
    fn new(lays: bool, fails: Option<&'static str>) -> Self {
        Self {
            asked: RefCell::new(Vec::new()),
            lays,
            fails,
            installed: RefCell::new(false),
        }
    }

    fn install(&self, program: &str) -> Result<(), String> {
        self.asked.borrow_mut().push(program.to_string());
        if let Some(why) = self.fails {
            return Err(why.to_string());
        }
        if self.lays {
            *self.installed.borrow_mut() = true;
        }
        Ok(())
    }
}

/// `spawn agent=grok` INSTALLS GROK BUILD ON DEMAND, BEFORE ITS LINE: absent, atpkg's install
/// runs once for `grok` and the worker goes on once it is there; present, nothing installs; a
/// person's hold (`[packages] exclude`, a removal) installs nothing; a failed install, or one
/// that leaves no `grok`, starts no worker and says why. A row the default set keeps never asks.
#[test]
fn spawn_agent_grok_installs_grok_on_demand_before_its_line() {
    let fresh = FakeInstaller::new(true, None);
    assert_eq!(
        ensure_program(
            grok(),
            |p| {
                assert_eq!(p, "grok");
                *fresh.installed.borrow()
            },
            |_| None,
            |p| fresh.install(p),
        ),
        Ok(Readiness::Installed)
    );
    assert_eq!(*fresh.asked.borrow(), ["grok"], "installed once, by name");

    let there = FakeInstaller::new(true, None);
    *there.installed.borrow_mut() = true;
    assert_eq!(
        ensure_program(
            grok(),
            |_| *there.installed.borrow(),
            |_| panic!("a present program is never held"),
            |p| there.install(p),
        ),
        Ok(Readiness::Present)
    );
    assert!(there.asked.borrow().is_empty(), "nothing installed");

    let held = FakeInstaller::new(true, None);
    let err = ensure_program(
        grok(),
        |_| false,
        |_| Some("`[packages] exclude` in aterm.toml names it".to_string()),
        |p| held.install(p),
    )
    .expect_err("held back");
    assert!(err.contains("exclude"), "{err}");
    assert!(held.asked.borrow().is_empty(), "a person's hold stands");

    let failing = FakeInstaller::new(true, Some("vendor unreachable: x.ai"));
    let err = ensure_program(grok(), |_| false, |_| None, |p| failing.install(p))
        .expect_err("a failed install");
    assert!(err.contains("vendor unreachable: x.ai"), "{err}");

    let empty = FakeInstaller::new(false, None);
    let err = ensure_program(
        grok(),
        |_| *empty.installed.borrow(),
        |_| None,
        |p| empty.install(p),
    )
    .expect_err("nothing laid");
    assert!(err.contains("left no grok installed"), "{err}");
    assert_eq!(*empty.asked.borrow(), ["grok"]);

    for cli in WORKERS.iter().filter(|c| c.on_demand().is_none()) {
        assert_eq!(
            ensure_program(
                cli,
                |_| panic!("{:?}: looked for", cli.agent),
                |_| panic!("{:?}: held", cli.agent),
                |_| panic!("{:?}: installed", cli.agent),
            ),
            Ok(Readiness::Kept),
            "{:?}",
            cli.agent
        );
    }
}

/// ADMISSION ASKS THE SIGN-IN BEFORE IT INSTALLS ANYTHING. Signed out, `spawn agent=grok` is
/// refused as signed out and the store is never asked: installing would lay `agents/grok` on
/// every shell's `PATH`, where an agent typing `grok` would open xAI's sign-in page. Signed in,
/// an absent `grok` is installed once and the starter is admitted as installed; a present one
/// is admitted as present. A row with no sign-in hazard and no on-demand program (Claude Code)
/// is admitted without asking the store at all.
#[test]
fn admission_asks_the_sign_in_before_it_installs_anything() {
    let d = aterm_tempfile::Builder::new()
        .prefix("aterm-grok-admit")
        .tempdir()
        .expect("scratch dir");
    let user = d.path().join("home");
    let default_home = user.join(DEFAULT_HOME);
    std::fs::create_dir_all(&default_home).expect("~/.grok");
    let asked = RefCell::new(Vec::<String>::new());
    let ask = |what: &str| asked.borrow_mut().push(what.to_string());

    let refused = admit(
        grok(),
        grok().plain(),
        Some(&user),
        |_| {
            ask("present");
            false
        },
        |_| {
            ask("held");
            None
        },
        |_| {
            ask("install");
            Ok(())
        },
    )
    .expect_err("signed out: refused");
    assert!(
        matches!(&refused, Refused::SignedOut(why) if why.contains("auth.json")),
        "{refused:?}"
    );
    assert!(
        asked.borrow().is_empty(),
        "nothing installed for a signed-out worker: {:?}",
        asked.borrow()
    );

    std::fs::write(default_home.join(AUTH_FILE), "{\"token\":\"t\"}").expect("a sign-in");
    let installed = RefCell::new(false);
    let admitted = admit(
        grok(),
        grok().plain(),
        Some(&user),
        |_| {
            ask("present");
            *installed.borrow()
        },
        |_| {
            ask("held");
            None
        },
        |p| {
            ask(&format!("install {p}"));
            *installed.borrow_mut() = true;
            Ok(())
        },
    )
    .expect("signed in: admitted");
    assert_eq!(admitted.readiness(), Readiness::Installed);
    assert_eq!(admitted.starter(), &grok().plain());
    assert_eq!(
        *asked.borrow(),
        ["present", "held", "install grok", "present"],
        "installed once, after the sign-in"
    );
    asked.borrow_mut().clear();

    let present = admit(
        grok(),
        grok().plain(),
        Some(&user),
        |_| true,
        |_| panic!("a present program is never held"),
        |_| panic!("a present program is never installed"),
    )
    .expect("present");
    assert_eq!(present.readiness(), Readiness::Present);

    let failing = admit(
        grok(),
        grok().plain(),
        Some(&user),
        |_| false,
        |_| None,
        |_| Err("vendor unreachable: x.ai".to_string()),
    )
    .expect_err("a failed install");
    assert!(
        matches!(&failing, Refused::Program(why) if why.contains("vendor unreachable")),
        "{failing:?}"
    );

    let claude = worker_cli(Program::Claude).expect("claude row");
    let kept = admit(
        claude,
        claude.plain(),
        None,
        |_| panic!("looked for"),
        |_| panic!("held"),
        |_| panic!("installed"),
    )
    .expect("a kept program");
    assert_eq!(kept.readiness(), Readiness::Kept);
}
