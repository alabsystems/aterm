// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Replay determinism, boundary restore, observation chain, and divergence tests.

use aterm_aos::checkpoint::{Checkpoint, Provenance};
use aterm_aos::effects::NoEffects;
use aterm_aos::replay::{Divergence, chain_link, empty_chain, first_divergence, replay};
use aterm_aos::task::Schema;
use aterm_aos::wire::Enc;
use aterm_aos::{BranchId, Revision};

use crate::toy::{ToyInput, ToyObs, ToySchema, ToyState, Variant, adds, code, fold, identity};

fn inputs() -> Vec<ToyInput> {
    vec![
        ToyInput::Add(3),
        ToyInput::Tick,
        ToyInput::Add(4),
        ToyInput::Reset,
        ToyInput::Add(-2),
        ToyInput::Add(9),
        ToyInput::Tick,
        ToyInput::Add(1),
    ]
}

fn provenance_at(cursor: u64) -> Provenance {
    Provenance {
        identity: identity(Variant::Good),
        log_epoch: 1,
        input_cursor: cursor,
        effect_cursor: 0,
        branch: BranchId(0),
        revision: Revision(0),
        parent: None,
    }
}

#[test]
fn replaying_the_same_inputs_twice_gives_the_same_run() {
    for variant in [Variant::Good, Variant::Stranded] {
        let a = fold(variant, ToyState::default(), &inputs());
        let b = fold(variant, ToyState::default(), &inputs());
        assert_eq!(a.state, b.state);
        assert_eq!(a.state_digest, b.state_digest);
        assert_eq!(a.observations, b.observations);
        assert_eq!(a.observation_chain, b.observation_chain);
        assert_eq!(a.refused, b.refused);
        assert_eq!(first_divergence(&a, &b), None);
    }
}

#[test]
fn restoring_at_every_boundary_and_replaying_the_rest_equals_the_straight_fold() {
    let schema = ToySchema::new();
    let all = inputs();
    let straight = fold(Variant::Good, ToyState::default(), &all);
    for k in 0..=all.len() {
        let prefix = fold(Variant::Good, ToyState::default(), &all[..k]);
        let cursor = k as u64;
        let c = Checkpoint::capture(&schema, &prefix.state, provenance_at(cursor));
        let c = Checkpoint::decode(&c.encode().unwrap()).unwrap();
        let restored = c.restore(&schema, &identity(Variant::Good)).unwrap();
        let rest = replay(
            &schema,
            &code(Variant::Good),
            restored,
            &all[k..],
            cursor,
            &mut NoEffects,
        );
        assert_eq!(rest.state_digest, straight.state_digest, "boundary {k}");
        assert_eq!(rest.state, straight.state, "boundary {k}");
        assert_eq!(
            rest.observations,
            straight.observations[k..],
            "boundary {k}"
        );
    }
}

#[test]
fn negative_control_a_restore_that_drops_a_field_diverges_at_some_boundary() {
    let schema = ToySchema::new();
    let lossy = ToySchema::lossy();
    let all = inputs();
    let straight = fold(Variant::Stranded, ToyState::default(), &all);
    let mut diverged = Vec::new();
    for k in 0..=all.len() {
        let prefix = fold(Variant::Stranded, ToyState::default(), &all[..k]);
        let c = Checkpoint::capture(&lossy, &prefix.state, provenance_at(k as u64));
        let restored = c.restore(&lossy, &identity(Variant::Good)).unwrap();
        let rest = replay(
            &schema,
            &code(Variant::Stranded),
            restored,
            &all[k..],
            k as u64,
            &mut NoEffects,
        );
        let mut tail = straight.clone();
        tail.observations.drain(..k);
        if first_divergence(&tail, &rest).is_some() {
            diverged.push(k);
        }
    }
    assert_eq!(diverged, vec![1, 6, 8]);
}

#[test]
fn a_replay_stops_at_the_first_refusal_and_runs_nothing_after_it() {
    let run = fold(Variant::Picky, ToyState::default(), &adds(&[1, 13, 2]));
    assert_eq!(run.observations, vec![(0, ToyObs::Added { total: 1 })]);
    let (cursor, refusal) = run.refused.clone().unwrap();
    assert_eq!((cursor, refusal.law), (1, "toy.no-thirteen"));
    assert_eq!(run.state.total, 1);
    assert_eq!(
        run.state_digest,
        fold(Variant::Picky, ToyState::default(), &adds(&[1])).state_digest
    );
}

#[test]
fn the_observation_chain_commits_to_every_observation_and_its_cursor() {
    let schema = ToySchema::new();
    let empty = fold(Variant::Good, ToyState::default(), &[]);
    assert_eq!(empty.observation_chain, empty_chain());

    let run = fold(Variant::Good, ToyState::default(), &adds(&[5, 6]));
    let mut chain = empty_chain();
    for (cursor, obs) in &run.observations {
        let mut e = Enc::new();
        schema.encode_observation(obs, &mut e);
        chain = chain_link(&chain, *cursor, e.as_bytes());
    }
    assert_eq!(run.observation_chain, chain);

    let shifted = replay(
        &schema,
        &code(Variant::Good),
        ToyState::default(),
        &adds(&[5, 6]),
        100,
        &mut NoEffects,
    );
    assert_eq!(shifted.observations.len(), 2);
    assert_eq!(shifted.observations[0].0, 100);
    assert_ne!(shifted.observation_chain, run.observation_chain);
}

#[test]
fn first_divergence_names_an_observation_first() {
    let a = fold(
        Variant::Good,
        ToyState::default(),
        &[ToyInput::Add(1), ToyInput::Tick],
    );
    let b = fold(
        Variant::Good,
        ToyState::default(),
        &[ToyInput::Add(2), ToyInput::Tick],
    );
    assert_eq!(
        first_divergence(&a, &b),
        Some(Divergence::Observation { cursor: 0 })
    );
    let s = fold(
        Variant::Stranded,
        ToyState::default(),
        &[ToyInput::Add(1), ToyInput::Tick],
    );
    assert_eq!(
        first_divergence(&a, &s),
        Some(Divergence::Observation { cursor: 1 })
    );
}

#[test]
fn first_divergence_names_a_refusal_one_run_made_and_the_other_did_not() {
    let picky = fold(Variant::Picky, ToyState::default(), &adds(&[1, 13, 2]));
    let good = fold(Variant::Good, ToyState::default(), &adds(&[1, 13, 2]));
    assert_eq!(
        first_divergence(&picky, &good),
        Some(Divergence::Refused { cursor: 1 })
    );
    assert_eq!(
        first_divergence(&good, &picky),
        Some(Divergence::Refused { cursor: 1 })
    );
}

#[test]
fn first_divergence_names_a_length_difference_with_no_refusal() {
    let short = fold(Variant::Good, ToyState::default(), &adds(&[1]));
    let long = fold(Variant::Good, ToyState::default(), &adds(&[1, 2]));
    assert_eq!(
        first_divergence(&short, &long),
        Some(Divergence::Length { a: 1, b: 2 })
    );
}

#[test]
fn first_divergence_names_end_states_that_differ_under_equal_observations() {
    let a = fold(Variant::Good, ToyState::default(), &adds(&[1]));
    let b = fold(
        Variant::Good,
        ToyState {
            seen: 5,
            ..ToyState::default()
        },
        &adds(&[1]),
    );
    assert_eq!(a.observations, b.observations);
    assert_eq!(
        first_divergence(&a, &b),
        Some(Divergence::State {
            a: a.state_digest,
            b: b.state_digest
        })
    );
}
