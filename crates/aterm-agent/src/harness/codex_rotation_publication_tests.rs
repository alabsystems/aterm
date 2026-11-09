// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::{cli::Env, codex_renewal as renewal};
use renewal::{Intent, Phase, Purpose, Receipt};

fn publication_fixture(prior: bool, commit: bool) -> (World, Env, Receipt) {
    let (world, reader) = fixture("exec-publication");
    let mut env = Env::from_process().unwrap();
    env.home = Some(world.home.clone());
    env.state = world.state.clone();
    env.config = Some(world.cfg.join("aterm.toml"));
    env.now = NOW;
    let proposal = codex_rotation::prepare(
        &world.seat(),
        &weekly_session(&world),
        &Identity::from_native("source", "user"),
        &reader,
        Duration::from_secs(60),
    )
    .unwrap()
    .unwrap();
    native_auth(&proposal.old_auth, "source");
    native_auth(&proposal.new_auth, "target");
    let next = proposal.successor().unwrap();
    let receipt = Receipt {
        destination: Some(next.identity),
        id: "a".repeat(32),
        intent: Intent {
            purpose: Purpose::WeeklyQuota,
            thread: "00000000-0000-0000-0000-000000000001".into(),
            wall: "00000000-0000-0000-0000-000000000002".into(),
            identity: proposal.source_identity.clone(),
        },
        sid: "s-one".into(),
        old_pid: 10,
        old_birth: 1,
        claim_revision: 1,
        human_seq: 0,
        old_auth: next.old_auth,
        new_auth: next.new_auth,
        generation: next.generation,
        row_stamp: renewal::row_stamp(&next.row),
        native_prepare: "sealed native recipe".into(),
        expires_ms: u64::MAX,
        phase: if prior {
            Phase::Committing
        } else {
            Phase::Ready
        },
        successor_pid: None,
        successor_birth: None,
        accepted_turn: None,
        launch_attempted: prior,
        candidate: None,
        current_attempt: None,
        revoked: false,
    };
    renewal::with_receipt_writer(&env, |_| renewal::write_receipt(&env, &receipt))
        .unwrap()
        .unwrap();
    if commit {
        proposal.commit(&receipt, NOW).unwrap();
    }
    (world, env, receipt)
}

fn attempted(receipt: &Receipt) -> bool {
    assert_eq!(receipt.phase == Phase::Committing, receipt.launch_attempted);
    receipt.launch_attempted
}

#[test]
fn cancellation_keeps_failed_committing_publication_and_reserves_attempted_pool() {
    let (world, env, mut receipt) = publication_fixture(false, true);
    let published =
        codex_rotation::publish_selected_committing_using(&env, &mut receipt, |env, projected| {
            let _failure = aterm_durable::seam::FailingDirs::with(aterm_durable::seam::EIO);
            renewal::write_receipt(env, projected)
        });
    assert_eq!(published, Err("receipt-parent-flush"));
    assert!(receipt.launch_attempted);
    assert!(
        !renewal::read_receipt(&env, &receipt.id)
            .unwrap()
            .launch_attempted
    );
    renewal::with_receipt_writer(&env, |writer| {
        assert!(
            writer
                .cancel(&receipt, renewal::CancellationKind::OwnerStop, 5, 5)?
                .is_some()
        );
        let retired = renewal::read_receipt(&env, &receipt.id).unwrap();
        assert!(retired.launch_attempted && retired.revoked);
        assert_eq!(retired.phase, Phase::Retired);
        assert_eq!(
            writer.retire_pool_record(&retired),
            Err("rotation-not-terminal")
        );
        Ok(())
    })
    .unwrap()
    .unwrap();
    assert_eq!(
        world
            .seat()
            .store(Cli::Codex)
            .load_logins(NOW)
            .unwrap()
            .0
            .native_rotations
            .len(),
        1
    );
}

fn publication_case(state: &aterm_spec::interp::State, buggy: u8) -> aterm_spec::interp::State {
    let prior = state["prior"] == 1;
    let fault = state["fault"];
    let (world, env, mut receipt) = publication_fixture(prior, fault != 2);
    let original = receipt.clone();
    let store = world.seat().store(Cli::Codex);
    let store_lock = (fault == 0).then(|| store.acquire_bounded().unwrap().unwrap());
    let signin = (fault == 1).then(|| store.signin_within().unwrap().unwrap());
    if fault == 3 {
        std::fs::write(
            env.config.as_ref().unwrap(),
            "[harness]\naccounts = 'off'\n",
        )
        .unwrap();
    }
    let mut entered = false;
    let write = |e: &Env, r: &Receipt| {
        entered = true;
        let failure = match fault {
            4 => Some(aterm_durable::seam::FailingDirs::with(
                aterm_durable::seam::EIO,
            )),
            5 => Some(aterm_durable::seam::FailingDirs::after(
                1,
                aterm_durable::seam::EIO,
            )),
            _ => None,
        };
        let result = renewal::write_receipt(e, r);
        drop(failure);
        result
    };
    let result = if buggy == 2 {
        // An unsafe caller that keeps its staged mirror only after acknowledgement.
        let mut staged = receipt.clone();
        let result = codex_rotation::publish_selected_committing_using(&env, &mut staged, write);
        if result == Ok(Some(())) {
            receipt = staged;
        }
        result
    } else {
        if buggy == 1 {
            // The old caller latched before either writer admission or validation.
            receipt.phase = Phase::Committing;
            receipt.launch_attempted = true;
        }
        codex_rotation::publish_selected_committing_using(&env, &mut receipt, write)
    };
    drop(signin);
    drop(store_lock);
    let expected = match fault {
        0 => Ok(None),
        1 => Err("signin-busy"),
        2 => Err("pool-commit-missing"),
        3 => Err("rotation-policy"),
        4 => Err("receipt-parent-flush"),
        5 => Err("receipt-directory-flush"),
        6 => Ok(Some(())),
        _ => unreachable!(),
    };
    assert_eq!(result, expected);
    if !entered && buggy == 0 {
        assert_eq!(receipt, original);
    }
    let stored = renewal::read_receipt(&env, &receipt.id).unwrap();
    if buggy == 3 && !entered {
        receipt.phase = Phase::Ready;
        receipt.launch_attempted = false;
    }
    let memory = attempted(&receipt);
    let mut acknowledged = result == Ok(Some(()));
    match buggy {
        4 if entered => acknowledged = true,
        5 => acknowledged = false,
        _ => {}
    }
    let mut next = state.clone();
    next.insert("done", 1);
    next.insert("entered", i64::from(entered));
    next.insert("memory", i64::from(memory));
    next.insert("disk", i64::from(attempted(&stored)));
    next.insert("acknowledged", i64::from(acknowledged));
    next
}

pub(crate) fn publication_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "PriorAttempt" => {
            next.insert("prior", 1);
            next.insert("memory", 1);
            next.insert("disk", 1);
        }
        "NextFault" => {
            next.insert("fault", state["fault"] + 1);
        }
        "Publish" => return publication_case(state, 0),
        _ => unreachable!(),
    }
    next
}

#[test]
fn exec_publication_refines_real_writer_and_file_failures() {
    let model = aterm_models_harness::exec_publication_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = publication_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (buggy, prior, fault, law) in [
        (1, 0, 0, "RefusalPreservesHistory"),
        (2, 0, 4, "AttemptRetained"),
        (2, 0, 5, "VisibleWriteCovered"),
        (3, 1, 0, "PriorRetained"),
        (4, 0, 5, "DurableReturn"),
        (5, 0, 6, "Available"),
    ] {
        let mut state = model.init_state();
        state.insert("prior", prior);
        state.insert("memory", prior);
        state.insert("disk", prior);
        state.insert("fault", fault);
        let next = publication_case(&state, buggy);
        assert!(!model.check_invariant(law, &next), "{law}");
        assert!(!model.successors("Publish", &state).contains(&next));
    }
}

#[test]
fn exec_publication_refusal_preserves_unexecuted_cancellation() {
    for signin_busy in [false, true] {
        let (world, env, mut receipt) = publication_fixture(false, true);
        let original = receipt.clone();
        let store = world.seat().store(Cli::Codex);
        let held = (!signin_busy).then(|| store.acquire_bounded().unwrap().unwrap());
        let signin = signin_busy.then(|| store.signin_within().unwrap().unwrap());
        let result = codex_rotation::publish_selected_committing(&env, &mut receipt);
        assert_eq!(
            result,
            if signin_busy {
                Err("signin-busy")
            } else {
                Ok(None)
            }
        );
        drop(signin);
        drop(held);
        assert_eq!(receipt, original);
        assert_eq!(renewal::read_receipt(&env, &receipt.id), Some(original));
        let retired = renewal::cancel_unexecuted_rotation(&env, &receipt, "work")
            .unwrap()
            .unwrap();
        assert!(retired.revoked);
        assert!(!retired.launch_attempted);
        assert_eq!(world.load(Cli::Codex).active.as_deref(), Some("main"));
        assert!(world.load(Cli::Codex).canary.is_none());
        assert!(
            store
                .load_logins(NOW)
                .unwrap()
                .0
                .native_rotations
                .is_empty()
        );
    }
}

#[test]
fn exec_publication_public_entry_preserves_retry_and_refuses_terminal_inputs() {
    let (_world, env, mut receipt) = publication_fixture(false, true);
    let original = receipt.clone();
    let mut invalid = Vec::new();
    for phase in [Phase::Committed, Phase::Consumed, Phase::Retired] {
        let mut wrong = original.clone();
        wrong.phase = phase;
        invalid.push(wrong);
    }
    for case in 0..9 {
        let mut wrong = original.clone();
        match case {
            0 => wrong.launch_attempted = true,
            1 => wrong.phase = Phase::Committing,
            2 => wrong.revoked = true,
            3 => wrong.intent.purpose = Purpose::Login,
            4 => wrong.destination = None,
            5 => wrong.accepted_turn = Some(wrong.intent.wall.clone()),
            6 => wrong.successor_pid = Some(10),
            7 => wrong.successor_birth = Some(1),
            8 => {
                wrong.current_attempt = Some(renewal::AttemptRef {
                    id: "b".repeat(32),
                    generation: 1,
                })
            }
            _ => unreachable!(),
        }
        invalid.push(wrong);
    }
    let mut candidate = original.clone();
    candidate.candidate = Some(renewal::Candidate {
        pid: 10,
        birth: 1,
        parent: 1,
        group: 10,
        session: 10,
        launcher_device: 1,
        launcher_inode: 1,
        launcher_path: b"/fixture".to_vec(),
        forward: true,
    });
    invalid.push(candidate);
    for mut wrong in invalid {
        let before = wrong.clone();
        assert!(codex_rotation::publish_selected_committing(&env, &mut wrong).is_err());
        assert_eq!(wrong, before);
        assert_eq!(
            renewal::read_receipt(&env, &receipt.id),
            Some(original.clone())
        );
    }
    assert_eq!(
        codex_rotation::publish_selected_committing(&env, &mut receipt),
        Ok(Some(()))
    );
    assert!(attempted(&receipt));
    let published = receipt.clone();
    assert_eq!(
        codex_rotation::publish_selected_committing(&env, &mut receipt),
        Ok(Some(()))
    );
    assert_eq!(receipt, published);
    assert_eq!(renewal::read_receipt(&env, &receipt.id), Some(published));
}
