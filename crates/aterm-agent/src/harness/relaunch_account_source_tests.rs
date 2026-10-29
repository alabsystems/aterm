// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn source() -> Snapshot {
    Snapshot {
        tab: "s-selected-source".into(),
        pid: 4242,
        start: "source-birth".into(),
        shell: 4241,
        program: "/example/claude".into(),
        argv: vec!["/example/claude".into()],
        session: Some("0badf00d-1111-2222-3333-444455556666".into()),
        cwd: "/example".into(),
        version: None,
        run: AgentRun::Claude,
        dialect: None,
        login: None,
        home: None,
        claude_dir: None,
        stored_login: true,
        profile: None,
    }
}

fn roster() -> accounts::Roster {
    accounts::Roster {
        rows: vec![
            accounts::Row {
                label: "selected".into(),
                dir: Some("/example/login".into()),
                ..Default::default()
            },
            accounts::Row {
                label: "other".into(),
                ..Default::default()
            },
        ],
    }
}

fn why() -> Restart {
    Restart::Account {
        to: "selected".into(),
        at: crate::supervise::policy::turn_end::AccountAt::Wall,
    }
}

fn selection_case(case: i64) -> bool {
    let original = source();
    let target = roster();
    let selected = AccountRestart::new(&why(), &original, &target.rows[0], move || case != 25);
    let mut fresh = original;
    let mut rows = target;
    let mut asked = why();
    match case {
        1 | 25 | 27 | 28 => {}
        2 => fresh.pid += 1,
        3 => fresh.start.push_str("-new"),
        4 => fresh.argv.push("--new".into()),
        5 => fresh.login = Some("/other/login".into()),
        6 => fresh.home = Some("/other/home".into()),
        7 => {
            fresh.profile = Some(super::super::super::profile::Context {
                plain: "/other/config".into(),
                records: "/other/records".into(),
                selection: super::super::super::profile::Selection::Standard,
                home: None,
                config: None,
                login: None,
            })
        }
        8 => fresh.program = "/other/program".into(),
        9 => fresh.tab.push_str("-new"),
        10 => {
            fresh.run = AgentRun::Codex(CodexRun {
                home: "/other/codex".into(),
                home_env: None,
                self_claimed: None,
            })
        }
        11 => fresh.claude_dir = Some("/other/records".into()),
        12 => fresh.stored_login = false,
        13 => rows.rows[0].dir = Some("/other/login".into()),
        14 => rows.rows[0].email = Some("other@example.test".into()),
        15 => rows.rows[0].identity_email = Some("other@example.test".into()),
        16 => rows.rows[0].class = accounts::Class::Shared,
        17 => rows.rows[0].browser = Some("another-browser".into()),
        18 => rows.rows[0].was.push(Some("/prior/login".into())),
        19 => rows.rows[0].vendor.flow = Some(crate::ai::connector::Flow::Device),
        20 => {
            rows.rows.remove(0);
        }
        21 => {
            asked = Restart::Account {
                to: "other".into(),
                at: crate::supervise::policy::turn_end::AccountAt::Wall,
            }
        }
        22 | 29 => asked = Restart::Memory,
        23 => {
            fresh.cwd = "/new/work".into();
            fresh.session = Some("0badf00d-7777-2222-3333-444455556666".into());
            fresh.version = Some("new-version".into());
            fresh.shell += 1;
            fresh.dialect = Some(Dialect::Bash);
        }
        24 => rows.rows[1].email = Some("other@example.test".into()),
        26 => {
            asked = Restart::Account {
                to: "selected".into(),
                at: crate::supervise::policy::turn_end::AccountAt::Ahead,
            }
        }
        _ => unreachable!(),
    }
    let observed = if case == 28 {
        Err("unreadable")
    } else {
        Ok(fresh)
    };
    restart_admitted(
        &asked,
        &rows,
        &observed,
        (!matches!(case, 27 | 29)).then_some(&selected),
    )
}

pub(crate) fn selection_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherCase" => {
            next.insert("case", state["case"] + 1);
        }
        "Choose" => {
            next.insert("phase", 1);
        }
        "Observe" => {
            next.insert("phase", 2);
            next.insert("admitted", i64::from(selection_case(state["case"])));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn account_restarts_bind_source_target_reason_and_generation() {
    let model = aterm_models_harness::account_restart_source_model();
    assert_eq!(model.mutant_dials(), (1..=29).collect::<Vec<_>>());
    assert!(aterm_spec::interp::prove_and_catch(&model) >= 29);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = selection_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for (case, admitted, law) in [
        (2, 1, "SelectedContext"),
        (23, 0, "Progress"),
        (27, 1, "RequiredSelection"),
    ] {
        let mut state = model.init_state();
        state.insert("phase", 1);
        state.insert("case", case);
        let mut bad = selection_transition(&state, "Observe");
        bad.insert("admitted", admitted);
        assert!(!model.check_invariant(law, &bad));
    }
}

#[test]
fn account_admission_refuses_before_any_filesystem_work() {
    let dir = crate::supervise::ScratchDir::new("relaunch", "account-source");
    let opts = Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: None,
        token: None,
        only_sid: Some(source().tab),
        dry_run: false,
        human_grace_s: 0,
        hand_back: true,
        background: false,
        aterm_state: None,
        ask: false,
        several_logins: false,
        usage_credits: false,
        instances: super::super::super::upgrade_drive::Instances::Listed(Vec::new()),
        usage_tap: None,
        agents_off: Vec::new(),
        land: false,
        press: None,
        timings: crate::harness::Timings::FAST,
    };
    let rows = roster();
    for case in 0..4 {
        let selected = AccountRestart::new(&why(), &source(), &rows.rows[0], move || case != 3);
        let mut fresh = source();
        if case == 1 {
            fresh.pid += 1;
        }
        let mut current_rows = rows.clone();
        if case == 2 {
            current_rows.rows[0].dir = Some("/other/login".into());
        }
        let report = restart_as(
            &opts,
            &why(),
            &current_rows,
            Ok(fresh),
            (case != 0).then_some(&selected),
            &Live,
        );
        assert_eq!(report.step, "wait:account-changed", "case {case}");
        assert!(!opts.state.exists());
        assert!(!opts.home.exists());
    }
}
