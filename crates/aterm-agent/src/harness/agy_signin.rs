// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! agy's native headless authorization handoff. The native CLI owns PKCE and its grant;
//! a successful run still requires canonical account verification before publication.

use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::wake_deadline::WakeDeadline;

use aterm_secrets::SecretBytes;

#[path = "agy_signin_observe.rs"]
mod observe;
#[path = "agy_signin_recovery.rs"]
mod recovery;
pub(super) use observe::NativeProfile;
#[cfg(not(test))]
pub(super) use observe::read_native_profile;

pub use super::login_drive::RecoveryKey;

const CALLBACK: &str = "https://antigravity.google/oauth-callback";
const LIMIT: usize = 64 * 1024;
const SCOPES: &[&str] = &[
    "openid",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/cclog",
    "https://www.googleapis.com/auth/experimentsandconfigs",
    "https://www.googleapis.com/auth/aicode",
];

/// Quiet failures contain no native output, account address, URL or authorization code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Profile,
    Process,
    Deadline,
    Output,
    Authorization,
    Callback,
    Native,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Profile => "Private agy login profile is unavailable",
            Self::Process => "Native agy login could not start or be controlled",
            Self::Deadline => "Native agy authorization exceeded its deadline",
            Self::Output => "Native agy authorization output exceeded its bound",
            Self::Authorization => "Native agy Google authorization request was refused",
            Self::Callback => "Native agy Google callback did not match its request",
            Self::Native => "Native agy did not complete its authorization",
        })
    }
}

/// An admitted request captured from the exact native child, never from mail or a webpage.
/// No printable representation exposes its OAuth query.
pub struct Authorization {
    url: SecretBytes,
    state: SecretBytes,
    binding: [u8; 32],
}

fn one(url: &str, key: &str) -> Result<String, Error> {
    let values = super::headless_oauth::query_values(url, key);
    if values.len() != 1
        || values[0].is_empty()
        || values[0].len() > 8192
        || values[0]
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(Error::Authorization);
    }
    Ok(values[0].clone())
}

impl Authorization {
    fn admit(url: &str) -> Result<Self, Error> {
        if url.len() > 16 * 1024
            || url.contains('#')
            || url.split('?').next() != Some("https://accounts.google.com/o/oauth2/auth")
            || super::login_drive::https_host(url).as_deref() != Some("accounts.google.com")
        {
            return Err(Error::Authorization);
        }
        for (key, expected) in [
            ("redirect_uri", CALLBACK),
            ("response_type", "code"),
            ("code_challenge_method", "S256"),
            ("access_type", "offline"),
        ] {
            if one(url, key)? != expected {
                return Err(Error::Authorization);
            }
        }
        let client = one(url, "client_id")?;
        if !client.ends_with(".apps.googleusercontent.com") {
            return Err(Error::Authorization);
        }
        let challenge = one(url, "code_challenge")?;
        if challenge.len() != 43
            || !challenge
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(Error::Authorization);
        }
        let scopes = super::headless_oauth::query_values(url, "scope");
        if scopes.len() != 1 {
            return Err(Error::Authorization);
        }
        let scopes: std::collections::BTreeSet<_> = scopes[0].split_whitespace().collect();
        if scopes != SCOPES.iter().copied().collect() {
            return Err(Error::Authorization);
        }
        let state = SecretBytes::from(one(url, "state")?);
        let mut digest = aterm_digest::Sha256::new();
        digest.update(url.as_bytes());
        Ok(Self {
            url: SecretBytes::from(url.as_bytes()),
            state,
            binding: digest.finalize(),
        })
    }

    /// In-memory request for the owned headless browser. Never log this value.
    pub fn url(&self) -> &str {
        self.url.as_str().unwrap_or_default()
    }

    pub(super) fn admits_google(&self, url: &str) -> bool {
        super::login_drive::https_host(url).as_deref() == Some("accounts.google.com")
            && super::headless_oauth::FlowBound::of(self.url())
                .ok()
                .is_some_and(|bound| {
                    bound
                        .outside(url, &["accounts.google.com", "antigravity.google"])
                        .is_none()
                })
    }

    /// Validate the original committed callback URL before the vendor page clears its query.
    /// Its code may be delivered only to the child that produced this request.
    pub fn callback(&self, url: &str) -> Result<Code, Error> {
        if url.split('?').next() != Some(CALLBACK)
            || url.contains('#')
            || super::login_drive::https_host(url).as_deref() != Some("antigravity.google")
            || !super::headless_oauth::query_values(url, "error").is_empty()
            || one(url, "state").map_err(|_| Error::Callback)?.as_bytes() != self.state.as_slice()
        {
            return Err(Error::Callback);
        }
        let code = one(url, "code").map_err(|_| Error::Callback)?;
        Ok(Code {
            value: SecretBytes::from(code),
            binding: self.binding,
            session: None,
        })
    }
}

/// State-checked, request-bound native handback; the code is wiped on drop.
pub struct Code {
    value: SecretBytes,
    binding: [u8; 32],
    session: Option<super::google_browser::PendingSession>,
}

impl Code {
    pub(super) fn with_session(mut self, session: super::google_browser::PendingSession) -> Self {
        self.session = Some(session);
        self
    }
}

/// A native grant location after CLI success. This is not proof of its account identity.
pub struct UnverifiedGrant {
    session: Option<super::google_browser::PendingSession>,
    path: PathBuf,
    program: PathBuf,
    _claim: super::browser_window::ProfileLock,
}

/// Failures in the explicitly configured Google-to-agy path, with no credential values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoogleLoginError {
    Configuration,
    Approval(super::google_direct::Error),
    Native(Error),
    Recovery(&'static str),
}

impl std::fmt::Display for GoogleLoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration => {
                f.write_str("Explicit agy Google identity binding is unavailable or changed")
            }
            Self::Approval(error) => std::fmt::Display::fmt(error, f),
            Self::Native(error) => std::fmt::Display::fmt(error, f),
            Self::Recovery(message) => f.write_str(message),
        }
    }
}

/// A completed native run with a configured expected account, still awaiting canonical proof.
pub struct UnverifiedGoogleGrant {
    native: UnverifiedGrant,
    expected_email: String,
    state: PathBuf,
    account: String,
    revision: Option<[u8; 32]>,
    recovery: Option<recovery::Journal>,
    work: Option<super::account_work::Scope>,
}

/// A native grant accepted by Google's canonical account endpoint. The native profile
/// remains exclusively claimed until this value is dropped.
pub struct VerifiedGoogleGrant {
    grant: UnverifiedGoogleGrant,
    profile: super::google_profile::GoogleProfile,
    digest: [u8; 32],
}

/// A native grant this Mac signed in, proved and published, checked again at a launch on this
/// Mac alone (design 2026-11-05 §4.15 rule 6): its explicit identity binding is current and its
/// grant file is present and opens. Its account is the one Google's userinfo proved when the
/// login was signed in, which the pool binding recorded; a launch never contacts Google and never
/// reads the grant's contents. It can start a worker but, unlike a [`VerifiedGoogleGrant`], it
/// never publishes a login. The native profile stays claimed until this value is dropped.
pub struct PublishedGoogleGrant {
    grant: UnverifiedGoogleGrant,
}

/// Why a published native grant cannot start a launch, judged locally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishedRefusal {
    /// The profile holds no grant file that is present and opens: the row is signed out here.
    SignedOut,
    /// The binding, program or profile does not permit this launch.
    Refused(&'static str),
}

impl PublishedRefusal {
    /// The refusal's message, with no credential value or address.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::SignedOut => "Published native Google grant is absent or unusable",
            Self::Refused(message) => message,
        }
    }
}

impl From<&'static str> for PublishedRefusal {
    fn from(message: &'static str) -> Self {
        Self::Refused(message)
    }
}

impl std::fmt::Display for PublishedRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    /// Negative control for the local launch check: when set, the check also requires the grant's
    /// contents to hold a consumer access token, the first step of the userinfo request a launch
    /// once made with it. No release build carries it.
    static LAUNCH_READS_GRANT_CONTENTS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test seam: runs `body` with the negative control [`LAUNCH_READS_GRANT_CONTENTS`] set.
#[cfg(test)]
pub(super) fn with_launch_reading_grant_contents<R>(body: impl FnOnce() -> R) -> R {
    LAUNCH_READS_GRANT_CONTENTS.with(|set| set.set(true));
    let out = body();
    LAUNCH_READS_GRANT_CONTENTS.with(|set| set.set(false));
    out
}

impl PublishedGoogleGrant {
    pub(super) fn account_label(&self) -> &str {
        &self.grant.account
    }

    /// Recheck the binding and the grant file's presence immediately before use.
    ///
    /// # Errors
    ///
    /// The binding changed, or the grant file is no longer present and openable.
    pub fn check_current(&self) -> Result<(), PublishedRefusal> {
        self.grant
            .check_binding()
            .map_err(|_| "Google binding changed")?;
        let path = self.grant.path();
        if !super::agy_quota_store::usable_grant(path) {
            return Err(PublishedRefusal::SignedOut);
        }
        #[cfg(any(test, debug_assertions))]
        if LAUNCH_READS_GRANT_CONTENTS.with(std::cell::Cell::get) {
            native_access_token(grant_bytes(path)?.as_slice())?;
        }
        Ok(())
    }

    /// Carry the published profile into a worker, replacing inherited profile routing.
    /// The native executable is the managed program that produced the grant.
    ///
    /// # Errors
    ///
    /// The grant is no longer current, or the inherited flags cannot be rebound to the profile.
    pub fn prepare_worker(
        self,
        mut starter: super::worker::Starter,
    ) -> Result<PublishedGoogleWorker, &'static str> {
        self.check_current().map_err(PublishedRefusal::message)?;
        let path = self.grant.path();
        let gemini = path
            .parent()
            .and_then(Path::parent)
            .ok_or("Native agy profile path is unavailable")?;
        let home = gemini
            .parent()
            .and_then(Path::to_str)
            .ok_or("Native agy profile home is unavailable")?;
        let flags = starter
            .args
            .as_ref()
            .map_err(|_| "Inherited agy flags are unavailable")?;
        starter.args = Ok(super::upgrade_agy::bound_worker_flags(flags, gemini)?);
        starter.exe = self.grant.native.program.clone();
        starter.env.retain(|(name, _)| {
            name != "HOME" && name != "SSH_CONNECTION" && name != "AGY_CLI_DISABLE_AUTO_UPDATE"
        });
        starter.env.extend([
            ("HOME".into(), home.into()),
            ("SSH_CONNECTION".into(), "127.0.0.1 1 127.0.0.1 22".into()),
            ("AGY_CLI_DISABLE_AUTO_UPDATE".into(), "true".into()),
        ]);
        for name in crate::ai::connector::AGY.credential_env {
            starter.env.retain(|(key, _)| key != name);
            if !starter.unset.contains(name) {
                starter.unset.push(name);
            }
        }
        Ok(PublishedGoogleWorker {
            grant: self,
            starter,
        })
    }
}

/// Worker launch prepared from a published native grant. Retains the profile claim and checks
/// the exact binding and the grant file's presence again when its starter is requested.
pub struct PublishedGoogleWorker {
    grant: PublishedGoogleGrant,
    starter: super::worker::Starter,
}

impl PublishedGoogleWorker {
    /// Recheck immediately before admitting/writing this worker's launch line.
    ///
    /// # Errors
    ///
    /// The binding changed or the grant file is no longer present and openable.
    pub fn starter(&self) -> Result<&super::worker::Starter, &'static str> {
        self.grant
            .check_current()
            .map_err(PublishedRefusal::message)?;
        Ok(&self.starter)
    }
}

impl VerifiedGoogleGrant {
    /// Retire the encrypted pending copy only after the caller commits the live binding.
    pub(super) fn finish_recovery(&self) -> Result<(), &'static str> {
        self.check_current()?;
        self.grant
            .recovery
            .as_ref()
            .map_or(Ok(()), recovery::Journal::retire)
    }
    pub(super) fn account_label(&self) -> &str {
        &self.grant.account
    }

    pub(super) fn digest(&self) -> Result<[u8; 32], &'static str> {
        self.check_current()?;
        Ok(self.digest)
    }

    pub(super) fn checkpoint_scope(&self) -> Result<[u8; 32], &'static str> {
        self.check_current()?;
        checkpoint_scope(
            &self.grant.state,
            self.grant.revision.ok_or("Google binding changed")?,
        )
        .ok_or("Google checkpoint root is unavailable")
    }
    /// Provider-confirmed account identity.
    pub fn profile(&self) -> &super::google_profile::GoogleProfile {
        &self.profile
    }

    /// Recheck configuration and exact grant bytes immediately before publication/use.
    pub fn check_current(&self) -> Result<(), &'static str> {
        self.grant
            .check_binding()
            .map_err(|_| "Google binding changed")?;
        if let Some(journal) = &self.grant.recovery {
            journal.unchanged()?;
        }
        let bytes = grant_bytes(self.grant.path())?;
        if grant_digest(bytes.as_slice()) != self.digest {
            return Err("Native Google grant changed after verification");
        }
        Ok(())
    }

    /// A cookie candidate must come from this grant's exact configured Google identity.
    pub(super) fn check_browser_identity(
        &self,
        identity: &super::google_identity::Identity,
    ) -> Result<(), &'static str> {
        self.check_current()?;
        let registry = registry(&self.grant.state).ok_or("Google binding changed")?;
        let (expected, _) = registry
            .resolve("agy", &self.grant.account)
            .ok_or("Google binding changed")?;
        if expected != identity {
            return Err("Google browser identity does not match the native grant");
        }
        Ok(())
    }

    /// Claimed native token path, available only while its account proof is current.
    pub fn path(&self) -> Result<&Path, &'static str> {
        self.check_current()?;
        Ok(self.grant.path())
    }
}

fn grant_bytes(path: &Path) -> Result<SecretBytes, &'static str> {
    aterm_secrets::fsx::read_private(path, LIMIT)
        .map_err(|_| "Private native Google grant is unavailable")?
        .filter(|bytes| !bytes.is_empty())
        .ok_or("Private native Google grant is empty")
}

fn grant_digest(bytes: &[u8]) -> [u8; 32] {
    let mut digest = aterm_digest::Sha256::new();
    digest.update(bytes);
    digest.finalize()
}

fn native_access_token(bytes: &[u8]) -> Result<SecretBytes, &'static str> {
    let value: aterm_json::Value =
        aterm_json::from_slice(bytes).map_err(|_| "Native Google grant is unreadable")?;
    // Current native StoredToken wraps golang.org/x/oauth2.Token in `token`.
    // Cached id_token_email and legacy id_token are not account proof.
    // Native 1.4.12 names this route `consumer`; its separate `gcp` route also
    // uses Google's account endpoint, so userinfo alone cannot establish the native route.
    let token = if value["auth_method"].as_str() != Some("consumer") {
        Err("Native Google grant is not a consumer login")
    } else {
        value["token"]["access_token"]
            .as_str()
            .filter(|s| {
                !s.is_empty() && s.len() <= LIMIT && s.bytes().all(|b| (0x21..=0x7e).contains(&b))
            })
            .map(|s| SecretBytes::from(s.as_bytes()))
            .ok_or("Native Google grant has no valid access token")
    };
    wipe_native_json(value);
    token
}

fn wipe_native_json(value: aterm_json::Value) {
    match value {
        aterm_json::Value::String(s) => drop(SecretBytes::from(s)),
        aterm_json::Value::Array(values) => values.into_iter().for_each(wipe_native_json),
        aterm_json::Value::Object(values) => values.into_values().for_each(wipe_native_json),
        _ => {}
    }
}

impl UnverifiedGoogleGrant {
    fn retain_work(&mut self, work: Option<super::account_work::Scope>) {
        if self.work.is_none() {
            self.work = work;
        }
        if let Some(session) = self.native.session.as_mut() {
            session.retain_work(self.work.clone());
        }
    }
    /// Prove the exact native token's Google account before making it usable.
    /// Temporary endpoint failures get at most three requests, each at most 30 seconds,
    /// with one- and two-second delays inside the caller's original deadline. The native
    /// grant and pending browser session stay claimed throughout these attempts.
    /// Provider rejection, deadline expiry, or concurrent binding/grant changes fail closed.
    /// `deadline` counts the time the host slept: each look at it (before a request, before a
    /// delay, before publication) reads both clocks, so no request starts once the host slept
    /// past it; a request or delay in flight ends at its own cap.
    pub fn verify(self, deadline: WakeDeadline) -> Result<VerifiedGoogleGrant, &'static str> {
        self.verify_using(deadline, super::google_profile::read)
            .map_err(super::google_profile::Error::message)
    }

    fn verify_using(
        self,
        deadline: WakeDeadline,
        read: impl FnMut(
            &str,
            &str,
            Duration,
        )
            -> Result<super::google_profile::GoogleProfile, super::google_profile::Error>,
    ) -> Result<VerifiedGoogleGrant, super::google_profile::Error> {
        self.verify_with_retry(|| deadline.remaining(), read, std::thread::sleep)
    }

    /// [`UnverifiedGoogleGrant::verify`]'s steps, where `left` says what is left of its deadline
    /// now, `read` is the provider's check and `pause` a delay between requests.
    fn verify_with_retry(
        self,
        left: impl Fn() -> Duration,
        mut read: impl FnMut(
            &str,
            &str,
            Duration,
        ) -> Result<
            super::google_profile::GoogleProfile,
            super::google_profile::Error,
        >,
        mut pause: impl FnMut(Duration),
    ) -> Result<VerifiedGoogleGrant, super::google_profile::Error> {
        self.check_binding().map_err(|_| "Google binding changed")?;
        let bytes = grant_bytes(self.path())?;
        let digest = grant_digest(bytes.as_slice());
        if let Some(journal) = &self.recovery {
            journal.accepts_native(digest)?;
        }
        let token = native_access_token(bytes.as_slice())?;
        let mut attempt = 0;
        let profile = loop {
            self.check_binding().map_err(|_| "Google binding changed")?;
            if let Some(journal) = &self.recovery {
                journal.unchanged()?;
            }
            if grant_digest(grant_bytes(self.path())?.as_slice()) != digest {
                return Err("Native Google grant changed during verification".into());
            }
            let wait = Some(left())
                .filter(|wait| !wait.is_zero())
                .ok_or("Google verification deadline expired")?;
            let result = read(
                token.as_str().ok_or("Native Google token is not text")?,
                self.expected_email(),
                wait.min(super::google_profile::REQUEST_HANG),
            );
            match result {
                Ok(profile) => break profile,
                Err(super::google_profile::Error::Unavailable) if attempt < 2 => {
                    let delay = Duration::from_secs(1 << attempt);
                    if left() <= delay {
                        return Err(super::google_profile::Error::Unavailable);
                    }
                    pause(delay);
                    attempt += 1;
                }
                Err(error) => {
                    if matches!(
                        error,
                        super::google_profile::Error::CredentialRejected
                            | super::google_profile::Error::Refused(_)
                    ) && !left().is_zero()
                        && let Some(journal) = &self.recovery
                        && !(error == super::google_profile::Error::CredentialRejected
                            && journal.supports_staged_refresh())
                    {
                        journal.retire()?;
                    }
                    return Err(error);
                }
            }
        };
        if left().is_zero() {
            return Err("Google verification deadline expired".into());
        }
        let mut verified = VerifiedGoogleGrant {
            grant: self,
            profile,
            digest,
        };
        verified.check_current()?;
        if let Some(session) = verified.grant.native.session.take() {
            session
                .commit(&verified)
                .map_err(|_| "Verified Google browser session could not be persisted")?;
        }
        if left().is_zero() {
            return Err("Google verification deadline expired".into());
        }
        verified.check_current()?;
        Ok(verified)
    }
    /// Private native grant, claimed until this guard is dropped.
    pub fn path(&self) -> &Path {
        self.native.path()
    }
    /// Exact configured Google account to verify with the provider.
    pub fn expected_email(&self) -> &str {
        &self.expected_email
    }
    /// Check again before verification/publication; a changed binding cannot publish a login.
    pub fn check_binding(&self) -> Result<(), GoogleLoginError> {
        if self
            .work
            .as_ref()
            .is_some_and(super::account_work::Scope::cancelled)
        {
            return Err(GoogleLoginError::Configuration);
        }
        self.check_binding_authority()?;
        if self
            .work
            .as_ref()
            .is_some_and(super::account_work::Scope::cancelled)
        {
            Err(GoogleLoginError::Configuration)
        } else {
            Ok(())
        }
    }

    fn check_binding_authority(&self) -> Result<(), GoogleLoginError> {
        binding_current(&self.state, &self.account, self.revision)
            .then_some(())
            .ok_or(GoogleLoginError::Configuration)
    }
}

#[cfg(test)]
pub(super) fn recovery_publication_fixture(
    req: &super::login_drive::Request<'_>,
    key: RecoveryKey,
    unavailable: bool,
) -> (Result<VerifiedGoogleGrant, String>, bool) {
    recovery::publication_fixture(req, key, unavailable)
}

fn registry(state: &Path) -> Option<super::google_identity::Registry> {
    let bytes = super::google_federation::read_registry(state).ok()??;
    super::google_identity::Registry::parse(std::str::from_utf8(&bytes).ok()?).ok()
}

fn binding_current(state: &Path, account: &str, revision: Option<[u8; 32]>) -> bool {
    revision.is_some()
        && registry(state).is_some_and(|r| r.binding_revision("agy", account) == revision)
}

fn checkpoint_scope(state: &Path, revision: [u8; 32]) -> Option<[u8; 32]> {
    if !state.is_absolute() {
        return None;
    }
    let root = std::fs::canonicalize(state).ok()?;
    let mut digest = aterm_digest::Sha256::new();
    digest.update(b"aterm.google-checkpoint-scope.v1\0");
    digest.update(root.as_os_str().as_encoded_bytes());
    digest.update(b"\0");
    digest.update(revision);
    Some(digest.finalize())
}

/// Recovery accepts only the original Google identity binding at the original state root.
/// This is a local policy check; it performs no provider or credential I/O.
pub(super) fn current_checkpoint_scope(
    state: &Path,
    account: &str,
    email: &str,
) -> Option<[u8; 32]> {
    let registry = registry(state)?;
    let (identity, binding) = registry.resolve("agy", account)?;
    if !identity.email().eq_ignore_ascii_case(email)
        || !binding.expected_email().eq_ignore_ascii_case(email)
    {
        return None;
    }
    checkpoint_scope(state, registry.binding_revision("agy", account)?)
}

fn native_profile(
    state: &Path,
    account: &str,
    identity: &super::google_identity::Identity,
) -> PathBuf {
    let mut digest = aterm_digest::Sha256::new();
    digest.update(b"aterm.agy-login.v1\0");
    for part in [account, identity.id(), identity.email()] {
        digest.update(part.as_bytes());
        digest.update(b"\0");
    }
    state
        .join("agy-logins")
        .join(aterm_codec::hex::encode_lower(&digest.finalize()))
}

/// Check, at a launch, a native grant this Mac published, on this Mac alone (design 2026-11-05
/// §4.15 rule 6): the program and profile are in place, the row's explicit Google binding is
/// current, the profile can be claimed, and its grant file is present and opens. It runs no CLI,
/// opens no browser, contacts no provider and never reads the grant's contents: the account was
/// proved by Google's userinfo when it was signed in, and agy renews its own access token. A grant
/// file that is absent or does not open answers [`PublishedRefusal::SignedOut`], which the pool's
/// staged renewal takes as the launch's sign-in.
///
/// # Errors
///
/// The row is signed out here, or the program, profile or binding does not permit the launch.
pub fn check_google_published(
    program: &Path,
    state: &Path,
    account: &str,
    expected_email: &str,
    profile: &Path,
) -> Result<PublishedGoogleGrant, PublishedRefusal> {
    if !program.is_absolute()
        || !program.is_file()
        || !state.is_absolute()
        || !profile.is_absolute()
    {
        return Err("Published native Google profile is unavailable".into());
    }
    let registry = registry(state).ok_or("Explicit Google binding is unavailable")?;
    let (identity, binding) = registry
        .resolve("agy", account)
        .ok_or("Explicit Google binding is unavailable")?;
    if !identity
        .email()
        .eq_ignore_ascii_case(binding.expected_email())
        || !expected_email.eq_ignore_ascii_case(binding.expected_email())
    {
        return Err("Google account does not match the selected pool row".into());
    }
    let claim = super::browser_window::ProfileLock::claim(profile)
        .map_err(|_| "Published native Google profile is held or unavailable")?;
    let published = PublishedGoogleGrant {
        grant: UnverifiedGoogleGrant {
            native: UnverifiedGrant {
                session: None,
                path: super::upgrade_agy::profile_grant(profile),
                program: program.into(),
                _claim: claim,
            },
            expected_email: expected_email.into(),
            state: state.into(),
            account: account.into(),
            revision: registry.binding_revision("agy", account),
            recovery: None,
            work: None,
        },
    };
    published.check_current()?;
    Ok(published)
}

/// Approve a native agy login using its explicitly bound Google identity and registered
/// session and credentials when needed. No account is selected implicitly and no interactive fallback is opened.
/// Native completion remains unverified until a canonical provider check accepts its grant.
pub fn run_google(
    program: &Path,
    state: &Path,
    home: &Path,
    account: &str,
    deadline: WakeDeadline,
) -> Result<UnverifiedGoogleGrant, GoogleLoginError> {
    recovery::direct(program, state, home, account, deadline)
}

/// Authenticate into the pool's private staging home, binding the selected row's email
/// to the explicit Google registry before starting native or browser authorization.
/// Keep the returned grant through canonical verification and atomic pool publication.
pub struct StagedGoogleLogin<'a> {
    pub profile: &'a Path,
    pub previous: Option<&'a Path>,
    pub expected_email: &'a str,
    /// An admitted pool attempt may retain encrypted unverified results across restart.
    pub recovery: Option<RecoveryKey>,
}

/// Run the selected account's native authorization in its separately claimed staging home.
pub fn run_google_staged(
    program: &Path,
    state: &Path,
    home: &Path,
    account: &str,
    staging: StagedGoogleLogin<'_>,
    deadline: WakeDeadline,
) -> Result<UnverifiedGoogleGrant, GoogleLoginError> {
    run_google_staged_scoped(program, state, home, account, staging, deadline, None)
}

/// Run staged authorization while retaining its automatic operation lifetime.
pub fn run_google_staged_scoped(
    program: &Path,
    state: &Path,
    home: &Path,
    account: &str,
    staging: StagedGoogleLogin<'_>,
    deadline: WakeDeadline,
    work: Option<super::account_work::Scope>,
) -> Result<UnverifiedGoogleGrant, GoogleLoginError> {
    if work
        .as_ref()
        .is_some_and(super::account_work::Scope::cancelled)
    {
        return Err(GoogleLoginError::Configuration);
    }
    if staging.recovery.is_some() {
        return recovery::run_scoped(program, state, home, account, staging, deadline, work);
    }
    let result =
        run_google_at_scoped(program, state, home, account, Some(staging), deadline, work)?;
    result.check_binding()?;
    Ok(result)
}

fn run_google_at(
    program: &Path,
    state: &Path,
    home: &Path,
    account: &str,
    staging: Option<StagedGoogleLogin<'_>>,
    deadline: WakeDeadline,
) -> Result<UnverifiedGoogleGrant, GoogleLoginError> {
    run_google_at_scoped(program, state, home, account, staging, deadline, None)
}

fn run_google_at_scoped(
    program: &Path,
    state: &Path,
    home: &Path,
    account: &str,
    staging: Option<StagedGoogleLogin<'_>>,
    deadline: WakeDeadline,
    work: Option<super::account_work::Scope>,
) -> Result<UnverifiedGoogleGrant, GoogleLoginError> {
    remaining(deadline).map_err(GoogleLoginError::Native)?;
    if !state.is_absolute() || !home.is_absolute() {
        return Err(GoogleLoginError::Configuration);
    }
    let registry = registry(state).ok_or(GoogleLoginError::Configuration)?;
    let (identity, binding) = registry
        .resolve("agy", account)
        .ok_or(GoogleLoginError::Configuration)?;
    if !identity
        .email()
        .eq_ignore_ascii_case(binding.expected_email())
    {
        return Err(GoogleLoginError::Configuration);
    }
    let profile = match staging.as_ref() {
        Some(staging) => {
            if !staging.profile.is_absolute()
                || !staging
                    .expected_email
                    .eq_ignore_ascii_case(binding.expected_email())
                || staging.profile == native_profile(state, account, identity)
                || staging
                    .previous
                    .is_some_and(|old| !old.is_absolute() || old == staging.profile)
            {
                return Err(GoogleLoginError::Configuration);
            }
            staging.profile.to_path_buf()
        }
        None => native_profile(state, account, identity),
    };
    let staged = staging.is_some();
    let claim = if let Some(staging) = staging {
        Some(stage_profile(staging.profile, staging.previous).map_err(GoogleLoginError::Native)?)
    } else {
        None
    };
    let revision = registry.binding_revision("agy", account);
    let current = || {
        !work
            .as_ref()
            .is_some_and(super::account_work::Scope::cancelled)
            && binding_current(state, account, revision)
    };
    let mut approval_error = None;
    let recovery = staged.then_some(StagedRecovery {
        expected_email: binding.expected_email(),
        current: &current,
    });
    let native = run_claimed(
        program,
        &profile,
        claim,
        recovery,
        deadline,
        |request, deadline| {
            let mailbox = super::headless_oauth::discover_mailbox_config(Some(home), Some(state));
            let context = super::google_direct::Context {
                work: work.clone(),
                identity,
                home,
                state,
                mailbox: mailbox.as_ref(),
                current: &current,
            };
            match super::google_direct::approve(request, &context, deadline) {
                Ok(code) => Ok(code),
                Err(error) => {
                    approval_error = Some(GoogleLoginError::Approval(error));
                    Err(Error::Authorization)
                }
            }
        },
    );
    if let Some(error) = approval_error {
        return Err(error);
    }
    let result = UnverifiedGoogleGrant {
        native: native.map_err(GoogleLoginError::Native)?,
        expected_email: binding.expected_email().into(),
        state: state.into(),
        account: account.into(),
        revision,
        recovery: None,
        work,
    };
    result.check_binding_authority()?;
    Ok(result)
}

impl UnverifiedGrant {
    /// Private native grant location. Keep this guard alive through verification/publication
    /// so another cooperative login cannot replace the grant between those operations.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

struct Child {
    child: std::process::Child,
    _slave: OwnedFd,
    master: std::fs::File,
    exit: OwnedFd,
    group_ended: bool,
}

impl Drop for Child {
    fn drop(&mut self) {
        // Never reap before ending the group: the unreaped leader reserves its identity.
        if !self.group_ended
            && let Ok(pid) = i32::try_from(self.child.id())
        {
            let _ = aterm_uds::process::kill_group_until_empty(pid);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What is left of `deadline`, which counts the time the host slept: [`Error::Deadline`] once
/// it passed.
fn remaining(deadline: WakeDeadline) -> Result<Duration, Error> {
    Some(deadline.remaining())
        .filter(|d| !d.is_zero())
        .ok_or(Error::Deadline)
}

/// End the child's unreaped process group, where an `ESRCH` is an ordinary end for a leader this
/// process has already seen exit or reap.
pub(super) fn finish_exited_group(leader: i32) -> std::io::Result<()> {
    match aterm_uds::process::kill_group_until_empty(leader) {
        Ok(()) => Ok(()),
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
            match aterm_uds::process::process_presence(leader) {
                aterm_uds::process::ProcessPresence::Exited
                | aterm_uds::process::ProcessPresence::Absent => Ok(()),
                _ => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

/// Run native agy in a private home and terminal. `approve` must use the admitted request's
/// original callback to construct its handback. It shares the same authorization deadline.
/// The caller must verify the resulting grant's canonical account before publishing it.
pub fn run(
    program: &Path,
    profile: &Path,
    deadline: WakeDeadline,
    approve: impl FnMut(&Authorization, WakeDeadline) -> Result<Code, Error>,
) -> Result<UnverifiedGrant, Error> {
    run_claimed(program, profile, None, None, deadline, approve)
}

fn stage_profile(
    profile: &Path,
    previous: Option<&Path>,
) -> Result<super::browser_window::ProfileLock, Error> {
    aterm_types::fs_restricted::ensure_private_dir(profile).map_err(|_| Error::Profile)?;
    let claim = super::browser_window::ProfileLock::claim(profile).map_err(|_| Error::Profile)?;
    let token = super::upgrade_agy::profile_grant(profile);
    // A staging home is owned by this attempt. Never overwrite a grant left by another
    // attempt, and never let native refresh mutate the currently published home.
    if token.try_exists().map_err(|_| Error::Profile)? {
        return Err(Error::Profile);
    }
    if let Some(previous) = previous {
        let source = super::upgrade_agy::profile_grant(previous);
        if let Some(bytes) = aterm_secrets::fsx::read_private(&source, LIMIT)
            .map_err(|_| Error::Profile)?
            .filter(|bytes| !bytes.is_empty())
        {
            let parent = token.parent().ok_or(Error::Profile)?;
            aterm_types::fs_restricted::ensure_private_dir(parent).map_err(|_| Error::Profile)?;
            aterm_secrets::fsx::write_atomic(&token, bytes.as_slice(), 0o600)
                .map_err(|_| Error::Profile)?;
        }
    }
    Ok(claim)
}

/// Only an owned staging home may prepare a rejected token for native renewal. The current
/// registry binding must survive both the provider read and the exact-byte recheck.
struct StagedRecovery<'a> {
    expected_email: &'a str,
    current: &'a dyn Fn() -> bool,
}

fn prepare_rejected_staging(
    profile: &Path,
    recovery: &StagedRecovery<'_>,
    deadline: WakeDeadline,
    check: impl FnOnce(&str, Duration) -> Result<(), super::google_profile::Error>,
) -> Result<bool, Error> {
    if !(recovery.current)() {
        return Err(Error::Authorization);
    }
    let path = super::upgrade_agy::profile_grant(profile);
    let bytes = grant_bytes(&path).map_err(|_| Error::Native)?;
    let token = native_access_token(bytes.as_slice()).map_err(|_| Error::Native)?;
    let rejected = matches!(
        check(
            token.as_str().ok_or(Error::Native)?,
            remaining(deadline)?.min(super::google_profile::REQUEST_HANG)
        ),
        Err(super::google_profile::Error::CredentialRejected)
    );
    if !rejected {
        return Ok(false);
    }
    remaining(deadline)?;
    if !(recovery.current)() {
        return Err(Error::Authorization);
    }
    let current = grant_bytes(&path).map_err(|_| Error::Profile)?;
    if grant_digest(current.as_slice()) != grant_digest(bytes.as_slice()) {
        return Err(Error::Profile);
    }
    let mut value: aterm_json::Value =
        aterm_json::from_slice(bytes.as_slice()).map_err(|_| Error::Native)?;
    let refresh = value["token"]["refresh_token"].as_str().is_some_and(|s| {
        !s.is_empty() && s.len() <= LIMIT && s.bytes().all(|b| (0x21..=0x7e).contains(&b))
    });
    let replacement = if refresh {
        // Native oauth2 owns refresh and its fallback. Expire only our staged copy;
        // never throw away a refresh credential because its access token was rejected.
        let fields = value
            .as_object_mut()
            .and_then(|v| v.get_mut("token"))
            .and_then(aterm_json::Value::as_object_mut)
            .expect("native access token validation established the token object");
        if let Some(old) = fields.insert("expiry".into(), "2000-01-01T00:00:00Z".into()) {
            wipe_native_json(old);
        }
        aterm_json::to_vec(&value).map(SecretBytes::from)
    } else {
        Ok(SecretBytes::from(Vec::new()))
    };
    wipe_native_json(value);
    let replacement = replacement.map_err(|_| Error::Native)?;
    remaining(deadline)?;
    if !(recovery.current)() {
        return Err(Error::Authorization);
    }
    if grant_digest(grant_bytes(&path).map_err(|_| Error::Profile)?.as_slice())
        != grant_digest(bytes.as_slice())
    {
        return Err(Error::Profile);
    }
    if refresh {
        aterm_secrets::fsx::write_atomic(&path, replacement.as_slice(), 0o600)
            .map_err(|_| Error::Profile)?;
    } else {
        std::fs::remove_file(path).map_err(|_| Error::Profile)?;
    }
    Ok(true)
}

fn run_claimed(
    program: &Path,
    profile: &Path,
    claim: Option<super::browser_window::ProfileLock>,
    recovery: Option<StagedRecovery<'_>>,
    deadline: WakeDeadline,
    mut approve: impl FnMut(&Authorization, WakeDeadline) -> Result<Code, Error>,
) -> Result<UnverifiedGrant, Error> {
    remaining(deadline)?;
    if !program.is_absolute() || !program.is_file() || !profile.is_absolute() {
        return Err(Error::Profile);
    }
    aterm_types::fs_restricted::ensure_private_dir(profile).map_err(|_| Error::Profile)?;
    let claim = match claim {
        Some(claim) => claim,
        None => super::browser_window::ProfileLock::claim(profile).map_err(|_| Error::Profile)?,
    };
    let mut approval_started = false;
    let mut result = run_attempt(program, profile, deadline, &mut |request, deadline| {
        approval_started = true;
        approve(request, deadline)
    });
    if matches!(result, Err(Error::Native))
        && !approval_started
        && let Some(recovery) = recovery
    {
        let rejected = prepare_rejected_staging(profile, &recovery, deadline, |token, wait| {
            super::google_profile::read(token, recovery.expected_email, wait).map(drop)
        })?;
        if rejected {
            // The first native child is gone, and this same claim remains held.
            // Retry once from an empty staging grant with the original deadline.
            result = run_attempt(program, profile, deadline, &mut approve);
        }
    }
    let (path, session) = result?;
    Ok(UnverifiedGrant {
        session,
        path,
        program: program.to_owned(),
        _claim: claim,
    })
}

fn run_attempt(
    program: &Path,
    profile: &Path,
    deadline: WakeDeadline,
    approve: &mut impl FnMut(&Authorization, WakeDeadline) -> Result<Code, Error>,
) -> Result<(PathBuf, Option<super::google_browser::PendingSession>), Error> {
    remaining(deadline)?;
    let gemini = profile.join(".gemini");
    let (master, slave, mut child) = aterm_pty::spawn_session_leader_on_pty(|_| {
        #[allow(clippy::disallowed_types, reason = "the controlling-terminal spawn seam takes StdCommand and applies inherit_nothing before spawning")]
        let mut cmd = std::process::Command::new(program);
        cmd.env_clear();
        for name in super::login_drive::PASSED {
            if *name != "HOME"
                && let Some(value) = std::env::var_os(name)
            {
                cmd.env(name, value);
            }
        }
        cmd.env("HOME", profile)
            .env("SSH_CONNECTION", "127.0.0.1 1 127.0.0.1 22")
            .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true")
            .env("TERM", "dumb")
            .current_dir(profile)
            .arg("--gemini_dir")
            .arg(&gemini)
            .args([
                "--app_data_dir=antigravity-cli",
                "-p",
                "/usage",
                "--output-format",
                "json",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        Ok(cmd)
    })
    .map_err(|_| Error::Process)?;
    // Install cleanup before acquiring any other fallible resource.
    let exit = match aterm_uds::exitwatch::exit_fd(child.id()) {
        Some(exit) => exit,
        None => {
            if let Ok(pid) = i32::try_from(child.id()) {
                let _ = aterm_uds::process::kill_group_until_empty(pid);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Process);
        }
    };
    let mut child = Child {
        child,
        _slave: slave,
        master: std::fs::File::from(master),
        exit,
        group_ended: false,
    };
    let stdout = child.child.stdout.take().ok_or(Error::Process)?;
    let stderr = child.child.stderr.take().ok_or(Error::Process)?;
    aterm_pty::set_nonblocking(child.master.as_raw_fd(), true).map_err(|_| Error::Process)?;
    let (sent, received) = mpsc::sync_channel(16);
    for (index, mut stream) in [Box::new(stdout) as Box<dyn Read + Send>, Box::new(stderr)]
        .into_iter()
        .enumerate()
    {
        let sent = sent.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 2048];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                    Ok(n) => {
                        if sent.send((index, SecretBytes::from(&chunk[..n]))).is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }
    drop(sent);
    let mut output = [SecretBytes::default(), SecretBytes::default()];
    let mut approved = false;
    let mut session = None;
    while !approved {
        let (index, chunk) =
            match received.recv_timeout(remaining(deadline)?.min(Duration::from_millis(100))) {
                Ok(chunk) => chunk,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
        if output.iter().map(SecretBytes::len).sum::<usize>() + chunk.len() > LIMIT {
            return Err(Error::Output);
        }
        let mut bytes = output[index].as_slice().to_vec();
        bytes.extend_from_slice(chunk.as_slice());
        output[index] = SecretBytes::from(bytes);
        let text = String::from_utf8_lossy(output[index].as_slice());
        let Some(start) = text.find("https://accounts.google.com/") else {
            continue;
        };
        let tail = &text[start..];
        let Some(end) = tail.find(|c: char| {
            c.is_whitespace() || c.is_control() || matches!(c, '"' | '<' | '>' | '\'')
        }) else {
            continue;
        };
        let request = Authorization::admit(&tail[..end])?;
        let code = approve(&request, deadline)?;
        if code.binding != request.binding {
            return Err(Error::Callback);
        }
        let mut line = code.value.as_slice().to_vec();
        line.push(b'\n');
        let line = SecretBytes::from(line);
        let mut bytes = line.as_slice();
        while !bytes.is_empty() {
            remaining(deadline)?;
            match child.master.write(bytes) {
                Ok(0) => return Err(Error::Process),
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // One tick at a time, so the next turn's look at both clocks ends a write
                    // the host slept past.
                    match aterm_uds::ready::until(
                        child.master.as_fd(),
                        aterm_uds::ready::Interest::Write,
                        Instant::now() + deadline.tick(),
                    ) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
                        Err(_) => return Err(Error::Deadline),
                    }
                }
                Err(_) => return Err(Error::Process),
            }
        }
        session = code.session;
        approved = true;
    }
    // Keep draining while awaiting native completion; auth output is never returned or logged.
    loop {
        let slice = Instant::now() + remaining(deadline)?.min(Duration::from_millis(20));
        match aterm_uds::ready::until(child.exit.as_fd(), aterm_uds::ready::Interest::Read, slice) {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => return Err(Error::Process),
        }
        for _ in 0..16 {
            if received.try_recv().is_err() {
                break;
            }
        }
    }
    let pid = i32::try_from(child.child.id()).map_err(|_| Error::Process)?;
    finish_exited_group(pid).map_err(|_| Error::Process)?;
    child.group_ended = true;
    let status = child.child.wait().map_err(|_| Error::Process)?;
    // No live group member remains before wait reaps its leader; Drop must not signal a reused
    // pid after wait releases that leader's reserved group id.
    let path = gemini
        .join(super::upgrade_agy::APP_DATA_DIR)
        .join(super::upgrade_agy::GRANT_FILE);
    let grant = aterm_secrets::fsx::read_private(&path, LIMIT).map_err(|_| Error::Native)?;
    if !status.success() || grant.is_none_or(|bytes| bytes.is_empty()) {
        return Err(Error::Native);
    }
    Ok((path, session))
}

/// Synthetic post-verification capability for publication race tests; never authenticates.
#[cfg(test)]
pub(super) fn publication_fixture(
    state: &Path,
    profile: &Path,
    account: &str,
    email: &str,
) -> VerifiedGoogleGrant {
    let path = profile.join(".gemini/antigravity-cli/antigravity-oauth-token");
    aterm_secrets::fsx::ensure_private_dir(path.parent().unwrap()).unwrap();
    let bytes = br#"{"auth_method":"consumer","token":{"access_token":"synthetic-token"}}"#;
    aterm_secrets::fsx::write_atomic(&path, bytes, 0o600).unwrap();
    VerifiedGoogleGrant {
        grant: UnverifiedGoogleGrant {
            native: UnverifiedGrant {
                session: None,
                path,
                program: PathBuf::from("/synthetic/agy"),
                _claim: super::browser_window::ProfileLock::claim(profile).unwrap(),
            },
            expected_email: email.into(),
            state: state.into(),
            account: account.into(),
            revision: registry(state).unwrap().binding_revision("agy", account),
            recovery: None,
            work: None,
        },
        profile: super::google_profile::GoogleProfile {
            email: email.into(),
            principal: [1; 32],
        },
        digest: grant_digest(bytes),
    }
}

/// Synthetic published grant for launch tests: a grant file is written and claimed, never read.
#[cfg(test)]
pub(super) fn published_fixture(
    state: &Path,
    profile: &Path,
    account: &str,
    email: &str,
) -> PublishedGoogleGrant {
    PublishedGoogleGrant {
        grant: publication_fixture(state, profile, account, email).grant,
    }
}

#[cfg(test)]
#[path = "agy_signin_tests.rs"]
mod tests;
