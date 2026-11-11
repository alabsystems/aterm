// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed provider effects for a root-authorized local adoption. The caller
//! retains authority, scheduling and atomic publication; providers only stage
//! and verify a login.

use super::*;

#[path = "cli_shared_inputs.rs"]
mod inputs;
pub use inputs::{MailboxInputs, StagedMailbox};
#[path = "cli_shared_claude.rs"]
mod claude;
pub use claude::{ClaudeProfile, read_claude_profile};
#[path = "cli_shared_kimi.rs"]
mod kimi;
pub use kimi::KimiOauth;
#[cfg(unix)]
pub use kimi::RefreshAuthority as KimiRefreshAuthority;
#[cfg(unix)]
pub use kimi::RefreshRecoveryContext as KimiRecoveryContext;
pub(in crate::harness) use kimi::credential_path as kimi_credential_path;
#[cfg(all(unix, not(test)))]
pub(in crate::harness) use kimi::verify_or_refresh as verify_or_refresh_kimi;
#[cfg(unix)]
pub(in crate::harness) use kimi::{
    KimiProof, NativeError as KimiNativeError, RejectedSnapshot as KimiRejectedSnapshot,
    expire_pending as expire_kimi_pending, observation_error as kimi_observation_error,
    pending_expiry as kimi_pending_expiry, pending_files as kimi_pending_files,
    rejected_snapshot as kimi_rejected_snapshot,
    verify_native_classified as verify_native_kimi_classified,
    verify_or_refresh_with_rejection as verify_or_refresh_kimi_with_rejection,
};
pub(in crate::harness) use kimi::{ManagedRoute as KimiManagedRoute, PROVIDER as KIMI_MANAGED};
#[cfg(all(test, unix))]
pub(in crate::harness) use kimi::{
    pending_proof_fixture as kimi_pending_proof_fixture, proof_fixture as kimi_proof_fixture,
    rejection_fixture as kimi_rejection_fixture,
};

/// A provider-returned identity with private evidence of the staged files.
/// `check_current` performs bounded local reads only, never provider I/O.
pub trait LoginProof {
    fn identity(&self) -> &str;
    fn principal(&self) -> [u8; 32];
    fn subscription(&self) -> Option<&str>;
    fn check_current(&self) -> Result<(), String>;
}

/// The composing executable chooses a closed provider adapter after validating
/// the exact root configuration. Catalog strings never construct effects.
pub struct Context<'a> {
    pub publication: Option<&'a super::super::codex_app_login::PublicationCustody>,
    /// Current root authority for the exact retained native attempt.
    pub admission: Option<&'a super::super::login_drive::RecoveryAdmission>,
    pub env: &'a Env,
    pub store: &'a accounts::Store,
    pub host: &'a dyn AccountsHost,
}

/// Whether this call reached a possible provider effect. Only a typed
/// no-start result permits another execution of the same retained attempt.
pub enum Execution {
    Completed,
    NotStarted(String),
    Unknown(String),
    /// Work may have started, but an authenticated service cannot currently continue it.
    Deferred(String),
}

impl Execution {
    fn into_result(self) -> Result<(), String> {
        match self {
            Self::Completed => Ok(()),
            Self::NotStarted(why) | Self::Unknown(why) | Self::Deferred(why) => Err(why),
        }
    }
}

/// An unavailable identity read is not evidence that another login is needed.
#[derive(Debug, PartialEq, Eq)]
pub enum ObservationError {
    Unavailable(String),
    RecoveryRequired(String),
    /// A prior confirmed rejection is still bound to the current local files.
    /// This is a recovery reason, never authority to perform another login.
    ReauthenticationRequired(String),
}

impl std::fmt::Display for ObservationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(why)
            | Self::RecoveryRequired(why)
            | Self::ReauthenticationRequired(why) => f.write_str(why),
        }
    }
}

pub trait Provider: Send + Sync {
    fn execute(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        browser: &RenewBrowser,
    ) -> Result<(), String>;
    fn execute_attempt(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        browser: &RenewBrowser,
    ) -> Execution {
        match self.execute(context, row, staging, browser) {
            Ok(()) => Execution::Completed,
            Err(why) => Execution::Unknown(why),
        }
    }
    fn observe(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
    ) -> Result<Option<Box<dyn LoginProof>>, ObservationError>;
}

/// Claude's existing OAuth driver. This does not import shared credentials.
pub struct ClaudeOauth;

impl ClaudeOauth {
    /// Run native Claude OAuth with only this root-approved mailbox route.
    pub fn execute_attempt_with_inputs(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        browser: &RenewBrowser,
        inputs: &MailboxInputs,
    ) -> Execution {
        if !matches!(browser, RenewBrowser::Remote { opener: Some(_) }) {
            return Execution::NotStarted("approved mailbox flow has no private opener".into());
        }
        let Some(path) = staging.to_str() else {
            return Execution::NotStarted("staging path is not UTF-8".into());
        };
        let Some(email) = row.email.as_deref() else {
            return Execution::NotStarted("approved account has no sign-in email".into());
        };
        let staged = match inputs.stage_in(staging) {
            Ok(staged) => staged,
            Err(why) => return Execution::NotStarted(why),
        };
        match context
            .host
            .renew_outcome_with_inputs(path, email, browser, staged.path())
        {
            Ok(exit) if exit.is_deferred() => Execution::Deferred(exit.failure_words()),
            Ok(exit) if exit.code == 0 => Execution::Completed,
            Ok(exit) => Execution::Unknown(exit.failure_words()),
            Err(why) if why.is_deferred() => Execution::Deferred(why.to_string()),
            Err(why) => Execution::Unknown(why.to_string()),
        }
    }
}

/// The executable supplies its bounded, no-UI reader for this exact staging
/// directory. No arbitrary service name or secret leaves the provider proof.
pub type KeychainReader = dyn Fn(&Path, std::time::Instant) -> Result<Option<aterm_uds::secret::Secret>, String>
    + Send
    + Sync;

impl ClaudeOauth {
    pub fn observe_with_keychain(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        reader: std::sync::Arc<KeychainReader>,
    ) -> Result<Option<Box<dyn LoginProof>>, ObservationError> {
        claude_observe(context, row, staging, Some(reader))
    }
}

struct KeychainSnapshot {
    staging: PathBuf,
    bytes: Option<aterm_uds::secret::Secret>,
    reader: std::sync::Arc<KeychainReader>,
}

impl KeychainSnapshot {
    fn read(staging: &Path, reader: std::sync::Arc<KeychainReader>) -> Result<Self, String> {
        let bytes = reader(staging, std::time::Instant::now() + AUTH_CHECK_BUDGET)?;
        Ok(Self {
            staging: staging.to_owned(),
            bytes,
            reader,
        })
    }
    fn check_current(&self) -> Result<(), String> {
        let current = Self::read(&self.staging, self.reader.clone())?;
        if self.bytes.as_ref().map(aterm_uds::secret::Secret::expose)
            != current
                .bytes
                .as_ref()
                .map(aterm_uds::secret::Secret::expose)
        {
            return Err("staged Keychain credential changed after provider verification".into());
        }
        Ok(())
    }
}

struct ClaudeProof {
    staging: PathBuf,
    identity: String,
    principal: [u8; 32],
    subscription: Option<String>,
    files: Vec<PrivateSnapshot>,
    keychain: Option<KeychainSnapshot>,
}

impl LoginProof for ClaudeProof {
    fn identity(&self) -> &str {
        &self.identity
    }
    fn principal(&self) -> [u8; 32] {
        self.principal
    }
    fn subscription(&self) -> Option<&str> {
        self.subscription.as_deref()
    }
    fn check_current(&self) -> Result<(), String> {
        for file in &self.files {
            file.check_current()?;
        }
        if let Some(keychain) = &self.keychain {
            keychain.check_current()?;
        }
        let observed = profile::identity_in_home(&self.staging, &self.identity);
        if observed.principal != Some(self.principal) || observed.email_matches != Some(true) {
            return Err("staged provider identity changed before adoption".into());
        }
        Ok(())
    }
}

impl Provider for ClaudeOauth {
    fn execute_attempt(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        browser: &RenewBrowser,
    ) -> Execution {
        let Some(path) = staging.to_str() else {
            return Execution::Unknown("staging path is not UTF-8".into());
        };
        let Some(email) = row.email.as_deref() else {
            return Execution::Unknown("approved account has no sign-in email".into());
        };
        match context.host.renew_outcome(path, email, browser) {
            Ok(exit) if exit.is_deferred() => Execution::Deferred(exit.failure_words()),
            Ok(exit) if exit.code == 0 => Execution::Completed,
            Ok(exit) => Execution::Unknown(exit.failure_words()),
            Err(why) if why.is_deferred() => Execution::Deferred(why.to_string()),
            Err(why) => Execution::Unknown(why.to_string()),
        }
    }
    fn execute(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        browser: &RenewBrowser,
    ) -> Result<(), String> {
        self.execute_attempt(context, row, staging, browser)
            .into_result()
    }
    fn observe(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
    ) -> Result<Option<Box<dyn LoginProof>>, ObservationError> {
        claude_observe(context, row, staging, None)
    }
}

fn claude_observe(
    context: &Context<'_>,
    row: &accounts::Row,
    staging: &Path,
    reader: Option<std::sync::Arc<KeychainReader>>,
) -> Result<Option<Box<dyn LoginProof>>, ObservationError> {
    if !accounts::login_made(staging) {
        return Ok(None);
    }
    let path = staging
        .to_str()
        .ok_or_else(|| ObservationError::RecoveryRequired("staging path is not UTF-8".into()))?;
    let email = row.expected_identity_email().ok_or_else(|| {
        ObservationError::RecoveryRequired("approved account has no provider identity".into())
    })?;
    let mut files = vec![
        PrivateSnapshot::read(staging.join(".credentials.json"), true)
            .map_err(ObservationError::RecoveryRequired)?,
    ];
    let keychain = reader
        .map(|reader| KeychainSnapshot::read(staging, reader))
        .transpose()
        .map_err(ObservationError::Unavailable)?;
    if keychain.as_ref().is_none_or(|item| item.bytes.is_none()) && files[0].bytes.is_none() {
        return Err(ObservationError::RecoveryRequired(
            "staged credential backend has no verifiable private bytes".into(),
        ));
    }
    let facts = status_facts(
        &context
            .host
            .status_in_home(path)
            .map_err(ObservationError::Unavailable)?,
        email,
    )
    .filter(StatusFacts::renews)
    .ok_or_else(|| {
        ObservationError::RecoveryRequired(
            "staged provider status does not prove the approved account".into(),
        )
    })?;
    // The status command may refresh profile metadata. Its selected credential
    // must stay fixed, while the resulting profile is pinned before publication.
    files.push(
        PrivateSnapshot::read(staging.join(".claude.json"), false)
            .map_err(ObservationError::RecoveryRequired)?,
    );
    let credential = keychain
        .as_ref()
        .and_then(|item| item.bytes.as_ref().map(aterm_uds::secret::Secret::expose))
        .or_else(|| {
            files[0]
                .bytes
                .as_ref()
                .map(aterm_uds::secret::Secret::expose)
        })
        .ok_or_else(|| {
            ObservationError::RecoveryRequired("staged Claude credential is absent".into())
        })?;
    let remote = claude::verify_credentials(
        credential,
        email,
        context.host.now().max(context.env.now),
        |token| context.host.shared_claude_profile(token, AUTH_CHECK_BUDGET),
    )?;
    let observed = context.host.claude_identity_in_home(path, email);
    if observed.email_matches != Some(true) || observed.principal != Some(remote.principal) {
        return Err(ObservationError::RecoveryRequired(
            "staged provider identity contradicts its status".into(),
        ));
    }
    let proof = ClaudeProof {
        staging: staging.to_owned(),
        identity: email.to_owned(),
        principal: remote.principal,
        subscription: facts.subscription,
        files,
        keychain,
    };
    proof
        .check_current()
        .map_err(ObservationError::RecoveryRequired)?;
    Ok(Some(Box::new(proof)))
}

/// A fresh device-local Codex sign-in through the registered native driver.
pub struct CodexOauth;

impl CodexOauth {
    /// Supply only mailbox inputs that the composing root adapter has checked.
    pub fn execute_with_inputs(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        inputs: &MailboxInputs,
    ) -> Result<(), String> {
        self.execute_inner(context, row, staging, Some(inputs))
            .into_result()
    }

    pub fn execute_attempt_with_inputs(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        inputs: &MailboxInputs,
    ) -> Execution {
        self.execute_inner(context, row, staging, Some(inputs))
    }

    fn execute_inner(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        inputs: Option<&MailboxInputs>,
    ) -> Execution {
        let request = match codex_request(context, row, staging) {
            Ok(request) => request,
            Err(why) => return Execution::NotStarted(why),
        };
        let result = if let Some(inputs) = inputs {
            let Some(home) = context.env.home.as_deref() else {
                return Execution::NotStarted("Codex sign-in has no local home".into());
            };
            context
                .host
                .vendor_sign_in_with_mailbox(&request, home, inputs.mailbox())
        } else {
            context.host.vendor_sign_in(&request)
        };
        codex_execution(result.outcome)
    }
}

fn codex_execution(outcome: super::super::login_drive::Outcome) -> Execution {
    match outcome {
        super::super::login_drive::Outcome::SignedIn { .. } => Execution::Completed,
        super::super::login_drive::Outcome::CompletedHeld
        | super::super::login_drive::Outcome::CustodyHeld => {
            Execution::Deferred("completed native login retained without publication".into())
        }
        super::super::login_drive::Outcome::NotRun(why) => Execution::NotStarted(why),
        super::super::login_drive::Outcome::PreparationFailed(stage) => {
            Execution::NotStarted(stage.reason().into())
        }
        super::super::login_drive::Outcome::PageNotOpened(why)
        | super::super::login_drive::Outcome::Unverified(why) => Execution::Unknown(why),
        _ => Execution::Unknown("device-local Codex sign-in did not complete".into()),
    }
}

fn codex_request<'a>(
    context: &'a Context<'_>,
    row: &'a accounts::Row,
    staging: &'a Path,
) -> Result<super::super::login_drive::Request<'a>, String> {
    let home = context
        .env
        .home
        .as_deref()
        .ok_or("Codex sign-in has no local home")?;
    if context.store.cli() != Cli::Codex || row.browser.is_some() {
        return Err("unsupported shared Codex local route".into());
    }
    let program = context
        .host
        .codex_launch_program()
        .unwrap_or_else(|| super::super::login_drive::program_of(Cli::Codex, home));
    let sign_in = context
        .host
        .codex_sign_in_program()
        .unwrap_or_else(|| super::super::login_drive::program_of(Cli::Codex, home));
    if super::super::codex_alab::auth_path_unread(&program).is_some()
        || super::super::codex_alab::auth_path_unread(&sign_in).is_some()
        || !crate::ai::facts::codex_file_store(&home.join(Cli::Codex.accounts().default_home))
    {
        return Err("installed Codex cannot safely use a private staged login".into());
    }
    let route = Cli::Codex
        .accounts()
        .signin
        .as_ref()
        .and_then(|signin| signin.route(row.flow(Cli::Codex)))
        .ok_or("shared Codex has no supported sign-in route")?;
    let hosts = route
        .hosts
        .of(row.vendor.region)
        .ok_or("shared Codex route is incomplete")?;
    Ok(super::super::login_drive::Request {
        publication: context.publication,
        enrollment: None,
        recovery: None,
        admission: None,
        cli: Cli::Codex,
        route,
        row,
        staging,
        store: context.store,
        hosts,
    })
}

impl Provider for CodexOauth {
    fn execute(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        _browser: &RenewBrowser,
    ) -> Result<(), String> {
        self.execute_inner(context, row, staging, None)
            .into_result()
    }

    fn execute_attempt(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        _browser: &RenewBrowser,
    ) -> Execution {
        self.execute_inner(context, row, staging, None)
    }

    fn observe(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
    ) -> Result<Option<Box<dyn LoginProof>>, ObservationError> {
        let email = row.expected_identity_email().ok_or_else(|| {
            ObservationError::RecoveryRequired("Codex approval has no identity".into())
        })?;
        super::super::codex_app_login::shared::verify_in_home(
            staging,
            email,
            AUTH_CHECK_BUDGET,
            context.host.now().max(context.env.now),
        )
        .map(|proof| proof.map(|proof| Box::new(CodexProof(proof)) as Box<dyn LoginProof>))
    }
}

struct CodexProof(super::super::codex_app_login::shared::VerifiedLogin);

impl LoginProof for CodexProof {
    fn identity(&self) -> &str {
        self.0.email()
    }
    fn principal(&self) -> [u8; 32] {
        self.0.principal()
    }
    fn subscription(&self) -> Option<&str> {
        self.0.plan()
    }
    fn check_current(&self) -> Result<(), String> {
        self.0.check_current()
    }
}

/// Exact private evidence; neither bytes nor their digest leave this object.
struct PrivateSnapshot {
    path: PathBuf,
    bytes: Option<aterm_uds::secret::Secret>,
    private: bool,
}

impl PrivateSnapshot {
    fn read(path: PathBuf, private: bool) -> Result<Self, String> {
        let bytes = read_staged_bytes(&path, private)?;
        Ok(Self {
            path,
            bytes,
            private,
        })
    }

    fn check_current(&self) -> Result<(), String> {
        let current = Self::read(self.path.clone(), self.private)?;
        if self.bytes.as_ref().map(aterm_uds::secret::Secret::expose)
            != current
                .bytes
                .as_ref()
                .map(aterm_uds::secret::Secret::expose)
        {
            return Err("staged private evidence changed after provider verification".into());
        }
        Ok(())
    }
}

fn read_staged_bytes(
    path: &Path,
    private: bool,
) -> Result<Option<aterm_uds::secret::Secret>, String> {
    #[cfg(unix)]
    {
        use aterm_uds::private_file::{PrivateFileError, read_owned, read_private};
        let parent = path
            .parent()
            .ok_or("staged evidence has no private parent")?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("invalid staged evidence name")?;
        let max = 2 * 1024 * 1024;
        let result = if private {
            read_private(parent, name, max)
        } else {
            read_owned(parent, name, max)
        };
        match result {
            Ok(bytes) => Ok(Some(aterm_uds::secret::Secret::from_vec(bytes))),
            Err(PrivateFileError::Missing) => Ok(None),
            Err(_) => Err("staged evidence is not private, owned, or intact".into()),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, private);
        Err("private shared-login evidence is unavailable on this platform".into())
    }
}

/// Scrubs every string of `value` as it drops it ([`aterm_uds::secret::Secret`]).
pub(crate) fn wipe_json(value: Value) {
    match value {
        Value::String(text) => {
            drop(aterm_uds::secret::Secret::from_vec(text.into_bytes()));
        }
        Value::Array(values) => values.into_iter().for_each(wipe_json),
        Value::Object(values) => values.into_values().for_each(wipe_json),
        _ => {}
    }
}

#[cfg(test)]
#[path = "cli_shared_provider_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "cli_shared_mailbox_tests.rs"]
mod mailbox_tests;
