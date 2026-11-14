// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::native_act::{ClaimOutcome, Retirement};
use super::super::upgrade::{Agent, Phase};
use super::*;
use aterm_spec::interp::State;

pub(super) fn pending() -> St {
    let image = Image {
        path: b"/model/agent".to_vec(),
        device: 1,
        inode: 2,
    };
    let process = Process {
        pid: 42,
        birth: 1,
        parent: 2,
        group: 42,
        session: 42,
        image: image.clone(),
    };
    let command = native_act::RestartCommand {
        image: image.clone(),
        program: String::from_utf8(image.path.clone()).unwrap(),
        argv: vec!["--resume".into(), "original-conversation".into()],
        env: Vec::new(),
        unset: None,
        cwd: None,
        heal: None,
        dialect: super::super::upgrade::Dialect::Zsh,
        settles: None,
        account_intent: Some(Box::new(
            super::super::accounts_pool::ExecutionIntent::external(),
        )),
    };
    let line = command.legacy_line().unwrap();
    let prepare = Request::prepare(Prepare {
        receipt_mode: None,
        rejection_domain: None,
        asked: None,
        id: "ab".repeat(16),
        host_epoch: "ef".repeat(16),
        holder: "model-holder".into(),
        revision: 1,
        expected_human_seq: Some(0),
        deadline_us: 100,
        duty: Scope::Upgrade,
        plan: NativePlan::Restart {
            cold: None,
            process: process.clone(),
            observation: None,
            shell: Process { pid: 43, ..process },
            launcher: image,
            socket: "/model/control.sock".into(),
            forward: Box::new(command.clone()),
            rollback: Some(Box::new(command)),
        },
        boot: None,
        custody: None,
    })
    .encode()
    .unwrap();
    assert!(matches!(
        Request::parse(&prepare),
        Some(Request::Prepare(_))
    ));
    St {
        agent: Agent::Claude,
        phase: Phase::Exiting { at_s: 7 },
        pending_since: 3,
        salt: 4,
        native_coordination: true,
        native_prepare: prepare,
        native: Some(Binding {
            id: "ab".repeat(16),
            capability: "cd".repeat(32),
        }),
        native_rollback: true,
        login: Some("retained@example.test".into()),
        account_intent: Some(super::super::accounts_pool::ExecutionIntent::external()),
        back_account_intent: Some(super::super::accounts_pool::ExecutionIntent::external()),
        line: line.clone(),
        back_line: line,
        from: "1.0.1".into(),
        to: "1.0.2".into(),
        source: "native".into(),
        ..St::default()
    }
}

fn reply(st: &mut St, retirement: Retirement) {
    terminal_reply(st, &Reply::Decision(Decision::Retired(retirement)));
}

/// The pending record, or the one whose admission reply was lost: only its wire is kept.
fn baseline(bound: bool) -> St {
    let mut st = pending();
    if !bound {
        st.native = None;
    }
    st
}

fn asked_baseline(bound: bool) -> St {
    let mut st = baseline(bound);
    let mut prepare = retained_prepare(&st).unwrap();
    // These are passive client records, not host qualifications or a CheckedSource.
    let mut previous = *prepare.clone();
    previous.id = "cd".repeat(16);
    st.native_completed_prepare = Some(Box::new(previous.clone()));
    previous.id = "de".repeat(16);
    st.native_noexec_prepare = Some(Box::new(previous));
    prepare.asked = Some(native_act::AskedIntent::Upgrade(
        native_act::AskedUpgradeIntent {
            predecessor: "cd".repeat(16),
        },
    ));
    st.native_prepare = Request::prepare(*prepare).encode().unwrap();
    st
}

fn unchanged_for(st: &St, bound: bool, asked: bool) -> bool {
    let mut copy = st.clone();
    copy.native_retired_unissued = false;
    copy == if asked {
        asked_baseline(bound)
    } else {
        baseline(bound)
    }
}

fn unchanged(st: &St, bound: bool) -> bool {
    unchanged_for(st, bound, false)
}

fn unadmitted_reply(st: &mut St) -> Result<Recovery, &'static str> {
    recover_reply(st, Reply::Unadmitted("ab".repeat(16)))
}

fn closed_reply(st: &mut St) -> Option<Recovery> {
    terminal_reply(st, &Reply::Closed("ab".repeat(16)))
}

pub(super) fn transition(state: &State, action: &str) -> State {
    let mut asked = state["asked"] == 1;
    let mut st = if asked {
        asked_baseline(state["bound"] == 1)
    } else {
        baseline(state["bound"] == 1)
    };
    if state["closed"] == 1 {
        assert_eq!(closed_reply(&mut st), Some(Recovery::Closed));
    } else if state["kept"] == 0 && state["observed"] == 0 && state["finished"] == 0 {
        assert_eq!(unadmitted_reply(&mut st), Ok(Recovery::Unadmitted));
    }
    if state["held"] == 1 {
        reply(&mut st, Retirement::NotGranted);
    } else if state["finished"] == 1 {
        reply(&mut st, Retirement::Exited);
    }
    let mut dispatched = false;
    // A lapsed answer is only for a cold or typed asked restart: the plain record is cold then.
    let mut cold = false;
    match action {
        "Ask" => {
            asked = true;
            st = asked_baseline(state["bound"] == 1);
        }
        "NotGranted" => reply(&mut st, Retirement::NotGranted),
        "Unknown" => {
            assert_eq!(
                recover_reply(&mut st, Reply::Decision(Decision::Unknown)),
                Err("native-unknown")
            );
        }
        "Exited" => reply(&mut st, Retirement::Exited),
        "Completed" => {
            terminal_reply(&mut st, &Reply::Committed(ClaimOutcome::Builtin));
            assert!(st.native_completed_prepare.is_some());
        }
        "Replay" => {
            for reply in [
                Reply::Committed(ClaimOutcome::Builtin),
                Reply::Decision(Decision::Retired(Retirement::Exited)),
                Reply::Decision(Decision::ProvenNotCommittedRollbackSafe),
            ] {
                let before = st.clone();
                terminal_reply(&mut st, &reply);
                assert_eq!(st, before, "a later terminal reply cannot replace the hold");
            }
        }
        "Reload" => st = St::from_json(&st.to_json()).expect("the real saved record reloads"),
        "Retarget" => {
            let mut replacement = St {
                line: "different conversation".into(),
                ..St::default()
            };
            st.kept_by_restart("tab", &mut replacement);
            st = replacement;
        }
        "Expire" => st.stop("no-resume", 100_000),
        "Rollback" => dispatched = st.roll_back(100_000, 0),
        "LoseAdmission" => st.native = None,
        "Unadmitted" => {
            let before = st.clone();
            let outcome = unadmitted_reply(&mut st);
            assert_eq!(
                outcome == Ok(Recovery::Unadmitted),
                st != before,
                "an unadmitted answer changes the record only when it is consumed"
            );
            if st.native.is_some() || st.native_retired_unissued {
                assert_eq!(st, before, "a binding or a hold keeps its record");
            }
        }
        "Lapsed" => {
            if !asked {
                cold = true;
                st = cold_baseline(state["bound"] == 1);
            }
            assert_eq!(lapsed_reply(&mut st), Ok(Recovery::Lapsed));
        }
        "Closed" => assert_eq!(closed_reply(&mut st), Some(Recovery::Closed)),
        _ => unreachable!("unmapped model action"),
    }
    let mut next = state.clone();
    next.insert("held", i64::from(st.native_retired_unissued));
    next.insert(
        "observed",
        // This is input history, independent of the implementation's mutable
        // hold bit. Deriving it from that bit would make NoRevival vacuous.
        i64::from(state["observed"] == 1 || action == "NotGranted"),
    );
    let bound = st.native.is_some();
    next.insert("bound", i64::from(bound));
    let kept = if cold {
        unchanged_cold(&st, bound)
    } else {
        unchanged_for(&st, bound, asked)
    };
    next.insert("kept", i64::from(kept));
    next.insert("asked", i64::from(asked));
    next.insert("owed", i64::from(matches!(st.phase, Phase::Exiting { .. })));
    next.insert("dispatched", i64::from(dispatched));
    next.insert(
        "finished",
        i64::from(!matches!(st.phase, Phase::Exiting { .. })),
    );
    // Input history, as `observed` is: the watch bit alone would make its law vacuous.
    next.insert(
        "closed",
        i64::from(state["closed"] == 1 || action == "Closed"),
    );
    next.insert("watch", i64::from(st.native_closed.is_some()));
    next
}

#[test]
fn retirement_debt_refines_saved_state_and_recovery_model() {
    let model = aterm_models_harness::native_retirement_debt_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
}

#[test]
fn retirement_debt_model_catches_the_old_terminal_failure_path() {
    let model = aterm_models_harness::native_retirement_debt_model();
    let mut st = pending();
    reply(&mut st, Retirement::NotGranted);
    // Negative control: perform the old real terminal path after losing its hold.
    st.native_retired_unissued = false;
    reply(&mut st, Retirement::Exited);
    let mut bad = model.init_state();
    bad.insert("observed", 1);
    bad.insert("held", i64::from(st.native_retired_unissued));
    bad.insert("kept", i64::from(unchanged(&st, true)));
    bad.insert("owed", i64::from(matches!(st.phase, Phase::Exiting { .. })));
    for invariant in ["NoRevival", "RecipeRetained", "DebtRetained"] {
        assert!(!model.check_invariant(invariant, &bad), "{invariant}");
    }
}

#[test]
fn retirement_debt_model_catches_an_unadmitted_clear_of_a_binding_or_a_hold() {
    let model = aterm_models_harness::native_retirement_debt_model();
    // Negative control: the clear without its guards, on a record that holds its binding.
    let mut st = pending();
    st.native_prepare.clear();
    let mut bad = model.init_state();
    bad.insert("bound", i64::from(st.native.is_some()));
    bad.insert("kept", i64::from(unchanged(&st, true)));
    assert!(!model.check_invariant("BindingRetained", &bad));
    // And on a held record whose admission reply was lost: the clear drops the hold too.
    let mut st = baseline(false);
    reply(&mut st, Retirement::NotGranted);
    st.native_retired_unissued = false;
    st.native_prepare.clear();
    let mut bad = model.init_state();
    bad.insert("observed", 1);
    bad.insert("bound", 0);
    bad.insert("held", i64::from(st.native_retired_unissued));
    bad.insert("kept", i64::from(unchanged(&st, false)));
    for invariant in ["NoRevival", "RecipeRetained"] {
        assert!(!model.check_invariant(invariant, &bad), "{invariant}");
    }
    // The real consumer refuses both, and refuses an answer about another id.
    let mut st = pending();
    assert_eq!(unadmitted_reply(&mut st), Err("native-unknown"));
    assert_eq!(st, pending());
    let mut st = baseline(false);
    reply(&mut st, Retirement::NotGranted);
    let held = st.clone();
    assert_eq!(unadmitted_reply(&mut st), Ok(Recovery::Held));
    assert_eq!(st, held);
    let mut st = baseline(false);
    assert_eq!(
        recover_reply(&mut st, Reply::Unadmitted("cd".repeat(16))),
        Err("native-unknown")
    );
    assert_eq!(st, baseline(false));
}

#[test]
fn retirement_debt_model_catches_a_closed_answer_that_leaves_no_watch() {
    let model = aterm_models_harness::native_retirement_debt_model();
    for bound in [false, true] {
        let mut st = baseline(bound);
        assert_eq!(closed_reply(&mut st), Some(Recovery::Closed));
        assert!(
            st.native_closed.is_some(),
            "the real answer keeps its watch"
        );
        // Negative control: the answer's clear without its watch, today's lapse-like drop.
        st.native_closed = None;
        let mut bad = model.init_state();
        bad.insert("bound", i64::from(st.native.is_some()));
        bad.insert("kept", i64::from(unchanged(&st, bound)));
        bad.insert("closed", 1);
        bad.insert("watch", i64::from(st.native_closed.is_some()));
        assert!(!model.check_invariant("ClosedLeavesAWatch", &bad));
    }
}

#[test]
fn an_unadmitted_answer_never_drops_a_terminal_record_of_the_same_id() {
    let Request::Prepare(prepare) = Request::parse(&pending().native_prepare).unwrap() else {
        panic!("the pending prepare")
    };
    for completed in [false, true] {
        let mut st = baseline(false);
        if completed {
            st.native_completed_prepare = Some(prepare.clone());
        } else {
            st.native_noexec_prepare = Some(prepare.clone());
        }
        let before = st.clone();
        assert_eq!(unadmitted_reply(&mut st), Err("native-unknown"));
        assert_eq!(st, before);
    }
    let mut st = baseline(false);
    assert_eq!(unadmitted_reply(&mut st), Ok(Recovery::Unadmitted));
    assert!(st.native_prepare.is_empty() && st.native.is_none() && !st.native_rollback);
    assert!(
        st.native_coordination && st.native_completed_prepare.is_none(),
        "nothing is recorded completed; the coordination stays"
    );
    assert!(matches!(st.phase, Phase::Exiting { .. }));
}

#[test]
fn an_unbound_typed_asked_answer_clears_only_its_exact_wire_and_rollback() {
    for relaunch in [false, true] {
        let mut st = asked_baseline(false);
        if relaunch {
            let mut prepare = retained_prepare(&st).unwrap();
            prepare.duty = Scope::Relaunch;
            prepare.asked = Some(native_act::AskedIntent::Relaunch(
                native_act::AskedRelaunchIntent {
                    predecessor: "cd".repeat(16),
                    ask: "memory:unadmitted-client-control".into(),
                },
            ));
            st.native_prepare = Request::prepare(*prepare).encode().unwrap();
        }
        let mut expected = st.clone();
        expected.native_prepare.clear();
        expected.native_rollback = false;
        assert_eq!(unadmitted_reply(&mut st), Ok(Recovery::Unadmitted));
        assert_eq!(st, expected);
        assert!(st.native_coordination);
        assert_eq!(
            st.native_completed_prepare.as_ref().unwrap().id,
            "cd".repeat(16)
        );
        assert_eq!(
            st.native_noexec_prepare.as_ref().unwrap().id,
            "de".repeat(16)
        );
    }
    for completed in [false, true] {
        let mut st = asked_baseline(false);
        let prepare = retained_prepare(&st).unwrap();
        if completed {
            st.native_completed_prepare = Some(prepare);
        } else {
            st.native_noexec_prepare = Some(prepare);
        }
        let before = st.clone();
        assert_eq!(unadmitted_reply(&mut st), Err("native-unknown"));
        assert_eq!(st, before);
    }
    let model = aterm_models_harness::native_retirement_debt_model();
    let mut before = model.init_state();
    assert!(model.fire("Ask", &mut before));
    let mut st = asked_baseline(true);
    let original = st.clone();
    assert_eq!(unadmitted_reply(&mut st), Err("native-unknown"));
    assert_eq!(st, original);
    // The same guard-removal mutant must also falsify the asked recipe's binding law.
    st.native_prepare.clear();
    before.insert("kept", i64::from(unchanged_for(&st, true, true)));
    assert!(!model.check_invariant("BindingRetained", &before));
}

fn lapsed_reply(st: &mut St) -> Result<Recovery, &'static str> {
    recover_reply(st, Reply::Lapsed("ab".repeat(16)))
}

/// The baseline record with its prepare made a cold crash restart, the kind (with a typed asked
/// one) a later host epoch answers `lapsed`.
fn cold_baseline(bound: bool) -> St {
    let mut st = baseline(bound);
    let mut prepare = retained_prepare(&st).unwrap();
    prepare.duty = Scope::Relaunch;
    let NativePlan::Restart { cold, .. } = &mut prepare.plan else {
        unreachable!()
    };
    *cold = Some(Box::new(native_act::ColdIntent {
        login: None,
        home: None,
        auth_observed: true,
        route: super::super::accounts::AuthRoute::Stored,
        conversation: None,
        profile: None,
        mode: super::super::accounts::Mode::Wall,
        billing: native_act::ColdBilling::Keep,
    }));
    st.native_prepare = Request::prepare(*prepare).encode().unwrap();
    st
}

/// [`unchanged_for`] of the cold record.
fn unchanged_cold(st: &St, bound: bool) -> bool {
    let mut copy = st.clone();
    copy.native_retired_unissued = false;
    copy == cold_baseline(bound)
}

#[test]
fn a_lapsed_answer_drops_exactly_the_operation_of_a_cold_or_asked_restart_bound_or_not() {
    for asked in [false, true] {
        for bound in [false, true] {
            let mut st = if asked {
                asked_baseline(bound)
            } else {
                cold_baseline(bound)
            };
            let mut expected = st.clone();
            expected.native = None;
            expected.native_prepare.clear();
            expected.native_rollback = false;
            assert_eq!(
                lapsed_reply(&mut st),
                Ok(Recovery::Lapsed),
                "asked={asked} bound={bound}"
            );
            assert_eq!(
                st, expected,
                "asked={asked} bound={bound}: only the operation, its binding and its phase go"
            );
            assert_eq!(refusal_word(Ok(Recovery::Lapsed)), "native-lapsed");
        }
    }
}

#[test]
fn a_lapsed_answer_never_drops_a_private_restart_an_unsupported_ask_history_or_a_hold() {
    let refused = |mut st: St, why: &str| {
        assert!(retained_prepare(&st).is_ok(), "{why}: the guard is reached");
        let before = st.clone();
        assert_eq!(lapsed_reply(&mut st), Err("native-unknown"), "{why}");
        assert_eq!(st, before, "{why}");
    };
    // A private restart (the renewal's kind) is not one the host answers.
    for bound in [false, true] {
        refused(baseline(bound), "private restart");
    }
    // A checked relaunch whose forward settles on another build is not a supported asked shape.
    let mut st = asked_baseline(false);
    let mut prepare = retained_prepare(&st).unwrap();
    prepare.duty = Scope::Relaunch;
    prepare.asked = Some(native_act::AskedIntent::Relaunch(
        native_act::AskedRelaunchIntent {
            predecessor: "cd".repeat(16),
            ask: "memory@1".into(),
        },
    ));
    let NativePlan::Restart {
        forward, process, ..
    } = &mut prepare.plan
    else {
        unreachable!()
    };
    forward.settles = Some(process.image.clone());
    assert!(!prepare.asked_restart_shape());
    st.native_prepare = Request::prepare(*prepare).encode().unwrap();
    refused(st, "unsupported asked shape");
    // A terminal record of the same id is a completion or a positive no-grant.
    for asked in [false, true] {
        for completed in [false, true] {
            let mut st = if asked {
                asked_baseline(false)
            } else {
                cold_baseline(false)
            };
            let prepare = retained_prepare(&st).unwrap();
            if completed {
                st.native_completed_prepare = Some(prepare);
            } else {
                st.native_noexec_prepare = Some(prepare);
            }
            refused(st, "terminal history");
        }
    }
    // An answer about another operation.
    let mut st = cold_baseline(false);
    let before = st.clone();
    assert_eq!(
        recover_reply(&mut st, Reply::Lapsed("cd".repeat(16))),
        Err("native-unknown")
    );
    assert_eq!(st, before);
    // Private restart debt stays held whatever a later epoch answers.
    let mut st = baseline(false);
    reply(&mut st, Retirement::NotGranted);
    let held = st.clone();
    assert_eq!(lapsed_reply(&mut st), Ok(Recovery::Held));
    assert_eq!(st, held);
}

pub(super) fn cold_transition(state: &State, action: &str) -> State {
    let mut cold = state["cold"] == 1;
    let mut candidate = state["candidate"] == 1;
    if action == "Cold" {
        cold = true;
    }
    if action == "Account" {
        candidate = true;
    }
    let mut st = if cold {
        cold_baseline(true)
    } else {
        baseline(true)
    };
    if candidate {
        st.native_account_candidate = Some(super::super::native_account_handoff::Pending::read(
            &aterm_json::Value::Null,
        ));
    }
    if state["held"] == 1 || action == "PriorRetirement" {
        reply(&mut st, Retirement::NotGranted);
    }
    match action {
        "Cold" | "Account" | "PriorRetirement" => {}
        "Unknown" => {
            let before = st.clone();
            let _ = recover_reply(&mut st, Reply::Decision(Decision::Unknown));
            assert_eq!(st, before);
        }
        "NoGrant" => {
            let got = recover_reply(
                &mut st,
                Reply::Decision(Decision::Retired(Retirement::NotGranted)),
            );
            assert_eq!(
                got,
                Ok(if cold && !candidate {
                    Recovery::Lapsed
                } else {
                    Recovery::Held
                })
            );
        }
        _ => unreachable!("unmapped cold-retirement action"),
    }
    let mut next = state.clone();
    next.insert("cold", i64::from(cold));
    next.insert("candidate", i64::from(candidate));
    next.insert("held", i64::from(st.native_retired_unissued));
    next.insert("proof", i64::from(action == "NoGrant"));
    next.insert(
        "closed",
        i64::from(st.native_prepare.is_empty() && st.native.is_none()),
    );
    next
}

#[test]
fn cold_retirement_recovery_refines_its_model() {
    let model = aterm_models_harness::cold_retirement_recovery_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = cold_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
}

#[test]
fn a_hold_with_missing_recipe_remains_inert_instead_of_becoming_a_new_round() {
    let bare = St::from_json(r#"{"phase":"done","native_retired_unissued":true}"#).unwrap();
    assert!(bare.native_coordination && coordinated(&bare));
    assert!(
        !St::from_json(r#"{"phase":"done","native_retired_unissued":false}"#)
            .unwrap()
            .native_coordination
    );
    let mut st = pending();
    assert!(st.stuck(u64::MAX).is_some());
    reply(&mut st, Retirement::NotGranted);
    assert!(st.stuck(u64::MAX).is_none());
    st.native_prepare.clear();
    st.native = None;
    st.phase = Phase::Done;
    st.confirm_by = 1;
    let loaded = St::from_json(&st.to_json()).unwrap();
    assert!(loaded.in_flight());
    assert!(!loaded.confirming());
    assert!(!super::super::upgrade_drive::settled(&loaded));
    assert!(coordinated(&loaded));
    let mut new = St::default();
    loaded.kept_by_restart("another-tab", &mut new);
    assert_eq!(new, loaded);
}
