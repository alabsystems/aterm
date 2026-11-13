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
        registry: None,
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
#[path = "mailbox_needs_tests.rs"]
mod tests;
