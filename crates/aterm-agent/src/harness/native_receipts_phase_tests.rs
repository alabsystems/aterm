// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::interp::State;

fn fixture() -> (aterm_tempfile::TempDir, Paths, Prepare, Binding) {
    let (home, paths, mut prepare, binding) = super::fixture();
    if let NativePlan::Restart {
        forward, rollback, ..
    } = &mut prepare.plan
    {
        *rollback = Some(forward.clone());
    }
    (home, paths, prepare, binding)
}

fn close(
    paths: &Paths,
    prepare: &Prepare,
    binding: &Binding,
    phase: ImagePhase,
) -> io::Result<Observed> {
    record_with_phase_at_limits(
        paths,
        prepare,
        binding,
        Some(Event::NoExec),
        Some(phase),
        LIMITS,
    )
}

fn opposite(phase: ImagePhase) -> ImagePhase {
    match phase {
        ImagePhase::Forward => ImagePhase::Rollback,
        ImagePhase::Rollback => ImagePhase::Forward,
    }
}

#[test]
fn exact_phase_survives_reopen_and_conflicting_retries_preserve_it() {
    for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
        let (_home, paths, prepare, binding) = fixture();
        assert_eq!(observe_phase_at(&paths, &prepare, &binding).unwrap(), None);
        assert!(!paths.dir.exists());
        record_event_at(&paths, &prepare, &binding, Some(Event::Prepare)).unwrap();
        let observed = close(&paths, &prepare, &binding, phase).unwrap();
        assert_eq!(observed.sequence, 1);
        append_other(&paths, &prepare, &binding);
        let before = std::fs::read(directory(&paths).join("noexec.astream")).unwrap();
        assert_eq!(close(&paths, &prepare, &binding, phase).unwrap(), observed);
        assert!(close(&paths, &prepare, &binding, opposite(phase)).is_err());
        assert_eq!(
            observe_phase_at(&paths, &prepare, &binding).unwrap(),
            Some(PhaseObserved {
                phase,
                sequence: observed.sequence,
                recorded_ms: observed.recorded_ms,
            })
        );
        let mut changed = prepare.clone();
        changed.deadline_us += 1;
        assert_eq!(observe_phase_at(&paths, &changed, &binding).unwrap(), None);
        assert!(close(&paths, &changed, &binding, phase).is_err());
        changed = prepare.clone();
        changed.host_epoch = "dd".repeat(16);
        assert_eq!(observe_phase_at(&paths, &changed, &binding).unwrap(), None);
        assert!(close(&paths, &changed, &binding, phase).is_err());
        let mut wrong = binding.clone();
        wrong.capability = "de".repeat(32);
        assert_eq!(observe_phase_at(&paths, &prepare, &wrong).unwrap(), None);
        assert!(close(&paths, &prepare, &wrong, phase).is_err());
        assert_eq!(
            std::fs::read(directory(&paths).join("noexec.astream")).unwrap(),
            before
        );
    }
}

#[test]
fn phase_less_history_and_possible_execution_never_gain_a_phase() {
    for legacy in [true, false] {
        let (_home, paths, prepare, binding) = fixture();
        if legacy {
            record_at(&paths, &prepare, &binding).unwrap();
        } else {
            record_event_at(&paths, &prepare, &binding, Some(Event::Prepare)).unwrap();
            record_event_at(&paths, &prepare, &binding, Some(Event::NoExec)).unwrap();
        }
        assert_eq!(observe_phase_at(&paths, &prepare, &binding).unwrap(), None);
        for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
            assert!(close(&paths, &prepare, &binding, phase).is_err());
        }
    }
    let (_home, paths, prepare, binding) = fixture();
    record_event_at(&paths, &prepare, &binding, Some(Event::Prepare)).unwrap();
    record_event_at(&paths, &prepare, &binding, Some(Event::MayExecute)).unwrap();
    assert!(close(&paths, &prepare, &binding, ImagePhase::Forward).is_err());
    assert_eq!(observe_phase_at(&paths, &prepare, &binding).unwrap(), None);
    let (_home, paths, prepare, binding) = super::fixture();
    record_event_at(&paths, &prepare, &binding, Some(Event::Prepare)).unwrap();
    assert!(close(&paths, &prepare, &binding, ImagePhase::Rollback).is_err());
    assert_eq!(observe_phase_at(&paths, &prepare, &binding).unwrap(), None);
}

#[test]
fn phase_frame_fits_the_original_exact_closing_budget() {
    assert_eq!(PHASE_SUBJECT.len(), ISSUER_SUBJECT.len());
    let (_home, paths, prepare, binding) = fixture();
    let cap = Limits {
        records: 3,
        bytes: 3 * ISSUER_FRAME,
        record_bytes: SUCCESSOR_BYTES,
    };
    record_event_at_limits(&paths, &prepare, &binding, Some(Event::Prepare), cap).unwrap();
    record_with_phase_at_limits(
        &paths,
        &prepare,
        &binding,
        Some(Event::NoExec),
        Some(ImagePhase::Rollback),
        cap,
    )
    .unwrap();
    let (key, view) = read(&paths).unwrap().unwrap();
    assert_eq!(view.sequence, 2);
    assert_eq!(view.committed_bytes, 2 * ISSUER_FRAME);
    assert!(view.within_capacity(cap, 0));
    let receipt = receipt(&key, &prepare, &binding).unwrap();
    assert_eq!(
        view.encode_phase(&key, &receipt, [1; 32], ImagePhase::Forward, 1)
            .len(),
        ISSUER_BYTES
    );
}

#[test]
fn a_failed_journal_barrier_withholds_typed_acknowledgement_and_retries() {
    let (_home, paths, prepare, binding) = fixture();
    record_event_at(&paths, &prepare, &binding, Some(Event::Prepare)).unwrap();
    let fault = aterm_durable::seam::FailingDirs::with_kind(io::ErrorKind::Unsupported);
    assert!(close(&paths, &prepare, &binding, ImagePhase::Rollback).is_err());
    drop(fault);
    let observed = close(&paths, &prepare, &binding, ImagePhase::Rollback).unwrap();
    assert_eq!(
        observe_phase_at(&paths, &prepare, &binding).unwrap(),
        Some(PhaseObserved {
            phase: ImagePhase::Rollback,
            sequence: observed.sequence,
            recorded_ms: observed.recorded_ms,
        })
    );
    assert_eq!(
        close(&paths, &prepare, &binding, ImagePhase::Rollback).unwrap(),
        observed
    );
    assert_eq!(read(&paths).unwrap().unwrap().1.sequence, 2);
}

#[test]
fn phase_frames_authenticate_tags_chain_and_exact_format() {
    let (_home, paths, prepare, binding) = fixture();
    record_event_at(&paths, &prepare, &binding, Some(Event::Prepare)).unwrap();
    let (key, view) = read(&paths).unwrap().unwrap();
    let receipt = receipt(&key, &prepare, &binding).unwrap();
    let epoch = key.digest(b"epoch\0", prepare.host_epoch.as_bytes());
    let valid = view.encode_phase(&key, &receipt, epoch, ImagePhase::Rollback, 11);
    assert!(record_subject(PHASE_SUBJECT, &valid));
    assert!(!record_subject(ISSUER_SUBJECT, &valid));
    for wrong_length in [
        valid[..valid.len() - 1].to_vec(),
        [valid.as_slice(), &[0]].concat(),
    ] {
        let (_, mut candidate) = read(&paths).unwrap().unwrap();
        assert!(candidate.ingest(&key, &wrong_length).is_err());
    }
    for at in 0..valid.len() {
        let (_, mut candidate) = read(&paths).unwrap().unwrap();
        let mut bad = valid.clone();
        bad[at] ^= 1;
        assert!(candidate.ingest(&key, &bad).is_err(), "byte {at}");
    }
    for tag in [0, 1, 2, 3, 4, 7, 255] {
        let (_, mut candidate) = read(&paths).unwrap().unwrap();
        let mut bad = valid[..valid.len() - 32].to_vec();
        bad[148] = tag;
        bad.extend_from_slice(hmac::sign(&key.mac, &bad).as_ref());
        assert!(candidate.ingest(&key, &bad).is_err(), "tag {tag}");
    }
    let (_, mut candidate) = read(&paths).unwrap().unwrap();
    candidate.ingest(&key, &valid).unwrap();
    assert!(candidate.ingest(&key, &valid).is_err());
}

fn step(state: &State, action: &str, phase: ImagePhase, fault: u8) -> State {
    let (_home, paths, prepare, binding) = fixture();
    record_event_at(&paths, &prepare, &binding, Some(Event::Prepare)).unwrap();
    match state["disk"] {
        1 => {
            close(&paths, &prepare, &binding, phase).unwrap();
        }
        2 => {
            record_event_at(&paths, &prepare, &binding, Some(Event::NoExec)).unwrap();
        }
        _ => {}
    }
    if state["other"] == 1 {
        append_other(&paths, &prepare, &binding);
    }
    let mut next = state.clone();
    match action {
        "Close" | "LostReply" => {
            close(&paths, &prepare, &binding, phase).unwrap();
            if action == "Close" {
                next.insert("ack", 1);
            } else {
                next.insert("attempted", 1);
            }
        }
        "Legacy" => {
            record_event_at(&paths, &prepare, &binding, Some(Event::NoExec)).unwrap();
        }
        "Refused" => {
            let mut wrong = binding.clone();
            wrong.capability = "de".repeat(32);
            assert!(close(&paths, &prepare, &wrong, phase).is_err());
            next.insert("attempted", 1);
            next.insert("failed", 1);
            if fault == 3 {
                next.insert("ack", 1);
            }
        }
        "Retry" => {
            if fault == 4 {
                next.insert("blocked", 1);
            } else {
                assert_eq!(
                    close(&paths, &prepare, &binding, phase).unwrap().sequence,
                    1
                );
                next.insert("ack", 1);
            }
            next.insert("retried", 1);
            next.insert("failed", 0);
        }
        "Conflict" => {
            let accepted = if fault == 1 {
                // Negative control: return the old NoExec as if it acknowledged
                // the requested, opposite phase (the missing exact-phase check).
                record_event_at(&paths, &prepare, &binding, Some(Event::NoExec)).is_ok()
            } else {
                close(&paths, &prepare, &binding, opposite(phase)).is_ok()
            };
            next.insert("accepted_conflict", i64::from(accepted));
            next.insert("conflict", 1);
        }
        "AppendOther" => {
            append_other(&paths, &prepare, &binding);
            next.insert("other", 1);
        }
        "Read" => {
            let observed = if fault == 2 {
                observe_at(&paths, &binding).unwrap().map(|_| phase)
            } else {
                observe_phase_at(&paths, &prepare, &binding)
                    .unwrap()
                    .map(|value| value.phase)
            };
            if state["disk"] == 2 {
                next.insert("inferred_legacy", i64::from(observed.is_some()));
            } else {
                assert_eq!(observed, Some(phase));
                let observed = observe_phase_at(&paths, &prepare, &binding)
                    .unwrap()
                    .unwrap();
                let sequence = if fault == 5 {
                    read(&paths).unwrap().unwrap().1.sequence - 1
                } else {
                    observed.sequence
                };
                next.insert("wrong_position", i64::from(sequence != 1));
            }
            next.insert("read", 1);
        }
        _ => unreachable!(),
    }
    let observed = observe_phase_at(&paths, &prepare, &binding).unwrap();
    next.insert(
        "disk",
        if observed.is_some() {
            1
        } else if observe_at(&paths, &binding).unwrap().is_some() {
            2
        } else {
            0
        },
    );
    next
}

fn append_other(paths: &Paths, prepare: &Prepare, binding: &Binding) {
    let mut prepare = prepare.clone();
    let mut binding = binding.clone();
    prepare.id = "55".repeat(16);
    binding.id = prepare.id.clone();
    record_event_at(paths, &prepare, &binding, Some(Event::Prepare)).unwrap();
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    let forward = step(state, action, ImagePhase::Forward, 0);
    assert_eq!(forward, step(state, action, ImagePhase::Rollback, 0));
    forward
}

#[test]
fn phase_observations_refine_exact_retries_and_legacy_unknowns() {
    let model = aterm_models_harness::noexec_phase_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
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
    let phase = ImagePhase::Forward;
    let closed = step(&model.init_state(), "Close", phase, 0);
    let legacy = step(&model.init_state(), "Legacy", phase, 0);
    let refused = step(&model.init_state(), "Refused", phase, 0);
    assert!(!model.check_invariant("ExactPhase", &step(&closed, "Conflict", phase, 1)));
    assert!(!model.check_invariant("LegacyUnknown", &step(&legacy, "Read", phase, 2)));
    assert!(!model.check_invariant(
        "NoFailedAcknowledgement",
        &step(&model.init_state(), "Refused", phase, 3)
    ));
    assert!(!model.check_invariant("RetryProgress", &step(&refused, "Retry", phase, 4)));
    let later = step(&closed, "AppendOther", phase, 0);
    assert!(!model.check_invariant("OriginalPosition", &step(&later, "Read", phase, 5)));
}
