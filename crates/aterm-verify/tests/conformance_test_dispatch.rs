// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 of `TestDispatch` (`aterm_models_capacity::test_dispatch_model`): the real budget
//! dispatcher `aterm_verify::test_plan::dispatch` runs three binaries charged 2, 2 and 1 of a
//! share of 3 on three workers, its observer seam (`dispatch_observed`) logging `Start<i>` as a
//! binary takes its charge and `End<i>` as it returns it, both under the budget's lock, so the
//! log is in the order the budget moved. Every run's log is replayed through the model: each
//! step must be the one successor the model allows. NEGATIVE CONTROL: the same binaries started
//! on three workers with no budget make a log the model refuses.

use std::sync::{Barrier, Mutex};

use aterm_models_capacity::test_dispatch_model;
use aterm_verify::test_plan::{Step, dispatch_observed, plan};

fn plan_of_three() -> aterm_verify::test_plan::Plan {
    let mut p = plan(
        &["b0", "b1", "b2"],
        &std::collections::BTreeMap::new(),
        3,
        3,
    );
    p.order = vec![0, 1, 2];
    p.charge = vec![2, 2, 1];
    p
}

/// Whether the model takes `log` step by step, one successor each.
fn refines(log: &[String]) -> Result<(), String> {
    let model = test_dispatch_model();
    let mut state = model.init_state();
    for step in log {
        let mut next = model.successors(step, &state);
        if next.len() != 1 {
            return Err(format!("{step} does not refine at {state:?} (log {log:?})"));
        }
        state = next.pop().expect("one successor");
    }
    Ok(())
}

#[test]
fn the_budget_dispatcher_refines_test_dispatch() {
    let p = plan_of_three();
    for _ in 0..50 {
        let log = Mutex::new(Vec::new());
        let observe = |step: Step| {
            log.lock().unwrap().push(match step {
                Step::Start(i) => format!("Start{i}"),
                Step::End(i) => format!("End{i}"),
            });
        };
        dispatch_observed(&p, 3, &observe, |_, _| ());
        let log = log.into_inner().unwrap();
        assert_eq!(log.len(), 6, "{log:?}");
        refines(&log).unwrap();
    }

    // The negative control: no budget. Every worker takes the next binary and they all start
    // before any ends, so the second charge of 2 starts beside the first.
    let log = Mutex::new(Vec::new());
    let all_started = Barrier::new(3);
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..3 {
            scope.spawn(|| {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                {
                    let mut log = log.lock().unwrap();
                    log.push(format!("Start{i}"));
                }
                all_started.wait();
                log.lock().unwrap().push(format!("End{i}"));
            });
        }
    });
    let mut log = log.into_inner().unwrap();
    log.sort_by_key(|s| (s.starts_with("End"), s.clone()));
    assert!(refines(&log).is_err(), "{log:?}");
}
