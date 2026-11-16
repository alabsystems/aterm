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
            limit: None,
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

/// ONLY AN EXEC HANDOFF'S PREPARED WRITER TRIES EACH LOCK ONCE: an exec handoff's `arm` and
/// `reserve` ask `with_prepared_writer_now` while they hold their session's own locks, which the
/// window's thread takes too, and their session asks again shortly. With the machine's sign-in
/// turn taken it answers `signin-busy`, and with `accounts.lock` taken `Ok(None)`, at its first
/// try: on a store whose ladder goes on trying for two minutes, each answer still comes back
/// inside the one-minute hang detector. `with_prepared_writer`, which a TUI session's quota
/// handoff publishes and commits through and its lost-login move publishes through, waits on the
/// store's ladder as it always did: on that same ladder, while another holds the turn or the lock
/// it answers nothing, and once that one lets go the TUI's publication of its receipt goes
/// through. CONTROL: with neither taken the exec handoff's writer judges the selection and runs
/// its callback.
#[test]
fn only_an_exec_handoffs_prepared_writer_tries_each_lock_once() {
    /// A pause of 20 ms, for two minutes: a ladder that outlasts the hang detector.
    const LADDER: &[Duration] = &[Duration::from_millis(20); 6000];
    /// What the TUI's publication answered, and the receipt then stored under its id.
    type Published = (Result<Option<Receipt>, &'static str>, Option<Receipt>);
    let (world, env, receipt) = publication_fixture(false, false);
    let exec = || {
        let (env, receipt) = (env.clone(), receipt.clone());
        let (sent, answer) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let judged = accounts::with_store_ladder(LADDER, || {
                codex_rotation::with_prepared_writer_now(&env, &receipt, |_, _| Ok(()))
            });
            let _ = sent.send(judged);
        });
        answer
            .recv_timeout(Duration::from_secs(60))
            .expect("the exec handoff's writer waited for a lock it found taken")
    };
    // The TUI quota control's `publish`: a new receipt of the selection under its own index.
    let tui = |id: &str| -> std::sync::mpsc::Receiver<Published> {
        let mut receipt = receipt.clone();
        receipt.id = id.repeat(32);
        let index = renewal::IndexKey::moved(&receipt.intent, receipt.old_pid, 1, id).unwrap();
        let env = env.clone();
        let (sent, answer) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let published = accounts::with_store_ladder(LADDER, || {
                codex_rotation::with_prepared_writer(&env, &receipt, |_, writer| {
                    let prior = writer.read_indexed(&index)?;
                    if prior.as_ref().is_some_and(|old| !old.may_replace()) {
                        return Err("existing-receipt");
                    }
                    writer.publish_indexed(&index, prior.as_ref(), &receipt)?;
                    Ok(receipt.clone())
                })
            });
            let _ = sent.send((published, renewal::read_receipt(&env, &receipt.id)));
        });
        answer
    };
    let held = |answer: &std::sync::mpsc::Receiver<Published>, why: &str| {
        assert!(
            matches!(
                answer.recv_timeout(Duration::from_millis(300)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ),
            "{why}: the TUI's writer answered while it was held"
        );
    };
    let published = |answer: std::sync::mpsc::Receiver<Published>, why: &str| {
        let (published, stored) = answer
            .recv_timeout(Duration::from_secs(60))
            .expect("the TUI's writer took it once it was let go");
        let published = published.expect(why).expect(why);
        assert_eq!(stored.as_ref(), Some(&published), "{why}");
    };
    let store = world.seat().store(Cli::Codex);

    let signin = store.signin_within().unwrap().unwrap();
    assert_eq!(exec(), Err("signin-busy"));
    let waiting = tui("b");
    held(&waiting, "sign-in turn");
    drop(signin);
    published(waiting, "sign-in turn");

    let lock = store.acquire_bounded().unwrap().unwrap();
    assert_eq!(exec(), Ok(None));
    let waiting = tui("c");
    held(&waiting, "accounts.lock");
    drop(lock);
    published(waiting, "accounts.lock");

    assert_eq!(exec(), Ok(Some(())), "CONTROL");
}
