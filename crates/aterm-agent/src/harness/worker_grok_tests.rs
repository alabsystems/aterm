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
    assert!(
        why.contains("the Mac asked to start it")
            && !why.contains(" here")
            && !why.contains("this Mac"),
        "a Mac that drives another over the fleet reads it too: {why}"
    );
}

/// What a fake atpkg store was asked, in order, and what it does: it holds a build once `lays`
/// ran an install, and `fails` is the words of an install that failed.
struct FakeStore {
    asked: RefCell<Vec<String>>,
    present: RefCell<bool>,
    held: Option<&'static str>,
    lays: bool,
    fails: Option<&'static str>,
}

impl FakeStore {
    fn new(
        present: bool,
        held: Option<&'static str>,
        lays: bool,
        fails: Option<&'static str>,
    ) -> Self {
        Self {
            asked: RefCell::new(Vec::new()),
            present: RefCell::new(present),
            held,
            lays,
            fails,
        }
    }

    fn present(&self, program: &str) -> bool {
        self.asked.borrow_mut().push(format!("present {program}"));
        *self.present.borrow()
    }

    fn held(&self, program: &str) -> Option<String> {
        self.asked.borrow_mut().push(format!("held {program}"));
        self.held.map(str::to_string)
    }

    fn install(&self, program: &str) -> Result<(), String> {
        self.asked.borrow_mut().push(format!("install {program}"));
        if let Some(why) = self.fails {
            return Err(why.to_string());
        }
        if self.lays {
            *self.present.borrow_mut() = true;
        }
        Ok(())
    }

    fn ensure(&self, cli: &super::super::WorkerCli) -> Result<Readiness, String> {
        ensure_program(
            cli,
            |p| self.present(p),
            |p| self.held(p),
            |p| self.install(p),
        )
    }

    fn asked(&self) -> Vec<String> {
        self.asked.borrow().clone()
    }
}

/// A SPAWN INSTALLS ITS PROGRAM WHERE ATPKG'S STORE HOLDS NONE AND NOBODY KEEPS IT OFF, FOR EVERY
/// ROW ATPKG SHIPS: every one is in atpkg's default set, but a spawn can come before its update
/// pass lands it (a fresh Mac) or after its install failed, and the line must not type a program
/// that is not there. Absent, atpkg's install runs once by name and the worker goes on once the
/// build is there. CONTROLS: present, nothing installs; a person's hold (`[packages] exclude`, a
/// removal) installs nothing and is said, carrying the hold's words, and the line runs any copy
/// of the shell's own; a failed install, or one that leaves nothing, starts no worker and
/// carries atpkg's words.
#[test]
fn a_spawn_installs_an_absent_program_unless_a_person_keeps_it_off() {
    for cli in &WORKERS {
        let Some(program) = cli.atpkg_program() else {
            let untouched = FakeStore::new(false, None, true, None);
            assert_eq!(untouched.ensure(cli), Ok(Readiness::Unshipped));
            assert!(untouched.asked().is_empty(), "{:?}", cli.agent);
            continue;
        };
        let absent = FakeStore::new(false, None, true, None);
        assert_eq!(
            absent.ensure(cli),
            Ok(Readiness::Installed),
            "{:?}",
            cli.agent
        );
        assert_eq!(
            absent.asked(),
            [
                format!("present {program}"),
                format!("held {program}"),
                format!("install {program}"),
                format!("present {program}"),
            ],
            "{:?}: installed once, by name",
            cli.agent
        );

        let there = FakeStore::new(true, Some("never asked"), true, None);
        assert_eq!(there.ensure(cli), Ok(Readiness::Present), "{:?}", cli.agent);
        assert_eq!(
            there.asked(),
            [format!("present {program}")],
            "CONTROL: present"
        );

        for hold in [
            "`[packages] exclude` in aterm.toml names it",
            "a person removed it (`aterm pkg install x` brings it back)",
        ] {
            let held = FakeStore::new(false, Some(hold), true, None);
            let readiness = held.ensure(cli).expect("a hold is no refusal");
            let Readiness::KeptOff(said) = &readiness else {
                panic!("{:?}: {readiness:?}", cli.agent);
            };
            assert!(said.contains(hold), "the hold's words are carried: {said}");
            assert_eq!(
                held.asked(),
                [format!("present {program}"), format!("held {program}")],
                "CONTROL: {:?} kept off installs nothing",
                cli.agent
            );
        }

        let failing = FakeStore::new(false, None, true, Some("vendor head unreachable"));
        let err = failing.ensure(cli).expect_err("a failed install");
        assert!(err.contains("vendor head unreachable"), "{err}");

        let empty = FakeStore::new(false, None, false, None);
        assert!(empty.ensure(cli).is_err(), "{:?}: nothing laid", cli.agent);
        assert_eq!(empty.asked().len(), 4, "{:?}", cli.agent);
    }
    assert_eq!(
        grok().atpkg_program(),
        Some(PROGRAM),
        "PRECONDITION: atpkg ships grok"
    );
}

/// ADMISSION ASKS THE SIGN-IN BEFORE IT READS THE STORE OR INSTALLS ANYTHING. Signed out, `spawn
/// agent=grok` is refused as signed out and the store is never asked: installing would lay
/// `agents/grok` on every shell's `PATH` signed out, and its line would open xAI's sign-in page.
/// Signed in, an absent `grok` is installed once and the starter is admitted as installed; a
/// held one is admitted kept off, installing nothing; a present one is admitted as present; a
/// failed install is refused as [`Refused::Program`]. A row with no sign-in hazard (Claude Code)
/// is admitted on the store's word alone.
#[test]
fn admission_asks_the_sign_in_before_it_installs_anything() {
    let d = aterm_tempfile::Builder::new()
        .prefix("aterm-grok-admit")
        .tempdir()
        .expect("scratch dir");
    let user = d.path().join("home");
    let default_home = user.join(DEFAULT_HOME);
    std::fs::create_dir_all(&default_home).expect("~/.grok");
    let admit_on = |cli, store: &FakeStore| {
        admit(
            cli,
            grok().plain(),
            Some(&user),
            |p| store.present(p),
            |p| store.held(p),
            |p| store.install(p),
        )
    };

    let signed_out = FakeStore::new(false, None, true, None);
    let refused = admit_on(grok(), &signed_out).expect_err("signed out: refused");
    assert!(
        matches!(&refused, Refused::SignedOut(why) if why.contains("auth.json")),
        "{refused:?}"
    );
    assert!(
        signed_out.asked().is_empty(),
        "nothing read or installed for a signed-out worker: {:?}",
        signed_out.asked()
    );

    std::fs::write(default_home.join(AUTH_FILE), "{\"token\":\"t\"}").expect("a sign-in");
    let absent = FakeStore::new(false, None, true, None);
    let admitted = admit_on(grok(), &absent).expect("signed in: admitted");
    assert_eq!(admitted.readiness(), &Readiness::Installed);
    assert_eq!(admitted.kept_off(), None);
    assert_eq!(admitted.starter(), &grok().plain());
    assert_eq!(
        absent.asked(),
        ["present grok", "held grok", "install grok", "present grok"],
        "installed once, after the sign-in"
    );

    let held = FakeStore::new(
        false,
        Some("`[packages] exclude` in aterm.toml names it"),
        true,
        None,
    );
    let admitted = admit_on(grok(), &held).expect("held: admitted");
    assert!(
        admitted
            .kept_off()
            .is_some_and(|said| said.contains("`[packages] exclude` in aterm.toml names it")),
        "{admitted:?}"
    );
    assert_eq!(
        held.asked(),
        ["present grok", "held grok"],
        "nothing installed"
    );

    let there = FakeStore::new(true, None, true, None);
    let present = admit_on(grok(), &there).expect("present");
    assert_eq!(present.readiness(), &Readiness::Present);

    let failing = FakeStore::new(false, None, true, Some("vendor unreachable: x.ai"));
    let refused = admit_on(grok(), &failing).expect_err("a failed install");
    assert!(
        matches!(&refused, Refused::Program(why) if why.contains("vendor unreachable: x.ai")),
        "{refused:?}"
    );

    let claude = worker_cli(Program::Claude).expect("claude row");
    let kept = admit(
        claude,
        claude.plain(),
        None,
        |_| true,
        |_| panic!("held"),
        |_| panic!("installed"),
    )
    .expect("an installed program");
    assert_eq!(kept.readiness(), &Readiness::Present);
}
