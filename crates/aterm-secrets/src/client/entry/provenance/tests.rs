// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::tests::Fixture;
use super::*;
use aterm_spec::interp::State as ModelState;

fn snapshot(paths: &Paths) -> EntrySnapshot {
    named_snapshot(paths, "token")
}

fn named_snapshot(paths: &Paths, name: &str) -> EntrySnapshot {
    read_verified_entry(paths, &config(paths).unwrap().unwrap(), name)
        .unwrap()
        .unwrap()
        .snapshot
}

/// A real healed split with readable current values but a retained keyed
/// epoch this recipient cannot open. Reuse the vault protocol's split fixture.
fn incomplete_audience() -> (EntrySnapshot, EntrySnapshot) {
    let _answering = ServersAnswering::set();
    let split = crate::client::remove::tests::keyed_split(false);
    let previous = named_snapshot(&split.m41, "shared");
    assert!(register(&previous).1.complete);
    split.heal();
    test_put_everyone(&split.macs.m22, "shared", b"v3").unwrap();
    assert!(
        !crate::client::remove::tend_epochs_at(
            &split.macs.m7,
            aterm_time::unix_millis().unwrap_or(0),
        )
        .unwrap()
        .is_empty()
    );
    split.heal();
    // A narrow edit can settle this visible register without acquiring the
    // old keyed epoch. That must not claim the whole audience is readable.
    test_put_everyone(&split.m41, "shared", b"entry-only-change").unwrap();
    sync(&split.m41, None).unwrap();
    let current = named_snapshot(&split.m41, "shared");
    let (history, selected) = register(&current);
    assert!(!selected.complete);
    assert!(!selected.ambiguous);
    assert_ne!(current.semantic_operation(), previous.semantic_operation());
    assert!(
        history
            .past_of(&[selected.operation])
            .holds(&register(&previous).1.operation)
    );
    split.macs.server.stop();
    (previous, current)
}

fn log_fixture() -> Fixture {
    let fixture = Fixture::new();
    seed_vault_genesis(&fixture.paths, &fixture._root.path().join("entry.genesis")).unwrap();
    fixture.publish(|doc| {
        doc.set_entry(&Audience::Everyone, "other", b"after-seeding")
            .unwrap();
    });
    assert_eq!(snapshot(&fixture.paths).origin(), EntryOrigin::Log);
    fixture
}

fn legacy_envelope(fixture: &Fixture) {
    let admin = admin_open(&fixture.paths).unwrap();
    let mut state = load_state(&fixture.paths).unwrap();
    state.seq += 1;
    let bytes = seal(
        &admin.config.name,
        state.seq,
        &admin.seed,
        &admin.commons_seed,
        admin.commons_bound,
        &admin.doc,
        &admin.handoff,
    )
    .unwrap();
    let envelope = envelope::parse(&bytes).unwrap();
    state.digest = Some(envelope.digest());
    fsx::write_atomic(&fixture.paths.cache_file(), &bytes, 0o600).unwrap();
    save_state(&fixture.paths, &state).unwrap();
}

fn register(snapshot: &EntrySnapshot) -> (&Log, &Register) {
    let Source::Log {
        history,
        register: Some(register),
    } = &snapshot.source
    else {
        panic!("expected a selected text register");
    };
    (history, register)
}

/// Independently signed sibling writes, admitted and materialized through the
/// real client. Equal-valued siblings have no value conflict but are concurrent.
fn concurrent(equal: bool) -> (EntrySnapshot, EntrySnapshot) {
    use crate::commons::bootstrap::Bootstrap;
    use crate::membership_log::EpochAudience;
    let _answering = ServersAnswering::set();
    let lab =
        crate::client::log_entries_tests::two_macs(&[(Audience::Everyone, "token", b"initial")]);
    let initial = snapshot(&lab.m22);
    let base = load_records(&lab.m22).unwrap().log().unwrap().clone();
    let identity = read_identity(&lab.m22).unwrap().unwrap();
    let opened = base.open_audience_entries(&identity).unwrap().unwrap();
    let epoch = opened.maximal_epochs[&EpochAudience::Everyone][0];
    let key = *opened.unwrapped_keys[&epoch];
    let draft = |paths: &Paths, value: &[u8]| {
        let bootstrap = Bootstrap::required(paths).unwrap();
        let request = bootstrap.request().unwrap();
        bootstrap.device().with_seed(|seed| {
            base.draft_put(
                &request.binding().actor(),
                2_000,
                epoch,
                &key,
                &Entry {
                    name: "token".into(),
                    value: SecretBytes::from(value),
                    audience: Audience::Everyone,
                    if_absent: false,
                    anchor: None,
                },
            )
            .unwrap()
            .sign(seed, *request.binding().incarnation())
            .unwrap()
        })
    };
    let a = draft(&lab.m22, b"sibling");
    let b = draft(&lab.m7, if equal { b"sibling" } else { b"different" });
    let (winner, loser) = if a.id() < b.id() { (&a, &b) } else { (&b, &a) };
    save_records(&lab.m22, loser.bytes()).unwrap();
    transport::post_records(&lab.url, loser.bytes()).unwrap();
    sync(&lab.m22, None).unwrap();
    let previous = if equal { snapshot(&lab.m22) } else { initial };
    save_records(&lab.m22, winner.bytes()).unwrap();
    transport::post_records(&lab.url, winner.bytes()).unwrap();
    sync(&lab.m22, None).unwrap();
    let current = snapshot(&lab.m22);
    let (history, selected) = register(&current);
    assert_eq!(selected.operation, *winner.id());
    assert_eq!(selected.ambiguous, !equal);
    assert_eq!(
        history
            .past_of(&[selected.operation])
            .holds(&register(&previous).1.operation),
        !equal
    );
    assert_ne!(current.semantic_operation(), previous.semantic_operation());
    lab.server.stop();
    (previous, current)
}

fn carry() -> (EntrySnapshot, EntrySnapshot) {
    let _answering = ServersAnswering::set();
    let lab =
        crate::client::log_entries_tests::two_macs(&[(Audience::Everyone, "token", b"initial")]);
    let previous = snapshot(&lab.m22);
    let peer = read_identity(&lab.m7).unwrap().unwrap().recipient();
    crate::client::remove::remove_member(&lab.m22, &join_code(&peer), false).unwrap();
    let current = snapshot(&lab.m22);
    assert_eq!(current.semantic_operation(), previous.semantic_operation());
    assert_ne!(current.wrapper_operation(), previous.wrapper_operation());
    lab.server.stop();
    (previous, current)
}

fn cross_epoch_round_trip() -> (EntrySnapshot, EntrySnapshot) {
    let _answering = ServersAnswering::set();
    let lab =
        crate::client::log_entries_tests::two_macs(&[(Audience::Everyone, "token", b"initial")]);
    let previous = snapshot(&lab.m22);
    test_put_everyone(&lab.m22, "token", b"intermediate").unwrap();
    let peer = read_identity(&lab.m7).unwrap().unwrap().recipient();
    crate::client::remove::remove_member(&lab.m22, &join_code(&peer), false).unwrap();
    test_put_everyone(&lab.m22, "token", b"initial").unwrap();
    sync(&lab.m22, None).unwrap();
    let current = snapshot(&lab.m22);
    assert_eq!(
        current.fingerprint.as_slice(),
        previous.fingerprint.as_slice()
    );
    assert_ne!(current.semantic_operation(), previous.semantic_operation());
    lab.server.stop();
    (previous, current)
}

/// Overlapping host registers can arrive independently through the log even
/// though a single document correctly refuses to author that overlap.
fn host_overlap(follow_on: bool) -> (EntrySnapshot, EntrySnapshot) {
    use crate::commons::bootstrap::Bootstrap;
    let _answering = ServersAnswering::set();
    let lab =
        crate::client::log_entries_tests::two_macs(&[(Audience::Everyone, "token", b"initial")]);
    let base = load_records(&lab.m7).unwrap().log().unwrap().clone();
    let single = Audience::Hosts(vec!["m7".into()]);
    let pair = Audience::Hosts(vec!["m22".into(), "m7".into()]);
    let branch = |paths: &Paths, audience: &Audience, value: &[u8]| {
        let bootstrap = Bootstrap::required(paths).unwrap();
        let request = bootstrap.request().unwrap();
        let actor = request.binding().actor();
        let incarnation = *request.binding().incarnation();
        bootstrap.device().with_seed(|seed| {
            let mut log = base.clone();
            let (draft, key) = log.draft_epoch(&actor, 2_000, audience.clone(), 1).unwrap();
            let record = draft.sign(seed, incarnation).unwrap();
            let epoch = *record.id();
            log.insert(record).unwrap();
            for (index, peer) in [&lab.m22, &lab.m7].into_iter().enumerate() {
                if index == 0 && audience == &single {
                    continue;
                }
                let recipient = read_identity(peer).unwrap().unwrap().recipient();
                let wrap = log
                    .draft_wrap(&actor, 2_001, epoch, &key, recipient)
                    .unwrap()
                    .sign(seed, incarnation)
                    .unwrap();
                log.insert(wrap).unwrap();
            }
            let put = log
                .draft_put(
                    &actor,
                    2_002,
                    epoch,
                    &key,
                    &Entry {
                        name: "token".into(),
                        value: SecretBytes::from(value),
                        audience: audience.clone(),
                        if_absent: false,
                        anchor: None,
                    },
                )
                .unwrap()
                .sign(seed, incarnation)
                .unwrap();
            log.insert(put).unwrap();
            (log, epoch, *key)
        })
    };
    let a = branch(&lab.m7, &single, b"one");
    let b = branch(&lab.m22, &pair, b"two");
    let mut combined = a.0.clone();
    combined.merge(&b.0).unwrap();
    let wire = crate::membership_log::encode_records(combined.records());
    save_records(&lab.m7, &wire).unwrap();
    transport::post_records(&lab.url, &wire).unwrap();
    sync(&lab.m7, None).unwrap();
    let mut previous = snapshot(&lab.m7);
    assert!(register(&previous).1.ambiguous);
    let audience = register(&previous).1.audience.clone();
    let (epoch, key) = if audience == single {
        (a.1, a.2)
    } else {
        (b.1, b.2)
    };
    let update = |value: &[u8]| {
        let log = load_records(&lab.m7).unwrap().log().unwrap().clone();
        let bootstrap = Bootstrap::required(&lab.m7).unwrap();
        let request = bootstrap.request().unwrap();
        let record = bootstrap.device().with_seed(|seed| {
            log.draft_put(
                &request.binding().actor(),
                3_000,
                epoch,
                &key,
                &Entry {
                    name: "token".into(),
                    value: SecretBytes::from(value),
                    audience: audience.clone(),
                    if_absent: false,
                    anchor: None,
                },
            )
            .unwrap()
            .sign(seed, *request.binding().incarnation())
            .unwrap()
        });
        save_records(&lab.m7, record.bytes()).unwrap();
        transport::post_records(&lab.url, record.bytes()).unwrap();
        sync(&lab.m7, None).unwrap();
        snapshot(&lab.m7)
    };
    let mut current = update(b"resolved");
    assert!(!register(&current).1.ambiguous);
    if follow_on {
        previous = current;
        current = update(b"later-resolved");
    }
    lab.server.stop();
    (previous, current)
}

fn observations(case: i64) -> (EntrySnapshot, EntrySnapshot) {
    if case == 15 {
        return cross_epoch_round_trip();
    }
    if case == 13 || case == 16 {
        return host_overlap(case == 16);
    }
    if case == 14 {
        return incomplete_audience();
    }
    if case == 0 {
        let fixture = Fixture::new();
        legacy_envelope(&fixture);
        let current = snapshot(&fixture.paths);
        return (current.clone(), current);
    }
    if case == 6 {
        return carry();
    }
    if case == 8 || case == 9 {
        return concurrent(case == 8);
    }
    let fixture = log_fixture();
    let previous = snapshot(&fixture.paths);
    match case {
        1 => {}
        2 => legacy_envelope(&fixture),
        3 => fixture.publish(|doc| {
            doc.set_entry(&Audience::Everyone, "other", b"unrelated")
                .unwrap();
        }),
        4 | 10 => fixture.publish(|doc| {
            doc.set_entry(&Audience::Everyone, "token", b"new-value")
                .unwrap();
        }),
        5 => {
            fixture.publish(|doc| {
                doc.set_entry(&Audience::Everyone, "token", b"intermediate")
                    .unwrap();
            });
            fixture.publish(|doc| {
                doc.set_entry(&Audience::Everyone, "token", b"inert-authentic-token")
                    .unwrap();
            });
        }
        7 => fixture.publish(|doc| {
            doc.unset_entry(&Audience::Everyone, "token").unwrap();
            doc.set_entry(
                &Audience::Hosts(vec!["admin".into(), "peer".into()]),
                "token",
                b"inert-authentic-token",
            )
            .unwrap();
        }),
        11 => {
            let other = log_fixture();
            return (previous, snapshot(&other.paths));
        }
        12 => {
            let mut state = load_state(&fixture.paths).unwrap();
            state.digest = None;
            state.source_identity =
                Some(source_identity(&config(&fixture.paths).unwrap().unwrap()));
            save_state(&fixture.paths, &state).unwrap();
            std::fs::remove_file(fixture.paths.cache_file()).unwrap();
        }
        _ => panic!("unknown provenance case"),
    }
    let current = snapshot(&fixture.paths);
    if case == 2 {
        assert!(
            current.log_ids.is_some(),
            "control: the fallback retains a trusted log"
        );
    }
    if case == 5 {
        assert_eq!(
            current.fingerprint.as_slice(),
            previous.fingerprint.as_slice()
        );
        assert_ne!(current.semantic_operation(), previous.semantic_operation());
    }
    if case == 10 {
        (current, previous)
    } else {
        (previous, current)
    }
}

pub(crate) fn transition(state: &ModelState, action: &str) -> ModelState {
    let mut next = state.clone();
    if action == "Case" {
        next.insert("scenario", state["scenario"] + 1);
        return next;
    }
    assert_eq!(action, "Observe");
    let (previous, current) = observations(state["scenario"]);
    next.insert("log", i64::from(current.origin() == EntryOrigin::Log));
    next.insert("supersedes", i64::from(current.supersedes(&previous)));
    next.insert("done", 1);
    next
}

#[test]
fn semantic_provenance_requires_actual_selection_and_causal_revision() {
    let model = aterm_models_db::entry_provenance_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    let mut observations = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{state:?} -> {actual:?}"
                );
                observations += usize::from(action.name == "Observe");
            }
        }
    }
    assert_eq!(observations, 17);
    for (mutant, case, invariant) in [
        (1, 2, "ActualSource"),
        (2, 6, "OnlyCausalSemanticChange"),
        (3, 8, "OnlyCausalSemanticChange"),
        (4, 14, "OnlyCausalSemanticChange"),
    ] {
        let mut state = model.init_state();
        state.insert("scenario", case);
        MUTANT.set(mutant);
        let bad = transition(&state, "Observe");
        MUTANT.set(0);
        assert!(!model.successors("Observe", &state).contains(&bad));
        assert!(!model.check_invariant(invariant, &bad));
    }
}

#[test]
fn full_and_named_selection_return_the_same_authenticated_winner() {
    let fixture = log_fixture();
    let store = load_records(&fixture.paths).unwrap();
    let log = store.log().unwrap();
    let identity = read_identity(&fixture.paths).unwrap().unwrap();
    let full = log.open_selection(&identity, None).unwrap().unwrap();
    let named = log
        .open_selection(&identity, Some(&["token"]))
        .unwrap()
        .unwrap();
    let token = full
        .opened
        .entries
        .iter()
        .find(|entry| entry.name == "token")
        .unwrap();
    assert_eq!(named.opened.entries.as_slice(), std::slice::from_ref(token));
    let key = (token.audience.clone(), token.name.clone());
    assert_eq!(named.operations[&key], full.operations[&key]);
    assert_eq!(named.puts[&key], full.puts[&key]);
    assert_eq!(named.operations.len(), 1);
}
