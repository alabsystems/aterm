// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::{
    contract::Scope,
    native_act::{Image, Process},
    upgrade::Dialect,
};
use aterm_spec::interp::State;

fn stored(login: Option<&str>, generation: &str) -> Box<ExecutionIntent> {
    Box::new(ExecutionIntent::Stored {
        namespace: "12".repeat(32),
        login: login.map(str::to_owned),
        owner: Some("account".into()),
        generation: generation.repeat(32),
    })
}

fn forward(prepare: &mut Prepare) -> &mut RestartCommand {
    match &mut prepare.plan {
        NativePlan::Restart { forward, .. } => forward,
        _ => unreachable!(),
    }
}

fn pair() -> (Prepare, Prepare) {
    let image = Image {
        path: b"/model/claude".to_vec(),
        device: 1,
        inode: 2,
    };
    let process = Process {
        pid: 42,
        birth: 3,
        parent: 43,
        group: 42,
        session: 43,
        image: image.clone(),
    };
    let command = RestartCommand {
        image: image.clone(),
        program: at!("/model/claude").into(),
        argv: vec!["--resume".into(), "original-conversation".into()],
        env: vec![
            ("KEEP".into(), "original".into()),
            (LOGIN_ENV.into(), at!("/model/old").into()),
        ],
        unset: None,
        cwd: Some(at!("/model/project").into()),
        heal: None,
        dialect: Dialect::Zsh,
        settles: None,
        account_intent: Some(stored(Some(at!("/model/old")), "34")),
    };
    let old = Prepare {
        rejection_domain: None,
        asked: None,
        id: "ab".repeat(16),
        host_epoch: "cd".repeat(16),
        holder: "model-holder".into(),
        revision: 1,
        expected_human_seq: Some(0),
        deadline_us: 100,
        duty: Scope::Relaunch,
        plan: NativePlan::Restart {
            process: process.clone(),
            observation: None,
            shell: Process {
                pid: 43,
                parent: 2,
                group: 43,
                ..process
            },
            launcher: image,
            socket: at!("/model/control.sock").into(),
            forward: Box::new(command.clone()),
            cold: None,
            rollback: Some(Box::new(command)),
        },
    };
    let mut next = old.clone();
    next.id = "ef".repeat(16);
    next.revision = 2;
    next.expected_human_seq = Some(1);
    next.deadline_us = 200;
    let command = forward(&mut next);
    command.env[1].1 = at!("/model/new").into();
    command.account_intent = Some(stored(Some(at!("/model/new")), "56"));
    (old, next)
}

fn fixture(state: &State) -> (Prepare, Prepare) {
    let (old, mut next) = pair();
    if state["context"] == 0 {
        next.holder = "different-holder".into();
    }
    if state["native"] == 0 {
        let NativePlan::Restart { socket, .. } = &mut next.plan else {
            unreachable!()
        };
        *socket = at!("/model/other.sock").into();
    }
    let cmd = forward(&mut next);
    if state["command"] == 0 {
        cmd.argv[1] = "different-conversation".into();
    }
    if state["namespace"] == 0 {
        let Some(ExecutionIntent::Stored { namespace, .. }) = cmd.account_intent.as_deref_mut()
        else {
            unreachable!()
        };
        *namespace = "78".repeat(32);
    }
    if state["routing"] == 0 {
        cmd.env[1].1 = at!("/model/wrong").into();
    }
    if state["stored"] == 0 {
        cmd.account_intent = Some(Box::new(ExecutionIntent::external()));
    }
    if state["fresh"] == 0 {
        next.id.clone_from(&old.id);
    }
    if state["canonical"] == 0 {
        next.id = "invalid".into();
    }
    (old, next)
}

pub(super) fn transition(state: &State, action: &str) -> State {
    let mut next = state.clone();
    if action == "Check" {
        let (old, candidate) = fixture(state);
        next.insert("checked", 1);
        next.insert(
            "accepted",
            i64::from(old.account_successor_recipe(&candidate, ImagePhase::Forward)),
        );
    } else {
        let field = match action {
            "ChangeContext" => "context",
            "ChangeNative" => "native",
            "ChangeCommand" => "command",
            "ExternalAccount" => "stored",
            "ChangeNamespace" => "namespace",
            "MismatchRouting" => "routing",
            "ReuseOperation" => "fresh",
            "InvalidEncoding" => "canonical",
            _ => unreachable!(),
        };
        next.insert(field, 0);
    }
    next
}

#[test]
fn recipe_admission_refines_the_model_for_every_input_combination() {
    let model = aterm_models_harness::native_account_successor_recipe_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
}

#[test]
fn replacing_the_baseline_with_the_candidate_breaks_the_model() {
    let model = aterm_models_harness::native_account_successor_recipe_model();
    let mut state = model.init_state();
    assert!(model.fire("ChangeCommand", &mut state));
    let (mut old, candidate) = fixture(&state);
    forward(&mut old).argv.clone_from(
        &active_command(&candidate.plan, ImagePhase::Forward)
            .unwrap()
            .argv,
    );
    assert!(old.account_successor_recipe(&candidate, ImagePhase::Forward));
    state.insert("checked", 1);
    state.insert("accepted", 1);
    assert!(!model.check_invariant("CommandPreserved", &state));
}

#[test]
fn default_account_removal_and_rollback_preserve_the_other_command() {
    let (old, mut next) = pair();
    let cmd = forward(&mut next);
    cmd.env.retain(|(name, _)| name != LOGIN_ENV);
    cmd.unset = Some(vec![LOGIN_ENV.into()]);
    cmd.account_intent = Some(stored(None, "56"));
    assert!(old.account_successor_recipe(&next, ImagePhase::Forward));
    let mut rollback = old.clone();
    rollback.id.clone_from(&next.id);
    if let (
        NativePlan::Restart {
            rollback: Some(back),
            ..
        },
        NativePlan::Restart { forward, .. },
    ) = (&mut rollback.plan, &next.plan)
    {
        *back = forward.clone();
    }
    assert!(old.account_successor_recipe(&rollback, ImagePhase::Rollback));
    assert!(!old.account_successor_recipe(&rollback, ImagePhase::Forward));
    if let NativePlan::Restart {
        rollback: Some(back),
        ..
    } = &mut next.plan
    {
        back.argv.push("--changed".into());
    }
    assert!(!old.account_successor_recipe(&next, ImagePhase::Forward));
}

#[test]
fn only_the_account_selector_may_change_in_the_environment() {
    let (old, next) = pair();
    for defect in 0..6 {
        let mut bad = next.clone();
        let cmd = forward(&mut bad);
        match defect {
            0 => cmd.env[0].1 = "changed".into(),
            1 => cmd.env.push((LOGIN_ENV.into(), at!("/model/new").into())),
            2 => cmd.unset = Some(vec![LOGIN_ENV.into()]),
            3 => cmd.unset = Some(vec!["KEEP".into()]),
            4 => cmd.account_intent = None,
            5 => cmd.unset = Some(Vec::new()),
            _ => unreachable!(),
        }
        assert!(
            !old.account_successor_recipe(&bad, ImagePhase::Forward),
            "defect {defect}"
        );
    }
}

#[test]
fn execution_identity_and_non_account_fields_are_frozen() {
    let (old, next) = pair();
    for defect in 0..12 {
        let mut bad = next.clone();
        match defect {
            0 => bad.id.clone_from(&old.id),
            1 => bad.host_epoch = "00".repeat(16),
            2 => bad.holder = "other-holder".into(),
            3 => bad.duty = Scope::Upgrade,
            4 => {
                let NativePlan::Restart { process, .. } = &mut bad.plan else {
                    unreachable!()
                };
                process.birth += 1;
            }
            5 => {
                let NativePlan::Restart { shell, .. } = &mut bad.plan else {
                    unreachable!()
                };
                shell.birth += 1;
            }
            6 => {
                let NativePlan::Restart { launcher, .. } = &mut bad.plan else {
                    unreachable!()
                };
                launcher.inode += 1;
            }
            7 => forward(&mut bad).image.inode += 1,
            8 => forward(&mut bad).program = at!("/model/other-agent").into(),
            9 => forward(&mut bad).cwd = Some(at!("/model/other-project").into()),
            10 => forward(&mut bad).heal = Some(at!("/model/other-hook").into()),
            11 => forward(&mut bad).dialect = Dialect::Bash,
            _ => unreachable!(),
        }
        assert!(
            !old.account_successor_recipe(&bad, ImagePhase::Forward),
            "defect {defect}"
        );
    }
}

#[test]
fn an_old_implicit_default_does_not_allow_a_new_implicit_default() {
    let (mut old, mut next) = pair();
    let previous = forward(&mut old);
    previous.env.retain(|(name, _)| name != LOGIN_ENV);
    previous.account_intent = Some(stored(None, "34"));
    assert!(old.account_successor_recipe(&next, ImagePhase::Forward));
    let candidate = forward(&mut next);
    candidate.env.retain(|(name, _)| name != LOGIN_ENV);
    candidate.account_intent = Some(stored(None, "56"));
    assert!(!old.account_successor_recipe(&next, ImagePhase::Forward));
    forward(&mut next).unset = Some(vec![LOGIN_ENV.into()]);
    assert!(old.account_successor_recipe(&next, ImagePhase::Forward));
    forward(&mut old)
        .env
        .push((LOGIN_ENV.into(), String::new()));
    assert!(old.account_successor_recipe(&next, ImagePhase::Forward));
}

#[test]
fn old_selector_spelling_is_not_reauthorized_but_the_new_one_is_exact() {
    let (mut old, mut next) = pair();
    forward(&mut old).env[1].1 = at!("/model/old-alias").into();
    assert!(old.account_successor_recipe(&next, ImagePhase::Forward));
    forward(&mut next).env[1].1 = at!("/model/new-alias").into();
    assert!(!old.account_successor_recipe(&next, ImagePhase::Forward));
}

#[test]
fn a_fresh_attempt_may_keep_the_same_account_but_cannot_reuse_the_operation() {
    let (old, _) = pair();
    let mut next = old.clone();
    next.id = "ef".repeat(16);
    next.deadline_us = 200;
    assert!(old.account_successor_recipe(&next, ImagePhase::Forward));
    next.id.clone_from(&old.id);
    assert!(!old.account_successor_recipe(&next, ImagePhase::Forward));
}

#[test]
fn canonical_builder_preserves_a_three_link_account_chain() {
    let (old, _) = pair();
    let original = old.clone();
    let default = old
        .with_account_successor(
            ImagePhase::Forward,
            "01".repeat(16),
            2,
            1,
            200,
            *stored(None, "56"),
        )
        .unwrap();
    assert_eq!(
        forward(&mut default.clone()).unset,
        Some(vec![LOGIN_ENV.into()])
    );
    let named = default
        .with_account_successor(
            ImagePhase::Forward,
            "02".repeat(16),
            3,
            2,
            300,
            *stored(Some(at!("/model/third")), "78"),
        )
        .unwrap();
    assert_eq!(forward(&mut named.clone()).unset, None);
    let retry = named
        .with_account_successor(
            ImagePhase::Forward,
            "03".repeat(16),
            4,
            3,
            400,
            *stored(Some(at!("/model/third")), "78"),
        )
        .unwrap();
    for (a, b) in [(&old, &default), (&default, &named), (&named, &retry)] {
        assert!(a.account_successor_recipe(b, ImagePhase::Forward));
        assert_eq!(
            active_command(&a.plan, ImagePhase::Rollback),
            active_command(&b.plan, ImagePhase::Rollback)
        );
    }
    assert_eq!(
        old, original,
        "building successors never mutates the retained predecessor"
    );
}

#[test]
fn builder_refusals_leave_the_predecessor_untouched() {
    let (mut old, _) = pair();
    let NativePlan::Restart { rollback, .. } = &mut old.plan else {
        unreachable!()
    };
    *rollback = None;
    let original = old.clone();
    assert!(
        old.with_account_successor(
            ImagePhase::Rollback,
            "01".repeat(16),
            2,
            1,
            200,
            *stored(Some(at!("/model/new")), "56"),
        )
        .is_none()
    );
    assert!(
        old.with_account_successor(
            ImagePhase::Forward,
            old.id.clone(),
            2,
            1,
            200,
            *stored(Some(at!("/model/new")), "56"),
        )
        .is_none()
    );
    assert!(
        old.with_account_successor(
            ImagePhase::Forward,
            "01".repeat(16),
            2,
            1,
            200,
            ExecutionIntent::external(),
        )
        .is_none()
    );
    assert!(
        old.with_account_successor(
            ImagePhase::Forward,
            "01".repeat(16),
            2,
            1,
            200,
            *stored(
                Some(&"x".repeat(crate::harness::native_act::MAX_RESUME + 1)),
                "56"
            ),
        )
        .is_none()
    );
    assert_eq!(old, original);
}
