// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Swarm sign-in credential distribution (`ai-signin.toml`) and steward `mail-code` relay
//! (`docs/DESIGN-alab-swarm-2026-10-27.md` §6.2, §12.3 C17c).
//!
//! - **Per-email passwords (`ai-signin.toml`)**: built from the admin's `ai-mailbox.toml`,
//!   `ai-pool.toml`, and `aterm-swarm-roster`. Each account's per-email password (and optional
//!   per-email second mailbox password / TOTP secret) is `age`-encrypted strictly to the `age1…`
//!   recipients of that account's `warm` Macs. A member Mac decrypts only its own warm accounts.
//! - **Catch-all `mail-code <email>` relay (`FleetOp::MailCode`)**: the catch-all login stays on
//!   steward Macs. A steward answers `mail-code <email>` only when the proved requester Mac's warm
//!   set (`ai-pool.toml`) or its senior live claim (`accounts/claims/1`) names `<email>`, seals the newest
//!   sign-in code or magic link for `<email>` to the requester's `age1…` recipient from
//!   `aterm-swarm-roster`, and logs each answer.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use aterm_secrets::age::{self, Identity, Recipient};
use aterm_secrets::swarm_roster::{SwarmMember, SwarmRoster};

use super::ai_pool::{self, LocalPoolIdentity, Pool};
use super::claims::{Book, View};
use super::headless_oauth::{MailboxConfig, MailboxCredential};

/// Materialized filename under `~/.secrets/` carrying age-sealed per-email passwords for warm Macs.
pub const AI_SIGNIN_FILE: &str = "ai-signin.toml";

/// Audit log filename under `<state_dir>/` recording answered `mail-code <email>` relay requests.
pub const MAIL_CODE_AUDIT_FILE: &str = "mail-code-relay.log";

/// Rotated audit log filename under `<state_dir>/` written when [`MAIL_CODE_AUDIT_FILE`] reaches [`MAIL_CODE_AUDIT_MAX_BYTES`].
pub const MAIL_CODE_AUDIT_PREV_FILE: &str = "mail-code-relay.log.1";

/// Maximum size (`1 MiB`) of [`MAIL_CODE_AUDIT_FILE`] before rotating to [`MAIL_CODE_AUDIT_PREV_FILE`].
pub const MAIL_CODE_AUDIT_MAX_BYTES: u64 = 1024 * 1024;

/// Bounded timeout for one `FleetOp::MailCode` dial from a member Mac to a steward.
pub const MAIL_CODE_DIAL_WITHIN: Duration = Duration::from_secs(10);

/// One account's age-sealed per-email credential entry in `ai-signin.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiSigninAccount {
    /// Lowercase account email address.
    pub email: String,
    /// Armored C2SP age v1 ciphertext encrypted to this account's warm Macs.
    pub sealed: String,
}

/// Parsed representation of `ai-signin.toml` (`DESIGN-alab-swarm` §6.2, C17c).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AiSigninDoc {
    /// Non-secret IMAP endpoint URL (`imaps://host:port/INBOX`, without embedded credentials).
    pub imap_url: Option<String>,
    /// Non-secret webmail inbox URL.
    pub webmail_url: Option<String>,
    /// Non-secret HTTP mailbox endpoint URL (without bearer token).
    pub http_url: Option<String>,
    /// True when stewards hold a catch-all mailbox login and answer `FleetOp::MailCode`.
    pub relay: bool,
    /// Per-email credential ciphertexts sealed to each account's warm recipients.
    pub accounts: Vec<AiSigninAccount>,
}

/// One account's sealed credentials: its explicit per-email values, as every Mac reads them, and
/// where the account's own `[[mailboxes]]` entry is sealed to it ([`sealed_route_for`]), that
/// entry under `route`. A Mac built before `route` ignores the field whole, so it never files
/// the entry's secrets as the address's own per-email ones.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct SealedAccountBundle {
    #[serde(skip_serializing_if = "Option::is_none")]
    password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mailbox_password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    totp_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    route: Option<SealedRoute>,
}

/// The account's own `[[mailboxes]]` entry, sealed whole: its login, URLs and secrets.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct SealedRoute {
    login: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    webmail_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    imap_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    http_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mailbox_password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    totp_secret: Option<String>,
}

impl SealedAccountBundle {
    const fn is_empty(&self) -> bool {
        self.password.is_none()
            && self.mailbox_password.is_none()
            && self.totp_secret.is_none()
            && self.route.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct WireAiSigninDoc {
    #[serde(skip_serializing_if = "Option::is_none")]
    imap_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    webmail_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    http_url: Option<String>,
    #[serde(default)]
    relay: bool,
    #[serde(default, rename = "accounts")]
    accounts: Vec<WireAiSigninAccount>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct WireAiSigninAccount {
    email: String,
    sealed: String,
}

/// Strip any embedded `user[:password]@` from an IMAP/HTTP URL so `ai-signin.toml` carries only
/// non-secret endpoint coordinates.
fn sanitize_endpoint_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let (scheme, rest) = trimmed.split_once("://")?;
    let (authority, tail) = match rest.find(['/', '?', '#']) {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, hp)| hp)
        .trim();
    if host_port.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{host_port}{tail}"))
}

fn imap_url_has_embedded_secret(url: &str) -> bool {
    let Some((_, rest)) = url.trim().split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    authority
        .rsplit_once('@')
        .is_some_and(|(userinfo, _)| userinfo.contains(':'))
}

/// True when `admin_cfg` carries a catch-all mailbox login that stays with stewards rather than
/// per-email passwords.
#[must_use]
pub fn has_catch_all_login(admin_cfg: &MailboxConfig) -> bool {
    admin_cfg
        .completed_mailboxes()
        .iter()
        .any(|route| route.ready() && route.shared())
        || admin_cfg.dir.is_some()
        || admin_cfg.password.is_some()
        || admin_cfg.mailbox_password.is_some()
        || admin_cfg.totp_secret.is_some()
        || admin_cfg.bearer_token.is_some()
        || admin_cfg.passwords.contains_key("*")
        || admin_cfg.mailbox_passwords.contains_key("*")
        || admin_cfg.totp_secrets.contains_key("*")
        || admin_cfg
            .imap_url
            .as_deref()
            .is_some_and(imap_url_has_embedded_secret)
}

fn explicit_per_email_value(
    map: &BTreeMap<String, String>,
    contested: &BTreeSet<String>,
    email: &str,
    pool_label: Option<&str>,
) -> Option<String> {
    let lower = email.trim().to_ascii_lowercase();
    if let Some(v) = map.get(&lower) {
        return Some(v.clone());
    }
    if let Some((local, _)) = lower.split_once('@')
        && !contested.contains(&local.replace(['.', '_'], "-"))
        && let Some(v) = map.get(local)
    {
        return Some(v.clone());
    }
    if let Some(lbl) = pool_label {
        let key = lbl.trim().to_ascii_lowercase();
        if !key.is_empty()
            && !contested.contains(&key.replace(['.', '_'], "-"))
            && let Some(v) = map.get(&key)
        {
            return Some(v.clone());
        }
    }
    None
}

/// The `[[mailboxes]]` entry sealed to `email`'s warm Macs, if any: the one `email` resolves
/// to, only where it is `email`'s own and names it itself
/// ([`super::mailbox_routes::MailboxRoute::private_to`]), has its password, is no other entry's
/// login nor the top-level `user`'s, and no other address of the pool (`pool_emails`) resolves
/// to it. Any other entry is a catch-all that stays with the stewards.
fn sealed_route_for(
    admin_cfg: &MailboxConfig,
    email: &str,
    pool_emails: &[String],
) -> Option<SealedRoute> {
    let at = super::mailbox_routes::resolve_index(&admin_cfg.mailboxes, Some(email))?;
    let raw = &admin_cfg.mailboxes[at];
    let route = admin_cfg.completed_route(raw, Some(email));
    let login = route.login.trim();
    let shares_login = admin_cfg.is_top_level_login(login)
        || admin_cfg
            .mailboxes
            .iter()
            .enumerate()
            .any(|(other, m)| other != at && m.login.trim().eq_ignore_ascii_case(login));
    let serves_another = pool_emails.iter().any(|other| {
        !other.eq_ignore_ascii_case(email)
            && super::mailbox_routes::resolve_index(&admin_cfg.mailboxes, Some(other)) == Some(at)
    });
    if !route.ready() || !route.private_to(email) || shares_login || serves_another {
        return None;
    }
    Some(SealedRoute {
        login: login.to_owned(),
        webmail_url: route.webmail_url.as_deref().and_then(sanitize_endpoint_url),
        imap_url: route.imap_url.as_deref().and_then(sanitize_endpoint_url),
        http_url: route.http_url.as_deref().and_then(sanitize_endpoint_url),
        password: route.password,
        mailbox_password: route.mailbox_password,
        totp_secret: route.totp_secret,
    })
}

/// The credentials sealed for `email`: where no `[[mailboxes]]` entry receives it, its explicit
/// per-email values; where one does, nothing unless it is the account's own
/// ([`sealed_route_for`]), and then that entry under `route` beside the per-email values a Mac
/// built before `route` reads, exactly as it was sealed them before (a mailbox shared with
/// other addresses, or a wildcard one, is a catch-all that stays with the stewards).
fn explicit_bundle_for_account(
    admin_cfg: &MailboxConfig,
    email: &str,
    pool_label: Option<&str>,
    pool_emails: &[String],
) -> SealedAccountBundle {
    let route = if admin_cfg.route_for(Some(email)).is_some() {
        let Some(own) = sealed_route_for(admin_cfg, email, pool_emails) else {
            return SealedAccountBundle::default();
        };
        Some(own)
    } else {
        None
    };
    SealedAccountBundle {
        password: explicit_per_email_value(
            &admin_cfg.passwords,
            &admin_cfg.contested_labels,
            email,
            pool_label,
        ),
        mailbox_password: explicit_per_email_value(
            &admin_cfg.mailbox_passwords,
            &admin_cfg.contested_labels,
            email,
            pool_label,
        ),
        totp_secret: explicit_per_email_value(
            &admin_cfg.totp_secrets,
            &admin_cfg.contested_labels,
            email,
            pool_label,
        ),
        route,
    }
}

fn member_is_denied(member: &SwarmMember, roster: &SwarmRoster) -> bool {
    roster.deny_labels.iter().any(|d| {
        d.eq_ignore_ascii_case(&member.label)
            || member.aliases.iter().any(|a| d.eq_ignore_ascii_case(a))
    }) || member.node_id.as_deref().is_some_and(|node| {
        roster
            .deny_nodes
            .iter()
            .any(|d| d.eq_ignore_ascii_case(node))
    }) || member.node_key.as_deref().is_some_and(|key| {
        roster
            .deny_nodes
            .iter()
            .any(|d| d.eq_ignore_ascii_case(key))
    })
}

fn member_matches_warm_label(member: &SwarmMember, warm_label: &str) -> bool {
    member.label.eq_ignore_ascii_case(warm_label)
        || member
            .aliases
            .iter()
            .any(|a| a.eq_ignore_ascii_case(warm_label))
}

fn warm_recipients_for_email(pool: &Pool, roster: &SwarmRoster, email: &str) -> Vec<Recipient> {
    let mut set = BTreeSet::new();
    for acc in &pool.accounts {
        if !acc.email.eq_ignore_ascii_case(email) {
            continue;
        }
        for warm_label in &acc.warm {
            if roster
                .deny_labels
                .iter()
                .any(|d| d.eq_ignore_ascii_case(warm_label))
            {
                continue;
            }
            for member in &roster.members {
                if member_is_denied(member, roster) {
                    continue;
                }
                if member_matches_warm_label(member, warm_label)
                    && let Ok(recipient) = Recipient::parse(member.recipient.trim())
                {
                    set.insert(recipient);
                }
            }
        }
    }
    set.into_iter().collect()
}

fn build_ai_signin_inner(
    admin_cfg: &MailboxConfig,
    pool: &Pool,
    roster: &SwarmRoster,
    seal_to_all_mutant: bool,
) -> Result<AiSigninDoc, String> {
    let mut by_email: BTreeMap<String, Option<String>> = BTreeMap::new();
    for acc in &pool.accounts {
        let lower = acc.email.trim().to_ascii_lowercase();
        if lower.is_empty() {
            continue;
        }
        let entry = by_email.entry(lower).or_insert(None);
        if entry.is_none() && acc.label.is_some() {
            entry.clone_from(&acc.label);
        }
    }

    let all_recipients: Vec<Recipient> = if seal_to_all_mutant {
        let mut set = BTreeSet::new();
        for member in &roster.members {
            if !member_is_denied(member, roster)
                && let Ok(r) = Recipient::parse(member.recipient.trim())
            {
                set.insert(r);
            }
        }
        set.into_iter().collect()
    } else {
        Vec::new()
    };

    let pool_emails: Vec<String> = by_email.keys().cloned().collect();
    let mut accounts = Vec::new();
    for (email, pool_label) in by_email {
        let bundle =
            explicit_bundle_for_account(admin_cfg, &email, pool_label.as_deref(), &pool_emails);
        if bundle.is_empty() {
            continue;
        }
        let recipients = if seal_to_all_mutant {
            all_recipients.clone()
        } else {
            warm_recipients_for_email(pool, roster, &email)
        };
        if recipients.is_empty() {
            continue;
        }
        let encoded =
            aterm_json::to_vec(&bundle).map_err(|e| format!("ai-signin bundle encode: {e}"))?;
        let cipher =
            age::encrypt(&recipients, &encoded).map_err(|e| format!("ai-signin age seal: {e}"))?;
        accounts.push(AiSigninAccount {
            email,
            sealed: age::armor(&cipher),
        });
    }

    Ok(AiSigninDoc {
        imap_url: admin_cfg
            .imap_url
            .as_deref()
            .and_then(sanitize_endpoint_url),
        webmail_url: admin_cfg
            .webmail_url
            .as_deref()
            .and_then(sanitize_endpoint_url),
        http_url: if admin_cfg.bearer_token.is_none() {
            admin_cfg
                .http_url
                .as_deref()
                .and_then(sanitize_endpoint_url)
        } else {
            None
        },
        relay: has_catch_all_login(admin_cfg),
        accounts,
    })
}

/// Build `ai-signin.toml` from the admin's `MailboxConfig`, `ai-pool.toml`, and `SwarmRoster`.
///
/// Only explicit per-email credentials are included, and each account's bundle is `age`-encrypted
/// strictly to the `age1…` recipients of that account's `warm` Macs. Catch-all credentials remain
/// with stewards and set `relay = true`.
///
/// # Errors
/// Returns an error string if serializing or age-sealing a per-email bundle fails.
pub fn build_ai_signin(
    admin_cfg: &MailboxConfig,
    pool: &Pool,
    roster: &SwarmRoster,
) -> Result<AiSigninDoc, String> {
    build_ai_signin_inner(admin_cfg, pool, roster, false)
}

/// Negative-control mutant for C17c tests: seals every per-email credential to ALL roster members
/// instead of only the account's `warm` Macs.
#[cfg(test)]
pub fn build_ai_signin_mutant_seal_to_all(
    admin_cfg: &MailboxConfig,
    pool: &Pool,
    roster: &SwarmRoster,
) -> Result<AiSigninDoc, String> {
    build_ai_signin_inner(admin_cfg, pool, roster, true)
}

impl AiSigninDoc {
    /// Serialize `self` as deterministic TOML for `ai-signin.toml`.
    ///
    /// # Errors
    /// Returns an error string if TOML serialization fails.
    pub fn to_toml(&self) -> Result<String, String> {
        let wire = WireAiSigninDoc {
            imap_url: self.imap_url.clone(),
            webmail_url: self.webmail_url.clone(),
            http_url: self.http_url.clone(),
            relay: self.relay,
            accounts: self
                .accounts
                .iter()
                .map(|a| WireAiSigninAccount {
                    email: a.email.clone(),
                    sealed: a.sealed.clone(),
                })
                .collect(),
        };
        aterm_toml::to_string(&wire).map_err(|e| format!("ai-signin toml encode: {e}"))
    }

    /// Parse `ai-signin.toml` text into an [`AiSigninDoc`].
    ///
    /// # Errors
    /// Returns an error string if `text` is not valid `ai-signin.toml`.
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let wire: WireAiSigninDoc =
            aterm_toml::from_str(text).map_err(|e| format!("ai-signin toml parse: {e}"))?;
        let mut accounts = Vec::with_capacity(wire.accounts.len());
        for acc in wire.accounts {
            let email = acc.email.trim().to_ascii_lowercase();
            if email.is_empty() || !email.contains('@') {
                return Err(format!("invalid email in ai-signin.toml: {:?}", acc.email));
            }
            accounts.push(AiSigninAccount {
                email,
                sealed: acc.sealed,
            });
        }
        Ok(Self {
            imap_url: wire.imap_url.as_deref().and_then(sanitize_endpoint_url),
            webmail_url: wire.webmail_url.as_deref().and_then(sanitize_endpoint_url),
            http_url: wire.http_url.as_deref().and_then(sanitize_endpoint_url),
            relay: wire.relay,
            accounts,
        })
    }

    /// Apply non-secret endpoints (`imap_url`, `webmail_url`, `http_url`, `relay`) onto `out` and,
    /// when `identity` is provided, decrypt every `[[accounts]]` entry addressed to `identity`
    /// into `out.passwords` / `out.mailbox_passwords` / `out.totp_secrets`.
    ///
    /// Entries sealed only to other Macs fail with `AgeError::NoMatch` and are skipped cleanly.
    /// Returns the number of per-email bundles decrypted by `identity`.
    pub fn apply_into_mailbox(
        &self,
        identity: Option<&Identity>,
        out: &mut MailboxConfig,
    ) -> usize {
        if out.imap_url.is_none() {
            out.imap_url.clone_from(&self.imap_url);
        }
        if out.webmail_url.is_none() {
            out.webmail_url.clone_from(&self.webmail_url);
        }
        if out.http_url.is_none() {
            out.http_url.clone_from(&self.http_url);
        }
        if self.relay {
            out.relay_enabled = true;
        }
        let Some(id) = identity else {
            return 0;
        };
        let mut opened_count = 0usize;
        for entry in &self.accounts {
            let Ok(plain) = age::decrypt(std::slice::from_ref(id), entry.sealed.as_bytes()) else {
                continue;
            };
            let Ok(bundle) = aterm_json::from_slice::<SealedAccountBundle>(plain.as_slice()) else {
                continue;
            };
            let key = entry.email.to_ascii_lowercase();
            if let Some(route) = bundle.route.filter(|r| !r.login.trim().is_empty()) {
                let names_it = |held: &super::mailbox_routes::MailboxRoute| {
                    held.receives.iter().any(|p| {
                        super::mailbox_routes::match_tier(p, Some(&key))
                            == Some(super::mailbox_routes::Tier::Exact)
                    })
                };
                if !out.mailboxes.iter().any(names_it) {
                    out.mailboxes.push(super::mailbox_routes::MailboxRoute {
                        login: route.login,
                        webmail_url: route.webmail_url,
                        imap_url: route.imap_url,
                        http_url: route.http_url,
                        password: route.password,
                        mailbox_password: route.mailbox_password,
                        totp_secret: route.totp_secret,
                        receives: vec![key],
                    });
                }
                opened_count += 1;
                continue;
            }
            if let Some(pw) = bundle.password {
                out.passwords.entry(key.clone()).or_insert(pw);
            }
            if let Some(mpw) = bundle.mailbox_password {
                out.mailbox_passwords.entry(key.clone()).or_insert(mpw);
            }
            if let Some(totp) = bundle.totp_secret {
                out.totp_secrets.entry(key).or_insert(totp);
            }
            opened_count += 1;
        }
        opened_count
    }
}

/// Discover and apply `ai-signin.toml` under `home` (or `store_dir`) into `out`, decrypting warm
/// per-email credentials with this Mac's local age identity when present.
pub fn load_local_ai_signin_into(
    home: Option<&Path>,
    store_dir: Option<&Path>,
    config_path: Option<&Path>,
    out: &mut MailboxConfig,
) {
    let mut candidates = Vec::new();
    if let Some(store) = store_dir {
        candidates.push(store.join(AI_SIGNIN_FILE));
    }
    if let Some(h) = home {
        candidates.push(h.join(".secrets").join(AI_SIGNIN_FILE));
        candidates.push(h.join(".config/aterm").join(AI_SIGNIN_FILE));
    }
    if let Some(cfg_file) = config_path
        && let Some(parent) = cfg_file.parent()
    {
        candidates.push(parent.join(AI_SIGNIN_FILE));
    }

    #[cfg(unix)]
    let local_identity = home.and_then(|h| {
        let paths = aterm_secrets::client::Paths::for_home(h);
        aterm_secrets::client::read_machine_identity(&paths)
            .ok()
            .flatten()
    });
    #[cfg(not(unix))]
    let local_identity: Option<Identity> = None;

    for path in candidates {
        if !path.is_file() {
            continue;
        }
        let Ok(Some(text)) = super::accounts::read_bounded(&path, 4 * 1024 * 1024) else {
            continue;
        };
        let Ok(doc) = AiSigninDoc::from_toml(&text) else {
            continue;
        };
        doc.apply_into_mailbox(local_identity.as_ref(), out);
    }

    if out.relay_enabled {
        if out.relay_home.is_none()
            && let Some(h) = home
        {
            out.relay_home = Some(h.to_path_buf());
        }
        if out.relay_state_dir.is_none() {
            out.relay_state_dir = store_dir
                .and_then(Path::parent)
                .map(Path::to_path_buf)
                .or_else(aterm_types::dirs::state_dir);
        }
    }
}

/// Parsed wire request on `FleetOp::MailCode` (`mail-code <email> [since=<ts>] [senders=…] [label=…]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailCodeRequest {
    /// Lowercase target email address whose sign-in code or magic link is requested.
    pub email: String,
    /// Optional earliest mail timestamp (Unix seconds) from the requester's sign-in start.
    pub since: Option<i64>,
    /// Optional comma-separated sender keywords (`MailFilter::senders`).
    pub senders: Vec<String>,
    /// Optional claimed `m<N>` label for pre-registration fallback when `node_id` is unset in roster.
    pub claimed_label: Option<String>,
}

fn valid_wire_email(email: &str) -> bool {
    !email.is_empty()
        && email.len() <= 254
        && email.contains('@')
        && !email.starts_with('@')
        && !email.ends_with('@')
        && !email
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '=')
}

fn valid_sender_word(word: &str) -> bool {
    !word.is_empty()
        && word.len() <= 64
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

impl MailCodeRequest {
    /// Format `self` as a single wire line (without trailing `\n`).
    #[must_use]
    pub fn to_wire(&self) -> String {
        let mut line = format!("mail-code {}", self.email.trim().to_ascii_lowercase());
        if let Some(since) = self.since {
            line.push_str(&format!(" since={since}"));
        }
        let clean_senders: Vec<String> = self
            .senders
            .iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| valid_sender_word(s))
            .collect();
        if !clean_senders.is_empty() {
            line.push_str(&format!(" senders={}", clean_senders.join(",")));
        }
        if let Some(lbl) = &self.claimed_label {
            let trimmed = lbl.trim();
            if !trimmed.is_empty() && !trimmed.chars().any(char::is_whitespace) {
                line.push_str(&format!(" label={trimmed}"));
            }
        }
        line
    }

    /// Parse a single wire line received on `FleetOp::MailCode`.
    ///
    /// # Errors
    /// Returns an error string when the verb, `<email>`, or optional `key=value` tokens are invalid.
    pub fn parse_wire(line: &str) -> Result<Self, String> {
        let mut words = line.split_ascii_whitespace();
        let Some(verb) = words.next() else {
            return Err("empty mail-code request".to_owned());
        };
        if verb != "mail-code" {
            return Err(format!("expected `mail-code`, got `{verb}`"));
        }
        let Some(raw_email) = words.next() else {
            return Err("missing email in mail-code request".to_owned());
        };
        if !valid_wire_email(raw_email) {
            return Err("invalid email in mail-code request".to_owned());
        }
        let email = raw_email.to_ascii_lowercase();
        let mut since = None;
        let mut senders = Vec::new();
        let mut claimed_label = None;
        for token in words {
            if let Some(val) = token.strip_prefix("since=") {
                since = Some(
                    val.parse::<i64>()
                        .map_err(|_| "invalid since timestamp".to_owned())?,
                );
            } else if let Some(val) = token.strip_prefix("senders=") {
                for item in val.split(',').filter(|s| !s.is_empty()) {
                    let lower = item.to_ascii_lowercase();
                    if !valid_sender_word(&lower) {
                        return Err("invalid sender token".to_owned());
                    }
                    senders.push(lower);
                }
            } else if let Some(val) = token.strip_prefix("label=") {
                if !val.is_empty() && val.len() <= 64 {
                    claimed_label = Some(val.to_owned());
                }
            } else {
                return Err(format!("unexpected mail-code argument `{token}`"));
            }
        }
        Ok(Self {
            email,
            since,
            senders,
            claimed_label,
        })
    }
}

/// Structured refusal reasons for `FleetOp::MailCode`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailCodeRefusal {
    /// Wire line could not be parsed.
    InvalidRequest(String),
    /// Requester node or label is on the swarm roster deny-list.
    Denied,
    /// Requester node is not bound to a valid `age1…` recipient in `aterm-swarm-roster`.
    UnknownRequester,
    /// Requested email is in neither the requester's `ai-pool.toml` warm set nor its active leases.
    UnauthorizedEmail,
    /// Listener holds no steward mailbox capable of serving `mail-code`.
    NotSteward,
    /// No matching verification code or link is currently present for the requested email.
    NoCodeYet,
    /// Encrypting the credential to the requester's age recipient failed.
    SealFailed,
}

impl MailCodeRefusal {
    /// Encode `self` for an `ERR <wire>` line.
    #[must_use]
    pub fn to_wire(&self) -> String {
        match self {
            Self::InvalidRequest(why) => {
                format!("invalid-request {}", aterm_codec::tty::one_line(why, 120))
            }
            Self::Denied => "denied".to_owned(),
            Self::UnknownRequester => "unknown-requester".to_owned(),
            Self::UnauthorizedEmail => "unauthorized-email".to_owned(),
            Self::NotSteward => "not-steward".to_owned(),
            Self::NoCodeYet => "no-code-yet".to_owned(),
            Self::SealFailed => "seal-failed".to_owned(),
        }
    }

    /// Parse an `ERR <wire>` payload into a [`MailCodeRefusal`].
    #[must_use]
    pub fn from_wire(wire: &str) -> Self {
        let trimmed = wire.trim();
        if trimmed == "denied" {
            return Self::Denied;
        }
        if trimmed == "unknown-requester" {
            return Self::UnknownRequester;
        }
        if trimmed == "unauthorized-email" {
            return Self::UnauthorizedEmail;
        }
        if trimmed == "not-steward" {
            return Self::NotSteward;
        }
        if trimmed == "no-code-yet" {
            return Self::NoCodeYet;
        }
        if trimmed == "seal-failed" {
            return Self::SealFailed;
        }
        if let Some(rest) = trimmed.strip_prefix("invalid-request") {
            return Self::InvalidRequest(rest.trim().to_owned());
        }
        Self::InvalidRequest(trimmed.to_owned())
    }
}

/// Audit record written whenever a steward answers `mail-code <email>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailCodeAudit {
    /// Unix timestamp (seconds) of the answer.
    pub at: i64,
    /// Canonical `m<N>` label of the asking Mac.
    pub requester_label: String,
    /// Proved fleet `node_id` of the asking Mac.
    pub requester_node: String,
    /// Target email address answered.
    pub email: String,
    /// Credential kind (`"otp"` or `"magic_link"`), never the secret value itself.
    pub kind: &'static str,
}

impl MailCodeAudit {
    /// Format as a single sanitized log line without secret values.
    #[must_use]
    pub fn log_line(&self) -> String {
        format!(
            "mail-code answered at={} label={} node={} email={} kind={}",
            self.at, self.requester_label, self.requester_node, self.email, self.kind
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct SealedMailCredential {
    kind: String,
    value: String,
}

/// Seal a [`MailboxCredential`] to `recipient` and return a single-line base64 ciphertext.
///
/// # Errors
/// Returns [`MailCodeRefusal::SealFailed`] if serialization or age encryption fails.
pub fn seal_credential_to_recipient(
    cred: &MailboxCredential,
    recipient: &Recipient,
) -> Result<(String, &'static str), MailCodeRefusal> {
    let (kind, value) = match cred {
        MailboxCredential::Otp(code) => ("otp", code.clone()),
        MailboxCredential::MagicLink(url) => ("magic_link", url.clone()),
    };
    let wire = SealedMailCredential {
        kind: kind.to_owned(),
        value,
    };
    let plain = aterm_json::to_vec(&wire).map_err(|_| MailCodeRefusal::SealFailed)?;
    let cipher = age::encrypt(std::slice::from_ref(recipient), &plain)
        .map_err(|_| MailCodeRefusal::SealFailed)?;
    Ok((aterm_secrets::b64::encode_nopad(&cipher), kind))
}

/// Decrypt a base64 age-sealed `mail-code` answer with `identity`.
///
/// # Errors
/// Returns an error string if base64 decoding, age decryption, or JSON decoding fails.
pub fn open_sealed_credential(
    sealed_b64: &str,
    identity: &Identity,
) -> Result<MailboxCredential, String> {
    let cipher = aterm_secrets::b64::decode_nopad(sealed_b64.trim())
        .ok_or_else(|| "invalid base64 in mail-code payload".to_owned())?;
    let plain = age::decrypt(std::slice::from_ref(identity), &cipher)
        .map_err(|e| format!("mail-code age decrypt: {e}"))?;
    let wire: SealedMailCredential = aterm_json::from_slice(plain.as_slice())
        .map_err(|e| format!("mail-code json decode: {e}"))?;
    match wire.kind.as_str() {
        "otp" => Ok(MailboxCredential::Otp(wire.value)),
        "magic_link" => Ok(MailboxCredential::MagicLink(wire.value)),
        other => Err(format!("unknown mail-code credential kind `{other}`")),
    }
}

/// Resolved, authorized requester for `mail-code <email>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedRequester {
    /// Canonical `m<N>` label of the requester in `aterm-swarm-roster`.
    pub label: String,
    /// Validated `age1…` recipient of the requester from `aterm-swarm-roster`.
    pub recipient: Recipient,
}

fn authorize_mail_code_inner(
    (proof_node, proof_key_hex): (&str, Option<&str>),
    req: &MailCodeRequest,
    roster: &SwarmRoster,
    pool: Option<&Pool>,
    claims: Option<&(View, Book)>,
    now: i64,
    allow_any_email_mutant: bool,
) -> Result<AuthorizedRequester, MailCodeRefusal> {
    if roster
        .deny_nodes
        .iter()
        .any(|n| n.eq_ignore_ascii_case(proof_node))
        || proof_key_hex.is_some_and(|key| {
            roster
                .deny_nodes
                .iter()
                .any(|n| n.eq_ignore_ascii_case(key))
        })
    {
        return Err(MailCodeRefusal::Denied);
    }
    if let Some(claimed) = req.claimed_label.as_deref()
        && roster
            .deny_labels
            .iter()
            .any(|d| d.eq_ignore_ascii_case(claimed))
    {
        return Err(MailCodeRefusal::Denied);
    }

    let member = roster
        .members
        .iter()
        .find(|m| m.node_id.as_deref() == Some(proof_node))
        .or_else(|| {
            let key_hex = proof_key_hex?;
            roster.members.iter().find(|m| {
                m.node_key
                    .as_deref()
                    .is_some_and(|k| k.eq_ignore_ascii_case(key_hex))
            })
        })
        .or_else(|| {
            let claimed = req.claimed_label.as_deref()?;
            roster.members.iter().find(|m| {
                m.node_id.is_none()
                    && (m.label.eq_ignore_ascii_case(claimed)
                        || m.aliases.iter().any(|a| a.eq_ignore_ascii_case(claimed)))
            })
        })
        .ok_or(MailCodeRefusal::UnknownRequester)?;

    if member_is_denied(member, roster) {
        return Err(MailCodeRefusal::Denied);
    }
    if let (Some(bound_hex), Some(proved_hex)) = (member.node_key.as_deref(), proof_key_hex)
        && !bound_hex.eq_ignore_ascii_case(proved_hex)
    {
        return Err(MailCodeRefusal::Denied);
    }
    let recipient =
        Recipient::parse(member.recipient.trim()).map_err(|_| MailCodeRefusal::UnknownRequester)?;
    let requester_id = LocalPoolIdentity::new(member.label.clone(), member.aliases.clone(), true);

    if !allow_any_email_mutant {
        let in_warm = pool.is_some_and(|p| {
            p.accounts.iter().any(|acc| {
                acc.email.eq_ignore_ascii_case(&req.email)
                    && acc.warm.iter().any(|w| requester_id.matches(w))
            })
        });
        let in_lease = claims.is_some_and(|(view, book)| {
            book.rows()
                .filter(|row| requester_id.matches(&row.label))
                .any(|row| {
                    row.claims.iter().any(|claim| {
                        claim.key.email.eq_ignore_ascii_case(&req.email)
                            && book
                                .senior(&claim.key, view, now)
                                .is_some_and(|holder| holder.machine == row.machine)
                    })
                })
        });
        if !in_warm && !in_lease {
            return Err(MailCodeRefusal::UnauthorizedEmail);
        }
    }

    Ok(AuthorizedRequester {
        label: member.label.clone(),
        recipient,
    })
}

/// Verify that `proof_node` is an un-denied member in `roster` with a valid `age1…` recipient AND
/// that `req.email` is named in either its `ai-pool.toml` warm set or its senior live claim at
/// `now` (`claims`: this steward's view of every Mac's claims row).
///
/// # Errors
/// Returns [`MailCodeRefusal`] when the node/label is denied, unknown, or not authorized for
/// `req.email`.
pub fn authorize_mail_code_requester(
    proof_node: &str,
    req: &MailCodeRequest,
    roster: &SwarmRoster,
    pool: Option<&Pool>,
    claims: Option<&(View, Book)>,
    now: i64,
) -> Result<AuthorizedRequester, MailCodeRefusal> {
    authorize_mail_code_inner((proof_node, None), req, roster, pool, claims, now, false)
}

/// Negative-control mutant for C17c tests: skips the warm-set / claim check and answers any email
/// requested by a roster member.
#[cfg(test)]
pub fn authorize_mail_code_mutant_any_email(
    proof_node: &str,
    req: &MailCodeRequest,
    roster: &SwarmRoster,
    pool: Option<&Pool>,
    claims: Option<&(View, Book)>,
    now: i64,
) -> Result<AuthorizedRequester, MailCodeRefusal> {
    authorize_mail_code_inner((proof_node, None), req, roster, pool, claims, now, true)
}

#[cfg(unix)]
fn append_audit_record(state_dir: &Path, audit: &MailCodeAudit) {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let _ = aterm_uds::private_file::ensure_private_dir(state_dir);
    let path = state_dir.join(MAIL_CODE_AUDIT_FILE);
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) >= MAIL_CODE_AUDIT_MAX_BYTES {
        let prev = state_dir.join(MAIL_CODE_AUDIT_PREV_FILE);
        let _ = std::fs::rename(&path, &prev);
        let _ = std::fs::set_permissions(&prev, std::fs::Permissions::from_mode(0o600));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
    {
        let _ = writeln!(f, "{}", audit.log_line());
    }
}

/// Evaluate one `mail-code <email>` request against the steward's local state (`state_dir`,
/// `home_dir`), returning `(sealed_b64, audit_record)` on success.
///
/// # Errors
/// Returns [`MailCodeRefusal`] when this Mac holds no steward mailbox, the requester is denied or
/// unauthorized for `<email>`, or no matching verification credential is present yet.
#[cfg(unix)]
pub fn steward_handle_mail_code_in(
    proof_node: &str,
    line: &str,
    state_dir: &Path,
    home_dir: Option<&Path>,
    now: i64,
) -> Result<(String, MailCodeAudit), MailCodeRefusal> {
    steward_handle_mail_code_with_key(proof_node, None, line, state_dir, home_dir, now)
}

#[cfg(unix)]
fn steward_handle_mail_code_with_key(
    proof_node: &str,
    proof_key: Option<aterm_net::fleet::NodeKey>,
    line: &str,
    state_dir: &Path,
    home_dir: Option<&Path>,
    now: i64,
) -> Result<(String, MailCodeAudit), MailCodeRefusal> {
    let req = MailCodeRequest::parse_wire(line).map_err(MailCodeRefusal::InvalidRequest)?;
    let home = home_dir.ok_or(MailCodeRefusal::NotSteward)?;

    let store_dir = state_dir.join("accounts");
    let mut cfg =
        super::headless_oauth::discover_mailbox_config_files(Some(home), Some(&store_dir), None)
            .ok_or(MailCodeRefusal::NotSteward)?;
    // Never recursively relay from a steward handler.
    cfg.relay_enabled = false;
    cfg.relay_home = None;
    cfg.relay_state_dir = None;

    let is_steward_mailbox = has_catch_all_login(&cfg)
        || home.join(".secrets/ai-mailbox.toml").is_file()
        || home.join(".config/aterm/ai-mailbox.toml").is_file()
        || store_dir.join("ai-mailbox.toml").is_file();
    let has_local_backend = cfg.dir.is_some()
        || cfg.http_url_for(Some(&req.email)).is_some()
        || cfg.effective_imap_url(Some(&req.email)).is_some();
    if !is_steward_mailbox || !has_local_backend || !cfg.is_configured_for(Some(&req.email)) {
        return Err(MailCodeRefusal::NotSteward);
    }

    let paths = aterm_secrets::client::Paths::for_home(home);
    if let Some(proved_key) = proof_key {
        let own_key_mismatch = match aterm_net::fleet_member::read_node_identity(state_dir)
            .ok()
            .flatten()
        {
            Some(own) => own.node() == proof_node && own.key() != proved_key,
            None => aterm_net::fleet_member::read_node_id(state_dir)
                .ok()
                .flatten()
                .is_some_and(|own_node| own_node == proof_node),
        };
        if own_key_mismatch {
            return Err(MailCodeRefusal::Denied);
        }
        let log_keys = crate::fleet_log::LocalLog::of(&paths).keys();
        if log_keys.as_ref().is_some_and(|l| l.holds_out(&proved_key)) {
            return Err(MailCodeRefusal::Denied);
        }
        if let Some(bound) = aterm_net::fleet_member::Keyring::read(state_dir).bound(proof_node)
            && bound != proved_key
            && !log_keys
                .as_ref()
                .is_some_and(|l| l.holds_out(&bound) && l.admits(&proved_key))
        {
            return Err(MailCodeRefusal::Denied);
        }
    }

    let mut roster = aterm_secrets::client::swarm_roster(&paths)
        .ok()
        .flatten()
        .ok_or(MailCodeRefusal::NotSteward)?;
    if let Ok(Some(denied)) = aterm_secrets::client::swarm_denied_nodes(&paths) {
        roster.deny_nodes.extend(denied);
        roster.canonicalize();
    }

    let pool_path = home.join(".secrets").join(ai_pool::AI_POOL_FILE);
    let pool = std::fs::read_to_string(&pool_path)
        .ok()
        .and_then(|raw| Pool::from_toml(&raw, &pool_path.display().to_string()).ok());
    let claims = super::claims::read_book(home);

    let key_hex = proof_key.map(|k| k.hex());
    let authorized = authorize_mail_code_inner(
        (proof_node, key_hex.as_deref()),
        &req,
        &roster,
        pool.as_ref(),
        claims.as_ref(),
        now,
        false,
    )?;

    if let Some(since) = req.since {
        cfg.since = Some(cfg.since.map_or(since, |prev| prev.max(since)));
    }
    if !req.senders.is_empty() {
        cfg.senders.clone_from(&req.senders);
    }

    let cred = super::headless_oauth::peek_newest_credential_for_relay(&cfg, &req.email)
        .ok_or(MailCodeRefusal::NoCodeYet)?;
    let (sealed_b64, kind) = seal_credential_to_recipient(&cred, &authorized.recipient)?;
    let audit = MailCodeAudit {
        at: now,
        requester_label: authorized.label,
        requester_node: proof_node.to_owned(),
        email: req.email,
        kind,
    };
    append_audit_record(state_dir, &audit);
    Ok((sealed_b64, audit))
}

/// Serve one `FleetOp::MailCode` stream in the fleet listener / node job, returning the
/// [`MailCodeAudit`] record when a code or link was sealed and sent.
///
/// # Errors
/// Returns an I/O error if reading or writing `stream` fails.
#[cfg(unix)]
pub fn serve_fleet_mail_code<S: std::io::Read + std::io::Write>(
    stream: &mut S,
    proof: Option<&aterm_net::fleet::NodeProof>,
    state_dir: &Path,
    home_dir: Option<&Path>,
    now: i64,
) -> std::io::Result<Result<MailCodeAudit, String>> {
    let mut captured_audit: Option<MailCodeAudit> = None;
    let outcome = aterm_net::fleet_member::serve_mail_code(stream, proof, |proved, line| {
        match steward_handle_mail_code_with_key(
            &proved.node,
            Some(proved.key),
            line,
            state_dir,
            home_dir,
            now,
        ) {
            Ok((sealed, audit)) => {
                captured_audit = Some(audit);
                Ok(sealed)
            }
            Err(refusal) => Err(refusal.to_wire()),
        }
    })?;
    match (outcome, captured_audit) {
        (Ok(_), Some(audit)) => Ok(Ok(audit)),
        (Ok(_), None) => Ok(Err("missing audit record".to_owned())),
        (Err(why), _) => Ok(Err(why)),
    }
}

#[cfg(unix)]
fn read_standby_steward_labels(home: &Path) -> Vec<String> {
    for candidate in [
        home.join(".secrets/swarm-policy.toml"),
        home.join(".config/aterm/swarm-policy.toml"),
    ] {
        let Ok(raw) = std::fs::read_to_string(&candidate) else {
            continue;
        };
        let Ok(table) = aterm_toml::from_str::<aterm_toml::Value>(&raw) else {
            continue;
        };
        if let Some(arr) = table.get("stewards").and_then(aterm_toml::Value::as_array) {
            let mut out = Vec::new();
            for item in arr {
                if let Some(label) = item.as_str().map(str::trim).filter(|s| !s.is_empty())
                    && !out
                        .iter()
                        .any(|existing: &String| existing.eq_ignore_ascii_case(label))
                {
                    out.push(label.to_owned());
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
    }
    Vec::new()
}

/// True if `host` matches `label` by exact stem (`<stem>.local`) or by leading `m<N>` label
/// (`"m22-Macbook-M5-Max.local"` matches `"m22"`, while `"Mac-mini-M4.local"` never matches `"m4"`).
#[cfg(unix)]
fn host_matches_label(host: &str, label: &str) -> bool {
    let host_stem = host.strip_suffix(".local").unwrap_or(host);
    let label_stem = label.strip_suffix(".local").unwrap_or(label);
    if host_stem.eq_ignore_ascii_case(label_stem) {
        return true;
    }
    if let (Some((hn, _)), Some((ln, _))) = (
        ai_pool::leading_m_label(host_stem),
        ai_pool::leading_m_label(label_stem),
    ) {
        return hn == ln;
    }
    false
}

/// True if `peer` (`peer_host`, `peer_node`, `peer_key`) is the Mac `label` names (by
/// `aterm-swarm-roster` `node_id` / `node_key` / alias binding when available, or by
/// [`host_matches_label`]).
#[cfg(unix)]
fn peer_matches_label(
    peer_host: &str,
    peer_node: Option<&str>,
    peer_key: Option<aterm_net::fleet::NodeKey>,
    label: &str,
    roster: Option<&SwarmRoster>,
) -> bool {
    if let Some(roster) = roster {
        if roster.is_label_denied(label)
            || peer_node.is_some_and(|n| roster.is_node_denied(n))
            || peer_key.is_some_and(|k| roster.is_node_denied(&k.hex()))
        {
            return false;
        }
        if let Some(member) = roster.member_by_label_or_alias(label) {
            if member_is_denied(member, roster) {
                return false;
            }
            if let (Some(pnode), Some(mnode)) = (peer_node, member.node_id.as_deref())
                && !pnode.eq_ignore_ascii_case(mnode)
            {
                return false;
            }
            if let (Some(pkey), Some(mkey)) = (peer_key, member.node_key.as_deref())
                && !mkey.eq_ignore_ascii_case(&pkey.hex())
            {
                return false;
            }
            if let (Some(pnode), Some(mnode)) = (peer_node, member.node_id.as_deref())
                && pnode.eq_ignore_ascii_case(mnode)
            {
                return true;
            }
            if let (Some(pkey), Some(mkey)) = (peer_key, member.node_key.as_deref())
                && mkey.eq_ignore_ascii_case(&pkey.hex())
            {
                return true;
            }
            if host_matches_label(peer_host, &member.label)
                || member
                    .aliases
                    .iter()
                    .any(|alias| host_matches_label(peer_host, alias))
            {
                return true;
            }
            return false;
        }
    }
    host_matches_label(peer_host, label)
}

/// Order strictly the steward-candidate peers (`standby_labels` from `swarm-policy.toml`, then
/// `Via::Issuer`) for `FleetOp::MailCode` relay, excluding `own_node` and ordinary non-steward
/// `Via::IssuerBook` / `Via::Joined` members so relay never fans out `O(N)`.
#[cfg(unix)]
#[must_use]
pub fn select_steward_relay_peers<'a>(
    peers: &'a [aterm_net::fleet_member::Peer],
    own_node: &str,
    standby_labels: &[String],
    home_dir: Option<&Path>,
) -> Vec<&'a aterm_net::fleet_member::Peer> {
    use aterm_net::fleet_member::{Peer, Via};
    let (roster, denied_nodes, log_keys) = match home_dir {
        Some(home) => {
            let paths = aterm_secrets::client::Paths::for_home(home);
            let roster = aterm_secrets::client::swarm_roster(&paths).ok().flatten();
            let denied = aterm_secrets::client::swarm_denied_nodes(&paths)
                .ok()
                .flatten()
                .unwrap_or_default();
            let log = crate::fleet_log::LocalLog::of(&paths).keys();
            (roster, denied, log)
        }
        None => (None, Vec::new(), None),
    };
    let peer_is_barred = |p: &Peer| {
        p.node.as_deref().is_some_and(|n| {
            aterm_net::fleet::is_denied_node(n, &denied_nodes)
                || roster.as_ref().is_some_and(|r| r.is_node_denied(n))
        }) || p.key.is_some_and(|k| {
            aterm_net::fleet::is_denied_key(&k, &denied_nodes)
                || roster.as_ref().is_some_and(|r| r.is_node_denied(&k.hex()))
                || log_keys.as_ref().is_some_and(|l| l.holds_out(&k))
        })
    };
    let mut ordered: Vec<&Peer> = Vec::new();
    let push_unique = |ordered: &mut Vec<&'a Peer>, p: &'a Peer| {
        if p.node.as_deref() != Some(own_node)
            && !peer_is_barred(p)
            && !ordered
                .iter()
                .any(|existing| existing.node == p.node && existing.host == p.host)
        {
            ordered.push(p);
        }
    };
    for standby in standby_labels {
        for p in peers {
            if !peer_is_barred(p)
                && peer_matches_label(&p.host, p.node.as_deref(), p.key, standby, roster.as_ref())
            {
                push_unique(&mut ordered, p);
            }
        }
    }
    for p in peers {
        if p.via == Via::Issuer {
            push_unique(&mut ordered, p);
        }
    }
    ordered
}

/// Dial a steward peer over `FleetOp::MailCode` to fetch and decrypt the newest verification
/// credential for `target_email` when `cfg.relay_enabled` is active.
#[cfg(unix)]
pub fn fetch_relay_mail_credential(
    cfg: &MailboxConfig,
    target_email: &str,
    within: Duration,
) -> Option<MailboxCredential> {
    use aterm_net::fleet_member::{
        Via, read_issuer_book, read_node_identity, read_peers, request_mail_code_from_peer,
    };
    if !cfg.relay_enabled {
        return None;
    }
    let home = cfg.relay_home.as_deref()?;
    let state_dir = cfg.relay_state_dir.as_deref()?;
    let paths = aterm_secrets::client::Paths::for_home(home);
    let identity = aterm_secrets::client::read_machine_identity(&paths)
        .ok()
        .flatten()?;
    let me = read_node_identity(state_dir).ok().flatten()?;
    let secret = aterm_net::fleet::read_key_file(state_dir).ok()?;

    let config_path = home.join(".config/aterm/aterm.toml");
    let claimed_label = ai_pool::resolve_local_pool_identity(Some(home), Some(&config_path), None)
        .map(|id| id.primary);

    let req = MailCodeRequest {
        email: target_email.trim().to_ascii_lowercase(),
        since: cfg.since,
        senders: cfg.senders.clone(),
        claimed_label,
    };
    let wire = req.to_wire();

    let mut peers = read_peers(state_dir).unwrap_or_default();
    if let Some(issuer_node) = peers
        .iter()
        .find(|p| p.via == Via::Issuer)
        .and_then(|p| p.node.clone())
    {
        for copy in read_issuer_book(state_dir, &issuer_node) {
            if !peers
                .iter()
                .any(|p| p.node == copy.node && p.node.is_some())
            {
                peers.push(copy);
            }
        }
    }

    let standby_labels = read_standby_steward_labels(home);
    let ordered = select_steward_relay_peers(&peers, me.node(), &standby_labels, Some(home));

    let deadline = std::time::Instant::now() + within;
    for peer in ordered {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match request_mail_code_from_peer(
            &peer.host,
            peer.addr,
            peer.port,
            (&secret, Some(&me)),
            &wire,
            remaining,
        ) {
            Ok(Ok(sealed_b64)) => {
                if let Ok(cred) = open_sealed_credential(&sealed_b64, &identity) {
                    return Some(cred);
                }
            }
            Ok(Err(refusal_wire)) => match MailCodeRefusal::from_wire(&refusal_wire) {
                MailCodeRefusal::NotSteward
                | MailCodeRefusal::UnauthorizedEmail
                | MailCodeRefusal::UnknownRequester
                | MailCodeRefusal::SealFailed => continue,
                MailCodeRefusal::NoCodeYet
                | MailCodeRefusal::Denied
                | MailCodeRefusal::InvalidRequest(_) => return None,
            },
            Err(_) => continue,
        }
    }
    None
}

/// Non-Unix stub (fleet transport and vault paths are Unix-only).
#[cfg(not(unix))]
pub fn fetch_relay_mail_credential(
    _cfg: &MailboxConfig,
    _target_email: &str,
    _within: Duration,
) -> Option<MailboxCredential> {
    None
}

#[cfg(test)]
#[path = "ai_signin_tests.rs"]
mod tests;
