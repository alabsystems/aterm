// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! First-party implementation of Kimi's public refresh-token contract. Initial
//! authorization remains native; renewal never starts a device or browser flow.
//!
//! Protocol reference: MoonshotAI/kimi-code, commit
//! 2c73f3f50af4fa7f2540ca0d2e33b07a1408e3a7, packages/oauth/src/{oauth,constants}.ts.

use super::*;
use std::sync::Arc;
use std::time::Instant;

const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";

/// Hang detector for one required refresh response inside its original recovery deadline.
/// The HTTP clock stops in sleep; native lease and caller authority keep their own expiry.
const REFRESH_REQUEST: std::time::Duration = std::time::Duration::from_secs(60);

#[path = "cli_shared_kimi_completion.rs"]
mod completion;
pub use completion::RecoveryContext;

const REVOKED: &[u8] = br#"{"access_token":"","refresh_token":"","token_type":"Bearer","expires_at":0,"expires_in":0,"scope":""}"#;

#[derive(Clone, Copy)]
struct Target<'a> {
    region: Region,
    expected: &'a str,
    on_rejected: Option<&'a dyn Fn() -> Result<(), String>>,
}

#[path = "cli_shared_kimi_rejection.rs"]
mod rejection;
pub(in crate::harness) use rejection::Snapshot;

pub(in crate::harness) fn rejected_snapshot(
    home: &Path,
    region: Region,
    expected: &str,
) -> Result<Option<Snapshot>, NativeError> {
    rejection::snapshot(
        home,
        Target {
            region,
            expected,
            on_rejected: None,
        },
    )
}

#[path = "cli_shared_kimi_lock.rs"]
pub(super) mod native_lock;

#[cfg(test)]
#[path = "cli_shared_kimi_refresh_tests.rs"]
mod tests;

#[derive(Debug)]
pub(super) struct AdmissionGuard {
    pub(super) admission: login_drive::RecoveryAuthority,
    pub(super) recovery: RecoveryContext,
    pub(super) work: Option<crate::harness::account_work::Scope>,
}

/// Permission for new HTTP work is distinct from recording its exact completed response.
pub trait Authority: aterm_http::Guard {
    #[cfg(test)]
    fn native_lease_clock(&self) -> Option<Arc<std::sync::atomic::AtomicU64>> {
        None
    }
    /// The exact attempt, provider binding, role and file/lease authority remain current.
    /// Work cancellation or expiry alone does not withdraw custody of a completed response.
    fn receipt_current(&self) -> bool;
    /// Current original account/role authority, without permission to mutate a native lease.
    fn binding_current(&self) -> bool {
        self.receipt_current()
    }
    /// The actual admitted root/account custody; unavailable custody prevents dispatch.
    /// # Errors
    /// This authority has no admitted protected completion location.
    fn recovery_context(&self) -> Result<RecoveryContext, String> {
        Err("Protected Kimi completion is unavailable".into())
    }
    /// Durably arm the one admitted exchange before it can reach the provider.
    /// # Errors
    /// Protected completion cannot be recorded under the current parent authority.
    fn checkpoint_started(&self, _config: [u8; 32], _predecessor: [u8; 32]) -> Result<(), String> {
        Err("Protected Kimi completion is unavailable".into())
    }
    /// Retire only this invocation's armed marker when exchange was never invoked.
    /// # Errors
    /// Exact newly armed custody or its durable retirement cannot be established.
    fn checkpoint_unstarted(&self) -> Result<(), String> {
        Err("Protected Kimi completion is unavailable".into())
    }
    /// Consume this call's no-dispatch cleanup right before invoking its exchange.
    /// # Errors
    /// The freshly armed private marker is no longer current.
    fn checkpoint_dispatching(&self) -> Result<(), String> {
        Ok(())
    }
    /// Retain an exact completed response, without granting native credential publication.
    /// # Errors
    /// Protected completion cannot be recorded under the current parent authority.
    fn checkpoint_completed(
        &self,
        _status: u16,
        _received: i64,
        _body: &[u8],
    ) -> Result<(), String> {
        Err("Protected Kimi completion is unavailable".into())
    }
    /// Retire only a receipt whose canonical candidate is already durable and unchanged.
    /// # Errors
    /// Its canonical candidate or protected receipt cannot be established.
    fn checkpoint_applied(&self) -> Result<(), String> {
        Err("Protected Kimi completion is unavailable".into())
    }
}

#[cfg(test)]
impl Authority for aterm_http::AlwaysAuthorized {
    fn receipt_current(&self) -> bool {
        true
    }
    fn checkpoint_started(&self, _: [u8; 32], _: [u8; 32]) -> Result<(), String> {
        Ok(())
    }
    fn checkpoint_completed(&self, _: u16, _: i64, _: &[u8]) -> Result<(), String> {
        Ok(())
    }
    fn checkpoint_applied(&self) -> Result<(), String> {
        Ok(())
    }
}

impl aterm_http::Guard for AdmissionGuard {
    fn is_authorized(&self) -> bool {
        !self
            .work
            .as_ref()
            .is_some_and(crate::harness::account_work::Scope::cancelled)
            && self.admission.current(None).is_ok()
    }
}

impl Authority for AdmissionGuard {
    fn receipt_current(&self) -> bool {
        self.admission.current(None).is_ok()
    }
    fn recovery_context(&self) -> Result<RecoveryContext, String> {
        Ok(self.recovery.clone())
    }
}

fn authorized(
    guard: &dyn Authority,
    deadline: login_drive::Deadline<'_>,
) -> Result<(), NativeError> {
    if deadline.expired() || !guard.is_authorized() {
        return Err(NativeError::Temporary);
    }
    Ok(())
}

fn receipt_authorized(guard: &dyn Authority) -> Result<(), NativeError> {
    if guard.receipt_current() {
        Ok(())
    } else {
        Err(NativeError::Temporary)
    }
}

pub(in crate::harness) fn verify_or_refresh(
    staging: &Path,
    region: Region,
    expected: &str,
    now: i64,
    deadline: login_drive::Deadline<'_>,
    guard: Arc<dyn Authority>,
) -> Result<Option<KimiProof>, NativeError> {
    verify_target(
        staging,
        Target {
            region,
            expected,
            on_rejected: None,
        },
        now,
        deadline,
        guard,
        Arc::new(aterm_http::TcpConnector),
    )
}

pub(in crate::harness) fn verify_or_refresh_with_rejection(
    staging: &Path,
    region: Region,
    expected: &str,
    now: i64,
    deadline: login_drive::Deadline<'_>,
    guard: Arc<dyn Authority>,
    on_rejected: &dyn Fn() -> Result<(), String>,
) -> Result<Option<KimiProof>, NativeError> {
    verify_target(
        staging,
        Target {
            region,
            expected,
            on_rejected: Some(on_rejected),
        },
        now,
        deadline,
        guard,
        Arc::new(aterm_http::TcpConnector),
    )
}

fn verify_target(
    staging: &Path,
    target: Target<'_>,
    now: i64,
    deadline: login_drive::Deadline<'_>,
    guard: Arc<dyn Authority>,
    connector: Arc<dyn aterm_http::Connect>,
) -> Result<Option<KimiProof>, NativeError> {
    let Target {
        region, expected, ..
    } = target;
    coordinated_using(
        staging,
        target,
        now,
        deadline,
        guard,
        |now, guard| verify_native_guarded(staging, region, expected, now, deadline, Some(guard)),
        |url, form, guard| {
            token_response_started(url, form, guard, deadline, connector, Instant::now())
        },
    )
}

fn token_response_started(
    url: &str,
    form: &[u8],
    guard: Arc<dyn Authority>,
    deadline: login_drive::Deadline<'_>,
    connector: Arc<dyn aterm_http::Connect>,
    started: Instant,
) -> Result<(u16, aterm_secrets::SecretBytes), NativeError> {
    let now = Instant::now();
    let by = (started + REFRESH_REQUEST)
        .min(now + deadline.remaining().map_err(|_| NativeError::Temporary)?);
    let wait = by.saturating_duration_since(Instant::now());
    if wait.is_zero() {
        return Err(NativeError::Temporary);
    }
    let client =
        Client::new(Trust::PlatformVerifier, ProxyMode::Direct, wait).with_connector(connector);
    let reply = client
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .header(
            "User-Agent",
            &format!("aterm/{}", aterm_stamp::app_version()),
        )
        .guard(guard)
        .limit(REPLY_MAX)
        .backstop(by.saturating_duration_since(Instant::now()))
        .send(form)
        .map_err(|_| NativeError::Temporary)?;
    Ok((
        reply.status(),
        aterm_secrets::SecretBytes::from(reply.into_body()),
    ))
}

#[cfg(test)]
#[path = "cli_shared_kimi_refresh/native_wait_tests.rs"]
mod native_wait_tests;

fn coordinated_using(
    staging: &Path,
    target: Target<'_>,
    now: i64,
    deadline: login_drive::Deadline<'_>,
    guard: Arc<dyn Authority>,
    mut read: impl FnMut(i64, Arc<dyn Authority>) -> Result<Option<KimiProof>, NativeError>,
    exchange: impl FnOnce(
        &str,
        &[u8],
        Arc<dyn Authority>,
    ) -> Result<(u16, aterm_secrets::SecretBytes), NativeError>,
) -> Result<Option<KimiProof>, NativeError> {
    authorized(guard.as_ref(), deadline)?;
    let context = guard
        .recovery_context()
        .map_err(|_| NativeError::Temporary)?;
    if let Some(record) = completion::Record::existing(staging, target, context.clone())? {
        let prepared = Arc::new(completion::Prepared::new(record, guard.clone(), target));
        let lease = prepared
            .restore(target, deadline)?
            .ok_or(NativeError::Temporary)?;
        let guarded = lease.guard();
        let result = read(now, guarded.clone());
        authorized(guarded.as_ref(), deadline)?;
        // Consuming an exact prior response never dispatches another refresh in this call.
        return match result {
            Ok(Some(mut proof)) => {
                proof.native_lock = Some(lease);
                Ok(Some(proof))
            }
            _ => Err(NativeError::Temporary),
        };
    }
    let started = Instant::now();
    let initial = read(now, guard.clone());
    authorized(guard.as_ref(), deadline)?;
    if matches!(
        &initial,
        Err(NativeError::Expired | NativeError::Rejected | NativeError::Local(_))
    ) && rejection::matches(staging, target)?
    {
        authorized(guard.as_ref(), deadline)?;
        record_rejected(target)?;
        authorized(guard.as_ref(), deadline)?;
        return Err(NativeError::PreviouslyRejected);
    }
    match initial {
        Err(NativeError::Expired | NativeError::Rejected) => {}
        result => return result,
    }
    // Read-only native observations do not create a machine key or sealed store.
    // Preparation reclaims and rechecks custody before the first irreversible exchange.
    let record = completion::Record::open(staging, target, context)?;
    let prepared = Arc::new(completion::Prepared::new(record, guard, target));
    let restored = prepared.restore(target, deadline)?;
    let guard: Arc<dyn Authority> = prepared;
    if let Some(lease) = restored {
        let guarded = lease.guard();
        let result = read(now, guarded.clone());
        authorized(guarded.as_ref(), deadline)?;
        return match result {
            Ok(Some(mut proof)) => {
                proof.native_lock = Some(lease);
                Ok(Some(proof))
            }
            _ => Err(NativeError::Temporary),
        };
    }
    let native_lock =
        native_lock::NativeLock::acquire(staging, target.region, deadline, guard.clone())?;
    #[cfg(test)]
    native_lock.observe_clock(guard.native_lease_clock());
    let guard = native_lock.guard();
    let now = now
        .checked_add(
            i64::try_from(started.elapsed().as_secs()).map_err(|_| NativeError::Temporary)?,
        )
        .ok_or(NativeError::Temporary)?;
    // A native peer may have rotated the token since the first read. Re-read
    // under its own exclusion protocol before deciding whether to exchange.
    let mut proof = run_using(
        staging,
        target,
        now,
        deadline,
        guard.as_ref(),
        |now| read(now, guard.clone()),
        |url, form| exchange(url, form, guard.clone()),
    )?;
    if let Some(proof) = proof.as_mut() {
        proof.native_lock = Some(native_lock);
    }
    Ok(proof)
}

fn run_using(
    staging: &Path,
    target: Target<'_>,
    now: i64,
    deadline: login_drive::Deadline<'_>,
    guard: &dyn Authority,
    mut read: impl FnMut(i64) -> Result<Option<KimiProof>, NativeError>,
    exchange: impl FnOnce(&str, &[u8]) -> Result<(u16, aterm_secrets::SecretBytes), NativeError>,
) -> Result<Option<KimiProof>, NativeError> {
    authorized(guard, deadline)?;
    let started = Instant::now();
    let initial = read(now);
    authorized(guard, deadline)?;
    if matches!(
        &initial,
        Err(NativeError::Expired | NativeError::Rejected | NativeError::Local(_))
    ) && rejection::matches(staging, target)?
    {
        authorized(guard, deadline)?;
        record_rejected(target)?;
        authorized(guard, deadline)?;
        return Err(NativeError::PreviouslyRejected);
    }
    match initial {
        Err(NativeError::Expired | NativeError::Rejected) => {}
        result => {
            authorized(guard, deadline)?;
            return result;
        }
    }
    let renewal_now = now
        .checked_add(
            i64::try_from(started.elapsed().as_secs()).map_err(|_| NativeError::Temporary)?,
        )
        .ok_or(NativeError::Temporary)?;
    renew_using(staging, target, renewal_now, deadline, guard, exchange)?;
    authorized(guard, deadline)?;
    let now = now
        .checked_add(
            i64::try_from(started.elapsed().as_secs()).map_err(|_| NativeError::Temporary)?,
        )
        .ok_or(NativeError::Temporary)?;
    let result = read(now);
    authorized(guard, deadline)?;
    match result {
        // The refresh grant succeeded and is already durable. A fresh access
        // token's propagation or expiry is not proof that another login is needed.
        Err(NativeError::Rejected | NativeError::Expired) => Err(NativeError::Temporary),
        result => result,
    }
}

fn record_rejected(target: Target<'_>) -> Result<(), NativeError> {
    if let Some(record) = target.on_rejected {
        record().map_err(|_| NativeError::Temporary)?;
    }
    Ok(())
}

fn form(refresh: &str) -> Secret {
    let mut bytes =
        format!("client_id={CLIENT_ID}&grant_type=refresh_token&refresh_token=").into_bytes();
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in refresh.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            bytes.push(byte);
        } else {
            bytes.extend_from_slice(&[b'%', HEX[(byte >> 4) as usize], HEX[(byte & 15) as usize]]);
        }
    }
    Secret::from_vec(bytes)
}

fn renew_using(
    staging: &Path,
    target: Target<'_>,
    now: i64,
    deadline: login_drive::Deadline<'_>,
    guard: &dyn Authority,
    exchange: impl FnOnce(&str, &[u8]) -> Result<(u16, aterm_secrets::SecretBytes), NativeError>,
) -> Result<(), NativeError> {
    authorized(guard, deadline)?;
    if accounts::email_refusal(target.expected).is_some() {
        return Err(NativeError::from(
            "Kimi refresh has no valid expected identity",
        ));
    }
    let region = target.region;
    let started = Instant::now();
    check_directory(staging)?;
    check_directory(&staging.join("credentials"))?;
    let config = FileEvidence::read(staging.join("config.toml"), false)?
        .ok_or("Kimi staged config is absent")?;
    check_config(config.bytes.expose(), region)?;
    let path = credential_path(staging, region);
    let credential =
        FileEvidence::read(path.clone(), true)?.ok_or("Kimi staged credential is absent")?;
    pending_expiry(credential.bytes.expose())?;
    let token: Value = aterm_json::from_slice(credential.bytes.expose())
        .map_err(|_| NativeError::from("Kimi staged credential is invalid"))?;
    let request = token["refresh_token"]
        .as_str()
        .filter(|token| !token.is_empty() && token.bytes().all(|b| (0x21..=0x7e).contains(&b)))
        .map(form)
        .ok_or_else(|| NativeError::from("Kimi native credential has no supported refresh token"));
    super::super::wipe_json(token);
    let request = request?;
    config.check_current()?;
    credential.check_current()?;
    authorized(guard, deadline)?;
    if guard
        .checkpoint_started(
            aterm_digest::Sha256::digest(config.bytes.expose()),
            aterm_digest::Sha256::digest(credential.bytes.expose()),
        )
        .is_err()
    {
        // Arm may have completed before its final authority check failed. The
        // exchange is still uncalled; only exact newly armed custody can retire.
        guard
            .checkpoint_unstarted()
            .map_err(|_| NativeError::Temporary)?;
        return Err(NativeError::Temporary);
    }
    if let Err(error) = authorized(guard, deadline) {
        guard
            .checkpoint_unstarted()
            .map_err(|_| NativeError::Temporary)?;
        return Err(error);
    }
    guard
        .checkpoint_dispatching()
        .map_err(|_| NativeError::Temporary)?;
    let (status, body) = exchange(
        &format!("{}/api/oauth/token", issuer(region)),
        request.expose(),
    )?;
    if body.len() > REPLY_MAX {
        return Err(NativeError::Temporary);
    }
    let received = now
        .checked_add(
            i64::try_from(started.elapsed().as_secs()).map_err(|_| NativeError::Temporary)?,
        )
        .ok_or(NativeError::Temporary)?;
    if !guard.binding_current() {
        return Err(NativeError::Temporary);
    }
    // Preserve the exact response before touching native state. A stale lease permits only
    // this private checkpoint, never canonical overwrite or a new provider effect.
    guard
        .checkpoint_completed(status, received, body.as_slice())
        .map_err(|_| NativeError::Temporary)?;
    receipt_authorized(guard)?;
    config.check_current()?;
    credential.check_current()?;
    let (replacement, rejected) = response_candidate(status, body.as_slice(), received)?;
    receipt_authorized(guard)?;
    config.check_current()?;
    credential.check_current()?;
    if rejected {
        rejection::save(staging, target, &config, &credential)?;
        receipt_authorized(guard)?;
        config.check_current()?;
        credential.check_current()?;
        record_rejected(target)?;
        receipt_authorized(guard)?;
        config.check_current()?;
        credential.check_current()?;
    }
    aterm_secrets::fsx::write_atomic(&path, replacement.as_slice(), 0o600)
        .map_err(|_| NativeError::Temporary)?;
    receipt_authorized(guard)?;
    guard
        .checkpoint_applied()
        .map_err(|_| NativeError::Temporary)?;
    if rejected {
        Err(NativeError::RefreshRejected)
    } else {
        Ok(())
    }
}

fn response_candidate(
    status: u16,
    body: &[u8],
    received: i64,
) -> Result<(aterm_secrets::SecretBytes, bool), NativeError> {
    if body.len() > REPLY_MAX {
        return Err(NativeError::Temporary);
    }
    let mut value: Value = aterm_json::from_slice(body).unwrap_or(Value::Null);
    // A gateway's unstructured 401/403 is not proof of a revoked grant.
    let rejected =
        matches!(status, 400 | 401 | 403) && value["error"].as_str() == Some("invalid_grant");
    let replacement = if rejected {
        // Match the native revoked-token representation so native restarts do
        // not retry the rejected refresh grant or treat it as never provisioned.
        Ok(aterm_secrets::SecretBytes::from(REVOKED))
    } else if status == 200 {
        replacement(&mut value, received)
    } else {
        Err(NativeError::Temporary)
    };
    super::super::wipe_json(value);
    let replacement = replacement?;
    Ok((replacement, rejected))
}

fn replacement(value: &mut Value, now: i64) -> Result<aterm_secrets::SecretBytes, NativeError> {
    for key in ["access_token", "refresh_token"] {
        if !value[key].as_str().is_some_and(|token| {
            !token.is_empty() && token.bytes().all(|b| (0x21..=0x7e).contains(&b))
        }) {
            return Err(NativeError::Temporary);
        }
    }
    let duration = value["expires_in"]
        .as_i64()
        .filter(|seconds| *seconds > 0)
        .ok_or(NativeError::Temporary)?;
    let expires = now.checked_add(duration).ok_or(NativeError::Temporary)?;
    if value["token_type"].as_str().unwrap_or("Bearer") != "Bearer" {
        return Err(NativeError::Temporary);
    }
    let fields = value.as_object_mut().ok_or(NativeError::Temporary)?;
    for (key, value) in [
        ("expires_at", expires.into()),
        ("token_type", "Bearer".into()),
    ] {
        if let Some(previous) = fields.insert(key.into(), value) {
            super::super::wipe_json(previous);
        }
    }
    // Persist only the native token schema, never arbitrary provider diagnostics.
    let extra: Vec<_> = fields
        .keys()
        .filter(|key| {
            !matches!(
                key.as_str(),
                "access_token"
                    | "refresh_token"
                    | "token_type"
                    | "expires_at"
                    | "expires_in"
                    | "scope"
            )
        })
        .cloned()
        .collect();
    for key in extra {
        if let Some(value) = fields.remove(&key) {
            super::super::wipe_json(value);
        }
    }
    if !fields
        .get("scope")
        .is_some_and(|value| value.as_str().is_some())
        && let Some(value) = fields.insert("scope".into(), "".into())
    {
        super::super::wipe_json(value);
    }
    aterm_json::to_vec(value)
        .map(aterm_secrets::SecretBytes::from)
        .map_err(|_| NativeError::Temporary)
}

#[cfg(test)]
pub(in crate::harness) fn rejection_fixture(home: &Path, region: Region, expected: &str) {
    let result = renew_using(
        home,
        Target {
            region,
            expected,
            on_rejected: None,
        },
        1000,
        login_drive::Deadline::after(std::time::Duration::from_secs(60)),
        &aterm_http::AlwaysAuthorized,
        |_, _| {
            Ok((
                400,
                aterm_secrets::SecretBytes::from(br#"{"error":"invalid_grant"}"#.as_slice()),
            ))
        },
    );
    assert!(matches!(result, Err(NativeError::RefreshRejected)));
}
