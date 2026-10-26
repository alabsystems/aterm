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
        if value
            .get("details")
            .and_then(|details| details.as_object())
            .is_some_and(|details| {
                details.iter().any(|(key, detail)| {
                    detail["kind"].as_str() == Some("requirement")
                        && !["local-class", "browser-profile", "provider-account-id"]
                            .contains(&key.as_str())
                })
            })
        {
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
                configuration
                    .get("email-addresses", key)
                    .and_then(|contact| contact["address"].as_str())
                    .filter(|s| accounts::email_refusal(s).is_none())
                    .ok_or_else(|| {
                        "approved provider identity is absent from its checked closure".to_owned()
                    })?
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

    fn fixture_with_extra(
        auth: Option<aterm_json::Value>,
        unknown_requirement: bool,
        browser: Option<&str>,
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
                        value: Some(json!({"address":"provider@example.test"})),
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
        let env = cli::Env {
            state: home.path().join("harness"),
            aterm_state: None,
            cwd: home.path().to_owned(),
            home: Some(home.path().to_owned()),
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
            config: Some(home.path().join("aterm.toml")),
            claude_tmp: None,
            claude_running: None,
            tz_s: 0,
        };
        let roster = env.accounts_roster().unwrap();
        std::fs::create_dir_all(roster.parent().unwrap()).unwrap();
        std::fs::write(
            &roster,
            accounts::append_rows("", std::slice::from_ref(&row)).unwrap(),
        )
        .unwrap();
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
}
