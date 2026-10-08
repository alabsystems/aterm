// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The small model-walking helpers the `derived_*` files share: one copy,
//! mounted once by `spec_suite.rs` and reached as `crate::walk`.

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};

/// `m` with only the invariant `name` kept: the interpreter's whole-space
/// check of that one claim.
pub fn only(m: &Model, name: &str) -> Model {
    let mut one = m.clone();
    one.invariants.retain(|inv| inv.name == name);
    assert_eq!(one.invariants.len(), 1, "{name} is an invariant");
    one
}

/// Fire `actions` in order from `m`'s initial state, each one required to be
/// enabled.
pub fn walk(m: &Model, actions: &[&str]) -> State {
    let mut state = m.init_state();
    for action in actions {
        assert!(m.fire(action, &mut state), "{action}: {state:?}");
    }
    state
}

/// Every state `m` reaches (first successor per action), invariants unchecked.
pub fn reachable(m: &Model) -> Vec<State> {
    // Walked resolved: `fire` takes an action's first successor, and
    // `Resolved::successors` lists the same successors in the same order,
    // without the string-keyed map `fire` builds per call.
    let r = interp::Resolved::new(m).expect("the model resolves");
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([r.init()]);
    let mut out = Vec::new();
    while let Some(s) = queue.pop_front() {
        if seen.contains(&s) {
            continue;
        }
        for a in 0..m.actions.len() {
            if let Some(next) = r.successors(a, &s).into_iter().next() {
                queue.push_back(next);
            }
        }
        out.push(r.state(&s));
        seen.insert(s);
    }
    out
}

/// Every action the committed machine (`Buggy = 0`) can take — an audit's
/// subject; the others are its negative controls.
pub fn committed_actions(m: &Model) -> BTreeSet<&'static str> {
    interp::fired_actions(&interp::with_buggy(m, 0))
}

/// Run `ty check <spec> --config <cfg> [extra...]`; return (exit-0, combined output).
pub fn ty_check(ty: &PathBuf, spec: &Path, cfg: &Path, extra: &[&str]) -> (bool, String) {
    let mut cmd = Command::new(ty);
    cmd.arg("check")
        .arg(spec)
        .arg("--config")
        .arg(cfg)
        .args(extra);
    // ARMED: partial-order reduction is a speed feature that does not
    // preserve coverage, and the `ty` this machine discovers has an
    // UNSOUND one (it reduced a 128-state model to 1 and still printed
    // "No errors found (exhaustive)"). Every `ty check` in this workspace
    // arms the whole space through the one shared place.
    aterm_spec::verify::arm_whole_space_check(&mut cmd);
    let out = aterm_spec::verify::ty_output(&mut cmd).expect("run ty check");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}
