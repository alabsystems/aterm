// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_spec::interp::State;
use std::cell::Cell;
use std::rc::Rc;

struct World {
    _home: aterm_tempfile::TempDir,
    paths: Paths,
    prepare: Prepare,
    binding: Binding,
}

impl World {
    fn new() -> Self {
        let (home, paths, mut prepare, binding) = crate::harness::native_receipts::tests::fixture();
        prepare.receipt_mode = Some(ReceiptMode::Recipients5);
        prepare.boot = Some(crate::harness::native_act::BootStamp {
            host: format!("macos:{}", "12".repeat(16)),
            boot: "34".repeat(16),
        });
        prepare.custody = Some(enroll_at(&paths).unwrap().stamp().into());
        prepare.rejection_domain = Some(rejection_domain_at(&paths).unwrap());
        reservation::reserve_at(&paths, &prepare, &binding, LIMITS).unwrap();
        binding::bind_at(&paths, &prepare, &binding).unwrap();
        Self {
            _home: home,
            paths,
            prepare,
            binding,
        }
    }

    fn writer(&self) -> io::Result<RecipientWriter> {
        open_at(&self.paths, &self.prepare, &self.binding)
    }

    fn history(&self) -> RecipientHistory {
        super::super::observe_at(&self.paths, &self.prepare)
            .unwrap()
            .unwrap()
    }
}

fn who(pid: u32) -> Recipient {
    Recipient {
        pid,
        birth: 80,
        group: pid,
        session: 400,
    }
}

#[test]
fn callbacks_follow_commit_keep_both_locks_and_are_never_replayed() {
    let world = World::new();
    let mut writer = world.writer().unwrap();
    let called = Rc::new(Cell::new(0));
    let before = called.clone();
    let barriers = Rc::new(Cell::new(0));
    let observed_barriers = barriers.clone();
    let hook = aterm_journal::seam::before_each_commit(move || {
        assert_eq!(before.get(), 0);
        observed_barriers.set(observed_barriers.get() + 1);
    });
    writer
        .record_then(ImagePhase::Forward, who(701), |observed| {
            assert_eq!(observed.sequence, 2);
            assert!(barriers.get() > 0);
            assert!(reservation::hold_at(&world.paths, &world.prepare, &world.binding).is_err());
            assert!(super::super::open_at(&world.paths, false).is_err());
            called.set(called.get() + 1);
        })
        .unwrap();
    drop(hook);
    assert_eq!(called.get(), 1);
    assert!(
        writer
            .record_then(ImagePhase::Forward, who(701), |_| called.set(2))
            .is_err()
    );
    assert!(writer.noexec().is_err());
    drop(writer);
    let mut reopened = world.writer().unwrap();
    assert!(
        reopened
            .record_then(ImagePhase::Forward, who(701), |_| called.set(2))
            .is_err()
    );
    reopened
        .record_then(ImagePhase::Rollback, who(702), |_| {
            called.set(called.get() + 1)
        })
        .unwrap();
    let sealed = reopened.seal().unwrap();
    assert_eq!(reopened.seal().unwrap(), sealed);
    assert!(
        reopened
            .record_then(ImagePhase::Rollback, who(703), |_| called.set(3))
            .is_err()
    );
    drop(reopened);
    assert_eq!(called.get(), 2);
    let history = world.history();
    assert_eq!(
        (history.forward, history.rollback),
        (Some(who(701)), Some(who(702)))
    );
    assert_eq!(history.state, Recorded::Sealed { may_execute: true });
}

#[test]
fn lost_ack_and_callback_failure_preserve_possible_execution() {
    for after in 0..3 {
        let world = World::new();
        let mut writer = world.writer().unwrap();
        match after {
            0 => {
                writer
                    .append(recipients::Append::Forward(who(701)))
                    .unwrap();
            }
            1 => {
                let result = writer.record_then(ImagePhase::Forward, who(701), |_| {
                    Err::<(), _>("delivery unknown")
                });
                assert_eq!(result.unwrap(), Err("delivery unknown"));
            }
            _ => {
                assert!(
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        writer.record_then(ImagePhase::Forward, who(701), |_| {
                            panic!("delivery interrupted")
                        })
                    }))
                    .is_err()
                );
            }
        }
        drop(writer);
        assert_eq!(world.history().forward, Some(who(701)));
        let mut recovered = world.writer().unwrap();
        assert!(
            recovered
                .record_then(ImagePhase::Forward, who(701), |_| panic!("replayed"))
                .is_err()
        );
    }
}

#[test]
fn missing_checkpoint_fences_this_writer_without_calling_the_continuation() {
    let world = World::new();
    let mut writer = world.writer().unwrap();
    let checkpoint = dir(&world.paths).join("recipients.checkpoint");
    std::fs::remove_file(&checkpoint).unwrap();
    std::fs::create_dir(&checkpoint).unwrap();
    let called = Cell::new(false);
    assert!(
        writer
            .record_then(ImagePhase::Forward, who(701), |_| called.set(true))
            .is_err()
    );
    assert!(!writer.usable);
    assert!(
        writer
            .record_then(ImagePhase::Rollback, who(702), |_| called.set(true))
            .is_err()
    );
    assert!(writer.noexec().is_err());
    assert!(writer.seal().is_err());
    assert!(!called.get());
}

#[test]
fn live_writer_refuses_lost_custody_and_terminal_retries_do_not_bypass_it() {
    for name in [
        "native-noexec/noexec.astream",
        "native-noexec/noexec.anchor",
        "native-noexec/noexec.checkpoint",
        "native-noexec.enrolled",
        "native-recipients/recipients.astream",
        "native-recipients/recipients.anchor",
        "native-recipients/recipients.checkpoint",
        "native-recipients.enrolled",
        "identity",
    ] {
        let world = World::new();
        let mut writer = world.writer().unwrap();
        let missing = world.paths.dir.join(name);
        std::fs::remove_file(&missing).unwrap();
        assert!(
            writer
                .record_then(ImagePhase::Forward, who(701), |_| panic!("lost custody"))
                .is_err(),
            "{name}"
        );
        assert!(!writer.usable);
        assert!(writer.noexec().is_err());
        assert!(writer.seal().is_err());
        assert!(!missing.exists());
    }
    let world = World::new();
    let mut writer = world.writer().unwrap();
    writer.noexec().unwrap();
    std::fs::remove_file(world.paths.dir.join("native-noexec.enrolled")).unwrap();
    assert!(writer.noexec().is_err());
    assert!(writer.seal().is_err());
}

#[test]
fn custody_lost_during_append_withholds_the_continuation() {
    for name in ["native-noexec/noexec.astream", "native-recipients.enrolled"] {
        let world = World::new();
        let mut writer = world.writer().unwrap();
        let missing = world.paths.dir.join(name);
        let hook = aterm_journal::seam::before_each_commit(move || {
            std::fs::remove_file(&missing).unwrap();
        });
        assert!(
            writer
                .record_then(ImagePhase::Forward, who(701), |_| panic!(
                    "lost during append"
                ))
                .is_err()
        );
        drop(hook);
        assert!(!writer.usable);
    }
}

#[test]
fn opening_does_not_create_missing_bindings_or_accept_replaced_reservations() {
    let world = World::new();
    let mut unbound = world.prepare.clone();
    unbound.id = "ee".repeat(16);
    let unbound_binding = Binding {
        id: unbound.id.clone(),
        capability: "aa".repeat(32),
    };
    reservation::reserve_at(&world.paths, &unbound, &unbound_binding, LIMITS).unwrap();
    let before = record_count_at(&world.paths).unwrap();
    assert!(open_at(&world.paths, &unbound, &unbound_binding).is_err());
    assert_eq!(record_count_at(&world.paths).unwrap(), before);
    let mut wrong = world.binding.clone();
    wrong.capability = "fe".repeat(32);
    assert!(open_at(&world.paths, &world.prepare, &wrong).is_err());
    let mut changed = world.prepare.clone();
    changed.custody = Some("ab".repeat(32));
    assert!(open_at(&world.paths, &changed, &world.binding).is_err());
    std::fs::remove_dir_all(directory(&world.paths)).unwrap();
    std::fs::remove_file(
        world
            .paths
            .dir
            .join(crate::harness::native_receipts::ENROLLMENT),
    )
    .unwrap();
    rejection_domain_at(&world.paths).unwrap();
    let mut other = world.prepare.clone();
    other.id = "ff".repeat(16);
    other.receipt_mode = None;
    let binding = Binding {
        id: other.id.clone(),
        capability: "aa".repeat(32),
    };
    record_event_at(&world.paths, &other, &binding, Some(Event::Prepare)).unwrap();
    reservation::reserve_at(&world.paths, &world.prepare, &world.binding, LIMITS).unwrap();
    assert!(world.writer().is_err());
}

#[test]
fn append_unwinding_fences_the_live_writer_before_any_continuation() {
    let world = World::new();
    let mut writer = world.writer().unwrap();
    let called = Cell::new(false);
    let hook = aterm_journal::seam::before_each_commit(|| panic!("interrupted append"));
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            writer.record_then(ImagePhase::Forward, who(701), |_| called.set(true))
        }))
        .is_err()
    );
    drop(hook);
    assert!(!writer.usable);
    assert!(
        writer
            .record_then(ImagePhase::Rollback, who(702), |_| called.set(true))
            .is_err()
    );
    assert!(!called.get());
}

#[test]
fn noexec_retry_is_stable_and_invalid_recipient_does_not_poison() {
    let world = World::new();
    let mut writer = world.writer().unwrap();
    assert!(
        writer
            .record_then(ImagePhase::Forward, who(1), |_| panic!("invalid"))
            .is_err()
    );
    assert!(writer.usable);
    let closed = writer.noexec().unwrap();
    assert_eq!(writer.noexec().unwrap(), closed);
    assert_eq!(writer.seal().unwrap(), closed);
    assert!(
        writer
            .record_then(ImagePhase::Forward, who(701), |_| panic!("terminal"))
            .is_err()
    );
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    transition_recorded(state, action, None)
}

fn transition_recorded(
    state: &State,
    action: &str,
    evidence: Option<&mut aterm_spec::xref::StepEvidence>,
) -> State {
    let world = World::new();
    let mut writer = Some(world.writer().unwrap());
    let calls = Cell::new(0);
    if state["recorded"] == 1 {
        if state["calls"] == 1 {
            writer
                .as_mut()
                .unwrap()
                .record_then(ImagePhase::Forward, who(701), |_| calls.set(1))
                .unwrap();
        } else {
            writer
                .as_mut()
                .unwrap()
                .append(recipients::Append::Forward(who(701)))
                .unwrap();
        }
    }
    if state["open"] == 0 {
        writer.take();
    }
    if state["intact"] == 0 {
        std::fs::remove_file(world.paths.dir.join("native-noexec.enrolled")).unwrap();
    }
    let mut next = state.clone();
    next.insert("ack", 0);
    next.insert("ready", 0);
    match action {
        "Send" => {
            next.insert(
                "ready",
                i64::from(state["recorded"] == 0 && state["intact"] == 1),
            );
            let mut run = || {
                writer
                    .as_mut()
                    .unwrap()
                    .record_then(ImagePhase::Forward, who(701), |_| {
                        calls.set(calls.get() + 1)
                    })
            };
            let result = if let Some(evidence) = evidence {
                evidence.step("Send", run)
            } else {
                run()
            };
            next.insert("ack", i64::from(result.is_ok()));
        }
        "LostAck" => {
            writer
                .as_mut()
                .unwrap()
                .append(recipients::Append::Forward(who(701)))
                .unwrap();
        }
        "Crash" => {
            writer.take();
            next.insert("seen", 0);
        }
        "Reopen" => {
            let run = || world.writer().ok();
            writer = if let Some(evidence) = evidence {
                evidence.step("Reopen", run)
            } else {
                run()
            };
        }
        "LoseCustody" => {
            std::fs::remove_file(world.paths.dir.join("native-noexec.enrolled")).unwrap();
        }
        "Read" => {}
        _ => unreachable!(),
    }
    let recorded = if let Some(writer) = &writer {
        writer.history().unwrap().unwrap()
    } else {
        world.history()
    }
    .forward
    .is_some();
    next.insert("open", i64::from(writer.is_some()));
    next.insert("recorded", i64::from(recorded));
    next.insert("ever", state["ever"].max(i64::from(recorded)));
    next.insert("calls", calls.get());
    next.insert(
        "intact",
        i64::from(world.paths.dir.join("native-noexec.enrolled").exists()),
    );
    if action == "Read" {
        next.insert("seen", i64::from(recorded));
    }
    next
}

#[test]
fn real_writer_refines_delivery_without_recovery_callbacks() {
    let model = aterm_models_harness::recipient_delivery_model();
    assert!(
        model
            .successors("Send", &model.init_state())
            .contains(&transition(&model.init_state(), "Send"))
    );
    let mut evidence = aterm_spec::xref::StepEvidence::new("RecipientDelivery");
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
    let mut premature = model.init_state();
    premature.insert("calls", 1);
    assert!(!model.check_invariant("RecordedBeforeCallback", &premature));
}
