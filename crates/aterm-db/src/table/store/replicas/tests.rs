// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::tests::{Lab, resign, seal};
use aterm_spec::interp::State;

fn certificate(lab: &Lab, actor: usize, scope: Scope, word: &str, invalid: bool) -> Vec<u8> {
    let bootstrap = lab.bootstrap(actor);
    let author = Session::open(&bootstrap).unwrap();
    let mut wires = Vec::new();
    for side in ["left", "right"] {
        let payload = crate::catalog::Descriptor::application(
            scope.trust(),
            "agreement",
            &format!("{word}.{side}"),
            1,
        )
        .unwrap()
        .encode()
        .unwrap();
        let mut wire = seal(
            &author,
            scope,
            Causal {
                sequence: 1,
                previous: None,
                parents: &[],
            },
            &payload,
        )
        .unwrap();
        if invalid {
            let at = wire.len() - 65;
            wire[at] ^= 1;
            resign(&bootstrap, &mut wire);
        }
        wires.push(wire);
    }
    Conflict::from_pair(&author, scope, &wires[0], &wires[1])
        .unwrap()
        .wire()
        .to_vec()
}

fn check(scenario: i64, mutant: u8) -> State {
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(bootstrap.paths(), bootstrap.trust());
    let scope = directory.catalog_scope();
    aterm_journal::create_private_dir_durable(&bootstrap.paths().dir.join("commons/tables"))
        .unwrap();
    let mut primary = directory.enroll(&session, scope).unwrap();
    let mut recovery = directory.recovery().enroll(&session, scope).unwrap();
    if scenario > 0 {
        let a = certificate(&lab, 1, scope, "primary", scenario == 4);
        let b = if scenario == 1 {
            a.clone()
        } else {
            certificate(
                &lab,
                if scenario == 5 { 2 } else { 1 },
                scope,
                "recovery",
                scenario == 3,
            )
        };
        primary.ingest_conflict(&session, &a).unwrap();
        recovery.ingest_conflict(&session, &b).unwrap();
        if scenario == 3 {
            assert!(
                !primary
                    .catalog_evidence_covers_retained(
                        &session,
                        &primary.metadata_evidence(&session).unwrap(),
                    )
                    .unwrap()
            );
            assert!(
                recovery
                    .catalog_evidence_covers_retained(
                        &session,
                        &recovery.metadata_evidence(&session).unwrap(),
                    )
                    .unwrap()
            );
        }
    }
    drop(primary);
    drop(recovery);
    let primary = directory.observe_store(&session, scope).unwrap();
    let recovery = directory.recovery().observe_store(&session, scope).unwrap();
    AGREEMENT_MUTANT.set(mutant);
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            AGREEMENT_MUTANT.set(0);
        }
    }
    let _reset = Reset;
    let merged = recovery.with_observed_primary(&session, primary);
    if scenario == 3 && mutant == 1 {
        let wrong = merged.as_ref().unwrap();
        // The old skip-by-actor policy erased the valid primary sources and
        // mistook the recovery's invalid ciphertext for complete evidence.
        assert!(
            wrong
                .catalog_evidence_covers_retained(
                    &session,
                    &wrong.metadata_evidence(&session).unwrap(),
                )
                .unwrap()
        );
    }
    State::from([
        ("scenario", scenario),
        ("done", 1),
        ("accepted", i64::from(merged.is_ok())),
        (
            "proofs",
            merged
                .as_ref()
                .map_or(0, |store| store.conflicts.len() as i64),
        ),
    ])
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    match action {
        "Case" => {
            let mut next = state.clone();
            next.insert("scenario", state["scenario"] + 1);
            next
        }
        "Combine" => check(state["scenario"], 0),
        _ => panic!("unknown agreement action {action}"),
    }
}

#[test]
fn replica_certificate_agreement_refines_complete_evidence() {
    let model = aterm_models_db::replica_certificate_agreement_model();
    aterm_spec::interp::prove_and_catch(&model);
    let mut cases = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
                cases += usize::from(action.name == "Combine");
            }
        }
    }
    assert_eq!(cases, 6);
    for scenario in [2, 3, 4] {
        assert!(!model.check_invariant("Agreement", &check(scenario, 1)));
    }
    assert!(!model.check_invariant("Available", &check(1, 2)));
    assert!(!model.check_invariant("Retained", &check(5, 3)));
}
