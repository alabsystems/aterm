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
        boot: None,
        custody: None,
    })
    .encode()
    .expect("a canonical retained preparation");
}

/// Make the retained preparation a cold crash restart, one of the kinds a later host epoch can
/// answer `lapsed`: the same id and recipe, a relaunch duty and an owned-helper cold intent.
pub(in crate::harness) fn retain_cold(state: &mut St) {
    let Some(Request::Prepare(mut prepare)) = Request::parse(&state.native_prepare) else {
        panic!("the fixture keeps a preparation")
    };
    prepare.duty = Scope::Relaunch;
    prepare.asked = None;
    let NativePlan::Restart { cold, .. } = &mut prepare.plan else {
        panic!("a restart plan")
    };
    *cold = Some(Box::new(native_act::ColdIntent {
        login: None,
        home: None,
        auth_observed: true,
        route: crate::harness::accounts::AuthRoute::Stored,
        conversation: None,
        profile: None,
        mode: crate::harness::accounts::Mode::Wall,
        billing: native_act::ColdBilling::Keep,
    }));
    state.native_prepare = Request::prepare(*prepare)
        .encode()
        .expect("a canonical cold preparation");
}

/// Make the retained preparation a typed asked restart (`ask` `None`: an Upgrade; `Some`: a
/// checked Memory relaunch with that ask word), the other kind a later host epoch can answer
/// `lapsed`.
pub(in crate::harness) fn retain_asked(state: &mut St, ask: Option<&str>) {
    let Some(Request::Prepare(mut prepare)) = Request::parse(&state.native_prepare) else {
        panic!("the fixture keeps a preparation")
    };
    let predecessor = "cd".repeat(16);
    let NativePlan::Restart {
        cold, observation, ..
    } = &mut prepare.plan
    else {
        panic!("a restart plan")
    };
    *cold = None;
    *observation = None;
    (prepare.duty, prepare.asked) = match ask {
        None => (
            Scope::Upgrade,
            Some(AskedIntent::Upgrade(AskedUpgradeIntent { predecessor })),
        ),
        Some(ask) => (
            Scope::Relaunch,
            Some(AskedIntent::Relaunch(native_act::AskedRelaunchIntent {
                predecessor,
                ask: ask.to_owned(),
            })),
        ),
    };
    assert!(prepare.asked_restart_shape(), "a supported asked shape");
    state.native_prepare = Request::prepare(*prepare)
        .encode()
        .expect("a canonical asked preparation");
}
