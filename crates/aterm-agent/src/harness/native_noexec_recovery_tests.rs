// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::codex_renewal::{self, RestartCompletion};
use aterm_spec::interp::State;

fn fixture(previous: bool, expired: bool) -> (St, Prepare) {
    let (mut st, expected) = super::completion::fixture(false, false, !expired);
    st.phase = Phase::Exiting { at_s: 1 };
    if previous {
        let previous = st.native_completed_prepare.as_mut().unwrap();
        previous.id = "12".repeat(16);
    } else {
        st.native_completed_prepare = None;
        st.native_observation = None;
    }
    st.native_prepare = Request::prepare(expected.clone()).encode().unwrap();
    st.native = Some(Binding {
        id: expected.id.clone(),
        capability: binding().capability,
    });
    assert_eq!(
        terminal_reply(
            &mut st,
            &Reply::Decision(Decision::ProvenNotCommittedRollbackSafe)
        ),
        Some(Recovery::Cancelled)
    );
    (st, expected)
}

fn corrupt(st: &mut St, expected: &mut Prepare, fault: i64) {
    match fault {
        0 | 1 => {}
        2 => expected.deadline_us += 1,
        3 => expected.id = "cd".repeat(16),
        4 => st.native = Some(binding()),
        5 => st.native_prepare = Request::prepare(expected.clone()).encode().unwrap(),
        6 => st.native_retired_unissued = true,
        7 => {
            st.native_account_candidate = Some(
                crate::harness::native_account_handoff::Pending::read(&aterm_json::Value::Null),
            )
        }
        8 => st.native_rollback = true,
        9 => st.native_completed_prepare = Some(Box::new(expected.clone())),
        10 => {
            st.native_observation = super::completion::fixture(false, false, false)
                .0
                .native_observation
        }
        11 => st.native_noexec_prepare = None,
        12 => st.tab = "another-session".into(),
        13 => expected.host_epoch = "malformed".into(),
        14 => st.native_coordination = false,
        15 => st.phase = Phase::Relaunched { at_s: 1 },
        _ => unreachable!(),
    }
}

fn attempt(model: &State, buggy: i64) -> (bool, usize) {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut options = opts(root.path());
    let (initial, mut expected) = fixture(model["fault"] == 1, model["expired"] == 1);
    let key = format!("renewal-{}", expected.id);
    upgrade_drive::save_checked(&options, &key, &initial).unwrap();
    let mut st = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
    corrupt(&mut st, &mut expected, model["fault"]);
    options.dry_run = model["dry"] == 1;
    let failure = (model["barrier_failed"] == 1)
        .then(|| aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported));
    let (mut client, worker) = endpoint(|_| None);
    let returned = match buggy {
        1..=3 => {
            drop(client);
            true
        }
        4 => {
            let _reply = native_act::exchange(&mut client, SID, &Request::Signal(binding()));
            drop(client);
            true
        }
        _ => {
            let result =
                codex_renewal::drive_connection(&options, SID, &expected, st, client, true);
            let accepted = if model["fault"] >= 2 {
                result.is_some()
            } else {
                matches!(result, Some(RestartCompletion::NotStarted))
            };
            accepted && buggy != 5
        }
    };
    drop(failure);
    if buggy == 0 {
        let after = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
        assert_eq!(after.native_noexec_prepare, initial.native_noexec_prepare);
        assert_eq!(
            after.native_completed_prepare,
            initial.native_completed_prepare
        );
        assert_eq!(after.native_observation, initial.native_observation);
        assert!(after.native.is_none() && after.native_prepare.is_empty());
    }
    (returned, worker.join().unwrap().len())
}

fn observed(state: &State, returned: bool, requests: usize) -> State {
    let mut next = state.clone();
    next.insert("attempts", state["attempts"] + 1);
    for (bad, flag) in [
        (state["fault"] >= 2 && returned, "invalid"),
        (state["barrier_failed"] == 1 && returned, "unpublished"),
        (state["dry"] == 1 && returned, "dry_result"),
        (requests != 0, "replayed"),
        (
            state["fault"] < 2 && state["dry"] == 0 && state["barrier_failed"] == 0 && !returned,
            "lost",
        ),
    ] {
        if bad {
            next.insert(flag, 1);
        }
    }
    next
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    if action == "Attempt" {
        let (returned, requests) = attempt(state, 0);
        observed(state, returned, requests)
    } else {
        let mut next = state.clone();
        match action {
            "NextFault" => next.insert("fault", state["fault"] + 1),
            "Expire" => next.insert("expired", 1),
            "Dry" => next.insert("dry", 1),
            "Live" => next.insert("dry", 0),
            "FailBarrier" => next.insert("barrier_failed", 1),
            "RepairBarrier" => next.insert("barrier_failed", 0),
            _ => unreachable!(),
        };
        next
    }
}

#[test]
fn noexec_recovery_refines_real_validation_barriers_and_transport() {
    let model = aterm_models_harness::native_noexec_recovery_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let after = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&after),
                    "{}: {state:?} -> {after:?}",
                    action.name
                );
            }
        }
    }
}

#[test]
fn noexec_negative_controls_break_each_law() {
    let model = aterm_models_harness::native_noexec_recovery_model();
    for (buggy, fault, dry, barrier, law) in [
        (1, 2, 0, 0, "PositiveExactEvidence"),
        (2, 0, 0, 1, "DurableNoexec"),
        (3, 0, 1, 0, "DryRunIsInert"),
        (4, 0, 0, 0, "NoReplay"),
        (5, 0, 0, 0, "PositiveResultRetained"),
    ] {
        let mut state = model.init_state();
        state.insert("fault", fault);
        state.insert("dry", dry);
        state.insert("barrier_failed", barrier);
        let (returned, requests) = attempt(&state, buggy);
        let after = observed(&state, returned, requests);
        assert!(!model.check_invariant(law, &after), "{law}");
        assert!(
            !model.successors("Attempt", &state).contains(&after),
            "{law}"
        );
    }
}

#[test]
fn positive_noexec_survives_reload_without_a_host_or_another_signal() {
    let root = aterm_tempfile::tempdir().unwrap();
    let options = opts(root.path());
    let mut state = bound_record(&options);
    let Request::Prepare(mut expected) = Request::parse(&state.native_prepare).unwrap() else {
        unreachable!()
    };
    // The host can positively close an expired original operation. Its durable
    // result must remain readable without renewing that operation's deadline.
    expected.deadline_us = 1;
    state.native_prepare = Request::prepare((*expected).clone()).encode().unwrap();
    let key = format!("renewal-{}", expected.id);
    upgrade_drive::save_checked(&options, &key, &state).unwrap();
    let state = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
    let (client, worker) = endpoint(|line| {
        Some(match request(line) {
            Request::Signal(_) => "OK native-act state=unknown\n".into(),
            Request::Prepare(_) => "OK native-act state=rollback-safe\n".into(),
            other => panic!("unexpected NoExec recovery request: {other:?}"),
        })
    });
    assert!(matches!(
        codex_renewal::drive_connection(&options, SID, &expected, state, client, true),
        Some(RestartCompletion::NotStarted)
    ));
    let seen = worker.join().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(matches!(request(&seen[0]), Request::Signal(_)));
    assert!(matches!(request(&seen[1]), Request::Prepare(_)));
    let saved = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
    assert!(saved.native.is_none() && saved.native_prepare.is_empty());
    assert_eq!(saved.native_noexec_prepare.as_ref(), Some(&expected));
    assert!(codex_renewal::prepare_restart(&options, SID, &expected).is_ok());
    assert!(matches!(
        codex_renewal::drive_restart(&options, SID, &expected),
        Some(RestartCompletion::NotStarted)
    ));
    assert!(!Path::new(options.sock.as_ref().unwrap()).exists());
}

#[test]
fn noexec_failed_barrier_can_be_repaired_after_reload_but_not_by_a_stale_writer() {
    let root = aterm_tempfile::tempdir().unwrap();
    let options = opts(root.path());
    let (initial, expected) = fixture(true, true);
    let key = format!("renewal-{}", expected.id);
    upgrade_drive::save_checked(&options, &key, &initial).unwrap();
    let original = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
    let stale = original.clone();
    let failure = aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported);
    assert!(codex_renewal::recover_completion(&options, SID, &expected, &original).is_none());
    let reloaded = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
    assert_eq!(
        reloaded.native_noexec_prepare,
        original.native_noexec_prepare
    );
    drop(failure);
    assert!(codex_renewal::recover_completion(&options, SID, &expected, &stale).is_none());
    assert!(matches!(
        codex_renewal::recover_completion(&options, SID, &expected, &reloaded),
        Some(RestartCompletion::NotStarted)
    ));
    assert!(codex_renewal::recover_completion(&options, SID, &expected, &original).is_none());
    assert!(matches!(
        codex_renewal::restart_completion(&options, SID, &expected),
        Some(RestartCompletion::NotStarted)
    ));
    assert!(codex_renewal::completed_restart(&options, SID, &expected).is_none());
}

#[test]
fn noexec_closed_replies_and_generic_completion_are_inert() {
    for previous in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let options = opts(root.path());
        let (mut st, expected) = fixture(previous, true);
        let before = st.clone();
        for reply in [
            Reply::Committed(native_act::ClaimOutcome::Builtin),
            Reply::Decision(Decision::Retired(native_act::Retirement::NotGranted)),
            Reply::Decision(Decision::Retired(native_act::Retirement::Exited)),
            Reply::Decision(Decision::Unknown),
        ] {
            assert_eq!(terminal_reply(&mut st, &reply), None);
            assert_eq!(st, before);
        }
        let (mut client, worker) = endpoint(|_| panic!("closed NoExec dispatched"));
        assert!(!complete(
            &options,
            "conversation",
            &mut client,
            SID,
            &mut st
        ));
        assert_eq!(
            recover(&options, "conversation", &mut client, SID, &mut st),
            Ok(Recovery::Cancelled)
        );
        assert_eq!(st.native_noexec_prepare.as_deref(), Some(&expected));
        drop(client);
        assert!(worker.join().unwrap().is_empty());
    }
    let (mut st, _) = super::completion::fixture(true, false, false);
    let before = st.clone();
    assert_eq!(
        terminal_reply(
            &mut st,
            &Reply::Decision(Decision::ProvenNotCommittedRollbackSafe)
        ),
        None
    );
    assert_eq!(
        st, before,
        "late cancellation must not erase completed rollback"
    );
}

#[test]
fn noexec_direct_readers_are_dry_and_malformed_records_are_rejected() {
    for (st, expected) in [
        fixture(false, true),
        super::completion::fixture(false, false, false),
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut options = opts(root.path());
        let key = format!("renewal-{}", expected.id);
        upgrade_drive::save_checked(&options, &key, &st).unwrap();
        let st = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
        let path = options.state.join("upgrade").join(format!("{key}.json"));
        let before = std::fs::read(&path).unwrap();
        options.dry_run = true;
        assert!(codex_renewal::recover_completion(&options, SID, &expected, &st).is_none());
        assert!(codex_renewal::restart_completion(&options, SID, &expected).is_none());
        assert!(codex_renewal::completed_restart(&options, SID, &expected).is_none());
        assert!(codex_renewal::prepare_restart(&options, SID, &expected).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        options.dry_run = false;
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let options = opts(root.path());
    let (st, expected) = fixture(false, true);
    for invalid in [
        aterm_json::Value::Null,
        aterm_json::Value::from("invalid"),
        aterm_json::Value::from(Request::Query(binding()).encode().unwrap()),
    ] {
        let mut value: aterm_json::Value = aterm_json::from_str(&st.to_json()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("native_noexec_prepare".into(), invalid);
        assert!(St::from_json(&value.to_string()).is_none());
    }
    let mut contradictory = st;
    contradictory.native_completed_prepare = Some(Box::new(expected.clone()));
    assert!(codex_renewal::recover_completion(&options, SID, &expected, &contradictory).is_none());
    let (mut completed, expected) = super::completion::fixture(false, false, false);
    completed.native_noexec_prepare = Some(Box::new(expected.clone()));
    assert!(codex_renewal::recover_completion(&options, SID, &expected, &completed).is_none());
    let key = format!("renewal-{}", expected.id);
    upgrade_drive::save_checked(&options, &key, &completed).unwrap();
    assert!(codex_renewal::completed_restart(&options, SID, &expected).is_none());
}

#[test]
fn noexec_contradictory_or_malformed_active_replies_cannot_rewrite_history() {
    let (closed, expected) = fixture(false, true);
    for fault in 0..3 {
        for reply in [
            Reply::Committed(native_act::ClaimOutcome::Builtin),
            Reply::Decision(Decision::Retired(native_act::Retirement::Exited)),
        ] {
            let mut st = closed.clone();
            st.native_prepare = Request::prepare(expected.clone()).encode().unwrap();
            if fault == 1 {
                st.native_prepare = "unreadable active plan".into();
            } else if fault == 2 {
                st.native = Some(Binding {
                    id: "fe".repeat(16),
                    capability: binding().capability,
                });
            }
            let before = st.clone();
            assert_eq!(terminal_reply(&mut st, &reply), None);
            assert_eq!(st, before);
        }
    }
}

#[test]
fn cold_predecessor_cannot_authorize_a_cold_reply_for_an_active_warm_plan() {
    let (mut st, mut warm) = super::completion::fixture(false, false, false);
    let NativePlan::Restart { cold, .. } = &mut st.native_completed_prepare.as_mut().unwrap().plan
    else {
        unreachable!()
    };
    *cold = Some(Box::new(native_act::ColdIntent {
        login: None,
        home: None,
        auth_observed: true,
        route: crate::harness::accounts::AuthRoute::Stored,
        conversation: Some("conversation".into()),
        profile: None,
        mode: crate::harness::accounts::Mode::Wall,
        billing: native_act::ColdBilling::Keep,
    }));
    warm.id = "cd".repeat(16);
    st.native_prepare = Request::prepare(warm.clone()).encode().unwrap();
    st.native = Some(Binding {
        id: warm.id,
        capability: binding().capability,
    });
    let before = st.clone();
    assert_eq!(
        terminal_reply(
            &mut st,
            &Reply::ColdRestartCommitted {
                claim: native_act::ClaimOutcome::Builtin,
                observation: None,
                intent: Box::new(crate::harness::accounts_pool::ExecutionIntent::external()),
            }
        ),
        None
    );
    assert_eq!(st, before);
}

#[test]
fn noexec_terminal_publication_failure_keeps_recoverable_positive_evidence() {
    let root = aterm_tempfile::tempdir().unwrap();
    let options = opts(root.path());
    let initial = bound_record(&options);
    let expected = retained_prepare(&initial).unwrap();
    let key = format!("renewal-{}", expected.id);
    upgrade_drive::save_checked(&options, &key, &initial).unwrap();
    let mut current = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
    assert_eq!(
        terminal_reply(
            &mut current,
            &Reply::Decision(Decision::ProvenNotCommittedRollbackSafe)
        ),
        Some(Recovery::Cancelled)
    );
    let fail = aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported);
    assert!(upgrade_drive::save_checked(&options, &key, &current).is_err());
    let visible = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
    assert_eq!(visible.native_noexec_prepare.as_ref(), Some(&expected));
    assert!(codex_renewal::restart_completion(&options, SID, &expected).is_none());
    drop(fail);
    assert!(matches!(
        codex_renewal::restart_completion(&options, SID, &expected),
        Some(RestartCompletion::NotStarted)
    ));
}

#[test]
fn noexec_survives_restore_then_yields_to_a_distinct_validated_plan() {
    let root = aterm_tempfile::tempdir().unwrap();
    let options = opts(root.path());
    let (initial, expected) = fixture(true, true);
    let predecessor = initial.native_completed_prepare.clone();
    upgrade_drive::save_checked(&options, "conversation", &initial).unwrap();
    let mut current = upgrade_drive::load_record(&options, "conversation")
        .unwrap()
        .unwrap();
    crate::harness::native_record::restore_inactive(&options, "conversation", &mut current, None)
        .unwrap();
    assert_eq!(current.native_noexec_prepare.as_deref(), Some(&expected));
    let mut next = current;
    assert_eq!(next.native_completed_prepare, predecessor);
    assert_eq!(next.native_noexec_prepare.as_deref(), Some(&expected));
    upgrade_drive::save_checked(&options, "conversation", &next).unwrap();
    // Real constructor transport validates a fresh operation before replacing
    // the bounded terminal slot; no signal or resume is sent by this test.
    let command = planned();
    let (live, _) = state(&command);
    next.pid = live.pid;
    next.shell = live.shell;
    next.tab = SID.into();
    next.line = live.line;
    next.back_line = live.back_line;
    let (mut client, worker) = endpoint(|line| {
        common(line).or_else(|| {
            let Request::Prepare(prepare) = request(line) else {
                panic!("unexpected effect")
            };
            Some(format!(
                "OK native-act state=admitted id={} cap={}\n",
                prepare.id,
                binding().capability
            ))
        })
    });
    assert_eq!(
        begin(
            &options,
            &mut client,
            &mut next,
            "conversation",
            Scope::Relaunch,
            &command.target,
            Some(&command),
            Some(&command)
        ),
        Ok(Recovery::Admitted)
    );
    assert!(next.native_noexec_prepare.is_none());
    assert_ne!(retained_prepare(&next).unwrap().id, expected.id);
    assert_eq!(next.native_completed_prepare, predecessor);
    drop(client);
    assert!(worker.join().unwrap().iter().all(|line| !matches!(
        actual_request(line),
        Some(Request::Signal(_) | Request::Resume(..))
    )));
}

#[test]
fn noexec_is_native_state_even_when_an_unknown_agent_owns_the_record() {
    let root = aterm_tempfile::tempdir().unwrap();
    let options = opts(root.path());
    let (st, _) = fixture(false, true);
    upgrade_drive::save_checked(&options, "unknown", &st).unwrap();
    let path = options.state.join("upgrade/unknown.json");
    let mut value: aterm_json::Value =
        aterm_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let object = value.as_object_mut().unwrap();
    object.insert("agent".into(), aterm_json::Value::from("unknown-agent"));
    for key in [
        "native_coordination",
        "native_generation",
        "native_write_id",
    ] {
        object.remove(key);
    }
    std::fs::write(path, value.to_string()).unwrap();
    assert!(upgrade_drive::load_record(&options, "unknown").is_err());
}
