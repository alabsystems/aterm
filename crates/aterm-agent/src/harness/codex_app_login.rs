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

/// Maximum OAuth or usage HTTP response body, in bytes.
pub const MESSAGE_MAX: usize = 256 * 1024;

/// Maximum credential file read size, in bytes. A document can retain tokens
/// from several separately bounded responses alongside independent metadata.
/// This bounds reads without rejecting a publication after token rotation.
const AUTH_FILE_MAX: usize = 2 * 1024 * 1024;

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

#[derive(Debug, Clone, Default)]
struct JwtClaims {
    email: Option<String>,
    plan: Option<String>,
    account_id: Option<String>,
    user_id: Option<String>,
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
    if segment.bytes().any(|b| b == b'-' || b == b'_') {
        let mut std_b64 = String::with_capacity(segment.len());
        for ch in segment.chars() {
            std_b64.push(match ch {
                '-' => '+',
                '_' => '/',
                other => other,
            });
        }
        aterm_codec::base64::decode(&std_b64).ok()
    } else {
        aterm_codec::base64::decode(segment).ok()
    }
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
    let claims = claims.as_object()?;
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
    let user_id = auth_ns
        .and_then(|a| {
            ["chatgpt_user_id", "user_id"].iter().find_map(|key| {
                a.get(key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
        })
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
        user_id,
        exp,
    })
}

#[derive(PartialEq, Eq)]
enum TokenUser {
    Unknown,
    Known(String),
    Conflicting,
}

fn token_user_claims(
    id_claims: Option<&JwtClaims>,
    access_claims: Option<&JwtClaims>,
) -> TokenUser {
    let id_user = id_claims.and_then(|c| c.user_id.as_deref());
    let access_user = access_claims.and_then(|c| c.user_id.as_deref());
    match (id_user, access_user) {
        (Some(a), Some(b)) if a != b => TokenUser::Conflicting,
        (Some(user), _) | (_, Some(user)) => TokenUser::Known(user.to_owned()),
        _ => TokenUser::Unknown,
    }
}

fn token_user(id_token: Option<&str>, access_token: &str) -> TokenUser {
    let id_claims = id_token.and_then(parse_jwt_claims);
    let access_claims = parse_jwt_claims(access_token);
    token_user_claims(id_claims.as_ref(), access_claims.as_ref())
}

#[derive(Debug, Clone)]
struct StoredAuth {
    doc: Value,
    id_token: Option<String>,
    access_token: String,
    refresh_token: Option<String>,
    account_id: Option<String>,
    last_refresh_secs: Option<i64>,
    id_claims: Option<JwtClaims>,
    access_claims: Option<JwtClaims>,
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

/// `auth_file`'s lock file: its name with [`LOCK_SUFFIX`] after it, in the same directory.
#[must_use]
pub fn lock_path(auth_file: &Path) -> PathBuf {
    let mut name = auth_file
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_else(|| AUTH_FILE.into());
    name.push(LOCK_SUFFIX);
    auth_file.with_file_name(name)
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
    let limit = AUTH_FILE_MAX as u64;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > limit {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn read_stored_auth(home: &Path) -> Option<StoredAuth> {
    let text = read_auth_file_bounded(home)?;
    let doc: Value = aterm_json::from_str(&text).ok()?;
    stored_auth_from_doc(doc)
}

fn conflicting_users(stored: &StoredAuth) -> bool {
    token_user_claims(stored.id_claims.as_ref(), stored.access_claims.as_ref())
        == TokenUser::Conflicting
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CodexStoredIdentity",
        action = "Read",
        project = "aterm_agent::harness::codex_app_login::tests::stored_identity_tests::stored_identity_transition"
    )
)]
fn load_stored_auth(home: &Path) -> Option<StoredAuth> {
    read_stored_auth(home).filter(|stored| !conflicting_users(stored))
}

fn check_stored_identity(stored: &StoredAuth, expected_email: Option<&str>) -> std::io::Result<()> {
    if conflicting_users(stored) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Codex login tokens name different users; sign in again",
        ));
    }
    if let Some(expected) = expected_email {
        for email in [&stored.id_claims, &stored.access_claims]
            .into_iter()
            .filter_map(|claims| claims.as_ref()?.email.as_deref())
        {
            if !super::accounts::email_matches(expected, email) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Codex login does not match the expected account; sign in again",
                ));
            }
        }
    }
    Ok(())
}

/// Check one credential snapshot against a managed row. Missing or undecodable
/// claims supply no contradiction; this neither authenticates JWTs nor leases the file.
pub(super) fn check_login_identity(
    home: &Path,
    expected_email: Option<&str>,
) -> std::io::Result<()> {
    if let Some(stored) = read_stored_auth(home) {
        check_stored_identity(&stored, expected_email)?;
    }
    Ok(())
}

fn stored_auth_from_doc(doc: Value) -> Option<StoredAuth> {
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
    let id_claims = id_token.as_deref().and_then(parse_jwt_claims);
    let access_claims = parse_jwt_claims(&access_token);
    let jwt_account_id = id_claims
        .as_ref()
        .and_then(|c| c.account_id.clone())
        .or_else(|| access_claims.as_ref().and_then(|c| c.account_id.clone()));
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
        id_claims,
        access_claims,
    })
}

fn prepare_auth_dest(home: &Path) -> std::io::Result<PathBuf> {
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
    Ok(dest)
}

fn write_auth_doc(home: &Path, doc: &Value) -> std::io::Result<()> {
    let dest = prepare_auth_dest(home)?;
    let mut rendered =
        aterm_json::to_string(doc).map_err(|e| std::io::Error::other(e.to_string()))?;
    rendered.push('\n');
    super::upgrade_models::write_atomic(&dest, &rendered)
}

/// The suffix of the lock file beside a login's `auth.json` ([`AuthLock`]).
pub const LOCK_SUFFIX: &str = ".lock";

/// The suffix of the refresh journal beside a login's `auth.json` ([`RefreshJournal`]).
pub const JOURNAL_SUFFIX: &str = ".refreshed";

/// The pause between looks at an [`AuthLock`] another refresh holds.
const LOCK_POLL: Duration = Duration::from_millis(20);

/// How long a sign-in's write waits for a refresh of the login it replaces ([`AuthLock`]).
const SIGN_IN_LOCK_WAIT: Duration = Duration::from_secs(15);

/// The least time a refresh must have left when it would send its request
/// ([`refresh_admitted`]). The provider spends the refresh token once the request reaches it,
/// so a request whose answer the backstop cuts off loses the rotated token. A fresh HTTPS
/// exchange is four round trips (DNS, TCP, TLS 1.3, then the request) and the provider's own
/// work: three seconds holds that on a 500 ms round trip, a slow mobile link's, with a second
/// left for the provider. With less, nothing is sent and the next read refreshes.
const MIN_REFRESH_BUDGET: Duration = Duration::from_secs(3);

/// How long a read that found its login refused looks for another program's rotation of it
/// before it says the login is signed out ([`confirmed_signed_out`]). `codex-alab` (`b09caa8d7`,
/// `codex_alab_core::auth_lock`) takes [`AuthLock`] across its refresh and save, while a vendor
/// `codex` binary (or a refresh that proceeded after its lock wait expired) takes no lock on
/// `auth.json`: where such a writer spent the refresh token first, aterm's refresh is refused as
/// reused, and the other writer writes the rotated login as soon as it has read the provider's
/// answer, an answer that left the provider before aterm's refusal did. The gap is one network
/// jitter and one file write, well under a second on a link that answered at all; two seconds
/// holds twice that.
const SIGNED_OUT_CONFIRM: Duration = Duration::from_secs(2);

/// `auth.json`'s name in `home` with `suffix` after it, in the same directory.
fn beside_auth(home: &Path, suffix: &str) -> PathBuf {
    let auth = auth_path_in_home(home);
    let mut name = auth
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_else(|| AUTH_FILE.into());
    name.push(suffix);
    auth.with_file_name(name)
}

/// An exclusive `flock(2)` on `auth.json.lock` beside a Codex login, held from the read before a
/// refresh's request to its publication, so no two aterm processes, threads, or `codex-alab`
/// sessions (`b09caa8d7`, `codex_alab_core::auth_lock`) spend one single-use refresh token: the
/// one that waited reads the rotated file and sends nothing. Released by `LOCK_UN` on drop, so a
/// child spawned meanwhile holds no copy of it. A vendor `codex` binary takes no file lock.
#[derive(Debug)]
pub struct AuthLock(std::fs::File);

impl AuthLock {
    /// `auth_file`'s lock file ([`lock_path`]) opened for a take: made `0600` where it is
    /// missing, never through a link, and refused where it is no regular file.
    fn open(auth_file: &Path) -> std::io::Result<std::fs::File> {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(lock_path(auth_file))?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the login's lock file is no regular file",
            ));
        }
        Ok(file)
    }

    /// The lock beside `auth_file`, waited for until `deadline` (one look where it has passed):
    /// `Ok(None)` where it was still held then, an error where it is refused or cannot be taken.
    fn until(auth_file: &Path, deadline: Instant) -> std::io::Result<Option<AuthLock>> {
        let file = Self::open(auth_file)?;
        loop {
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard's own take: its Drop releases by LOCK_UN"
            )]
            let taken = file.try_lock();
            match taken {
                Ok(()) => return Ok(Some(AuthLock(file))),
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(err)) => return Err(err),
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            std::thread::sleep(left.min(LOCK_POLL));
        }
    }

    /// The lock beside `auth_file` if no one holds it now: `Ok(None)` where another open of it
    /// holds it, an error where it is refused or cannot be taken.
    pub fn try_take(auth_file: &Path) -> std::io::Result<Option<AuthLock>> {
        Self::until(auth_file, Instant::now())
    }

    /// The lock beside `auth_file`, waited for up to `wait`: `Ok(None)` where it was still held
    /// then, an error where it is refused or cannot be taken.
    pub fn take(auth_file: &Path, wait: Duration) -> std::io::Result<Option<AuthLock>> {
        let deadline = Instant::now()
            .checked_add(wait)
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(86_400));
        Self::until(auth_file, deadline)
    }

    /// The lock for `home`'s login, waited for until `deadline`; `None` where it stayed held, or
    /// `home` or its lock file is a link, or the lock file is no regular file or cannot be made.
    fn within(home: &Path, deadline: Instant) -> Option<AuthLock> {
        if std::fs::symlink_metadata(home).is_ok_and(|md| md.file_type().is_symlink()) {
            return None;
        }
        Self::until(&auth_path_in_home(home), deadline)
            .ok()
            .flatten()
    }
}

impl Drop for AuthLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// The tokens a refresh response named, and what its publication adds beside them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RefreshedTokens {
    access_token: String,
    id_token: Option<String>,
    refresh_token: Option<String>,
    account_id: Option<String>,
    last_refresh: String,
}

impl RefreshedTokens {
    /// `doc` with these tokens in its `tokens` and `last_refresh` stamped; independent metadata
    /// is kept. `None` where `doc` holds no `tokens` object.
    fn merged_into(&self, mut doc: Value) -> Option<Value> {
        let tokens = doc.as_object_mut()?.get_mut("tokens")?.as_object_mut()?;
        tokens.insert(
            "access_token".to_owned(),
            Value::String(self.access_token.clone()),
        );
        if let Some(id) = &self.id_token {
            tokens.insert("id_token".to_owned(), Value::String(id.clone()));
        }
        if let Some(account) = &self.account_id {
            tokens.insert("account_id".to_owned(), Value::String(account.clone()));
        }
        if let Some(refresh) = &self.refresh_token {
            tokens.insert("refresh_token".to_owned(), Value::String(refresh.clone()));
        }
        doc.as_object_mut()?.insert(
            "last_refresh".to_owned(),
            Value::String(self.last_refresh.clone()),
        );
        Some(doc)
    }
}

/// A refresh response kept in a private `auth.json.refreshed` (`0600`) beside the login before
/// anything can fail to publish it: the provider has spent the old refresh token once it
/// answers, so a failed re-read or write of `auth.json` must not lose the new one. `base` names
/// the authentication it was refreshed from ([`authentication_digest`]); the next read publishes
/// it onto `auth.json` while the file still holds that authentication ([`recover_journal`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct RefreshJournal {
    base: String,
    tokens: RefreshedTokens,
}

/// A digest of `doc`'s `auth_mode` and `tokens`, the authentication [`same_authentication`]
/// compares, so a journal names what it was refreshed from without holding a copy of it.
fn authentication_digest(doc: &Value) -> Option<String> {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-codex-refresh-base-v1");
    for field in [doc.get("auth_mode"), doc.get("tokens")] {
        let rendered = field.map(aterm_json::to_string).transpose().ok()?;
        let bytes = rendered.as_deref().unwrap_or_default().as_bytes();
        hash.update([u8::from(field.is_some())]);
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    Some(aterm_codec::hex::encode_lower(&hash.finalize()))
}

impl RefreshJournal {
    fn to_value(&self) -> Value {
        let t = &self.tokens;
        json!({
            "aterm_codex_refresh": 1,
            "base": self.base,
            "access_token": t.access_token,
            "id_token": t.id_token,
            "refresh_token": t.refresh_token,
            "account_id": t.account_id,
            "last_refresh": t.last_refresh,
        })
    }

    fn of_value(value: &Value) -> Option<RefreshJournal> {
        (value.get("aterm_codex_refresh")?.as_u64()? == 1).then_some(())?;
        let text = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        Some(RefreshJournal {
            base: text("base")?,
            tokens: RefreshedTokens {
                access_token: text("access_token")?,
                id_token: text("id_token"),
                refresh_token: text("refresh_token"),
                account_id: text("account_id"),
                last_refresh: text("last_refresh")?,
            },
        })
    }

    /// Kept durably beside `home`'s login (`0600`, renamed into place).
    fn write(&self, home: &Path) -> std::io::Result<()> {
        let mut rendered = aterm_json::to_string(&self.to_value())
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        rendered.push('\n');
        super::upgrade_models::write_atomic(&beside_auth(home, JOURNAL_SUFFIX), &rendered)
    }

    /// The journal beside `home`'s login: `None` where there is none or it is a link or no
    /// regular file; `Some(None)` where it does not read as one.
    fn read(home: &Path) -> Option<Option<RefreshJournal>> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
        }
        let file = options.open(beside_auth(home, JOURNAL_SUFFIX)).ok()?;
        if !file.metadata().ok()?.is_file() {
            return None;
        }
        let mut bytes = Vec::new();
        let limit = AUTH_FILE_MAX as u64;
        if file
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() as u64 > limit
        {
            return Some(None);
        }
        Some(
            aterm_json::from_slice::<Value>(&bytes)
                .ok()
                .as_ref()
                .and_then(RefreshJournal::of_value),
        )
    }

    fn discard(home: &Path) {
        let _ = std::fs::remove_file(beside_auth(home, JOURNAL_SUFFIX));
    }
}

/// Publishes a journaled refresh response ([`RefreshJournal`]) onto `home`'s `auth.json` while the
/// file still holds the authentication it was refreshed from, whose refresh token is spent, then
/// drops the journal; drops it unpublished where another login now stands in the file (a newer
/// rotation or sign-in, which a publication would overwrite), and keeps it where the file cannot
/// be read now, for a later read. The caller holds [`AuthLock`].
fn recover_journal_held(home: &Path) {
    let Some(journal) = RefreshJournal::read(home) else {
        return;
    };
    let Some(journal) = journal else {
        RefreshJournal::discard(home);
        return;
    };
    let Some(current) = load_stored_auth(home) else {
        return;
    };
    if authentication_digest(&current.doc).as_deref() != Some(journal.base.as_str()) {
        RefreshJournal::discard(home);
        return;
    }
    if let Some(doc) = journal.tokens.merged_into(current.doc)
        && stored_auth_from_doc(doc.clone()).is_some()
        && write_auth_doc(home, &doc).is_ok()
    {
        RefreshJournal::discard(home);
    }
}

/// [`recover_journal_held`] under [`AuthLock`], waited for until `deadline`, where a journal
/// stands beside `home`'s login.
fn recover_journal(home: &Path, deadline: Instant) {
    if std::fs::symlink_metadata(beside_auth(home, JOURNAL_SUFFIX)).is_err() {
        return;
    }
    if let Some(_held) = AuthLock::within(home, deadline) {
        recover_journal_held(home);
    }
}

/// Reads the ChatGPT account stored in `<home>/auth.json` directly from its OAuth JWT claims,
/// spawning no subprocess. Returns `None` when `auth.json` is absent, unreadable, API-key-only,
/// holds no decodable ChatGPT token, or its tokens name different known users.
#[must_use]
pub fn read_account_in_home(home: &Path) -> Option<Account> {
    let stored = load_stored_auth(home)?;
    let StoredAuth {
        id_claims,
        access_claims,
        ..
    } = stored;
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

fn is_loopback_endpoint(url: &str) -> bool {
    aterm_http::Uri::parse(url).is_some_and(|uri| is_loopback_host(uri.host()))
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1") || host.eq_ignore_ascii_case("localhost")
}

fn is_allowed_auth_endpoint(url: &str) -> bool {
    refresh_endpoint(url).is_some()
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
    if let Some(exp) = stored.access_claims.as_ref().and_then(|c| c.exp)
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
    updated_disk_auth(prior, load_stored_auth(home)?)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CodexRefreshIdentity",
        action = "Observe",
        project = "aterm_agent::harness::codex_app_login::tests::identity_transition"
    )
)]
fn updated_disk_auth(prior: &StoredAuth, disk: StoredAuth) -> Option<StoredAuth> {
    let same_account = prior
        .account_id
        .as_ref()
        .zip(disk.account_id.as_ref())
        .is_some_and(|(a, b)| a == b);
    let same_user = matches!(
        (
            token_user_claims(prior.id_claims.as_ref(), prior.access_claims.as_ref()),
            token_user_claims(disk.id_claims.as_ref(), disk.access_claims.as_ref()),
        ),
        (TokenUser::Known(a), TokenUser::Known(b)) if a == b
    );
    let same_login = ["login_owner_id", "oauth_provenance"]
        .iter()
        .all(|field| prior.doc["tokens"].get(field) == disk.doc["tokens"].get(field));
    (same_account
        && same_user
        && same_login
        && !disk.access_token.trim().is_empty()
        && disk.access_token != prior.access_token)
        .then_some(disk)
}

/// The `CodexRefreshIdentity` environment selector, named where the whole-source
/// closure can collect it: the conformance fixture choosing its token-identity and
/// workspace case is not a production observation step.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CodexRefreshIdentity",
        action = "AnotherCase",
        reason = "The conformance fixture selects which token identity and workspace snapshot the observation must judge; updated_disk_auth does not choose its caller's credentials. codex_refresh_identity_conforms_and_preserves_workspace_and_login exercises every case."
    )
)]
#[doc(hidden)]
pub fn codex_refresh_identity_environment() {}

/// The `CodexRefreshAuthority` environment selector, named where the whole-source
/// closure can collect it: the conformance fixture choosing its recorded-authority
/// case is not a production refresh step.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CodexRefreshAuthority",
        action = "AnotherCase",
        reason = "The conformance fixture selects which recorded OAuth authority or legacy fallback the refresh must honour; refresh_stored_auth does not choose its stored provenance. codex_refresh_uses_the_recorded_oauth_authority exercises every case."
    )
)]
#[doc(hidden)]
pub fn codex_refresh_authority_environment() {}

/// The `CodexLoginPublication` environment selector, named where the whole-source
/// closure can collect it: the conformance fixture choosing its token-response,
/// transport and entropy case is not a production exchange step.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CodexLoginPublication",
        action = "AnotherCase",
        reason = "The conformance fixture selects which token response, transport and entropy the exchange must judge; CodeExchange::persist does not choose its token endpoint's answers. codex_login_publication_conforms_and_rejects_conflicting_token_users exercises every case."
    )
)]
#[doc(hidden)]
pub fn codex_login_publication_environment() {}

/// The `CodexRefreshResult` environment selector, visible to the whole-source
/// closure: choosing a later file edit or a failed publication is a fixture step.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CodexRefreshResult",
        action = "AnotherCase",
        reason = "The conformance fixture selects the independent post-publication file edit or writer failure; refresh_admitted does not choose another writer's changes. refreshed_result_is_the_published_snapshot_not_a_later_login exercises every case."
    )
)]
#[doc(hidden)]
pub fn codex_refresh_result_environment() {}

/// Environment choices for the stored-credential admission model.
#[doc(hidden)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CodexStoredIdentity",
        action = "AdvanceId",
        reason = "Choose the stored ID token's unknown or known user claim."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CodexStoredIdentity",
        action = "AdvanceAccess",
        reason = "Choose the stored access token's unknown or known user claim."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CodexStoredIdentity",
        action = "Opaque",
        reason = "Choose legacy opaque tokens without decodable identity metadata."
    )
)]
pub fn codex_stored_identity_environment() {}

/// Environment choices and external file replacement for the Codex row-identity model.
#[doc(hidden)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CodexRowIdentity",
        action = "Expected",
        reason = "Choose whether the managed row declares an explicit expected email."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CodexRowIdentity",
        action = "Email",
        reason = "Choose the stored credential's unknown or known top-level or ID-token email claim."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CodexRowIdentity",
        action = "AccessEmail",
        reason = "Choose the stored credential's unknown or known access-token email claim."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CodexRowIdentity",
        action = "Replace",
        reason = "External caller or process replaces auth.json on disk between launch preparation and usage polling."
    )
)]
pub fn codex_row_identity_environment() {}

fn same_authentication(prior: &StoredAuth, disk: &StoredAuth) -> bool {
    prior.doc.get("auth_mode") == disk.doc.get("auth_mode")
        && prior.doc.get("tokens") == disk.doc.get("tokens")
}

/// The `CodexRefreshPublication` environment selectors, named where the whole-source
/// closure can collect them: the conformance fixture choosing its case and boundary
/// is not a production refresh step.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CodexRefreshPublication",
        action = "AnotherCase",
        reason = "The conformance fixture selects which concurrent credential edit the refresh must survive; refresh_stored_auth does not choose its caller's edit. a_refresh_response_cannot_replace_a_login_changed_during_the_request exercises every case."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CodexRefreshPublication",
        action = "BeforeRequest",
        reason = "The conformance fixture selects whether the independent edit lands before the HTTP request or before its response is published; this selector is not a production refresh step. a_refresh_response_cannot_replace_a_login_changed_during_the_request exercises both boundaries."
    )
)]
#[doc(hidden)]
pub fn codex_refresh_publication_environment() {}

struct RefreshAuthority {
    token_url: String,
    client_id: String,
}

fn refresh_endpoint(url: &str) -> Option<aterm_http::Uri> {
    if url
        .bytes()
        .any(|b| b <= 0x20 || b >= 0x7f || matches!(b, b'\\' | b'?' | b'#'))
    {
        return None;
    }
    let uri = aterm_http::Uri::parse(url)?;
    (uri.scheme() == aterm_http::Scheme::Https || is_loopback_host(uri.host())).then_some(uri)
}

/// Recorded authority wins over the legacy fallback. An invalid present record
/// is not a legacy login, and JWT claims never supply refresh authority.
fn refresh_authority(stored: &StoredAuth, fallback_token_url: &str) -> Option<RefreshAuthority> {
    let Some(record) = stored.doc["tokens"].get("oauth_provenance") else {
        refresh_endpoint(fallback_token_url)?;
        return Some(RefreshAuthority {
            token_url: fallback_token_url.to_owned(),
            client_id: CLIENT_ID.to_owned(),
        });
    };
    let issuer = record.get("issuer")?.as_str()?;
    let client_id = record.get("client_id")?.as_str()?;
    if client_id.is_empty()
        || client_id.trim() != client_id
        || client_id.chars().any(char::is_control)
    {
        return None;
    }
    let issuer_uri = refresh_endpoint(issuer)?;
    let token_url = format!("{}/oauth/token", issuer.trim_end_matches('/'));
    let target = refresh_endpoint(&token_url)?;
    if target.scheme() != issuer_uri.scheme()
        || target.host() != issuer_uri.host()
        || target.port() != issuer_uri.port()
    {
        return None;
    }
    Some(RefreshAuthority {
        token_url,
        client_id: client_id.to_owned(),
    })
}

/// What one refresh of a stored login came to ([`refresh_admitted`]).
#[derive(Debug)]
enum Refreshed {
    /// The login now in the file: this refresh's publication, or another writer's newer
    /// rotation of the same login.
    Done(Box<StoredAuth>),
    /// The provider refused the refresh token for good (codex-alab's rule: a `401`, an
    /// `invalid_grant` `400`, or `refresh_token_expired`, `refresh_token_reused` or
    /// `refresh_token_invalidated`): the login is signed out until it signs in again.
    Rejected,
    /// Nothing is known: no request was sent, the network, a timeout, another answer, or a
    /// response that could not be published (kept in the journal then, [`RefreshJournal`]).
    Failed,
}

impl Refreshed {
    fn done(self) -> Option<StoredAuth> {
        match self {
            Refreshed::Done(stored) => Some(*stored),
            Refreshed::Rejected | Refreshed::Failed => None,
        }
    }
}

/// Whether a token endpoint's non-`200` answer refuses the refresh token for good, as codex-alab
/// reads one (`login/src/auth/manager.rs`, `classify_refresh_token_failure`): its `error` (or
/// `error.code`, or `code`) names the refresh token expired, reused or invalidated, or the
/// answer is a `401`, or an `invalid_grant` `400`.
fn refresh_refused(status: u16, body: &[u8]) -> bool {
    let parsed: Option<Value> = aterm_json::from_slice(body).ok();
    let text = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_lowercase)
    };
    let code = parsed.as_ref().and_then(|json| {
        text(json.get("error"))
            .or_else(|| text(json.get("error").and_then(|error| error.get("code"))))
            .or_else(|| text(json.get("code")))
    });
    status == 401
        || matches!(
            code.as_deref(),
            Some("refresh_token_expired" | "refresh_token_reused" | "refresh_token_invalidated")
        )
        || (status == 400 && code.as_deref() == Some("invalid_grant"))
}

/// Observe the credential again before the request and publication. A token
/// rotation already visible at either boundary wins over this refresh. Other aterm
/// refreshes of the login are ordered by [`AuthLock`]; this is not an atomic
/// compare-and-swap with other programs' later file replacements.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CodexRefreshPublication",
        action = "Refresh",
        project = "aterm_agent::harness::codex_app_login::tests::refresh_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CodexRefreshIdentity",
        action = "Observe",
        project = "aterm_agent::harness::codex_app_login::tests::identity_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CodexRefreshAuthority",
        action = "Refresh",
        project = "aterm_agent::harness::codex_app_login::tests::authority_transition"
    )
)]
fn refresh_stored_auth(
    home: &Path,
    stored: &StoredAuth,
    fallback_token_url: &str,
    wait: Duration,
    now: i64,
    expected_email: Option<&str>,
) -> Option<StoredAuth> {
    refresh_outcome(home, stored, fallback_token_url, wait, now, expected_email).done()
}

/// [`refresh_stored_auth`], saying why where it published nothing ([`Refreshed`]). A login, or
/// a response, whose tokens name an email other than `expected_email` sends nothing more and
/// keeps nothing: no request for the one, no journal or publication of the other.
fn refresh_outcome(
    home: &Path,
    stored: &StoredAuth,
    fallback_token_url: &str,
    wait: Duration,
    now: i64,
    expected_email: Option<&str>,
) -> Refreshed {
    if check_stored_identity(stored, expected_email).is_err() {
        return Refreshed::Failed;
    }
    let admit = |candidate: &StoredAuth| check_stored_identity(candidate, expected_email).is_ok();
    refresh_admitted(
        home,
        stored,
        fallback_token_url,
        wait,
        now,
        admit,
        write_auth_doc,
    )
}

/// One refresh of `stored`, `home`'s login, within `wait`: under [`AuthLock`] from the read
/// before its request to its publication, a journaled response ([`RefreshJournal`]) published
/// first, and a response `admit` takes journaled before the file is read again, so a failed
/// re-read or write keeps the rotated refresh token for the next read. `publish` writes the
/// refreshed login (`write_auth_doc`; a conformance fixture passes its own).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CodexRefreshResult",
        action = "Refresh",
        project = "aterm_agent::harness::codex_app_login::tests::result_transition"
    )
)]
fn refresh_admitted(
    home: &Path,
    stored: &StoredAuth,
    fallback_token_url: &str,
    wait: Duration,
    now: i64,
    admit: impl Fn(&StoredAuth) -> bool,
    publish: impl FnOnce(&Path, &Value) -> std::io::Result<()>,
) -> Refreshed {
    let Some(deadline) = Instant::now().checked_add(wait).filter(|_| !wait.is_zero()) else {
        return Refreshed::Failed;
    };
    let prior_user = token_user(stored.id_token.as_deref(), &stored.access_token);
    if prior_user == TokenUser::Conflicting {
        return Refreshed::Failed;
    }
    let Some(_held) = AuthLock::within(home, deadline) else {
        return Refreshed::Failed;
    };
    recover_journal_held(home);
    let Some(before_request) = load_stored_auth(home) else {
        return Refreshed::Failed;
    };
    if !same_authentication(stored, &before_request) {
        return updated_disk_auth(stored, before_request)
            .map_or(Refreshed::Failed, |login| Refreshed::Done(Box::new(login)));
    }
    let (Some(authority), Some(refresh_token), Some(base)) = (
        refresh_authority(stored, fallback_token_url),
        stored.refresh_token.as_deref(),
        authentication_digest(&stored.doc),
    ) else {
        return Refreshed::Failed;
    };
    let token_url = &authority.token_url;
    let Ok(body) = aterm_json::to_string(&json!({
        "client_id": authority.client_id,
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
    })) else {
        return Refreshed::Failed;
    };
    // The lock may have been waited for: a request with too little time left could spend the
    // refresh token at the provider and lose its answer to the backstop.
    let left = deadline.saturating_duration_since(Instant::now());
    if left < MIN_REFRESH_BUDGET {
        return Refreshed::Failed;
    }
    let Ok(resp) = http_client(token_url, left)
        .request(Method::Post, token_url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("User-Agent", "codex-cli")
        .header("originator", ORIGINATOR)
        .limit(MESSAGE_MAX)
        .backstop(left)
        .send(body.as_bytes())
    else {
        return Refreshed::Failed;
    };
    if resp.status() != 200 {
        return if refresh_refused(resp.status(), resp.body()) {
            Refreshed::Rejected
        } else {
            Refreshed::Failed
        };
    }
    let Some(payload) = aterm_json::from_slice::<Value>(resp.body()).ok() else {
        return Refreshed::Failed;
    };
    let text = |key: &str| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let (Some(new_access), new_id) = (text("access_token"), text("id_token")) else {
        return Refreshed::Failed;
    };
    let after_user = token_user(new_id.or(stored.id_token.as_deref()), new_access);
    match (&prior_user, &after_user) {
        (_, TokenUser::Conflicting) => return Refreshed::Failed,
        (TokenUser::Known(a), TokenUser::Known(b)) if a != b => return Refreshed::Failed,
        _ => {}
    }
    let tokens = RefreshedTokens {
        access_token: new_access.to_owned(),
        id_token: new_id.map(str::to_owned),
        refresh_token: text("refresh_token").map(str::to_owned),
        account_id: stored.account_id.clone().or_else(|| {
            new_id
                .and_then(parse_jwt_claims)
                .and_then(|c| c.account_id)
                .or_else(|| parse_jwt_claims(new_access).and_then(|c| c.account_id))
        }),
        last_refresh: aterm_types::rfc3339::format_rfc3339(u64::try_from(now.max(0)).unwrap_or(0)),
    };
    // A response for another account is neither kept nor published.
    let admitted = tokens
        .merged_into(before_request.doc)
        .and_then(stored_auth_from_doc)
        .is_some_and(|candidate| admit(&candidate));
    if !admitted {
        return Refreshed::Failed;
    }
    // The provider has spent the old refresh token: keep its answer before anything below can
    // fail. A journal that cannot be written leaves the publication below as the only copy.
    let journal = RefreshJournal { base, tokens };
    let _ = journal.write(home);
    let Some(current) = load_stored_auth(home) else {
        return Refreshed::Failed;
    };
    if !same_authentication(stored, &current) {
        RefreshJournal::discard(home);
        return updated_disk_auth(stored, current)
            .map_or(Refreshed::Failed, |login| Refreshed::Done(Box::new(login)));
    }
    // Preserve independent metadata changes without discarding the response's
    // rotated refresh token when the complete authentication record is unchanged.
    let Some(candidate) = journal
        .tokens
        .merged_into(current.doc)
        .and_then(stored_auth_from_doc)
    else {
        return Refreshed::Failed;
    };
    if publish(home, &candidate.doc).is_err() {
        return Refreshed::Failed;
    }
    RefreshJournal::discard(home);
    // The completed write owns this result. A later file replacement belongs
    // to another operation and must not change this refresh's identity.
    Refreshed::Done(Box::new(candidate))
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

fn has_origin_host_prefix(url: &str, origin: &str) -> bool {
    url.strip_prefix(origin)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', ':']))
}

/// Normalizes `base_url` using Codex's backend rules and appends `/wham/usage` or `/api/codex/usage`.
#[must_use]
pub fn usage_url_from_base(base_url: &str) -> String {
    let mut base = base_url.trim().trim_end_matches('/').to_owned();
    if (has_origin_host_prefix(&base, "https://chatgpt.com")
        || has_origin_host_prefix(&base, "https://chat.openai.com"))
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
    /// Every full recognized window, with its own reset. If no window is full but the server
    /// refuses ordinary use, one fallback wall names the most-used recognized window.
    /// Keeping simultaneous walls prevents an earlier reset from releasing a later limit.
    pub walls: Vec<(&'static str, Option<i64>)>,
}

/// Reads `account/rateLimits/read`'s normalized answer at `now`: `None` where it holds no `rateLimits`.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CodexUsageWalls",
        action = "Parse",
        project = "aterm_agent::harness::accounts_pool::tests::quota::quota_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CodexUsageWalls",
        action = "Case",
        reason = "The fixture selects provider percentages, slot order and reset availability before parsing."
    )
)]
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
    let mut walls: Vec<_> = [(session, SESSION_LIMIT), (week, WEEK_LIMIT)]
        .into_iter()
        .filter_map(|(w, name)| {
            w.filter(|w| w.used_percent >= 100.0)
                .map(|w| (name, w.resets_at))
        })
        .collect();
    let refused = result.get("ordinaryUsageAllowed").and_then(Value::as_bool) == Some(false)
        || limits
            .get("rateLimitReachedType")
            .is_some_and(|t| !t.is_null());
    if walls.is_empty() && refused {
        let latest = [session, week]
            .into_iter()
            .flatten()
            .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent));
        walls.push(match latest {
            Some(w) if Some(w) == week => (WEEK_LIMIT, w.resets_at),
            Some(w) => (SESSION_LIMIT, w.resets_at),
            None => (SESSION_LIMIT, None),
        });
    }
    Some(Usage { sample, walls })
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

fn get_usage_once(
    usage_url: &str,
    stored: &StoredAuth,
    wait: Duration,
    expected_email: Option<&str>,
) -> Option<(u16, Vec<u8>)> {
    check_stored_identity(stored, expected_email).ok()?;
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

/// What one read of a Codex login's usage found ([`read_login_with`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    /// The login's usage.
    Usage(Usage),
    /// The provider refused the login itself: `/wham/usage` refused its access token (`401`)
    /// and the refresh token was refused for good, or there was none to refresh it with, and a
    /// second ask after a look for another writer's rotation was refused too
    /// (`SIGNED_OUT_CONFIRM`). It stays signed out until it signs in again.
    SignedOut,
    /// Nothing was learned: no login file, no reader for the CLI, a login that names another
    /// user than its row expects, the network, a timeout (a refusal with no time left to
    /// confirm among them), a refused refreshed token, or an answer it could not read.
    Unread,
}

impl Reading {
    /// The usage it read, where it read one.
    #[must_use]
    pub fn usage(self) -> Option<Usage> {
        match self {
            Reading::Usage(usage) => Some(usage),
            Reading::SignedOut | Reading::Unread => None,
        }
    }
}

impl From<Option<Usage>> for Reading {
    fn from(usage: Option<Usage>) -> Reading {
        usage.map_or(Reading::Unread, Reading::Usage)
    }
}

/// [`read_login_with`]'s usage for a login no row constrains.
#[must_use]
pub fn read_usage_with(
    home: &Path,
    usage_url: &str,
    fallback_token_url: &str,
    wait: Duration,
    now: i64,
) -> Option<Usage> {
    read_usage_with_expected_email(home, usage_url, fallback_token_url, wait, now, None)
}

/// Read usage using snapshots constrained by a managed row's expected email
/// ([`read_login_with`]'s usage).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CodexRowIdentity",
        action = "Poll",
        project = "aterm_agent::harness::codex_app_login::tests::row_identity_tests::row_identity_transition"
    )
)]
fn read_usage_with_expected_email(
    home: &Path,
    usage_url: &str,
    fallback_token_url: &str,
    wait: Duration,
    now: i64,
    expected_email: Option<&str>,
) -> Option<Usage> {
    read_login_with(
        home,
        usage_url,
        fallback_token_url,
        wait,
        now,
        expected_email,
    )
    .usage()
}

/// Reads `<home>/auth.json`, refreshes its OAuth token when near/past expiry or rejected with
/// `401`, fetches `usage_url`, and returns its [`Reading`] at `now`. A refresh response a
/// failed publication journaled is published first ([`RefreshJournal`]). Recorded OAuth
/// provenance selects the refresh issuer and client; `fallback_token_url` applies only to older
/// records without it. Contradictory known token users, or a token email other than
/// `expected_email` (a managed row's), are refused before any HTTP request.
#[must_use]
pub fn read_login_with(
    home: &Path,
    usage_url: &str,
    fallback_token_url: &str,
    wait: Duration,
    now: i64,
    expected_email: Option<&str>,
) -> Reading {
    let deadline = Instant::now()
        .checked_add(wait)
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(86_400));
    let left = || deadline.saturating_duration_since(Instant::now());
    recover_journal(home, deadline);
    let Some(mut stored) = load_stored_auth(home) else {
        return Reading::Unread;
    };
    if check_stored_identity(&stored, expected_email).is_err() {
        return Reading::Unread;
    }
    if needs_proactive_refresh(&stored, now) {
        if let Some(reloaded) = reload_updated_disk_auth(home, &stored) {
            stored = reloaded;
        }
        // Where this refresh fails the access token may still serve; the read below says.
        if needs_proactive_refresh(&stored, now)
            && let Some(refreshed) = refresh_stored_auth(
                home,
                &stored,
                fallback_token_url,
                left(),
                now,
                expected_email,
            )
        {
            stored = refreshed;
        }
    }
    let Some((mut status, mut body)) = get_usage_once(usage_url, &stored, left(), expected_email)
    else {
        return Reading::Unread;
    };
    if status == 401
        && let Some(reloaded) = reload_updated_disk_auth(home, &stored)
    {
        stored = reloaded;
        let Some(again) = get_usage_once(usage_url, &stored, left(), expected_email) else {
            return Reading::Unread;
        };
        (status, body) = again;
    }
    if status == 401 {
        if stored.refresh_token.is_none() {
            return confirmed_signed_out(home, &stored, usage_url, deadline, now, expected_email);
        }
        match refresh_outcome(
            home,
            &stored,
            fallback_token_url,
            left(),
            now,
            expected_email,
        ) {
            Refreshed::Done(refreshed) => {
                let Some(again) = get_usage_once(usage_url, &refreshed, left(), expected_email)
                else {
                    return Reading::Unread;
                };
                (status, body) = again;
            }
            // Confirmed before it says signed out: a rotation another writer lands meanwhile
            // is a login, not a refusal.
            Refreshed::Rejected => {
                return confirmed_signed_out(
                    home,
                    &stored,
                    usage_url,
                    deadline,
                    now,
                    expected_email,
                );
            }
            Refreshed::Failed => return Reading::Unread,
        }
    }
    usage_reading(status, &body, now)
}

/// A `/wham/usage` answer's [`Reading`]: its usage where it served one.
fn usage_reading(status: u16, body: &[u8], now: i64) -> Reading {
    if !(200..300).contains(&status) {
        return Reading::Unread;
    }
    aterm_json::from_slice::<Value>(body)
        .ok()
        .and_then(|payload| usage_from_backend(&payload, now))
        .into()
}

/// `home`'s login `stored`, which the provider refused once ([`read_login_with`]), read again
/// before anything says it is signed out, since one refusal can be a race lost to another
/// program's refresh ([`SIGNED_OUT_CONFIRM`]). Within that pause, a rotation another writer
/// lands in the file is the login: its usage is read. With none, the usage is asked once more
/// with the refused access token (no refresh is sent again: its token is spent), and a second
/// `401` is [`Reading::SignedOut`]. Unread where the read's `deadline` comes first, or the second
/// ask says nothing.
fn confirmed_signed_out(
    home: &Path,
    stored: &StoredAuth,
    usage_url: &str,
    deadline: Instant,
    now: i64,
    expected_email: Option<&str>,
) -> Reading {
    let Some(end) = Instant::now()
        .checked_add(SIGNED_OUT_CONFIRM)
        .filter(|end| *end < deadline)
    else {
        return Reading::Unread;
    };
    let rotated = loop {
        if let Some(rotated) = reload_updated_disk_auth(home, stored) {
            break Some(rotated);
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break None;
        }
        std::thread::sleep(LOCK_POLL.min(left));
    };
    let login = rotated.as_ref().unwrap_or(stored);
    let left = deadline.saturating_duration_since(Instant::now());
    let Some((status, body)) = get_usage_once(usage_url, login, left, expected_email) else {
        return Reading::Unread;
    };
    if status == 401 && rotated.is_none() {
        return Reading::SignedOut;
    }
    usage_reading(status, &body, now)
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
    read_usage_in_home_for(home, user_home, wait, now, None)
}

/// [`read_usage_in_home`] for a row that expects `expected_email`.
pub(super) fn read_usage_in_home_for(
    home: &Path,
    user_home: &Path,
    wait: Duration,
    now: i64,
    expected_email: Option<&str>,
) -> Option<Usage> {
    read_login_in_home(home, user_home, wait, now, expected_email).usage()
}

/// [`read_usage_in_home_for`]'s [`Reading`] ([`read_login_with`]).
#[must_use]
pub fn read_login_in_home(
    home: &Path,
    user_home: &Path,
    wait: Duration,
    now: i64,
    expected_email: Option<&str>,
) -> Reading {
    let usage_url = resolve_usage_url(home, user_home);
    let token_url = format!("{DEFAULT_ISSUER}/oauth/token");
    read_login_with(home, &usage_url, &token_url, wait, now, expected_email)
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

#[cfg(test)]
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

fn parse_strict_query_params(query: &str) -> Option<Vec<(String, String)>> {
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (raw_k, raw_v) = part.split_once('=').unwrap_or((part, ""));
            Some((percent_decode_query(raw_k)?, percent_decode_query(raw_v)?))
        })
        .collect()
}

fn unique_query_param<'a>(
    params: &'a [(String, String)],
    key: &str,
) -> Result<Option<&'a str>, ()> {
    let mut found = None;
    for (k, v) in params {
        if k == key {
            if found.is_some() {
                return Err(());
            }
            found = Some(v.as_str());
        }
    }
    Ok(found)
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
                let Some(params) = parse_strict_query_params(query) else {
                    write_http_reply(
                        &mut stream,
                        "400 Bad Request",
                        "text/plain",
                        "Invalid callback query",
                    );
                    continue;
                };
                let state_ok = matches!(
                    unique_query_param(&params, "state"),
                    Ok(Some(st))
                        if st == expected_state
                            || st.strip_suffix(":life_sciences") == Some(expected_state.as_str())
                );
                if !state_ok {
                    write_http_reply(
                        &mut stream,
                        "400 Bad Request",
                        "text/plain",
                        "State mismatch",
                    );
                    continue;
                }
                let (Ok(err_param), Ok(code_param)) = (
                    unique_query_param(&params, "error"),
                    unique_query_param(&params, "code"),
                ) else {
                    write_http_reply(
                        &mut stream,
                        "400 Bad Request",
                        "text/plain",
                        "Duplicate callback parameter",
                    );
                    done.store(true, Ordering::SeqCst);
                    let _ = tx.send(CallbackOutcome::Failed);
                    return;
                };
                if err_param.is_some_and(|e| !e.is_empty()) {
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
                let Some(code) = code_param.filter(|c| !c.is_empty()) else {
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
    endpoints: &Endpoints,
    redirect_uri: &str,
    code: &str,
    code_verifier: &str,
    wait: Duration,
    now: i64,
) -> Option<Option<String>> {
    let mut entropy = [0u8; 32];
    let owner = SystemRandom::new()
        .fill(&mut entropy)
        .ok()
        .and_then(|()| base64url_encode(&entropy));
    CodeExchange {
        home,
        endpoints,
        redirect_uri,
        code,
        code_verifier,
        wait,
        now,
    }
    .persist(owner.as_deref())
}

struct CodeExchange<'a> {
    home: &'a Path,
    endpoints: &'a Endpoints,
    redirect_uri: &'a str,
    code: &'a str,
    code_verifier: &'a str,
    wait: Duration,
    now: i64,
}

impl CodeExchange<'_> {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CodexLoginPublication",
            action = "Exchange",
            project = "aterm_agent::harness::codex_app_login::tests::login_transition"
        )
    )]
    fn persist(self, owner: Option<&str>) -> Option<Option<String>> {
        let Self {
            home,
            endpoints,
            redirect_uri,
            code,
            code_verifier,
            wait,
            now,
        } = self;
        let owner = owner.filter(|owner| !owner.is_empty())?;
        let token_url = &endpoints.token_url;
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
        let id_claims = parse_jwt_claims(id_token)?;
        let access_claims = parse_jwt_claims(access_token)?;
        if token_user(Some(id_token), access_token) == TokenUser::Conflicting {
            return None;
        }
        let account_id = id_claims.account_id.or(access_claims.account_id);
        let email = id_claims.email.or(access_claims.email);
        // Only bind an issuer that actually names this token exchange. Explicit
        // transport overrides retain legacy absent provenance, never a false issuer.
        let provenance = refresh_endpoint(&endpoints.issuer)
            .filter(|_| {
                format!("{}/oauth/token", endpoints.issuer.trim_end_matches('/')) == *token_url
            })
            .map(|_| json!({"issuer":endpoints.issuer, "client_id":CLIENT_ID}));
        let stamp = aterm_types::rfc3339::format_rfc3339(u64::try_from(now.max(0)).unwrap_or(0));
        let mut doc = json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": Value::Null,
            "tokens": {
                "id_token": id_token,
                "access_token": access_token,
                "refresh_token": refresh_token,
                "account_id": account_id,
                "login_owner_id": owner,
            },
            "last_refresh": stamp,
        });
        if let Some(provenance) = provenance {
            doc.as_object_mut()?
                .get_mut("tokens")?
                .as_object_mut()?
                .insert("oauth_provenance".into(), provenance);
        }
        // Ordered after a refresh of the login this replaces ([`AuthLock`]), in a folder made
        // ready first so a first sign-in takes the lock too; the sign-in is written even where
        // that refresh outlasts the wait, since its code is spent too.
        prepare_auth_dest(home).ok()?;
        let _held = AuthLock::within(home, Instant::now() + wait.min(SIGN_IN_LOCK_WAIT));
        write_auth_doc(home, &doc).ok()?;
        Some(email)
    }
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
    if !is_loopback_endpoint(&endpoints.token_url) && !pinned(&endpoints.token_url, hosts) {
        return Drove {
            outcome: Outcome::OffPin,
            host: https_host(&endpoints.token_url),
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
        Approval::BotCheck(why) => {
            cancelled.store(true, Ordering::SeqCst);
            let _ = server_thread.join();
            return Drove {
                outcome: Outcome::BotCheck(why),
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
        endpoints,
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
