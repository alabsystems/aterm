// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The registered mailbox bundle's inline fields. Never follows file pointers.

use super::{Component, Part};
use crate::SecretBytes;
use std::collections::{BTreeMap, BTreeSet};
use zeroize::Zeroize as _;

struct Document(aterm_toml::Table);
impl Drop for Document {
    fn drop(&mut self) {
        fn wipe(value: aterm_toml::Value) {
            match value {
                aterm_toml::Value::String(mut s) => s.zeroize(),
                aterm_toml::Value::Array(a) => a.into_iter().for_each(wipe),
                aterm_toml::Value::Table(t) => t.into_iter().for_each(|(_, value)| wipe(value)),
                _ => {}
            }
        }
        std::mem::replace(&mut self.0, aterm_toml::Table::new())
            .into_iter()
            .for_each(|(_, value)| wipe(value));
    }
}

/// Unambiguous inline credentials of an authenticated registered mailbox bundle.
/// Construction does not establish provenance; its caller must supply verified bytes.
#[derive(Default)]
pub struct InlineSource {
    values: BTreeMap<Component, SecretBytes>,
    ambiguous: BTreeSet<Component>,
    identities: BTreeSet<Component>,
}

impl std::fmt::Debug for InlineSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InlineSource")
            .field("components", &self.values.len())
            .field("ambiguous", &self.ambiguous.len())
            .finish()
    }
}

fn string<'a>(table: &'a aterm_toml::Table, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| table.get(key)?.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn direct(table: &aterm_toml::Table, part: Part) -> Option<&str> {
    secret(
        table,
        match part {
            Part::Password => &[
                "password",
                "pass",
                "imap_password",
                "imap_pass",
                "proton_password",
                "webmail_password",
            ],
            Part::SecondPassword => &["mailbox_password", "second_password"],
            Part::Totp => &["totp_secret", "totp", "otpauth", "totp_uri", "otp_secret"],
        },
    )
}

fn secret<'a>(table: &'a aterm_toml::Table, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| table.get(key)?.as_str())
        .filter(|value| !value.trim().is_empty())
}

fn keyed<'a>(table: &'a aterm_toml::Table, login: &str, part: Part) -> (Option<&'a str>, bool) {
    let names: &[&str] = match part {
        Part::Password => &[
            "passwords",
            "password",
            "webmail_passwords",
            "imap_passwords",
        ],
        Part::SecondPassword => &["mailbox_passwords", "mailbox_password", "second_passwords"],
        Part::Totp => &["totp", "totp_secrets", "totp_secret", "otpauth"],
    };
    let mut found = None;
    for name in names {
        let Some(values) = table.get(name).and_then(aterm_toml::Value::as_table) else {
            continue;
        };
        for (key, value) in values {
            if !key.trim().eq_ignore_ascii_case(login) {
                continue;
            }
            let Some(value) = value.as_str().filter(|value| !value.trim().is_empty()) else {
                continue;
            };
            if found.is_some_and(|previous| previous != value) {
                return (None, true);
            }
            found = Some(value);
        }
    }
    (found, false)
}

impl InlineSource {
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() > 4 * 1024 * 1024 {
            return None;
        }
        let doc = Document(aterm_toml::from_str(text).ok()?);
        let table = doc
            .0
            .get("ai")
            .and_then(aterm_toml::Value::as_table)
            .and_then(|t| t.get("mailbox"))
            .and_then(aterm_toml::Value::as_table)
            .or_else(|| {
                doc.0
                    .get("harness")
                    .and_then(aterm_toml::Value::as_table)
                    .and_then(|t| t.get("mailbox"))
                    .and_then(aterm_toml::Value::as_table)
            })
            .or_else(|| doc.0.get("mailbox").and_then(aterm_toml::Value::as_table))
            .unwrap_or(&doc.0);
        let mut out = Self::default();
        let url_login = string(table, &["imap_url", "imap"])
            .and_then(|url| super::configured_login(url, ""))
            .filter(|login| !login.trim().is_empty());
        let default_login =
            string(table, &["user", "username", "imap_user"]).or(url_login.as_deref());
        if let Some(login) = default_login {
            out.add(table, table, login, true);
        }
        let mut route_tables = vec![table];
        if !std::ptr::eq(&doc.0, table) {
            route_tables.push(&doc.0);
        }
        for route_table in route_tables {
            let Some(routes) = route_table
                .get("mailboxes")
                .and_then(aterm_toml::Value::as_array)
            else {
                continue;
            };
            for route in routes.iter().filter_map(aterm_toml::Value::as_table) {
                let Some(login) = string(route, &["login", "user", "username"]) else {
                    continue;
                };
                out.add(
                    route,
                    table,
                    login,
                    default_login.is_some_and(|d| d.eq_ignore_ascii_case(login)),
                );
            }
        }
        Some(out)
    }

    fn add(
        &mut self,
        route: &aterm_toml::Table,
        defaults: &aterm_toml::Table,
        login: &str,
        same_default: bool,
    ) {
        let mut sources: Vec<&str> = [
            "webmail_url",
            "webmail",
            "imap_url",
            "imap",
            "http_url",
            "webhook_url",
            "url",
        ]
        .iter()
        .filter_map(|key| string(route, &[*key]))
        .collect();
        if sources.is_empty() && same_default {
            sources = [
                "webmail_url",
                "webmail",
                "imap_url",
                "imap",
                "http_url",
                "webhook_url",
                "url",
            ]
            .iter()
            .filter_map(|key| string(defaults, &[*key]))
            .collect();
        }
        if sources.is_empty() {
            let domain = login.rsplit_once('@').map(|(_, domain)| domain);
            sources.extend(match domain.map(str::to_ascii_lowercase).as_deref() {
                Some("proton.me" | "proton.ch" | "protonmail.com" | "protonmail.ch" | "pm.me") => {
                    Some("https://mail.proton.me/")
                }
                Some("gmail.com" | "googlemail.com") => Some("imaps://imap.gmail.com:993/INBOX"),
                _ => None,
            });
        }
        for part in [Part::Password, Part::SecondPassword, Part::Totp] {
            let (keyed, conflicting) = keyed(defaults, login, part);
            let value = direct(route, part)
                .or(keyed)
                .or_else(|| same_default.then(|| direct(defaults, part)).flatten());
            for source in &sources {
                let Some(authority) = super::configured_authority(source) else {
                    continue;
                };
                let Some(actual_login) = super::configured_login(source, login) else {
                    continue;
                };
                let Some(component) = Component::for_mailbox(&authority, &actual_login, part)
                else {
                    continue;
                };
                self.identities.insert(component);
                if conflicting {
                    self.values.remove(&component);
                    self.ambiguous.insert(component);
                    continue;
                }
                let Some(value) = value.filter(|v| !v.chars().any(char::is_control)) else {
                    continue;
                };
                if self.ambiguous.contains(&component) {
                    continue;
                }
                if let Some(previous) = self.values.get(&component) {
                    if previous.as_slice() != value.as_bytes() {
                        self.values.remove(&component);
                        self.ambiguous.insert(component);
                    }
                } else {
                    self.values
                        .insert(component, SecretBytes::from(value.as_bytes()));
                }
            }
        }
    }

    pub fn components(&self) -> impl Iterator<Item = Component> + '_ {
        self.values.keys().copied()
    }

    #[must_use]
    pub fn value(&self, component: Component) -> Option<&SecretBytes> {
        self.values.get(&component)
    }

    #[must_use]
    pub fn recognizes(&self, component: Component) -> bool {
        self.identities.contains(&component) && !self.ambiguous.contains(&component)
    }
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
