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
#[cfg(test)]
use super::wake_deadline::WakeDeadline;
use crate::ai::connector::{Cli, Region};

#[path = "google_federation_recovery.rs"]
pub(super) mod recovery;

const REGISTRY_FILE: &str = "google-identities.toml";

#[derive(Clone, PartialEq, Eq)]
pub(super) struct NativeAttempt {
    recovery: Option<login_drive::RecoveryKey>,
    admission: Option<login_drive::RecoveryAuthority>,
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
            admission: request
                .admission
                .map(login_drive::RecoveryAdmission::authority),
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
    work: Option<super::account_work::Scope>,
}

impl FederationScope {
    fn current(&self) -> Result<(), String> {
        self.work_current()?;
        self.binding_current()?;
        self.work_current()
    }

    fn binding_current(&self) -> Result<(), String> {
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

    fn work_current(&self) -> Result<(), String> {
        if self
            .work
            .as_ref()
            .is_some_and(super::account_work::Scope::cancelled)
        {
            Err("automatic account work is cancelled".into())
        } else {
            Ok(())
        }
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
        self.work_current()?;
        self.check_receipt_request(request)?;
        self.work_current()
    }

    fn check_receipt_request(&self, request: &login_drive::Request<'_>) -> Result<(), String> {
        if NativeAttempt::capture(request) != self.attempt {
            return Err("Google federation native attempt changed".into());
        }
        self.check_attempt()?;
        self.binding_current()
    }
}

pub(super) struct PendingFederation {
    session: Option<PendingSession>,
    native: Option<KimiProof>,
    scope: FederationScope,
}

/// Native Kimi's canonical account proof and the Google binding that authorized its flow.
pub(super) struct VerifiedKimiGoogle<'a> {
    native: KimiProof,
    journal: Option<&'a recovery::Journal>,
    scope: FederationScope,
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
    pub(super) fn retain_work(&mut self, work: Option<super::account_work::Scope>) {
        if self.scope.work.is_none() {
            self.scope.work = work;
        }
        if let Some(session) = self.session.as_mut() {
            session.retain_work(self.scope.work.clone());
        }
    }
    pub(super) fn verified_email(&self) -> Option<&str> {
        self.native.as_ref().map(LoginProof::identity)
    }

    pub(super) fn complete(
        mut self,
        request: &login_drive::Request<'_>,
        outcome: &login_drive::Outcome,
        deadline: login_drive::Deadline<'_>,
        journal: Option<&recovery::Journal>,
    ) -> Result<(), String> {
        let cached = self.native.take();
        let native = self.complete_checked(request, outcome, deadline, journal, |scope| {
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
        })?;
        if let Some(native) = native
            && let Some(admission) = request.admission.filter(|a| !a.is_shared_root())
        {
            let journal = journal.ok_or("pool Kimi proof has no native staging claim")?;
            admission.retain_kimi(native, journal.native_claim())?;
        }
        Ok(())
    }

    pub(super) fn commit_recovered(
        mut self,
        request: &login_drive::Request<'_>,
        journal: &recovery::Journal,
        deadline: login_drive::Deadline<'_>,
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
        deadline: login_drive::Deadline<'_>,
        verify: impl FnOnce(&FederationScope) -> Result<KimiProof, String>,
    ) -> Result<(), String> {
        self.complete_checked(request, outcome, deadline, None, verify)
            .map(|_| ())
    }

    fn complete_checked(
        self,
        request: &login_drive::Request<'_>,
        outcome: &login_drive::Outcome,
        deadline: login_drive::Deadline<'_>,
        journal: Option<&recovery::Journal>,
        verify: impl FnOnce(&FederationScope) -> Result<KimiProof, String>,
    ) -> Result<Option<KimiProof>, String> {
        if !matches!(outcome, login_drive::Outcome::SignedIn { .. }) {
            return Ok(None);
        }
        self.scope.check_request(request)?;
        if deadline.expired() {
            return Err("Kimi federation verification deadline expired".into());
        }
        let native = verify(&self.scope)?;
        self.scope.check_request(request)?;
        if deadline.expired() {
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
        if deadline.expired() {
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
    let Some(attempt) = approver
        .native_attempt
        .as_ref()
        .filter(|attempt| attempt.cli == *cli && attempt.row.label == *account)
        .cloned()
    else {
        return Some(quiet("Google federation has no bound native attempt"));
    };
    if !native_identity_matches(&attempt.row, handoff.email, binding.expected_email()) {
        return Some(Approval::Refused(
            "Google binding does not match the native account identity".into(),
        ));
    }
    let Some(home) = approver.home.as_deref() else {
        return Some(Approval::NotRun(
            "Google credential home is unavailable".into(),
        ));
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
        work: approver.work.clone(),
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
    if handoff.deadline.expired() {
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
    let mut browser = match MemoryBrowser::start_until(
        identity,
        &approver.google_state,
        Some(home),
        handoff.deadline,
        approver.work.clone(),
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
        if handoff.deadline.expired() {
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
    browser: &mut MemoryBrowser<'_>,
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
    deadline: login_drive::Deadline<'_>,
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
    deadline: login_drive::Deadline<'_>,
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
    if deadline.expired() {
        return Err("Google verification mail deadline expired");
    }
    match oauth::poll_mailbox_config_until(
        &config,
        Some(target),
        deadline.limited(Duration::from_secs(60)),
        false,
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
    browser: &mut MemoryBrowser<'_>,
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

fn native_identity_matches(
    row: &super::accounts::Row,
    login: Option<&str>,
    expected: &str,
) -> bool {
    let login_matches = match (row.email.as_deref(), login) {
        (Some(declared), Some(reported)) => super::accounts::email_matches(declared, reported),
        (None, None) => true,
        _ => false,
    };
    login_matches
        && row
            .expected_identity_email()
            .is_some_and(|identity| super::accounts::email_matches(identity, expected))
}

#[cfg(test)]
#[path = "google_federation_tests.rs"]
mod tests;
