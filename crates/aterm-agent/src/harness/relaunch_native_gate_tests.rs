// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::upgrade_drive::{HarnessToken, Instances, Tell};
use super::*;
use std::io::{BufRead, Write};
use std::os::unix::net::UnixStream;

const TAB: &str = "s-disabled-native-caller";
const SESSION: &str = "bbbbbbbb-0000-0000-0000-0000000000ca";

fn options(world: &Path) -> Opts {
    let mut timings = crate::harness::Timings::FAST;
    // This guard must observe the still-live test process, never wait for it to exit.
    // Zero is the negative policy input; there is no latency assertion or vendor process.
    timings.exited = Duration::ZERO;
    Opts {
        home: world.join("home"),
        state: world.join("state"),
        account_roster: Some(world.join("config/accounts.toml")),
        sock: Some(world.join("fake.sock").to_string_lossy().into_owned()),
        token: Some(HarnessToken::new("private-caller-fixture")),
        only_sid: Some(TAB.into()),
        dry_run: false,
        human_grace_s: 0,
        hand_back: false,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: false,
        usage_credits: false,
        usage_tap: None,
        instances: Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
        land: false,
        timings,
        tell: Tell::Off,
        press: None,
    }
}

fn endpoint() -> (Client, std::thread::JoinHandle<Vec<String>>) {
    endpoint_with(|line| {
        if line == format!("@{TAB} native-act constructors") {
            "OK native-act state=constructors plans=-\n"
        } else if line.starts_with(&format!("@{TAB} lease acquire ")) {
            "ERR busy\n"
        } else {
            // A regression can reach only this private fake endpoint, never a real PTY.
            "ERR unexpected fixture action\n"
        }
        .into()
    })
}

fn endpoint_with(
    mut answer: impl FnMut(&str) -> String + Send + 'static,
) -> (Client, std::thread::JoinHandle<Vec<String>>) {
    let (near, far) = UnixStream::pair().unwrap();
    for stream in [&near, &far] {
        stream
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
        stream
            .set_write_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
    }
    let worker = std::thread::spawn(move || {
        let mut writer = far.try_clone().unwrap();
        let mut reader = std::io::BufReader::new(far);
        let mut seen = Vec::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                break;
            }
            let line = line.trim_end().to_string();
            let reply = answer(&line);
            seen.push(line);
            writer.write_all(reply.as_bytes()).unwrap();
        }
        seen
    });
    (crate::RelayClient::new(near), worker)
}

#[test]
fn uncertain_queries_and_unsealed_timeouts_preserve_native_debt() {
    for case in ["query-error", "retired", "unresolved"] {
        let world = aterm_tempfile::tempdir().unwrap();
        let mut opts = options(world.path());
        opts.timings.held = Duration::ZERO;
        let mut st = state(true);
        st.pid = 0;
        st.shell = 0;
        st.phase = upgrade::Phase::Relaunched { at_s: 1 };
        st.back_line = "original rollback".into();
        st.native_prepare = "retained original preparation".into();
        st.native = Some(super::super::native_act::Binding {
            id: "ab".repeat(16),
            capability: "cd".repeat(32),
        });
        let original = st.clone();
        let (mut client, worker) = endpoint_with(move |line| {
            if line.contains(" lease acquire ") || line.contains(" lease release ") {
                "ERR busy\n".into()
            } else if line.contains(" native-act query ") {
                if case == "query-error" {
                    "ERR unavailable\n"
                } else {
                    "OK native-act state=unknown\n"
                }
                .into()
            } else if line.contains(" native-act reconcile ") {
                if case == "retired" {
                    "OK native-act state=retired reason=no-exec-grant\n"
                } else {
                    "OK native-act state=unknown\n"
                }
                .into()
            } else {
                panic!("uncertain native debt must not dispatch {line}")
            }
        });
        let got = await_new(&opts, report(), &mut st, &mut client, SESSION, &Live);
        assert_eq!(
            got.step,
            match case {
                "query-error" => "wait:native-recovery",
                "retired" => super::super::native_restart::WAIT_RETIRED_UNISSUED,
                _ => "wait:native-retirement",
            }
        );
        assert_eq!(st.phase, original.phase);
        assert_eq!(st.back_line, original.back_line);
        assert_eq!(st.native_prepare, original.native_prepare);
        assert_eq!(st.native_retired_unissued, case == "retired");
        assert!(!super::super::upgrade_drive::ledger_path(&opts).exists());
        drop(client);
        let seen = worker.join().unwrap();
        assert_eq!(
            seen.iter()
                .filter(|line| line.contains(" native-act reconcile "))
                .count(),
            usize::from(case != "query-error")
        );
    }
}

#[test]
fn retirement_queries_without_a_registered_process_never_consume_debt() {
    for retired_at in 1..=3 {
        for found_gone in [false, true] {
            let world = aterm_tempfile::tempdir().unwrap();
            let mut opts = options(world.path());
            opts.timings.held = Duration::ZERO;
            let mut st = state(true);
            st.pid = 0;
            st.shell = 0;
            st.phase = upgrade::Phase::Relaunched { at_s: 1 };
            st.found_gone = found_gone;
            st.back_line = "original rollback".into();
            st.native_prepare = "retained original preparation".into();
            st.native = Some(super::super::native_act::Binding {
                id: "ab".repeat(16),
                capability: "cd".repeat(32),
            });
            let before = st.clone();
            let mut queries = 0;
            let (mut client, worker) = endpoint_with(move |line| {
                if line.contains(" lease acquire ") || line.contains(" lease release ") {
                    "ERR busy\n".into()
                } else if line.contains(" native-act query ") {
                    queries += 1;
                    if queries >= retired_at {
                        "OK native-act state=retired reason=no-exec-grant\n".into()
                    } else {
                        "OK native-act state=unknown\n".into()
                    }
                } else {
                    panic!("a terminal observation cannot dispatch {line}")
                }
            });
            let got = await_new(&opts, report(), &mut st, &mut client, SESSION, &Live);
            assert_eq!(
                got.step,
                super::super::native_restart::WAIT_RETIRED_UNISSUED
            );
            let saved = load(&opts, SESSION).unwrap();
            assert!(saved.native_retired_unissued);
            assert_eq!(saved.phase, before.phase);
            assert_eq!(saved.found_gone, before.found_gone);
            assert_eq!(saved.back_line, before.back_line);
            assert_eq!(saved.native_prepare, before.native_prepare);
            assert!(!super::super::upgrade_drive::ledger_path(&opts).exists());
            drop(client);
            assert_eq!(
                worker
                    .join()
                    .unwrap()
                    .iter()
                    .filter(|line| line.contains(" native-act query "))
                    .count(),
                retired_at
            );
        }
    }
}

#[test]
fn a_closure_observes_retirement_before_testing_whether_the_tab_was_taken() {
    let world = aterm_tempfile::tempdir().unwrap();
    let opts = options(world.path());
    let mut st = state(true);
    st.native = Some(super::super::native_act::Binding {
        id: "ab".repeat(16),
        capability: "cd".repeat(32),
    });
    let (mut client, worker) = endpoint_with(|line| {
        assert!(line.contains(" native-act query "));
        "OK native-act state=retired reason=no-exec-grant\n".into()
    });
    let got = close_if_taken(&opts, &report(), &mut st, SESSION, &mut client, TAB).unwrap();
    assert_eq!(
        got.step,
        super::super::native_restart::WAIT_RETIRED_UNISSUED
    );
    assert!(load(&opts, SESSION).unwrap().native_retired_unissued);
    assert!(!super::super::upgrade_drive::ledger_path(&opts).exists());
    drop(client);
    assert_eq!(worker.join().unwrap().len(), 1);
}

#[test]
fn retirement_after_registration_keeps_the_prior_gone_observation() {
    for during_poll in [false, true] {
        let world = aterm_tempfile::tempdir().unwrap();
        let mut opts = options(world.path());
        opts.timings.held = Duration::ZERO;
        let pid = std::process::id();
        let start = kernel_start(pid).unwrap();
        let sessions = opts.home.join(".claude/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
        sessions.join(format!("{pid}.json")),
        format!(
            r#"{{"pid":{pid},"sessionId":"{SESSION}","cwd":"/","version":"1.0.2","status":"idle","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
        ),
    )
    .unwrap();
        let mut st = state(true);
        st.pid = 0;
        st.shell = ids(pid).unwrap().0;
        st.phase = upgrade::Phase::Relaunched { at_s: 7 };
        st.found_gone = true;
        st.native_prepare = "retained original recipe".into();
        st.native = Some(super::super::native_act::Binding {
            id: "ab".repeat(16),
            capability: "cd".repeat(32),
        });
        let mut queries = 0;
        let (mut client, worker) = endpoint_with(move |line| {
            if line.contains(" lease acquire ") {
                "ERR busy\n".into()
            } else if line.contains(" await agent ") {
                "OK idle\n".into()
            } else if line.contains(" native-act query ") {
                queries += 1;
                if queries == 2 && during_poll {
                    "OK native-act state=resumed phase=forward\n".into()
                } else if queries < 3 {
                    "OK native-act state=unknown\n".into()
                } else {
                    "OK native-act state=retired reason=no-exec-grant\n".into()
                }
            } else {
                panic!("unexpected registration observation: {line}")
            }
        });
        let result = await_new(&opts, report(), &mut st, &mut client, SESSION, &Live);
        assert_eq!(
            result.step,
            super::super::native_restart::WAIT_RETIRED_UNISSUED
        );
        let saved = load(&opts, SESSION).unwrap();
        assert!(saved.native_retired_unissued && saved.found_gone);
        assert_eq!(saved.phase, upgrade::Phase::Relaunched { at_s: 7 });
        assert_eq!(saved.native_prepare, "retained original recipe");
        drop(client);
        assert_eq!(
            worker
                .join()
                .unwrap()
                .iter()
                .filter(|line| line.contains(" native-act query "))
                .count(),
            3
        );
    }
}

#[test]
fn retired_carry_never_enters_idle_scheduling_or_settles_its_debt() {
    let world = aterm_tempfile::tempdir().unwrap();
    let opts = options(world.path());
    let mut st = state(true);
    st.native_retired_unissued = true;
    st.phase = super::super::upgrade::Phase::Relaunched { at_s: 7 };
    save(&opts, SESSION, &st);
    let original = load(&opts, SESSION).unwrap();
    assert!(original.in_flight());
    assert!(!owed(&opts));
    assert!(!carry_owed_of(&opts, None));
    assert_eq!(resume_as(&opts, false, None).step, "current");
    assert_eq!(settle(&opts).step, "current");
    for got in [
        carried_done(&opts, report(), SESSION, END_LANDED),
        done_quietly(&opts, report(), &mut st, SESSION, "1.0.2", 42),
        done_with_nothing(
            &opts,
            report(),
            &mut st,
            SESSION,
            "done:taskless",
            NOBODY_ASKED,
            "1.0.2",
            42,
        ),
        settle_look(&opts, &report(), &mut st, SESSION, 100).unwrap(),
    ] {
        assert_eq!(
            got.step,
            super::super::native_restart::WAIT_RETIRED_UNISSUED
        );
    }
    assert_eq!(st.phase, original.phase);
    assert_eq!(load(&opts, SESSION).unwrap(), original);
    assert!(!super::super::upgrade_drive::ledger_path(&opts).exists());
}

#[test]
fn retirement_discovered_during_abort_is_saved_before_any_further_observation() {
    use crate::harness::native_act::{Image, NativePlan, Prepare, Process, Request};
    let world = aterm_tempfile::tempdir().unwrap();
    let opts = options(world.path());
    let process = Process::observe(std::process::id()).unwrap();
    let command = PlannedCommand {
        target: std::env::current_exe().unwrap(),
        argv: Vec::new(),
        env: Vec::new(),
        unset: None,
        cwd: None,
        heal: None,
        dialect: Dialect::Zsh,
        account_intent: Some(super::super::accounts_pool::ExecutionIntent::external()),
    }
    .capture()
    .unwrap();
    let mut st = state(true);
    st.line = command.legacy_line().unwrap();
    st.native_prepare = Request::prepare(Prepare {
        id: "ab".repeat(16),
        host_epoch: "ef".repeat(16),
        holder: super::super::upgrade_drive::hand_name(),
        revision: 1,
        deadline_us: 100,
        duty: crate::harness::contract::Scope::Relaunch,
        expected_human_seq: Some(0),
        plan: NativePlan::Restart {
            cold: None,
            process: process.clone(),
            shell: process,
            observation: None,
            launcher: Image::at(&std::env::current_exe().unwrap()).unwrap(),
            socket: opts.sock.clone().unwrap(),
            forward: Box::new(command),
            rollback: None,
        },
    })
    .encode()
    .unwrap();
    let original = st.clone();
    let mut prepares = 0;
    let (mut client, worker) = endpoint_with(move |line| {
        if line.contains(" lease acquire ") {
            return "OK lease acquired\n".into();
        }
        if line.contains(" lease release ") {
            return "OK lease released\n".into();
        }
        if line.contains(" native-act prepare ") {
            prepares += 1;
            return if prepares == 1 {
                format!(
                    "OK native-act state=admitted id={} cap={}\n",
                    "ab".repeat(16),
                    "cd".repeat(32)
                )
            } else {
                "OK native-act state=retired reason=no-exec-grant\n".into()
            };
        }
        if line.contains(" native-act query ") {
            return "OK native-act state=unknown\n".into();
        }
        panic!("unexpected native action: {line}")
    });
    let result = relaunch(&opts, report(), &mut st, &mut client, SESSION, &Live);
    assert_eq!(
        result.step,
        super::super::native_restart::WAIT_RETIRED_UNISSUED
    );
    let saved = load(&opts, SESSION).unwrap();
    assert!(saved.native_retired_unissued);
    assert_eq!(saved.native_prepare, original.native_prepare);
    assert_eq!(saved.phase, original.phase);
    drop(client);
    let seen = worker.join().unwrap();
    assert_eq!(
        seen.iter()
            .filter(|line| line.contains(" native-act prepare "))
            .count(),
        2
    );
    assert!(seen.iter().any(|line| line.contains(" lease release ")));
}

fn report() -> Report {
    Report {
        pid: std::process::id(),
        tab: TAB.into(),
        session: SESSION.into(),
        from: "1".into(),
        to: "2".into(),
        step: String::new(),
    }
}

fn state(coordinated: bool) -> St {
    St {
        account_intent: Some(super::super::accounts_pool::ExecutionIntent::external()),
        agent: upgrade::Agent::Claude,
        phase: upgrade::Phase::Exiting { at_s: now_s() },
        pid: std::process::id(),
        shell: std::process::id(),
        tab: TAB.into(),
        line: "fixed legacy fixture".into(),
        native_coordination: coordinated,
        ..St::default()
    }
}

#[test]
fn retained_retirement_does_not_reacquire_a_terminal_lease() {
    let world = aterm_tempfile::tempdir().unwrap();
    let opts = options(world.path());
    let (mut client, worker) = endpoint();
    let mut st = state(true);
    st.native_retired_unissued = true;
    let before = st.clone();
    let result = relaunch(&opts, report(), &mut st, &mut client, SESSION, &Live);
    assert_eq!(
        result.step,
        super::super::native_restart::WAIT_RETIRED_UNISSUED
    );
    assert_eq!(outcome(&result.step), Outcome::Busy);
    assert_eq!(st, before);
    let result = await_new(&opts, report(), &mut st, &mut client, SESSION, &Live);
    assert_eq!(
        result.step,
        super::super::native_restart::WAIT_RETIRED_UNISSUED
    );
    assert_eq!(st, before);
    let wait = native_retirement_wait(&opts, &report(), &mut st, SESSION, &mut client).unwrap();
    assert_eq!(
        wait.step,
        super::super::native_restart::WAIT_RETIRED_UNISSUED
    );
    drop(client);
    assert!(worker.join().unwrap().is_empty());
}

#[test]
fn retained_account_attribution_round_trips_but_legacy_or_missing_namespace_is_no_grant() {
    use super::super::accounts_pool::ExecutionIntent;
    let world = aterm_tempfile::tempdir().unwrap();
    let mut opts = options(world.path());
    let seat = account_seat(&opts).unwrap();
    let intent = super::super::accounts_pool::capture_execution(
        &seat,
        accounts::Cli::Claude,
        &accounts::ProcessLogin {
            login: None,
            default_home: true,
            stored_login: true,
        },
    )
    .unwrap();
    let mut st = state(false);
    st.account_intent = Some(intent.clone());
    st.back_account_intent = Some(ExecutionIntent::external());
    save_checked(&opts, SESSION, &st).unwrap();
    let saved = load(&opts, SESSION).unwrap();
    assert_eq!(saved.account_intent, st.account_intent);
    assert_eq!(saved.back_account_intent, st.back_account_intent);
    let command = PlannedCommand {
        dialect: Dialect::Zsh,
        heal: None,
        cwd: None,
        target: std::env::current_exe().unwrap(),
        argv: Vec::new(),
        env: Vec::new(),
        unset: None,
        account_intent: Some(intent.clone()),
    }
    .capture()
    .unwrap();
    let encoded = command.encode().unwrap();
    assert_eq!(
        super::super::native_act::RestartCommand::parse(&encoded),
        Some(command)
    );
    let mut next_round = st.clone();
    next_round.signalled(st.pid, st.shell, TAB, "new command".into(), now_s());
    assert_eq!(
        next_round.account_intent, None,
        "a new line never inherits old authority"
    );
    assert_eq!(next_round.back_account_intent, None);
    let mut cancelled = st.clone();
    let undo = cancelled.signalled(st.pid, st.shell, TAB, "new command".into(), now_s());
    cancelled.account_intent = Some(intent.clone());
    cancelled.unsent(undo);
    assert_eq!(
        cancelled.account_intent, None,
        "an unsent command leaves no authority behind"
    );
    assert!(account_execution(&opts, saved.account_intent.as_ref()).is_ok());
    assert!(
        account_execution(&opts, None).is_err(),
        "legacy omission is not External"
    );
    opts.account_roster = None;
    assert!(
        account_execution(&opts, Some(&intent)).is_err(),
        "missing local context is no permission"
    );
    assert!(account_execution(&opts, Some(&ExecutionIntent::external())).is_ok());
    st.back_line = "rollback".into();
    st.source = "new-version".into();
    assert!(st.roll_back(now_s(), 0));
    assert_eq!(st.account_intent, Some(ExecutionIntent::external()));
    assert_eq!(st.back_account_intent, None);
}

#[test]
fn helper_refused_during_observation_keeps_the_same_operation_and_owed_work() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let world = aterm_tempfile::tempdir().unwrap();
    let mut opts = options(world.path());
    opts.timings.held = Duration::ZERO;
    let queries = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&queries);
    let (socket, requests) = crate::harness::fake_instance(
        &world.path().join("unissued.sock"),
        "private-caller-fixture",
        move |line| {
            if line == "version" {
                "OK fixture".into()
            } else if line.contains(" native-act query ") {
                if observed.fetch_add(1, Ordering::SeqCst) < 2 {
                    "OK native-act state=unknown".into()
                } else {
                    "OK native-act state=unissued phase=forward".into()
                }
            } else {
                "ERR observation only".into()
            }
        },
    );
    opts.sock = Some(socket);
    let mut st = state(true);
    let original_exit = now_s().saturating_sub(20);
    st.exited_at = original_exit;
    st.phase = upgrade::Phase::Relaunched { at_s: now_s() };
    st.cause = CAUSE_MEMORY.into();
    st.source = "new-release".into();
    st.back_line = "retained rollback".into();
    st.native = Some(super::super::native_act::Binding {
        id: "ab".repeat(16),
        capability: "cd".repeat(32),
    });
    let original = st.native.clone();
    let mut client = connect(&opts, TAB).unwrap();
    let _waits = virtual_waits();
    let result = await_new(&opts, report(), &mut st, &mut client, SESSION, &Live);
    assert_eq!(result.step, "wait:native-launch-unissued");
    assert!(
        queries.load(Ordering::SeqCst) >= 3,
        "no unissued proof existed at entry"
    );
    assert_eq!(st.native, original);
    assert!(matches!(st.phase, upgrade::Phase::Exiting { .. }));
    assert_eq!(
        st.phase,
        upgrade::Phase::Exiting {
            at_s: original_exit
        }
    );
    assert_eq!(st.back_line, "retained rollback");
    assert_eq!(st.source, "new-release");
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .all(|line| !line.contains(" native-act reconcile ") && !line.contains(" turn "))
    );
}

#[test]
fn external_shell_admission_never_falls_back_to_the_default_login() {
    use super::super::accounts_pool::ExecutionIntent;
    let world = aterm_tempfile::tempdir().unwrap();
    let opts = options(world.path());
    let inherited = vec![format!("HOME={}", opts.home.display())];
    let mut intent = ExecutionIntent::external();
    assert!(external_from_env(&opts, &intent, &inherited, None).is_err());
    let mut external = inherited.clone();
    external.push("ANTHROPIC_API_KEY=synthetic-credential".into());
    assert!(external_from_env(&opts, &intent, &external, None).is_ok());
    intent.bind_external_environment(
        &[(
            accounts::HOME_ENV.into(),
            opts.home.join(".claude").display().to_string(),
        )],
        None,
        None,
    );
    assert!(
        external_from_env(&opts, &intent, &inherited, None).is_err(),
        "a custom-home alias is not external"
    );
    intent.bind_external_environment(
        &[(
            accounts::HOME_ENV.into(),
            world.path().join("separate-home").display().to_string(),
        )],
        None,
        None,
    );
    assert!(external_from_env(&opts, &intent, &inherited, None).is_ok());
    intent.bind_external_environment(
        &[("ANTHROPIC_API_KEY".into(), "must-not-be-stored".into())],
        None,
        None,
    );
    assert!(!intent.to_value().to_string().contains("must-not-be-stored"));
}

/// A stored login on a home of its own (`CLAUDE_CONFIG_DIR`) is an external route, rechecked on
/// its shell's environment before its line is typed. Where that reads empty, as macOS gives a
/// platform `/bin/zsh`'s, the recheck reads the launch environment its agent recorded; with no
/// record it still refuses, which before the record was every such relaunch's indefinite wait.
#[test]
fn a_stored_login_on_its_own_home_is_rechecked_on_its_recorded_launch_environment() {
    use super::super::accounts_pool::ExecutionIntent;
    let world = aterm_tempfile::tempdir().unwrap();
    let opts = options(world.path());
    let own_home = world.path().join("separate-home").display().to_string();
    let line = [(accounts::HOME_ENV.to_string(), own_home.clone())];
    let platform_shell: Vec<String> = Vec::new();
    let mut intent = ExecutionIntent::external();
    intent.bind_external_environment(&line, None, None);
    assert!(
        external_from_env(&opts, &intent, &platform_shell, None).is_err(),
        "with no record, an empty shell environment is no evidence"
    );
    let user_home = format!("HOME={}", opts.home.display());
    let recorded = accounts::LaunchEnv::of_exec_env(&[
        user_home.clone(),
        format!("{}={own_home}", accounts::HOME_ENV),
        "PATH=/usr/bin".into(),
    ])
    .expect("a stored login's launch environment");
    intent.bind_external_environment(&line, None, Some(&recorded));
    assert!(external_from_env(&opts, &intent, &platform_shell, None).is_ok());
    // The record is the base only where the shell's own environment reads empty.
    let readable = ["HOME=/fixture/readable-shell".to_string()];
    let base = |inherited: &[String]| intent.external_environment(inherited).unwrap();
    assert!(base(&readable).contains(&readable[0]) && !base(&readable).contains(&user_home));
    assert!(base(&platform_shell).contains(&user_home));
    // The record is kept with the intent, round-trips exactly, and keeps none of what the
    // checks do not read; a record edited to carry a credential conveys nothing.
    let stored = intent.to_value();
    assert!(!stored.to_string().contains("PATH="));
    assert_eq!(ExecutionIntent::from_value(&stored), Some(intent.clone()));
    let mut edited = stored.clone();
    edited.as_object_mut().unwrap().insert(
        "launch".into(),
        aterm_json::json!([user_home, "ANTHROPIC_API_KEY=synthetic-credential"]),
    );
    assert_eq!(ExecutionIntent::from_value(&edited), None);
}

fn assert_only_discovery_and_hand(seen: &[String]) {
    assert!(
        seen.iter()
            .any(|line| line == &format!("@{TAB} native-act constructors"))
    );
    assert!(
        seen.iter()
            .all(|line| line == &format!("@{TAB} native-act constructors")
                || line.starts_with(&format!("@{TAB} lease acquire ")))
    );
}

fn exit_input(world: &Path, st: &St) -> (Snapshot, Plan) {
    let snap = Snapshot {
        launch_env: None,
        auth_observed: true,
        stored_login: false,
        profile: None,
        tab: TAB.into(),
        pid: std::process::id(),
        start: "captured start".into(),
        shell: std::process::id(),
        program: std::env::current_exe().unwrap(),
        argv: Vec::new(),
        session: Some(SESSION.into()),
        cwd: world.to_string_lossy().into_owned(),
        version: None,
        run: AgentRun::Claude,
        dialect: Some(Dialect::Zsh),
        login: None,
        home: None,
        claude_dir: None,
    };
    let plan = Plan {
        shell: snap.shell,
        dialect: Dialect::Zsh,
        line: st.line.clone(),
        tap_dropped: None,
        line_dir: None,
        command: None,
        cold: None,
    };
    (snap, plan)
}

#[test]
fn live_account_admission_runs_only_before_a_new_exit_plan() {
    for case in ["crash", "asked", "clean", "one-shot", "in-flight"] {
        let world = aterm_tempfile::tempdir().unwrap();
        let mut opts = options(world.path());
        let mut exited = aterm_uds::spawnfd::command(crate::test_program::kept("/usr/bin/true"))
            .spawn()
            .unwrap();
        let pid = exited.id();
        exited.wait().unwrap();
        let mut st = state(false);
        st.pid = pid;
        st.cause = "exit".into();
        st.salt = now_s();
        let (mut snap, _) = exit_input(world.path(), &st);
        snap.pid = pid;
        snap.argv.push("claude".into());
        if case == "one-shot" {
            snap.argv.push("--print".into());
        }
        if case == "in-flight" {
            save_checked(&opts, SESSION, &st).unwrap();
            assert!(load(&opts, SESSION).is_some_and(|saved| saved.in_flight()));
        }
        opts.dry_run = true;
        let calls = std::cell::Cell::new(0);
        let admission = |_: &Snapshot| {
            calls.set(calls.get() + 1);
            Err("held test account".into())
        };
        let left = match case {
            "crash" => ExitRecord::Survived(Box::new(SessionFile {
                pid,
                session_id: SESSION.into(),
                cwd: snap.cwd.clone(),
                version: "2.1.283".into(),
                status: "busy".into(),
                status_updated_at_ms: 1,
                proc_start: snap.start.clone(),
                kind: "interactive".into(),
                entrypoint: "cli".into(),
                claude_dir: None,
                profile: None,
            })),
            "clean" => ExitRecord::Removed,
            _ => ExitRecord::Crashed,
        };
        let asked = (!matches!(case, "crash" | "clean"))
            .then_some(super::super::restart_stamp::Cause::Asked);
        let result = after_exit_admitting(&opts, &snap, &left, false, asked, false, &admission);
        match case {
            "crash" | "asked" => {
                assert_eq!(calls.get(), 1);
                assert_eq!(result.step, "wait:account");
            }
            "clean" => {
                assert_eq!(calls.get(), 0);
                assert_eq!(result.step, "ended:graceful-exit");
            }
            "one-shot" => {
                assert_eq!(calls.get(), 0);
                assert_eq!(result.step, "ended:one-shot");
            }
            _ => {
                assert_eq!(calls.get(), 0);
                assert!(result.step.starts_with("would-resume:"), "{}", result.step);
            }
        }
    }
}

#[test]
fn live_admitted_login_reaches_the_saved_plan_before_a_held_execution() {
    let world = aterm_tempfile::tempdir().unwrap();
    let mut opts = options(world.path());
    std::fs::create_dir_all(&opts.home).unwrap();
    let (socket, requests) = crate::harness::fake_instance(
        &world.path().join("plan.sock"),
        "private-caller-fixture",
        |line| {
            if line == "version" {
                "OK fixture".into()
            } else if line == "sessions" {
                format!("OK 1\nlocal {TAB} 0 idle claude")
            } else if line.ends_with(" native-act constructors") {
                "OK native-act state=constructors plans=-".into()
            } else if line.contains(" lease acquire ") {
                "ERR busy".into()
            } else {
                "ERR unexpected fixture action".into()
            }
        },
    );
    opts.sock = Some(socket);
    let mut shell = aterm_uds::spawnfd::command(crate::test_program::kept("/bin/bash"))
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "printf 'ready\\n'; read -r line",
        ])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .unwrap();
    aterm_uds::deadline::prints(&mut shell, "ready", aterm_spec::wait::MUST_HAPPEN);
    let mut exited = aterm_uds::spawnfd::command(crate::test_program::kept("/usr/bin/true"))
        .spawn()
        .unwrap();
    let pid = exited.id();
    exited.wait().unwrap();
    let (mut snap, _) = exit_input(world.path(), &state(false));
    snap.pid = pid;
    snap.shell = shell.id();
    snap.argv = vec!["claude".into()];
    snap.login = Some(world.path().join("previous-login").display().to_string());
    snap.stored_login = true;
    snap.profile = Some(super::super::profile::Context {
        plain: opts.home.join(".claude.json"),
        records: opts.home.join(".claude"),
        selection: super::super::profile::Selection::Standard,
        home: None,
        config: None,
        login: snap.login.clone(),
    });
    let captured = snap.clone();
    let chosen_login = world.path().join("current-login").display().to_string();
    let admit = |source: &Snapshot| {
        let mut chosen = source.clone();
        chosen.login = Some(chosen_login.clone());
        Ok(ColdAdmission::immediate(Some(chosen)))
    };
    let result = after_exit_admitting(
        &opts,
        &snap,
        &ExitRecord::Crashed,
        false,
        Some(super::super::restart_stamp::Cause::Asked),
        false,
        &admit,
    );
    let saved = load(&opts, SESSION);
    drop(shell.stdin.take());
    aterm_uds::deadline::kill_within(&mut shell, aterm_spec::wait::MUST_HAPPEN);
    assert!(result.step.starts_with("wait:"), "{}", result.step);
    let saved = saved.expect("the admitted plan was saved before trying the held line");
    assert_eq!(saved.login.as_deref(), Some(chosen_login.as_str()));
    assert!(saved.line.contains(&chosen_login), "{}", saved.line);
    assert!(!saved.line.contains(captured.login.as_deref().unwrap()));
    assert_eq!(
        snap, captured,
        "planning preserves the original profile and capture"
    );
    let requests = requests.lock().unwrap();
    assert!(requests.iter().any(|line| line.contains(" lease acquire ")));
    assert!(requests.iter().all(|line| !line.contains(" turn ")));
}

#[test]
fn disabled_native_kind_relaunch_held_continues_legacy_exit_guard_only_when_fresh() {
    for coordinated in [false, true] {
        let world = aterm_tempfile::tempdir().unwrap();
        let opts = options(world.path());
        let (mut client, worker) = endpoint();
        let mut st = state(coordinated);
        let result = relaunch(&opts, report(), &mut st, &mut client, SESSION, &Live);
        drop(client);
        let seen = worker.join().unwrap();
        assert_eq!(
            result.step,
            if coordinated {
                "wait:native-constructor-disabled"
            } else {
                "wait:exiting"
            }
        );
        assert_eq!(st.native_coordination, coordinated);
        assert_only_discovery_and_hand(&seen);
    }
}

#[test]
fn disabled_native_kind_exit_launch_tail_preserves_legacy_guard_and_coordinated_refusal() {
    for coordinated in [false, true] {
        let world = aterm_tempfile::tempdir().unwrap();
        let opts = options(world.path());
        let (client, worker) = endpoint();
        let st = state(coordinated);
        let (snap, plan) = exit_input(world.path(), &st);
        // This is the production after_exit_as tail, after its record/plan/roster selection.
        // The live PID simulates the mandatory final exit guard refusing a late continuation.
        let result = launch_after_exit(&opts, &snap, ExitCause::Exit, report(), st, plan, client);
        let seen = worker.join().unwrap();
        assert_eq!(
            result.step,
            if coordinated {
                "wait:native-constructor-disabled"
            } else {
                "wait:exiting"
            }
        );
        assert_only_discovery_and_hand(&seen);
    }
}

#[test]
fn missing_private_token_relaunch_held_preserves_only_zero_native_legacy_state() {
    for coordinated in [false, true] {
        let world = aterm_tempfile::tempdir().unwrap();
        let mut opts = options(world.path());
        opts.token = None;
        let (mut client, worker) = endpoint();
        let mut st = state(coordinated);
        assert!(st.native.is_none() && st.native_prepare.is_empty());
        let result = relaunch(&opts, report(), &mut st, &mut client, SESSION, &Live);
        drop(client);
        let seen = worker.join().unwrap();
        assert_eq!(
            result.step,
            if coordinated {
                "wait:native-coordinated-unavailable"
            } else {
                "wait:exiting"
            }
        );
        assert_eq!(st.native_coordination, coordinated);
        assert!(
            seen.iter()
                .all(|line| line.starts_with(&format!("@{TAB} lease acquire ")))
        );
    }
}

#[test]
fn missing_private_token_exit_launch_tail_preserves_only_zero_native_legacy_state() {
    for coordinated in [false, true] {
        let world = aterm_tempfile::tempdir().unwrap();
        let mut opts = options(world.path());
        opts.token = None;
        let (client, worker) = endpoint();
        let st = state(coordinated);
        assert!(st.native.is_none() && st.native_prepare.is_empty());
        let (snap, plan) = exit_input(world.path(), &st);
        let result = launch_after_exit(&opts, &snap, ExitCause::Exit, report(), st, plan, client);
        let seen = worker.join().unwrap();
        assert_eq!(
            result.step,
            if coordinated {
                "wait:native-coordinated-unavailable"
            } else {
                "wait:exiting"
            }
        );
        assert!(
            seen.iter()
                .all(|line| line.starts_with(&format!("@{TAB} lease acquire ")))
        );
    }
}
