// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Read ownership codes through configured mailbox routes, including native Proton sessions.
use super::headless_oauth::{self, MailboxConfig, MailboxCredential};
use std::{collections::BTreeSet, path::PathBuf, time::Duration};

pub fn configured() -> Result<MailboxConfig, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("Home directory unavailable")?;
    headless_oauth::discover_mailbox_config(Some(&home), None)
        .ok_or_else(|| "No stored mail account configuration is available".into())
}
pub fn candidates(config: &MailboxConfig, hint: &str) -> Vec<String> {
    accounts(config)
        .into_iter()
        .filter(|email| matches_hint(email, hint))
        .collect()
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
pub fn code(config: &MailboxConfig, hint: &str, since: u64) -> Result<String, String> {
    let candidates = candidates(config, hint);
    if candidates.is_empty() {
        return Err("No stored mailbox route matches Proton's recovery address".into());
    }
    for email in candidates.into_iter().take(3) {
        let mut config = config.clone();
        config.senders = vec!["proton".into()];
        config.since = Some(since.saturating_sub(5).min(i64::MAX as u64) as i64);
        match headless_oauth::poll_mailbox_config_for_credential(
            &config,
            Some(&email),
            Duration::from_secs(90),
        ) {
            Ok(MailboxCredential::Otp(code)) => return Ok(code),
            Ok(MailboxCredential::MagicLink(_)) | Err(_) => {}
        }
    }
    Err("The configured mail routes did not supply Proton's recovery code".into())
}
fn matches_hint(email: &str, hint: &str) -> bool {
    let email = email.trim().to_ascii_lowercase();
    let hint = hint.trim().to_ascii_lowercase();
    let (Some((local, domain)), Some((pattern, wanted_domain))) =
        (email.split_once('@'), hint.split_once('@'))
    else {
        return false;
    };
    if domain != wanted_domain {
        return false;
    }
    if let (Some(first), Some(last)) = (pattern.find('*'), pattern.rfind('*')) {
        local.starts_with(&pattern[..first]) && local.ends_with(&pattern[last + 1..])
    } else {
        local == pattern
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn recovery_hint_does_not_select_another_domain_or_prefix() {
        assert!(super::matches_hint("alex@gmail.com", "a********@gmail.com"));
        assert!(!super::matches_hint("bob@gmail.com", "a********@gmail.com"));
        assert!(!super::matches_hint(
            "alex@other.test",
            "a********@gmail.com"
        ));
        assert!(super::matches_hint("alex@gmail.com", "alex@gmail.com"));
        assert!(!super::matches_hint("alex2@gmail.com", "alex@gmail.com"));
    }
}
