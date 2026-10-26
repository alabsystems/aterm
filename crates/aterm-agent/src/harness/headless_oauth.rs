// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Autonomous headless browser OAuth authorizer over Chrome DevTools Protocol (CDP) pipes.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use aterm_json::{Value, json};

use super::open_url::{
    DEFAULT_MAILBOX_DIR, EMAIL_FILE_MAX, MAILBOX_ENV, MailboxFlow, decode_html_amp,
    decode_mime_base64_parts, decode_quoted_printable, email_matches_target, extract_login_hint,
    percent_decode, percent_encode,
};

/// Environment variable naming a Chromium / `chrome-headless-shell` binary to drive.
pub const BROWSER_BIN_ENV: &str = "ATERM_BROWSER";

/// Environment variable forcing headless browser OAuth authorization (`1` or `true`).
pub const HEADLESS_ENV: &str = "ATERM_OPEN_URL_HEADLESS";

/// Environment variable naming an HTTP(S) or IMAP(S) mailbox URL for OAuth verification emails.
pub const MAILBOX_URL_ENV: &str = "ATERM_OPEN_URL_MAILBOX_URL";

/// Environment variable holding a Bearer token for an HTTP(S) mailbox endpoint.
pub const MAILBOX_TOKEN_ENV: &str = "ATERM_OPEN_URL_MAILBOX_TOKEN";

/// Environment variable naming an IMAP or IMAPS URL.
pub const IMAP_URL_ENV: &str = "ATERM_OPEN_URL_IMAP_URL";

/// Environment variable naming the IMAP / HTTP Basic username.
pub const IMAP_USER_ENV: &str = "ATERM_OPEN_URL_IMAP_USER";

/// Environment variable holding the IMAP / HTTP Basic / Webmail password.
pub const IMAP_PASS_ENV: &str = "ATERM_OPEN_URL_IMAP_PASS";

/// Environment variable naming a webmail inbox URL (e.g. `https://mail.proton.me/u/0/inbox`) for
/// headless CDP polling.
pub const WEBMAIL_URL_ENV: &str = "ATERM_OPEN_URL_WEBMAIL_URL";

/// Environment variable holding a webmail password for headless CDP sign-in.
pub const WEBMAIL_PASS_ENV: &str = "ATERM_OPEN_URL_WEBMAIL_PASS";

/// Environment variable holding a Base32 RFC 6238 TOTP secret for headless webmail 2FA.
pub const TOTP_SECRET_ENV: &str = "ATERM_OPEN_URL_TOTP_SECRET";

/// File name for mailbox configuration under `<accounts store>` or `~/.config/aterm/`.
pub const MAILBOX_TOML_FILE: &str = "mailbox.toml";

/// File name for mailbox configuration under `~/.secrets/`.
pub const SECRETS_MAILBOX_TOML_FILE: &str = "ai-mailbox.toml";

/// Subdirectory under the accounts store holding persistent per-account browser profiles.
pub const BROWSER_PROFILES_DIR: &str = "browsers";

/// Subdirectory under Claude Code's accounts store holding the other CLIs' browser profiles
/// ([`vendor_profile_dir`]): apart from [`BROWSER_PROFILES_DIR`], which Claude Code's own
/// renewals search by email and label ([`resolve_browser_profile_dir`]), so a vendor's sign-in
/// never makes a profile there that one of them would then pick.
pub const VENDOR_PROFILES_DIR: &str = "vendor-browsers";

/// A label as [`MailboxConfig::contested_labels`] keeps it.
fn contest_key(label: &str) -> String {
    label.trim().to_ascii_lowercase().replace('_', "-")
}

/// Configuration for fetching incoming OAuth verification emails (magic links or 6-digit OTPs).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailboxConfig {
    /// Local Maildir / spool directory containing verification message files.
    pub dir: Option<PathBuf>,
    /// HTTP/HTTPS REST / JMAP / Mailpit / Webhook endpoint URL.
    pub http_url: Option<String>,
    /// Optional Bearer token for HTTP mailbox endpoint authentication.
    pub bearer_token: Option<String>,
    /// IMAP / IMAPS endpoint URL.
    pub imap_url: Option<String>,
    /// Webmail inbox URL polled via headless browser CDP.
    pub webmail_url: Option<String>,
    /// Username / email for IMAP, HTTP Basic, or Webmail authentication.
    pub user: Option<String>,
    /// Default password for IMAP, HTTP Basic, or Webmail authentication.
    pub password: Option<String>,
    /// Default second-stage mailbox password (for two-password webmail accounts).
    pub mailbox_password: Option<String>,
    /// Default Base32 TOTP secret (RFC 6238) for webmail 2FA.
    pub totp_secret: Option<String>,
    /// Per-account passwords keyed by lowercase email address.
    pub passwords: BTreeMap<String, String>,
    /// Per-account second-stage mailbox passwords keyed by lowercase email address.
    pub mailbox_passwords: BTreeMap<String, String>,
    /// Per-account Base32 TOTP secrets keyed by lowercase email address.
    pub totp_secrets: BTreeMap<String, String>,
    /// Optional file tracking already-consumed message digests.
    pub seen_file: Option<PathBuf>,
    /// Words in the sender of the vendor's sign-in mail (`connector::Accounts::mail_senders`),
    /// which a webmail inbox's rows are chosen by and, where any is given, every other mail's
    /// `From:` domain must name ([`MailFilter`]); empty for Claude Code's own reads, whose
    /// webmail rows are chosen by `claude` and `anthropic`.
    pub senders: Vec<String>,
    /// Where set, the earliest a mail may be dated (unix seconds): a vendor sign-in's start, so
    /// a code mailed before it is never taken ([`MailFilter`]).
    pub since: Option<i64>,
    /// Labels (lowercased, `_` read as `-`) that rows of two pools, or a pool and a fleet, give
    /// to different emails: their `<label>` secrets are Claude Code's own rows' alone, and
    /// [`MailboxConfig::alias_label_to_email`] maps them onto no other pool's email.
    pub contested_labels: BTreeSet<String>,
}

impl MailboxConfig {
    /// Create a [`MailboxConfig`] backed by a local Maildir, spool directory, or `.mbox` file.
    #[must_use]
    pub fn from_dir(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let seen_file = Some(default_seen_file_for_path(&dir));
        Self {
            dir: Some(dir),
            seen_file,
            ..Self::default()
        }
    }

    /// True when at least one mailbox source (directory, HTTP URL, IMAP URL, or webmail
    /// credentials) is configured.
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.dir.is_some()
            || self.http_url.is_some()
            || self.imap_url.is_some()
            || self.webmail_url.is_some()
            || self.password.is_some()
            || !self.passwords.is_empty()
    }

    /// True when a mailbox source is configured that can serve `target_email`.
    #[must_use]
    pub fn is_configured_for(&self, target_email: Option<&str>) -> bool {
        let has_pw_or_unscoped = target_email.is_none()
            || self.passwords.is_empty()
            || self.password_for(target_email).is_some();
        let imap_has_embedded_pw = self
            .imap_url
            .as_deref()
            .and_then(parse_imap_url)
            .is_some_and(|p| p.password.is_some());
        self.dir.is_some()
            || self.http_url.is_some()
            || (self.imap_url.is_some() && (has_pw_or_unscoped || imap_has_embedded_pw))
            || (self.webmail_url.is_some() && has_pw_or_unscoped)
            || self.password_for(target_email).is_some()
            || (target_email.is_none() && !self.passwords.is_empty())
    }

    /// Look up the password for `target_email` (checking email, local-part, and derived account
    /// label before falling back to `password` or `"*"`).
    #[must_use]
    pub fn password_for(&self, target_email: Option<&str>) -> Option<String> {
        for key in candidate_lookup_keys(target_email) {
            if let Some(p) = self.passwords.get(&key) {
                return Some(p.clone());
            }
        }
        self.password
            .clone()
            .or_else(|| self.passwords.get("*").cloned())
    }

    /// Look up the second-stage mailbox password for `target_email`.
    #[must_use]
    pub fn mailbox_password_for(&self, target_email: Option<&str>) -> Option<String> {
        for key in candidate_lookup_keys(target_email) {
            if let Some(p) = self.mailbox_passwords.get(&key) {
                return Some(p.clone());
            }
        }
        self.mailbox_password
            .clone()
            .or_else(|| self.mailbox_passwords.get("*").cloned())
    }

    /// Look up the Base32 TOTP secret for `target_email`.
    #[must_use]
    pub fn totp_secret_for(&self, target_email: Option<&str>) -> Option<String> {
        for key in candidate_lookup_keys(target_email) {
            if let Some(s) = self.totp_secrets.get(&key) {
                return Some(s.clone());
            }
        }
        self.totp_secret
            .clone()
            .or_else(|| self.totp_secrets.get("*").cloned())
    }

    /// Look up the mailbox login username for `target_email` (preferring an explicit `user`,
    /// otherwise stripping any RFC 5233 `+tag` subaddress from `target_email`).
    #[must_use]
    pub fn webmail_user_for(&self, target_email: Option<&str>) -> Option<String> {
        if let Some(u) = self
            .user
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(u.to_owned());
        }
        let em = target_email.map(str::trim).filter(|s| !s.is_empty())?;
        if let Some((local, domain)) = em.split_once('@') {
            let base_local = local.split('+').next().unwrap_or(local);
            if !base_local.is_empty() {
                return Some(format!("{base_local}@{domain}"));
            }
        }
        Some(em.to_owned())
    }

    /// Map any per-account credentials keyed by `label` onto `email`, unless the label is
    /// contested ([`MailboxConfig::contested_labels`]): then the `<label>` secrets are another
    /// row's, and are never handed to `email`'s mailbox.
    pub fn alias_label_to_email(&mut self, label: Option<&str>, email: Option<&str>) {
        if label.is_some_and(|l| self.contested_labels.contains(&contest_key(l))) {
            return;
        }
        self.alias_own_label_to_email(label, email);
    }

    /// [`MailboxConfig::alias_label_to_email`] for a row of Claude Code's own roster or fleet,
    /// whose labels the `<label>` secret names were made for: aliased contested or not.
    pub fn alias_own_label_to_email(&mut self, label: Option<&str>, email: Option<&str>) {
        let (Some(lbl), Some(em)) = (
            label.map(str::trim).filter(|s| !s.is_empty()),
            email.map(str::trim).filter(|s| !s.is_empty()),
        ) else {
            return;
        };
        let label_key = lbl.to_ascii_lowercase();
        let underscored_key = label_key.replace('-', "_");
        let dashed_key = label_key.replace('_', "-");
        let email_key = em.to_ascii_lowercase();
        for key in [&label_key, &underscored_key, &dashed_key] {
            if let Some(pw) = self.passwords.get(key).cloned() {
                self.passwords.entry(email_key.clone()).or_insert(pw);
            }
            if let Some(mpw) = self.mailbox_passwords.get(key).cloned() {
                self.mailbox_passwords
                    .entry(email_key.clone())
                    .or_insert(mpw);
            }
            if let Some(totp) = self.totp_secrets.get(key).cloned() {
                self.totp_secrets.entry(email_key.clone()).or_insert(totp);
            }
        }
    }

    /// Effective IMAP/IMAPS URL for `target_email`, auto-discovering standard IMAPS endpoints
    /// or a running local ProtonMail Bridge (`127.0.0.1:1143`) when a password is configured.
    #[must_use]
    pub fn effective_imap_url(&self, target_email: Option<&str>) -> Option<String> {
        let has_pw = self.password_for(target_email).is_some();
        if let Some(url) = &self.imap_url {
            let has_embedded_pw = parse_imap_url(url).is_some_and(|p| p.password.is_some());
            if target_email.is_none() || self.passwords.is_empty() || has_pw || has_embedded_pw {
                return Some(url.clone());
            }
            return None;
        }
        if has_pw
            && let Some(email) = target_email
            && let Some((_, domain)) = email.trim().rsplit_once('@')
        {
            let dom = domain.to_ascii_lowercase();
            match dom.as_str() {
                "gmail.com" | "googlemail.com" => {
                    return Some("imaps://imap.gmail.com:993/INBOX".to_owned());
                }
                "fastmail.com" | "fastmail.fm" | "messagingengine.com" => {
                    return Some("imaps://imap.fastmail.com:993/INBOX".to_owned());
                }
                "icloud.com" | "me.com" | "mac.com" => {
                    return Some("imaps://imap.mail.me.com:993/INBOX".to_owned());
                }
                "yahoo.com" | "ymail.com" => {
                    return Some("imaps://imap.mail.yahoo.com:993/INBOX".to_owned());
                }
                "zoho.com" | "zohomail.com" => {
                    return Some("imaps://imap.zoho.com:993/INBOX".to_owned());
                }
                "gmx.com" | "gmx.net" => {
                    return Some("imaps://imap.gmx.com:993/INBOX".to_owned());
                }
                "mail.com" => {
                    return Some("imaps://imap.mail.com:993/INBOX".to_owned());
                }
                "outlook.com" | "hotmail.com" | "live.com" | "msn.com" => {
                    return Some("imaps://outlook.office365.com:993/INBOX".to_owned());
                }
                "purelymail.com" => {
                    return Some("imaps://imap.purelymail.com:993/INBOX".to_owned());
                }
                "migadu.com" => {
                    return Some("imaps://imap.migadu.com:993/INBOX".to_owned());
                }
                "mailbox.org" => {
                    return Some("imaps://imap.mailbox.org:993/INBOX".to_owned());
                }
                "runbox.com" => {
                    return Some("imaps://mail.runbox.com:993/INBOX".to_owned());
                }
                "posteo.de" | "posteo.net" | "posteo.org" => {
                    return Some("imaps://posteo.de:993/INBOX".to_owned());
                }
                "proton.me" | "proton.ch" | "protonmail.com" | "protonmail.ch" | "pm.me" => {
                    let bridge_addr = std::net::SocketAddr::from(([127, 0, 0, 1], 1143));
                    if TcpStream::connect_timeout(&bridge_addr, Duration::from_millis(50)).is_ok() {
                        return Some("imap://127.0.0.1:1143/INBOX".to_owned());
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Effective webmail inbox URL for `target_email` when headless CDP webmail polling is
    /// applicable.
    #[must_use]
    pub fn effective_webmail_url(&self, target_email: Option<&str>) -> Option<String> {
        let has_pw = self.password_for(target_email).is_some();
        if let Some(url) = &self.webmail_url {
            if target_email.is_none() || self.passwords.is_empty() || has_pw {
                return Some(url.clone());
            }
            return None;
        }
        if has_pw
            && let Some(email) = target_email
            && let Some((_, domain)) = email.trim().rsplit_once('@')
        {
            let dom = domain.to_ascii_lowercase();
            match dom.as_str() {
                "proton.me" | "proton.ch" | "protonmail.com" | "protonmail.ch" | "pm.me" => {
                    return Some("https://mail.proton.me/u/0/inbox".to_owned());
                }
                "fastmail.com" | "fastmail.fm" | "messagingengine.com" => {
                    return Some("https://app.fastmail.com/mail/Inbox".to_owned());
                }
                "zoho.com" | "zohomail.com" => {
                    return Some("https://mail.zoho.com/zm/".to_owned());
                }
                "outlook.com" | "hotmail.com" | "live.com" | "msn.com" => {
                    return Some("https://outlook.live.com/mail/0/inbox".to_owned());
                }
                "purelymail.com" => {
                    return Some("https://purelymail.com/webmail/".to_owned());
                }
                "migadu.com" => {
                    return Some("https://webmail.migadu.com/".to_owned());
                }
                "tuta.com" | "tuta.io" | "tutanota.com" | "tutanota.de" | "tutamail.com" => {
                    return Some("https://app.tuta.com/mail".to_owned());
                }
                _ => {}
            }
        }
        None
    }

    /// True when this configuration uses headless webmail only.
    #[must_use]
    pub fn is_webmail_only(&self, target_email: Option<&str>) -> bool {
        self.dir.is_none()
            && self.http_url.is_none()
            && self.effective_imap_url(target_email).is_none()
            && self.effective_webmail_url(target_email).is_some()
    }

    /// Merge missing fields from `other` into `self`.
    pub fn merge_from(&mut self, other: Self) {
        if self.dir.is_none() {
            self.dir = other.dir;
        }
        if self.http_url.is_none() {
            self.http_url = other.http_url;
        }
        if self.bearer_token.is_none() {
            self.bearer_token = other.bearer_token;
        }
        if self.imap_url.is_none() {
            self.imap_url = other.imap_url;
        }
        if self.webmail_url.is_none() {
            self.webmail_url = other.webmail_url;
        }
        if self.user.is_none() {
            self.user = other.user;
        }
        if self.password.is_none() {
            self.password = other.password;
        }
        if self.mailbox_password.is_none() {
            self.mailbox_password = other.mailbox_password;
        }
        if self.totp_secret.is_none() {
            self.totp_secret = other.totp_secret;
        }
        for (k, v) in other.passwords {
            self.passwords.entry(k).or_insert(v);
        }
        for (k, v) in other.mailbox_passwords {
            self.mailbox_passwords.entry(k).or_insert(v);
        }
        for (k, v) in other.totp_secrets {
            self.totp_secrets.entry(k).or_insert(v);
        }
        if self.seen_file.is_none() {
            self.seen_file = other.seen_file;
        }
        self.contested_labels.extend(other.contested_labels);
        if self.senders.is_empty() {
            self.senders = other.senders;
        }
        if self.since.is_none() {
            self.since = other.since;
        }
    }

    /// Parse a [`MailboxConfig`] from a TOML string or a single-line secret.
    #[must_use]
    pub fn from_toml_str(text: &str, home: Option<&Path>) -> Option<Self> {
        let Ok(doc) = aterm_toml::from_str::<aterm_toml::Table>(text) else {
            return parse_single_line_mailbox_secret(text);
        };
        let table = doc
            .get("ai")
            .and_then(aterm_toml::Value::as_table)
            .and_then(|t| t.get("mailbox"))
            .and_then(aterm_toml::Value::as_table)
            .or_else(|| {
                doc.get("harness")
                    .and_then(aterm_toml::Value::as_table)
                    .and_then(|t| t.get("mailbox"))
                    .and_then(aterm_toml::Value::as_table)
            })
            .or_else(|| doc.get("mailbox").and_then(aterm_toml::Value::as_table))
            .unwrap_or(&doc);

        let expand_path = |raw: &str| -> PathBuf {
            if let Some(rest) = raw.strip_prefix("~/")
                && let Some(h) = home
            {
                return h.join(rest);
            }
            PathBuf::from(raw)
        };
        let read_secret_file = |raw: &str| -> Option<String> {
            let path = expand_path(raw);
            let s = super::accounts::read_bounded(&path, 4096).ok()??;
            let trimmed = s.trim_end_matches(['\r', '\n']).trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        };
        let get_str_in = |tbl: &aterm_toml::Table, keys: &[&str]| -> Option<String> {
            keys.iter()
                .find_map(|k| tbl.get(k).and_then(aterm_toml::Value::as_str))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let get_str = |keys: &[&str]| -> Option<String> { get_str_in(table, keys) };

        let mut cfg = Self::default();
        if let Some(d) = get_str(&["dir", "maildir", "path"]) {
            cfg.dir = Some(expand_path(&d));
        }
        if let Some(u) = get_str(&["imap_url", "imap"]) {
            cfg.imap_url = Some(u);
        }
        if let Some(u) = get_str(&["http_url", "webhook_url"]) {
            cfg.http_url = Some(u);
        }
        if let Some(u) = get_str(&["webmail_url", "webmail"]) {
            cfg.webmail_url = Some(u);
        }
        if let Some(u) = get_str(&["url"]) {
            if u.starts_with("imap://") || u.starts_with("imaps://") {
                cfg.imap_url.get_or_insert(u);
            } else if u.starts_with("http://") || u.starts_with("https://") {
                if is_webmail_ui_url(&u) {
                    cfg.webmail_url.get_or_insert(u);
                } else {
                    cfg.http_url.get_or_insert(u);
                }
            } else {
                cfg.dir.get_or_insert_with(|| expand_path(&u));
            }
        }
        cfg.user = get_str(&["user", "username", "imap_user"]);
        cfg.password = get_str(&[
            "password",
            "pass",
            "imap_password",
            "imap_pass",
            "proton_password",
            "webmail_password",
        ])
        .or_else(|| {
            get_str(&["password_file", "pass_file", "webmail_password_file"])
                .and_then(|p| read_secret_file(&p))
        });
        cfg.mailbox_password = get_str(&["mailbox_password", "second_password"]).or_else(|| {
            get_str(&["mailbox_password_file", "second_password_file"])
                .and_then(|p| read_secret_file(&p))
        });
        cfg.totp_secret = get_str(&["totp_secret", "totp", "otpauth", "totp_uri", "otp_secret"])
            .or_else(|| {
                get_str(&[
                    "totp_secret_file",
                    "totp_file",
                    "otpauth_file",
                    "totp_uri_file",
                ])
                .and_then(|p| read_secret_file(&p))
            });
        cfg.bearer_token = get_str(&["bearer_token", "token"]).or_else(|| {
            get_str(&["token_file", "bearer_token_file"]).and_then(|p| read_secret_file(&p))
        });

        for tbl_key in [
            "passwords",
            "password",
            "webmail_passwords",
            "imap_passwords",
        ] {
            if let Some(pw_table) = table.get(tbl_key).and_then(aterm_toml::Value::as_table) {
                for (k, v) in pw_table {
                    if let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                        cfg.passwords
                            .insert(k.trim().to_ascii_lowercase(), s.to_owned());
                    }
                }
            }
        }
        for tbl_key in ["password_files", "pass_files", "webmail_password_files"] {
            if let Some(pw_files) = table.get(tbl_key).and_then(aterm_toml::Value::as_table) {
                for (k, v) in pw_files {
                    if let Some(path_str) = v.as_str().map(str::trim).filter(|s| !s.is_empty())
                        && let Some(secret) = read_secret_file(path_str)
                    {
                        cfg.passwords.insert(k.trim().to_ascii_lowercase(), secret);
                    }
                }
            }
        }
        for tbl_key in ["mailbox_passwords", "mailbox_password", "second_passwords"] {
            if let Some(mpw_table) = table.get(tbl_key).and_then(aterm_toml::Value::as_table) {
                for (k, v) in mpw_table {
                    if let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                        cfg.mailbox_passwords
                            .insert(k.trim().to_ascii_lowercase(), s.to_owned());
                    }
                }
            }
        }
        for tbl_key in ["mailbox_password_files", "second_password_files"] {
            if let Some(mpw_files) = table.get(tbl_key).and_then(aterm_toml::Value::as_table) {
                for (k, v) in mpw_files {
                    if let Some(path_str) = v.as_str().map(str::trim).filter(|s| !s.is_empty())
                        && let Some(secret) = read_secret_file(path_str)
                    {
                        cfg.mailbox_passwords
                            .insert(k.trim().to_ascii_lowercase(), secret);
                    }
                }
            }
        }
        for tbl_key in ["totp", "totp_secrets", "totp_secret", "otpauth"] {
            if let Some(totp_table) = table.get(tbl_key).and_then(aterm_toml::Value::as_table) {
                for (k, v) in totp_table {
                    if let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                        cfg.totp_secrets
                            .insert(k.trim().to_ascii_lowercase(), s.to_owned());
                    }
                }
            }
        }
        for tbl_key in ["totp_files", "totp_secret_files", "otpauth_files"] {
            if let Some(totp_files) = table.get(tbl_key).and_then(aterm_toml::Value::as_table) {
                for (k, v) in totp_files {
                    if let Some(path_str) = v.as_str().map(str::trim).filter(|s| !s.is_empty())
                        && let Some(secret) = read_secret_file(path_str)
                    {
                        cfg.totp_secrets
                            .insert(k.trim().to_ascii_lowercase(), secret);
                    }
                }
            }
        }
        let parse_account_entry = |cfg: &mut Self, raw_key: Option<&str>, v: &aterm_toml::Value| {
            let base_key = raw_key
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_ascii_lowercase);
            if let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                if let Some(key) = base_key {
                    let keys = [key.clone(), key.replace('_', "-")];
                    if parse_otpauth_uri(s).is_some() {
                        for k in keys {
                            cfg.totp_secrets.entry(k).or_insert_with(|| s.to_owned());
                        }
                    } else {
                        for k in keys {
                            cfg.passwords.entry(k).or_insert_with(|| s.to_owned());
                        }
                    }
                }
            } else if let Some(sub) = v.as_table() {
                let mut keys = Vec::new();
                if let Some(k) = base_key {
                    let dashed = k.replace('_', "-");
                    keys.push(k);
                    if !keys.contains(&dashed) {
                        keys.push(dashed);
                    }
                }
                for field in ["email", "user", "username", "label", "name", "id"] {
                    if let Some(val) = get_str_in(sub, &[field]) {
                        let k = val.to_ascii_lowercase();
                        if !keys.contains(&k) {
                            keys.push(k);
                        }
                    }
                }
                if keys.is_empty() {
                    return;
                }
                if let Some(pw) = get_str_in(
                    sub,
                    &[
                        "password",
                        "pass",
                        "imap_password",
                        "imap_pass",
                        "proton_password",
                        "webmail_password",
                    ],
                )
                .or_else(|| {
                    get_str_in(
                        sub,
                        &["password_file", "pass_file", "webmail_password_file"],
                    )
                    .and_then(|p| read_secret_file(&p))
                }) {
                    for k in &keys {
                        cfg.passwords.insert(k.clone(), pw.clone());
                    }
                }
                if let Some(mpw) = get_str_in(sub, &["mailbox_password", "second_password"])
                    .or_else(|| {
                        get_str_in(sub, &["mailbox_password_file", "second_password_file"])
                            .and_then(|p| read_secret_file(&p))
                    })
                {
                    for k in &keys {
                        cfg.mailbox_passwords.insert(k.clone(), mpw.clone());
                    }
                }
                if let Some(totp) = get_str_in(
                    sub,
                    &["totp_secret", "totp", "otpauth", "totp_uri", "otp_secret"],
                )
                .or_else(|| {
                    get_str_in(
                        sub,
                        &[
                            "totp_secret_file",
                            "totp_file",
                            "otpauth_file",
                            "totp_uri_file",
                        ],
                    )
                    .and_then(|p| read_secret_file(&p))
                }) {
                    for k in &keys {
                        cfg.totp_secrets.insert(k.clone(), totp.clone());
                    }
                }
            }
        };
        if let Some(accts_val) = table.get("accounts") {
            if let Some(accts) = accts_val.as_table() {
                for (k, v) in accts {
                    parse_account_entry(&mut cfg, Some(k), v);
                }
            } else if let Some(accts_arr) = accts_val.as_array() {
                for item in accts_arr {
                    parse_account_entry(&mut cfg, None, item);
                }
            }
        }
        for (k, v) in table {
            let lower_k = k.trim().to_ascii_lowercase();
            if matches!(
                lower_k.as_str(),
                "password"
                    | "pass"
                    | "imap_password"
                    | "imap_pass"
                    | "proton_password"
                    | "webmail_password"
                    | "password_file"
                    | "pass_file"
                    | "webmail_password_file"
                    | "mailbox_password"
                    | "second_password"
                    | "mailbox_password_file"
                    | "second_password_file"
                    | "totp_secret"
                    | "totp"
                    | "otpauth"
                    | "totp_uri"
                    | "otp_secret"
                    | "totp_secret_file"
                    | "totp_file"
                    | "otpauth_file"
                    | "totp_uri_file"
            ) {
                continue;
            }
            if let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                if let Some(acct) = lower_k
                    .strip_suffix("_mailbox_password_file")
                    .or_else(|| lower_k.strip_suffix("-mailbox-password-file"))
                    .or_else(|| lower_k.strip_suffix("_second_password_file"))
                    .or_else(|| lower_k.strip_suffix("-second-password-file"))
                    .filter(|a| !a.is_empty())
                {
                    if let Some(secret) = read_secret_file(s) {
                        for key in [acct.to_owned(), acct.replace('_', "-")] {
                            cfg.mailbox_passwords
                                .entry(key)
                                .or_insert_with(|| secret.clone());
                        }
                    }
                    continue;
                }
                if let Some(acct) = lower_k
                    .strip_suffix("_webmail_password_file")
                    .or_else(|| lower_k.strip_suffix("-webmail-password-file"))
                    .or_else(|| lower_k.strip_suffix("_password_file"))
                    .or_else(|| lower_k.strip_suffix("-password-file"))
                    .or_else(|| lower_k.strip_suffix("_pass_file"))
                    .or_else(|| lower_k.strip_suffix("-pass-file"))
                    .filter(|a| !a.is_empty())
                {
                    if let Some(secret) = read_secret_file(s) {
                        for key in [acct.to_owned(), acct.replace('_', "-")] {
                            cfg.passwords.entry(key).or_insert_with(|| secret.clone());
                        }
                    }
                    continue;
                }
                if let Some(acct) = lower_k
                    .strip_suffix("_totp_secret_file")
                    .or_else(|| lower_k.strip_suffix("-totp-secret-file"))
                    .or_else(|| lower_k.strip_suffix("_totp_file"))
                    .or_else(|| lower_k.strip_suffix("-totp-file"))
                    .or_else(|| lower_k.strip_suffix("_otpauth_file"))
                    .or_else(|| lower_k.strip_suffix("-otpauth-file"))
                    .or_else(|| lower_k.strip_suffix("_2fa_file"))
                    .or_else(|| lower_k.strip_suffix("-2fa-file"))
                    .filter(|a| !a.is_empty())
                {
                    if let Some(secret) = read_secret_file(s) {
                        for key in [acct.to_owned(), acct.replace('_', "-")] {
                            cfg.totp_secrets
                                .entry(key)
                                .or_insert_with(|| secret.clone());
                        }
                    }
                    continue;
                }
                if let Some(acct) = lower_k
                    .strip_suffix("_mailbox_password")
                    .or_else(|| lower_k.strip_suffix("-mailbox-password"))
                    .or_else(|| lower_k.strip_suffix("_second_password"))
                    .or_else(|| lower_k.strip_suffix("-second-password"))
                    .filter(|a| !a.is_empty())
                {
                    for key in [acct.to_owned(), acct.replace('_', "-")] {
                        cfg.mailbox_passwords
                            .entry(key)
                            .or_insert_with(|| s.to_owned());
                    }
                    continue;
                }
                if let Some(acct) = lower_k
                    .strip_suffix("_webmail_password")
                    .or_else(|| lower_k.strip_suffix("-webmail-password"))
                    .or_else(|| lower_k.strip_suffix("_password"))
                    .or_else(|| lower_k.strip_suffix("-password"))
                    .or_else(|| lower_k.strip_suffix("_pass"))
                    .or_else(|| lower_k.strip_suffix("-pass"))
                    .filter(|a| !a.is_empty())
                {
                    for key in [acct.to_owned(), acct.replace('_', "-")] {
                        cfg.passwords.entry(key).or_insert_with(|| s.to_owned());
                    }
                    continue;
                }
                if let Some(acct) = lower_k
                    .strip_suffix("_totp_secret")
                    .or_else(|| lower_k.strip_suffix("-totp-secret"))
                    .or_else(|| lower_k.strip_suffix("_totp"))
                    .or_else(|| lower_k.strip_suffix("-totp"))
                    .or_else(|| lower_k.strip_suffix("_otpauth"))
                    .or_else(|| lower_k.strip_suffix("-otpauth"))
                    .or_else(|| lower_k.strip_suffix("_2fa"))
                    .or_else(|| lower_k.strip_suffix("-2fa"))
                    .filter(|a| !a.is_empty())
                {
                    for key in [acct.to_owned(), acct.replace('_', "-")] {
                        cfg.totp_secrets.entry(key).or_insert_with(|| s.to_owned());
                    }
                    continue;
                }
            }
            let is_reserved_table = matches!(
                lower_k.as_str(),
                "passwords"
                    | "webmail_passwords"
                    | "imap_passwords"
                    | "password_files"
                    | "pass_files"
                    | "webmail_password_files"
                    | "mailbox_passwords"
                    | "second_passwords"
                    | "mailbox_password_files"
                    | "second_password_files"
                    | "totp_secrets"
                    | "totp_files"
                    | "totp_secret_files"
                    | "otpauth_files"
                    | "accounts"
                    | "ai"
                    | "harness"
                    | "mailbox"
                    | "fleet"
            );
            if !is_reserved_table
                && (v.as_table().is_some()
                    || k.contains('@')
                    || k == "*"
                    || is_machine_account_label(k)
                    || derive_account_label_from_email(&format!("{lower_k}@proton.me")).is_some())
            {
                parse_account_entry(&mut cfg, Some(k), v);
            }
        }
        let derived_totp_emails: Vec<(String, String, String)> = cfg
            .totp_secrets
            .iter()
            .filter_map(|(k, v)| {
                let email = parse_otpauth_uri(v)?.account_email?;
                Some((k.clone(), email, v.clone()))
            })
            .collect();
        for (k, email, totp_val) in derived_totp_emails {
            cfg.totp_secrets.entry(email.clone()).or_insert(totp_val);
            cfg.alias_label_to_email(Some(&k), Some(&email));
        }
        if let Some(totp) = cfg.totp_secret.clone()
            && let Some(params) = parse_otpauth_uri(&totp)
            && let Some(email) = params.account_email
        {
            cfg.totp_secrets.entry(email.clone()).or_insert(totp);
            if cfg.user.is_none() {
                cfg.user = Some(email);
            }
        }

        (cfg != Self::default()).then_some(cfg)
    }
}

/// Whether a `~/.secrets` name is a fleet roster, the accounts sheet or a CLI's roster
/// (`ai-accounts.csv`, `ai-accounts-<cli>.toml`): fleet data, never a mailbox fragment, whatever
/// machine or account its name happens to carry.
fn is_fleet_roster_name(lower: &str) -> bool {
    lower == "ai-accounts.csv" || lower.starts_with("ai-accounts-")
}

fn is_machine_account_label(key: &str) -> bool {
    let lower = key.trim().to_ascii_lowercase().replace('_', "-");
    let (m_part, slot_part) = lower
        .split_once('-')
        .map_or((lower.as_str(), None), |(a, b)| (a, Some(b)));
    let valid_m = m_part.strip_prefix('m').is_some_and(|d| {
        (1..=3).contains(&d.len())
            && d.bytes().all(|b| b.is_ascii_digit())
            && d.parse::<u32>().is_ok_and(|n| n >= 1)
    });
    let valid_slot = slot_part.is_none_or(|s| {
        (1..=2).contains(&s.len())
            && s.bytes().all(|b| b.is_ascii_digit())
            && s.parse::<u32>().is_ok_and(|n| n >= 1)
    });
    valid_m && valid_slot
}

fn parse_local_machines_and_slot_label(local: &str) -> (Vec<String>, Option<u32>) {
    let tokens: Vec<String> = local
        .split(['.', '-', '_'])
        .map(str::to_ascii_lowercase)
        .collect();
    for (idx, tok) in tokens.iter().enumerate() {
        let is_m = tok.strip_prefix('m').is_some_and(|d| {
            (1..=3).contains(&d.len())
                && d.bytes().all(|b| b.is_ascii_digit())
                && d.parse::<u32>().is_ok_and(|n| (1..=999).contains(&n))
        });
        if !is_m {
            continue;
        }
        let mut machines = vec![tok.clone()];
        let mut slot = None;
        for (rem_idx, next) in tokens[idx + 1..].iter().enumerate() {
            if next.is_empty() || !next.bytes().all(|b| b.is_ascii_digit()) {
                break;
            }
            let Ok(n) = next.parse::<u32>() else {
                break;
            };
            let is_last = tokens
                .get(idx + 1 + rem_idx + 1)
                .is_none_or(|t| t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()));
            if is_last && (2..=9).contains(&n) {
                slot = Some(n);
            } else if (1..=999).contains(&n) {
                let m2 = format!("m{n}");
                if !machines.contains(&m2) {
                    machines.push(m2);
                }
            }
        }
        return (machines, slot);
    }
    (Vec::new(), None)
}

/// Derive a canonical machine/slot account label (such as `"m27"` or `"m27-2"`) from an email
/// address.
#[must_use]
pub fn derive_account_label_from_email(email: &str) -> Option<String> {
    let (raw_local, _) = email.trim().split_once('@')?;
    let (base, plus_tag) = raw_local
        .split_once('+')
        .map_or((raw_local, None), |(b, t)| (b, Some(t)));
    let (mut machines, mut slot) = parse_local_machines_and_slot_label(base);
    if let Some(tag) = plus_tag {
        if machines.is_empty() {
            (machines, slot) = parse_local_machines_and_slot_label(tag);
        } else if slot.is_none()
            && let Some(first) = tag.split(['.', '-', '_', '+']).next()
            && !first.is_empty()
            && first.bytes().all(|b| b.is_ascii_digit())
            && let Ok(n) = first.parse::<u32>()
            && (2..=9).contains(&n)
        {
            slot = Some(n);
        }
    }
    if machines.is_empty() {
        return None;
    }
    let joined = machines.join("-");
    Some(match slot {
        Some(n) => format!("{joined}-{n}"),
        None => joined,
    })
}

fn candidate_lookup_keys(target_email: Option<&str>) -> Vec<String> {
    let mut keys = Vec::new();
    let mut push_unique = |k: String| {
        if !k.is_empty() && !keys.contains(&k) {
            keys.push(k);
        }
    };
    let Some(raw) = target_email.map(str::trim).filter(|s| !s.is_empty()) else {
        return keys;
    };
    let lower = raw.to_ascii_lowercase();
    push_unique(lower.clone());
    push_unique(sanitize_profile_key(&lower));
    if let Some((raw_local, domain)) = lower.split_once('@') {
        let base_local = raw_local.split('+').next().unwrap_or(raw_local);
        let derived_full = derive_account_label_from_email(&lower);
        let base_email = (base_local != raw_local && !base_local.is_empty())
            .then(|| format!("{base_local}@{domain}"));
        let derived_base = base_email
            .as_deref()
            .and_then(derive_account_label_from_email);
        if derived_full != derived_base {
            push_unique(raw_local.to_owned());
            push_unique(raw_local.replace(['.', '_'], "-"));
            push_unique(raw_local.replace(['.', '-'], "_"));
            if let Some(lbl) = &derived_full {
                push_unique(lbl.clone());
                push_unique(lbl.replace('-', "_"));
            }
        }
        if let Some(base_em) = &base_email {
            push_unique(base_em.clone());
            push_unique(sanitize_profile_key(base_em));
        }
        for loc in [raw_local, base_local] {
            push_unique(loc.to_owned());
            push_unique(loc.replace(['.', '_'], "-"));
            push_unique(loc.replace(['.', '-'], "_"));
        }
        if let Some(lbl) = derived_full {
            push_unique(lbl.clone());
            push_unique(lbl.replace('-', "_"));
        }
    } else {
        push_unique(lower.replace(['.', '_'], "-"));
        push_unique(lower.replace(['.', '-'], "_"));
    }
    keys
}

fn is_webmail_ui_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("mail.proton.me")
        || lower.contains("app.fastmail.com")
        || lower.contains("mail.zoho.com")
        || lower.contains("outlook.live.com/mail")
        || lower.contains("purelymail.com/webmail")
        || lower.contains("webmail.migadu.com")
        || lower.contains("app.tuta.com")
}

fn parse_single_line_mailbox_secret(text: &str) -> Option<MailboxConfig> {
    let mut cfg = MailboxConfig::default();
    for raw_line in text.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') {
            return None;
        }
        if let Some(params) = parse_otpauth_uri(trimmed) {
            cfg.totp_secret.get_or_insert_with(|| trimmed.to_owned());
            if let Some(email) = params.account_email {
                cfg.totp_secrets.insert(email.clone(), trimmed.to_owned());
                cfg.user.get_or_insert(email);
            }
            continue;
        }
        if trimmed.starts_with("imap://") || trimmed.starts_with("imaps://") {
            cfg.imap_url.get_or_insert_with(|| trimmed.to_owned());
            continue;
        }
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            if is_webmail_ui_url(trimmed) {
                cfg.webmail_url.get_or_insert_with(|| trimmed.to_owned());
            } else {
                cfg.http_url.get_or_insert_with(|| trimmed.to_owned());
            }
            continue;
        }
        if !trimmed.contains('=') && !trimmed.chars().any(char::is_whitespace) {
            if cfg.password.is_none() {
                cfg.password = Some(trimmed.to_owned());
            } else if cfg.mailbox_password.is_none() {
                cfg.mailbox_password = Some(trimmed.to_owned());
            }
            continue;
        }
        return None;
    }
    if let (Some(u), Some(pw)) = (cfg.user.clone(), cfg.password.clone()) {
        cfg.passwords.entry(u).or_insert(pw);
    }
    (cfg != MailboxConfig::default()).then_some(cfg)
}

fn read_single_line_secret_file(path: &Path) -> Option<String> {
    let s = super::accounts::read_bounded(path, 4096).ok()??;
    let trimmed = s.trim_end_matches(['\r', '\n']).trim();
    (!trimmed.is_empty() && !trimmed.contains(['\r', '\n'])).then(|| trimmed.to_owned())
}

/// The labels that rows of different pools (Claude Code's roster, each other CLI's, and each
/// one's fleet file under `home`'s `~/.secrets`) give to different emails
/// ([`MailboxConfig::contested_labels`]).
fn contested_labels(
    home: Option<&Path>,
    claude_rosters: &[PathBuf],
    cli_rosters: &[(super::accounts::Cli, PathBuf)],
) -> BTreeSet<String> {
    let mut emails: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut note = |label: &str, email: Option<&str>| {
        if let Some(email) = email.map(str::trim).filter(|e| !e.is_empty()) {
            emails
                .entry(contest_key(label))
                .or_default()
                .insert(email.to_ascii_lowercase());
        }
    };
    for path in claude_rosters.iter().filter(|p| p.is_file()) {
        if let Ok(roster) = super::accounts::Roster::load(path) {
            for row in &roster.rows {
                note(&row.label, row.email.as_deref());
            }
        }
    }
    for (cli, path) in cli_rosters.iter().filter(|(_, p)| p.is_file()) {
        if let Ok(roster) = super::accounts::Roster::load_for(*cli, path) {
            for row in &roster.rows {
                note(&row.label, row.email.as_deref());
            }
        }
    }
    if let Some(h) = home {
        let fleet = h
            .join(".secrets")
            .join(super::accounts_sync::FLEET_ROSTER_FILE);
        if let Ok(Some(text)) = super::accounts::read_bounded(&fleet, 256 * 1024)
            && let Ok(fleet) = super::accounts_sync::FleetRoster::from_toml(&text, "fleet")
        {
            for acct in &fleet.accounts {
                note(&acct.label, Some(&acct.email));
            }
        }
        for cli in super::accounts_sync::OTHER_CLIS {
            let path = h
                .join(".secrets")
                .join(super::accounts_sync::cli_fleet_file(cli));
            if let Ok(Some(text)) = super::accounts::read_bounded(&path, 256 * 1024)
                && let Ok(fleet) =
                    super::accounts_sync::FleetRoster::from_toml_for(cli, &text, "fleet")
            {
                for acct in &fleet.accounts {
                    note(&acct.label, Some(&acct.email));
                }
            }
        }
    }
    emails
        .into_iter()
        .filter(|(_, emails)| emails.len() > 1)
        .map(|(label, _)| label)
        .collect()
}

fn discover_mailbox_config_files_raw(
    home: Option<&Path>,
    store_dir: Option<&Path>,
    config_path: Option<&Path>,
) -> MailboxConfig {
    let mut cfg = MailboxConfig::default();
    let mut toml_candidates = Vec::new();
    if let Some(store) = store_dir {
        toml_candidates.push(store.join(MAILBOX_TOML_FILE));
    }
    if let Some(h) = home {
        toml_candidates.push(h.join(".secrets").join(SECRETS_MAILBOX_TOML_FILE));
        toml_candidates.push(h.join(".secrets").join(MAILBOX_TOML_FILE));
        toml_candidates.push(h.join(".config/aterm").join(MAILBOX_TOML_FILE));
        toml_candidates.push(h.join(".config/aterm/aterm.toml"));
    }
    let mut roster_candidates = Vec::new();
    let mut cli_roster_candidates = Vec::new();
    let mut fleet_roster_candidates = Vec::new();
    if let Some(h) = home {
        let roster_path = h.join(".config/aterm/accounts.toml");
        toml_candidates.push(roster_path.clone());
        roster_candidates.push(roster_path);
        for cli in super::accounts::Cli::ALL {
            if cli != super::accounts::Cli::Claude {
                cli_roster_candidates.push((
                    cli,
                    h.join(".config/aterm").join(cli.accounts().roster_file),
                ));
            }
        }
        fleet_roster_candidates.push(
            h.join(".config/aterm")
                .join(super::accounts_sync::FLEET_ROSTER_FILE),
        );
        fleet_roster_candidates.push(
            h.join(".secrets")
                .join(super::accounts_sync::FLEET_ROSTER_FILE),
        );
    }
    if let Some(cfg_file) = config_path {
        if let Some(parent) = cfg_file.parent() {
            toml_candidates.push(parent.join(MAILBOX_TOML_FILE));
            let sibling_roster = parent.join("accounts.toml");
            toml_candidates.push(sibling_roster.clone());
            roster_candidates.push(sibling_roster);
            for cli in super::accounts::Cli::ALL {
                if cli != super::accounts::Cli::Claude {
                    cli_roster_candidates.push((cli, parent.join(cli.accounts().roster_file)));
                }
            }
            fleet_roster_candidates.push(parent.join(super::accounts_sync::FLEET_ROSTER_FILE));
        }
        toml_candidates.push(cfg_file.to_path_buf());
    }
    for path in toml_candidates {
        if path.is_file()
            && let Ok(text) = std::fs::read_to_string(&path)
            && let Some(parsed) = MailboxConfig::from_toml_str(&text, home)
        {
            cfg.merge_from(parsed);
        }
    }
    if let Some(h) = home {
        let secrets_dir = h.join(".secrets");
        if secrets_dir.is_dir() {
            for name in [
                "ai_mailbox_password",
                "ai-mailbox-password",
                "mailbox_password",
                "mailbox-password",
                "proton_password",
                "proton-password",
                "imap_password",
                "webmail_password",
            ] {
                if cfg.password.is_none()
                    && let Some(secret) = read_single_line_secret_file(&secrets_dir.join(name))
                {
                    cfg.password = Some(secret);
                }
            }
            for name in [
                "ai_mailbox_second_password",
                "ai-mailbox-second-password",
                "second_password",
                "second-password",
                "proton_mailbox_password",
                "proton-mailbox-password",
                "proton_second_password",
                "proton-second-password",
            ] {
                if cfg.mailbox_password.is_none()
                    && let Some(secret) = read_single_line_secret_file(&secrets_dir.join(name))
                {
                    cfg.mailbox_password = Some(secret);
                }
            }
            for name in [
                "ai_mailbox_totp",
                "ai-mailbox-totp",
                "mailbox_totp",
                "mailbox-totp",
                "proton_totp",
                "proton-totp",
                "totp_secret",
                "totp-secret",
            ] {
                if cfg.totp_secret.is_none()
                    && let Some(secret) = read_single_line_secret_file(&secrets_dir.join(name))
                {
                    if let Some(params) = parse_otpauth_uri(&secret)
                        && let Some(email) = params.account_email
                    {
                        cfg.totp_secrets
                            .entry(email)
                            .or_insert_with(|| secret.clone());
                    }
                    cfg.totp_secret = Some(secret);
                }
            }
            if let Ok(entries) = std::fs::read_dir(&secrets_dir) {
                for entry in entries.flatten() {
                    if !entry.file_type().is_ok_and(|ft| ft.is_file()) {
                        continue;
                    }
                    let Ok(fname) = entry.file_name().into_string() else {
                        continue;
                    };
                    let lower = fname.to_ascii_lowercase();
                    if matches!(
                        lower.as_str(),
                        "ai_mailbox_password"
                            | "ai-mailbox-password"
                            | "mailbox_password"
                            | "mailbox-password"
                            | "proton_password"
                            | "proton-password"
                            | "imap_password"
                            | "webmail_password"
                            | "ai_mailbox_second_password"
                            | "ai-mailbox-second-password"
                            | "second_password"
                            | "second-password"
                            | "proton_mailbox_password"
                            | "proton-mailbox-password"
                            | "proton_second_password"
                            | "proton-second-password"
                            | "ai_mailbox_totp"
                            | "ai-mailbox-totp"
                            | "mailbox_totp"
                            | "mailbox-totp"
                            | "proton_totp"
                            | "proton-totp"
                            | "totp_secret"
                            | "totp-secret"
                    ) {
                        continue;
                    }
                    let acct_keys =
                        |acct: &str| -> [String; 2] { [acct.to_owned(), acct.replace('_', "-")] };
                    if let Some(acct) = lower
                        .strip_suffix("_mailbox_password")
                        .or_else(|| lower.strip_suffix("-mailbox-password"))
                        .or_else(|| lower.strip_suffix("_second_password"))
                        .or_else(|| lower.strip_suffix("-second-password"))
                        .filter(|a| !a.is_empty())
                    {
                        if let Some(secret) = read_single_line_secret_file(&entry.path()) {
                            for key in acct_keys(acct) {
                                cfg.mailbox_passwords
                                    .entry(key)
                                    .or_insert_with(|| secret.clone());
                            }
                        }
                    } else if let Some(acct) = lower
                        .strip_suffix("_password")
                        .or_else(|| lower.strip_suffix("-password"))
                        .or_else(|| lower.strip_suffix("_pass"))
                        .or_else(|| lower.strip_suffix("-pass"))
                        .or_else(|| lower.strip_suffix(".pass"))
                        .filter(|a| !a.is_empty())
                    {
                        if let Some(secret) = read_single_line_secret_file(&entry.path()) {
                            for key in acct_keys(acct) {
                                cfg.passwords.entry(key).or_insert_with(|| secret.clone());
                            }
                        }
                    } else if let Some(acct) = lower
                        .strip_suffix("_totp_secret")
                        .or_else(|| lower.strip_suffix("-totp-secret"))
                        .or_else(|| lower.strip_suffix("_totp"))
                        .or_else(|| lower.strip_suffix("-totp"))
                        .or_else(|| lower.strip_suffix(".totp"))
                        .or_else(|| lower.strip_suffix("_otpauth"))
                        .or_else(|| lower.strip_suffix("-otpauth"))
                        .or_else(|| lower.strip_suffix("_2fa"))
                        .or_else(|| lower.strip_suffix("-2fa"))
                        .filter(|a| !a.is_empty())
                        && let Some(secret) = read_single_line_secret_file(&entry.path())
                    {
                        if let Some(params) = parse_otpauth_uri(&secret)
                            && let Some(email) = params.account_email
                        {
                            cfg.totp_secrets
                                .entry(email)
                                .or_insert_with(|| secret.clone());
                        }
                        for key in acct_keys(acct) {
                            cfg.totp_secrets
                                .entry(key)
                                .or_insert_with(|| secret.clone());
                        }
                    } else if !lower.starts_with('.')
                        && !is_fleet_roster_name(&lower)
                        && !matches!(
                            lower.as_str(),
                            "fleet-accounts.toml" | "ai-mailbox.toml" | "mailbox.toml"
                        )
                    {
                        let stem = lower
                            .strip_suffix(".toml")
                            .or_else(|| lower.strip_suffix(".txt"))
                            .or_else(|| lower.strip_suffix(".secret"))
                            .or_else(|| lower.strip_suffix(".env"))
                            .or_else(|| lower.strip_suffix(".conf"))
                            .unwrap_or(&lower);
                        let derived = derive_account_label_from_email(&format!("{stem}@proton.me"));
                        if (is_machine_account_label(stem)
                            || stem.contains('@')
                            || derived.is_some())
                            && let Ok(Some(raw_text)) =
                                super::accounts::read_bounded(&entry.path(), 4096)
                            && let Some(parsed) = MailboxConfig::from_toml_str(&raw_text, home)
                        {
                            let mut target_keys = acct_keys(stem).to_vec();
                            if let Some(d) = derived.as_deref() {
                                for k in acct_keys(d) {
                                    if !target_keys.contains(&k) {
                                        target_keys.push(k);
                                    }
                                }
                            }
                            for key in &target_keys {
                                if let Some(pw) = parsed.password.as_ref() {
                                    cfg.passwords
                                        .entry(key.clone())
                                        .or_insert_with(|| pw.clone());
                                }
                                if let Some(mpw) = parsed.mailbox_password.as_ref() {
                                    cfg.mailbox_passwords
                                        .entry(key.clone())
                                        .or_insert_with(|| mpw.clone());
                                }
                                if let Some(totp) = parsed.totp_secret.as_ref() {
                                    cfg.totp_secrets
                                        .entry(key.clone())
                                        .or_insert_with(|| totp.clone());
                                }
                            }
                            for (k, pw) in parsed.passwords {
                                cfg.passwords.entry(k).or_insert(pw);
                            }
                            for (k, mpw) in parsed.mailbox_passwords {
                                cfg.mailbox_passwords.entry(k).or_insert(mpw);
                            }
                            for (em, totp) in parsed.totp_secrets {
                                cfg.totp_secrets.entry(em).or_insert(totp);
                            }
                            if cfg.webmail_url.is_none() {
                                cfg.webmail_url = parsed.webmail_url;
                            }
                            if cfg.imap_url.is_none() {
                                cfg.imap_url = parsed.imap_url;
                            }
                            if cfg.http_url.is_none() {
                                cfg.http_url = parsed.http_url;
                            }
                            if cfg.bearer_token.is_none() {
                                cfg.bearer_token = parsed.bearer_token;
                            }
                            if cfg.dir.is_none() {
                                cfg.dir = parsed.dir;
                            }
                        }
                    }
                }
            }
        }
    }
    let totp_email_aliases: Vec<(String, String)> = cfg
        .totp_secrets
        .iter()
        .filter_map(|(k, v)| {
            let email = parse_otpauth_uri(v)?.account_email?;
            Some((k.clone(), email))
        })
        .collect();
    for (k, email) in totp_email_aliases {
        cfg.alias_label_to_email(Some(&k), Some(&email));
    }
    cfg.contested_labels = contested_labels(home, &roster_candidates, &cli_roster_candidates);
    for roster_path in roster_candidates {
        if roster_path.is_file()
            && let Ok(roster) = super::accounts::Roster::load(&roster_path)
        {
            for row in &roster.rows {
                cfg.alias_own_label_to_email(Some(&row.label), row.email.as_deref());
            }
        }
    }
    for (cli, roster_path) in cli_roster_candidates {
        if roster_path.is_file()
            && let Ok(roster) = super::accounts::Roster::load_for(cli, &roster_path)
        {
            for row in &roster.rows {
                cfg.alias_label_to_email(Some(&row.label), row.email.as_deref());
            }
        }
    }
    for fleet_path in fleet_roster_candidates {
        if fleet_path.is_file()
            && let Ok(Some(text)) = super::accounts::read_bounded(&fleet_path, 256 * 1024)
            && let Ok(fleet) = super::accounts_sync::FleetRoster::from_toml(&text, "fleet")
        {
            for acct in &fleet.accounts {
                cfg.alias_own_label_to_email(Some(&acct.label), Some(&acct.email));
            }
        }
    }
    if let Some(h) = home {
        let csv_path = h.join(".secrets/ai-accounts.csv");
        let csv_text = csv_path
            .is_file()
            .then(|| {
                super::accounts::read_bounded(&csv_path, 512 * 1024)
                    .ok()
                    .flatten()
            })
            .flatten();
        if let Some(csv_text) = csv_text.as_deref()
            && let Ok(fleet) = super::accounts_sync::parse_accounting_csv(csv_text, "csv")
        {
            for acct in &fleet.accounts {
                cfg.alias_own_label_to_email(Some(&acct.label), Some(&acct.email));
            }
        }
        // Every other pool's fleet rows name their labels' emails too: its own file in
        // `~/.secrets`, else its column of the sheet (`accounts_sync::load_cli_fleet`).
        for cli in super::accounts_sync::OTHER_CLIS {
            let fleet_path = h
                .join(".secrets")
                .join(super::accounts_sync::cli_fleet_file(cli));
            let fleet = if fleet_path.is_file() {
                super::accounts::read_bounded(&fleet_path, 256 * 1024)
                    .ok()
                    .flatten()
                    .and_then(|text| {
                        super::accounts_sync::FleetRoster::from_toml_for(cli, &text, "fleet").ok()
                    })
            } else {
                csv_text
                    .as_deref()
                    .filter(|text| super::accounts_sync::csv_names_column(cli, text))
                    .and_then(|text| {
                        super::accounts_sync::parse_accounting_csv_for(cli, text, "csv").ok()
                    })
                    .map(|fleet| fleet.roster)
            };
            for acct in fleet.iter().flat_map(|fleet| &fleet.accounts) {
                cfg.alias_label_to_email(Some(&acct.label), Some(&acct.email));
            }
        }
    }
    if cfg.dir.is_none() {
        if let Some(store) = store_dir {
            let store_mb = store.join(DEFAULT_MAILBOX_DIR);
            if store_mb.is_dir() {
                cfg.dir = Some(store_mb);
            }
        }
        if cfg.dir.is_none()
            && let Some(h) = home
        {
            let config_mb = h.join(".config/aterm").join(DEFAULT_MAILBOX_DIR);
            if config_mb.is_dir() {
                cfg.dir = Some(config_mb);
            }
        }
    }
    if cfg.seen_file.is_none() {
        cfg.seen_file = store_dir
            .map(|s| s.join("mailbox.seen"))
            .or_else(|| cfg.dir.as_deref().map(default_seen_file_for_path));
    }
    cfg
}

fn default_seen_file_for_path(path: &Path) -> PathBuf {
    if path.is_file()
        && let Some(parent) = path.parent()
        && let Some(name) = path.file_name()
    {
        return parent.join(format!(".{}.seen", name.to_string_lossy()));
    }
    path.join(".mailbox.seen")
}

/// Discover any configured mailbox source from on-disk configuration files and directories
/// without reading process-global environment variables.
#[must_use]
pub fn discover_mailbox_config_files(
    home: Option<&Path>,
    store_dir: Option<&Path>,
    config_path: Option<&Path>,
) -> Option<MailboxConfig> {
    let cfg = discover_mailbox_config_files_raw(home, store_dir, config_path);
    cfg.is_configured().then_some(cfg)
}

/// Interpret an explicit mailbox override without changing local path bytes.
#[must_use]
pub(super) fn mailbox_config_from_override(value: &OsStr, home: Option<&Path>) -> MailboxConfig {
    let mut cfg = MailboxConfig::default();
    if value.is_empty() {
        return cfg;
    }
    if let Some(url) = value.to_str().map(str::trim) {
        if url.starts_with("imap://") || url.starts_with("imaps://") {
            cfg.imap_url = Some(url.to_owned());
            return cfg;
        }
        if url.starts_with("http://") || url.starts_with("https://") {
            if is_webmail_ui_url(url) {
                cfg.webmail_url = Some(url.to_owned());
            } else {
                cfg.http_url = Some(url.to_owned());
            }
            return cfg;
        }
    }
    let path = PathBuf::from(value);
    if path
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.trim_end() == "toml")
        && let Ok(text) = std::fs::read_to_string(&path)
        && let Some(parsed) = MailboxConfig::from_toml_str(&text, home)
    {
        cfg.merge_from(parsed);
    } else {
        cfg.seen_file = Some(default_seen_file_for_path(&path));
        cfg.dir = Some(path);
    }
    cfg
}

/// Discover any configured mailbox source.
#[must_use]
pub fn discover_mailbox_config(
    home: Option<&Path>,
    store_dir: Option<&Path>,
) -> Option<MailboxConfig> {
    let mut cfg = MailboxConfig::default();

    let non_empty_env = |name: &str| -> Option<String> {
        std::env::var(name)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };

    if let Some(value) = std::env::var_os(MAILBOX_ENV) {
        cfg.merge_from(mailbox_config_from_override(&value, home));
    }
    if let Some(url) = non_empty_env(MAILBOX_URL_ENV) {
        if url.starts_with("imap://") || url.starts_with("imaps://") {
            cfg.imap_url = Some(url);
        } else if url.starts_with("http://") || url.starts_with("https://") {
            if is_webmail_ui_url(&url) {
                cfg.webmail_url = Some(url);
            } else {
                cfg.http_url = Some(url);
            }
        }
    }
    if let Some(imap) = non_empty_env(IMAP_URL_ENV) {
        cfg.imap_url = Some(imap);
    }
    if let Some(u) = non_empty_env(IMAP_USER_ENV) {
        cfg.user = Some(u);
    }
    if let Some(p) = non_empty_env(IMAP_PASS_ENV) {
        cfg.password = Some(p);
    }
    if let Some(w) = non_empty_env(WEBMAIL_URL_ENV) {
        cfg.webmail_url = Some(w);
    }
    if let Some(p) = non_empty_env(WEBMAIL_PASS_ENV) {
        cfg.password = Some(p);
    }
    if let Some(totp) = non_empty_env(TOTP_SECRET_ENV) {
        if let Some(params) = parse_otpauth_uri(&totp)
            && let Some(email) = params.account_email
        {
            cfg.totp_secrets
                .entry(email.clone())
                .or_insert_with(|| totp.clone());
            cfg.user.get_or_insert(email);
        }
        cfg.totp_secret = Some(totp);
    }
    if let Some(tok) = non_empty_env(MAILBOX_TOKEN_ENV) {
        cfg.bearer_token = Some(tok);
    }

    let xdg_config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(|v| PathBuf::from(v).join("aterm/aterm.toml"));
    cfg.merge_from(discover_mailbox_config_files_raw(
        home,
        store_dir,
        xdg_config.as_deref(),
    ));

    if cfg.seen_file.is_none() {
        cfg.seen_file = store_dir
            .map(|s| s.join("mailbox.seen"))
            .or_else(|| cfg.dir.as_deref().map(default_seen_file_for_path));
    }

    cfg.is_configured().then_some(cfg)
}

/// A discovered headless-capable Chromium binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadlessBrowser {
    /// Path to the executable.
    pub path: PathBuf,
    /// True when the executable is `chrome-headless-shell` (headless by construction).
    pub is_headless_shell: bool,
}

/// Credential extracted from an incoming verification email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailboxCredential {
    /// A verification / sign-in magic link URL.
    MagicLink(String),
    /// A 6-digit one-time verification code (OTP).
    Otp(String),
}

pub(super) fn is_shell_binary(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains("headless-shell") || n.contains("headless_shell"))
}

pub(super) fn browser_cache_roots(home: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
    {
        roots.push(xdg.join("aterm/browser.noindex"));
        roots.push(xdg.join("aterm/browser"));
    }
    if let Some(h) = home {
        if cfg!(target_os = "macos") {
            roots.push(h.join("Library/Caches/aterm/browser.noindex"));
        }
        roots.push(h.join(".cache/aterm/browser.noindex"));
        roots.push(h.join(".cache/aterm/browser"));
    }
    roots
}

/// The chrome-headless-shell unpacked in the browser cache `root`: the newest
/// `chrome-headless-shell-*` directory (by name) that holds one, the browser lane's
/// (`verify provision`) or the harness's own ([`super::headless_fetch`]).
pub(super) fn shell_in(root: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("chrome-headless-shell-"))
        })
        .collect();
    dirs.sort();
    dirs.reverse();
    // Each pinned platform's executable in its unpacked archive, else one laid bare.
    let subs: Vec<PathBuf> = aterm_uds::host::chrome::PINNED
        .iter()
        .map(|pin| aterm_uds::host::chrome::pinned_binary(Path::new(""), pin.platform))
        .chain([PathBuf::from("chrome-headless-shell")])
        .collect();
    for dir in dirs {
        for sub in &subs {
            let candidate = dir.join(sub);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Find a headless-capable Chromium executable.
#[must_use]
pub fn find_headless_browser(home: Option<&Path>) -> Option<HeadlessBrowser> {
    if let Some(named) = std::env::var_os(BROWSER_BIN_ENV).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(named);
        if path.is_file() {
            return Some(HeadlessBrowser {
                is_headless_shell: is_shell_binary(&path),
                path,
            });
        }
    }
    for root in browser_cache_roots(home) {
        if let Some(path) = shell_in(&root) {
            return Some(HeadlessBrowser {
                path,
                is_headless_shell: true,
            });
        }
    }
    if cfg!(target_os = "macos") {
        let apps = [
            "Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
            "Chromium.app/Contents/MacOS/Chromium",
            "Google Chrome.app/Contents/MacOS/Google Chrome",
        ];
        for app in apps {
            let sys = Path::new("/Applications").join(app);
            if sys.is_file() {
                return Some(HeadlessBrowser {
                    path: sys,
                    is_headless_shell: false,
                });
            }
            if let Some(h) = home {
                let usr = h.join("Applications").join(app);
                if usr.is_file() {
                    return Some(HeadlessBrowser {
                        path: usr,
                        is_headless_shell: false,
                    });
                }
            }
        }
    } else if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for name in [
                "chrome-headless-shell",
                "chromium",
                "chromium-browser",
                "google-chrome",
                "google-chrome-stable",
            ] {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(HeadlessBrowser {
                        is_headless_shell: is_shell_binary(&candidate),
                        path: candidate,
                    });
                }
            }
        }
    }
    None
}

/// Sanitize an email or label into a safe directory component for a persistent browser profile.
#[must_use]
pub fn sanitize_profile_key(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(64));
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch.to_ascii_lowercase());
        } else if ch == '@' {
            out.push_str("-at-");
        }
        if out.len() >= 64 {
            break;
        }
    }
    let trimmed = out.trim_matches(['.', '-', '_']);
    if trimmed.is_empty() {
        "default".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Compute the persistent browser profile directory for `account_key` under `store_dir`.
#[must_use]
pub fn browser_profile_dir(store_dir: &Path, account_key: Option<&str>) -> PathBuf {
    let key = account_key.map_or_else(|| "default".to_owned(), sanitize_profile_key);
    store_dir.join(BROWSER_PROFILES_DIR).join(key)
}

fn strip_email_subaddress(email: &str) -> Option<String> {
    let (local, domain) = email.trim().split_once('@')?;
    let (base, tag) = local.split_once('+')?;
    if base.is_empty() {
        return None;
    }
    let base_email = format!("{base}@{domain}");
    if derive_account_label_from_email(email) != derive_account_label_from_email(&base_email) {
        return None;
    }
    let tag_has_account_number = tag
        .split(['.', '-', '_', '+'])
        .any(|tok| !tok.is_empty() && tok.bytes().all(|b| b.is_ascii_digit()));
    (!tag_has_account_number).then_some(base_email)
}

fn exact_email_profile_dir(store_dir: &Path, email: &str) -> PathBuf {
    let lower = email.trim().to_ascii_lowercase();
    let key = sanitize_profile_key(&lower);
    if key == lower.replace('@', "-at-") {
        store_dir.join(BROWSER_PROFILES_DIR).join(key)
    } else {
        store_dir
            .join(BROWSER_PROFILES_DIR)
            .join(format!("{key}-{}", &fnv1a_hex(&lower)[..8]))
    }
}

/// Whether `store_dir` already has a persistent headless browser profile for `label` or `email`.
#[must_use]
pub fn has_browser_profile(store_dir: &Path, label: &str, email: Option<&str>) -> bool {
    resolve_browser_profile_dir(store_dir, Some(label), email).is_dir()
}

/// Infer the account label from `ATERM_ACCOUNT_LABEL`, else from `CLAUDE_CONFIG_DIR`
/// (`<store>/logins/<label>[+<seq>]`) when set ([`super::open_url::account_label_of`]).
#[must_use]
pub fn infer_account_label_from_env() -> Option<String> {
    super::open_url::account_label_of(&|name| std::env::var_os(name))
}

/// Resolve the persistent browser profile directory under `store_dir`, reusing an existing
/// `<store_dir>/browsers/<email>` or `<store_dir>/browsers/<label>` directory when present.
#[must_use]
pub fn resolve_browser_profile_dir(
    store_dir: &Path,
    label: Option<&str>,
    email: Option<&str>,
) -> PathBuf {
    let base_email = email.and_then(strip_email_subaddress);
    if let Some(em) = email {
        let exact_dir = exact_email_profile_dir(store_dir, em);
        if exact_dir.is_dir() {
            return exact_dir;
        }
        if let Some(base) = base_email.as_deref() {
            let base_dir = browser_profile_dir(store_dir, Some(base));
            if base_dir.is_dir() {
                return base_dir;
            }
        }
    }
    if let Some(lbl) = label {
        for candidate in [lbl.to_owned(), lbl.replace('_', "-"), lbl.replace('-', "_")] {
            let dir = browser_profile_dir(store_dir, Some(&candidate));
            if dir.is_dir() {
                return dir;
            }
        }
    }
    if let Some(em) = email
        && let Some(derived) = derive_account_label_from_email(em)
    {
        let dir = browser_profile_dir(store_dir, Some(&derived));
        if dir.is_dir() {
            return dir;
        }
    }
    if let Some(base) = base_email.as_deref() {
        exact_email_profile_dir(store_dir, base)
    } else if let Some(em) = email {
        exact_email_profile_dir(store_dir, em)
    } else {
        browser_profile_dir(store_dir, label)
    }
}

/// The persistent browser profile a vendor sign-in of the row with `email` (else its
/// `label_key`, `<cli>-<label>`) runs in under Claude Code's store `store_dir`: keyed by the
/// EXACT email, never its `+tag`-less base or a label's profile, so a plus-addressed row never
/// signs in through another row's signed-in profile; an email the profile key would not spell
/// one to one (a `+`, or one cut at 64 characters) carries its digest as well. It is the
/// vendors' own, under [`VENDOR_PROFILES_DIR`], unless that is not made yet and Claude Code's
/// own profile of that exact email is (`<store_dir>/browsers/<key>`, the one its renewals pick
/// first for the email): then the vendors share that one. So no vendor sign-in makes a
/// directory Claude Code's renewals would pick ([`resolve_browser_profile_dir`]) in place of a
/// row's label profile, or in place of the person's browser where the row has none.
#[must_use]
pub fn vendor_profile_dir(store_dir: &Path, label_key: &str, email: Option<&str>) -> PathBuf {
    let vendors = store_dir.join(VENDOR_PROFILES_DIR);
    let Some(email) = email.map(str::trim).filter(|e| !e.is_empty()) else {
        return vendors.join(sanitize_profile_key(label_key));
    };
    let lower = email.to_ascii_lowercase();
    let key = sanitize_profile_key(&lower);
    let exact = key == lower.replace('@', "-at-");
    let key = if exact {
        key
    } else {
        format!("{key}-{}", &fnv1a_hex(&lower)[..8])
    };
    let own = vendors.join(&key);
    let claudes = store_dir.join(BROWSER_PROFILES_DIR).join(&key);
    if exact && !own.is_dir() && claudes.is_dir() {
        claudes
    } else {
        own
    }
}

/// The key of a vendor row's browser profile where the row names no email: `<cli>-<label>`
/// (the label as it is where it already starts so).
#[must_use]
pub fn vendor_label_key(cli: super::accounts::Cli, label: &str) -> String {
    let prefix = format!("{}-", cli.id());
    if label.starts_with(&prefix) {
        label.to_owned()
    } else {
        format!("{prefix}{label}")
    }
}

/// The persistent browser profile of `cli`'s row `label` (with `email`): the one its headless
/// sign-ins run in and `accounts browser` opens in a window. Claude Code's is under its own
/// store (`store_dir`, [`resolve_browser_profile_dir`]); every other CLI's is the email's own
/// under Claude Code's store (`shared_dir`, [`vendor_profile_dir`]), shared by every vendor,
/// and with Claude Code only where its own profile of that exact email already is.
#[must_use]
pub fn row_browser_profile(
    cli: super::accounts::Cli,
    store_dir: &Path,
    shared_dir: &Path,
    label: &str,
    email: Option<&str>,
) -> PathBuf {
    match cli {
        super::accounts::Cli::Claude => resolve_browser_profile_dir(store_dir, Some(label), email),
        _ => vendor_profile_dir(shared_dir, &vendor_label_key(cli, label), email),
    }
}

/// Run authorization with a persistent store profile or an owned scratch profile.
/// An authorization error does not make persistent cookies or profile data disposable.
///
/// # Errors
///
/// Scratch allocation failure, a persistent profile another browser or another of aterm's
/// launches has open ([`super::browser_window::ProfileLock::claim`], its
/// [`super::browser_window::Refusal::brief`]), or the authorization's diagnostic, unchanged.
#[cfg(unix)]
pub(super) fn with_browser_profile(
    store_dir: Option<&Path>,
    label: Option<&str>,
    email: Option<&str>,
    authorize: impl FnOnce(&Path) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    let temp_dir;
    let _claimed;
    let profile_dir = if let Some(store) = store_dir {
        let dir = resolve_browser_profile_dir(store, label, email);
        let _ = aterm_types::fs_restricted::ensure_private_dir(dir.parent().unwrap_or(store));
        let _ = aterm_types::fs_restricted::ensure_private_dir(&dir);
        _claimed = super::browser_window::ProfileLock::claim(&dir).map_err(|why| why.brief())?;
        dir
    } else {
        temp_dir =
            aterm_tempfile::tempdir().map_err(|_| "could not create scratch browser profile")?;
        temp_dir.path().to_path_buf()
    };
    authorize(&profile_dir)
}

/// Build the command-line arguments for launching `browser` with `profile_dir`: `--headless=new`
/// unless it is a headless shell, the profile's own flags ([`profile_args`]), the DevTools pipe,
/// and a blank first page.
#[must_use]
pub fn headless_launch_args(browser: &HeadlessBrowser, profile_dir: &Path) -> Vec<OsString> {
    let mut args = Vec::new();
    if !browser.is_headless_shell {
        args.push("--headless=new".into());
    }
    args.extend(profile_args(profile_dir));
    args.push("--remote-debugging-pipe".into());
    args.push("about:blank".into());
    args
}

/// The flags every launch on the browser profile `profile_dir` takes, headless or in a window
/// ([`super::browser_window::window_args`]): the profile itself, the window's size, no first-run
/// or default-browser prompt, no background work, and the profile's own password store and
/// mock keychain, so the cookies one launch saved are the ones the next one reads.
#[must_use]
pub fn profile_args(profile_dir: &Path) -> Vec<OsString> {
    let mut args = Vec::new();
    let mut user_data = OsString::from("--user-data-dir=");
    user_data.push(profile_dir);
    args.push(user_data);
    for flag in [
        "--window-size=1280,900",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-sync",
        "--disable-background-timer-throttling",
        "--disable-renderer-backgrounding",
        "--disable-backgrounding-occluded-windows",
        "--password-store=basic",
        "--use-mock-keychain",
    ] {
        args.push(flag.into());
    }
    // Chromium requires this exception as root; ordinary Linux users keep its sandbox.
    #[cfg(target_os = "linux")]
    if aterm_uds::peer::our_uid() == 0 {
        args.push("--no-sandbox".into());
    }
    args
}

/// How long a headless browser asked to close (`Browser.close`) has to exit by itself before it
/// is killed: a kill at once kept a renewed session's cookies in 3 of 10 runs (the profile lab,
/// docs/DESIGN-ai-accounts-multi-cli-2026-10-23.md §5 (h)).
pub const BROWSER_EXIT_GRACE: Duration = Duration::from_secs(5);

/// A browser this process started over its DevTools pipe. Its caller asks it to close first;
/// dropped (or [`BrowserChild::close`]d), it is given its grace ([`BROWSER_EXIT_GRACE`]) to exit
/// and write its profile, and only then killed and reaped.
#[cfg(unix)]
#[derive(Debug)]
pub struct BrowserChild {
    child: aterm_uds::spawnfd::LaneChild,
    grace: Duration,
}

#[cfg(unix)]
impl BrowserChild {
    /// `child`, given [`BROWSER_EXIT_GRACE`].
    #[must_use]
    pub fn new(child: aterm_uds::spawnfd::LaneChild) -> BrowserChild {
        BrowserChild::with_grace(child, BROWSER_EXIT_GRACE)
    }

    /// `child`, given `grace`.
    #[must_use]
    pub fn with_grace(child: aterm_uds::spawnfd::LaneChild, grace: Duration) -> BrowserChild {
        BrowserChild { child, grace }
    }

    /// Waits up to the grace for the browser to exit, then kills it; its exit status.
    /// # Errors
    /// The child could not be waited for.
    pub fn close(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let until = Instant::now() + self.grace;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= until {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = self.child.kill();
        self.child.wait()
    }
}

#[cfg(unix)]
impl Drop for BrowserChild {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn strip_tag_block(input: &str, tag: &str) -> String {
    let open_pat = format!("<{tag}");
    let close_pat = format!("</{tag}");
    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0usize;
    let mut search_from = 0usize;
    let is_html_space = |b: u8| matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'\x0c');
    while let Some(rel_start) = lower[search_from..].find(&open_pat) {
        let start = search_from + rel_start;
        let name_end = start + open_pat.len();
        if !lower
            .as_bytes()
            .get(name_end)
            .is_some_and(|&b| is_html_space(b) || matches!(b, b'>' | b'/'))
        {
            search_from = name_end;
            continue;
        }
        out.push_str(&input[cursor..start]);
        out.push('\n');
        let close_end = lower[name_end..]
            .match_indices(close_pat.as_str())
            .find_map(|(offset, _)| {
                let end = name_end + offset + close_pat.len();
                let spaces = lower.as_bytes()[end..]
                    .iter()
                    .take_while(|&&b| is_html_space(b))
                    .count();
                (lower.as_bytes().get(end + spaces) == Some(&b'>')).then_some(end + spaces + 1)
            });
        if let Some(end) = close_end {
            cursor = end;
            search_from = end;
        } else {
            return out;
        }
    }
    out.push_str(&input[cursor..]);
    out
}

fn strip_html_tags(input: &str) -> String {
    let uncommented = super::open_url::strip_html_comments(input);
    let cleaned = strip_tag_block(&strip_tag_block(&uncommented, "style"), "script");
    let mut out = String::with_capacity(cleaned.len());
    let mut rest = cleaned.as_str();
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let from_lt = &rest[lt..];
        if let Some(gt) = super::open_url::find_tag_close(from_lt) {
            out.push(' ');
            let tag_inner = &from_lt[1..gt];
            let t = tag_inner
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches('/')
                .to_ascii_lowercase();
            if matches!(
                t.as_str(),
                "p" | "/p"
                    | "div"
                    | "/div"
                    | "tr"
                    | "/tr"
                    | "td"
                    | "/td"
                    | "li"
                    | "/li"
                    | "br"
                    | "h1"
                    | "/h1"
                    | "h2"
                    | "/h2"
                    | "h3"
                    | "/h3"
            ) {
                out.push('\n');
            } else {
                out.push(' ');
            }
            rest = &from_lt[gt + 1..];
        } else {
            out.push('<');
            rest = &from_lt[1..];
        }
    }
    out.push_str(rest);
    out
}

fn is_six_digit_otp(token: &str) -> Option<String> {
    let cleaned = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
    if let Some((a, b)) = cleaned.split_once('-')
        && a.len() == 3
        && b.len() == 3
        && a.bytes().all(|c| c.is_ascii_digit())
        && b.bytes().all(|c| c.is_ascii_digit())
    {
        return Some(format!("{a}{b}"));
    }
    if cleaned.len() == 6 && cleaned.bytes().all(|c| c.is_ascii_digit()) {
        // Avoid matching 6-digit dates/postal/color/query/base64/currency fragments.
        if token.starts_with(['#', ':', '=', '$', '%', '/']) || token.ends_with(['=', '%']) {
            return None;
        }
        return Some(cleaned.to_owned());
    }
    None
}

fn is_mime_part_header_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let Some((name, _)) = trimmed.split_once(':') else {
        return false;
    };
    let name_low = name.trim().to_ascii_lowercase();
    matches!(
        name_low.as_str(),
        "content-type"
            | "content-transfer-encoding"
            | "content-disposition"
            | "content-id"
            | "content-description"
            | "mime-version"
    )
}

/// Extract all candidate 6-digit verification codes (OTPs) from an incoming email addressed to
/// `target_email` in priority order (context-matched codes before fallback numeric tokens).
#[must_use]
pub fn extract_otps_from_email(raw_email: &str, target_email: Option<&str>) -> Vec<String> {
    if let Some(target) = target_email
        && !email_matches_target(raw_email, target)
    {
        return Vec::new();
    }
    let mut extra_text = decode_mime_base64_parts(raw_email);
    if !extra_text.is_empty() {
        extra_text = super::open_url::decode_rfc2047_words(&super::open_url::decode_qp_mime_parts(
            &extra_text,
        ));
    }
    let mut prioritized = Vec::new();
    let push_unique = |list: &mut Vec<String>, code: String| {
        if !list.iter().any(|existing| existing == &code) {
            list.push(code);
        }
    };
    let mut is_json_with_body = false;
    if let Ok(doc) = aterm_json::from_str::<Value>(raw_email) {
        let mut ignored_urls = Vec::new();
        super::open_url::extract_json_email_fields(
            &doc,
            &mut ignored_urls,
            &mut prioritized,
            &mut extra_text,
        );
        if doc.is_object() {
            is_json_with_body = true;
        }
    }
    if !prioritized.is_empty() {
        return prioritized;
    }
    let body_section = super::open_url::split_header_body(raw_email).map_or(raw_email, |(_, b)| b);
    let qp_decoded = if is_json_with_body || super::open_url::is_top_level_base64_email(raw_email) {
        String::new()
    } else if super::open_url::is_quoted_printable_email(raw_email) {
        super::open_url::decode_rfc2047_words(&decode_quoted_printable(body_section))
    } else {
        super::open_url::decode_rfc2047_words(&super::open_url::decode_qp_mime_parts(body_section))
    };
    let subject_extra = if is_json_with_body {
        String::new()
    } else {
        super::open_url::extract_rfc5322_subject(raw_email).unwrap_or_default()
    };
    let srcdoc_extra =
        super::open_url::extract_iframe_srcdoc_fragments(&format!("{qp_decoded}\n{extra_text}"));
    let combined = format!("{srcdoc_extra}\n{extra_text}\n{qp_decoded}\n{subject_extra}");
    let plain = decode_html_amp(&strip_html_tags(&combined));
    let mut caption_promoted = Vec::new();
    let mut standalone_fallbacks = Vec::new();
    let mut fallbacks = Vec::new();
    let mut context_lines_left = 0usize;
    let mut single_digit_run = String::new();
    let mut single_digit_run_context = false;
    let mut nonempty_line_idx = 0usize;
    let mut last_standalone: Option<(usize, String)> = None;
    fn clean_digits(tok: &str) -> Option<&str> {
        let trimmed = tok.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
        (!trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_digit())).then_some(trimmed)
    }
    let flush_single_digit_run = |run: &mut String,
                                  run_ctx: &mut bool,
                                  pri: &mut Vec<String>,
                                  st_fb: &mut Vec<String>,
                                  last_st: &mut Option<(usize, String)>,
                                  line_idx: usize| {
        if run.len() == 6 {
            let code = run.clone();
            if *run_ctx {
                if !pri.iter().any(|existing| existing == &code) {
                    pri.push(code);
                }
            } else {
                if !st_fb.iter().any(|existing| existing == &code) {
                    st_fb.push(code.clone());
                }
                *last_st = Some((line_idx, code));
            }
        }
        run.clear();
        *run_ctx = false;
    };
    for line in plain.lines() {
        let trimmed_line = line.trim();
        if super::open_url::is_mime_boundary_line(trimmed_line, &[]) {
            flush_single_digit_run(
                &mut single_digit_run,
                &mut single_digit_run_context,
                &mut prioritized,
                &mut standalone_fallbacks,
                &mut last_standalone,
                nonempty_line_idx,
            );
            context_lines_left = 0;
            last_standalone = None;
            continue;
        }
        if is_mime_part_header_line(line) {
            continue;
        }
        let raw_words: Vec<&str> = line.split_whitespace().collect();
        if raw_words.is_empty() {
            continue;
        }
        let words: Vec<&str> = line
            .split(|c: char| c.is_whitespace() || !c.is_ascii())
            .filter(|s| !s.is_empty())
            .collect();
        if words.is_empty() {
            continue;
        }
        let line_has_non_ascii_letters = line.chars().any(|c| !c.is_ascii() && c.is_alphabetic());
        nonempty_line_idx += 1;
        if raw_words.len() == 1
            && words.len() == 1
            && !line_has_non_ascii_letters
            && let Some(d) = clean_digits(words[0])
            && d.len() == 1
        {
            if single_digit_run.is_empty() {
                single_digit_run_context = context_lines_left > 0;
            }
            single_digit_run.push_str(d);
            if single_digit_run.len() == 6 {
                flush_single_digit_run(
                    &mut single_digit_run,
                    &mut single_digit_run_context,
                    &mut prioritized,
                    &mut standalone_fallbacks,
                    &mut last_standalone,
                    nonempty_line_idx,
                );
            }
            continue;
        }
        flush_single_digit_run(
            &mut single_digit_run,
            &mut single_digit_run_context,
            &mut prioritized,
            &mut standalone_fallbacks,
            &mut last_standalone,
            nonempty_line_idx.saturating_sub(1),
        );
        let lower = line.to_ascii_lowercase();
        let line_has_context = lower.contains("code")
            || lower.contains("verify")
            || lower.contains("verification")
            || lower.contains("sign in")
            || lower.contains("sign-in")
            || lower.contains("login")
            || lower.contains("log in")
            || lower.contains("one-time")
            || lower.contains("passcode")
            || lower.contains("otp")
            || lower.contains("authenticate")
            || line.contains("验证码")
            || line.contains("验证")
            || line.contains("登录")
            || line.contains("动态码")
            || line.contains("一次性密码")
            || line.contains("安全码")
            || line.contains("口令")
            || line.contains("認證")
            || line.contains("驗證碼")
            || line.contains("認証コード")
            || line.contains("確認コード")
            || line.contains("인증");
        if line_has_context {
            context_lines_left = 4;
            if let Some((prev_idx, ref prev_otp)) = last_standalone
                && nonempty_line_idx.saturating_sub(prev_idx) <= 2
            {
                push_unique(&mut caption_promoted, prev_otp.clone());
            }
        }
        let has_context = context_lines_left > 0;
        if raw_words.len() == 1
            && words.len() == 1
            && !line_has_non_ascii_letters
            && let Some(otp) = is_six_digit_otp(words[0])
        {
            if has_context {
                push_unique(&mut prioritized, otp);
            } else {
                push_unique(&mut standalone_fallbacks, otp.clone());
                last_standalone = Some((nonempty_line_idx, otp));
            }
            if !line_has_context {
                context_lines_left = context_lines_left.saturating_sub(1);
            }
            continue;
        }
        if words.len() == 6
            && words
                .iter()
                .all(|w| clean_digits(w).is_some_and(|d| d.len() == 1))
        {
            let joined: String = words.iter().filter_map(|w| clean_digits(w)).collect();
            if has_context {
                push_unique(&mut prioritized, joined);
            } else {
                push_unique(&mut standalone_fallbacks, joined.clone());
                last_standalone = Some((nonempty_line_idx, joined));
            }
            if !line_has_context {
                context_lines_left = context_lines_left.saturating_sub(1);
            }
            continue;
        }
        let mut idx = 0usize;
        while idx < words.len() {
            if let Some(otp) = is_six_digit_otp(words[idx]) {
                if has_context {
                    push_unique(&mut prioritized, otp);
                } else {
                    push_unique(&mut fallbacks, otp);
                }
                idx += 1;
                continue;
            }
            if has_context
                && idx + 6 <= words.len()
                && (idx == 0 || clean_digits(words[idx - 1]).is_none())
                && (idx + 6 == words.len() || clean_digits(words[idx + 6]).is_none())
                && words[idx..idx + 6]
                    .iter()
                    .all(|w| clean_digits(w).is_some_and(|d| d.len() == 1))
            {
                let joined: String = words[idx..idx + 6]
                    .iter()
                    .filter_map(|w| clean_digits(w))
                    .collect();
                push_unique(&mut prioritized, joined);
                idx += 6;
                continue;
            }
            if has_context
                && idx + 2 <= words.len()
                && (idx == 0 || clean_digits(words[idx - 1]).is_none())
                && (idx + 2 == words.len() || clean_digits(words[idx + 2]).is_none())
                && let (Some(a), Some(b)) = (clean_digits(words[idx]), clean_digits(words[idx + 1]))
                && a.len() == 3
                && b.len() == 3
            {
                push_unique(&mut prioritized, format!("{a}{b}"));
                idx += 2;
                continue;
            }
            idx += 1;
        }
        if !line_has_context {
            context_lines_left = context_lines_left.saturating_sub(1);
        }
    }
    flush_single_digit_run(
        &mut single_digit_run,
        &mut single_digit_run_context,
        &mut prioritized,
        &mut standalone_fallbacks,
        &mut last_standalone,
        nonempty_line_idx,
    );
    for cp in caption_promoted {
        push_unique(&mut prioritized, cp);
    }
    for st in standalone_fallbacks {
        push_unique(&mut prioritized, st);
    }
    for fb in fallbacks {
        push_unique(&mut prioritized, fb);
    }
    prioritized
}

/// Extract a 6-digit verification code (OTP) from an incoming email addressed to `target_email`.
#[must_use]
pub fn extract_otp_from_email(raw_email: &str, target_email: Option<&str>) -> Option<String> {
    extract_otps_from_email(raw_email, target_email)
        .into_iter()
        .next()
}

/// Extract all candidate magic links (or, when the message has no magic links, all 6-digit OTPs)
/// from `raw_email` addressed to `target_email`.
#[must_use]
pub fn extract_all_credentials_from_email(
    raw_email: &str,
    target_email: Option<&str>,
) -> Vec<MailboxCredential> {
    let (auth_links, fallback_links) =
        super::open_url::extract_ranked_magic_links_from_email(raw_email, target_email);
    if !auth_links.is_empty() {
        return auth_links
            .into_iter()
            .chain(fallback_links)
            .map(MailboxCredential::MagicLink)
            .collect();
    }
    let otps = extract_otps_from_email(raw_email, target_email);
    if !otps.is_empty() {
        return otps.into_iter().map(MailboxCredential::Otp).collect();
    }
    fallback_links
        .into_iter()
        .map(MailboxCredential::MagicLink)
        .collect()
}

/// Extract either a magic link or a 6-digit OTP from `raw_email` addressed to `target_email`.
#[must_use]
pub fn extract_credential_from_email(
    raw_email: &str,
    target_email: Option<&str>,
) -> Option<MailboxCredential> {
    extract_all_credentials_from_email(raw_email, target_email)
        .into_iter()
        .next()
}

/// Which mails a sign-in's mailbox read may take a credential from: where `senders` names any
/// word, only a mail whose `From:` address's domain names one as a whole label (`kimi` names
/// `kimi.ai` and `mail.kimi.com`; `x.ai` names `x.ai` and `mail.x.ai`, never `max.ai`), and
/// where `since` is set, only one dated at or after it (less [`MAIL_CLOCK_SLACK_S`]); a mail
/// with no readable sender or date is then refused. A refused mail is neither typed into a page
/// nor marked consumed.
#[derive(Debug, Clone, Copy, Default)]
pub struct MailFilter<'a> {
    /// The vendor's sender words; empty takes any sender (Claude Code's own reads).
    pub senders: &'a [String],
    /// The earliest date a mail may carry (unix seconds).
    pub since: Option<i64>,
}

/// How far a mail's `Date:` may run behind the sign-in's start and still be taken (clock skew).
pub const MAIL_CLOCK_SLACK_S: i64 = 120;

impl MailFilter<'_> {
    /// The filter `cfg` names ([`MailboxConfig::senders`], [`MailboxConfig::since`]).
    #[must_use]
    pub fn of(cfg: &MailboxConfig) -> MailFilter<'_> {
        MailFilter {
            senders: &cfg.senders,
            since: cfg.since,
        }
    }

    /// Whether the mail `raw` (a message's text, or a mailbox API's JSON item) may give its
    /// credential.
    #[must_use]
    pub fn admits(&self, raw: &str) -> bool {
        if self.senders.is_empty() && self.since.is_none() {
            return true;
        }
        let (from, date) = mail_from_and_date(raw);
        if !self.senders.is_empty()
            && !from
                .as_deref()
                .is_some_and(|domain| self.senders.iter().any(|w| domain_names(domain, w)))
        {
            return false;
        }
        match self.since {
            None => true,
            Some(since) => date.is_some_and(|at| at >= since.saturating_sub(MAIL_CLOCK_SLACK_S)),
        }
    }
}

/// Whether the mail domain `domain` names the sender word `word` as whole labels: `word` is the
/// domain or one of its labels (a word without a dot), or the domain's own tail (a word with
/// one).
#[must_use]
pub fn domain_names(domain: &str, word: &str) -> bool {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    let word = word.trim().to_ascii_lowercase();
    if word.is_empty() || domain.is_empty() {
        return false;
    }
    if word.contains('.') {
        domain == word || domain.ends_with(&format!(".{word}"))
    } else {
        domain.split('.').any(|label| label == word)
    }
}

/// The `From:` address's domain and the `Date:` (unix seconds) of the mail `raw`: its header
/// block, or a mailbox API's JSON item (`From`/`from`/`Sender`/`sender`, nested headers, or
/// `{Address|address|emailAddress}`; `Date`/`date`/`Created`/`created_at`/`receivedDateTime`/
/// `receivedAt`/`internalDate`, RFC 3339, RFC 2822, or unix epoch).
#[must_use]
pub fn mail_from_and_date(raw: &str) -> (Option<String>, Option<i64>) {
    let trimmed = raw.trim_start();
    if trimmed.starts_with('{')
        && let Ok(item) = aterm_json::from_str::<Value>(trimmed)
    {
        let from = ["From", "from", "Sender", "sender"]
            .iter()
            .find_map(|k| item.get(k).and_then(address_text))
            .or_else(|| json_header_value(&item, "from"));
        let date = [
            "Date",
            "date",
            "Created",
            "created_at",
            "receivedDateTime",
            "sentDateTime",
            "receivedAt",
            "sentAt",
            "received_at",
            "sent_at",
            "internalDate",
            "timestamp",
        ]
        .iter()
        .find_map(|k| item.get(k).and_then(parse_json_date_value))
        .or_else(|| {
            json_header_value(&item, "date")
                .and_then(|d| parse_mail_date(&d).or_else(|| parse_rfc3339_any(&d)))
        });
        return (from.as_deref().and_then(address_domain), date);
    }
    let head = raw
        .split("\r\n\r\n")
        .next()
        .unwrap_or(raw)
        .split("\n\n")
        .next()
        .unwrap_or(raw);
    let mut from = None;
    let mut date = None;
    let mut last: Option<&mut Option<String>> = None;
    for line in head.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with([' ', '\t']) {
            if let Some(Some(value)) = last.as_mut().map(|l| l.as_mut()) {
                value.push(' ');
                value.push_str(line.trim());
            }
            continue;
        }
        last = None;
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("from") && from.is_none() {
            from = Some(value.trim().to_owned());
            last = Some(&mut from);
        } else if name.eq_ignore_ascii_case("date") && date.is_none() {
            date = Some(value.trim().to_owned());
            last = Some(&mut date);
        }
    }
    let decoded_date = date.map(|d| super::open_url::decode_rfc2047_words(&d));
    (
        from.as_deref().and_then(address_domain),
        decoded_date
            .as_deref()
            .and_then(|d| parse_mail_date(d).or_else(|| parse_rfc3339_any(d))),
    )
}

fn json_header_value(item: &Value, header_name: &str) -> Option<String> {
    let containers = [
        Some(item),
        item.get("payload"),
        item.get("Content"),
        item.get("content"),
        item.get("data"),
        item.get("message"),
        item.get("mail"),
    ];
    for container in containers.into_iter().flatten() {
        let Some(obj) = container.as_object() else {
            continue;
        };
        for (k, v) in obj {
            if !matches!(k.to_ascii_lowercase().as_str(), "headers" | "header") {
                continue;
            }
            if let Some(hdr_map) = v.as_object() {
                for (hk, hv) in hdr_map {
                    if hk.eq_ignore_ascii_case(header_name)
                        && let Some(s) = address_text(hv)
                    {
                        return Some(s);
                    }
                }
            } else if let Some(hdr_arr) = v.as_array() {
                for entry in hdr_arr {
                    let Some(entry_obj) = entry.as_object() else {
                        continue;
                    };
                    let matches_name = entry_obj.iter().any(|(ek, ev)| {
                        matches!(ek.to_ascii_lowercase().as_str(), "name" | "key" | "header")
                            && ev
                                .as_str()
                                .is_some_and(|s| s.trim().eq_ignore_ascii_case(header_name))
                    });
                    if matches_name
                        && let Some((_, ev)) = entry_obj.iter().find(|(ek, _)| {
                            matches!(ek.to_ascii_lowercase().as_str(), "value" | "values" | "val")
                        })
                        && let Some(s) = address_text(ev)
                    {
                        return Some(s);
                    }
                }
            }
        }
    }
    None
}

fn parse_epoch_timestamp(n: i64) -> Option<i64> {
    if n >= 100_000_000_000 {
        Some(n / 1000)
    } else if n >= 100_000_000 {
        Some(n)
    } else {
        None
    }
}

fn parse_json_date_value(v: &Value) -> Option<i64> {
    if let Some(s) = v.as_str() {
        let trimmed = s.trim();
        if !trimmed.is_empty()
            && trimmed.bytes().all(|b| b.is_ascii_digit())
            && let Ok(n) = trimmed.parse::<i64>()
            && let Some(ts) = parse_epoch_timestamp(n)
        {
            return Some(ts);
        }
        return parse_mail_date(trimmed).or_else(|| parse_rfc3339_any(trimmed));
    }
    v.as_i64().and_then(parse_epoch_timestamp)
}

/// The address a mailbox API's `From` names: a string, an object's `Address`, `address`,
/// `email`, nested `emailAddress`, MailHog's `Mailbox` + `Domain`, or the first of an array.
fn address_text(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_owned());
    }
    if let Some(obj) = v.as_object() {
        for k in ["Address", "address", "email"] {
            if let Some(s) = obj.get(k).and_then(Value::as_str) {
                return Some(s.to_owned());
            }
        }
        for k in ["emailAddress", "email_address"] {
            if let Some(nested) = obj.get(k).and_then(address_text) {
                return Some(nested);
            }
        }
        let mb = ["Mailbox", "mailbox", "local", "localPart", "local_part"]
            .iter()
            .find_map(|k| obj.get(*k).and_then(Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let dom = ["Domain", "domain", "host"]
            .iter()
            .find_map(|k| obj.get(*k).and_then(Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let (Some(m), Some(d)) = (mb, dom) {
            return Some(format!("{m}@{d}"));
        }
    }
    v.as_array()?.first().and_then(address_text)
}

/// The domain of the one address in a `From:` value (`Name <a@b.c>`, `a@b.c (Name)`, or `a@b.c`).
fn address_domain(value: &str) -> Option<String> {
    let decoded = super::open_url::decode_rfc2047_words(value);
    let value = decoded.as_str();
    let addr = match (value.rfind('<'), value.rfind('>')) {
        (Some(open), Some(close)) if open < close => &value[open + 1..close],
        _ => value.split('(').next().unwrap_or(value),
    };
    let addr = addr.trim().trim_matches('"');
    let (_, domain) = addr.rsplit_once('@')?;
    let domain = domain.trim().to_ascii_lowercase();
    (!domain.is_empty()
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')))
    .then_some(domain)
}

/// An RFC 3339 time with any offset or fraction (`2026-10-23T22:15:03.5+02:00`), unix seconds.
fn parse_rfc3339_any(text: &str) -> Option<i64> {
    let t = text.trim();
    let (date, time) = t.split_once(['T', 't', ' '])?;
    let mut d = date.split('-');
    let (y, m, day) = (
        d.next()?.parse::<i64>().ok()?,
        d.next()?.parse::<u32>().ok()?,
        d.next()?.parse::<u32>().ok()?,
    );
    let (clock, offset) = match time.find(['Z', 'z', '+', '-']) {
        Some(at) => time.split_at(at),
        None => (time, "Z"),
    };
    let mut c = clock.split(':');
    let (hh, mm) = (
        c.next()?.parse::<i64>().ok()?,
        c.next()?.parse::<i64>().ok()?,
    );
    let ss = c
        .next()
        .and_then(|s| s.split('.').next())
        .map_or(Some(0), |s| s.parse::<i64>().ok())?;
    let zone = match offset {
        "Z" | "z" => 0,
        o => zone_seconds(&o.replace(':', ""))?,
    };
    civil_seconds(y, m, day, hh, mm, ss).map(|at| at - zone)
}

/// An RFC 2822 `Date:` (`Thu, 23 Oct 2026 22:15:03 +0000`, the weekday and seconds optional, a
/// `GMT`/`UT`/`UTC`/US zone name or a numeric offset), unix seconds.
#[must_use]
pub fn parse_mail_date(text: &str) -> Option<i64> {
    let text = text.split('(').next().unwrap_or(text);
    let text = text.split_once(',').map_or(text, |(_, rest)| rest);
    let mut words = text.split_whitespace();
    let day = words.next()?.parse::<u32>().ok()?;
    let month = words.next()?.to_ascii_lowercase();
    let month = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ]
    .iter()
    .position(|m| month.starts_with(m))?;
    let year = words.next()?.parse::<i64>().ok()?;
    let year = match year {
        0..=49 => 2000 + year,
        50..=999 => 1900 + year,
        y => y,
    };
    let mut clock = words.next()?.split(':');
    let (hh, mm) = (
        clock.next()?.parse::<i64>().ok()?,
        clock.next()?.parse::<i64>().ok()?,
    );
    let ss = clock.next().map_or(Some(0), |s| s.parse::<i64>().ok())?;
    let zone = words.next().map_or(Some(0), zone_seconds)?;
    let month = u32::try_from(month).ok()? + 1;
    civil_seconds(year, month, day, hh, mm, ss).map(|at| at - zone)
}

/// A zone's offset east of UTC, in seconds (`+0200`, `-0700`, `GMT`, `EST`, …).
fn zone_seconds(zone: &str) -> Option<i64> {
    let named = match zone.to_ascii_uppercase().as_str() {
        "GMT" | "UT" | "UTC" | "Z" => Some(0),
        "EDT" => Some(-4),
        "EST" | "CDT" => Some(-5),
        "CST" | "MDT" => Some(-6),
        "MST" | "PDT" => Some(-7),
        "PST" => Some(-8),
        _ => None,
    };
    if let Some(hours) = named {
        return Some(hours * 3600);
    }
    let (sign, digits) = match zone.as_bytes().first()? {
        b'+' => (1, &zone[1..]),
        b'-' => (-1, &zone[1..]),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hh = digits[..2].parse::<i64>().ok()?;
    let mm = digits[2..].parse::<i64>().ok()?;
    Some(sign * (hh * 3600 + mm * 60))
}

/// Unix seconds of a UTC civil time (days from civil, Howard Hinnant's algorithm).
fn civil_seconds(y: i64, m: u32, d: u32, hh: i64, mm: i64, ss: i64) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

fn extract_unseen_credential_from_email(
    raw_email: &str,
    target_email: Option<&str>,
    seen_digests: &BTreeSet<String>,
    seen_otps: &BTreeSet<String>,
    filter: &MailFilter<'_>,
) -> Option<(MailboxCredential, bool)> {
    if !filter.admits(raw_email) {
        return None;
    }
    let mut unseen = extract_all_credentials_from_email(raw_email, target_email)
        .into_iter()
        .filter(|cred| match cred {
            MailboxCredential::MagicLink(url) => {
                let digest = fnv1a_hex(url);
                !seen_digests.contains(&digest) && !seen_otps.contains(&digest)
            }
            MailboxCredential::Otp(code) => !seen_otps.contains(&otp_flow_key(code, target_email)),
        });
    let cred = unseen.next()?;
    let exhausted = unseen.next().is_none();
    Some((cred, exhausted))
}

fn record_consumed_credential(
    seen_file: Option<&Path>,
    seen_digests: &mut BTreeSet<String>,
    seen_otps: &mut BTreeSet<String>,
    target_email: Option<&str>,
    message_digest: &str,
    cred: &MailboxCredential,
    exhausted: bool,
) {
    if exhausted {
        record_seen_digest(seen_file, seen_digests, message_digest);
    }
    match cred {
        MailboxCredential::MagicLink(url) => {
            let digest = fnv1a_hex(url);
            record_seen_digest(seen_file, seen_digests, &digest);
            seen_otps.insert(digest);
        }
        MailboxCredential::Otp(code) => {
            seen_otps.insert(otp_flow_key(code, target_email));
        }
    }
}

fn fnv1a_hex(input: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in input.as_bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn otp_flow_key(code: &str, target_email: Option<&str>) -> String {
    format!(
        "otp:{}:{}",
        target_email.unwrap_or("").trim().to_ascii_lowercase(),
        fnv1a_hex(code),
    )
}

fn remember_flow_otp(
    credential: &MailboxCredential,
    target_email: Option<&str>,
    seen_otps: &mut BTreeSet<String>,
) {
    match credential {
        MailboxCredential::MagicLink(url) => {
            seen_otps.insert(fnv1a_hex(url));
        }
        MailboxCredential::Otp(code) => {
            seen_otps.insert(otp_flow_key(code, target_email));
        }
    }
}

fn load_seen_digests(seen_file: Option<&Path>) -> BTreeSet<String> {
    let Some(path) = seen_file else {
        return BTreeSet::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeSet::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn record_seen_digest(seen_file: Option<&Path>, seen: &mut BTreeSet<String>, digest: &str) {
    if !seen.insert(digest.to_owned()) {
        return;
    }
    let Some(path) = seen_file else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{digest}");
    }
}

struct ParsedImapUrl {
    tls: bool,
    user: Option<String>,
    password: Option<String>,
    host: String,
    port: u16,
    mailbox: String,
}

fn strip_ascii_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &s[prefix.len()..])
}

fn parse_imap_url(url: &str) -> Option<ParsedImapUrl> {
    let trimmed = url.trim();
    let (tls, rest) = if let Some(r) = strip_ascii_prefix_ci(trimmed, "imaps://") {
        (true, r)
    } else {
        let r = strip_ascii_prefix_ci(trimmed, "imap://")?;
        (false, r)
    };
    let (authority, tail) = authority_and_tail(rest);
    let path_part = tail.strip_prefix('/').unwrap_or("");
    let (user, password, host_port) = match authority.rsplit_once('@') {
        Some((userinfo, hp)) => match userinfo.split_once(':') {
            Some((u, p)) => (Some(percent_decode(u)), Some(percent_decode(p)), hp),
            None => (Some(percent_decode(userinfo)), None, hp),
        },
        None => (None, None, authority),
    };
    let default_port = if tls { 993 } else { 143 };
    let (host, port) = if let Some(v6) = host_port.strip_prefix('[') {
        let (addr, after) = v6.split_once(']')?;
        if !addr.contains(':') {
            return None;
        }
        let port = match after {
            "" => default_port,
            _ => after.strip_prefix(':')?.parse::<u16>().ok()?,
        };
        (addr.to_owned(), port)
    } else {
        match host_port.rsplit_once(':') {
            Some((h, p)) => (h.to_owned(), p.parse::<u16>().ok()?),
            None => (host_port.to_owned(), default_port),
        }
    };
    if host.is_empty() || port == 0 || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let mailbox_raw = path_part
        .split(['?', '#'])
        .next()
        .unwrap_or(path_part)
        .trim_matches('/');
    let mailbox = if mailbox_raw.is_empty() {
        "INBOX".to_owned()
    } else {
        percent_decode(mailbox_raw)
    };
    Some(ParsedImapUrl {
        tls,
        user,
        password,
        host,
        port,
        mailbox,
    })
}

fn escape_imap_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '\r' | '\n') {
            continue;
        }
        if matches!(ch, '\\' | '"') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn imap_tagged_status(trimmed: &str, tag: &str) -> Option<bool> {
    for (status, ok) in [("OK", true), ("NO", false), ("BAD", false)] {
        let prefix = format!("{tag} {status}");
        if trimmed
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(&prefix))
            && trimmed
                .as_bytes()
                .get(prefix.len())
                .is_none_or(|b| matches!(b, b' ' | b'\t'))
        {
            return Some(ok);
        }
    }
    None
}

fn imap_literal_suffix_len(line: &str) -> std::io::Result<Option<usize>> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let Some((_, inner)) = trimmed.strip_suffix('}').and_then(|s| s.rsplit_once('{')) else {
        return Ok(None);
    };
    let digits = inner.strip_suffix('+').unwrap_or(inner);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(None);
    }
    let n = digits
        .parse::<usize>()
        .ok()
        .filter(|n| *n <= usize::try_from(EMAIL_FILE_MAX).unwrap_or(usize::MAX))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "IMAP literal exceeds mail limit",
            )
        })?;
    Ok(Some(n))
}

fn imap_read_until_tag(reader: &mut impl BufRead, tag: &str) -> Option<(bool, Vec<String>)> {
    const MAX_PROTOCOL_LINES: usize = 16_384;
    let max_bytes = usize::try_from(EMAIL_FILE_MAX).unwrap_or(512 * 1024) + 64 * 1024;
    let mut total_bytes = 0usize;
    let mut lines = Vec::new();
    let mut continuing_untagged = false;
    for _ in 0..MAX_PROTOCOL_LINES {
        let mut line = String::new();
        let n = reader.read_line(&mut line).ok()?;
        if n == 0 {
            return None;
        }
        total_bytes = total_bytes.saturating_add(n);
        if total_bytes > max_bytes {
            return None;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if let Some(ok) = imap_tagged_status(trimmed, tag) {
            return Some((ok, lines));
        }
        let is_untagged = continuing_untagged || trimmed.trim_start().starts_with("* ");
        // An oversized literal still owns the following octets: refuse it rather than
        // treating its body as protocol lines, where a tag-like line could end the read.
        let literal_n = if is_untagged {
            imap_literal_suffix_len(&line).ok()?
        } else {
            None
        };
        lines.push(line);
        if let Some(lit_len) = literal_n {
            total_bytes = total_bytes.saturating_add(lit_len);
            if total_bytes > max_bytes {
                return None;
            }
            if lit_len == 0 {
                lines.push(String::new());
            } else {
                let mut lit = Vec::with_capacity(lit_len.min(64 * 1024));
                reader
                    .by_ref()
                    .take(lit_len as u64)
                    .read_to_end(&mut lit)
                    .ok()?;
                if lit.len() != lit_len {
                    return None;
                }
                lines.push(String::from_utf8_lossy(&lit).into_owned());
            }
            continuing_untagged = true;
        } else {
            continuing_untagged = false;
        }
    }
    None
}

fn imap_search_sequences<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<u32> {
    let mut seqs = Vec::new();
    for line in lines {
        let mut words = line.split_ascii_whitespace();
        if words.next() != Some("*")
            || !words
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case("SEARCH"))
        {
            continue;
        }
        seqs.extend(words.filter_map(|word| word.parse::<u32>().ok()));
    }
    seqs
}

#[test]
fn imap_search_responses_ignore_utf8_and_require_the_search_word() {
    assert_eq!(
        imap_search_sequences([
            "* 12345é",
            "* SEARCHING 99",
            "* SeArCh 1 2 invalid 3",
            "* SEARCH",
            "* search 4",
            "a3 OK SEARCH completed",
        ]),
        [1, 2, 3, 4]
    );
}

#[test]
fn oversized_imap_literals_cannot_turn_body_lines_into_command_responses() {
    let read = |wire: &str| imap_read_until_tag(&mut BufReader::new(wire.as_bytes()), "f0");
    for size in [(EMAIL_FILE_MAX + 1).to_string(), format!("{}0", usize::MAX)] {
        for suffix in ["", "+"] {
            let wire = format!(
                "* 1 FETCH (BODY[] {{{size}{suffix}}}\r\nf0 OK text inside the literal\r\n"
            );
            assert!(read(&wire).is_none(), "refuse literal size {size}{suffix}");
        }
    }

    let limit = usize::try_from(EMAIL_FILE_MAX).unwrap();
    let mut body = String::from("f0 OK text inside the literal\r\n");
    body.push_str(&"x".repeat(limit - body.len()));
    for body in [String::new(), body] {
        for suffix in ["", "+"] {
            let wire = format!(
                "* 1 FETCH (BODY[] {{{}{suffix}}}\r\n{body})\r\nf0 NO actual response\r\n",
                body.len()
            );
            let (ok, lines) = read(&wire).expect("a literal within the mail limit");
            assert!(!ok, "only the actual tagged response ends the command");
            assert_eq!(lines[1], body);
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "TCP IMAP fetch binds host, port, mailbox, credentials, target filter, and seen state"
)]
fn fetch_imap_tcp_credential(
    host: &str,
    port: u16,
    mailbox: &str,
    user: &str,
    password: &str,
    target_email: Option<&str>,
    seen_file: Option<&Path>,
    seen: &mut BTreeSet<String>,
    seen_otps: &mut BTreeSet<String>,
    filter: &MailFilter<'_>,
) -> Option<MailboxCredential> {
    let addr = (host, port).to_socket_addrs().ok()?.next()?;
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let mut writer = stream.try_clone().ok()?;
    let mut reader = BufReader::new(stream);

    let mut greeting = String::new();
    if reader.read_line(&mut greeting).ok()? == 0 {
        return None;
    }

    let login_cmd = format!(
        "a1 LOGIN \"{}\" \"{}\"\r\n",
        escape_imap_quoted(user),
        escape_imap_quoted(password)
    );
    writer.write_all(login_cmd.as_bytes()).ok()?;
    writer.flush().ok()?;
    if !imap_read_until_tag(&mut reader, "a1")?.0 {
        return None;
    }

    let select_cmd = format!("a2 SELECT \"{}\"\r\n", escape_imap_quoted(mailbox));
    writer.write_all(select_cmd.as_bytes()).ok()?;
    writer.flush().ok()?;
    if !imap_read_until_tag(&mut reader, "a2")?.0 {
        return None;
    }

    writer.write_all(b"a3 SEARCH ALL\r\n").ok()?;
    writer.flush().ok()?;
    let (search_ok, search_lines) = imap_read_until_tag(&mut reader, "a3")?;
    if !search_ok {
        return None;
    }

    let seqs = imap_search_sequences(search_lines.iter().map(String::as_str));

    for (idx, seq) in seqs.into_iter().rev().take(10).enumerate() {
        let tag = format!("f{idx}");
        let fetch_cmd = format!("{tag} FETCH {seq} BODY.PEEK[]\r\n");
        if writer.write_all(fetch_cmd.as_bytes()).is_err() || writer.flush().is_err() {
            break;
        }
        let Some((ok, msg_lines)) = imap_read_until_tag(&mut reader, &tag) else {
            break;
        };
        if !ok || msg_lines.is_empty() {
            continue;
        }
        let fetch_idx = msg_lines.iter().position(|l| {
            let t = l.trim_start();
            t.starts_with("* ") && t.to_ascii_uppercase().contains("FETCH")
        });
        let literal_body = fetch_idx.and_then(|fi| {
            let mut li = fi;
            let mut fallback_literal: Option<&str> = None;
            while li < msg_lines.len() {
                if li > fi && msg_lines[li].trim_start().starts_with("* ") {
                    break;
                }
                if imap_literal_suffix_len(&msg_lines[li])
                    .ok()
                    .flatten()
                    .is_some()
                    && let Some(body) = msg_lines.get(li + 1)
                {
                    let upper = msg_lines[li].to_ascii_uppercase();
                    let is_full_body = upper.contains("BODY[]")
                        || upper.contains("BODY.PEEK[]")
                        || (upper.contains("RFC822")
                            && !upper.contains("RFC822.HEADER")
                            && !upper.contains("RFC822.SIZE")
                            && !upper.contains("RFC822.TEXT"));
                    if is_full_body {
                        return Some(body.as_str());
                    }
                    let is_header_only =
                        upper.contains("RFC822.HEADER") || upper.contains("BODY[HEADER");
                    if !is_header_only || fallback_literal.is_none() {
                        fallback_literal = Some(body.as_str());
                    }
                    li += 2;
                    continue;
                }
                li += 1;
            }
            fallback_literal
        });
        let start = fetch_idx.map_or_else(
            || {
                usize::from(
                    msg_lines
                        .first()
                        .is_some_and(|l| l.trim_start().starts_with("* ")),
                )
            },
            |fi| fi + 1,
        );
        let trimmed_fallback: String;
        let raw_msg = if let Some(body) = literal_body {
            body
        } else {
            let mut end = msg_lines.len();
            while end > start && msg_lines[end - 1].trim_start().starts_with("* ") {
                end -= 1;
            }
            if end > start && msg_lines[end - 1].trim_start().starts_with(')') {
                end -= 1;
            }
            trimmed_fallback = msg_lines[start..end].concat();
            trimmed_fallback
                .trim_end_matches(['\r', '\n'])
                .strip_suffix(')')
                .unwrap_or(&trimmed_fallback)
        };
        if raw_msg.trim().is_empty() {
            continue;
        }
        let digest = fnv1a_hex(raw_msg);
        if seen.contains(&digest) {
            continue;
        }
        if let Some((cred, exhausted)) =
            extract_unseen_credential_from_email(raw_msg, target_email, seen, seen_otps, filter)
        {
            let store_cmd = if exhausted {
                format!("s{idx} STORE {seq} +FLAGS (\\Seen)\r\na9 LOGOUT\r\n")
            } else {
                "a9 LOGOUT\r\n".to_owned()
            };
            let _ = writer.write_all(store_cmd.as_bytes());
            let _ = writer.flush();
            let _ = imap_read_until_tag(&mut reader, "a9");
            record_consumed_credential(
                seen_file,
                seen,
                seen_otps,
                target_email,
                &digest,
                &cred,
                exhausted,
            );
            return Some(cred);
        }
    }

    let _ = writer.write_all(b"a9 LOGOUT\r\n");
    let _ = writer.flush();
    let _ = imap_read_until_tag(&mut reader, "a9");
    None
}

#[expect(
    clippy::too_many_arguments,
    reason = "IMAPS fetch binds the URL, credentials, target filter, seen state and the vendor mail filter"
)]
fn fetch_imaps_curl_credential(
    parsed: &ParsedImapUrl,
    user: &str,
    password: &str,
    target_email: Option<&str>,
    seen_file: Option<&Path>,
    seen: &mut BTreeSet<String>,
    seen_otps: &mut BTreeSet<String>,
    filter: &MailFilter<'_>,
) -> Option<MailboxCredential> {
    let userpwd = format!("{user}:{password}");
    let url_host = if parsed.host.contains(':') {
        format!("[{}]", parsed.host)
    } else {
        parsed.host.clone()
    };
    let search_url = format!(
        "imaps://{url_host}:{}/{}?ALL",
        parsed.port,
        percent_encode(&parsed.mailbox)
    );
    let out = super::open_url::curl_output(
        &[
            "--disable",
            "--globoff",
            "--proto",
            "=imaps",
            "-sS",
            "--max-time",
            "10",
        ],
        &[("user", &userpwd), ("url", &search_url)],
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let search_stdout = String::from_utf8_lossy(&out.stdout);
    let seqs = imap_search_sequences(search_stdout.lines());
    for seq in seqs.into_iter().rev().take(8) {
        let msg_url = format!(
            "imaps://{url_host}:{}/{};MAILINDEX={seq}",
            parsed.port,
            percent_encode(&parsed.mailbox)
        );
        let Ok(msg_out) = super::open_url::curl_output(
            &[
                "--disable",
                "--globoff",
                "--proto",
                "=imaps",
                "-sS",
                "--max-time",
                "10",
            ],
            &[("user", &userpwd), ("url", &msg_url)],
        ) else {
            continue;
        };
        if !msg_out.status.success() {
            continue;
        }
        let raw_msg = String::from_utf8_lossy(&msg_out.stdout);
        let digest = fnv1a_hex(&raw_msg);
        if seen.contains(&digest) {
            continue;
        }
        if let Some((cred, exhausted)) =
            extract_unseen_credential_from_email(&raw_msg, target_email, seen, seen_otps, filter)
        {
            record_consumed_credential(
                seen_file,
                seen,
                seen_otps,
                target_email,
                &digest,
                &cred,
                exhausted,
            );
            return Some(cred);
        }
    }
    None
}

fn fetch_imap_credential(
    cfg: &MailboxConfig,
    target_email: Option<&str>,
    seen: &mut BTreeSet<String>,
    seen_otps: &mut BTreeSet<String>,
) -> Option<MailboxCredential> {
    let imap_url = cfg.effective_imap_url(target_email)?;
    if cleartext_refusal(&imap_url).is_some() {
        return None;
    }
    let parsed = parse_imap_url(&imap_url)?;
    let fallback_user = cfg.webmail_user_for(target_email);
    let user = parsed
        .user
        .as_deref()
        .or(fallback_user.as_deref())
        .unwrap_or("user");
    let cfg_pw = cfg.password_for(target_email);
    let password = parsed
        .password
        .as_deref()
        .or(cfg_pw.as_deref())
        .unwrap_or("");
    if parsed.tls {
        fetch_imaps_curl_credential(
            &parsed,
            user,
            password,
            target_email,
            cfg.seen_file.as_deref(),
            seen,
            seen_otps,
            &MailFilter::of(cfg),
        )
    } else {
        fetch_imap_tcp_credential(
            &parsed.host,
            parsed.port,
            &parsed.mailbox,
            user,
            password,
            target_email,
            cfg.seen_file.as_deref(),
            seen,
            seen_otps,
            &MailFilter::of(cfg),
        )
    }
}

fn authority_and_tail(rest: &str) -> (&str, &str) {
    rest.find(['/', '?', '#'])
        .map_or((rest, ""), |end| rest.split_at(end))
}

/// Whether `host` is this machine's loopback (`localhost`, `127.0.0.0/8`, `::1`).
fn is_loopback_host(host: &str) -> bool {
    let bare = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .filter(|s| s.contains(':'))
        .unwrap_or(host);
    bare.eq_ignore_ascii_case("localhost")
        || bare
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Why no credential is sent to `url`: `imap://` and `http://` carry it in cleartext, so they are
/// used only for a loopback host.
fn cleartext_refusal(url: &str) -> Option<&'static str> {
    let trimmed = url.trim();
    if strip_ascii_prefix_ci(trimmed, "imap://").is_some() {
        let allowed = parse_imap_url(trimmed).is_some_and(|imap| is_loopback_host(&imap.host));
        return (!allowed).then_some(
            "imap:// sends its login in cleartext, so it is used only for a loopback host; \
             name an imaps:// mailbox",
        );
    }
    let rest = strip_ascii_prefix_ci(trimmed, "http://")?;
    let (authority, _) = authority_and_tail(rest);
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
    let host = match host_port.strip_prefix('[') {
        Some(v6) => v6
            .split_once(']')
            .filter(|(addr, after)| {
                addr.contains(':')
                    && (after.is_empty()
                        || after
                            .strip_prefix(':')
                            .and_then(|p| p.parse::<u16>().ok())
                            .is_some_and(|p| p > 0))
            })
            .map_or("", |(addr, _)| addr),
        None => host_port.rsplit_once(':').map_or(host_port, |(h, p)| {
            if p.parse::<u16>().is_ok_and(|port| port > 0) {
                h
            } else {
                ""
            }
        }),
    };
    (!is_loopback_host(host)).then_some(
        "http:// sends its password or token in cleartext, so it is used only for a loopback \
         host; name an https:// mailbox",
    )
}

/// Why `cfg` is not polled for `target_email`: an `imap://` login, or an `http://` password or
/// token, to a host that is not loopback ([`cleartext_refusal`]).
#[must_use]
pub fn plaintext_credential_refusal(
    cfg: &MailboxConfig,
    target_email: Option<&str>,
) -> Option<&'static str> {
    if let Some(why) = cfg
        .effective_imap_url(target_email)
        .as_deref()
        .and_then(cleartext_refusal)
    {
        return Some(why);
    }
    if let Some(url) = cfg.http_url.as_deref() {
        let credentialed = url_has_embedded_userinfo(url)
            || cfg.bearer_token.as_deref().is_some_and(|t| !t.is_empty())
            || (cfg.webmail_user_for(target_email).is_some()
                && cfg.password_for(target_email).is_some());
        if credentialed && let Some(why) = cleartext_refusal(url) {
            return Some(why);
        }
    }
    if let Some(wurl) = cfg.effective_webmail_url(target_email) {
        let credentialed = url_has_embedded_userinfo(&wurl)
            || cfg.password_for(target_email).is_some()
            || cfg.mailbox_password_for(target_email).is_some()
            || cfg.totp_secret_for(target_email).is_some();
        if credentialed && let Some(why) = cleartext_refusal(&wurl) {
            return Some(why);
        }
    }
    None
}

struct LoopbackHttpUrl<'a> {
    authority: &'a str,
    host: String,
    userinfo: Option<&'a str>,
    port: u16,
    target: String,
}

fn parse_loopback_http_url(url: &str) -> Option<LoopbackHttpUrl<'_>> {
    let rest = strip_ascii_prefix_ci(url.trim(), "http://")?;
    let (raw_authority, tail) = authority_and_tail(rest);
    let (userinfo, authority) = match raw_authority.rsplit_once('@') {
        Some((ui, host_port)) => (Some(ui), host_port),
        None => (None, raw_authority),
    };
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (addr, after) = v6.split_once(']')?;
        if !addr.contains(':') {
            return None;
        }
        let port = match after {
            "" => 80,
            _ => after.strip_prefix(':')?.parse::<u16>().ok()?,
        };
        (addr.to_owned(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_owned(), p.parse::<u16>().ok()?),
            None => (authority.to_owned(), 80),
        }
    };
    if port == 0
        || !is_loopback_host(&host)
        || authority
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let target = tail.split('#').next().unwrap_or("");
    let target = if target.is_empty() || target.starts_with('?') {
        format!("/{target}")
    } else {
        target.to_owned()
    };
    if target.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    Some(LoopbackHttpUrl {
        authority,
        host,
        userinfo,
        port,
        target,
    })
}

fn is_loopback_http_url(url: &str) -> bool {
    parse_loopback_http_url(url).is_some()
}

fn fetch_loopback_http_body(
    url: &str,
    bearer_token: Option<&str>,
    basic_auth: Option<(&str, &str)>,
) -> Option<String> {
    if bearer_token.is_some_and(|t| t.contains(['\r', '\n'])) {
        return None;
    }
    let parsed = parse_loopback_http_url(url)?;
    let LoopbackHttpUrl {
        authority,
        host,
        userinfo: embedded_userinfo,
        port,
        target: path_and_query,
    } = parsed;
    let addr = if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        if !ip.is_loopback() {
            return None;
        }
        std::net::SocketAddr::new(ip, port)
    } else {
        (host.as_str(), port)
            .to_socket_addrs()
            .ok()?
            .find(|sa| sa.ip().is_loopback())?
    };
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let mut req = format!(
        "GET {path_and_query} HTTP/1.1\r\nHost: {authority}\r\nAccept: application/json, message/rfc822, text/plain\r\nConnection: close\r\n"
    );
    if let Some(token) = bearer_token.filter(|t| !t.is_empty()) {
        req.push_str(&format!("Authorization: Bearer {token}\r\n"));
    } else if let Some(ui) = embedded_userinfo.filter(|s| !s.is_empty()) {
        let decoded_ui = match ui.split_once(':') {
            Some((u, p)) => format!("{}:{}", percent_decode(u), percent_decode(p)),
            None => percent_decode(ui),
        };
        let b64 = super::open_url::encode_base64(decoded_ui.as_bytes());
        req.push_str(&format!("Authorization: Basic {b64}\r\n"));
    } else if let Some((u, p)) = basic_auth.filter(|(u, p)| !u.is_empty() && !p.is_empty()) {
        let b64 = super::open_url::encode_base64(format!("{u}:{p}").as_bytes());
        req.push_str(&format!("Authorization: Basic {b64}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).ok()?;
    stream.flush().ok()?;
    let mut buf = Vec::new();
    stream.take(EMAIL_FILE_MAX).read_to_end(&mut buf).ok()?;
    let raw = String::from_utf8_lossy(&buf);
    let mut cookies = BTreeMap::new();
    let resp = super::open_url::parse_raw_http_response(&raw, &mut cookies)?;
    (200..300).contains(&resp.status).then_some(resp.body)
}

fn url_has_embedded_userinfo(url: &str) -> bool {
    let trimmed = url.trim();
    let rest = strip_ascii_prefix_ci(trimmed, "https://")
        .or_else(|| strip_ascii_prefix_ci(trimmed, "http://"))
        .unwrap_or("");
    let (raw_authority, _) = authority_and_tail(rest);
    raw_authority
        .rsplit_once('@')
        .is_some_and(|(ui, _)| !ui.is_empty())
}

fn fetch_http_url_body(
    url: &str,
    cfg: &MailboxConfig,
    target_email: Option<&str>,
) -> Option<String> {
    let trimmed = url.trim();
    let cfg_pw = cfg.password_for(target_email);
    let cfg_user = cfg.webmail_user_for(target_email);
    let basic_auth = cfg_user.as_deref().zip(cfg_pw.as_deref());
    if is_loopback_http_url(trimmed) {
        fetch_loopback_http_body(trimmed, cfg.bearer_token.as_deref(), basic_auth)
    } else if strip_ascii_prefix_ci(trimmed, "https://").is_some()
        || strip_ascii_prefix_ci(trimmed, "http://").is_some()
    {
        let credential = if let Some(token) = cfg.bearer_token.as_deref().filter(|t| !t.is_empty())
        {
            Some(("header", format!("Authorization: Bearer {token}")))
        } else if !url_has_embedded_userinfo(trimmed)
            && let Some((u, p)) = basic_auth
        {
            Some(("user", format!("{u}:{p}")))
        } else {
            None
        };
        if (credential.is_some() || url_has_embedded_userinfo(trimmed))
            && cleartext_refusal(trimmed).is_some()
        {
            return None;
        }
        let mut options = vec![("url", trimmed)];
        if let Some((name, value)) = &credential {
            options.push((*name, value.as_str()));
        }
        let out = super::open_url::curl_output(
            &[
                "--disable",
                "--globoff",
                "--fail",
                "--proto",
                "=http,https",
                "-sS",
                "--max-time",
                "10",
            ],
            &options,
        )
        .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

fn candidate_http_detail_urls(list_url: &str, msg_id: &str) -> Vec<String> {
    let msg_id_trimmed = msg_id.trim();
    if msg_id_trimmed.is_empty() {
        return Vec::new();
    }
    let base = list_url
        .split('?')
        .next()
        .unwrap_or(list_url)
        .trim_end_matches('/');
    let encoded_id = percent_encode(msg_id_trimmed);
    let mut urls = Vec::new();
    if let Some(prefix) = base.strip_suffix("/api/v1/messages") {
        urls.push(format!("{prefix}/api/v1/message/{encoded_id}"));
    }
    if base.ends_with("/messages") || base.ends_with("/emails") || base.ends_with("/items") {
        let candidate = format!("{base}/{encoded_id}");
        if !urls.contains(&candidate) {
            urls.push(candidate);
        }
    }
    urls
}

fn json_item_missing_full_body_id(item: &Value) -> Option<&str> {
    let obj = item.as_object()?;
    let has_full_body = obj.keys().any(|k| {
        matches!(
            k.to_ascii_lowercase().as_str(),
            "html" | "body" | "content" | "raw" | "payload" | "htmlbody" | "html_body"
        )
    });
    if has_full_body {
        return None;
    }
    obj.iter().find_map(|(k, v)| {
        matches!(
            k.to_ascii_lowercase().as_str(),
            "id" | "message_id" | "messageid"
        )
        .then(|| v.as_str())
        .flatten()
        .filter(|s| !s.trim().is_empty())
    })
}

fn json_summary_allows_target(item: &Value, target_email: Option<&str>) -> bool {
    let Some(target) = target_email.map(str::trim).filter(|s| !s.is_empty()) else {
        return true;
    };
    let Some(obj) = item.as_object() else {
        return false;
    };
    let (saw_recipient, matched) = super::open_url::check_json_recipients(obj, target, 0);
    if saw_recipient { matched } else { true }
}

fn json_summary_allows_filter(raw_item: &str, filter: &MailFilter<'_>) -> bool {
    if filter.senders.is_empty() && filter.since.is_none() {
        return true;
    }
    let (from, date) = mail_from_and_date(raw_item);
    if !filter.senders.is_empty()
        && let Some(domain) = from.as_deref()
        && !filter.senders.iter().any(|w| domain_names(domain, w))
    {
        return false;
    }
    if let (Some(since), Some(at)) = (filter.since, date)
        && at < since.saturating_sub(MAIL_CLOCK_SLACK_S)
    {
        return false;
    }
    true
}

fn fetch_http_mailbox_credential(
    cfg: &MailboxConfig,
    target_email: Option<&str>,
    seen: &mut BTreeSet<String>,
    seen_otps: &mut BTreeSet<String>,
) -> Option<MailboxCredential> {
    let filter = MailFilter::of(cfg);
    let raw_url = cfg.http_url.as_deref()?;
    let url = if raw_url.contains("{email}") {
        raw_url.replace("{email}", &percent_encode(target_email.unwrap_or("")))
    } else {
        raw_url.to_owned()
    };
    let body = fetch_http_url_body(&url, cfg, target_email)?;

    if body.trim().is_empty() {
        return None;
    }

    if let Ok(doc) = aterm_json::from_str::<Value>(&body) {
        let items_opt = doc.as_array().or_else(|| {
            let obj = doc.as_object()?;
            ["messages", "items", "emails", "data", "value", "results"]
                .into_iter()
                .find_map(|k| obj.get(k).and_then(Value::as_array))
        });
        if let Some(items) = items_opt {
            for item in items.iter().rev() {
                let Ok(item_str) = aterm_json::to_string(item) else {
                    continue;
                };
                let digest = fnv1a_hex(&item_str);
                if seen.contains(&digest) {
                    continue;
                }
                if let Some((cred, exhausted)) = extract_unseen_credential_from_email(
                    &item_str,
                    target_email,
                    seen,
                    seen_otps,
                    &filter,
                ) {
                    record_consumed_credential(
                        cfg.seen_file.as_deref(),
                        seen,
                        seen_otps,
                        target_email,
                        &digest,
                        &cred,
                        exhausted,
                    );
                    return Some(cred);
                }
                if json_summary_allows_target(item, target_email)
                    && json_summary_allows_filter(&item_str, &filter)
                    && let Some(msg_id) = json_item_missing_full_body_id(item)
                {
                    let mut fetched_detail = false;
                    for detail_url in candidate_http_detail_urls(&url, msg_id) {
                        let Some(detail_body) = fetch_http_url_body(&detail_url, cfg, target_email)
                        else {
                            continue;
                        };
                        if detail_body.trim().is_empty() {
                            continue;
                        }
                        fetched_detail = true;
                        let detail_digest = fnv1a_hex(&detail_body);
                        if seen.contains(&detail_digest) {
                            break;
                        }
                        if let Some((cred, exhausted)) = extract_unseen_credential_from_email(
                            &detail_body,
                            target_email,
                            seen,
                            seen_otps,
                            &filter,
                        ) {
                            record_consumed_credential(
                                cfg.seen_file.as_deref(),
                                seen,
                                seen_otps,
                                target_email,
                                &detail_digest,
                                &cred,
                                exhausted,
                            );
                            if exhausted {
                                record_seen_digest(cfg.seen_file.as_deref(), seen, &digest);
                            }
                            return Some(cred);
                        }
                        break;
                    }
                    if fetched_detail {
                        seen.insert(digest);
                    }
                }
            }
            return None;
        }
    }

    let digest = fnv1a_hex(&body);
    if seen.contains(&digest) {
        return None;
    }
    let (cred, exhausted) =
        extract_unseen_credential_from_email(&body, target_email, seen, seen_otps, &filter)?;
    record_consumed_credential(
        cfg.seen_file.as_deref(),
        seen,
        seen_otps,
        target_email,
        &digest,
        &cred,
        exhausted,
    );
    Some(cred)
}

fn is_mbox_file(path: &Path, raw: &str) -> bool {
    raw.starts_with("From ")
        || path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|ext| ext.eq_ignore_ascii_case("mbox"))
}

fn split_mbox_messages(raw: &str) -> Vec<&str> {
    let mut msgs = Vec::new();
    let mut start = 0usize;
    for (idx, _) in raw.match_indices("\nFrom ") {
        let prev = raw[start..idx].trim();
        if !prev.is_empty() {
            msgs.push(prev);
        }
        start = idx + 1;
    }
    let tail = raw[start..].trim();
    if !tail.is_empty() {
        msgs.push(tail);
    }
    msgs
}

fn scan_maildir_once(
    mailbox_dir: &Path,
    target_email: Option<&str>,
    tried: &mut BTreeSet<(PathBuf, u64, std::time::SystemTime)>,
    seen_file: Option<&Path>,
    (seen_digests, seen_otps): (&mut BTreeSet<String>, &mut BTreeSet<String>),
    filter: &MailFilter<'_>,
) -> Option<MailboxCredential> {
    let mut candidates: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    if mailbox_dir.is_file()
        && let Ok(meta) = mailbox_dir.metadata()
        && meta.len() > 0
    {
        let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let path = mailbox_dir.to_path_buf();
        let is_mbox_ext = path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|ext| ext.eq_ignore_ascii_case("mbox"));
        let clicked_marker = path.with_file_name(format!(
            "{}.clicked",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
        if (is_mbox_ext || !clicked_marker.exists())
            && !tried.contains(&(path.clone(), meta.len(), mtime))
        {
            candidates.push((mtime, meta.len(), path));
        }
    }
    let dirs = [
        mailbox_dir.to_path_buf(),
        mailbox_dir.join("new"),
        mailbox_dir.join("cur"),
    ];
    for dir in &dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        candidates.extend(entries.filter_map(Result::ok).filter_map(|e| {
            let path = e.path();
            let name = e.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.')
                || name_str.ends_with(".clicked")
                || name_str.ends_with(".toml")
                || name_str.ends_with(".seen")
            {
                return None;
            }
            let is_mbox_ext = name_str.to_ascii_lowercase().ends_with(".mbox");
            let base_name = name_str.split(":2,").next().unwrap_or(&name_str);
            let clicked_marker = dir.join(format!("{name_str}.clicked"));
            let base_marker = mailbox_dir.join(format!("{base_name}.clicked"));
            if !is_mbox_ext && (clicked_marker.exists() || base_marker.exists()) {
                return None;
            }
            let meta = e.metadata().ok()?;
            if !meta.is_file() || meta.len() == 0 {
                return None;
            }
            let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            if tried.contains(&(path.clone(), meta.len(), mtime)) {
                return None;
            }
            Some((mtime, meta.len(), path))
        }));
    }
    if candidates.is_empty() && !mailbox_dir.is_dir() && !mailbox_dir.is_file() {
        return None;
    }
    candidates.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (mtime, len, path) in candidates {
        let mut raw = String::new();
        let Ok(file) = std::fs::File::open(&path) else {
            tried.insert((path.clone(), len, mtime));
            continue;
        };
        if file.take(EMAIL_FILE_MAX).read_to_string(&mut raw).is_err() {
            tried.insert((path.clone(), len, mtime));
            continue;
        }
        if is_mbox_file(&path, &raw) {
            for msg in split_mbox_messages(&raw).into_iter().rev() {
                let msg_digest = fnv1a_hex(msg);
                if seen_digests.contains(&msg_digest) {
                    continue;
                }
                if let Some((found, exhausted)) = extract_unseen_credential_from_email(
                    msg,
                    target_email,
                    seen_digests,
                    seen_otps,
                    filter,
                ) {
                    record_consumed_credential(
                        seen_file,
                        seen_digests,
                        seen_otps,
                        target_email,
                        &msg_digest,
                        &found,
                        exhausted,
                    );
                    return Some(found);
                }
            }
            tried.insert((path.clone(), len, mtime));
            continue;
        }
        let digest = fnv1a_hex(&raw);
        if seen_digests.contains(&digest) {
            tried.insert((path.clone(), len, mtime));
            continue;
        }
        if let Some((found, exhausted)) = extract_unseen_credential_from_email(
            &raw,
            target_email,
            seen_digests,
            seen_otps,
            filter,
        ) {
            record_consumed_credential(
                seen_file,
                seen_digests,
                seen_otps,
                target_email,
                &digest,
                &found,
                exhausted,
            );
            if exhausted {
                tried.insert((path.clone(), len, mtime));
                if let Some(name) = path.file_name() {
                    let name_str = name.to_string_lossy();
                    let parent = path.parent().unwrap_or(mailbox_dir);
                    let marker = parent.join(format!("{name_str}.clicked"));
                    let _ = std::fs::write(marker, b"1\n");
                    if !mailbox_dir.is_file() && parent != mailbox_dir {
                        let base_name = name_str.split(":2,").next().unwrap_or(&name_str);
                        let root_marker = mailbox_dir.join(format!("{base_name}.clicked"));
                        let _ = std::fs::write(root_marker, b"1\n");
                    }
                }
            }
            return Some(found);
        }
        tried.insert((path.clone(), len, mtime));
    }
    None
}

fn poll_non_webmail_once(
    cfg: &MailboxConfig,
    target_email: Option<&str>,
    tried_files: &mut BTreeSet<(PathBuf, u64, std::time::SystemTime)>,
    seen_digests: &mut BTreeSet<String>,
    seen_otps: &mut BTreeSet<String>,
) -> Option<MailboxCredential> {
    if let Some(dir) = cfg.dir.as_deref()
        && let Some(cred) = scan_maildir_once(
            dir,
            target_email,
            tried_files,
            cfg.seen_file.as_deref(),
            (seen_digests, seen_otps),
            &MailFilter::of(cfg),
        )
    {
        return Some(cred);
    }
    if let Some(cred) = fetch_imap_credential(cfg, target_email, seen_digests, seen_otps) {
        return Some(cred);
    }
    if let Some(cred) = fetch_http_mailbox_credential(cfg, target_email, seen_digests, seen_otps) {
        return Some(cred);
    }
    None
}

/// Poll `cfg` for either a magic link or a 6-digit OTP addressed to `target_email`.
pub fn poll_mailbox_config_for_credential(
    cfg: &MailboxConfig,
    target_email: Option<&str>,
    timeout: Duration,
) -> Result<MailboxCredential, &'static str> {
    let mut seen_otps = BTreeSet::new();
    poll_mailbox_config_for_credential_with_seen_otps(cfg, target_email, timeout, &mut seen_otps)
}

pub(super) fn poll_mailbox_config_for_credential_with_seen_otps(
    cfg: &MailboxConfig,
    target_email: Option<&str>,
    timeout: Duration,
    seen_otps: &mut BTreeSet<String>,
) -> Result<MailboxCredential, &'static str> {
    if let Some(why) = plaintext_credential_refusal(cfg, target_email) {
        return Err(why);
    }
    let has_non_webmail = cfg.dir.is_some()
        || cfg.effective_imap_url(target_email).is_some()
        || cfg.http_url.is_some();
    if !has_non_webmail {
        if cfg.effective_webmail_url(target_email).is_some() {
            return Err("webmail inbox requires headless browser");
        }
        return Err("headless browser requires sign-in, but no mailbox is configured");
    }
    let deadline = Instant::now() + timeout;
    let mut tried_files = BTreeSet::new();
    let mut seen_digests = load_seen_digests(cfg.seen_file.as_deref());
    let mut last_network_poll: Option<Instant> = None;
    let network_interval = Duration::from_millis(150);

    loop {
        if let Some(dir) = cfg.dir.as_deref()
            && let Some(cred) = scan_maildir_once(
                dir,
                target_email,
                &mut tried_files,
                cfg.seen_file.as_deref(),
                (&mut seen_digests, seen_otps),
                &MailFilter::of(cfg),
            )
        {
            return Ok(cred);
        }
        let should_poll_net =
            last_network_poll.is_none_or(|last| last.elapsed() >= network_interval);
        if should_poll_net {
            if cfg.effective_imap_url(target_email).is_some() || cfg.http_url.is_some() {
                last_network_poll = Some(Instant::now());
            }
            if let Some(cred) =
                fetch_imap_credential(cfg, target_email, &mut seen_digests, seen_otps)
            {
                return Ok(cred);
            }
            if let Some(cred) =
                fetch_http_mailbox_credential(cfg, target_email, &mut seen_digests, seen_otps)
            {
                return Ok(cred);
            }
        }
        if Instant::now() >= deadline {
            return Err("timed out waiting for verification email in mailbox");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Poll `mailbox_dir` for either a magic link or a 6-digit OTP addressed to `target_email`.
pub fn poll_mailbox_for_credential(
    mailbox_dir: &Path,
    target_email: Option<&str>,
    timeout: Duration,
) -> Result<MailboxCredential, &'static str> {
    poll_mailbox_config_for_credential(&MailboxConfig::from_dir(mailbox_dir), target_email, timeout)
}

/// Split NUL-delimited JSON messages from a CDP pipe buffer.
pub fn split_nul_messages(pending: &mut Vec<u8>) -> Vec<Value> {
    let mut out = Vec::new();
    while let Some(end) = pending.iter().position(|&b| b == 0) {
        let text: Vec<u8> = pending.drain(..=end).take(end).collect();
        out.push(
            aterm_json::from_slice(&text).unwrap_or_else(|e| json!({"unparsed": e.to_string()})),
        );
    }
    out
}

fn pump_cdp_reader(mut from: std::io::PipeReader, tx: &Sender<Value>) {
    let mut pending = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => pending.extend_from_slice(&buf[..n]),
        }
        for msg in split_nul_messages(&mut pending) {
            if tx.send(msg).is_err() {
                return;
            }
        }
    }
}

/// Chrome DevTools Protocol client over `--remote-debugging-pipe` (fds 3 and 4).
pub struct CdpClient {
    to: std::io::PipeWriter,
    from: Receiver<Value>,
    next_id: u64,
    events: Vec<Value>,
}

impl CdpClient {
    /// Create a CDP pipe client over `to` (browser fd 3) and `from` (browser fd 4).
    #[must_use]
    pub fn new(to: std::io::PipeWriter, from: std::io::PipeReader) -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || pump_cdp_reader(from, &tx));
        Self {
            to,
            from: rx,
            next_id: 0,
            events: Vec::new(),
        }
    }

    /// Send a CDP command and wait for its matching response within `timeout`.
    pub fn call(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, &'static str> {
        self.next_id += 1;
        let id = self.next_id;
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let (Some(s), Some(map)) = (session, msg.as_object_mut()) {
            map.insert("sessionId".to_owned(), Value::String(s.to_owned()));
        }
        let mut bytes = aterm_json::to_vec(&msg).map_err(|_| "failed encoding CDP command")?;
        bytes.push(0);
        self.to
            .write_all(&bytes)
            .and_then(|()| self.to.flush())
            .map_err(|_| "failed writing to browser DevTools pipe")?;
        let t0 = Instant::now();
        loop {
            let remaining = timeout.saturating_sub(t0.elapsed());
            if remaining.is_zero() {
                return Err("timed out waiting for browser DevTools response");
            }
            match self.from.recv_timeout(remaining) {
                Ok(m) => {
                    if m.get("id").and_then(Value::as_u64) != Some(id) {
                        self.events.push(m);
                        continue;
                    }
                    if m.get("error").is_some() {
                        return Err("browser DevTools protocol returned an error");
                    }
                    return Ok(m.get("result").cloned().unwrap_or(Value::Null));
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err("timed out waiting for browser DevTools response");
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("browser closed its DevTools pipe prematurely");
                }
            }
        }
    }

    /// Takes the events named `method` (of `session`, where one is named) that arrived so far,
    /// in order, leaving the others.
    pub fn take_events(&mut self, method: &str, session: Option<&str>) -> Vec<Value> {
        while let Ok(message) = self.from.try_recv() {
            self.events.push(message);
        }
        let mut taken = Vec::new();
        self.events.retain(|ev| {
            let hit = ev.get("method").and_then(Value::as_str) == Some(method)
                && session.is_none_or(|s| ev.get("sessionId").and_then(Value::as_str) == Some(s));
            if hit {
                taken.push(ev.clone());
            }
            !hit
        });
        taken
    }

    /// Drain buffered CDP events for `session`, returning the first URL (`params.url`,
    /// `params.frame.url`, or `params.frame.unreachableUrl`) matching `predicate`.
    pub fn take_matching_event_url(
        &mut self,
        session: &str,
        predicate: impl Fn(&str) -> bool,
    ) -> Option<String> {
        let mut found = None;
        self.events.retain(|ev| {
            let ev_session = ev.get("sessionId").and_then(Value::as_str);
            if ev_session.is_some_and(|s| s != session) {
                return true;
            }
            if found.is_none() {
                for ptr in [
                    "/params/url",
                    "/params/frame/url",
                    "/params/frame/unreachableUrl",
                ] {
                    if let Some(u) = ev.pointer(ptr).and_then(Value::as_str)
                        && predicate(u)
                    {
                        found = Some(u.to_owned());
                        break;
                    }
                }
            }
            false
        });
        found
    }
}

/// Shared field admission for OAuth OTPs and webmail TOTPs. Full-code fields keep the whole code;
/// split fields need one single-character control per digit, all owned by the same form.
const CODE_INPUT_HELPERS_JS: &str = r#"
  const usableCodeInputs = (inputs) => inputs.filter(input =>
    !input.disabled && !input.readOnly && !input.hidden &&
    !["hidden", "checkbox", "radio", "password", "submit", "button", "image", "reset"].includes(String(input.type || "").toLowerCase()));
  const codeInputsFor = (explicit, singleChar) => {
    const selected = usableCodeInputs(explicit);
    if (selected.length === 0) return usableCodeInputs(singleChar);
    if (selected.some(input => input.maxLength !== 1)) return selected;
    const form = selected[0].form;
    if (!form) return selected;
    const group = usableCodeInputs(singleChar).filter(input => input.form === form);
    return group.includes(selected[0]) ? group : selected;
  };
  const fillCodeInputs = (inputs, value) => {
    if (!value || inputs.length === 0) return null;
    const full = inputs.find(input => input.maxLength !== 1 &&
      (input.maxLength < 0 || input.maxLength >= value.length));
    if (full) {
      setNativeValue(full, value);
      return full;
    }
    if (inputs.length !== value.length || !inputs.every(input => input.maxLength === 1)) {
      return null;
    }
    const form = inputs[0].form;
    if (!form || !inputs.every(input => input.form === form)) return null;
    for (let i = 0; i < inputs.length; i++) {
      setNativeValue(inputs[i], value.charAt(i));
    }
    return inputs[0];
  };
"#;

/// An enabled control or native form submission must be attempted before latching a step.
/// Missing actions leave input-event-driven pages to advance on their next DOM observation.
const SUBMIT_INPUT_HELPERS_JS: &str = r#"
  const disallowedSubmitLabel = label =>
    /cancel|deny|decline|reject|revoke|logout|log out|sign out|signout|not you|sign up|create|with |without |switch |another |do not |don't |dont |agree|accept|cookie|terms|policy|allow all|同意|接受|取消|拒绝|注册|退出|登出|注销|切换|条款|协议/i.test(label);
  const submitInput = (input, buttons, labelPattern) => {
    const enabled = control => !control.disabled && !control.hidden &&
      !control.matches(":disabled") && control.getAttribute("aria-disabled") !== "true";
    if (!enabled(input) || input.readOnly || input.type === "hidden") return false;
    const form = input.form;
    const owner = button => "form" in button ? button.form : button.closest("form");
    const owned = buttons.filter(button => owner(button) === form);
    const btnLabel = button => String(button.innerText || button.value || button.getAttribute("aria-label") || "").trim();
    const isSubmit = button => (button.tagName === "BUTTON" && button.type === "submit") ||
      (button.tagName === "INPUT" && ["submit", "image"].includes(button.type));
    const button = (form && owned.find(button => isSubmit(button) && enabled(button) && !disallowedSubmitLabel(btnLabel(button)))) ||
      owned.find(button => button.type !== "reset" && enabled(button) &&
        !disallowedSubmitLabel(btnLabel(button)) &&
        labelPattern.test(btnLabel(button)));
    if (button) {
      button.click();
      return true;
    }
    if (form && !owned.some(isSubmit) && typeof form.requestSubmit === "function") {
      form.requestSubmit();
      return true;
    }
    return false;
  };
"#;

/// Build the JavaScript expression evaluated inside the OAuth page to inspect and advance the
/// flow.
#[must_use]
pub fn oauth_step_js(
    email: Option<&str>,
    otp: Option<&str>,
    email_submitted: bool,
    otp_submitted: bool,
) -> String {
    let email_json = aterm_json::to_string(&email.unwrap_or("")).unwrap_or_else(|_| "\"\"".into());
    let otp_json = aterm_json::to_string(&otp.unwrap_or("")).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"(() => {{
  const rawHref = String(window.location && window.location.href || "");
  const failedHref = rawHref.startsWith("chrome-error://")
    ? String(
        (window.loadTimeData && window.loadTimeData.data_ && window.loadTimeData.data_.summary && window.loadTimeData.data_.summary.failedUrl) ||
        (document.querySelector('#reload-button') && document.querySelector('#reload-button').getAttribute("data-url")) ||
        ""
      )
    : "";
  const href = (failedHref.startsWith("http://127.0.0.1:") || failedHref.startsWith("http://localhost:"))
    ? failedHref
    : rawHref;
  if (href.startsWith("http://127.0.0.1:") || href.startsWith("http://localhost:")) {{
    return {{ state: failedHref ? "callback_recovered" : "callback", url: href }};
  }}
  const setNativeValue = (el, val) => {{
    if (typeof el.focus === "function") el.focus();
    const proto = Object.getPrototypeOf(el);
    const desc = proto && Object.getOwnPropertyDescriptor(proto, "value");
    if (desc && desc.set) {{
      desc.set.call(el, val);
    }} else {{
      el.value = val;
    }}
    el.dispatchEvent(new Event("input", {{ bubbles: true }}));
    el.dispatchEvent(new Event("change", {{ bubbles: true }}));
  }};
{CODE_INPUT_HELPERS_JS}
{SUBMIT_INPUT_HELPERS_JS}
  const visibleControl = (el) => Boolean(el) && !el.disabled && !el.hidden && el.type !== "hidden" &&
    !el.matches(":disabled") && el.getAttribute("aria-disabled") !== "true";
  const buttons = Array.from(document.querySelectorAll("button, input[type='submit'], input[type='image'], [role='button'], a[href]"));
  const emailVal = {email_json};
  const targetEmail = String(emailVal || "").trim().toLowerCase();
  const bodyText = String(document.body && document.body.innerText || "").toLowerCase();
  const btnText = (btn) => String(btn.innerText || btn.value || btn.getAttribute("aria-label") || btn.getAttribute("title") || btn.getAttribute("alt") || "").trim().toLowerCase();
  const switchBtn = buttons.find(btn => visibleControl(btn) &&
    /use another account|different account|switch account|sign in with another|not you|sign out|log out/i.test(btnText(btn)));
  const emailsIn = (text) => String(text).toLowerCase().match(/[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{{2,}}/g) || [];
  const canonEmail = (em) => {{
    const s = String(em || "").trim().toLowerCase();
    const at = s.indexOf("@");
    if (at <= 0) return s;
    const local = s.slice(0, at).split("+")[0];
    return local + s.slice(at);
  }};
  const targetCanon = canonEmail(targetEmail);
  const hasTargetEmail = (text) => Boolean(targetCanon) && emailsIn(text).some(e => canonEmail(e) === targetCanon && (!e.includes("+") || !targetEmail.includes("+") || e === targetEmail));
  const pageEmails = emailsIn(bodyText)
    .filter(e => !e.endsWith("@anthropic.com"));
  const hasForeignAccount = Boolean(targetEmail) && !hasTargetEmail(bodyText) && (
    buttons.some(btn => visibleControl(btn) && btnText(btn).startsWith("continue as ") && emailsIn(btnText(btn)).length > 0 && !hasTargetEmail(btnText(btn))) ||
    (pageEmails.length > 0 && (Boolean(switchBtn) || /signed in as|logged in as|continuing as/i.test(bodyText)))
  );
  if (hasForeignAccount) {{
    if (switchBtn) {{
      switchBtn.click();
      return {{ state: "switched_account", url: href }};
    }}
    return {{ state: "wrong_account", url: href }};
  }}
  for (const btn of buttons) {{
    if (!visibleControl(btn)) continue;
    const text = btnText(btn);
    const hrefAttr = String(btn.getAttribute("href") || "");
    const hrefLower = hrefAttr.toLowerCase();
    const isCancel = text.includes("cancel") || text.includes("deny") || text.includes("decline") || text.includes("reject") || text.includes("revoke") || text.includes("do not ") || text.includes("don't ") || text.includes("dont ") || text.includes("not allow") || text.includes("not authorize") ||
      hrefLower.includes("deny") || hrefLower.includes("cancel") || hrefLower.includes("reject") || hrefLower.includes("decline") || hrefLower.includes("revoke") || hrefLower.includes("logout") || hrefLower.includes("signout") || hrefLower.includes("sign-out") || hrefLower.includes("access_denied") || hrefLower.includes("?error=") || hrefLower.includes("&error=");
    if (!isCancel && (
      text === "authorize" ||
      text.startsWith("authorize ") ||
      text === "allow" ||
      text.startsWith("allow ") ||
      text === "approve" ||
      text.startsWith("approve ") ||
      text === "accept" ||
      text === "grant access" ||
      hrefAttr.includes("/oauth/approve") ||
      hrefAttr.includes("localhost:") ||
      hrefAttr.includes("127.0.0.1:")
    )) {{
      btn.click();
      return {{ state: "clicked_consent", url: href, target: hrefAttr || text }};
    }}
  }}
  const explicitOtp = Array.from(document.querySelectorAll("input[autocomplete='one-time-code' i], input[name*='code' i], input[name*='otp' i], input[inputmode='numeric' i], input[data-testid*='otp' i]")).filter(visibleControl);
  const singleCharBoxes = Array.from(document.querySelectorAll("input[maxlength='1']")).filter(visibleControl);
  const otpInputs = codeInputsFor(explicitOtp, singleCharBoxes);
  const otpVal = {otp_json};
  const otpDone = {otp_submitted};
  if (otpInputs.length > 0) {{
    if (otpVal && !otpDone) {{
      const filledInput = fillCodeInputs(otpInputs, otpVal);
      if (!filledInput) return {{ state: "loading", url: href }};
      if (!submitInput(filledInput, buttons, /verify|continue|submit|sign in|log in|下一步|继续|验证|确认|提交|登录/i)) {{
        return {{ state: "loading", url: href }};
      }}
      return {{ state: "submitted_otp", url: href }};
    }}
    return {{ state: "needs_mailbox", url: href }};
  }}
  const emailInput = Array.from(document.querySelectorAll("input[type='email'], input[name*='email' i], input[autocomplete*='email' i], input[autocomplete='username' i]")).filter(el => !el.readOnly && !["hidden", "checkbox", "radio", "password", "submit", "button", "image", "reset"].includes(String(el.type || "").toLowerCase())).find(visibleControl) || null;
  const emailDone = {email_submitted};
  if (emailInput && emailVal && !emailDone) {{
    setNativeValue(emailInput, emailVal);
    if (!submitInput(emailInput, buttons, /continue|sign in|log in|next|send|下一步|继续|登录|发送|验证|确认|提交/i)) {{
      return {{ state: "loading", url: href }};
    }}
    return {{ state: "submitted_email", url: href }};
  }}
  const approveForm = !emailInput ? Array.from(document.querySelectorAll("form[action*='approve'], form[action*='authorize']")).find(f => {{
    const act = String(f.getAttribute("action") || "").toLowerCase();
    return !act.includes("deny") && !act.includes("cancel") && !act.includes("reject") && !act.includes("decline") && !act.includes("revoke") && !act.includes("logout") && !act.includes("signout") && !act.includes("sign-out") && !act.includes("access_denied") && !act.includes("?error=") && !act.includes("&error=");
  }}) : null;
  if (approveForm) {{
    if (typeof approveForm.requestSubmit === "function") {{
      approveForm.requestSubmit();
    }} else {{
      approveForm.submit();
    }}
    return {{ state: "clicked_consent", url: href }};
  }}
  if (bodyText.includes("check your email") || bodyText.includes("magic link") || bodyText.includes("sent a link") || bodyText.includes("verification code") || bodyText.includes("link has expired") || bodyText.includes("link expired") || bodyText.includes("invalid link") || bodyText.includes("already been used")) {{
    return {{ state: "needs_mailbox", url: href }};
  }}
  if (targetEmail) {{
    for (const btn of buttons) {{
      if (!visibleControl(btn)) continue;
      const text = btnText(btn);
      const isCancel = text.includes("cancel") || text.includes("deny") || text.includes("decline") || text.includes("reject") || text.includes("do not ") || text.includes("don't ") || text.includes("dont ") || text.includes("not allow") || text.includes("not authorize");
      if (!isCancel && text.startsWith("continue as ") && hasTargetEmail(text)) {{
        btn.click();
        return {{ state: "clicked_consent", url: href, target: text }};
      }}
    }}
  }}
  for (const btn of buttons) {{
    if (!visibleControl(btn)) continue;
    const text = btnText(btn);
    const isCancel = text.includes("cancel") || text.includes("deny") || text.includes("decline") || text.includes("reject") || text.includes("do not ") || text.includes("don't ") || text.includes("dont ") || text.includes("not allow") || text.includes("not authorize");
    if (targetEmail && text.startsWith("continue as ") && emailsIn(text).length > 0 && !hasTargetEmail(text)) {{
      continue;
    }}
    if (!isCancel && (
      text.startsWith("continue as ") ||
      (href.includes("/oauth/") && (text === "continue" || text === "confirm" || text === "select"))
    )) {{
      btn.click();
      return {{ state: "clicked_consent", url: href, target: text }};
    }}
  }}
  return {{ state: "loading", url: href }};
}})()"#
    )
}

struct OtpAuthParams {
    secret: String,
    period: u64,
    digits: u32,
    account_email: Option<String>,
}

fn parse_otpauth_uri(input: &str) -> Option<OtpAuthParams> {
    let trimmed = input.trim();
    let lower = trimmed.to_ascii_lowercase();
    let (label_part, query_part) = if let Some(rest) = trimmed
        .get(10..)
        .filter(|_| lower.starts_with("otpauth://"))
    {
        let after_host = rest
            .find(['/', '?'])
            .map(|idx| &rest[idx..])
            .unwrap_or_default();
        if let Some((lbl, q)) = after_host.split_once('?') {
            (lbl.trim_start_matches('/'), q)
        } else {
            return None;
        }
    } else if lower.starts_with("secret=")
        || lower.contains("&secret=")
        || lower.contains("?secret=")
    {
        let q = trimmed.split_once('?').map_or(trimmed, |(_, q)| q);
        ("", q)
    } else {
        return None;
    };

    let mut account_email = None;
    if !label_part.is_empty() {
        let decoded_label = percent_decode(label_part);
        let candidate = decoded_label
            .split_once(':')
            .map_or(decoded_label.as_str(), |(_, acct)| acct)
            .trim();
        if candidate.contains('@') && !candidate.chars().any(char::is_whitespace) {
            account_email = Some(candidate.to_ascii_lowercase());
        }
    }

    let mut secret = String::new();
    let mut period = 30u64;
    let mut digits = 6u32;
    for pair in query_part.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        match k.trim().to_ascii_lowercase().as_str() {
            "secret" => {
                secret = percent_decode(v.trim());
            }
            "period" => {
                if let Ok(p) = v.trim().parse::<u64>()
                    && p > 0
                {
                    period = p;
                }
            }
            "digits" => {
                if let Ok(d) = v.trim().parse::<u32>()
                    && (6..=8).contains(&d)
                {
                    digits = d;
                }
            }
            _ => {}
        }
    }
    (!secret.trim().is_empty()).then_some(OtpAuthParams {
        secret,
        period,
        digits,
        account_email,
    })
}

/// Decode an RFC 4648 Base32 secret or `otpauth://` URI (case-insensitive, ignoring spaces,
/// hyphens, and `=` padding).
#[must_use]
pub fn base32_decode(secret: &str) -> Option<Vec<u8>> {
    let extracted = parse_otpauth_uri(secret).map(|p| p.secret);
    let raw = extracted.as_deref().unwrap_or(secret);
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0u8;
    for ch in raw.chars() {
        if ch.is_ascii_whitespace() || ch == '-' || ch == '=' {
            continue;
        }
        let val = match ch.to_ascii_uppercase() {
            'A'..='Z' => (ch.to_ascii_uppercase() as u8 - b'A') as u32,
            '2'..='7' => (ch as u8 - b'2' + 26) as u32,
            _ => return None,
        };
        buf = (buf << 5) | val;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    (!out.is_empty()).then_some(out)
}

fn sha1_digest(data: &[u8]) -> [u8; 20] {
    let mut h0: u32 = 0x6745_2301;
    let mut h1: u32 = 0xEFCD_AB89;
    let mut h2: u32 = 0x98BA_DCFE;
    let mut h3: u32 = 0x1032_5476;
    let mut h4: u32 = 0xC3D2_E1F0;

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = Vec::with_capacity((data.len() + 9).div_ceil(64) * 64);
    msg.extend_from_slice(data);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (i, &word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(word);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let mut a = h0;
        let mut b = h1;
        let mut c = h2;
        let mut d = h3;
        let mut e = h4;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut out = [0u8; 20];
    out[0..4].copy_from_slice(&h0.to_be_bytes());
    out[4..8].copy_from_slice(&h1.to_be_bytes());
    out[8..12].copy_from_slice(&h2.to_be_bytes());
    out[12..16].copy_from_slice(&h3.to_be_bytes());
    out[16..20].copy_from_slice(&h4.to_be_bytes());
    out
}

fn hmac_sha1(key: &[u8], message: &[u8]) -> [u8; 20] {
    let mut k_block = [0u8; 64];
    if key.len() > 64 {
        let hashed = sha1_digest(key);
        k_block[..20].copy_from_slice(&hashed);
    } else {
        k_block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k_block[i];
        opad[i] ^= k_block[i];
    }
    let mut inner_input = Vec::with_capacity(64 + message.len());
    inner_input.extend_from_slice(&ipad);
    inner_input.extend_from_slice(message);
    let inner_hash = sha1_digest(&inner_input);

    let mut outer_input = [0u8; 84];
    outer_input[..64].copy_from_slice(&opad);
    outer_input[64..].copy_from_slice(&inner_hash);
    sha1_digest(&outer_input)
}

/// Generate an RFC 6238 TOTP code for `secret_base32` (raw Base32 or `otpauth://` URI) at Unix
/// timestamp `unix_secs`.
#[must_use]
pub fn generate_totp_at(secret_base32: &str, unix_secs: u64) -> Option<String> {
    let (raw_secret, period, digits) = if let Some(params) = parse_otpauth_uri(secret_base32) {
        (params.secret, params.period, params.digits)
    } else {
        (secret_base32.to_owned(), 30, 6)
    };
    let key = base32_decode(&raw_secret)?;
    let counter = (unix_secs / period.max(1)).to_be_bytes();
    let mac = hmac_sha1(&key, &counter);
    let offset = (mac[19] & 0x0f) as usize;
    let clamped_digits = digits.clamp(6, 8);
    let modulus = 10u32.pow(clamped_digits);
    let code = u32::from_be_bytes([
        mac[offset] & 0x7f,
        mac[offset + 1],
        mac[offset + 2],
        mac[offset + 3],
    ]) % modulus;
    let width = clamped_digits as usize;
    Some(format!("{code:0width$}"))
}

pub(super) fn webmail_step_js(
    (email, password, mailbox_password, totp_code): (
        Option<&str>,
        Option<&str>,
        Option<&str>,
        Option<&str>,
    ),
    (user_submitted, login_submitted, totp_submitted, mailbox_pw_submitted): (
        bool,
        bool,
        bool,
        bool,
    ),
    inbox_tick: usize,
    senders: &[String],
) -> String {
    let sender_words: Vec<String> = if senders.is_empty() {
        vec!["claude".to_owned(), "anthropic".to_owned()]
    } else {
        senders.iter().map(|w| w.to_ascii_lowercase()).collect()
    };
    let senders_json = aterm_json::to_string(&sender_words).unwrap_or_else(|_| "[]".to_owned());
    let email_json = aterm_json::to_string(&Value::from(email.unwrap_or(""))).unwrap_or_default();
    let pw_json = aterm_json::to_string(&Value::from(password.unwrap_or(""))).unwrap_or_default();
    let mpw_json =
        aterm_json::to_string(&Value::from(mailbox_password.unwrap_or(""))).unwrap_or_default();
    let totp_json =
        aterm_json::to_string(&Value::from(totp_code.unwrap_or(""))).unwrap_or_default();
    format!(
        r#"(() => {{
  const href = String(window.location && window.location.href || "");
  const setNativeValue = (el, val) => {{
    el.focus();
    const proto = Object.getPrototypeOf(el);
    const desc = proto && Object.getOwnPropertyDescriptor(proto, "value");
    if (desc && desc.set) {{
      desc.set.call(el, val);
    }} else {{
      el.value = val;
    }}
    el.dispatchEvent(new Event("input", {{ bubbles: true }}));
    el.dispatchEvent(new Event("change", {{ bubbles: true }}));
  }};
{CODE_INPUT_HELPERS_JS}
{SUBMIT_INPUT_HELPERS_JS}
  const buttons = Array.from(document.querySelectorAll("button, input[type='submit'], input[type='image']"));
  const clickSubmit = (input) => submitInput(input, buttons,
    /sign in|log in|authenticate|unlock|verify|continue|next|submit|下一步|继续|登录|验证|确认|提交/i);
  const visibleControl = (el) => Boolean(el) && !el.disabled && !el.hidden && el.type !== "hidden" &&
    !el.matches(":disabled") && el.getAttribute("aria-disabled") !== "true";
  const editableControl = (el) => visibleControl(el) && !el.readOnly &&
    !["hidden", "checkbox", "radio", "submit", "button", "image", "reset"].includes(String(el.type || "").toLowerCase());

  const userEl = Array.from(document.querySelectorAll("input#username, input[name='username'], input[name='email'], input[name='loginfmt'], input#login_id, input[name='LOGIN_ID'], input#_user, input[name='_user'], input[autocomplete='username'], input[autocomplete*='email'], input[type='email']")).filter(el => String(el.type || "").toLowerCase() !== "password").find(editableControl) || null;
  const passEl = Array.from(document.querySelectorAll("input#password, input[name='password'], input[name='passwd'], input#_pass, input[name='_pass'], input[type='password']:not(#mailboxPassword):not([name='mailboxPassword'])")).find(editableControl) || null;
  const explicitTotp = Array.from(document.querySelectorAll("input#twoFa, input[name='twoFa'], input[name='totp'], input[name='otc'], input[autocomplete='one-time-code'], input[id*='totp' i], input[name*='totp' i], input[name*='otp' i], input[data-testid*='totp' i], input[inputmode='numeric']"));
  const splitTotp = Array.from(document.querySelectorAll("input[maxlength='1']"));
  const totpInputs = codeInputsFor(explicitTotp, splitTotp);
  const mpwEl = Array.from(document.querySelectorAll("input#mailboxPassword, input[name='mailboxPassword']")).find(editableControl) || null;
  const emailVal = {email_json};
  const pwVal = {pw_json};
  if (userEl && !passEl && !totpInputs.length && !mpwEl && emailVal) {{
    if (!pwVal) {{
      return {{ state: "needs_password", url: href }};
    }}
    if (!{user_submitted}) {{
      setNativeValue(userEl, emailVal);
      if (!clickSubmit(userEl)) return {{ state: "loading", url: href }};
      return {{ state: "submitted_user", url: href }};
    }}
    return {{ state: "loading", url: href }};
  }}
  if (passEl) {{
    if (!pwVal) {{
      return {{ state: "needs_password", url: href }};
    }}
    if (!{login_submitted}) {{
      if (userEl && emailVal) {{
        setNativeValue(userEl, emailVal);
      }}
      setNativeValue(passEl, pwVal);
      if (!clickSubmit(passEl)) return {{ state: "loading", url: href }};
      return {{ state: "submitted_login", url: href }};
    }}
    return {{ state: "loading", url: href }};
  }}

  const totpVal = {totp_json};
  if (totpInputs.length > 0) {{
    if (!totpVal) {{
      return {{ state: "needs_totp", url: href }};
    }}
    if (!{totp_submitted}) {{
      const filledInput = fillCodeInputs(totpInputs, totpVal);
      if (!filledInput) return {{ state: "loading", url: href }};
      if (!clickSubmit(filledInput)) return {{ state: "loading", url: href }};
      return {{ state: "submitted_totp", url: href }};
    }}
    return {{ state: "loading", url: href }};
  }}

  const mpwVal = {mpw_json} || pwVal;
  if (mpwEl) {{
    if (!mpwVal) {{
      return {{ state: "needs_password", url: href }};
    }}
    if (!{mailbox_pw_submitted}) {{
      setNativeValue(mpwEl, mpwVal);
      if (!clickSubmit(mpwEl)) return {{ state: "loading", url: href }};
      return {{ state: "submitted_mailbox_password", url: href }};
    }}
    return {{ state: "loading", url: href }};
  }}

  let htmlParts = [];
  let textParts = [];
  const iframes = Array.from(document.querySelectorAll("iframe")).reverse();
  for (const frame of iframes) {{
    try {{
      const doc = frame.contentDocument;
      if (doc && doc.body) {{
        htmlParts.push(String(doc.body.innerHTML || ""));
        textParts.push(String(doc.body.innerText || ""));
      }}
    }} catch (_) {{}}
    try {{
      const srcdoc = frame.srcdoc || frame.getAttribute("srcdoc");
      if (srcdoc) {{
        htmlParts.push(String(srcdoc));
      }}
    }} catch (_) {{}}
  }}
  const msgNodes = Array.from(document.querySelectorAll(
    "article, [data-testid*='message-content'], [data-testid*='message-body'], .message-content, .message-body"
  )).reverse();
  for (const node of msgNodes) {{
    htmlParts.push(String(node.innerHTML || ""));
    textParts.push(String(node.innerText || ""));
  }}
  const shadowHosts = Array.from(document.querySelectorAll("*"))
    .filter(el => Boolean(el.shadowRoot))
    .reverse();
  for (const host of shadowHosts) {{
    htmlParts.push(String(host.shadowRoot.innerHTML || ""));
    textParts.push(String(host.shadowRoot.textContent || ""));
  }}
  if (document.body) {{
    htmlParts.push(String(document.body.innerHTML || ""));
    textParts.push(String(document.body.innerText || ""));
  }}

  const rows = Array.from(document.querySelectorAll("[data-testid*='message-item'], [data-shortcut-target='item-container'], .item-container, [role='listitem'], [role='row'], tr"));
  const senderWords = {senders_json};
  const vendorAuthRows = [];
  const senderRows = [];
  const authRows = [];
  for (const row of rows) {{
    const rt = String(row.innerText || "").toLowerCase();
    const hasSender = senderWords.some(word => rt.includes(word));
    const hasAuth = rt.includes("sign in") || rt.includes("sign-in") || rt.includes("verification") || rt.includes("verify") || rt.includes("login") || rt.includes("log in") || rt.includes("magic") || rt.includes("code") || rt.includes("otp") || rt.includes("passcode") || rt.includes("验证码") || rt.includes("验证") || rt.includes("登录") || rt.includes("动态码") || rt.includes("安全码") || rt.includes("認證") || rt.includes("驗證碼") || rt.includes("認証コード") || rt.includes("確認コード") || rt.includes("인증");
    if (hasSender && hasAuth) {{
      vendorAuthRows.push(row);
    }} else if (hasSender) {{
      senderRows.push(row);
    }} else if (hasAuth) {{
      authRows.push(row);
    }}
  }}
  const candidates = vendorAuthRows.concat(senderRows, authRows);
  let clickedRow = false;
  if (candidates.length > 0) {{
    const limit = Math.min(candidates.length, 5);
    const idx = Math.floor({inbox_tick} / 2) % limit;
    const chosen = candidates[idx] || candidates[0];
    chosen.click();
    clickedRow = true;
  }}

  return {{
    state: "inbox",
    url: href,
    clicked_row: clickedRow,
    html: htmlParts.join("\n"),
    text: textParts.join("\n")
  }};
}})()"#
    )
}

/// Poll a webmail inbox (such as `https://mail.proton.me/u/0/inbox`) in a secondary CDP tab
/// inside the same headless browser instance, logging in with `cfg`'s credentials (and optional
/// RFC 6238 TOTP 2FA) and extracting the newest unseen magic link or 6-digit OTP from the DOM.
/// `seen_otps` is the target-qualified, in-memory history of one authorization only.
pub fn poll_webmail_via_cdp(
    cdp: &mut CdpClient,
    cfg: &MailboxConfig,
    target_email: Option<&str>,
    timeout: Duration,
    poll_interval: Duration,
    seen_digests: &mut BTreeSet<String>,
    seen_otps: &mut BTreeSet<String>,
) -> Result<MailboxCredential, &'static str> {
    if let Some(why) = plaintext_credential_refusal(cfg, target_email) {
        return Err(why);
    }
    let raw_webmail_url = cfg
        .effective_webmail_url(target_email)
        .ok_or("headless browser requires sign-in, but no webmail URL is configured")?;
    let webmail_url = raw_webmail_url.trim();
    if let Some(why) = cleartext_refusal(webmail_url) {
        return Err(why);
    }
    if !strip_ascii_prefix_ci(webmail_url, "https://")
        .is_some_and(|rest| !authority_and_tail(rest).0.is_empty())
        && !is_loopback_http_url(webmail_url)
    {
        return Err("webmail URL must use https:// or loopback http://");
    }
    let login_user = cfg.webmail_user_for(target_email);
    let password = cfg.password_for(target_email);
    let mailbox_password = cfg.mailbox_password_for(target_email);
    let totp_secret = cfg.totp_secret_for(target_email);

    let deadline = Instant::now() + timeout;
    let step_timeout = || deadline.saturating_duration_since(Instant::now());

    let target = cdp.call(
        None,
        "Target.createTarget",
        json!({"url": "about:blank"}),
        step_timeout(),
    )?;
    let target_id = target
        .get("targetId")
        .and_then(Value::as_str)
        .ok_or("browser created no webmail targetId")?
        .to_owned();
    let attached = cdp.call(
        None,
        "Target.attachToTarget",
        json!({"targetId": target_id, "flatten": true}),
        step_timeout(),
    )?;
    let session = attached
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or("browser attached no webmail sessionId")?
        .to_owned();

    let result = (|| {
        for method in ["Page.enable", "Runtime.enable"] {
            let _ = cdp.call(Some(&session), method, json!({}), step_timeout())?;
        }
        let _ = cdp.call(
            Some(&session),
            "Page.navigate",
            json!({"url": webmail_url}),
            step_timeout(),
        )?;

        let mut user_submitted = false;
        let mut login_submitted = false;
        let mut last_submitted_totp: Option<String> = None;
        let mut mailbox_pw_submitted = false;
        let mut inbox_tick = 0usize;
        let mut tried_files = BTreeSet::new();

        for _ in 0..80 {
            if let Some(cred) =
                poll_non_webmail_once(cfg, target_email, &mut tried_files, seen_digests, seen_otps)
            {
                return Ok(cred);
            }
            if Instant::now() >= deadline {
                return Err("timed out waiting for verification email in webmail inbox");
            }
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let totp_code = totp_secret
                .as_deref()
                .and_then(|s| generate_totp_at(s, now_secs));
            let expr = webmail_step_js(
                (
                    login_user.as_deref(),
                    password.as_deref(),
                    mailbox_password.as_deref(),
                    totp_code.as_deref(),
                ),
                (
                    user_submitted,
                    login_submitted,
                    totp_code.is_some() && totp_code == last_submitted_totp,
                    mailbox_pw_submitted,
                ),
                inbox_tick,
                &cfg.senders,
            );
            let eval = cdp.call(
                Some(&session),
                "Runtime.evaluate",
                json!({
                    "expression": expr,
                    "awaitPromise": true,
                    "returnByValue": true
                }),
                step_timeout(),
            )?;
            let _ = cdp.take_matching_event_url(&session, |_| false);
            let val = eval
                .pointer("/result/value")
                .cloned()
                .unwrap_or(Value::Null);
            let state = val
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("loading");
            match state {
                "needs_password" => {
                    return Err(
                        "webmail sign-in requires a password in mailbox.toml or ai-mailbox.toml",
                    );
                }
                "needs_totp" => {
                    return Err(
                        "webmail sign-in requires a TOTP 2FA secret in mailbox.toml or ai-mailbox.toml",
                    );
                }
                "submitted_user" => {
                    user_submitted = true;
                }
                "submitted_login" => {
                    login_submitted = true;
                }
                "submitted_totp" => {
                    last_submitted_totp = totp_code;
                }
                "submitted_mailbox_password" => {
                    mailbox_pw_submitted = true;
                }
                "inbox" => {
                    inbox_tick = inbox_tick.saturating_add(1);
                    let html = val.get("html").and_then(Value::as_str).unwrap_or("");
                    let text = val.get("text").and_then(Value::as_str).unwrap_or("");
                    if !html.is_empty() || !text.is_empty() {
                        let synthetic = format!(
                            "Delivered-To: {}\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{}\n{}",
                            target_email.unwrap_or(""),
                            html,
                            text
                        );
                        let digest = fnv1a_hex(&synthetic);
                        if !seen_digests.contains(&digest)
                            && let Some((cred, exhausted)) = extract_unseen_credential_from_email(
                                &synthetic,
                                target_email,
                                seen_digests,
                                seen_otps,
                                &MailFilter::default(),
                            )
                        {
                            record_consumed_credential(
                                cfg.seen_file.as_deref(),
                                seen_digests,
                                seen_otps,
                                target_email,
                                &digest,
                                &cred,
                                exhausted,
                            );
                            return Ok(cred);
                        }
                    }
                }
                _ => {}
            }
            if !poll_interval.is_zero() {
                std::thread::sleep(poll_interval);
            }
        }
        Err("timed out waiting for verification email in webmail inbox")
    })();

    let _ = cdp.call(
        None,
        "Target.closeTarget",
        json!({"targetId": target_id}),
        Duration::from_secs(2),
    );
    result
}

/// Poll any configured mailbox source ([`MailboxConfig`]: Maildir, IMAP/IMAPS, HTTP API, or
/// headless CDP Webmail) for either a magic link or a 6-digit OTP addressed to `target_email`.
/// Retain `seen_otps` across retries of one authorization, then discard it: short codes can
/// recur in another authorization or account, unlike a consumed message or unique magic link.
pub fn poll_mailbox_or_webmail_for_credential(
    cdp: &mut CdpClient,
    cfg: &MailboxConfig,
    target_email: Option<&str>,
    timeout: Duration,
    poll_interval: Duration,
    seen_otps: &mut BTreeSet<String>,
) -> Result<MailboxCredential, &'static str> {
    if let Some(why) = plaintext_credential_refusal(cfg, target_email) {
        return Err(why);
    }
    let has_non_webmail = cfg.dir.is_some()
        || cfg.effective_imap_url(target_email).is_some()
        || cfg.http_url.is_some();
    let result = if cfg.effective_webmail_url(target_email).is_some() {
        let t0 = Instant::now();
        let mut seen_digests = load_seen_digests(cfg.seen_file.as_deref());
        let mut tried_files = BTreeSet::new();
        let from_non_webmail = if has_non_webmail {
            poll_non_webmail_once(
                cfg,
                target_email,
                &mut tried_files,
                &mut seen_digests,
                seen_otps,
            )
        } else {
            None
        };
        if let Some(cred) = from_non_webmail {
            Ok(cred)
        } else {
            let remaining = timeout.saturating_sub(t0.elapsed());
            match poll_webmail_via_cdp(
                cdp,
                cfg,
                target_email,
                remaining,
                poll_interval,
                &mut seen_digests,
                seen_otps,
            ) {
                Ok(cred) => Ok(cred),
                Err(err) => {
                    if !has_non_webmail || t0.elapsed() >= timeout {
                        Err(err)
                    } else {
                        poll_mailbox_config_for_credential_with_seen_otps(
                            cfg,
                            target_email,
                            timeout.saturating_sub(t0.elapsed()),
                            seen_otps,
                        )
                    }
                }
            }
        }
    } else if has_non_webmail {
        poll_mailbox_config_for_credential_with_seen_otps(cfg, target_email, timeout, seen_otps)
    } else {
        Err("headless browser requires sign-in, but no mailbox is configured")
    };
    if let Ok(credential) = &result {
        remember_flow_otp(credential, target_email, seen_otps);
    }
    result
}

/// Reuse the HTTP probe's consumed credential once, then resume the configured source.
#[cfg(unix)]
pub(super) fn poll_mailbox_or_pending_credential(
    pending: &mut Option<MailboxCredential>,
    cdp: &mut CdpClient,
    cfg: &MailboxConfig,
    target_email: Option<&str>,
    timeout: Duration,
    poll_interval: Duration,
    seen_otps: &mut BTreeSet<String>,
) -> Result<MailboxCredential, &'static str> {
    if let Some(credential) = pending.take() {
        remember_flow_otp(&credential, target_email, seen_otps);
        return Ok(credential);
    }
    poll_mailbox_or_webmail_for_credential(
        cdp,
        cfg,
        target_email,
        timeout,
        poll_interval,
        seen_otps,
    )
}

fn callback_delivery_result(delivery: Result<u16, &'static str>) -> Result<(), &'static str> {
    if (200..400).contains(&delivery?) {
        Ok(())
    } else {
        Err("the local OAuth callback listener rejected the authorization code")
    }
}

/// Drive an active CDP session (`cdp`) through the OAuth flow for `authorize_url`.
pub fn drive_cdp_oauth(
    cdp: &mut CdpClient,
    authorize_url: &str,
    target_email: Option<&str>,
    timeout: Duration,
    poll_interval: Duration,
    mut mailbox_credential: impl FnMut(Option<&str>) -> Result<MailboxCredential, &'static str>,
    deliver_callback: impl FnMut(&str) -> Result<u16, &'static str>,
) -> Result<(), &'static str> {
    drive_cdp_oauth_with_webmail(
        cdp,
        authorize_url,
        target_email,
        timeout,
        poll_interval,
        |_cdp, em| mailbox_credential(em),
        deliver_callback,
    )
}

/// Drive an active CDP session (`cdp`) through the OAuth flow for `authorize_url`, passing
/// `&mut CdpClient` to `mailbox_credential` so webmail inboxes can be polled in a secondary CDP
/// tab.
pub fn drive_cdp_oauth_with_webmail(
    cdp: &mut CdpClient,
    authorize_url: &str,
    target_email: Option<&str>,
    timeout: Duration,
    poll_interval: Duration,
    mut mailbox_credential: impl FnMut(
        &mut CdpClient,
        Option<&str>,
    ) -> Result<MailboxCredential, &'static str>,
    mut deliver_callback: impl FnMut(&str) -> Result<u16, &'static str>,
) -> Result<(), &'static str> {
    let flow = MailboxFlow::new(authorize_url)?;
    let email = target_email
        .filter(|e| !e.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| extract_login_hint(authorize_url));
    let deadline = Instant::now() + timeout;
    let step_timeout = || deadline.saturating_duration_since(Instant::now());

    let target = cdp.call(
        None,
        "Target.createTarget",
        json!({"url": "about:blank"}),
        step_timeout(),
    )?;
    let target_id = target
        .get("targetId")
        .and_then(Value::as_str)
        .ok_or("browser created no targetId")?
        .to_owned();
    let attached = cdp.call(
        None,
        "Target.attachToTarget",
        json!({"targetId": target_id, "flatten": true}),
        step_timeout(),
    )?;
    let session = attached
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or("browser attached no sessionId")?
        .to_owned();

    for method in ["Page.enable", "Runtime.enable"] {
        let _ = cdp.call(Some(&session), method, json!({}), step_timeout())?;
    }

    let _ = cdp.call(
        Some(&session),
        "Page.navigate",
        json!({"url": authorize_url}),
        step_timeout(),
    )?;

    let mut email_submitted = false;
    let mut otp_code: Option<String> = None;
    let mut otp_submitted = false;
    let mut mailbox_polls = 0usize;
    let mut needs_mailbox_ticks = 0usize;
    const MAX_MAILBOX_POLLS: usize = 3;

    let result = (|| {
        for _ in 0..120 {
            if Instant::now() >= deadline {
                return Err("timed out waiting for headless browser OAuth completion");
            }
            let expr = oauth_step_js(
                email.as_deref(),
                otp_code.as_deref(),
                email_submitted,
                otp_submitted,
            );
            let eval = cdp.call(
                Some(&session),
                "Runtime.evaluate",
                json!({
                    "expression": expr,
                    "awaitPromise": true,
                    "returnByValue": true
                }),
                step_timeout(),
            )?;
            let val = eval
                .pointer("/result/value")
                .cloned()
                .unwrap_or(Value::Null);
            let state = val
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("loading");
            let url = val.get("url").and_then(Value::as_str).unwrap_or("");
            let target_attr = val.get("target").and_then(Value::as_str).unwrap_or("");

            if state == "callback" || (state != "callback_recovered" && flow.is_callback(url)) {
                let cb_url = if flow.is_callback(url) {
                    url
                } else {
                    return Err("headless browser reached an invalid local callback URL");
                };
                // The page may already have delivered the callback before this duplicate GET.
                let _ = deliver_callback(cb_url);
                return Ok(());
            }
            if state == "callback_recovered" {
                if !flow.is_callback(url) {
                    return Err("headless browser reached an invalid local callback URL");
                }
                return callback_delivery_result(deliver_callback(url));
            }
            if let Some(cb_url) = cdp.take_matching_event_url(&session, |u| flow.is_callback(u)) {
                return callback_delivery_result(deliver_callback(&cb_url));
            }
            if flow.is_callback(target_attr) {
                return callback_delivery_result(deliver_callback(target_attr));
            }

            match state {
                "clicked_consent" | "switched_account" => {
                    if state == "switched_account" {
                        email_submitted = false;
                    }
                    needs_mailbox_ticks = 0;
                    if !poll_interval.is_zero() {
                        std::thread::sleep(poll_interval);
                    }
                }
                "wrong_account" => {
                    return Err("headless browser profile is signed in to a different account");
                }
                "submitted_email" => {
                    email_submitted = true;
                    needs_mailbox_ticks = 0;
                    if mailbox_polls == 0 {
                        mailbox_polls += 1;
                        match mailbox_credential(cdp, email.as_deref())? {
                            MailboxCredential::MagicLink(magic_url) => {
                                if !flow.admits_provider_url(&magic_url) {
                                    return Err("this OAuth destination needs the manual browser");
                                }
                                let _ = cdp.call(
                                    Some(&session),
                                    "Page.navigate",
                                    json!({"url": magic_url}),
                                    step_timeout(),
                                )?;
                            }
                            MailboxCredential::Otp(code) => {
                                otp_code = Some(code);
                                otp_submitted = false;
                            }
                        }
                    }
                }
                "needs_mailbox" => {
                    let should_poll = mailbox_polls == 0
                        || (needs_mailbox_ticks >= 2 && mailbox_polls < MAX_MAILBOX_POLLS);
                    if should_poll {
                        mailbox_polls += 1;
                        needs_mailbox_ticks = 0;
                        match mailbox_credential(cdp, email.as_deref())? {
                            MailboxCredential::MagicLink(magic_url) => {
                                if !flow.admits_provider_url(&magic_url) {
                                    return Err("this OAuth destination needs the manual browser");
                                }
                                let _ = cdp.call(
                                    Some(&session),
                                    "Page.navigate",
                                    json!({"url": magic_url}),
                                    step_timeout(),
                                )?;
                            }
                            MailboxCredential::Otp(code) => {
                                otp_code = Some(code);
                                otp_submitted = false;
                            }
                        }
                    } else {
                        needs_mailbox_ticks += 1;
                        if !poll_interval.is_zero() {
                            std::thread::sleep(poll_interval);
                        }
                    }
                }
                "submitted_otp" => {
                    otp_submitted = true;
                    needs_mailbox_ticks = 0;
                    if !poll_interval.is_zero() {
                        std::thread::sleep(poll_interval);
                    }
                }
                _ => {
                    if !poll_interval.is_zero() {
                        std::thread::sleep(poll_interval);
                    }
                }
            }
        }
        Err("headless browser OAuth exceeded maximum steps without reaching callback")
    })();

    let _ = cdp.call(
        None,
        "Target.closeTarget",
        json!({"targetId": target_id}),
        Duration::from_secs(2),
    );
    let _ = cdp.call(None, "Browser.close", json!({}), Duration::from_secs(2));
    result
}

/// Complete OAuth authorization headlessly over CDP pipes using `mailbox_cfg` when needed.
pub fn authorize_with_headless_browser_config(
    authorize_url: &str,
    store_dir: Option<&Path>,
    mailbox_cfg: Option<&MailboxConfig>,
    target_email: Option<&str>,
    timeout: Duration,
) -> Result<(), &'static str> {
    authorize_with_headless_browser_config_and_credential(
        authorize_url,
        store_dir,
        mailbox_cfg,
        target_email,
        timeout,
        None,
    )
}

/// Continue an HTTP probe with its already-consumed credential before polling again. A window's
/// pass that never ran on the profile is made up first ([`super::browser_window::make_up_tidy`]).
#[cfg(unix)]
pub(super) fn authorize_with_headless_browser_config_and_credential(
    authorize_url: &str,
    store_dir: Option<&Path>,
    mailbox_cfg: Option<&MailboxConfig>,
    target_email: Option<&str>,
    timeout: Duration,
    mut pending_credential: Option<MailboxCredential>,
) -> Result<(), &'static str> {
    use aterm_uds::spawnfd::{ChildStdio, LaneCommand, spawn_with_two_fds};
    use std::os::fd::OwnedFd;

    let _flow = MailboxFlow::new(authorize_url)?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let browser = find_headless_browser(home.as_deref()).ok_or(
        "no headless browser found (install Google Chrome, name a Chromium in \
         $ATERM_BROWSER, or in an aterm checkout run `targo --unverified verify provision \
         chrome-headless-shell`)",
    )?;

    let email = target_email
        .filter(|e| !e.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| extract_login_hint(authorize_url));

    let label = infer_account_label_from_env();
    with_browser_profile(
        store_dir,
        label.as_deref(),
        email.as_deref(),
        |profile_dir| {
            let (cmd_read, cmd_write) =
                std::io::pipe().map_err(|_| "could not open CDP command pipe")?;
            let (out_read, out_write) =
                std::io::pipe().map_err(|_| "could not open CDP event pipe")?;

            let mut cmd = LaneCommand::new(&browser.path);
            cmd.args(headless_launch_args(&browser, profile_dir))
                .stdin(ChildStdio::Null)
                .stdout(ChildStdio::Null)
                .stderr(ChildStdio::Null);

            let child = spawn_with_two_fds(cmd, OwnedFd::from(cmd_read), OwnedFd::from(out_write))
                .map_err(|_| "could not spawn headless browser")?;
            let _guard = BrowserChild::new(child);
            let mut cdp = CdpClient::new(cmd_write, out_read);
            let _ = super::browser_window::make_up_tidy(
                &mut cdp,
                profile_dir,
                Instant::now() + super::browser_window::MAKE_UP_WITHIN,
            );
            let mut seen_otps = BTreeSet::new();

            let t0 = Instant::now();
            drive_cdp_oauth_with_webmail(
                &mut cdp,
                authorize_url,
                email.as_deref(),
                timeout,
                Duration::from_millis(200),
                |cdp_inner, em| {
                    let mb = mailbox_cfg
                        .filter(|c| c.is_configured_for(em))
                        .ok_or("headless browser requires sign-in, but no mailbox is configured")?;
                    poll_mailbox_or_pending_credential(
                        &mut pending_credential,
                        cdp_inner,
                        mb,
                        em,
                        timeout.saturating_sub(t0.elapsed()),
                        Duration::from_millis(200),
                        &mut seen_otps,
                    )
                },
                |cb_url| {
                    let mut cookies = BTreeMap::new();
                    super::open_url::loopback_http_exchange("GET", cb_url, &mut cookies, None)
                        .map(|r| r.status)
                },
            )
        },
    )
}

/// Non-Unix continuation stub for [`authorize_with_headless_browser_config`].
#[cfg(not(unix))]
pub(super) fn authorize_with_headless_browser_config_and_credential(
    _authorize_url: &str,
    _store_dir: Option<&Path>,
    _mailbox_cfg: Option<&MailboxConfig>,
    _target_email: Option<&str>,
    _timeout: Duration,
    _pending_credential: Option<MailboxCredential>,
) -> Result<(), &'static str> {
    Err("headless browser OAuth is supported on Unix hosts only")
}

/// Complete OAuth authorization headlessly over CDP pipes.
pub fn authorize_with_headless_browser(
    authorize_url: &str,
    store_dir: Option<&Path>,
    mailbox_dir: Option<&Path>,
    target_email: Option<&str>,
    timeout: Duration,
) -> Result<(), &'static str> {
    let cfg = mailbox_dir.map(MailboxConfig::from_dir);
    authorize_with_headless_browser_config(
        authorize_url,
        store_dir,
        cfg.as_ref(),
        target_email,
        timeout,
    )
}

/// One page of a vendor sign-in as the headless browser reads it ([`VENDOR_PAGE_JS`]): what
/// [`vendor_step`] decides on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Page {
    /// Its URL.
    pub url: String,
    /// Its first heading, lowercase.
    pub heading: String,
    /// Its visible text, bounded, as the page writes it.
    pub text: String,
    /// Its enabled, visible buttons' labels, lowercase, in document order.
    pub buttons: Vec<String>,
    /// Its enabled, visible inputs.
    pub inputs: Vec<Field>,
}

/// One input of a [`Page`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// An email (or username) input.
    Email,
    /// A code input: one-time, numeric, or one character per box.
    Code,
    /// A password input.
    Password,
    /// Anything else.
    Other,
}

impl Field {
    fn of(word: &str) -> Field {
        match word {
            "email" => Field::Email,
            "code" => Field::Code,
            "password" => Field::Password,
            _ => Field::Other,
        }
    }
}

/// What a vendor sign-in has done on its pages so far, and what bounds it ([`vendor_step`]).
#[derive(Debug, Clone, Copy)]
pub struct Gate<'a> {
    /// The hosts its pages may be on.
    pub hosts: &'a [&'a str],
    /// How the page hands the login back.
    pub completion: crate::ai::connector::Completion,
    /// The code the CLI printed, for a device flow.
    pub code: Option<&'a str>,
    /// The row's email.
    pub email: Option<&'a str>,
    /// Words of the vendor's own addresses (`connector::Accounts::mail_senders`), never taken
    /// for another account's.
    pub senders: &'a [&'a str],
    /// The one-time code the mailbox gave, once it gave one.
    pub otp: Option<&'a str>,
    /// Consent presses made.
    pub pressed: u32,
    /// The device page's one confirm press was made.
    pub confirmed: bool,
    /// The device code was typed into the page.
    pub filled_code: bool,
    /// The row's email was submitted.
    pub filled_email: bool,
    /// The mailbox's one-time code was submitted.
    pub filled_otp: bool,
    /// The flow the sign-in started ([`FlowBound::of`] its first page), which every page and
    /// mailed link must stay within.
    pub bound: Option<&'a FlowBound>,
    /// The command a person runs to sign the row's browser profile in once (`accounts browser`),
    /// which a stop on a sign-in page names.
    pub sign_in_once: Option<&'a str>,
    /// Whether the row's email may be typed into the vendor's page and its mailbox read for the
    /// mailed code ([`crate::ai::connector::SignIn::types_email`]).
    pub types_email: bool,
}

/// The OAuth flow a vendor sign-in started, as its first page's URL names it: the client, where
/// the login is handed back, and the state; and the loopback callback's port, where that is on
/// this machine. A page or a mailed link naming another client or another hand-back is another
/// flow, which nothing is pressed on or followed to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlowBound {
    /// `client_id`.
    pub client_id: Option<String>,
    /// `redirect_uri`, decoded.
    pub redirect_uri: Option<String>,
    /// `state`.
    pub state: Option<String>,
    /// The port of a loopback `redirect_uri` (`http://127.0.0.1:<port>/…`, `localhost`).
    pub callback_port: Option<u16>,
}

pub(super) fn query_values(url: &str, key: &str) -> Vec<String> {
    let Some((_, tail)) = url.split_once('?') else {
        return Vec::new();
    };
    let query = tail.split('#').next().unwrap_or("");
    query
        .split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (super::open_url::percent_decode(k) == key).then(|| super::open_url::percent_decode(v))
        })
        .collect()
}

fn unique_query_value(url: &str, key: &str) -> Result<Option<String>, ()> {
    let mut vals = query_values(url, key).into_iter();
    let Some(first) = vals.next() else {
        return Ok(None);
    };
    vals.all(|v| v == first).then_some(Some(first)).ok_or(())
}

/// The decoded value of the query parameter `key` of `url`, absent where values conflict.
#[must_use]
pub fn query_value(url: &str, key: &str) -> Option<String> {
    unique_query_value(url, key).ok().flatten()
}

/// The port of `url` where it is a loopback `http://` URL (`127.0.0.1`, `localhost`).
fn loopback_port(url: &str) -> Option<u16> {
    if url
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return None;
    }
    let rest = url
        .strip_prefix("http://127.0.0.1:")
        .or_else(|| url.strip_prefix("http://localhost:"))?;
    let port_str = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    port_str.parse::<u16>().ok().filter(|p| *p > 0)
}

impl FlowBound {
    /// The flow `start` (the sign-in's first page) names.
    ///
    /// # Errors
    /// Returns a reason without the URL or its values if duplicate `client_id`,
    /// `redirect_uri` or `state` parameters have conflicting decoded values.
    pub fn of(start: &str) -> Result<FlowBound, &'static str> {
        let client_id = unique_query_value(start, "client_id")
            .map_err(|()| "the sign-in URL names conflicting clients")?;
        let redirect_uri = unique_query_value(start, "redirect_uri")
            .map_err(|()| "the sign-in URL names conflicting callback destinations")?;
        let state = unique_query_value(start, "state")
            .map_err(|()| "the sign-in URL names conflicting OAuth states")?;
        Ok(FlowBound {
            client_id,
            callback_port: redirect_uri.as_deref().and_then(loopback_port),
            redirect_uri,
            state,
        })
    }

    /// Why `url` (a page, or a mailed link) is outside this flow on `hosts`: it names another
    /// client, or hands its login to another place than this flow's, or to a host that is
    /// neither this machine nor a pinned one, or carries another OAuth state on a flow URL.
    #[must_use]
    pub fn outside(&self, url: &str, hosts: &[&str]) -> Option<String> {
        let clients = query_values(url, "client_id");
        if clients.len() > 1 && clients.windows(2).any(|w| w[0] != w[1]) {
            return Some("the page is another client's sign-in".to_owned());
        }
        if let (Some(started), Some(named)) = (&self.client_id, clients.first())
            && started != named
        {
            return Some("the page is another client's sign-in".to_owned());
        }
        let backs = query_values(url, "redirect_uri");
        for back in &backs {
            if let Some(started) = &self.redirect_uri
                && started != back
            {
                return Some(
                    "the page hands its login to another place than the sign-in's".to_owned(),
                );
            }
            if loopback_port(back).is_none() && !super::login_drive::pinned(back, hosts) {
                return Some(
                    "the page hands its login off this machine and off the vendor's hosts"
                        .to_owned(),
                );
            }
        }
        if (!clients.is_empty() || !backs.is_empty())
            && let Some(started) = &self.state
        {
            let states = query_values(url, "state");
            if states.iter().any(|named| named != started) {
                return Some("the page is another sign-in flow's (its state differs)".to_owned());
            }
        }
        None
    }
}

/// What [`vendor_step`] decides for one page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Press the page's button at this index ([`Page::buttons`]).
    Press(usize),
    /// Type the CLI's device code into the page's code input, and submit it.
    FillCode,
    /// Type the row's email into the page's email input, and submit it.
    FillEmail,
    /// Type the mailbox's one-time code into the page's code input, and submit it.
    FillOtp,
    /// Read the mailbox for the vendor's sign-in mail.
    Mailbox,
    /// Read the page again.
    Wait,
    /// The page approved the sign-in.
    Done,
    /// Only the person can go on from here: why, without the URL or code.
    Stop(String),
    /// The page wants the person's own sign-in to the vendor's site (another identity provider,
    /// a password): why, without the URL or code, naming [`Gate::sign_in_once`] where it is set.
    SignIn(String),
    /// The page is refused: why.
    Refuse(String),
}

/// The most consent presses one vendor sign-in makes.
pub const MAX_CONSENT_PRESSES: u32 = 4;

/// Identity providers other than the vendor's own, on which nothing is ever pressed.
pub const OTHER_IDPS: &[&str] = &[
    "accounts.google.com",
    "appleid.apple.com",
    "x.com",
    "twitter.com",
    "api.x.com",
    "login.microsoftonline.com",
    "login.live.com",
    "github.com",
    "facebook.com",
    "www.facebook.com",
];

fn has_field(page: &Page, field: Field) -> bool {
    page.inputs.contains(&field)
}

/// Whether `token` has the printed `code`'s shape: its length, its dashes where the code has
/// them, letters and digits elsewhere, and a digit.
fn code_shaped(token: &str, code: &str) -> bool {
    token.len() == code.len()
        && token.chars().zip(code.chars()).all(|(t, c)| {
            if c == '-' {
                t == '-'
            } else {
                t.is_ascii_alphanumeric()
            }
        })
        && token.chars().any(|c| c.is_ascii_digit())
}

/// The tokens of `text` shaped like `code`, and whether `text` shows `code` itself (as a token,
/// or with its spacing removed, as a page that boxes each character shows it). A word with an
/// `@` in it is an address, never a code, so its parts are not read as one (a row's email such
/// as `andy-2024@…` that Kimi's card names).
fn codes_shown(text: &str, code: &str) -> (bool, bool) {
    let want = code.to_ascii_uppercase();
    let mut shaped = false;
    let mut same = false;
    let tokens = text
        .split_whitespace()
        .filter(|word| !word.contains('@'))
        .flat_map(|word| word.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')));
    for token in tokens {
        if code_shaped(token, code) {
            shaped = true;
            same |= token.eq_ignore_ascii_case(&want);
        }
    }
    let compact: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    same |= compact.contains(&want);
    (shaped || same, same)
}

fn sign_up_page(page: &Page) -> bool {
    let path = page
        .url
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once('/'))
        .map(|(_, path)| {
            path.split(['?', '#'])
                .next()
                .unwrap_or("")
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    [
        "signup",
        "sign-up",
        "sign_up",
        "register",
        "create-account",
        "create_account",
        "createaccount",
    ]
    .iter()
    .any(|w| path.contains(w))
        || [
            "create an account",
            "create account",
            "create your account",
            "sign up",
            "注册账号",
            "创建账号",
        ]
        .iter()
        .any(|w| page.heading.contains(w))
}

fn emails_in(text: &str) -> Vec<String> {
    text.split(|c: char| {
        !matches!(
            c,
            'a'..='z' | 'A'..='Z' | '0'..='9' | '@' | '.' | '_' | '%' | '+' | '-'
        )
    })
    .map(|w| {
        w.trim_matches(|c: char| !c.is_ascii_alphanumeric())
            .to_ascii_lowercase()
    })
    .filter(|w| {
        w.split_once('@').is_some_and(|(l, d)| {
            !l.is_empty()
                && !d.contains('@')
                && d.split('.').count() >= 2
                && d.split('.').all(|part| !part.is_empty())
        })
    })
    .collect()
}

/// An email as a vendor's account names it: trimmed and lowercased, its `+tag` kept, since a
/// plus-addressed row is another account of the vendor's ([`super::accounts::email_matches`]).
fn exact_email(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

/// The words with which a page names the account signed in to it, in its text or on a button:
/// Kimi's device card offers `Switch account` (kimi.ai) or `切换账号` (kimi.com) beside the
/// account it names.
const SIGNED_IN_WORDS: &[&str] = &[
    "signed in as",
    "logged in as",
    "continue as",
    "not you",
    "switch account",
    "切换账号",
];

fn says_signed_in(page: &Page) -> bool {
    let lower = page.text.to_ascii_lowercase();
    SIGNED_IN_WORDS.iter().any(|w| lower.contains(w))
        || page
            .buttons
            .iter()
            .any(|b| b.starts_with("continue as ") || SIGNED_IN_WORDS.iter().any(|w| b.contains(w)))
}

/// Whether the page names an account other than the row's as the one signed in: any email it
/// shows (in its text or on a button) that is not exactly the row's, the vendor's own addresses
/// apart ([`domain_names`]).
fn another_account(page: &Page, gate: &Gate<'_>) -> bool {
    let Some(want) = gate.email.map(exact_email) else {
        return false;
    };
    if !says_signed_in(page) {
        return false;
    }
    let vendor = |e: &str| {
        e.split_once('@')
            .is_some_and(|(_, d)| gate.senders.iter().any(|s| domain_names(d, s)))
    };
    emails_in(&page.text)
        .into_iter()
        .chain(page.buttons.iter().flat_map(|b| emails_in(b)))
        .any(|e| exact_email(&e) != want && !vendor(&e))
}

/// Whether the page names the row's exact email as the account signed in to it (its signed-in
/// words beside the email, [`SIGNED_IN_WORDS`]), and no other: the page's proof of the row's
/// account. A page that only echoes the email aterm typed (`we sent a code to …`) proves
/// nothing.
fn shows_email(page: &Page, gate: &Gate<'_>) -> bool {
    let Some(want) = gate.email.map(exact_email) else {
        return false;
    };
    says_signed_in(page)
        && !another_account(page, gate)
        && emails_in(&page.text)
            .into_iter()
            .chain(page.buttons.iter().flat_map(|b| emails_in(b)))
            .any(|e| exact_email(&e) == want)
}

/// Whether a button may ever be pressed: never one that cancels, declines, signs up, signs in
/// with another provider, or agrees to or accepts anything, or names cookies, terms or a policy,
/// or allows all (a cookie banner's): a vendor's terms and a site's cookies are the person's to
/// accept.
fn pressable(label: &str) -> bool {
    ![
        "cancel",
        "deny",
        "decline",
        "reject",
        "revoke",
        "logout",
        "log out",
        "sign out",
        "signout",
        "not you",
        "sign up",
        "create",
        "with ",
        "without ",
        "switch ",
        "another ",
        "agree",
        "accept",
        "cookie",
        "terms",
        "policy",
        "allow all",
        "同意",
        "接受",
        "取消",
        "拒绝",
        "注册",
        "退出",
        "登出",
        "注销",
        "切换",
        "条款",
        "协议",
    ]
    .iter()
    .any(|w| label.contains(w))
}

fn button_of(page: &Page, labels: &[&str]) -> Option<usize> {
    page.buttons.iter().position(|b| {
        pressable(b)
            && labels
                .iter()
                .any(|l| b == l || b.strip_prefix(l).is_some_and(|rest| rest.starts_with(' ')))
    })
}

const CONFIRM_LABELS: &[&str] = &[
    "confirm",
    "authorize",
    "approve",
    "allow",
    "continue",
    "yes",
];

/// The confirm labels of a device card that names the account signed in to it
/// ([`says_signed_in`]): Kimi's `Log in` (kimi.ai) and `登录` (kimi.com). On any other page they
/// are a sign-in form's submit, never a confirm.
const SIGNED_IN_CONFIRM_LABELS: &[&str] = &["log in", "登录"];

/// The device page's confirm button. A page whose text shows the code takes one of
/// [`CONFIRM_LABELS`], or, on a card that names the account signed in, one of
/// [`SIGNED_IN_CONFIRM_LABELS`]. A card that shows its code only in its URL (Kimi's) takes only
/// one of [`SIGNED_IN_CONFIRM_LABELS`], its own `Log in`: its other buttons (a banner's
/// `Continue` or `Allow all`) are not the card's.
fn confirm_button(page: &Page, in_text: bool) -> Option<usize> {
    if !in_text {
        return button_of(page, SIGNED_IN_CONFIRM_LABELS);
    }
    button_of(page, CONFIRM_LABELS).or_else(|| {
        says_signed_in(page)
            .then(|| button_of(page, SIGNED_IN_CONFIRM_LABELS))
            .flatten()
    })
}

/// Whether a device page names another code than the CLI printed (`other`) and whether it names
/// the printed `code` (`same`): its text shows the code (as [`codes_shown`] reads it), or its URL's
/// `user_code` is the code and the page names the account signed in (Kimi's card shows its code
/// only in its URL). A `user_code` that differs, or a code-shaped token in a text that does not
/// show the printed code, is another code.
fn device_code(page: &Page, code: &str) -> (bool, bool) {
    let (shaped, shown) = codes_shown(&page.text, code);
    let in_url = query_values(&page.url, "user_code");
    let url_other = in_url.iter().any(|named| !named.eq_ignore_ascii_case(code));
    let url_same = !in_url.is_empty() && !url_other && says_signed_in(page);
    (url_other || (shaped && !shown), shown || url_same)
}

/// The tail a stop on a sign-in page carries: the command that signs the row's browser profile
/// in once, where the gate names one.
fn sign_in_once_tail(gate: &Gate<'_>) -> String {
    gate.sign_in_once.map_or_else(String::new, |command| {
        format!(
            "; if the row's browser profile is signed out there, sign it in once with `{command}`"
        )
    })
}

/// Whether the page asks to agree to something: a button labelled `agree`, `同意` or `接受`, or
/// `accept` other than a cookie banner's, or a heading that names terms or a policy (a terms
/// update whose one button is `Continue`). The vendor's terms are the person's to accept, so
/// nothing on such a page is pressed; a cookie banner's buttons are never pressed either
/// ([`pressable`]), but the page beneath it may be.
fn asks_to_agree(page: &Page) -> bool {
    page.buttons.iter().any(|b| {
        ["agree", "同意", "接受"].iter().any(|w| b.contains(w))
            || b.contains("accept") && !b.contains("cookie")
    }) || ["terms", "policy", "条款", "协议", "隐私"]
        .iter()
        .any(|w| page.heading.contains(w))
}

/// Whether the page is a bot-protection block page (Cloudflare's `Sorry, you have been
/// blocked`), which only a person past it can go on from.
fn block_page(page: &Page) -> bool {
    let lower = page.text.to_ascii_lowercase();
    lower.contains("sorry, you have been blocked") && lower.contains("cloudflare")
}

const CONSENT_LABELS: &[&str] = &[
    "authorize",
    "allow",
    "approve",
    "continue",
    "confirm",
    "grant access",
];

const MAIL_WORDS: &[&str] = &[
    "check your email",
    "check your inbox",
    "we sent",
    "we've sent",
    "we have sent",
    "verification code",
    "magic link",
    "sent a link",
    "验证码",
    "已发送",
    "查收邮件",
    "验证邮件",
];

const SUCCESS_WORDS: &[&str] = &[
    "you can close this",
    "you may close this",
    "device authorized",
    "device has been authorized",
    "successfully authorized",
    "authorization successful",
    "authorization complete",
    "you're all set",
    "you are all set",
    "login successful",
    "signed in successfully",
    "return to your terminal",
    "go back to your terminal",
    "授权成功",
    "登录成功",
    "设备已授权",
    "可以关闭此页面",
    "可关闭此窗口",
];

/// The step one vendor sign-in page calls for (design §6): a pure judgment of the page's host,
/// text, buttons and inputs. Nothing is pressed off the pinned hosts, on another identity
/// provider, on a sign-up page or on a bot-protection block page; a device page is confirmed once,
/// and only when it shows the code the CLI printed (in its text, or in its URL on a card naming
/// the account signed in); a page showing another code, or another account signed in, is
/// refused; another provider's page or a password asks for the person's sign-in
/// ([`Step::SignIn`]); where the row's email may not be typed ([`Gate::types_email`]), an email
/// field or the mailed code's steps wait for the person's sign-in as well.
#[must_use]
pub fn vendor_step(page: &Page, gate: &Gate<'_>) -> Step {
    let step = page_step(page, gate);
    if !gate.types_email && matches!(step, Step::FillEmail | Step::Mailbox | Step::FillOtp) {
        return Step::SignIn(format!(
            "the page asks for the account's email or its mailed code, which this vendor's \
             sign-in takes only from a person (its form signs a new account up as readily as \
             it signs one in){}",
            sign_in_once_tail(gate)
        ));
    }
    step
}

/// [`vendor_step`] before its [`Gate::types_email`] rule.
fn page_step(page: &Page, gate: &Gate<'_>) -> Step {
    use crate::ai::connector::Completion;
    let url = page.url.as_str();
    if url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:") {
        let Some(port) = loopback_port(url) else {
            return Step::Stop("the sign-in page left https".to_owned());
        };
        if gate.completion == Completion::Device {
            return Step::Refuse("a device sign-in's page went to a local callback".to_owned());
        }
        let wanted = gate.bound.and_then(|b| b.callback_port);
        if wanted.is_some_and(|w| w != port) {
            return Step::Refuse(
                "the page went to a local port other than the sign-in's callback".to_owned(),
            );
        }
        if let Some(started) = gate.bound.and_then(|b| b.state.as_deref()) {
            let states = query_values(url, "state");
            if states.iter().any(|named| named != started) {
                return Step::Refuse(
                    "the callback is another sign-in flow's (its state differs)".to_owned(),
                );
            }
        }
        if !query_values(url, "error").is_empty() {
            return Step::Refuse("the sign-in callback reported an OAuth error".to_owned());
        }
        return Step::Done;
    }
    if url.is_empty()
        || url == "about:blank"
        || url.starts_with("chrome-error://")
        || url.starts_with("data:")
    {
        return Step::Wait;
    }
    let Some(host) = super::login_drive::https_host(url) else {
        return Step::Stop("the sign-in page left https".to_owned());
    };
    if !gate.hosts.iter().any(|h| h.eq_ignore_ascii_case(&host)) {
        let idp = OTHER_IDPS.contains(&host.as_str()) || super::browser_window::idp_cookie(&host);
        return if idp {
            Step::SignIn(format!(
                "the sign-in went to {host}, another identity provider aterm never signs in to{}",
                sign_in_once_tail(gate)
            ))
        } else {
            Step::Stop(format!("the sign-in left its pinned hosts for {host}"))
        };
    }
    if block_page(page) {
        return Step::Stop(
            "the vendor's site answered this browser with a bot-protection block page, so only \
             a person can go on"
                .to_owned(),
        );
    }
    if sign_up_page(page) {
        return Step::Stop("the page asks to create an account, which aterm never does".to_owned());
    }
    if let Some(why) = gate.bound.and_then(|b| b.outside(url, gate.hosts)) {
        return Step::Refuse(why);
    }
    if another_account(page, gate) {
        return Step::Refuse("the browser profile is signed in to another account".to_owned());
    }
    let lower = page.text.to_ascii_lowercase();
    if has_field(page, Field::Email) && !gate.filled_email {
        return match gate.email {
            Some(_) => Step::FillEmail,
            None => Step::Stop("the page asks for an email and the row names none".to_owned()),
        };
    }
    let mail_page =
        gate.filled_email && !gate.filled_otp || MAIL_WORDS.iter().any(|w| lower.contains(w));
    if has_field(page, Field::Code) && mail_page && !gate.filled_otp {
        return if gate.otp.is_some() {
            Step::FillOtp
        } else {
            Step::Mailbox
        };
    }
    if has_field(page, Field::Password) {
        return Step::SignIn(format!(
            "the page asks for a password, which aterm does not hold{}",
            sign_in_once_tail(gate)
        ));
    }
    if asks_to_agree(page) {
        return Step::Wait;
    }
    if gate.completion == Completion::Device
        && let Some(code) = gate.code
    {
        let (other, same) = device_code(page, code);
        if other {
            return Step::Refuse("the page shows another code than the CLI printed".to_owned());
        }
        if same {
            if gate.confirmed {
                return Step::Wait;
            }
            let in_text = codes_shown(&page.text, code).1;
            return confirm_button(page, in_text).map_or(Step::Wait, Step::Press);
        }
        if has_field(page, Field::Code) && !gate.filled_code {
            return Step::FillCode;
        }
    }
    if SUCCESS_WORDS.iter().any(|w| lower.contains(w)) {
        return Step::Done;
    }
    if !has_field(page, Field::Code)
        && MAIL_WORDS.iter().any(|w| lower.contains(w))
        && !gate.filled_otp
    {
        return Step::Mailbox;
    }
    let consent = (gate.completion == Completion::Loopback || gate.filled_code || gate.confirmed)
        && !has_field(page, Field::Email)
        && !has_field(page, Field::Code);
    if consent
        && gate.pressed < MAX_CONSENT_PRESSES
        && let Some(at) = button_of(page, CONSENT_LABELS)
    {
        return Step::Press(at);
    }
    Step::Wait
}

/// Reads the page into a [`Page`]: its URL, first heading, visible text (8000 characters at
/// most), enabled visible buttons' labels and inputs' kinds.
pub const VENDOR_PAGE_JS: &str = r#"(() => {
  const visible = el => !el.disabled && !el.hidden && el.type !== "hidden" &&
    el.getAttribute("aria-disabled") !== "true" && el.getClientRects().length > 0;
  const label = b => String(b.innerText || b.value || b.getAttribute("aria-label") || "").trim().toLowerCase().slice(0, 80);
  const buttons = Array.from(document.querySelectorAll("button, input[type='submit'], [role='button']")).filter(visible);
  const kind = i => {
    const t = String(i.type || "").toLowerCase(), n = String(i.name || "").toLowerCase();
    const a = String(i.autocomplete || "").toLowerCase(), m = String(i.inputMode || "").toLowerCase();
    if (t === "password") return "password";
    if (t === "email" || a.includes("email") || a === "username") return "email";
    if (a === "one-time-code" || n.includes("code") || n.includes("otp") || m === "numeric" || i.maxLength === 1) return "code";
    if (n.includes("email")) return "email";
    return "other";
  };
  const inputs = Array.from(document.querySelectorAll("input")).filter(visible)
    .filter(i => !i.readOnly && !["checkbox", "radio", "submit", "button", "image", "reset"].includes(String(i.type || "").toLowerCase()))
    .map(kind);
  const h = document.querySelector("h1, h2");
  return {
    url: String(window.location && window.location.href || ""),
    heading: String(h && h.innerText || "").trim().toLowerCase().slice(0, 200),
    text: String(document.body && document.body.innerText || "").slice(0, 8000),
    buttons: buttons.map(label),
    inputs
  };
})()"#;

/// The JavaScript that carries out `step` on the page read by [`VENDOR_PAGE_JS`]: a press of the
/// same enumeration's button, or `value` typed into the first input of the step's kind and
/// submitted.
#[must_use]
pub fn vendor_act_js(step: &Step, value: Option<&str>) -> String {
    let (verb, index) = match step {
        Step::Press(at) => ("press", *at),
        Step::FillCode | Step::FillOtp => ("code", 0),
        Step::FillEmail => ("email", 0),
        _ => ("none", 0),
    };
    let value_json = aterm_json::to_string(&value.unwrap_or("")).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"(() => {{
  const setNativeValue = (el, val) => {{
    if (typeof el.focus === "function") el.focus();
    const proto = Object.getPrototypeOf(el);
    const desc = proto && Object.getOwnPropertyDescriptor(proto, "value");
    if (desc && desc.set) {{ desc.set.call(el, val); }} else {{ el.value = val; }}
    el.dispatchEvent(new Event("input", {{ bubbles: true }}));
    el.dispatchEvent(new Event("change", {{ bubbles: true }}));
  }};
{CODE_INPUT_HELPERS_JS}
{SUBMIT_INPUT_HELPERS_JS}
  const visible = el => !el.disabled && !el.hidden && el.type !== "hidden" &&
    el.getAttribute("aria-disabled") !== "true" && el.getClientRects().length > 0;
  const textInput = el => visible(el) && !el.readOnly &&
    !["hidden", "checkbox", "radio", "password", "submit", "button", "image", "reset"].includes(String(el.type || "").toLowerCase());
  const buttons = Array.from(document.querySelectorAll("button, input[type='submit'], [role='button']")).filter(visible);
  const verb = "{verb}";
  const value = {value_json};
  if (verb === "press") {{
    const b = buttons[{index}];
    if (!b) return {{ ok: false }};
    b.click();
    return {{ ok: true }};
  }}
  if (verb === "email") {{
    const isCodeField = el => {{
      const t = String(el.type || "").toLowerCase(), n = String(el.name || "").toLowerCase();
      const a = String(el.autocomplete || "").toLowerCase(), m = String(el.inputMode || "").toLowerCase();
      return t !== "email" && !a.includes("email") && a !== "username" &&
        (a === "one-time-code" || n.includes("code") || n.includes("otp") || m === "numeric" || el.maxLength === 1);
    }};
    const input = Array.from(document.querySelectorAll("input[type='email'], input[name*='email' i], input[autocomplete*='email' i], input[autocomplete='username' i]")).filter(el => !isCodeField(el)).find(textInput);
    if (!input || !value) return {{ ok: false }};
    setNativeValue(input, value);
    return {{ ok: submitInput(input, buttons, /continue|next|sign in|log in|send|submit|下一步|继续|登录|发送|验证|确认|提交/i) }};
  }}
  if (verb === "code") {{
    const explicit = Array.from(document.querySelectorAll("input[autocomplete='one-time-code' i], input[name*='code' i], input[name*='otp' i], input[inputmode='numeric' i]")).filter(textInput);
    const boxes = Array.from(document.querySelectorAll("input[maxlength='1']")).filter(textInput);
    const filled = fillCodeInputs(codeInputsFor(explicit, boxes), value);
    if (!filled) return {{ ok: false }};
    return {{ ok: submitInput(filled, buttons, /continue|verify|confirm|submit|next|sign in|log in|下一步|继续|验证|确认|提交|登录/i) }};
  }}
  return {{ ok: false }};
}})()"#
    )
}

/// Reads [`VENDOR_PAGE_JS`]'s answer.
#[must_use]
pub fn page_of(value: &Value) -> Page {
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    Page {
        url: text("url"),
        heading: text("heading"),
        text: text("text"),
        buttons: value
            .get("buttons")
            .and_then(Value::as_array)
            .map(|b| {
                b.iter()
                    .map(|l| l.as_str().unwrap_or("").to_owned())
                    .collect()
            })
            .unwrap_or_default(),
        inputs: value
            .get("inputs")
            .and_then(Value::as_array)
            .map(|i| {
                i.iter()
                    .map(|k| Field::of(k.as_str().unwrap_or("")))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// How often a vendor sign-in's page is read again.
const VENDOR_TICK: Duration = Duration::from_millis(400);

/// How long a vendor drive waits on one unchanged page that shows nothing aterm presses or
/// fills before it stops for a person ([`PageStall`]).
pub const VENDOR_STALL: Duration = Duration::from_secs(90);

/// The page a vendor drive is waiting on, and since when it is unchanged. A page that shows
/// nothing aterm presses or fills for [`VENDOR_STALL`] waits for a person (a sign-in through
/// another provider, a phone number, a CAPTCHA), so the drive stops there, naming the page,
/// instead of waiting out its whole budget; a device page already confirmed waits on the CLI,
/// never on a person, and never stalls.
#[derive(Debug)]
pub struct PageStall {
    seen: Option<(String, Instant)>,
    within: Duration,
}

impl Default for PageStall {
    fn default() -> PageStall {
        PageStall::within(VENDOR_STALL)
    }
}

impl PageStall {
    /// A stall that stops a page unchanged for `within` (a test's short one; else
    /// [`VENDOR_STALL`]).
    #[must_use]
    pub fn within(within: Duration) -> PageStall {
        PageStall { seen: None, within }
    }

    /// Notes that the drive decided `step` on `page` at `now`; `Some(why)` once the same page
    /// has asked nothing of aterm for its stall ([`VENDOR_STALL`]).
    pub fn note(
        &mut self,
        page: &Page,
        step: &Step,
        gate: &Gate<'_>,
        now: Instant,
    ) -> Option<String> {
        if !matches!(step, Step::Wait) || gate.confirmed {
            self.seen = None;
            return None;
        }
        let key = format!(
            "{}\n{}\n{:?}\n{:?}",
            page.url, page.heading, page.buttons, page.inputs
        );
        match &self.seen {
            Some((seen, since)) if *seen == key => {
                (now.saturating_duration_since(*since) >= self.within).then(|| {
                    format!(
                        "the sign-in waits on {} for a person: nothing there is aterm's to press \
                         or fill{}",
                        page_place(page, gate),
                        sign_in_once_tail(gate)
                    )
                })
            }
            _ => {
                self.seen = Some((key, now));
                None
            }
        }
    }
}

/// Where `page` is, for a person: its host and path (no query, which may carry the code) and
/// its heading, with the row's email, any other email and the CLI's code left out.
#[must_use]
pub fn page_place(page: &Page, gate: &Gate<'_>) -> String {
    let url = page.url.split(['?', '#']).next().unwrap_or("");
    let place = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let mut heading = page.heading.clone();
    for email in emails_in(&page.heading) {
        heading = heading.replace(&email, "…");
    }
    if let Some(code) = gate.code.filter(|c| !c.is_empty()) {
        heading = heading
            .replace(&code.to_ascii_lowercase(), "…")
            .replace(code, "…");
    }
    let heading = super::one_line(heading.trim(), 60);
    if heading.is_empty() {
        format!("`{}`", super::one_line(place, 80))
    } else {
        format!("`{}` (\"{heading}\")", super::one_line(place, 80))
    }
}

/// Drives one vendor sign-in page in `cdp` from `start` ([`vendor_step`] each tick) until the
/// page approves it, `finished` says the CLI is done, or `deadline`; `mailbox` answers a
/// [`Step::Mailbox`]. A page unchanged for [`VENDOR_STALL`] with nothing of aterm's on it ends
/// it [`super::login_drive::Approval::NeedsSignIn`] ([`PageStall`]).
pub fn drive_cdp_vendor(
    cdp: &mut CdpClient,
    start: &str,
    gate: Gate<'_>,
    finished: &dyn Fn() -> bool,
    deadline: Instant,
    mailbox: impl FnMut(&mut CdpClient, Option<&str>) -> Result<MailboxCredential, &'static str>,
) -> super::login_drive::Approval {
    drive_cdp_vendor_within(
        cdp,
        start,
        gate,
        (finished, deadline),
        PageStall::default(),
        mailbox,
    )
}

/// What a carried-out `step` on `page` leaves in `gate`: a device page's press is its one
/// confirm where the page names the printed code ([`device_code`], in its text or, on a
/// signed-in card, its URL), any other press a consent press; a fill marks its field filled.
pub(super) fn note_acted(gate: &mut Gate<'_>, page: &Page, step: &Step) {
    match step {
        Step::Press(_) => {
            if gate.completion == crate::ai::connector::Completion::Device
                && gate.code.is_some_and(|c| device_code(page, c).1)
            {
                gate.confirmed = true;
            } else {
                gate.pressed += 1;
            }
        }
        Step::FillCode => gate.filled_code = true,
        Step::FillEmail => gate.filled_email = true,
        Step::FillOtp => gate.filled_otp = true,
        _ => {}
    }
}

/// [`drive_cdp_vendor`] with `stall` its page stall (a test's short one).
#[allow(clippy::too_many_lines)]
pub(super) fn drive_cdp_vendor_within(
    cdp: &mut CdpClient,
    start: &str,
    gate: Gate<'_>,
    (finished, deadline): (&dyn Fn() -> bool, Instant),
    mut stall: PageStall,
    mut mailbox: impl FnMut(&mut CdpClient, Option<&str>) -> Result<MailboxCredential, &'static str>,
) -> super::login_drive::Approval {
    use super::login_drive::Approval;
    let step_timeout = || deadline.saturating_duration_since(Instant::now());
    let opened = (|| -> Result<(String, String), &'static str> {
        let target = cdp.call(
            None,
            "Target.createTarget",
            json!({"url": "about:blank"}),
            step_timeout(),
        )?;
        let target_id = target
            .get("targetId")
            .and_then(Value::as_str)
            .ok_or("browser created no targetId")?
            .to_owned();
        let attached = cdp.call(
            None,
            "Target.attachToTarget",
            json!({"targetId": target_id, "flatten": true}),
            step_timeout(),
        )?;
        let session = attached
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or("browser attached no sessionId")?
            .to_owned();
        for method in ["Page.enable", "Runtime.enable"] {
            let _ = cdp.call(Some(&session), method, json!({}), step_timeout())?;
        }
        let _ = cdp.call(
            Some(&session),
            "Page.navigate",
            json!({"url": start}),
            step_timeout(),
        )?;
        Ok((target_id, session))
    })();
    let (target_id, session) = match opened {
        Ok(opened) => opened,
        Err(why) => {
            return Approval::NeedsYou(format!(
                "the headless browser did not open the page ({why})"
            ));
        }
    };
    let mut gate = gate;
    let mut otp: Option<String> = None;
    let mut page_proved = false;
    let mut mailbox_polls = 0usize;
    let mut post_otp_wait_ticks = 0usize;
    let mut last_place: Option<String> = None;
    let result = loop {
        if finished() {
            break Approval::Approved { page: page_proved };
        }
        if Instant::now() >= deadline {
            break Approval::NeedsYou(match &last_place {
                Some(place) => {
                    format!("the sign-in page was not approved in time (last on {place})")
                }
                None => "the sign-in page was not approved in time".to_owned(),
            });
        }
        let eval = cdp.call(
            Some(&session),
            "Runtime.evaluate",
            json!({"expression": VENDOR_PAGE_JS, "returnByValue": true}),
            step_timeout(),
        );
        let page = match eval {
            Ok(eval) => page_of(eval.pointer("/result/value").unwrap_or(&Value::Null)),
            Err(_) => {
                std::thread::sleep(VENDOR_TICK);
                continue;
            }
        };
        let view = Gate {
            otp: otp.as_deref(),
            ..gate
        };
        if shows_email(&page, &view)
            && super::login_drive::https_host(&page.url)
                .is_some_and(|h| view.hosts.iter().any(|p| p.eq_ignore_ascii_case(&h)))
        {
            page_proved = true;
        }
        let step = vendor_step(&page, &view);
        last_place = Some(page_place(&page, &view));
        if let Some(why) = stall.note(&page, &step, &view, Instant::now()) {
            break Approval::NeedsSignIn(why);
        }
        let value = match step {
            Step::FillCode => gate.code,
            Step::FillEmail => gate.email,
            Step::FillOtp => otp.as_deref(),
            _ => None,
        };
        match &step {
            Step::Done => {
                if gate.completion == crate::ai::connector::Completion::Device {
                    while !finished() && Instant::now() < deadline {
                        std::thread::sleep(VENDOR_TICK);
                    }
                }
                break Approval::Approved { page: page_proved };
            }
            Step::Stop(why) => break Approval::NeedsYou(why.clone()),
            Step::SignIn(why) => break Approval::NeedsSignIn(why.clone()),
            Step::Refuse(why) => break Approval::Refused(why.clone()),
            Step::Wait => {
                let lower = page.text.to_ascii_lowercase();
                let still_wants_mail =
                    has_field(&page, Field::Code) || MAIL_WORDS.iter().any(|w| lower.contains(w));
                if gate.filled_otp && mailbox_polls < 3 && still_wants_mail {
                    post_otp_wait_ticks += 1;
                    if post_otp_wait_ticks >= 2 {
                        gate.filled_otp = false;
                        otp = None;
                        post_otp_wait_ticks = 0;
                    }
                } else {
                    post_otp_wait_ticks = 0;
                }
            }
            Step::Mailbox => {
                post_otp_wait_ticks = 0;
                if mailbox_polls >= 3 {
                    break Approval::NeedsYou(
                        "the vendor's sign-in mail did not come, or no mailbox is configured"
                            .to_owned(),
                    );
                }
                mailbox_polls += 1;
                match mailbox(cdp, gate.email) {
                    Ok(MailboxCredential::Otp(code)) => {
                        otp = Some(code);
                        gate.filled_otp = false;
                    }
                    Ok(MailboxCredential::MagicLink(link)) => {
                        if !super::login_drive::pinned(&link, gate.hosts) {
                            break Approval::NeedsYou(
                                "the sign-in mail's link is off the pinned hosts".to_owned(),
                            );
                        }
                        if let Some(why) = gate.bound.and_then(|b| b.outside(&link, gate.hosts)) {
                            break Approval::Refused(format!("the sign-in mail's link: {why}"));
                        }
                        let _ = cdp.call(
                            Some(&session),
                            "Page.navigate",
                            json!({"url": link}),
                            step_timeout(),
                        );
                        gate.filled_otp = true;
                    }
                    Err(why) => break Approval::NeedsYou(why.to_owned()),
                }
            }
            Step::Press(_) | Step::FillCode | Step::FillEmail | Step::FillOtp => {
                post_otp_wait_ticks = 0;
                let acted = cdp
                    .call(
                        Some(&session),
                        "Runtime.evaluate",
                        json!({"expression": vendor_act_js(&step, value), "returnByValue": true}),
                        step_timeout(),
                    )
                    .ok()
                    .and_then(|v| v.pointer("/result/value/ok").and_then(Value::as_bool))
                    .unwrap_or(false);
                if acted {
                    note_acted(&mut gate, &page, &step);
                }
            }
        }
        std::thread::sleep(VENDOR_TICK);
    };
    let _ = cdp.call(
        None,
        "Target.closeTarget",
        json!({"targetId": target_id}),
        Duration::from_secs(2),
    );
    let _ = cdp.call(None, "Browser.close", json!({}), Duration::from_secs(2));
    result
}

/// Runs `f` in the browser profile `dir` ([`vendor_profile_dir`]).
#[cfg(unix)]
fn in_profile(
    dir: &Path,
    f: impl FnOnce(&Path) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    f(dir)
}

/// The headless browser as a vendor sign-in's [`super::login_drive::Approver`]: the persistent
/// profile of the row's email (else `<cli>-<label>`) under Claude Code's store, shared by every
/// vendor ([`row_browser_profile`], [`vendor_profile_dir`]), where a window's pass that never
/// ran is made up first ([`super::browser_window::make_up_tidy`]); the mailbox configured for
/// that email, its inbox rows chosen by the vendor's senders.
#[derive(Debug, Clone)]
pub struct VendorApprover {
    /// Claude Code's store directory (`accounts::Store::shared_dir`).
    pub shared_dir: PathBuf,
    /// The profile's key where the row names no email.
    pub label_key: String,
    /// `aterm harness accounts browser <label> --cli <id>`, which a stop on the vendor's sign-in
    /// page names.
    pub sign_in_once: String,
    /// The mailbox, where one is configured for the row.
    pub mailbox: Option<MailboxConfig>,
    /// The vendor's sender words.
    pub senders: &'static [&'static str],
    /// `$HOME`, for the browser's location.
    pub home: Option<PathBuf>,
    /// Whether the row's email may be typed into the vendor's page
    /// ([`crate::ai::connector::SignIn::types_email`]).
    pub types_email: bool,
}

impl VendorApprover {
    /// The approver for `label` (of `cli`'s pool) with `email`, under `shared_dir`.
    #[must_use]
    pub fn new(
        cli: super::accounts::Cli,
        label: &str,
        email: Option<&str>,
        shared_dir: &Path,
        home: Option<&Path>,
    ) -> VendorApprover {
        let label_key = vendor_label_key(cli, label);
        let senders = cli.accounts().mail_senders;
        let mailbox = discover_mailbox_config(home, Some(shared_dir))
            .map(|mut cfg| {
                cfg.alias_label_to_email(Some(label), email);
                cfg.senders = senders.iter().map(|s| (*s).to_owned()).collect();
                cfg
            })
            .filter(|cfg| cfg.is_configured_for(email));
        VendorApprover {
            shared_dir: shared_dir.to_path_buf(),
            label_key,
            sign_in_once: format!("aterm harness accounts browser {label} --cli {}", cli.id()),
            mailbox,
            senders,
            home: home.map(Path::to_path_buf),
            types_email: cli
                .accounts()
                .signin
                .as_ref()
                .is_some_and(|signin| signin.types_email),
        }
    }

    /// The browser profile a sign-in of the row with `email` runs in ([`vendor_profile_dir`]).
    #[must_use]
    pub fn profile_dir(&self, email: Option<&str>) -> PathBuf {
        vendor_profile_dir(&self.shared_dir, &self.label_key, email)
    }
}

impl super::login_drive::Approver for VendorApprover {
    #[cfg(unix)]
    fn approve(
        &mut self,
        handoff: &super::login_drive::Handoff<'_>,
    ) -> super::login_drive::Approval {
        use super::login_drive::Approval;
        use aterm_uds::spawnfd::{ChildStdio, LaneCommand, spawn_with_two_fds};
        use std::os::fd::OwnedFd;
        let bound = match FlowBound::of(handoff.url) {
            Ok(bound) => bound,
            Err(why) => return Approval::Refused(why.to_owned()),
        };
        let profile_dir = self.profile_dir(handoff.email);
        let _ = aterm_types::fs_restricted::ensure_private_dir(
            profile_dir.parent().unwrap_or(&self.shared_dir),
        );
        let _ = aterm_types::fs_restricted::ensure_private_dir(&profile_dir);
        let _claimed = match super::browser_window::ProfileLock::claim(&profile_dir) {
            Ok(claimed) => claimed,
            Err(why) => {
                return Approval::NeedsYou(format!(
                    "the row's browser profile is {}",
                    why.words(&profile_dir)
                ));
            }
        };
        let Some(browser) = find_headless_browser(self.home.as_deref()) else {
            return Approval::NeedsYou("no headless browser is installed here".to_owned());
        };
        let mut outcome = Approval::NeedsYou("the headless browser did not run".to_owned());
        let started = aterm_time::unix_secs().and_then(|t| i64::try_from(t).ok());
        let mailbox = self.mailbox.clone().map(|mut cfg| {
            cfg.since = started;
            cfg
        });
        let senders = self.senders;
        let sign_in_once = self.sign_in_once.as_str();
        let types_email = self.types_email;
        let ran = in_profile(&profile_dir, |profile_dir| {
            let (cmd_read, cmd_write) =
                std::io::pipe().map_err(|_| "could not open CDP command pipe")?;
            let (out_read, out_write) =
                std::io::pipe().map_err(|_| "could not open CDP event pipe")?;
            let mut cmd = LaneCommand::new(&browser.path);
            cmd.args(headless_launch_args(&browser, profile_dir))
                .stdin(ChildStdio::Null)
                .stdout(ChildStdio::Null)
                .stderr(ChildStdio::Null);
            let child = spawn_with_two_fds(cmd, OwnedFd::from(cmd_read), OwnedFd::from(out_write))
                .map_err(|_| "could not spawn headless browser")?;
            let _guard = BrowserChild::new(child);
            let mut cdp = CdpClient::new(cmd_write, out_read);
            let _ = super::browser_window::make_up_tidy(
                &mut cdp,
                profile_dir,
                handoff
                    .deadline
                    .min(Instant::now() + super::browser_window::MAKE_UP_WITHIN),
            );
            let mut seen_otps = BTreeSet::new();
            let gate = Gate {
                hosts: handoff.hosts,
                completion: handoff.completion,
                code: handoff.code,
                email: handoff.email,
                senders,
                otp: None,
                pressed: 0,
                confirmed: false,
                filled_code: false,
                filled_email: false,
                filled_otp: false,
                bound: Some(&bound),
                sign_in_once: Some(sign_in_once),
                types_email,
            };
            outcome = drive_cdp_vendor(
                &mut cdp,
                handoff.url,
                gate,
                handoff.finished,
                handoff.deadline,
                |cdp_inner, em| {
                    let mb = mailbox.as_ref().ok_or(
                        "the page asks for the sign-in mail, and no mailbox is configured",
                    )?;
                    poll_mailbox_or_webmail_for_credential(
                        cdp_inner,
                        mb,
                        em,
                        handoff.deadline.saturating_duration_since(Instant::now()),
                        Duration::from_millis(400),
                        &mut seen_otps,
                    )
                },
            );
            Ok(())
        });
        match ran {
            Ok(()) => outcome,
            Err(why) => Approval::NeedsYou(why.to_owned()),
        }
    }

    #[cfg(not(unix))]
    fn approve(&mut self, _: &super::login_drive::Handoff<'_>) -> super::login_drive::Approval {
        super::login_drive::Approval::NeedsYou(
            "the headless sign-in runs on Unix hosts only".to_owned(),
        )
    }
}

#[cfg(test)]
#[path = "headless_vendor_tests.rs"]
pub(super) mod vendor_tests;

#[cfg(test)]
mod uri_authority_tests {
    #[test]
    fn userinfo_ends_before_path_query_and_fragment() {
        for (url, embedded) in [
            ("https://user:password@mail.example", true),
            ("http://user@mail.example?to=a@b", true),
            ("https://user:password@mail.example#fragment@tail", true),
            ("https://mail.example?to=a@b", false),
            ("https://mail.example#fragment@tail", false),
            ("https://mail.example/path@tail", false),
            ("https://mail.example/?to=a@b", false),
            ("https://@mail.example", false),
            ("https://mail.example", false),
            ("https://?to=a@b", false),
            ("ftp://user:password@mail.example", false),
            ("HTTPS://user@mail.example", true),
        ] {
            assert_eq!(super::url_has_embedded_userinfo(url), embedded, "{url}");
        }

        for (url, selected) in [
            ("http://localhost:1777", true),
            ("http://user:password@127.0.0.1:1777?to=a@b", true),
            ("http://[::1]:1777?to=a@b", true),
            ("http://[2001:db8::1]:1777?to=a@b", false),
            ("http://remote.example?to=owner@127.0.0.1:1777", false),
            ("http://remote.example#owner@localhost:1777", false),
            ("http://remote.example/path@127.0.0.1:1777", false),
            ("https://localhost:1777", false),
        ] {
            assert_eq!(super::is_loopback_http_url(url), selected, "{url}");
        }

        for (url, authority, userinfo, target) in [
            ("http://localhost:1777", "localhost:1777", None, "/"),
            (
                "http://[::1]:1777?to=a@b#fragment@tail",
                "[::1]:1777",
                None,
                "/?to=a@b",
            ),
            (
                "http://user:password@127.0.0.1:1777?to=a@b#fragment@tail",
                "127.0.0.1:1777",
                Some("user:password"),
                "/?to=a@b",
            ),
            (
                "http://localhost:1777/mail?to=a@b#fragment@tail",
                "localhost:1777",
                None,
                "/mail?to=a@b",
            ),
            (
                "http://@localhost:1777#tail",
                "localhost:1777",
                Some(""),
                "/",
            ),
        ] {
            let parsed = super::parse_loopback_http_url(url).unwrap();
            assert_eq!(parsed.authority, authority, "{url}");
            assert_eq!(parsed.userinfo, userinfo, "{url}");
            assert_eq!(parsed.port, 1777, "{url}");
            assert_eq!(parsed.target, target, "{url}");
        }
    }
}
