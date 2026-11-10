// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::native_act::BootStamp;
use aterm_spec::interp::State;

struct World {
    _home: aterm_tempfile::TempDir,
    paths: Paths,
    marked: Prepare,
    plain: Prepare,
    binding: Binding,
    canonical_marked: Prepare,
    canonical_plain: Prepare,
}

impl World {
    fn new(state: &State) -> Self {
        let (home, paths, mut marked, binding) = super::super::tests::fixture();
        marked.receipt_mode = Some(ReceiptMode::Recipients5);
        marked.boot = Some(BootStamp {
            host: format!("macos:{}", "12".repeat(16)),
            boot: "34".repeat(16),
        });
        marked.custody = Some("56".repeat(32));
        marked.rejection_domain = Some(rejection_domain_at(&paths).unwrap());
        let mut plain = marked.clone();
        plain.receipt_mode = None;
        if state["owner"] == 2 {
            reserve_at(&paths, &marked, &binding, LIMITS).unwrap();
        } else if state["owner"] == 1 {
            record_event_at(&paths, &plain, &binding, Some(Event::Prepare)).unwrap();
            if state["closed"] == 1 {
                record_event_at(&paths, &plain, &binding, Some(Event::NoExec)).unwrap();
            }
        }
        let canonical_marked = marked.clone();
        let canonical_plain = plain.clone();
        if state["domain"] == 0 {
            marked.rejection_domain = Some("ff".repeat(32));
            plain.rejection_domain = marked.rejection_domain.clone();
        }
        if state["available"] == 0 {
            std::fs::remove_file(directory(&paths).join("noexec.astream")).unwrap();
        }
        Self {
            _home: home,
            paths,
            marked,
            plain,
            binding,
            canonical_marked,
            canonical_plain,
        }
    }

    fn project(&self, next: &mut State) {
        if next["available"] == 0 {
            assert!(read(&self.paths).is_err());
            return;
        }
        let (key, view) = read(&self.paths).unwrap().unwrap();
        let Some(entry) = view.entries.get(&key.operation(&self.marked.id)) else {
            next.insert("owner", 0);
            return;
        };
        let owner = if entry.receipt.prepare == key.prepare(&self.canonical_marked).unwrap() {
            2
        } else {
            assert_eq!(
                entry.receipt.prepare,
                key.prepare(&self.canonical_plain).unwrap()
            );
            1
        };
        next.insert("owner", owner);
        next.insert("closed", i64::from(entry.state == IssuerState::NoExec));
    }
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    let world = World::new(state);
    let mut next = state.clone();
    let limits = if state["room"] == 0 {
        Limits {
            records: 0,
            bytes: 0,
            ..LIMITS
        }
    } else {
        LIMITS
    };
    match action {
        "Reserve" => {
            let mut evidence = aterm_spec::xref::StepEvidence::new("RecipientReservation");
            let result = evidence.step("Reserve", || {
                reserve_at(&world.paths, &world.marked, &world.binding, limits)
            });
            assert!(evidence.audit().is_complete());
            next.insert("reserved_ack", i64::from(result.is_ok()));
            next.insert("legacy_ack", 0);
            if result.is_ok() {
                next.insert("reserved_ever", 1);
            }
        }
        "Legacy" => {
            let result = record_event_at_limits(
                &world.paths,
                &world.plain,
                &world.binding,
                Some(Event::Prepare),
                limits,
            );
            next.insert("legacy_ack", i64::from(result.is_ok()));
            next.insert("reserved_ack", 0);
            if result.is_ok() {
                next.insert("legacy_ever", 1);
            }
        }
        "Close" => {
            record_event_at(
                &world.paths,
                &world.plain,
                &world.binding,
                Some(Event::NoExec),
            )
            .unwrap();
            next.insert("reserved_ack", 0);
            next.insert("legacy_ack", 0);
        }
        "Crash" => {
            next.insert("reserved_ack", 0);
            next.insert("legacy_ack", 0);
        }
        "Lose" => {
            std::fs::remove_file(directory(&world.paths).join("noexec.astream")).unwrap();
            next.insert("available", 0);
            next.insert("reserved_ack", 0);
            next.insert("legacy_ack", 0);
        }
        "WrongDomain" => {
            next.insert("domain", 0);
            next.insert("reserved_ack", 0);
            next.insert("legacy_ack", 0);
        }
        "Tight" => {
            next.insert("room", 0);
        }
        _ => unreachable!(),
    }
    world.project(&mut next);
    next
}

#[test]
fn reservation_refines_shared_ownership_crash_and_custody_model() {
    let model = aterm_models_harness::recipient_reservation_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                assert!(
                    model
                        .successors(action.name, &state)
                        .contains(&transition(&state, action.name)),
                    "{}: {state:?}",
                    action.name
                );
            }
        }
    }
    let mut missing = model.init_state();
    assert!(model.fire("Reserve", &mut missing));
    assert!(model.fire("Lose", &mut missing));
    // Negative control: silently replace missing custody with a fresh namespace.
    let replacement = World::new(&model.init_state());
    missing.insert(
        "reserved_ack",
        i64::from(
            reserve_at(
                &replacement.paths,
                &replacement.marked,
                &replacement.binding,
                LIMITS,
            )
            .is_ok(),
        ),
    );
    assert!(!model.check_invariant("Custody", &missing));
}

#[test]
fn retries_preserve_the_first_acknowledgement_and_all_legacy_writers_refuse() {
    let model = aterm_models_harness::recipient_reservation_model();
    let world = World::new(&model.init_state());
    let first = reserve_at(&world.paths, &world.marked, &world.binding, LIMITS).unwrap();
    let before = std::fs::read(directory(&world.paths).join("noexec.astream")).unwrap();
    assert_eq!(
        reserve_at(&world.paths, &world.marked, &world.binding, LIMITS).unwrap(),
        first
    );
    for event in [
        None,
        Some(Event::Prepare),
        Some(Event::MayExecute),
        Some(Event::NoExec),
        Some(Event::Seal),
    ] {
        assert!(record_event_at(&world.paths, &world.marked, &world.binding, event).is_err());
        assert!(record_event_at(&world.paths, &world.plain, &world.binding, event).is_err());
    }
    let mut other = world.marked.clone();
    other.deadline_us += 1;
    assert!(reserve_at(&world.paths, &other, &world.binding, LIMITS).is_err());
    let mut wrong = world.binding.clone();
    wrong.capability = "ff".repeat(32);
    assert!(reserve_at(&world.paths, &world.marked, &wrong, LIMITS).is_err());
    assert_eq!(
        std::fs::read(directory(&world.paths).join("noexec.astream")).unwrap(),
        before
    );
    assert!(observe_closure_at(&world.paths, &world.marked).is_err());
    assert!(!world.paths.dir.join("native-recipients").exists());
    let key = existing_key(&world.paths).unwrap();
    let (log, _) = open_existing(&world.paths, &key, LIMITS).unwrap();
    let records = log.read_range(astream_wire::Offset(0), 2).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].subject, ISSUER_SUBJECT);
    assert_eq!(&records[0].body[..4], b"ANI2");
    assert_eq!(records[0].seq.0, first.sequence());
}

#[test]
fn only_marked_canonical_recipes_can_touch_existing_custody() {
    let (_home, paths, plain, binding) = super::super::tests::fixture();
    assert!(reserve_at(&paths, &plain, &binding, LIMITS).is_err());
    assert!(!paths.dir.exists());
    let mut invalid = plain;
    invalid.receipt_mode = Some(ReceiptMode::Recipients5);
    assert!(reserve_at(&paths, &invalid, &binding, LIMITS).is_err());
    assert!(!paths.dir.exists());
}

#[test]
fn a_closed_or_possibly_executed_reservation_is_never_reopened() {
    let model = aterm_models_harness::recipient_reservation_model();
    for event in [Event::MayExecute, Event::NoExec, Event::Seal] {
        let world = World::new(&model.init_state());
        reserve_at(&world.paths, &world.marked, &world.binding, LIMITS).unwrap();
        {
            let key = existing_key(&world.paths).unwrap();
            let (mut log, view) = open_existing(&world.paths, &key, LIMITS).unwrap();
            log.append(
                ISSUER_SUBJECT,
                view.encode_event(
                    &key,
                    &receipt(&key, &world.marked, &world.binding).unwrap(),
                    key.digest(b"epoch\0", world.marked.host_epoch.as_bytes()),
                    event,
                    1,
                ),
            )
            .unwrap();
        }
        let before = std::fs::read(directory(&world.paths).join("noexec.astream")).unwrap();
        assert!(reserve_at(&world.paths, &world.marked, &world.binding, LIMITS).is_err());
        assert_eq!(
            std::fs::read(directory(&world.paths).join("noexec.astream")).unwrap(),
            before
        );
    }
}

#[test]
fn simultaneous_legacy_and_reservation_claims_choose_one_owner() {
    let world = World::new(&aterm_models_harness::recipient_reservation_model().init_state());
    let barrier = std::sync::Barrier::new(2);
    let (reserved, legacy) = std::thread::scope(|scope| {
        let reserved = scope.spawn(|| {
            barrier.wait();
            reserve_at(&world.paths, &world.marked, &world.binding, LIMITS)
        });
        let legacy = scope.spawn(|| {
            barrier.wait();
            record_event_at(
                &world.paths,
                &world.plain,
                &world.binding,
                Some(Event::Prepare),
            )
        });
        (reserved.join().unwrap(), legacy.join().unwrap())
    });
    assert_ne!(reserved.is_ok(), legacy.is_ok());
    assert_eq!(read(&world.paths).unwrap().unwrap().1.sequence, 1);
}
