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
    extract_magic_links_from_email, percent_decode, percent_encode,
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

/// Environment variable naming a webmail inbox URL (e.g. `https://mail.proton.me/u/0/inbox`) for headless CDP polling.
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

    /// True when at least one mailbox source (directory, HTTP URL, IMAP URL, or webmail credentials) is configured.
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

    /// Map any per-account credentials keyed by `label` onto `email`.
    pub fn alias_label_to_email(&mut self, label: Option<&str>, email: Option<&str>) {
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

/// Derive a canonical machine/slot account label (such as `"m27"` or `"m27-2"`) from an email
/// address.
#[must_use]
pub fn derive_account_label_from_email(email: &str) -> Option<String> {
    let (raw_local, _) = email.trim().split_once('@')?;
    let local = raw_local.split('+').next().unwrap_or(raw_local);
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
        let base = machines.join("-");
        return Some(match slot {
            Some(n) => format!("{base}-{n}"),
            None => base,
        });
    }
    None
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
        if base_local != raw_local && !base_local.is_empty() {
            let base_email = format!("{base_local}@{domain}");
            push_unique(base_email.clone());
            push_unique(sanitize_profile_key(&base_email));
        }
        for loc in [raw_local, base_local] {
            push_unique(loc.to_owned());
            push_unique(loc.replace(['.', '_'], "-"));
            push_unique(loc.replace(['.', '-'], "_"));
        }
        if let Some(lbl) = derive_account_label_from_email(&lower) {
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
    let mut fleet_roster_candidates = Vec::new();
    if let Some(h) = home {
        let roster_path = h.join(".config/aterm/accounts.toml");
        toml_candidates.push(roster_path.clone());
        roster_candidates.push(roster_path);
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
                        && !matches!(
                            lower.as_str(),
                            "fleet-accounts.toml"
                                | "ai-accounts.csv"
                                | "ai-mailbox.toml"
                                | "mailbox.toml"
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
    for roster_path in roster_candidates {
        if roster_path.is_file()
            && let Ok(roster) = super::accounts::Roster::load(&roster_path)
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
                cfg.alias_label_to_email(Some(&acct.label), Some(&acct.email));
            }
        }
    }
    if let Some(h) = home {
        let csv_path = h.join(".secrets/ai-accounts.csv");
        if csv_path.is_file()
            && let Ok(Some(csv_text)) = super::accounts::read_bounded(&csv_path, 512 * 1024)
            && let Ok(fleet) = super::accounts_sync::parse_accounting_csv(&csv_text, "csv")
        {
            for acct in &fleet.accounts {
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

fn is_shell_binary(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains("headless-shell") || n.contains("headless_shell"))
}

fn browser_cache_roots(home: Option<&Path>) -> Vec<PathBuf> {
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
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
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
        for dir in dirs {
            for sub in [
                "chrome-headless-shell-mac-arm64/chrome-headless-shell",
                "chrome-headless-shell-mac-x64/chrome-headless-shell",
                "chrome-headless-shell-linux64/chrome-headless-shell",
                "chrome-headless-shell",
            ] {
                let candidate = dir.join(sub);
                if candidate.is_file() {
                    return Some(HeadlessBrowser {
                        path: candidate,
                        is_headless_shell: true,
                    });
                }
            }
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
    let base = local.split('+').next().unwrap_or(local);
    (base != local && !base.is_empty()).then(|| format!("{base}@{domain}"))
}

/// Whether `store_dir` already has a persistent headless browser profile for `label` or `email`.
#[must_use]
pub fn has_browser_profile(store_dir: &Path, label: &str, email: Option<&str>) -> bool {
    resolve_browser_profile_dir(store_dir, Some(label), email).is_dir()
}

/// Infer the account label from `CLAUDE_CONFIG_DIR` (`<store>/logins/<label>[+<seq>]`) when set.
#[must_use]
pub fn infer_account_label_from_env() -> Option<String> {
    let dir = std::env::var_os(super::accounts::HOME_ENV)?;
    let name = Path::new(&dir).file_name()?.to_str()?;
    let label = name.rsplit_once('+').map_or(name, |(l, _)| l).trim();
    (!label.is_empty()).then(|| label.to_owned())
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
        let dir = browser_profile_dir(store_dir, Some(em));
        if dir.is_dir() {
            return dir;
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
    browser_profile_dir(store_dir, base_email.as_deref().or(email).or(label))
}

/// Run authorization with a persistent store profile or an owned scratch profile.
/// An authorization error does not make persistent cookies or profile data disposable.
///
/// # Errors
///
/// Scratch allocation failure or the authorization's diagnostic, unchanged.
#[cfg(unix)]
pub(super) fn with_browser_profile(
    store_dir: Option<&Path>,
    label: Option<&str>,
    email: Option<&str>,
    authorize: impl FnOnce(&Path) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    let temp_dir;
    let profile_dir = if let Some(store) = store_dir {
        let dir = resolve_browser_profile_dir(store, label, email);
        let _ = aterm_types::fs_restricted::ensure_private_dir(dir.parent().unwrap_or(store));
        let _ = aterm_types::fs_restricted::ensure_private_dir(&dir);
        dir
    } else {
        temp_dir =
            aterm_tempfile::tempdir().map_err(|_| "could not create scratch browser profile")?;
        temp_dir.path().to_path_buf()
    };
    authorize(&profile_dir)
}

/// Build the command-line arguments for launching `browser` with `profile_dir`.
#[must_use]
pub fn headless_launch_args(browser: &HeadlessBrowser, profile_dir: &Path) -> Vec<OsString> {
    let mut args = Vec::new();
    if !browser.is_headless_shell {
        args.push("--headless=new".into());
    }
    let mut user_data = OsString::from("--user-data-dir=");
    user_data.push(profile_dir);
    args.push(user_data);
    for flag in [
        "--remote-debugging-pipe",
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
    args.push("about:blank".into());
    args
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
        // Avoid matching 6-digit dates/postal/color fragments when preceded by `#` or `:`.
        if token.starts_with('#') || token.starts_with(':') {
            return None;
        }
        return Some(cleaned.to_owned());
    }
    None
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
    let qp_decoded = if is_json_with_body {
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
    let mut fallbacks = Vec::new();
    let mut context_lines_left = 0usize;
    let mut single_digit_run = String::new();
    let mut single_digit_run_context = false;
    fn clean_digits(tok: &str) -> Option<&str> {
        let trimmed = tok.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
        (!trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_digit())).then_some(trimmed)
    }
    let flush_single_digit_run =
        |run: &mut String, run_ctx: &mut bool, pri: &mut Vec<String>, fb: &mut Vec<String>| {
            if run.len() == 6 {
                let code = run.clone();
                if *run_ctx {
                    if !pri.iter().any(|existing| existing == &code) {
                        pri.push(code);
                    }
                } else if !fb.iter().any(|existing| existing == &code) {
                    fb.push(code);
                }
            }
            run.clear();
            *run_ctx = false;
        };
    for line in plain.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.is_empty() {
            continue;
        }
        if words.len() == 1
            && let Some(d) = clean_digits(words[0])
            && d.len() == 1
        {
            if single_digit_run.is_empty() {
                single_digit_run_context = context_lines_left > 0;
            }
            single_digit_run.push_str(d);
            continue;
        }
        flush_single_digit_run(
            &mut single_digit_run,
            &mut single_digit_run_context,
            &mut prioritized,
            &mut fallbacks,
        );
        let lower = line.to_ascii_lowercase();
        let line_has_context = lower.contains("code")
            || lower.contains("verify")
            || lower.contains("verification")
            || lower.contains("sign in")
            || lower.contains("login")
            || lower.contains("one-time");
        if line_has_context {
            context_lines_left = 3;
        }
        let has_context = context_lines_left > 0;
        if words.len() == 1
            && let Some(otp) = is_six_digit_otp(words[0])
        {
            push_unique(&mut prioritized, otp);
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
            push_unique(&mut prioritized, joined);
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
        &mut fallbacks,
    );
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
    let links = extract_magic_links_from_email(raw_email, target_email);
    if !links.is_empty() {
        return links
            .into_iter()
            .map(MailboxCredential::MagicLink)
            .collect();
    }
    extract_otps_from_email(raw_email, target_email)
        .into_iter()
        .map(MailboxCredential::Otp)
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

fn extract_unseen_credential_from_email(
    raw_email: &str,
    target_email: Option<&str>,
    seen_digests: &BTreeSet<String>,
    seen_otps: &BTreeSet<String>,
) -> Option<(MailboxCredential, bool)> {
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

fn parse_imap_url(url: &str) -> Option<ParsedImapUrl> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("imaps://") {
        (true, r)
    } else {
        let r = url.strip_prefix("imap://")?;
        (false, r)
    };
    let (authority, path_part) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx + 1..]),
        None => (rest, ""),
    };
    let (user, password, host_port) = match authority.rsplit_once('@') {
        Some((userinfo, hp)) => match userinfo.split_once(':') {
            Some((u, p)) => (Some(percent_decode(u)), Some(percent_decode(p)), hp),
            None => (Some(percent_decode(userinfo)), None, hp),
        },
        None => (None, None, authority),
    };
    let default_port = if tls { 993 } else { 143 };
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => (h.to_owned(), p.parse::<u16>().ok()?),
        None => (host_port.to_owned(), default_port),
    };
    if host.is_empty() || port == 0 {
        return None;
    }
    let mailbox_raw = path_part
        .split('?')
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

fn imap_read_until_tag(
    reader: &mut BufReader<TcpStream>,
    tag: &str,
) -> Option<(bool, Vec<String>)> {
    let mut lines = Vec::new();
    let ok_prefix = format!("{tag} OK");
    let no_prefix = format!("{tag} NO");
    let bad_prefix = format!("{tag} BAD");
    for _ in 0..512 {
        let mut line = String::new();
        let n = reader.read_line(&mut line).ok()?;
        if n == 0 {
            return None;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed
            .get(..ok_prefix.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&ok_prefix))
        {
            return Some((true, lines));
        }
        if trimmed
            .get(..no_prefix.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&no_prefix))
            || trimmed
                .get(..bad_prefix.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&bad_prefix))
        {
            return Some((false, lines));
        }
        lines.push(line);
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
        let literal_len = fetch_idx.and_then(|fi| {
            let hdr = msg_lines[fi].trim_end();
            let inner = hdr.strip_suffix('}')?.rsplit_once('{')?.1;
            inner
                .strip_suffix('+')
                .unwrap_or(inner)
                .parse::<usize>()
                .ok()
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
        let mut end = msg_lines.len();
        while end > start && msg_lines[end - 1].trim_start().starts_with("* ") {
            end -= 1;
        }
        if end > start && msg_lines[end - 1].trim_start().starts_with(')') {
            end -= 1;
        }
        let joined: String = msg_lines[start..end].concat();
        let raw_msg = match literal_len {
            Some(n) if n <= joined.len() && joined.is_char_boundary(n) => &joined[..n],
            _ => joined.as_str(),
        };
        if raw_msg.trim().is_empty() {
            continue;
        }
        let digest = fnv1a_hex(raw_msg);
        if seen.contains(&digest) {
            continue;
        }
        if let Some((cred, exhausted)) =
            extract_unseen_credential_from_email(raw_msg, target_email, seen, seen_otps)
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

fn fetch_imaps_curl_credential(
    parsed: &ParsedImapUrl,
    user: &str,
    password: &str,
    target_email: Option<&str>,
    seen_file: Option<&Path>,
    seen: &mut BTreeSet<String>,
    seen_otps: &mut BTreeSet<String>,
) -> Option<MailboxCredential> {
    let userpwd = format!("{user}:{password}");
    let search_url = format!(
        "imaps://{}:{}/{}?ALL",
        parsed.host,
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
            "imaps://{}:{}/{};MAILINDEX={seq}",
            parsed.host,
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
            extract_unseen_credential_from_email(&raw_msg, target_email, seen, seen_otps)
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
        )
    }
}

fn authority_and_tail(rest: &str) -> (&str, &str) {
    rest.find(['/', '?', '#'])
        .map_or((rest, ""), |end| rest.split_at(end))
}

/// Whether `host` is this machine's loopback (`localhost`, `127.0.0.0/8`, `::1`).
fn is_loopback_host(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    bare.eq_ignore_ascii_case("localhost")
        || bare
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Why no credential is sent to `url`: `imap://` and `http://` carry it in cleartext, so only to a loopback host.
fn cleartext_refusal(url: &str) -> Option<&'static str> {
    if let Some(imap) = parse_imap_url(url) {
        return (!imap.tls && !is_loopback_host(&imap.host)).then_some(
            "imap:// sends its login in cleartext, so it is used only for a loopback host; \
             name an imaps:// mailbox",
        );
    }
    let (authority, _) = authority_and_tail(url.strip_prefix("http://")?);
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
    let host = match host_port.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(v6),
        None => host_port.rsplit_once(':').map_or(host_port, |(h, _)| h),
    };
    (!is_loopback_host(host)).then_some(
        "http:// sends its password or token in cleartext, so it is used only for a loopback \
         host; name an https:// mailbox",
    )
}

/// Why `cfg` is not polled for `target_email`: an `imap://` login, or an `http://` password or token, to a host that is not loopback ([`cleartext_refusal`]).
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
    let url = cfg.http_url.as_deref()?;
    let credentialed = url_has_embedded_userinfo(url)
        || cfg.bearer_token.as_deref().is_some_and(|t| !t.is_empty())
        || (cfg.webmail_user_for(target_email).is_some()
            && cfg.password_for(target_email).is_some());
    credentialed.then(|| cleartext_refusal(url)).flatten()
}

struct LoopbackHttpUrl<'a> {
    authority: &'a str,
    userinfo: Option<&'a str>,
    port: u16,
    target: String,
}

fn parse_loopback_http_url(url: &str) -> Option<LoopbackHttpUrl<'_>> {
    let rest = url.strip_prefix("http://")?;
    let (raw_authority, tail) = authority_and_tail(rest);
    let (userinfo, authority) = match raw_authority.rsplit_once('@') {
        Some((ui, host_port)) => (Some(ui), host_port),
        None => (None, raw_authority),
    };
    let (host, port) = authority.rsplit_once(':')?;
    if !matches!(host, "127.0.0.1" | "localhost") {
        return None;
    }
    let target = tail.split('#').next().unwrap_or("");
    let target = if target.is_empty() || target.starts_with('?') {
        format!("/{target}")
    } else {
        target.to_owned()
    };
    Some(LoopbackHttpUrl {
        authority,
        userinfo,
        port: port.parse().ok()?,
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
    let parsed = parse_loopback_http_url(url)?;
    let LoopbackHttpUrl {
        authority,
        userinfo: embedded_userinfo,
        port,
        target: path_and_query,
    } = parsed;
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
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
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
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
    let cfg_pw = cfg.password_for(target_email);
    let cfg_user = cfg.webmail_user_for(target_email);
    let basic_auth = cfg_user.as_deref().zip(cfg_pw.as_deref());
    if is_loopback_http_url(url) {
        fetch_loopback_http_body(url, cfg.bearer_token.as_deref(), basic_auth)
    } else if url.starts_with("https://") || url.starts_with("http://") {
        let credential = if let Some(token) = cfg.bearer_token.as_deref().filter(|t| !t.is_empty())
        {
            Some(("header", format!("Authorization: Bearer {token}")))
        } else if !url_has_embedded_userinfo(url)
            && let Some((u, p)) = basic_auth
        {
            Some(("user", format!("{u}:{p}")))
        } else {
            None
        };
        if (credential.is_some() || url_has_embedded_userinfo(url))
            && cleartext_refusal(url).is_some()
        {
            return None;
        }
        let mut options = vec![("url", url)];
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

fn fetch_http_mailbox_credential(
    cfg: &MailboxConfig,
    target_email: Option<&str>,
    seen: &mut BTreeSet<String>,
    seen_otps: &mut BTreeSet<String>,
) -> Option<MailboxCredential> {
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
                if let Some((cred, exhausted)) =
                    extract_unseen_credential_from_email(&item_str, target_email, seen, seen_otps)
                {
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
        extract_unseen_credential_from_email(&body, target_email, seen, seen_otps)?;
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
    seen_digests: &mut BTreeSet<String>,
    seen_otps: &mut BTreeSet<String>,
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
                if let Some((found, exhausted)) =
                    extract_unseen_credential_from_email(msg, target_email, seen_digests, seen_otps)
                {
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
        if let Some((found, exhausted)) =
            extract_unseen_credential_from_email(&raw, target_email, seen_digests, seen_otps)
        {
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
            seen_digests,
            seen_otps,
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
    let has_non_webmail = cfg.dir.is_some()
        || cfg.effective_imap_url(target_email).is_some()
        || cfg.http_url.is_some();
    if !has_non_webmail {
        if cfg.effective_webmail_url(target_email).is_some() {
            return Err("webmail inbox requires headless browser");
        }
        return Err("headless browser requires sign-in, but no mailbox is configured");
    }
    if let Some(why) = plaintext_credential_refusal(cfg, target_email) {
        return Err(why);
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
                &mut seen_digests,
                seen_otps,
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
    !["hidden", "checkbox", "radio"].includes(input.type));
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
  const submitInput = (input, buttons, labelPattern) => {
    const enabled = control => !control.disabled && !control.hidden &&
      !control.matches(":disabled") && control.getAttribute("aria-disabled") !== "true";
    if (!enabled(input) || input.readOnly || input.type === "hidden") return false;
    const form = input.form;
    const owner = button => "form" in button ? button.form : button.closest("form");
    const owned = buttons.filter(button => owner(button) === form);
    const isSubmit = button => (button.tagName === "BUTTON" && button.type === "submit") ||
      (button.tagName === "INPUT" && ["submit", "image"].includes(button.type));
    const button = (form && owned.find(button => isSubmit(button) && enabled(button))) ||
      owned.find(button => button.type !== "reset" && enabled(button) &&
        labelPattern.test(String(button.innerText || button.value || "")));
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
  const buttons = Array.from(document.querySelectorAll("button, input[type='submit'], input[type='image'], [role='button'], a[href]"));
  const emailVal = {email_json};
  const targetEmail = String(emailVal || "").trim().toLowerCase();
  const bodyText = String(document.body && document.body.innerText || "").toLowerCase();
  const btnText = (btn) => String(btn.innerText || btn.value || btn.getAttribute("aria-label") || "").trim().toLowerCase();
  const switchBtn = buttons.find(btn => !btn.disabled &&
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
  const hasTargetEmail = (text) => Boolean(targetCanon) && emailsIn(text).some(e => canonEmail(e) === targetCanon);
  const pageEmails = emailsIn(bodyText)
    .filter(e => !e.endsWith("@anthropic.com"));
  const hasForeignAccount = Boolean(targetEmail) && !hasTargetEmail(bodyText) && (
    buttons.some(btn => !btn.disabled && btnText(btn).startsWith("continue as ") && emailsIn(btnText(btn)).length > 0 && !hasTargetEmail(btnText(btn))) ||
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
    if (btn.disabled) continue;
    const text = btnText(btn);
    const hrefAttr = String(btn.getAttribute("href") || "");
    const isCancel = text.includes("cancel") || text.includes("deny") || text.includes("decline") || text.includes("reject") || text.includes("do not ") || text.includes("don't ") || text.includes("dont ") || text.includes("not allow") || text.includes("not authorize");
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
  const approveForm = document.querySelector("form[action*='approve'], form[action*='authorize']");
  if (approveForm) {{
    if (typeof approveForm.requestSubmit === "function") {{
      approveForm.requestSubmit();
    }} else {{
      approveForm.submit();
    }}
    return {{ state: "clicked_consent", url: href }};
  }}
  const explicitOtp = Array.from(document.querySelectorAll("input[autocomplete='one-time-code'], input[name*='code'], input[name*='otp'], input[inputmode='numeric'], input[data-testid*='otp']"));
  const singleCharBoxes = Array.from(document.querySelectorAll("input[maxlength='1']"));
  const otpInputs = codeInputsFor(explicitOtp, singleCharBoxes);
  const otpVal = {otp_json};
  const otpDone = {otp_submitted};
  if (otpInputs.length > 0) {{
    if (otpVal && !otpDone) {{
      const filledInput = fillCodeInputs(otpInputs, otpVal);
      if (!filledInput) return {{ state: "loading", url: href }};
      if (!submitInput(filledInput, buttons, /verify|continue|submit|sign in|log in/i)) {{
        return {{ state: "loading", url: href }};
      }}
      return {{ state: "submitted_otp", url: href }};
    }}
    return {{ state: "needs_mailbox", url: href }};
  }}
  const visibleControl = (el) => Boolean(el) && !el.disabled && !el.hidden && el.type !== "hidden" &&
    !el.matches(":disabled") && el.getAttribute("aria-disabled") !== "true";
  const emailInput = Array.from(document.querySelectorAll("input[type='email'], input[name='email'], input[autocomplete*='email'], input[autocomplete='username']")).find(visibleControl) || null;
  const emailDone = {email_submitted};
  if (emailInput && emailVal && !emailDone) {{
    setNativeValue(emailInput, emailVal);
    if (!submitInput(emailInput, buttons, /continue|sign in|log in|next|send/i)) {{
      return {{ state: "loading", url: href }};
    }}
    return {{ state: "submitted_email", url: href }};
  }}
  if (bodyText.includes("check your email") || bodyText.includes("magic link") || bodyText.includes("sent a link") || bodyText.includes("verification code") || bodyText.includes("link has expired") || bodyText.includes("link expired") || bodyText.includes("invalid link") || bodyText.includes("already been used")) {{
    return {{ state: "needs_mailbox", url: href }};
  }}
  if (targetEmail) {{
    for (const btn of buttons) {{
      if (btn.disabled) continue;
      const text = btnText(btn);
      const isCancel = text.includes("cancel") || text.includes("deny") || text.includes("decline") || text.includes("reject") || text.includes("do not ") || text.includes("don't ") || text.includes("dont ") || text.includes("not allow") || text.includes("not authorize");
      if (!isCancel && text.startsWith("continue as ") && hasTargetEmail(text)) {{
        btn.click();
        return {{ state: "clicked_consent", url: href, target: text }};
      }}
    }}
  }}
  for (const btn of buttons) {{
    if (btn.disabled) continue;
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
) -> String {
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
    /sign in|log in|authenticate|unlock|verify|continue|next|submit/i);
  const visibleControl = (el) => Boolean(el) && !el.disabled && !el.hidden && el.type !== "hidden" &&
    !el.matches(":disabled") && el.getAttribute("aria-disabled") !== "true";

  const userEl = Array.from(document.querySelectorAll("input#username, input[name='username'], input[name='email'], input[name='loginfmt'], input#login_id, input[name='LOGIN_ID'], input#_user, input[name='_user'], input[autocomplete='username'], input[autocomplete*='email'], input[type='email']")).find(visibleControl) || null;
  const passEl = Array.from(document.querySelectorAll("input#password, input[name='password'], input[name='passwd'], input#_pass, input[name='_pass'], input[type='password']:not(#mailboxPassword):not([name='mailboxPassword'])")).find(visibleControl) || null;
  const emailVal = {email_json};
  const pwVal = {pw_json};
  if (userEl && !passEl && emailVal) {{
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

  const explicitTotp = Array.from(document.querySelectorAll("input#twoFa, input[name='twoFa'], input[name='totp'], input[name='otc'], input[autocomplete='one-time-code'], input[id*='totp' i], input[name*='totp' i], input[name*='otp' i], input[data-testid*='totp' i], input[inputmode='numeric']"));
  const splitTotp = Array.from(document.querySelectorAll("input[maxlength='1']"));
  const totpInputs = codeInputsFor(explicitTotp, splitTotp);
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

  const mpwEl = Array.from(document.querySelectorAll("input#mailboxPassword, input[name='mailboxPassword']")).find(visibleControl) || null;
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
  const authRows = [];
  const senderRows = [];
  for (const row of rows) {{
    const rt = String(row.innerText || "").toLowerCase();
    if (rt.includes("sign in") || rt.includes("verification") || rt.includes("verify") || rt.includes("login") || rt.includes("magic")) {{
      authRows.push(row);
    }} else if (rt.includes("claude") || rt.includes("anthropic")) {{
      senderRows.push(row);
    }}
  }}
  const candidates = authRows.concat(senderRows);
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
    let webmail_url = cfg
        .effective_webmail_url(target_email)
        .ok_or("headless browser requires sign-in, but no webmail URL is configured")?;
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
                            "To: {}\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{}\n{}",
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
                // This reached page may already have delivered the callback before this duplicate GET.
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

/// Continue an HTTP probe with its already-consumed credential before polling again.
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

            struct ChildGuard(aterm_uds::spawnfd::LaneChild);
            impl Drop for ChildGuard {
                fn drop(&mut self) {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                }
            }
            let _guard = ChildGuard(child);
            let mut cdp = CdpClient::new(cmd_write, out_read);
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
            ("HTTPS://user@mail.example", false),
        ] {
            assert_eq!(super::url_has_embedded_userinfo(url), embedded, "{url}");
        }

        for (url, selected) in [
            ("http://localhost:1777", true),
            ("http://user:password@127.0.0.1:1777?to=a@b", true),
            ("http://remote.example?to=owner@127.0.0.1:1777", false),
            ("http://remote.example#owner@localhost:1777", false),
            ("http://remote.example/path@127.0.0.1:1777", false),
            ("http://[::1]:1777?to=a@b", false),
            ("https://localhost:1777", false),
        ] {
            assert_eq!(super::is_loopback_http_url(url), selected, "{url}");
        }

        for (url, authority, userinfo, target) in [
            ("http://localhost:1777", "localhost:1777", None, "/"),
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
