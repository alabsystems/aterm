// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit Google federation from a native vendor login, without changing its default policy.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_json::{Value, json};

use super::cli::shared_provider::{
    KimiNativeError, KimiProof, LoginProof, verify_native_kimi_classified,
};
use super::google_browser::{MemoryBrowser, Page as BrowserPage, PendingSession};
use super::google_credentials::Credentials;
use super::google_identity::{Identity, Registry};
use super::headless_oauth::{self as oauth, FlowBound, Gate, Step, VendorApprover};
use super::login_drive::{self, Approval, Handoff};
use crate::ai::connector::{Cli, Region};

#[path = "google_federation_recovery.rs"]
pub(super) mod recovery;

const REGISTRY_FILE: &str = "google-identities.toml";

#[derive(Clone, PartialEq, Eq)]
pub(super) struct NativeAttempt {
    recovery: Option<login_drive::RecoveryKey>,
    admission: Option<login_drive::RecoveryAdmission>,
    cli: Cli,
    store_cli: Cli,
    device: bool,
    row: super::accounts::Row,
    staging: PathBuf,
    state: PathBuf,
    hosts: Vec<String>,
}

impl NativeAttempt {
    pub(super) fn state(&self) -> &Path {
        &self.state
    }

    pub(super) fn capture(request: &login_drive::Request<'_>) -> Self {
        Self {
            recovery: request.recovery,
            admission: request.admission.cloned(),
            cli: request.cli,
            store_cli: request.store.cli(),
            device: matches!(
                request.route.drive,
                crate::ai::connector::Drive::Lines {
                    completion: crate::ai::connector::Completion::Device,
                    ..
                }
            ),
            row: request.row.clone(),
            staging: request.staging.into(),
            state: request
                .store
                .dir
                .parent()
                .unwrap_or(&request.store.dir)
                .into(),
            hosts: request
                .hosts
                .iter()
                .map(|host| (*host).to_owned())
                .collect(),
        }
    }
}

#[derive(Clone)]
struct FederationScope {
    attempt: NativeAttempt,
    provider: String,
    identity: Identity,
    expected: String,
    revision: [u8; 32],
    region: Region,
}

impl FederationScope {
    fn current(&self) -> Result<(), String> {
        if let Some(admission) = &self.attempt.admission {
            if self.attempt.recovery != Some(admission.key()) {
                return Err("Google recovery admission differs from its key".into());
            }
            admission.current(None)?;
        }
        let registry = read_registry(&self.attempt.state)
            .ok()
            .flatten()
            .and_then(|bytes| parse_registry(&bytes))
            .ok_or("Google federation binding is unavailable")?;
        let (identity, _) = registry
            .resolve(&self.provider, &self.attempt.row.label)
            .ok_or("Google federation binding changed")?;
        if identity != &self.identity
            || registry.binding_revision(&self.provider, &self.attempt.row.label)
                != Some(self.revision)
        {
            return Err("Google federation binding changed".into());
        }
        Ok(())
    }

    fn check_attempt(&self) -> Result<(), String> {
        let hosts = Cli::Kimi
            .accounts()
            .signin
            .as_ref()
            .and_then(|signin| signin.route(Some(crate::ai::connector::Flow::Device)))
            .and_then(|route| route.hosts.of(Some(self.region)))
            .ok_or("Kimi federation route is unavailable")?;
        if self.attempt.cli != Cli::Kimi
            || self.attempt.store_cli != Cli::Kimi
            || !self.attempt.device
            || !self.attempt.staging.is_absolute()
            || !self.attempt.state.is_absolute()
            || !self
                .attempt
                .hosts
                .iter()
                .map(String::as_str)
                .eq(hosts.iter().copied())
            || self.attempt.row.vendor.flow.is_some()
            || self.attempt.row.vendor.region != Some(self.region)
            || self.attempt.row.held_by.is_some()
            || !self
                .attempt
                .row
                .expected_identity_email()
                .is_some_and(|email| email.eq_ignore_ascii_case(&self.expected))
        {
            return Err("Google federation native attempt is not admitted".into());
        }
        Ok(())
    }

    fn check_request(&self, request: &login_drive::Request<'_>) -> Result<(), String> {
        if NativeAttempt::capture(request) != self.attempt {
            return Err("Google federation native attempt changed".into());
        }
        self.check_attempt()?;
        self.current()
    }
}

pub(super) struct PendingFederation {
    scope: FederationScope,
    session: Option<PendingSession>,
    native: Option<KimiProof>,
}

/// Native Kimi's canonical account proof and the Google binding that authorized its flow.
pub(super) struct VerifiedKimiGoogle<'a> {
    scope: FederationScope,
    native: KimiProof,
    journal: Option<&'a recovery::Journal>,
}

impl VerifiedKimiGoogle<'_> {
    pub(super) fn check_browser_identity(&self, identity: &Identity) -> Result<(), String> {
        self.scope.current()?;
        if let Some(admission) = &self.scope.attempt.admission {
            admission.current(Some(self.native.principal()))?;
        }
        if let Some(journal) = self.journal {
            journal.check_attempt(&self.scope.attempt)?;
        }
        if identity != &self.scope.identity
            || !self
                .native
                .identity()
                .eq_ignore_ascii_case(&self.scope.expected)
        {
            return Err("Google cookie identity does not match the verified Kimi account".into());
        }
        self.native.check_native(
            &self.scope.attempt.staging,
            self.scope.region,
            &self.scope.expected,
        )
    }
}

impl PendingFederation {
    pub(super) fn verified_email(&self) -> Option<&str> {
        self.native.as_ref().map(LoginProof::identity)
    }

    pub(super) fn complete(
        mut self,
        request: &login_drive::Request<'_>,
        outcome: &login_drive::Outcome,
        deadline: Instant,
        journal: Option<&recovery::Journal>,
    ) -> Result<(), String> {
        let cached = self.native.take();
        self.complete_checked(request, outcome, deadline, journal, |scope| {
            if let Some(journal) = journal {
                journal.check(request)?;
            }
            if let Some(native) = cached {
                native.check_native(request.staging, scope.region, &scope.expected)?;
                return Ok(native);
            }
            let now = aterm_time::unix_secs()
                .and_then(|now| i64::try_from(now).ok())
                .ok_or("Kimi account clock is unavailable")?;
            let result = verify_native_kimi_classified(
                request.staging,
                scope.region,
                &scope.expected,
                now,
                deadline,
            );
            if let Some(journal) = journal {
                journal.check(request)?;
            }
            match result {
                Ok(Some(native)) => Ok(native),
                Ok(None) => Err("Kimi did not retain a native credential".into()),
                Err(KimiNativeError::Refused(why)) => {
                    if let Some(journal) = journal {
                        journal.reject(request)?;
                    }
                    Err(why)
                }
                Err(error) => Err(error.to_string()),
            }
        })
        .map(|_| ())
    }

    pub(super) fn commit_recovered(
        mut self,
        request: &login_drive::Request<'_>,
        journal: &recovery::Journal,
        deadline: Instant,
    ) -> Result<KimiProof, String> {
        let native = self
            .native
            .take()
            .ok_or("Kimi recovered proof is unavailable")?;
        self.complete_checked(
            request,
            &login_drive::Outcome::SignedIn {
                email: None,
                page: false,
            },
            deadline,
            Some(journal),
            |scope| {
                native.check_native(request.staging, scope.region, &scope.expected)?;
                Ok(native)
            },
        )?
        .ok_or_else(|| "Kimi recovered proof is unavailable".into())
    }

    #[cfg(test)]
    fn complete_using(
        self,
        request: &login_drive::Request<'_>,
        outcome: &login_drive::Outcome,
        deadline: Instant,
        verify: impl FnOnce(&FederationScope) -> Result<KimiProof, String>,
    ) -> Result<(), String> {
        self.complete_checked(request, outcome, deadline, None, verify)
            .map(|_| ())
    }

    fn complete_checked(
        self,
        request: &login_drive::Request<'_>,
        outcome: &login_drive::Outcome,
        deadline: Instant,
        journal: Option<&recovery::Journal>,
        verify: impl FnOnce(&FederationScope) -> Result<KimiProof, String>,
    ) -> Result<Option<KimiProof>, String> {
        if !matches!(outcome, login_drive::Outcome::SignedIn { .. }) {
            return Ok(None);
        }
        self.scope.check_request(request)?;
        if Instant::now() >= deadline {
            return Err("Kimi federation verification deadline expired".into());
        }
        let native = verify(&self.scope)?;
        self.scope.check_request(request)?;
        if Instant::now() >= deadline {
            return Err("Kimi federation verification deadline expired".into());
        }
        let proof = VerifiedKimiGoogle {
            scope: self.scope,
            native,
            journal,
        };
        proof.check_browser_identity(&proof.scope.identity)?;
        if let Some(session) = self.session {
            session
                .commit_kimi(&proof)
                .map_err(|_| "Verified Kimi Google session could not be persisted")?;
        }
        if Instant::now() >= deadline {
            return Err("Kimi federation verification deadline expired".into());
        }
        proof.check_browser_identity(&proof.scope.identity)?;
        Ok(Some(proof.native))
    }
}

/// Missing configuration keeps the ordinary vendor route. An explicit but unusable binding
/// fails quietly; it never switches to another identity or asks the person to sign in.
pub(super) fn configured(approver: &mut VendorApprover, handoff: &Handoff<'_>) -> Option<Approval> {
    let bytes = match read_registry(&approver.google_state) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return None,
        Err(why) => return Some(Approval::NotRun(why.into())),
    };
    let registry = match parse_registry(&bytes) {
        Some(registry) => registry,
        None => {
            return Some(Approval::NotRun(
                "Google identity registry is invalid".into(),
            ));
        }
    };
    let (cli, account) = &approver.google_account;
    let provider = match provider_key(cli.id(), handoff.hosts) {
        Some(provider) => provider,
        None => {
            return Some(Approval::NotRun(
                "Google federation has no admitted provider realm".into(),
            ));
        }
    };
    let (identity, binding) = registry.resolve(provider, account)?;
    if !matches!(provider, "kimi-global" | "kimi-mainland-cn") {
        return Some(quiet(
            "Google federation for this provider is not implemented",
        ));
    }
    if !handoff
        .email
        .is_some_and(|email| email.eq_ignore_ascii_case(binding.expected_email()))
    {
        return Some(Approval::Refused(
            "Google binding does not match the native account identity".into(),
        ));
    }
    let Some(home) = approver.home.as_deref() else {
        return Some(Approval::NotRun(
            "Google credential home is unavailable".into(),
        ));
    };
    let Some(attempt) = approver
        .native_attempt
        .as_ref()
        .filter(|attempt| attempt.cli == *cli && attempt.row.label == *account)
        .cloned()
    else {
        return Some(quiet("Google federation has no bound native attempt"));
    };
    let Some(revision) = registry.binding_revision(provider, account) else {
        return Some(quiet("Google federation binding is unavailable"));
    };
    let scope = FederationScope {
        attempt,
        provider: provider.into(),
        identity: identity.clone(),
        expected: binding.expected_email().into(),
        revision,
        region: if provider == "kimi-global" {
            Region::Global
        } else {
            Region::MainlandCn
        },
    };
    if let Err(why) = scope.check_attempt().and_then(|()| scope.current()) {
        return Some(quiet(why));
    }
    if !handoff
        .hosts
        .iter()
        .copied()
        .eq(scope.attempt.hosts.iter().map(String::as_str))
        || handoff.completion != crate::ai::connector::Completion::Device
    {
        return Some(quiet(
            "Google federation handoff differs from the native attempt",
        ));
    }
    let mut session = None;
    let result = run(identity, approver, handoff, home, &mut session, || {
        scope.current().is_ok()
    });
    if matches!(result, Approval::Approved { .. }) {
        approver.google_pending = Some(PendingFederation {
            scope,
            session,
            native: None,
        });
    }
    Some(result)
}

fn parse_registry(bytes: &[u8]) -> Option<Registry> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| Registry::parse(text).ok())
}

pub(super) fn read_registry(root: &Path) -> Result<Option<Vec<u8>>, &'static str> {
    use aterm_uds::private_file::{PrivateFileError, read_private};
    match read_private(root, REGISTRY_FILE, super::google_identity::MAX_BYTES) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(PrivateFileError::Missing) => Ok(None),
        Err(PrivateFileError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(_) => Err("Google identity registry is unavailable or not private"),
    }
}

fn provider_key<'a>(cli: &'a str, hosts: &[&str]) -> Option<&'a str> {
    if cli != "kimi" {
        return Some(cli);
    }
    let global = hosts.contains(&"www.kimi.ai");
    let mainland = hosts.contains(&"www.kimi.com");
    match (global, mainland) {
        (true, false) => Some("kimi-global"),
        (false, true) => Some("kimi-mainland-cn"),
        _ => None,
    }
}

/// The inner Google client and callback are separate from the native CLI's outer flow.
/// Construction requires an observed vendor-owned callback, never one taken from config.
#[derive(PartialEq, Eq)]
struct InnerFlow {
    bound: FlowBound,
    callback: String,
    scope: String,
}

impl InnerFlow {
    fn admit(url: &str, hosts: &[&str]) -> Option<Self> {
        if login_drive::https_host(url).as_deref() != Some("accounts.google.com") {
            return None;
        }
        let path = url
            .split_once("://")?
            .1
            .split_once('/')?
            .1
            .split(['?', '#'])
            .next()?;
        if !matches!(
            path,
            "o/oauth2/v2/auth" | "o/oauth2/auth" | "o/oauth2/auth/identifier"
        ) {
            return None;
        }
        let bound = FlowBound::of(url).ok()?;
        if bound.client_id.as_deref().is_none_or(str::is_empty) {
            return None;
        }
        let callback = bound.redirect_uri.as_ref()?;
        if !login_drive::pinned(callback, hosts) {
            return None;
        }
        // Known Kimi callbacks remain scoped to the handoff's realm. Other providers need
        // their own callback and grant handling before this route can run.
        if !matches!(
            callback.as_str(),
            "https://www.kimi.ai/google-callback" | "https://www.kimi.com/google-callback"
        ) {
            return None;
        }
        let scopes = oauth::query_values(url, "scope");
        let scope = scopes.first()?;
        if scope.trim().is_empty()
            || scopes.iter().any(|value| value != scope)
            || !scope.split_whitespace().all(|value| {
                matches!(
                    value,
                    "openid"
                        | "email"
                        | "profile"
                        | "https://www.googleapis.com/auth/userinfo.email"
                        | "https://www.googleapis.com/auth/userinfo.profile"
                )
            })
        {
            return None;
        }
        Some(Self {
            callback: callback.clone(),
            scope: scope.clone(),
            bound,
        })
    }

    fn admits_google(&self, url: &str, hosts: &[&str]) -> bool {
        login_drive::https_host(url).as_deref() == Some("accounts.google.com")
            && self.bound.outside(url, hosts).is_none()
            && oauth::query_values(url, "error").is_empty()
            && oauth::query_values(url, "scope")
                .iter()
                .all(|scope| scope == &self.scope)
    }

    fn is_callback(&self, url: &str) -> bool {
        let location = |value: &str| value.split(['?', '#']).next().unwrap_or("").to_owned();
        location(url) == location(&self.callback)
            && {
                let codes = oauth::query_values(url, "code");
                codes.len() == 1 && !codes[0].is_empty()
            }
            && oauth::query_values(url, "error").is_empty()
            && self.bound.state.as_ref().is_none_or(|state| {
                let got = oauth::query_values(url, "state");
                !got.is_empty() && got.iter().all(|value| value == state)
            })
    }
}

fn bind_target(pending: &mut Option<InnerFlow>, url: &str, hosts: &[&str]) -> Option<InnerFlow> {
    if let Some(flow) = pending.as_ref() {
        return if flow.admits_google(url, hosts) {
            pending.take()
        } else {
            None
        };
    }
    InnerFlow::admit(url, hosts)
}

fn quiet(why: impl ToString) -> Approval {
    Approval::NotRun(why.to_string())
}

fn run(
    identity: &Identity,
    approver: &VendorApprover,
    handoff: &Handoff<'_>,
    home: &Path,
    pending_session: &mut Option<PendingSession>,
    configuration_current: impl Fn() -> bool,
) -> Approval {
    if Instant::now() >= handoff.deadline {
        return quiet("Google federation reached its deadline");
    }
    if !configuration_current() {
        return quiet("Google identity configuration changed during sign-in");
    }
    let mut credentials: Option<Credentials> = None;
    let outer = match FlowBound::of(handoff.url) {
        Ok(bound) => bound,
        Err(why) => return Approval::Refused(why.into()),
    };
    if !login_drive::pinned(handoff.url, handoff.hosts) {
        return Approval::Refused("Native sign-in URL is outside its provider realm".into());
    }
    let mut browser = match MemoryBrowser::start(
        identity,
        &approver.google_state,
        Some(home),
        handoff.deadline,
    ) {
        Ok(browser) => browser,
        Err(error) => return quiet(error),
    };
    // An absent, expired or damaged snapshot can always fall back to the explicitly bound
    // credential. Validation completes before any cookie is imported.
    match browser.restore_session(home) {
        Ok(_) | Err(super::google_browser::Error::Session) => {}
        // A failed browser import may have applied only part of the snapshot. Reap this
        // context instead of treating broken browser control as a clean fresh login.
        Err(error) => return quiet(error),
    }
    let root = match browser.open(handoff.url) {
        Ok(root) => root,
        Err(error) => return quiet(error),
    };
    let mut google: Option<(BrowserPage, InnerFlow)> = None;
    let mut pending_inner: Option<InnerFlow> = None;
    let mut selected_google = false;
    let mut callback_seen = false;
    let mut last_selection: Option<Instant> = None;
    let mut selections = 0;
    let mut password_sent = false;
    let mut identifier_sent = false;
    let mut totp_sent = false;
    let mut mail_code: Option<aterm_secrets::SecretBytes> = None;
    let mut mail_code_sent = false;
    let mut seen_mail_codes = BTreeSet::new();
    let started = aterm_time::unix_secs().and_then(|now| i64::try_from(now).ok());
    let mut page_proved = false;
    let mut bot_since: Option<Instant> = None;
    let mut gate = Gate {
        hosts: handoff.hosts,
        completion: handoff.completion,
        code: handoff.code,
        email: handoff.email,
        senders: approver.senders,
        otp: None,
        pressed: 0,
        confirmed: false,
        filled_code: false,
        filled_email: false,
        filled_otp: false,
        bound: Some(&outer),
        sign_in_once: None,
        types_email: approver.types_email,
    };
    loop {
        if Instant::now() >= handoff.deadline {
            return quiet("Google federation reached its deadline");
        }
        if !configuration_current() {
            return quiet("Google identity configuration changed during sign-in");
        }
        if let Some(credentials) = credentials.as_mut()
            && let Err(error) = credentials.check_current()
        {
            return quiet(error);
        }
        if (handoff.finished)() {
            // Read one response before draining final callback events, even when the popup closed.
            if let Err(error) = browser.pages() {
                return quiet(error);
            }
            if let Some((page, inner)) = &google {
                match browser.navigation_commits(page) {
                    Ok(urls) => callback_seen |= urls.iter().any(|url| inner.is_callback(url)),
                    Err(error) => return quiet(error),
                }
            }
            if callback_seen {
                match browser.into_pending_session(home) {
                    Ok(session) => *pending_session = Some(session),
                    Err(error) => return quiet(error),
                }
            }
            return Approval::Approved { page: page_proved };
        }
        if selected_google && google.is_none() && !callback_seen {
            let mut requests = match browser.window_opens(&root) {
                Ok(requests) => requests,
                Err(error) => return quiet(error),
            };
            match browser.navigation_requests(&root) {
                Ok(navigations) => requests.extend(navigations),
                Err(error) => return quiet(error),
            }
            for url in requests {
                if let Some(flow) = InnerFlow::admit(&url, handoff.hosts) {
                    if pending_inner
                        .as_ref()
                        .is_some_and(|pending| pending != &flow)
                    {
                        return Approval::Refused(
                            "Vendor opened conflicting Google sign-in flows".into(),
                        );
                    }
                    pending_inner = Some(flow);
                }
            }
            let pages = match browser.pages() {
                Ok(pages) => pages,
                Err(error) => return quiet(error),
            };
            for info in pages {
                // Only a direct popup of this owned root (or the root itself) is eligible.
                if info.target() != root.target() && info.opener() != Some(root.target()) {
                    continue;
                }
                let flow = bind_target(&mut pending_inner, info.url(), handoff.hosts);
                let Some(flow) = flow else {
                    continue;
                };
                let page = match browser.attach(&info) {
                    Ok(page) => page,
                    Err(error) => return quiet(error),
                };
                google = Some((page, flow));
                break;
            }
        }
        if let Some((page, inner)) = &google {
            match browser.navigation_commits(page) {
                Ok(urls) => callback_seen |= urls.iter().any(|url| inner.is_callback(url)),
                Err(error) => return quiet(error),
            }
            if callback_seen {
                google = None;
                continue;
            }
            let view = match read_page(&mut browser, page) {
                Ok(view) => view,
                Err(_) => {
                    // Keep the owned flow after a popup closes. A response drains its last
                    // committed callback event before native completion decides success.
                    if let Err(error) = browser.pages() {
                        return quiet(error);
                    }
                    match browser.navigation_commits(page) {
                        Ok(urls) => callback_seen |= urls.iter().any(|url| inner.is_callback(url)),
                        Err(error) => return quiet(error),
                    }
                    if callback_seen {
                        google = None;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                }
            };
            if inner.admits_google(&view.url, handoff.hosts) {
                if oauth::bot_check(&view) {
                    return Approval::BotCheck("Google sign-in requires a security check".into());
                }
                let mut act = |password: Option<&str>, totp: Option<&str>| {
                    let otp = if totp_sent {
                        None
                    } else {
                        totp.and_then(|secret| {
                            aterm_time::unix_secs()
                                .and_then(|now| oauth::generate_totp_at(secret, now))
                        })
                    };
                    let script = aterm_secrets::SecretBytes::from(google_action(
                        identity.email(),
                        password,
                        if totp_sent {
                            Authenticator::Submitted
                        } else {
                            otp.as_deref()
                                .map(Authenticator::Ready)
                                .unwrap_or(Authenticator::Unavailable)
                        },
                        identifier_sent,
                        password_sent,
                        identity.verification_email().map(|target| {
                            (
                                target,
                                mail_code
                                    .as_ref()
                                    .and_then(aterm_secrets::SecretBytes::as_str),
                            )
                        }),
                        mail_code_sent,
                    ));
                    browser.evaluate(page, script.as_str().unwrap_or_default(), true)
                };
                let action = match credentials.as_mut() {
                    Some(credentials) => credentials
                        .with_google_page(&view.url, |password, totp| act(Some(password), totp)),
                    None => Ok(act(None, None)),
                };
                let value = match action {
                    Ok(Ok(value)) => value,
                    Ok(Err(error)) => return quiet(error),
                    Err(error) => return quiet(error),
                };
                match value.as_str() {
                    Some("credentials") => {
                        credentials = match Credentials::load(identity, home) {
                            Ok(loaded) => Some(loaded),
                            Err(error) => return quiet(error),
                        };
                    }
                    // Final callback cookies are committed only after native Kimi verification.
                    Some("consent") => {}
                    Some("identifier") => identifier_sent = true,
                    Some("password") => password_sent = true,
                    Some("totp") => totp_sent = true,
                    Some("totp_rejected") => {
                        return quiet("Google rejected the submitted authenticator code");
                    }
                    Some("mailbox") => {
                        match verification_mail(
                            identity,
                            approver,
                            started,
                            handoff.deadline,
                            &mut seen_mail_codes,
                        ) {
                            Ok(code) => mail_code = Some(aterm_secrets::SecretBytes::from(code)),
                            Err(why) => return quiet(why),
                        }
                    }
                    Some("mailbox_unconfigured") => {
                        return quiet("Google verification mailbox is not configured");
                    }
                    Some("mailbox_refused") => {
                        return quiet("Google verification destination does not match its binding");
                    }
                    Some("mailcode") => {
                        mail_code_sent = true;
                        mail_code = None;
                    }
                    Some("mailcode_rejected") => {
                        return quiet("Google rejected the submitted verification code");
                    }
                    Some("challenge") => {
                        return quiet("Google sign-in requires an unsupported verification step");
                    }
                    Some("refused") => {
                        return Approval::Refused(
                            "Google sign-in identity or origin changed".into(),
                        );
                    }
                    _ => {}
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            if inner.is_callback(&view.url) {
                callback_seen = true;
                google = None;
                continue;
            }
            if !view.url.starts_with("about:blank") {
                return Approval::Refused("Google popup left its admitted callback".into());
            }
        }
        let page = match read_page(&mut browser, &root) {
            Ok(page) => page,
            Err(error) => return quiet(error),
        };
        if oauth::bot_check(&page) {
            if bot_since.get_or_insert_with(Instant::now).elapsed() >= oauth::VENDOR_STALL {
                return Approval::BotCheck("Vendor sign-in requires a security check".into());
            }
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }
        bot_since = None;
        let loopback = outer.callback_port.is_some()
            && (page.url.starts_with("http://127.0.0.1:")
                || page.url.starts_with("http://localhost:"));
        if !login_drive::pinned(&page.url, handoff.hosts) && !loopback {
            // A root navigation into Google is admitted on the next iteration only after our selection.
            if selected_google
                && login_drive::https_host(&page.url).as_deref() == Some("accounts.google.com")
            {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            if page.url == "about:blank" {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            return Approval::Refused("Vendor sign-in left its admitted origin".into());
        }
        if let Some(why) = outer.outside(&page.url, handoff.hosts) {
            return Approval::Refused(why);
        }
        page_proved |= oauth::shows_email(&page, &gate);
        let step = oauth::vendor_step(&page, &gate);
        match &step {
            Step::Done => {
                // The device CLI still has to consume its grant. Never infer that from a browser success page.
                if handoff.completion != crate::ai::connector::Completion::Device {
                    return Approval::Approved { page: page_proved };
                }
            }
            Step::Press(_) | Step::FillCode | Step::FillEmail | Step::FillOtp => {
                let value = match step {
                    Step::FillCode => gate.code,
                    Step::FillEmail => gate.email,
                    _ => None,
                };
                let script = format!(
                    "(() => {{if(location.href !== {})return {{ok:false}};return {};}})()",
                    json!(&page.url),
                    oauth::vendor_act_js(&step, value)
                );
                let acted = browser
                    .evaluate(&root, &script, true)
                    .is_ok_and(|result| result.get("ok").and_then(Value::as_bool) == Some(true));
                if acted {
                    oauth::note_acted(&mut gate, &page, &step);
                }
            }
            Step::SignIn(_) | Step::Wait
                if google.is_none()
                    && !callback_seen
                    && last_selection.is_none_or(|at| at.elapsed() >= Duration::from_secs(1)) =>
            {
                if selections >= 3 {
                    return quiet("Vendor did not open its Google sign-in");
                }
                if choose_google(&mut browser, &root, &page.url).unwrap_or(false) {
                    selected_google = true;
                    last_selection = Some(Instant::now());
                    selections += 1;
                }
            }
            Step::Refuse(why) => return Approval::Refused(why.clone()),
            Step::Stop(_) | Step::Mailbox => {
                return quiet("Vendor federation requires an unimplemented verification step");
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn read_page(
    browser: &mut MemoryBrowser,
    page: &BrowserPage,
) -> Result<oauth::Page, super::google_browser::Error> {
    browser
        .evaluate(page, oauth::VENDOR_PAGE_JS, false)
        .map(|value| oauth::page_of(&value))
}

fn verification_mail(
    identity: &Identity,
    approver: &VendorApprover,
    started: Option<i64>,
    deadline: Instant,
    seen: &mut BTreeSet<String>,
) -> Result<String, &'static str> {
    verification_mail_from(
        identity,
        approver.home.as_deref(),
        approver
            .mailbox_source
            .as_ref()
            .or(approver.mailbox.as_ref()),
        started,
        deadline,
        seen,
    )
}

pub(super) fn verification_mail_from(
    identity: &Identity,
    home: Option<&Path>,
    source: Option<&oauth::MailboxConfig>,
    started: Option<i64>,
    deadline: Instant,
    seen: &mut BTreeSet<String>,
) -> Result<String, &'static str> {
    let target = identity
        .verification_email()
        .ok_or("Google verification mailbox is not configured")?;
    let mut config = verification_config_from(identity, home, source)?;
    config.senders = vec!["accounts.google.com".into(), "google.com".into()];
    config.required_account = Some(identity.email().to_owned());
    config.since = Some(started.ok_or("Verification mail clock is unavailable")?);
    config.strict_since = true;
    // The code-only relay has no account-body witness. Use a full-message reader here.
    config.relay_enabled = false;
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err("Google verification mail deadline expired");
    }
    match oauth::poll_mailbox_config_for_credential_with_seen_otps(
        &config,
        Some(target),
        left.min(Duration::from_secs(60)),
        seen,
    )? {
        oauth::MailboxCredential::Otp(code)
            if code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            Ok(code)
        }
        _ => Err("Google verification message contains no admitted numeric code"),
    }
}

#[cfg(test)]
fn verification_config(
    identity: &Identity,
    approver: &VendorApprover,
) -> Result<oauth::MailboxConfig, &'static str> {
    verification_config_from(
        identity,
        approver.home.as_deref(),
        approver
            .mailbox_source
            .as_ref()
            .or(approver.mailbox.as_ref()),
    )
}

fn verification_config_from(
    identity: &Identity,
    home: Option<&Path>,
    source: Option<&oauth::MailboxConfig>,
) -> Result<oauth::MailboxConfig, &'static str> {
    let target = identity
        .verification_email()
        .ok_or("Google verification mailbox is not configured")?;
    let explicitly_routed = source.is_some_and(|config| {
        config.route_for(Some(target)).is_some()
            || super::mail_recovery::accounts(config)
                .iter()
                .any(|account| account.eq_ignore_ascii_case(target))
    });
    // A saved grant for this exact address outranks an unrelated legacy catch-all mailbox,
    // but never overrides an explicit route (including one that is currently unavailable).
    if !explicitly_routed
        && let Some(home) = home
        && aterm_mail::oauth::session::shared(home, target)
            .map_err(|_| "Stored Google verification mail authorization is unavailable")?
            .is_some()
    {
        return Ok(oauth::MailboxConfig {
            user: Some(target.into()),
            imap_url: Some("imaps://imap.gmail.com/INBOX".into()),
            ..Default::default()
        });
    }
    source
        .filter(|config| config.is_configured_for(Some(target)))
        .cloned()
        .ok_or("Google verification mailbox is unavailable")
}

fn select_google(url: &str) -> String {
    format!(
        r#"(() => {{
        if(location.href !== {url}) return false;
        const visible = e => e && !e.disabled && e.getClientRects().length;
        const button = [...document.querySelectorAll('button, [role=button]')].find(e => e.getClientRects().length && /^continue with google$/i.test(e.innerText.trim()));
        if(!button) return false;
        for(const box of document.querySelectorAll('input[type=checkbox]')) {{
            const labels = [...(box.labels || [])];
            const label = labels.map(e=>e.innerText).join(' ');
            if(!box.disabled && (visible(box)||labels.some(visible)) && !box.checked && /agree/i.test(label) && /terms|privacy/i.test(label) && !/captcha|robot|human/i.test(label)) {{box.click();return false;}}
        }}
        if(!visible(button)) return false;
        button.scrollIntoView({{block:'center'}});
        const rect=button.getBoundingClientRect();
        return {{x:rect.x+rect.width/2,y:rect.y+rect.height/2}};
    }})()"#,
        url = json!(url)
    )
}

fn choose_google(
    browser: &mut MemoryBrowser,
    page: &BrowserPage,
    url: &str,
) -> Result<bool, super::google_browser::Error> {
    let value = browser.evaluate(page, &select_google(url), true)?;
    let Some((x, y)) = value
        .get("x")
        .and_then(Value::as_f64)
        .zip(value.get("y").and_then(Value::as_f64))
    else {
        return Ok(false);
    };
    browser.click(page, x, y)?;
    Ok(true)
}

#[derive(Clone, Copy)]
pub(super) enum Authenticator<'a> {
    Unavailable,
    Ready(&'a str),
    Submitted,
}

pub(super) fn google_action(
    email: &str,
    password: Option<&str>,
    authenticator: Authenticator<'_>,
    identifier_sent: bool,
    password_sent: bool,
    mail: Option<(&str, Option<&str>)>,
    mail_sent: bool,
) -> String {
    format!(
        r#"(() => {{
        if(location.origin !== 'https://accounts.google.com') return 'refused';
        const visible=e=>e&&!e.disabled&&e.getClientRects().length;
        if(/\/(?:signup|recovery|changepassword)(?:\/|$)/i.test(location.pathname)||/\/challenge\/(?:sms|ipp)(?:\/|$)/i.test(location.pathname))return 'challenge';
        if([...document.querySelectorAll('input[type=password][autocomplete=new-password]')].some(visible))return 'challenge';
        if([...document.querySelectorAll('iframe[src*=recaptcha],#captchaimg,input[name=ca]')].some(visible)) return 'challenge';
        const email={email};
        const identities=[...document.querySelectorAll('[data-email],[data-identifier],#profileIdentifier')].filter(visible);
        const exact=e=>String(e.getAttribute('data-email')||e.getAttribute('data-identifier')||e.textContent).trim().toLowerCase()===email.toLowerCase();
        const choice=identities.find(e=>e.hasAttribute('data-identifier')&&exact(e));
        if(choice && !document.querySelector('input[type=password]') && /accountchooser|selectaccount/i.test(location.pathname)) {{choice.click();return 'wait';}}
        const fill=(field,value,next)=>{{const button=[...document.querySelectorAll(next)].find(visible);if(!button)return false;Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(field,value);field.dispatchEvent(new Event('input',{{bubbles:true}}));field.dispatchEvent(new Event('change',{{bubbles:true}}));button.click();return true;}};
        const identifier=document.querySelector('#identifierId');
        if(visible(identifier)) return {identifier_sent}?'wait':(fill(identifier,email,'#identifierNext')?'identifier':'wait');
        const field=[...document.querySelectorAll('input[type=password]')].find(visible);
        if(field) {{if(!identities.some(exact)||({password_sent}&&field.getAttribute('aria-invalid')==='true'))return 'refused';if({password_sent})return 'wait';const password={password};if(password===null)return 'credentials';return fill(field,password,'#passwordNext')?'password':'wait';}}
        const totp=document.querySelector('#totpPin,input[name=totpPin]');
        if(visible(totp)) {{if(!identities.some(exact))return 'refused';if({totp_sent})return totp.getAttribute('aria-invalid')==='true'?'totp_rejected':'wait';const otp={otp};return otp?(fill(totp,otp,'#totpNext')?'totp':'wait'):({password}===null?'credentials':'challenge');}}
        const mailField=[...document.querySelectorAll('#idvPin,input[name=idvPin]')].find(visible);
        if(mailField) {{
            if(!identities.some(exact))return 'refused';
            const recipient={recipient};
            if(!recipient)return 'mailbox_unconfigured';
            const prompt=(mailField.closest('form')||document.body).innerText;
            const hints=prompt.match(/[a-zA-Z0-9.*•_%+\-]+@[a-zA-Z0-9.*•\-]+/g)||[];
            const compatible=hint=>{{
                hint=hint.replace(/\.+$/,'');
                if(!/[a-z0-9]/i.test(hint.split('@')[0]))return false;
                const pattern=hint.toLowerCase().replace(/[.+?^${{}}()|[\]\\]/g,'\\$&').replace(/[*•]+/g,'.*');
                return new RegExp('^'+pattern+'$').test(recipient.toLowerCase());
            }};
            if(!/email|e-mail/i.test(prompt)||!hints.some(compatible))return 'mailbox_refused';
            if({mail_sent})return mailField.getAttribute('aria-invalid')==='true'?'mailcode_rejected':'wait';
            const code={mail_code};
            if(!code)return 'mailbox';
            return fill(mailField,code,'#idvPreregisteredEmailNext,#idvPinNext')?'mailcode':'wait';
        }}
        if(/\/challenge\//.test(location.pathname))return 'challenge';
        if(/\/oauth\//.test(location.pathname) && identities.some(exact)) {{
            const approve=document.querySelector('#submit_approve_access');
            if(visible(approve)){{if(approve.dataset.atermGoogleConsentSent==='true')return 'wait';approve.dataset.atermGoogleConsentSent='true';approve.click();return 'consent';}}
        }}
        return 'wait';
    }})()"#,
        email = json!(email),
        password = json!(password),
        otp = json!(match authenticator {
            Authenticator::Ready(code) => Some(code),
            _ => None,
        }),
        totp_sent = matches!(authenticator, Authenticator::Submitted),
        recipient = json!(mail.map(|(target, _)| target)),
        mail_code = json!(mail.and_then(|(_, code)| code))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOGLE: &str = "https://accounts.google.com/o/oauth2/v2/auth?client_id=vendor-client&redirect_uri=https%3A%2F%2Fwww.kimi.ai%2Fgoogle-callback&scope=email%20profile&state=inner-state";
    const HOSTS: &[&str] = &["auth.kimi.ai", "www.kimi.ai"];

    #[test]
    fn google_federation_uses_one_harness_identity_root() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let store = super::super::accounts::Store::at_for(root.path(), Cli::Kimi, &home);
        let mut approver = VendorApprover::with_mailbox(
            Cli::Kimi,
            "one",
            &store.shared_dir(),
            &home,
            oauth::MailboxConfig::default(),
        );
        assert_eq!(approver.shared_dir, root.path().join("accounts"));
        assert_eq!(approver.google_state, root.path());
        let row = super::super::accounts::Row {
            label: "one".into(),
            ..Default::default()
        };
        let route = Cli::Kimi
            .accounts()
            .signin
            .as_ref()
            .unwrap()
            .route(None)
            .unwrap();
        let request = login_drive::Request {
            recovery: None,
            admission: None,
            cli: Cli::Kimi,
            row: &row,
            staging: &home,
            store: &store,
            route,
            hosts: HOSTS,
        };
        login_drive::Approver::begin(&mut approver, &request).unwrap();
        assert_eq!(approver.google_state, root.path());
        assert_eq!(
            approver.native_attempt.as_ref().unwrap().state(),
            root.path()
        );
    }

    #[test]
    fn kimi_cookie_completion_requires_current_native_attempt_and_provider_proof() {
        use super::super::cli::shared_provider::kimi_proof_fixture;
        use super::super::{accounts, google_browser, google_browser_store};
        const EMAIL: &str = "person@example.com";
        const OLD: &[u8] = br#"[{"name":"fixture","value":"old-cookie","domain":".google.com","path":"/","secure":true}]"#;
        const NEW: &[u8] = br#"[{"name":"fixture","value":"new-cookie","domain":".google.com","path":"/","secure":true}]"#;
        for scenario in 0..12 {
            let root = aterm_tempfile::tempdir().unwrap();
            let state = root.path().join("state");
            let home = root.path().join("home");
            let staging = root.path().join("staging");
            for path in [&state, &home, &staging] {
                aterm_secrets::fsx::ensure_private_dir(path).unwrap();
            }
            let text = "[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='kimi-global'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n";
            aterm_secrets::fsx::write_atomic(&state.join(REGISTRY_FILE), text.as_bytes(), 0o600)
                .unwrap();
            let registry = Registry::parse(text).unwrap();
            let identity = registry.resolve("kimi-global", "one").unwrap().0.clone();
            let store = accounts::Store::at_for(&state, Cli::Kimi, &home);
            let row = accounts::Row {
                label: "one".into(),
                email: Some(EMAIL.into()),
                vendor: accounts::Vendor {
                    region: Some(Region::Global),
                    ..Default::default()
                },
                ..Default::default()
            };
            let route = Cli::Kimi
                .accounts()
                .signin
                .as_ref()
                .unwrap()
                .route(Some(crate::ai::connector::Flow::Device))
                .unwrap();
            let request = login_drive::Request {
                recovery: Some(login_drive::RecoveryKey([1; 32])),
                admission: None,
                cli: Cli::Kimi,
                row: &row,
                staging: &staging,
                store: &store,
                route,
                hosts: HOSTS,
            };
            let scope = FederationScope {
                attempt: NativeAttempt::capture(&request),
                provider: "kimi-global".into(),
                identity: identity.clone(),
                expected: EMAIL.into(),
                revision: registry.binding_revision("kimi-global", "one").unwrap(),
                region: Region::Global,
            };
            let claim = identity.claim(&state).unwrap();
            let snapshot = claim.profile().join("session.age");
            google_browser_store::save(&home, &identity, &claim, OLD).unwrap();
            let previous = std::fs::read(&snapshot).unwrap();
            drop(claim);
            let candidate_identity = if scenario == 8 {
                Registry::parse("[[identities]]\nid='other'\nemail='person@example.com'\n[[bindings]]\nprovider='kimi-global'\naccount='one'\nidentity='other'\nexpected_email='person@example.com'\n").unwrap().resolve("kimi-global", "one").unwrap().0.clone()
            } else {
                identity.clone()
            };
            let pending = PendingFederation {
                scope,
                native: None,
                session: Some(google_browser::pending_fixture(
                    &candidate_identity,
                    &state,
                    &home,
                    NEW,
                )),
            };
            let proof_staging = if scenario == 10 {
                root.path().join("another-native-home")
            } else {
                staging.clone()
            };
            let proof = kimi_proof_fixture(
                &proof_staging,
                if scenario == 9 {
                    Region::MainlandCn
                } else {
                    Region::Global
                },
                if scenario == 7 {
                    "another@example.com"
                } else {
                    EMAIL
                },
            );
            let changed = accounts::Row {
                label: "another".into(),
                ..row.clone()
            };
            let request = login_drive::Request {
                recovery: if scenario == 11 {
                    Some(login_drive::RecoveryKey([11; 32]))
                } else {
                    request.recovery
                },
                row: if scenario == 2 { &changed } else { &row },
                ..request
            };
            let outcome = if scenario == 1 {
                login_drive::Outcome::Failed
            } else {
                login_drive::Outcome::SignedIn {
                    email: Some(EMAIL.into()),
                    page: false,
                }
            };
            let deadline = if scenario == 3 {
                Instant::now()
            } else {
                Instant::now() + Duration::from_secs(60)
            };
            let result = pending.complete_using(&request, &outcome, deadline, |_| {
                assert!(
                    !matches!(scenario, 1..=3 | 11),
                    "failed, changed or expired attempt contacted Kimi"
                );
                assert_eq!(std::fs::read(&snapshot).unwrap(), previous);
                assert!(
                    candidate_identity.claim(&state).is_err(),
                    "pending identity was released before provider proof"
                );
                if scenario == 4 {
                    return Err("provider unavailable".into());
                }
                if scenario == 5 {
                    aterm_secrets::fsx::write_atomic(&state.join(REGISTRY_FILE), b"changed", 0o600)
                        .unwrap();
                }
                if scenario == 6 {
                    aterm_secrets::fsx::write_atomic(
                        &staging.join("config.toml"),
                        b"changed",
                        0o600,
                    )
                    .unwrap();
                }
                Ok(proof)
            });
            if scenario == 0 {
                result.unwrap();
                assert_ne!(std::fs::read(&snapshot).unwrap(), previous);
                let claim = identity.claim(&state).unwrap();
                assert_eq!(
                    google_browser_store::load(&home, &identity, &claim)
                        .unwrap()
                        .unwrap()
                        .as_slice(),
                    NEW
                );
            } else {
                if scenario == 1 {
                    result.unwrap();
                } else {
                    assert!(result.is_err());
                }
                assert_eq!(std::fs::read(&snapshot).unwrap(), previous);
                assert!(candidate_identity.claim(&state).is_ok());
                if scenario == 8 {
                    let claim = candidate_identity.claim(&state).unwrap();
                    assert!(!claim.profile().join("session.age").exists());
                }
            }
        }
    }

    #[test]
    fn exact_saved_gmail_grant_supplies_verification_mail_without_a_config_file() {
        let home = aterm_tempfile::tempdir().unwrap();
        let identity = |target: &str| {
            Registry::parse(&format!("[[identities]]\nid='google'\nemail='person@example.com'\nverification_email='{target}'\n[[bindings]]\nprovider='kimi-global'\naccount='one'\nidentity='google'\nexpected_email='person@example.com'\n")).unwrap().resolve("kimi-global","one").unwrap().0.clone()
        };
        let wanted = identity("rescue@gmail.com");
        aterm_mail::oauth::session::register(
            home.path(),
            "rescue@gmail.com",
            "fixture-client",
            None,
            aterm_mail::Secret::new("fixture-refresh".into()),
        )
        .unwrap();
        let mut approver = VendorApprover::with_mailbox(
            super::super::accounts::Cli::Kimi,
            "one",
            home.path(),
            home.path(),
            oauth::MailboxConfig::default(),
        );
        approver.mailbox = None;
        approver.mailbox_source = None;
        let native = verification_config(&wanted, &approver).unwrap();
        assert_eq!(native.user.as_deref(), Some("rescue@gmail.com"));
        assert!(native.has_native_google_route(Some("rescue@gmail.com")));
        assert!(native.password.is_none());
        assert!(verification_config(&identity("missing@gmail.com"), &approver).is_err());
        let explicit=oauth::MailboxConfig::from_toml_str("[[mailboxes]]\nlogin='owner@proton.me'\npassword='fixture-only'\nwebmail_url='https://mail.proton.me/u/0/inbox'\nreceives=['rescue@gmail.com']\n",None).unwrap();
        approver.mailbox_source = Some(explicit);
        assert_eq!(
            verification_config(&wanted, &approver)
                .unwrap()
                .route_for(Some("rescue@gmail.com"))
                .unwrap()
                .login,
            "owner@proton.me"
        );
        approver.mailbox_source=Some(oauth::MailboxConfig::from_toml_str("[[mailboxes]]\nlogin='owner@proton.me'\nwebmail_url='https://mail.proton.me/u/0/inbox'\nreceives=['rescue@gmail.com']\n",None).unwrap());
        assert!(
            verification_config(&wanted, &approver).is_err(),
            "an unavailable explicit route cannot silently change providers"
        );
    }

    #[test]
    fn inner_google_flow_is_distinct_and_refuses_other_clients_callbacks_and_scopes() {
        let flow = InnerFlow::admit(GOOGLE, HOSTS).unwrap();
        assert!(flow.admits_google("https://accounts.google.com/v3/signin/identifier", HOSTS));
        assert!(
            flow.is_callback("https://www.kimi.ai/google-callback?code=value&state=inner-state")
        );
        for url in [
            GOOGLE.replace("vendor-client", "other-client"),
            GOOGLE.replace("inner-state", "outer-state"),
            GOOGLE.replace(
                "scope=email%20profile",
                "scope=email%20profile%20https%3A%2F%2Fmail.google.com%2F",
            ),
            GOOGLE.replace("www.kimi.ai", "www.kimi.com"),
            GOOGLE.replace("accounts.google.com", "accounts.google.com.evil.test"),
        ] {
            assert!(!flow.admits_google(&url, HOSTS));
        }
        // Kimi's observed Google request omits state. Retain it when supplied;
        // an absent state still requires this owned flow and the native account proof.
        let stateless = InnerFlow::admit(&GOOGLE.replace("&state=inner-state", ""), HOSTS).unwrap();
        assert!(stateless.is_callback("https://www.kimi.ai/google-callback?code=value"));
        for url in [
            "https://www.kimi.ai/google-callback?state=inner-state",
            "https://www.kimi.ai/google-callback?code=&state=inner-state",
            "https://www.kimi.ai/google-callback?code=a&code=b&state=inner-state",
            "https://www.kimi.ai/google-callback?code=value&state=outer-state",
            "https://www.kimi.ai/google-callback?code=value",
            "https://www.kimi.ai/google-callback?error=denied&state=inner-state",
            "https://www.kimi.ai/elsewhere?code=value&state=inner-state",
        ] {
            assert!(!flow.is_callback(url));
        }
        for url in [
            GOOGLE.replace(
                "redirect_uri=",
                "redirect_uri=https%3A%2F%2Fevil.test%2F&unused=",
            ),
            GOOGLE.replace(
                "scope=email%20profile",
                "scope=https%3A%2F%2Fmail.google.com%2F",
            ),
            format!("{GOOGLE}&client_id=other-client"),
        ] {
            assert!(InnerFlow::admit(&url, HOSTS).is_none());
        }
        // The observed Google SDK flow can omit state. Its context and opener remain mandatory.
        assert!(InnerFlow::admit(&GOOGLE.replace("&state=inner-state", ""), HOSTS).is_some());
        let mut pending = InnerFlow::admit(GOOGLE, HOSTS);
        assert!(
            bind_target(
                &mut pending,
                &GOOGLE.replace("vendor-client", "swapped-client"),
                HOSTS
            )
            .is_none()
        );
        assert!(
            pending.is_some(),
            "a mismatched target must not consume the observed binding"
        );
        assert!(
            bind_target(
                &mut pending,
                "https://accounts.google.com/v3/signin/identifier",
                HOSTS
            )
            .is_some()
        );
        assert!(pending.is_none());
    }

    #[test]
    fn configured_binding_is_exact_and_an_expired_handoff_does_not_launch_a_browser() {
        use crate::ai::connector::Completion;
        let root = aterm_tempfile::tempdir().unwrap();
        let mut approver = VendorApprover::with_mailbox(
            super::super::accounts::Cli::Kimi,
            "work",
            root.path(),
            root.path(),
            oauth::MailboxConfig::default(),
        );
        let handoff = Handoff {
            url: "https://www.kimi.ai/login?user_code=ABCD",
            code: Some("ABCD"),
            completion: Completion::Device,
            hosts: HOSTS,
            email: Some("person@example.com"),
            finished: &|| false,
            deadline: Instant::now(),
        };
        assert!(configured(&mut approver, &handoff).is_none());
        let text = "[[identities]]\nid='one'\nemail='person@example.com'\ncredential_ref='missing-google-entry'\n[[bindings]]\nprovider='kimi-global'\naccount='work'\nidentity='one'\nexpected_email='person@example.com'\n";
        aterm_uds::private_file::write_private(root.path(), REGISTRY_FILE, text.as_bytes())
            .unwrap();
        assert!(matches!(
            login_drive::Approver::approve(&mut approver, &handoff),
            Approval::NotRun(_)
        ));
        assert!(
            !root.path().join("google-identities").exists(),
            "an expired handoff must not start a browser"
        );
        let wrong = Handoff {
            email: Some("other@example.com"),
            ..handoff
        };
        assert!(matches!(
            configured(&mut approver, &wrong),
            Some(Approval::Refused(_))
        ));
        approver.google_account.1 = "other".into();
        assert!(configured(&mut approver, &handoff).is_none());
        assert_eq!(
            provider_key("kimi", &["www.kimi.com"]),
            Some("kimi-mainland-cn")
        );
        assert_eq!(provider_key("kimi", &["www.kimi.com", "www.kimi.ai"]), None);
    }

    #[test]
    #[ignore = "runs script behavior in the pinned full headless browser"]
    fn browser_actions_enforce_origin_identity_single_submission_and_leave_challenges_alone() {
        let root = aterm_tempfile::tempdir().unwrap();
        let registry = Registry::parse("[[identities]]\nid='fixture'\nemail='person@example.com'\n[[bindings]]\nprovider='fixture'\naccount='one'\nidentity='fixture'\nexpected_email='person@example.com'\n").unwrap();
        let identity = registry.resolve("fixture", "one").unwrap().0;
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let mut browser = MemoryBrowser::start(
            identity,
            root.path(),
            home.as_deref(),
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
        let page = browser.open("about:blank").unwrap();
        for (field, path) in [
            (
                "<input type=password><button id=passwordNext>Next</button>",
                "/v3/signin/challenge/pwd",
            ),
            (
                "<input id=totpPin><button id=totpNext>Next</button>",
                "/v3/signin/challenge/totp",
            ),
        ] {
            browser.evaluate(&page, &format!("document.body.innerHTML='<span id=profileIdentifier>person@example.com</span>{field}';window.actions=0;document.querySelector('button').onclick=()=>window.actions++;true"), false).unwrap();
            let action = google_action(
                "person@example.com",
                None,
                Authenticator::Unavailable,
                true,
                false,
                None,
                false,
            );
            let script = format!(
                "((location)=>{action})({{origin:'https://accounts.google.com',pathname:{}}})",
                json!(path)
            );
            assert_eq!(
                browser.evaluate(&page, &script, true).unwrap(),
                json!("credentials")
            );
            assert_eq!(
                browser.evaluate(&page, "window.actions", false).unwrap(),
                json!(0)
            );
            assert_eq!(
                browser
                    .evaluate(&page, "document.querySelector('input').value", false)
                    .unwrap(),
                json!("")
            );
        }
        browser.evaluate(&page,"document.body.innerHTML='<span id=profileIdentifier>person@example.com</span><input id=totpPin><button type=button id=totpNext>Next</button>';window.actions=0;document.querySelector('button').onclick=()=>window.actions++;true",false).unwrap();
        let submitted = google_action(
            "person@example.com",
            Some("fixture-only"),
            Authenticator::Submitted,
            true,
            true,
            None,
            false,
        );
        let submitted = format!(
            "((location)=>{submitted})({{origin:'https://accounts.google.com',pathname:'/v3/signin/challenge/totp'}})"
        );
        assert_eq!(
            browser.evaluate(&page, &submitted, true).unwrap(),
            json!("wait")
        );
        browser
            .evaluate(
                &page,
                "document.querySelector('#totpPin').setAttribute('aria-invalid','true');true",
                false,
            )
            .unwrap();
        assert_eq!(
            browser.evaluate(&page, &submitted, true).unwrap(),
            json!("totp_rejected")
        );
        assert_eq!(
            browser.evaluate(&page, "window.actions", false).unwrap(),
            json!(0)
        );
        for (origin, account, expected, actions) in [
            (
                "https://accounts.google.com",
                "person@example.com",
                "consent",
                1,
            ),
            (
                "https://accounts.google.com",
                "other@example.com",
                "wait",
                0,
            ),
            (
                "https://accounts.google.com.evil.test",
                "person@example.com",
                "refused",
                0,
            ),
        ] {
            browser.evaluate(&page, &format!("document.body.innerHTML='<span id=profileIdentifier>{account}</span><button id=submit_approve_access>Continue</button>';window.actions=0;document.querySelector('button').onclick=()=>window.actions++;true"), false).unwrap();
            let action = google_action(
                "person@example.com",
                None,
                Authenticator::Unavailable,
                true,
                true,
                None,
                false,
            );
            let script = format!(
                "((location)=>{action})({{origin:{},pathname:'/signin/oauth/consent'}})",
                json!(origin)
            );
            assert_eq!(
                browser.evaluate(&page, &script, true).unwrap(),
                json!(expected)
            );
            if expected == "consent" {
                assert_eq!(
                    browser.evaluate(&page, &script, true).unwrap(),
                    json!("wait")
                );
            }
            assert_eq!(
                browser.evaluate(&page, "window.actions", false).unwrap(),
                json!(actions)
            );
        }
        for (origin, identity, sent, state, actions) in [
            (
                "https://accounts.google.com",
                "person@example.com",
                false,
                "password",
                1,
            ),
            (
                "https://accounts.google.com",
                "person@example.com",
                true,
                "wait",
                0,
            ),
            (
                "https://accounts.google.com",
                "other@example.com",
                false,
                "refused",
                0,
            ),
            (
                "https://accounts.google.com.evil.test",
                "person@example.com",
                false,
                "refused",
                0,
            ),
        ] {
            let fixture = format!(
                "document.body.innerHTML='<span id=profileIdentifier>{identity}</span><input type=password><button id=passwordNext>Next</button>';window.actions=0;document.querySelector('button').onclick=()=>window.actions++;true"
            );
            browser.evaluate(&page, &fixture, false).unwrap();
            let script = google_action(
                "person@example.com",
                Some("fake-fixture-only"),
                Authenticator::Unavailable,
                false,
                sent,
                None,
                false,
            );
            let scoped = format!(
                "((location)=>{script})({{origin:{},pathname:'/v3/signin/challenge/pwd'}})",
                json!(origin)
            );
            assert_eq!(
                browser.evaluate(&page, &scoped, true).unwrap(),
                json!(state)
            );
            assert_eq!(
                browser.evaluate(&page, "window.actions", false).unwrap(),
                json!(actions)
            );
        }
        for (account, hint, code, submitted, expected, actions) in [
            (
                "person@example.com",
                "r***@example.com",
                None,
                false,
                "mailbox",
                0,
            ),
            (
                "person@example.com",
                "r***@example.com.",
                Some("246810"),
                false,
                "mailcode",
                1,
            ),
            (
                "person@example.com",
                "r***@example.com",
                Some("246810"),
                true,
                "wait",
                0,
            ),
            (
                "other@example.com",
                "r***@example.com",
                Some("246810"),
                false,
                "refused",
                0,
            ),
            (
                "person@example.com",
                "other@example.com",
                Some("246810"),
                false,
                "mailbox_refused",
                0,
            ),
        ] {
            let fixture = format!(
                "document.body.innerHTML='<span id=profileIdentifier>{account}</span><form><p>A verification code was sent by email to {hint}</p><input id=idvPin><button type=button id=idvPreregisteredEmailNext>Next</button></form>';window.actions=0;document.querySelector('button').onclick=()=>window.actions++;true"
            );
            browser.evaluate(&page, &fixture, false).unwrap();
            let script = google_action(
                "person@example.com",
                Some("fake-fixture-only"),
                Authenticator::Unavailable,
                true,
                true,
                Some(("rescue@example.com", code)),
                submitted,
            );
            let scoped = format!(
                "((location)=>{script})({{origin:'https://accounts.google.com',pathname:'/v3/signin/challenge/ipe'}})"
            );
            assert_eq!(
                browser.evaluate(&page, &scoped, true).unwrap(),
                json!(expected)
            );
            assert_eq!(
                browser.evaluate(&page, "window.actions", false).unwrap(),
                json!(actions)
            );
        }
        browser.evaluate(&page,"document.body.innerHTML='<span id=profileIdentifier>person@example.com</span><form><p>Email sent to rescue@example.com</p><input id=idvPin aria-invalid=true><button type=button id=idvPreregisteredEmailNext>Next</button></form>';window.actions=0;document.querySelector('button').onclick=()=>window.actions++;true",false).unwrap();
        let rejected = google_action(
            "person@example.com",
            Some("fixture-only"),
            Authenticator::Unavailable,
            true,
            true,
            Some(("rescue@example.com", None)),
            true,
        );
        let rejected = format!(
            "((location)=>{rejected})({{origin:'https://accounts.google.com',pathname:'/v3/signin/challenge/ipe'}})"
        );
        assert_eq!(
            browser.evaluate(&page, &rejected, true).unwrap(),
            json!("mailcode_rejected")
        );
        assert_eq!(
            browser.evaluate(&page, "window.actions", false).unwrap(),
            json!(0)
        );
        for path in [
            "/signin/recovery",
            "/signup",
            "/v3/signin/challenge/sms",
            "/v3/signin/challenge/ipp",
        ] {
            let script = google_action(
                "person@example.com",
                Some("fake-fixture-only"),
                Authenticator::Unavailable,
                true,
                true,
                Some(("rescue@example.com", Some("246810"))),
                false,
            );
            let scoped = format!(
                "((location)=>{script})({{origin:'https://accounts.google.com',pathname:{}}})",
                json!(path)
            );
            assert_eq!(
                browser.evaluate(&page, &scoped, true).unwrap(),
                json!("challenge")
            );
            assert_eq!(
                browser.evaluate(&page, "window.actions", false).unwrap(),
                json!(0)
            );
        }
        browser.evaluate(&page,"document.body.innerHTML='<input type=password autocomplete=new-password><button id=passwordNext>Next</button>';window.actions=0;document.querySelector('button').onclick=()=>window.actions++;true",false).unwrap();
        let script = google_action(
            "person@example.com",
            Some("fake-fixture-only"),
            Authenticator::Unavailable,
            false,
            false,
            None,
            false,
        );
        let scoped = format!(
            "((location)=>{script})({{origin:'https://accounts.google.com',pathname:'/signin'}})"
        );
        assert_eq!(
            browser.evaluate(&page, &scoped, true).unwrap(),
            json!("challenge")
        );
        assert_eq!(
            browser.evaluate(&page, "window.actions", false).unwrap(),
            json!(0)
        );
        browser.evaluate(&page, "document.body.innerHTML='<label><input type=checkbox id=terms>I agree to the terms and privacy policy</label><label><input type=checkbox id=captcha>I am human</label><button>Continue with Google</button>';window.actions=0;document.querySelector('button').onclick=()=>window.actions++;true", false).unwrap();
        assert!(!choose_google(&mut browser, &page, "about:blank").unwrap());
        assert!(choose_google(&mut browser, &page, "about:blank").unwrap());
        assert_eq!(browser.evaluate(&page, "({terms:document.querySelector('#terms').checked,captcha:document.querySelector('#captcha').checked,actions:window.actions})", false).unwrap(), json!({"terms":true,"captcha":false,"actions":1}));
    }

    #[test]
    #[ignore = "contacts Kimi anonymously through the pinned headless browser"]
    fn live_kimi_google_transition_preserves_context_and_vendor_callback() {
        let root = aterm_tempfile::tempdir().unwrap();
        let registry = Registry::parse("[[identities]]\nid='anonymous-probe'\nemail='probe@example.invalid'\n[[bindings]]\nprovider='kimi-global'\naccount='probe'\nidentity='anonymous-probe'\nexpected_email='probe@example.invalid'\n").unwrap();
        let identity = registry.resolve("kimi-global", "probe").unwrap().0;
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut browser =
            MemoryBrowser::start(identity, root.path(), home.as_deref(), deadline).unwrap();
        let page = browser.open("https://www.kimi.ai/login").unwrap();
        let mut last_select = Instant::now() - Duration::from_secs(2);
        let mut clicks = 0;
        let mut pending: Option<InnerFlow> = None;
        let mut observed = Vec::new();
        loop {
            assert!(
                Instant::now() < deadline,
                "anonymous Google transition did not complete after {clicks} pointer clicks; pages={observed:?}"
            );
            for url in browser.window_opens(&page).unwrap() {
                if login_drive::https_host(&url).as_deref() == Some("accounts.google.com") {
                    println!(
                        "anonymous Kimi Google request: state_present={} callback_present={}",
                        !oauth::query_values(&url, "state").is_empty(),
                        oauth::query_values(&url, "redirect_uri").len() == 1
                    );
                }
                if let Some(flow) = InnerFlow::admit(&url, HOSTS) {
                    pending = Some(flow);
                }
            }
            observed.clear();
            for info in browser.pages().unwrap() {
                observed.push((
                    login_drive::https_host(info.url()),
                    info.opener() == Some(page.target()),
                ));
                if info.target() != page.target() && info.opener() != Some(page.target()) {
                    continue;
                }
                let inner = bind_target(&mut pending, info.url(), HOSTS);
                if let Some(inner) = inner {
                    assert_eq!(inner.callback, "https://www.kimi.ai/google-callback");
                    return;
                }
            }
            if last_select.elapsed() >= Duration::from_secs(2) {
                let view = read_page(&mut browser, &page).unwrap();
                assert!(
                    !oauth::bot_check(&view),
                    "anonymous vendor page requires a security check"
                );
                if login_drive::pinned(&view.url, HOSTS)
                    && choose_google(&mut browser, &page, &view.url).unwrap()
                {
                    clicks += 1;
                }
                last_select = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    #[cfg(target_os = "macos")]
    struct NativeClaudeProbe(aterm_uds::spawnfd::Child);

    #[cfg(target_os = "macos")]
    impl Drop for NativeClaudeProbe {
        fn drop(&mut self) {
            if let Ok(pid) = i32::try_from(self.0.id()) {
                let _ = aterm_uds::process::kill_group_until_empty(pid);
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[cfg(target_os = "macos")]
    fn native_claude_probe(
        program: &std::path::Path,
        home: &std::path::Path,
        deadline: Instant,
    ) -> (NativeClaudeProbe, aterm_secrets::SecretBytes) {
        use std::io::Read;
        use std::os::fd::AsFd;
        aterm_secrets::fsx::ensure_private_dir(home).unwrap();
        let (mut reader, writer) = std::io::pipe().unwrap();
        let mut command = aterm_uds::spawnfd::Command::new(login_drive::SANDBOX_EXEC);
        command
            .args(["-p", login_drive::NO_DESK])
            .arg(program)
            .args([
                "auth",
                "login",
                "--claudeai",
                "--email",
                "probe@example.invalid",
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("CLAUDE_CONFIG_DIR", home.join("config"))
            .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", home.join("secure"))
            .env("DISABLE_AUTOUPDATER", "1")
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .env("BROWSER", "/usr/bin/false")
            .env("TERM", "dumb")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(writer.try_clone().unwrap())
            .stderr(writer);
        let child = NativeClaudeProbe(command.spawn_in_dedicated_process_group().unwrap());
        drop(command);
        let mut output = aterm_secrets::SecretBytes::from(Vec::new());
        loop {
            aterm_uds::ready::until(reader.as_fd(), aterm_uds::ready::Interest::Read, deadline)
                .expect("native Claude printed no authorization request");
            let mut chunk = [0; 4096];
            // A ready pipe read has one reader; the child cannot consume its bytes.
            let count = reader.read(&mut chunk).unwrap();
            assert!(
                count != 0,
                "native Claude closed before an authorization request"
            );
            let mut bytes = output.as_slice().to_vec();
            bytes.extend_from_slice(&chunk[..count]);
            assert!(
                bytes.len() <= 64 * 1024,
                "native Claude exceeded the probe output bound"
            );
            output = aterm_secrets::SecretBytes::from(bytes);
            let text = String::from_utf8_lossy(output.as_slice());
            let super::super::open_url::Pin::Endpoints(endpoints) =
                super::super::open_url::CLAUDE_PIN
            else {
                unreachable!()
            };
            for (endpoint, _) in endpoints {
                if let Some(start) = text.find(&format!("{endpoint}?")) {
                    let tail = &text[start..];
                    if let Some(end) = tail.find(|c: char| c.is_whitespace() || c.is_control()) {
                        let url = &tail[..end];
                        assert!(
                            FlowBound::of(url).is_ok(),
                            "native Claude request has no admitted outer binding"
                        );
                        println!(
                            "native Claude request captured on host={:?}",
                            login_drive::https_host(url)
                        );
                        return (child, aterm_secrets::SecretBytes::from(url.to_owned()));
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "contacts Claude anonymously through the pinned headless browser"]
    fn live_claude_google_transition_observes_vendor_callback() {
        let root = aterm_tempfile::tempdir().unwrap();
        let registry = Registry::parse("[[identities]]\nid='anonymous-probe'\nemail='probe@example.invalid'\n[[bindings]]\nprovider='claude'\naccount='probe'\nidentity='anonymous-probe'\nexpected_email='probe@example.invalid'\n").unwrap();
        let identity = registry.resolve("claude", "probe").unwrap().0;
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let deadline = Instant::now() + Duration::from_secs(60);
        #[cfg(target_os = "macos")]
        let native = std::env::var_os("ATERM_TEST_CLAUDE").map(|program| {
            native_claude_probe(
                std::path::Path::new(&program),
                &root.path().join("native-home"),
                deadline,
            )
        });
        #[cfg(target_os = "macos")]
        let start = native
            .as_ref()
            .map(|(_, url)| url.as_str().unwrap())
            .unwrap_or("https://claude.ai/login");
        #[cfg(not(target_os = "macos"))]
        let start = "https://claude.ai/login";
        let mut browser =
            MemoryBrowser::start(identity, root.path(), home.as_deref(), deadline).unwrap();
        let page = browser.open(start).unwrap();
        let mut last_select = Instant::now() - Duration::from_secs(2);
        let mut clicks = 0;
        let mut observed = Vec::new();
        let mut challenge_seen = false;
        loop {
            assert!(
                Instant::now() < deadline,
                "anonymous Claude transition did not complete after {clicks} clicks; challenge_seen={challenge_seen}; hosts={observed:?}"
            );
            let mut urls = browser.window_opens(&page).unwrap();
            observed.clear();
            for info in browser.pages().unwrap() {
                observed.push(login_drive::https_host(info.url()));
                if info.target() == page.target() || info.opener() == Some(page.target()) {
                    urls.push(info.url().to_owned());
                }
            }
            for url in urls {
                if login_drive::https_host(&url).as_deref() != Some("accounts.google.com") {
                    continue;
                }
                let callback = oauth::query_values(&url, "redirect_uri");
                if callback.len() == 1 {
                    let callback = callback[0].split(['?', '#']).next().unwrap();
                    println!(
                        "anonymous Claude Google callback={callback} client_present={} state_present={}",
                        oauth::query_values(&url, "client_id").len() == 1,
                        oauth::query_values(&url, "state").len() == 1
                    );
                    assert!(callback.starts_with("https://"));
                    return;
                }
            }
            if last_select.elapsed() >= Duration::from_secs(2) {
                let view = read_page(&mut browser, &page).unwrap();
                if oauth::bot_check(&view) {
                    challenge_seen = true;
                    // Ordinary page loading may clear a challenge by itself. Touch
                    // nothing while it is present; never press a verification widget.
                    std::thread::sleep(Duration::from_millis(200));
                    continue;
                }
                if super::super::open_url::CLAUDE_PIN.admits(&view.url)
                    && choose_google(&mut browser, &page, &view.url).unwrap()
                {
                    clicks += 1;
                }
                last_select = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    #[test]
    fn verification_in_shared_inbox_selects_the_google_account_without_consuming_others() {
        let root = aterm_tempfile::tempdir().unwrap();
        let maildir = root.path().join("mail");
        std::fs::create_dir(&maildir).unwrap();
        let cfg = oauth::MailboxConfig {
            dir: Some(maildir.clone()),
            ..Default::default()
        };
        let approver = VendorApprover::with_mailbox(
            super::super::accounts::Cli::Kimi,
            "fixture",
            root.path(),
            root.path(),
            cfg,
        );
        let identity = |email: &str| {
            let source = format!(
                "[[identities]]\nid='one'\nemail='{email}'\nverification_email='rescue@example.com'\n[[bindings]]\nprovider='kimi-global'\naccount='one'\nidentity='one'\nexpected_email='{email}'\n"
            );
            Registry::parse(&source)
                .unwrap()
                .resolve("kimi-global", "one")
                .unwrap()
                .0
                .clone()
        };
        for (file, account, code, sender) in [
            (
                "a",
                "other@example.com",
                "111111",
                "noreply@accounts.google.com",
            ),
            (
                "b",
                "person@example.com",
                "222222",
                "noreply@accounts.google.com",
            ),
            (
                "c",
                "person@example.com",
                "333333",
                "noreply@accounts.google.com.evil.test",
            ),
        ] {
            let encoded = aterm_codec::base64::encode(
                format!("Verification code: {code}\nGoogle account: {account}\n").as_bytes(),
            )
            .unwrap();
            let raw = format!(
                "From: {sender}\r\nTo: rescue@example.com\r\nDate: Thu, 01 Jan 1970 00:16:40 +0000\r\nContent-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\n{encoded}\r\n"
            );
            std::fs::write(maildir.join(file), raw).unwrap();
        }
        std::fs::write(maildir.join("stale"),"From: noreply@accounts.google.com\r\nTo: rescue@example.com\r\nDate: Thu, 01 Jan 1970 00:16:10 +0000\r\n\r\nGoogle account person@example.com\nVerification code: 444444\n").unwrap();
        let mut seen = BTreeSet::new();
        assert_eq!(
            verification_mail(
                &identity("person@example.com"),
                &approver,
                Some(1000),
                Instant::now() + Duration::from_secs(1),
                &mut seen
            )
            .unwrap(),
            "222222"
        );
        assert_eq!(
            verification_mail(
                &identity("other@example.com"),
                &approver,
                Some(1000),
                Instant::now() + Duration::from_secs(1),
                &mut seen
            )
            .unwrap(),
            "111111"
        );
        assert!(
            verification_mail(
                &identity("person@example.com"),
                &approver,
                Some(1000),
                Instant::now() + Duration::from_millis(50),
                &mut seen
            )
            .is_err(),
            "neither a consumed code nor a code from before this login may be reused"
        );
    }
}
