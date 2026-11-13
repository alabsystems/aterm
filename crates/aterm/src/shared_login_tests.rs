// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_db::{accounts::proposals::Proposals, database::Change};
use aterm_json::json;
use aterm_secrets::commons::bootstrap::Bootstrap;

#[test]
fn shared_command_requires_explicit_nonzero_identifiers_without_duplicate_options() {
    let args = vec![
        "m3.2".into(),
        "--account".into(),
        "11".repeat(32).into(),
        "--decision".into(),
        "22".repeat(32).into(),
        "--request".into(),
        "33".repeat(16).into(),
    ];
    let parsed = parse(&args).unwrap();
    assert_eq!(parsed.label, "m3.2");
    assert_eq!(parsed.account, [0x11; 32]);
    assert_eq!(parsed.decision, [0x22; 32]);
    assert_eq!(parsed.request, [0x33; 16]);
    assert!(parse(&args[..5]).is_err());
    let mut invalid = args.clone();
    invalid[6] = "00".repeat(16).into();
    assert!(parse(&invalid).is_err());
    let mut duplicate = args.clone();
    duplicate.extend(["--request".into(), "44".repeat(16).into()]);
    assert!(parse(&duplicate).is_err());
    let mut unknown = args;
    unknown.extend(["--automatic".into(), "yes".into()]);
    assert!(parse(&unknown).is_err());
}

fn fixture() -> (aterm_tempfile::TempDir, Approved, accounts::Row) {
    fixture_with_extra(None, false, None)
}

fn environment(home: &std::path::Path) -> cli::Env {
    cli::Env {
        state: home.join("harness"),
        aterm_state: None,
        cwd: home.to_owned(),
        home: Some(home.to_owned()),
        claude_dir: None,
        claude_config_unusable: None,
        claude_profile: None,
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        codex_home: None,
        xdg_config_home: None,
        codex_login: None,
        agy_conversation: None,
        now: 1_700_000_000,
        sid: String::new(),
        sock: None,
        config: Some(home.join("aterm.toml")),
        claude_tmp: None,
        claude_running: None,
        tz_s: 0,
    }
}

fn write_roster(env: &cli::Env, row: &accounts::Row) {
    let roster = env.accounts_roster().unwrap();
    std::fs::create_dir_all(roster.parent().unwrap()).unwrap();
    std::fs::write(
        roster,
        accounts::append_rows("", std::slice::from_ref(row)).unwrap(),
    )
    .unwrap();
}

fn fixture_with_extra(
    auth: Option<aterm_json::Value>,
    unknown_requirement: bool,
    browser: Option<&str>,
) -> (aterm_tempfile::TempDir, Approved, accounts::Row) {
    fixture_with_identity(
        auth,
        unknown_requirement,
        browser,
        json!({"address":"provider@example.test"}),
    )
}

fn fixture_with_identity(
    auth: Option<aterm_json::Value>,
    unknown_requirement: bool,
    browser: Option<&str>,
    identity: aterm_json::Value,
) -> (aterm_tempfile::TempDir, Approved, accounts::Row) {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&home.path().join("machine"));
    client::create(
        &paths,
        home.path().join("test.vault").to_str().unwrap(),
        None,
        Some("m1"),
    )
    .unwrap();
    let proposals = Proposals::at(paths.clone());
    let mut account = json!({
        "provider":"anthropic","method":"oauth","email":"alias@example.test",
        "details": {"provider-identity":{"kind":"email-ref","value":"canonical"},"provider-account-id":{"kind":"requirement","value":"stable-provider-id"}}
    });
    if let Some(auth) = auth {
        account.as_object_mut().unwrap().insert("auth".into(), auth);
    }
    if unknown_requirement {
        account
            .as_object_mut()
            .unwrap()
            .get_mut("details")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(
                "extra-challenge".into(),
                json!({"kind":"requirement","value":"sms"}),
            );
    }
    if let Some(browser) = browser {
        account
            .as_object_mut()
            .unwrap()
            .get_mut("details")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(
                "browser-profile".into(),
                json!({"kind":"requirement","value":browser}),
            );
    }
    proposals
        .write(
            [1; 16],
            vec![
                Change {
                    collection: "accounts".into(),
                    key: "one".into(),
                    value: Some(account),
                },
                Change {
                    collection: "email-addresses".into(),
                    key: "canonical".into(),
                    value: Some(identity),
                },
            ],
        )
        .unwrap();
    let db = Activations::at(paths.clone());
    let admin = client::admin_open(&paths).unwrap();
    let actor = Bootstrap::required(&paths)
        .unwrap()
        .request()
        .unwrap()
        .binding()
        .actor();
    let request = db
        .configuration(&admin, [7; 32], "one", actor, vec![])
        .unwrap();
    let (record, _) = db.issue(&admin, [2; 16], request).unwrap();
    let expected = client::config(&paths).unwrap().unwrap();
    let approved = Approved {
        paths,
        expected,
        account: [7; 32],
        decision: record.id(),
    };
    let row = accounts::Row {
        label: "local".into(),
        email: Some("alias@example.test".into()),
        identity_email: Some("provider@example.test".into()),
        browser: browser.map(str::to_owned),
        ..accounts::Row::default()
    };
    (home, approved, row)
}

#[test]
fn root_adapter_binds_direct_alias_and_principal_without_trusting_local_labels() {
    let (_home, approved, row) = fixture();
    let grant = approved.prepare(&row).unwrap();
    assert_eq!(grant.identity, "provider@example.test");
    assert_eq!(
        grant.expected_principal,
        aterm_agent::harness::profile::principal_id("stable-provider-id")
    );
    let mut wrong = row.clone();
    wrong.identity_email = None;
    assert!(approved.prepare(&wrong).is_err());
    wrong = row.clone();
    wrong.email = Some("another@example.test".into());
    assert!(approved.prepare(&wrong).is_err());
    wrong = row.clone();
    wrong.label = "renamed-local".into();
    assert_eq!(approved.prepare(&wrong).unwrap(), grant);
    Proposals::at(approved.paths.clone())
        .write(
            [3; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "one".into(),
                value: Some(json!({
                    "provider":"anthropic","method":"oauth","email":"redirected@example.test"
                })),
            }],
        )
        .unwrap();
    assert!(approved.current(&grant, &row).is_err());
}

#[test]
fn root_adapter_refuses_an_authentication_route_it_cannot_enforce() {
    let (_home, approved, row) = fixture_with_extra(
        Some(json!([{"kind":"phone","requirements":["sms"]}])),
        false,
        None,
    );
    let error = approved.prepare(&row).unwrap_err();
    assert!(
        error.contains("additional authentication routes"),
        "{error}"
    );
}

#[test]
fn prepared_inputs_recover_without_enabling_ordinary_shared_renewal() {
    use aterm_secrets::text::Audience;
    let (_home, mut approved, row) = fixture();
    let admin = client::admin_open(&approved.paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "mailbox-input", &aterm_json::to_vec(&json!({
        "schema":1,"kind":"mailbox","target":"alias@example.test",
        "transport":"https","endpoint":"https://mail.example.test/inbox","bearer_token":"synthetic-token"
    })).unwrap()).unwrap();
    client::admin_publish(&approved.paths, &admin, &doc).unwrap();
    approved.expected = client::config(&approved.paths).unwrap().unwrap();
    let proposals = Proposals::at(approved.paths.clone());
    let mut account = proposals.snapshot().unwrap().cells[&("accounts".into(), "one".into())]
        .candidates[0]
        .value
        .clone();
    account.as_object_mut().unwrap().insert(
        "secret_ref".into(),
        json!({"vault":approved.expected.name,"entry":"mailbox-input"}),
    );
    account
        .as_object_mut()
        .unwrap()
        .get_mut("details")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert(
            "login-input-format".into(),
            json!({"kind":"requirement","value":"mailbox-v1"}),
        );
    proposals
        .write(
            [12; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "one".into(),
                value: Some(account),
            }],
        )
        .unwrap();
    let admin = client::admin_open(&approved.paths).unwrap();
    let db = Activations::at(approved.paths.clone());
    let actor = Bootstrap::required(&approved.paths)
        .unwrap()
        .request()
        .unwrap()
        .binding()
        .actor();
    let request = db
        .configuration(
            &admin,
            approved.account,
            "one",
            actor,
            vec![approved.decision],
        )
        .unwrap();
    approved.decision = db.issue(&admin, [13; 16], request).unwrap().0.id();
    let grant = approved.prepare(&row).unwrap();
    assert!(approved.prepared().unwrap().credential().is_some());
    let candidates = automatic_candidates(&approved.paths.home).unwrap();
    assert_eq!(candidates.len(), 1);
    assert!(!candidates[0].ordinary_renewal);
    assert!(automatic_authority(&approved.paths.home, grant.source, &row).is_err());
    let recovery = checkpoint_authority(&approved.paths.home, grant.source, &row).unwrap();
    assert_eq!(recovery.prepare(&row).unwrap(), grant);
    recovery.current(&grant, &row).unwrap();
    let stop = db
        .stop_request(approved.account, vec![approved.decision])
        .unwrap();
    db.issue(&admin, [14; 16], stop).unwrap();
    assert!(recovery.current(&grant, &row).is_err());
    assert!(checkpoint_authority(&approved.paths.home, grant.source, &row).is_err());
}

#[test]
fn first_adoption_discovers_only_current_root_decisions_without_a_local_roster() {
    let (_home, approved, _) = fixture();
    let env = environment(&approved.paths.home);
    assert!(!env.accounts_roster().unwrap().exists());
    let candidates = automatic_candidates(&approved.paths.home).unwrap();
    assert_eq!(candidates.len(), 1);
    let candidate = &candidates[0];
    let grant = candidate.authority.prepare(&candidate.row).unwrap();
    assert_eq!(grant.source.account, approved.account);
    assert_eq!(grant.decision, approved.decision);
    assert_eq!(candidate.cli, accounts::Cli::Claude);
    assert_ne!(
        candidate.row.label, "one",
        "proposal keys are not local labels"
    );
    Proposals::at(approved.paths.clone())
        .write(
            [8; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "proposal-only".into(),
                value: Some(json!({
                    "provider":"anthropic", "method":"oauth",
                    "email":"unapproved@example.test"
                })),
            }],
        )
        .unwrap();
    assert_eq!(automatic_candidates(&approved.paths.home).unwrap().len(), 1);
    let db = Activations::at(approved.paths.clone());
    let admin = client::admin_open(&approved.paths).unwrap();
    let stop = db
        .stop_request(approved.account, vec![approved.decision])
        .unwrap();
    db.issue(&admin, [9; 16], stop).unwrap();
    assert!(
        automatic_candidates(&approved.paths.home)
            .unwrap()
            .is_empty()
    );

    let (_home, unsupported, _) = fixture_with_extra(None, true, None);
    assert!(
        automatic_candidates(&unsupported.paths.home)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn automatic_upkeep_resolves_only_the_pinned_network_and_current_root_head() {
    super::repository_catalog::tests::exercise_owned_upkeep();
    let (_home, approved, row) = fixture();
    let grant = approved.prepare(&row).unwrap();
    let automatic = automatic_authority(&approved.paths.home, grant.source, &row).unwrap();
    assert_eq!(automatic.prepare(&row).unwrap(), grant);
    let mut foreign = grant.source;
    foreign.network[0] ^= 1;
    assert!(automatic_authority(&approved.paths.home, foreign, &row).is_err());
    let db = Activations::at(approved.paths.clone());
    let admin = client::admin_open(&approved.paths).unwrap();
    let stop = db
        .stop_request(approved.account, vec![grant.decision])
        .unwrap();
    db.issue(&admin, [3; 16], stop).unwrap();
    assert!(automatic.current(&grant, &row).is_err());
    assert!(automatic_authority(&approved.paths.home, grant.source, &row).is_err());
}

#[test]
fn dependency_setup_rechecks_root_and_local_switches_before_the_provider() {
    use aterm_agent::harness::cli::shared_provider::{Context, Execution, Provider};
    use std::cell::Cell;

    struct Host<'a> {
        approved: &'a Approved,
        config: &'a std::path::Path,
        off: Option<&'a str>,
        checked: Cell<bool>,
        launched: Cell<usize>,
    }
    impl cli::AccountsHost for Host<'_> {
        fn automatic_sign_in(&self) -> bool {
            true
        }
        fn native_credential_setup(&self) -> bool {
            self.checked.set(true);
            if let Some(off) = self.off {
                std::fs::write(self.config, off).unwrap();
            } else {
                let db = Activations::at(self.approved.paths.clone());
                let admin = client::admin_open(&self.approved.paths).unwrap();
                let stop = db
                    .stop_request(self.approved.account, vec![self.approved.decision])
                    .unwrap();
                db.issue(&admin, [97; 16], stop).unwrap();
            }
            false
        }
        fn stdin_is_terminal(&self) -> bool {
            false
        }
        fn auth_status(&self, _: &str) -> Result<aterm_agent::harness::align::Captured, String> {
            panic!("no authentication read")
        }
        fn unaskable(&self) -> Option<String> {
            None
        }
        fn login(&self, _: Option<&str>) -> Result<i32, String> {
            panic!("no ordinary login")
        }
        fn now(&self) -> i64 {
            1_700_000_000
        }
        fn renew_flow(&self, _: &str, _: &str, _: &cli::RenewBrowser) -> Result<i32, String> {
            self.launched.set(self.launched.get() + 1);
            Err("unexpected provider launch".into())
        }
    }
    for off in [
        None,
        Some("[harness]\naccounts = \"off\"\n"),
        Some("[harness]\nlogins = \"off\"\n"),
        Some("[harness]\nenabled = false\n"),
    ] {
        let (home, approved, row) = fixture();
        let env = environment(home.path());
        let store = env.accounts_store();
        let host = Host {
            approved: &approved,
            config: env.config.as_deref().unwrap(),
            off,
            checked: Cell::new(false),
            launched: Cell::new(0),
        };
        let context = Context {
            publication: None,
            admission: None,
            env: &env,
            store: &store,
            host: &host,
        };
        assert!(matches!(
            approved.execute_attempt(&context, &row, home.path(), &cli::RenewBrowser::Default),
            Execution::NotStarted(_)
        ));
        assert!(host.checked.get());
        assert_eq!(host.launched.get(), 0);
    }
}

#[test]
fn root_adapter_refuses_an_unrecognized_required_detail() {
    let (_home, approved, row) = fixture_with_extra(None, true, None);
    let error = approved.prepare(&row).unwrap_err();
    assert!(error.contains("unsupported required detail"), "{error}");
}

#[test]
fn referenced_identity_requirements_are_not_silently_accepted() {
    use std::cell::Cell;

    struct Host(Cell<usize>);
    impl cli::AccountsHost for Host {
        fn stdin_is_terminal(&self) -> bool {
            true
        }
        fn auth_status(&self, _: &str) -> Result<aterm_agent::harness::align::Captured, String> {
            panic!("no live authentication probe")
        }
        fn unaskable(&self) -> Option<String> {
            None
        }
        fn login(&self, _: Option<&str>) -> Result<i32, String> {
            panic!("no ordinary login")
        }
        fn now(&self) -> i64 {
            1_700_000_000
        }
        fn renew_flow(&self, _: &str, _: &str, _: &cli::RenewBrowser) -> Result<i32, String> {
            self.0.set(self.0.get() + 1);
            Err("inert provider failed after admission".into())
        }
    }

    for (key, kind, accepted) in [
        ("extra-challenge", "requirement", false),
        ("provider-account-id", "requirement", false),
        ("extra-challenge", "note", true),
        ("extra-challenge", "status", true),
    ] {
        let (home, approved, row) = fixture_with_identity(
            None,
            false,
            None,
            json!({
                "address":"provider@example.test",
                "details":{(key):{"kind":kind,"value":"hardware approval"}}
            }),
        );
        assert_eq!(
            approved.prepare(&row).is_ok(),
            accepted,
            "identity detail kind={kind}"
        );
        let env = environment(home.path());
        write_roster(&env, &row);
        let host = Host(Cell::new(0));
        assert_ne!(
            cli::shared_renewal::run(
                &env,
                &host,
                &row.label,
                [1; 16],
                &approved,
                &mut Vec::new(),
                &mut Vec::new(),
            ),
            ExitCode::SUCCESS
        );
        assert_eq!(host.0.get(), usize::from(accepted), "{key}/{kind}");
        assert_eq!(
            env.accounts_store()
                .load_logins(env.now)
                .unwrap()
                .0
                .shared
                .is_empty(),
            !accepted,
            "only an admitted attempt consumes the shared grant"
        );
    }
}

#[test]
fn unused_contact_requirements_do_not_restrict_the_selected_identity() {
    for referenced_identity in [true, false] {
        let (_home, mut approved, mut row) = fixture();
        let proposals = Proposals::at(approved.paths.clone());
        let snapshot = proposals.snapshot().unwrap();
        let mut account = snapshot.cells[&("accounts".into(), "one".into())].candidates[0]
            .value
            .clone();
        account
            .as_object_mut()
            .unwrap()
            .insert("identities".into(), json!([{"kind":"email","id":"unused"}]));
        if !referenced_identity {
            account
                .as_object_mut()
                .unwrap()
                .get_mut("details")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove("provider-identity");
            row.identity_email = None;
        }
        proposals
            .write(
                [3; 16],
                vec![
                    Change {
                        collection: "accounts".into(),
                        key: "one".into(),
                        value: Some(account),
                    },
                    Change {
                        collection: "email-addresses".into(),
                        key: "unused".into(),
                        value: Some(json!({
                            "address":"unused@example.test",
                            "details":{"challenge":{"kind":"requirement","value":"sms"}}
                        })),
                    },
                ],
            )
            .unwrap();
        let db = Activations::at(approved.paths.clone());
        let admin = client::admin_open(&approved.paths).unwrap();
        let actor = Bootstrap::required(&approved.paths)
            .unwrap()
            .request()
            .unwrap()
            .binding()
            .actor();
        let request = db
            .configuration(
                &admin,
                approved.account,
                "one",
                actor,
                vec![approved.decision],
            )
            .unwrap();
        approved.decision = db.issue(&admin, [4; 16], request).unwrap().0.id();
        let prepared = db.prepare(approved.account, &approved.expected).unwrap();
        assert!(
            prepared
                .configuration()
                .get("email-addresses", "unused")
                .is_some()
        );
        assert_eq!(
            approved.prepare(&row).unwrap().identity,
            row.expected_identity_email().unwrap()
        );
    }
}

#[test]
fn root_adapter_retains_the_browser_requirement_and_rejects_a_different_row() {
    let (_home, approved, mut row) = fixture_with_extra(None, false, Some("Required profile"));
    let grant = approved.prepare(&row).unwrap();
    assert_eq!(
        grant.required_browser_profile.as_deref(),
        Some("Required profile")
    );
    row.browser = Some("Other profile".into());
    assert!(approved.prepare(&row).is_err());
}

#[test]
fn a_required_browser_profile_cannot_fall_back_to_the_default_browser() {
    use std::cell::Cell;

    struct Host(Cell<usize>);
    impl cli::AccountsHost for Host {
        fn stdin_is_terminal(&self) -> bool {
            true
        }
        fn auth_status(&self, _: &str) -> Result<aterm_agent::harness::align::Captured, String> {
            panic!("no live status")
        }
        fn unaskable(&self) -> Option<String> {
            None
        }
        fn login(&self, _: Option<&str>) -> Result<i32, String> {
            panic!("no ordinary login")
        }
        fn now(&self) -> i64 {
            1_700_000_000
        }
        fn renew_flow(&self, _: &str, _: &str, _: &cli::RenewBrowser) -> Result<i32, String> {
            self.0.set(self.0.get() + 1);
            Err("injected provider stop".into())
        }
    }

    let (home, approved, row) = fixture_with_extra(None, false, Some("Required profile"));
    let env = environment(home.path());
    write_roster(&env, &row);
    let host = Host(Cell::new(0));
    assert_ne!(
        cli::shared_renewal::run(
            &env,
            &host,
            &row.label,
            [1; 16],
            &approved,
            &mut Vec::new(),
            &mut Vec::new()
        ),
        ExitCode::SUCCESS
    );
    assert_eq!(host.0.get(), 0);
    assert!(
        env.accounts_store()
            .load_logins(env.now)
            .unwrap()
            .0
            .shared
            .is_empty()
    );
}

#[test]
fn offline_sync_policy_preserves_one_shot_authority_and_observed_stops() {
    use std::cell::Cell;

    struct Host(Cell<usize>);
    impl cli::AccountsHost for Host {
        fn stdin_is_terminal(&self) -> bool {
            true
        }
        fn auth_status(&self, _: &str) -> Result<aterm_agent::harness::align::Captured, String> {
            panic!("no live authentication probe")
        }
        fn unaskable(&self) -> Option<String> {
            None
        }
        fn login(&self, _: Option<&str>) -> Result<i32, String> {
            panic!("no ordinary login")
        }
        fn now(&self) -> i64 {
            1_700_000_000
        }
        fn renew_flow(&self, _: &str, _: &str, _: &cli::RenewBrowser) -> Result<i32, String> {
            self.0.set(self.0.get() + 1);
            Err("inert provider failed after admission".into())
        }
    }

    struct StopAfterPrepare<'a> {
        approved: &'a Approved,
        stop: bool,
        stopped: Cell<bool>,
        checked: Cell<usize>,
        prepared: Cell<usize>,
        current_passed: Cell<usize>,
    }
    impl Authority for StopAfterPrepare<'_> {
        fn prepare(&self, row: &accounts::Row) -> Result<Grant, String> {
            let grant = self.approved.prepare(row)?;
            self.prepared.set(self.prepared.get() + 1);
            if self.stop && !self.stopped.replace(true) {
                let db = Activations::at(self.approved.paths.clone());
                let admin = client::admin_open(&self.approved.paths).unwrap();
                let stop = db
                    .stop_request(self.approved.account, vec![grant.decision])
                    .unwrap();
                db.issue(&admin, [3; 16], stop).unwrap();
            }
            Ok(grant)
        }
        fn current(&self, grant: &Grant, row: &accounts::Row) -> Result<(), String> {
            self.checked.set(self.checked.get() + 1);
            let result = self.approved.current(grant, row);
            self.current_passed
                .set(self.current_passed.get() + usize::from(result.is_ok()));
            result
        }
    }

    // `[commons] exclude_tables` is no arm of its own: `Policy::includes` gives the same false
    // as `enabled=false` for these tables, and renewal reads no `[commons]`. A renewal path
    // that read `exclude_tables` otherwise than `enabled=false` would go unseen here.
    for (policy, config) in ["[commons]\n", "[commons]\nenabled=false\n"]
        .into_iter()
        .enumerate()
    {
        for stop in [false, true] {
            let (home, approved, row) = fixture();
            let env = environment(home.path());
            write_roster(&env, &row);
            let grant = approved.prepare(&row).unwrap();
            let proposals = Proposals::at(approved.paths.clone())
                .snapshot()
                .unwrap()
                .scope
                .digest();
            let activations = Activations::at(approved.paths.clone())
                .initialize()
                .unwrap()
                .digest();
            aterm_uds::atomic_file::write(env.config.as_ref().unwrap(), config.as_bytes(), 0o600)
                .unwrap();
            {
                let bootstrap = Bootstrap::required(&approved.paths).unwrap();
                let session = aterm_db::table::Session::open(&bootstrap).unwrap();
                let directory =
                    aterm_db::catalog::Directory::new(&approved.paths, bootstrap.trust());
                let observed = directory
                    .snapshot_with_config(&session, env.config.as_ref().unwrap())
                    .unwrap();
                let expected = if policy == 0 {
                    aterm_db::catalog::State::Subscribed
                } else {
                    aterm_db::catalog::State::Excluded
                };
                for table in [proposals, activations] {
                    assert_eq!(observed.entries[&table].state, expected);
                }
            }
            let authority = StopAfterPrepare {
                approved: &approved,
                stop,
                stopped: Cell::new(false),
                checked: Cell::new(0),
                prepared: Cell::new(0),
                current_passed: Cell::new(0),
            };
            let host = Host(Cell::new(0));
            let mut output = Vec::new();
            let mut errors = Vec::new();
            assert_ne!(
                cli::shared_renewal::run(
                    &env,
                    &host,
                    &row.label,
                    [1; 16],
                    &authority,
                    &mut output,
                    &mut errors
                ),
                ExitCode::SUCCESS
            );
            assert!(
                authority.checked.get() > 0,
                "current root authority must be checked: {}",
                String::from_utf8_lossy(&errors)
            );
            assert_eq!(
                host.0.get(),
                usize::from(!stop),
                "policy={policy} stop={stop}: {}",
                String::from_utf8_lossy(&errors)
            );
            let store = env.accounts_store();
            let logins = store.load_logins(env.now).unwrap().0;
            let attempt = logins.shared.latest(grant.source).cloned();
            if stop {
                assert_eq!(authority.checked.get(), 1);
                assert_eq!(authority.current_passed.get(), 0);
                assert!(attempt.is_none(), "a stopped grant must not be consumed");
                assert!(matches!(
                    Activations::at(approved.paths.clone())
                        .state(approved.account)
                        .unwrap(),
                    aterm_db::accounts::activations::State::Stopped(_)
                ));
            } else {
                assert_eq!(authority.current_passed.get(), authority.checked.get());
                assert!(
                    String::from_utf8_lossy(&errors)
                        .contains("inert provider failed after admission")
                );
                let attempt = attempt.as_ref().unwrap();
                assert_eq!(attempt.intent.request, [1; 16]);
                assert_eq!(attempt.intent.source, grant.source);
                assert_eq!(attempt.intent.decision, grant.decision);
                assert_eq!(attempt.intent.generation, grant.generation);
                assert_eq!(attempt.intent.configuration, grant.configuration);
                assert_eq!(attempt.intent.identity, grant.identity);
                assert_eq!(attempt.intent.label, row.label);
                assert_eq!(attempt.outcome, accounts::shared::Outcome::Unknown);
                assert_eq!(
                    logins
                        .shared
                        .principal_source(grant.expected_principal.unwrap()),
                    Some(grant.source)
                );
            }
            output.clear();
            errors.clear();
            let checked_before_retry = authority.checked.get();
            assert_ne!(
                cli::shared_renewal::run(
                    &env,
                    &host,
                    &row.label,
                    [2; 16],
                    &authority,
                    &mut output,
                    &mut errors
                ),
                ExitCode::SUCCESS
            );
            assert_eq!(
                host.0.get(),
                usize::from(!stop),
                "a fresh nonce must not replay the root grant"
            );
            if stop {
                assert_eq!(authority.checked.get(), checked_before_retry);
            } else {
                assert!(
                    authority.checked.get() > checked_before_retry,
                    "advisory recovery must recheck the current root authority"
                );
                assert_eq!(authority.current_passed.get(), authority.checked.get());
            }
            assert_eq!(authority.prepared.get(), 1 + usize::from(!stop));
            let retried = store.load_logins(env.now).unwrap().0;
            assert_eq!(retried.shared.latest(grant.source), attempt.as_ref());
            assert_eq!(
                std::fs::read(env.config.as_ref().unwrap()).unwrap(),
                config.as_bytes()
            );
        }
    }
}
