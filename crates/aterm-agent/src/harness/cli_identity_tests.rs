// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::RefCell;

struct TerminalCodex {
    reported: Option<&'static str>,
    homes: RefCell<Vec<PathBuf>>,
    probes: RefCell<Vec<PathBuf>>,
    mutation: Option<(PathBuf, String, bool)>,
}
impl TerminalCodex {
    fn new(reported: Option<&'static str>) -> Self {
        Self {
            reported,
            homes: RefCell::new(Vec::new()),
            probes: RefCell::new(Vec::new()),
            mutation: None,
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
        self.reported.map(str::to_owned)
    }
    fn vendor_sign_in(
        &self,
        req: &crate::harness::login_drive::Request<'_>,
    ) -> crate::harness::login_drive::Drove {
        self.vendor_login(req).unwrap();
        crate::harness::login_drive::Drove {
            outcome: crate::harness::login_drive::Outcome::SignedIn {
                email: self.reported.map(str::to_owned),
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
fn duplicate_canonical_identities_and_unsupported_alias_options_refuse() {
    let text = "[[account]]\nlabel='one'\nclass='dedicated'\ndir='/one'\nemail='alias@example.test'\nidentity_email='owner@example.test'\n\n[[account]]\nlabel='two'\nclass='dedicated'\ndir='/two'\nemail='OWNER@example.test'\n";
    assert!(Roster::parse_for(Cli::Codex, text).is_err());
    assert!(Roster::parse_for(Cli::Claude, text).is_err());
    assert!(
        parsed(
            "accounts add account --email alias@example.test --identity-email owner@example.test"
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
            "account",
            "owner@example.test",
            staging.to_str().unwrap(),
            None,
            proof,
            env.now - 1,
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
            (&roster, &path),
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
