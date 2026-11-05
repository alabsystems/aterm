// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::restore::AgentRestore;
use aterm_agent::harness::relaunch::{AgentRun, Snapshot};

fn original(case: i64) -> Snapshot {
    let profile = (case > 2).then(|| Context {
        plain: "/fixture/.claude.json".into(),
        records: "/fixture/.claude".into(),
        selection: match case {
            5 => Selection::CustomOauth,
            6 => Selection::Unknown,
            _ => Selection::Standard,
        },
        home: (case > 3).then(|| if case == 4 { "" } else { "/fixture" }.into()),
        config: (case == 4).then(String::new),
        login: (case > 3).then(|| if case == 4 { "" } else { "/fixture/login" }.into()),
    });
    let stored_login = case != 2 && case != 5 && case != 0;
    // Case 7 is a stored login an older build captured, with no launch record.
    let launch_env = aterm_agent::harness::accounts::LaunchEnv::of_exec_env(&[
        "HOME=/fixture".into(),
        "PATH=/usr/bin".into(),
        format!(
            "{}=/fixture/login",
            aterm_agent::harness::accounts::LOGIN_ENV
        ),
    ])
    .filter(|_| stored_login && case != 7);
    Snapshot {
        launch_env,
        tab: "s-restore-auth".into(),
        pid: 42,
        start: "Thu Nov 05 01:00:00 2026".into(),
        shell: 41,
        program: "/fixture/claude".into(),
        argv: vec!["/fixture/claude".into()],
        session: Some("0badf00d-1111-2222-3333-444455556666".into()),
        cwd: "/fixture/work".into(),
        version: None,
        run: AgentRun::Claude,
        dialect: None,
        login: Some("/fixture/login".into()),
        home: None,
        claude_dir: None,
        stored_login,
        auth_observed: case != 0,
        profile,
    }
}

fn restore_case(case: i64) -> [i64; 3] {
    let before = original(case);
    let mut record = AgentRestore::of(before.clone());
    if case == 8 {
        record.word = "codex".into();
    }
    let json = aterm_json::to_vec(&record).unwrap();
    let toml = aterm_toml::to_string(&record).unwrap();
    let reads = [
        aterm_json::from_slice::<AgentRestore>(&json).ok(),
        aterm_toml::from_str::<AgentRestore>(&toml).ok(),
    ];
    let mut preserved = true;
    let mut synthesized = false;
    let mut accepted = true;
    for read in reads {
        // A mixed-provider record must fail decoding itself. A later refusal to
        // construct a snapshot must not conceal a decoder that admitted it.
        let decoded = read.is_some();
        let restored = read.and_then(|record| record.snapshot(before.tab.clone(), before.shell));
        accepted &= decoded && (case == 8 || restored.is_some());
        if let Some(after) = restored {
            preserved &= before.stored_login == after.stored_login
                && before.auth_observed == after.auth_observed
                && before.profile == after.profile
                && before.login == after.login
                && before.home == after.home
                && before.launch_env == after.launch_env;
            if case == 0 {
                synthesized |= AgentRestore::of(after).claude_auth.is_some();
            }
        }
    }
    [
        i64::from(preserved),
        i64::from(synthesized),
        i64::from(accepted),
    ]
}

pub(crate) fn restore_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    if action == "NextCase" {
        next.insert("case", state["case"] + 1);
    } else {
        let [preserved, synthesized, accepted] = restore_case(state["case"]);
        next.insert("preserved", preserved);
        next.insert("synthesized", synthesized);
        next.insert("accepted", accepted);
        next.insert("phase", 1);
    }
    next
}

#[test]
fn claude_restore_auth_conforms_without_inventing_or_dropping_attribution() {
    let model = aterm_models_harness::claude_restore_auth_model();
    aterm_spec::interp::prove_and_catch(&model);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = restore_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
            }
        }
    }
    let mut bad = model.init_state();
    bad = restore_transition(&bad, "Restore");
    bad.insert("synthesized", 1);
    assert!(!model.check_invariant("NoInventedEvidence", &bad));
    bad.insert("preserved", 0);
    assert!(!model.check_invariant("CapturedAttribution", &bad));
}
