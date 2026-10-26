// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure-Rust Codex OAuth PKCE browser sign-in, `auth.json` identity reader, token refresh and
//! `/wham/usage` rate-limit reader. Replaces every `codex app-server` subprocess so account
//! renewals, identity checks and pool usage reads run wholly in process over `aterm-http` and
//! write only `<home>/auth.json` (`0600`), never touching a foreign SQLite database.

#![forbid(unsafe_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use aterm_http::{Client, Method, ProxyMode, Trust};
use aterm_json::{Value, json};
use aterm_uds::secret::Secret;
use ring::rand::{SecureRandom as _, SystemRandom};

use super::login_drive::{Approval, Approver, Drove, Handoff, Outcome, https_host, pinned};
use crate::ai::connector::Completion;

/// Per-account credential file inside a Codex login directory (`CODEX_AUTH_PATH`).
pub const AUTH_FILE: &str = "auth.json";

/// OpenAI's OAuth issuer for ChatGPT-backed Codex logins.
pub const DEFAULT_ISSUER: &str = "https://auth.openai.com";

/// Default ChatGPT backend base URL when `config.toml` names no `chatgpt_base_url`.
pub const DEFAULT_CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api";

/// Codex CLI's public OAuth client identifier.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Originator header and query parameter sent with Codex OAuth and usage requests.
pub const ORIGINATOR: &str = "codex_cli_rs";

/// Registered loopback ports for Codex's OAuth redirect URI (`1455` primary, `1457` fallback).
pub const LOOPBACK_PORTS: [u16; 2] = [1455, 1457];

/// Maximum HTTP response body read from OAuth token or usage endpoints.
pub const MESSAGE_MAX: usize = 256 * 1024;

/// Maximum loopback HTTP callback request head read from the local browser redirect.
const CALLBACK_REQ_MAX: usize = 16 * 1024;

/// Refresh an access token proactively once its JWT `exp` is within five minutes.
const PROACTIVE_REFRESH_MARGIN_SECS: i64 = 300;

/// Refresh tokens proactively once `last_refresh` is at least eight days old.
const STALE_REFRESH_SECS: i64 = 8 * 86_400;

const OAUTH_SCOPES: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";

/// OAuth and loopback callback endpoints for a Codex browser sign-in or token refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// Authorization server base URL (`https://auth.openai.com`).
    pub issuer: String,
    /// Token exchange and refresh URL (`https://auth.openai.com/oauth/token`).
    pub token_url: String,
    /// Candidate `127.0.0.1` ports for `/auth/callback`, tried in order.
    pub callback_ports: Vec<u16>,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            issuer: DEFAULT_ISSUER.to_owned(),
            token_url: format!("{DEFAULT_ISSUER}/oauth/token"),
            callback_ports: LOOPBACK_PORTS.to_vec(),
        }
    }
}

/// The ChatGPT account a Codex login names: its email (where present) and plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// `email`.
    pub email: Option<String>,
    /// `planType`, one bounded word.
    pub plan: Option<String>,
}

/// Reads an `account/read`-shaped JSON object: `None` unless it names a ChatGPT account.
#[must_use]
pub fn account_of(result: &Value) -> Option<Account> {
    let account = result.get("account")?;
    (account.get("type")?.as_str()? == "chatgpt").then_some(())?;
    Some(Account {
        email: account
            .get("email")
            .and_then(Value::as_str)
            .map(str::to_owned),
        plan: account
            .get("planType")
            .and_then(Value::as_str)
            .map(|p| super::one_line(p, 32))
            .filter(|p| !p.is_empty()),
    })
}

#[derive(Debug, Default)]
struct JwtClaims {
    email: Option<String>,
    plan: Option<String>,
    account_id: Option<String>,
    exp: Option<i64>,
}

fn base64url_encode(bytes: &[u8]) -> Option<String> {
    let encoded = aterm_codec::base64::encode(bytes).ok()?;
    Some(
        encoded
            .trim_end_matches('=')
            .replace('+', "-")
            .replace('/', "_"),
    )
}

fn base64url_decode(segment: &str) -> Option<Vec<u8>> {
    if segment.is_empty() || segment.bytes().any(|b| b == b'+' || b == b'/') {
        return None;
    }
    let std_b64 = segment.replace('-', "+").replace('_', "/");
    aterm_codec::base64::decode(&std_b64).ok()
}

fn parse_jwt_claims(token: &str) -> Option<JwtClaims> {
    let mut parts = token.split('.');
    let header = parts.next()?;
    let payload = parts.next()?;
    let sig = parts.next()?;
    if header.is_empty() || payload.is_empty() || sig.is_empty() || parts.next().is_some() {
        return None;
    }
    let decoded = base64url_decode(payload)?;
    let claims: Value = aterm_json::from_slice(&decoded).ok()?;
    let email = claims
        .get("email")
        .and_then(Value::as_str)
        .or_else(|| {
            claims
                .get("https://api.openai.com/profile")
                .and_then(|p| p.get("email"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let auth_ns = claims.get("https://api.openai.com/auth");
    let plan = auth_ns
        .and_then(|a| a.get("chatgpt_plan_type"))
        .and_then(Value::as_str)
        .map(|p| super::one_line(p, 32))
        .filter(|p| !p.is_empty());
    let account_id = auth_ns
        .and_then(|a| a.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let exp = claims.get("exp").and_then(|v| {
        v.as_i64().or_else(|| {
            v.as_f64()
                .filter(|f| f.is_finite() && *f >= i64::MIN as f64 && *f < i64::MAX as f64)
                .map(|f| f as i64)
        })
    });
    Some(JwtClaims {
        email,
        plan,
        account_id,
        exp,
    })
}

#[derive(Debug, Clone)]
struct StoredAuth {
    doc: Value,
    id_token: Option<String>,
    access_token: String,
    refresh_token: Option<String>,
    account_id: Option<String>,
    last_refresh_secs: Option<i64>,
}

/// Path to `auth.json` inside `home` (or `home` itself when `home` names a `.json` file).
#[must_use]
pub fn auth_path_in_home(home: &Path) -> PathBuf {
    if home.extension().and_then(|e| e.to_str()) == Some("json") && !home.is_dir() {
        home.to_path_buf()
    } else {
        home.join(AUTH_FILE)
    }
}

fn read_auth_file_bounded(home: &Path) -> Option<String> {
    if std::fs::symlink_metadata(home).is_ok_and(|md| md.file_type().is_symlink()) {
        return None;
    }
    let path = auth_path_in_home(home);
    #[cfg(not(unix))]
    if std::fs::symlink_metadata(&path)
        .ok()?
        .file_type()
        .is_symlink()
    {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options.open(&path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let limit = MESSAGE_MAX as u64;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > limit {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn load_stored_auth(home: &Path) -> Option<StoredAuth> {
    let text = read_auth_file_bounded(home)?;
    let doc: Value = aterm_json::from_str(&text).ok()?;
    if let Some(mode) = doc.get("auth_mode").and_then(Value::as_str)
        && mode != "chatgpt"
        && mode != "chatgptAuthTokens"
    {
        return None;
    }
    let tokens = doc.get("tokens")?.as_object()?;
    let access_token = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?
        .to_owned();
    let id_token = tokens
        .get("id_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let refresh_token = tokens
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let explicit_account_id = tokens
        .get("account_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let jwt_account_id = id_token
        .as_deref()
        .and_then(parse_jwt_claims)
        .and_then(|c| c.account_id)
        .or_else(|| parse_jwt_claims(&access_token).and_then(|c| c.account_id));
    let last_refresh_secs = doc
        .get("last_refresh")
        .and_then(Value::as_str)
        .and_then(aterm_types::rfc3339::parse_utc_fractional);
    Some(StoredAuth {
        doc,
        id_token,
        access_token,
        refresh_token,
        account_id: explicit_account_id.or(jwt_account_id),
        last_refresh_secs,
    })
}

fn write_auth_doc(home: &Path, doc: &Value) -> std::io::Result<()> {
    if std::fs::symlink_metadata(home).is_ok_and(|md| md.file_type().is_symlink()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "refusing to write auth.json through symlink",
        ));
    }
    let dest = if home.extension().and_then(|e| e.to_str()) == Some("json") && !home.is_dir() {
        if let Some(parent) = home.parent().filter(|p| !p.as_os_str().is_empty()) {
            aterm_types::fs_restricted::create_dir_all_unshared(parent, 0o700)?;
        }
        home.to_path_buf()
    } else {
        aterm_types::fs_restricted::ensure_private_dir(home)?;
        home.join(AUTH_FILE)
    };
    if std::fs::symlink_metadata(&dest).is_ok_and(|md| md.file_type().is_symlink()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "refusing to overwrite symlinked auth.json",
        ));
    }
    let mut rendered =
        aterm_json::to_string(doc).map_err(|e| std::io::Error::other(e.to_string()))?;
    rendered.push('\n');
    super::upgrade_models::write_atomic(&dest, &rendered)
}

/// Reads the ChatGPT account stored in `<home>/auth.json` directly from its OAuth JWT claims,
/// spawning no subprocess. Returns `None` when `auth.json` is absent, unreadable, API-key-only,
/// or holds no decodable ChatGPT token.
#[must_use]
pub fn read_account_in_home(home: &Path) -> Option<Account> {
    let stored = load_stored_auth(home)?;
    let id_claims = stored.id_token.as_deref().and_then(parse_jwt_claims);
    let access_claims = parse_jwt_claims(&stored.access_token);
    if id_claims.is_none() && access_claims.is_none() {
        return None;
    }
    let email = id_claims
        .as_ref()
        .and_then(|c| c.email.clone())
        .or_else(|| access_claims.as_ref().and_then(|c| c.email.clone()));
    let plan = id_claims
        .and_then(|c| c.plan)
        .or_else(|| access_claims.and_then(|c| c.plan));
    Some(Account { email, plan })
}

/// Reads the ChatGPT account email from `<home>/auth.json`, if present.
#[must_use]
pub fn read_account_email(home: &Path) -> Option<String> {
    read_account_in_home(home)?.email
}

fn endpoint_authority(url: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = url
        .strip_prefix("https://")
        .map(|r| ("https", r))
        .or_else(|| url.strip_prefix("http://").map(|r| ("http", r)))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty()
        || authority.contains('@')
        || authority.bytes().any(|b| b <= 0x20 || b >= 0x7f)
    {
        return None;
    }
    Some((scheme, authority))
}

fn is_loopback_authority(authority: &str) -> bool {
    if let Some(rest) = authority.strip_prefix("[::1]") {
        return rest.is_empty()
            || rest
                .strip_prefix(':')
                .and_then(|p| p.parse::<u16>().ok())
                .is_some();
    }
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    if !(host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost")) {
        return false;
    }
    port.is_none_or(|p| p.parse::<u16>().is_ok())
}

fn is_loopback_endpoint(url: &str) -> bool {
    endpoint_authority(url).is_some_and(|(_, auth)| is_loopback_authority(auth))
}

fn is_allowed_auth_endpoint(url: &str) -> bool {
    endpoint_authority(url)
        .is_some_and(|(scheme, auth)| scheme == "https" || is_loopback_authority(auth))
}

fn http_client(url: &str, timeout: Duration) -> Client {
    let proxy = if is_loopback_endpoint(url) {
        ProxyMode::Direct
    } else {
        ProxyMode::Environment
    };
    Client::new(Trust::PlatformVerifier, proxy, timeout)
}

fn needs_proactive_refresh(stored: &StoredAuth, now: i64) -> bool {
    if stored.refresh_token.is_none() {
        return false;
    }
    if let Some(exp) = parse_jwt_claims(&stored.access_token).and_then(|c| c.exp)
        && exp <= now.saturating_add(PROACTIVE_REFRESH_MARGIN_SECS)
    {
        return true;
    }
    if let Some(last) = stored.last_refresh_secs
        && now.saturating_sub(last) >= STALE_REFRESH_SECS
    {
        return true;
    }
    false
}

/// Reloads `auth.json` when another process (such as a live `codex-alab` session sharing
/// `CODEX_AUTH_PATH`) rotated the stored access token since `prior` was read, avoiding replay of a
/// single-use OAuth refresh token.
fn reload_updated_disk_auth(home: &Path, prior: &StoredAuth) -> Option<StoredAuth> {
    let disk = load_stored_auth(home)?;
    let same_account = match (prior.account_id.as_deref(), disk.account_id.as_deref()) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    };
    (same_account && disk.access_token != prior.access_token).then_some(disk)
}

fn refresh_stored_auth(
    home: &Path,
    stored: &StoredAuth,
    token_url: &str,
    wait: Duration,
    now: i64,
) -> Option<StoredAuth> {
    if wait.is_zero() || !is_allowed_auth_endpoint(token_url) {
        return None;
    }
    let refresh_token = stored.refresh_token.as_deref()?;
    let body = aterm_json::to_string(&json!({
        "client_id": CLIENT_ID,
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
    }))
    .ok()?;
    let resp = http_client(token_url, wait)
        .request(Method::Post, token_url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("User-Agent", "codex-cli")
        .header("originator", ORIGINATOR)
        .limit(MESSAGE_MAX)
        .backstop(wait)
        .send(body.as_bytes())
        .ok()?;
    if resp.status() != 200 {
        return None;
    }
    let payload: Value = aterm_json::from_slice(resp.body()).ok()?;
    let new_access = payload
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let mut doc = stored.doc.clone();
    {
        let tokens_obj = doc.as_object_mut()?.get_mut("tokens")?.as_object_mut()?;
        tokens_obj.insert(
            "access_token".to_owned(),
            Value::String(new_access.to_owned()),
        );
        let new_id = payload
            .get("id_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        if let Some(new_id) = new_id {
            tokens_obj.insert("id_token".to_owned(), Value::String(new_id.to_owned()));
        }
        if let Some(acc_id) = new_id
            .and_then(parse_jwt_claims)
            .and_then(|c| c.account_id)
            .or_else(|| parse_jwt_claims(new_access).and_then(|c| c.account_id))
        {
            tokens_obj.insert("account_id".to_owned(), Value::String(acc_id));
        }
        if let Some(new_refresh) = payload
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            tokens_obj.insert(
                "refresh_token".to_owned(),
                Value::String(new_refresh.to_owned()),
            );
        }
    }
    if let Some(root) = doc.as_object_mut() {
        let stamp = aterm_types::rfc3339::format_rfc3339(u64::try_from(now.max(0)).unwrap_or(0));
        root.insert("last_refresh".to_owned(), Value::String(stamp));
    }
    write_auth_doc(home, &doc).ok()?;
    load_stored_auth(home)
}

fn read_configured_base_url(config_path: &Path) -> Option<String> {
    let text = super::accounts::read_bounded(config_path, 256 * 1024).ok()??;
    let table: aterm_toml::Value = aterm_toml::from_str(&text).ok()?;
    table
        .get("chatgpt_base_url")
        .and_then(aterm_toml::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(usage_url_from_base)
        .filter(|url| is_allowed_auth_endpoint(url))
}

/// Resolves the ChatGPT usage endpoint URL from `<home>/config.toml` or `<user_home>/.codex/config.toml`.
#[must_use]
pub fn resolve_usage_url(home: &Path, user_home: &Path) -> String {
    let config_dir = if home.extension().and_then(|e| e.to_str()) == Some("json") && !home.is_dir()
    {
        home.parent().unwrap_or(home)
    } else {
        home
    };
    read_configured_base_url(&config_dir.join("config.toml"))
        .or_else(|| read_configured_base_url(&user_home.join(".codex/config.toml")))
        .unwrap_or_else(|| usage_url_from_base(DEFAULT_CHATGPT_BASE_URL))
}

/// Normalizes `base_url` using Codex's backend rules and appends `/wham/usage` or `/api/codex/usage`.
#[must_use]
pub fn usage_url_from_base(base_url: &str) -> String {
    let mut base = base_url.trim().trim_end_matches('/').to_owned();
    if (base.starts_with("https://chatgpt.com") || base.starts_with("https://chat.openai.com"))
        && !base.contains("/backend-api")
    {
        base.push_str("/backend-api");
    }
    if base.contains("/backend-api") {
        format!("{base}/wham/usage")
    } else {
        format!("{base}/api/codex/usage")
    }
}

/// One Codex login's usage as `account/rateLimits/read` or `/wham/usage` answers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    /// The pool's sample of it: its five-hour window as the session, its week as the week, each
    /// share taken up to the next whole percent.
    pub sample: crate::harness::accounts::Sample,
    /// The wall it stands at, where the server says ordinary use is not allowed, names a reached
    /// limit, or shows a full window: the limit's name ([`crate::harness::accounts::SESSION_LIMIT`]
    /// or [`crate::harness::accounts::WEEK_LIMIT`]) and the reset it named.
    pub wall: Option<(&'static str, Option<i64>)>,
}

/// Reads `account/rateLimits/read`'s normalized answer at `now`: `None` where it holds no `rateLimits`.
#[must_use]
pub fn usage_of(result: &Value, now: i64) -> Option<Usage> {
    use crate::ai::codex_limits::{Bucket, Slot, Window};
    use crate::harness::accounts::{SESSION_LIMIT, Sample, WEEK_LIMIT};
    let limits = result.get("rateLimits")?.as_object()?;
    let windows: Vec<Window> = [("primary", Slot::Primary), ("secondary", Slot::Secondary)]
        .into_iter()
        .filter_map(|(key, slot)| {
            let w = limits.get(key)?.as_object()?;
            Some(Window {
                slot,
                used_percent: w.get("usedPercent")?.as_f64()?,
                window_minutes: w.get("windowDurationMins").and_then(Value::as_u64),
                resets_at: w.get("resetsAt").and_then(Value::as_i64),
            })
        })
        .collect();
    // Clamped to 0..=100 first, so the cast neither truncates nor wraps.
    let whole = |share: f64| share.ceil().clamp(0.0, 100.0) as u32;
    let of = |bucket: Bucket| windows.iter().find(|w| w.bucket() == bucket);
    let (session, week) = (of(Bucket::FiveHour), of(Bucket::Week));
    let sample = Sample {
        session_pct: session.map(|w| whole(w.used_percent)),
        session_reset: session.and_then(|w| w.resets_at),
        week_pct: week.map(|w| whole(w.used_percent)),
        week_reset: week.and_then(|w| w.resets_at),
        session_read_at: session.map(|_| now),
        week_read_at: week.map(|_| now),
        at: now,
        by: String::new(),
    };
    let full = [(session, SESSION_LIMIT), (week, WEEK_LIMIT)]
        .into_iter()
        .find_map(|(w, name)| {
            w.filter(|w| w.used_percent >= 100.0)
                .map(|w| (name, w.resets_at))
        });
    let refused = result.get("ordinaryUsageAllowed").and_then(Value::as_bool) == Some(false)
        || limits
            .get("rateLimitReachedType")
            .is_some_and(|t| !t.is_null());
    let wall = full.or_else(|| {
        refused.then(|| {
            let latest = [session, week]
                .into_iter()
                .flatten()
                .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent));
            match latest {
                Some(w) if Some(w) == week => (WEEK_LIMIT, w.resets_at),
                Some(w) => (SESSION_LIMIT, w.resets_at),
                None => (SESSION_LIMIT, None),
            }
        })
    });
    Some(Usage { sample, wall })
}

fn map_backend_window(window: Option<&Value>, now: i64) -> Value {
    let Some(w) = window.and_then(Value::as_object) else {
        return Value::Null;
    };
    let Some(used_percent) = w.get("used_percent").and_then(Value::as_f64) else {
        return Value::Null;
    };
    let duration_mins = w
        .get("limit_window_seconds")
        .and_then(Value::as_i64)
        .filter(|&secs| secs > 0)
        .and_then(|secs| u64::try_from(secs).ok())
        .map(|secs| secs.saturating_add(59) / 60);
    let resets_at = w.get("reset_at").and_then(Value::as_i64).or_else(|| {
        w.get("reset_after_seconds")
            .and_then(Value::as_i64)
            .filter(|&secs| secs >= 0)
            .and_then(|secs| now.checked_add(secs))
    });
    json!({
        "usedPercent": used_percent,
        "windowDurationMins": duration_mins,
        "resetsAt": resets_at,
    })
}

fn map_reached_type(raw: Option<&Value>) -> Value {
    let text = match raw {
        Some(Value::String(s)) => Some(s.as_str()),
        Some(Value::Object(map)) => map
            .get("type")
            .or_else(|| map.get("kind"))
            .and_then(Value::as_str),
        _ => None,
    };
    match text
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "unknown")
    {
        Some(kind) => Value::String(kind.to_owned()),
        None => Value::Null,
    }
}

/// Maps a raw `/wham/usage` or `/api/codex/usage` JSON payload into [`Usage`] via [`usage_of`].
#[must_use]
pub fn usage_from_backend(payload: &Value, now: i64) -> Option<Usage> {
    let obj = payload.as_object()?;
    if !obj.contains_key("rate_limit") && !obj.contains_key("plan_type") {
        return None;
    }
    let rl = obj.get("rate_limit").and_then(Value::as_object);
    let rl_allowed = rl.and_then(|r| {
        r.get("allowed").and_then(Value::as_bool).or_else(|| {
            r.get("limit_reached")
                .and_then(Value::as_bool)
                .map(|lim| !lim)
        })
    });
    let spend_reached = obj
        .get("spend_control")
        .and_then(Value::as_object)
        .and_then(|s| s.get("reached"))
        .and_then(Value::as_bool)
        == Some(true);
    let ordinary_allowed = if spend_reached {
        Some(false)
    } else {
        rl_allowed
    };
    let reached_type = map_reached_type(
        obj.get("rate_limit_reached_type")
            .or_else(|| rl.and_then(|r| r.get("rate_limit_reached_type"))),
    );
    let normalized = json!({
        "ordinaryUsageAllowed": ordinary_allowed,
        "rateLimits": {
            "rateLimitReachedType": reached_type,
            "primary": map_backend_window(rl.and_then(|r| r.get("primary_window")), now),
            "secondary": map_backend_window(rl.and_then(|r| r.get("secondary_window")), now),
        }
    });
    usage_of(&normalized, now)
}

fn get_usage_once(usage_url: &str, stored: &StoredAuth, wait: Duration) -> Option<(u16, Vec<u8>)> {
    if wait.is_zero() || !is_allowed_auth_endpoint(usage_url) {
        return None;
    }
    let client = http_client(usage_url, wait);
    let mut req = client
        .request(Method::Get, usage_url)
        .header("Accept", "application/json")
        .header("User-Agent", "codex-cli")
        .header("originator", ORIGINATOR)
        .secret_header(
            "Authorization",
            Secret::new(format!("Bearer {}", stored.access_token).as_bytes()),
        )
        .limit(MESSAGE_MAX)
        .backstop(wait);
    if let Some(account_id) = stored.account_id.as_deref()
        && account_id.bytes().all(|b| (0x20..0x7f).contains(&b))
    {
        req = req.header("ChatGPT-Account-Id", account_id);
    }
    let resp = req.send(&[]).ok()?;
    Some((resp.status(), resp.body().to_vec()))
}

/// Reads `<home>/auth.json`, refreshes its OAuth token against `token_url` when near/past expiry
/// or rejected with `401`, fetches `usage_url`, and returns [`Usage`] at `now`.
#[must_use]
pub fn read_usage_with(
    home: &Path,
    usage_url: &str,
    token_url: &str,
    wait: Duration,
    now: i64,
) -> Option<Usage> {
    let deadline = Instant::now()
        .checked_add(wait)
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(86_400));
    let mut stored = load_stored_auth(home)?;
    if needs_proactive_refresh(&stored, now) {
        if let Some(reloaded) = reload_updated_disk_auth(home, &stored) {
            stored = reloaded;
        }
        if needs_proactive_refresh(&stored, now) {
            let left = deadline.saturating_duration_since(Instant::now());
            if let Some(refreshed) = refresh_stored_auth(home, &stored, token_url, left, now) {
                stored = refreshed;
            }
        }
    }
    let left = deadline.saturating_duration_since(Instant::now());
    let (mut status, mut body) = get_usage_once(usage_url, &stored, left)?;
    if status == 401
        && let Some(reloaded) = reload_updated_disk_auth(home, &stored)
    {
        stored = reloaded;
        let left = deadline.saturating_duration_since(Instant::now());
        (status, body) = get_usage_once(usage_url, &stored, left)?;
    }
    if status == 401 && stored.refresh_token.is_some() {
        let left = deadline.saturating_duration_since(Instant::now());
        let refreshed = refresh_stored_auth(home, &stored, token_url, left, now)?;
        let left = deadline.saturating_duration_since(Instant::now());
        (status, body) = get_usage_once(usage_url, &refreshed, left)?;
    }
    if !(200..300).contains(&status) {
        return None;
    }
    let payload: Value = aterm_json::from_slice(&body).ok()?;
    usage_from_backend(&payload, now)
}

/// Reads one Codex login's live usage in `home` directly over HTTPS (`/wham/usage` or
/// `/api/codex/usage` as `config.toml` under `home` or `user_home` configures it), refreshing
/// `<home>/auth.json` in place when needed.
#[must_use]
pub fn read_usage_in_home(
    home: &Path,
    user_home: &Path,
    wait: Duration,
    now: i64,
) -> Option<Usage> {
    let usage_url = resolve_usage_url(home, user_home);
    let token_url = format!("{DEFAULT_ISSUER}/oauth/token");
    read_usage_with(home, &usage_url, &token_url, wait, now)
}

fn percent_encode_component(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 0x0f)]));
        }
    }
    out
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn percent_decode_query(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hi = hex_nibble(*bytes.get(i + 1)?)?;
                let lo = hex_nibble(*bytes.get(i + 2)?)?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn parse_query_params(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .filter_map(|part| {
            let (raw_k, raw_v) = part.split_once('=').unwrap_or((part, ""));
            Some((percent_decode_query(raw_k)?, percent_decode_query(raw_v)?))
        })
        .collect()
}

fn encode_form(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (idx, (k, v)) in pairs.iter().enumerate() {
        if idx > 0 {
            out.push('&');
        }
        out.push_str(&percent_encode_component(k));
        out.push('=');
        out.push_str(&percent_encode_component(v));
    }
    out
}

fn build_authorize_url(
    issuer: &str,
    redirect_uri: &str,
    code_challenge: &str,
    state: &str,
) -> String {
    let base = issuer.trim_end_matches('/');
    let query = encode_form(&[
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", redirect_uri),
        ("scope", OAUTH_SCOPES),
        ("code_challenge", code_challenge),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("state", state),
        ("originator", ORIGINATOR),
    ]);
    format!("{base}/oauth/authorize?{query}")
}

fn send_loopback_cancel(port: u16) {
    let Ok(addr) = format!("127.0.0.1:{port}").parse() else {
        return;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(250)) else {
        return;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
    let req =
        format!("GET /cancel HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    let _ = stream.write_all(req.as_bytes());
    let mut scratch = [0u8; 64];
    let _ = stream.read(&mut scratch);
}

fn bind_callback_listener(ports: &[u16], deadline: Instant) -> std::io::Result<TcpListener> {
    let mut last_err = std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "no callback port configured",
    );
    for &port in ports {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => return Ok(listener),
            Err(err) if err.kind() == std::io::ErrorKind::AddrInUse && port != 0 => {
                last_err = err;
                if port == LOOPBACK_PORTS[0] && Instant::now() < deadline {
                    send_loopback_cancel(port);
                    for _ in 0..8 {
                        let nap = deadline
                            .saturating_duration_since(Instant::now())
                            .min(Duration::from_millis(50));
                        if nap.is_zero() {
                            break;
                        }
                        std::thread::sleep(nap);
                        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
                            return Ok(listener);
                        }
                    }
                }
            }
            Err(err) => last_err = err,
        }
    }
    Err(last_err)
}

#[derive(Debug)]
enum CallbackOutcome {
    Code(String),
    Failed,
    Cancelled,
}

fn write_http_reply(stream: &mut TcpStream, status: &str, content_type: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    let mut drain = [0u8; 1024];
    let mut drained = 0usize;
    while drained < 16 * 1024 {
        match stream.read(&mut drain) {
            Ok(0) | Err(_) => break,
            Ok(n) => drained += n,
        }
    }
}

fn is_loopback_host_header(value: &str, port: u16) -> bool {
    let host = value.trim();
    if let Some(rest) = host.strip_prefix("[::1]") {
        return rest.is_empty()
            || rest
                .strip_prefix(':')
                .and_then(|p| p.parse::<u16>().ok())
                .is_some_and(|p| p == port);
    }
    let (name, port_part) = match host.split_once(':') {
        Some((n, p)) => (n, Some(p)),
        None => (host, None),
    };
    if !(name == "127.0.0.1" || name.eq_ignore_ascii_case("localhost")) {
        return false;
    }
    port_part.is_none_or(|p| p.parse::<u16>().ok() == Some(port))
}

fn read_request_target(stream: &mut TcpStream, port: u16) -> Option<String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 512];
    while buf.len() < CALLBACK_REQ_MAX {
        let take = chunk.len().min(CALLBACK_REQ_MAX - buf.len());
        match stream.read(&mut chunk[..take]) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.windows(2).any(|w| w == b"\n\n")
                {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(_) => return None,
        }
    }
    parse_callback_request_target(&buf, port)
}

fn parse_callback_request_target(buf: &[u8], port: u16) -> Option<String> {
    let crlf_end = buf.windows(4).position(|w| w == b"\r\n\r\n");
    let lf_end = buf.windows(2).position(|w| w == b"\n\n");
    let header_end = match (crlf_end, lf_end) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => return None,
    };
    let head = buf.get(..header_end)?;
    let line_end = head.iter().position(|&b| b == b'\r' || b == b'\n')?;
    let line = std::str::from_utf8(&head[..line_end]).ok()?;
    let mut words = line.split_whitespace();
    let method = words.next()?;
    let target = words.next()?;
    let version = words.next()?;
    if method != "GET" || !version.starts_with("HTTP/1.") || words.next().is_some() {
        return None;
    }
    let raw_headers = head
        .get(line_end..)?
        .strip_prefix(b"\r\n")
        .or_else(|| head.get(line_end..)?.strip_prefix(b"\n"))?;
    if raw_headers.is_empty() {
        return None;
    }
    let mut saw_host = false;
    for raw_line in raw_headers.split(|&b| b == b'\n') {
        let raw_line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if raw_line.is_empty() {
            break;
        }
        if raw_line.starts_with(b" ") || raw_line.starts_with(b"\t") || raw_line.contains(&b'\r') {
            return None;
        }
        let colon = raw_line.iter().position(|&b| b == b':')?;
        let name = &raw_line[..colon];
        if name.is_empty() || name.iter().any(u8::is_ascii_whitespace) {
            return None;
        }
        if name.eq_ignore_ascii_case(b"host") {
            if saw_host {
                return None;
            }
            let val = std::str::from_utf8(&raw_line[colon + 1..]).ok()?;
            if !is_loopback_host_header(val, port) {
                return None;
            }
            saw_host = true;
        }
    }
    saw_host.then(|| target.to_owned())
}

fn serve_callback_loop(
    listener: TcpListener,
    port: u16,
    expected_state: String,
    cancelled: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    tx: mpsc::Sender<CallbackOutcome>,
    deadline: Instant,
) {
    let _ = listener.set_nonblocking(true);
    while !cancelled.load(Ordering::SeqCst) && Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let io_wait = deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(2))
                    .max(Duration::from_millis(1));
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(io_wait));
                let _ = stream.set_write_timeout(Some(io_wait));
                let Some(target) = read_request_target(&mut stream, port) else {
                    write_http_reply(&mut stream, "400 Bad Request", "text/plain", "Bad Request");
                    continue;
                };
                let (path, query) = target
                    .split_once('?')
                    .map_or((target.as_str(), ""), |(p, q)| (p, q));
                if path == "/cancel" {
                    write_http_reply(&mut stream, "200 OK", "text/plain", "Cancelled");
                    done.store(true, Ordering::SeqCst);
                    let _ = tx.send(CallbackOutcome::Cancelled);
                    return;
                }
                if path != "/auth/callback" {
                    write_http_reply(&mut stream, "404 Not Found", "text/plain", "Not Found");
                    continue;
                }
                let params = parse_query_params(query);
                let param = |key: &str| {
                    params
                        .iter()
                        .find(|(k, _)| k == key)
                        .map(|(_, v)| v.as_str())
                };
                let state_ok = param("state").is_some_and(|st| {
                    st == expected_state
                        || st.strip_suffix(":life_sciences") == Some(expected_state.as_str())
                });
                if !state_ok {
                    write_http_reply(
                        &mut stream,
                        "400 Bad Request",
                        "text/plain",
                        "State mismatch",
                    );
                    continue;
                }
                if param("error").is_some_and(|e| !e.is_empty()) {
                    write_http_reply(
                        &mut stream,
                        "200 OK",
                        "text/html; charset=utf-8",
                        "<!doctype html><title>Sign-in failed</title><p>Sign-in was not completed.</p>",
                    );
                    done.store(true, Ordering::SeqCst);
                    let _ = tx.send(CallbackOutcome::Failed);
                    return;
                }
                let Some(code) = param("code").filter(|c| !c.is_empty()) else {
                    write_http_reply(
                        &mut stream,
                        "400 Bad Request",
                        "text/plain",
                        "Missing authorization code",
                    );
                    done.store(true, Ordering::SeqCst);
                    let _ = tx.send(CallbackOutcome::Failed);
                    return;
                };
                write_http_reply(
                    &mut stream,
                    "200 OK",
                    "text/html; charset=utf-8",
                    "<!doctype html><title>Signed in</title><p>Sign-in complete. You may close this window.</p>",
                );
                done.store(true, Ordering::SeqCst);
                let _ = tx.send(CallbackOutcome::Code(code.to_owned()));
                return;
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::ConnectionAborted
                ) =>
            {
                if e.kind() == std::io::ErrorKind::WouldBlock {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            Err(_) => {
                done.store(true, Ordering::SeqCst);
                let _ = tx.send(CallbackOutcome::Failed);
                return;
            }
        }
    }
    done.store(true, Ordering::SeqCst);
}

fn exchange_code_and_persist(
    home: &Path,
    token_url: &str,
    redirect_uri: &str,
    code: &str,
    code_verifier: &str,
    wait: Duration,
    now: i64,
) -> Option<Option<String>> {
    if wait.is_zero() || !is_allowed_auth_endpoint(token_url) {
        return None;
    }
    let form = encode_form(&[
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", code_verifier),
    ]);
    let resp = http_client(token_url, wait)
        .request(Method::Post, token_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .header("User-Agent", "codex-cli")
        .header("originator", ORIGINATOR)
        .limit(MESSAGE_MAX)
        .backstop(wait)
        .send(form.as_bytes())
        .ok()?;
    if resp.status() != 200 {
        return None;
    }
    let payload: Value = aterm_json::from_slice(resp.body()).ok()?;
    let id_token = payload
        .get("id_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let access_token = payload
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let refresh_token = payload
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let id_claims = parse_jwt_claims(id_token);
    let access_claims = parse_jwt_claims(access_token);
    if id_claims.is_none() && access_claims.is_none() {
        return None;
    }
    let account_id = id_claims
        .as_ref()
        .and_then(|c| c.account_id.clone())
        .or_else(|| access_claims.as_ref().and_then(|c| c.account_id.clone()));
    let email = id_claims
        .and_then(|c| c.email)
        .or_else(|| access_claims.and_then(|c| c.email));
    let stamp = aterm_types::rfc3339::format_rfc3339(u64::try_from(now.max(0)).unwrap_or(0));
    let doc = json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": Value::Null,
        "tokens": {
            "id_token": id_token,
            "access_token": access_token,
            "refresh_token": refresh_token,
            "account_id": account_id,
        },
        "last_refresh": stamp,
    });
    write_auth_doc(home, &doc).ok()?;
    Some(email)
}

/// Runs Codex's native loopback PKCE browser sign-in against `endpoints`, writing `<home>/auth.json`
/// (`0600`) on completion and returning [`Outcome::SignedIn`] with the ChatGPT email from the
/// issued token. Nothing writes the authorization URL, PKCE verifier or OAuth state to any log or
/// ledger.
pub fn login_with(
    home: &Path,
    endpoints: &Endpoints,
    approver: &mut dyn Approver,
    hosts: &[&str],
    email: Option<&str>,
    deadline: Instant,
) -> Drove {
    let rng = SystemRandom::new();
    let mut verifier_bytes = [0u8; 32];
    let mut state_bytes = [0u8; 32];
    if rng.fill(&mut verifier_bytes).is_err() || rng.fill(&mut state_bytes).is_err() {
        return Drove {
            outcome: Outcome::NotRun("system random number generator failed".to_owned()),
            host: None,
        };
    }
    let (Some(code_verifier), Some(state)) = (
        base64url_encode(&verifier_bytes),
        base64url_encode(&state_bytes),
    ) else {
        return Drove {
            outcome: Outcome::Failed,
            host: None,
        };
    };
    let challenge_digest = aterm_digest::Sha256::digest(code_verifier.as_bytes());
    let Some(code_challenge) = base64url_encode(&challenge_digest) else {
        return Drove {
            outcome: Outcome::Failed,
            host: None,
        };
    };

    let probe_port = endpoints
        .callback_ports
        .first()
        .copied()
        .unwrap_or(LOOPBACK_PORTS[0]);
    let probe_redirect = format!("http://127.0.0.1:{probe_port}/auth/callback");
    let probe_url =
        build_authorize_url(&endpoints.issuer, &probe_redirect, &code_challenge, &state);
    let host = https_host(&probe_url);
    if !pinned(&probe_url, hosts) {
        return Drove {
            outcome: Outcome::OffPin,
            host,
        };
    }
    if Instant::now() >= deadline {
        return Drove {
            outcome: Outcome::Hung,
            host,
        };
    }

    let listener = match bind_callback_listener(&endpoints.callback_ports, deadline) {
        Ok(listener) => listener,
        Err(e) => {
            return Drove {
                outcome: Outcome::NotRun(format!(
                    "could not bind loopback OAuth callback on 127.0.0.1: {e}"
                )),
                host,
            };
        }
    };
    let port = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(e) => {
            return Drove {
                outcome: Outcome::NotRun(format!("could not read loopback callback port: {e}")),
                host,
            };
        }
    };
    let redirect_uri = format!("http://127.0.0.1:{port}/auth/callback");
    let auth_url = build_authorize_url(&endpoints.issuer, &redirect_uri, &code_challenge, &state);
    let host = https_host(&auth_url);
    if !pinned(&auth_url, hosts) {
        return Drove {
            outcome: Outcome::OffPin,
            host,
        };
    }

    let cancelled = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let server_thread = {
        let cancelled = Arc::clone(&cancelled);
        let done = Arc::clone(&done);
        let expected_state = state;
        std::thread::spawn(move || {
            serve_callback_loop(
                listener,
                port,
                expected_state,
                cancelled,
                done,
                tx,
                deadline,
            );
        })
    };

    let finished = || done.load(Ordering::SeqCst);
    let page = match approver.approve(&Handoff {
        url: &auth_url,
        code: None,
        completion: Completion::Loopback,
        hosts,
        email,
        finished: &finished,
        deadline,
    }) {
        Approval::Approved { page } => page,
        Approval::NeedsYou(why) => {
            cancelled.store(true, Ordering::SeqCst);
            let _ = server_thread.join();
            return Drove {
                outcome: Outcome::NeedsYou(why),
                host,
            };
        }
        Approval::NeedsSignIn(why) => {
            cancelled.store(true, Ordering::SeqCst);
            let _ = server_thread.join();
            return Drove {
                outcome: Outcome::NeedsSignIn(why),
                host,
            };
        }
        Approval::Refused(why) => {
            cancelled.store(true, Ordering::SeqCst);
            let _ = server_thread.join();
            return Drove {
                outcome: Outcome::Refused(why),
                host,
            };
        }
    };

    let wait = deadline.saturating_duration_since(Instant::now());
    let callback = rx.recv_timeout(wait);
    cancelled.store(true, Ordering::SeqCst);
    let _ = server_thread.join();

    let code = match callback {
        Ok(CallbackOutcome::Code(code)) => code,
        Ok(CallbackOutcome::Failed | CallbackOutcome::Cancelled) => {
            return Drove {
                outcome: Outcome::Failed,
                host,
            };
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            return Drove {
                outcome: Outcome::Hung,
                host,
            };
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            return Drove {
                outcome: Outcome::Failed,
                host,
            };
        }
    };

    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Drove {
            outcome: Outcome::Hung,
            host,
        };
    }
    let now = crate::supervise::limit::unix_now();
    match exchange_code_and_persist(
        home,
        &endpoints.token_url,
        &redirect_uri,
        &code,
        &code_verifier,
        left,
        now,
    ) {
        Some(signed_in_email) => Drove {
            outcome: Outcome::SignedIn {
                email: signed_in_email,
                page,
            },
            host,
        },
        None => Drove {
            outcome: Outcome::Failed,
            host,
        },
    }
}

/// Runs Codex's native loopback PKCE browser sign-in in `home` against OpenAI's production OAuth
/// endpoints ([`Endpoints::default`]).
pub fn login_in_home(
    home: &Path,
    approver: &mut dyn Approver,
    hosts: &[&str],
    email: Option<&str>,
    deadline: Instant,
) -> Drove {
    login_with(
        home,
        &Endpoints::default(),
        approver,
        hosts,
        email,
        deadline,
    )
}

#[cfg(test)]
#[path = "codex_app_login_tests.rs"]
pub(crate) mod tests;
