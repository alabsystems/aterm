// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::cell::Cell;
use std::rc::Rc;

struct World {
    _home: aterm_tempfile::TempDir,
    paths: Paths,
    prepare: Prepare,
    binding: Binding,
    original: Observed,
}

impl World {
    fn new() -> Self {
        let world = Self::unbound();
        binding::bind_at(&world.paths, &world.prepare, &world.binding).unwrap();
        world
    }

    fn unbound() -> Self {
        let (home, paths, mut prepare, binding) = crate::harness::native_receipts::tests::fixture();
        prepare.receipt_mode = Some(ReceiptMode::Recipients5);
        prepare.boot = Some(crate::harness::native_act::BootStamp {
            host: format!("macos:{}", "12".repeat(16)),
            boot: "34".repeat(16),
        });
        prepare.custody = Some(enroll_at(&paths).unwrap().stamp().into());
        prepare.rejection_domain = Some(rejection_domain_at(&paths).unwrap());
        let reserved = reservation::reserve_at(&paths, &prepare, &binding, LIMITS).unwrap();
        Self {
            _home: home,
            paths,
            prepare,
            binding,
            original: Observed {
                sequence: reserved.sequence(),
                recorded_ms: reserved.recorded_ms(),
            },
        }
    }

    fn writer(&self) -> writer::RecipientWriter {
        writer::open_at(&self.paths, &self.prepare, &self.binding).unwrap()
    }

    fn close(&self) -> io::Result<Observed> {
        close_at(&self.paths, &self.prepare, &self.binding)
    }

    // Construct crash prefixes through the actual legacy codec and durable log.
    fn legacy(&self, event: Event) {
        let key = existing_key(&self.paths).unwrap();
        let (mut log, view) = open_existing(&self.paths, &key, LIMITS).unwrap();
        let body = view.encode_event(
            &key,
            &receipt(&key, &self.prepare, &self.binding).unwrap(),
            key.digest(b"epoch\0", self.prepare.host_epoch.as_bytes()),
            event,
            123,
        );
        log.append(ISSUER_SUBJECT, body).unwrap();
    }

    fn state(&self) -> (IssuerState, u64, usize) {
        let (key, view) = read(&self.paths).unwrap().unwrap();
        let entry = &view.entries[&key.operation(&self.prepare.id)];
        (entry.state, view.sequence, view.reserved_records)
    }

    fn bind_history_to(&self, origin: Observed, events: &[recipients::Append]) {
        let legacy_key = existing_key(&self.paths).unwrap();
        let (_legacy, legacy_view) = open_existing(&self.paths, &legacy_key, LIMITS).unwrap();
        let custody = legacy_view.custody(&legacy_key).unwrap();
        let mut opened = super::super::open_at(&self.paths, false).unwrap();
        assert_eq!(opened.view.sequence, 0);
        let expected = receipt(&opened.key, &self.prepare, &self.binding).unwrap();
        // Bind the supplied origin, including authenticated false origins a
        // faulty earlier writer could have persisted. The remaining history
        // uses the real codec and journal so closure must judge that origin.
        let mut body = b"ARB1".to_vec();
        body.extend_from_slice(&0_u64.to_le_bytes());
        body.extend_from_slice(&123_u64.to_le_bytes());
        body.extend_from_slice(&[0; 32]);
        body.extend_from_slice(&expected.operation);
        body.extend_from_slice(&expected.prepare);
        body.extend_from_slice(&expected.capability);
        body.extend_from_slice(&opened.key.digest(b"legacy-custody\0", custody.as_bytes()));
        body.extend_from_slice(&origin.sequence.to_le_bytes());
        body.extend_from_slice(&origin.recorded_ms.to_le_bytes());
        body.extend_from_slice(hmac::sign(&opened.key.mac, &body).as_ref());
        opened
            .view
            .ingest_record(&opened.key, "native/recipient-binding/1", &body)
            .unwrap();
        opened
            .log
            .append("native/recipient-binding/1", body)
            .unwrap();
        for event in std::iter::once(recipients::Append::Prepare).chain(events.iter().copied()) {
            let body = recipients::encode_event(
                &opened.key,
                opened.view.sequence,
                opened.view.head,
                &self.prepare,
                &self.binding,
                event,
                124,
            )
            .unwrap();
            opened
                .view
                .ingest_record(&opened.key, recipients::SUBJECT, &body)
                .unwrap();
            opened.log.append(recipients::SUBJECT, body).unwrap();
        }
    }
}

#[test]
fn terminal_history_cannot_hide_a_binding_to_the_wrong_original_position() {
    for wrong_time in [false, true] {
        let world = World::unbound();
        world.bind_history_to(
            Observed {
                sequence: world.original.sequence + u64::from(!wrong_time),
                recorded_ms: world.original.recorded_ms + u64::from(wrong_time),
            },
            &[recipients::Append::NoExec],
        );
        assert!(world.close().is_err());
        assert_eq!(world.state(), (IssuerState::Prepared, 1, 2));
    }
}

#[test]
fn an_existing_authenticated_record_is_not_an_original_reservation() {
    for event in [
        Event::Prepare,
        Event::MayExecute,
        Event::NoExec,
        Event::Seal,
    ] {
        for exact in [false, true] {
            let world = World::unbound();
            let (wrong, events) = if event == Event::Prepare {
                let mut other = world.prepare.clone();
                other.id = "01".repeat(16);
                let binding = Binding {
                    id: other.id.clone(),
                    capability: "cd".repeat(32),
                };
                let reserved =
                    reservation::reserve_at(&world.paths, &other, &binding, LIMITS).unwrap();
                (
                    Observed {
                        sequence: reserved.sequence(),
                        recorded_ms: reserved.recorded_ms(),
                    },
                    vec![recipients::Append::NoExec],
                )
            } else {
                world.legacy(event);
                let (key, view) = read(&world.paths).unwrap().unwrap();
                let wrong = view.entries[&key.operation(&world.prepare.id)].observed;
                let events = match event {
                    Event::MayExecute => vec![
                        recipients::Append::Forward(recipient()),
                        recipients::Append::Seal,
                    ],
                    Event::NoExec => vec![recipients::Append::NoExec],
                    Event::Seal => vec![recipients::Append::Seal],
                    _ => unreachable!(),
                };
                (wrong, events)
            };
            {
                // Negative control: a reader checking only existence, sequence and
                // timestamp accepts this real record. Its authenticated identity or
                // event must still disqualify it as this operation's original Prepare.
                let key = existing_key(&world.paths).unwrap();
                let (log, _) = open_existing(&world.paths, &key, LIMITS).unwrap();
                let records = log
                    .read_range(astream_wire::Offset(wrong.sequence), 1)
                    .unwrap();
                let record = &records[0];
                assert_eq!(record.seq.0, wrong.sequence);
                assert_eq!(record.subject, ISSUER_SUBJECT);
                assert_eq!(&record.body[..4], b"ANI2");
                assert_eq!(&record.body[12..20], &wrong.recorded_ms.to_le_bytes());
            }
            world.bind_history_to(if exact { world.original } else { wrong }, &events);
            let before = world.state();
            let head = read(&world.paths).unwrap().unwrap().1.head;
            let complete_head = || {
                let opened = super::super::open_at(&world.paths, false).unwrap();
                (opened.view.sequence, opened.view.head)
            };
            let complete_before = complete_head();
            let history = super::super::observe_at(&world.paths, &world.prepare)
                .unwrap()
                .unwrap();
            let result = world.close();
            assert_eq!(result.is_ok(), exact, "event={event:?}, exact={exact}");
            if exact {
                assert_eq!(world.close().unwrap(), result.unwrap());
            } else {
                assert_eq!(world.state(), before);
                assert_eq!(read(&world.paths).unwrap().unwrap().1.head, head);
            }
            assert_eq!(
                super::super::observe_at(&world.paths, &world.prepare)
                    .unwrap()
                    .unwrap(),
                history
            );
            assert_eq!(complete_head(), complete_before);
        }
    }
}

fn recipient() -> Recipient {
    Recipient {
        pid: 701,
        birth: 80,
        group: 701,
        session: 400,
    }
}

#[test]
fn terminal_histories_close_exactly_and_retry_without_releasing_the_id() {
    for mode in 0..3 {
        for partial in 0..=2 {
            if mode != 2 && partial == 1 {
                continue;
            }
            let world = World::new();
            let mut writer = world.writer();
            if mode == 2 {
                writer
                    .record_then(ImagePhase::Forward, recipient(), |_| ())
                    .unwrap();
            }
            if mode == 0 {
                writer.noexec().unwrap();
            } else {
                writer.seal().unwrap();
            }
            drop(writer);
            let target = match mode {
                0 => IssuerState::NoExec,
                1 => IssuerState::Sealed { may_execute: false },
                _ => IssuerState::Sealed { may_execute: true },
            };
            if mode == 2 && partial > 0 {
                world.legacy(Event::MayExecute);
            }
            if partial == 2 {
                world.legacy(if mode == 0 {
                    Event::NoExec
                } else {
                    Event::Seal
                });
            }
            let observed = world.close().unwrap();
            assert_eq!(world.close().unwrap(), observed);
            let (state, sequence, reserved) = world.state();
            assert_eq!((state, reserved), (target, 0));
            assert_eq!(observed.sequence + 1, sequence);
            assert!(
                reservation::reserve_at(&world.paths, &world.prepare, &world.binding, LIMITS)
                    .is_err()
            );
            assert!(writer::open_at(&world.paths, &world.prepare, &world.binding).is_err());
            // Closure preserves the independently readable full recipient history.
            let history = super::super::observe_at(&world.paths, &world.prepare)
                .unwrap()
                .unwrap();
            assert_eq!(history.forward.is_some(), mode == 2);
        }
    }
}

#[test]
fn nonterminal_and_incompatible_histories_cannot_create_noexec() {
    for phase in 0..3 {
        let world = World::new();
        if phase > 0 {
            let mut writer = world.writer();
            if phase == 2 {
                writer
                    .record_then(ImagePhase::Forward, recipient(), |_| ())
                    .unwrap();
            }
        }
        assert!(world.close().is_err());
        assert_eq!(world.state(), (IssuerState::Prepared, 1, 2));
    }
    for mode in 0..3 {
        let world = World::new();
        let mut writer = world.writer();
        if mode == 2 {
            writer
                .record_then(ImagePhase::Forward, recipient(), |_| ())
                .unwrap();
        }
        if mode == 0 {
            writer.noexec().unwrap();
        } else {
            writer.seal().unwrap();
        }
        drop(writer);
        world.legacy(if mode == 2 {
            Event::NoExec
        } else {
            Event::MayExecute
        });
        let before = world.state();
        assert!(world.close().is_err());
        assert_eq!(world.state(), before);
    }
}

#[test]
fn exact_binding_and_both_custodians_are_required_even_on_terminal_retry() {
    let world = World::new();
    world.writer().noexec().unwrap();
    let mut wrong = world.prepare.clone();
    wrong.host_epoch.push('x');
    assert!(close_at(&world.paths, &wrong, &world.binding).is_err());
    assert_eq!(world.state(), (IssuerState::Prepared, 1, 2));
    world.close().unwrap();
    std::fs::remove_file(world.paths.dir.join(ENROLLMENT)).unwrap();
    assert!(world.close().is_err());
    assert_eq!(world.state().0, IssuerState::NoExec);
}

#[test]
fn custody_loss_between_closing_records_withholds_ack_and_preserves_execution() {
    let world = World::new();
    let mut writer = world.writer();
    writer
        .record_then(ImagePhase::Forward, recipient(), |_| ())
        .unwrap();
    writer.seal().unwrap();
    drop(writer);
    let path = world.paths.dir.join(ENROLLMENT);
    let calls = Rc::new(Cell::new(0));
    let seen = calls.clone();
    let hook = aterm_journal::seam::before_each_commit(move || {
        seen.set(seen.get() + 1);
        if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
    });
    assert!(world.close().is_err());
    drop(hook);
    assert!(calls.get() > 0);
    assert_eq!(world.state(), (IssuerState::MayExecute, 2, 1));
}

fn seal_as(world: &World, complete: i64) {
    let mut writer = world.writer();
    match complete {
        1 => {
            writer.noexec().unwrap();
        }
        2 => {
            writer.seal().unwrap();
        }
        3 => {
            writer
                .record_then(ImagePhase::Forward, recipient(), |_| ())
                .unwrap();
            writer.seal().unwrap();
        }
        _ => {}
    }
}

fn transition_recorded(
    state: &aterm_spec::interp::State,
    action: &str,
    evidence: Option<&mut aterm_spec::xref::StepEvidence>,
) -> aterm_spec::interp::State {
    let world = World::new();
    seal_as(&world, state["complete"]);
    match state["legacy"] {
        1 => world.legacy(Event::MayExecute),
        2 => world.legacy(Event::NoExec),
        3 => world.legacy(Event::Seal),
        4 => {
            world.legacy(Event::MayExecute);
            world.legacy(Event::Seal);
        }
        _ => {}
    }
    if state["custody"] == 0 {
        std::fs::remove_file(world.paths.dir.join(ENROLLMENT)).unwrap();
    }
    let mut next = state.clone();
    next.insert("ack", 0);
    next.insert("ready", 0);
    match action {
        "NoExec" | "EmptySeal" | "ExecutedSeal" => {
            let complete = match action {
                "NoExec" => 1,
                "EmptySeal" => 2,
                _ => 3,
            };
            seal_as(&world, complete);
            next.insert("complete", complete);
        }
        "Prefix" => world.legacy(Event::MayExecute),
        "LoseCustody" => {
            std::fs::remove_file(world.paths.dir.join(ENROLLMENT)).unwrap();
            next.insert("custody", 0);
        }
        "WrongBinding" => {
            next.insert("exact", 0);
        }
        "Close" => {
            let mut prepare = world.prepare.clone();
            if state["exact"] == 0 {
                prepare.host_epoch.push('x');
            }
            let run = || close_at(&world.paths, &prepare, &world.binding);
            let result = if let Some(evidence) = evidence {
                evidence.step("Close", run)
            } else {
                run()
            };
            next.insert("ack", i64::from(result.is_ok()));
            next.insert(
                "ready",
                i64::from(state["complete"] > 0 && state["custody"] == 1 && state["exact"] == 1),
            );
        }
        _ => unreachable!(),
    }
    next.insert(
        "legacy",
        match world.state().0 {
            IssuerState::Prepared => 0,
            IssuerState::MayExecute => 1,
            IssuerState::NoExec => 2,
            IssuerState::Sealed { may_execute: false } => 3,
            IssuerState::Sealed { may_execute: true } => 4,
            _ => panic!("unexpected legacy state"),
        },
    );
    next
}

pub(crate) fn transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    transition_recorded(state, action, None)
}

#[test]
fn real_closure_refines_all_reachable_crash_prefixes() {
    let model = aterm_models_harness::reservation_closure_model();
    assert!(
        model
            .successors("Close", &model.init_state())
            .contains(&transition(&model.init_state(), "Close"))
    );
    let mut evidence = aterm_spec::xref::StepEvidence::new("ReservationClosure");
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
    assert!(evidence.audit().is_complete());
    let mut false_noexec = model.init_state();
    false_noexec.insert("complete", 3);
    false_noexec.insert("legacy", 2);
    assert!(!model.check_invariant("NoFalseNoExec", &false_noexec));
}
