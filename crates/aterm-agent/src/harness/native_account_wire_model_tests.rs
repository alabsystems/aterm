// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::{interp::State, xref::StepEvidence};

pub(super) fn mutate(wire: String, field: &str) -> String {
    let Some(start) = wire
        .find(&format!(" {field}="))
        .map(|i| i + field.len() + 2)
    else {
        return wire;
    };
    let mut bytes = wire.into_bytes();
    bytes[start] = if bytes[start] == b'0' { b'1' } else { b'0' };
    String::from_utf8(bytes).unwrap()
}

fn evaluate(state: &State, fault: u8, evidence: Option<&mut StepEvidence>) -> bool {
    static BASE: std::sync::OnceLock<Relation> = std::sync::OnceLock::new();
    static FORWARD_ONLY: std::sync::OnceLock<Relation> = std::sync::OnceLock::new();
    let base = BASE.get_or_init(|| relation(fixture(), ImagePhase::Rollback));
    let rel = if state["lifecycle"] == 1 {
        FORWARD_ONLY.get_or_init(|| {
            let parent = fixture();
            let mut prepare = parent.prepare().clone();
            let NativePlan::Restart { rollback, .. } = &mut prepare.plan else {
                unreachable!()
            };
            *rollback = None;
            relation(
                Predecessor::new(prepare, parent.binding().clone()).unwrap(),
                ImagePhase::Forward,
            )
        })
    } else {
        base
    };
    // The base parent without its rollback, made once: a fixture value, whose making checks its
    // encoding by parsing it again.
    static NO_ROLLBACK: std::sync::OnceLock<Predecessor> = std::sync::OnceLock::new();
    let parent = if state["closure_phase"] == 0 && fault != 11 {
        NO_ROLLBACK
            .get_or_init(|| {
                let parent = base.predecessor();
                let mut prepare = parent.prepare().clone();
                let NativePlan::Restart { rollback, .. } = &mut prepare.plan else {
                    unreachable!()
                };
                *rollback = None;
                Predecessor::new(prepare, parent.binding().clone()).unwrap()
            })
            .clone()
    } else {
        base.predecessor().clone()
    };
    let mut request = match state["request"] {
        0 => Request::Claim(rel.clone()),
        1 => Request::Fence(rel.clone()),
        2 => Request::Closure(parent.clone()),
        _ => Request::Observe(rel.clone()),
    };
    let receipt = RelationReceipt::for_relation(rel, 2, 3);
    let mut reply = match state["reply"] {
        0 => Reply::Claimed {
            receipt,
            binding: binding(rel),
            state: ChildState::Prepared,
        },
        1 => Reply::Fenced(receipt),
        2 => Reply::Closure {
            receipt: ClosureReceipt::for_predecessor(&parent, 1, 2),
            phase: ImagePhase::Rollback,
            successor: (state["successor"] == 1).then(|| "01".repeat(16)),
        },
        3 => Reply::Observed {
            receipt,
            outcome: Observation::Fenced,
        },
        4 => Reply::Unknown,
        5 => Reply::Denied(Denial::Authority),
        6 => Reply::Unsupported,
        _ => Reply::Unavailable(Unavailable::Busy),
    };
    let mut wire = reply.encode().unwrap();
    for (variable, field, ignored) in [
        ("parent", "parent", 1),
        ("epoch", "epoch", 2),
        ("candidate", "child", 3),
        ("digest", "digest", 4),
    ] {
        if state[variable] == 0 && fault != ignored {
            wire = mutate(wire, field);
        }
    }
    if state["phase"] == 0 && fault != 5 && state["reply"] != 2 {
        wire = if rel.phase() == ImagePhase::Rollback {
            wire.replace("phase=rollback", "phase=forward")
        } else {
            wire.replace("phase=forward", "phase=rollback")
        };
    }
    reply = Reply::decode(&wire).unwrap();
    if let Reply::Closure { successor, .. } = &mut reply
        && state["successor"] == 0
        && fault != 12
    {
        *successor = Some(parent.prepare().id.clone());
    }
    if let Reply::Claimed {
        binding,
        state: child,
        ..
    } = &mut reply
    {
        if state["binding"] == 0 && fault != 8 {
            binding.capability.clear();
        }
        if state["lifecycle"] < 2 && fault != 9 {
            *child = ChildState::NoExec(if state["lifecycle"] == 0 {
                ImagePhase::Forward
            } else {
                ImagePhase::Rollback
            });
        }
    }
    if fault == 6 {
        request = Request::Fence(rel.clone());
    }
    if fault == 7 {
        reply = Reply::Fenced(RelationReceipt::for_relation(rel, 2, 3));
    }
    match evidence {
        Some(e) => e.step("Check", || reply.acknowledges(&request)),
        None => reply.acknowledges(&request),
    }
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    step(state, action, None)
}

fn step(state: &State, action: &str, evidence: Option<&mut StepEvidence>) -> State {
    let mut next = state.clone();
    match action {
        "NextRequest" => {
            next.insert("request", state["request"] + 1);
        }
        "NextReply" => {
            next.insert("reply", state["reply"] + 1);
        }
        "WithSuccessor" => {
            next.insert("successor", 1);
        }
        "Check" => {
            next.insert("checked", 1);
            next.insert("accepted", i64::from(evaluate(state, 0, evidence)));
        }
        _ => {
            next.insert(
                match action {
                    "WrongParent" => "parent",
                    "WrongEpoch" => "epoch",
                    "WrongCandidate" => "candidate",
                    "WrongDigest" => "digest",
                    "WrongPhase" => "phase",
                    "WrongBinding" => "binding",
                    "WrongLifecycle" => "lifecycle",
                    "MissingRollback" => "lifecycle",
                    "WrongClosurePhase" => "closure_phase",
                    "WrongSuccessor" => "successor",
                    _ => panic!("unknown action {action}"),
                },
                0,
            );
            if action == "MissingRollback" {
                next.insert("lifecycle", 1);
            }
        }
    }
    next
}

#[test]
fn reply_matching_refines_real_codec_and_all_identity_checks() {
    let model = aterm_models_harness::account_wire_reply_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    let mut evidence = StepEvidence::new("AccountWireReply");
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = step(&state, action.name, Some(&mut evidence));
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    assert!(evidence.audit().is_complete(), "{:?}", evidence.audit());
    assert_eq!(transition(&model.init_state(), "Check")["accepted"], 1);
}

#[test]
fn every_omitted_guard_breaks_the_real_projection() {
    let model = aterm_models_harness::account_wire_reply_model();
    for (variable, value, request, reply, fault, law) in [
        ("parent", 0, 0, 0, 1, "ExactParent"),
        ("epoch", 0, 0, 0, 2, "ExactEpoch"),
        ("candidate", 0, 0, 0, 3, "ExactCandidate"),
        ("digest", 0, 0, 0, 4, "ExactDigest"),
        ("phase", 0, 0, 0, 5, "ExactPhase"),
        ("reply", 1, 0, 1, 6, "MatchingRole"),
        ("reply", 3, 1, 3, 7, "NoDiagnosticAck"),
        ("binding", 0, 0, 0, 8, "BindingValid"),
        ("lifecycle", 0, 0, 0, 9, "CompatibleLifecycle"),
        ("lifecycle", 1, 0, 0, 9, "CompatibleLifecycle"),
        ("closure_phase", 0, 2, 2, 11, "ClosurePhaseExists"),
        ("successor", 0, 2, 2, 12, "ValidSuccessor"),
    ] {
        let mut state = model.init_state();
        state.insert("request", request);
        state.insert("reply", reply);
        state.insert(variable, value);
        assert!(!evaluate(&state, 0, None), "control for {law}");
        let mut wrong = state.clone();
        wrong.insert("checked", 1);
        wrong.insert("accepted", i64::from(evaluate(&state, fault, None)));
        assert_eq!(wrong["accepted"], 1, "fault {fault}: {law}");
        assert!(!model.check_invariant(law, &wrong), "fault {fault}: {law}");
    }
}
