// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::alab_roster::{SourceSnapshot, TestSourceOverride};
use aterm_spec::interp::State;

fn resolve_scope(state: &State, action: &str, promote: bool) -> State {
    let mut next = state.clone();
    if action != "Select" {
        let key = match action {
            "Automatic" => "init",
            "Key" => "key",
            "Named" => "named",
            "Beside" => "beside",
            "Pool" => "pool",
            "Vault" => "vault",
            "Sheet" => "sheet",
            "Retire" => "retired",
            _ => panic!("unknown selection action"),
        };
        next.insert(key, 1);
        return next;
    }
    let _registry = TestSourceOverride::new(SourceSnapshot::Absent);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path();
    let config = home.join("aterm.toml");
    let roster = home.join("accounts.toml");
    let secrets = home.join(".secrets");
    fs::create_dir(&secrets).unwrap();
    fs::write(
        &config,
        if state["key"] == 1 {
            "[fleet]\naccounts_source='chosen.csv'\n"
        } else {
            ""
        },
    )
    .unwrap();
    if state["named"] == 1 || state["beside"] == 1 {
        fs::write(
            home.join(FLEET_ROSTER_FILE),
            if state["named"] == 1 {
                "opt_in='named'\n"
            } else {
                ""
            },
        )
        .unwrap();
    }
    for (key, name) in [
        ("pool", super::super::super::ai_pool::AI_POOL_FILE),
        ("vault", FLEET_ROSTER_FILE),
        ("sheet", ACCOUNTS_CSV_FILE),
    ] {
        if state[key] == 1 {
            // Unusable content still selects this entry; selection must not fall through.
            fs::write(secrets.join(name), "unusable source").unwrap();
        }
    }
    let scope = if state["init"] == 1 || promote {
        Scope::Init
    } else {
        Scope::Configured
    };
    let store = accounts::Store::at(&home.join("state"));
    if state["retired"] == 1 {
        let held = store.acquire().unwrap();
        store.retire_source_held(&held, 1).unwrap();
    }
    let selected = select_for(&store, Some(home), Some(&config), Some(&roster), scope).unwrap();
    let slot = match selected {
        Selection::Absent => 0,
        Selection::Key(_) => 1,
        Selection::Pool(_) => 2,
        Selection::VaultFleet { .. } => 3,
        Selection::Sheet { .. } => 4,
        Selection::Beside { .. } => 5,
    };
    next.insert("selected", slot);
    next.insert("phase", 1);
    next
}

pub(crate) fn resolve(state: &State, action: &str) -> State {
    resolve_scope(state, action, false)
}

#[test]
fn source_selection_conforms_and_rejects_promoted_manual_scope() {
    let model = aterm_models_harness::account_source_selection_model();
    aterm_spec::interp::prove_and_catch(&model);
    let mut selections = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                assert!(
                    model
                        .successors(action.name, &state)
                        .contains(&resolve(&state, action.name)),
                    "{state:?}"
                );
                selections += usize::from(action.name == "Select");
            }
        }
    }
    assert_eq!(selections, 256);
    let mut manual = model.init_state();
    manual.insert("vault", 1);
    let promoted = resolve_scope(&manual, "Select", true);
    assert!(!model.successors("Select", &manual).contains(&promoted));
    assert!(!model.check_invariant("SelectionMatchesPolicy", &promoted));
}

#[test]
fn assignment_and_missing_roster_read_the_selected_source_instead_of_its_copy() {
    use super::super::login_tests::{Desk, NOW, upkeep_effects};
    use std::sync::{Arc, Mutex};

    let _registry = TestSourceOverride::new(SourceSnapshot::Absent);
    let desk = Desk::new("m27");
    let config = desk.config.join("aterm.toml");
    let row = |label: &str| {
        format!(
            "opt_in='named'\n[[account]]\nlabel='{label}'\nemail='{label}@example.test'\nmachines=['m27']\n"
        )
    };
    fs::write(desk.fleet_file(), row("stale")).unwrap();
    let secrets = desk.home.join(".secrets");
    fs::create_dir(&secrets).unwrap();
    let vault = secrets.join(FLEET_ROSTER_FILE);
    fs::write(&vault, row("selected")).unwrap();
    let selected = select(
        Some(&desk.home),
        Some(&config),
        Some(&desk.roster()),
        Scope::Init,
    );
    assert_eq!(selected.source(), Some(vault.display().to_string()));
    // A one-machine input is still read-only; row contents cannot grant write authority.
    assert_eq!(selected.origin(), FleetOrigin::Vault(vault));
    let env = quiet_env(&desk.home, &desk.state, &config, NOW);
    let Assigning::Names(fleet, _) = assigning_source(Cli::Claude, &env) else {
        panic!("the selected source must supply the assignments");
    };
    assert_eq!(fleet.accounts[0].label, "selected");

    let cooldown = Mutex::new(None);
    let rediscovery = Mutex::new(Rediscovery::default());
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let fetch = |_: &Env, _: &str| panic!("the selected local input must be captured");
    let spawn = |_: Box<dyn FnOnce() + Send>| panic!("a sync must not start a sign-in");
    let fx = upkeep_effects(&fetch, &spawn, &cooldown, &rediscovery, &guard);
    assert!(sync_if_missing(
        &desk.home,
        &desk.state,
        &desk.roster(),
        &config,
        NOW,
        &fx
    ));
    assert_eq!(desk.rows().rows[0].label, "selected");
}

#[test]
fn a_legacy_copy_directs_edits_only_to_the_local_projection() {
    use super::super::login_tests::{Desk, NOW};
    let _registry = TestSourceOverride::new(SourceSnapshot::Absent);
    let desk = Desk::new("m27");
    let config = desk.config.join("aterm.toml");
    let secrets = desk.home.join(".secrets");
    fs::create_dir(&secrets).unwrap();
    let copy = secrets.join(FLEET_ROSTER_FILE);
    let text = FleetRoster::from_toml(
        "[[account]]\nlabel='old'\nemail='old@example.test'\nmachines=['m27']\n",
        "legacy fixture",
    )
    .unwrap()
    .to_toml_for(Some(OptIn::Discovered));
    fs::write(&copy, &text).unwrap();
    fs::write(desk.fleet_file(), &text).unwrap();
    let selected = select(
        Some(&desk.home),
        Some(&config),
        Some(&desk.roster()),
        Scope::Init,
    );
    let FleetOrigin::Local { files, .. } = selected.origin() else {
        panic!("the legacy copy must retain a local editing destination");
    };
    assert_eq!(files, [desk.fleet_file()]);
    let env = quiet_env(&desk.home, &desk.state, &config, NOW);
    let edit = local_fleet_edit(
        &env,
        &files[0],
        &Row {
            label: "new".into(),
            email: Some("new@example.test".into()),
            ..Row::default()
        },
    )
    .unwrap();
    assert_eq!(edit.path, desk.fleet_file());
    assert!(edit.after.contains("new@example.test"));
    assert_eq!(edit.before, text);
    assert_eq!(fs::read_to_string(copy).unwrap(), text);
}

#[cfg(unix)]
#[test]
fn local_edit_targets_cannot_alias_a_vault_input() {
    use super::super::login_tests::{Desk, NOW};
    for hard in [false, true] {
        let desk = Desk::new("m27");
        let secrets = desk.home.join(".secrets");
        fs::create_dir(&secrets).unwrap();
        let source = secrets.join(FLEET_ROSTER_FILE);
        let text = "[[account]]\nlabel='old'\nemail='old@example.test'\nmachines=['m27']\n";
        fs::write(&source, text).unwrap();
        if hard {
            fs::hard_link(&source, desk.fleet_file()).unwrap();
        } else {
            std::os::unix::fs::symlink(&source, desk.fleet_file()).unwrap();
        }
        let env = quiet_env(
            &desk.home,
            &desk.state,
            &desk.config.join("aterm.toml"),
            NOW,
        );
        let result = local_fleet_edit(
            &env,
            &desk.fleet_file(),
            &Row {
                label: "new".into(),
                email: Some("new@example.test".into()),
                ..Row::default()
            },
        );
        assert!(result.unwrap_err().contains("aliases"));
        assert_eq!(fs::read_to_string(source).unwrap(), text);
    }
}
