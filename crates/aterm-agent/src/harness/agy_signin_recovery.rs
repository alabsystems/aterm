// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Encrypted, unpublished native completions. These records never authorize a browser
//! restore or pool publication; a resumed grant still needs canonical Google proof.

use super::*;

const PREFIX: &[u8] = b"aterm.agy-pending.v2\0";
const TOKEN_ONLY_PREFIX: &[u8] = b"aterm.agy-pending.v1\0";
const COOKIES_MAX: usize = super::super::google_browser_store::MAX_BYTES;
const MAX: usize = LIMIT + COOKIES_MAX + 8192;
const UNAVAILABLE: &str = "Protected pending native Google login is unavailable";

#[cfg(test)]
#[path = "agy_signin_recovery_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "agy_signin_direct_recovery_tests.rs"]
mod direct_tests;

#[path = "agy_signin_direct_refresh.rs"]
mod direct_refresh;

struct Candidate {
    token: SecretBytes,
    cookies: SecretBytes,
    previous: Option<[u8; 32]>,
}

pub(super) struct Journal {
    storage: super::super::pending_native_store::Store,
    scope: [u8; 32],
    key: RecoveryKey,
    native_digest: Option<[u8; 32]>,
    staged: bool,
}

impl Journal {
    fn open(
        state: &Path,
        home: &Path,
        account: &str,
        scope: [u8; 32],
        key: RecoveryKey,
        folder: &str,
    ) -> Result<(Self, Option<Candidate>), &'static str> {
        let mut hash = aterm_digest::Sha256::new();
        hash.update(b"aterm.agy-pending-account.v1\0");
        hash.update(account.as_bytes());
        let directory = state
            .join(folder)
            .join(aterm_codec::hex::encode_lower(&hash.finalize()));
        let (storage, plaintext) =
            super::super::pending_native_store::Store::open(&directory, home, MAX)?;
        let candidate = plaintext
            .as_ref()
            .map(|bytes| decode(bytes.as_slice(), scope, key))
            .transpose()?
            .flatten();
        Ok((
            Self {
                storage,
                scope,
                key,
                native_digest: candidate
                    .as_ref()
                    .map(|candidate| grant_digest(candidate.token.as_slice())),
                staged: folder != "pending-native-google-direct",
            },
            candidate,
        ))
    }

    pub(super) fn unchanged(&self) -> Result<(), &'static str> {
        self.storage.unchanged()
    }

    fn save(&mut self, candidate: &Candidate) -> Result<(), &'static str> {
        let plaintext = encode(candidate, self.scope, self.key)?;
        self.storage.save(plaintext.as_slice())?;
        self.native_digest = Some(grant_digest(candidate.token.as_slice()));
        Ok(())
    }

    pub(super) fn accepts_native(&self, digest: [u8; 32]) -> Result<(), &'static str> {
        if self.native_digest != Some(digest) {
            return Err("Native Google credential differs from its pending completion");
        }
        Ok(())
    }

    fn capture(
        &mut self,
        result: &UnverifiedGoogleGrant,
        identity: &super::super::google_identity::Identity,
        home: &Path,
    ) -> Result<(), GoogleLoginError> {
        let token = grant_bytes(result.path()).map_err(GoogleLoginError::Recovery)?;
        let (cookies, previous) = result
            .native
            .session
            .as_ref()
            .map(|pending| pending.recovery_cookies(identity, home))
            .transpose()
            .map_err(|_| GoogleLoginError::Recovery("Pending Google cookie identity changed"))?
            .unwrap_or_default();
        self.save(&Candidate {
            token,
            cookies: SecretBytes::from(cookies),
            previous,
        })
        .map_err(GoogleLoginError::Recovery)
    }

    pub(super) fn supports_staged_refresh(&self) -> bool {
        self.staged
    }

    pub(super) fn retire(&self) -> Result<(), &'static str> {
        self.storage.retire()
    }
}

#[cfg(test)]
pub(super) fn publication_fixture(
    req: &super::super::login_drive::Request<'_>,
    key: RecoveryKey,
    unavailable: bool,
) -> (Result<VerifiedGoogleGrant, String>, bool) {
    let state = req.store.dir.parent().unwrap();
    let home = req.store.user_home().unwrap();
    let program = std::env::current_exe().unwrap();
    let fresh = std::cell::Cell::new(false);
    let deadline = Instant::now() + Duration::from_secs(60);
    let result = run_with(
        &program,
        state,
        home,
        &req.row.label,
        StagedGoogleLogin {
            profile: req.staging,
            previous: None,
            expected_email: req.row.email.as_deref().unwrap(),
            recovery: Some(key),
        },
        deadline,
        |program, state, _, account, staging, _| {
            fresh.set(true);
            let staging = staging.unwrap();
            let mut grant =
                super::publication_fixture(state, staging.profile, account, staging.expected_email)
                    .grant;
            grant.native.program = program.into();
            Ok(grant)
        },
    )
    .map_err(|error| error.to_string())
    .and_then(|grant| {
        grant
            .verify_with_retry(
                deadline,
                |_, expected, _| {
                    if unavailable {
                        return Err(super::super::google_profile::Error::Unavailable);
                    }
                    Ok(super::super::google_profile::GoogleProfile {
                        email: expected.into(),
                        principal: [1; 32],
                    })
                },
                |_| {},
                Instant::now,
            )
            .map_err(|error| error.to_string())
    });
    (result, fresh.get())
}

fn encode(
    candidate: &Candidate,
    scope: [u8; 32],
    key: RecoveryKey,
) -> Result<SecretBytes, &'static str> {
    if candidate.token.is_empty()
        || candidate.token.len() > LIMIT
        || candidate.cookies.len() > COOKIES_MAX
    {
        return Err(UNAVAILABLE);
    }
    native_access_token(candidate.token.as_slice())?;
    let mut bytes = PREFIX.to_vec();
    bytes.extend_from_slice(&scope);
    bytes.extend_from_slice(&key.0);
    bytes.push(u8::from(candidate.previous.is_some()));
    if let Some(previous) = candidate.previous {
        bytes.extend_from_slice(&previous);
    }
    for field in [candidate.token.as_slice(), candidate.cookies.as_slice()] {
        bytes.extend_from_slice(&(field.len() as u32).to_be_bytes());
        bytes.extend_from_slice(field);
    }
    Ok(SecretBytes::from(bytes))
}

fn field<'a>(rest: &mut &'a [u8], limit: usize) -> Result<&'a [u8], &'static str> {
    let length = u32::from_be_bytes(
        rest.get(..4)
            .ok_or(UNAVAILABLE)?
            .try_into()
            .map_err(|_| UNAVAILABLE)?,
    ) as usize;
    if length > limit {
        return Err(UNAVAILABLE);
    }
    *rest = &rest[4..];
    let value = rest.get(..length).ok_or(UNAVAILABLE)?;
    *rest = &rest[length..];
    Ok(value)
}

fn decode(
    bytes: &[u8],
    scope: [u8; 32],
    key: RecoveryKey,
) -> Result<Option<Candidate>, &'static str> {
    let (mut rest, token_only) = if let Some(rest) = bytes.strip_prefix(PREFIX) {
        (rest, false)
    } else {
        (
            bytes.strip_prefix(TOKEN_ONLY_PREFIX).ok_or(UNAVAILABLE)?,
            true,
        )
    };
    let saved_scope = rest.get(..32).ok_or(UNAVAILABLE)?;
    rest = &rest[32..];
    let saved_key = rest.get(..32).ok_or(UNAVAILABLE)?;
    rest = &rest[32..];
    let previous = if token_only {
        None
    } else {
        let tag = *rest.first().ok_or(UNAVAILABLE)?;
        rest = &rest[1..];
        match tag {
            0 => None,
            1 => {
                let value = rest
                    .get(..32)
                    .ok_or(UNAVAILABLE)?
                    .try_into()
                    .map_err(|_| UNAVAILABLE)?;
                rest = &rest[32..];
                Some(value)
            }
            _ => return Err(UNAVAILABLE),
        }
    };
    let token = field(&mut rest, LIMIT)?;
    let cookies = field(&mut rest, COOKIES_MAX)?;
    if !rest.is_empty() || token.is_empty() {
        return Err(UNAVAILABLE);
    }
    native_access_token(token)?;
    if saved_scope != scope || saved_key != key.0 {
        return Ok(None);
    }
    Ok(Some(Candidate {
        token: SecretBytes::from(token),
        // Version 1 did not retain browser snapshot custody. Preserve its native
        // grant for canonical revalidation, but never restore its unscoped cookies.
        cookies: SecretBytes::from(if token_only { &[][..] } else { cookies }),
        previous,
    }))
}

pub(super) fn run_scoped(
    program: &Path,
    state: &Path,
    home: &Path,
    account: &str,
    staging: StagedGoogleLogin<'_>,
    deadline: Instant,
    work: Option<super::super::account_work::Scope>,
) -> Result<UnverifiedGoogleGrant, GoogleLoginError> {
    run_with_journal(
        program,
        state,
        home,
        account,
        staging,
        deadline,
        |token, expected, wait| super::super::google_profile::read(token, expected, wait).map(drop),
        |program, state, home, account, staging, deadline| {
            run_google_at_scoped(
                program,
                state,
                home,
                account,
                staging,
                deadline,
                work.clone(),
            )
        },
        ("pending-native-google", work.clone()),
    )
}

pub(super) fn direct(
    program: &Path,
    state: &Path,
    home: &Path,
    account: &str,
    deadline: Instant,
) -> Result<UnverifiedGoogleGrant, GoogleLoginError> {
    direct_refresh::run_with(
        program,
        state,
        home,
        account,
        deadline,
        super::super::google_profile::read,
        run_google_at,
    )
}

fn check_completed(
    result: &UnverifiedGoogleGrant,
    program: &Path,
    state: &Path,
    account: &str,
    path: &Path,
    scope: [u8; 32],
) -> Result<(), GoogleLoginError> {
    // Recording this exact completed native effect is retirement metadata, not
    // admission of another provider/status/publication operation.
    result.check_binding_authority()?;
    if checkpoint_scope(
        state,
        result.revision.ok_or(GoogleLoginError::Configuration)?,
    ) != Some(scope)
        || current_checkpoint_scope(state, account, &result.expected_email) != Some(scope)
        || result.account != account
        || result.state != state
        || result.path() != path
        || result.native.program != program
    {
        return Err(GoogleLoginError::Configuration);
    }
    Ok(())
}

fn direct_with(
    program: &Path,
    state: &Path,
    home: &Path,
    account: &str,
    deadline: Instant,
    fresh: impl FnOnce(
        &Path,
        &Path,
        &Path,
        &str,
        Option<StagedGoogleLogin<'_>>,
        Instant,
    ) -> Result<UnverifiedGoogleGrant, GoogleLoginError>,
) -> Result<UnverifiedGoogleGrant, GoogleLoginError> {
    remaining(deadline).map_err(GoogleLoginError::Native)?;
    if !state.is_absolute() || !home.is_absolute() {
        return Err(GoogleLoginError::Configuration);
    }
    let registry = registry(state).ok_or(GoogleLoginError::Configuration)?;
    let (identity, binding) = registry
        .resolve("agy", account)
        .ok_or(GoogleLoginError::Configuration)?;
    let scope = current_checkpoint_scope(state, account, binding.expected_email())
        .ok_or(GoogleLoginError::Configuration)?;
    if !program.is_absolute() || !program.is_file() {
        return Err(GoogleLoginError::Native(Error::Profile));
    }
    let profile = native_profile(state, account, identity);
    let path = profile.join(".gemini/antigravity-cli/antigravity-oauth-token");
    let mut key = aterm_digest::Sha256::new();
    key.update(b"aterm.agy-direct-recovery.v1\0");
    let (mut journal, candidate) = Journal::open(
        state,
        home,
        account,
        scope,
        RecoveryKey(key.finalize()),
        "pending-native-google-direct",
    )
    .map_err(GoogleLoginError::Recovery)?;
    remaining(deadline).map_err(GoogleLoginError::Native)?;
    let restored = if let Some(candidate) = candidate {
        aterm_secrets::fsx::ensure_private_dir(&profile)
            .map_err(|_| GoogleLoginError::Native(Error::Profile))?;
        let claim = super::super::browser_window::ProfileLock::claim(&profile)
            .map_err(|_| GoogleLoginError::Native(Error::Profile))?;
        let current = aterm_secrets::fsx::read_private(&path, LIMIT)
            .map_err(|_| GoogleLoginError::Native(Error::Profile))?;
        if current.as_ref().is_some_and(|bytes| {
            grant_digest(bytes.as_slice()) == grant_digest(candidate.token.as_slice())
        }) {
            let session = if candidate.cookies.is_empty() {
                None
            } else {
                Some(
                    super::super::google_browser::PendingSession::recover(
                        identity,
                        state,
                        home,
                        candidate.cookies,
                        candidate.previous,
                    )
                    .map_err(|_| {
                        GoogleLoginError::Recovery("Pending Google cookie identity is unavailable")
                    })?,
                )
            };
            Some(UnverifiedGoogleGrant {
                native: UnverifiedGrant {
                    session,
                    path: path.clone(),
                    program: program.into(),
                    _claim: claim,
                },
                expected_email: binding.expected_email().into(),
                state: state.into(),
                account: account.into(),
                revision: registry.binding_revision("agy", account),
                recovery: None,
                work: None,
            })
        } else {
            // A direct profile may be in use or deliberately changed. Never copy an
            // older encrypted token over it, nor resurrect a deleted credential.
            drop(claim);
            None
        }
    } else {
        None
    };
    let resumed = restored.is_some();
    let mut result = match restored {
        Some(result) => result,
        None => fresh(program, state, home, account, None, deadline)?,
    };
    check_completed(&result, program, state, account, &path, scope)?;
    if resumed {
        journal.unchanged().map_err(GoogleLoginError::Recovery)?;
    } else {
        journal.capture(&result, identity, home)?;
    }
    result.recovery = Some(journal);
    remaining(deadline).map_err(GoogleLoginError::Native)?;
    Ok(result)
}

#[cfg(test)]
fn run_with(
    program: &Path,
    state: &Path,
    home: &Path,
    account: &str,
    staging: StagedGoogleLogin<'_>,
    deadline: Instant,
    fresh: impl FnOnce(
        &Path,
        &Path,
        &Path,
        &str,
        Option<StagedGoogleLogin<'_>>,
        Instant,
    ) -> Result<UnverifiedGoogleGrant, GoogleLoginError>,
) -> Result<UnverifiedGoogleGrant, GoogleLoginError> {
    run_with_checked(
        program,
        state,
        home,
        account,
        staging,
        deadline,
        |_, _, _| Ok(()),
        fresh,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the canonical reader and native runner are independent injected effects of one bounded recovery attempt"
)]
#[cfg(test)]
fn run_with_checked(
    program: &Path,
    state: &Path,
    home: &Path,
    account: &str,
    staging: StagedGoogleLogin<'_>,
    deadline: Instant,
    check: impl FnOnce(&str, &str, Duration) -> Result<(), super::super::google_profile::Error>,
    fresh: impl FnOnce(
        &Path,
        &Path,
        &Path,
        &str,
        Option<StagedGoogleLogin<'_>>,
        Instant,
    ) -> Result<UnverifiedGoogleGrant, GoogleLoginError>,
) -> Result<UnverifiedGoogleGrant, GoogleLoginError> {
    run_with_journal(
        program,
        state,
        home,
        account,
        staging,
        deadline,
        check,
        fresh,
        ("pending-native-google", None),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the journal domain and injected effects belong to one scoped recovery attempt"
)]
fn run_with_journal(
    program: &Path,
    state: &Path,
    home: &Path,
    account: &str,
    staging: StagedGoogleLogin<'_>,
    deadline: Instant,
    check: impl FnOnce(&str, &str, Duration) -> Result<(), super::super::google_profile::Error>,
    fresh: impl FnOnce(
        &Path,
        &Path,
        &Path,
        &str,
        Option<StagedGoogleLogin<'_>>,
        Instant,
    ) -> Result<UnverifiedGoogleGrant, GoogleLoginError>,
    journal_context: (&'static str, Option<super::super::account_work::Scope>),
) -> Result<UnverifiedGoogleGrant, GoogleLoginError> {
    let (folder, work) = journal_context;
    let work_current = || {
        if work
            .as_ref()
            .is_some_and(super::super::account_work::Scope::cancelled)
        {
            Err(GoogleLoginError::Configuration)
        } else {
            Ok(())
        }
    };
    work_current()?;
    let failure = || GoogleLoginError::Native(Error::Profile);
    remaining(deadline).map_err(GoogleLoginError::Native)?;
    if !program.is_absolute()
        || !program.is_file()
        || !state.is_absolute()
        || !home.is_absolute()
        || !staging.profile.is_absolute()
        || staging
            .previous
            .is_some_and(|old| !old.is_absolute() || old == staging.profile)
    {
        return Err(GoogleLoginError::Configuration);
    }
    let key = staging.recovery.ok_or(GoogleLoginError::Configuration)?;
    let scope = current_checkpoint_scope(state, account, staging.expected_email)
        .ok_or(GoogleLoginError::Configuration)?;
    let registry = registry(state).ok_or(GoogleLoginError::Configuration)?;
    let (identity, binding) = registry
        .resolve("agy", account)
        .ok_or(GoogleLoginError::Configuration)?;
    if staging.profile == native_profile(state, account, identity) {
        return Err(GoogleLoginError::Configuration);
    }
    let (mut journal, candidate) = Journal::open(state, home, account, scope, key, folder)
        .map_err(GoogleLoginError::Recovery)?;
    remaining(deadline).map_err(GoogleLoginError::Native)?;
    let expected_path = staging
        .profile
        .join(".gemini/antigravity-cli/antigravity-oauth-token");
    let mut resumed = candidate.is_some();
    let had_candidate = resumed;
    let mut seed = None;
    let candidate = if let Some(candidate) = candidate {
        journal.unchanged().map_err(GoogleLoginError::Recovery)?;
        let token =
            native_access_token(candidate.token.as_slice()).map_err(GoogleLoginError::Recovery)?;
        work_current()?;
        let verdict = check(
            token.as_str().ok_or_else(failure)?,
            binding.expected_email(),
            remaining(deadline)
                .map_err(GoogleLoginError::Native)?
                .min(Duration::from_secs(30)),
        );
        work_current()?;
        remaining(deadline).map_err(GoogleLoginError::Native)?;
        journal.unchanged().map_err(GoogleLoginError::Recovery)?;
        if current_checkpoint_scope(state, account, staging.expected_email) != Some(scope) {
            return Err(GoogleLoginError::Configuration);
        }
        match verdict {
            Ok(()) => Some(candidate),
            Err(super::super::google_profile::Error::CredentialRejected) => {
                // This private seed is never published. Keep the old encrypted record
                // until native renewal has produced a replacement completion.
                let renewal_seed = aterm_tempfile::tempdir().map_err(|_| failure())?;
                let _claim =
                    stage_profile(renewal_seed.path(), None).map_err(GoogleLoginError::Native)?;
                let path = renewal_seed
                    .path()
                    .join(".gemini/antigravity-cli/antigravity-oauth-token");
                aterm_secrets::fsx::ensure_private_dir(path.parent().ok_or_else(failure)?)
                    .map_err(|_| failure())?;
                aterm_secrets::fsx::write_atomic(&path, candidate.token.as_slice(), 0o600)
                    .map_err(|_| failure())?;
                prepare_rejected_staging(
                    renewal_seed.path(),
                    &StagedRecovery {
                        expected_email: staging.expected_email,
                        current: &|| {
                            work_current().is_ok()
                                && current_checkpoint_scope(state, account, staging.expected_email)
                                    == Some(scope)
                        },
                    },
                    deadline,
                    |_, _| Err(super::super::google_profile::Error::CredentialRejected),
                )
                .map_err(GoogleLoginError::Native)?;
                seed = Some(renewal_seed);
                resumed = false;
                None
            }
            Err(_) => {
                return Err(GoogleLoginError::Recovery(
                    "Pending native Google verification is unavailable or refused",
                ));
            }
        }
    } else {
        None
    };
    work_current()?;
    let mut result = if let Some(candidate) = candidate {
        let claim = stage_profile(staging.profile, None).map_err(GoogleLoginError::Native)?;
        let path = staging
            .profile
            .join(".gemini/antigravity-cli/antigravity-oauth-token");
        aterm_secrets::fsx::ensure_private_dir(
            path.parent().ok_or(GoogleLoginError::Configuration)?,
        )
        .map_err(|_| failure())?;
        aterm_secrets::fsx::write_atomic(&path, candidate.token.as_slice(), 0o600)
            .map_err(|_| failure())?;
        let session = if candidate.cookies.is_empty() {
            None
        } else {
            Some(
                super::super::google_browser::PendingSession::recover_scoped(
                    identity,
                    state,
                    home,
                    candidate.cookies,
                    candidate.previous,
                    work.clone(),
                )
                .map_err(|_| failure())?,
            )
        };
        UnverifiedGoogleGrant {
            native: UnverifiedGrant {
                session,
                path,
                program: program.into(),
                _claim: claim,
            },
            expected_email: binding.expected_email().into(),
            state: state.into(),
            account: account.into(),
            revision: registry.binding_revision("agy", account),
            recovery: None,
            work: work.clone(),
        }
    } else {
        journal.unchanged().map_err(GoogleLoginError::Recovery)?;
        if current_checkpoint_scope(state, account, staging.expected_email) != Some(scope) {
            return Err(GoogleLoginError::Configuration);
        }
        let previous = seed
            .as_ref()
            .and_then(|seed| {
                seed.path()
                    .join(".gemini/antigravity-cli/antigravity-oauth-token")
                    .is_file()
                    .then_some(seed.path())
            })
            .or_else(|| (!had_candidate).then_some(staging.previous).flatten());
        work_current()?;
        fresh(
            program,
            state,
            home,
            account,
            Some(StagedGoogleLogin {
                previous,
                ..staging
            }),
            deadline,
        )?
    };
    result.retain_work(work.clone());
    check_completed(&result, program, state, account, &expected_path, scope)?;
    if !resumed {
        // Preserve a completed native effect even if its authorization budget just
        // expired. The final deadline check forbids verification in this attempt;
        // a later admitted attempt can use this bounded local checkpoint.
        journal.capture(&result, identity, home)?;
    } else {
        journal.unchanged().map_err(GoogleLoginError::Recovery)?;
    }
    result.recovery = Some(journal);
    remaining(deadline).map_err(GoogleLoginError::Native)?;
    work_current()?;
    Ok(result)
}
