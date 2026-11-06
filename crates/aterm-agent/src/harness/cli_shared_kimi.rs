// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fresh, device-local Kimi login, checked against its region's `/me` endpoint.

use super::*;
use crate::ai::connector::{Flow, Region};
use crate::harness::{headless_oauth, login_drive};
use aterm_http::{Client, ProxyMode, Trust};
use aterm_json::Value;
use aterm_uds::secret::Secret;

const REPLY_MAX: usize = 256 * 1024;
const PROVIDER: &str = "managed:kimi-code";

#[cfg(unix)]
#[path = "cli_shared_kimi_refresh.rs"]
mod refresh;
#[cfg(unix)]
pub(in crate::harness) use refresh::{
    Snapshot as RejectedSnapshot, rejected_snapshot, verify_or_refresh,
    verify_or_refresh_with_rejection,
};

/// Kimi's native device flow, with a separate staged home on every machine.
pub struct KimiOauth;

impl KimiOauth {
    pub fn execute_attempt_with_mailbox(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        mailbox: Option<&headless_oauth::MailboxConfig>,
    ) -> Execution {
        let request = match request(context, row, staging) {
            Ok(request) => request,
            Err(why) => return Execution::NotStarted(why),
        };
        let result = if let Some(mailbox) = mailbox {
            let Some(home) = context.env.home.as_deref() else {
                return Execution::NotStarted("Kimi sign-in has no local home".into());
            };
            context
                .host
                .vendor_sign_in_with_mailbox(&request, home, mailbox.clone())
        } else {
            context.host.vendor_sign_in(&request)
        };
        match result.outcome {
            login_drive::Outcome::SignedIn { .. } => Execution::Completed,
            login_drive::Outcome::NotRun(why) => Execution::NotStarted(why),
            login_drive::Outcome::PageNotOpened(why) | login_drive::Outcome::Unverified(why) => {
                Execution::Unknown(why)
            }
            _ => Execution::Unknown("device-local Kimi sign-in did not complete".into()),
        }
    }
    /// Full region-scoped provider identity; never an abbreviated display key.
    pub fn principal(region: Region, user: &str) -> Option<[u8; 32]> {
        if user.is_empty() || user.len() > 1024 || user.chars().any(char::is_control) {
            return None;
        }
        let mut hash = aterm_digest::Sha256::new();
        hash.update(b"aterm/kimi-provider-principal/1\0");
        for field in [region.as_str(), user] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field.as_bytes());
        }
        Some(hash.finalize())
    }
}

impl Provider for KimiOauth {
    fn execute(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        _browser: &RenewBrowser,
    ) -> Result<(), String> {
        self.execute_attempt_with_mailbox(context, row, staging, None)
            .into_result()
    }

    fn execute_attempt(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
        _browser: &RenewBrowser,
    ) -> Execution {
        self.execute_attempt_with_mailbox(context, row, staging, None)
    }

    fn observe(
        &self,
        context: &Context<'_>,
        row: &accounts::Row,
        staging: &Path,
    ) -> Result<Option<Box<dyn LoginProof>>, ObservationError> {
        let region = region(row).map_err(ObservationError::RecoveryRequired)?;
        let email = row.expected_identity_email().ok_or_else(|| {
            ObservationError::RecoveryRequired("Kimi approval has no identity".into())
        })?;
        #[cfg(unix)]
        if context.admission.is_some() {
            let request =
                request(context, row, staging).map_err(ObservationError::RecoveryRequired)?;
            let home = context.env.home.as_deref().ok_or_else(|| {
                ObservationError::Unavailable("Kimi recovery home is unavailable".into())
            })?;
            if let Some(mut journal) =
                crate::harness::google_federation::recovery::Journal::open_scoped(
                    &request,
                    home,
                    context.host.work_scope(),
                )
                .map_err(ObservationError::Unavailable)?
            {
                let deadline =
                    crate::harness::wake_deadline::WakeDeadline::after(AUTH_CHECK_BUDGET);
                if let Some(proof) = journal.observe_existing(&request, home, deadline)? {
                    return Ok(Some(Box::new(proof)));
                }
                return journal
                    .observe_current(&request, home, deadline)
                    .map(|proof| proof.map(|proof| Box::new(proof) as Box<dyn LoginProof>));
            }
        }
        let now = context.host.now().max(context.env.now);
        #[cfg(unix)]
        if let Some(admission) = context.admission {
            let _claim =
                crate::harness::browser_window::ProfileLock::claim(staging).map_err(|_| {
                    ObservationError::Unavailable(
                        "Kimi native staging is held or unavailable".into(),
                    )
                })?;
            return verify_or_refresh(
                staging,
                region,
                email,
                now,
                std::time::Instant::now() + AUTH_CHECK_BUDGET,
                std::sync::Arc::new(refresh::AdmissionGuard(admission.clone())),
            )
            .map(|proof| proof.map(|proof| Box::new(proof) as Box<dyn LoginProof>))
            .map_err(observation_error);
        }
        verify_native_classified(
            staging,
            region,
            email,
            now,
            std::time::Instant::now() + AUTH_CHECK_BUDGET,
        )
        .map(|proof| proof.map(|proof| Box::new(proof) as Box<dyn LoginProof>))
        .map_err(observation_error)
    }
}

pub(in crate::harness) fn observation_error(error: NativeError) -> ObservationError {
    match error {
        NativeError::Temporary => ObservationError::Unavailable(error.to_string()),
        #[cfg(unix)]
        NativeError::RefreshRejected => ObservationError::RecoveryRequired(error.to_string()),
        #[cfg(unix)]
        NativeError::PreviouslyRejected => {
            ObservationError::ReauthenticationRequired(error.to_string())
        }
        NativeError::Expired
        | NativeError::Rejected
        | NativeError::Local(_)
        | NativeError::Refused(_) => ObservationError::RecoveryRequired(error.to_string()),
    }
}

#[derive(Debug)]
pub(in crate::harness) enum NativeError {
    /// A supported local bearer whose checked expiry has elapsed. No provider I/O ran.
    Expired,
    /// The pinned refresh endpoint rejected the grant and its exact snapshot was recorded.
    #[cfg(unix)]
    RefreshRejected,
    /// An unchanged local receipt of a prior confirmed refresh rejection.
    #[cfg(unix)]
    PreviouslyRejected,
    Rejected,
    Temporary,
    Local(String),
    Refused(String),
}
impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Expired => f.write_str("Kimi native credential has expired"),
            #[cfg(unix)]
            Self::RefreshRejected | Self::PreviouslyRejected => {
                f.write_str("Kimi rejected the native refresh grant")
            }
            Self::Rejected => f.write_str("Kimi rejected the native credential"),
            Self::Temporary => f.write_str("Kimi identity verification is temporarily unavailable"),
            Self::Local(why) | Self::Refused(why) => f.write_str(why),
        }
    }
}
impl From<&str> for NativeError {
    fn from(why: &str) -> Self {
        Self::Local(why.into())
    }
}
impl From<String> for NativeError {
    fn from(why: String) -> Self {
        Self::Local(why)
    }
}

pub(in crate::harness) fn verify_native_classified(
    staging: &Path,
    region: Region,
    expected: &str,
    now: i64,
    deadline: std::time::Instant,
) -> Result<Option<KimiProof>, NativeError> {
    verify_native_guarded(staging, region, expected, now, deadline, None)
}

fn verify_native_guarded(
    staging: &Path,
    region: Region,
    expected: &str,
    now: i64,
    deadline: std::time::Instant,
    guard: Option<std::sync::Arc<dyn aterm_http::Guard>>,
) -> Result<Option<KimiProof>, NativeError> {
    if std::time::Instant::now() >= deadline {
        return Err(NativeError::Temporary);
    }
    let proof = verify_with(staging, region, expected, now, |url, token| {
        let wait = deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|wait| !wait.is_zero())
            .ok_or_else(|| "Kimi identity verification deadline expired".to_owned())?
            .min(AUTH_CHECK_BUDGET);
        let client = Client::new(Trust::PlatformVerifier, ProxyMode::Direct, wait);
        let mut request = client
            .get(url)
            .header("Accept", "application/json")
            .secret_header("Authorization", {
                let mut header = b"Bearer ".to_vec();
                header.extend_from_slice(token.as_bytes());
                Secret::from_vec(header)
            })
            .limit(REPLY_MAX)
            .backstop(wait);
        if let Some(guard) = &guard {
            request = request.guard(guard.clone());
        }
        let reply = request
            .send(&[])
            .map_err(|_| "Kimi identity request did not complete".to_owned())?;
        Ok((reply.status(), reply.body().to_vec()))
    });
    if std::time::Instant::now() >= deadline
        || guard.as_ref().is_some_and(|guard| !guard.is_authorized())
    {
        return Err(NativeError::Temporary);
    }
    proof
}

fn region(row: &accounts::Row) -> Result<Region, String> {
    if row.vendor.flow.is_some() {
        return Err("Kimi native device flow takes no local flow override".into());
    }
    row.vendor
        .region
        .ok_or_else(|| "Kimi approval requires an account region".into())
}

fn request<'a>(
    context: &'a Context<'_>,
    row: &'a accounts::Row,
    staging: &'a Path,
) -> Result<login_drive::Request<'a>, String> {
    let region = region(row)?;
    if context.store.cli() != Cli::Kimi || row.browser.is_some() {
        return Err("unsupported shared Kimi local route".into());
    }
    let route = Cli::Kimi
        .accounts()
        .signin
        .as_ref()
        .and_then(|signin| signin.route(Some(Flow::Device)))
        .ok_or("Kimi device sign-in is unavailable")?;
    let hosts = route
        .hosts
        .of(Some(region))
        .ok_or("Kimi device sign-in has no approved hosts")?;
    Ok(login_drive::Request {
        recovery: context.admission.map(login_drive::RecoveryAdmission::key),
        admission: context.admission,
        cli: Cli::Kimi,
        route,
        row,
        staging,
        store: context.store,
        hosts,
    })
}

fn base(region: Region) -> &'static str {
    match region {
        Region::Global => "https://api.kimi.ai/coding/v1",
        Region::MainlandCn => "https://api.kimi.com/coding/v1",
    }
}

fn issuer(region: Region) -> &'static str {
    match region {
        Region::Global => "https://auth.kimi.ai",
        Region::MainlandCn => "https://auth.kimi.com",
    }
}

fn oauth_key(region: Region) -> String {
    if region == Region::MainlandCn {
        return "oauth/kimi-code".into();
    }
    // Kimi hashes the ordered compact JSON, including these camel-case keys.
    let input = format!(
        r#"{{"oauthHost":"{}","baseUrl":"{}"}}"#,
        issuer(region),
        base(region)
    );
    let digest = aterm_digest::Sha256::digest(input.as_bytes());
    format!(
        "oauth/kimi-code-env-{}",
        aterm_codec::hex::encode_lower(&digest[..8])
    )
}

pub(in crate::harness) fn credential_path(staging: &Path, region: Region) -> PathBuf {
    let key = oauth_key(region);
    staging
        .join("credentials")
        .join(format!("{}.json", &key[6..]))
}

#[cfg(unix)]
pub(in crate::harness) fn pending_files(
    staging: &Path,
    region: Region,
) -> Result<(aterm_secrets::SecretBytes, aterm_secrets::SecretBytes), String> {
    check_directory(staging)?;
    check_directory(&staging.join("credentials"))?;
    let config = FileEvidence::read(staging.join("config.toml"), false)?
        .ok_or("Kimi staged config is absent")?;
    let credential = FileEvidence::read(credential_path(staging, region), true)?
        .ok_or("Kimi staged credential is absent")?;
    check_config(config.bytes.expose(), region)?;
    pending_expiry(credential.bytes.expose())?;
    Ok((
        aterm_secrets::SecretBytes::from(config.bytes.expose()),
        aterm_secrets::SecretBytes::from(credential.bytes.expose()),
    ))
}

#[cfg(unix)]
pub(in crate::harness) fn pending_expiry(bytes: &[u8]) -> Result<i64, String> {
    let value: Value =
        aterm_json::from_slice(bytes).map_err(|_| "Kimi pending credential is invalid")?;
    let result = if value["token_type"].as_str() != Some("Bearer")
        || !value["access_token"].as_str().is_some_and(|token| {
            !token.is_empty() && token.bytes().all(|b| (0x21..=0x7e).contains(&b))
        }) {
        Err("Kimi pending credential has no supported bearer".to_owned())
    } else {
        value["expires_at"]
            .as_i64()
            .ok_or_else(|| "Kimi pending credential has no expiry".to_owned())
    };
    super::wipe_json(value);
    result
}

#[cfg(unix)]
pub(in crate::harness) fn expire_pending(staging: &Path, region: Region) -> Result<(), String> {
    let path = credential_path(staging, region);
    let bytes =
        FileEvidence::read(path.clone(), true)?.ok_or("Kimi pending credential is absent")?;
    let mut value: Value = aterm_json::from_slice(bytes.bytes.expose())
        .map_err(|_| "Kimi pending credential is invalid")?;
    let replacement = if let Some(fields) = value.as_object_mut() {
        if let Some(old) = fields.insert("expires_at".into(), 0_i64.into()) {
            super::wipe_json(old);
        }
        aterm_json::to_vec(&value)
            .map(aterm_secrets::SecretBytes::from)
            .map_err(|_| "Kimi pending credential could not be encoded")
    } else {
        Err("Kimi pending credential is not an object")
    };
    super::wipe_json(value);
    let replacement = replacement?;
    bytes.check_current()?;
    aterm_secrets::fsx::write_atomic(&path, replacement.as_slice(), 0o600)
        .map_err(|_| "Kimi pending credential could not be expired".to_owned())
}

struct FileEvidence {
    path: PathBuf,
    bytes: Secret,
    private: bool,
}

impl FileEvidence {
    fn read(path: PathBuf, private: bool) -> Result<Option<Self>, String> {
        Ok(super::read_staged_bytes(&path, private)?.map(|bytes| Self {
            path,
            bytes,
            private,
        }))
    }

    fn check_current(&self) -> Result<(), String> {
        let current = Self::read(self.path.clone(), self.private)?
            .ok_or("Kimi staged evidence disappeared after verification")?;
        if current.bytes.expose() != self.bytes.expose() {
            return Err("Kimi staged evidence changed after verification".into());
        }
        Ok(())
    }
}

pub(in crate::harness) struct KimiProof {
    #[cfg(unix)]
    native_lock: Option<refresh::native_lock::NativeLock>,
    #[cfg(unix)]
    region: Region,
    #[cfg(unix)]
    staging: PathBuf,
    credential: FileEvidence,
    config: FileEvidence,
    email: String,
    principal: [u8; 32],
}

impl KimiProof {
    #[cfg(unix)]
    pub(in crate::harness) fn check_native(
        &self,
        staging: &Path,
        region: Region,
        expected: &str,
    ) -> Result<(), String> {
        if self.staging != staging
            || self.region != region
            || !self.email.eq_ignore_ascii_case(expected)
        {
            return Err("Kimi account proof does not match the native attempt".into());
        }
        self.check_current()
    }
}

impl LoginProof for KimiProof {
    fn identity(&self) -> &str {
        &self.email
    }
    fn principal(&self) -> [u8; 32] {
        self.principal
    }
    fn subscription(&self) -> Option<&str> {
        None
    }
    fn check_current(&self) -> Result<(), String> {
        #[cfg(unix)]
        if let Some(lock) = &self.native_lock {
            lock.check_current()?;
        }
        self.credential.check_current()?;
        self.config.check_current()
    }
}

fn check_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    aterm_uds::private_file::check_private_dir(path)
        .map_err(|_| "Kimi staging directory is not private and owned")?;
    let meta =
        std::fs::symlink_metadata(path).map_err(|_| "Kimi staging directory is unavailable")?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("Kimi staging directory is not a real directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != aterm_uds::peer::our_uid() || meta.mode() & 0o077 != 0 {
            return Err("Kimi staging directory is not private and owned".into());
        }
    }
    Ok(())
}

fn check_config(bytes: &[u8], region: Region) -> Result<(), String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Kimi staged config is not UTF-8")?;
    let doc: aterm_toml::Value =
        aterm_toml::from_str(text).map_err(|_| "Kimi staged config is invalid")?;
    let provider = doc
        .get("providers")
        .and_then(|providers| providers.get(PROVIDER))
        .ok_or("Kimi staged config has no managed provider")?;
    let fields = provider
        .as_table()
        .ok_or("Kimi staged config has no managed provider")?;
    let word = |key| provider.get(key).and_then(aterm_toml::Value::as_str);
    let oauth = provider
        .get("oauth")
        .and_then(aterm_toml::Value::as_table)
        .ok_or("Kimi staged config has no file credential binding")?;
    let auth_word = |key| oauth.get(key).and_then(aterm_toml::Value::as_str);
    if fields
        .keys()
        .any(|key| !matches!(key.as_str(), "type" | "base_url" | "api_key" | "oauth"))
        || oauth
            .keys()
            .any(|key| !matches!(key.as_str(), "storage" | "key" | "oauth_host"))
        || word("type") != Some("kimi")
        || word("base_url") != Some(base(region))
        || provider
            .get("api_key")
            .is_some_and(|key| key.as_str() != Some(""))
        || auth_word("storage") != Some("file")
        || auth_word("key") != Some(oauth_key(region).as_str())
        || oauth
            .get("oauth_host")
            .is_some_and(|host| host.as_str() != Some(issuer(region)))
        || (region == Region::Global && auth_word("oauth_host") != Some(issuer(region)))
    {
        return Err("Kimi staged provider differs from the approved native route".into());
    }
    let model = doc
        .get("default_model")
        .and_then(aterm_toml::Value::as_str)
        .ok_or("Kimi staged config has no default model")?;
    if doc
        .get("models")
        .and_then(|models| models.get(model))
        .and_then(|model| model.get("provider"))
        .and_then(aterm_toml::Value::as_str)
        != Some(PROVIDER)
    {
        return Err("Kimi default model does not use the staged login".into());
    }
    Ok(())
}

fn verify_with(
    staging: &Path,
    region: Region,
    expected: &str,
    now: i64,
    request: impl FnOnce(&str, &str) -> Result<(u16, Vec<u8>), String>,
) -> Result<Option<KimiProof>, NativeError> {
    check_directory(staging)?;
    let credential_dir = staging.join("credentials");
    match std::fs::symlink_metadata(&credential_dir) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Kimi staged credentials directory is unavailable".into()),
    }
    check_directory(&credential_dir)?;
    let config = FileEvidence::read(staging.join("config.toml"), false)?;
    if let Some(config) = &config {
        check_config(config.bytes.expose(), region)?;
    }
    let Some(credential) = FileEvidence::read(credential_path(staging, region), true)? else {
        return Ok(None);
    };
    let config = config.ok_or("Kimi staged config is absent")?;
    let token: Value = aterm_json::from_slice(credential.bytes.expose())
        .map_err(|_| "Kimi staged credential is invalid")?;
    let access = (|| {
        let access = token["access_token"]
            .as_str()
            .filter(|token| !token.is_empty() && token.bytes().all(|b| (0x21..=0x7e).contains(&b)))
            .ok_or_else(|| NativeError::from("Kimi staged credential has no valid bearer"))?;
        if token["token_type"].as_str() != Some("Bearer") {
            return Err(NativeError::from(
                "Kimi staged credential has unsupported token type",
            ));
        }
        let expiry = token["expires_at"]
            .as_i64()
            .ok_or_else(|| NativeError::from("Kimi staged credential has no valid expiry"))?;
        if expiry <= now {
            return Err(NativeError::Expired);
        }
        Ok(Secret::new(access.as_bytes()))
    })();
    super::wipe_json(token);
    let access = access?;
    let access = std::str::from_utf8(access.expose()).map_err(|_| "invalid Kimi token text")?;
    let (status, body) =
        request(&format!("{}/me", base(region)), access).map_err(|_| NativeError::Temporary)?;
    credential
        .check_current()
        .and_then(|()| config.check_current())
        .map_err(|_| NativeError::Temporary)?;
    if status == 401 {
        return Err(NativeError::Rejected);
    }
    if status != 200 || body.len() > REPLY_MAX {
        return Err(NativeError::Temporary);
    }
    let identity: Value = aterm_json::from_slice(&body).map_err(|_| NativeError::Temporary)?;
    let email = identity["email"]
        .as_str()
        .filter(|email| accounts::email_refusal(email).is_none())
        .ok_or(NativeError::Temporary)?;
    let principal = identity["user_id"]
        .as_str()
        .and_then(|user| KimiOauth::principal(region, user))
        .ok_or(NativeError::Temporary)?;
    if !accounts::email_matches(email, expected) {
        return Err(NativeError::Refused(
            "Kimi identity does not match the approved email".into(),
        ));
    }
    let proof = KimiProof {
        #[cfg(unix)]
        native_lock: None,
        #[cfg(unix)]
        region,
        #[cfg(unix)]
        staging: staging.into(),
        credential,
        config,
        email: email.to_owned(),
        principal,
    };
    proof.check_current().map_err(|_| NativeError::Temporary)?;
    Ok(Some(proof))
}

/// Synthetic canonical response for lifecycle tests; never used by production login.
#[cfg(all(test, unix))]
pub(in crate::harness) fn proof_fixture(staging: &Path, region: Region, email: &str) -> KimiProof {
    aterm_secrets::fsx::ensure_private_dir(staging).unwrap();
    aterm_secrets::fsx::ensure_private_dir(&staging.join("credentials")).unwrap();
    aterm_secrets::fsx::write_atomic(&credential_path(staging, region), br#"{"access_token":"fixture-access","refresh_token":"fixture-refresh","token_type":"Bearer","expires_at":2000}"#, 0o600).unwrap();
    let config = format!(
        "default_model='fixture'\n[providers.'{PROVIDER}']\ntype='kimi'\nbase_url='{}'\napi_key=''\n[providers.'{PROVIDER}'.oauth]\nstorage='file'\nkey='{}'\noauth_host='{}'\n[models.fixture]\nprovider='{PROVIDER}'\n",
        base(region),
        oauth_key(region),
        issuer(region)
    );
    aterm_secrets::fsx::write_atomic(&staging.join("config.toml"), config.as_bytes(), 0o600)
        .unwrap();
    verify_with(staging, region, email, 1000, |_, _| {
        Ok((
            200,
            aterm_json::to_vec(
                &aterm_json::json!({"email":email,"user_id":"fixture-full-user-id"}),
            )
            .unwrap(),
        ))
    })
    .unwrap()
    .unwrap()
}

#[cfg(all(test, unix))]
pub(in crate::harness) fn pending_proof_fixture(
    staging: &Path,
    region: Region,
    email: &str,
    initialize: bool,
) -> KimiProof {
    if initialize {
        drop(proof_fixture(staging, region, email));
        aterm_secrets::fsx::write_atomic(&credential_path(staging, region), br#"{"access_token":"fixture-access","refresh_token":"fixture-refresh","token_type":"Bearer","expires_at":4000000000}"#, 0o600).unwrap();
    }
    verify_with(staging, region, email, 1000, |_, _| {
        Ok((
            200,
            aterm_json::to_vec(
                &aterm_json::json!({"email": email, "user_id": "fixture-full-user-id"}),
            )
            .unwrap(),
        ))
    })
    .unwrap()
    .unwrap()
}

#[cfg(all(test, unix))]
#[path = "cli_shared_kimi_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
pub(in crate::harness) use refresh::rejection_fixture;
