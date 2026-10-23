// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of the fork→exec child window and the write-all loop: each holds, and each law
//! is broken by its own mutant.

use aterm_models_session::{fork_exec_model, write_all_model};
use aterm_spec::derive::Model;
use aterm_spec::interp;

/// The laws the mutant at `dial` breaks.
fn broken_at(m: &Model, dial: i64) -> Vec<&'static str> {
    interp::violated_invariants(&interp::with_buggy(m, dial))
        .into_iter()
        .collect()
}

#[test]
fn the_fork_exec_window_runs_only_safe_steps_and_closes_the_master() {
    let m = fork_exec_model();
    assert_eq!(broken_at(&m, 1), ["OnlySafeBeforeExec"]);
    assert_eq!(broken_at(&m, 2), ["MasterClosedBeforeExec"]);
    assert_eq!(broken_at(&m, 3), ["SafeImpliesEnvPrebuilt"]);
    let mut s = m.init_state();
    for action in ["Fork", "Setrlimit", "Chdir", "CloseMaster", "Exec"] {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    assert_eq!((s["pc"], s["masterClosed"], s["unsafeOpRan"]), (5, 1, 0));
    assert!(!m.action_enabled("UnsafeEnvOp", &m.init_state()));
}

#[test]
fn the_write_all_loop_reports_completion_only_at_the_end() {
    let m = write_all_model();
    assert_eq!(broken_at(&m, 1), ["NoSilentDrop"]);
}
