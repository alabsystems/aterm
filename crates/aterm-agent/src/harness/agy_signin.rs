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
mod tests {
    use super::*;

    #[test]
    fn temporary_google_verification_keeps_the_same_claimed_candidate() {
        use super::super::{google_browser, google_browser_store, google_profile};
        use std::cell::Cell;
        const OLD: &[u8] = br#"[{"name":"fixture","value":"old","domain":".google.com","path":"/","secure":true}]"#;
        const NEW: &[u8] = br#"[{"name":"fixture","value":"new","domain":".google.com","path":"/","secure":true}]"#;
        for scenario in 0..10 {
            let home = aterm_tempfile::tempdir().unwrap();
            let state = aterm_tempfile::tempdir().unwrap();
            let profile = aterm_tempfile::tempdir().unwrap();
            aterm_secrets::fsx::write_atomic(&state.path().join("google-identities.toml"), b"[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n", 0o600).unwrap();
            let identity = registry(state.path())
                .unwrap()
                .resolve("agy", "one")
                .unwrap()
                .0
                .clone();
            let claim = identity.claim(state.path()).unwrap();
            let snapshot = claim.profile().join("session.age");
            google_browser_store::save(home.path(), &identity, &claim, OLD).unwrap();
            let previous = std::fs::read(&snapshot).unwrap();
            drop(claim);
            let mut grant =
                publication_fixture(state.path(), profile.path(), "one", "person@example.com")
                    .grant;
            let native_path = grant.path().to_path_buf();
            let original = std::fs::read(&native_path).unwrap();
            grant.native.session = Some(google_browser::pending_fixture(
                &identity,
                state.path(),
                home.path(),
                NEW,
            ));
            let clock = Cell::new(Instant::now());
            let deadline = clock.get()
                + if scenario == 8 {
                    Duration::from_millis(500)
                } else {
                    Duration::from_secs(60)
                };
            let mut calls = 0;
            let mut delays = Vec::new();
            let mut token_seen = None;
            let result = grant.verify_with_retry(
                || deadline.saturating_duration_since(clock.get()),
                |token, expected, wait| {
                    calls += 1;
                    assert_eq!(expected, "person@example.com");
                    assert!(
                        !wait.is_zero() && wait <= crate::harness::google_profile::REQUEST_HANG
                    );
                    assert_eq!(std::fs::read(&native_path).unwrap(), original);
                    assert_eq!(std::fs::read(&snapshot).unwrap(), previous);
                    assert!(
                        identity.claim(state.path()).is_err(),
                        "Google claim released between requests"
                    );
                    assert!(
                        super::super::browser_window::ProfileLock::claim(profile.path()).is_err(),
                        "native claim released between requests"
                    );
                    if let Some(previous_token) = &token_seen {
                        assert_eq!(previous_token, token);
                    }
                    token_seen = Some(token.to_owned());
                    match scenario {
                        0 if calls == 2 => {}
                        1 if calls == 3 => {}
                        3 => return Err(google_profile::Error::CredentialRejected),
                        4 => return Err(google_profile::Error::Refused("wrong account")),
                        9 if calls == 2 => return Err(google_profile::Error::CredentialRejected),
                        _ => return Err(google_profile::Error::Unavailable),
                    }
                    Ok(google_profile::GoogleProfile {
                        email: expected.into(),
                        principal: [1; 32],
                    })
                },
                |delay| {
                    delays.push(delay);
                    clock.set(if scenario == 7 {
                        deadline
                    } else {
                        clock.get() + delay
                    });
                    if scenario == 5 {
                        aterm_secrets::fsx::write_atomic(
                            &state.path().join("google-identities.toml"),
                            b"changed",
                            0o600,
                        )
                        .unwrap();
                    } else if scenario == 6 {
                        aterm_secrets::fsx::write_atomic(&native_path, b"changed", 0o600).unwrap();
                    }
                },
            );
            assert_eq!(
                calls,
                match scenario {
                    0 | 9 => 2,
                    1 | 2 => 3,
                    _ => 1,
                },
                "scenario {scenario}"
            );
            assert_eq!(
                delays,
                match scenario {
                    1 | 2 => vec![Duration::from_secs(1), Duration::from_secs(2)],
                    0 | 5 | 6 | 7 | 9 => vec![Duration::from_secs(1)],
                    _ => vec![],
                },
                "scenario {scenario}"
            );
            if scenario <= 1 {
                assert!(result.unwrap().path().is_ok());
                let claim = identity.claim(state.path()).unwrap();
                assert_eq!(
                    google_browser_store::load(home.path(), &identity, &claim)
                        .unwrap()
                        .unwrap()
                        .as_slice(),
                    NEW
                );
            } else {
                let error = result.err().expect("failure must not publish a proof");
                if scenario == 2 || scenario == 8 {
                    assert_eq!(error, google_profile::Error::Unavailable);
                }
                if scenario == 3 || scenario == 9 {
                    assert_eq!(error, google_profile::Error::CredentialRejected);
                }
                assert_eq!(std::fs::read(&snapshot).unwrap(), previous);
                assert!(identity.claim(state.path()).is_ok());
                assert!(super::super::browser_window::ProfileLock::claim(profile.path()).is_ok());
            }
        }
    }

    #[test]
    fn native_google_pending_session_commits_only_after_current_provider_proof() {
        use super::super::{google_browser, google_browser_store};
        const OLD: &[u8] = br#"[{"name":"fixture","value":"old-session","domain":".google.com","path":"/","secure":true}]"#;
        const NEW: &[u8] = br#"[{"name":"fixture","value":"renewed-session","domain":".google.com","path":"/","secure":true}]"#;
        for scenario in 0..7 {
            let home = aterm_tempfile::tempdir().unwrap();
            let state = aterm_tempfile::tempdir().unwrap();
            let profile = aterm_tempfile::tempdir().unwrap();
            aterm_secrets::fsx::write_atomic(&state.path().join("google-identities.toml"), b"[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n", 0o600).unwrap();
            let identity = registry(state.path())
                .unwrap()
                .resolve("agy", "one")
                .unwrap()
                .0
                .clone();
            let claim = identity.claim(state.path()).unwrap();
            let snapshot = claim.profile().join("session.age");
            google_browser_store::save(home.path(), &identity, &claim, OLD).unwrap();
            let previous = std::fs::read(&snapshot).unwrap();
            drop(claim);
            let candidate_identity = if scenario == 6 {
                super::super::google_identity::Registry::parse("[[identities]]\nid='another-namespace'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='another-namespace'\nexpected_email='person@example.com'\n").unwrap().resolve("agy", "one").unwrap().0.clone()
            } else {
                identity.clone()
            };
            let mut grant =
                publication_fixture(state.path(), profile.path(), "one", "person@example.com")
                    .grant;
            grant.native.session = Some(google_browser::pending_fixture(
                &candidate_identity,
                state.path(),
                home.path(),
                NEW,
            ));
            let deadline = WakeDeadline::after(if scenario == 5 {
                Duration::ZERO
            } else {
                Duration::from_secs(60)
            });
            let result = grant.verify_using(deadline, |_, expected, _| {
                assert_ne!(scenario, 5, "expired verification contacted the provider");
                assert_eq!(expected, "person@example.com");
                assert_eq!(
                    std::fs::read(&snapshot).unwrap(),
                    previous,
                    "unverified cookies reached disk"
                );
                assert!(
                    candidate_identity.claim(state.path()).is_err(),
                    "identity ownership ended before provider proof"
                );
                match scenario {
                    1 => aterm_secrets::fsx::write_atomic(
                        &profile
                            .path()
                            .join(".gemini/antigravity-cli/antigravity-oauth-token"),
                        b"changed",
                        0o600,
                    )
                    .unwrap(),
                    2 => aterm_secrets::fsx::write_atomic(
                        &state.path().join("google-identities.toml"),
                        b"changed",
                        0o600,
                    )
                    .unwrap(),
                    3 => return Err(super::super::google_profile::Error::CredentialRejected),
                    _ => {}
                }
                Ok(super::super::google_profile::GoogleProfile {
                    email: if scenario == 4 {
                        "another@example.com"
                    } else {
                        expected
                    }
                    .into(),
                    principal: [1; 32],
                })
            });
            if scenario == 0 {
                let verified = result.unwrap();
                assert!(verified.path().is_ok());
                let bytes = std::fs::read(&snapshot).unwrap();
                assert_ne!(bytes, previous);
                assert!(
                    !bytes
                        .windows(b"renewed-session".len())
                        .any(|part| part == b"renewed-session")
                );
                let claim = identity.claim(state.path()).unwrap();
                assert_eq!(
                    google_browser_store::load(home.path(), &identity, &claim)
                        .unwrap()
                        .unwrap()
                        .as_slice(),
                    NEW
                );
            } else {
                assert!(result.is_err());
                assert_eq!(std::fs::read(&snapshot).unwrap(), previous);
                assert!(candidate_identity.claim(state.path()).is_ok());
                if scenario == 6 {
                    let claim = candidate_identity.claim(state.path()).unwrap();
                    assert!(!claim.profile().join("session.age").exists());
                }
            }
        }
    }

    /// A GRANT VERIFICATION ITS HOST SLEPT PAST STARTS NO NEW REQUEST (KI-1105-14): an hour's
    /// verification whose host slept two hours during its first account check, which Google
    /// could not answer, ends then as unavailable, with no retry. NEGATIVE CONTROL: on the clock
    /// that stops in sleep alone the hour was left, and it paused (a second, then two) and asked
    /// twice more.
    #[cfg(unix)]
    #[test]
    fn a_grant_verification_its_host_slept_past_starts_no_new_request() {
        let state = aterm_tempfile::tempdir().unwrap();
        let profile = aterm_tempfile::tempdir().unwrap();
        aterm_secrets::fsx::write_atomic(&state.path().join("google-identities.toml"), b"[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n", 0o600).unwrap();
        let grant =
            publication_fixture(state.path(), profile.path(), "one", "person@example.com").grant;
        let _seam = super::super::wake_deadline::SleepSeam::hold();
        let deadline = WakeDeadline::after(Duration::from_secs(3600));
        let mut calls = 0;
        let result = grant.verify_using(deadline, |_, _, _| {
            calls += 1;
            super::super::wake_deadline::host_slept(Duration::from_secs(2 * 3600));
            Err(super::super::google_profile::Error::Unavailable)
        });
        assert_eq!(
            result.err(),
            Some(super::super::google_profile::Error::Unavailable)
        );
        assert_eq!(calls, 1, "no request after the sleep");
    }

    #[test]
    fn native_google_subject_requires_guarded_scoped_identity() {
        use super::super::accounts;
        let root = aterm_tempfile::tempdir().unwrap();
        let mut logins = accounts::Logins::default();
        logins.labels.insert(
            "one".into(),
            accounts::Binding {
                dir: Some(root.path().to_str().unwrap().into()),
                ..Default::default()
            },
        );
        accounts::note_google_identity(&mut logins, "one", "person@example.com", [1; 32]);
        let serialized = logins.to_json();
        let restored = accounts::Logins::from_json(&serialized, 2).unwrap();
        assert_eq!(restored.labels["one"].google_principal, Some([1; 32]));
        assert!(accounts::require_google_principal(&restored, "one", [1; 32]).is_ok());
        assert!(accounts::require_google_principal(&restored, "one", [2; 32]).is_err());
        for case in 0..3 {
            let mut document: aterm_json::Value = aterm_json::from_str(&serialized).unwrap();
            if case == 0 {
                document
                    .as_object_mut()
                    .unwrap()
                    .insert("schema".into(), aterm_json::Value::from(3u64));
            } else {
                let binding = document
                    .as_object_mut()
                    .unwrap()
                    .get_mut("labels")
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .get_mut("one")
                    .unwrap()
                    .as_object_mut()
                    .unwrap();
                if case == 1 {
                    binding.insert(
                        "google_principal".into(),
                        aterm_json::Value::from("invalid"),
                    );
                } else {
                    binding.insert("proved_dir".into(), aterm_json::Value::Null);
                }
            }
            assert!(accounts::Logins::from_json(&document.to_string(), 2).is_err());
        }
        accounts::note_proof(&mut logins, "one", accounts::Proof::Unchecked);
        let revoked = accounts::Logins::from_json(&logins.to_json(), 2).unwrap();
        assert_eq!(revoked.labels["one"].google_principal, None);
    }

    #[test]
    fn native_google_checkpoint_retains_subject_and_exact_grant_bytes() {
        use super::super::accounts;
        use crate::ai::connector::Cli;
        let state = aterm_tempfile::tempdir().unwrap();
        let staging = aterm_tempfile::tempdir().unwrap();
        aterm_secrets::fsx::write_atomic(
            &state.path().join("google-identities.toml"),
            b"[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n",
            0o600,
        ).unwrap();
        let verified =
            publication_fixture(state.path(), staging.path(), "one", "person@example.com");
        let logins = accounts::Logins::default();
        let actual = std::fs::canonicalize(staging.path()).unwrap();
        assert!(
            accounts::write_renewal_checkpoint_for(
                Cli::Agy,
                None,
                "one",
                "person@example.com",
                staging.path().to_str().unwrap(),
                None,
                Some(accounts::Proof::Status),
                1,
                &logins
            )
            .is_err()
        );
        accounts::write_google_renewal_checkpoint(
            "one",
            staging.path().to_str().unwrap(),
            &verified,
            1,
            &logins,
        )
        .unwrap();
        let read = || accounts::read_renewal_checkpoint_for(Cli::Agy, staging.path(), &actual, 2);
        let checkpoint = read().unwrap();
        assert_eq!(checkpoint.google_principal, Some([1; 32]));
        assert_eq!(
            checkpoint.google_grant_digest,
            Some(verified.digest().unwrap())
        );
        let row = accounts::Row {
            label: "one".into(),
            email: Some("person@example.com".into()),
            ..Default::default()
        };
        assert!(accounts::checkpoint_identity_matches(
            Cli::Agy,
            &row,
            &checkpoint
        ));
        assert!(accounts::checkpoint_current(
            &row,
            &accounts::State::default(),
            &logins,
            &checkpoint,
            Some(state.path())
        ));
        let token = verified.path().unwrap().to_owned();
        aterm_secrets::fsx::write_atomic(&token, b"replaced after checkpoint", 0o600).unwrap();
        assert!(read().is_none());
        assert!(!accounts::checkpoint_current(
            &row,
            &accounts::State::default(),
            &logins,
            &checkpoint,
            Some(state.path())
        ));
        assert!(verified.digest().is_err());
    }

    #[test]
    fn native_token_requires_current_nested_schema_not_cached_identity() {
        let valid =
            br#"{"auth_method":"consumer","token":{"access_token":"synthetic-token"},"id_token_email":"wrong@example.com"}"#;
        assert_eq!(
            native_access_token(valid).unwrap().as_str(),
            Some("synthetic-token")
        );
        for invalid in [
            br#"{"auth_method":"consumer","access_token":"synthetic-token"}"#.as_slice(),
            br#"{"id_token_email":"person@example.com","id_token":"synthetic-jwt"}"#,
            br#"{"auth_method":"consumer","token":{"access_token":""}}"#,
            br#"{"auth_method":"consumer","token":{"access_token":"bad\r\nheader"}}"#,
            br#"{"auth_method":"gcp","token":{"access_token":"synthetic-token"}}"#,
            br#"{"token":{"access_token":"synthetic-token"}}"#,
        ] {
            assert!(native_access_token(invalid).is_err());
        }
    }

    #[test]
    fn account_proof_refuses_replaced_grant_and_changed_binding() {
        let state = aterm_tempfile::tempdir().unwrap();
        let profile = aterm_tempfile::tempdir().unwrap();
        let text = "[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n";
        let config = state.path().join("google-identities.toml");
        aterm_secrets::fsx::write_atomic(&config, text.as_bytes(), 0o600).unwrap();
        let revision = registry(state.path())
            .unwrap()
            .binding_revision("agy", "one");
        let gemini = profile.path().join(".gemini");
        let path = gemini.join("antigravity-cli/antigravity-oauth-token");
        aterm_secrets::fsx::ensure_private_dir(path.parent().unwrap()).unwrap();
        let bytes = br#"{"auth_method":"consumer","token":{"access_token":"synthetic-token"}}"#;
        aterm_secrets::fsx::write_atomic(&path, bytes, 0o600).unwrap();
        // Synthetic proof exercises the post-verification publication guard, not Google login.
        let verified = VerifiedGoogleGrant {
            grant: UnverifiedGoogleGrant {
                native: UnverifiedGrant {
                    session: None,
                    path: path.clone(),
                    program: PathBuf::from("/synthetic/agy"),
                    _claim: super::super::browser_window::ProfileLock::claim(profile.path())
                        .unwrap(),
                },
                expected_email: "person@example.com".into(),
                state: state.path().to_owned(),
                account: "one".into(),
                revision,
                recovery: None,
                work: None,
            },
            profile: super::super::google_profile::GoogleProfile {
                email: "person@example.com".into(),
                principal: [1; 32],
            },
            digest: grant_digest(bytes),
        };
        assert!(verified.path().is_ok());
        aterm_secrets::fsx::write_atomic(&path, b"replacement", 0o600).unwrap();
        assert!(verified.path().is_err());
        aterm_secrets::fsx::write_atomic(&path, bytes, 0o600).unwrap();
        assert!(verified.path().is_ok());
        aterm_secrets::fsx::write_atomic(
            &config,
            text.replace("person@", "other@").as_bytes(),
            0o600,
        )
        .unwrap();
        assert!(verified.path().is_err());
    }

    const BINDING: &str = "[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n";

    /// A bound state root, a published profile and a program file that is never run.
    fn published_world() -> (aterm_tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let root = aterm_tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let profile = root.path().join("profile");
        for dir in [&state, &profile] {
            aterm_secrets::fsx::ensure_private_dir(dir).unwrap();
        }
        aterm_secrets::fsx::write_atomic(
            &state.join("google-identities.toml"),
            BINDING.as_bytes(),
            0o600,
        )
        .unwrap();
        let program = root.path().join("agy");
        aterm_secrets::fsx::write_atomic(&program, b"never run", 0o600).unwrap();
        (root, state, profile, program)
    }

    fn write_grant(profile: &Path, bytes: &[u8]) -> PathBuf {
        let path = super::super::upgrade_agy::profile_grant(profile);
        aterm_secrets::fsx::ensure_private_dir(path.parent().unwrap()).unwrap();
        aterm_secrets::fsx::write_atomic(&path, bytes, 0o600).unwrap();
        path
    }

    /// The launch check of a published grant answers from this Mac alone (§4.15 rule 6): a grant
    /// whose bytes hold no token at all is still a signed-in row, because the check asks only
    /// that the file is present and opens, so it holds nothing a provider request could carry,
    /// and records no probe.
    #[test]
    fn a_launch_checks_a_published_grant_locally_without_its_contents() {
        let (_root, state, profile, program) = published_world();
        write_grant(&profile, b"opaque bytes no request can carry");
        let (checked, probes) = super::super::provider_read::captured(|| {
            check_google_published(&program, &state, "one", "person@example.com", &profile)
        });
        assert!(checked.is_ok(), "{:?}", checked.err());
        assert!(probes.is_empty());
    }

    /// CONTROL: with the launch reading the grant's contents, as the userinfo request it once
    /// made had to, the same grant is refused, so the test above holds only because nothing reads
    /// them.
    #[test]
    fn control_a_launch_reading_grant_contents_refuses_the_same_grant() {
        let (_root, state, profile, program) = published_world();
        write_grant(&profile, b"opaque bytes no request can carry");
        let checked = with_launch_reading_grant_contents(|| {
            check_google_published(&program, &state, "one", "person@example.com", &profile)
        });
        assert!(matches!(checked, Err(PublishedRefusal::Refused(_))));
    }

    #[test]
    fn a_published_grant_that_is_absent_or_does_not_open_is_signed_out_here() {
        let (_root, state, profile, program) = published_world();
        let check =
            || check_google_published(&program, &state, "one", "person@example.com", &profile);
        assert_eq!(check().err(), Some(PublishedRefusal::SignedOut));
        let path = write_grant(&profile, b"");
        assert_eq!(check().err(), Some(PublishedRefusal::SignedOut));
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(check().err(), Some(PublishedRefusal::SignedOut));
        std::fs::remove_dir(&path).unwrap();
        write_grant(&profile, b"grant");
        assert!(check().is_ok());
        assert!(matches!(
            check_google_published(&program, &state, "one", "other@example.com", &profile),
            Err(PublishedRefusal::Refused(_))
        ));
    }

    #[test]
    fn a_published_worker_rebinds_its_profile_and_rechecks_before_admission() {
        let (_root, state, profile, program) = published_world();
        let path = write_grant(&profile, b"grant");
        let gemini = profile.join(".gemini");
        let prepared =
            check_google_published(&program, &state, "one", "person@example.com", &profile)
                .unwrap()
                .prepare_worker(super::super::worker::Starter {
                    exe: PathBuf::from("/inherited/agy"),
                    env: vec![
                        ("HOME".into(), "/inherited/home".into()),
                        ("GEMINI_API_KEY".into(), "synthetic-key".into()),
                    ],
                    args: Ok(vec![
                        "-gemini_dir=/inherited".into(),
                        "--app_data_dir".into(),
                        "old".into(),
                        "--mode".into(),
                        "plan".into(),
                    ]),
                    unset: Vec::new(),
                })
                .unwrap();
        let starter = prepared.starter().unwrap();
        assert_eq!(starter.exe, program);
        assert_eq!(
            starter.args.as_ref().unwrap(),
            &vec![
                "--mode".to_owned(),
                "plan".into(),
                "--gemini_dir".into(),
                gemini.to_str().unwrap().into(),
                "--app_data_dir=antigravity-cli".into()
            ]
        );
        assert!(
            starter
                .env
                .iter()
                .any(|(k, v)| k == "HOME" && v == profile.to_str().unwrap())
        );
        assert!(!starter.env.iter().any(|(k, _)| k == "GEMINI_API_KEY"));
        assert!(starter.unset.contains(&"GEMINI_API_KEY"));
        assert!(super::super::browser_window::ProfileLock::claim(&profile).is_err());
        // agy rewrites its own grant as it renews its token: that is still this row's login.
        aterm_secrets::fsx::write_atomic(&path, b"renewed by agy", 0o600).unwrap();
        assert!(prepared.starter().is_ok());
        std::fs::remove_file(&path).unwrap();
        assert!(prepared.starter().is_err());
        write_grant(&profile, b"grant");
        aterm_secrets::fsx::write_atomic(
            &state.join("google-identities.toml"),
            BINDING.replace("person@", "other@").as_bytes(),
            0o600,
        )
        .unwrap();
        assert!(prepared.starter().is_err());
    }

    #[test]
    fn worker_profile_rebinding_respects_flag_looking_values_and_refuses_unknown_flags() {
        let bind = |flags: &[&str]| {
            super::super::upgrade_agy::bound_worker_flags(
                &flags.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
                Path::new("/private/.gemini"),
            )
        };
        let flags = bind(&[
            "--model",
            "--gemini_dir=not-a-profile",
            "--gemini_dir",
            "--mode=not-a-permission",
            "--mode=plan",
        ])
        .unwrap();
        assert_eq!(
            &flags[..3],
            ["--model", "--gemini_dir=not-a-profile", "--mode=plan"]
        );
        for flags in [
            &["--unknown"][..],
            &["--gemini_dir"],
            &["--settings", "{}"],
            &["--", "operand"],
        ] {
            assert!(bind(flags).is_err());
        }
    }

    fn request(state: &str) -> String {
        let scope = SCOPES.join("%20");
        format!(
            "https://accounts.google.com/o/oauth2/auth?client_id=native-fixture.apps.googleusercontent.com&redirect_uri=https%3A%2F%2Fantigravity.google%2Foauth-callback&response_type=code&access_type=offline&code_challenge_method=S256&code_challenge=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&state={state}&scope={scope}"
        )
    }

    #[test]
    fn google_callback_cannot_cross_native_requests_or_change_its_destination() {
        let first = Authorization::admit(&request("first-state")).unwrap();
        let second = Authorization::admit(&request("second-state")).unwrap();
        let code = first
            .callback(
                "https://antigravity.google/oauth-callback?code=fixture-code&state=first-state",
            )
            .unwrap();
        assert_eq!(code.value.as_str(), Some("fixture-code"));
        assert_eq!(code.binding, first.binding);
        assert_ne!(code.binding, second.binding);
        for callback in [
            "https://antigravity.google/oauth-callback?code=fixture-code&state=second-state",
            "https://antigravity.google/oauth-callback?code=fixture-code",
            "https://antigravity.google/oauth-callback?code=fixture-code&state=first-state&state=first-state",
            "https://antigravity.google/oauth-callback?code=fixture-code%0Anext&state=first-state",
            "https://antigravity.google/oauth-callback?code=fixture-code&state=first-state&error=denied",
            "https://antigravity.google.evil.test/oauth-callback?code=fixture-code&state=first-state",
            "https://antigravity.google/elsewhere?code=fixture-code&state=first-state",
        ] {
            assert!(first.callback(callback).is_err());
        }
        for url in [
            request("first-state") + "&client_id=other.apps.googleusercontent.com",
            request("first-state").replace("S256", "plain"),
            request("first-state").replace("antigravity.google", "attacker.test"),
            request("first-state").replace("auth/aicode", "auth/gmail.readonly"),
            request("first-state").replace("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "short"),
        ] {
            assert!(Authorization::admit(&url).is_err());
        }
    }

    #[test]
    fn expired_native_attempt_starts_no_child_and_creates_no_profile() {
        let root = aterm_tempfile::tempdir().unwrap();
        let profile = root.path().join("not-created");
        let result = run(
            Path::new("/usr/bin/true"),
            &profile,
            WakeDeadline::after(Duration::ZERO),
            |_, _| panic!("expired attempt invoked approval"),
        );
        assert!(matches!(result, Err(Error::Deadline)));
        assert!(!profile.exists());
    }

    #[test]
    fn explicit_google_path_needs_credentials_only_after_a_native_handoff() {
        let state = aterm_tempfile::tempdir().unwrap();
        let home = aterm_tempfile::tempdir().unwrap();
        let registry = "[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n";
        aterm_uds::private_file::write_private(
            state.path(),
            "google-identities.toml",
            registry.as_bytes(),
        )
        .unwrap();
        let result = run_google(
            Path::new("/not-a-native-program"),
            state.path(),
            home.path(),
            "one",
            WakeDeadline::after(Duration::from_secs(30)),
        );
        assert!(matches!(
            result,
            Err(GoogleLoginError::Native(Error::Profile))
        ));
        assert!(!state.path().join("agy-logins").exists());
        assert!(!state.path().join("google-identities").exists());
        let result = run_google(
            Path::new("/not-a-native-program"),
            state.path(),
            home.path(),
            "other",
            WakeDeadline::after(Duration::from_secs(30)),
        );
        assert!(matches!(result, Err(GoogleLoginError::Configuration)));
    }

    #[test]
    fn staging_claim_precedes_seed_and_refuses_existing_grants() {
        let root = aterm_tempfile::tempdir().unwrap();
        let previous = root.path().join("previous");
        let staging = root.path().join("staging");
        let suffix = ".gemini/antigravity-cli/antigravity-oauth-token";
        let source = previous.join(suffix);
        aterm_secrets::fsx::ensure_private_dir(source.parent().unwrap()).unwrap();
        aterm_secrets::fsx::write_atomic(&source, b"original", 0o600).unwrap();
        let claim = stage_profile(&staging, Some(&previous)).unwrap();
        assert!(super::super::browser_window::ProfileLock::claim(&staging).is_err());
        aterm_secrets::fsx::write_atomic(&source, b"new-live-grant", 0o600).unwrap();
        assert!(matches!(
            stage_profile(&staging, Some(&previous)),
            Err(Error::Profile)
        ));
        assert_eq!(std::fs::read(staging.join(suffix)).unwrap(), b"original");
        drop(claim);
        assert!(matches!(
            stage_profile(&staging, Some(&previous)),
            Err(Error::Profile)
        ));
        assert_eq!(std::fs::read(&source).unwrap(), b"new-live-grant");
        assert_eq!(std::fs::read(staging.join(suffix)).unwrap(), b"original");
    }

    #[test]
    fn staged_google_login_binds_the_row_before_touching_its_profile() {
        let state = aterm_tempfile::tempdir().unwrap();
        let home = aterm_tempfile::tempdir().unwrap();
        aterm_uds::private_file::write_private(
            state.path(),
            "google-identities.toml",
            b"[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='agy'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n",
        )
        .unwrap();
        let staging = state.path().join("staged");
        let previous = state.path().join("previous");
        let attempt = |email: &str, profile: &Path| {
            run_google_staged(
                Path::new("/not-a-native-program"),
                state.path(),
                home.path(),
                "one",
                StagedGoogleLogin {
                    profile,
                    previous: Some(&previous),
                    expected_email: email,
                    recovery: None,
                },
                WakeDeadline::after(Duration::from_secs(30)),
            )
        };
        assert!(matches!(
            attempt("other@example.com", &staging),
            Err(GoogleLoginError::Configuration)
        ));
        assert!(!staging.exists());
        assert!(matches!(
            attempt("person@example.com", Path::new("relative")),
            Err(GoogleLoginError::Configuration)
        ));
        let registry = registry(state.path()).unwrap();
        let identity = registry.resolve("agy", "one").unwrap().0;
        let active = native_profile(state.path(), "one", identity);
        assert!(matches!(
            attempt("person@example.com", &active),
            Err(GoogleLoginError::Configuration)
        ));
        assert!(!active.exists());
        let token = previous.join(".gemini/antigravity-cli/antigravity-oauth-token");
        aterm_secrets::fsx::ensure_private_dir(token.parent().unwrap()).unwrap();
        aterm_secrets::fsx::write_atomic(&token, b"synthetic-native-grant", 0o600).unwrap();
        assert!(matches!(
            attempt("PERSON@example.com", &staging),
            Err(GoogleLoginError::Native(Error::Profile))
        ));
        assert!(!active.exists());
        assert_eq!(std::fs::read(&token).unwrap(), b"synthetic-native-grant");
        assert_eq!(
            std::fs::read(staging.join(".gemini/antigravity-cli/antigravity-oauth-token")).unwrap(),
            b"synthetic-native-grant"
        );
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn xnu_zombie_only_group_is_a_completed_cleanup() {
        use std::io::Read as _;
        use std::os::fd::AsRawFd as _;

        struct Forked(i32);
        impl Drop for Forked {
            fn drop(&mut self) {
                if self.0 <= 1 {
                    return;
                }
                let mut status = 0;
                // SAFETY: this test's child leads its own group and remains unreaped here.
                unsafe {
                    libc::kill(-self.0, libc::SIGKILL);
                    libc::waitpid(self.0, &raw mut status, 0);
                }
            }
        }

        let (mut proof_r, proof_w) = std::io::pipe().unwrap();
        let (proof_r_fd, proof_w_fd) = (proof_r.as_raw_fd(), proof_w.as_raw_fd());
        // SAFETY: the child calls only async-signal-safe functions (`close`, `setpgid`, `write`,
        // `_exit`) on descriptors prepared before the fork, then exits without unwinding.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork: {}", std::io::Error::last_os_error());
        if pid == 0 {
            // SAFETY: the forked child follows the contract above.
            unsafe {
                libc::close(proof_r_fd);
                let ok = libc::setpgid(0, 0) == 0;
                let byte = u8::from(ok);
                libc::write(proof_w_fd, (&raw const byte).cast(), 1);
                libc::close(proof_w_fd);
                libc::_exit(i32::from(!ok));
            }
        }
        assert!(pid > 1, "a forked child has a positive process ID");
        let mut child = Forked(pid);
        drop(proof_w);
        let mut proof = [0u8; 1];
        proof_r.read_exact(&mut proof).unwrap();
        assert_eq!(proof, [1], "the child made itself its group leader");
        drop(proof_r);

        let until = Instant::now() + Duration::from_secs(60);
        loop {
            // SAFETY: signal zero observes this child's positive, uniquely held group.
            let found = unsafe { libc::kill(-pid, 0) };
            let error = std::io::Error::last_os_error();
            if found < 0 && error.raw_os_error() == Some(libc::EPERM) {
                break;
            }
            assert!(
                Instant::now() < until,
                "the child did not reach XNU's zombie-only group state: {error}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        finish_exited_group(pid).expect("a complete zombie-only group is already ended");

        let mut status = 0;
        // SAFETY: this test owns and reaps this child exactly once.
        assert_eq!(unsafe { libc::waitpid(pid, &raw mut status, 0) }, pid);
        child.0 = 0;
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);
    }

    #[test]
    fn rejected_staging_reset_preserves_refusals_and_concurrent_changes() {
        const SAVED: &[u8] =
            br#"{"auth_method":"consumer","token":{"access_token":"synthetic-only"}}"#;
        for scenario in 0..8 {
            let profile = aterm_tempfile::tempdir().unwrap();
            let path = profile
                .path()
                .join(".gemini/antigravity-cli/antigravity-oauth-token");
            aterm_secrets::fsx::ensure_private_dir(path.parent().unwrap()).unwrap();
            let saved = if scenario == 7 {
                br#"{"auth_method":"consumer","metadata":"keep","token":{"access_token":"synthetic-only","refresh_token":"synthetic-refresh","expiry":"2100-01-01T00:00:00Z"}}"#.as_slice()
            } else {
                SAVED
            };
            aterm_secrets::fsx::write_atomic(&path, saved, 0o600).unwrap();
            let current = std::cell::Cell::new(true);
            let claim = super::super::browser_window::ProfileLock::claim(profile.path()).unwrap();
            let deadline = if scenario == 6 {
                WakeDeadline::after(Duration::ZERO)
            } else {
                WakeDeadline::after(Duration::from_secs(3600))
            };
            let result = prepare_rejected_staging(
                profile.path(),
                &StagedRecovery {
                    expected_email: "person@example.com",
                    current: &|| current.get(),
                },
                deadline,
                |token, wait| {
                    assert_eq!(token, "synthetic-only");
                    assert!(
                        !wait.is_zero() && wait <= crate::harness::google_profile::REQUEST_HANG,
                        "the account check is capped, not given the hour: {wait:?}"
                    );
                    assert!(
                        super::super::browser_window::ProfileLock::claim(profile.path()).is_err()
                    );
                    match scenario {
                        0 => return Ok(()),
                        1 => {
                            return Err(super::super::google_profile::Error::Refused(
                                "transport failed",
                            ));
                        }
                        2 => {
                            return Err(super::super::google_profile::Error::Refused(
                                "identity mismatch",
                            ));
                        }
                        4 => current.set(false),
                        5 => aterm_secrets::fsx::write_atomic(&path, b"replaced", 0o600).unwrap(),
                        _ => {}
                    }
                    Err(super::super::google_profile::Error::CredentialRejected)
                },
            );
            match scenario {
                0..=2 => assert_eq!(result, Ok(false)),
                3 | 7 => assert_eq!(result, Ok(true)),
                4 => assert_eq!(result, Err(Error::Authorization)),
                5 => assert_eq!(result, Err(Error::Profile)),
                6 => assert_eq!(result, Err(Error::Deadline)),
                _ => unreachable!(),
            }
            if scenario == 3 {
                assert!(!path.exists());
            } else if scenario == 7 {
                let mut expected: aterm_json::Value = aterm_json::from_slice(saved).unwrap();
                expected
                    .as_object_mut()
                    .unwrap()
                    .get_mut("token")
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert("expiry".into(), "2000-01-01T00:00:00Z".into());
                let actual: aterm_json::Value =
                    aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                assert_eq!(actual, expected);
            } else {
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    if scenario == 5 {
                        b"replaced".as_slice()
                    } else {
                        SAVED
                    }
                );
            }
            drop(claim);
        }
    }

    #[test]
    #[ignore = "contacts the installed native agy anonymously; ATERM_TEST_AGY names its pinned binary"]
    fn native_expired_and_malformed_grants_reach_a_fresh_google_handoff() {
        let program = std::env::var_os("ATERM_TEST_AGY")
            .map(PathBuf::from)
            .expect("ATERM_TEST_AGY is required");
        let mut failed = Vec::new();
        for (case, seed) in [
            ("empty", None),
            ("expired", Some(br#"{"auth_method":"consumer","id_token_email":"synthetic@example.invalid","token":{"access_token":"aterm-synthetic-access","token_type":"Bearer","refresh_token":"aterm-synthetic-invalid-refresh","expiry":"2000-01-01T00:00:00Z"}}"#.as_slice())),
            ("rejected-before-expiry", Some(br#"{"auth_method":"consumer","id_token_email":"synthetic@example.invalid","token":{"access_token":"aterm-synthetic-access","token_type":"Bearer","refresh_token":"aterm-synthetic-invalid-refresh","expiry":"2100-01-01T00:00:00Z"}}"#.as_slice())),
            ("malformed", Some(b"invalid synthetic native grant".as_slice())),
        ] {
            let profile = aterm_tempfile::tempdir().unwrap();
            if let Some(seed) = seed {
                let path = profile.path().join(".gemini/antigravity-cli/antigravity-oauth-token");
                aterm_secrets::fsx::ensure_private_dir(path.parent().unwrap()).unwrap();
                aterm_secrets::fsx::write_atomic(&path, seed, 0o600).unwrap();
            }
            let staging = aterm_tempfile::tempdir().unwrap();
            let claim = stage_profile(staging.path(), Some(profile.path())).unwrap();
            let original = std::fs::read(profile.path().join(".gemini/antigravity-cli/antigravity-oauth-token")).ok();
            let mut observed = false;
            let result = run_claimed(&program, staging.path(), Some(claim), Some(StagedRecovery { expected_email: "synthetic@example.invalid", current: &|| true }), WakeDeadline::after(Duration::from_secs(60)), |_, _| {
                observed = true;
                Err(Error::Callback)
            });
            println!("native recovery {case}: handoff={observed} failure={:?}", result.as_ref().err());
            if !observed || !matches!(result, Err(Error::Callback)) {
                failed.push(case);
            }
            assert_eq!(std::fs::read(profile.path().join(".gemini/antigravity-cli/antigravity-oauth-token")).ok(), original);
            let _claim = super::super::browser_window::ProfileLock::claim(staging.path()).unwrap();
        }
        assert!(
            failed.is_empty(),
            "native recovery omitted Google handoff: {failed:?}"
        );
    }

    #[test]
    #[ignore = "contacts the installed native agy anonymously; ATERM_TEST_AGY names its pinned binary"]
    fn native_headless_handoff_is_admitted_and_cancellation_releases_its_profile() {
        let program = std::env::var_os("ATERM_TEST_AGY")
            .map(PathBuf::from)
            .expect("ATERM_TEST_AGY is required");
        let profile = aterm_tempfile::tempdir().unwrap();
        let mut observed = false;
        let result = run(
            &program,
            profile.path(),
            WakeDeadline::after(Duration::from_secs(30)),
            |request, _| {
                assert!(
                    request
                        .url()
                        .starts_with("https://accounts.google.com/o/oauth2/auth?")
                );
                observed = true;
                Err(Error::Callback)
            },
        );
        assert!(observed, "native handoff was not observed");
        assert!(matches!(result, Err(Error::Callback)));
        let _claim = super::super::browser_window::ProfileLock::claim(profile.path()).unwrap();
    }
}
