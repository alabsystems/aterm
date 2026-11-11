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
use super::wake_deadline::WakeDeadline;
use crate::ai::connector::Completion;

#[path = "codex_shared_login.rs"]
pub mod shared;

#[path = "codex_app_login_custody.rs"]
mod custody;
#[path = "codex_enrollment_artifact_bridge.rs"]
mod enrollment_artifact_bridge;
#[path = "codex_grant_completion.rs"]
mod grant_completion;
use custody::legacy_reader_allowed;
pub use custody::{OWNER_MARKER_SUFFIX, OWNER_MARKER_TEXT, legacy_file_reader_allowed};
pub(super) use enrollment_artifact_bridge::{
    reconcile_native_recovery, reconcile_retained_enrollment,
};

#[path = "codex_app_login_rejection.rs"]
mod code_rejection;
#[path = "codex_app_login_refresh_transport.rs"]
mod refresh_transport;
pub(in crate::harness) use code_rejection::{
    CodeRejection, MAX_REJECTION_BODY, is_definitive_code_rejection,
};
pub(in crate::harness) use grant_completion::NoGrantCapture;
pub(in crate::harness) use grant_completion::{CapturedLogin, CodeGrantCapture};
pub use grant_completion::{FreshCodeGrant, StagedWitness};
#[cfg(all(test, unix))]
pub(in crate::harness) use refresh_transport::with_refresh_fixture_endpoint;
pub(in crate::harness) use refresh_transport::{
    ReceivedRefresh, RefreshExchange, exchange_owned_refresh, merge_owned_refresh,
    owned_refresh_token_digest,
};

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
const CALLBACK_IO_WAIT: Duration = Duration::from_secs(2);
/// How long the callback reads an accepted connection's request head.
#[cfg(not(test))]
const CALLBACK_HEAD_WAIT: Duration = CALLBACK_IO_WAIT;
/// How long the callback reads an accepted connection's request head: the unit tests' clients
/// send theirs right after they connect, or close at once, so a head a loaded machine holds is
/// waited for by the hang detector, not answered `400`.
#[cfg(test)]
const CALLBACK_HEAD_WAIT: Duration = aterm_spec::wait::MUST_HAPPEN;
const CALLBACK_DRAIN_WAIT: Duration = Duration::from_millis(50);
const CALLBACK_DRAIN_MAX: usize = 16 * 1024;

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

#[derive(Clone)]
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

impl std::fmt::Debug for StoredAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredAuth").finish_non_exhaustive()
    }
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

/// The lock [`AuthLock::open`] makes beside [`AUTH_FILE`], and leaves there.
pub const AUTH_LOCK_FILE: &str = "auth.json.lock";

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
    read_auth_file_bounded_with(home, aterm_uds::regular_file::open)
}

fn read_auth_file_bounded_with(
    home: &Path,
    open: impl FnOnce(&Path) -> std::io::Result<(std::fs::File, std::fs::Metadata)>,
) -> Option<String> {
    if std::fs::symlink_metadata(home).is_ok_and(|md| md.file_type().is_symlink()) {
        return None;
    }
    let path = auth_path_in_home(home);
    // The shared opener rejects final links/reparse points using the opened handle,
    // including a name replaced after its preliminary check. It fails closed on
    // unsupported platforms and never creates or modifies the credential file.
    let (file, meta) = open(&path).ok()?;
    let limit = AUTH_FILE_MAX as u64;
    if meta.len() > limit {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0).saturating_add(1));
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

/// The account and user the login stored in `home` names, read from its auth file alone, with no
/// network and no renewal: a read of who a row is, never of its credential. `None` where it holds
/// no login, its tokens name different users, or it does not name both.
pub(crate) fn stored_login_key(home: &Path) -> Option<super::profile::AccountKey> {
    let stored = load_stored_auth(home)?;
    let TokenUser::Known(user) =
        token_user_claims(stored.id_claims.as_ref(), stored.access_claims.as_ref())
    else {
        return None;
    };
    super::profile::AccountKey::codex(stored.account_id.as_deref()?, &user)
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
    if std::fs::symlink_metadata(&dest).is_ok_and(|md| md.file_type().is_symlink() || !md.is_file())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "refusing to overwrite non-regular auth.json",
        ));
    }
    Ok(dest)
}

fn write_auth_doc(home: &Path, doc: &Value) -> std::io::Result<()> {
    let dest = prepare_auth_dest(home)?;
    let mut rendered =
        aterm_json::to_string(doc).map_err(|e| std::io::Error::other(e.to_string()))?;
    rendered.push('\n');
    super::atomic_write::write_atomic(&dest, &rendered)
}

/// The suffix of the lock file beside a login's `auth.json` ([`AuthLock`]).
pub const LOCK_SUFFIX: &str = ".lock";

/// The suffix of the refresh journal beside a login's `auth.json` ([`RefreshJournal`]).
pub const JOURNAL_SUFFIX: &str = ".refreshed";

/// The pause between disk reads looking for an unlocked writer's credential rotation
/// during [`confirmed_signed_out`]. Lock acquisition uses `aterm_uds::file_lock`.
const LOCK_POLL: Duration = Duration::from_millis(20);

/// Hang detector for a sign-in's write waiting for the login's refresh ([`AuthLock`]).
const SIGN_IN_LOCK_HANG: Duration = Duration::from_secs(60);

/// Hang detector for one required token response, bounded by the original authorization.
const EXCHANGE_WAIT: Duration = Duration::from_secs(60);

fn exchange_wait(parent: Duration, started: Instant) -> Duration {
    parent.min(EXCHANGE_WAIT.saturating_sub(started.elapsed()))
}

/// The least time a refresh must have left when it would send its request
/// ([`refresh_admitted`]). The provider spends the refresh token once the request reaches it,
/// so a request whose answer the backstop cuts off loses the rotated token. A fresh HTTPS
/// exchange is four round trips (DNS, TCP, TLS 1.3, then the request) and the provider's own
/// work: three seconds holds that on a 500 ms round trip, a slow mobile link's, with a second
/// left for the provider. With less, nothing is sent and the next read refreshes.
const MIN_REFRESH_BUDGET: Duration = Duration::from_secs(3);

/// Hang detector for a valid rotation another writer may still be publishing after a
/// single-use refresh was refused. Its absence at expiry is not a sign-out proof.
const ROTATION_HANG: Duration = Duration::from_secs(60);

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

/// An exclusive lock on `auth.json.lock` beside a Codex login. A refresh caller must re-read
/// authentication after acquiring the guard and hold it through response publication.
///
/// This serializes cooperating writers only. The lock alone cannot prevent token reuse by a
/// writer that bypasses it, or after a lost response or failed publication. Aterm additionally
/// records and recovers refresh attempts through `auth.json.refreshed` while holding the lock;
/// another program must honor both protocols for the cross-program guarantee (ATERM-KI-1027-12).
///
/// Drop explicitly unlocks the file. The Unix child-exclusion request made by
/// `aterm_uds::keep_out_of_children` is best-effort, not a guarantee of fork exclusion.
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
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            options.custom_flags(0x0020_0000);
        }
        let file = options.open(lock_path(auth_file))?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the login's lock file is no regular file",
            ));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            if metadata.file_attributes() & 0x0000_0400 != 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "the login's lock file is no regular file",
                ));
            }
        }
        Ok(file)
    }

    /// The lock beside `auth_file`, waited for until `deadline` (one look where it has passed):
    /// `Ok(None)` where it was still held then, an error where it is refused or cannot be taken.
    fn until(auth_file: &Path, deadline: Instant) -> std::io::Result<Option<AuthLock>> {
        let file = Self::open(auth_file)?;
        match aterm_uds::file_lock::lock_exclusive_until(
            &file,
            deadline,
            "the Codex login lock is held",
        ) {
            Ok(()) => Ok(Some(AuthLock(file))),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error),
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
#[derive(Clone, PartialEq, Eq)]
struct RefreshedTokens {
    access_token: String,
    id_token: Option<String>,
    refresh_token: Option<String>,
    account_id: Option<String>,
    last_refresh: String,
}

impl std::fmt::Debug for RefreshedTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshedTokens").finish_non_exhaustive()
    }
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

/// A private `auth.json.refreshed` (`0600`) records the token digest before sending a refresh.
/// An uncertain attempt cannot send that token again, even after metadata changes or a reload.
/// A usable response replaces the attempt and is recovered onto its original authentication.
/// Credentials in `auth.json` remain available while the result is uncertain. Other programs
/// must honor this journal too if they share the login.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RefreshJournal {
    EnrollmentResponse {
        admission: Box<super::codex_enrollment::Staging>,
        predecessor: Option<String>,
        candidate: NativeCredential,
        digest: String,
    },
    Attempted {
        token: String,
    },
    Rejected {
        token: String,
    },
    Response {
        base: String,
        /// Absent only in journals written before attempts were recorded.
        token: Option<String>,
        tokens: RefreshedTokens,
    },
}

#[derive(Clone, PartialEq, Eq)]
struct NativeCredential(String);

impl std::fmt::Debug for NativeCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeCredential(<redacted>)")
    }
}

fn credential_digest(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
}

/// A reserved credential receipt, including an unreadable obstruction, owns its stage.
/// This observation grants neither completion nor recovery and must never be used to replay HTTP.
pub(super) fn retains_credential_custody(home: &Path) -> bool {
    !matches!(std::fs::symlink_metadata(beside_auth(home, JOURNAL_SUFFIX)), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}

fn require_private_enrollment_directory(home: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        aterm_uds::private_file::JudgedDir::private(home)
            .map(|_| ())
            .map_err(|_| "enrollment directory is not private".into())
    }
    #[cfg(not(unix))]
    {
        let _ = home;
        Err("native enrollment recovery is unavailable on this platform".into())
    }
}

/// Materialize only the exact response of a fresh, Store-admitted enrollment recovery.
/// The caller separately checks current account authority before checked publication.
pub(super) fn materialize_enrollment(
    admission: &super::codex_enrollment::Staging,
    deadline: WakeDeadline,
    work: Option<&super::account_work::Scope>,
    store: &super::accounts::Store,
    signer: &super::accounts::SigninTurn,
) -> Result<String, String> {
    let home = &admission.directory;
    require_private_enrollment_directory(home)?;
    if deadline.passed() || work.is_some_and(super::account_work::Scope::cancelled) {
        return Err("enrollment recovery work expired".into());
    }
    let _held = AuthLock::try_take(&auth_path_in_home(home))
        .map_err(|_| "enrollment credential lock unavailable")?
        .ok_or("enrollment credential is busy")?;
    materialize_enrollment_held(admission, deadline, work, store, signer, &_held)
}

enum EnrollmentPublicationGuard<'a> {
    Legacy {
        store: &'a super::accounts::Store,
        signer: &'a super::accounts::SigninTurn,
    },
    #[cfg(unix)]
    Native {
        store: &'a super::accounts::Store,
        turn: &'a super::native_enrollment_admission::NativeRecoveryTurn,
        current: &'a super::codex_native_row::CurrentNativeRow<'a>,
    },
}
impl EnrollmentPublicationGuard<'_> {
    fn check(&self, _admission: &super::codex_enrollment::Staging) -> Result<(), String> {
        match self {
            Self::Legacy { store, signer } => signer
                .check_store(store)
                .map_err(|_| "enrollment signer changed".into()),
            #[cfg(unix)]
            Self::Native {
                store,
                turn,
                current,
            } => {
                current.require_recovery(turn.admission())?;
                if turn.admission().staging() != _admission {
                    return Err("native recovery admission changed".into());
                }
                turn.check_store(store)
            }
        }
    }
}

fn materialize_enrollment_held(
    admission: &super::codex_enrollment::Staging,
    deadline: WakeDeadline,
    work: Option<&super::account_work::Scope>,
    store: &super::accounts::Store,
    signer: &super::accounts::SigninTurn,
    _held: &AuthLock,
) -> Result<String, String> {
    materialize_enrollment_guarded(
        admission,
        deadline,
        work,
        &EnrollmentPublicationGuard::Legacy { store, signer },
        _held,
    )
}

fn materialize_enrollment_guarded(
    admission: &super::codex_enrollment::Staging,
    deadline: WakeDeadline,
    work: Option<&super::account_work::Scope>,
    guard: &EnrollmentPublicationGuard<'_>,
    _held: &AuthLock,
) -> Result<String, String> {
    let home = &admission.directory;
    require_private_enrollment_directory(home)?;
    guard.check(admission)?;
    if deadline.passed() || work.is_some_and(super::account_work::Scope::cancelled) {
        return Err("enrollment recovery work expired".into());
    }
    let Some(RefreshJournal::EnrollmentResponse {
        admission: recorded,
        predecessor,
        candidate,
        digest,
    }) = RefreshJournal::read(home).map_err(|_| "enrollment response unavailable")?
    else {
        return Err("no sealed enrollment response".into());
    };
    if recorded.as_ref() != admission
        || !admission.directories_current()
        || std::fs::canonicalize(home).ok().as_ref() != Some(home)
    {
        return Err("enrollment response admission changed".into());
    }
    let current = match std::fs::symlink_metadata(auth_path_in_home(home)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Ok(_) => Some(credential_digest(
            read_auth_file_bounded(home)
                .ok_or("enrollment credential unreadable")?
                .as_bytes(),
        )),
        Err(_) => return Err("enrollment credential unreadable".into()),
    };
    if current.as_ref() != Some(&digest) {
        if current != predecessor {
            return Err("enrollment credential was replaced".into());
        }
        let doc: Value =
            aterm_json::from_str(&candidate.0).map_err(|_| "invalid enrollment response")?;
        let parsed = stored_auth_from_doc(doc).ok_or("invalid enrollment response")?;
        if conflicting_users(&parsed) {
            return Err("conflicting enrollment identity".into());
        }
        guard.check(admission)?;
        let directories_current = admission.directories_current();
        if !directories_current
            || deadline.passed()
            || work.is_some_and(super::account_work::Scope::cancelled)
        {
            return Err("enrollment recovery work expired".into());
        }
        super::atomic_write::write_atomic(&auth_path_in_home(home), &candidate.0)
            .map_err(|_| "enrollment credential publication failed")?;
    }
    guard.check(admission)?;
    let directories_current = admission.directories_current();
    if !directories_current
        || deadline.passed()
        || work.is_some_and(super::account_work::Scope::cancelled)
    {
        return Err("enrollment recovery work expired".into());
    }
    flush_auth_directory(home).map_err(|_| "enrollment credential flush failed")?;
    Ok(digest)
}

pub(super) fn enrollment_bytes_current(
    admission: &super::codex_enrollment::Staging,
    digest: &str,
) -> bool {
    read_auth_file_bounded(&admission.directory)
        .is_some_and(|bytes| credential_digest(bytes.as_bytes()) == digest)
}

/// Compare the exact retained bytes under their original parent and actual
/// AuthLock. This read-only check supplies no producer or publication authority.
pub(super) fn retained_document_current(
    witness: &StagedWitness,
    auth: &AuthLock,
    expected: &[u8],
) -> bool {
    let Ok((path, _, _)) = witness.coordinates() else {
        return false;
    };
    let Some(home) = path.parent() else {
        return false;
    };
    let current = || {
        witness.matches_auth_file(path)
            && astream_sys::file_matches_path(&auth.0, &lock_path(path)).unwrap_or(false)
    };
    if expected.is_empty() || auth_path_in_home(home) != path || !current() {
        return false;
    }
    let matches = read_auth_file_bounded(home).is_some_and(|actual| {
        let actual = zeroize::Zeroizing::new(actual);
        aterm_digest::ct_eq(actual.as_bytes(), expected)
    });
    matches && current()
}

pub(super) fn sealed_enrollment_digest(
    admission: &super::codex_enrollment::Staging,
) -> Result<Option<String>, String> {
    match RefreshJournal::read(&admission.directory).map_err(|_| "enrollment journal unreadable")? {
        None => Ok(None),
        Some(RefreshJournal::EnrollmentResponse {
            admission: recorded,
            digest,
            ..
        }) if recorded.as_ref() == admission && enrollment_bytes_current(admission, &digest) => {
            Ok(Some(digest))
        }
        _ => Err("enrollment response changed".into()),
    }
}

/// A bounded, nonmutating hint only. Fresh admission and locked materialization recheck it.
pub(super) fn enrollment_response_available(admission: &super::codex_enrollment::Staging) -> bool {
    matches!(RefreshJournal::read(&admission.directory), Ok(Some(RefreshJournal::EnrollmentResponse { admission: recorded, .. })) if recorded.as_ref() == admission)
}

/// Remove only the response named by a fenced, trusted Store's durable Applied record.
/// No account writer is acquired under the credential lock and no credential is modified.
pub(super) fn cleanup_applied_enrollment(
    applied: &super::codex_enrollment::AppliedArtifact,
) -> Result<(), String> {
    let admission = applied.admission();
    let home = &admission.directory;
    if applied.generation() == 0
        || !admission.directories_current()
        || std::fs::canonicalize(home).ok().as_ref() != Some(home)
    {
        return Err("enrollment cleanup directory changed".into());
    }
    if matches!(std::fs::symlink_metadata(beside_auth(home, JOURNAL_SUFFIX)), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(());
    }
    let _held = AuthLock::try_take(&auth_path_in_home(home))
        .map_err(|_| "enrollment cleanup lock unavailable")?
        .ok_or("enrollment cleanup credential busy")?;
    require_private_enrollment_directory(home)?;
    match RefreshJournal::read(home).map_err(|_| "enrollment cleanup journal unreadable")? {
        None => Ok(()),
        Some(RefreshJournal::EnrollmentResponse {
            admission: recorded,
            digest,
            ..
        }) if recorded.as_ref() == admission && digest == applied.digest() => {
            RefreshJournal::discard(home).map_err(|_| "enrollment cleanup failed")?;
            flush_auth_directory(home).map_err(|_| "enrollment cleanup flush failed".into())
        }
        _ => Err("enrollment cleanup response differs".into()),
    }
}

fn refresh_token_digest(token: &str) -> String {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-codex-refresh-token-v1");
    hash.update(token.as_bytes());
    aterm_codec::hex::encode_lower(&hash.finalize())
}

fn flush_auth_directory(home: &Path) -> std::io::Result<()> {
    let path = auth_path_in_home(home);
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    aterm_durable::Flush::Durable.dir(parent)
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
        match self {
            Self::EnrollmentResponse {
                admission,
                predecessor,
                candidate,
                digest,
            } => json!({
                "aterm_codex_refresh": 3,
                "state": "enrollment_response",
                "admission": admission,
                "predecessor": predecessor,
                "candidate": candidate.0,
                "digest": digest,
            }),
            Self::Attempted { token } | Self::Rejected { token } => json!({
                "aterm_codex_refresh": 2,
                "state": if matches!(self, Self::Rejected { .. }) { "rejected" } else { "attempted" },
                "refresh_token_digest": token,
            }),
            Self::Response {
                base,
                token,
                tokens: t,
            } => json!({
                "aterm_codex_refresh": 1,
                "base": base,
                "refresh_token_digest": token,
                "access_token": t.access_token,
                "id_token": t.id_token,
                "refresh_token": t.refresh_token,
                "account_id": t.account_id,
                "last_refresh": t.last_refresh,
            }),
        }
    }

    fn of_value(value: &Value) -> Option<RefreshJournal> {
        if value.get("aterm_codex_refresh")?.as_u64()? == 3 {
            let object = value.as_object()?;
            if object.len() != 6 || value.get("state")?.as_str()? != "enrollment_response" {
                return None;
            }
            let admission: super::codex_enrollment::Staging =
                aterm_json::from_value(value.get("admission")?.clone()).ok()?;
            let candidate = value.get("candidate")?.as_str()?;
            let digest = value.get("digest")?.as_str()?;
            if candidate.len() > AUTH_FILE_MAX
                || credential_digest(candidate.as_bytes()) != digest
                || !admission.request.valid()
            {
                return None;
            }
            let predecessor = match value.get("predecessor")? {
                Value::Null => None,
                value => Some(value.as_str()?.to_owned()),
            };
            return Some(Self::EnrollmentResponse {
                admission: Box::new(admission),
                predecessor,
                candidate: NativeCredential(candidate.to_owned()),
                digest: digest.to_owned(),
            });
        }
        let text = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let token = match value.get("refresh_token_digest") {
            None | Some(Value::Null) => None,
            Some(value) => {
                let token = value.as_str()?;
                (token.len() == 64
                    && token
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)))
                .then_some(())?;
                Some(token.to_owned())
            }
        };
        match value.get("aterm_codex_refresh")?.as_u64()? {
            2 => {
                return match value.get("state")?.as_str()? {
                    "attempted" => Some(Self::Attempted { token: token? }),
                    "rejected" => Some(Self::Rejected { token: token? }),
                    _ => None,
                };
            }
            1 => {}
            _ => return None,
        }
        Some(Self::Response {
            base: text("base")?,
            token,
            tokens: RefreshedTokens {
                access_token: text("access_token")?,
                id_token: text("id_token"),
                refresh_token: text("refresh_token"),
                account_id: text("account_id"),
                last_refresh: text("last_refresh")?,
            },
        })
    }

    /// Kept durably beside `home`'s login (`0600`, with the rename flushed too).
    fn write(&self, home: &Path) -> std::io::Result<()> {
        self.write_before_publish(home, || Ok(()))
    }

    fn write_before_publish(
        &self,
        home: &Path,
        before_publish: impl FnOnce() -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let mut rendered = aterm_json::to_string(&self.to_value())
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        rendered.push('\n');
        super::atomic_write::write_atomic_before_publish(
            &beside_auth(home, JOURNAL_SUFFIX),
            &rendered,
            before_publish,
        )?;
        flush_auth_directory(home)
    }

    /// Only absence allows a new attempt. Unreadable, malformed, linked or non-regular
    /// journals could hold evidence of a spent token, so they are errors, never discarded.
    fn read(home: &Path) -> std::io::Result<Option<RefreshJournal>> {
        let invalid = || {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid Codex refresh journal",
            )
        };
        let path = beside_auth(home, JOURNAL_SUFFIX);
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                return Err(invalid());
            }
            Ok(_) => {}
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            options.custom_flags(0x0020_0000);
        }
        let file = options.open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(invalid());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            if metadata.file_attributes() & 0x0000_0400 != 0 {
                return Err(invalid());
            }
        }
        // A JSON string may escape every candidate byte as six bytes; metadata is bounded separately.
        let limit = (AUTH_FILE_MAX as u64)
            .saturating_mul(6)
            .saturating_add(65_536);
        if metadata.len() > limit {
            return Err(invalid());
        }
        let mut bytes = Vec::with_capacity(
            usize::try_from(metadata.len())
                .unwrap_or(0)
                .saturating_add(1),
        );
        file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(invalid());
        }
        aterm_json::from_slice::<Value>(&bytes)
            .ok()
            .as_ref()
            .and_then(Self::of_value)
            .map(Some)
            .ok_or_else(invalid)
    }

    fn discard(home: &Path) -> std::io::Result<()> {
        match std::fs::remove_file(beside_auth(home, JOURNAL_SUFFIX)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}

/// Recover a usable response before allowing another request. An attempted or rejected token
/// stays blocked until the token changes; unrelated metadata edits do not clear it. A response
/// is published only over its original authentication, and retained if that authentication
/// changed but still carries the spent token. Legacy responses have only the base binding.
/// The caller holds [`AuthLock`].
fn recover_journal_held(home: &Path) -> Result<(), Refreshed> {
    if !legacy_reader_allowed(home) {
        return Err(Refreshed::Failed);
    }
    let Some(journal) = RefreshJournal::read(home).map_err(|_| Refreshed::Failed)? else {
        return Ok(());
    };
    if matches!(&journal, RefreshJournal::EnrollmentResponse { .. }) {
        return Err(Refreshed::Failed);
    }
    let current = load_stored_auth(home).ok_or(Refreshed::Failed)?;
    let current_token = current.refresh_token.as_deref().map(refresh_token_digest);
    match journal {
        RefreshJournal::Attempted { token } if current_token.as_ref() == Some(&token) => {
            return Err(Refreshed::Failed);
        }
        RefreshJournal::Rejected { token } if current_token.as_ref() == Some(&token) => {
            return Err(Refreshed::Rejected);
        }
        RefreshJournal::Response {
            base,
            token,
            tokens,
        } => {
            if authentication_digest(&current.doc).as_deref() == Some(base.as_str()) {
                let doc = tokens.merged_into(current.doc).ok_or(Refreshed::Failed)?;
                stored_auth_from_doc(doc.clone()).ok_or(Refreshed::Failed)?;
                write_auth_doc(home, &doc).map_err(|_| Refreshed::Failed)?;
            } else if token.is_some() && token == current_token {
                // The login rename may have landed before its directory flush failed. When
                // the provider kept the same refresh token, recognize its applied response
                // without writing over metadata or treating it as an uncertain attempt.
                let applied = tokens
                    .merged_into(current.doc.clone())
                    .and_then(stored_auth_from_doc)
                    .is_some_and(|candidate| same_authentication(&current, &candidate));
                if !applied {
                    return Err(Refreshed::Failed);
                }
            }
        }
        _ => {}
    }
    // A replacement visible by name may still await its directory barrier. Keep the old
    // spent-token evidence until the current authentication's publication is durable.
    flush_auth_directory(home).map_err(|_| Refreshed::Failed)?;
    RefreshJournal::discard(home).map_err(|_| Refreshed::Failed)
}

/// [`recover_journal_held`] under [`AuthLock`], waited for until `deadline`, where a journal
/// stands beside `home`'s login.
fn recover_journal(home: &Path, deadline: Instant) {
    if std::fs::symlink_metadata(beside_auth(home, JOURNAL_SUFFIX)).is_err() {
        return;
    }
    if let Some(_held) = AuthLock::within(home, deadline) {
        let _ = recover_journal_held(home);
    }
}

/// Reads the ChatGPT account stored in `<home>/auth.json` directly from its OAuth JWT claims,
/// spawning no subprocess. Returns `None` when `auth.json` is absent, unreadable, API-key-only,
/// holds no decodable ChatGPT token, or its tokens name different known users or account emails.
#[must_use]
pub fn read_account_in_home(home: &Path) -> Option<Account> {
    let stored = load_stored_auth(home)?;
    let email = preferred_email(&stored).map(str::to_owned);
    check_stored_identity(&stored, email.as_deref()).ok()?;
    let StoredAuth {
        id_claims,
        access_claims,
        ..
    } = stored;
    if id_claims.is_none() && access_claims.is_none() {
        return None;
    }
    let plan = id_claims
        .and_then(|c| c.plan)
        .or_else(|| access_claims.and_then(|c| c.plan));
    Some(Account { email, plan })
}

fn preferred_email(stored: &StoredAuth) -> Option<&str> {
    stored
        .id_claims
        .as_ref()
        .and_then(|claims| claims.email.as_deref())
        .or_else(|| {
            stored
                .access_claims
                .as_ref()
                .and_then(|claims| claims.email.as_deref())
        })
}

/// Domain-separated native identity proof for a same-account renewal handoff.
/// Missing or disagreeing user/workspace claims never authorize following a login.
#[must_use]
pub fn renewal_identity(home: &Path) -> Option<super::codex_renewal::Identity> {
    let stored = load_stored_auth(home)?;
    stored_identity(&stored)
}

fn stored_identity(stored: &StoredAuth) -> Option<super::codex_renewal::Identity> {
    let account = stored.account_id.as_deref().filter(|s| !s.is_empty())?;
    // CodexAuth::get_account_id uses TokenData.account_id: an explicit selected
    // workspace intentionally outranks the JWT's default workspace.
    let TokenUser::Known(user) =
        token_user_claims(stored.id_claims.as_ref(), stored.access_claims.as_ref())
    else {
        return None;
    };
    Some(super::codex_renewal::Identity::from_native(account, &user))
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
/// before its request to its publication. A prior response is recovered first, and an attempt
/// recorded before sending. Only a transport failure known to have sent no bytes can retry the
/// same token. A response `admit` takes is journaled before the file is read again, so a failed
/// re-read or write keeps the rotated token. `publish` writes the refreshed login
/// (`write_auth_doc`; a conformance fixture passes its own).
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
    if !legacy_reader_allowed(home) {
        return Refreshed::Failed;
    }
    let prior_user = token_user(stored.id_token.as_deref(), &stored.access_token);
    if prior_user == TokenUser::Conflicting {
        return Refreshed::Failed;
    }
    let Some(_held) = AuthLock::within(home, deadline) else {
        return Refreshed::Failed;
    };
    if let Err(blocked) = recover_journal_held(home) {
        return blocked;
    }
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
    let token = refresh_token_digest(refresh_token);
    if (RefreshJournal::Attempted {
        token: token.clone(),
    })
    .write(home)
    .is_err()
    {
        // Nothing was sent, so removing our failed preparation cannot hide a spent token.
        let _ = RefreshJournal::discard(home);
        return Refreshed::Failed;
    }
    let left = deadline.saturating_duration_since(Instant::now());
    if left < MIN_REFRESH_BUDGET {
        let _ = RefreshJournal::discard(home);
        return Refreshed::Failed;
    }
    let response = http_client(token_url, left)
        .request(Method::Post, token_url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("User-Agent", "codex-cli")
        .header("originator", ORIGINATOR)
        .limit(MESSAGE_MAX)
        .backstop(left)
        .send_phased(body.as_bytes());
    let resp = match response {
        Ok(response) => response,
        Err(failed) => {
            if failed.phase == aterm_http::Phase::NotSent {
                let _ = RefreshJournal::discard(home);
            }
            return Refreshed::Failed;
        }
    };
    if resp.status() != 200 {
        return if refresh_refused(resp.status(), resp.body()) {
            let _ = (RefreshJournal::Rejected { token }).write(home);
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
    let journal = RefreshJournal::Response {
        base,
        token: Some(token.clone()),
        tokens: tokens.clone(),
    };
    let _ = journal.write(home);
    let Some(current) = load_stored_auth(home) else {
        return Refreshed::Failed;
    };
    if !same_authentication(stored, &current) {
        if current
            .refresh_token
            .as_deref()
            .map(refresh_token_digest)
            .as_deref()
            != Some(token.as_str())
        {
            let _ = RefreshJournal::discard(home);
        }
        return updated_disk_auth(stored, current)
            .map_or(Refreshed::Failed, |login| Refreshed::Done(Box::new(login)));
    }
    // Preserve independent metadata changes without discarding the response's
    // rotated refresh token when the complete authentication record is unchanged.
    let Some(candidate) = tokens
        .merged_into(current.doc)
        .and_then(stored_auth_from_doc)
    else {
        return Refreshed::Failed;
    };
    if publish(home, &candidate.doc).is_err() || flush_auth_directory(home).is_err() {
        return Refreshed::Failed;
    }
    let _ = RefreshJournal::discard(home);
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
    url.get(..origin.len())
        .filter(|prefix| prefix.eq_ignore_ascii_case(origin))
        .and_then(|_| url.get(origin.len()..))
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
    get_usage_once_using(usage_url, stored, wait, expected_email, &client)
}

fn get_usage_once_using(
    usage_url: &str,
    stored: &StoredAuth,
    wait: Duration,
    expected_email: Option<&str>,
    client: &Client,
) -> Option<(u16, Vec<u8>)> {
    check_stored_identity(stored, expected_email).ok()?;
    if wait.is_zero() || !is_allowed_auth_endpoint(usage_url) {
        return None;
    }
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
    if let Some(account_id) = request_account_id(stored) {
        req = req.header("ChatGPT-Account-Id", account_id);
    }
    let resp = req.send(&[]).ok()?;
    Some((resp.status(), resp.body().to_vec()))
}

fn request_account_id(stored: &StoredAuth) -> Option<&str> {
    stored
        .account_id
        .as_deref()
        .filter(|account| account.bytes().all(|byte| (0x20..0x7f).contains(&byte)))
}

/// Opaque local evidence of the credentials and route used for an auth observation.
/// This is neither provider identity proof nor a lease on the credential file.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CredentialContext([u8; 32]);

impl std::fmt::Debug for CredentialContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialContext([redacted])")
    }
}

impl CredentialContext {
    /// Private local persistence only; never include this value in logs or public output.
    pub(crate) fn to_private_wire(&self) -> String {
        format!("v1:{}", aterm_codec::hex::encode_lower(&self.0))
    }

    /// Read only the canonical, bounded private spelling, without accepting other versions.
    pub(crate) fn from_private_wire(text: &str) -> Option<Self> {
        let hex = text.strip_prefix("v1:")?;
        if !aterm_codec::hex::is_lower(hex, 64) {
            return None;
        }
        let mut bytes = [0; 32];
        aterm_codec::hex::decode_into(hex.as_bytes(), &mut bytes).then_some(Self(bytes))
    }

    fn for_login(stored: &StoredAuth, usage_url: &str, fallback_token_url: &str) -> Self {
        let mut hash = aterm_digest::Sha256::new();
        hash.update(b"aterm-codex-auth-observation-v1");
        let mut field = |value: Option<&str>| match value {
            None => hash.update([0]),
            Some(value) => {
                hash.update([1]);
                hash.update((value.len() as u64).to_be_bytes());
                hash.update(value.as_bytes());
            }
        };
        field(stored.id_token.as_deref());
        field(Some(&stored.access_token));
        field(stored.refresh_token.as_deref());
        field(request_account_id(stored));
        field(Some(usage_url));
        let authority = stored
            .refresh_token
            .as_ref()
            .and_then(|_| refresh_authority(stored, fallback_token_url));
        field(
            authority
                .as_ref()
                .map(|authority| authority.token_url.as_str()),
        );
        field(
            authority
                .as_ref()
                .map(|authority| authority.client_id.as_str()),
        );
        Self(hash.finalize())
    }
}

/// A provider result with the exact final credential context that produced it.
pub(crate) struct ObservedLogin {
    pub reading: Reading,
    pub context: Option<CredentialContext>,
}

/// File material and expected-identity applicability from one immutable snapshot.
pub(crate) struct CurrentLoginContext {
    pub context: CredentialContext,
    pub identity_matches: bool,
}

struct LoginSnapshot {
    reading: Reading,
    identity: Option<super::codex_renewal::Identity>,
    context: Option<CredentialContext>,
}

impl LoginSnapshot {
    fn unread() -> Self {
        Self {
            reading: Reading::Unread,
            identity: None,
            context: None,
        }
    }

    fn read(reading: Reading, stored: &StoredAuth, usage_url: &str, token_url: &str) -> Self {
        let context = (!matches!(&reading, Reading::Unread))
            .then(|| CredentialContext::for_login(stored, usage_url, token_url));
        let identity = if matches!(&reading, Reading::SignedOut) {
            None
        } else {
            stored_identity(stored)
        };
        Self {
            reading,
            identity,
            context,
        }
    }

    fn observed(self) -> ObservedLogin {
        ObservedLogin {
            reading: self.reading,
            context: self.context,
        }
    }
}

/// What one read of a Codex login's usage found ([`read_login_with`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    /// The login's usage.
    Usage(Usage),
    /// The provider refused a nonrefreshable access credential twice (`401`), and no newer
    /// rotation was read. A rejected single-use refresh whose rotation remains unresolved
    /// yields [`Self::Unread`] instead.
    SignedOut,
    /// Nothing was learned: no login file, no reader for the CLI, a login that names another
    /// user than its row expects, the network, a timeout (a refusal with no time left to
    /// confirm among them), an unresolved single-use refresh, a refused refreshed token,
    /// or an answer it could not read.
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
#[cfg(test)]
fn read_usage_with(
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
#[cfg(test)]
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
fn read_login_with(
    home: &Path,
    usage_url: &str,
    fallback_token_url: &str,
    wait: Duration,
    now: i64,
    expected_email: Option<&str>,
) -> Reading {
    read_login_snapshot(
        home,
        usage_url,
        fallback_token_url,
        wait,
        now,
        expected_email,
    )
    .reading
}

/// Successful usage tied to the immutable credential snapshot used by the HTTP
/// request, rather than a later read of a login file another process can replace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentifiedUsage {
    pub identity: super::codex_renewal::Identity,
    pub usage: Usage,
}

/// Read usage as [`read_login_with`] does, retaining the exact request identity.
/// A successful response without known account and user claims yields `None`.
#[must_use]
fn read_identified_login_with(
    home: &Path,
    usage_url: &str,
    fallback_token_url: &str,
    wait: Duration,
    now: i64,
    expected_email: Option<&str>,
) -> Option<IdentifiedUsage> {
    let snapshot = read_login_snapshot(
        home,
        usage_url,
        fallback_token_url,
        wait,
        now,
        expected_email,
    );
    Some(IdentifiedUsage {
        identity: snapshot.identity?,
        usage: snapshot.reading.usage()?,
    })
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CodexRowIdentity",
        action = "Poll",
        project = "aterm_agent::harness::codex_app_login::tests::row_identity_tests::row_identity_transition"
    )
)]
fn read_login_snapshot(
    home: &Path,
    usage_url: &str,
    fallback_token_url: &str,
    wait: Duration,
    now: i64,
    expected_email: Option<&str>,
) -> LoginSnapshot {
    read_login_snapshot_using(
        home,
        usage_url,
        fallback_token_url,
        wait,
        now,
        expected_email,
        None,
    )
}

fn read_login_snapshot_using(
    home: &Path,
    usage_url: &str,
    fallback_token_url: &str,
    wait: Duration,
    now: i64,
    expected_email: Option<&str>,
    usage_client: Option<&Client>,
) -> LoginSnapshot {
    read_login_snapshot_confirming(
        home,
        (usage_url, fallback_token_url),
        wait,
        now,
        expected_email,
        usage_client,
        |stored, deadline| {
            confirmed_signed_out(
                home,
                stored,
                (usage_url, fallback_token_url),
                (deadline, Instant::now(), || {}),
                now,
                expected_email,
                usage_client,
            )
        },
    )
}

fn read_login_snapshot_confirming(
    home: &Path,
    (usage_url, fallback_token_url): (&str, &str),
    wait: Duration,
    now: i64,
    expected_email: Option<&str>,
    usage_client: Option<&Client>,
    confirm: impl FnOnce(&StoredAuth, Instant) -> LoginSnapshot,
) -> LoginSnapshot {
    if !legacy_reader_allowed(home) {
        return LoginSnapshot::unread();
    }
    let get_usage = |stored: &StoredAuth, left: Duration| {
        if !legacy_reader_allowed(home) {
            return None;
        }
        match usage_client {
            Some(client) => get_usage_once_using(usage_url, stored, left, expected_email, client),
            None => get_usage_once(usage_url, stored, left, expected_email),
        }
    };
    let deadline = Instant::now()
        .checked_add(wait)
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(86_400));
    let left = || deadline.saturating_duration_since(Instant::now());
    recover_journal(home, deadline);
    let Some(mut stored) = load_stored_auth(home) else {
        return LoginSnapshot::unread();
    };
    if check_stored_identity(&stored, expected_email).is_err() {
        return LoginSnapshot::unread();
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
    let Some((mut status, mut body)) = get_usage(&stored, left()) else {
        return LoginSnapshot::unread();
    };
    if status == 401
        && let Some(reloaded) = reload_updated_disk_auth(home, &stored)
    {
        stored = reloaded;
        let Some(again) = get_usage(&stored, left()) else {
            return LoginSnapshot::unread();
        };
        (status, body) = again;
    }
    if status == 401 {
        if stored.refresh_token.is_none() {
            return confirm(&stored, deadline);
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
                let Some(again) = get_usage(&refreshed, left()) else {
                    return LoginSnapshot::unread();
                };
                (status, body) = again;
                stored = *refreshed;
            }
            // A rejected single-use refresh can race another writer's valid rotation.
            Refreshed::Rejected => {
                return confirm(&stored, deadline);
            }
            Refreshed::Failed => return LoginSnapshot::unread(),
        }
    }
    if !legacy_reader_allowed(home) {
        return LoginSnapshot::unread();
    }
    LoginSnapshot::read(
        usage_reading(status, &body, now),
        &stored,
        usage_url,
        fallback_token_url,
    )
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

/// Recheck a refused login without replaying its refresh. A published rotation is read
/// as the new login. An unresolved single-use refresh remains Unread even after another
/// access refusal: a missing response is not account sign-out. With no refresh token,
/// two actual access refusals identify the nonrefreshable credential as signed out.
/// The original deadline bounds every observation and request.
fn confirmed_signed_out(
    home: &Path,
    stored: &StoredAuth,
    (usage_url, fallback_token_url): (&str, &str),
    (deadline, started, mut observing): (Instant, Instant, impl FnMut()),
    now: i64,
    expected_email: Option<&str>,
    usage_client: Option<&Client>,
) -> LoginSnapshot {
    if Instant::now() >= deadline {
        return LoginSnapshot::unread();
    }
    let end = if stored.refresh_token.is_none() {
        started
    } else {
        let Some(end) = started
            .checked_add(ROTATION_HANG)
            .filter(|end| *end < deadline)
        else {
            return LoginSnapshot::unread();
        };
        end
    };
    let rotated = loop {
        if !legacy_reader_allowed(home) {
            return LoginSnapshot::unread();
        }
        if let Some(rotated) = reload_updated_disk_auth(home, stored) {
            break Some(rotated);
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break None;
        }
        observing();
        std::thread::sleep(LOCK_POLL.min(left));
    };
    let login = rotated.as_ref().unwrap_or(stored);
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() || !legacy_reader_allowed(home) {
        return LoginSnapshot::unread();
    }
    let response = match usage_client {
        Some(client) => get_usage_once_using(usage_url, login, left, expected_email, client),
        None => get_usage_once(usage_url, login, left, expected_email),
    };
    let Some((status, body)) = response else {
        return LoginSnapshot::unread();
    };
    if !legacy_reader_allowed(home) {
        return LoginSnapshot::unread();
    }
    if status == 401 && rotated.is_none() {
        if stored.refresh_token.is_some() {
            return LoginSnapshot::unread();
        }
        return LoginSnapshot::read(Reading::SignedOut, login, usage_url, fallback_token_url);
    }
    LoginSnapshot::read(
        usage_reading(status, &body, now),
        login,
        usage_url,
        fallback_token_url,
    )
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
    use_token: super::provider_read::Use,
) -> Option<Usage> {
    read_usage_in_home_for(home, user_home, wait, now, None, use_token)
}

/// [`read_usage_in_home`] for a row that expects `expected_email`.
pub(super) fn read_usage_in_home_for(
    home: &Path,
    user_home: &Path,
    wait: Duration,
    now: i64,
    expected_email: Option<&str>,
    use_token: super::provider_read::Use,
) -> Option<Usage> {
    read_login_in_home(home, user_home, wait, now, expected_email, use_token).usage()
}

/// [`read_usage_in_home_for`]'s [`Reading`] ([`read_login_with`]).
#[must_use]
pub fn read_login_in_home(
    home: &Path,
    user_home: &Path,
    wait: Duration,
    now: i64,
    expected_email: Option<&str>,
    use_token: super::provider_read::Use,
) -> Reading {
    if use_token
        .begin_home(super::provider_read::Read::CodexEndpoint, home, now)
        .is_err()
    {
        return Reading::Unread;
    }
    let usage_url = resolve_usage_url(home, user_home);
    let token_url = format!("{DEFAULT_ISSUER}/oauth/token");
    read_login_snapshot(home, &usage_url, &token_url, wait, now, expected_email).reading
}

/// Read a file-backed login while retaining the credentials actually used by its final request.
pub(crate) fn observed_in_home(
    home: &Path,
    user_home: &Path,
    wait: Duration,
    now: i64,
    expected_email: Option<&str>,
    use_token: super::provider_read::Use,
) -> ObservedLogin {
    if use_token
        .begin_home(super::provider_read::Read::CodexEndpoint, home, now)
        .is_err()
    {
        return LoginSnapshot::unread().observed();
    }
    if !file_context_available(home) {
        return LoginSnapshot::unread().observed();
    }
    let usage_url = resolve_usage_url(home, user_home);
    let token_url = format!("{DEFAULT_ISSUER}/oauth/token");
    read_login_snapshot(home, &usage_url, &token_url, wait, now, expected_email).observed()
}

/// Observe current semantic file credentials and route without refreshing or contacting a provider.
/// Missing, unreadable or non-file storage is unavailable, not proof that credentials changed.
pub(crate) fn current_context(home: &Path, user_home: &Path) -> Option<CredentialContext> {
    Some(current_login_context(home, user_home, None)?.context)
}

/// Capture exact material even when its identity contradicts the expected row.
/// Identity failure is not evidence of a credential change; both facts share one read.
pub(crate) fn current_login_context(
    home: &Path,
    user_home: &Path,
    expected_email: Option<&str>,
) -> Option<CurrentLoginContext> {
    if !file_context_available(home) {
        return None;
    }
    let stored = read_stored_auth(home)?;
    let email = preferred_email(&stored);
    let identity_matches = check_stored_identity(&stored, email).is_ok()
        && expected_email.is_none_or(|expected| {
            email.is_some_and(|email| super::accounts::email_matches(expected, email))
        });
    let context = CredentialContext::for_login(
        &stored,
        &resolve_usage_url(home, user_home),
        &format!("{DEFAULT_ISSUER}/oauth/token"),
    );
    Some(CurrentLoginContext {
        context,
        identity_matches,
    })
}

fn file_context_available(home: &Path) -> bool {
    auth_path_in_home(home)
        .parent()
        .is_some_and(crate::ai::facts::codex_file_store)
}

/// Read fresh usage using credentials in `home` and the route in the running
/// session's shared Codex configuration. Account storage does not select a route.
#[must_use]
pub fn read_identified_login_with_config(
    home: &Path,
    config_home: &Path,
    user_home: &Path,
    wait: Duration,
    now: i64,
    expected_email: Option<&str>,
    use_token: super::provider_read::Use,
) -> Option<IdentifiedUsage> {
    use_token
        .begin_home(super::provider_read::Read::CodexEndpoint, home, now)
        .ok()?;
    let usage_url = resolve_usage_url(config_home, user_home);
    let token_url = format!("{DEFAULT_ISSUER}/oauth/token");
    read_identified_login_with(home, &usage_url, &token_url, wait, now, expected_email)
}

/// Explicit native-fixture transport: verify a loopback HTTPS server against
/// supplied roots without changing platform trust or OAuth refresh transport.
/// Installed release builds cannot select this path.
#[cfg(debug_assertions)]
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "the native fixture carries explicit trust roots and consumed account authority"
)]
pub fn read_identified_login_with_fixture_roots(
    home: &Path,
    config_home: &Path,
    user_home: &Path,
    wait: Duration,
    now: i64,
    expected_email: Option<&str>,
    roots: Vec<Vec<u8>>,
    use_token: super::provider_read::Use,
) -> Option<IdentifiedUsage> {
    use_token
        .begin_home(super::provider_read::Read::CodexEndpoint, home, now)
        .ok()?;
    let usage_url = resolve_usage_url(config_home, user_home);
    if !usage_url.starts_with("https://") || !is_loopback_endpoint(&usage_url) {
        return None;
    }
    let client = Client::new(Trust::Roots(roots), ProxyMode::Direct, wait);
    let snapshot = read_login_snapshot_using(
        home,
        &usage_url,
        &format!("{DEFAULT_ISSUER}/oauth/token"),
        wait,
        now,
        expected_email,
        Some(&client),
    );
    Some(IdentifiedUsage {
        identity: snapshot.identity?,
        usage: snapshot.reading.usage()?,
    })
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

pub(super) fn bind_callback_listener(
    ports: &[u16],
    deadline: WakeDeadline,
) -> std::io::Result<TcpListener> {
    let mut last_err = std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "no callback port configured",
    );
    for &port in ports {
        if deadline.passed() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "callback listener bind deadline expired",
            ));
        }
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => return Ok(listener),
            Err(err) => last_err = err,
        }
    }
    Err(last_err)
}

#[derive(Debug)]
enum CallbackOutcome {
    Code(String),
    Failed,
    /// The sign-in's deadline passed, by sleep or awake time, before a callback came.
    Expired,
}

fn callback_io_budget(deadline: Instant, now: Instant) -> std::io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(now);
    if remaining.is_zero() {
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "native callback I/O deadline expired",
        ))
    } else {
        Ok(remaining)
    }
}

struct CallbackRead<R, N> {
    parent: Option<WakeDeadline>,
    deadline: Instant,
    read: R,
    now: N,
}

impl<R, N> Read for CallbackRead<R, N>
where
    R: FnMut(&mut [u8], Duration) -> std::io::Result<usize>,
    N: Fn() -> Instant,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let remaining = callback_io_budget(self.deadline, (self.now)())?;
        if self.parent.is_some_and(|parent| parent.passed()) {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        let read = (self.read)(buf, remaining)?;
        callback_io_budget(self.deadline, (self.now)())?;
        if self.parent.is_some_and(|parent| parent.passed()) {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        Ok(read)
    }
}

const CALLBACK_POLL: Duration = Duration::from_millis(50);

fn callback_stopped(
    deadline: WakeDeadline,
    cancelled: &AtomicBool,
    work: Option<&super::account_work::Scope>,
) -> bool {
    deadline.passed()
        || cancelled.load(Ordering::SeqCst)
        || work.is_some_and(super::account_work::Scope::cancelled)
}

fn callback_reader<'a>(
    stream: &'a mut TcpStream,
    deadline: WakeDeadline,
    cancelled: &'a AtomicBool,
    work: Option<&'a super::account_work::Scope>,
) -> impl Read + 'a {
    CallbackRead {
        parent: Some(deadline),
        deadline: deadline.instant(),
        now: Instant::now,
        read: move |buf: &mut [u8], _remaining: Duration| loop {
            if callback_stopped(deadline, cancelled, work) {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            stream.set_read_timeout(Some(deadline.remaining().min(CALLBACK_POLL)))?;
            let result = stream.read(buf);
            if callback_stopped(deadline, cancelled, work) {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            match result {
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                result => return result,
            }
        },
    }
}

fn write_callback_until(
    mut bytes: &[u8],
    deadline: Instant,
    now: impl Fn() -> Instant,
    mut write: impl FnMut(&[u8], Duration) -> std::io::Result<usize>,
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        let remaining = callback_io_budget(deadline, now())?;
        let written = write(bytes, remaining);
        callback_io_budget(deadline, now())?;
        match written {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "native callback reply made no write progress",
                ));
            }
            Ok(n) => bytes = &bytes[n..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn drain_callback(from: &mut impl Read) {
    let mut drain = [0u8; 1024];
    let mut drained = 0usize;
    while drained < CALLBACK_DRAIN_MAX {
        let take = drain.len().min(CALLBACK_DRAIN_MAX - drained);
        match from.read(&mut drain[..take]) {
            Ok(0) | Err(_) => break,
            Ok(n) => drained += n,
        }
    }
}

fn write_http_reply(
    stream: &mut TcpStream,
    deadline: WakeDeadline,
    cancelled: &AtomicBool,
    work: Option<&super::account_work::Scope>,
    status: &str,
    content_type: &str,
    body: &str,
) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let reply = deadline.within(CALLBACK_IO_WAIT);
    let _ = write_callback_until(
        head.as_bytes(),
        reply.instant(),
        Instant::now,
        |bytes, _remaining| loop {
            if callback_stopped(reply, cancelled, work) {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            stream.set_write_timeout(Some(reply.remaining().min(CALLBACK_POLL)))?;
            let result = stream.write(bytes);
            if callback_stopped(reply, cancelled, work) {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            match result {
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                result => return result,
            }
        },
    );
    let _ = stream.shutdown(std::net::Shutdown::Write);
    drain_callback(&mut callback_reader(
        stream,
        deadline.within(CALLBACK_DRAIN_WAIT),
        cancelled,
        work,
    ));
}

fn is_loopback_host_header(value: &str, port: u16) -> bool {
    let host = value.trim();
    if let Some(rest) = host.strip_prefix("[::1]") {
        return rest.is_empty()
            || rest
                .strip_prefix(':')
                .and_then(crate::parse_uint::<u16>)
                .is_some_and(|p| p == port);
    }
    let (name, port_part) = match host.split_once(':') {
        Some((n, p)) => (n, Some(p)),
        None => (host, None),
    };
    if !(name == "127.0.0.1" || name.eq_ignore_ascii_case("localhost")) {
        return false;
    }
    port_part.is_none_or(|p| crate::parse_uint::<u16>(p) == Some(port))
}

fn read_request_target(stream: &mut impl Read, port: u16) -> Option<String> {
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
    if method != "GET"
        || !matches!(version, "HTTP/1.0" | "HTTP/1.1")
        || !target.starts_with('/')
        || target.starts_with("//")
        || words.next().is_some()
    {
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

/// Retires the actual callback worker before the caller can release its own resources.
struct CallbackOwner {
    cancelled: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl CallbackOwner {
    fn join(&mut self) -> bool {
        self.cancelled.store(true, Ordering::SeqCst);
        let Some(worker) = self.worker.take() else {
            return true;
        };
        match worker.join() {
            Ok(()) => true,
            Err(payload) => {
                if let Err(secondary) =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(payload)))
                {
                    std::mem::forget(secondary);
                }
                false
            }
        }
    }
}

impl Drop for CallbackOwner {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

struct CallbackServer {
    listener: TcpListener,
    port: u16,
    expected_state: String,
    cancelled: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    tx: mpsc::Sender<CallbackOutcome>,
    deadline: WakeDeadline,
    work: Option<super::account_work::Scope>,
}

fn serve_callback_loop(server: CallbackServer) {
    let CallbackServer {
        listener,
        port,
        expected_state,
        cancelled,
        done,
        tx,
        deadline,
        work,
    } = server;
    while !callback_stopped(deadline, &cancelled, work.as_ref()) {
        let accept_until = deadline.within(Duration::from_millis(10)).instant();
        match aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Until(accept_until),
            aterm_uds::AcceptedMode::Blocking,
        ) {
            Ok((mut stream, _)) => {
                // The accepted descriptor retains the original budget through every head read.
                let target = read_request_target(
                    &mut callback_reader(
                        &mut stream,
                        deadline.within(CALLBACK_HEAD_WAIT),
                        &cancelled,
                        work.as_ref(),
                    ),
                    port,
                );
                if callback_stopped(deadline, &cancelled, work.as_ref()) {
                    break;
                }
                let Some(target) = target else {
                    write_http_reply(
                        &mut stream,
                        deadline,
                        &cancelled,
                        work.as_ref(),
                        "400 Bad Request",
                        "text/plain",
                        "Bad Request",
                    );
                    continue;
                };
                let (path, query) = target
                    .split_once('?')
                    .map_or((target.as_str(), ""), |(p, q)| (p, q));
                if path != "/auth/callback" {
                    write_http_reply(
                        &mut stream,
                        deadline,
                        &cancelled,
                        work.as_ref(),
                        "404 Not Found",
                        "text/plain",
                        "Not Found",
                    );
                    continue;
                }
                let Some(params) = parse_strict_query_params(query) else {
                    write_http_reply(
                        &mut stream,
                        deadline,
                        &cancelled,
                        work.as_ref(),
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
                        deadline,
                        &cancelled,
                        work.as_ref(),
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
                        deadline,
                        &cancelled,
                        work.as_ref(),
                        "400 Bad Request",
                        "text/plain",
                        "Duplicate callback parameter",
                    );
                    if callback_stopped(deadline, &cancelled, work.as_ref()) {
                        break;
                    }
                    done.store(true, Ordering::SeqCst);
                    let _ = tx.send(CallbackOutcome::Failed);
                    return;
                };
                if err_param.is_some_and(|e| !e.is_empty()) {
                    write_http_reply(
                        &mut stream,
                        deadline,
                        &cancelled,
                        work.as_ref(),
                        "200 OK",
                        "text/html; charset=utf-8",
                        "<!doctype html><title>Sign-in failed</title><p>Sign-in was not completed.</p>",
                    );
                    if callback_stopped(deadline, &cancelled, work.as_ref()) {
                        break;
                    }
                    done.store(true, Ordering::SeqCst);
                    let _ = tx.send(CallbackOutcome::Failed);
                    return;
                }
                let Some(code) = code_param.filter(|c| !c.is_empty()) else {
                    write_http_reply(
                        &mut stream,
                        deadline,
                        &cancelled,
                        work.as_ref(),
                        "400 Bad Request",
                        "text/plain",
                        "Missing authorization code",
                    );
                    if callback_stopped(deadline, &cancelled, work.as_ref()) {
                        break;
                    }
                    done.store(true, Ordering::SeqCst);
                    let _ = tx.send(CallbackOutcome::Failed);
                    return;
                };
                write_http_reply(
                    &mut stream,
                    deadline,
                    &cancelled,
                    work.as_ref(),
                    "200 OK",
                    "text/html; charset=utf-8",
                    "<!doctype html><title>Signed in</title><p>Sign-in complete. You may close this window.</p>",
                );
                if callback_stopped(deadline, &cancelled, work.as_ref()) {
                    break;
                }
                done.store(true, Ordering::SeqCst);
                let _ = tx.send(CallbackOutcome::Code(code.to_owned()));
                return;
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::ConnectionAborted
                ) => {}
            Err(_) => {
                if callback_stopped(deadline, &cancelled, work.as_ref()) {
                    break;
                }
                done.store(true, Ordering::SeqCst);
                let _ = tx.send(CallbackOutcome::Failed);
                return;
            }
        }
    }
    // The deadline passed with no callback, unless the sign-in cancelled the wait: said so before
    // `done`, so a sign-in that sees `done` reads a sign-in out of time, never a failed one.
    if !cancelled.load(Ordering::SeqCst) {
        let _ = tx.send(CallbackOutcome::Expired);
    }
    done.store(true, Ordering::SeqCst);
}

/// Per-invocation custody, sealed only by the native producer after durable publication.
#[derive(Default)]
pub struct PublicationCustody(std::sync::OnceLock<SavedPublication>);

impl std::fmt::Debug for PublicationCustody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PublicationCustody")
            .finish_non_exhaustive()
    }
}

struct SavedPublication {
    deadline: WakeDeadline,
    target: PublicationTarget,
    digest: String,
    work: Option<super::account_work::Cancellation>,
}

struct PublicationTarget {
    home: std::path::PathBuf,
    #[cfg(unix)]
    identity: aterm_uds::directory_identity::Identity,
}

impl PublicationTarget {
    fn capture(home: &Path) -> Option<Self> {
        let home = std::fs::canonicalize(home).ok()?;
        #[cfg(unix)]
        let identity = aterm_uds::directory_identity::read(&home).ok()?;
        Some(Self {
            home,
            #[cfg(unix)]
            identity,
        })
    }

    fn current(&self, home: &Path) -> bool {
        if std::fs::canonicalize(home).ok().as_ref() != Some(&self.home) {
            return false;
        }
        #[cfg(unix)]
        {
            aterm_uds::directory_identity::read(home).ok() == Some(self.identity)
        }
        #[cfg(not(unix))]
        {
            true
        }
    }
}

impl PublicationCustody {
    pub(super) fn is_saved(&self) -> bool {
        self.0.get().is_some()
    }
    fn seal(
        &self,
        target: PublicationTarget,
        deadline: WakeDeadline,
        work: Option<&super::account_work::Scope>,
        digest: String,
    ) -> Option<()> {
        if !target.current(&target.home)
            || credential_digest(read_auth_file_bounded(&target.home)?.as_bytes()) != digest
        {
            return None;
        }
        self.0
            .set(SavedPublication {
                deadline,
                target,
                digest,
                work: work.map(super::account_work::Scope::cancellation),
            })
            .ok()
    }

    pub(super) fn require_current(&self, home: &Path) -> Result<(), String> {
        let Some(saved) = self.0.get() else {
            return Ok(());
        };
        let bytes_current = read_auth_file_bounded(home)
            .is_some_and(|bytes| credential_digest(bytes.as_bytes()) == saved.digest);
        if !bytes_current || !saved.target.current(home) {
            return Err("native publication work or credential changed".into());
        }
        self.require_work_current()
    }

    /// A final work fence after the caller's last filesystem authority observation.
    pub(super) fn require_work_current(&self) -> Result<(), String> {
        if self.0.get().is_some_and(|saved| {
            saved
                .work
                .as_ref()
                .is_some_and(super::account_work::Cancellation::cancelled)
                || saved.deadline.passed()
        }) {
            return Err("native publication work stopped".into());
        }
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct NativeDestination<'a> {
    pub enrollment: Option<&'a super::codex_enrollment::Artifact>,
    pub publication: Option<&'a PublicationCustody>,
}

struct CodeExchange<'a> {
    native_scope: Option<&'a super::native_enrollment_admission::NativeBrowserTurn>,
    enrollment: Option<&'a super::codex_enrollment::Artifact>,
    publication: Option<&'a PublicationCustody>,
    home: &'a Path,
    endpoints: &'a Endpoints,
    redirect_uri: &'a str,
    code: &'a str,
    code_verifier: &'a str,
    deadline: WakeDeadline,
    work: Option<&'a super::account_work::Scope>,
    now: i64,
}

#[derive(Debug, PartialEq, Eq)]
enum ExchangeError {
    Unsealed,
    Retained,
    Protected,
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::harness) enum EnrollmentCut {
    BeforeSeal,
    AfterSeal,
    AfterAuthWrite,
    AfterAuthFlush,
}

impl CodeExchange<'_> {
    fn exchange(self) -> Result<Option<String>, ExchangeError> {
        let mut entropy = [0u8; 32];
        let owner = SystemRandom::new()
            .fill(&mut entropy)
            .ok()
            .and_then(|()| base64url_encode(&entropy));
        self.persist(owner.as_deref())
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "CodexLoginPublication",
            action = "Exchange",
            project = "aterm_agent::harness::codex_app_login::tests::login_transition"
        )
    )]
    fn persist(self, owner: Option<&str>) -> Result<Option<String>, ExchangeError> {
        self.persist_using(owner, |home, deadline| {
            AuthLock::within(home, deadline.instant())
        })
    }

    fn persist_using(
        self,
        owner: Option<&str>,
        take_lock: impl FnOnce(&Path, WakeDeadline) -> Option<AuthLock>,
    ) -> Result<Option<String>, ExchangeError> {
        self.persist_inner(
            owner,
            take_lock,
            #[cfg(test)]
            None,
        )
    }

    fn persist_inner(
        self,
        owner: Option<&str>,
        take_lock: impl FnOnce(&Path, WakeDeadline) -> Option<AuthLock>,
        #[cfg(test)] cut: Option<EnrollmentCut>,
    ) -> Result<Option<String>, ExchangeError> {
        self.persist_capturing_inner(
            owner,
            None::<NoGrantCapture>,
            take_lock,
            #[cfg(test)]
            cut,
        )
        .0
    }

    #[cfg(all(test, unix))]
    fn persist_capturing<C: CodeGrantCapture>(
        self,
        owner: Option<&str>,
        capture: Option<C>,
    ) -> (Option<Option<String>>, Option<C::Output>) {
        let (result, captured) = self.persist_capturing_result(owner, capture);
        (result.ok(), captured)
    }

    fn persist_capturing_result<C: CodeGrantCapture>(
        self,
        owner: Option<&str>,
        capture: Option<C>,
    ) -> (Result<Option<String>, ExchangeError>, Option<C::Output>) {
        self.persist_capturing_inner(
            owner,
            capture,
            |home, deadline| AuthLock::within(home, deadline.instant()),
            #[cfg(test)]
            None,
        )
    }

    // The admitted enrollment journal and the managed grant carrier are
    // independent custody channels. Publication failure must retain the carrier;
    // carrier existence must never pretend an enrollment journal was sealed.
    fn persist_capturing_inner<C: CodeGrantCapture>(
        self,
        owner: Option<&str>,
        capture: Option<C>,
        take_lock: impl FnOnce(&Path, WakeDeadline) -> Option<AuthLock>,
        #[cfg(test)] cut: Option<EnrollmentCut>,
    ) -> (Result<Option<String>, ExchangeError>, Option<C::Output>) {
        self.persist_capturing_with_request_clock(
            owner,
            capture,
            take_lock,
            #[cfg(test)]
            cut,
            Instant::now,
        )
    }

    fn persist_capturing_with_request_clock<C: CodeGrantCapture>(
        self,
        owner: Option<&str>,
        mut capture: Option<C>,
        take_lock: impl FnOnce(&Path, WakeDeadline) -> Option<AuthLock>,
        #[cfg(test)] cut: Option<EnrollmentCut>,
        request_started: impl FnOnce() -> Instant,
    ) -> (Result<Option<String>, ExchangeError>, Option<C::Output>) {
        let mut retained = None;
        let mut response_sealed = false;
        let mut protected = false;
        let home = self.home;
        let mut dispatched = false;
        let result = (|| {
            let Self {
                native_scope,
                enrollment,
                publication,
                home,
                endpoints,
                redirect_uri,
                code,
                code_verifier,
                deadline,
                work,
                now,
            } = self;
            if publication.is_some_and(PublicationCustody::is_saved) {
                protected = true;
                return None;
            }
            if let Some(enrollment) = enrollment {
                enrollment.bind_deadline(deadline).ok()?;
            }
            let owner = owner.filter(|owner| !owner.is_empty())?;
            if let Some(scope) = native_scope {
                scope.enter_exchange(deadline, work).ok()?;
            }
            let token_url = &endpoints.token_url;
            if work.is_some_and(super::account_work::Scope::cancelled)
                || deadline.passed()
                || !is_allowed_auth_endpoint(token_url)
            {
                return None;
            }
            // Take the publication lock before spending the authorization code. A lock refusal
            // must not leave a successful exchange with nowhere serialized to save its tokens.
            // Preparing the destination first also lets a first sign-in take the lock.
            prepare_auth_dest(home).ok()?;
            let publication_target = if publication.is_some() {
                Some(PublicationTarget::capture(home)?)
            } else {
                None
            };
            // Reopen the exact prepared owner claim under exchange admission,
            // before taking the staged credential lock.
            if let Some(capture) = capture.as_mut() {
                capture.claim_exchange().ok()?;
            }
            let _held = take_lock(home, deadline.within(SIGN_IN_LOCK_HANG))?;
            let existing_journal = RefreshJournal::read(home).ok()?;
            if matches!(
                existing_journal.as_ref(),
                Some(RefreshJournal::EnrollmentResponse { .. })
            ) {
                protected = true;
                return None;
            }
            let predecessor = if enrollment.is_some() || capture.is_some() {
                match std::fs::symlink_metadata(auth_path_in_home(home)) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Ok(_) => Some(credential_digest(read_auth_file_bounded(home)?.as_bytes())),
                    Err(_) => return None,
                }
            } else {
                None
            };
            if let Some(enrollment) = enrollment {
                if !enrollment.staging().directories_current() {
                    protected = true;
                    return None;
                }
                if std::fs::canonicalize(home).ok().as_ref()
                    != Some(&enrollment.staging().directory)
                {
                    return None;
                }
                // A previously sealed native response belongs to its admitted attempt.
                if existing_journal.is_some() {
                    return None;
                }
            }
            match std::fs::symlink_metadata(beside_auth(home, JOURNAL_SUFFIX)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Ok(md) if md.is_file() && !md.file_type().is_symlink() => {}
                Ok(_) | Err(_) => return None,
            }
            let form = encode_form(&[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_ID),
                ("code", code),
                ("redirect_uri", redirect_uri),
                ("code_verifier", code_verifier),
            ]);
            if let Some(capture) = capture.as_mut() {
                capture.begin_exchange().ok()?;
            }
            if publication_target
                .as_ref()
                .is_some_and(|target| !target.current(home))
                || work.is_some_and(super::account_work::Scope::cancelled)
            {
                return None;
            }
            // Scope observation can wait on its lifetime state mutex. Only
            // the budget left after that wait can admit this single POST.
            let wait = exchange_wait(deadline.remaining(), request_started());
            if wait.is_zero() {
                return None;
            }
            if let Some(scope) = native_scope {
                scope.check_exchange().ok()?;
            }
            // From this point any transport error may have reached the provider.
            dispatched = true;
            // Test transport is selected only after declared endpoint,
            // native exchange, owner claim, stage/Auth and send-budget checks.
            // Provenance below continues to use the original token_url.
            #[cfg(test)]
            let fixture_contact = tests::code_contact_url(token_url, native_scope, enrollment);
            #[cfg(test)]
            let contact_url = &fixture_contact;
            #[cfg(not(test))]
            let contact_url = token_url;
            let resp = http_client(contact_url, wait)
                .request(Method::Post, contact_url)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .header("Accept", "application/json")
                .header("User-Agent", "codex-cli")
                .header("originator", ORIGINATOR)
                .limit(MESSAGE_MAX)
                .backstop(wait)
                .send(form.as_bytes())
                .ok()?;
            if resp.status() != 200 {
                // A completed TLS response may contain unexpected sensitive
                // bytes even when it cannot authorize a native grant.
                if let Some(capture) = capture.take() {
                    let rejection =
                        StagedWitness::open(&auth_path_in_home(home))
                            .ok()
                            .and_then(|witness| {
                                CodeRejection::mint(resp.status(), resp.body(), witness, &_held)
                            });
                    retained = Some(match rejection {
                        Some(rejection) => capture.retain_rejected(rejection),
                        None => capture.retain_unverified(resp.body()),
                    });
                }
                return None;
            }
            let built = (|| {
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
                        format!("{}/oauth/token", endpoints.issuer.trim_end_matches('/'))
                            == *token_url
                    })
                    .map(|_| json!({"issuer":endpoints.issuer, "client_id":CLIENT_ID}));
                let stamp =
                    aterm_types::rfc3339::format_rfc3339(u64::try_from(now.max(0)).unwrap_or(0));
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
                Some((doc, email))
            })();
            let Some((doc, email)) = built else {
                if let Some(capture) = capture.take() {
                    retained = Some(capture.retain_unverified(resp.body()));
                }
                return None;
            };
            if let Some(capture) = capture.take() {
                let mut bytes = match aterm_json::to_vec(&doc) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        retained = Some(capture.retain_unverified(resp.body()));
                        return None;
                    }
                };
                bytes.push(b'\n');
                let identity = stored_auth_from_doc(doc.clone())
                    .filter(|stored| stored.refresh_token.is_some())
                    .and_then(|stored| stored_identity(&stored));
                let witness = StagedWitness::open(&auth_path_in_home(home));
                let output = match (identity, witness) {
                    (Some(identity), Ok(witness)) => {
                        match FreshCodeGrant::mint(
                            identity,
                            owner,
                            bytes,
                            witness,
                            predecessor.as_deref(),
                            &_held,
                        ) {
                            Ok(grant) => capture.retain_verified(grant),
                            Err(bytes) => {
                                let bytes = Secret::from_vec(bytes);
                                capture.retain_unverified(bytes.expose())
                            }
                        }
                    }
                    _ => {
                        let bytes = Secret::from_vec(bytes);
                        capture.retain_unverified(bytes.expose())
                    }
                };
                let durable = C::verified_durable(&output);
                retained = Some(output);
                if !durable {
                    return None;
                }
            }
            if let Some(enrollment) = enrollment {
                if !enrollment.staging().directories_current() {
                    protected = true;
                    return None;
                }
                if retains_credential_custody(home) {
                    protected = true;
                    return None;
                }
                let mut candidate = aterm_json::to_string(&doc).ok()?;
                candidate.push('\n');
                if candidate.len() > AUTH_FILE_MAX {
                    return None;
                }
                let current = match std::fs::symlink_metadata(auth_path_in_home(home)) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Ok(_) => Some(credential_digest(read_auth_file_bounded(home)?.as_bytes())),
                    Err(_) => return None,
                };
                let receipt = RefreshJournal::EnrollmentResponse {
                    admission: Box::new(enrollment.staging().clone()),
                    predecessor: predecessor.clone(),
                    digest: credential_digest(candidate.as_bytes()),
                    candidate: NativeCredential(candidate.clone()),
                };
                #[cfg(test)]
                let saved = if cut == Some(EnrollmentCut::BeforeSeal) {
                    receipt.write_before_publish(home, || {
                        Err(std::io::ErrorKind::PermissionDenied.into())
                    })
                } else {
                    receipt.write(home)
                };
                #[cfg(not(test))]
                let saved = receipt.write(home);
                response_sealed = saved.is_ok()
                    || RefreshJournal::read(home).ok().flatten().as_ref() == Some(&receipt);
                saved.ok()?;
                #[cfg(test)]
                if cut == Some(EnrollmentCut::AfterSeal) {
                    return None;
                }
                if current != predecessor {
                    return None;
                }
                super::atomic_write::write_atomic(&auth_path_in_home(home), &candidate).ok()?;
                #[cfg(test)]
                if cut == Some(EnrollmentCut::AfterAuthWrite) {
                    return None;
                }
                flush_auth_directory(home).ok()?;
                #[cfg(test)]
                if cut == Some(EnrollmentCut::AfterAuthFlush) {
                    return None;
                }
                if let Some(publication) = publication {
                    publication.seal(
                        publication_target?,
                        deadline,
                        work,
                        credential_digest(candidate.as_bytes()),
                    )?;
                }
                return Some(email);
            }
            write_auth_doc(home, &doc).ok()?;
            flush_auth_directory(home).ok()?;
            RefreshJournal::discard(home).ok()?;
            flush_auth_directory(home).ok()?;
            if let Some(publication) = publication {
                response_sealed = true;
                let mut candidate = aterm_json::to_string(&doc).ok()?;
                candidate.push('\n');
                publication.seal(
                    publication_target?,
                    deadline,
                    work,
                    credential_digest(candidate.as_bytes()),
                )?;
            }
            Some(email)
        })();
        if !dispatched && let Some(capture) = capture.take() {
            retained = Some(capture.not_dispatched());
        }
        let protected =
            protected || (!response_sealed && result.is_none() && retains_credential_custody(home));
        let result = result.ok_or(if protected {
            ExchangeError::Protected
        } else if response_sealed {
            ExchangeError::Retained
        } else {
            ExchangeError::Unsealed
        });
        (result, retained)
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
    deadline: WakeDeadline,
) -> Drove {
    login_with_custody(
        home,
        endpoints,
        approver,
        hosts,
        email,
        deadline,
        None::<NoGrantCapture>,
    )
    .into_parts()
    .0
}

pub(in crate::harness) fn login_with_custody<C: CodeGrantCapture>(
    home: &Path,
    endpoints: &Endpoints,
    approver: &mut dyn Approver,
    hosts: &[&str],
    email: Option<&str>,
    deadline: WakeDeadline,
    capture: Option<C>,
) -> CapturedLogin<C::Output> {
    login_with_enrollment_custody(
        home,
        endpoints,
        approver,
        hosts,
        email,
        deadline,
        (NativeDestination::default(), capture),
    )
}

pub(in crate::harness) fn login_with_enrollment_custody<C: CodeGrantCapture>(
    home: &Path,
    endpoints: &Endpoints,
    approver: &mut dyn Approver,
    hosts: &[&str],
    email: Option<&str>,
    deadline: WakeDeadline,
    custody: (NativeDestination<'_>, Option<C>),
) -> CapturedLogin<C::Output> {
    let (destination, mut capture) = custody;
    let native_scope = approver.native_enrollment_scope();
    let mut retained = None;
    let drove = (|| {
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
        if deadline.passed() {
            return Drove {
                outcome: Outcome::Hung,
                host,
            };
        }

        let listener = match match &native_scope {
            Some(scope) => scope.callback().map_err(std::io::Error::other),
            None => bind_callback_listener(&endpoints.callback_ports, deadline),
        } {
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
        let auth_url =
            build_authorize_url(&endpoints.issuer, &redirect_uri, &code_challenge, &state);
        let host = https_host(&auth_url);
        if !pinned(&auth_url, hosts) {
            return Drove {
                outcome: Outcome::OffPin,
                host,
            };
        }

        let work = approver.work_scope();
        let cancelled = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let mut server = {
            let worker_cancelled = Arc::clone(&cancelled);
            let done = Arc::clone(&done);
            let expected_state = state;
            let worker_work = work.clone();
            let run = move || {
                serve_callback_loop(CallbackServer {
                    listener,
                    port,
                    expected_state,
                    cancelled: worker_cancelled,
                    done,
                    tx,
                    deadline,
                    work: worker_work,
                });
            };
            let run: Box<dyn FnOnce() + Send> = match work.clone() {
                Some(scope) => Box::new(scope.wrap(run)),
                None => Box::new(run),
            };
            let worker = match std::thread::Builder::new()
                .name("codex-callback".to_owned())
                .spawn(run)
            {
                Ok(worker) => worker,
                Err(_) => {
                    return Drove {
                        outcome: Outcome::Failed,
                        host,
                    };
                }
            };
            CallbackOwner {
                cancelled: Arc::clone(&cancelled),
                worker: Some(worker),
            }
        };

        let finished = || done.load(Ordering::SeqCst);
        let page = match approver.approve(&Handoff {
            url: &auth_url,
            code: None,
            completion: Completion::Loopback,
            hosts,
            email,
            finished: &finished,
            deadline: super::login_drive::Deadline::waking(deadline, &Instant::now),
        }) {
            Approval::Approved { page } => page,
            Approval::NotRun(why) => {
                let _ = server.join();
                return Drove {
                    outcome: Outcome::PageNotOpened(why),
                    host,
                };
            }
            Approval::NeedsYou(why) => {
                let _ = server.join();
                return Drove {
                    outcome: Outcome::NeedsYou(why),
                    host,
                };
            }
            Approval::NeedsSignIn(why) => {
                let _ = server.join();
                return Drove {
                    outcome: Outcome::NeedsSignIn(why),
                    host,
                };
            }
            Approval::Refused(why) => {
                let _ = server.join();
                return Drove {
                    outcome: Outcome::Refused(why),
                    host,
                };
            }
            Approval::BotCheck(why) => {
                let _ = server.join();
                return Drove {
                    outcome: Outcome::BotCheck(why),
                    host,
                };
            }
        };

        // One tick at a time, so a callback wait the host slept past ends within a tick of its waking.
        let callback = deadline.recv(&rx);
        if !server.join() {
            return Drove {
                outcome: Outcome::Failed,
                host,
            };
        }

        let code = match callback {
            Ok(CallbackOutcome::Code(code)) => code,
            Ok(CallbackOutcome::Failed) => {
                return Drove {
                    outcome: Outcome::Failed,
                    host,
                };
            }
            Ok(CallbackOutcome::Expired) | Err(mpsc::RecvTimeoutError::Timeout) => {
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

        // Retain the original work budget across publication-lock admission and exchange.
        let left = deadline.remaining();
        if left.is_zero() {
            return Drove {
                outcome: Outcome::Hung,
                host,
            };
        }
        let now = crate::supervise::limit::unix_now();
        let exchange = CodeExchange {
            native_scope: native_scope.as_deref(),
            enrollment: destination.enrollment,
            publication: destination.publication,
            home,
            endpoints,
            redirect_uri: &redirect_uri,
            code: &code,
            code_verifier: &code_verifier,
            deadline,
            work: work.as_ref(),
            now,
        };
        let (exchanged, output) = match capture.take() {
            None => (exchange.exchange(), None),
            Some(capture) => {
                let mut entropy = [0u8; 32];
                let owner = SystemRandom::new()
                    .fill(&mut entropy)
                    .ok()
                    .and_then(|()| base64url_encode(&entropy));
                exchange.persist_capturing_result(owner.as_deref(), Some(capture))
            }
        };
        retained = output;
        match exchanged {
            Ok(_)
                if deadline.passed()
                    || work
                        .as_ref()
                        .is_some_and(super::account_work::Scope::cancelled) =>
            {
                Drove {
                    outcome: Outcome::CompletedHeld,
                    host,
                }
            }
            Ok(signed_in_email) => Drove {
                outcome: Outcome::SignedIn {
                    email: signed_in_email,
                    page,
                },
                host,
            },
            Err(ExchangeError::Retained) => Drove {
                outcome: Outcome::CompletedHeld,
                host,
            },
            Err(ExchangeError::Protected) => Drove {
                outcome: Outcome::CustodyHeld,
                host,
            },
            Err(ExchangeError::Unsealed) => Drove {
                outcome: Outcome::Failed,
                host,
            },
        }
    })();
    // The capture remains here only when CodeExchange was never invoked.
    // Once invoked, that exchange owns all sent/unknown classification.
    if let Some(capture) = capture.take() {
        retained = Some(capture.not_dispatched());
    }
    CapturedLogin { drove, retained }
}

/// Runs Codex's native loopback PKCE browser sign-in in `home` against OpenAI's production OAuth
/// endpoints ([`Endpoints::default`]).
pub fn login_in_home(
    home: &Path,
    approver: &mut dyn Approver,
    hosts: &[&str],
    email: Option<&str>,
    deadline: WakeDeadline,
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

#[path = "codex_native_usage_http.rs"]
mod native_usage_http;
pub(in crate::harness) use native_usage_http::{
    LegacyUsageRead, native_legacy_observation, native_usage_probe, native_usage_route,
};

#[cfg(test)]
#[path = "codex_app_login_tests.rs"]
pub(crate) mod tests;
