// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::accounts::LOGIN_ENV;
use crate::harness::accounts_pool::ExecutionIntent;
use crate::harness::contract::Scope;
use crate::harness::native_act::{Image, Process, RestartCommand};
use crate::harness::upgrade::Dialect;

#[path = "native_account_wire_model_tests.rs"]
mod model;

fn account(generation: u8) -> ExecutionIntent {
    ExecutionIntent::Stored {
        namespace: "12".repeat(32),
        login: None,
        owner: Some("account".into()),
        generation: format!("{generation:02x}").repeat(32),
    }
}

fn absolute(path: &str) -> String {
    if cfg!(windows) {
        format!("C:\\{path}")
    } else {
        format!("/{path}")
    }
}

fn fixture() -> Predecessor {
    let image = |n| Image {
        path: format!("/private/image-{n}").into_bytes(),
        device: 1,
        inode: n,
    };
    let process = |pid| Process {
        pid,
        birth: 70,
        parent: 1,
        group: pid,
        session: 400,
        image: image(u64::from(pid)),
    };
    let command = RestartCommand {
        account_intent: Some(Box::new(account(1))),
        image: image(90),
        program: absolute("private/image-90"),
        argv: vec!["--resume".into(), "private-conversation-canary".into()],
        env: vec![("PRIVATE_CANARY".into(), "do-not-log-this-value".into())],
        unset: Some(vec![LOGIN_ENV.into()]),
        cwd: None,
        heal: None,
        dialect: Dialect::Zsh,
        settles: None,
    };
    let prepare = Prepare {
        rejection_domain: None,
        asked: None,
        id: "ab".repeat(16),
        host_epoch: "cd".repeat(16),
        holder: "private-holder-canary".into(),
        revision: 1,
        expected_human_seq: Some(7),
        deadline_us: 1_000_000,
        duty: Scope::Relaunch,
        plan: NativePlan::Restart {
            cold: None,
            process: process(500),
            shell: process(400),
            launcher: image(20),
            observation: None,
            socket: absolute("private/native.sock"),
            forward: Box::new(command.clone()),
            rollback: Some(Box::new(command)),
        },
    };
    let binding = Binding {
        id: prepare.id.clone(),
        capability: "ef".repeat(32),
    };
    Predecessor::new(prepare, binding).unwrap()
}

fn relation(parent: Predecessor, phase: ImagePhase) -> Relation {
    let candidate = parent
        .prepare()
        .with_account_successor(phase, "01".repeat(16), 2, 8, 1_000_001, account(2))
        .unwrap();
    Relation::new(parent, phase, candidate).unwrap()
}

fn binding(relation: &Relation) -> Binding {
    Binding {
        id: relation.candidate().id.clone(),
        capability: "56".repeat(32),
    }
}

#[test]
fn all_counted_request_kinds_roundtrip() {
    let parent = fixture();
    for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
        let rel = relation(parent.clone(), phase);
        for request in [
            Request::Closure(parent.clone()),
            Request::Claim(rel.clone()),
            Request::Fence(rel.clone()),
            Request::Observe(rel),
        ] {
            let body = request.encode();
            assert!(body.len() <= MAX_BODY);
            assert_eq!(Request::decode(&body), Some(request.clone()));
            assert_eq!(Request::decode(&body).unwrap().encode(), body);
        }
    }
}

#[test]
fn request_decoder_rejects_incomplete_noncanonical_and_misframed_payloads() {
    let body = Request::Claim(relation(fixture(), ImagePhase::Forward)).encode();
    for end in 0..body.len() {
        assert!(Request::decode(&body[..end]).is_none(), "truncation {end}");
    }
    for index in [0, 1, 2, 3, 4, 5, 6, 7, 16, 48] {
        let mut bad = body.clone();
        bad[index] = 0xff;
        assert!(Request::decode(&bad).is_none(), "header {index}");
    }
    for at in [8, 12] {
        let mut bad = body.clone();
        bad[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Request::decode(&bad).is_none());
    }
    let mut bad = body.clone();
    bad.push(0);
    assert!(Request::decode(&bad).is_none());
    assert!(Request::decode(&vec![0; MAX_BODY + 1]).is_none());
    let mut bad = body.clone();
    bad[4] = 1;
    assert!(
        Request::decode(&bad).is_none(),
        "closure cannot carry candidate or phase"
    );
    let mut bad = Request::Closure(fixture()).encode();
    bad[5] = 1;
    assert!(Request::decode(&bad).is_none());
    let mut bad = body.clone();
    bad[HEADER] = 0xff;
    assert!(Request::decode(&bad).is_none());
    let mut noncanonical = body.clone();
    noncanonical.insert(HEADER + "prepare".len(), b' ');
    let len = u32::from_le_bytes(body[8..12].try_into().unwrap());
    noncanonical[8..12].copy_from_slice(&(len + 1).to_le_bytes());
    assert!(
        Request::decode(&noncanonical).is_none(),
        "nested spacing is canonical"
    );
}

#[test]
fn large_canonical_recipes_fit_existing_counted_body_without_double_hex() {
    let base = fixture();
    let large = |n| {
        let mut p = base.prepare().clone();
        p.holder = "h".repeat(1024);
        let NativePlan::Restart {
            forward,
            rollback,
            process,
            shell,
            launcher,
            socket,
            ..
        } = &mut p.plan
        else {
            unreachable!()
        };
        let path = format!("/{}", "x".repeat(super::super::native_act::MAX_PATH - 1));
        process.image.path = path.as_bytes().to_vec();
        shell.image.path = path.as_bytes().to_vec();
        launcher.path = path.as_bytes().to_vec();
        *socket = absolute(
            &"x".repeat(super::super::native_act::MAX_PATH - if cfg!(windows) { 3 } else { 1 }),
        );
        forward.argv.push("x".repeat(n));
        rollback.as_mut().unwrap().argv.push("x".repeat(n));
        Predecessor::new(p, base.binding().clone())
    };
    let (mut lo, mut hi) = (0, super::super::native_act::MAX_REQUEST);
    while lo + 1 < hi {
        let n = (lo + hi) / 2;
        if large(n).is_some() {
            lo = n;
        } else {
            hi = n;
        }
    }
    assert!(large(hi).is_none());
    let request = Request::Claim(relation(large(lo).unwrap(), ImagePhase::Forward));
    let bytes = request.encode();
    assert!(bytes.len() > aterm_types::control_verbs::MAX_REQUEST_LINE);
    assert!(bytes.len() <= MAX_BODY && MAX_BODY < 256 * 1024);
    assert_eq!(Request::decode(&bytes), Some(request));
}

#[test]
fn commitments_bind_exact_recipes_binding_phase_and_domain() {
    let parent = fixture();
    let old = relation(parent.clone(), ImagePhase::Forward);
    assert_ne!(old.digest().0, parent.digest().0);
    for change in 0..5 {
        let mut p = parent.prepare().clone();
        let mut b = parent.binding().clone();
        match change {
            0 => p.holder.push('x'),
            1 => p.deadline_us += 2,
            2 => p.revision += 2,
            3 => p.expected_human_seq = Some(9),
            _ => b.capability = "77".repeat(32),
        }
        let new = Predecessor::new(p, b).unwrap();
        assert_ne!(new.digest(), parent.digest());
        assert_ne!(relation(new, ImagePhase::Forward).digest(), old.digest());
    }
    assert_ne!(
        relation(parent.clone(), ImagePhase::Rollback).digest(),
        old.digest()
    );
    let mut candidate = old.candidate().clone();
    candidate.deadline_us += 1;
    assert_ne!(
        Relation::new(parent.clone(), ImagePhase::Forward, candidate)
            .unwrap()
            .digest(),
        old.digest()
    );
    let mut candidate = old.candidate().clone();
    candidate.id = parent.prepare().id.clone();
    assert!(Relation::new(parent.clone(), ImagePhase::Forward, candidate).is_none());
    let mut b = parent.binding().clone();
    b.id = "02".repeat(16);
    assert!(Predecessor::new(parent.prepare().clone(), b).is_none());
    assert_ne!(
        commitment(b"x", &[b"a", b"bc"]),
        commitment(b"x", &[b"ab", b"c"])
    );
}

fn replies(rel: &Relation) -> Vec<Reply> {
    let receipt = RelationReceipt::for_relation(rel, u64::MAX, u64::MAX);
    let mut result = vec![
        Reply::Closure {
            receipt: ClosureReceipt::for_predecessor(rel.predecessor(), u64::MAX, u64::MAX),
            phase: rel.phase(),
            successor: None,
        },
        Reply::Closure {
            receipt: ClosureReceipt::for_predecessor(rel.predecessor(), 2, 3),
            phase: rel.phase(),
            successor: Some(rel.candidate().id.clone()),
        },
        Reply::Fenced(receipt.clone()),
        Reply::Observed {
            receipt: receipt.clone(),
            outcome: Observation::Fenced,
        },
        Reply::Unknown,
        Reply::Denied(Denial::Authority),
        Reply::Denied(Denial::Epoch),
        Reply::Denied(Denial::Phase),
        Reply::Denied(Denial::Conflict),
        Reply::Denied(Denial::State),
        Reply::Denied(Denial::Deadline),
        Reply::Denied(Denial::Account),
        Reply::Unsupported,
        Reply::Unavailable(Unavailable::Busy),
        Reply::Unavailable(Unavailable::Capacity),
        Reply::Unavailable(Unavailable::Custody),
        Reply::Unavailable(Unavailable::Entropy),
        Reply::Unavailable(Unavailable::Constructor),
    ];
    for state in [
        ChildState::Prepared,
        ChildState::MayExecute,
        ChildState::NoExec(ImagePhase::Forward),
        ChildState::NoExec(ImagePhase::Rollback),
        ChildState::Sealed { may_execute: false },
        ChildState::Sealed { may_execute: true },
    ] {
        if rel.phase() == ImagePhase::Rollback && state == ChildState::NoExec(ImagePhase::Forward) {
            continue;
        }
        result.push(Reply::Claimed {
            receipt: receipt.clone(),
            binding: binding(rel),
            state,
        });
        result.push(Reply::Observed {
            receipt: receipt.clone(),
            outcome: Observation::Claimed(state),
        });
    }
    result
}

#[test]
fn every_reply_roundtrips_at_maximum_integer_width_and_redacts_capabilities() {
    for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
        let rel = relation(fixture(), phase);
        for reply in replies(&rel) {
            let wire = reply.encode().unwrap();
            assert!(wire.len() <= MAX_REPLY);
            assert_eq!(Reply::decode(&wire), Some(reply.clone()));
            assert_eq!(
                Reply::decode_line(wire.strip_suffix('\n').unwrap()),
                Some(reply.clone())
            );
            assert!(Reply::decode_line(&wire).is_none());
            assert!(!format!("{reply:?}").contains(&"56".repeat(32)));
            if matches!(reply, Reply::Observed { .. }) {
                assert!(!wire.contains(" cap="));
            }
        }
        let text = format!(
            "{:?} {:?} {:?}",
            rel.predecessor(),
            rel,
            Request::Claim(rel.clone())
        );
        for private in [
            "private-conversation-canary",
            "private-holder-canary",
            "do-not-log-this-value",
            &"ef".repeat(32),
        ] {
            assert!(!text.contains(private));
        }
    }
}

#[test]
fn reply_decoding_and_correlation_refuse_ambiguous_or_mismatched_evidence() {
    let rel = relation(fixture(), ImagePhase::Rollback);
    let claimed = Reply::Claimed {
        receipt: RelationReceipt::for_relation(&rel, 2, 3),
        binding: binding(&rel),
        state: ChildState::Prepared,
    };
    let wire = claimed.encode().unwrap();
    let request = Request::Claim(rel.clone());
    assert!(claimed.acknowledges(&request));
    for before in ["epoch=", "parent=", "child=", "digest="] {
        let start = wire.find(before).unwrap() + before.len();
        let mut changed = wire.as_bytes().to_vec();
        changed[start] = if changed[start] == b'0' { b'1' } else { b'0' };
        let other = Reply::decode(std::str::from_utf8(&changed).unwrap()).unwrap();
        assert!(!other.acknowledges(&request), "{before}");
    }
    for bad in [
        wire.replace("seq=2", "seq=02"),
        wire.replace(" v=1", " v=01"),
        wire.replace("\n", " unknown=1\n"),
        wire.replace("\n", " seq=2\n"),
        wire.replace(' ', "  "),
        wire.replace(' ', "\t"),
        wire.replace('\n', "\r\n"),
        wire.trim_end().to_owned(),
        format!("{wire}\n"),
        wire.replace("OK ", "ERR "),
        wire.replace("cap=56", "cap=zz"),
        wire.replace("child-state=prepared", "child-state=noexec-forward"),
    ] {
        assert!(Reply::decode(&bad).is_none());
    }
    for reply in replies(&rel) {
        let expected = matches!(reply, Reply::Claimed { .. });
        assert_eq!(reply.acknowledges(&request), expected);
    }
    let mut wrong = binding(&rel);
    wrong.id = "99".repeat(16);
    let malformed = Reply::Claimed {
        receipt: RelationReceipt::for_relation(&rel, 2, 3),
        binding: wrong,
        state: ChildState::Prepared,
    };
    assert!(malformed.encode().is_none());
    assert!(!malformed.acknowledges(&request));
    assert!(Reply::decode(&"x".repeat(MAX_REPLY + 1)).is_none());
    assert!(Reply::decode_line(&"x".repeat(MAX_REPLY)).is_none());
}

#[test]
fn closure_acknowledgements_require_an_existing_stored_account_phase() {
    let base = fixture();
    for rollback_only in [false, true] {
        let mut prepare = base.prepare().clone();
        let NativePlan::Restart {
            forward, rollback, ..
        } = &mut prepare.plan
        else {
            unreachable!()
        };
        if rollback_only {
            forward.account_intent = Some(Box::new(ExecutionIntent::external()));
        } else {
            *rollback = None;
        }
        let parent = Predecessor::new(prepare, base.binding().clone()).unwrap();
        let request = Request::Closure(parent.clone());
        for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
            let supported = (phase == ImagePhase::Rollback) == rollback_only;
            assert_eq!(parent.supports_phase(phase), supported);
            for successor in [None, Some("01".repeat(16))] {
                let reply = Reply::Closure {
                    receipt: ClosureReceipt::for_predecessor(&parent, 1, 2),
                    phase,
                    successor,
                };
                let decoded = Reply::decode(&reply.encode().unwrap()).unwrap();
                assert_eq!(decoded.acknowledges(&request), supported);
            }
        }
    }
    let request = Request::Closure(base.clone());
    for successor in [base.prepare().id.clone(), "invalid".into()] {
        let reply = Reply::Closure {
            receipt: ClosureReceipt::for_predecessor(&base, 1, 2),
            phase: ImagePhase::Forward,
            successor: Some(successor),
        };
        assert!(reply.encode().is_none());
        assert!(!reply.acknowledges(&request));
    }
}

#[test]
fn predecessor_excludes_cold_observed_and_non_account_restarts() {
    use crate::harness::native_act::{ColdBilling, ColdIntent};
    use crate::harness::native_observe::ObservationBinding;
    let base = fixture();
    for variant in 0..3 {
        let mut prepare = base.prepare().clone();
        let NativePlan::Restart {
            cold,
            observation,
            forward,
            rollback,
            ..
        } = &mut prepare.plan
        else {
            unreachable!()
        };
        match variant {
            0 => {
                forward.account_intent = None;
                rollback.as_mut().unwrap().account_intent =
                    Some(Box::new(ExecutionIntent::external()));
            }
            1 => {
                *cold = Some(Box::new(ColdIntent {
                    login: None,
                    home: None,
                    auth_observed: false,
                    route: crate::harness::accounts::AuthRoute::Outside,
                    conversation: None,
                    profile: None,
                    mode: crate::harness::accounts::Mode::Early,
                    billing: ColdBilling::Keep,
                }))
            }
            _ => {
                *observation = Some(ObservationBinding {
                    id: "11".repeat(16),
                    host_epoch: prepare.host_epoch.clone(),
                    capability: "22".repeat(32),
                })
            }
        }
        assert!(
            NativeRequest::prepare(prepare.clone()).encode().is_some(),
            "native fixture {variant}"
        );
        assert!(
            Predecessor::new(prepare, base.binding().clone()).is_none(),
            "variant {variant}"
        );
    }
}

#[test]
fn observations_correlate_without_acknowledging_or_granting_a_binding() {
    let rel = relation(fixture(), ImagePhase::Rollback);
    let request = Request::Observe(rel.clone());
    for outcome in [
        Observation::Fenced,
        Observation::Claimed(ChildState::Prepared),
    ] {
        let reply = Reply::Observed {
            receipt: RelationReceipt::for_relation(&rel, 1, 2),
            outcome,
        };
        let wire = reply.encode().unwrap();
        let decoded = Reply::decode(&wire).unwrap();
        assert_eq!(decoded.observation_for(&request), Some(outcome));
        assert_eq!(decoded.observation_for(&Request::Claim(rel.clone())), None);
        assert!(!decoded.acknowledges(&request));
        for field in ["epoch", "parent", "child", "digest"] {
            let changed = Reply::decode(&model::mutate(wire.clone(), field)).unwrap();
            assert_eq!(changed.observation_for(&request), None, "{field}");
        }
        let wrong_phase = Reply::decode(&wire.replace("phase=rollback", "phase=forward")).unwrap();
        assert_eq!(wrong_phase.observation_for(&request), None);
    }
    let incompatible = Reply::Observed {
        receipt: RelationReceipt::for_relation(&rel, 1, 2),
        outcome: Observation::Claimed(ChildState::NoExec(ImagePhase::Forward)),
    };
    assert_eq!(incompatible.observation_for(&request), None);
    for reply in replies(&rel) {
        if !matches!(reply, Reply::Observed { .. }) {
            assert_eq!(reply.observation_for(&request), None);
        }
    }
}

#[test]
fn child_closure_requires_a_command_for_its_recorded_phase() {
    let parent = fixture();
    let mut prepare = parent.prepare().clone();
    let NativePlan::Restart { rollback, .. } = &mut prepare.plan else {
        unreachable!()
    };
    *rollback = None;
    let rel = relation(
        Predecessor::new(prepare, parent.binding().clone()).unwrap(),
        ImagePhase::Forward,
    );
    let receipt = RelationReceipt::for_relation(&rel, 2, 3);
    for phase in [ImagePhase::Forward, ImagePhase::Rollback] {
        let state = ChildState::NoExec(phase);
        let claim = Reply::Claimed {
            receipt: receipt.clone(),
            binding: binding(&rel),
            state,
        };
        let claim = Reply::decode(&claim.encode().unwrap()).unwrap();
        assert_eq!(
            claim.acknowledges(&Request::Claim(rel.clone())),
            phase == ImagePhase::Forward
        );
        let observation = Reply::Observed {
            receipt: receipt.clone(),
            outcome: Observation::Claimed(state),
        };
        let observation = Reply::decode(&observation.encode().unwrap()).unwrap();
        assert_eq!(
            observation
                .observation_for(&Request::Observe(rel.clone()))
                .is_some(),
            phase == ImagePhase::Forward
        );
    }
}
