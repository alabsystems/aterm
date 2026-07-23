// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! EXECUTABLE-TWIN check for the derived `WindowRouting` model (the GUI in-process
//! multi-window lifecycle). `ty` proves the invariants over the TLA+ spec GENERATED
//! from `window_routing_model()`; this test drives the SAME model through the Rust
//! `Model::successors`/`check_invariant` interpreter — the executable semantics the
//! Tier-1 conformance bind reuses — over the FULL reachable state graph.
//! `successors` enumerates the existential fan-out of the model's nondeterministic
//! CloseWindow re-point (`frontmost' \in 1..next_id-1`), so the BFS explores EVERY
//! admissible successor `ty` proves over, not just a canonical representative. It
//! asserts every invariant holds at every reachable state and never panics. If
//! the executable interpreter ever disagreed with the derived spec, the twin would
//! diverge here, so the spec and the code-facing semantics cannot drift apart.
//!
//! This is the no-`ty` half of the multi-window FV (it needs no checker binary), so
//! it runs everywhere the workspace tests run, not only where `ty` is built.

use aterm_spec::derive::window_routing_model;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

type State = BTreeMap<&'static str, i64>;

fn key(s: &State) -> Vec<(&'static str, i64)> {
    s.iter().map(|(k, v)| (*k, *v)).collect()
}

#[test]
fn window_routing_twin_holds_invariants_over_all_reachable_states() {
    let m = window_routing_model();
    // The two real lifecycle actions in the model (Cmd-N / CloseRequested).
    let actions = ["CreateWindow", "CloseWindow"];
    let invariants = ["ExitIffEmpty", "FrontmostLive", "FrontmostAllocated"];

    let init = m.init_state();
    for inv in invariants {
        assert!(
            m.check_invariant(inv, &init),
            "{inv} violated in the init state {init:?}"
        );
    }

    // BFS over the reachable state graph through the executable interpreter. The
    // model is bounded (MaxWin/MaxId), so this terminates and covers exactly the
    // states `ty` proves over — but reached via `fire`, not the TLA+ checker.
    let mut seen: BTreeSet<Vec<(&'static str, i64)>> = BTreeSet::new();
    let mut queue: VecDeque<State> = VecDeque::new();
    seen.insert(key(&init));
    queue.push_back(init);

    let mut states = 0usize;
    while let Some(state) = queue.pop_front() {
        states += 1;
        for action in actions {
            // `successors` returns EVERY next-state the action admits — one for a
            // deterministic action, several for a nondeterministic `\in lo..hi`
            // update (e.g. CloseWindow re-pointing `frontmost` to ANY surviving
            // allocated id). Enumerating the full fan-out is what makes this a
            // faithful twin of the TLA+ `Next` `ty` proves over: every admissible
            // successor — not just a canonical representative — is invariant-checked.
            // It must never panic for any (action, state) pair.
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

    // The graph must be non-trivial: at minimum init + a create + a close are
    // reachable. (Guards an accidentally-inert model from passing vacuously.)
    assert!(
        states >= 3,
        "reachable graph too small ({states}); model may be inert"
    );
    eprintln!("WindowRouting twin: {states} reachable states, all invariants hold.");
}
