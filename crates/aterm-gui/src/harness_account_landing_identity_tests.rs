// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[derive(Clone, Copy)]
struct Case {
    expected: i64,
    before: i64,
    current: i64,
    default: bool,
    mode: i64,
    legacy: bool,
}

fn email(kind: i64) -> &'static str {
    match kind {
        1 => "one@example.test",
        2 => "two@example.test",
        _ => unreachable!(),
    }
}

fn write_profile(path: &Path, kind: i64) -> Vec<u8> {
    let value = if kind == 0 {
        aterm_json::json!({"oauthAccount":{"accountUuid":"opaque"}})
    } else {
        aterm_json::json!({"oauthAccount":{"emailAddress":email(kind)}})
    };
    let bytes = aterm_json::to_vec(&value).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, &bytes).unwrap();
    bytes
}

fn landing(mode: i64) -> Landing {
    match mode {
        0 => Landing::Immediate,
        1 => Landing::Carried,
        2 => Landing::Asked,
        _ => unreachable!(),
    }
}

fn images(a: &Accts) -> [Option<Vec<u8>>; 4] {
    let store = a.store();
    [
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ]
    .map(|path| std::fs::read(path).ok())
}

fn setup(case: Case) -> (Arc<Accts>, WorkerIdle, PathBuf) {
    let a = Accts::new();
    let target = if case.default { "main" } else { "work-2" };
    let source = if case.default { "work-2" } else { "main" };
    let mut roster = accounts::Roster::load(&a.roster_path()).unwrap();
    if case.expected != 0 {
        let row = roster
            .rows
            .iter_mut()
            .find(|row| row.label == target)
            .unwrap();
        row.email = Some("delivery-alias@example.test".into());
        row.identity_email = Some(email(case.expected).to_ascii_uppercase());
    }
    std::fs::write(
        a.roster_path(),
        accounts::append_rows_for(accounts::Cli::Claude, "", &roster.rows).unwrap(),
    )
    .unwrap();
    let profile = if case.default {
        a.tmp.path().join(if case.legacy {
            "home/.claude/.config.json"
        } else {
            "home/.claude.json"
        })
    } else {
        PathBuf::from(a.login_dir(target)).join(if case.legacy {
            ".config.json"
        } else {
            ".claude.json"
        })
    };
    write_profile(&profile, case.expected);
    a.store()
        .transact(a.clock(), &mut None, |state| {
            state.active = Some(source.into());
            state.proven = true;
            Ok(Vec::new())
        })
        .unwrap();
    let sid = "s-landing-identity-model";
    a.spawn(sid, ACCT_TALK_A, source);
    a.wall(ACCT_TALK_A, ACCT_NOW - 10);
    let idle = a.worker(sid, &on());
    a.unread.store(true, Ordering::SeqCst);
    assert_eq!(moves(&idle.account(usage_wall())), Some(target));
    assert_eq!(
        idle.restart(&move_to(target, AccountAt::Wall)).as_deref(),
        Some("wait:account-unverified")
    );
    a.readable();
    assert!(refresh_snapshot(sid, &idle.kept, &idle.hooks));
    assert!(idle.kept.lock().unwrap().account.flying.is_some());
    (a, idle, profile)
}

fn complete(
    a: &Arc<Accts>,
    idle: &WorkerIdle,
    profile: &Path,
    case: Case,
) -> (Applied, bool, bool) {
    write_profile(profile, case.before);
    let before = images(a);
    let path = profile.to_path_buf();
    let written = Arc::new(Mutex::new(None));
    let captured = Arc::clone(&written);
    a.at_seat_read(2, move |_| {
        *captured.lock().unwrap() = Some(write_profile(&path, case.current));
    });
    let result = account_land(&idle.sid, &idle.kept, &idle.hooks, landing(case.mode));
    let expected = written
        .lock()
        .unwrap()
        .take()
        .expect("the completion reached its writer boundary");
    let preserved = std::fs::read(profile).unwrap() == expected;
    (result, images(a) == before, preserved)
}

fn run(case: Case, poll: bool, retry: bool, replay: bool) -> [(&'static str, i64); 7] {
    let (a, idle, profile) = setup(case);
    let (first, mut quiet, mut preserved) = complete(&a, &idle, &profile, case);
    if poll {
        assert_eq!(first, Applied::Unread);
        assert!(quiet);
        let (again, unchanged, kept) = complete(
            &a,
            &idle,
            &profile,
            Case {
                before: case.current,
                ..case
            },
        );
        assert_eq!(again, Applied::Unread);
        quiet = unchanged;
        preserved &= kept;
    }
    if retry {
        assert_eq!(first, Applied::Unread);
        assert!(quiet);
        let (second, unchanged, kept) = complete(
            &a,
            &idle,
            &profile,
            Case {
                before: case.expected,
                current: case.expected,
                ..case
            },
        );
        assert_eq!(second, Applied::Yes);
        quiet = unchanged;
        preserved &= kept;
    }
    if replay {
        let before = images(&a);
        let bytes = std::fs::read(&profile).unwrap();
        assert_eq!(
            account_land(&idle.sid, &idle.kept, &idle.hooks, landing(case.mode)),
            Applied::Unread
        );
        preserved &= std::fs::read(&profile).unwrap() == bytes;
        quiet = images(&a) == before;
    }
    let events = a.ledger();
    let kept = idle.kept.lock().unwrap();
    [
        ("pending", i64::from(kept.account.flying.is_some())),
        (
            "written",
            i64::try_from(events.iter().filter(|word| **word == "move").count()).unwrap(),
        ),
        ("moved", i64::from(kept.account.moved.is_some())),
        ("notified", i64::try_from(a.asked("made:")).unwrap()),
        ("preserved", i64::from(preserved)),
        (
            "struck",
            i64::try_from(events.iter().filter(|word| **word == "struck").count()).unwrap(),
        ),
        ("quiet", i64::from(quiet)),
    ]
}

fn case(state: &aterm_spec::interp::State, legacy: bool, replaying: bool) -> Case {
    Case {
        expected: state["expected"],
        before: state["before"],
        current: if replaying {
            state["denied"]
        } else {
            state["credential"]
        },
        default: state["default"] == 1,
        mode: state["mode"],
        legacy,
    }
}

pub(crate) fn identity_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Expected" | "Before" | "Credential" | "Mode" => {
            account_landing_identity_environment();
            let key = match action {
                "Expected" => "expected",
                "Before" => "before",
                "Credential" => "credential",
                "Mode" => "mode",
                _ => unreachable!(),
            };
            next.insert(key, state[key] + 1);
        }
        "Default" => {
            account_landing_identity_environment();
            next.insert("default", 1);
        }
        "Correct" => {
            account_landing_identity_environment();
            next.insert("credential", state["expected"]);
            next.insert("phase", 2);
        }
        "Complete" | "Poll" | "Retry" | "Replay" => {
            let poll = action == "Poll" || state["polls"] == 1;
            let replay = action == "Replay";
            let retry = action == "Retry" || (replay && state["phase"] == 3);
            let replaying = action != "Complete";
            let result = run(case(state, false, replaying), poll, retry, replay);
            assert_eq!(
                result,
                run(case(state, true, replaying), poll, retry, replay)
            );
            for (field, value) in result {
                next.insert(field, value);
            }
            next.insert(
                "phase",
                if action == "Complete" || action == "Poll" {
                    1
                } else if action == "Retry" {
                    3
                } else {
                    4
                },
            );
            if action == "Complete" {
                next.insert("denied", state["credential"]);
            }
            if action == "Poll" {
                next.insert("polls", 1);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn landing_identity_conforms_and_correction_completes_once() {
    let model = aterm_models_harness::account_landing_identity_model();
    assert_eq!(model.mutant_dials(), (1..=15).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    // Each step builds its own accounts world, little but file system work, so the steps run on
    // the file system lane's workers ([`aterm_spec::fs_lane::PLACES`]).
    let mut steps = Vec::new();
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                steps.push((state.clone(), action.name));
            }
        }
    }
    aterm_spec::check::each_on(
        aterm_spec::fs_lane::PLACES,
        &steps,
        |&(ref state, action)| {
            let next = identity_transition(state, action);
            assert!(
                model.successors(action, state).contains(&next),
                "{action}: {state:?} -> {next:?}"
            );
        },
    );
    let mut state = model.init_state();
    state.insert("expected", 1);
    state.insert("before", 1);
    state.insert("credential", 2);
    let good = identity_transition(&state, "Complete");
    for (field, value, law) in [
        ("written", 1, "Admission"),
        ("pending", 0, "Pending"),
        ("moved", 1, "Publication"),
        ("notified", 1, "Notification"),
        ("preserved", 0, "Evidence"),
        ("quiet", 0, "Records"),
    ] {
        let mut bad = good.clone();
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad), "{law}");
    }
    let corrected = identity_transition(&good, "Correct");
    let completed = identity_transition(&corrected, "Retry");
    let mut bad = completed.clone();
    bad.insert("written", 0);
    assert!(!model.check_invariant("Progress", &bad));
    let mut bad = identity_transition(&completed, "Replay");
    bad.insert("written", 2);
    assert!(!model.check_invariant("Once", &bad));
}

#[test]
fn default_completion_reads_the_observed_process_home() {
    let case = Case {
        expected: 1,
        before: 1,
        current: 2,
        default: true,
        mode: 1,
        legacy: false,
    };
    let (a, idle, _) = setup(case);
    let home = a.tmp.path().join("completion-home");
    let profile = home.join(".claude.json");
    let wrong = write_profile(&profile, 2);
    let pid = idle.kept.lock().unwrap().snapshot.as_ref().unwrap().pid;
    a.procs.lock().unwrap().get_mut(&pid).unwrap().push(format!(
        "{}={}",
        accounts::HOMEDIR_ENV,
        home.display()
    ));
    let before = images(&a);
    assert_eq!(
        account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Carried),
        Applied::Unread
    );
    assert_eq!(images(&a), before);
    assert_eq!(std::fs::read(&profile).unwrap(), wrong);
    assert!(idle.kept.lock().unwrap().account.flying.is_some());
    assert_eq!(a.asked("made:"), 0);
    write_profile(&profile, 1);
    assert_eq!(
        account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Carried),
        Applied::Yes
    );
    assert_eq!(a.asked("made:"), 1);
    assert_eq!(a.asked("restart:"), 1);
}

#[test]
fn a_bad_target_profile_does_not_strand_an_existing_refusal_or_nonlanding() {
    for rejected in [false, true] {
        let (a, idle) = if rejected {
            let (a, idle, held) = super::account_immediate::blocked_rejection();
            drop(held.lock().unwrap().take());
            (a, idle)
        } else {
            super::account_landing::pending(true)
        };
        let mut roster = accounts::Roster::load(&a.roster_path()).unwrap();
        roster
            .rows
            .iter_mut()
            .find(|row| row.label == "work-2")
            .unwrap()
            .email = Some(email(1).into());
        std::fs::write(
            a.roster_path(),
            accounts::append_rows_for(accounts::Cli::Claude, "", &roster.rows).unwrap(),
        )
        .unwrap();
        let profile = PathBuf::from(a.login_dir("work-2")).join(".claude.json");
        let bytes = write_profile(&profile, 2);
        assert_eq!(
            account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Carried),
            Applied::No
        );
        let kept = idle.kept.lock().unwrap();
        assert!(kept.account.flying.is_none(), "rejected={rejected}");
        assert!(kept.account.moved.is_none(), "rejected={rejected}");
        assert_eq!(a.state().sessions[&idle.sid].account, "main");
        assert_ne!(a.state().active.as_deref(), Some("work-2"));
        assert_eq!(a.asked("made:"), 0);
        assert_eq!(std::fs::read(profile).unwrap(), bytes);
    }
}

#[test]
fn default_completion_keeps_the_observed_custom_oauth_selector() {
    let case = Case {
        expected: 1,
        before: 1,
        current: 2,
        default: true,
        mode: 1,
        legacy: false,
    };
    let (a, idle, _) = setup(case);
    let profile = a.tmp.path().join("home/.claude-custom-oauth.json");
    let wrong = write_profile(&profile, 2);
    let pid = idle.kept.lock().unwrap().snapshot.as_ref().unwrap().pid;
    a.procs
        .lock()
        .unwrap()
        .get_mut(&pid)
        .unwrap()
        .push("CLAUDE_CODE_CUSTOM_OAUTH_URL=https://oauth.example.test".into());
    let before = images(&a);
    assert_eq!(
        account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Carried),
        Applied::Unread
    );
    assert_eq!(images(&a), before);
    assert_eq!(std::fs::read(&profile).unwrap(), wrong);
    assert_eq!(a.asked("made:"), 0);
    write_profile(&profile, 1);
    assert_eq!(
        account_land(&idle.sid, &idle.kept, &idle.hooks, Landing::Carried),
        Applied::Yes
    );
    assert_eq!(a.asked("made:"), 1);
}
