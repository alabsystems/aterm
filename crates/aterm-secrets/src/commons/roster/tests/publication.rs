// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn scenario(action: &str) -> (bool, bool) {
    let mut f = Fixture::new(2);
    let mut source = f.genesis.id();
    if matches!(action, "FutureDescendant" | "ReusedSource") {
        let copy = f.raw(
            Some(source),
            f.payload().as_slice(),
            f.genesis.facts(),
            &[f.identities[0].recipient(), f.identities[1].recipient()],
        );
        f.journal.ingest(copy.bytes()).unwrap();
        if action == "ReusedSource" {
            source = copy.id();
        }
    } else if action != "Original" {
        let (left, right) = if action == "MergedPastCopy" {
            (
                f.raw(
                    Some(f.genesis.id()),
                    b"opaque selected control policy",
                    &[],
                    &[f.identities[0].recipient(), f.identities[1].recipient()],
                ),
                rotate(&ROOT, &f.eligibility, &f.journal, f.grants(2)).unwrap(),
            )
        } else {
            let left = rotate(&ROOT, &f.eligibility, &f.journal, f.grants(2)).unwrap();
            let payload = left.open(&f.identities[0]).unwrap();
            let right = f.raw(
                Some(f.genesis.id()),
                payload.as_slice(),
                left.facts(),
                &[f.identities[0].recipient(), f.identities[1].recipient()],
            );
            (left, right)
        };
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
        for node in [&left, &right, &recovery] {
            f.journal.ingest(node.bytes()).unwrap();
        }
        source = left.id();
        if action == "DiscardedSource" {
            source = right.id();
        } else if action == "MergedPastCopy" {
            let payload = right.open(&f.identities[0]).unwrap();
            let copy = f.raw(
                Some(recovery.id()),
                payload.as_slice(),
                right.facts(),
                &[f.identities[0].recipient(), f.identities[1].recipient()],
            );
            source = copy.id();
            f.journal.ingest(copy.bytes()).unwrap();
            let roster = decode(payload.as_slice()).unwrap();
            assert!(!f.journal.single_historical_key_publication(
                source,
                roster.generation,
                roster.commitment,
            ));
            // A selected-parent-only walk sees only the copy. The original
            // publication is behind the recovery's discarded parent.
            assert!(!f.journal.selected_ancestor(right.id(), source));
            assert!(f.journal.selected_ancestor(left.id(), source));
        } else {
            assert_eq!(action, "FutureSibling");
        }
    }
    // Even a selected recovery of a repeated key is not current authority.
    let current_before_rotation = matches!(current(&f.journal, &f.identities[0]), Ok(Some(_)));
    let fresh = rotate(&ROOT, &f.eligibility, &f.journal, f.grants(2)).unwrap();
    f.journal.ingest(fresh.bytes()).unwrap();
    let access = current(&f.journal, &f.identities[0]).unwrap().unwrap();
    let opened = access.historical(&f.journal, &f.identities[0], source);
    if let Ok(ref history) = opened {
        assert_eq!(*history.source(), source);
        history.with_key(&f.journal, |_| ()).unwrap();
    }
    (opened.is_ok(), current_before_rotation)
}

pub(super) fn publication_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let number = [
        "Original",
        "FutureDescendant",
        "ReusedSource",
        "FutureSibling",
        "MergedPastCopy",
        "DiscardedSource",
    ]
    .iter()
    .position(|name| *name == action)
    .unwrap()
        + 1;
    let (historical, current) = scenario(action);
    let mut next = state.clone();
    next.insert("scenario", number as i64);
    next.insert("historical", i64::from(historical));
    next.insert("current", i64::from(current));
    next
}

#[test]
fn publication_conforms_and_global_history_and_relaxed_current_mutants_are_detected() {
    let model = aterm_models_db::historical_publication_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let initial = model.init_state();
    for action in &model.actions {
        let actual = publication_transition(&initial, action.name);
        assert!(
            model.successors(action.name, &initial).contains(&actual),
            "{}: {actual:?}",
            action.name
        );
    }
    // Reinstall the former global history rule on the original genesis after a
    // future copy. It denies the good source even after fresh access is restored.
    let mut f = Fixture::new(2);
    let copy = f.raw(
        Some(f.genesis.id()),
        f.payload().as_slice(),
        f.genesis.facts(),
        &[f.identities[0].recipient(), f.identities[1].recipient()],
    );
    f.journal.ingest(copy.bytes()).unwrap();
    let decoded = decode(f.payload().as_slice()).unwrap();
    let mut global = initial.clone();
    global.insert("scenario", 2);
    global.insert(
        "historical",
        i64::from(
            f.journal
                .single_key_publication(decoded.generation, decoded.commitment),
        ),
    );
    assert!(!model.check_invariant("OriginalEvidenceSurvives", &global));
    assert!(
        !model
            .successors("FutureDescendant", &initial)
            .contains(&global)
    );

    // The old genesis is unique in its own prefix, but that cannot authorize
    // reusing its key as the current epoch after the copy has arrived.
    let mut relaxed = initial;
    relaxed.insert("scenario", 2);
    relaxed.insert("historical", 1);
    relaxed.insert(
        "current",
        i64::from(f.journal.single_historical_key_publication(
            f.genesis.id(),
            decoded.generation,
            decoded.commitment,
        )),
    );
    assert!(!model.check_invariant("CurrentReuseRefused", &relaxed));
}
