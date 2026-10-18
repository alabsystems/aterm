// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Conformance: the real `transact()` obeys the derived no-lost-update discipline.
//!
//! `aterm_models_session::transact_model()`: a transaction commits only if no
//! write intervened since it read the head (`CommitClean`, `seq = tbase`);
//! otherwise it aborts (`Abort`, `seq > tbase`) without clobbering the concurrent
//! write. Each real `Surface::transact` outcome is checked against the model's
//! own guard (`Model::action_enabled`), and a conflicted transaction must apply
//! nothing. Pure Rust, so it always runs.

use crate::support::write_cap;
use aterm_buffer::{Edit, Surface, TxnOutcome};
use aterm_models_session::transact_model;
use std::collections::BTreeMap;

/// Project the state of a transaction attempting to commit (`active = 1`)
/// against `tbase`, with no loss yet. Heads stay small so the model's
/// `seq <= MaxSeq - K` bound does not interfere.
fn attempting(seq: u64, tbase: u64) -> BTreeMap<&'static str, i64> {
    [
        ("seq", seq as i64),
        ("tbase", tbase as i64),
        ("active", 1),
        ("lost", 0),
    ]
    .into_iter()
    .collect()
}

#[test]
fn real_transact_clean_commit_matches_model() {
    let m = transact_model();
    let mut s = Surface::new();
    let base = s.seq(); // read the head; no concurrent write follows
    let st = attempting(s.seq().0, base.0); // seq == tbase

    assert!(
        m.action_enabled("CommitClean", &st),
        "model: clean commit enabled when seq == base"
    );
    assert!(
        !m.action_enabled("Abort", &st),
        "model: abort disabled when there is no conflict"
    );

    let outcome = s.transact(&write_cap(), base, vec![Edit::AppendLine("x".into())]);
    assert!(
        matches!(outcome, TxnOutcome::Committed(_)),
        "real transact must commit when base == head"
    );
}

#[test]
fn real_transact_conflict_aborts_no_lost_update() {
    let m = transact_model();
    let mut s = Surface::new();
    let base = s.seq(); // txn reads the head at base
    // A concurrent write advances the head past `base`.
    s.apply(&write_cap(), Edit::AppendLine("concurrent".into()));
    let st = attempting(s.seq().0, base.0); // seq > tbase

    assert!(
        m.action_enabled("Abort", &st),
        "model: a conflict must abort (seq > tbase)"
    );
    assert!(
        !m.action_enabled("CommitClean", &st),
        "model: a conflict must not clean-commit"
    );

    let head_before = s.seq().0;
    let outcome = s.transact(&write_cap(), base, vec![Edit::AppendLine("txn".into())]);
    assert!(
        matches!(outcome, TxnOutcome::Conflict),
        "real transact must CONFLICT under a concurrent write, not commit against a stale base"
    );
    // A conflicted transaction applied nothing, so the concurrent write survives.
    assert_eq!(
        s.seq().0,
        head_before,
        "a conflicted transact must apply no edit (no lost update)"
    );
}
