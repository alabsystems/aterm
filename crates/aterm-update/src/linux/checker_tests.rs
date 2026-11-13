// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::cell::Cell;

#[test]
fn a_refused_initial_or_replacement_thread_remains_retryable() {
    let watch = CheckerWatch::new();
    let first = watch.register(60);
    let refuse = || Err(std::io::Error::from(std::io::ErrorKind::WouldBlock));
    assert!(!launch(&watch, first, refuse));
    assert_eq!(watch.snapshot().unwrap().phase, CheckerPhase::StartFailed);
    let next = watch.supersede(first).unwrap();
    assert!(!launch(&watch, next, refuse));
    assert_eq!(watch.snapshot().unwrap().respawns, 0);
    let last = watch.supersede(next).unwrap();
    assert!(launch(&watch, last, || Ok(())));
    assert_eq!(watch.snapshot().unwrap().phase, CheckerPhase::Starting);
}

#[test]
fn a_panicked_pass_retries_and_a_retired_worker_stops() {
    let watch = CheckerWatch::new();
    let generation = watch.register(60);
    let passes = Cell::new(0);
    let pauses = Cell::new(0);
    let panics = Cell::new(0);
    run(
        &watch,
        generation,
        |_| {
            passes.set(passes.get() + 1);
            assert_eq!(watch.snapshot().unwrap().phase, CheckerPhase::Checking);
            if passes.get() == 1 {
                panic!("injected failed check");
            }
            true
        },
        || {
            pauses.set(pauses.get() + 1);
            assert_eq!(watch.snapshot().unwrap().phase, CheckerPhase::Waiting);
            if pauses.get() == 2 {
                watch.supersede(generation).unwrap();
            }
            false
        },
        || panics.set(panics.get() + 1),
    );
    assert_eq!((passes.get(), pauses.get(), panics.get()), (2, 2, 1));
    assert_eq!(watch.snapshot().unwrap().checks, 1);
    run(
        &watch,
        generation,
        |_| panic!("retired worker ran"),
        || false,
        || {},
    );
}

#[test]
fn a_pass_without_the_lock_does_not_claim_completed_work() {
    let watch = CheckerWatch::new();
    let generation = watch.register(60);
    run(
        &watch,
        generation,
        |_| false,
        || {
            watch.supersede(generation).unwrap();
            false
        },
        || panic!("no pass panicked"),
    );
    assert_eq!(watch.snapshot().unwrap().checks, 0);
}

#[test]
fn a_network_retry_survives_busy_passes_and_is_consumed_by_owned_work() {
    use std::cell::RefCell;
    let watch = CheckerWatch::new();
    let generation = watch.register(60);
    let seen = RefCell::new(Vec::new());
    let pauses = Cell::new(0);
    run(
        &watch,
        generation,
        |trigger| {
            seen.borrow_mut().push(trigger);
            let count = seen.borrow().len();
            if count == 4 {
                watch.supersede(generation).unwrap();
            }
            count != 2
        },
        || {
            pauses.set(pauses.get() + 1);
            pauses.get() == 1
        },
        || panic!("no pass panicked"),
    );
    assert_eq!(
        *seen.borrow(),
        [
            CheckTrigger::Scheduled,
            CheckTrigger::NetworkMoved,
            CheckTrigger::NetworkMoved,
            CheckTrigger::Scheduled,
        ]
    );
}
