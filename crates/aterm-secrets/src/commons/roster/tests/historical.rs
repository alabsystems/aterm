// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn rotated(f: &mut Fixture) {
    let node = rotate(&ROOT, &f.eligibility, &f.journal, f.grants(2)).unwrap();
    f.journal.ingest(node.bytes()).unwrap();
}

fn selected(f: &mut Fixture) -> (Node, Node) {
    let left = rotate(&ROOT, &f.eligibility, &f.journal, f.grants(2)).unwrap();
    let right = rotate(&ROOT, &f.eligibility, &f.journal, f.grants(2)).unwrap();
    f.journal.ingest(left.bytes()).unwrap();
    f.journal.ingest(right.bytes()).unwrap();
    let recovery = Node::seal(
        &ROOT,
        NETWORK,
        Edit::Recover {
            heads: &[left.id(), right.id()],
            selected: left.id(),
        },
        &[],
        0,
    )
    .unwrap();
    f.journal.ingest(recovery.bytes()).unwrap();
    rotated(f);
    (left, right)
}

fn historical_scenario(action: &str) -> bool {
    let mut f = Fixture::new(2);
    let original = current(&f.journal, &f.identities[0]).unwrap().unwrap();
    let expected = key(&original, &f.journal);
    let mut source = f.genesis.id();
    let access = match action {
        "Current" | "Missing" | "ForeignRecipient" => original,
        "Rotated" => {
            rotated(&mut f);
            current(&f.journal, &f.identities[0]).unwrap().unwrap()
        }
        "Stale" => {
            rotated(&mut f);
            original
        }
        "Forked" => {
            let left = rotate(&ROOT, &f.eligibility, &f.journal, f.grants(2)).unwrap();
            let right = rotate(&ROOT, &f.eligibility, &f.journal, f.grants(2)).unwrap();
            f.journal.ingest(left.bytes()).unwrap();
            f.journal.ingest(right.bytes()).unwrap();
            original
        }
        "RetiredLocal" => {
            let access = current(&f.journal, &f.identities[1]).unwrap().unwrap();
            let node = rotate_retiring(
                &ROOT,
                &f.eligibility,
                &f.journal,
                f.grants(1),
                &[f.requests[1].binding().actor()],
            )
            .unwrap();
            f.journal.ingest(node.bytes()).unwrap();
            assert!(current(&f.journal, &f.identities[1]).is_err());
            return access
                .historical(&f.journal, &f.identities[1], source)
                .is_ok();
        }
        "Selected" | "Discarded" => {
            let (left, right) = selected(&mut f);
            source = if action == "Selected" {
                left.id()
            } else {
                right.id()
            };
            current(&f.journal, &f.identities[0]).unwrap().unwrap()
        }
        _ => panic!("unknown historical action"),
    };
    if action == "Missing" {
        source = [93; 32];
    }
    let identity = &f.identities[usize::from(action == "ForeignRecipient")];
    match access.historical(&f.journal, identity, source) {
        Ok(history) => {
            assert_eq!(*history.source(), source);
            let read = history.with_key(&f.journal, |key| *key).unwrap();
            if source == f.genesis.id() {
                assert_eq!(read, expected);
            }
            true
        }
        Err(_) => false,
    }
}

fn historical_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let scenario = [
        "Current",
        "Rotated",
        "Stale",
        "Missing",
        "ForeignRecipient",
        "Forked",
        "RetiredLocal",
        "Selected",
        "Discarded",
    ]
    .iter()
    .position(|name| *name == action)
    .unwrap()
        + 1;
    let mut next = state.clone();
    next.insert("scenario", scenario as i64);
    next.insert("exposed", i64::from(historical_scenario(action)));
    next
}

#[test]
fn historical_key_authorization_conforms_and_discarded_branch_mutant_is_detected() {
    let model = aterm_models_db::historical_access_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let initial = model.init_state();
    for action in &model.actions {
        let actual = historical_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&actual),
            "{}: {actual:?}",
            action.name
        );
    }
    let mut mutant = initial.clone();
    mutant.insert("scenario", 9);
    mutant.insert("exposed", 1);
    assert!(!model.successors("Discarded", &initial).contains(&mutant));
    assert!(!model.check_invariant("OnlyCurrentSelectedReleases", &mutant));
}

#[test]
fn historical_handle_is_withdrawn_after_the_next_control_edit() {
    let mut f = Fixture::new(2);
    let access = current(&f.journal, &f.identities[0]).unwrap().unwrap();
    let past = access
        .historical(&f.journal, &f.identities[0], f.genesis.id())
        .unwrap();
    assert!(past.with_key(&f.journal, |_| ()).is_ok());
    rotated(&mut f);
    assert!(past.with_key(&f.journal, |_| ()).is_err());
}

#[test]
fn new_member_needs_explicit_historical_key_handoff() {
    let f = Fixture::new(2);
    let members = current(&f.journal, &f.identities[0])
        .unwrap()
        .unwrap()
        .members;
    let (genesis, _) = prepare(&ROOT, NETWORK, 1, members[..1].to_vec(), None, &[]).unwrap();
    let (next, _) = prepare(&ROOT, NETWORK, 2, members, Some(genesis.id()), &[]).unwrap();
    let directory = aterm_tempfile::tempdir().unwrap();
    let mut journal = Journal::open(
        directory.path(),
        Trust {
            network: NETWORK,
            authority: authority(&ROOT).unwrap(),
            genesis: genesis.id(),
        },
    )
    .unwrap();
    journal.ingest(genesis.bytes()).unwrap();
    journal.ingest(next.bytes()).unwrap();
    let reader = current(&journal, &f.identities[1]).unwrap().unwrap();
    assert!(reader.with_key(&journal, |_| ()).is_ok());
    assert!(
        reader
            .historical(&journal, &f.identities[1], genesis.id())
            .is_err()
    );
    let original = current(&journal, &f.identities[0]).unwrap().unwrap();
    assert!(
        original
            .historical(&journal, &f.identities[0], genesis.id())
            .is_ok()
    );
}
