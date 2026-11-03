// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Missing mailbox components are requirements, not failed passwords or absent
//! messages. Only an observed requirement of a registered mailbox raises a need.

use super::{headless_oauth::MailboxConfig, mailbox_routes::MailboxRoute};
#[cfg(any(unix, test))]
use aterm_secrets::mailbox_component::InlineSource;
use aterm_secrets::mailbox_component::{Component, Part, canonical_authority, configured_login};
#[cfg(unix)]
use aterm_secrets::mailbox_component::{SOURCE_ENTRY, configured_authority};
use std::{collections::BTreeMap, path::Path};

#[cfg(test)]
std::thread_local! {
    static REGISTRY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn endpoints(route: &MailboxRoute) -> Vec<String> {
    let mut sources: Vec<String> = [
        route.webmail_url.as_deref(),
        route.imap_url.as_deref(),
        route.http_url.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter_map(endpoint_authority)
    .collect();
    if sources.is_empty() && !route.names_a_url() {
        let source = match route.login_domain().as_deref() {
            Some("proton.me" | "proton.ch" | "protonmail.com" | "protonmail.ch" | "pm.me") => {
                Some("https://mail.proton.me/")
            }
            Some("gmail.com" | "googlemail.com") => Some("imaps://imap.gmail.com:993/INBOX"),
            _ => None,
        };
        sources.extend(source.and_then(canonical_authority));
    }
    sources.sort();
    sources.dedup();
    sources
}

fn endpoint_authority(endpoint: &str) -> Option<String> {
    let scheme = endpoint.trim().split_once("://")?.0;
    if scheme.eq_ignore_ascii_case("imap") || scheme.eq_ignore_ascii_case("imaps") {
        super::headless_oauth::parse_imap_url(endpoint)?;
        return canonical_authority(&super::mailbox_routes::login_source(endpoint));
    }
    canonical_authority(endpoint)
}

fn default_route(config: &MailboxConfig) -> Option<MailboxRoute> {
    Some(MailboxRoute {
        login: config.user.clone().or_else(|| {
            config
                .imap_url
                .as_deref()
                .and_then(|url| configured_login(url, ""))
                .filter(|login| !login.trim().is_empty())
        })?,
        webmail_url: config.webmail_url.clone(),
        imap_url: config.imap_url.clone(),
        http_url: config.http_url.clone(),
        password: config.password.clone(),
        mailbox_password: config.mailbox_password.clone(),
        totp_secret: config.totp_secret.clone(),
        receives: Vec::new(),
    })
}

fn unique_component(route: &MailboxRoute, part: Part) -> Option<Component> {
    let sources = endpoints(route);
    if sources.len() != 1 {
        return None;
    }
    let mut logins = [
        route.webmail_url.as_deref(),
        route.imap_url.as_deref(),
        route.http_url.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter_map(|source| configured_login(source, &route.login))
    .map(|login| login.trim().to_ascii_lowercase())
    .collect::<Vec<_>>();
    logins.sort();
    logins.dedup();
    if logins.len() > 1 {
        return None;
    }
    Component::for_mailbox(
        &sources[0],
        logins.first().map_or(route.login.as_str(), String::as_str),
        part,
    )
}

pub(super) fn default_component(config: &MailboxConfig, part: Part) -> Option<String> {
    let route = default_route(config)?;
    config
        .mailbox_components
        .get(&unique_component(&route, part)?)
        .cloned()
}

/// An absent IMAP password is required only when no other mailbox path can
/// provide this authorization's message. Browser login observes its own fields.
pub(super) fn record_missing_imap_password(config: &MailboxConfig, target: Option<&str>) {
    if config.effective_webmail_url(target).is_some() {
        return;
    }
    record_observed_missing(config, target, Part::Password);
}

pub(super) fn record_observed_missing(config: &MailboxConfig, target: Option<&str>, part: Part) {
    if let Some(home) = config.mailbox_source_home.as_deref() {
        record_missing_for_target(home, config, target, part);
    }
}

/// Only exact, single-authority fields can fill a route. A credential delivered
/// for one host is never promoted to a password used on several hosts.
pub(super) fn overlay_route(config: &MailboxConfig, route: &mut MailboxRoute) {
    for part in [Part::Password, Part::SecondPassword, Part::Totp] {
        let Some(component) = unique_component(route, part) else {
            continue;
        };
        let Some(value) = config.mailbox_components.get(&component) else {
            continue;
        };
        let into = match part {
            Part::Password => &mut route.password,
            Part::SecondPassword => &mut route.mailbox_password,
            Part::Totp => &mut route.totp_secret,
        };
        if into.as_deref().is_none_or(|value| value.trim().is_empty()) {
            *into = Some(value.clone());
        }
    }
}

/// Load delivered components only when both the bundle defining their identity
/// and the individual values are authenticated registered vault entries.
pub(super) fn load_components(home: Option<&Path>, config: &mut MailboxConfig) {
    #[cfg(unix)]
    {
        use aterm_secrets::client::{self, Paths};
        let Some(home) = home else {
            return;
        };
        let paths = Paths::for_home(home);
        let Ok(Some(source_config)) = client::config(&paths) else {
            return;
        };
        let Ok(Some(bundle)) = client::read_verified_entry(&paths, &source_config, SOURCE_ENTRY)
        else {
            return;
        };
        let Some(source) = bundle.value().as_str().and_then(InlineSource::parse) else {
            return;
        };
        config.mailbox_source_home = Some(home.to_path_buf());
        let Ok(names) = client::registered_names(&paths) else {
            return;
        };
        for name in names {
            let Some(component) =
                Component::from_entry(&name).filter(|component| source.recognizes(*component))
            else {
                continue;
            };
            let Ok(Some(entry)) = client::read_verified_entry(&paths, &source_config, &name) else {
                continue;
            };
            let Some(value) = entry.value().as_str().and_then(stored_component_value) else {
                continue;
            };
            config
                .mailbox_components
                .insert(component, value.to_owned());
        }
    }
    #[cfg(not(unix))]
    let _ = (home, config);
}

#[cfg(any(unix, test))]
fn stored_component_value(value: &str) -> Option<&str> {
    let value = value.strip_suffix('\n').unwrap_or(value);
    let value = value.strip_suffix('\r').unwrap_or(value);
    (!value.trim().is_empty() && !value.chars().any(char::is_control)).then_some(value)
}

/// Record an observed missing field for the actual mailbox, never the AI
/// account's alias. No UI action, terminal prompt or value is emitted here.
pub fn record_missing(
    home: &Path,
    endpoint: &str,
    login: &str,
    part: Part,
    record: Option<&Path>,
) -> bool {
    #[cfg(unix)]
    {
        use aterm_secrets::{
            client::{self, Paths},
            need::{self, Purpose, Want},
        };
        if login_held(record, endpoint, login) {
            return false;
        }
        let Some(component) = endpoint_authority(endpoint)
            .and_then(|authority| Component::for_mailbox(&authority, login, part))
        else {
            return false;
        };
        let paths = Paths::for_home(home);
        #[cfg(test)]
        REGISTRY_READS.with(|reads| reads.set(reads.get() + 1));
        let Ok(Some(config)) = client::config(&paths) else {
            return false;
        };
        let Ok(Some(bundle)) = client::read_verified_entry(&paths, &config, SOURCE_ENTRY) else {
            return false;
        };
        let Some(source) = bundle.value().as_str().and_then(InlineSource::parse) else {
            return false;
        };
        if !source.recognizes(component) || source.value(component).is_some() {
            return false;
        }
        match client::read_verified_entry(&paths, &config, &component.entry()) {
            Ok(None) => need::raise(
                &paths,
                Want::MailboxComponent(component),
                Purpose::HarnessSignIn,
            )
            .is_ok(),
            Ok(Some(_)) | Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (home, endpoint, login, part, record);
        false
    }
}

#[cfg(unix)]
fn login_held(record: Option<&Path>, endpoint: &str, login: &str) -> bool {
    let mut sources = vec![super::mailbox_routes::login_source(endpoint)];
    if let Some(canonical) = configured_authority(endpoint) {
        let default = match canonical.split_once("://").map(|pair| pair.0) {
            Some("https") => ":443",
            Some("http") => ":80",
            Some("imaps") => ":993",
            Some("imap") => ":143",
            _ => "",
        };
        if let Some(without_port) = canonical.strip_suffix(default) {
            sources.push(without_port.to_owned());
        }
        sources.push(canonical);
    }
    sources.iter().any(|source| {
        super::mailbox_routes::login_held(record, source, login, super::mailbox_routes::unix_now())
            .is_some()
    })
}

pub(super) fn record_missing_for_target(
    home: &Path,
    config: &MailboxConfig,
    target: Option<&str>,
    part: Part,
) -> bool {
    if config.mailbox_source_home.as_deref() != Some(home)
        || config.dir.is_some()
        || (config.relay_enabled && target.is_some())
        || (config.routed && config.unready.is_some())
    {
        return false;
    }
    let route = config.route_for(target).or_else(|| default_route(config));
    let Some(route) = route else {
        return false;
    };
    if route.http_url.is_some() {
        return false;
    }
    let sources = endpoints(&route);
    if sources.len() != 1 {
        return false;
    }
    let originals: Vec<&str> = [
        route.webmail_url.as_deref(),
        route.imap_url.as_deref(),
        route.http_url.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect();
    if unique_component(&route, part).is_none() {
        return false;
    }
    let endpoint = originals.first().copied().unwrap_or(&sources[0]);
    let Some(login) = configured_login(endpoint, &route.login) else {
        return false;
    };
    #[cfg(unix)]
    if originals
        .iter()
        .any(|source| login_held(config.login_record.as_deref(), source, &login))
    {
        return false;
    }
    record_missing(home, endpoint, &login, part, config.login_record.as_deref())
}

/// Strip component values when making a single-mailbox view: its completed
/// fields already carry the selected values, never another mailbox's secret.
pub(super) fn empty_components() -> BTreeMap<Component, String> {
    BTreeMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn webmail_challenge_then_maildir_success_leaves_no_credential_need() {
        use super::super::headless_oauth::{
            CdpClient, MailboxCredential, poll_mailbox_or_webmail_for_credential,
            split_nul_messages,
        };
        use aterm_secrets::{client, need, text::Audience};
        use std::io::{Read, Write};

        for fallback in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let home = root.path().join("member");
            let paths = client::Paths::for_home(&home);
            client::create(
                &paths,
                root.path().join("fixture.vault").to_str().unwrap(),
                None,
                Some("member"),
            )
            .unwrap();
            let text = "user = 'mailbox@example.test'\nwebmail_url = 'https://mail.example.test/'\npassword = 'fixture-password'\n";
            let admin = client::admin_open(&paths).unwrap();
            let mut doc = admin.doc.clone();
            doc.set_entry(&Audience::Everyone, SOURCE_ENTRY, text.as_bytes())
                .unwrap();
            client::admin_publish(&paths, &admin, &doc).unwrap();
            let mut cfg = MailboxConfig::from_toml_str(text, None).unwrap();
            load_components(Some(&home), &mut cfg);
            let maildir = root.path().join("maildir");
            std::fs::create_dir(&maildir).unwrap();
            if fallback {
                cfg.dir = Some(maildir.clone());
            }
            let (commands, to_browser) = std::io::pipe().unwrap();
            let (from_browser, mut replies) = std::io::pipe().unwrap();
            let browser = std::thread::spawn(move || {
                let mut commands = commands;
                let mut pending = Vec::new();
                let mut buf = [0u8; 8192];
                loop {
                    let n = commands.read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    pending.extend_from_slice(&buf[..n]);
                    for msg in split_nul_messages(&mut pending) {
                        let method = msg["method"].as_str().unwrap();
                        let result = match method {
                            "Target.createTarget" => aterm_json::json!({"targetId": "mail"}),
                            "Target.attachToTarget" => aterm_json::json!({"sessionId": "mail"}),
                            "Runtime.evaluate" => {
                                if fallback {
                                    std::fs::write(
                                        maildir.join("fresh.eml"),
                                        "To: <codex@example.test>\r\n\r\nYour verification code: 654321",
                                    )
                                    .unwrap();
                                }
                                aterm_json::json!({"result": {"value": {
                                    "state": "needs_totp",
                                    "url": "https://mail.example.test/login/2fa"
                                }}})
                            }
                            _ => aterm_json::json!({}),
                        };
                        let mut reply = aterm_json::to_vec(
                            &aterm_json::json!({"id": msg["id"], "result": result}),
                        )
                        .unwrap();
                        reply.push(0);
                        replies.write_all(&reply).unwrap();
                        if method == "Target.closeTarget" {
                            return;
                        }
                    }
                }
            });
            let mut cdp = CdpClient::new(to_browser, from_browser);
            let result = poll_mailbox_or_webmail_for_credential(
                &mut cdp,
                &cfg,
                Some("codex@example.test"),
                std::time::Duration::from_secs(60),
                std::time::Duration::ZERO,
                &mut Default::default(),
            );
            drop(cdp);
            browser.join().unwrap();
            if fallback {
                assert_eq!(result, Ok(MailboxCredential::Otp("654321".into())));
                assert!(need::peek(&paths).unwrap().is_empty());
            } else {
                assert!(result.unwrap_err().contains("TOTP"));
                let needs = need::peek(&paths).unwrap();
                assert_eq!(needs.len(), 1);
                assert_eq!(
                    needs[0].want,
                    need::Want::MailboxComponent(
                        Component::for_mailbox(
                            "https://mail.example.test",
                            "mailbox@example.test",
                            Part::Totp,
                        )
                        .unwrap()
                    )
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn missing_imap_password_waits_while_alternative_mail_paths_exist() {
        use aterm_secrets::{client, need, text::Audience};
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("member");
        let paths = client::Paths::for_home(&home);
        client::create(
            &paths,
            root.path().join("fixture.vault").to_str().unwrap(),
            None,
            Some("member"),
        )
        .unwrap();
        let text = "user = 'mailbox@example.test'\nimap_url = 'imaps://imap.example.test/INBOX'\n";
        let admin = client::admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, SOURCE_ENTRY, text.as_bytes())
            .unwrap();
        client::admin_publish(&paths, &admin, &doc).unwrap();
        let mut cfg = MailboxConfig::from_toml_str(text, None).unwrap();
        load_components(Some(&home), &mut cfg);
        cfg.relay_enabled = true;
        record_missing_imap_password(&cfg, Some("codex@example.test"));
        for part in [Part::Password, Part::SecondPassword, Part::Totp] {
            assert!(!record_missing_for_target(
                &home,
                &cfg,
                Some("codex@example.test"),
                part
            ));
        }
        assert!(need::peek(&paths).unwrap().is_empty());
        cfg.relay_enabled = false;
        cfg.dir = Some(root.path().join("maildir"));
        record_missing_imap_password(&cfg, Some("codex@example.test"));
        for part in [Part::Password, Part::SecondPassword, Part::Totp] {
            assert!(!record_missing_for_target(
                &home,
                &cfg,
                Some("codex@example.test"),
                part
            ));
        }
        assert!(need::peek(&paths).unwrap().is_empty());
        cfg.dir = None;
        record_missing_imap_password(&cfg, Some("codex@example.test"));
        let needs = need::peek(&paths).unwrap();
        assert_eq!(needs.len(), 1);
        assert_eq!(
            needs[0].want,
            need::Want::MailboxComponent(
                Component::for_mailbox(
                    "imaps://imap.example.test",
                    "mailbox@example.test",
                    Part::Password,
                )
                .unwrap()
            )
        );
    }

    #[test]
    fn parsed_mock_configuration_cannot_read_the_process_home_registry() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = aterm_types::dirs::home_dir().unwrap_or_else(|| root.path().to_path_buf());
        let config = MailboxConfig::from_toml_str(
            "user = 'mock@example'\nwebmail_url = 'https://mail.example/'\n",
            None,
        )
        .unwrap();
        let reads = REGISTRY_READS.with(std::cell::Cell::get);
        assert!(!record_missing_for_target(
            &home,
            &config,
            None,
            Part::Password
        ));
        assert_eq!(reads, REGISTRY_READS.with(std::cell::Cell::get));
    }

    #[cfg(unix)]
    #[test]
    fn delivered_default_credentials_never_inherit_into_another_authority() {
        use aterm_secrets::{client, text::Audience};
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("member");
        let paths = client::Paths::for_home(&home);
        client::create(
            &paths,
            root.path().join("fixture.vault").to_str().unwrap(),
            None,
            Some("member"),
        )
        .unwrap();
        let text = "user = 'owner@example'\nwebmail_url = 'https://mail-a.example/'\n[[mailboxes]]\nlogin = 'owner@example'\nwebmail_url = 'https://mail-b.example/'\nreceives = ['alias@example']\n";
        let admin = client::admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, SOURCE_ENTRY, text.as_bytes())
            .unwrap();
        for part in [Part::Password, Part::SecondPassword, Part::Totp] {
            let component =
                Component::for_mailbox("https://mail-a.example", "owner@example", part).unwrap();
            doc.set_entry(&Audience::Everyone, &component.entry(), b"fixture-value")
                .unwrap();
        }
        client::admin_publish(&paths, &admin, &doc).unwrap();
        let mut config = MailboxConfig::from_toml_str(text, None).unwrap();
        load_components(Some(&home), &mut config);
        assert!(
            config.password.is_none()
                && config.mailbox_password.is_none()
                && config.totp_secret.is_none()
        );
        assert_eq!(config.password_for(None).as_deref(), Some("fixture-value"));
        assert_eq!(
            config.mailbox_password_for(None).as_deref(),
            Some("fixture-value")
        );
        assert_eq!(
            config.totp_secret_for(None).as_deref(),
            Some("fixture-value")
        );
        let other = config.route_for(Some("alias@example")).unwrap();
        assert!(
            other.password.is_none()
                && other.mailbox_password.is_none()
                && other.totp_secret.is_none()
        );
        assert!(config.password_for(Some("alias@example")).is_none());
    }

    #[test]
    fn registered_inline_aliases_match_native_mailbox_fields() {
        for (part, aliases) in [
            (
                Part::Password,
                &[
                    "passwords",
                    "password",
                    "webmail_passwords",
                    "imap_passwords",
                ][..],
            ),
            (
                Part::SecondPassword,
                &["mailbox_passwords", "mailbox_password", "second_passwords"][..],
            ),
            (
                Part::Totp,
                &["totp", "totp_secrets", "totp_secret", "otpauth"][..],
            ),
        ] {
            for alias in aliases {
                let text = format!(
                    "user = 'owner@example'\nwebmail_url = 'https://mail.example/'\n[{alias}]\n'owner@example' = 'fixture-value'\n"
                );
                let native = MailboxConfig::from_toml_str(&text, None).unwrap();
                let value = match part {
                    Part::Password => native.password_for(Some("owner@example")),
                    Part::SecondPassword => native.mailbox_password_for(Some("owner@example")),
                    Part::Totp => native.totp_secret_for(Some("owner@example")),
                }
                .unwrap();
                let source = InlineSource::parse(&text).unwrap();
                let component =
                    Component::for_mailbox("https://mail.example", "owner@example", part).unwrap();
                assert_eq!(
                    source.value(component).unwrap().as_str(),
                    Some(value.as_str()),
                    "{alias}"
                );
            }
        }
        let source = InlineSource::parse("user = 'owner@example'\nwebmail_url = 'https://mail.example/'\n[passwords]\n'owner@example' = 'first'\n[imap_passwords]\n'OWNER@example' = 'different'\n").unwrap();
        let component =
            Component::for_mailbox("https://mail.example", "owner@example", Part::Password)
                .unwrap();
        assert!(!source.recognizes(component));
        assert!(source.value(component).is_none());
    }

    #[test]
    fn delivered_password_spaces_are_preserved() {
        assert_eq!(
            stored_component_value("  fixture password  \r\n"),
            Some("  fixture password  ")
        );
        assert!(stored_component_value(" \r\n").is_none());
        assert!(stored_component_value("fixture\npassword").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn registered_default_requests_only_observed_missing_components_and_respects_holds() {
        use aterm_secrets::{client, need, text::Audience};
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("member");
        let paths = client::Paths::for_home(&home);
        client::create(
            &paths,
            root.path().join("fixture.vault").to_str().unwrap(),
            None,
            Some("member"),
        )
        .unwrap();
        let admin = client::admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, SOURCE_ENTRY, b"user = 'owner@example'\nwebmail_url = 'https://mail.example/'\npassword = 'fixture-password'\n").unwrap();
        client::admin_publish(&paths, &admin, &doc).unwrap();
        let mut config = MailboxConfig::from_toml_str("user = 'owner@example'\nwebmail_url = 'https://mail.example/'\npassword = 'fixture-password'\n", None).unwrap();
        load_components(Some(&home), &mut config);
        assert!(
            need::peek(&paths).unwrap().is_empty(),
            "optional absent factors do not request themselves"
        );
        assert!(!record_missing_for_target(
            &home,
            &config,
            Some("codex-m22@example"),
            Part::Password
        ));
        assert!(!record_missing(
            &home,
            "https://other.example/",
            "owner@example",
            Part::Totp,
            None
        ));
        let record = root.path().join("login-record");
        super::super::mailbox_routes::note_login_refused(
            Some(&record),
            "https://mail.example",
            "owner@example",
            super::super::mailbox_routes::unix_now(),
        );
        config.login_record = Some(record.clone());
        assert!(!record_missing(
            &home,
            "https://mail.example:443/",
            "owner@example",
            Part::Totp,
            Some(&record)
        ));
        assert!(
            !record_missing_for_target(&home, &config, None, Part::Totp),
            "canonical port must not bypass an existing refusal"
        );
        super::super::mailbox_routes::note_login_accepted(
            Some(&record),
            "https://mail.example",
            "owner@example",
        );
        assert!(record_missing_for_target(
            &home,
            &config,
            Some("codex-m22@example"),
            Part::SecondPassword
        ));
        let needs = need::peek(&paths).unwrap();
        assert_eq!(needs.len(), 1);
        assert_eq!(
            needs[0].want,
            need::Want::MailboxComponent(
                Component::for_mailbox(
                    "https://mail.example",
                    "owner@example",
                    Part::SecondPassword
                )
                .unwrap()
            )
        );
        assert!(!format!("{:?}", needs[0].want).contains("codex-m22"));
    }

    #[test]
    fn delivered_components_fill_only_the_exact_registered_authority() {
        let component = Component::for_mailbox(
            "https://mail.example",
            "owner@example",
            Part::SecondPassword,
        )
        .unwrap();
        let mut config = MailboxConfig::default();
        config
            .mailbox_components
            .insert(component, "second-secret-fixture".into());
        let mut route = MailboxRoute {
            login: "owner@example".into(),
            webmail_url: Some("https://mail.example/inbox".into()),
            ..Default::default()
        };
        overlay_route(&config, &mut route);
        assert_eq!(
            route.mailbox_password.as_deref(),
            Some("second-secret-fixture")
        );
        route.mailbox_password = None;
        route.webmail_url = Some("https://other.example/inbox".into());
        overlay_route(&config, &mut route);
        assert!(route.mailbox_password.is_none());
        route.webmail_url = Some("https://mail.example/inbox".into());
        route.imap_url = Some("imaps://different.example/INBOX".into());
        overlay_route(&config, &mut route);
        assert!(
            route.mailbox_password.is_none(),
            "one scoped secret must not serve several authorities"
        );
    }

    /// NEGATIVE CONTROL: a top-level `http_url` does not silence password needs on an explicit
    /// `[[mailboxes]]` route without `http_url`, a route with its own `http_url` does not request
    /// a mailbox password, and a stripped unready `routed_for` view never fabricates a fallback
    /// domain endpoint.
    #[cfg(unix)]
    #[test]
    fn record_missing_for_target_respects_route_http_url_and_refuses_stripped_unready_views() {
        use aterm_secrets::{client, need, text::Audience};
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("member");
        let paths = client::Paths::for_home(&home);
        client::create(
            &paths,
            root.path().join("fixture.vault").to_str().unwrap(),
            None,
            Some("member"),
        )
        .unwrap();
        let text = concat!(
            "user = 'default@example.test'\n",
            "http_url = 'https://relay.example.test/'\n",
            "[[mailboxes]]\n",
            "login = 'http-route@example.test'\n",
            "http_url = 'https://route-http.example.test/'\n",
            "receives = ['http-route@example.test']\n",
            "[[mailboxes]]\n",
            "login = 'imap-route@example.test'\n",
            "imap_url = 'imaps://imap.example.test/INBOX'\n",
            "receives = ['imap-route@example.test']\n",
        );
        let admin = client::admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(&Audience::Everyone, SOURCE_ENTRY, text.as_bytes())
            .unwrap();
        client::admin_publish(&paths, &admin, &doc).unwrap();
        let mut config = MailboxConfig::from_toml_str(text, None).unwrap();
        load_components(Some(&home), &mut config);

        assert!(
            !record_missing_for_target(
                &home,
                &config,
                Some("http-route@example.test"),
                Part::Password
            ),
            "a route with its own http_url must not request a mailbox password"
        );
        assert!(need::peek(&paths).unwrap().is_empty());

        let stripped = config.routed_for(Some("imap-route@example.test"));
        assert!(stripped.routed && stripped.unready.is_some());
        assert!(
            !record_missing_for_target(
                &home,
                &stripped,
                Some("imap-route@example.test"),
                Part::Password
            ),
            "a stripped unready routed_for view must not fabricate a domain-inferred endpoint"
        );
        assert!(need::peek(&paths).unwrap().is_empty());

        assert!(
            record_missing_for_target(
                &home,
                &config,
                Some("imap-route@example.test"),
                Part::Password
            ),
            "top-level http_url must not suppress an unrelated [[mailboxes]] IMAP route's need"
        );
        let needs = need::peek(&paths).unwrap();
        assert_eq!(needs.len(), 1);
        assert_eq!(
            needs[0].want,
            need::Want::MailboxComponent(
                Component::for_mailbox(
                    "imaps://imap.example.test",
                    "imap-route@example.test",
                    Part::Password,
                )
                .unwrap()
            )
        );
    }
}
