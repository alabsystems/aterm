// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    catalog::Directory,
    control::{Endpoint, Repair, Side},
    table::{Session, store::Store, tests::Lab},
};
use aterm_json::json;
use aterm_net::lan::{Connection, Scope as LinkScope};
use aterm_secrets::commons::roster::Role;
use std::{collections::BTreeMap, net::TcpListener, thread, time::Duration};

#[path = "dispatch_tests.rs"]
mod dispatch;

fn allocation(lab: &Lab, shares: &[u64]) -> Allocation {
    let bootstrap = lab.bootstrap(0);
    let terms = Terms {
        resource: [41; 32],
        window: [42; 32],
        unit: "requests".into(),
        limit: shares.iter().sum(),
        shares: shares
            .iter()
            .enumerate()
            .map(|(i, share)| {
                (
                    lab.bootstrap(i).request().unwrap().binding().actor(),
                    *share,
                )
            })
            .collect(),
    };
    let proposed = Proposal::prepare(bootstrap.trust(), terms).unwrap();
    Allocation::verify(proposed.bytes(), proposed.fingerprint(), bootstrap.trust()).unwrap()
}
fn resources(lab: &Lab, n: usize, allocation: &Allocation) -> Resources {
    Resources::at(lab.paths[n].clone(), allocation.clone())
}
fn rounds(lab: &Lab) {
    let endpoints: Vec<_> = (0..2)
        .map(|n| {
            Endpoint::from_bootstrap(&lab.bootstrap(n))
                .unwrap()
                .unwrap()
                .with_commons(lab.paths[n].clone(), lab.paths[n].home.join("config.toml"))
                .unwrap()
        })
        .collect();
    let (mut a, mut b) = (Repair::default(), Repair::default());
    for _ in 0..3 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::scope(|threads| {
            let remote = threads.spawn(|| {
                endpoints[1].exchange(
                    Connection::admit(listener.accept().unwrap().0, LinkScope::Loopback).unwrap(),
                    Side::Responder,
                    &mut b,
                )
            });
            endpoints[0]
                .exchange(
                    Connection::connect(address, LinkScope::Loopback, Duration::from_secs(60))
                        .unwrap(),
                    Side::Initiator,
                    &mut a,
                )
                .unwrap();
            remote.join().unwrap().unwrap();
        });
    }
}

#[test]
fn one_machine_reserves_offline_and_exact_retries_keep_the_original_receipt() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[3]);
    let db = resources(&lab, 0, &allocation);
    assert!(db.reserve([1; 16], 1).is_err());
    db.initialize().unwrap();
    let first = db.reserve([1; 16], 2).unwrap();
    assert!(first.fresh);
    let reopened = resources(&lab, 0, &allocation);
    let retry = reopened.reserve([1; 16], 2).unwrap();
    assert!(!retry.fresh);
    assert_eq!(retry.receipt, first.receipt);
    assert!(reopened.reserve([1; 16], 1).is_err());
    assert!(reopened.reserve([2; 16], 2).is_err());
    reopened.reserve([2; 16], 1).unwrap();
    assert!(reopened.reserve([3; 16], 1).is_err());
    assert_eq!(reopened.snapshot().unwrap().retained, 2);
    assert_eq!(reopened.snapshot().unwrap().used.values().sum::<u64>(), 3);
}

#[test]
fn full_width_reservations_do_not_wrap_and_concurrent_requests_cannot_double_spend() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[u64::MAX]);
    let db = resources(&lab, 0, &allocation);
    db.initialize().unwrap();
    db.reserve([1; 16], u64::MAX).unwrap();
    assert!(db.reserve([2; 16], 1).is_err());
    assert_eq!(
        db.snapshot().unwrap().used.values().copied().next(),
        Some(u64::MAX)
    );

    let lab = Lab::new(&[Role::Writer]);
    let allocation = self::allocation(&lab, &[1]);
    let db = resources(&lab, 0, &allocation);
    db.initialize().unwrap();
    let barrier = std::sync::Barrier::new(2);
    let accepted = thread::scope(|threads| {
        let tasks: Vec<_> = [1, 2]
            .into_iter()
            .map(|request| {
                let barrier = &barrier;
                let db = &db;
                threads.spawn(move || {
                    barrier.wait();
                    db.reserve([request; 16], 1).is_ok()
                })
            })
            .collect();
        tasks
            .into_iter()
            .map(|task| usize::from(task.join().unwrap()))
            .sum::<usize>()
    });
    assert_eq!(accepted, 1);
    assert_eq!(db.snapshot().unwrap().used.values().sum::<u64>(), 1);
}

#[test]
fn concurrent_initializers_cannot_retain_two_pins_for_one_window() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[1]);
    let mut terms = allocation.terms().clone();
    terms.limit = 2;
    let proposal = Proposal::prepare(allocation.trust(), terms).unwrap();
    let other =
        Allocation::verify(proposal.bytes(), proposal.fingerprint(), allocation.trust()).unwrap();
    let left = resources(&lab, 0, &allocation);
    let right = resources(&lab, 0, &other);
    let barrier = std::sync::Barrier::new(2);
    let accepted = thread::scope(|threads| {
        let a = threads.spawn(|| {
            barrier.wait();
            left.initialize().is_ok()
        });
        let b = threads.spawn(|| {
            barrier.wait();
            right.initialize().is_ok()
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_ne!(accepted.0, accepted.1);
    let (winner, loser) = if accepted.0 {
        (&left, &right)
    } else {
        (&right, &left)
    };
    winner.reserve([1; 16], 1).unwrap();
    assert!(loser.reserve([2; 16], 1).is_err());
    assert_eq!(winner.snapshot().unwrap().used.values().sum::<u64>(), 1);
}

#[test]
fn two_machines_spend_only_disjoint_shares_then_converge_over_existing_repair() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let allocation = allocation(&lab, &[2, 3]);
    let a = resources(&lab, 0, &allocation);
    let b = resources(&lab, 1, &allocation);
    assert_eq!(a.initialize().unwrap(), b.initialize().unwrap());
    a.reserve([1; 16], 2).unwrap();
    b.reserve([1; 16], 3).unwrap();
    assert!(a.reserve([2; 16], 1).is_err());
    assert!(b.reserve([2; 16], 1).is_err());
    rounds(&lab);
    assert_eq!(a.snapshot().unwrap(), b.snapshot().unwrap());
    assert_eq!(a.snapshot().unwrap().used.values().sum::<u64>(), 5);
    assert!(!b.reserve([1; 16], 3).unwrap().fresh);
}

#[test]
fn default_replication_discovers_the_table_without_activating_local_accounting() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let allocation = allocation(&lab, &[1, 1]);
    let a = resources(&lab, 0, &allocation);
    let b = resources(&lab, 1, &allocation);
    let scope = a.initialize().unwrap();
    a.reserve([1; 16], 1).unwrap();
    rounds(&lab);
    let bootstrap = lab.bootstrap(1);
    let session = Session::open(&bootstrap).unwrap();
    let discovered = Directory::new(&lab.paths[1], bootstrap.trust())
        .snapshot(&session)
        .unwrap();
    assert_eq!(
        discovered.entries[&scope.digest()].state,
        crate::catalog::State::Subscribed
    );
    drop(session);
    assert!(b.snapshot().is_err());
    assert!(b.reserve([1; 16], 1).is_err());
    b.initialize().unwrap();
    assert_eq!(b.snapshot().unwrap().used.values().sum::<u64>(), 1);
    b.reserve([1; 16], 1).unwrap();
}

#[test]
fn another_pin_for_the_same_resource_window_never_resets_this_installation() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[3]);
    let a = resources(&lab, 0, &allocation);
    a.initialize().unwrap();
    a.reserve([1; 16], 2).unwrap();
    let mut changed = allocation.terms().clone();
    changed.limit = 10;
    let proposed = Proposal::prepare(allocation.trust(), changed).unwrap();
    let other =
        Allocation::verify(proposed.bytes(), proposed.fingerprint(), allocation.trust()).unwrap();
    let b = resources(&lab, 0, &other);
    assert!(b.initialize().is_err());
    assert!(b.reserve([2; 16], 1).is_err());
    assert!(b.snapshot().is_err());
    assert_eq!(a.snapshot().unwrap().used.values().sum::<u64>(), 2);
    assert!(!a.reserve([1; 16], 2).unwrap().fresh);
}

fn raw(lab: &Lab, allocation: &Allocation, debit: ledger::Debit) {
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let scope = descriptor(&bootstrap, allocation).unwrap().scope;
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let mut store = Store::open(&session, &directory.path(scope).unwrap(), scope).unwrap();
    store.commit_catalog(&session, &debit.encode()).unwrap();
}

#[test]
fn a_decreasing_or_conflicting_counter_is_retained_and_blocks_further_reservation() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[3]);
    let db = resources(&lab, 0, &allocation);
    db.initialize().unwrap();
    db.reserve([1; 16], 2).unwrap();
    raw(
        &lab,
        &allocation,
        ledger::Debit {
            allocation: allocation.id(),
            request: [2; 16],
            cost: 1,
            cumulative: 1,
        },
    );
    assert!(db.snapshot().is_err());
    assert!(db.reserve([3; 16], 1).is_err());
}

#[test]
fn old_request_retries_are_idempotent_but_new_stale_counter_records_refuse() {
    let lab = Lab::new(&[Role::Writer]);
    let allocation = allocation(&lab, &[3]);
    let db = resources(&lab, 0, &allocation);
    db.initialize().unwrap();
    let first = db.reserve([1; 16], 1).unwrap();
    db.reserve([2; 16], 1).unwrap();
    let retry = db.reserve([1; 16], 1).unwrap();
    assert!(!retry.fresh);
    assert_eq!(retry.receipt, first.receipt);
    raw(
        &lab,
        &allocation,
        ledger::Debit {
            allocation: allocation.id(),
            request: [1; 16],
            cost: 1,
            cumulative: 1,
        },
    );
    assert!(db.snapshot().is_err());
    assert!(db.reserve([3; 16], 1).is_err());
}

#[test]
fn cli_proposals_do_not_activate_and_reservations_do_not_execute_provider_work() {
    let lab = Lab::new(&[Role::Writer]);
    let actor = hex::encode_lower(&lab.bootstrap(0).request().unwrap().binding().actor());
    let terms = aterm_json::to_string(
        &json!({"resource":hex::encode_lower(&[41;32]),"window":hex::encode_lower(&[42;32]),
        "unit":"requests","limit":2,"shares":{actor:2}}),
    )
    .unwrap();
    let proposal = command::run_at(
        "resources-propose",
        BTreeMap::from([("--terms", terms.as_str())]),
        lab.paths[0].clone(),
    )
    .unwrap();
    assert_eq!(proposal["authority"].as_str(), Some("none"));
    let wire = proposal["allocation"].as_str().unwrap();
    let pin = proposal["fingerprint"].as_str().unwrap();
    let options = || BTreeMap::from([("--allocation", wire), ("--pin", pin)]);
    assert!(command::run_at("resources-status", options(), lab.paths[0].clone()).is_err());
    command::run_at("resources-init", options(), lab.paths[0].clone()).unwrap();
    let request = hex::encode_lower(&[1; 16]);
    let mut opts = options();
    opts.insert("--request", &request);
    opts.insert("--cost", "2");
    let fresh = command::run_at("resources-reserve", opts.clone(), lab.paths[0].clone()).unwrap();
    let retry = command::run_at("resources-reserve", opts, lab.paths[0].clone()).unwrap();
    assert_eq!(fresh["fresh"].as_bool(), Some(true));
    assert_eq!(retry["fresh"].as_bool(), Some(false));
    assert_eq!(fresh["scope"].as_str(), Some("accounting-only"));
    assert_eq!(fresh["receipt_id"], retry["receipt_id"]);
    let report = command::run_at("resources-status", options(), lab.paths[0].clone()).unwrap();
    assert_eq!(
        report["shares"][0]["unreserved_upper_bound"].as_u64(),
        Some(0)
    );
}

pub(crate) fn reservation_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let two = matches!(action, "TwoOffline" | "NoShare");
    let lab = Lab::new(&if two {
        vec![Role::Writer; 2]
    } else {
        vec![Role::Writer]
    });
    let allocation = allocation(
        &lab,
        if action == "TwoOffline" {
            &[2, 3]
        } else {
            &[3]
        },
    );
    let db = resources(&lab, 0, &allocation);
    if action == "NoPin" {
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        Directory::new(&lab.paths[0], bootstrap.trust())
            .publish(&session, &descriptor(&bootstrap, &allocation).unwrap())
            .unwrap();
    } else if action != "Uninitialized" {
        db.initialize().unwrap();
    }
    let unpinned_read = action == "NoPin" && db.snapshot().is_ok();
    let mut original = None;
    if matches!(
        action,
        "Retry" | "WrongCost" | "WrongPin" | "BadHistory" | "Exhausted"
    ) {
        original = Some(
            db.reserve([1; 16], if action == "Exhausted" { 3 } else { 2 })
                .unwrap()
                .receipt,
        );
    }
    let result = match action {
        "Reserve" => db.reserve([1; 16], 3),
        "Retry" => db.reserve([1; 16], 2),
        "WrongCost" => db.reserve([1; 16], 1),
        "Exhausted" | "Uninitialized" | "NoPin" => db.reserve([2; 16], 1),
        "WrongPin" => {
            let mut terms = allocation.terms().clone();
            terms.limit = 10;
            let proposal = Proposal::prepare(allocation.trust(), terms).unwrap();
            let other =
                Allocation::verify(proposal.bytes(), proposal.fingerprint(), allocation.trust())
                    .unwrap();
            let other = resources(&lab, 0, &other);
            assert!(other.initialize().is_err());
            other.reserve([2; 16], 1)
        }
        "BadHistory" => {
            raw(
                &lab,
                &allocation,
                ledger::Debit {
                    allocation: allocation.id(),
                    request: [2; 16],
                    cost: 1,
                    cumulative: 1,
                },
            );
            assert!(db.snapshot().is_err());
            db.reserve([3; 16], 1)
        }
        "NoShare" => {
            let other = resources(&lab, 1, &allocation);
            other.initialize().unwrap();
            other.reserve([1; 16], 1)
        }
        "TwoOffline" => {
            let other = resources(&lab, 1, &allocation);
            other.initialize().unwrap();
            other.reserve([1; 16], 3).unwrap();
            db.reserve([1; 16], 2)
        }
        _ => panic!("unknown resource action"),
    };
    // Bad-history usage is the acknowledged prefix, not a spendable projection
    // of the corrupted tail. The API must refuse to report any such balance.
    let used = if action == "BadHistory" {
        2
    } else if matches!(action, "Uninitialized" | "NoPin") {
        0
    } else {
        db.snapshot().unwrap().used.values().sum::<u64>()
            + if action == "TwoOffline" {
                resources(&lab, 1, &allocation)
                    .snapshot()
                    .unwrap()
                    .used
                    .values()
                    .sum::<u64>()
            } else {
                0
            }
    };
    let receipt = result
        .as_ref()
        .ok()
        .map(|r| &r.receipt)
        .or(original.as_ref());
    let durable = receipt.is_some_and(|receipt| {
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let scope = descriptor(&bootstrap, &allocation).unwrap().scope;
        let directory = Directory::new(&lab.paths[0], bootstrap.trust());
        let store = Store::open(&session, &directory.path(scope).unwrap(), scope).unwrap();
        store.receipt(&session, &receipt.id).unwrap().as_ref() == Some(receipt)
    });
    let mut next = state.clone();
    next.insert(
        "scenario",
        match action {
            "Reserve" => 1,
            "Retry" => 2,
            "WrongCost" => 3,
            "Exhausted" => 4,
            "WrongPin" => 5,
            "BadHistory" => 6,
            "NoShare" => 7,
            "TwoOffline" => 8,
            "Uninitialized" => 9,
            "NoPin" => 10,
            _ => unreachable!(),
        },
    );
    next.insert("accepted", i64::from(result.is_ok() || unpinned_read));
    next.insert("fresh", i64::from(result.as_ref().is_ok_and(|r| r.fresh)));
    next.insert("durable", i64::from(durable));
    next.insert(
        "same",
        i64::from(result.as_ref().map_or(true, |r| {
            original.as_ref().is_none_or(|old| *old == r.receipt)
        })),
    );
    next.insert("used", used as i64);
    next.insert("capacity", if action == "TwoOffline" { 5 } else { 3 });
    next
}

#[test]
fn resource_reservations_conform_and_unsafe_receipts_are_detected() {
    let model = aterm_models_db::resource_reservation_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = reservation_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {next:?}",
                    action.name
                );
            }
        }
    }
    let mut unsafe_retry = reservation_transition(&model.init_state(), "Retry");
    unsafe_retry.insert("fresh", 1);
    assert!(!model.check_invariant("RetryIsNotFresh", &unsafe_retry));
    assert!(
        !model
            .successors("Retry", &model.init_state())
            .contains(&unsafe_retry)
    );
}
