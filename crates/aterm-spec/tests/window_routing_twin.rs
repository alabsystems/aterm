// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Executable twin of the derived `WindowRouting` model: a BFS through `Model::successors`,
//! which enumerates every branch of a nondeterministic update (CloseWindow's
//! `frontmost' \in 1..next_id-1`), checks every invariant at every reachable state, so the
//! interpreter the Tier-1 bind reuses cannot drift from the spec `ty` proves. Needs no checker
//! binary.

use aterm_spec::derive::window_routing_model;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

type State = BTreeMap<&'static str, i64>;

fn key(s: &State) -> Vec<(&'static str, i64)> {
    s.iter().map(|(k, v)| (*k, *v)).collect()
}

#[test]
fn window_routing_twin_holds_invariants_over_all_reachable_states() {
    let m = window_routing_model();
    let actions = [
        "CreateWindow",
        "CloseWindow",
        "ConnectedCreateWindow",
        "RaiseSession",
    ];
    let invariants = ["ExitIffEmpty", "FrontmostLive", "FrontmostAllocated"];

    let init = m.init_state();
    for inv in invariants {
        assert!(
            m.check_invariant(inv, &init),
            "{inv} violated in the init state {init:?}"
        );
    }

    // The model is bounded (MaxWin/MaxId), so the BFS terminates.
    let mut seen: BTreeSet<Vec<(&'static str, i64)>> = BTreeSet::new();
    let mut queue: VecDeque<State> = VecDeque::new();
    seen.insert(key(&init));
    queue.push_back(init);

    let mut states = 0usize;
    while let Some(state) = queue.pop_front() {
        states += 1;
        for action in actions {
            for next in m.successors(action, &state) {
                for inv in invariants {
                    assert!(
                        m.check_invariant(inv, &next),
                        "{inv} violated after {action}: {state:?} -> {next:?}",
                    );
                }
                if seen.insert(key(&next)) {
                    queue.push_back(next);
                }
            }
        }
    }

    // Init, a create and a close at least: an inert model fails here.
    assert!(
        states >= 3,
        "reachable graph too small ({states}); model may be inert"
    );
    eprintln!("WindowRouting twin: {states} reachable states, all invariants hold.");
}
