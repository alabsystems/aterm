// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shared-namespace exclusion and missing complete-history evidence.

use super::*;

fn recipes(paths: &Paths, mut marked: Prepare) -> (Prepare, Prepare) {
    super::mode::enrolled(paths, &mut marked);
    let mut unmarked = marked.clone();
    unmarked.receipt_mode = None;
    assert!(Request::prepare(marked.clone()).encode().is_some());
    assert!(Request::prepare(unmarked.clone()).encode().is_some());
    (marked, unmarked)
}

pub(super) fn reservation_body(
    key: &Key,
    view: &View,
    marked: &Prepare,
    binding: &Binding,
    event: Event,
) -> Vec<u8> {
    view.encode_event(
        key,
        &receipt(key, marked, binding).unwrap(),
        key.digest(b"epoch\0", marked.host_epoch.as_bytes()),
        event,
        1,
    )
}

pub(super) fn append_reservation_fixture(
    paths: &Paths,
    marked: &Prepare,
    binding: &Binding,
    event: Event,
) {
    if event == Event::Prepare {
        super::super::reservation::reserve_at(paths, marked, binding, LIMITS).unwrap();
        return;
    }
    let key = write_key(paths).unwrap();
    let (mut log, mut view) = open(paths, &key, LIMITS).unwrap();
    let body = reservation_body(&key, &view, marked, binding, event);
    // Exercise the existing parser's identity and transition checks before appending.
    // Later lifecycle states remain a fixture until the protocol implements closure.
    view.ingest_bounded(&key, &body, LIMITS).unwrap();
    log.append(ISSUER_SUBJECT, body).unwrap();
}

#[test]
fn separate_journals_can_both_own_the_same_operation_without_a_shared_reservation() {
    let (_legacy_home, legacy, prepare, binding) = fixture();
    let (_complete_home, complete, _, _) = fixture();
    let (marked, unmarked) = recipes(&legacy, prepare);

    // Negative control: the old writer is otherwise able to admit this exact unmarked
    // recipe. An independently authenticated new journal cannot exclude that claim.
    record_event_at(&legacy, &unmarked, &binding, Some(Event::Prepare)).unwrap();
    recipients::append_fixture(
        &complete,
        &unmarked,
        &binding,
        testing::RecipientAppend::Prepare,
    )
    .unwrap();
    assert!(contains_operation_at(&legacy, &marked.id).unwrap());
    assert!(contains_operation_at(&complete, &marked.id).unwrap());
    assert_eq!(
        observe_closure_at(&legacy, &unmarked).unwrap(),
        Closure::Exact(Recorded::Prepared)
    );
    assert!(
        recipients::observe_at(&complete, &unmarked)
            .unwrap()
            .is_some()
    );
}

#[test]
fn legacy_quarantine_blocks_unmarked_reuse_at_every_committed_prefix() {
    for events in [
        vec![Event::Prepare],
        vec![Event::Prepare, Event::MayExecute],
        vec![Event::Prepare, Event::NoExec],
        vec![Event::Prepare, Event::Seal],
        vec![Event::Prepare, Event::MayExecute, Event::Seal],
    ] {
        let (_home, paths, prepare, binding) = fixture();
        let (marked, unmarked) = recipes(&paths, prepare);
        let mut unstamped = unmarked.clone();
        unstamped.boot = None;
        unstamped.custody = None;
        unstamped.rejection_domain = None;
        assert!(Request::prepare(unstamped.clone()).encode().is_some());
        for event in events {
            append_reservation_fixture(&paths, &marked, &binding, event);
            // Every call drops its log and reopens authenticated custody, as after
            // losing all issuer memory at this committed prefix.
            let before = std::fs::read(directory(&paths).join("noexec.astream")).unwrap();
            for recipe in [&unmarked, &unstamped] {
                for capability in [binding.capability.clone(), "aa".repeat(32)] {
                    let rival = Binding {
                        id: binding.id.clone(),
                        capability,
                    };
                    for attempted in [
                        None,
                        Some(Event::Prepare),
                        Some(Event::MayExecute),
                        Some(Event::NoExec),
                        Some(Event::Seal),
                    ] {
                        assert!(record_event_at(&paths, recipe, &rival, attempted).is_err());
                    }
                }
            }
            assert!(!reject_unadmitted_at(&paths, &unmarked, LIMITS).unwrap());
            assert!(contains_operation_at(&paths, &marked.id).unwrap());
            assert_eq!(observe_closure_at(&paths, &marked).unwrap(), Closure::Other);
            assert_eq!(
                observe_closure_at(&paths, &unmarked).unwrap(),
                Closure::Other
            );
            assert_eq!(
                std::fs::read(directory(&paths).join("noexec.astream")).unwrap(),
                before
            );
        }
    }
}

#[test]
fn legacy_quarantine_cannot_be_inserted_after_another_recipe_owns_the_id() {
    let (_home, paths, prepare, binding) = fixture();
    let (marked, unmarked) = recipes(&paths, prepare);
    record_event_at(&paths, &unmarked, &binding, Some(Event::Prepare)).unwrap();
    assert!(super::super::reservation::reserve_at(&paths, &marked, &binding, LIMITS).is_err());
    let key = existing_key(&paths).unwrap();
    let (_log, mut view) = open_existing(&paths, &key, LIMITS).unwrap();
    let body = reservation_body(&key, &view, &marked, &binding, Event::Prepare);
    assert!(view.ingest_bounded(&key, &body, LIMITS).is_err());
    assert_eq!(
        observe_closure_at(&paths, &unmarked).unwrap(),
        Closure::Exact(Recorded::Prepared)
    );
}

#[test]
fn a_legacy_reservation_does_not_replace_missing_complete_history() {
    let (_home, paths, prepare, binding) = fixture();
    let (marked, _) = recipes(&paths, prepare);
    append_reservation_fixture(&paths, &marked, &binding, Event::Prepare);
    recipients::append_fixture(&paths, &marked, &binding, testing::RecipientAppend::Prepare)
        .unwrap();
    recipients::append_fixture(
        &paths,
        &marked,
        &binding,
        testing::RecipientAppend::Forward(Recipient {
            pid: 701,
            birth: 80,
            group: 701,
            session: 400,
        }),
    )
    .unwrap();
    assert!(
        recipients::observe_at(&paths, &marked)
            .unwrap()
            .unwrap()
            .forward
            .is_some()
    );

    let missing = paths.dir.join("native-recipients/recipients.astream");
    std::fs::remove_file(&missing).unwrap();
    assert!(recipients::observe_at(&paths, &marked).is_err());
    assert!(observe_closure_at(&paths, &marked).is_err());
    assert!(!missing.exists());
    assert!(contains_operation_at(&paths, &marked.id).unwrap());
}
