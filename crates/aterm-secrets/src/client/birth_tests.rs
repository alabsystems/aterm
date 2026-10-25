// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::{
    ENTRY,
    binding::Pending,
    bootstrap::{Bootstrap, Descriptor},
    journal::Journal,
    node::{Edit, Node},
    roster::{Grant, Role},
};
use aterm_spec::interp::State;

fn descriptor(admin: &Admin) -> Descriptor {
    admin_keys::control_descriptor(&admin.doc).unwrap().unwrap()
}

fn birth_transition(state: &State, action: &str) -> State {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join("birth.vault");
    let create = if action == "Legacy" {
        test_create_unbound
    } else {
        create
    };
    create(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    let first = admin_open(&paths).unwrap();
    assert_eq!(first.seq(), 1);
    let (scenario, accepted, stable) = if action == "Legacy" {
        assert!(
            admin_keys::control_descriptor(&first.doc)
                .unwrap()
                .is_none()
        );
        sync(&paths, None).unwrap();
        admin_publish(&paths, &first, &first.doc).unwrap();
        let after = admin_open(&paths).unwrap();
        assert!(
            admin_keys::control_descriptor(&after.doc)
                .unwrap()
                .is_none()
        );
        (6, true, true)
    } else {
        let trust = descriptor(&first).trust();
        let canonical = Node::foundation(first.commons_authority().unwrap()).unwrap();
        assert_eq!(trust.genesis, canonical.id());
        let bootstrap = Bootstrap::required(&paths).unwrap();
        assert!(bootstrap.access().unwrap().is_some());
        assert!(bootstrap.witness().unwrap().is_some());
        match action {
            "Create" => (1, true, true),
            "Rotate" => {
                for _ in 0..2 {
                    let admin = admin_open(&paths).unwrap();
                    admin_rotate(&paths, &admin, &admin.doc).unwrap();
                }
                let after = admin_open(&paths).unwrap();
                assert_ne!(first.config.signer, after.config.signer);
                (2, true, descriptor(&after).trust() == trust)
            }
            "RemoveRoot" => {
                let mut doc = first.doc.clone();
                doc.unset_entry(&Audience::Hosts(vec!["m1".into()]), ENTRY)
                    .unwrap();
                let accepted = admin_publish(&paths, &first, &doc).is_ok();
                (
                    3,
                    accepted,
                    descriptor(&admin_open(&paths).unwrap()).trust() == trust,
                )
            }
            "ReplaceRoot" => {
                let replacement = Node::seal(
                    &first.commons_seed,
                    trust.network,
                    Edit::Create {
                        hosts: &[],
                        payload: b"another root",
                    },
                    &[machine_identity(&paths).unwrap().recipient()],
                    0,
                )
                .unwrap();
                let mut doc = first.doc.clone();
                doc.set_entry(
                    &Audience::Hosts(vec!["m1".into()]),
                    ENTRY,
                    &Descriptor::from_genesis(replacement).unwrap().bytes(),
                )
                .unwrap();
                let accepted = admin_publish(&paths, &first, &doc).is_ok();
                (
                    4,
                    accepted,
                    descriptor(&admin_open(&paths).unwrap()).trust() == trust,
                )
            }
            "Enroll" => {
                let request = bootstrap.request().unwrap();
                let (mut pending, challenge) =
                    Pending::issue(&request, trust.network, trust.authority).unwrap();
                let possession = pending
                    .verify(&bootstrap.device().answer(&request, &challenge).unwrap())
                    .unwrap();
                let mut journal = Journal::open_existing(bootstrap.journal_dir(), trust).unwrap();
                let certificate = first
                    .approve_commons_member(&journal, "m1", possession)
                    .unwrap();
                let node = first
                    .prepare_commons_rotation(
                        &journal,
                        vec![Grant::certified("m1", certificate, Role::Writer)],
                    )
                    .unwrap();
                journal.ingest(node.bytes()).unwrap();
                let published = Descriptor::from_journal(&journal).unwrap();
                drop(journal);
                let mut doc = first.doc.clone();
                doc.set_entry(
                    &Audience::Hosts(vec!["m1".into()]),
                    ENTRY,
                    &published.bytes(),
                )
                .unwrap();
                admin_publish(&paths, &first, &doc).unwrap();
                (
                    5,
                    true,
                    descriptor(&admin_open(&paths).unwrap()).trust() == trust,
                )
            }
            _ => panic!("unknown birth action"),
        }
    };
    let after = admin_open(&paths).unwrap();
    let access = Bootstrap::cached(&paths)
        .unwrap()
        .and_then(|b| b.access().unwrap());
    let available = access.is_some();
    let writer = access.is_some_and(|a| a.local().role() == Role::Writer);
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("bound", i64::from(after.commons_bound));
    next.insert("stable", i64::from(stable));
    next.insert("available", i64::from(available));
    next.insert("writer", i64::from(writer));
    next.insert("accepted", i64::from(accepted));
    if scenario == 3 || scenario == 4 {
        assert_eq!(
            after.seq(),
            first.seq(),
            "rejected root edits publish nothing"
        );
    }
    next
}

#[test]
fn birth_conforms_through_rotation_root_mutations_enrollment_and_legacy_sync() {
    let model = aterm_models_db::foundation_birth_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = birth_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {next:?}",
                    action.name
                );
            }
        }
    }
    let initial = model.init_state();
    let mut mutant = birth_transition(&initial, "RemoveRoot");
    mutant.insert("accepted", 1);
    assert!(!model.successors("RemoveRoot", &initial).contains(&mutant));
    assert!(!model.check_invariant("NoRootReplacement", &mutant));
}
