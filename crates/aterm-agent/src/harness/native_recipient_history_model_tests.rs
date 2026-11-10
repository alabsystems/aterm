// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::native_act::BootStamp;
use crate::harness::native_receipts::testing::RecipientAppend;
use aterm_spec::interp::State;

fn fixture() -> (aterm_tempfile::TempDir, Key, Prepare, Binding) {
    let (home, paths, mut prepare, binding) = crate::harness::native_receipts::tests::fixture();
    let key = write_key(&paths).unwrap();
    prepare.receipt_mode = Some(ReceiptMode::Recipients5);
    prepare.boot = Some(BootStamp {
        host: format!("macos:{}", "12".repeat(16)),
        boot: "34".repeat(16),
    });
    prepare.custody = Some("56".repeat(32));
    prepare.rejection_domain = Some("78".repeat(32));
    (home, key, prepare, binding)
}

fn bind(view: &mut View, key: &Key, prepare: &Prepare, binding: &Binding) -> io::Result<()> {
    let entry = Entry {
        receipt: receipt(key, prepare, binding)?,
        legacy_custody: [3; 32],
        legacy_sequence: 0,
        legacy_time: 1,
        observed: Observed {
            sequence: view.sequence,
            recorded_ms: 1,
        },
    };
    view.ingest_record(key, SUBJECT, &view.encode(key, &entry))
}

fn record(
    view: &mut View,
    key: &Key,
    prepare: &Prepare,
    binding: &Binding,
    event: RecipientAppend,
) -> io::Result<()> {
    let body =
        recipients::encode_fixture(key, view.sequence, view.head, prepare, binding, event, 2)?;
    view.ingest_record(key, recipients::SUBJECT, &body)
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    transition_recorded(state, action, None)
}

fn transition_recorded(
    state: &State,
    action: &str,
    evidence: Option<&mut aterm_spec::xref::StepEvidence>,
) -> State {
    let (_home, key, mut prepare, binding) = fixture();
    let mut view = View::default();
    if state["bound"] == 1 {
        bind(&mut view, &key, &prepare, &binding).unwrap();
    }
    if state["phase"] > 0 {
        record(
            &mut view,
            &key,
            &prepare,
            &binding,
            RecipientAppend::Prepare,
        )
        .unwrap();
    }
    if state["phase"] == 2 {
        record(&mut view, &key, &prepare, &binding, RecipientAppend::NoExec).unwrap();
    }
    let mut next = state.clone();
    next.insert("ack", 0);
    next.insert("ready", 0);
    match action {
        "Bind" => {
            bind(&mut view, &key, &prepare, &binding).unwrap();
        }
        "WrongBinding" => {
            next.insert("matches", 0);
            next.insert("seen", 0);
        }
        "Record" => {
            if state["matches"] == 0 {
                prepare.deadline_us += 1;
            }
            let event = if state["phase"] == 0 {
                RecipientAppend::Prepare
            } else {
                RecipientAppend::NoExec
            };
            let mut run = || record(&mut view, &key, &prepare, &binding, event);
            let result = if let Some(evidence) = evidence {
                evidence.step("Record", run)
            } else {
                run()
            };
            next.insert("ack", i64::from(result.is_ok()));
            next.insert(
                "ready",
                i64::from(state["bound"] == 1 && state["matches"] == 1 && state["phase"] < 2),
            );
        }
        "Read" => {
            if state["matches"] == 0 {
                prepare.deadline_us += 1;
            }
            let run = || view.observe(&key, &prepare).unwrap().is_some();
            let seen = if let Some(evidence) = evidence {
                evidence.step("Read", run)
            } else {
                run()
            };
            next.insert("seen", i64::from(seen));
        }
        _ => unreachable!(),
    }
    let operation = key.operation(&prepare.id);
    next.insert("bound", i64::from(view.entries.contains_key(&operation)));
    next.insert(
        "phase",
        view.histories.get(&operation).map_or(0, |entry| {
            if entry.state == IssuerState::NoExec {
                2
            } else {
                1
            }
        }),
    );
    next.insert("records", i64::try_from(view.sequence).unwrap());
    next.insert("reserved", i64::try_from(view.reserved).unwrap());
    next
}

#[test]
fn bound_dispatch_and_observation_refine_the_model() {
    let model = aterm_models_harness::bound_recipient_journal_model();
    assert!(
        model
            .successors("Record", &model.init_state())
            .contains(&transition(&model.init_state(), "Record"))
    );
    let mut evidence = aterm_spec::xref::StepEvidence::new("BoundRecipientJournal");
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                assert!(
                    model
                        .successors(action.name, &state)
                        .contains(&transition_recorded(
                            &state,
                            action.name,
                            Some(&mut evidence)
                        )),
                    "{}: {state:?}",
                    action.name
                );
            }
        }
    }
    let audit = evidence.audit();
    assert!(audit.is_complete(), "{audit:?}");
    // Negative control: removing the binding dispatcher lets the authentic event
    // decoder accept Prepare. The model must reject that actual unbound result.
    let (_home, key, prepare, binding) = fixture();
    let body = recipients::encode_fixture(
        &key,
        0,
        [0; 32],
        &prepare,
        &binding,
        RecipientAppend::Prepare,
        2,
    )
    .unwrap();
    assert!(recipients::decode_entry(&key, &body, 0, [0; 32], &BTreeMap::new()).is_ok());
    let mut bad = model.init_state();
    bad.insert("phase", 1);
    assert!(!model.check_invariant("BindingFirst", &bad));
}

#[test]
fn bound_history_consumes_reserved_slots_and_releases_unused_closure_space() {
    let (_home, key, mut prepare, mut binding) = fixture();
    let mut view = View::default();
    let maximum =
        (LIMITS.records / 5).min(LIMITS.bytes / (FRAME_BYTES + 4 * recipients::FRAME_BYTES));
    let first = prepare.clone();
    let first_binding = binding.clone();
    for number in 0..maximum {
        if number > 0 {
            prepare.id = format!("{number:032x}");
            binding.id = prepare.id.clone();
        }
        bind(&mut view, &key, &prepare, &binding).unwrap();
    }
    prepare.id = format!("{maximum:032x}");
    binding.id = prepare.id.clone();
    assert!(bind(&mut view, &key, &prepare, &binding).is_err());
    record(
        &mut view,
        &key,
        &first,
        &first_binding,
        RecipientAppend::Prepare,
    )
    .unwrap();
    assert_eq!(view.reserved, maximum * 4 - 1);
    assert!(bind(&mut view, &key, &prepare, &binding).is_err());
    record(
        &mut view,
        &key,
        &first,
        &first_binding,
        RecipientAppend::NoExec,
    )
    .unwrap();
    assert_eq!(view.reserved, maximum * 4 - 4);
    bind(&mut view, &key, &prepare, &binding).unwrap();
    assert!(capacity(view.reserved, view.bytes, view.sequence, LIMITS));
    prepare.id = format!("{:032x}", maximum + 1);
    binding.id = prepare.id.clone();
    assert!(bind(&mut view, &key, &prepare, &binding).is_err());
}

#[test]
fn authenticated_event_needs_exact_binding_subject_and_shared_chain() {
    let (_home, key, prepare, binding) = fixture();
    let mut view = View::default();
    let unbound = recipients::encode_fixture(
        &key,
        0,
        view.head,
        &prepare,
        &binding,
        RecipientAppend::Prepare,
        2,
    )
    .unwrap();
    assert!(
        view.ingest_record(&key, recipients::SUBJECT, &unbound)
            .is_err()
    );
    bind(&mut view, &key, &prepare, &binding).unwrap();
    assert!(
        view.ingest_record(&key, recipients::SUBJECT, &unbound)
            .is_err()
    );
    for field in ["recipe", "capability", "operation"] {
        let mut changed = prepare.clone();
        let mut capability = binding.clone();
        match field {
            "recipe" => changed.deadline_us += 1,
            "capability" => capability.capability = "ab".repeat(32),
            _ => {
                changed.id = "cd".repeat(16);
                capability.id = changed.id.clone();
            }
        }
        assert!(
            record(
                &mut view,
                &key,
                &changed,
                &capability,
                RecipientAppend::Prepare
            )
            .is_err()
        );
    }
    let body = recipients::encode_fixture(
        &key,
        view.sequence,
        view.head,
        &prepare,
        &binding,
        RecipientAppend::Prepare,
        2,
    )
    .unwrap();
    assert!(view.ingest_record(&key, SUBJECT, &body).is_err());
    assert!(view.ingest_record(&key, ISSUER_SUBJECT, &body).is_err());
    assert_eq!(view.sequence, 1);
    view.ingest_record(&key, recipients::SUBJECT, &body)
        .unwrap();
    assert_eq!(view.sequence, 2);
    assert!(
        view.ingest_record(&key, recipients::SUBJECT, &body)
            .is_err()
    );
    assert_eq!(view.sequence, 2);
}
