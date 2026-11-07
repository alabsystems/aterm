// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::codex_renewal::{self, RestartCompletion};
use crate::harness::native_observe::{ObservationBinding, Registration, SavedObservation};
use aterm_spec::interp::State;

pub(super) fn fixture(rollback: bool, wrapper: bool, future: bool) -> (St, Prepare) {
    static SHELL: std::sync::OnceLock<Process> = std::sync::OnceLock::new();
    let shell = SHELL
        .get_or_init(|| Process::observe(std::process::id()).unwrap())
        .clone();
    // These are bounded observation words, not live processes or kernel proof.
    let mut original = shell.clone();
    original.pid += 10_000;
    original.birth += 1;
    original.parent = shell.pid;
    original.group = original.pid;
    let mut forward = planned().capture().unwrap();
    let mut back = forward.clone();
    back.image.inode += 1;
    if wrapper {
        for command in [&mut forward, &mut back] {
            command.settles = Some(command.image.clone());
            command.image.inode += 10;
        }
    }
    let mut successor = original.clone();
    successor.pid += 1;
    successor.birth += 1;
    successor.group = successor.pid;
    successor.image = if rollback {
        back.settled()
    } else {
        forward.settled()
    }
    .clone();
    let prepare = Prepare {
        asked_upgrade: None,
        id: "ab".repeat(16),
        host_epoch: "ef".repeat(16),
        holder: "completion-fixture".into(),
        revision: 1,
        expected_human_seq: Some(0),
        deadline_us: if future {
            native_act::monotonic_us().unwrap() + 300_000_000
        } else {
            1
        },
        duty: Scope::Relaunch,
        plan: NativePlan::Restart {
            process: original,
            shell: shell.clone(),
            launcher: shell.image,
            socket: "/completion-fixture.sock".into(),
            forward: Box::new(forward),
            rollback: Some(Box::new(back)),
            observation: None,
            cold: None,
        },
    };
    assert!(Request::prepare(prepare.clone()).encode().is_some());
    let observation = SavedObservation {
        registration: Registration {
            id: "56".repeat(16),
            host_epoch: prepare.host_epoch.clone(),
            vendor: successor,
            shell: match &prepare.plan {
                NativePlan::Restart { shell, .. } => shell.clone(),
                _ => unreachable!(),
            },
        },
        binding: ObservationBinding {
            id: "56".repeat(16),
            host_epoch: prepare.host_epoch.clone(),
            capability: "78".repeat(32),
        },
    };
    assert!(observation.encode().is_some());
    let state = St {
        tab: SID.into(),
        native_coordination: true,
        native_completed_prepare: Some(Box::new(prepare.clone())),
        native_observation: Some(observation),
        native_rollback: rollback,
        phase: Phase::Relaunched { at_s: 1 },
        ..St::default()
    };
    (state, prepare)
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
            );
        }
        8 => {
            let observed = st.native_observation.as_mut().unwrap();
            observed.registration.host_epoch = "12".repeat(16);
            observed.binding.host_epoch = observed.registration.host_epoch.clone();
        }
        9 => {
            st.native_observation
                .as_mut()
                .unwrap()
                .registration
                .shell
                .birth += 1
        }
        10 => {
            st.native_observation
                .as_mut()
                .unwrap()
                .registration
                .vendor
                .image
                .inode += 1
        }
        11 => st.native_observation.as_mut().unwrap().binding.id = "12".repeat(16),
        12 => st.native_observation = None,
        13 => st.native_completed_prepare = None,
        14 => st.phase = Phase::Exiting { at_s: 1 },
        15 => st.tab = "another-session".into(),
        16 => expected.host_epoch = "malformed".into(),
        17 => {
            st.native_observation
                .as_mut()
                .unwrap()
                .registration
                .vendor
                .parent += 1
        }
        18 => st.native_coordination = false,
        19 => {
            st.native_observation
                .as_mut()
                .unwrap()
                .registration
                .vendor
                .session += 1
        }
        20 => {
            let NativePlan::Restart { process, .. } = &expected.plan else {
                unreachable!()
            };
            let vendor = &mut st.native_observation.as_mut().unwrap().registration.vendor;
            vendor.pid = process.pid;
            vendor.birth = process.birth;
        }
        21 => {
            st.native_observation
                .as_mut()
                .unwrap()
                .registration
                .vendor
                .group += 1
        }
        _ => unreachable!(),
    }
}

/// Exercise the actual connection entry; the peer counts any native request.
/// `buggy` deliberately bypasses one boundary as a negative control.
fn attempt(model: &State, buggy: i64) -> (bool, usize) {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut options = opts(root.path());
    let (mut initial, mut expected) = fixture(
        model["rollback"] == 1,
        model["wrapper"] == 1,
        model["fault"] == 1,
    );
    if model["fault"] == 12 {
        initial.native_observation = None;
    }
    let key = format!("renewal-{}", expected.id);
    upgrade_drive::save_checked(&options, &key, &initial).unwrap();
    let mut st = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
    corrupt(&mut st, &mut expected, model["fault"]);
    options.dry_run = model["dry"] == 1;
    let failure = (model["barrier_failed"] == 1)
        .then(|| aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported));
    let (mut client, worker) = endpoint(|_| None);
    let result = match buggy {
        1 => {
            drop(client);
            true
        }
        2 | 3 => {
            assert!(upgrade_drive::save_checked(&options, &key, &st).is_err());
            drop(client);
            true
        }
        4 => {
            let _reply = native_act::exchange(&mut client, SID, &Request::Signal(binding()));
            drop(client);
            true
        }
        _ => {
            let returned =
                codex_renewal::drive_connection(&options, SID, &expected, st, client, true)
                    .is_some();
            returned && buggy != 5
        }
    };
    drop(failure);
    if buggy == 0 {
        let after = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
        assert_eq!(
            after.native_completed_prepare,
            initial.native_completed_prepare
        );
        assert_eq!(after.native_observation, initial.native_observation);
        assert!(after.native.is_none() && after.native_prepare.is_empty());
    }
    (result, worker.join().unwrap().len())
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
            "Rollback" => next.insert("rollback", 1),
            "Wrapper" => next.insert("wrapper", 1),
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
fn completion_recovery_refines_real_validation_barriers_and_transport() {
    let model = aterm_models_harness::native_completion_recovery_model();
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
fn completion_recovery_negative_controls_break_each_law() {
    let model = aterm_models_harness::native_completion_recovery_model();
    for (buggy, fault, dry, barrier, law) in [
        (1, 2, 0, 0, "ExactCompletion"),
        (2, 0, 0, 1, "DurableCompletion"),
        (3, 0, 1, 0, "DryRunIsInert"),
        (4, 0, 0, 0, "NoReplay"),
        (5, 0, 0, 0, "ValidCompletionRetained"),
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
fn a_saved_completion_needs_no_host_and_survives_its_original_deadline() {
    for rollback in [false, true] {
        for wrapper in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let options = opts(root.path());
            let (state, expected) = fixture(rollback, wrapper, false);
            let key = format!("renewal-{}", expected.id);
            upgrade_drive::save_checked(&options, &key, &state).unwrap();
            assert!(codex_renewal::prepare_restart(&options, SID, &expected).is_ok());
            for drive in [
                codex_renewal::drive_restart,
                codex_renewal::drive_resume_only,
            ] {
                let returned = drive(&options, SID, &expected).unwrap();
                assert_eq!(matches!(returned, RestartCompletion::Rollback(_)), rollback);
            }
            let (forward, observed) =
                codex_renewal::completed_restart(&options, SID, &expected).unwrap();
            assert_eq!(forward, !rollback);
            assert_eq!(Some(observed), state.native_observation);
            assert!(!std::path::Path::new(options.sock.as_ref().unwrap()).exists());
        }
    }
}

#[test]
fn a_failed_completion_barrier_preserves_its_writer_and_rejects_stale_clones() {
    let root = aterm_tempfile::tempdir().unwrap();
    let options = opts(root.path());
    let (initial, expected) = fixture(false, true, false);
    let key = format!("renewal-{}", expected.id);
    upgrade_drive::save_checked(&options, &key, &initial).unwrap();
    let original = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
    let stale = original.clone();
    let failed = aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported);
    assert!(codex_renewal::recover_completion(&options, SID, &expected, &original).is_none());
    let visible = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
    assert_eq!(
        visible.native_completed_prepare,
        original.native_completed_prepare
    );
    assert_eq!(visible.native_observation, original.native_observation);
    drop(failed);
    assert!(codex_renewal::recover_completion(&options, SID, &expected, &stale).is_none());
    assert!(codex_renewal::recover_completion(&options, SID, &expected, &original).is_some());
    let mut newer = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
    newer.wait = "newer status writer".into();
    upgrade_drive::save_checked(&options, &key, &newer).unwrap();
    assert!(codex_renewal::recover_completion(&options, SID, &expected, &original).is_none());
    assert_eq!(
        upgrade_drive::load_record(&options, &key)
            .unwrap()
            .unwrap()
            .wait,
        newer.wait
    );
    assert!(codex_renewal::completed_restart(&options, SID, &expected).is_some());
}

#[test]
fn completion_without_an_observation_is_saved_and_never_replayed() {
    let root = aterm_tempfile::tempdir().unwrap();
    let options = opts(root.path());
    let st = bound_record(&options);
    let Request::Prepare(expected) = Request::parse(&st.native_prepare).unwrap() else {
        unreachable!()
    };
    let key = format!("renewal-{}", expected.id);
    upgrade_drive::save_checked(&options, &key, &st).unwrap();
    let st = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
    let (client, worker) = endpoint(|line| {
        Some(match request(line) {
            Request::Signal(_) => "OK native-act state=signalled\n".into(),
            Request::Query(_) => "OK native-act state=committed claim=builtin revision=-\n".into(),
            other => panic!("unexpected unobserved completion request: {other:?}"),
        })
    });
    assert!(codex_renewal::drive_connection(&options, SID, &expected, st, client, true).is_none());
    assert_eq!(worker.join().unwrap().len(), 2);
    let saved = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
    assert_eq!(saved.native_completed_prepare.as_ref(), Some(&expected));
    assert!(saved.native_observation.is_none());
    assert!(saved.native.is_none() && saved.native_prepare.is_empty());
    assert!(codex_renewal::drive_restart(&options, SID, &expected).is_none());
    assert!(codex_renewal::drive_resume_only(&options, SID, &expected).is_none());
    assert!(codex_renewal::completed_restart(&options, SID, &expected).is_none());
}

#[test]
fn unobserved_completion_repairs_a_failed_barrier_without_publishing_a_result() {
    let root = aterm_tempfile::tempdir().unwrap();
    let options = opts(root.path());
    let (mut initial, expected) = fixture(false, false, false);
    initial.native_observation = None;
    let key = format!("renewal-{}", expected.id);
    upgrade_drive::save_checked(&options, &key, &initial).unwrap();
    let original = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
    let stale = original.clone();
    let generation = crate::harness::native_record::generation(&original);
    let failed = aterm_durable::seam::FailingDirs::with_kind(std::io::ErrorKind::Unsupported);
    assert!(codex_renewal::recover_completion(&options, SID, &expected, &original).is_none());
    let visible = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
    assert!(crate::harness::native_record::generation(&visible) > generation);
    drop(failed);
    assert!(codex_renewal::recover_completion(&options, SID, &expected, &stale).is_none());
    assert_eq!(
        crate::harness::native_record::generation(&stale),
        generation
    );
    assert!(codex_renewal::recover_completion(&options, SID, &expected, &original).is_none());
    assert!(crate::harness::native_record::generation(&original) > generation);
    let repaired = upgrade_drive::load_record(&options, &key).unwrap().unwrap();
    assert!(repaired.native_observation.is_none());
    assert_eq!(
        repaired.native_completed_prepare,
        initial.native_completed_prepare
    );
    assert!(repaired.native.is_none() && repaired.native_prepare.is_empty());
}
