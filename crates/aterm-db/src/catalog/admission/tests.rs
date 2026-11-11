// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{crypto::Causal, table::tests::Lab};
use aterm_secrets::commons::roster::Role;
use aterm_spec::interp::State as ModelState;

fn check(scenario: i64, mutant: u8) -> ModelState {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let descriptor = Descriptor::application(bootstrap.trust(), "admission", "data", 1).unwrap();
    let mut catalog = directory.catalog(&session).unwrap();
    match scenario {
        1 | 12 => {
            catalog
                .commit_catalog(&session, &descriptor.encode().unwrap())
                .unwrap();
            if scenario == 12 {
                let peer = lab.bootstrap(1).request().unwrap().binding().actor();
                assert!(
                    catalog
                        .sign_cutoff(&session, peer, crate::table::store::Signing::Fresh)
                        .unwrap()
                );
                assert!(
                    catalog
                        .metadata_evidence(&session)
                        .unwrap()
                        .iter()
                        .any(|item| item.cutoff && item.catalog_eligible && item.record.is_some())
                );
            }
        }
        2 => {
            catalog
                .commit_catalog(&session, b"authenticated inert catalog payload")
                .unwrap();
        }
        3..=5 | 8..=10 => {
            let peer = lab.bootstrap(1);
            let author = Session::open(&peer).unwrap();
            let missing = [[61; 32]];
            let missing_parent = matches!(scenario, 3 | 9);
            let parents: &[[u8; 32]] = if missing_parent { &missing } else { &[] };
            let wire = crate::table::tests::seal(
                &author,
                directory.catalog_scope(),
                Causal {
                    sequence: if missing_parent { 2 } else { 1 },
                    previous: missing_parent.then_some(missing[0]),
                    parents,
                },
                &descriptor.encode().unwrap(),
            )
            .unwrap();
            catalog.ingest(&session, &wire).unwrap();
            if matches!(scenario, 5 | 10) {
                let other = if scenario == 10 {
                    descriptor.clone()
                } else {
                    Descriptor::application(bootstrap.trust(), "fork", "data", 1).unwrap()
                };
                let fork = crate::table::tests::seal(
                    &author,
                    directory.catalog_scope(),
                    Causal {
                        sequence: 1,
                        previous: None,
                        parents: &[],
                    },
                    &other.encode().unwrap(),
                )
                .unwrap();
                catalog.ingest(&session, &fork).unwrap();
            }
        }
        13..=17 => {
            let peer = lab.bootstrap(1);
            let author = Session::open(&peer).unwrap();
            let mut wires = Vec::new();
            for role in ["left", "right"] {
                let declared =
                    Descriptor::application(bootstrap.trust(), "proof", role, 1).unwrap();
                let mut payload = declared.encode().unwrap();
                if scenario == 15 {
                    payload = vec![0; MAX_PAYLOAD + 1];
                    payload[0] = u8::from(role == "right");
                }
                let missing = [[92; 32]];
                let previous = (scenario == 16).then_some(missing[0]);
                let parents: &[[u8; 32]] = if matches!(scenario, 16 | 17) {
                    &missing
                } else {
                    &[]
                };
                let mut wire = crate::table::tests::seal(
                    &author,
                    directory.catalog_scope(),
                    Causal {
                        sequence: if scenario == 16 {
                            RECORDS_PER_CREATOR as u64 + 1
                        } else {
                            1
                        },
                        previous,
                        parents,
                    },
                    &payload,
                )
                .unwrap();
                if scenario == 14 {
                    let at = wire.len() - 65;
                    wire[at] ^= 1;
                    crate::table::tests::resign(&peer, &mut wire);
                }
                wires.push(wire);
            }
            if scenario >= 14 {
                assert!(catalog.ingest(&session, &wires[0]).is_err());
                assert!(catalog.ingest(&session, &wires[1]).is_err());
            }
            let proof = crate::table::conflict::Conflict::from_pair(
                &session,
                directory.catalog_scope(),
                &wires[0],
                &wires[1],
            )
            .unwrap();
            catalog.ingest_conflict(&session, proof.wire()).unwrap();
            assert_eq!(catalog.inventory(&session).unwrap().count(), 0);
            assert!(catalog.metadata_evidence(&session).unwrap().is_empty());
        }
        _ => {}
    }
    drop(catalog);
    if (8..=10).contains(&scenario) {
        // This is the same reservation ordinary discovery creates, without
        // completing enrollment or granting new table authority.
        aterm_uds::private_file::create_private_dir(&directory.path(descriptor.scope).unwrap())
            .unwrap();
    }
    drop(session);
    if matches!(scenario, 4 | 8) {
        lab.rotate(&[Role::Writer, Role::Reader]);
    }
    let bootstrap = lab.bootstrap(if scenario == 11 { 1 } else { 0 });
    let session = Session::open(&bootstrap).unwrap();
    let catalog = match scenario {
        11 => Directory::new(&lab.paths[1], bootstrap.trust())
            .catalog(&session)
            .unwrap(),
        6 => Store::create(&session, &lab.paths[0].home.join("different-table")).unwrap(),
        7 => directory
            .recovery()
            .enroll(&session, directory.catalog_scope())
            .unwrap(),
        _ => directory
            .observe_store(&session, directory.catalog_scope())
            .unwrap(),
    };
    if scenario == 1 {
        let (_, mut evidence) = catalog.catalog_and_evidence(&session).unwrap();
        assert!(
            catalog
                .catalog_evidence_covers_retained(&session, &evidence)
                .unwrap()
        );
        assert!(
            evidence
                .iter()
                .any(|item| item.catalog_eligible && item.record.is_none())
        );
        evidence.clear();
        assert!(
            !catalog
                .catalog_evidence_covers_retained(&session, &evidence)
                .unwrap()
        );
    }
    let files = || {
        lab.paths
            .iter()
            .map(|paths| crate::tests::files(&paths.dir))
            .collect::<Vec<_>>()
    };
    let before = files();
    // These fixtures really omit a retained declaration from ordinary discovery.
    // They are not synthetic boolean versions of the completeness predicate.
    if (3..=5).contains(&scenario) {
        assert!(
            !directory
                .snapshot_existing(&session)
                .unwrap()
                .entries
                .contains_key(&descriptor.scope.digest())
        );
        assert!(!catalog.metadata_evidence(&session).unwrap().is_empty());
    }
    MUTANT.set(mutant);
    crate::table::store::CATALOG_PROOF_POLICY_OFF.set(mutant == 4);
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            MUTANT.set(0);
            crate::table::store::CATALOG_PROOF_POLICY_OFF.set(false);
        }
    }
    let _reset = Reset;
    let result = directory.snapshot_from_primary(&session, &catalog);
    let accepted = result.is_ok();
    let present = result
        .as_ref()
        .is_ok_and(|snapshot| snapshot.entries.contains_key(&descriptor.scope.digest()));
    if scenario == 2 && mutant == 0 {
        assert_eq!(result.as_ref().unwrap().inert, 1);
    }
    assert_eq!(files(), before, "catalog admission mutated source custody");
    ModelState::from([
        ("scenario", scenario),
        ("done", 1),
        ("accepted", i64::from(accepted)),
        ("present", i64::from(present)),
    ])
}

pub(crate) fn transition(state: &ModelState, action: &str) -> ModelState {
    match action {
        "Case" => {
            let mut next = state.clone();
            next.insert("scenario", state["scenario"] + 1);
            next
        }
        "Read" => check(state["scenario"], 0),
        _ => panic!("unknown catalog admission action {action}"),
    }
}

#[test]
fn catalog_admission_refines_complete_evidence_with_real_hidden_declarations() {
    let model = aterm_models_db::catalog_admission_model();
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
                cases += usize::from(action.name == "Read");
            }
        }
    }
    assert_eq!(cases, 18);
    for scenario in [3, 4, 5, 9, 10, 13] {
        assert!(
            !model.check_invariant("PartialRefused", &check(scenario, 1)),
            "the bypass must expose a real hidden declaration in case {scenario}"
        );
    }
    assert!(!model.check_invariant("CompleteAvailable", &check(0, 2)));
    assert!(!model.check_invariant("DeclarationPreserved", &check(1, 2)));
    assert!(!model.check_invariant("DeclarationPreserved", &check(8, 1)));
    assert!(!model.check_invariant("PartialRefused", &check(11, 3)));
    for scenario in 15..=17 {
        assert!(!model.check_invariant("CompleteAvailable", &check(scenario, 4)));
    }
}
