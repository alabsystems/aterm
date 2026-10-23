// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Autonomous headless browser OAuth authorizer over Chrome DevTools Protocol (CDP) pipes.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use aterm_json::{Value, json};

use super::open_url::{
    DEFAULT_MAILBOX_DIR, EMAIL_FILE_MAX, MAILBOX_ENV, MailboxFlow, decode_html_amp,
    decode_mime_base64_parts, decode_quoted_printable, email_matches_target, extract_login_hint,
    extract_magic_link_from_email, percent_decode, percent_encode,
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
    /// Webmail inbox URL (e.g. `https://mail.proton.me/u/0/inbox`) polled via headless browser CDP.
    pub webmail_url: Option<String>,
    /// Username / email for IMAP, HTTP Basic, or Webmail authentication.
    pub user: Option<String>,
    /// Default password / app-password / Bridge password for IMAP, HTTP Basic, or Webmail authentication.
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
    /// Create a [`MailboxConfig`] backed by a local Maildir / spool directory.
    #[must_use]
    pub fn from_dir(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let seen_file = Some(dir.join(".mailbox.seen"));
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

    /// Look up the password for `target_email` (falling back to the default `password`).
    #[must_use]
    pub fn password_for(&self, target_email: Option<&str>) -> Option<String> {
        if let Some(email) = target_email {
            let key = email.trim().to_ascii_lowercase();
            if let Some(p) = self.passwords.get(&key) {
                return Some(p.clone());
            }
        }
        self.password.clone()
    }

    /// Look up the second-stage mailbox password for `target_email` (falling back to `mailbox_password`).
    #[must_use]
    pub fn mailbox_password_for(&self, target_email: Option<&str>) -> Option<String> {
        if let Some(email) = target_email {
            let key = email.trim().to_ascii_lowercase();
            if let Some(p) = self.mailbox_passwords.get(&key) {
                return Some(p.clone());
            }
        }
        self.mailbox_password.clone()
    }

    /// Look up the Base32 TOTP secret for `target_email` (falling back to `totp_secret`).
    #[must_use]
    pub fn totp_secret_for(&self, target_email: Option<&str>) -> Option<String> {
        if let Some(email) = target_email {
            let key = email.trim().to_ascii_lowercase();
            if let Some(s) = self.totp_secrets.get(&key) {
                return Some(s.clone());
            }
        }
        self.totp_secret.clone()
    }

    /// Effective webmail inbox URL for `target_email` when headless CDP webmail polling is applicable.
    #[must_use]
    pub fn effective_webmail_url(&self, target_email: Option<&str>) -> Option<String> {
        if let Some(url) = &self.webmail_url {
            return Some(url.clone());
        }
        if self.password_for(target_email).is_some()
            && let Some(email) = target_email
            && let Some((_, domain)) = email.trim().rsplit_once('@')
        {
            let dom = domain.to_ascii_lowercase();
            if matches!(
                dom.as_str(),
                "proton.me" | "protonmail.com" | "protonmail.ch" | "pm.me"
            ) {
                return Some("https://mail.proton.me/u/0/inbox".to_owned());
            }
        }
        None
    }

    /// True when this configuration uses headless webmail only (no Maildir, HTTP API, or IMAP server).
    #[must_use]
    pub fn is_webmail_only(&self, target_email: Option<&str>) -> bool {
        self.dir.is_none()
            && self.http_url.is_none()
            && self.imap_url.is_none()
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

    /// Parse a [`MailboxConfig`] from a TOML string.
    #[must_use]
    pub fn from_toml_str(text: &str, home: Option<&Path>) -> Option<Self> {
        let doc = aterm_toml::from_str::<aterm_toml::Table>(text).ok()?;
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
            let file = std::fs::File::open(path).ok()?;
            let mut s = String::new();
            file.take(4096).read_to_string(&mut s).ok()?;
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
                cfg.http_url.get_or_insert(u);
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
        cfg.mailbox_password = get_str(&["mailbox_password", "second_password"])
            .or_else(|| get_str(&["mailbox_password_file"]).and_then(|p| read_secret_file(&p)));
        cfg.totp_secret = get_str(&["totp_secret", "totp"]).or_else(|| {
            get_str(&["totp_secret_file", "totp_file"]).and_then(|p| read_secret_file(&p))
        });
        cfg.bearer_token = get_str(&["bearer_token", "token"]).or_else(|| {
            get_str(&["token_file", "bearer_token_file"]).and_then(|p| read_secret_file(&p))
        });

        if let Some(pw_table) = table.get("passwords").and_then(aterm_toml::Value::as_table) {
            for (k, v) in pw_table {
                if let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    cfg.passwords
                        .insert(k.trim().to_ascii_lowercase(), s.to_owned());
                }
            }
        }
        if let Some(totp_table) = table.get("totp").and_then(aterm_toml::Value::as_table) {
            for (k, v) in totp_table {
                if let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    cfg.totp_secrets
                        .insert(k.trim().to_ascii_lowercase(), s.to_owned());
                }
            }
        }
        if let Some(accts) = table.get("accounts").and_then(aterm_toml::Value::as_table) {
            for (k, v) in accts {
                let key = k.trim().to_ascii_lowercase();
                if let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    cfg.passwords.insert(key, s.to_owned());
                } else if let Some(sub) = v.as_table() {
                    if let Some(pw) = get_str_in(sub, &["password", "pass"]).or_else(|| {
                        get_str_in(sub, &["password_file", "pass_file"])
                            .and_then(|p| read_secret_file(&p))
                    }) {
                        cfg.passwords.insert(key.clone(), pw);
                    }
                    if let Some(mpw) = get_str_in(sub, &["mailbox_password", "second_password"])
                        .or_else(|| {
                            get_str_in(sub, &["mailbox_password_file"])
                                .and_then(|p| read_secret_file(&p))
                        })
                    {
                        cfg.mailbox_passwords.insert(key.clone(), mpw);
                    }
                    if let Some(totp) = get_str_in(sub, &["totp_secret", "totp"]).or_else(|| {
                        get_str_in(sub, &["totp_secret_file", "totp_file"])
                            .and_then(|p| read_secret_file(&p))
                    }) {
                        cfg.totp_secrets.insert(key, totp);
                    }
                }
            }
        }

        cfg.is_configured().then_some(cfg)
    }
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

    if let Some(val) = non_empty_env(MAILBOX_ENV) {
        if val.starts_with("imap://") || val.starts_with("imaps://") {
            cfg.imap_url = Some(val);
        } else if val.starts_with("http://") || val.starts_with("https://") {
            cfg.http_url = Some(val);
        } else {
            let path = PathBuf::from(&val);
            if path.extension().is_some_and(|ext| ext == "toml")
                && let Ok(text) = std::fs::read_to_string(&path)
                && let Some(parsed) = MailboxConfig::from_toml_str(&text, home)
            {
                cfg.merge_from(parsed);
            } else {
                cfg.dir = Some(path);
            }
        }
    }
    if let Some(url) = non_empty_env(MAILBOX_URL_ENV) {
        if url.starts_with("imap://") || url.starts_with("imaps://") {
            cfg.imap_url = Some(url);
        } else if url.starts_with("http://") || url.starts_with("https://") {
            cfg.http_url = Some(url);
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
        cfg.totp_secret = Some(totp);
    }
    if let Some(tok) = non_empty_env(MAILBOX_TOKEN_ENV) {
        cfg.bearer_token = Some(tok);
    }

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
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
    {
        toml_candidates.push(xdg.join("aterm").join(MAILBOX_TOML_FILE));
        toml_candidates.push(xdg.join("aterm/aterm.toml"));
    }
    for path in toml_candidates {
        if path.is_file()
            && let Ok(text) = std::fs::read_to_string(&path)
            && let Some(parsed) = MailboxConfig::from_toml_str(&text, home)
        {
            cfg.merge_from(parsed);
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
            .or_else(|| cfg.dir.as_ref().map(|d| d.join(".mailbox.seen")));
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

/// Whether `store_dir` already has a persistent headless browser profile for `label` or `email`.
#[must_use]
pub fn has_browser_profile(store_dir: &Path, label: &str, email: Option<&str>) -> bool {
    if browser_profile_dir(store_dir, Some(label)).is_dir() {
        return true;
    }
    email.is_some_and(|em| browser_profile_dir(store_dir, Some(em)).is_dir())
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
    if std::env::consts::OS == "linux" {
        args.push("--no-sandbox".into());
    }
    args.push("about:blank".into());
    args
}

fn strip_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        if ch == '<' {
            in_tag = true;
            out.push(' ');
        } else if ch == '>' {
            in_tag = false;
            out.push(' ');
        } else if !in_tag {
            out.push(ch);
        }
    }
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
        // Avoid matching common 6-digit dates/postal/color fragments when preceded by `#` or `:`
        if token.starts_with('#') || token.starts_with(':') {
            return None;
        }
        return Some(cleaned.to_owned());
    }
    None
}

/// Extract a 6-digit verification code (OTP) from an incoming email addressed to `target_email`.
#[must_use]
pub fn extract_otp_from_email(raw_email: &str, target_email: Option<&str>) -> Option<String> {
    if let Some(target) = target_email
        && !email_matches_target(raw_email, target)
    {
        return None;
    }
    let mut extra_text = decode_mime_base64_parts(raw_email);
    if let Ok(doc) = aterm_json::from_str::<Value>(raw_email)
        && let Some(obj) = doc.as_object()
    {
        for (k, v) in obj {
            if matches!(
                k.to_ascii_lowercase().as_str(),
                "otp" | "code" | "verification_code" | "login_code"
            ) && let Some(c) = v.as_str()
                && let Some(otp) = is_six_digit_otp(c.trim())
            {
                return Some(otp);
            }
            if matches!(
                k.to_ascii_lowercase().as_str(),
                "body" | "html" | "text" | "raw" | "snippet" | "subject"
            ) && let Some(s) = v.as_str()
            {
                extra_text.push('\n');
                extra_text.push_str(s);
                extra_text.push_str(&decode_mime_base64_parts(s));
            }
        }
    }
    let body_section = raw_email
        .split_once("\r\n\r\n")
        .or_else(|| raw_email.split_once("\n\n"))
        .map_or(raw_email, |(_, b)| b);
    let qp_decoded = decode_quoted_printable(body_section);
    let combined = if extra_text.is_empty() {
        qp_decoded
    } else {
        format!("{qp_decoded}\n{extra_text}")
    };
    let plain = strip_html_tags(&decode_html_amp(&combined));
    let mut fallback = None;
    for line in plain.lines() {
        let lower = line.to_ascii_lowercase();
        let has_context = lower.contains("code")
            || lower.contains("verify")
            || lower.contains("verification")
            || lower.contains("sign in")
            || lower.contains("login")
            || lower.contains("one-time");
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.len() == 1
            && let Some(otp) = is_six_digit_otp(words[0])
        {
            return Some(otp);
        }
        for word in words {
            if let Some(otp) = is_six_digit_otp(word) {
                if has_context {
                    return Some(otp);
                }
                if fallback.is_none() {
                    fallback = Some(otp);
                }
            }
        }
    }
    fallback
}

/// Extract either a magic link or a 6-digit OTP from `raw_email` addressed to `target_email`.
#[must_use]
pub fn extract_credential_from_email(
    raw_email: &str,
    target_email: Option<&str>,
) -> Option<MailboxCredential> {
    extract_magic_link_from_email(raw_email, target_email)
        .map(MailboxCredential::MagicLink)
        .or_else(|| extract_otp_from_email(raw_email, target_email).map(MailboxCredential::Otp))
}

fn fnv1a_hex(input: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in input.as_bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
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
        if trimmed.len() >= ok_prefix.len()
            && trimmed[..ok_prefix.len()].eq_ignore_ascii_case(&ok_prefix)
        {
            return Some((true, lines));
        }
        if (trimmed.len() >= no_prefix.len()
            && trimmed[..no_prefix.len()].eq_ignore_ascii_case(&no_prefix))
            || (trimmed.len() >= bad_prefix.len()
                && trimmed[..bad_prefix.len()].eq_ignore_ascii_case(&bad_prefix))
        {
            return Some((false, lines));
        }
        lines.push(line);
    }
    None
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

    let mut seqs: Vec<u32> = Vec::new();
    for line in &search_lines {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("* SEARCH")
            .or_else(|| trimmed.strip_prefix("* search"))
        {
            for tok in rest.split_whitespace() {
                if let Ok(seq) = tok.parse::<u32>() {
                    seqs.push(seq);
                }
            }
        }
    }

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
        let start = usize::from(
            msg_lines
                .first()
                .is_some_and(|l| l.trim_start().starts_with("* ")),
        );
        let end = if msg_lines.last().is_some_and(|l| l.trim() == ")") && msg_lines.len() > start {
            msg_lines.len() - 1
        } else {
            msg_lines.len()
        };
        let raw_msg: String = msg_lines[start..end].concat();
        if raw_msg.trim().is_empty() {
            continue;
        }
        let digest = fnv1a_hex(&raw_msg);
        if seen.contains(&digest) {
            continue;
        }
        if let Some(cred) = extract_credential_from_email(&raw_msg, target_email) {
            let store_cmd = format!("s{idx} STORE {seq} +FLAGS (\\Seen)\r\na9 LOGOUT\r\n");
            let _ = writer.write_all(store_cmd.as_bytes());
            let _ = writer.flush();
            record_seen_digest(seen_file, seen, &digest);
            return Some(cred);
        }
    }

    let _ = writer.write_all(b"a9 LOGOUT\r\n");
    let _ = writer.flush();
    None
}

fn fetch_imaps_curl_credential(
    parsed: &ParsedImapUrl,
    user: &str,
    password: &str,
    target_email: Option<&str>,
    seen_file: Option<&Path>,
    seen: &mut BTreeSet<String>,
) -> Option<MailboxCredential> {
    let userpwd = format!("{user}:{password}");
    let search_url = format!(
        "imaps://{}:{}/{}?ALL",
        parsed.host,
        parsed.port,
        percent_encode(&parsed.mailbox)
    );
    let mut search_cmd = aterm_uds::spawnfd::command("curl");
    search_cmd.args([
        "--disable",
        "--globoff",
        "--proto",
        "=imaps",
        "-sS",
        "--max-time",
        "10",
        "-u",
        &userpwd,
        &search_url,
    ]);
    let out = search_cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let search_stdout = String::from_utf8_lossy(&out.stdout);
    let mut seqs: Vec<u32> = Vec::new();
    for line in search_stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("* SEARCH")
            .or_else(|| trimmed.strip_prefix("* search"))
        {
            for tok in rest.split_whitespace() {
                if let Ok(seq) = tok.parse::<u32>() {
                    seqs.push(seq);
                }
            }
        }
    }
    for seq in seqs.into_iter().rev().take(8) {
        let msg_url = format!(
            "imaps://{}:{}/{};MAILINDEX={seq}",
            parsed.host,
            parsed.port,
            percent_encode(&parsed.mailbox)
        );
        let mut msg_cmd = aterm_uds::spawnfd::command("curl");
        msg_cmd.args([
            "--disable",
            "--globoff",
            "--proto",
            "=imaps",
            "-sS",
            "--max-time",
            "10",
            "-u",
            &userpwd,
            &msg_url,
        ]);
        let Ok(msg_out) = msg_cmd.output() else {
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
        if let Some(cred) = extract_credential_from_email(&raw_msg, target_email) {
            record_seen_digest(seen_file, seen, &digest);
            return Some(cred);
        }
    }
    None
}

fn fetch_imap_credential(
    cfg: &MailboxConfig,
    target_email: Option<&str>,
    seen: &mut BTreeSet<String>,
) -> Option<MailboxCredential> {
    let imap_url = cfg.imap_url.as_deref()?;
    let parsed = parse_imap_url(imap_url)?;
    let user = parsed
        .user
        .as_deref()
        .or(cfg.user.as_deref())
        .or(target_email)
        .unwrap_or("user");
    let password = parsed
        .password
        .as_deref()
        .or(cfg.password.as_deref())
        .unwrap_or("");
    if parsed.tls {
        fetch_imaps_curl_credential(
            &parsed,
            user,
            password,
            target_email,
            cfg.seen_file.as_deref(),
            seen,
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
        )
    }
}

fn fetch_loopback_http_body(url: &str, bearer_token: Option<&str>) -> Option<String> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path_and_query) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let (_, port_str) = authority.rsplit_once(':')?;
    let port: u16 = port_str.parse().ok()?;
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let mut req = format!(
        "GET {path_and_query} HTTP/1.1\r\nHost: {authority}\r\nAccept: application/json, message/rfc822, text/plain\r\nConnection: close\r\n"
    );
    if let Some(token) = bearer_token.filter(|t| !t.is_empty()) {
        req.push_str(&format!("Authorization: Bearer {token}\r\n"));
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

fn fetch_http_mailbox_credential(
    cfg: &MailboxConfig,
    target_email: Option<&str>,
    seen: &mut BTreeSet<String>,
) -> Option<MailboxCredential> {
    let raw_url = cfg.http_url.as_deref()?;
    let url = if raw_url.contains("{email}") {
        raw_url.replace("{email}", &percent_encode(target_email.unwrap_or("")))
    } else {
        raw_url.to_owned()
    };
    let body = if url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:") {
        fetch_loopback_http_body(&url, cfg.bearer_token.as_deref())?
    } else if url.starts_with("https://") || url.starts_with("http://") {
        let mut cmd = aterm_uds::spawnfd::command("curl");
        cmd.args([
            "--disable",
            "--globoff",
            "--proto",
            "=http,https",
            "-sS",
            "--max-time",
            "10",
        ]);
        if let Some(token) = cfg.bearer_token.as_deref().filter(|t| !t.is_empty()) {
            cmd.args(["-H", &format!("Authorization: Bearer {token}")]);
        } else if let (Some(u), Some(p)) = (cfg.user.as_deref(), cfg.password.as_deref()) {
            cmd.args(["-u", &format!("{u}:{p}")]);
        }
        cmd.arg(&url);
        let out = cmd.output().ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8_lossy(&out.stdout).into_owned()
    } else {
        return None;
    };

    if body.trim().is_empty() {
        return None;
    }

    if let Ok(doc) = aterm_json::from_str::<Value>(&body) {
        let items_opt = doc.as_array().or_else(|| {
            let obj = doc.as_object()?;
            ["messages", "items", "emails", "data"]
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
                if let Some(cred) = extract_credential_from_email(&item_str, target_email) {
                    record_seen_digest(cfg.seen_file.as_deref(), seen, &digest);
                    return Some(cred);
                }
            }
            return None;
        }
    }

    let digest = fnv1a_hex(&body);
    if seen.contains(&digest) {
        return None;
    }
    let cred = extract_credential_from_email(&body, target_email)?;
    record_seen_digest(cfg.seen_file.as_deref(), seen, &digest);
    Some(cred)
}

fn scan_maildir_once(
    mailbox_dir: &Path,
    target_email: Option<&str>,
    tried: &mut BTreeSet<(PathBuf, u64, std::time::SystemTime)>,
) -> Option<MailboxCredential> {
    let entries = std::fs::read_dir(mailbox_dir).ok()?;
    let mut candidates: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
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
            let clicked_marker = mailbox_dir.join(format!("{name_str}.clicked"));
            if clicked_marker.exists() {
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
        })
        .collect();
    candidates.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (mtime, len, path) in candidates {
        tried.insert((path.clone(), len, mtime));
        let mut raw = String::new();
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        if file.take(EMAIL_FILE_MAX).read_to_string(&mut raw).is_err() {
            continue;
        }
        if let Some(found) = extract_credential_from_email(&raw, target_email) {
            if let Some(name) = path.file_name() {
                let marker = mailbox_dir.join(format!("{}.clicked", name.to_string_lossy()));
                let _ = std::fs::write(marker, b"1\n");
            }
            return Some(found);
        }
    }
    None
}

/// Poll `cfg` for either a magic link or a 6-digit OTP addressed to `target_email`.
pub fn poll_mailbox_config_for_credential(
    cfg: &MailboxConfig,
    target_email: Option<&str>,
    timeout: Duration,
) -> Result<MailboxCredential, &'static str> {
    if !cfg.is_configured() {
        return Err("headless browser requires sign-in, but no mailbox is configured");
    }
    let deadline = Instant::now() + timeout;
    let mut tried_files = BTreeSet::new();
    let mut seen_digests = load_seen_digests(cfg.seen_file.as_deref());
    let mut last_network_poll: Option<Instant> = None;
    let network_interval = Duration::from_millis(150);

    loop {
        if let Some(dir) = cfg.dir.as_deref()
            && let Some(cred) = scan_maildir_once(dir, target_email, &mut tried_files)
        {
            return Ok(cred);
        }
        let should_poll_net =
            last_network_poll.is_none_or(|last| last.elapsed() >= network_interval);
        if should_poll_net {
            if cfg.imap_url.is_some() || cfg.http_url.is_some() {
                last_network_poll = Some(Instant::now());
            }
            if let Some(cred) = fetch_imap_credential(cfg, target_email, &mut seen_digests) {
                return Ok(cred);
            }
            if let Some(cred) = fetch_http_mailbox_credential(cfg, target_email, &mut seen_digests)
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
}

/// Build the JavaScript expression evaluated inside the OAuth page to inspect and advance the flow.
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
  const href = String(window.location && window.location.href || "");
  if (href.startsWith("http://127.0.0.1:") || href.startsWith("http://localhost:")) {{
    return {{ state: "callback", url: href }};
  }}
  const setNativeValue = (el, val) => {{
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
  const buttons = Array.from(document.querySelectorAll("button, input[type='submit'], [role='button'], a[href]"));
  for (const btn of buttons) {{
    if (btn.disabled) continue;
    const text = String(btn.innerText || btn.value || btn.getAttribute("aria-label") || "").trim().toLowerCase();
    const hrefAttr = String(btn.getAttribute("href") || "");
    const isCancel = text.includes("cancel") || text.includes("deny") || text.includes("decline") || text.includes("reject");
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
  const otpInput = document.querySelector("input[autocomplete='one-time-code'], input[name*='code'], input[name*='otp'], input[inputmode='numeric']");
  const otpVal = {otp_json};
  const otpDone = {otp_submitted};
  if (otpInput) {{
    if (otpVal && !otpDone) {{
      setNativeValue(otpInput, otpVal);
      const form = otpInput.closest("form");
      const submitBtn = (form && form.querySelector("button[type='submit'], input[type='submit'], button:not([type='button'])"))
        || buttons.find(b => /verify|continue|submit|sign in|log in/i.test(String(b.innerText || b.value || "")));
      if (submitBtn) {{
        submitBtn.click();
      }} else if (form) {{
        form.submit();
      }}
      return {{ state: "submitted_otp", url: href }};
    }}
    return {{ state: "needs_mailbox", url: href }};
  }}
  const emailInput = document.querySelector("input[type='email'], input[name='email'], input[autocomplete*='email'], input[autocomplete='username']");
  const emailVal = {email_json};
  const emailDone = {email_submitted};
  if (emailInput && emailVal && !emailDone) {{
    setNativeValue(emailInput, emailVal);
    const form = emailInput.closest("form");
    const submitBtn = (form && form.querySelector("button[type='submit'], input[type='submit'], button:not([type='button'])"))
      || buttons.find(b => /continue|sign in|log in|next|send/i.test(String(b.innerText || b.value || "")));
    if (submitBtn) {{
      submitBtn.click();
    }} else if (form) {{
      form.submit();
    }}
    return {{ state: "submitted_email", url: href }};
  }}
  const bodyText = String(document.body && document.body.innerText || "").toLowerCase();
  if (bodyText.includes("check your email") || bodyText.includes("magic link") || bodyText.includes("sent a link") || bodyText.includes("verification code")) {{
    return {{ state: "needs_mailbox", url: href }};
  }}
  return {{ state: "loading", url: href }};
}})()"#
    )
}

/// Decode an RFC 4648 Base32 secret (case-insensitive, ignoring spaces, hyphens, and `=` padding).
#[must_use]
pub fn base32_decode(secret: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0u8;
    for ch in secret.chars() {
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

/// Generate a 6-digit RFC 6238 TOTP code for `secret_base32` at Unix timestamp `unix_secs`.
#[must_use]
pub fn generate_totp_at(secret_base32: &str, unix_secs: u64) -> Option<String> {
    let key = base32_decode(secret_base32)?;
    let counter = (unix_secs / 30).to_be_bytes();
    let mac = hmac_sha1(&key, &counter);
    let offset = (mac[19] & 0x0f) as usize;
    let code = u32::from_be_bytes([
        mac[offset] & 0x7f,
        mac[offset + 1],
        mac[offset + 2],
        mac[offset + 3],
    ]) % 1_000_000;
    Some(format!("{code:06}"))
}

fn webmail_step_js(
    email: Option<&str>,
    password: Option<&str>,
    mailbox_password: Option<&str>,
    totp_code: Option<&str>,
    login_submitted: bool,
    totp_submitted: bool,
    mailbox_pw_submitted: bool,
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
  const buttons = Array.from(document.querySelectorAll("button, input[type='submit']"));
  const clickSubmit = (el) => {{
    const form = el && el.closest("form");
    const btn = (form && form.querySelector("button[type='submit'], input[type='submit'], button:not([type='button'])"))
      || buttons.find(b => !b.disabled && /sign in|log in|authenticate|unlock|verify|continue|submit/i.test(String(b.innerText || b.value || "")));
    if (btn) {{
      btn.click();
    }} else if (form) {{
      form.submit();
    }}
  }};

  const userEl = document.querySelector("input#username, input[name='username'], input[autocomplete='username'], input[type='email']");
  const passEl = document.querySelector("input#password, input[name='password'], input[type='password']:not(#mailboxPassword)");
  const emailVal = {email_json};
  const pwVal = {pw_json};
  if (passEl && !{login_submitted}) {{
    if (!pwVal) {{
      return {{ state: "needs_password", url: href }};
    }}
    if (userEl && emailVal) {{
      setNativeValue(userEl, emailVal);
    }}
    setNativeValue(passEl, pwVal);
    clickSubmit(passEl);
    return {{ state: "submitted_login", url: href }};
  }}

  const totpEl = document.querySelector("input#twoFa, input[name='totp'], input[autocomplete='one-time-code'], input[id*='totp' i]");
  const totpVal = {totp_json};
  if (totpEl && totpVal && !{totp_submitted}) {{
    setNativeValue(totpEl, totpVal);
    clickSubmit(totpEl);
    return {{ state: "submitted_totp", url: href }};
  }}

  const mpwEl = document.querySelector("input#mailboxPassword, input[name='mailboxPassword']");
  const mpwVal = {mpw_json} || pwVal;
  if (mpwEl && mpwVal && !{mailbox_pw_submitted}) {{
    setNativeValue(mpwEl, mpwVal);
    clickSubmit(mpwEl);
    return {{ state: "submitted_mailbox_password", url: href }};
  }}

  let htmlParts = [];
  let textParts = [];
  if (document.body) {{
    htmlParts.push(String(document.body.innerHTML || ""));
    textParts.push(String(document.body.innerText || ""));
  }}
  const iframes = Array.from(document.querySelectorAll("iframe"));
  for (const frame of iframes) {{
    try {{
      const doc = frame.contentDocument;
      if (doc && doc.body) {{
        htmlParts.push(String(doc.body.innerHTML || ""));
        textParts.push(String(doc.body.innerText || ""));
      }}
    }} catch (_) {{}}
  }}

  const rows = Array.from(document.querySelectorAll("[data-testid*='message-item'], [data-shortcut-target='item-container'], .item-container, [role='listitem'], [role='row'], tr"));
  let clickedRow = false;
  for (const row of rows) {{
    const rt = String(row.innerText || "").toLowerCase();
    if (rt.includes("claude") || rt.includes("anthropic") || rt.includes("sign in") || rt.includes("verification") || rt.includes("login")) {{
      row.click();
      clickedRow = true;
      break;
    }}
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
pub fn poll_webmail_via_cdp(
    cdp: &mut CdpClient,
    cfg: &MailboxConfig,
    target_email: Option<&str>,
    timeout: Duration,
    poll_interval: Duration,
    seen_digests: &mut BTreeSet<String>,
) -> Result<MailboxCredential, &'static str> {
    let webmail_url = cfg
        .effective_webmail_url(target_email)
        .ok_or("headless browser requires sign-in, but no webmail URL is configured")?;
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

        let mut login_submitted = false;
        let mut totp_submitted = false;
        let mut mailbox_pw_submitted = false;

        for _ in 0..80 {
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
                target_email,
                password.as_deref(),
                mailbox_password.as_deref(),
                totp_code.as_deref(),
                login_submitted,
                totp_submitted,
                mailbox_pw_submitted,
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
            match state {
                "needs_password" => {
                    return Err(
                        "webmail sign-in requires a password in mailbox.toml or ai-mailbox.toml",
                    );
                }
                "submitted_login" => {
                    login_submitted = true;
                }
                "submitted_totp" => {
                    totp_submitted = true;
                }
                "submitted_mailbox_password" => {
                    mailbox_pw_submitted = true;
                }
                _ => {
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
                            && let Some(cred) =
                                extract_credential_from_email(&synthetic, target_email)
                        {
                            record_seen_digest(cfg.seen_file.as_deref(), seen_digests, &digest);
                            return Ok(cred);
                        }
                    }
                }
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
pub fn poll_mailbox_or_webmail_for_credential(
    cdp: &mut CdpClient,
    cfg: &MailboxConfig,
    target_email: Option<&str>,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<MailboxCredential, &'static str> {
    if cfg.is_webmail_only(target_email) {
        let mut seen_digests = load_seen_digests(cfg.seen_file.as_deref());
        return poll_webmail_via_cdp(
            cdp,
            cfg,
            target_email,
            timeout,
            poll_interval,
            &mut seen_digests,
        );
    }
    if cfg.dir.is_some() || cfg.imap_url.is_some() || cfg.http_url.is_some() {
        return poll_mailbox_config_for_credential(cfg, target_email, timeout);
    }
    if cfg.effective_webmail_url(target_email).is_some() {
        let mut seen_digests = load_seen_digests(cfg.seen_file.as_deref());
        return poll_webmail_via_cdp(
            cdp,
            cfg,
            target_email,
            timeout,
            poll_interval,
            &mut seen_digests,
        );
    }
    Err("headless browser requires sign-in, but no mailbox is configured")
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

/// Drive an active CDP session (`cdp`) through the OAuth flow for `authorize_url`, passing `&mut CdpClient`
/// to `mailbox_credential` so webmail inboxes can be polled in a secondary CDP tab.
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
    let mut mailbox_polled = false;

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

            if state == "callback" || flow.is_callback(url) {
                let cb_url = if flow.is_callback(url) {
                    url
                } else {
                    return Err("headless browser reached an invalid local callback URL");
                };
                let _ = deliver_callback(cb_url);
                return Ok(());
            }
            if flow.is_callback(target_attr) {
                let status = deliver_callback(target_attr)?;
                if (200..400).contains(&status) {
                    return Ok(());
                }
                return Err("the local OAuth callback listener rejected the authorization code");
            }

            match state {
                "clicked_consent" => {
                    if !poll_interval.is_zero() {
                        std::thread::sleep(poll_interval);
                    }
                }
                "submitted_email" => {
                    email_submitted = true;
                    if !mailbox_polled {
                        mailbox_polled = true;
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
                            }
                        }
                    }
                }
                "needs_mailbox" => {
                    if !mailbox_polled {
                        mailbox_polled = true;
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
                            }
                        }
                    } else if !poll_interval.is_zero() {
                        std::thread::sleep(poll_interval);
                    }
                }
                "submitted_otp" => {
                    otp_submitted = true;
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
#[cfg(unix)]
pub fn authorize_with_headless_browser_config(
    authorize_url: &str,
    store_dir: Option<&Path>,
    mailbox_cfg: Option<&MailboxConfig>,
    target_email: Option<&str>,
    timeout: Duration,
) -> Result<(), &'static str> {
    use aterm_uds::spawnfd::{ChildStdio, LaneCommand, spawn_with_two_fds};
    use std::os::fd::OwnedFd;

    let _flow = MailboxFlow::new(authorize_url)?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let browser = find_headless_browser(home.as_deref())
        .ok_or("no headless browser found (run `targo --unverified verify provision chrome-headless-shell`)")?;

    let email = target_email
        .filter(|e| !e.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| extract_login_hint(authorize_url));

    let temp_dir;
    let profile_dir = if let Some(store) = store_dir {
        let dir = browser_profile_dir(store, email.as_deref());
        let _ = aterm_types::fs_restricted::ensure_private_dir(dir.parent().unwrap_or(store));
        let _ = aterm_types::fs_restricted::ensure_private_dir(&dir);
        dir
    } else {
        temp_dir =
            aterm_tempfile::tempdir().map_err(|_| "could not create scratch browser profile")?;
        temp_dir.path().to_path_buf()
    };

    let (cmd_read, cmd_write) = std::io::pipe().map_err(|_| "could not open CDP command pipe")?;
    let (out_read, out_write) = std::io::pipe().map_err(|_| "could not open CDP event pipe")?;

    let mut cmd = LaneCommand::new(&browser.path);
    cmd.args(headless_launch_args(&browser, &profile_dir))
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

    drive_cdp_oauth_with_webmail(
        &mut cdp,
        authorize_url,
        email.as_deref(),
        timeout,
        Duration::from_millis(200),
        |cdp_inner, em| {
            let mb = mailbox_cfg
                .filter(|c| c.is_configured())
                .ok_or("headless browser requires sign-in, but no mailbox is configured")?;
            poll_mailbox_or_webmail_for_credential(
                cdp_inner,
                mb,
                em,
                timeout,
                Duration::from_millis(200),
            )
        },
        |cb_url| {
            let mut cookies = BTreeMap::new();
            super::open_url::loopback_http_exchange("GET", cb_url, &mut cookies, None)
                .map(|r| r.status)
        },
    )
}

/// Non-Unix stub for [`authorize_with_headless_browser_config`].
#[cfg(not(unix))]
pub fn authorize_with_headless_browser_config(
    _authorize_url: &str,
    _store_dir: Option<&Path>,
    _mailbox_cfg: Option<&MailboxConfig>,
    _target_email: Option<&str>,
    _timeout: Duration,
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
