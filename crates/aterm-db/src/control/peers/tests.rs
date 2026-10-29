// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn actor(n: usize) -> Actor {
    let mut actor = [0; 48];
    actor[..8].copy_from_slice(&(n as u64).to_le_bytes());
    actor
}

#[test]
fn capacity_never_evicts_a_live_lease_and_idle_eviction_is_oldest_first() {
    let cache = Cache::default();
    let mut first = Repair::default();
    let held = cache.acquire(actor(0), &mut first).unwrap();
    assert!(
        cache.0.try_lock().is_ok(),
        "no lock is held by a live lease"
    );
    for n in 1..CAPACITY {
        let mut repair = Repair::default();
        drop(cache.acquire(actor(n), &mut repair).unwrap());
    }
    let mut next = Repair::default();
    drop(cache.acquire(actor(CAPACITY), &mut next).unwrap());
    let pool = cache.0.lock().unwrap();
    assert!(pool.active.contains_key(&actor(0)));
    assert_eq!(pool.active.len() + pool.idle.len(), CAPACITY);
    assert!(!pool.idle.iter().any(|(id, _)| *id == actor(1)));
    drop(pool);
    drop(held);

    let cache = Cache::default();
    let mut repairs = vec![Repair::default(); CAPACITY];
    let leases: Vec<_> = repairs
        .iter_mut()
        .enumerate()
        .map(|(n, repair)| cache.acquire(actor(n), repair).unwrap())
        .collect();
    assert!(cache.acquire(actor(CAPACITY), &mut next).is_err());
    assert_eq!(cache.0.lock().unwrap().active.len(), CAPACITY);
    drop(leases);
    assert_eq!(cache.0.lock().unwrap().idle.len(), CAPACITY);
}

#[test]
fn a_lone_channel_in_either_direction_needs_no_preference_or_wait() {
    let cache = Cache::default();
    let mut repair = Repair::default();
    for preferred in [false, true] {
        drop(
            cache
                .acquire_on_with(actor(1), &mut repair, preferred, Instant::now(), || {
                    panic!("a lone authenticated channel must not wait")
                })
                .unwrap(),
        );
    }
}

#[test]
fn only_a_preferred_contender_waits_and_it_keeps_released_progress() {
    use std::{sync::mpsc, thread, time::Duration};

    let cache = Cache::default();
    let mut active = Repair::default();
    let mut held = cache
        .acquire_on(actor(1), &mut active, false, Instant::now())
        .unwrap();
    held.cursor = Some([7; 32]);
    let (waiting, observed) = mpsc::channel();
    thread::scope(|scope| {
        let winner = scope.spawn(|| {
            let mut repair = Repair::default();
            let mut waiting = Some(waiting);
            let lease = cache
                .acquire_on_with(
                    actor(1),
                    &mut repair,
                    true,
                    Instant::now() + Duration::from_secs(60),
                    || {
                        if let Some(waiting) = waiting.take() {
                            waiting.send(()).unwrap();
                        }
                    },
                )
                .unwrap();
            assert_eq!(lease.cursor, Some([7; 32]));
        });
        observed.recv_timeout(Duration::from_secs(60)).unwrap();
        let pool = cache.0.lock().unwrap();
        assert_eq!(pool.active.get(&actor(1)), Some(&false));
        drop(pool);
        let mut losing = Repair::default();
        assert!(cache.acquire(actor(1), &mut losing).is_err());
        drop(held);
        winner.join().unwrap();
    });
    assert!(cache.0.lock().unwrap().active.is_empty());
}

#[test]
fn a_preferred_collision_obeys_the_original_exchange_deadline() {
    let cache = Cache::default();
    let mut active = Repair::default();
    let _held = cache.acquire(actor(1), &mut active).unwrap();
    let mut repair = Repair::default();
    let error = cache
        .acquire_on_with(actor(1), &mut repair, true, Instant::now(), || {
            panic!("an exhausted exchange must not enter a wait")
        })
        .err()
        .unwrap();
    assert!(error.to_string().contains("exchange deadline"));
}

#[test]
fn unwind_returns_progress_and_caller_keeps_its_report() {
    let cache = Cache::default();
    let mut repair = Repair::default();
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut lease = cache.acquire(actor(1), &mut repair).unwrap();
        lease.cursor = Some([3; 32]);
        lease.application = Some(crate::replica::Report::default());
        panic!("interrupted round");
    }));
    assert!(unwound.is_err());
    assert_eq!(repair.cursor, Some([3; 32]));
    assert!(repair.application.is_some());
    let mut fresh = Repair::default();
    let resumed = cache.acquire(actor(1), &mut fresh).unwrap();
    assert_eq!(resumed.cursor, Some([3; 32]));
    assert!(
        resumed.application.is_none(),
        "a new round must not inherit a stale report"
    );
}

/// Run every enabled transition against actual cache and lease objects.
pub fn cache_transition(
    state: &aterm_spec::interp::State,
    action: &str,
    forget: bool,
) -> aterm_spec::interp::State {
    let cache = Cache::default();
    if state["a"] > 1 {
        let mut seed = Repair::default();
        let mut lease = cache.acquire(actor(1), &mut seed).unwrap();
        lease.cursor = Some([3; 32]);
    }
    let cache = if forget { Cache::default() } else { cache };
    let mut a = Repair::default();
    let mut b = Repair::default();
    let mut next_a = Repair::default();
    let mut next_b = Repair::default();
    let mut held_a = if state["a"] == 1 || state["a"] == 3 {
        Some(cache.acquire(actor(1), &mut a).unwrap())
    } else {
        None
    };
    let mut held_b = if state["b"] == 1 {
        Some(cache.acquire(actor(2), &mut b).unwrap())
    } else {
        None
    };
    let mut next = state.clone();
    match action {
        "Acquire" => held_a = Some(cache.acquire(actor(1), &mut next_a).unwrap()),
        "Duplicate" => {
            let mut other = Repair::default();
            next.insert(
                "duplicate",
                i64::from(cache.acquire(actor(1), &mut other).is_ok()),
            );
        }
        "Advance" => {
            held_a.as_mut().unwrap().cursor = Some([3; 32]);
            next.insert("wrote", 1);
        }
        "Release" => {
            drop(held_a.take());
        }
        "OtherAcquire" => held_b = Some(cache.acquire(actor(2), &mut next_b).unwrap()),
        "OtherRelease" => {
            drop(held_b.take());
        }
        _ => panic!("unknown cache transition"),
    }
    let progress = if let Some(lease) = &held_a {
        lease.cursor.is_some()
    } else {
        cache
            .0
            .lock()
            .unwrap()
            .idle
            .iter()
            .find(|(id, _)| *id == actor(1))
            .is_some_and(|(_, repair)| repair.cursor.is_some())
    };
    next.insert("a", i64::from(held_a.is_some()) + 2 * i64::from(progress));
    next.insert("b", i64::from(held_b.is_some()));
    next
}

#[test]
fn leases_conform_and_forgetting_returned_progress_is_detected() {
    let model = aterm_models_db::peer_repair_cache_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let states = aterm_spec::interp::reachable_states(&model);
    for state in &states {
        for action in &model.actions {
            let expected = model.successors(action.name, state);
            if expected.is_empty() {
                continue;
            }
            let actual = cache_transition(state, action.name, false);
            assert!(
                expected.contains(&actual),
                "{}: {state:?} -> {actual:?}",
                action.name
            );
        }
    }
    let saved = states
        .iter()
        .find(|state| state["a"] == 2 && state["b"] == 0)
        .unwrap();
    let broken = cache_transition(saved, "Acquire", true);
    assert!(!model.successors("Acquire", saved).contains(&broken));
    assert!(!model.check_invariant("RetainedProgress", &broken));
}
