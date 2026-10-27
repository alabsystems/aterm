// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Composition of root-approved account data with the existing local login
//! workflow. Shared configuration never becomes an unchecked local roster row.

use aterm_agent::harness::{
    accounts,
    cli::{
        self,
        shared_renewal::{Authority, Grant},
    },
};
use aterm_db::accounts::activations::Activations;
use aterm_secrets::client::{self, Config, Paths};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, process::ExitCode};

const USAGE: &str = "aterm harness accounts shared-renew <local-label> --account <digest> --decision <digest> --request <nonce> [--config <aterm.toml>]\nRenews one existing local Claude OAuth row using an exact current root grant.\n";

struct Approved {
    paths: Paths,
    expected: Config,
    account: [u8; 32],
    decision: [u8; 32],
}

fn requirement<'a>(value: &'a aterm_json::Value, key: &str) -> Result<Option<&'a str>, String> {
    let Some(detail) = value.get("details").and_then(|details| details.get(key)) else {
        return Ok(None);
    };
    if detail["kind"].as_str() != Some("requirement") {
        return Err(format!("shared renewal detail {key} must be a requirement"));
    }
    detail["value"]
        .as_str()
        .map(Some)
        .ok_or_else(|| format!("invalid shared renewal detail {key}"))
}

fn has_unsupported_requirements(value: &aterm_json::Value, supported: &[&str]) -> bool {
    value
        .get("details")
        .and_then(|details| details.as_object())
        .is_some_and(|details| {
            details.iter().any(|(key, detail)| {
                detail["kind"].as_str() == Some("requirement") && !supported.contains(&key.as_str())
            })
        })
}

impl Approved {
    fn read(&self, row: &accounts::Row) -> Result<Grant, String> {
        if client::config(&self.paths)
            .map_err(|e| e.to_string())?
            .as_ref()
            != Some(&self.expected)
        {
            return Err("configured vault changed during shared renewal".into());
        }
        let prepared = Activations::at(self.paths.clone())
            .prepare(self.account, &self.expected)
            .map_err(|e| e.to_string())?;
        let record = prepared.decision();
        if record.id() != self.decision {
            return Err("shared renewal decision is no longer current".into());
        }
        let configuration = prepared.configuration();
        let value = configuration.account();
        if has_unsupported_requirements(
            value,
            &["local-class", "browser-profile", "provider-account-id"],
        ) {
            return Err("shared renewal contains an unsupported required detail".into());
        }
        if value["provider"].as_str() != Some("anthropic")
            || value["method"].as_str() != Some("oauth")
            || prepared.credential().is_some()
        {
            return Err("shared renewal currently supports local Claude OAuth without copying a shared credential".into());
        }
        let email = value["email"]
            .as_str()
            .filter(|s| accounts::email_refusal(s).is_none())
            .ok_or_else(|| "shared renewal requires a direct approved email".to_owned())?;
        let identity = match value
            .get("details")
            .and_then(|d| d.get("provider-identity"))
        {
            None => email,
            Some(detail) => {
                if detail["kind"].as_str() != Some("email-ref") {
                    return Err("provider-identity must be an email-ref".into());
                }
                let key = detail["value"]
                    .as_str()
                    .ok_or_else(|| "invalid approved identity reference".to_owned())?;
                let key = key.strip_prefix("email-addresses:").unwrap_or(key);
                let absent =
                    || "approved provider identity is absent from its checked closure".to_owned();
                let contact = configuration
                    .get("email-addresses", key)
                    .ok_or_else(absent)?;
                let address = contact["address"]
                    .as_str()
                    .filter(|s| accounts::email_refusal(s).is_none())
                    .ok_or_else(absent)?;
                if has_unsupported_requirements(contact, &[]) {
                    return Err(
                        "shared renewal cannot enforce a required provider identity detail".into(),
                    );
                }
                address
            }
        };
        if !row
            .email
            .as_deref()
            .is_some_and(|e| accounts::email_matches(e, email))
            || !row
                .expected_identity_email()
                .is_some_and(|e| accounts::email_matches(e, identity))
        {
            return Err(
                "local row does not match the approved sign-in and provider identity".into(),
            );
        }
        let browser = requirement(value, "browser-profile")?;
        if browser.is_some_and(|browser| browser.trim().is_empty()) {
            return Err("invalid required browser profile".into());
        }
        if requirement(value, "local-class")?.is_some_and(|class| class != row.class.as_str())
            || browser.is_some_and(|browser| Some(browser) != row.browser.as_deref())
        {
            return Err("local account policy differs from the approved configuration".into());
        }
        if value.get("realm").is_some_and(|v| !v.is_null())
            || value
                .get("auth")
                .is_some_and(|routes| routes.as_array().is_none_or(|routes| !routes.is_empty()))
        {
            return Err(
                "shared renewal does not support custom realms or additional authentication routes"
                    .into(),
            );
        }
        let principal = match requirement(value, "provider-account-id")? {
            None => None,
            Some(value) => Some(
                aterm_agent::harness::profile::principal_id(value)
                    .ok_or_else(|| "invalid approved provider account id".to_owned())?,
            ),
        };
        let trust = record.trust();
        let mut hash = aterm_digest::Sha256::new();
        hash.update(b"aterm/shared-login-network/1\0");
        hash.update(trust.network);
        hash.update(trust.authority);
        hash.update(trust.genesis);
        Ok(Grant {
            source: accounts::shared::Source {
                network: hash.finalize(),
                account: self.account,
            },
            decision: record.id(),
            generation: record.request().generation,
            configuration: prepared.configuration().digest(),
            identity: identity.into(),
            expected_principal: principal,
            required_browser_profile: browser.map(str::to_owned),
        })
    }
}

impl Authority for Approved {
    fn prepare(&self, row: &accounts::Row) -> Result<Grant, String> {
        self.read(row)
    }
    fn current(&self, grant: &Grant, row: &accounts::Row) -> Result<(), String> {
        if self.read(row)? != *grant {
            return Err("shared renewal approval changed".into());
        }
        Ok(())
    }
}

struct Command {
    label: String,
    account: [u8; 32],
    decision: [u8; 32],
    request: [u8; 16],
    config: Option<PathBuf>,
}

fn digest<const N: usize>(text: &str) -> Result<[u8; N], String> {
    let mut digest = [0; N];
    if !aterm_codec::hex::decode_into(text.as_bytes(), &mut digest) || digest == [0; N] {
        return Err("invalid shared renewal identifier".into());
    }
    Ok(digest)
}

fn parse(args: &[OsString]) -> Result<Command, String> {
    let label = args
        .first()
        .and_then(|s| s.to_str())
        .filter(|s| accounts::admissible_label(s))
        .ok_or_else(|| "shared renewal requires one valid local label".to_owned())?;
    let mut options = BTreeMap::new();
    let (pairs, remainder) = args[1..].as_chunks::<2>();
    for pair in pairs {
        let key = pair[0]
            .to_str()
            .filter(|s| matches!(*s, "--account" | "--decision" | "--request" | "--config"))
            .ok_or_else(|| "unknown shared renewal option".to_owned())?;
        let value = pair[1]
            .to_str()
            .ok_or_else(|| "shared renewal options must be UTF-8".to_owned())?;
        if options.insert(key, value).is_some() {
            return Err("duplicate shared renewal option".into());
        }
    }
    if !remainder.is_empty() {
        return Err("shared renewal option is missing its value".into());
    }
    let mut take = |key| {
        options
            .remove(key)
            .ok_or_else(|| "missing shared renewal identifier".to_owned())
    };
    let account = digest(take("--account")?)?;
    let decision = digest(take("--decision")?)?;
    let request = digest(take("--request")?)?;
    let config = options.remove("--config").map(PathBuf::from);
    Ok(Command {
        label: label.to_owned(),
        account,
        decision,
        request,
        config,
    })
}

pub(super) fn try_run(args: &[OsString]) -> Option<ExitCode> {
    if args.first().is_none_or(|a| a != "accounts")
        || args.get(1).is_none_or(|a| a != "shared-renew")
    {
        return None;
    }
    if args[2..].iter().any(|arg| arg == "--help" || arg == "-h") {
        return Some(aterm_uds::stdout::answer_exit("aterm", USAGE, 0));
    }
    let result: Result<ExitCode, String> = (|| {
        let command = parse(&args[2..])?;
        let mut env = cli::Env::from_process()?;
        if let Some(config) = command.config {
            env.config = Some(config);
        }
        let paths = Paths::live().map_err(|e| e.to_string())?;
        let expected = client::config(&paths)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "shared renewal requires an enrolled vault".to_owned())?;
        let authority = Approved {
            paths,
            expected,
            account: command.account,
            decision: command.decision,
        };
        Ok(cli::shared_renewal::run(
            &env,
            &cli::LiveAccounts,
            &command.label,
            command.request,
            &authority,
            &mut std::io::stdout(),
            &mut std::io::stderr(),
        ))
    })();
    Some(match result {
        Ok(code) => code,
        Err(why) => {
            aterm_uds::stdout::answer_exit("aterm shared-renew", &format!("{why}\n{USAGE}"), 2)
        }
    })
}

#[cfg(test)]
mod tests {
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
            fn auth_status(
                &self,
                _: &str,
            ) -> Result<aterm_agent::harness::align::Captured, String> {
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
    fn a_stop_withholds_even_a_previously_matching_owned_configuration() {
        let (_home, approved, row) = fixture();
        let grant = approved.prepare(&row).unwrap();
        let db = Activations::at(approved.paths.clone());
        let admin = client::admin_open(&approved.paths).unwrap();
        let stop = db
            .stop_request(approved.account, vec![grant.decision])
            .unwrap();
        db.issue(&admin, [3; 16], stop).unwrap();
        assert!(approved.current(&grant, &row).is_err());
    }

    #[test]
    fn a_required_browser_profile_cannot_fall_back_to_the_default_browser() {
        use std::cell::Cell;

        struct Host(Cell<usize>);
        impl cli::AccountsHost for Host {
            fn stdin_is_terminal(&self) -> bool {
                true
            }
            fn auth_status(
                &self,
                _: &str,
            ) -> Result<aterm_agent::harness::align::Captured, String> {
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
            fn auth_status(
                &self,
                _: &str,
            ) -> Result<aterm_agent::harness::align::Captured, String> {
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

        for policy in 0..3 {
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
                let config = match policy {
                    0 => "[commons]\n".to_owned(),
                    1 => "[commons]\nenabled=false\n".to_owned(),
                    _ => {
                        format!(
                            "[commons]\nexclude_tables=['{}','{}']\n",
                            aterm_codec::hex::encode_lower(&proposals),
                            aterm_codec::hex::encode_lower(&activations)
                        )
                    }
                };
                std::fs::write(env.config.as_ref().unwrap(), &config).unwrap();
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
                assert_eq!(authority.checked.get(), checked_before_retry);
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
}
