// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::accounts_sync::{self, FleetRoster, MANUAL_EARLY_FLEET, OptIn};
use crate::harness::alab_roster::{SourceSnapshot, TestSourceOverride};
use aterm_spec::interp::State;
use std::time::Duration;

struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        accounts::SYNC_COMMIT_CUT.set(None);
        accounts::SYNC_COMMIT_REFUSE.set(false);
        MANUAL_EARLY_FLEET.set(false);
    }
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    transition_with(state, action, false)
}

fn transition_with(state: &State, action: &str, early: bool) -> State {
    let mut next = state.clone();
    if action != "Commit" {
        let field = match action {
            "Operation" => "operation",
            "Source" => "legacy",
            "Cut" => "cut",
            _ => unreachable!(),
        };
        next.insert(field, state[field] + 1);
        return next;
    }
    let _registry = TestSourceOverride::new(SourceSnapshot::Absent);
    let tmp = tmp_dir("manual-receipt");
    let env = env_at(&tmp);
    crate::test_config::write(env.config.as_ref().unwrap(), "[fleet]\nmachine='m3'\n").unwrap();
    let store = env.accounts_store();
    store
        .transact_logins(env.now, &mut None, |_, _| Ok(Vec::new()))
        .unwrap();
    let roster = env.accounts_roster().unwrap();
    let fleet = roster.with_file_name(accounts_sync::FLEET_ROSTER_FILE);
    let text = FleetRoster::from_toml(
        "[[account]]\nlabel='old'\nemail='old@example.test'\nmachines=['m3']\n",
        "fixture",
    )
    .unwrap()
    .to_toml_for(Some(OptIn::Discovered));
    std::fs::write(&fleet, &text).unwrap();
    let input = env
        .home
        .as_ref()
        .unwrap()
        .join(".secrets")
        .join(accounts_sync::FLEET_ROSTER_FILE);
    if state["legacy"] == 1 {
        std::fs::create_dir_all(input.parent().unwrap()).unwrap();
        std::fs::write(&input, &text).unwrap();
        let future = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(4_000_000_000);
        for path in [&input, &fleet] {
            std::fs::File::open(path)
                .unwrap()
                .set_modified(future)
                .unwrap();
        }
    }
    let before_input = std::fs::read(&input).ok();
    let paths = [store.state_file(), store.logins_file(), store.ledger_path()];
    let before_records = paths.each_ref().map(|path| std::fs::read(path).ok());
    let before_roster = std::fs::read(&roster).ok();
    let before_fleet = std::fs::read(&fleet).ok();
    let _restore = Restore;
    accounts::SYNC_COMMIT_REFUSE.set(state["cut"] == 0);
    accounts::SYNC_COMMIT_CUT.set(match state["cut"] {
        1 => Some(0),
        2 => Some(5),
        _ => None,
    });
    MANUAL_EARLY_FLEET.set(early);
    let row = accounts::Row {
        label: "m3.2".into(),
        email: Some("new@example.test".into()),
        class: Class::Preferred,
        ..accounts::Row::default()
    };
    let success = if state["operation"] == 0 {
        accounts_add(Cli::Claude, &env, &row, &|| true, &mut |_| {}).is_ok()
    } else {
        let host = FakeHost::new(&|_| panic!("metadata must not authenticate"));
        run_accounts_adopt(
            Cli::Claude,
            &env,
            &host,
            &row,
            None,
            &mut Vec::new(),
            &mut Vec::new(),
        ) == ExitCode::SUCCESS
    };
    accounts::SYNC_COMMIT_REFUSE.set(false);
    accounts::SYNC_COMMIT_CUT.set(None);
    MANUAL_EARLY_FLEET.set(false);
    let pending = store.sync_needs_recovery();
    let durable = success || pending;
    let intact = durable
        || (std::fs::read(&roster).ok() == before_roster
            && std::fs::read(&fleet).ok() == before_fleet);
    if pending {
        assert!(store.recover_sync_within(&[], &roster).unwrap());
    }
    let complete = Roster::load(&roster).is_ok_and(|roster| roster.row(&row.label).is_some())
        && FleetRoster::from_toml(&std::fs::read_to_string(&fleet).unwrap(), "projection")
            .unwrap()
            .accounts
            .iter()
            .any(|account| account.label == row.label);
    next.insert("done", 1);
    next.insert("durable", i64::from(durable));
    next.insert("pending", i64::from(pending));
    next.insert("intact", i64::from(intact));
    next.insert(
        "inputs",
        i64::from(std::fs::read(input).ok() == before_input),
    );
    next.insert(
        "records",
        i64::from(paths.each_ref().map(|path| std::fs::read(path).ok()) == before_records),
    );
    next.insert("complete", i64::from(complete));
    next.insert(
        "retired",
        i64::from(store.source_policy().unwrap().retired()),
    );
    next
}

#[test]
fn manual_add_and_adopt_refine_the_same_durable_receipt() {
    let model = aterm_models_harness::account_manual_receipt_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    let mut cases = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
                cases += usize::from(action.name == "Commit");
            }
        }
    }
    assert_eq!(cases, 16);
    for operation in 0..2 {
        let mut state = model.init_state();
        state.insert("operation", operation);
        let bad = transition_with(&state, "Commit", true);
        assert!(!model.check_invariant("BeforeIntentIsClean", &bad));
    }
}

fn planned(already_listed: bool) -> (Tmp, Env, accounts::Row, accounts_sync::ManualRosterPlan) {
    let tmp = tmp_dir("manual-stale-plan");
    let env = env_at(&tmp);
    crate::test_config::write(env.config.as_ref().unwrap(), "[fleet]\nmachine='m3'\n").unwrap();
    let row = accounts::Row {
        label: "new".into(),
        email: Some("new@example.test".into()),
        class: Class::Preferred,
        ..accounts::Row::default()
    };
    let text = if already_listed {
        "opt_in='discovered'\n[[account]]\nlabel='new'\nemail='new@example.test'\nmachines=['m3']\n"
    } else {
        "opt_in='discovered'\n[[account]]\nlabel='old'\nemail='old@example.test'\nmachines=['m3']\n"
    };
    let roster = env.accounts_roster().unwrap();
    std::fs::write(
        roster.with_file_name(accounts_sync::FLEET_ROSTER_FILE),
        text,
    )
    .unwrap();
    let plan = accounts_sync::ManualRosterPlan::capture(Cli::Claude, &env, &roster, &row).unwrap();
    (tmp, env, row, plan)
}

fn publish_plan(
    env: &Env,
    row: &accounts::Row,
    plan: &accounts_sync::ManualRosterPlan,
) -> Result<bool, String> {
    let store = env.accounts_store();
    let held = store.acquire_bounded().unwrap().unwrap();
    let logical = env.accounts_roster().unwrap();
    let (target, mode) = roster_target(&logical)?;
    let before = accounts::roster_text(&target)?;
    let after = accounts::append_rows_for(
        Cli::Claude,
        before.as_deref().unwrap_or(""),
        std::slice::from_ref(row),
    )?;
    store
        .transact_roster_prepared_held(
            &held,
            env.now,
            &mut None,
            accounts::SyncTarget {
                path: &target,
                before: before.as_deref(),
                after: Some(&after),
                mode,
                fleet: plan.fleet.as_ref().map(|edit| edit.target(&logical)),
            },
            plan.retires_legacy(),
            (plan.guard(), || plan.check_current()),
        )
        .map_err(SyncError::into_message)
}

#[test]
fn a_manual_plan_preserves_a_concurrent_fleet_edit_even_for_an_already_listed_row() {
    let _registry = TestSourceOverride::new(SourceSnapshot::Absent);
    for already in [false, true] {
        let (_control_tmp, control_env, control_row, control_plan) = planned(already);
        assert!(publish_plan(&control_env, &control_row, &control_plan).unwrap());
        let (_tmp, env, row, plan) = planned(already);
        assert_eq!(plan.fleet.as_ref().unwrap().changed(), !already);
        let fleet = env
            .accounts_roster()
            .unwrap()
            .with_file_name(accounts_sync::FLEET_ROSTER_FILE);
        let concurrent = "opt_in='discovered'\n[[account]]\nlabel='concurrent'\nemail='concurrent@example.test'\nmachines=['m3']\n";
        std::fs::write(&fleet, concurrent).unwrap();
        assert!(publish_plan(&env, &row, &plan).is_err());
        assert_eq!(std::fs::read_to_string(fleet).unwrap(), concurrent);
        assert!(!env.accounts_roster().unwrap().exists());
        assert!(!env.accounts_store().sync_needs_recovery());
    }
}

#[test]
fn a_manual_plan_rechecks_configuration_and_higher_priority_inputs_before_its_receipt() {
    let _registry = TestSourceOverride::new(SourceSnapshot::Absent);
    for config_changed in [false, true] {
        let (_tmp, env, row, plan) = planned(false);
        let roster = env.accounts_roster().unwrap();
        let fleet = roster.with_file_name(accounts_sync::FLEET_ROSTER_FILE);
        let before = std::fs::read(&fleet).unwrap();
        if config_changed {
            crate::test_config::write(env.config.as_ref().unwrap(), "[fleet]\nmachine='m4'\n")
                .unwrap();
        } else {
            let source = env
                .home
                .as_ref()
                .unwrap()
                .join(".secrets")
                .join(accounts_sync::FLEET_ROSTER_FILE);
            std::fs::create_dir_all(source.parent().unwrap()).unwrap();
            std::fs::write(
                source,
                "[[account]]\nlabel='remote'\nemail='remote@example.test'\nmachines=['m3']\n",
            )
            .unwrap();
        }
        assert!(publish_plan(&env, &row, &plan).is_err());
        assert_eq!(std::fs::read(fleet).unwrap(), before);
        assert!(!roster.exists());
        assert!(!env.accounts_store().sync_needs_recovery());
    }
}

#[cfg(unix)]
#[test]
fn a_manual_plan_rejects_parent_retargeting_with_an_unchanged_canonical_roster() {
    let _registry = TestSourceOverride::new(SourceSnapshot::Absent);
    let (tmp, mut env, row, _) = planned(false);
    let shared = tmp.path().join("shared-roster.toml");
    std::fs::write(&shared, "").unwrap();
    let first = tmp.path().join("first");
    let second = tmp.path().join("second");
    let fleet_text = std::fs::read(
        env.accounts_roster()
            .unwrap()
            .with_file_name(accounts_sync::FLEET_ROSTER_FILE),
    )
    .unwrap();
    for dir in [&first, &second] {
        std::fs::create_dir(dir).unwrap();
        std::fs::write(dir.join("aterm.toml"), "[fleet]\nmachine='m3'\n").unwrap();
        std::fs::write(dir.join(accounts_sync::FLEET_ROSTER_FILE), &fleet_text).unwrap();
        std::os::unix::fs::symlink(&shared, dir.join(accounts::ACCOUNTS_FILE)).unwrap();
    }
    let view = tmp.path().join("view");
    std::os::unix::fs::symlink(&first, &view).unwrap();
    env.config = Some(view.join("aterm.toml"));
    let plan = accounts_sync::ManualRosterPlan::capture(
        Cli::Claude,
        &env,
        &env.accounts_roster().unwrap(),
        &row,
    )
    .unwrap();
    std::fs::remove_file(&view).unwrap();
    std::os::unix::fs::symlink(&second, &view).unwrap();
    assert!(publish_plan(&env, &row, &plan).is_err());
    assert_eq!(std::fs::read(shared).unwrap(), b"");
    for dir in [&first, &second] {
        assert_eq!(
            std::fs::read(dir.join(accounts_sync::FLEET_ROSTER_FILE)).unwrap(),
            fleet_text
        );
    }
    assert!(!env.accounts_store().sync_needs_recovery());
}

#[cfg(unix)]
#[test]
fn explicitly_admitted_native_add_still_protects_source_outputs() {
    let _registry = TestSourceOverride::new(SourceSnapshot::Absent);
    for alias in [false, true] {
        let tmp = tmp_dir("native-add-source-alias");
        let env = env_at(&tmp);
        crate::test_config::write(env.config.as_ref().unwrap(), "[fleet]\nmachine='m3'\n").unwrap();
        let input = env
            .home
            .as_ref()
            .unwrap()
            .join(".secrets")
            .join(accounts_sync::FLEET_ROSTER_FILE);
        std::fs::create_dir_all(input.parent().unwrap()).unwrap();
        std::fs::write(&input, "account=[]\n").unwrap();
        let roster = env.accounts_roster().unwrap();
        if alias {
            std::os::unix::fs::symlink(&input, &roster).unwrap();
        }
        let captured = crate::harness::alab_roster::capture_for(env.config.as_ref().unwrap());
        let row = accounts::Row {
            label: "new".into(),
            email: Some("new@example.test".into()),
            ..accounts::Row::default()
        };
        let result = accounts_add_admitted(
            Cli::Claude,
            &env,
            &row,
            &|| true,
            &mut |_| {},
            Some(captured.native_guard().unwrap()),
        );
        assert_eq!(result.is_ok(), !alias);
        assert_eq!(std::fs::read_to_string(&input).unwrap(), "account=[]\n");
        assert!(!env.accounts_store().sync_needs_recovery());
    }
}
