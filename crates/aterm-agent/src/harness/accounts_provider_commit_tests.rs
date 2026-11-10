// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg(unix)]

use super::*;
use crate::harness::alab_roster::{SourceSnapshot, TestSourceOverride};
use aterm_spec::interp::State;

#[test]
fn pending_default_provider_receipt_bypasses_an_identical_input_hold() {
    let _source = TestSourceOverride::new(SourceSnapshot::Absent);
    let bed = Bed::new("default-recovery-hold");
    bed.secret(
        &cli_fleet_file(Cli::Codex),
        "[[account]]\nlabel='new'\nemail='new@example.test'\nmachines=['m16']\n",
    );
    let store = accounts::Store::at_for(&bed.state, Cli::Codex, &bed.home);
    accounts::SYNC_COMMIT_CUT.with(|cut| cut.set(Some(0)));
    let interrupted = bed.sync(Cli::Codex, None);
    accounts::SYNC_COMMIT_CUT.with(|cut| cut.set(None));
    assert!(interrupted.is_err());
    assert!(store.sync_needs_recovery());
    assert!(!bed.roster(Cli::Codex).exists());
    let inputs = cli_sync_inputs(Cli::Codex, &bed.home, &bed.state, &bed.aterm_toml());
    let rediscovery = std::sync::Mutex::new(Rediscovery {
        default_held: BTreeMap::from([(Cli::Codex, (inputs, Instant::now()))]),
        ..Rediscovery::default()
    });
    assert!(default_sync_holds(&rediscovery, Cli::Codex, &inputs));
    let cooldown = std::sync::Mutex::new(None);
    let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let fx = super::super::login_tests::upkeep_effects(
        &read_named,
        &spawn,
        &cooldown,
        &rediscovery,
        &guard,
    );
    upkeep_pools(
        &bed.home,
        &bed.state,
        (&bed.roster(Cli::Claude), NOW, UpkeepStep::Sync),
        &fx,
    );
    assert!(!store.sync_needs_recovery());
    assert!(bed.rows(Cli::Codex).iter().any(|row| row.label == "new"));
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    transition_with_early_roster(state, action, false)
}

fn transition_with_early_roster(state: &State, action: &str, early_roster: bool) -> State {
    let mut next = state.clone();
    let variable = match action {
        "Provider" => Some("provider"),
        "Cut" => Some("cut"),
        "Fault" => Some("fault"),
        "Recover" => None,
        _ => panic!("unknown provider publication action"),
    };
    if let Some(variable) = variable {
        next.insert(variable, state[variable] + 1);
        return next;
    }
    let _source = TestSourceOverride::new(SourceSnapshot::Absent);
    let cli = [Cli::Codex, Cli::Grok, Cli::Kimi, Cli::Agy][state["provider"] as usize];
    let cut = [0, 1, 2, 3, 4, 6, 7][state["cut"] as usize];
    let bed = Bed::new("provider-publication");
    let store = bed.seed_records(cli);
    super::super::super::ai_pool::tests::seed_local_swarm_roster(&bed.home, "m100", &["m16"]);
    bed.secret(
        super::super::super::ai_pool::AI_POOL_FILE,
        &format!(
            "version=1\n[[account]]\nprovider='{}'\nlabel='new'\n\
                     email='new@example.test'\nwarm=['m100']\n",
            if cli == Cli::Kimi {
                "kimi:global"
            } else {
                cli.id()
            }
        ),
    );
    store
        .transact_within(&[], NOW, &mut None, |state| {
            state.swarm_label = Some("m16".into());
            state.swarm_aliases.clear();
            state.home_accounts = BTreeSet::from(["kept".into()]);
            state.prepared_accounts.insert("elsewhere".into());
            Ok(Vec::new())
        })
        .unwrap();
    let roster = bed.roster(cli);
    let paths = [
        roster.clone(),
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.source_policy_file(),
    ];
    let images = || {
        paths
            .iter()
            .map(|path| fs::read(path).ok())
            .collect::<Vec<_>>()
    };
    let before = images();
    accounts::SYNC_COMMIT_EARLY_ROSTER.with(|value| value.set(early_roster));
    accounts::SYNC_COMMIT_CUT.with(|value| value.set(Some(cut)));
    let result = bed.sync(cli, None);
    accounts::SYNC_COMMIT_CUT.with(|value| value.set(None));
    accounts::SYNC_COMMIT_EARLY_ROSTER.with(|value| value.set(false));
    let error = result.expect_err("the real provider publication must hit the cut");
    assert!(
        error.contains("injected interruption"),
        "{cli:?}/{cut}: {error}"
    );
    next.insert("early", i64::from(cut == 0 && images() != before));
    next.insert("fenced", i64::from(store.load(NOW).is_err()));
    let caller = match state["fault"] {
        0 => roster.clone(),
        1 => {
            fs::write(&roster, "# independently edited\n").unwrap();
            roster.clone()
        }
        2 => bed.config.join("foreign.toml"),
        _ => panic!("unknown recovery fault"),
    };
    let before_recovery = images();
    let recovered = store.recover_sync_within(&[], &caller);
    let completed = matches!(recovered, Ok(true));
    let after_recovery = images();
    next.insert("completed", i64::from(completed));
    next.insert(
        "preserved",
        i64::from(completed || after_recovery == before_recovery),
    );
    let exact = if completed {
        let rows = bed.rows(cli);
        let current = store.load(NOW).unwrap().0;
        rows.iter().any(|row| row.label == "new")
            && rows.iter().any(|row| row.label == "kept")
            && fs::read(store.logins_file()).ok() == before[2]
            && current.swarm_label.as_deref() == Some("m100")
            && current.swarm_aliases == ["m16"]
            && current.home_accounts == BTreeSet::from(["new".into()])
            && current.prepared_accounts.contains("elsewhere")
            && current.active.as_deref() == Some("kept")
            && current.pinned
            && current.accounts["kept"].auth == accounts::Auth::SignedOut
            && store.source_policy().unwrap().retired()
    } else {
        true
    };
    next.insert("exact", i64::from(exact));
    let repeated = store.recover_sync_within(&[], &caller);
    next.insert(
        "once",
        i64::from(
            images() == after_recovery
                && if completed {
                    matches!(repeated, Ok(false))
                } else {
                    repeated.is_err()
                },
        ),
    );
    next.insert("phase", 1);
    next
}

/// Four providers, seven durable cuts and three recovery conditions enter the real
/// sync. A receipt around the later state write alone cannot pass cut zero.
#[test]
fn provider_roster_and_pool_identity_recover_as_one_publication() {
    let model = aterm_models_harness::account_provider_commit_model();
    aterm_spec::interp::prove_and_catch(&model);
    let mut recoveries = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let actual = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&actual),
                    "{state:?} -> {actual:?}"
                );
                recoveries += usize::from(action.name == "Recover");
            }
        }
    }
    assert_eq!(recoveries, 84);
    let premature = transition_with_early_roster(&model.init_state(), "Recover", true);
    assert!(
        !model
            .successors("Recover", &model.init_state())
            .contains(&premature)
    );
    assert!(!model.check_invariant("NoEarlyImages", &premature));
}
