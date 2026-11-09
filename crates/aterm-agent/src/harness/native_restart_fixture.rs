// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Canonical requests for transport fixtures. Kernel observations are supplied
//! separately by each test; these words do not grant a real native operation.

use super::*;

pub(in crate::harness) fn retain_bound_preparation(state: &mut St) {
    let binding = state.native.as_ref().expect("the fixture has a binding");
    let process = Process::observe(std::process::id()).expect("the test process");
    let command = native_act::RestartCommand {
        image: process.image.clone(),
        program: std::env::current_exe()
            .unwrap()
            .into_os_string()
            .into_string()
            .unwrap(),
        argv: Vec::new(),
        env: Vec::new(),
        unset: None,
        cwd: None,
        heal: None,
        dialect: crate::harness::upgrade::Dialect::Zsh,
        settles: None,
        account_intent: state.account_intent.clone().map(Box::new),
    };
    state.native_prepare = Request::prepare(Prepare {
        rejection_domain: None,
        id: binding.id.clone(),
        host_epoch: "ef".repeat(16),
        holder: "fixture-holder".into(),
        revision: 1,
        expected_human_seq: Some(0),
        deadline_us: 100,
        duty: Scope::Relaunch,
        asked: None,
        plan: NativePlan::Restart {
            process: process.clone(),
            shell: process.clone(),
            launcher: process.image,
            socket: "/fixture/native.sock".into(),
            forward: Box::new(command.clone()),
            rollback: Some(Box::new(command)),
            observation: None,
            cold: None,
        },
    })
    .encode()
    .expect("a canonical retained preparation");
}
