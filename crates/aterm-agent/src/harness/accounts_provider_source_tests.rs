// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::alab_roster::{Lab, SourceSnapshot, TestSourceOverride, test_machine};
use aterm_spec::interp::State;
use std::sync::Arc;

struct Bed {
    _root: aterm_tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    config: PathBuf,
}

impl Bed {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        fs::create_dir_all(home.join(".secrets")).unwrap();
        let config = root.path().join("aterm.toml");
        fs::write(&config, "[fleet]\nmachine='m16'\n").unwrap();
        Self {
            _root: root,
            home,
            state,
            config,
        }
    }

    fn pool(&self, cli: Cli) {
        fs::write(self.pool_path(), format!(
            "version=1\n[[account]]\nprovider='{}'\nlabel='new'\nemail='new@example.test'\nwarm=['m16']\n",
            if cli == Cli::Kimi { "kimi:global" } else { cli.id() }
        )).unwrap();
    }

    fn pool_path(&self) -> PathBuf {
        self.home
            .join(".secrets")
            .join(super::super::super::ai_pool::AI_POOL_FILE)
    }

    fn env(&self) -> Env {
        quiet_env(&self.home, &self.state, &self.config, 1)
    }

    fn store(&self, cli: Cli) -> accounts::Store {
        accounts::Store::at_for(&self.state, cli, &self.home)
    }

    fn roster(&self, cli: Cli) -> PathBuf {
        accounts::roster_beside_for(&self.config, cli).unwrap()
    }

    fn images(&self, cli: Cli) -> Vec<Option<Vec<u8>>> {
        let store = self.store(cli);
        [
            self.roster(cli),
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.wake_path(),
            store.source_policy_file(),
        ]
        .iter()
        .map(|path| fs::read(path).ok())
        .collect()
    }
}

fn native_account(label: &str, email: &str, slot: u64) -> crate::harness::alab_roster::LabAccount {
    crate::harness::alab_roster::LabAccount {
        pk: format!("ac-{slot:016x}"),
        cli: Cli::Codex,
        vendor: Default::default(),
        label: label.into(),
        legacy_label: None,
        email: email.into(),
        identity: None,
        browser: None,
        joinable: true,
        preferred: vec![(test_machine("m16"), Some(slot))],
    }
}

#[test]
fn completion_uses_the_native_source_and_refuses_pending_registry_state() {
    let bed = Bed::new();
    bed.pool(Cli::Codex);
    let registry = TestSourceOverride::new(SourceSnapshot::Present(Arc::new(Lab {
        me: Some(test_machine("m16")),
        accounts: vec![native_account("native", "new@example.test", 1)],
        ..Lab::default()
    })));
    let row = Row {
        label: "native".into(),
        email: Some("new@example.test".into()),
        ..Row::default()
    };
    let source = codex_completion_source(&bed.env(), &row).unwrap();
    assert_eq!(source.fleet.unwrap().roster.accounts[0].label, "native");
    assert!(source.pool_text.is_none());
    assert!(source.held_by.is_none());
    registry.set(SourceSnapshot::Pending);
    assert!(codex_completion_source(&bed.env(), &row).is_err());
    registry.set(SourceSnapshot::Absent);
    let source = codex_completion_source(&bed.env(), &row).unwrap();
    assert!(source.pool_text.is_some());
    assert_eq!(source.fleet.unwrap().roster.accounts[0].label, "new");
}

#[cfg(unix)]
#[test]
fn completion_refuses_a_retired_hard_link_and_fenced_policy() {
    let _registry = TestSourceOverride::new(SourceSnapshot::Absent);
    for pending in [false, true] {
        let bed = Bed::new();
        bed.pool(Cli::Codex);
        fs::hard_link(
            bed.pool_path(),
            bed.home.join(".secrets").join(cli_fleet_file(Cli::Codex)),
        )
        .unwrap();
        let store = bed.store(Cli::Codex);
        let held = store.acquire().unwrap();
        accounts::SYNC_COMMIT_CUT.set(pending.then_some(0));
        let retired = store.retire_source_held(&held, 1);
        accounts::SYNC_COMMIT_CUT.set(None);
        assert_eq!(retired.is_err(), pending);
        drop(held);
        assert!(
            codex_completion_source(
                &bed.env(),
                &Row {
                    label: "new".into(),
                    email: Some("new@example.test".into()),
                    ..Row::default()
                }
            )
            .is_err()
        );
    }
}

#[test]
fn completion_ignores_pool_bytes_in_a_sheet_without_a_provider_column() {
    let bed = Bed::new();
    let _registry = TestSourceOverride::new(SourceSnapshot::Absent);
    let row = Row {
        label: "new".into(),
        email: Some("new@example.test".into()),
        ..Row::default()
    };
    let before = codex_completion_source(&bed.env(), &row).unwrap();
    assert!(before.fleet.is_none());
    bed.pool(Cli::Codex);
    fs::rename(
        bed.pool_path(),
        bed.home.join(".secrets").join(ACCOUNTS_CSV_FILE),
    )
    .unwrap();
    assert_eq!(codex_completion_source(&bed.env(), &row).unwrap(), before);
}

#[test]
fn already_retired_empty_assignments_need_no_writer() {
    for cli in OTHER_CLIS {
        for native in [false, true] {
            let bed = Bed::new();
            let _registry = TestSourceOverride::new(if native {
                SourceSnapshot::Present(Arc::new(Lab {
                    me: Some(test_machine("m16")),
                    ..Lab::default()
                }))
            } else {
                SourceSnapshot::Absent
            });
            fs::write(bed.pool_path(), "version=1\naccount=[]\n").unwrap();
            let store = bed.store(cli);
            let held = store.acquire().unwrap();
            store.retire_source_held(&held, 1).unwrap();
            let before = bed.images(cli);
            assert!(matches!(
                sync_cli_with(
                    cli,
                    (&bed.home, &bed.state, &bed.config),
                    (None, None),
                    2,
                    (&fetch_source_text, &[]),
                ),
                Ok(CliSync::Unlisted { .. })
            ));
            assert_eq!(bed.images(cli), before);
        }
    }
}

#[test]
fn explicit_sheet_rechecks_eligibility_without_observing_unrelated_configuration() {
    let bed = Bed::new();
    let registry = TestSourceOverride::new(SourceSnapshot::Absent);
    let sheet = bed.home.join("sheet.csv");
    fs::write(&sheet, "email,computer,codex\nnew@example.test,m16,1\n").unwrap();
    let captured = ProviderSource::capture(
        Cli::Codex,
        &bed.env(),
        &bed.store(Cli::Codex),
        &bed.roster(Cli::Codex),
        (sheet.to_str(), Some("m16")),
        false,
        &fetch_source_text,
    )
    .unwrap();
    fs::write(&bed.config, "[fleet]\nmachine='m17'\n").unwrap();
    captured.validate(Cli::Codex, &bed.env()).unwrap();
    registry.set(SourceSnapshot::Pending);
    assert!(captured.validate(Cli::Codex, &bed.env()).is_err());
}

#[test]
fn a_valid_migration_stays_committed_when_a_later_sync_plan_is_refused() {
    let bed = Bed::new();
    let cli = Cli::Codex;
    let login = bed.home.join("existing-login");
    fs::create_dir_all(&login).unwrap();
    fs::write(login.join("identity"), "preserved").unwrap();
    let original = Row {
        label: "legacy".into(),
        email: Some("one@example.test".into()),
        dir: Some(login.to_str().unwrap().into()),
        ..Row::default()
    };
    fs::write(
        bed.roster(cli),
        accounts::append_rows_for(cli, "", &[original]).unwrap(),
    )
    .unwrap();
    let mut first = native_account("native-one", "one@example.test", 1);
    first.legacy_label = Some("legacy".into());
    let registry = TestSourceOverride::new(SourceSnapshot::Present(Arc::new(Lab {
        me: Some(test_machine("m16")),
        settled: true,
        accounts: vec![first, native_account("native-two", "two@example.test", 2)],
        ..Lab::default()
    })));
    let mut committed = Vec::new();
    let result = sync_cli_with_replan(
        cli,
        (&bed.home, &bed.state, &bed.config),
        (None, None),
        false,
        1,
        (&fetch_source_text, &[]),
        || {
            committed = bed.images(cli);
            assert!(bed.store(cli).source_policy().unwrap().retired());
            registry.set(SourceSnapshot::Pending);
        },
    );
    assert!(result.is_err());
    assert!(!committed.is_empty());
    assert_eq!(bed.images(cli), committed);
    let roster = Roster::load_for(cli, &bed.roster(cli)).unwrap();
    assert!(roster.row("legacy").is_none());
    assert_eq!(
        roster.row("native-one").unwrap().dir.as_deref(),
        login.to_str()
    );
    assert!(roster.row("native-two").is_none());
    assert_eq!(
        fs::read_to_string(login.join("identity")).unwrap(),
        "preserved"
    );
}

#[test]
fn every_provider_refuses_changed_sources_after_signin_custody() {
    for cli in OTHER_CLIS {
        for change in 0..5 {
            // Vault-backed pool identity exists only on Unix. Source bytes,
            // configuration and account policy changes run on every platform.
            #[cfg(not(unix))]
            if change == 3 {
                continue;
            }
            let bed = Bed::new();
            bed.pool(cli);
            let registry = TestSourceOverride::new(SourceSnapshot::Absent);
            let mut before = bed.images(cli);
            let mut replanned = false;
            let result = sync_cli_with_replan(
                cli,
                (&bed.home, &bed.state, &bed.config),
                (None, None),
                false,
                1,
                (&fetch_source_text, &[]),
                || {
                    replanned = true;
                    match change {
                        0 => fs::write(bed.pool_path(), "version=1\naccount=[]\n").unwrap(),
                        1 => fs::write(&bed.config, "[fleet]\nmachine='m17'\n").unwrap(),
                        2 => registry.set(SourceSnapshot::Pending),
                        #[cfg(unix)]
                        3 => super::super::super::ai_pool::tests::seed_local_swarm_roster(
                            &bed.home,
                            "m100",
                            &["m16"],
                        ),
                        4 => {
                            let store = bed.store(cli);
                            let held = store.acquire().unwrap();
                            store.retire_source_held(&held, 2).unwrap();
                            before = bed.images(cli);
                        }
                        _ => unreachable!(),
                    }
                },
            );
            assert!(replanned);
            assert!(result.is_err(), "{cli:?}/{change}: {result:?}");
            assert_eq!(bed.images(cli), before);
            assert!(!bed.store(cli).login_dir("new").unwrap().exists());
        }
    }
}

#[test]
fn providers_retire_empty_replacements_and_preserve_explicit_inputs() {
    for cli in OTHER_CLIS {
        for native in [false, true] {
            let bed = Bed::new();
            let registry = TestSourceOverride::new(if native {
                SourceSnapshot::Present(Arc::new(Lab {
                    me: Some(test_machine("m16")),
                    settled: true,
                    ..Lab::default()
                }))
            } else {
                SourceSnapshot::Absent
            });
            let legacy = bed.home.join(".secrets").join(cli_fleet_file(cli));
            fs::write(&legacy, "retained legacy bytes").unwrap();
            if !native {
                fs::write(bed.pool_path(), "version=1\naccount=[]\n").unwrap();
            }
            let result = sync_cli_with(
                cli,
                (&bed.home, &bed.state, &bed.config),
                (None, None),
                1,
                (&fetch_source_text, &[]),
            );
            assert!(
                matches!(result, Ok(CliSync::Unlisted { .. })),
                "{cli:?}: {result:?}"
            );
            assert!(bed.store(cli).source_policy().unwrap().retired());
            assert_eq!(
                fs::read_to_string(&legacy).unwrap(),
                "retained legacy bytes"
            );
            assert!(!bed.roster(cli).exists());
            drop(registry);
        }

        let bed = Bed::new();
        let _registry = TestSourceOverride::new(SourceSnapshot::Absent);
        bed.pool(cli);
        let input = fs::read(bed.pool_path()).unwrap();
        let result = sync_cli_with(
            cli,
            (&bed.home, &bed.state, &bed.config),
            (bed.pool_path().to_str(), None),
            1,
            (&fetch_source_text, &[]),
        );
        assert!(matches!(result, Ok(CliSync::Synced(_))), "{result:?}");
        assert!(!bed.store(cli).source_policy().unwrap().retired());
        assert_eq!(fs::read(bed.pool_path()).unwrap(), input);
    }
}

#[cfg(unix)]
#[test]
fn a_provider_replacement_cannot_retire_itself_through_a_legacy_alias() {
    let _registry = TestSourceOverride::new(SourceSnapshot::Absent);
    for cli in OTHER_CLIS {
        for hard in [false, true] {
            let bed = Bed::new();
            let legacy = bed.home.join(".secrets").join(cli_fleet_file(cli));
            fs::write(&legacy, "version=1\naccount=[]\n").unwrap();
            if hard {
                fs::hard_link(&legacy, bed.pool_path()).unwrap();
            } else {
                std::os::unix::fs::symlink(&legacy, bed.pool_path()).unwrap();
            }
            let before = bed.images(cli);
            let result = sync_cli_with(
                cli,
                (&bed.home, &bed.state, &bed.config),
                (None, None),
                1,
                (&fetch_source_text, &[]),
            );
            assert!(result.is_err(), "{cli:?}/{hard}: {result:?}");
            assert_eq!(bed.images(cli), before);
            assert!(!bed.store(cli).source_policy().unwrap().retired());
            assert_eq!(
                fs::read_to_string(legacy).unwrap(),
                "version=1\naccount=[]\n"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn retired_alias_refusals_do_not_read_excluded_bytes_for_retry() {
    let _registry = TestSourceOverride::new(SourceSnapshot::Absent);
    for cli in OTHER_CLIS {
        for hard in [false, true] {
            let bed = Bed::new();
            let legacy = bed.home.join(".secrets").join(cli_fleet_file(cli));
            fs::write(&legacy, "version=1\naccount=[]\n").unwrap();
            if hard {
                fs::hard_link(&legacy, bed.pool_path()).unwrap();
            } else {
                std::os::unix::fs::symlink(&legacy, bed.pool_path()).unwrap();
            }
            let store = bed.store(cli);
            let held = store.acquire().unwrap();
            store.retire_source_held(&held, 1).unwrap();
            drop(held);
            let before = cli_sync_inputs(cli, &bed.home, &bed.state, &bed.config);
            fs::write(&legacy, "changed excluded bytes").unwrap();
            assert_eq!(
                cli_sync_inputs(cli, &bed.home, &bed.state, &bed.config),
                before
            );
            assert!(load_cli_fleet(cli, &bed.env(), &bed.home, None, &fetch_source_text).is_err());
        }
    }
}

#[test]
fn native_label_migration_commits_its_retirement_at_every_cut() {
    use crate::harness::alab_roster::LabAccount;
    for cut in [0, 4, 6, 7, usize::MAX] {
        let bed = Bed::new();
        let cli = Cli::Codex;
        let store = bed.store(cli);
        let mut state = accounts::State::default();
        state
            .accounts
            .insert("legacy".into(), accounts::Account::default());
        store.save(&state).unwrap();
        fs::write(
            bed.roster(cli),
            accounts::append_rows_for(
                cli,
                "",
                &[Row {
                    label: "legacy".into(),
                    email: Some("native@example.test".into()),
                    ..Row::default()
                }],
            )
            .unwrap(),
        )
        .unwrap();
        let legacy = bed.home.join(".secrets").join(cli_fleet_file(cli));
        fs::write(&legacy, "preserved legacy input").unwrap();
        let _source = TestSourceOverride::new(SourceSnapshot::Present(Arc::new(Lab {
            me: Some(test_machine("m16")),
            settled: true,
            accounts: vec![LabAccount {
                pk: "ac-0000000000000001".into(),
                cli,
                vendor: Default::default(),
                label: "native".into(),
                legacy_label: Some("legacy".into()),
                email: "native@example.test".into(),
                identity: None,
                browser: None,
                joinable: true,
                preferred: vec![(test_machine("m16"), Some(1))],
            }],
            ..Lab::default()
        })));
        accounts::SYNC_COMMIT_CUT.set(Some(cut));
        let result = sync_cli_with(
            cli,
            (&bed.home, &bed.state, &bed.config),
            (None, None),
            1,
            (&fetch_source_text, &[]),
        );
        accounts::SYNC_COMMIT_CUT.set(None);
        if cut != usize::MAX {
            assert!(result.unwrap_err().contains("injected interruption"));
            assert!(store.source_policy().is_err());
            assert!(store.recover_sync_within(&[], &bed.roster(cli)).unwrap());
        } else {
            result.unwrap();
        }
        assert!(store.source_policy().unwrap().retired());
        assert!(
            Roster::load_for(cli, &bed.roster(cli))
                .unwrap()
                .row("native")
                .is_some()
        );
        assert_eq!(
            fs::read_to_string(legacy).unwrap(),
            "preserved legacy input"
        );
    }
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    transition_for(state, action, Cli::Codex, false)
}

fn transition_for(state: &State, action: &str, cli: Cli, bypass: bool) -> State {
    let mut next = state.clone();
    let input = match action {
        "Origin" => Some("origin"),
        "Change" => Some("change"),
        "Config" => Some("config"),
        "Stage" => Some("stage"),
        "Admit" => None,
        _ => panic!("unknown provider capture action"),
    };
    if let Some(input) = input {
        next.insert(input, state[input] + 1);
        return next;
    }
    let bed = Bed::new();
    bed.pool(cli);
    let lab = Lab {
        me: Some(test_machine("m16")),
        ..Lab::default()
    };
    let initial = if state["origin"] == 0 {
        SourceSnapshot::Absent
    } else {
        SourceSnapshot::Present(Arc::new(lab.clone()))
    };
    let registry = TestSourceOverride::new(initial.clone());
    let store = bed.store(cli);
    let captured = ProviderSource::capture(
        cli,
        &bed.env(),
        &store,
        &bed.roster(cli),
        (None, None),
        true,
        &fetch_source_text,
    )
    .unwrap();
    if state["stage"] == 1 {
        let held = store.acquire().unwrap();
        store.recover_sync_held(&held, &bed.roster(cli)).unwrap();
        captured.validate(cli, &bed.env()).unwrap();
    }
    match state["change"] {
        0 => {}
        1 => registry.set(if state["origin"] == 0 {
            SourceSnapshot::Absent
        } else {
            SourceSnapshot::Present(Arc::new(lab.clone()))
        }),
        2 => registry.set(SourceSnapshot::Disabled),
        3 => registry.set(SourceSnapshot::Pending),
        4 if state["origin"] == 0 => fs::write(bed.pool_path(), "version=1\naccount=[]\n").unwrap(),
        4 => registry.set(SourceSnapshot::Present(Arc::new(Lab {
            me: Some(test_machine("m17")),
            ..lab
        }))),
        5 if state["origin"] == 0 => registry.set(SourceSnapshot::Present(Arc::new(lab))),
        5 => registry.set(SourceSnapshot::Absent),
        _ => unreachable!(),
    }
    if state["config"] == 1 {
        fs::write(&bed.config, "[harness]\nenabled=false\n").unwrap();
    }
    let held = store.acquire().unwrap();
    store.recover_sync_held(&held, &bed.roster(cli)).unwrap();
    SKIP_REVALIDATION.set(bypass);
    let result = captured.validate(cli, &bed.env());
    SKIP_REVALIDATION.set(false);
    next.insert("admitted", i64::from(result.is_ok()));
    next.insert("done", 1);
    next
}

#[test]
fn provider_capture_model_executes_real_observations_and_writer_reacquisition() {
    let model = aterm_models_harness::account_provider_capture_model();
    aterm_spec::interp::prove_and_catch(&model);
    for cli in OTHER_CLIS {
        for state in aterm_spec::interp::reachable_states(&model) {
            for action in &model.actions {
                if model.action_enabled(action.name, &state) {
                    let actual = if cli == Cli::Codex {
                        transition(&state, action.name)
                    } else {
                        transition_for(&state, action.name, cli, false)
                    };
                    assert!(
                        model.successors(action.name, &state).contains(&actual),
                        "{cli:?}/{state:?}: {actual:?}"
                    );
                }
            }
        }
    }
    let mut stale = model.init_state();
    stale.insert("change", 2);
    stale.insert("stage", 1);
    let bypassed = transition_for(&stale, "Admit", Cli::Codex, true);
    assert!(!model.check_invariant("ChangedSourceRefused", &bypassed));
}
