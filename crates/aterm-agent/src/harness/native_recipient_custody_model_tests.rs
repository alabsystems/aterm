// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_spec::interp::State;

fn from_state(state: &State) -> World {
    let mut world = World::new();
    if state["reservation"] == 1 || state["bound"] == 1 {
        world.reserve();
    }
    if state["bound"] == 1 {
        world.bind().unwrap();
    }
    if state["generation"] == 0 {
        world.replace_legacy();
    }
    if state["reservation"] == 0 && state["bound"] == 1 {
        std::fs::remove_file(directory(&world.paths).join("noexec.astream")).unwrap();
    }
    match state["phase"] {
        0 => {
            std::fs::remove_file(world.paths.dir.join(ENROLLMENT)).unwrap();
            if state["journal"] == 0 {
                std::fs::remove_dir_all(dir(&world.paths)).unwrap();
            }
        }
        1 => world.pending(state["journal"] == 1),
        _ if state["journal"] == 0 => {
            std::fs::remove_dir_all(dir(&world.paths)).unwrap();
        }
        _ => {}
    }
    if state["identity"] == 0 {
        world.prepare.custody = Some("ff".repeat(32));
    }
    world
}

fn write_pending(world: &World) {
    let base = base_key(&world.paths, false).unwrap();
    let location = std::fs::canonicalize(&world.paths.dir).unwrap();
    let enrollment = Enrollment {
        nonce: [0x33; 32],
        active: false,
    };
    aterm_journal::write_private_durable(
        &world.paths.dir,
        ENROLLMENT,
        &enrollment.encode(&base, location.as_os_str().as_encoded_bytes()),
    )
    .unwrap();
}

fn build_pending(world: &World) {
    let base = base_key(&world.paths, false).unwrap();
    let location = std::fs::canonicalize(&world.paths.dir).unwrap();
    let location = location.as_os_str().as_encoded_bytes();
    let bytes = private_file::read_private(&world.paths.dir, ENROLLMENT, ENROLLMENT_BYTES).unwrap();
    let enrollment = Enrollment::decode(&bytes, &base, location).unwrap();
    let key = enrollment.key(&base, location);
    aterm_journal::create_private_dir_durable(&dir(&world.paths)).unwrap();
    let mut log = Log::open_until(
        &dir(&world.paths),
        NAME,
        &key.anchor,
        false,
        LIMITS,
        Instant::now() + Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(binding::replay(&log, &key).unwrap().sequence, 0);
    log.finish_open().unwrap();
}

fn reset_outputs(next: &mut State) {
    for field in ["enroll_ack", "bind_ack", "enroll_ready", "bind_ready"] {
        next.insert(field, 0);
    }
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    transition_recorded(state, action, None)
}

fn transition_recorded(
    state: &State,
    action: &str,
    evidence: Option<&mut aterm_spec::xref::StepEvidence>,
) -> State {
    let mut world = from_state(state);
    let mut next = state.clone();
    reset_outputs(&mut next);
    match action {
        "Enroll" => {
            let ready = (state["phase"] == 0 && state["journal"] == 0)
                || state["phase"] == 1
                || (state["phase"] == 2 && state["journal"] == 1);
            let result = if let Some(evidence) = evidence {
                evidence.step("Enroll", || enroll_at(&world.paths))
            } else {
                enroll_at(&world.paths)
            };
            next.insert("enroll_ready", i64::from(ready));
            next.insert("enroll_ack", i64::from(result.is_ok()));
            if let Ok(custody) = result {
                next.insert("phase", 2);
                next.insert("journal", 1);
                if state["identity"] == 1 && state["reservation"] == 0 {
                    world.prepare.custody = Some(custody.stamp().into());
                }
            }
        }
        "Bind" => {
            let ready = state["phase"] == 2
                && state["journal"] == 1
                && state["reservation"] == 1
                && state["identity"] == 1
                && state["generation"] == 1;
            let result = if let Some(evidence) = evidence {
                evidence.step("Bind", || world.bind())
            } else {
                world.bind()
            };
            next.insert("bind_ready", i64::from(ready));
            next.insert("bind_ack", i64::from(result.is_ok()));
        }
        "Pending" => {
            write_pending(&world);
            next.insert("phase", 1);
        }
        "Journal" => {
            build_pending(&world);
            next.insert("journal", 1);
        }
        "Reserve" => {
            world.reserve();
            next.insert("reservation", 1);
        }
        "LoseJournal" => {
            std::fs::remove_dir_all(dir(&world.paths)).unwrap();
            next.insert("journal", 0);
        }
        "LoseEnrollment" => {
            std::fs::remove_file(world.paths.dir.join(ENROLLMENT)).unwrap();
            next.insert("phase", 0);
        }
        "ForgetAll" => {
            std::fs::remove_file(world.paths.dir.join(ENROLLMENT)).unwrap();
            if dir(&world.paths).exists() {
                std::fs::remove_dir_all(dir(&world.paths)).unwrap();
            }
            next.insert("phase", 0);
            next.insert("journal", 0);
            next.insert("bound", 0);
            next.insert("identity", 0);
            next.insert("generation", 1);
        }
        "WrongStamp" => {
            world.prepare.custody = Some("ff".repeat(32));
            next.insert("identity", 0);
        }
        "LoseReservation" => {
            std::fs::remove_file(directory(&world.paths).join("noexec.astream")).unwrap();
            next.insert("reservation", 0);
        }
        "ReplaceReservation" => {
            world.replace_legacy();
            next.insert("generation", 0);
        }
        "Crash" => {}
        _ => unreachable!(),
    }
    if next["phase"] == 2 && next["journal"] == 1 {
        let opened = open_at(&world.paths, false).unwrap();
        next.insert("bound", i64::from(opened.view.sequence > 0));
    }
    next
}

#[test]
fn real_enrollment_and_binding_refine_the_custody_model() {
    let model = aterm_models_harness::recipient_custody_model();
    aterm_spec::interp::prove_and_catch(&model);
    let mut evidence = aterm_spec::xref::StepEvidence::new("RecipientCustody");
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
    let world = World::new();
    world.reserve();
    let acknowledged = world.bind().is_ok();
    let mut wrong = transition(&model.init_state(), "Enroll");
    wrong.insert("bind_ack", i64::from(acknowledged));
    assert!(!model.check_invariant("ReservationRequired", &wrong));
}
