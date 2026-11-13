// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::login_tests::{Desk, NOW};
use super::*;
use crate::harness::alab_roster::{SourceSnapshot, TestSourceOverride};
use aterm_spec::interp::State;

struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        accounts::SYNC_COMMIT_CUT.set(None);
        SOURCE_WRITES_MUTANT.set(0);
    }
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    transition_with(state, action, 0)
}

fn transition_with(state: &State, action: &str, mutant: u8) -> State {
    let mut next = state.clone();
    if action != "Sync" {
        let field = if action == "Source" { "kind" } else { "cut" };
        next.insert(field, state[field] + 1);
        return next;
    }
    let _registry = TestSourceOverride::new(SourceSnapshot::Absent);
    let mut desk = Desk::new("m27");
    desk.default_login("one@example.test");
    fs::write(
        desk.home.join(".claude.json.backup"),
        "{\"oauthAccount\":{\"emailAddress\":\"two@example.test\"}}\n",
    )
    .unwrap();
    let secrets = desk.home.join(".secrets");
    fs::create_dir(&secrets).unwrap();
    let copy = secrets.join(FLEET_ROSTER_FILE);
    let fleet = FleetRoster::from_toml(
        "[[account]]\nlabel='m27'\nemail='one@example.test'\nmachines=['m27']\n",
        "fixture",
    )
    .unwrap();
    let input = match state["kind"] {
        0 => copy.clone(),
        1 => {
            // An externally owned source must not inherit local discovery policy.
            fs::write(&copy, fleet.to_toml_for(Some(OptIn::Discovered))).unwrap();
            copy.clone()
        }
        2 => {
            let text = fleet.to_toml_for(Some(OptIn::Discovered));
            fs::write(&copy, &text).unwrap();
            fs::write(desk.fleet_file(), &text).unwrap();
            // The projection's next write cannot become newer than this copy.
            // Migration uses durable retirement instead of timestamp order.
            let future = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(4_000_000_000);
            for path in [&copy, &desk.fleet_file()] {
                fs::File::options()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_modified(future)
                    .unwrap();
            }
            copy.clone()
        }
        3 => {
            let sheet = secrets.join(ACCOUNTS_CSV_FILE);
            fs::write(
                &sheet,
                "email,computer,claude\none@example.test,m27,1\ntwo@example.test,m27,1\n",
            )
            .unwrap();
            sheet
        }
        4 => {
            // The source leaf does not exist, but the local projection would
            // create it through a parent directory alias.
            #[cfg(unix)]
            {
                let alias = desk.home.join("linked-config");
                std::os::unix::fs::symlink(&secrets, &alias).unwrap();
                desk.config = alias;
            }
            #[cfg(not(unix))]
            {
                desk.config = secrets.clone();
            }
            fs::write(desk.config.join("aterm.toml"), "[fleet]\nmachine='m27'\n").unwrap();
            copy.clone()
        }
        _ => unreachable!(),
    };
    let before = fs::read(&input).ok();
    let store = desk.store();
    if state["cut"] == 0 {
        fs::create_dir_all(&store.dir).unwrap();
        fs::write(store.state_file(), "{\"schema\":999}").unwrap();
    }
    SOURCE_WRITES_MUTANT.set(mutant);
    accounts::SYNC_COMMIT_CUT.set((state["cut"] == 1).then_some(0));
    let _restore = Restore;
    let result = sync_accounts_for_init_with(
        &desk.home,
        &desk.state,
        &desk.roster(),
        NOW,
        || {},
        &fetch_source_text,
    );
    accounts::SYNC_COMMIT_CUT.set(None);
    let forbidden = state["kind"] == 4 && mutant != 3;
    assert_eq!(
        result.is_ok(),
        state["cut"] == 2 && !forbidden,
        "{state:?}: {result:?}"
    );
    next.insert("done", 1);
    next.insert("preserved", i64::from(fs::read(&input).ok() == before));
    next.insert("pending", i64::from(store.sync_needs_recovery()));
    next.insert("published", i64::from(desk.roster().exists()));
    let recovered =
        state["cut"] == 1 && !forbidden && store.recover_sync_within(&[], &desk.roster()).unwrap();
    next.insert("recovered", i64::from(recovered));
    if state["cut"] > 0 && !forbidden {
        assert_eq!(
            desk.rows().rows.len(),
            if state["kind"] == 1 { 1 } else { 2 }
        );
        if state["kind"] == 3 {
            assert!(
                !copy.exists(),
                "a sheet import must not seed a competing source"
            );
        }
        if state["kind"] == 2 {
            assert!(store.source_policy().unwrap().retired());
            assert!(!needs_init_accounts_sync(
                &desk.home,
                &desk.state,
                &desk.roster(),
                NOW
            ));
        }
        assert_eq!(fs::read(&input).ok() == before, next["preserved"] == 1);
    }
    next
}

#[test]
fn automatic_sources_are_preserved_through_admission_and_recovery() {
    let model = aterm_models_harness::account_source_preservation_model();
    aterm_spec::interp::prove_and_catch(&model);
    assert!(aterm_spec::verify::sweep_faults(std::slice::from_ref(&model)).is_empty());
    let mut cases = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
                cases += usize::from(action.name == "Sync");
            }
        }
    }
    assert_eq!(cases, 15);
    for (mutant, kind, cut) in [(1, 0, 1), (2, 2, 1), (3, 4, 2)] {
        let mut state = model.init_state();
        state.insert("kind", kind);
        state.insert("cut", cut);
        let bad = transition_with(&state, "Sync", mutant);
        assert!(!model.check_invariant("SourcePreserved", &bad));
        assert!(!model.successors("Sync", &state).contains(&bad));
    }
}

#[test]
fn external_source_formatting_does_not_schedule_another_import() {
    let _registry = TestSourceOverride::new(SourceSnapshot::Absent);
    let desk = Desk::new("m27");
    desk.default_login("one@example.test");
    let secrets = desk.home.join(".secrets");
    fs::create_dir(&secrets).unwrap();
    let path = secrets.join(FLEET_ROSTER_FILE);
    let text = "# External comments remain intact.\n[[account]]\nlabel='m27'\nemail='one@example.test'\nmachines=['m27']\n";
    fs::write(&path, text).unwrap();
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).unwrap();
    assert!(!needs_init_accounts_sync(
        &desk.home,
        &desk.state,
        &desk.roster(),
        NOW
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
    fs::write(&path, format!("opt_in='discovered'\n{text}")).unwrap();
    assert!(
        needs_init_accounts_sync(&desk.home, &desk.state, &desk.roster(), NOW),
        "a policy-only change is a new source decision"
    );
    let changed = text.replace("one@example.test", "replacement@example.test");
    fs::write(&path, &changed).unwrap();
    assert!(needs_init_accounts_sync(
        &desk.home,
        &desk.state,
        &desk.roster(),
        NOW
    ));
}

#[test]
fn an_unassigned_sheet_keeps_local_discovery_without_a_legacy_copy() {
    let _registry = TestSourceOverride::new(SourceSnapshot::Absent);
    let desk = Desk::new("m27");
    desk.default_login("one@example.test");
    fs::write(
        desk.home.join(".claude.json.backup"),
        "{\"oauthAccount\":{\"emailAddress\":\"two@example.test\"}}\n",
    )
    .unwrap();
    let secrets = desk.home.join(".secrets");
    fs::create_dir(&secrets).unwrap();
    let sheet = secrets.join(ACCOUNTS_CSV_FILE);
    let copy = secrets.join(FLEET_ROSTER_FILE);
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).unwrap();
    assert!(!copy.exists());
    let before = fs::read(desk.roster()).unwrap();
    fs::write(&sheet, "email,computer,claude\nother@example.test,m28,1\n").unwrap();
    assert!(!needs_init_accounts_sync(
        &desk.home,
        &desk.state,
        &desk.roster(),
        NOW
    ));
    let report = sync_accounts_for_init_with(
        &desk.home,
        &desk.state,
        &desk.roster(),
        NOW,
        || {},
        &fetch_source_text,
    )
    .unwrap();
    assert_eq!(report.rows, 2);
    assert_eq!(fs::read(desk.roster()).unwrap(), before);
    assert!(!copy.exists());
    fs::write(&sheet, "email,computer,claude\none@example.test,m27,1\n").unwrap();
    assert!(needs_init_accounts_sync(
        &desk.home,
        &desk.state,
        &desk.roster(),
        NOW
    ));
    sync_accounts_for_init(&desk.home, &desk.state, &desk.roster(), NOW).unwrap();
    assert!(desk.store().source_policy().unwrap().retired());
    assert!(!copy.exists());
}
