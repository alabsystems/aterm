// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::alab_roster::{Lab, SourceSnapshot, TestSourceOverride, test_machine};
use aterm_spec::interp::State;
use std::sync::Arc;

#[cfg(unix)]
const AI_POOL: &str = super::super::super::ai_pool::AI_POOL_FILE;

fn pool_text(cli: Cli) -> String {
    format!(
        "version=1\n[[account]]\nprovider='{}'\nlabel='new'\nemail='new@example.test'\nwarm=['m16']\n",
        if cli == Cli::Kimi {
            "kimi:global"
        } else {
            cli.id()
        }
    )
}

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
        fs::create_dir_all(&home).unwrap();
        let config = root.path().join("aterm.toml");
        fs::write(&config, "[fleet]\nmachine='m16'\n").unwrap();
        Self {
            _root: root,
            home,
            state,
            config,
        }
    }

    /// The vault's pool holds one account, `new`, warm on m16.
    #[cfg(unix)]
    fn pool(&self, cli: Cli) {
        self.put(AI_POOL, &pool_text(cli));
    }

    /// The vault entry `name` holds `text` (an empty `text` removes it), read in place.
    #[cfg(unix)]
    fn put(&self, name: &str, text: &str) {
        crate::vault_entry::seed(&self.home, &[(name, text.as_bytes())]);
    }

    /// What the vault entry `name` holds now.
    #[cfg(unix)]
    fn held(&self, name: &str) -> Option<String> {
        crate::vault_entry::read_text(&self.home, name, 1 << 20)
            .text()
            .map(str::to_owned)
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

struct RefuseReceipt(std::sync::Arc<std::sync::atomic::AtomicUsize>);

#[test]
fn admitted_receipt_recovers_before_a_new_source_is_available() {
    for entry in 0..4 {
        let bed = Bed::new();
        let cli = if entry < 2 { Cli::Claude } else { Cli::Codex };
        let store = bed.store(cli);
        let path = bed.roster(cli);
        let text = "# previously admitted\n";
        {
            let held = store.acquire().unwrap();
            accounts::SYNC_COMMIT_CUT.set(Some(0));
            let result = store.transact_roster_held(
                &held,
                1,
                &mut None,
                accounts::SyncTarget {
                    path: &path,
                    before: None,
                    after: Some(text),
                    mode: None,
                    fleet: None,
                },
                false,
            );
            accounts::SYNC_COMMIT_CUT.set(None);
            assert!(result.is_err());
        }
        assert!(store.sync_needs_recovery());
        let _source = TestSourceOverride::new(SourceSnapshot::Pending);
        let how = SyncHow {
            fetch: &fetch_source_text,
            ladder: &[],
            fleet_snapshots: &[],
            heal: None,
            allow_empty_pool: true,
        };
        let _ = match entry {
            0 => sync_accounts_for_init_with(
                &bed.home,
                &bed.state,
                &path,
                2,
                || {},
                &fetch_source_text,
            )
            .map(|_| ())
            .map_err(SyncError::into_message),
            1 => sync_accounts_with(&bed.env(), Some("alab"), None, &how)
                .map(|_| ())
                .map_err(SyncError::into_message),
            2 => sync_cli_with(
                cli,
                (&bed.home, &bed.state, &bed.config),
                (None, None),
                2,
                (&fetch_source_text, &[]),
            )
            .map(|_| ()),
            _ => crate::harness::cli::default_completion::complete(&bed.env(), None, |_| None)
                .map(|_| ()),
        };
        assert!(!store.sync_needs_recovery(), "entry={entry}");
        assert_eq!(fs::read_to_string(path).unwrap(), text, "entry={entry}");
    }
}

impl crate::harness::alab_roster::SourceGuard for RefuseReceipt {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn same_source(&self, _: &dyn crate::harness::alab_roster::SourceGuard) -> bool {
        false
    }
    fn with_admitted(
        &self,
        _: Box<dyn FnOnce() -> Result<(), SyncError> + '_>,
    ) -> Result<(), SyncError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(SyncError::retry(
            Retry::Changed,
            "refused at native receipt boundary",
        ))
    }
}

#[test]
fn native_producers_reach_the_receipt_guard_without_publishing_first() {
    for route in 0..8 {
        let bed = Bed::new();
        let cli = if route < 4 { Cli::Claude } else { Cli::Codex };
        let store = bed.store(cli);
        let mut state = accounts::State::default();
        if route == 6 {
            state
                .accounts
                .insert("legacy".into(), accounts::Account::default());
        }
        store.save(&state).unwrap();
        let mut account = native_account(
            if route == 7 { "main" } else { "native" },
            "native@example.test",
            1,
        );
        account.cli = cli;
        if route == 6 {
            account.legacy_label = Some("legacy".into());
        }
        let empty = route == 3 || route == 5;
        let lab = Lab {
            me: Some(test_machine("m16")),
            settled: route == 6,
            accounts: if empty { Vec::new() } else { vec![account] },
            ..Lab::default()
        };
        if route == 6 || route == 7 {
            let row = Row {
                label: if route == 6 { "legacy" } else { "main" }.into(),
                email: (route == 6).then(|| "native@example.test".into()),
                class: accounts::Class::Preferred,
                ..Row::default()
            };
            fs::write(
                bed.roster(cli),
                accounts::append_rows_for(cli, "", &[row]).unwrap(),
            )
            .unwrap();
        }
        if route == 7 {
            fs::create_dir_all(bed.home.join(".codex")).unwrap();
        }
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let _source = TestSourceOverride::new(SourceSnapshot::Present(Arc::new(lab))).with_guard(
            crate::harness::alab_roster::ReceiptGuard::new(RefuseReceipt(calls.clone())),
        );
        let before = bed.images(cli);
        let how = SyncHow {
            fetch: &fetch_source_text,
            ladder: &[],
            fleet_snapshots: &[],
            heal: None,
            allow_empty_pool: true,
        };
        let result = match route {
            0 | 1 | 3 => sync_accounts_with(&bed.env(), (route == 1).then_some("alab"), None, &how)
                .map(|_| ())
                .map_err(SyncError::into_message),
            2 => sync_accounts_for_init_with(
                &bed.home,
                &bed.state,
                &bed.roster(cli),
                1,
                || {},
                &fetch_source_text,
            )
            .map(|_| ())
            .map_err(SyncError::into_message),
            7 => crate::harness::cli::default_completion::complete(&bed.env(), None, |_| {
                Some("native@example.test".into())
            })
            .map(|_| ()),
            _ => sync_cli_with(
                cli,
                (&bed.home, &bed.state, &bed.config),
                (None, None),
                1,
                (&fetch_source_text, &[]),
            )
            .map(|_| ()),
        };
        assert!(
            result
                .as_ref()
                .is_err_and(|why| why.contains("refused at native receipt boundary")),
            "route={route}: {result:?}"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "route={route}"
        );
        assert_eq!(bed.images(cli), before, "route={route}");
        assert!(!store.sync_needs_recovery(), "route={route}");
        assert!(
            !store.login_dir("native").unwrap().exists(),
            "route={route}"
        );
    }
}

#[test]
fn completion_uses_the_native_source_and_refuses_pending_registry_state() {
    let bed = Bed::new();
    #[cfg(unix)]
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
    #[cfg(unix)]
    {
        assert!(source.pool_text.is_some());
        assert_eq!(source.fleet.unwrap().roster.accounts[0].label, "new");
    }
    #[cfg(not(unix))]
    assert!(source.pool_text.is_none() && source.fleet.is_none());
}

#[cfg(unix)]
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
    bed.put(ACCOUNTS_CSV_FILE, &pool_text(Cli::Codex));
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
            #[cfg(unix)]
            bed.put(AI_POOL, "version=1\naccount=[]\n");
            let store = bed.store(cli);
            let held = store.acquire().unwrap();
            store.retire_source_held(&held, 1).unwrap();
            let before = bed.images(cli);
            let result = sync_cli_with(
                cli,
                (&bed.home, &bed.state, &bed.config),
                (None, None),
                2,
                (&fetch_source_text, &[]),
            );
            if !native && cfg!(not(unix)) {
                assert!(matches!(result, Ok(CliSync::NoSource)), "{result:?}");
            } else {
                assert!(matches!(result, Ok(CliSync::Unlisted { .. })), "{result:?}");
            }
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
        SyncCaller::Person,
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
        for vault in [
            false,
            #[cfg(unix)]
            true,
        ] {
            for change in 0..5 {
                if !vault && matches!(change, 0 | 3) {
                    continue;
                }
                let bed = Bed::new();
                let registry = if vault {
                    #[cfg(unix)]
                    {
                        bed.pool(cli);
                        TestSourceOverride::new(SourceSnapshot::Absent)
                    }
                    #[cfg(not(unix))]
                    panic!("vault capture fixtures require Unix");
                } else {
                    let mut account = native_account("new", "new@example.test", 1);
                    account.cli = cli;
                    account.vendor.region =
                        (cli == Cli::Kimi).then_some(crate::ai::connector::Region::Global);
                    TestSourceOverride::new(SourceSnapshot::Present(Arc::new(Lab {
                        me: Some(test_machine("m16")),
                        accounts: vec![account],
                        ..Lab::default()
                    })))
                };
                let mut before = bed.images(cli);
                let mut replanned = false;
                let result = sync_cli_with_replan(
                    cli,
                    (&bed.home, &bed.state, &bed.config),
                    (None, None),
                    SyncCaller::Person,
                    1,
                    (&fetch_source_text, &[]),
                    || {
                        replanned = true;
                        match change {
                            #[cfg(unix)]
                            0 => bed.put(AI_POOL, "version=1\naccount=[]\n"),
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
                assert!(replanned, "{cli:?}/{change}/vault={vault}");
                assert!(
                    result.is_err(),
                    "{cli:?}/{change}/vault={vault}: {result:?}"
                );
                assert_eq!(bed.images(cli), before);
                assert!(!bed.store(cli).login_dir("new").unwrap().exists());
            }
        }
    }
}

#[test]
fn providers_retire_empty_replacements_and_preserve_explicit_inputs() {
    for cli in OTHER_CLIS {
        for native in [
            #[cfg(unix)]
            false,
            true,
        ] {
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
            // The CLI's fleet file an older aterm left in `~/.secrets`: the owner's backup, which
            // nothing reads and nothing rewrites.
            let legacy = bed.home.join(".secrets").join(cli_fleet_file(cli));
            fs::create_dir_all(legacy.parent().unwrap()).unwrap();
            fs::write(&legacy, "retained legacy bytes").unwrap();
            #[cfg(unix)]
            if !native {
                bed.put(AI_POOL, "version=1\naccount=[]\n");
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
        // An explicit input is the person's own file, outside the vault, and stays as it was.
        let explicit = bed.home.join("pool.toml");
        fs::write(&explicit, pool_text(cli)).unwrap();
        let input = fs::read(&explicit).unwrap();
        let result = sync_cli_with(
            cli,
            (&bed.home, &bed.state, &bed.config),
            (explicit.to_str(), None),
            1,
            (&fetch_source_text, &[]),
        );
        assert!(matches!(result, Ok(CliSync::Synced(_))), "{result:?}");
        assert!(!bed.store(cli).source_policy().unwrap().retired());
        assert_eq!(fs::read(&explicit).unwrap(), input);
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
        #[cfg(unix)]
        let legacy = cli_fleet_file(cli);
        #[cfg(unix)]
        bed.put(&legacy, "preserved legacy input");
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
        #[cfg(unix)]
        assert_eq!(bed.held(&legacy).as_deref(), Some("preserved legacy input"));
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
    #[cfg(not(unix))]
    assert_ne!(state["origin"], 0, "vault capture fixtures require Unix");
    let bed = Bed::new();
    #[cfg(unix)]
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
        #[cfg(unix)]
        4 if state["origin"] == 0 => bed.put(AI_POOL, "version=1\naccount=[]\n"),
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
                    #[cfg(not(unix))]
                    if action.name == "Admit" && state["origin"] == 0 {
                        continue;
                    }
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
    #[cfg(not(unix))]
    stale.insert("origin", 1);
    stale.insert("change", 2);
    stale.insert("stage", 1);
    let bypassed = transition_for(&stale, "Admit", Cli::Codex, true);
    assert!(!model.check_invariant("ChangedSourceRefused", &bypassed));
}
