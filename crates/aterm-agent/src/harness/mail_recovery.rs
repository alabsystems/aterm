// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! The stored mailbox configuration and the mail account identities it names. No code a
//! vendor mails to verify a person (Proton's human verification, Google's `Verify it's you`)
//! is read here or anywhere in aterm to be answered.
use super::headless_oauth::{self, MailboxConfig};
use std::{collections::BTreeSet, path::PathBuf};

pub fn configured() -> Result<MailboxConfig, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("Home directory unavailable")?;
    headless_oauth::discover_mailbox_config(Some(&home), None)
        .ok_or_else(|| "No stored mail account configuration is available".into())
}
pub fn accounts(config: &MailboxConfig) -> Vec<String> {
    config
        .mailboxes
        .iter()
        .map(|route| route.login.clone())
        .chain(config.passwords.keys().cloned())
        .chain(config.user.iter().cloned())
        .filter(|email| email.contains('@') && email != "*@*")
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
