// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn observe_pool_session(model: &aterm_spec::interp::State) -> [i64; 4] {
    let a = Accts::new();
    let cli = accounts::Cli::Codex;
    let path = a.roster_path().with_file_name(cli.accounts().roster_file);
    std::fs::rename(a.roster_path(), &path).unwrap();
    let login = a.login_dir("work-2");
    std::fs::write(
        Path::new(&login).join("auth.json"),
        r#"{"tokens":{"access_token":"opaque"}}"#,
    )
    .unwrap();
    let sid = "s-pool-session";
    let mut snap = a.spawn(sid, ACCT_TALK_A, "work-2");
    snap.program = "/opt/bin/codex".into();
    snap.argv = vec!["/opt/bin/codex".into()];
    a.tabs.lock().unwrap().insert(sid.into(), snap.clone());
    a.procs
        .lock()
        .unwrap()
        .insert(snap.pid, vec![format!("CODEX_AUTH_PATH={login}/auth.json")]);
    let mut idle = a.worker(sid, &on());
    idle.agent = Program::Codex;
    let seat = a.seat();
    let pool_seat = accounts_pool::Seat {
        config_dir: path.parent().unwrap(),
        state: &seat.state,
        home: &seat.home,
        now: seat.now,
    };
    let roster = pool_seat.bound_roster(cli).unwrap();
    let (look, env) = idle.pool_account_look(cli, &roster, &seat.home).unwrap();
    assert!(
        idle.lost_seen(&look, AccountPoint::TurnEnd, seat.now)
            .is_some()
    );
    if model["present"] == 1 {
        let mut current = snap.clone();
        match model["process"] {
            2 => current.pid += 1,
            3 => current.start.push_str(" replaced"),
            4 => current.argv.push("--new-auth".into()),
            _ => {}
        }
        if model["login"] == 2 {
            current.login = Some("/new/login".into());
        }
        if model["metadata"] == 1 {
            current.cwd = "/new/directory".into();
            current.session = Some("new-metadata".into());
            current.version = Some("new-version".into());
        }
        a.tabs.lock().unwrap().insert(sid.into(), current);
        assert!(refresh_snapshot(sid, &idle.kept, &idle.hooks));
    } else {
        idle.kept.lock().unwrap().snapshot = None;
    }
    if model["generation"] == 2 {
        idle.kept.lock().unwrap().account.landing = Arc::default();
    }
    let store = pool_seat.store(cli);
    store
        .transact(seat.now, &mut None, |state| {
            state.active = Some("work-2".into());
            state.proven = false;
            state.canary = Some(accounts::Canary {
                sid: sid.into(),
                at: seat.now,
                took: false,
                native_receipt: None,
            });
            if model["present"] == 1 {
                accounts::note_session(state, sid, "pool-a", Some("newer"), seat.now);
            }
            Ok(Vec::new())
        })
        .unwrap();
    let before = store.load(seat.now).unwrap().0.sessions.get(sid).cloned();
    let pairs = env
        .iter()
        .filter_map(|entry| entry.split_once('='))
        .collect::<Vec<_>>();
    let applied = std::cell::Cell::new(false);
    let _ = accounts_pool::session_move(
        &pool_seat,
        &accounts_pool::Session {
            cli,
            sid,
            supervisor: accounts::Supervisor::Aterm,
            env: &pairs,
            seen: &look.seen,
            sample: Some(accounts::Sample {
                at: seat.now,
                session_pct: Some(42),
                ..accounts::Sample::default()
            }),
            point: accounts::Point::TurnEnd,
            mode: accounts::Mode::Off,
            relaunch: relaunch::PoolRelaunch {
                dialect: upgrade::Dialect::Zsh,
                cd: Some(&look.snap.cwd),
                exe: &look.snap.program,
                flags: &[],
                resume: look.snap.session.as_deref(),
                home: None,
            },
        },
        |note| applied.set(look.note_pool_session(&idle.kept, note)),
    );
    let state = store.load(seat.now).unwrap().0;
    let row = state.sessions.get(sid);
    assert!(
        state.proven && state.canary.is_none(),
        "valid late account proof"
    );
    assert_eq!(
        state.accounts["work-2"]
            .sample
            .as_ref()
            .and_then(|s| s.session_pct),
        Some(42),
        "valid late usage evidence"
    );
    assert_eq!(state.accounts["work-2"].ran_at.is_some(), applied.get());
    let events = store
        .tail(100)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == accounts::Kind::Move && event.sid.as_deref() == Some(sid))
        .count();
    [
        i64::from(applied.get()),
        if row.is_some_and(|row| row.account == "work-2") {
            1
        } else {
            2
        },
        i64::from(row != before.as_ref()),
        i64::try_from(events).unwrap(),
    ]
}

#[test]
fn pooled_session_notes_recheck_the_local_generation_at_the_writer() {
    let model = aterm_models_harness::account_session_attribution_model();
    for case in 0..=7 {
        let mut state = model.init_state();
        state.insert("phase", 1);
        match case {
            1 => {
                state.insert("generation", 2);
            }
            2 => {
                state.insert("process", 2);
            }
            3 => {
                state.insert("process", 3);
            }
            4 => {
                state.insert("login", 2);
            }
            5 => {
                state.insert("present", 0);
            }
            6 => {
                state.insert("metadata", 1);
            }
            7 => {
                state.insert("process", 4);
            }
            _ => {}
        }
        let current = case == 0 || case == 6;
        assert_eq!(
            observe_pool_session(&state),
            [
                i64::from(current),
                if current { 1 } else { 2 },
                i64::from(current),
                i64::from(current)
            ],
            "interleaving {case}: session writes need current context; late account evidence survives"
        );
    }
}

pub(crate) fn session_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    if action != "Write" {
        return account_session::session_transition(state, action);
    }
    let mut next = state.clone();
    next.insert("phase", 2);
    for (key, value) in ["applied", "row", "changed", "events"]
        .into_iter()
        .zip(observe_pool_session(state))
    {
        next.insert(key, value);
    }
    next
}

#[test]
fn pooled_session_attribution_conforms_through_the_durable_writer() {
    let model = aterm_models_harness::account_session_attribution_model();
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = session_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (field, value, law) in [
        ("generation", 2, "GenerationAdmitted"),
        ("process", 2, "ProcessAdmitted"),
        ("login", 2, "LoginAdmitted"),
        ("present", 0, "PresentAdmitted"),
    ] {
        let mut state = model.init_state();
        state.insert("phase", 1);
        state.insert(field, value);
        let mut bad = session_transition(&state, "Write");
        assert_eq!(bad["applied"], 0);
        assert_eq!(bad["changed"], 0);
        bad.insert("applied", 1);
        assert!(!model.check_invariant(law, &bad));
        bad.insert("applied", 0);
        bad.insert("row", 1);
        assert!(!model.check_invariant("RefusalPreserves", &bad));
    }
    let mut state = model.init_state();
    state.insert("phase", 1);
    let good = session_transition(&state, "Write");
    for (key, value, law) in [
        ("applied", 0, "CurrentCompletes"),
        ("row", 2, "WritesObservedRow"),
        ("events", 0, "RecordsTransition"),
    ] {
        let mut bad = good.clone();
        bad.insert(key, value);
        assert!(!model.check_invariant(law, &bad));
    }
}

/// A pooled Codex tab's look carries what its own login file says it runs on
/// (`docs/DESIGN-account-discovery-2026-11-09.md`, step 2): the file its exec environment's
/// `CODEX_AUTH_PATH` names, read in the seat's home, so the hold, the label and the sighting
/// reconcile the roster's belief against it as Claude Code's do.
/// NEGATIVE CONTROL: with `pool_account_look` leaving `seen` unread, no login is observed:
/// `` left: None right: Some("unicorn…") ``.
#[test]
fn a_pool_look_observes_the_tabs_own_login_file() {
    const UNICORN: &str = "unicorn.m15.2@proton.example";
    let a = Accts::new();
    let cli = accounts::Cli::Codex;
    let path = a.roster_path().with_file_name(cli.accounts().roster_file);
    std::fs::rename(a.roster_path(), &path).unwrap();
    let login = a.login_dir("work-2");
    // An unsigned stand-in for a ChatGPT token: the claims are all the reader decodes.
    let part = |bytes: &[u8]| {
        aterm_codec::base64::encode(bytes)
            .unwrap()
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_")
    };
    let token = format!(
        "{}.{}.fixture",
        part(br#"{"alg":"none"}"#),
        part(format!(r#"{{"email":"{UNICORN}"}}"#).as_bytes())
    );
    let auth = Path::new(&login).join("auth.json");
    std::fs::write(
        &auth,
        format!(r#"{{"tokens":{{"id_token":"{token}","access_token":"{token}"}}}}"#),
    )
    .unwrap();
    let sid = "s-pool-observed";
    let mut snap = a.spawn(sid, ACCT_TALK_A, "work-2");
    snap.program = "/opt/bin/codex".into();
    snap.argv = vec!["/opt/bin/codex".into()];
    a.tabs.lock().unwrap().insert(sid.into(), snap.clone());
    a.procs
        .lock()
        .unwrap()
        .insert(snap.pid, vec![format!("CODEX_AUTH_PATH={login}/auth.json")]);
    let mut idle = a.worker(sid, &on());
    idle.agent = Program::Codex;
    let seat = a.seat();
    let pool_seat = accounts_pool::Seat {
        config_dir: path.parent().unwrap(),
        state: &seat.state,
        home: &seat.home,
        now: seat.now,
    };
    let roster = pool_seat.bound_roster(cli).unwrap();
    let (look, _) = idle.pool_account_look(cli, &roster, &seat.home).unwrap();
    assert_eq!(look.seen.email(), Some(UNICORN));
    let seen = look.seen.seen().unwrap();
    assert_eq!(seen.path, auth);
    assert_eq!(
        seen.src,
        aterm_agent::harness::journal::ObservedSrc::AuthFile
    );
    assert_eq!(
        look.label.as_deref(),
        accounts::observed_label(&roster, &look.who, &look.seen)
    );
}

const SIGHTED: &str = "unicorn.m15.2@proton.example";

/// A Codex tab on the row `work-2`, whose own `auth.json` names [`SIGHTED`], under `cfg`.
fn codex_tab(sid: &str, cfg: &SupervisorConfig) -> (Arc<Accts>, WorkerIdle) {
    let a = Accts::new();
    let cli = accounts::Cli::Codex;
    let path = a.roster_path().with_file_name(cli.accounts().roster_file);
    std::fs::rename(a.roster_path(), &path).unwrap();
    let login = a.login_dir("work-2");
    // An unsigned stand-in for a ChatGPT token: the claims are all the reader decodes.
    let part = |bytes: &[u8]| {
        aterm_codec::base64::encode(bytes)
            .unwrap()
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_")
    };
    let token = format!(
        "{}.{}.fixture",
        part(br#"{"alg":"none"}"#),
        part(format!(r#"{{"email":"{SIGHTED}"}}"#).as_bytes())
    );
    std::fs::write(
        Path::new(&login).join("auth.json"),
        format!(r#"{{"tokens":{{"id_token":"{token}","access_token":"{token}"}}}}"#),
    )
    .unwrap();
    let mut snap = a.spawn(sid, ACCT_TALK_A, "work-2");
    snap.program = "/opt/bin/codex".into();
    snap.argv = vec!["/opt/bin/codex".into()];
    a.tabs.lock().unwrap().insert(sid.into(), snap.clone());
    a.procs
        .lock()
        .unwrap()
        .insert(snap.pid, vec![format!("CODEX_AUTH_PATH={login}/auth.json")]);
    let mut idle = a.worker(sid, cfg);
    idle.agent = Program::Codex;
    (a, idle)
}

fn sightings(idle: &WorkerIdle) -> observe::Sightings {
    with_kept_of(&idle.kept, |k| k.account.sighted.clone())
}

/// THE DEAD HOOK (`docs/DESIGN-account-discovery-2026-11-09.md`, step 2): a Codex tab's account
/// is sighted when the loop tells its point ([`IdleHost::account_seen`]), though aterm
/// supervises no Codex (the tab is watched only, so no pool advises it) and with the harness
/// switched off: a sighting is an observation, which no account switch gates. What it then
/// ends with is the login it sighted, handed back as idle.
/// NEGATIVE CONTROL: with `account_seen` leaving Codex to `pool_advice_for`, which a watched
/// tab never reaches, nothing is sighted: `` assertion `left != right` failed ``.
#[test]
fn a_watched_codex_tab_is_sighted_at_its_point_whatever_the_switches_say() {
    for cfg in [on(), off()] {
        let (_a, idle) = codex_tab("s-pool-sighted", &cfg);
        assert!(idle.beside(), "CONTROL: aterm supervises no Codex");
        assert_eq!(sightings(&idle), observe::Sightings::default());
        IdleHost::account_seen(&idle);
        let mut sighted = sightings(&idle);
        assert_ne!(sighted, observe::Sightings::default());
        let ended = sighted.end(ACCT_NOW);
        assert_eq!(
            ended.login.map(|login| (login.email, login.state)),
            Some((SIGHTED.to_owned(), journal::LoginState::Idle))
        );
    }
}

/// A Grok Build tab on the row `work-2`, whose own `auth.json` names [`SIGHTED`] in a made-up
/// login record.
fn grok_tab(sid: &str) -> (Arc<Accts>, WorkerIdle) {
    let a = Accts::new();
    let cli = accounts::Cli::Grok;
    let path = a.roster_path().with_file_name(cli.accounts().roster_file);
    std::fs::rename(a.roster_path(), &path).unwrap();
    let login = a.login_dir("work-2");
    std::fs::write(
        Path::new(&login).join("auth.json"),
        format!(
            r#"{{"https://accounts.x.ai/sign-in":{{"key":"fixture-not-a-token","user_id":"grok-user-fixture","email":"{SIGHTED}"}}}}"#
        ),
    )
    .unwrap();
    let mut snap = a.spawn(sid, ACCT_TALK_A, "work-2");
    snap.program = "/opt/bin/grok".into();
    snap.argv = vec!["/opt/bin/grok".into()];
    a.tabs.lock().unwrap().insert(sid.into(), snap.clone());
    a.procs
        .lock()
        .unwrap()
        .insert(snap.pid, vec![format!("GROK_AUTH_PATH={login}/auth.json")]);
    let mut idle = a.worker(sid, &off());
    idle.agent = Program::Grok;
    (a, idle)
}

/// Step 3: a Grok Build tab's account is the one its own login file names, sighted at its point
/// as a Codex tab's is, with the harness switched off. One signed in by a credential variable
/// runs on no account of its own and is not sighted.
/// NEGATIVE CONTROL: with `account_seen` matching no `Program::Grok`, nothing is sighted:
/// `` assertion `left != right` failed ``.
#[test]
fn a_grok_tab_is_sighted_at_its_point() {
    let (_a, idle) = grok_tab("s-grok-sighted");
    assert_eq!(sightings(&idle), observe::Sightings::default());
    IdleHost::account_seen(&idle);
    let mut sighted = sightings(&idle);
    assert_ne!(sighted, observe::Sightings::default());
    assert_eq!(
        sighted
            .end(ACCT_NOW)
            .login
            .map(|login| (login.email, login.realm)),
        Some((SIGHTED.to_owned(), "grok".to_owned()))
    );
    let (a, idle) = grok_tab("s-grok-env");
    let pid = a.tabs.lock().unwrap()["s-grok-env"].pid;
    a.procs
        .lock()
        .unwrap()
        .get_mut(&pid)
        .unwrap()
        .push("XAI_API_KEY=fixture".into());
    IdleHost::account_seen(&idle);
    assert_eq!(sightings(&idle), observe::Sightings::default());
}

/// The scoped Kimi plan reads the same login file the look, the hold and the sighting do: a
/// scoped Kimi Code process launched on a row's home whose credential a `/login` rewrote names
/// the account it runs on now, and one whose model is on another provider names none.
#[test]
fn a_scoped_kimi_plan_reads_the_tabs_own_login_file() {
    let a = Accts::new();
    let home = Path::new(&a.login_dir("work-2")).to_owned();
    let part = |bytes: &[u8]| {
        aterm_codec::base64::encode(bytes)
            .unwrap()
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_")
    };
    let token = format!(
        "{}.{}.fixture",
        part(br#"{"alg":"none"}"#),
        part(format!(r#"{{"user_id":"kimi-user-fixture","email":"{SIGHTED}"}}"#).as_bytes())
    );
    std::fs::create_dir_all(home.join("credentials")).unwrap();
    std::fs::write(
        home.join("credentials/kimi-code.json"),
        format!(r#"{{"access_token":"{token}","token_type":"Bearer"}}"#),
    )
    .unwrap();
    std::fs::write(
        home.join("config.toml"),
        "default_model = \"model\"\n\
         [providers.\"managed:kimi-code\"]\ntype = \"kimi\"\n\
         base_url = \"https://api.kimi.com/coding/v1\"\napi_key = \"\"\n\
         [providers.\"managed:kimi-code\".oauth]\nstorage = \"file\"\nkey = \"oauth/kimi-code\"\n\
         [providers.own]\ntype = \"openai\"\n\
         [models.model]\nprovider = \"managed:kimi-code\"\n\
         [models.own]\nprovider = \"own\"\n",
    )
    .unwrap();
    let seat = a.seat();
    let mut args = atpkg::caller_shell::ProcArgs {
        exec_path: "/opt/bin/kimi".into(),
        argv: vec!["/opt/bin/kimi".into()],
        env: vec![format!("KIMI_CODE_HOME={}", home.display())],
    };
    let seen = scoped_kimi_seen(&args, &seat.home);
    assert_eq!(seen.email(), Some(SIGHTED));
    assert_eq!(
        seen.seen().unwrap().path,
        home.join("credentials/kimi-code.json")
    );
    args.argv.extend(["-m".into(), "own".into()]);
    assert_eq!(scoped_kimi_seen(&args, &seat.home), Observation::Unread);
}

/// A Muse Code tab keeps no pool and no roster: its account is the one its own `auth.json`
/// names, sighted at its point as any other program's.
/// NEGATIVE CONTROL: with `account_seen` matching no `Program::Muse`, nothing is sighted:
/// `` assertion `left != right` failed ``.
#[test]
fn a_muse_tab_is_sighted_at_its_point() {
    let a = Accts::new();
    let sid = "s-muse-sighted";
    let seat = a.seat();
    let auth = seat.home.join(".config/muse/auth.json");
    std::fs::create_dir_all(auth.parent().unwrap()).unwrap();
    std::fs::write(
        &auth,
        format!(r#"{{"providers":{{"meta":{{"mechanism":"oauth","user_email":"{SIGHTED}"}}}}}}"#),
    )
    .unwrap();
    let mut snap = a.spawn(sid, ACCT_TALK_A, "main");
    snap.program = "/opt/bin/muse".into();
    snap.argv = vec!["/opt/bin/muse".into()];
    a.tabs.lock().unwrap().insert(sid.into(), snap.clone());
    a.procs
        .lock()
        .unwrap()
        .insert(snap.pid, vec![format!("HOME={}", seat.home.display())]);
    let mut idle = a.worker(sid, &off());
    idle.agent = Program::Muse;
    IdleHost::account_seen(&idle);
    let mut sighted = sightings(&idle);
    assert_ne!(sighted, observe::Sightings::default());
    assert_eq!(
        sighted.end(ACCT_NOW).login.map(|login| login.email),
        Some(SIGHTED.to_owned())
    );
}

/// THE SESSION THAT NEVER ENDED: when its agent leaves for good, a Codex tab's sightings are
/// taken from what the host kept and ended, as a Claude Code tab's are, so the journal hears
/// the session's end, the registry stops counting the tab on that login, and the next agent
/// in the tab inherits nothing. The rest of what the host kept of its accounts is the
/// recorder's, which only Claude Code has, and stays.
/// NEGATIVE CONTROL: with `account_left` taking sightings only where `accounts_for(agent)`,
/// the Codex tab's are left standing: `` assertion `left != right` failed `` (none taken).
#[test]
fn every_programs_sightings_end_with_its_agent() {
    let (_a, mut idle) = codex_tab("s-pool-gone", &on());
    IdleHost::account_seen(&idle);
    with_kept_of(&idle.kept, |k| k.account.wall_again = 3);
    let taken = account_left(&idle.kept, Program::Codex);
    assert_ne!(taken, observe::Sightings::default());
    assert_eq!(sightings(&idle), observe::Sightings::default());
    assert_eq!(with_kept_of(&idle.kept, |k| k.account.wall_again), 3);
    // CONTROL: Claude Code's recorder forgets everything it kept of the session's accounts.
    IdleHost::account_seen(&idle);
    with_kept_of(&idle.kept, |k| k.account.wall_again = 3);
    idle.agent = Program::Claude;
    let taken = account_left(&idle.kept, Program::Claude);
    assert_ne!(taken, observe::Sightings::default());
    assert_eq!(sightings(&idle), observe::Sightings::default());
    assert_eq!(with_kept_of(&idle.kept, |k| k.account.wall_again), 0);
}
