// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Conformance: the real `transact()` obeys the DERIVED no-lost-update discipline.
//!
//! `aterm_spec::derive::transact_model()` is the drift-free model of optimistic
//! concurrency: a transaction commits its edits atomically ONLY if no concurrent
//! write intervened since it read the head (`seq == tbase`); otherwise it aborts,
//! never clobbering the concurrent write (a lost update). The model encodes this
//! as `CommitClean` (guard `seq = tbase`) vs `Abort` (guard `seq > tbase`), and
//! `ty` proves `NoLostUpdate` (Buggy=1 → counterexample) in
//! `aterm-spec/tests/derived_ring_ty.rs`.
//!
//! This binds that model to the code that runs — `Surface::transact`, which
//! returns `Conflict` when `self.seq() != base` and otherwise applies all edits.
//! Each real outcome is checked against the model's OWN guard
//! (`Model::action_enabled`), and the conflict case asserts the no-lost-update
//! safety directly: a conflicted transaction applies nothing. Pure Rust + real
//! code, so it always runs.

use aterm_buffer::{Edit, Surface, SurfaceId, TxnOutcome, WriteCap};
use aterm_spec::derive::transact_model;
use std::collections::BTreeMap;
use std::num::NonZeroU64;

fn surface(id: u64) -> Surface {
    Surface::new(SurfaceId(NonZeroU64::new(id).unwrap()))
}

/// Project the optimistic-CC state the model reasons about, for a txn attempting
/// to commit (`active = 1`) against base version `tbase`, with no loss yet.
/// (Heads are kept small so the model's exhaustive-bound guard `seq <= MaxSeq - K`
/// does not interfere — the discipline under test is `seq == tbase`.)
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
    let mut s = surface(1);
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

    let outcome = s.transact(&WriteCap, base, vec![Edit::AppendLine("x".into())]);
    assert!(
        matches!(outcome, TxnOutcome::Committed(_)),
        "real transact must commit when base == head"
    );
}

#[test]
fn real_transact_conflict_aborts_no_lost_update() {
    let m = transact_model();
    let mut s = surface(2);
    let base = s.seq(); // txn reads the head at base
    // A concurrent write advances the head past `base`.
    s.apply(&WriteCap, Edit::AppendLine("concurrent".into()));
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
    let outcome = s.transact(&WriteCap, base, vec![Edit::AppendLine("txn".into())]);
    assert!(
        matches!(outcome, TxnOutcome::Conflict),
        "real transact must CONFLICT under a concurrent write, not commit against a stale base"
    );
    // The no-lost-update safety, observed directly: a conflicted transaction
    // applied NOTHING, so the concurrent write is not clobbered.
    assert_eq!(
        s.seq().0,
        head_before,
        "a conflicted transact must apply no edit (no lost update)"
    );
}
