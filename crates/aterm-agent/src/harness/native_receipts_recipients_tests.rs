// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

fn fixture() -> (aterm_tempfile::TempDir, Paths, Prepare, Binding) {
    let (home, paths, mut prepare, binding) = super::super::tests::fixture();
    prepare.boot = Some(super::super::super::native_act::BootStamp {
        host: format!("macos:{}", "12".repeat(16)),
        boot: "34".repeat(16),
    });
    if let NativePlan::Restart {
        forward, rollback, ..
    } = &mut prepare.plan
    {
        *rollback = Some(forward.clone());
    }
    assert!(Request::prepare(prepare.clone()).encode().is_some());
    (home, paths, prepare, binding)
}

fn recipient(pid: u32) -> Recipient {
    Recipient {
        pid,
        birth: 80,
        group: pid,
        session: 400,
    }
}

fn body(
    view: &View,
    key: &Key,
    prepare: &Prepare,
    binding: &Binding,
    event: u8,
    who: Option<Recipient>,
) -> Vec<u8> {
    let receipt = receipt(key, prepare, binding).unwrap();
    let epoch = key.digest(b"epoch\0", prepare.host_epoch.as_bytes());
    let mut body = view.encode_event(key, &receipt, epoch, Event::Prepare, 91);
    body.truncate(181);
    body[..4].copy_from_slice(b"ANG5");
    body[148] = event;
    body.extend_from_slice(&boot_digest(key, prepare).unwrap());
    let who = who.unwrap_or(Recipient {
        pid: 0,
        birth: 0,
        group: 0,
        session: 0,
    });
    body.extend_from_slice(&who.pid.to_le_bytes());
    body.extend_from_slice(&who.birth.to_le_bytes());
    body.extend_from_slice(&who.group.to_le_bytes());
    body.extend_from_slice(&who.session.to_le_bytes());
    body.extend_from_slice(hmac::sign(&key.mac, &body).as_ref());
    assert_eq!(body.len(), BODY_BYTES);
    body
}

fn append(paths: &Paths, prepare: &Prepare, binding: &Binding, event: u8, who: Option<Recipient>) {
    let key = write_key(paths).unwrap();
    let (mut log, view) = open(paths, &key, LIMITS).unwrap();
    log.append(SUBJECT, body(&view, &key, prepare, binding, event, who))
        .unwrap();
}

#[test]
fn complete_history_survives_reopen_without_becoming_legacy_authority() {
    let (_home, paths, prepare, binding) = fixture();
    assert!(observe_at(&paths, &prepare).unwrap().is_none());
    assert!(!paths.dir.exists());
    append(&paths, &prepare, &binding, 1, None);
    let prepared = observe_at(&paths, &prepare).unwrap().unwrap();
    assert_eq!(prepared.state, Recorded::Prepared);
    assert_eq!((prepared.forward, prepared.rollback), (None, None));
    append(&paths, &prepare, &binding, 2, Some(recipient(701)));
    append(&paths, &prepare, &binding, 3, Some(recipient(702)));
    append(&paths, &prepare, &binding, 5, None);
    let history = observe_at(&paths, &prepare).unwrap().unwrap();
    assert_eq!(history.forward, Some(recipient(701)));
    assert_eq!(history.rollback, Some(recipient(702)));
    assert_eq!(history.state, Recorded::Sealed { may_execute: true });
    assert_eq!(history.observed.sequence, 3);
    assert_eq!(
        super::super::observe_closure_at(&paths, &prepare).unwrap(),
        Closure::Recipients(history.clone())
    );
    let before = std::fs::read(directory(&paths).join("noexec.astream")).unwrap();
    for event in [
        Event::Prepare,
        Event::MayExecute,
        Event::NoExec,
        Event::Seal,
    ] {
        assert!(record_event_at(&paths, &prepare, &binding, Some(event)).is_err());
    }
    assert_eq!(
        before,
        std::fs::read(directory(&paths).join("noexec.astream")).unwrap()
    );
    let mut other = prepare.clone();
    other.deadline_us += 1;
    assert!(observe_at(&paths, &other).unwrap().is_none());
    other = prepare.clone();
    other.boot.as_mut().unwrap().boot = "56".repeat(16);
    assert!(observe_at(&paths, &other).unwrap().is_none());
}

#[test]
fn legacy_and_unstamped_records_never_mean_an_empty_complete_history() {
    let (_home, paths, mut prepare, binding) = fixture();
    record_event_at(&paths, &prepare, &binding, Some(Event::Prepare)).unwrap();
    assert!(observe_at(&paths, &prepare).unwrap().is_none());
    prepare.boot = None;
    assert!(observe_at(&paths, &prepare).is_err());
}

#[test]
fn transitions_reject_replaced_recipients_downgrades_and_post_terminal_grants() {
    let (_home, paths, prepare, binding) = fixture();
    let key = write_key(&paths).unwrap();
    for prefix in [
        vec![1],
        vec![1, 2],
        vec![1, 3],
        vec![1, 2, 3],
        vec![1, 4],
        vec![1, 5],
        vec![1, 2, 5],
    ] {
        for next in 1..=6 {
            let mut view = View::default();
            for event in &prefix {
                let bytes = body(
                    &view,
                    &key,
                    &prepare,
                    &binding,
                    *event,
                    match event {
                        2 => Some(recipient(701)),
                        3 => Some(recipient(702)),
                        _ => None,
                    },
                );
                view.ingest(&key, &bytes).unwrap();
            }
            let expected = match prefix.as_slice() {
                [1] => matches!(next, 2..=5),
                [1, 2] => matches!(next, 3 | 5),
                [1, 3] | [1, 2, 3] => next == 5,
                _ => false,
            };
            let bytes = body(
                &view,
                &key,
                &prepare,
                &binding,
                next,
                match next {
                    2 => Some(recipient(703)),
                    3 => Some(recipient(704)),
                    _ => None,
                },
            );
            assert_eq!(
                view.ingest(&key, &bytes).is_ok(),
                expected,
                "{prefix:?}, {next}"
            );
        }
    }
    let mut view = View::default();
    view.ingest(&key, &body(&view, &key, &prepare, &binding, 1, None))
        .unwrap();
    view.ingest(
        &key,
        &body(&view, &key, &prepare, &binding, 2, Some(recipient(701))),
    )
    .unwrap();
    assert!(
        view.ingest(
            &key,
            &body(&view, &key, &prepare, &binding, 3, Some(recipient(701)))
        )
        .is_err()
    );
    let mut wrong = recipient(702);
    wrong.session += 1;
    assert!(
        view.ingest(&key, &body(&view, &key, &prepare, &binding, 3, Some(wrong)))
            .is_err()
    );
    let mut reused_pid = recipient(701);
    reused_pid.birth += 1;
    view.ingest(
        &key,
        &body(&view, &key, &prepare, &binding, 3, Some(reused_pid)),
    )
    .unwrap();
    assert_eq!(
        view.entries[&key.operation(&prepare.id)]
            .recipients
            .as_ref()
            .unwrap()
            .rollback,
        Some(reused_pid)
    );
    let old = view.encode_event(
        &key,
        &receipt(&key, &prepare, &binding).unwrap(),
        key.digest(b"epoch\0", prepare.host_epoch.as_bytes()),
        Event::Seal,
        92,
    );
    assert!(view.ingest(&key, &old).is_err());
}

#[test]
fn mixed_history_reserves_records_and_bytes_without_repricing_legacy_records() {
    let (_home, paths, prepare, binding) = fixture();
    let key = write_key(&paths).unwrap();
    let mut view = View::default();
    view.ingest(&key, &body(&view, &key, &prepare, &binding, 1, None))
        .unwrap();
    let exact = Limits {
        records: 4,
        bytes: 4 * FRAME_BYTES,
        record_bytes: BODY_BYTES,
    };
    assert!(view.within_capacity(exact, 0));
    assert!(!view.within_capacity(
        Limits {
            records: 3,
            ..exact
        },
        0
    ));
    assert!(!view.within_capacity(
        Limits {
            bytes: exact.bytes - 1,
            ..exact
        },
        0
    ));
    assert!(!view.can_append(Some(Event::Prepare), exact));
    view.ingest(
        &key,
        &body(&view, &key, &prepare, &binding, 3, Some(recipient(702))),
    )
    .unwrap();
    assert_eq!(
        view.entries.values().next().unwrap().reservation(),
        (1, FRAME_BYTES)
    );
    assert!(view.within_capacity(exact, 1));
    let mut legacy = prepare.clone();
    legacy.id = "77".repeat(16);
    let legacy_binding = Binding {
        id: legacy.id.clone(),
        capability: binding.capability.clone(),
    };
    let bytes = view.encode_event(
        &key,
        &receipt(&key, &legacy, &legacy_binding).unwrap(),
        key.digest(b"epoch\0", legacy.host_epoch.as_bytes()),
        Event::Prepare,
        92,
    );
    view.ingest(&key, &bytes).unwrap();
    let mixed = Limits {
        records: 6,
        bytes: 3 * FRAME_BYTES + 3 * ISSUER_FRAME,
        record_bytes: BODY_BYTES,
    };
    assert!(view.within_capacity(mixed, 0));
    assert!(!view.within_capacity(
        Limits {
            bytes: mixed.bytes - 1,
            ..mixed
        },
        0
    ));
}

#[test]
fn authenticated_shape_damage_and_unbound_fields_are_rejected() {
    let (_home, paths, prepare, binding) = fixture();
    let key = write_key(&paths).unwrap();
    for offset in [4, 20, 52, 84, 116, 149, 181, 213, 233] {
        let mut view = View::default();
        let mut bytes = body(&view, &key, &prepare, &binding, 1, None);
        bytes[offset] ^= 1;
        assert!(view.ingest(&key, &bytes).is_err());
    }
    for who in [
        Recipient {
            pid: 1,
            ..recipient(701)
        },
        Recipient {
            birth: 0,
            ..recipient(701)
        },
        Recipient {
            group: 702,
            ..recipient(701)
        },
        Recipient {
            session: 1,
            ..recipient(701)
        },
    ] {
        let mut view = View::default();
        view.ingest(&key, &body(&view, &key, &prepare, &binding, 1, None))
            .unwrap();
        assert!(
            view.ingest(&key, &body(&view, &key, &prepare, &binding, 2, Some(who)))
                .is_err()
        );
    }
    let mut view = View::default();
    assert!(
        view.ingest(
            &key,
            &body(&view, &key, &prepare, &binding, 1, Some(recipient(701)))
        )
        .is_err()
    );
    assert!(!record_subject(
        ISSUER_SUBJECT,
        &body(&view, &key, &prepare, &binding, 1, None)
    ));
}

#[test]
fn authenticated_records_cannot_change_identity_or_upgrade_a_legacy_operation() {
    let (_home, paths, prepare, binding) = fixture();
    let key = write_key(&paths).unwrap();
    for offset in [52, 84, 116, 149, 181] {
        let mut view = View::default();
        view.ingest(&key, &body(&view, &key, &prepare, &binding, 1, None))
            .unwrap();
        let mut changed = body(&view, &key, &prepare, &binding, 2, Some(recipient(701)));
        changed[offset] ^= 1;
        changed.truncate(233);
        changed.extend_from_slice(hmac::sign(&key.mac, &changed).as_ref());
        assert!(view.ingest(&key, &changed).is_err());
    }
    let mut view = View::default();
    let old = view.encode_event(
        &key,
        &receipt(&key, &prepare, &binding).unwrap(),
        key.digest(b"epoch\0", prepare.host_epoch.as_bytes()),
        Event::Prepare,
        91,
    );
    view.ingest(&key, &old).unwrap();
    for event in [1, 2] {
        assert!(
            view.ingest(
                &key,
                &body(
                    &view,
                    &key,
                    &prepare,
                    &binding,
                    event,
                    if event == 2 {
                        Some(recipient(701))
                    } else {
                        None
                    }
                )
            )
            .is_err()
        );
    }
    let (mut log, view) = open(&paths, &key, LIMITS).unwrap();
    let mut wrong_epoch = body(&view, &key, &prepare, &binding, 1, None);
    wrong_epoch[149] ^= 1;
    wrong_epoch.truncate(233);
    wrong_epoch.extend_from_slice(hmac::sign(&key.mac, &wrong_epoch).as_ref());
    log.append(SUBJECT, wrong_epoch).unwrap();
    drop(log);
    assert!(observe_at(&paths, &prepare).unwrap().is_none());
}

#[test]
fn new_noexec_is_not_a_legacy_receipt_or_successor_parent() {
    let (_home, paths, prepare, binding) = fixture();
    append(&paths, &prepare, &binding, 1, None);
    append(&paths, &prepare, &binding, 4, None);
    assert_eq!(
        observe_at(&paths, &prepare).unwrap().unwrap().state,
        Recorded::NoExec
    );
    assert!(
        super::super::observe_at(&paths, &binding)
            .unwrap()
            .is_none()
    );
    assert!(observe_prepare_at(&paths, &prepare).unwrap().is_none());
    assert!(record_event_at(&paths, &prepare, &binding, Some(Event::NoExec)).is_err());
    let (key, mut view) = read(&paths).unwrap().unwrap();
    let mut next = prepare.clone();
    next.id = "99".repeat(16);
    let next_binding = Binding {
        id: next.id.clone(),
        capability: binding.capability.clone(),
    };
    let bytes = view.encode_successor(
        &key,
        &receipt(&key, &next, &next_binding).unwrap(),
        key.digest(b"epoch\0", prepare.host_epoch.as_bytes()),
        key.operation(&prepare.id),
        92,
    );
    assert!(view.ingest(&key, &bytes).is_err());
    assert!(record_successor_at(&paths, &prepare, &binding, &next, &next_binding, LIMITS).is_err());
}

fn probe(view: &View, operation: Digest, other: i64, complete: i64) -> State {
    let model = aterm_models_harness::recipient_journal_model();
    let mut state = model.init_state();
    let entry = view.entries.get(&operation);
    let history = entry.and_then(|entry| entry.recipients.as_ref());
    let mode = if history.is_some() {
        2
    } else if entry.is_some() {
        1
    } else {
        0
    };
    let phase = if mode != 2 {
        0
    } else {
        match entry.unwrap().state {
            IssuerState::Prepared => 1,
            IssuerState::MayExecute if history.unwrap().rollback.is_none() => 2,
            IssuerState::MayExecute => 3,
            IssuerState::Sealed { may_execute: false } => 4,
            IssuerState::Sealed { may_execute: true } => 5,
            IssuerState::NoExec => 6,
            IssuerState::Fenced | IssuerState::Rejected => unreachable!(),
        }
    };
    for (key, value) in [
        ("mode", mode),
        ("state", phase),
        (
            "forward",
            i64::from(history.is_some_and(|h| h.forward.is_some())),
        ),
        (
            "rollback",
            i64::from(history.is_some_and(|h| h.rollback.is_some())),
        ),
        ("records", i64::try_from(view.sequence).unwrap() - other),
        (
            "reserved",
            entry.map_or(0, |e| i64::try_from(e.reservation().0).unwrap()),
        ),
        ("other", other),
        ("closed", i64::from(phase >= 4)),
        ("legacy", i64::from(mode == 1)),
        ("complete", complete),
    ] {
        state.insert(key, value);
    }
    state
}

fn transition(actions: &[&str]) {
    let (_home, paths, prepare, binding) = fixture();
    let key = write_key(&paths).unwrap();
    let model = aterm_models_harness::recipient_journal_model();
    let mut state = model.init_state();
    let (mut other, mut complete) = (0, 0);
    for action in actions {
        assert!(model.fire(action, &mut state), "{action}: {state:?}");
        match *action {
            "Read" => complete = i64::from(observe_at(&paths, &prepare).unwrap().is_some()),
            "LegacyWrite" => {
                assert!(record_event_at(&paths, &prepare, &binding, Some(Event::Seal)).is_err());
            }
            "Other" | "Legacy" => {
                let (mut log, view) = open(&paths, &key, LIMITS).unwrap();
                let mut old = prepare.clone();
                if *action == "Other" {
                    old.id = "88".repeat(16);
                    other = 1;
                }
                let old_binding = Binding {
                    id: old.id.clone(),
                    capability: binding.capability.clone(),
                };
                log.append(
                    super::super::SUBJECT,
                    view.encode(&key, &receipt(&key, &old, &old_binding).unwrap(), 91),
                )
                .unwrap();
            }
            other_action => {
                let (event, who) = match other_action {
                    "Prepare" => (1, None),
                    "Forward" => (2, Some(recipient(701))),
                    "Rollback" => (3, Some(recipient(702))),
                    "NoExec" => (4, None),
                    "Seal" => (5, None),
                    _ => unreachable!(),
                };
                append(&paths, &prepare, &binding, event, who);
            }
        }
        let view = read(&paths)
            .unwrap()
            .map(|(_, view)| view)
            .unwrap_or_default();
        assert_eq!(
            probe(&view, key.operation(&prepare.id), other, complete),
            state,
            "{action}"
        );
        assert!(view.within_capacity(
            Limits {
                records: 4,
                ..LIMITS
            },
            0
        ));
        for law in &model.invariants {
            assert!(model.check_invariant(law.name, &state), "{}", law.name);
        }
    }
    if state["state"] == 2 {
        // A reader that accepted NoExec after recording a recipient would license
        // replacement without excluding that recipient. The projection rejects it.
        let mut bad = state.clone();
        bad.insert("state", 6);
        bad.insert("closed", 1);
        bad.insert("reserved", 0);
        bad.insert("records", state["records"] + 1);
        assert!(!model.check_invariant("NoExecNeverGranted", &bad));
        assert!(!model.successors("NoExec", &state).contains(&bad));
    }
}

#[test]
fn production_replay_and_observation_refine_the_recipient_journal() {
    for actions in [
        "Read Prepare Read Forward Read Rollback Seal Read",
        "Prepare Rollback Other Seal Read",
        "Prepare NoExec Read",
        "Prepare Seal Read",
        "Other Legacy Read",
        "Prepare LegacyWrite Forward",
    ] {
        transition(&actions.split_whitespace().collect::<Vec<_>>());
    }
}
