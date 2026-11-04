// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unpublished Kimi completions, scoped to an admitted parent and Google binding.

use super::super::cli::shared_provider::{
    KimiNativeError, expire_kimi_pending, kimi_credential_path, kimi_pending_expiry,
    kimi_pending_files, verify_native_kimi_classified, verify_or_refresh_kimi_with_rejection,
};
use super::*;
use aterm_secrets::SecretBytes;

#[cfg(test)]
#[path = "google_federation_recovery_tests.rs"]
mod tests;

const PREFIX: &[u8] = b"aterm.kimi-pending.v1\0";
const REPLACEMENT_PREFIX: &[u8] = b"aterm.kimi-pending.v2\0";
const REJECTED_PREFIX: &[u8] = b"aterm.kimi-refresh-rejected.v1\0";
const FILE_MAX: usize = 2 * 1024 * 1024;
const COOKIES_MAX: usize = super::super::google_browser_store::MAX_BYTES;
const MAX: usize = FILE_MAX * 2 + COOKIES_MAX + 8192;
const UNAVAILABLE: &str = "Protected Kimi completion is unavailable";

struct Candidate {
    config: SecretBytes,
    credential: SecretBytes,
    cookies: SecretBytes,
    previous: Option<[u8; 32]>,
    predecessor: Option<[u8; 32]>,
}

struct RefreshGuard(FederationScope);
impl std::fmt::Debug for RefreshGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KimiRefreshGuard(opaque)")
    }
}
impl aterm_http::Guard for RefreshGuard {
    fn is_authorized(&self) -> bool {
        self.0
            .check_attempt()
            .and_then(|()| self.0.current())
            .is_ok()
    }
}

pub(in crate::harness) struct Journal {
    scope: FederationScope,
    key: login_drive::RecoveryKey,
    storage: super::super::pending_native_store::Store,
    candidate: Option<Candidate>,
    files: Option<([u8; 32], [u8; 32])>,
    resumed: bool,
    rejection_observed: bool,
    fallback_cookies: Option<(SecretBytes, Option<[u8; 32]>)>,
    _native_claim: std::sync::Arc<super::super::browser_window::ProfileLock>,
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    aterm_digest::Sha256::digest(bytes)
}

fn scope_digest(scope: &FederationScope) -> Result<[u8; 32], String> {
    let root = scope
        .attempt
        .state
        .canonicalize()
        .map_err(|_| UNAVAILABLE)?;
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm.kimi-pending-scope.v1\0");
    for field in [
        root.to_str().ok_or(UNAVAILABLE)?,
        scope.provider.as_str(),
        scope.attempt.row.label.as_str(),
        scope.expected.as_str(),
    ] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    hash.update(scope.revision);
    if scope
        .attempt
        .admission
        .as_ref()
        .is_some_and(login_drive::RecoveryAdmission::is_shared_root)
    {
        hash.update(b"\0shared-root-admission.v1\0");
    }
    Ok(hash.finalize())
}

fn field<'a>(rest: &mut &'a [u8], max: usize) -> Result<&'a [u8], String> {
    let len = u32::from_be_bytes(
        rest.get(..4)
            .ok_or(UNAVAILABLE)?
            .try_into()
            .map_err(|_| UNAVAILABLE)?,
    ) as usize;
    if len > max {
        return Err(UNAVAILABLE.into());
    }
    *rest = &rest[4..];
    let bytes = rest.get(..len).ok_or(UNAVAILABLE)?;
    *rest = &rest[len..];
    Ok(bytes)
}

fn decode(
    bytes: &[u8],
    scope: [u8; 32],
    key: login_drive::RecoveryKey,
) -> Result<Option<Candidate>, String> {
    if let Some(rest) = bytes.strip_prefix(REJECTED_PREFIX) {
        if rest.len() != 64 {
            return Err(UNAVAILABLE.into());
        }
        // This checkpoint deliberately contains no recoverable grant. A later
        // admission cannot resurrect its predecessor either.
        return Ok(None);
    }
    let replacement = bytes.starts_with(REPLACEMENT_PREFIX);
    let mut rest = bytes
        .strip_prefix(if replacement {
            REPLACEMENT_PREFIX
        } else {
            PREFIX
        })
        .ok_or(UNAVAILABLE)?;
    let saved_scope = rest.get(..32).ok_or(UNAVAILABLE)?;
    rest = &rest[32..];
    let saved_key = rest.get(..32).ok_or(UNAVAILABLE)?;
    rest = &rest[32..];
    let previous = match rest.first().copied().ok_or(UNAVAILABLE)? {
        0 => {
            rest = &rest[1..];
            None
        }
        1 => {
            let hash = rest
                .get(1..33)
                .ok_or(UNAVAILABLE)?
                .try_into()
                .map_err(|_| UNAVAILABLE)?;
            rest = &rest[33..];
            Some(hash)
        }
        _ => return Err(UNAVAILABLE.into()),
    };
    let config = field(&mut rest, FILE_MAX)?;
    let credential = field(&mut rest, FILE_MAX)?;
    let cookies = field(&mut rest, COOKIES_MAX)?;
    let predecessor = if replacement {
        match rest.first().copied().ok_or(UNAVAILABLE)? {
            0 => {
                rest = &rest[1..];
                None
            }
            1 => {
                let hash = rest
                    .get(1..33)
                    .ok_or(UNAVAILABLE)?
                    .try_into()
                    .map_err(|_| UNAVAILABLE)?;
                rest = &rest[33..];
                Some(hash)
            }
            _ => return Err(UNAVAILABLE.into()),
        }
    } else {
        None
    };
    if !rest.is_empty() || config.is_empty() || credential.is_empty() {
        return Err(UNAVAILABLE.into());
    }
    if saved_scope != scope || saved_key != key.0 {
        return Ok(None);
    }
    Ok(Some(Candidate {
        config: SecretBytes::from(config),
        credential: SecretBytes::from(credential),
        cookies: SecretBytes::from(cookies),
        previous,
        predecessor,
    }))
}

impl Journal {
    pub(super) fn native_claim(&self) -> std::sync::Arc<super::super::browser_window::ProfileLock> {
        self._native_claim.clone()
    }
    fn checkpoint_current(
        &mut self,
        request: &login_drive::Request<'_>,
        home: &Path,
    ) -> Result<(), String> {
        if self.rejection_observed {
            return self.checkpoint_rejected(request);
        }
        self.check_context(request)?;
        if let Ok((config, credential)) = kimi_pending_files(request.staging, self.scope.region)
            && self.files != Some((digest(config.as_slice()), digest(credential.as_slice())))
        {
            self.capture(request, None, home)?;
        }
        Ok(())
    }
    fn checkpoint_rejected(&mut self, request: &login_drive::Request<'_>) -> Result<(), String> {
        self.check_context(request)?;
        // Once the provider rejected this attempt, even a failed storage write
        // must not turn a later generic error into a cached usable seed.
        self.rejection_observed = true;
        let mut rejected = REJECTED_PREFIX.to_vec();
        rejected.extend_from_slice(&scope_digest(&self.scope)?);
        rejected.extend_from_slice(&self.key.0);
        self.storage.save(&rejected).map_err(str::to_owned)
    }

    fn renew_recorded(
        &mut self,
        request: &login_drive::Request<'_>,
        scope: &FederationScope,
        now: i64,
        deadline: Instant,
    ) -> Result<Option<KimiProof>, KimiNativeError> {
        let journal = std::cell::RefCell::new(self);
        verify_or_refresh_kimi_with_rejection(
            request.staging,
            scope.region,
            &scope.expected,
            now,
            deadline,
            std::sync::Arc::new(RefreshGuard(scope.clone())),
            &|| journal.borrow_mut().checkpoint_rejected(request),
        )
    }

    /// Verify or renew current retained files without adopting an older
    /// completion's cookies. A rotated grant survives a later identity outage.
    pub(in crate::harness) fn observe_current(
        &mut self,
        request: &login_drive::Request<'_>,
        home: &Path,
        deadline: Instant,
    ) -> Result<Option<KimiProof>, super::super::cli::shared_provider::ObservationError> {
        use super::super::cli::shared_provider::{ObservationError, kimi_observation_error};
        self.check_context(request)
            .map_err(ObservationError::Unavailable)?;
        let now = aterm_time::unix_secs()
            .and_then(|n| i64::try_from(n).ok())
            .ok_or_else(|| {
                ObservationError::Unavailable("Kimi account clock is unavailable".into())
            })?;
        let scope = self.scope.clone();
        let result = self.renew_recorded(request, &scope, now, deadline);
        self.check_context(request)
            .map_err(ObservationError::Unavailable)?;
        if matches!(
            &result,
            Err(KimiNativeError::RefreshRejected | KimiNativeError::PreviouslyRejected)
        ) {
            self.checkpoint_rejected(request)
                .map_err(ObservationError::Unavailable)?;
        } else {
            self.checkpoint_current(request, home)
                .map_err(ObservationError::Unavailable)?;
        }
        result.map_err(kimi_observation_error)
    }
    pub(in crate::harness) fn open(
        request: &login_drive::Request<'_>,
        home: &Path,
    ) -> Result<Option<Self>, String> {
        let Some(key) = request.recovery else {
            return Ok(None);
        };
        if request.cli != Cli::Kimi {
            return Ok(None);
        }
        let attempt = NativeAttempt::capture(request);
        let Some(bytes) = read_registry(&attempt.state).map_err(str::to_owned)? else {
            return Ok(None);
        };
        let registry = parse_registry(&bytes).ok_or("Google identity registry is invalid")?;
        let provider = provider_key(request.cli.id(), request.hosts)
            .ok_or("Kimi Google realm is unavailable")?;
        let Some((identity, binding)) = registry.resolve(provider, &request.row.label) else {
            return Ok(None);
        };
        let scope = FederationScope {
            attempt,
            provider: provider.into(),
            identity: identity.clone(),
            expected: binding.expected_email().into(),
            revision: registry
                .binding_revision(provider, &request.row.label)
                .ok_or(UNAVAILABLE)?,
            region: request.row.vendor.region.ok_or(UNAVAILABLE)?,
        };
        scope.check_request(request)?;
        let mut account = aterm_digest::Sha256::new();
        account.update(b"aterm.kimi-pending-account.v1\0");
        account.update(provider.as_bytes());
        account.update(b"\0");
        account.update(request.row.label.as_bytes());
        let directory = scope
            .attempt
            .state
            .join(
                if scope
                    .attempt
                    .admission
                    .as_ref()
                    .is_some_and(login_drive::RecoveryAdmission::is_shared_root)
                {
                    "pending-native-kimi-google-shared"
                } else {
                    "pending-native-kimi-google"
                },
            )
            .join(aterm_codec::hex::encode_lower(&account.finalize()));
        let (storage, plaintext) =
            super::super::pending_native_store::Store::open(&directory, home, MAX)
                .map_err(str::to_owned)?;
        let native_claim = super::super::browser_window::ProfileLock::claim(request.staging)
            .map_err(|_| "Kimi native staging is held or unavailable")?;
        let candidate = plaintext
            .as_ref()
            .map(|bytes| decode(bytes.as_slice(), scope_digest(&scope)?, key))
            .transpose()?
            .flatten();
        let files = candidate
            .as_ref()
            .map(|c| (digest(c.config.as_slice()), digest(c.credential.as_slice())));
        Ok(Some(Self {
            scope,
            key,
            storage,
            candidate,
            files,
            resumed: false,
            rejection_observed: false,
            fallback_cookies: None,
            _native_claim: std::sync::Arc::new(native_claim),
        }))
    }

    pub(in crate::harness) fn check_context(
        &self,
        request: &login_drive::Request<'_>,
    ) -> Result<(), String> {
        self.scope.check_request(request)?;
        self.storage.unchanged().map_err(str::to_owned)
    }

    pub(in crate::harness) fn check(
        &self,
        request: &login_drive::Request<'_>,
    ) -> Result<(), String> {
        self.check_attempt(&NativeAttempt::capture(request))
    }

    pub(in crate::harness) fn check_attempt(&self, attempt: &NativeAttempt) -> Result<(), String> {
        if attempt != &self.scope.attempt {
            return Err("Kimi recovery attempt changed".into());
        }
        self.scope.check_attempt()?;
        self.scope.current()?;
        self.storage.unchanged().map_err(str::to_owned)?;
        let (config, credential) = kimi_pending_files(&attempt.staging, self.scope.region)?;
        if self.files != Some((digest(config.as_slice()), digest(credential.as_slice()))) {
            return Err("Kimi native files differ from their pending completion".into());
        }
        Ok(())
    }

    pub(super) fn reject(&self, request: &login_drive::Request<'_>) -> Result<(), String> {
        self.check(request)?;
        self.storage.retire().map_err(str::to_owned)
    }

    pub(in crate::harness) fn capture(
        &mut self,
        request: &login_drive::Request<'_>,
        pending: Option<&PendingFederation>,
        home: &Path,
    ) -> Result<(), String> {
        if let Some(pending) = pending {
            pending.scope.check_request(request)?;
            if scope_digest(&pending.scope)? != scope_digest(&self.scope)? {
                return Err("Kimi pending Google scope differs from its native completion".into());
            }
        }
        if self.rejection_observed {
            return self.checkpoint_rejected(request);
        }
        if self.resumed {
            return self.check(request);
        }
        self.scope.check_request(request)?;
        let (config, credential) = kimi_pending_files(request.staging, self.scope.region)?;
        let (cookies, previous) = pending
            .and_then(|p| p.session.as_ref())
            .map(|s| s.recovery_cookies(&self.scope.identity, home))
            .transpose()
            .map_err(|_| "Kimi pending Google identity changed")?
            .or_else(|| {
                self.fallback_cookies
                    .as_ref()
                    .map(|(cookies, previous)| (cookies.as_slice(), *previous))
            })
            .unwrap_or_default();
        if config.is_empty()
            || credential.is_empty()
            || config.len() > FILE_MAX
            || credential.len() > FILE_MAX
            || cookies.len() > COOKIES_MAX
        {
            return Err(UNAVAILABLE.into());
        }
        // Native token storage fsyncs its replacement file before rename, but
        // does not fsync the directory. That rename may be lost at a crash even
        // when our later encrypted checkpoint survived. Bind repair to the exact
        // prior credential and the identical pinned config, never any other file.
        let predecessor = request
            .admission
            .filter(|a| a.is_shared_root())
            .and_then(|_| {
                self.files
                    .filter(|(config_hash, credential_hash)| {
                        *config_hash == digest(config.as_slice())
                            && *credential_hash != digest(credential.as_slice())
                    })
                    .map(|(_, credential_hash)| credential_hash)
            });
        let mut bytes = REPLACEMENT_PREFIX.to_vec();
        bytes.extend_from_slice(&scope_digest(&self.scope)?);
        bytes.extend_from_slice(&self.key.0);
        if let Some(hash) = previous {
            bytes.push(1);
            bytes.extend_from_slice(&hash);
        } else {
            bytes.push(0);
        }
        for field in [config.as_slice(), credential.as_slice(), cookies] {
            bytes.extend_from_slice(&(field.len() as u32).to_be_bytes());
            bytes.extend_from_slice(field);
        }
        if let Some(hash) = predecessor {
            bytes.push(1);
            bytes.extend_from_slice(&hash);
        } else {
            bytes.push(0);
        }
        let bytes = SecretBytes::from(bytes);
        self.storage.save(bytes.as_slice()).map_err(str::to_owned)?;
        self.files = Some((digest(config.as_slice()), digest(credential.as_slice())));
        self.check(request)
    }

    /// Observe the exact retained root staging without invoking a consumed
    /// native action. Restore absent files, or replace the exact journaled
    /// predecessor credential while retaining the identical pinned config.
    pub(in crate::harness) fn observe_existing(
        &mut self,
        request: &login_drive::Request<'_>,
        home: &Path,
        deadline: Instant,
    ) -> Result<Option<KimiProof>, super::super::cli::shared_provider::ObservationError> {
        self.observe_existing_with(request, home, deadline, |journal, scope, now| {
            journal.renew_recorded(request, scope, now, deadline)
        })
    }

    #[cfg(test)]
    fn observe_existing_using(
        &mut self,
        request: &login_drive::Request<'_>,
        home: &Path,
        deadline: Instant,
        read: impl FnOnce(&FederationScope, i64) -> Result<Option<KimiProof>, KimiNativeError>,
    ) -> Result<Option<KimiProof>, super::super::cli::shared_provider::ObservationError> {
        self.observe_existing_with(request, home, deadline, |_, scope, now| read(scope, now))
    }

    fn observe_existing_with(
        &mut self,
        request: &login_drive::Request<'_>,
        home: &Path,
        deadline: Instant,
        read: impl FnOnce(
            &mut Self,
            &FederationScope,
            i64,
        ) -> Result<Option<KimiProof>, KimiNativeError>,
    ) -> Result<Option<KimiProof>, super::super::cli::shared_provider::ObservationError> {
        use super::super::cli::shared_provider::{ObservationError, kimi_observation_error};
        if request.admission.is_none() {
            return Err(ObservationError::Unavailable(
                "Kimi retained observation has no live admission".into(),
            ));
        }
        self.scope
            .check_request(request)
            .map_err(ObservationError::Unavailable)?;
        self.storage
            .unchanged()
            .map_err(|why| ObservationError::Unavailable(why.into()))?;
        let Some(candidate) = self.candidate.take() else {
            return Ok(None);
        };
        if Instant::now() >= deadline {
            return Err(ObservationError::Unavailable(
                "Kimi recovery deadline expired".into(),
            ));
        }
        let files = [
            (
                request.staging.join("config.toml"),
                candidate.config.as_slice(),
            ),
            (
                kimi_credential_path(request.staging, self.scope.region),
                candidate.credential.as_slice(),
            ),
        ];
        let mut missing = Vec::new();
        let mut replacements = Vec::new();
        for (path, expected) in &files {
            match aterm_secrets::fsx::read_private(path, FILE_MAX)
                .map_err(|_| ObservationError::Unavailable(UNAVAILABLE.into()))?
            {
                Some(bytes) if bytes.as_slice() != *expected => {
                    if *path == kimi_credential_path(request.staging, self.scope.region)
                        && candidate.predecessor == Some(digest(bytes.as_slice()))
                    {
                        replacements.push((path, *expected, digest(bytes.as_slice())));
                        continue;
                    }
                    // A new native completion needs its own proof. Its predecessor's
                    // unpublished cookies stay quarantined; no existing file is replaced.
                    return Ok(None);
                }
                Some(_) => {}
                None => missing.push((path, *expected)),
            }
        }
        for (path, bytes, predecessor) in replacements {
            self.check_context(request)
                .map_err(ObservationError::Unavailable)?;
            if Instant::now() >= deadline {
                return Err(ObservationError::Unavailable(
                    "Kimi recovery deadline expired".into(),
                ));
            }
            let current = aterm_secrets::fsx::read_private(path, FILE_MAX)
                .map_err(|_| ObservationError::Unavailable(UNAVAILABLE.into()))?;
            if current.as_ref().map(|bytes| digest(bytes.as_slice())) != Some(predecessor) {
                return Err(ObservationError::Unavailable(
                    "Kimi predecessor changed during recovery".into(),
                ));
            }
            aterm_secrets::fsx::write_atomic(path, bytes, 0o600)
                .map_err(|_| ObservationError::Unavailable(UNAVAILABLE.into()))?;
        }
        for (path, bytes) in missing {
            self.check_context(request)
                .map_err(ObservationError::Unavailable)?;
            if Instant::now() >= deadline {
                return Err(ObservationError::Unavailable(
                    "Kimi recovery deadline expired".into(),
                ));
            }
            aterm_secrets::fsx::ensure_private_dir(
                path.parent()
                    .ok_or_else(|| ObservationError::Unavailable(UNAVAILABLE.into()))?,
            )
            .map_err(|_| ObservationError::Unavailable(UNAVAILABLE.into()))?;
            if !aterm_secrets::fsx::write_new(path, bytes, 0o600)
                .map_err(|_| ObservationError::Unavailable(UNAVAILABLE.into()))?
            {
                return Err(ObservationError::Unavailable(
                    "Kimi retained file was claimed by another writer".into(),
                ));
            }
        }
        self.check(request).map_err(ObservationError::Unavailable)?;
        if Instant::now() >= deadline {
            return Err(ObservationError::Unavailable(
                "Kimi recovery deadline expired".into(),
            ));
        }
        let now = aterm_time::unix_secs()
            .and_then(|n| i64::try_from(n).ok())
            .ok_or_else(|| {
                ObservationError::Unavailable("Kimi account clock is unavailable".into())
            })?;
        let scope = self.scope.clone();
        let native = match read(self, &scope, now) {
            Err(KimiNativeError::Temporary) => {
                self.fallback_cookies = Some((candidate.cookies, candidate.previous));
                self.checkpoint_current(request, home)
                    .map_err(ObservationError::Unavailable)?;
                return Err(kimi_observation_error(KimiNativeError::Temporary));
            }
            Err(
                error @ (KimiNativeError::RefreshRejected | KimiNativeError::PreviouslyRejected),
            ) => {
                self.checkpoint_rejected(request)
                    .map_err(ObservationError::Unavailable)?;
                return Err(kimi_observation_error(error));
            }
            result => result.map_err(kimi_observation_error)?,
        }
        .ok_or_else(|| {
            ObservationError::Unavailable("Kimi retained credential was not verified".into())
        })?;
        native
            .check_native(request.staging, self.scope.region, &self.scope.expected)
            .map_err(ObservationError::Unavailable)?;
        self.check_context(request)
            .map_err(ObservationError::Unavailable)?;
        let session = if candidate.cookies.is_empty() {
            None
        } else {
            Some(
                PendingSession::recover(
                    &self.scope.identity,
                    &self.scope.attempt.state,
                    home,
                    candidate.cookies,
                    candidate.previous,
                )
                .map_err(|_| {
                    ObservationError::Unavailable(
                        "Kimi pending Google session is unavailable".into(),
                    )
                })?,
            )
        };
        let pending = PendingFederation {
            scope: self.scope.clone(),
            session,
            native: Some(native),
        };
        let (config, credential) = kimi_pending_files(request.staging, self.scope.region)
            .map_err(ObservationError::Unavailable)?;
        if self.files != Some((digest(config.as_slice()), digest(credential.as_slice()))) {
            self.capture(request, Some(&pending), home)
                .map_err(ObservationError::Unavailable)?;
        }
        pending
            .commit_recovered(request, self, deadline)
            .map(Some)
            .map_err(ObservationError::Unavailable)
    }

    pub(in crate::harness) fn has_native_seed(&self) -> bool {
        self.files.is_some() && !self.resumed
    }

    pub(in crate::harness) fn resume(
        &mut self,
        request: &login_drive::Request<'_>,
        home: &Path,
        deadline: Instant,
    ) -> Result<Option<PendingFederation>, String> {
        self.resume_with(request, home, deadline, |journal, scope, now| {
            if scope
                .attempt
                .admission
                .as_ref()
                .is_some_and(|a| !a.is_shared_root())
            {
                return journal.renew_recorded(request, scope, now, deadline);
            }
            verify_native_kimi_classified(
                request.staging,
                scope.region,
                &scope.expected,
                now,
                deadline,
            )
        })
    }

    #[cfg(test)]
    fn resume_using(
        &mut self,
        request: &login_drive::Request<'_>,
        home: &Path,
        deadline: Instant,
        read: impl FnOnce(&FederationScope, i64) -> Result<Option<KimiProof>, KimiNativeError>,
    ) -> Result<Option<PendingFederation>, String> {
        self.resume_with(request, home, deadline, |_, scope, now| read(scope, now))
    }

    fn resume_with(
        &mut self,
        request: &login_drive::Request<'_>,
        home: &Path,
        deadline: Instant,
        read: impl FnOnce(
            &mut Self,
            &FederationScope,
            i64,
        ) -> Result<Option<KimiProof>, KimiNativeError>,
    ) -> Result<Option<PendingFederation>, String> {
        self.scope.check_request(request)?;
        self.storage.unchanged().map_err(str::to_owned)?;
        if request.admission.is_some_and(|a| !a.is_shared_root()) {
            let native = self
                .observe_existing_with(request, home, deadline, read)
                .map_err(|error| error.to_string())?;
            self.resumed = native.is_some();
            return Ok(native.map(|native| PendingFederation {
                scope: self.scope.clone(),
                session: None,
                native: Some(native),
            }));
        }
        let Some(candidate) = self.candidate.take() else {
            return Ok(None);
        };
        if Instant::now() >= deadline {
            return Err("Kimi recovery deadline expired".into());
        }
        let credential_path = kimi_credential_path(request.staging, self.scope.region);
        for path in [request.staging.join("config.toml"), credential_path.clone()] {
            if aterm_secrets::fsx::read_private(&path, FILE_MAX)
                .map_err(|_| UNAVAILABLE)?
                .is_some()
            {
                return Err("Kimi recovery refuses an existing staged native file".into());
            }
        }
        aterm_secrets::fsx::ensure_private_dir(credential_path.parent().ok_or(UNAVAILABLE)?)
            .map_err(|_| UNAVAILABLE)?;
        aterm_secrets::fsx::write_atomic(
            &request.staging.join("config.toml"),
            candidate.config.as_slice(),
            0o600,
        )
        .map_err(|_| UNAVAILABLE)?;
        aterm_secrets::fsx::write_atomic(&credential_path, candidate.credential.as_slice(), 0o600)
            .map_err(|_| UNAVAILABLE)?;
        self.check(request)?;
        let now = aterm_time::unix_secs()
            .and_then(|n| i64::try_from(n).ok())
            .ok_or("Kimi account clock is unavailable")?;
        if kimi_pending_expiry(candidate.credential.as_slice())? <= now {
            self.fallback_cookies = Some((candidate.cookies, candidate.previous));
            return Ok(None);
        }
        let scope = self.scope.clone();
        let result = read(self, &scope, now);
        self.check(request)?;
        if Instant::now() >= deadline {
            return Err("Kimi recovery deadline expired".into());
        }
        let native = match result {
            Ok(Some(native)) => native,
            Ok(None) => return Err("Kimi pending credential was not verified".into()),
            Err(KimiNativeError::Rejected) => {
                expire_kimi_pending(request.staging, self.scope.region)?;
                self.fallback_cookies = Some((candidate.cookies, candidate.previous));
                return Ok(None);
            }
            Err(KimiNativeError::Refused(why)) => {
                self.reject(request)?;
                return Err(why);
            }
            Err(error) => return Err(error.to_string()),
        };
        native.check_native(request.staging, self.scope.region, &self.scope.expected)?;
        let session = if candidate.cookies.is_empty() {
            None
        } else {
            Some(
                PendingSession::recover(
                    &self.scope.identity,
                    &self.scope.attempt.state,
                    home,
                    candidate.cookies,
                    candidate.previous,
                )
                .map_err(|_| "Kimi pending Google session is unavailable")?,
            )
        };
        self.resumed = true;
        Ok(Some(PendingFederation {
            scope: self.scope.clone(),
            session,
            native: Some(native),
        }))
    }

    pub(in crate::harness) fn pending(&mut self, home: &Path) -> Result<PendingFederation, String> {
        let session = match self.fallback_cookies.take() {
            Some((cookies, previous)) if !cookies.is_empty() => Some(
                PendingSession::recover(
                    &self.scope.identity,
                    &self.scope.attempt.state,
                    home,
                    cookies,
                    previous,
                )
                .map_err(|_| "Kimi pending Google session is unavailable")?,
            ),
            _ => None,
        };
        Ok(PendingFederation {
            scope: self.scope.clone(),
            session,
            native: None,
        })
    }
}
