// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn fixture() -> (aterm_tempfile::TempDir, Paths, Prepare, Binding) {
    let (home, paths, mut prepare, binding) = super::fixture();
    prepare.rejection_domain = Some(rejection_domain_at(&paths).unwrap());
    (home, paths, prepare, binding)
}

#[test]
fn legacy_unmarked_recipes_cannot_be_rejected_as_never_admitted() {
    let (_home, paths, prepare, _) = super::fixture();
    assert!(reject_unadmitted_at(&paths, &prepare, five_records()).is_err());
    assert!(!paths.dir.exists());
}

#[test]
fn rejection_survives_reopen_and_never_becomes_admission_or_noexec() {
    let (_home, paths, prepare, binding) = fixture();
    assert!(reject_unadmitted_at(&paths, &prepare, five_records()).unwrap());
    assert!(contains_operation_at(&paths, &prepare.id).unwrap());
    let before = std::fs::read(directory(&paths).join("noexec.astream")).unwrap();
    assert!(reject_unadmitted_at(&paths, &prepare, five_records()).unwrap());
    assert_eq!(
        std::fs::read(directory(&paths).join("noexec.astream")).unwrap(),
        before
    );
    assert_eq!(observe_prepare_at(&paths, &prepare).unwrap(), None);
    assert_eq!(observe_at(&paths, &binding).unwrap(), None);
    for event in [
        Event::Prepare,
        Event::MayExecute,
        Event::NoExec,
        Event::Seal,
    ] {
        assert!(record_event_at(&paths, &prepare, &binding, Some(event)).is_err());
    }
    let mut changed = prepare.clone();
    changed.deadline_us += 1;
    assert!(!reject_unadmitted_at(&paths, &changed, five_records()).unwrap());
    assert!(record_event_at(&paths, &changed, &binding, Some(Event::Prepare)).is_err());

    // Negative implementation: forget the rejection and admit the old recipe
    // against unrelated empty custody. The model must reject that transition.
    let (_other_home, other_paths, other_prepare, other_binding) = fixture();
    record_event_at(
        &other_paths,
        &other_prepare,
        &other_binding,
        Some(Event::Prepare),
    )
    .unwrap();
    let model = aterm_models_harness::issuer_journal_model();
    let mut bad = model.init_state();
    bad.insert("a_rejected", 1);
    bad.insert(
        "a",
        issuer_number(
            read(&other_paths)
                .unwrap()
                .unwrap()
                .1
                .entries
                .values()
                .next()
                .unwrap()
                .state,
        ),
    );
    assert!(!model.check_invariant("RejectionPermanent", &bad));
}

#[test]
fn even_a_copied_identity_cannot_admit_or_reject_another_journal_domains_recipe() {
    let (_home, paths, prepare, binding) = fixture();
    let (_other_home, other_paths, _, _) = super::fixture();
    aterm_secrets::client::machine_identity(&other_paths).unwrap();
    std::fs::copy(paths.dir.join("identity"), other_paths.dir.join("identity")).unwrap();
    let other_domain = rejection_domain_at(&other_paths).unwrap();
    assert_ne!(prepare.rejection_domain.as_ref(), Some(&other_domain));
    assert!(reject_unadmitted_at(&other_paths, &prepare, five_records()).is_err());
    assert!(record_event_at(&other_paths, &prepare, &binding, Some(Event::Prepare)).is_err());
    assert_eq!(read(&other_paths).unwrap().unwrap().1.sequence, 0);
    assert!(!contains_operation_at(&other_paths, &prepare.id).unwrap());
    assert!(reject_unadmitted_at(&paths, &prepare, five_records()).unwrap());
    let mut other_prepare = prepare.clone();
    other_prepare.rejection_domain = Some(other_domain);
    record_event_at(&other_paths, &other_prepare, &binding, Some(Event::Prepare)).unwrap();
}

#[test]
fn rejection_does_not_reinterpret_an_existing_issuer_record() {
    for events in [
        vec![Event::Prepare],
        vec![Event::Prepare, Event::MayExecute],
        vec![Event::Prepare, Event::NoExec],
        vec![Event::Prepare, Event::Seal],
        vec![Event::Prepare, Event::MayExecute, Event::Seal],
    ] {
        let (_home, paths, prepare, binding) = fixture();
        for event in events {
            record_event_at(&paths, &prepare, &binding, Some(event)).unwrap();
        }
        let before = std::fs::read(directory(&paths).join("noexec.astream")).unwrap();
        assert!(!reject_unadmitted_at(&paths, &prepare, five_records()).unwrap());
        assert_eq!(
            std::fs::read(directory(&paths).join("noexec.astream")).unwrap(),
            before
        );
    }
}

#[test]
fn rejection_and_admission_are_serialized_by_the_real_journal() {
    let (_home, paths, prepare, binding) = fixture();
    aterm_secrets::client::machine_identity(&paths).unwrap();
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let worker_gate = gate.clone();
    let worker_paths = paths.clone();
    let worker_prepare = prepare.clone();
    let worker = std::thread::spawn(move || {
        worker_gate.wait();
        record_event_at(
            &worker_paths,
            &worker_prepare,
            &binding,
            Some(Event::Prepare),
        )
        .is_ok()
    });
    gate.wait();
    let rejected = reject_unadmitted_at(&paths, &prepare, five_records()).unwrap();
    let admitted = worker.join().unwrap();
    assert_ne!(rejected, admitted);
    let view = read(&paths).unwrap().unwrap().1;
    assert_eq!(view.sequence, 1);
    assert_eq!(
        view.entries.values().next().unwrap().state,
        if rejected {
            IssuerState::Rejected
        } else {
            IssuerState::Prepared
        }
    );
}

#[test]
fn rejection_preserves_reserved_closure_capacity_and_refuses_damaged_custody() {
    let (_home, paths, prepare, binding) = fixture();
    record_event_at_limits(
        &paths,
        &prepare,
        &binding,
        Some(Event::Prepare),
        five_records(),
    )
    .unwrap();
    for id in ["12", "34"] {
        let mut other = prepare.clone();
        other.id = id.repeat(16);
        assert!(reject_unadmitted_at(&paths, &other, five_records()).unwrap());
    }
    let mut overflow = prepare.clone();
    overflow.id = "56".repeat(16);
    assert!(reject_unadmitted_at(&paths, &overflow, five_records()).is_err());
    record_event_at_limits(
        &paths,
        &prepare,
        &binding,
        Some(Event::MayExecute),
        five_records(),
    )
    .unwrap();
    record_event_at_limits(
        &paths,
        &prepare,
        &binding,
        Some(Event::Seal),
        five_records(),
    )
    .unwrap();
    assert_eq!(read(&paths).unwrap().unwrap().1.sequence, 5);
    std::fs::write(directory(&paths).join("noexec.checkpoint"), [0; 40]).unwrap();
    assert!(reject_unadmitted_at(&paths, &overflow, five_records()).is_err());
}
