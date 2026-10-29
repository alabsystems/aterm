// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[derive(Clone, Copy)]
struct Case {
    default: bool,
    expected: i64,
    initial: i64,
    current: i64,
    backup: bool,
    legacy: bool,
}

fn email(kind: i64) -> &'static str {
    match kind {
        1 => "one@example.test",
        2 => "two@example.test",
        _ => unreachable!(),
    }
}

fn profile_path(a: &Accts, case: Case) -> PathBuf {
    if case.default {
        if case.legacy {
            a.tmp.path().join("home/.claude/.config.json")
        } else {
            a.tmp.path().join("home/.claude.json")
        }
    } else {
        PathBuf::from(a.login_dir("work-2")).join(if case.legacy {
            ".config.json"
        } else {
            ".claude.json"
        })
    }
}

fn write_profile(path: &Path, kind: i64) -> Vec<u8> {
    let value = if kind == 0 {
        aterm_json::json!({"oauthAccount":{"accountUuid":"opaque-id"}})
    } else {
        aterm_json::json!({"oauthAccount":{"emailAddress":email(kind),"accountUuid":"named-id"}})
    };
    let bytes = aterm_json::to_vec(&value).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, &bytes).unwrap();
    bytes
}

fn setup(case: Case) -> (Arc<Accts>, WorkerIdle, String, PathBuf) {
    let a = Accts::new();
    let to = if case.default { "main" } else { "work-2" };
    let from = if case.default { "work-2" } else { "main" };
    let mut roster = accounts::Roster::load(&a.roster_path()).unwrap();
    if case.expected != 0 {
        let row = roster.rows.iter_mut().find(|row| row.label == to).unwrap();
        row.email = Some("delivery-alias@example.test".into());
        row.identity_email = Some(email(case.expected).to_ascii_uppercase());
    }
    std::fs::write(
        a.roster_path(),
        accounts::append_rows_for(accounts::Cli::Claude, "", &roster.rows).unwrap(),
    )
    .unwrap();
    let path = profile_path(&a, case);
    write_profile(&path, case.initial);
    a.store()
        .transact(a.clock(), &mut None, |state| {
            state.active = Some(from.into());
            state.proven = true;
            if !case.backup {
                state.accounts.entry("pool-a".into()).or_default().auth = accounts::Auth::SignedOut;
            }
            Ok(Vec::new())
        })
        .unwrap();
    a.spawn("s-host-identity", ACCT_TALK_A, from);
    a.wall(ACCT_TALK_A, ACCT_NOW - 10);
    let idle = a.worker("s-host-identity", &on());
    (a, idle, to.into(), path)
}

fn selected(advice: &Option<AccountAdvice>, target: &str) -> i64 {
    match moves(advice) {
        Some(label) if label == target => 1,
        Some("pool-a") => 2,
        None => 0,
        other => panic!("unexpected choice: {other:?}"),
    }
}

fn advice_case(case: Case) -> i64 {
    let (a, idle, target, path) = setup(case);
    let bytes = std::fs::read(&path).unwrap();
    let auth = a.state().account(&target).auth;
    let result = selected(&idle.account(usage_wall()), &target);
    assert_eq!(a.asked("restart:"), 0);
    assert_eq!(a.state().account(&target).auth, auth);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    result
}

fn act_case(case: Case, hold: i64) -> [(&'static str, i64); 6] {
    let (a, idle, target, path) = setup(case);
    assert_eq!(selected(&idle.account(usage_wall()), &target), 1);
    let bytes = write_profile(&path, case.current);
    if hold != 0 {
        a.store()
            .transact(a.clock(), &mut None, |state| {
                state.active = Some(target.clone());
                state.proven = false;
                state.pinned = hold == 1;
                state.canary = (hold == 2).then(|| accounts::Canary {
                    sid: "another-session".into(),
                    at: a.clock(),
                    took: false,
                });
                Ok(Vec::new())
            })
            .unwrap();
    }
    let files = [
        a.store().state_file(),
        a.store().logins_file(),
        a.store().ledger_path(),
        a.store().wake_path(),
    ];
    let before = files.each_ref().map(|file| std::fs::read(file).ok());
    let _ = idle.restart(&move_to(&target, AccountAt::Wall));
    let started = i64::try_from(a.asked("restart:")).unwrap();
    let rotated = i64::try_from(a.state().rotations.len()).unwrap();
    let unchanged = i64::from(files.each_ref().map(|file| std::fs::read(file).ok()) == before);
    let preserved = i64::from(std::fs::read(path).unwrap() == bytes);
    let next = if started == 0 {
        selected(&idle.account(usage_wall()), &target)
    } else {
        1
    };
    [
        ("phase", 2),
        ("started", started),
        ("rotated", rotated),
        ("unchanged", unchanged),
        ("preserved", preserved),
        ("next", next),
    ]
}

fn fixture(state: &aterm_spec::interp::State, legacy: bool, act: bool) -> Case {
    Case {
        default: state["default"] == 1,
        expected: state["expected"],
        initial: if act {
            state["seen"]
        } else {
            state["credential"]
        },
        current: state["credential"],
        backup: state["backup"] == 1,
        legacy,
    }
}

pub(crate) fn identity_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if !matches!(action, "Advice" | "Act") {
        account_host_identity_environment();
    }
    match action {
        "Expected" => {
            next.insert("expected", state["expected"] + 1);
        }
        "Credential" | "Change" => {
            next.insert("credential", state["credential"] + 1);
        }
        "Default" => {
            next.insert("default", 1);
        }
        "Backup" => {
            next.insert("backup", 1);
        }
        "Hold" => {
            next.insert("hold", state["hold"] + 1);
        }
        "Unknown" => {
            next.insert("credential", 0);
        }
        "Advice" => {
            let chosen = advice_case(fixture(state, false, false));
            assert_eq!(chosen, advice_case(fixture(state, true, false)));
            next.insert("seen", state["credential"]);
            next.insert("selected", chosen);
            next.insert("next", chosen);
            next.insert("phase", if chosen == 1 { 1 } else { 2 });
        }
        "Act" => {
            let result = act_case(fixture(state, false, true), state["hold"]);
            assert_eq!(result, act_case(fixture(state, true, true), state["hold"]));
            for (field, value) in result {
                next.insert(field, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn live_claude_identity_conforms_and_keeps_healthy_alternatives_available() {
    let model = aterm_models_harness::account_host_identity_model();
    assert_eq!(model.mutant_dials(), (1..=11).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = identity_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    let mut state = model.init_state();
    state.insert("phase", 1);
    state.insert("expected", 1);
    state.insert("seen", 1);
    state.insert("credential", 2);
    state.insert("selected", 1);
    state.insert("backup", 1);
    let good = identity_transition(&state, "Act");
    for (field, value, law) in [
        ("started", 1, "Admission"),
        ("rotated", 1, "Rotation"),
        ("unchanged", 0, "Records"),
        ("preserved", 0, "Evidence"),
        ("next", 1, "Progress"),
    ] {
        let mut bad = good.clone();
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad), "{law}");
    }
    state.insert("hold", 2);
    let mut bad = identity_transition(&state, "Act");
    bad.insert("next", 2);
    assert!(!model.check_invariant("Holding", &bad));
    let mut state = model.init_state();
    state.insert("expected", 1);
    state.insert("credential", 2);
    state.insert("backup", 1);
    let mut bad = identity_transition(&state, "Advice");
    bad.insert("selected", 1);
    assert!(!model.check_invariant("Choice", &bad));
}

#[test]
fn a_profile_changed_between_selection_and_the_writer_refuses_the_move() {
    for default in [false, true] {
        let case = Case {
            default,
            expected: 1,
            initial: 1,
            current: 2,
            backup: true,
            legacy: false,
        };
        let (a, idle, target, path) = setup(case);
        assert_eq!(selected(&idle.account(usage_wall()), &target), 1);
        let changed = path.clone();
        a.at_seat_read(2, move |_| {
            write_profile(&changed, 2);
        });
        let before = std::fs::read(a.store().state_file()).unwrap();
        let ledger = a.ledger();
        assert_eq!(
            idle.restart(&move_to(&target, AccountAt::Wall)).as_deref(),
            Some("wait:account-changed")
        );
        assert_eq!(a.asked("restart:"), 0);
        assert_eq!(std::fs::read(a.store().state_file()).unwrap(), before);
        assert_eq!(a.ledger(), ledger);
        assert_eq!(selected(&idle.account(usage_wall()), &target), 2);
    }
}

#[test]
fn default_target_identity_uses_the_captured_process_home() {
    let case = Case {
        default: true,
        expected: 1,
        initial: 1,
        current: 2,
        backup: true,
        legacy: false,
    };
    let (a, idle, target, path) = setup(case);
    let before = std::fs::read(&path).unwrap();
    let other_home = a.tmp.path().join("another-home");
    let other = other_home.join(".claude.json");
    let bytes = write_profile(&other, 2);
    let pid = idle.kept.lock().unwrap().snapshot.as_ref().unwrap().pid;
    a.procs
        .lock()
        .unwrap()
        .get_mut(&pid)
        .unwrap()
        .push(format!("HOME={}", other_home.display()));
    assert_eq!(selected(&idle.account(usage_wall()), &target), 2);
    assert_eq!(std::fs::read(other).unwrap(), bytes);
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn contradictory_identity_does_not_bypass_a_pin_or_another_sessions_canary() {
    for pinned in [false, true] {
        let case = Case {
            default: false,
            expected: 1,
            initial: 2,
            current: 2,
            backup: true,
            legacy: false,
        };
        let (a, idle, target, path) = setup(case);
        let bytes = std::fs::read(&path).unwrap();
        a.store()
            .transact(a.clock(), &mut None, |state| {
                state.active = Some(target.clone());
                state.proven = false;
                state.pinned = pinned;
                state.canary = (!pinned).then(|| accounts::Canary {
                    sid: "another-session".into(),
                    at: a.clock(),
                    took: false,
                });
                Ok(Vec::new())
            })
            .unwrap();
        assert_eq!(
            selected(&idle.account(usage_wall()), &target),
            0,
            "pinned={pinned}"
        );
        assert!(a.state().rotations.is_empty());
        assert_eq!(a.state().active.as_deref(), Some(target.as_str()));
        assert_eq!(a.state().pinned, pinned);
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn a_corrected_identity_is_eligible_without_a_persistent_reset() {
    let case = Case {
        default: false,
        expected: 1,
        initial: 2,
        current: 1,
        backup: true,
        legacy: false,
    };
    let (a, idle, target, path) = setup(case);
    let before = a.state().account(&target).auth;
    assert_eq!(selected(&idle.account(usage_wall()), &target), 2);
    write_profile(&path, 1);
    assert_eq!(selected(&idle.account(usage_wall()), &target), 1);
    assert_eq!(a.state().account(&target).auth, before);
}
