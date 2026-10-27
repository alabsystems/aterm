// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::RefCell;

struct TerminalCodex {
    reported: Option<&'static str>,
    homes: RefCell<Vec<PathBuf>>,
    probes: RefCell<Vec<PathBuf>>,
    mutation: Option<(PathBuf, String, bool)>,
    credential_fault: Option<bool>,
}
impl TerminalCodex {
    fn new(reported: Option<&'static str>) -> Self {
        Self {
            reported,
            homes: RefCell::new(Vec::new()),
            probes: RefCell::new(Vec::new()),
            mutation: None,
            credential_fault: None,
        }
    }
}
impl AccountsHost for TerminalCodex {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no Claude probe")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no Claude login")
    }
    fn now(&self) -> i64 {
        1_758_412_900
    }
    fn vendor_login(&self, req: &crate::harness::login_drive::Request<'_>) -> Result<i32, String> {
        assert_eq!(req.cli, Cli::Codex);
        self.homes.borrow_mut().push(req.staging.to_path_buf());
        std::fs::write(req.staging.join("auth.json"), "isolated fixture login").unwrap();
        if let Some((path, text, false)) = &self.mutation {
            std::fs::write(path, text).unwrap();
        }
        Ok(0)
    }
    fn codex_account(&self, home: &Path) -> Option<String> {
        assert!(home.join("auth.json").is_file());
        assert_eq!(self.homes.borrow().last().map(PathBuf::as_path), Some(home));
        self.probes.borrow_mut().push(home.to_path_buf());
        if let Some((path, text, true)) = &self.mutation {
            std::fs::write(path, text).unwrap();
        }
        if let Some(replace_with_directory) = self.credential_fault {
            let credential = home.join("auth.json");
            std::fs::rename(&credential, home.join("auth.retained")).unwrap();
            if replace_with_directory {
                std::fs::create_dir(credential).unwrap();
            }
        }
        self.reported.map(str::to_owned)
    }
    fn vendor_sign_in(
        &self,
        req: &crate::harness::login_drive::Request<'_>,
    ) -> crate::harness::login_drive::Drove {
        self.vendor_login(req).unwrap();
        crate::harness::login_drive::Drove {
            outcome: crate::harness::login_drive::Outcome::SignedIn {
                // A missing driver identity makes renewal use the same staging
                // probe as terminal login, where this fixture injects its fault.
                email: if self.credential_fault.is_some() {
                    None
                } else {
                    self.reported.map(str::to_owned)
                },
                page: false,
            },
            host: None,
        }
    }
}

fn command(line: &str, env: &Env, host: &dyn AccountsHost) -> (String, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let Cmd::AccountsOf(of) = parsed(line).unwrap() else {
        panic!("Codex account command")
    };
    let code = run_accounts_of(&of, env, host, &mut out, &mut err);
    (
        format!("{code:?}"),
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn codex_login_alias_is_separate_from_reported_identity_and_round_trips() {
    let tmp = tmp_dir("codex-login-identity");
    let env = env_at(&tmp);
    let (code, _, err) = run_line(
        "accounts add account --cli codex --email login+alias@example.test --identity-email owner@example.test",
        &env,
    );
    assert_eq!(code, exit(0), "{err}");
    let host = TerminalCodex::new(Some(" OWNER@example.test\n"));
    let (code, out, err) = command("accounts login account --cli codex", &env, &host);
    assert_eq!(code, exit(0), "{out}{err}");
    assert_eq!(host.probes.borrow().len(), 1);
    let store = env.accounts_store_for(Cli::Codex).unwrap();
    let (logins, _) = store.load_logins(host.now()).unwrap();
    let roster = Roster::load_for(Cli::Codex, &tmp.path().join("accounts-codex.toml")).unwrap();
    let row = roster.row("account").unwrap();
    assert_eq!(row.email.as_deref(), Some("login+alias@example.test"));
    assert_eq!(row.identity_email.as_deref(), Some("owner@example.test"));
    let bound = accounts::bound(&roster, &logins);
    let binding = logins.get("account").unwrap();
    assert_eq!(
        binding.identity_proof_for(bound.row("account").unwrap()),
        Some(accounts::Proof::Status)
    );
    assert_eq!(binding.proved_email.as_deref(), Some("owner@example.test"));
    let (code, json, err) = command("accounts list --json --cli codex", &env, &host);
    assert_eq!(code, exit(0), "{err}");
    let report: aterm_json::Value = aterm_json::from_str(&json).unwrap();
    let account = report["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["label"].as_str() == Some("account"))
        .unwrap();
    assert_eq!(account["email"].as_str(), Some("login+alias@example.test"));
    assert_eq!(
        account["identity_email"].as_str(),
        Some("owner@example.test")
    );
    assert_eq!(account["identity_proof"].as_str(), Some("status"));
    let mut changed = bound.row("account").unwrap().clone();
    changed.identity_email = Some("another@example.test".into());
    assert_eq!(binding.identity_proof_for(&changed), None);
    changed = bound.row("account").unwrap().clone();
    changed.dir = Some("/a/different/login".into());
    assert_eq!(binding.identity_proof_for(&changed), None);
    let legacy = accounts::Binding {
        dir: binding.dir.clone(),
        proved_by: Some(accounts::Proof::Status),
        ..accounts::Binding::default()
    };
    assert_eq!(
        legacy.identity_proof_for(bound.row("account").unwrap()),
        None
    );
}

#[test]
fn terminal_codex_refuses_wrong_or_missing_identity_without_replacing_a_binding() {
    for reported in [Some("another@example.test"), None] {
        let tmp = tmp_dir("codex-identity-refusal");
        let env = env_at(&tmp);
        let (code, _, err) = run_line(
            "accounts add account --cli codex --email owner@example.test",
            &env,
        );
        assert_eq!(code, exit(0), "{err}");
        let good = TerminalCodex::new(Some("owner@example.test"));
        assert_eq!(
            command("accounts login account --cli codex", &env, &good).0,
            exit(0)
        );
        let store = env.accounts_store_for(Cli::Codex).unwrap();
        let before = store
            .load_logins(good.now())
            .unwrap()
            .0
            .get("account")
            .unwrap()
            .clone();
        let bad = TerminalCodex::new(reported);
        let (code, _, err) = command("accounts login account --cli codex", &env, &bad);
        assert_eq!(code, exit(1), "{err}");
        let after = store
            .load_logins(bad.now())
            .unwrap()
            .0
            .get("account")
            .unwrap()
            .clone();
        assert_eq!(after.dir, before.dir);
        assert_eq!(after.proved_email, before.proved_email);
        assert_eq!(after.renewed_at, before.renewed_at);
        assert!(
            bad.homes.borrow()[0].join("auth.json").is_file(),
            "the completed login is retained"
        );
    }
}

#[test]
fn a_credential_lost_during_the_identity_probe_cannot_publish() {
    for verb in ["login", "renew"] {
        for prior in [false, true] {
            for replace_with_directory in [false, true] {
                let tmp = tmp_dir("codex-lost-credential-publication");
                let env = env_at(&tmp);
                assert_eq!(
                    run_line(
                        "accounts add account --cli codex --email owner@example.test",
                        &env
                    )
                    .0,
                    exit(0)
                );
                if prior {
                    let good = TerminalCodex::new(Some("owner@example.test"));
                    assert_eq!(
                        command("accounts login account --cli codex", &env, &good).0,
                        exit(0)
                    );
                }
                let store = env.accounts_store_for(Cli::Codex).unwrap();
                let before = store.load_logins(env.now).unwrap().0;
                let mut host = TerminalCodex::new(Some("owner@example.test"));
                host.credential_fault = Some(replace_with_directory);
                let (code, out, err) =
                    command(&format!("accounts {verb} account --cli codex"), &env, &host);
                assert_eq!(host.probes.borrow().len(), 1);
                assert_eq!(code, exit(1), "{verb}, prior={prior}: {out}{err}");
                assert_eq!(store.load_logins(env.now).unwrap().0, before);
                let staging = host.homes.borrow()[0].clone();
                assert_eq!(
                    std::fs::read(staging.join("auth.retained")).unwrap(),
                    b"isolated fixture login"
                );
                assert_eq!(staging.join("auth.json").is_dir(), replace_with_directory);
                assert!(!staging.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
            }
        }
    }
}

#[test]
fn plain_claude_login_refuses_stale_policy_and_binding_for_every_auth_outcome() {
    for change in ["email", "removed", "malformed", "missing", "binding"] {
        for outcome in ["signed-in", "signed-out", "failed", "during-status"] {
            let tmp = tmp_dir("plain-login-policy");
            let env = env_at(&tmp);
            assert_eq!(acc_quiet(add("work", Class::Dedicated), &env).0, exit(0));
            let store = env.accounts_store();
            let path = env.accounts_roster().unwrap();
            let original = std::fs::read_to_string(&path).unwrap();
            let original_dir = store.login_dir("work").unwrap();
            let after = std::cell::RefCell::new(None);
            let snapshot = || {
                [
                    store.state_file(),
                    store.logins_file(),
                    store.ledger_path(),
                    store.wake_path(),
                ]
                .map(|path| std::fs::read(path).ok())
            };
            let mutate = || {
                match change {
                    "email" => std::fs::write(
                        &path,
                        format!("{original}\nemail = 'changed@example.test'\n"),
                    )
                    .unwrap(),
                    "removed" => std::fs::write(&path, "").unwrap(),
                    "malformed" => std::fs::write(&path, "[[broken").unwrap(),
                    "missing" => std::fs::rename(&path, path.with_extension("kept")).unwrap(),
                    "binding" => {
                        store
                            .transact_logins(env.now, &mut None, |state, logins| {
                                Ok(accounts::note_renewed(
                                    state,
                                    logins,
                                    "work",
                                    original_dir.to_str(),
                                    "/concurrent-login",
                                    None,
                                    env.now,
                                ))
                            })
                            .unwrap();
                    }
                    _ => unreachable!(),
                }
                *after.borrow_mut() = Some(snapshot());
            };
            let status = |_: &str| {
                assert_ne!(outcome, "failed", "failed login must not query status");
                if outcome == "during-status" {
                    mutate();
                }
                said(outcome != "signed-out")
            };
            let mut host = FakeHost::new(&status);
            host.tty = true;
            host.writes_home = true;
            host.login = Ok(if outcome == "failed" { 1 } else { 0 });
            host.after_login = (outcome != "during-status").then_some(&mutate);
            let (code, out, err) = acc(
                AccountsCmd::Login {
                    label: "work".into(),
                },
                &env,
                &host,
            );
            assert_eq!(code, exit(1), "{change}/{outcome}: {err}");
            assert_eq!(
                snapshot(),
                after.borrow().clone().unwrap(),
                "{change}/{outcome}"
            );
            assert!(!out.contains("work: signed in"));
            assert!(!original_dir.join(".claude.json").is_file());
            let staging = PathBuf::from(host.logins.borrow()[0].as_ref().unwrap());
            assert!(staging.join(".claude.json").is_file());
            assert!(!staging.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
        }
    }
}

#[test]
fn plain_claude_login_preserves_the_bound_directory_and_adopts_only_staging() {
    let tmp = tmp_dir("plain-login-bound-directory");
    let env = env_at(&tmp);
    let (old, _) = three_accounts(&tmp, &env);
    let fresh = tmp.path().join("fresh-login");
    login_made_in(&fresh);
    let previous = every_file(&fresh);
    let store = env.accounts_store();
    store
        .transact_logins(env.now, &mut None, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                "work-2",
                Some(&old),
                fresh.to_str().unwrap(),
                None,
                env.now,
            ))
        })
        .unwrap();
    let status = |_: &str| said(true);
    let mut host = FakeHost::new(&status);
    host.tty = true;
    host.writes_home = true;
    let (code, _, err) = acc(
        AccountsCmd::Login {
            label: "work-2".into(),
        },
        &env,
        &host,
    );
    assert_eq!(code, exit(0), "{err}");
    let staging = host.logins.borrow()[0].as_ref().unwrap().clone();
    assert_ne!(staging, fresh.to_str().unwrap());
    assert_eq!(*host.asked.borrow(), vec![staging.clone()]);
    assert_eq!(every_file(&fresh), previous);
    assert_eq!(
        store
            .load_logins(env.now)
            .unwrap()
            .0
            .get("work-2")
            .unwrap()
            .dir
            .as_deref(),
        Some(staging.as_str())
    );
}

#[test]
fn a_claude_identity_expectation_cannot_use_unchecked_plain_login() {
    let tmp = tmp_dir("claude-identity-plain-login");
    let env = env_at(&tmp);
    std::fs::write(
        env.accounts_roster().unwrap(),
        "[[account]]\nlabel='account'\nclass='dedicated'\nidentity_email='owner@example.test'\n",
    )
    .unwrap();
    let mut host = FakeHost::new(&never_asked);
    host.tty = true;
    let (code, _, err) = acc(
        AccountsCmd::Login {
            label: "account".into(),
        },
        &env,
        &host,
    );
    assert_eq!(code, exit(1));
    assert!(err.contains("requires a checked identity"), "{err}");
    assert!(host.logins.borrow().is_empty());
    assert!(!env.accounts_store().state_file().exists());
}

#[test]
fn duplicate_canonical_identities_and_unsupported_alias_options_refuse() {
    let text = "[[account]]\nlabel='one'\nclass='dedicated'\ndir='/one'\nemail='alias@example.test'\nidentity_email='owner@example.test'\n\n[[account]]\nlabel='two'\nclass='dedicated'\ndir='/two'\nemail='OWNER@example.test'\n";
    assert!(Roster::parse_for(Cli::Codex, text).is_err());
    assert!(Roster::parse_for(Cli::Claude, text).is_err());
    assert!(
        parsed(
            "accounts add account --cli grok --email alias@example.test --identity-email owner@example.test"
        )
        .is_err()
    );
    assert!(
        parsed(
            "accounts adopt-default account --cli kimi --email alias@example.test --identity-email owner@example.test --region global"
        )
        .is_err()
    );
}

#[test]
fn changed_or_removed_policy_cannot_publish_a_completed_login() {
    for (verb, during_probe) in [("login", false), ("login", true), ("renew", false)] {
        for reported in [
            Some("owner@example.test"),
            Some("another@example.test"),
            None,
        ] {
            for replacement in [
                "",
                "invalid [ roster",
                "[[account]]\nlabel='account'\nclass='dedicated'\nidentity_email='another@example.test'\n",
            ] {
                let tmp = tmp_dir("codex-policy-changed");
                let env = env_at(&tmp);
                assert_eq!(
                    run_line(
                        "accounts add account --cli codex --email owner@example.test",
                        &env
                    )
                    .0,
                    exit(0)
                );
                let good = TerminalCodex::new(Some("owner@example.test"));
                assert_eq!(
                    command("accounts login account --cli codex", &env, &good).0,
                    exit(0)
                );
                let store = env.accounts_store_for(Cli::Codex).unwrap();
                let before = store.load_logins(env.now).unwrap().0;
                let mut changed = TerminalCodex::new(reported);
                changed.mutation = Some((
                    tmp.path().join("accounts-codex.toml"),
                    replacement.into(),
                    during_probe,
                ));
                assert_eq!(
                    command(
                        &format!("accounts {verb} account --cli codex"),
                        &env,
                        &changed
                    )
                    .0,
                    exit(1)
                );
                assert_eq!(store.load_logins(env.now).unwrap().0, before);
                let staging = changed.homes.borrow()[0].clone();
                assert!(staging.join("auth.json").is_file());
                assert!(!staging.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
            }
        }
    }
}

#[test]
fn invalid_adoption_identity_is_rejected_before_a_provider_probe() {
    let tmp = tmp_dir("codex-adopt-invalid-identity");
    let env = env_at(&tmp);
    let host = TerminalCodex::new(Some("owner@example.test"));
    assert_eq!(
        command(
            "accounts adopt-default account --cli codex --identity-email invalid",
            &env,
            &host
        )
        .0,
        exit(1)
    );
    assert!(host.probes.borrow().is_empty());
}

#[test]
fn codex_recovery_requires_scoped_identity_and_a_status_check() {
    for (proof, changed) in [
        (None, false),
        (Some(accounts::Proof::Unchecked), false),
        (Some(accounts::Proof::Status), false),
        (Some(accounts::Proof::Status), true),
    ] {
        let tmp = tmp_dir("codex-identity-recovery");
        let env = env_at(&tmp);
        assert_eq!(
            run_line(
                "accounts add account --cli codex --identity-email owner@example.test",
                &env
            )
            .0,
            exit(0)
        );
        let store = env.accounts_store_for(Cli::Codex).unwrap();
        let staging = store.claim_staging_dir("account").unwrap();
        std::fs::write(staging.join("auth.json"), "isolated fixture login").unwrap();
        accounts::write_renewal_checkpoint_for(
            Cli::Codex,
            None,
            "account",
            "owner@example.test",
            staging.to_str().unwrap(),
            None,
            proof,
            env.now - 1,
            &store.load_logins(env.now).unwrap().0,
        )
        .unwrap();
        let path = tmp.path().join("accounts-codex.toml");
        let roster = Roster::load_for(Cli::Codex, &path).unwrap();
        if changed {
            let text = std::fs::read_to_string(&path)
                .unwrap()
                .replace("owner@example.test", "changed@example.test");
            std::fs::write(&path, text).unwrap();
        }
        let recovered = crate::harness::accounts_sync::recover_staging_renewals_for(
            Cli::Codex,
            &store,
            &path,
            env.now,
        );
        assert_eq!(
            recovered,
            proof == Some(accounts::Proof::Status) && !changed
        );
        let logins = store.load_logins(env.now).unwrap().0;
        let bound = accounts::bound(&roster, &logins);
        assert_eq!(
            logins
                .get("account")
                .and_then(|b| b.identity_proof_for(bound.row("account").unwrap())),
            proof.filter(|p| *p == accounts::Proof::Status && !changed)
        );
        assert!(staging.join("auth.json").is_file());
    }
}

pub(crate) fn identity_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let tmp = tmp_dir("codex-identity-model");
    let env = env_at(&tmp);
    let (options, reported) = match action {
        "Exact" | "ChangedIdentity" | "ChangedDirectory" | "PolicyChanged" => {
            ("--email owner@example.test", Some("owner@example.test"))
        }
        "Alias" => (
            "--email alias@example.test --identity-email owner@example.test",
            Some("owner@example.test"),
        ),
        "Wrong" => ("--email owner@example.test", Some("another@example.test")),
        "Missing" => ("--email owner@example.test", None),
        "Anonymous" => ("", None),
        _ => panic!("unknown identity action"),
    };
    let (code, _, err) = run_line(&format!("accounts add account --cli codex {options}"), &env);
    assert_eq!(code, exit(0), "{err}");
    let mut host = TerminalCodex::new(reported);
    if action == "PolicyChanged" {
        host.mutation = Some((tmp.path().join("accounts-codex.toml"), String::new(), true));
    }
    let _ = command("accounts login account --cli codex", &env, &host);
    let store = env.accounts_store_for(Cli::Codex).unwrap();
    let (mut logins, _) = store.load_logins(host.now()).unwrap();
    let roster = Roster::load_for(Cli::Codex, &tmp.path().join("accounts-codex.toml")).unwrap();
    if action == "ChangedDirectory" {
        logins.labels.get_mut("account").unwrap().dir = Some("/another/login".into());
    }
    let bound = accounts::bound(&roster, &logins);
    let mut row = bound.row("account").cloned().unwrap_or_default();
    if action == "ChangedIdentity" {
        row.identity_email = Some("another@example.test".into());
    }
    let binding = logins.get("account");
    let mut next = state.clone();
    next.insert(
        "scenario",
        match action {
            "Exact" => 1,
            "Alias" => 2,
            "Wrong" => 3,
            "Missing" => 4,
            "Anonymous" => 5,
            "ChangedIdentity" => 6,
            "ChangedDirectory" => 7,
            "PolicyChanged" => 8,
            _ => unreachable!(),
        },
    );
    next.insert("bound", i64::from(binding.is_some_and(|b| b.dir.is_some())));
    next.insert(
        "proved",
        i64::from(binding.and_then(|b| b.identity_proof_for(&row)).is_some()),
    );
    next.insert(
        "kept",
        i64::from(host.homes.borrow()[0].join("auth.json").is_file()),
    );
    next
}

#[test]
fn identity_binding_conforms_and_a_fabricated_proof_is_rejected() {
    let model = aterm_models_harness::account_identity_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = identity_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {next:?}",
                    action.name
                );
            }
        }
    }
    let mut wrong = identity_transition(&model.init_state(), "Wrong");
    wrong.insert("bound", 1);
    assert!(!model.check_invariant("RefusalCannotBind", &wrong));
}

struct PublicationHost {
    credential: PathBuf,
    retained: PathBuf,
    fault: i64,
    clocks: std::cell::Cell<usize>,
}

impl AccountsHost for PublicationHost {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("publication uses the supplied provider proof")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("publication runs no provider login")
    }
    fn now(&self) -> i64 {
        assert_eq!(self.clocks.replace(self.clocks.get() + 1), 0);
        if self.fault > 0 {
            std::fs::rename(&self.credential, &self.retained).unwrap();
            if self.fault == 2 {
                std::fs::create_dir(&self.credential).unwrap();
            }
        }
        1_758_412_900
    }
}

fn publication_case(provider: i64, prior: bool, fault: i64) -> [(&'static str, i64); 5] {
    let cli = [Cli::Codex, Cli::Grok, Cli::Kimi][provider as usize];
    let tmp = tmp_dir("signin-publication-presence");
    let env = env_at(&tmp);
    let store = env.accounts_store_for(cli).unwrap();
    let path = env.accounts_roster_for(cli).unwrap();
    let row = accounts::Row {
        label: "account".into(),
        email: Some("owner@example.test".into()),
        vendor: accounts::Vendor {
            region: (cli == Cli::Kimi).then_some(crate::ai::connector::Region::Global),
            ..Default::default()
        },
        ..Default::default()
    };
    std::fs::write(&path, accounts::append_rows_for(cli, "", &[row]).unwrap()).unwrap();
    if prior {
        let old = tmp.path().join("prior-login");
        let credential = old.join(cli.connector().stores[0]);
        std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
        std::fs::write(credential, "prior login").unwrap();
        store
            .transact_logins(env.now, &mut None, |state, logins| {
                Ok(accounts::note_renewed(
                    state,
                    logins,
                    "account",
                    None,
                    old.to_str().unwrap(),
                    None,
                    env.now,
                ))
            })
            .unwrap();
    }
    let records = store.read_account_records(Some(&path), env.now).unwrap();
    let row = records.roster.row("account").unwrap();
    let plan = sign_in_plan(
        cli,
        &store,
        SignInPolicy {
            row,
            logins: &records.logins,
            roster_path: &path,
            own_dir: None,
        },
        env.now,
        "login",
        &mut Vec::new(),
        &mut Vec::new(),
    )
    .unwrap();
    let credential = plan.staging.join(cli.connector().stores[0]);
    std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
    std::fs::write(&credential, "prepared login").unwrap();
    let host = PublicationHost {
        retained: credential.with_extension("retained"),
        credential,
        fault,
        clocks: std::cell::Cell::new(0),
    };
    let snapshot = || {
        [
            path.clone(),
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.state_witness_file(),
            store.logins_witness_file(),
            store.dir.join("sync-commit.json"),
        ]
        .map(|path| std::fs::read(path).ok())
    };
    let before = snapshot();
    let mut err = Vec::new();
    let result = bind_signed_in(
        cli,
        env.now,
        &host,
        &store,
        (row, &path),
        None,
        &plan,
        accounts::Proof::Status,
        "login",
        &mut Vec::new(),
        &mut err,
    );
    assert_eq!(host.clocks.get(), 1);
    if fault > 0 {
        assert!(
            String::from_utf8(err)
                .unwrap()
                .contains("the staging login disappeared before publication"),
            "the refusal must be from the publication check"
        );
    }
    let evidence = if fault == 0 {
        &host.credential
    } else {
        &host.retained
    };
    let retained = std::fs::read(evidence).unwrap() == b"prepared login";
    if fault > 0 {
        assert_eq!(store.load_logins(env.now).unwrap().0, records.logins);
    }
    [
        ("phase", 1),
        ("published", i64::from(result == Renewal::Renewed)),
        ("changed", i64::from(before != snapshot())),
        (
            "checkpoint",
            i64::from(
                plan.staging
                    .join(accounts::RENEWAL_CHECKPOINT_FILE)
                    .is_file(),
            ),
        ),
        ("retained", i64::from(retained)),
    ]
}

pub(crate) fn publication_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherProvider" => {
            next.insert("provider", state["provider"] + 1);
        }
        "PriorBinding" => {
            next.insert("prior", 1);
        }
        "AnotherFault" => {
            next.insert("fault", state["fault"] + 1);
        }
        "Publish" => {
            for (key, value) in
                publication_case(state["provider"], state["prior"] == 1, state["fault"])
            {
                next.insert(key, value);
            }
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn signin_publication_conforms_and_missing_credentials_cannot_bind() {
    let model = aterm_models_harness::signin_publication_model();
    assert_eq!(model.mutant_dials(), (1..=8).collect::<Vec<_>>());
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 36);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = publication_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
            }
        }
    }
    for fault in [0, 1, 2] {
        let mut state = model.init_state();
        state.insert("fault", fault);
        let after = publication_transition(&state, "Publish");
        let faults = if fault == 0 {
            [
                ("published", 0, "ResultPolicy"),
                ("changed", 0, "CompleteRecord"),
                ("checkpoint", 0, "CompleteRecord"),
            ]
        } else {
            [
                ("published", 1, "ResultPolicy"),
                ("changed", 1, "RefusalPreserves"),
                ("checkpoint", 1, "RefusalPreserves"),
            ]
        };
        for (field, value, law) in faults
            .into_iter()
            .chain([("retained", 0, "EvidenceRetained")])
        {
            let mut bad = after.clone();
            bad.insert(field, value);
            assert!(!model.check_invariant(law, &bad));
        }
    }
}
