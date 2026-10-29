// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};

const REQUEST: [u8; 16] = [61; 16];
const OPERATION: Digest = [62; 32];
fn observed(outcome: Outcome) -> Observation {
    Observation::new(outcome, [63; 32]).unwrap()
}

#[test]
fn invocation_is_durable_before_entry_and_retries_never_reenter() {
    for outcome in [Outcome::Completed, Outcome::Rejected] {
        let lab = Lab::new(&[Role::Writer]);
        let allocation = allocation(&lab, &[2]);
        let db = resources(&lab, 0, &allocation);
        db.initialize().unwrap();
        let answer = observed(outcome);
        let first = db
            .dispatch_once(REQUEST, 1, OPERATION, || {
                // This also proves no persistent lock is held across adapter code.
                let pending = resources(&lab, 0, &allocation)
                    .attempt(REQUEST)
                    .unwrap()
                    .unwrap();
                assert_eq!(pending.observation, ObservationState::Unknown);
                assert_eq!(db.snapshot().unwrap().used.values().sum::<u64>(), 1);
                Some(answer)
            })
            .unwrap();
        assert!(first.invoked);
        assert_eq!(first.observation, ObservationState::Recorded(answer));
        let reopened = resources(&lab, 0, &allocation);
        let second = reopened
            .dispatch_once(REQUEST, 1, OPERATION, || panic!("repeated entry"))
            .unwrap();
        assert!(!second.invoked);
        assert_eq!(second.receipt, first.receipt);
        assert_eq!(second.observation, ObservationState::Recorded(answer));
        assert!(reopened.reserve(REQUEST, 1).is_err());
        assert_eq!(reopened.snapshot().unwrap().used.values().sum::<u64>(), 1);
    }
}

#[test]
fn crashes_and_unknown_results_keep_the_charge_without_retry_permission() {
    for crash in [0, 1, 2] {
        let lab = Lab::new(&[Role::Writer]);
        let allocation = allocation(&lab, &[1]);
        let db = resources(&lab, 0, &allocation);
        db.initialize().unwrap();
        let count = Cell::new(0);
        let result = catch_unwind(AssertUnwindSafe(|| {
            db.dispatch_with(
                REQUEST,
                1,
                OPERATION,
                || {
                    if crash == 0 {
                        panic!("crash before adapter entry");
                    }
                },
                || {
                    count.set(count.get() + 1);
                    if crash == 1 {
                        panic!("crash after adapter entry");
                    }
                    None
                },
            )
        }));
        assert_eq!(result.is_err(), crash < 2);
        assert_eq!(count.get(), usize::from(crash > 0));
        let reopened = resources(&lab, 0, &allocation);
        let pending = reopened
            .dispatch_once(REQUEST, 1, OPERATION, || panic!("repeated entry"))
            .unwrap();
        assert_eq!(pending.observation, ObservationState::Unknown);
        assert!(!pending.invoked);
        assert!(
            reopened
                .dispatch_once([64; 16], 1, OPERATION, || panic!("over budget"))
                .is_err()
        );
        assert_eq!(reopened.snapshot().unwrap().used.values().sum::<u64>(), 1);
    }
}

#[test]
fn legacy_reservations_and_changed_intents_cannot_be_upgraded_or_repurposed() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[3]);
    let db = resources(&lab, 0, &allocation);
    db.initialize().unwrap();
    db.reserve(REQUEST, 1).unwrap();
    assert!(db.attempt(REQUEST).is_err());
    assert!(
        db.dispatch_once(REQUEST, 1, OPERATION, || panic!("legacy upgrade"))
            .is_err()
    );
    let request = [65; 16];
    db.dispatch_once(request, 1, OPERATION, || None).unwrap();
    assert!(
        db.dispatch_once(request, 2, OPERATION, || panic!("new cost"))
            .is_err()
    );
    assert!(
        db.dispatch_once(request, 1, [66; 32], || panic!("new operation"))
            .is_err()
    );
    for (id, cost, operation) in [
        ([0; 16], 1, OPERATION),
        ([67; 16], 0, OPERATION),
        ([67; 16], 1, [0; 32]),
    ] {
        assert!(
            db.dispatch_once(id, cost, operation, || panic!("invalid input"))
                .is_err()
        );
    }
    assert!(Observation::new(Outcome::Completed, [0; 32]).is_err());
    assert!(db.attempt([0; 16]).is_err());
    assert_eq!(db.snapshot().unwrap().used.values().sum::<u64>(), 2);
}

#[test]
fn attempt_command_reports_retained_state_without_spending_or_running() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[2]);
    let db = resources(&lab, 0, &allocation);
    db.initialize().unwrap();
    let saved = db.dispatch_once(REQUEST, 1, OPERATION, || None).unwrap();
    let wire = hex::encode_lower(allocation.bytes());
    let pin = hex::encode_lower(&allocation.id());
    let request = hex::encode_lower(&REQUEST);
    let options = BTreeMap::from([
        ("--allocation", wire.as_str()),
        ("--pin", pin.as_str()),
        ("--request", request.as_str()),
    ]);
    let before = db.snapshot().unwrap();
    let status = command::run_at("resources-attempt", options, lab.paths[0].clone()).unwrap();
    assert_eq!(status["scope"].as_str(), Some("observation-only"));
    assert_eq!(status["attempt"]["state"].as_str(), Some("unknown"));
    assert_eq!(
        status["attempt"]["receipt_id"].as_str(),
        Some(hex::encode_lower(&saved.receipt.id).as_str())
    );
    assert_eq!(db.snapshot().unwrap(), before);
}

#[test]
fn failed_observation_storage_keeps_the_durable_attempt_and_adapter_evidence() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[2]);
    let db = resources(&lab, 0, &allocation);
    db.initialize().unwrap();
    let answer = observed(Outcome::Completed);
    let result = db
        .dispatch_once(REQUEST, 1, OPERATION, || {
            assert!(db.attempt(REQUEST).unwrap().is_some());
            lab.rotate(&[Role::Reader]);
            Some(answer)
        })
        .unwrap();
    assert!(result.invoked);
    assert!(
        matches!(result.observation, ObservationState::Unconfirmed { observation, ref reason }
        if observation == answer && !reason.is_empty())
    );
    assert_ne!(result.receipt.id, [0; 32]);
    // Demotion keeps read access to the retained liability, even though it
    // prevents recording the outcome or entering the adapter again.
    let retained = db.attempt(REQUEST).unwrap().unwrap();
    assert_eq!(retained.receipt, result.receipt);
    assert_eq!(retained.observation, ObservationState::Unknown);
    assert_eq!(db.snapshot().unwrap().used.values().sum::<u64>(), 1);
    assert!(
        db.dispatch_once(REQUEST, 1, OPERATION, || panic!("retired writer reentered"))
            .is_err()
    );
}

#[test]
fn a_concurrent_retry_observes_pending_intent_without_entering_the_adapter() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[1]);
    let db = resources(&lab, 0, &allocation);
    db.initialize().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (leave_tx, leave_rx) = mpsc::channel();
    thread::scope(|threads| {
        let count_worker = Arc::clone(&count);
        let db_worker = &db;
        let first = threads.spawn(move || {
            db_worker.dispatch_once(REQUEST, 1, OPERATION, || {
                count_worker.fetch_add(1, Ordering::SeqCst);
                entered_tx.send(()).unwrap();
                leave_rx.recv_timeout(Duration::from_secs(60)).unwrap();
                Some(observed(Outcome::Completed))
            })
        });
        entered_rx.recv_timeout(Duration::from_secs(60)).unwrap();
        let retry = db
            .dispatch_once(REQUEST, 1, OPERATION, || panic!("concurrent entry"))
            .unwrap();
        assert!(!retry.invoked);
        assert_eq!(retry.observation, ObservationState::Unknown);
        leave_tx.send(()).unwrap();
        assert!(
            first
                .join()
                .unwrap()
                .unwrap()
                .observation
                .confirmed()
                .is_some()
        );
    });
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn two_offline_writers_dispatch_their_shares_and_repair_without_reentry() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let allocation = allocation(&lab, &[1, 1]);
    let a = resources(&lab, 0, &allocation);
    let b = resources(&lab, 1, &allocation);
    a.initialize().unwrap();
    b.initialize().unwrap();
    a.dispatch_once(REQUEST, 1, OPERATION, || Some(observed(Outcome::Completed)))
        .unwrap();
    b.dispatch_once(REQUEST, 1, [68; 32], || None).unwrap();
    rounds(&lab);
    assert_eq!(a.snapshot().unwrap(), b.snapshot().unwrap());
    assert_eq!(a.snapshot().unwrap().used.values().sum::<u64>(), 2);
    assert!(
        a.dispatch_once(REQUEST, 1, OPERATION, || panic!("reentry after repair"))
            .unwrap()
            .observation
            .confirmed()
            .is_some()
    );
    assert!(
        b.dispatch_once(REQUEST, 1, [68; 32], || panic!("unknown outcome retried"))
            .unwrap()
            .observation
            .confirmed()
            .is_none()
    );
}

fn raw_payload(lab: &Lab, allocation: &Allocation, actor: usize, payload: &[u8]) {
    let bootstrap = lab.bootstrap(actor);
    let session = Session::open(&bootstrap).unwrap();
    let scope = descriptor(&bootstrap, allocation).unwrap().scope;
    let directory = Directory::new(&lab.paths[actor], bootstrap.trust());
    Store::open(&session, &directory.path(scope).unwrap(), scope)
        .unwrap()
        .commit_catalog(&session, payload)
        .unwrap();
}

#[test]
fn false_or_duplicate_observations_and_duplicate_intents_fail_closed() {
    for fault in 0..5 {
        let lab = Lab::new(&[Role::Writer]);
        let allocation = allocation(&lab, &[2]);
        let db = resources(&lab, 0, &allocation);
        db.initialize().unwrap();
        let first = db.dispatch_once(REQUEST, 1, OPERATION, || None).unwrap();
        let mut completion = ledger::Completion {
            allocation: allocation.id(),
            request: REQUEST,
            claim: first.receipt.id,
            observation: observed(Outcome::Completed),
        };
        match fault {
            0 => completion.claim = [70; 32],
            1 => completion.request = [71; 16],
            2 => completion.allocation = [72; 32],
            3 => raw_payload(&lab, &allocation, 0, &completion.encode()),
            4 => {}
            _ => unreachable!(),
        }
        let bytes = if fault == 4 {
            ledger::Debit {
                allocation: allocation.id(),
                request: REQUEST,
                cost: 1,
                cumulative: 1,
            }
            .encode_attempt(OPERATION)
        } else {
            completion.encode()
        };
        raw_payload(&lab, &allocation, 0, &bytes);
        assert!(db.snapshot().is_err());
        assert!(
            db.dispatch_once([73; 16], 1, OPERATION, || panic!(
                "invalid history grants entry"
            ))
            .is_err()
        );
    }
}

#[test]
fn another_writer_cannot_complete_a_peers_attempt() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let allocation = allocation(&lab, &[1, 1]);
    let a = resources(&lab, 0, &allocation);
    let b = resources(&lab, 1, &allocation);
    a.initialize().unwrap();
    b.initialize().unwrap();
    let claim = a.dispatch_once(REQUEST, 1, OPERATION, || None).unwrap();
    b.dispatch_once(REQUEST, 1, OPERATION, || None).unwrap();
    rounds(&lab);
    raw_payload(
        &lab,
        &allocation,
        1,
        &ledger::Completion {
            allocation: allocation.id(),
            request: REQUEST,
            claim: claim.receipt.id,
            observation: observed(Outcome::Completed),
        }
        .encode(),
    );
    assert!(b.attempt(REQUEST).is_err());
    assert!(
        b.dispatch_once([76; 16], 1, OPERATION, || panic!(
            "forged completion grants entry"
        ))
        .is_err()
    );
}

#[test]
fn missing_table_custody_cannot_turn_an_attempt_into_a_fresh_invocation() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[2]);
    let db = resources(&lab, 0, &allocation);
    let scope = db.initialize().unwrap();
    db.dispatch_once(REQUEST, 1, OPERATION, || None).unwrap();
    let directory = Directory::new(&lab.paths[0], lab.bootstrap(0).trust());
    let path = directory.path(scope).unwrap();
    std::fs::rename(&path, path.with_extension("preserved")).unwrap();
    assert!(
        db.dispatch_once(REQUEST, 1, OPERATION, || panic!(
            "lost history grants entry"
        ))
        .is_err()
    );
    assert!(db.initialize().is_err());
}

fn invoke(db: &Resources, count: &Cell<i64>, outcome: Option<Outcome>) -> Result<Attempt> {
    db.dispatch_once(REQUEST, 1, OPERATION, || {
        count.set(count.get() + 1);
        outcome.map(observed)
    })
}
fn fresh(db: &Resources) -> super::super::dispatch::Permit {
    match db.admit(REQUEST, 1, OPERATION).unwrap() {
        super::super::dispatch::Admission::Fresh(permit) => permit,
        _ => panic!("new request did not produce a permit"),
    }
}
fn enter(
    permit: super::super::dispatch::Permit,
    count: &Cell<i64>,
    answer: i64,
) -> super::super::dispatch::Invoked {
    permit.invoke(|| {
        count.set(count.get() + 1);
        match answer {
            0 => None,
            1 => Some(observed(Outcome::Completed)),
            2 => Some(observed(Outcome::Rejected)),
            _ => panic!("unknown adapter answer"),
        }
    })
}
fn lose_history(lab: &Lab, allocation: &Allocation, db: &Resources) -> (i64, i64) {
    let prior = db.snapshot().unwrap();
    let directory = Directory::new(&lab.paths[0], lab.bootstrap(0).trust());
    let path = directory
        .path(descriptor(&lab.bootstrap(0), allocation).unwrap().scope)
        .unwrap();
    std::fs::rename(&path, path.with_extension("preserved")).unwrap();
    assert!(
        db.attempt(REQUEST).is_err(),
        "custody loss must be explicit"
    );
    (
        prior.used.values().sum::<u64>() as i64,
        prior.retained as i64,
    )
}

pub(crate) fn dispatch_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let phase = state["phase"];
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[1]);
    let db = resources(&lab, 0, &allocation);
    if action != "NotAdmitted" && (phase != 6 || state["damaged"] == 1) {
        db.initialize().unwrap();
    }
    let entered = Cell::new(0);
    let mut permit = None;
    let mut invoked = None;
    let mut loss = None;
    match phase {
        0 => {}
        1 => permit = Some(fresh(&db)),
        2 => invoked = Some(enter(fresh(&db), &entered, state["answer"])),
        3 => {
            invoke(
                &db,
                &entered,
                Some(if state["observed"] == 1 {
                    Outcome::Completed
                } else {
                    Outcome::Rejected
                }),
            )
            .unwrap();
        }
        4 => {
            let p = fresh(&db);
            if state["entered"] == 1 {
                drop(enter(p, &entered, 0));
            } else {
                drop(p);
            }
        }
        5 => {
            db.reserve(REQUEST, 1).unwrap();
        }
        6 if state["damaged"] == 1 => {
            if state["legacy"] == 1 {
                db.reserve(REQUEST, 1).unwrap();
            } else if state["witness"] == 2 {
                invoke(&db, &entered, Some(Outcome::Completed)).unwrap();
            } else {
                let p = fresh(&db);
                if state["entered"] == 1 {
                    drop(enter(p, &entered, 0));
                } else {
                    drop(p);
                }
            }
            loss = Some(lose_history(&lab, &allocation, &db));
        }
        6 => {}
        _ => unreachable!(),
    }
    if state["retried"] == 1 {
        let _ = invoke(&db, &entered, None);
    }
    if state["wrong"] == 1 {
        assert!(
            db.dispatch_once(REQUEST, 1, [74; 32], || panic!("changed intent"))
                .is_err()
        );
    }
    let original = db.attempt(REQUEST).ok().flatten();
    let before = entered.get();
    let mut next = state.clone();
    match action {
        "Admit" => permit = Some(fresh(&db)),
        "Complete" | "Reject" | "Unknown" => {
            let answer = match action {
                "Complete" => 1,
                "Reject" => 2,
                _ => 0,
            };
            invoked = Some(enter(permit.take().unwrap(), &entered, answer));
        }
        "Record" => {
            let entered = invoked.take().unwrap();
            let expected = entered
                .observation()
                .map_or(ObservationState::Unknown, ObservationState::Recorded);
            let returned = db.record(entered);
            let original = original
                .as_ref()
                .expect("recording needs an admitted attempt");
            assert!(returned.invoked);
            assert_eq!(returned.receipt, original.receipt);
            assert_eq!(returned.operation, original.operation);
            assert_eq!(returned.observation, expected);
        }
        "CrashBefore" => drop(permit.take()),
        "CrashAfter" => drop(invoked.take()),
        "LoseHistory" => {
            drop(permit.take());
            drop(invoked.take());
            loss = Some(lose_history(&lab, &allocation, &db));
            next.insert("damaged", 1);
        }
        "Legacy" => {
            db.reserve(REQUEST, 1).unwrap();
            next.insert("legacy", 1);
        }
        "NotAdmitted" => {
            assert!(invoke(&db, &entered, None).is_err());
        }
        "Retry" => {
            let returned = invoke(&db, &entered, None);
            match original.as_ref() {
                Some(original) => {
                    let returned = returned.expect("a valid retry returns retained state");
                    assert!(!returned.invoked);
                    assert_eq!(returned.operation, original.operation);
                    assert_eq!(returned.observation, original.observation);
                    next.insert("same", i64::from(returned.receipt == original.receipt));
                }
                None => {
                    assert!(returned.is_err(), "absent grant returned an attempt");
                }
            }
            next.insert("retried", 1);
            next.insert("reentered", i64::from(entered.get() != before));
        }
        "WrongOperation" => {
            let refused = db
                .dispatch_once(REQUEST, 1, [75; 32], || {
                    entered.set(entered.get() + 1);
                    None
                })
                .is_err();
            next.insert("wrong", 1);
            next.insert("refused", i64::from(refused));
        }
        _ => panic!("unknown dispatch action"),
    }
    let attempt_read = db.attempt(REQUEST);
    let snapshot = db.snapshot();
    if loss.is_some() {
        assert!(snapshot.is_err(), "lost history was silently reinitialized");
        assert!(
            attempt_read.is_err(),
            "lost history produced a readable attempt"
        );
    }
    let attempt = attempt_read.ok().flatten();
    let (charged, log, witness) = match snapshot {
        Ok(s) => (
            s.used.values().sum::<u64>() as i64,
            s.retained as i64,
            s.retained as i64,
        ),
        Err(_) => {
            assert!(
                loss.is_some() || action == "NotAdmitted" || phase == 6,
                "a valid model prefix became unreadable"
            );
            loss.map_or((0, 0, 0), |(used, witness)| (used, 0, witness))
        }
    };
    let observed = attempt
        .as_ref()
        .and_then(|a| a.observation.confirmed())
        .map_or(0, |o| {
            if o.outcome() == Outcome::Completed {
                1
            } else {
                2
            }
        });
    let actual_phase =
        if loss.is_some() || (charged == 0 && (action == "NotAdmitted" || phase == 6)) {
            6
        } else if permit.is_some() {
            1
        } else if invoked.is_some() {
            2
        } else if observed > 0 {
            3
        } else if attempt.is_some() {
            4
        } else if charged > 0 {
            5
        } else {
            0
        };
    let answer = invoked
        .as_ref()
        .and_then(|i| i.observation())
        .map_or(observed, |o| {
            if o.outcome() == Outcome::Completed {
                1
            } else {
                2
            }
        });
    next.insert("phase", actual_phase);
    next.insert("charged", charged);
    next.insert("entered", entered.get());
    next.insert("observed", observed);
    // This fixture has one unit debit; count actual remaining journal records,
    // not the boolean presence of the projection's final observation.
    next.insert("count", (log - charged).max(0));
    next.insert("answer", answer);
    next.insert("log", log);
    next.insert("witness", witness);
    next
}

#[test]
fn invocation_lifecycle_conforms_and_duplicate_entry_is_detected() {
    let model = aterm_models_db::resource_dispatch_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let steps: Vec<_> = aterm_spec::interp::reachable_states(&model)
        .iter()
        .flat_map(|state| {
            model
                .actions
                .iter()
                .filter(|action| model.action_enabled(action.name, state))
                .map(|action| (state.clone(), action.name))
                .collect::<Vec<_>>()
        })
        .collect();
    // Each step builds its own Lab in its own temporary home, so the steps share
    // nothing and run on every core.
    let reached =
        aterm_spec::check::each_on(aterm_spec::fs_lane::PLACES, &steps, |(state, action)| {
            let next = dispatch_transition(state, action);
            assert!(
                model.successors(action, state).contains(&next),
                "{action}: {state:?} -> {next:?}"
            );
            next
        });
    let step = |from: &aterm_spec::interp::State, action: &str| {
        steps
            .iter()
            .zip(&reached)
            .find(|((state, name), _)| state == from && *name == action)
            .map(|(_, next)| next.clone())
            .expect("the step is enabled from a reachable state")
    };
    let admitted = step(&model.init_state(), "Admit");
    let mut duplicate = step(&admitted, "Complete");
    duplicate.insert("entered", 2);
    assert!(!model.check_invariant("AtMostOneEntry", &duplicate));
}
