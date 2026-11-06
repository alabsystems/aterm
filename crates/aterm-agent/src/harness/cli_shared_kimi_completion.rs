// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bound machine-encrypted custody of one Kimi refresh, using the native completion store.
//! An armed request with no complete response remains unknown and cannot dispatch again.

use super::*;
use std::path::PathBuf;

const PREFIX: &[u8] = b"aterm.kimi-completed-refresh.v1\0";
const MAX: usize = REPLY_MAX + 4096;

/// Data locating an admitted account's recovery custody. It grants no provider authority.
#[derive(Clone)]
pub struct RecoveryContext {
    root: PathBuf,
    home: PathBuf,
    account: String,
    key: login_drive::RecoveryKey,
    authority_binding: [u8; 32],
}

impl std::fmt::Debug for RecoveryContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KimiRecoveryContext(opaque)")
    }
}

impl RecoveryContext {
    pub(in crate::harness) fn new(
        store: &accounts::Store,
        account: &str,
        key: login_drive::RecoveryKey,
        home: &Path,
    ) -> Self {
        Self {
            root: store.dir.clone(),
            home: home.into(),
            account: account.into(),
            authority_binding: key.0,
            key,
        }
    }
    pub(in crate::harness) fn with_binding(mut self, binding: [u8; 32]) -> Self {
        self.authority_binding = binding;
        self
    }
}

struct Envelope {
    binding: [u8; 32],
    config: [u8; 32],
    predecessor: [u8; 32],
    response: Option<(u16, i64, aterm_secrets::SecretBytes)>,
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    aterm_digest::Sha256::digest(bytes)
}

fn fixed<const N: usize>(rest: &mut &[u8]) -> Result<[u8; N], NativeError> {
    let value = rest
        .get(..N)
        .ok_or(NativeError::Temporary)?
        .try_into()
        .map_err(|_| NativeError::Temporary)?;
    *rest = &rest[N..];
    Ok(value)
}

fn decode(bytes: &[u8]) -> Result<Envelope, NativeError> {
    let mut rest = bytes.strip_prefix(PREFIX).ok_or(NativeError::Temporary)?;
    let binding = fixed(&mut rest)?;
    let config = fixed(&mut rest)?;
    let predecessor = fixed(&mut rest)?;
    let response = match fixed::<1>(&mut rest)? {
        [0] => None,
        [1] => {
            let status = u16::from_be_bytes(fixed(&mut rest)?);
            let received = i64::from_be_bytes(fixed(&mut rest)?);
            let len = usize::try_from(u32::from_be_bytes(fixed(&mut rest)?))
                .map_err(|_| NativeError::Temporary)?;
            if len > REPLY_MAX {
                return Err(NativeError::Temporary);
            }
            let body = rest.get(..len).ok_or(NativeError::Temporary)?;
            rest = &rest[len..];
            Some((status, received, aterm_secrets::SecretBytes::from(body)))
        }
        _ => return Err(NativeError::Temporary),
    };
    if !rest.is_empty() {
        return Err(NativeError::Temporary);
    }
    Ok(Envelope {
        binding,
        config,
        predecessor,
        response,
    })
}

pub(super) struct Record {
    store: crate::harness::pending_native_store::Store,
    directory: PathBuf,
    context: RecoveryContext,
    root: PathBuf,
    home: PathBuf,
    staging: PathBuf,
    input_staging: PathBuf,
    binding: [u8; 32],
    envelope: Option<Envelope>,
    freshly_armed: bool,
}

impl Record {
    #[cfg(test)]
    pub(super) fn ciphertext_path(&self) -> PathBuf {
        self.directory.join("candidate.age")
    }
    #[cfg(test)]
    pub(super) fn saved_response(&self) -> Option<(u16, i64, &[u8])> {
        self.envelope
            .as_ref()?
            .response
            .as_ref()
            .map(|(status, received, body)| (*status, *received, body.as_slice()))
    }
    pub(super) fn open(
        staging: &Path,
        target: Target<'_>,
        context: RecoveryContext,
    ) -> Result<Self, NativeError> {
        Self::open_using(staging, target, context, true)?.ok_or(NativeError::Temporary)
    }
    pub(super) fn existing(
        staging: &Path,
        target: Target<'_>,
        context: RecoveryContext,
    ) -> Result<Option<Self>, NativeError> {
        Self::open_using(staging, target, context, false)
    }
    fn open_using(
        staging: &Path,
        target: Target<'_>,
        context: RecoveryContext,
        prepare: bool,
    ) -> Result<Option<Self>, NativeError> {
        if !context.root.is_absolute()
            || !context.home.is_absolute()
            || !staging.is_absolute()
            || !accounts::admissible_label(&context.account)
        {
            return Err(NativeError::Temporary);
        }
        if prepare {
            aterm_secrets::fsx::ensure_private_dir(&context.root)
                .map_err(|_| NativeError::Temporary)?;
        } else {
            match std::fs::symlink_metadata(&context.root) {
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    return Ok(None);
                }
                Err(_) => return Err(NativeError::Temporary),
            }
        }
        let root = context
            .root
            .canonicalize()
            .map_err(|_| NativeError::Temporary)?;
        let home = context
            .home
            .canonicalize()
            .map_err(|_| NativeError::Temporary)?;
        let input_staging = staging.to_owned();
        let staging = staging.canonicalize().map_err(|_| NativeError::Temporary)?;
        let mut slot = aterm_digest::Sha256::new();
        slot.update(b"aterm.kimi-refresh-account.v1\0");
        slot.update(context.account.as_bytes());
        slot.update(issuer(target.region).as_bytes());
        let directory = root
            .join("pending-native-kimi-refresh")
            .join(aterm_codec::hex::encode_lower(&slot.finalize()));
        let mut bound = aterm_digest::Sha256::new();
        bound.update(PREFIX);
        bound.update(context.key.0);
        bound.update(context.authority_binding);
        // The admitted recovery key and exact before-images can authorize a new staging
        // directory after restart. The transient staging pathname is fenced for this call,
        // but is not the durable account identity.
        for field in [
            root.as_os_str().as_encoded_bytes(),
            home.as_os_str().as_encoded_bytes(),
            context.account.as_bytes(),
            issuer(target.region).as_bytes(),
            target.expected.as_bytes(),
        ] {
            bound.update(
                u64::try_from(field.len())
                    .map_err(|_| NativeError::Temporary)?
                    .to_be_bytes(),
            );
            bound.update(field);
        }
        let binding = bound.finalize();
        let opened = if prepare {
            Some(
                crate::harness::pending_native_store::Store::open(&directory, &home, MAX)
                    .map_err(|_| NativeError::Temporary)?,
            )
        } else {
            crate::harness::pending_native_store::Store::open_existing(&directory, &home, MAX)
                .map_err(|_| NativeError::Temporary)?
        };
        let Some((store, saved)) = opened else {
            return Ok(None);
        };
        let envelope = saved
            .as_ref()
            .map(|bytes| decode(bytes.as_slice()))
            .transpose()?;
        if envelope.as_ref().is_some_and(|e| e.binding != binding) {
            return Err(NativeError::Temporary);
        }
        Ok(Some(Self {
            store,
            directory,
            context,
            root,
            home,
            staging,
            input_staging,
            binding,
            envelope,
            freshly_armed: false,
        }))
    }

    fn current(&self) -> Result<(), NativeError> {
        if self.context.root.canonicalize().ok().as_ref() != Some(&self.root)
            || self.context.home.canonicalize().ok().as_ref() != Some(&self.home)
            || self.input_staging.canonicalize().ok().as_ref() != Some(&self.staging)
        {
            return Err(NativeError::Temporary);
        }
        self.store.unchanged().map_err(|_| NativeError::Temporary)
    }

    fn save(&mut self, envelope: Envelope) -> Result<(), NativeError> {
        self.current()?;
        let mut bytes = PREFIX.to_vec();
        for hash in [envelope.binding, envelope.config, envelope.predecessor] {
            bytes.extend_from_slice(&hash);
        }
        match &envelope.response {
            None => bytes.push(0),
            Some((status, received, body)) => {
                if body.len() > REPLY_MAX {
                    return Err(NativeError::Temporary);
                }
                bytes.push(1);
                bytes.extend_from_slice(&status.to_be_bytes());
                bytes.extend_from_slice(&received.to_be_bytes());
                bytes.extend_from_slice(
                    &u32::try_from(body.len())
                        .map_err(|_| NativeError::Temporary)?
                        .to_be_bytes(),
                );
                bytes.extend_from_slice(body.as_slice());
            }
        }
        let bytes = aterm_secrets::SecretBytes::from(bytes);
        self.store
            .save(bytes.as_slice())
            .map_err(|_| NativeError::Temporary)?;
        // Unknown directory durability must not authorize a refresh or retire its only receipt.
        aterm_durable::Flush::Durable
            .dir(&self.directory)
            .map_err(|_| NativeError::Temporary)?;
        self.current()?;
        self.envelope = Some(envelope);
        Ok(())
    }

    pub(super) fn arm(
        &mut self,
        config: [u8; 32],
        predecessor: [u8; 32],
    ) -> Result<(), NativeError> {
        if self.envelope.is_some() {
            return Err(NativeError::Temporary);
        }
        self.save(Envelope {
            binding: self.binding,
            config,
            predecessor,
            response: None,
        })?;
        self.freshly_armed = true;
        Ok(())
    }

    fn unstarted(&mut self) -> Result<(), NativeError> {
        if !self.freshly_armed || !self.envelope.as_ref().is_some_and(|e| e.response.is_none()) {
            return Err(NativeError::Temporary);
        }
        self.current()?;
        self.store.retire().map_err(|_| NativeError::Temporary)?;
        aterm_durable::Flush::Durable
            .dir(&self.directory)
            .map_err(|_| NativeError::Temporary)?;
        self.freshly_armed = false;
        self.envelope = None;
        Ok(())
    }
    fn dispatching(&mut self) -> Result<(), NativeError> {
        if !self.freshly_armed {
            return Err(NativeError::Temporary);
        }
        self.current()?;
        self.freshly_armed = false;
        Ok(())
    }

    pub(super) fn completed(
        &mut self,
        status: u16,
        received: i64,
        body: &[u8],
    ) -> Result<(), NativeError> {
        let previous = self.envelope.as_ref().ok_or(NativeError::Temporary)?;
        if previous.response.is_some() {
            return Err(NativeError::Temporary);
        }
        self.save(Envelope {
            binding: self.binding,
            config: previous.config,
            predecessor: previous.predecessor,
            response: Some((status, received, aterm_secrets::SecretBytes::from(body))),
        })
    }

    pub(super) fn applied(&self, target: Target<'_>) -> Result<(), NativeError> {
        self.current()?;
        let envelope = self.envelope.as_ref().ok_or(NativeError::Temporary)?;
        let (status, received, body) = envelope.response.as_ref().ok_or(NativeError::Temporary)?;
        let (candidate, _) = response_candidate(*status, body.as_slice(), *received)?;
        let config = FileEvidence::read(self.staging.join("config.toml"), false)?
            .ok_or(NativeError::Temporary)?;
        let credential = FileEvidence::read(credential_path(&self.staging, target.region), true)?
            .ok_or(NativeError::Temporary)?;
        if digest(config.bytes.expose()) != envelope.config
            || credential.bytes.expose() != candidate.as_slice()
        {
            return Err(NativeError::Temporary);
        }
        config.check_current()?;
        credential.check_current()?;
        aterm_durable::Flush::Durable
            .dir(credential.path.parent().ok_or(NativeError::Temporary)?)
            .map_err(|_| NativeError::Temporary)?;
        self.store.retire().map_err(|_| NativeError::Temporary)
    }

    pub(super) fn restore(
        &self,
        target: Target<'_>,
        deadline: login_drive::Deadline<'_>,
        authority: Arc<dyn Authority>,
    ) -> Result<Option<native_lock::NativeLock>, NativeError> {
        let Some(envelope) = &self.envelope else {
            return Ok(None);
        };
        self.current()?;
        let (status, received, body) = envelope.response.as_ref().ok_or(NativeError::Temporary)?;
        let (candidate, rejected) = response_candidate(*status, body.as_slice(), *received)?;
        let lease = native_lock::NativeLock::acquire(
            &self.staging,
            target.region,
            deadline,
            authority.clone(),
        )?;
        #[cfg(test)]
        lease.observe_clock(authority.native_lease_clock());
        let guard = lease.guard();
        authorized(guard.as_ref(), deadline)?;
        let config = FileEvidence::read(self.staging.join("config.toml"), false)?
            .ok_or(NativeError::Temporary)?;
        let credential = FileEvidence::read(credential_path(&self.staging, target.region), true)?
            .ok_or(NativeError::Temporary)?;
        if digest(config.bytes.expose()) != envelope.config {
            return Err(NativeError::Temporary);
        }
        let applied = credential.bytes.expose() == candidate.as_slice();
        if !applied && digest(credential.bytes.expose()) != envelope.predecessor {
            return Err(NativeError::Temporary);
        }
        check_config(config.bytes.expose(), target.region)?;
        config.check_current()?;
        credential.check_current()?;
        authorized(guard.as_ref(), deadline)?;
        if rejected {
            rejection::save(&self.staging, target, &config, &credential)?;
            record_rejected(target)?;
        }
        receipt_authorized(guard.as_ref())?;
        config.check_current()?;
        credential.check_current()?;
        if !applied {
            aterm_secrets::fsx::write_atomic(&credential.path, candidate.as_slice(), 0o600)
                .map_err(|_| NativeError::Temporary)?;
        }
        authorized(guard.as_ref(), deadline)?;
        self.applied(target)?;
        if rejected {
            return Err(NativeError::PreviouslyRejected);
        }
        Ok(Some(lease))
    }
}

// The existing sealed-store claim retires before the original counted authority/scope.
pub(super) struct Prepared {
    record: std::sync::Mutex<Option<Record>>,
    target: (Region, String),
    source: Arc<dyn Authority>,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KimiCompletedRefresh(opaque)")
    }
}

impl Prepared {
    pub(super) fn new(record: Record, source: Arc<dyn Authority>, target: Target<'_>) -> Self {
        Self {
            record: std::sync::Mutex::new(Some(record)),
            source,
            target: (target.region, target.expected.into()),
        }
    }
    pub(super) fn restore(
        self: &Arc<Self>,
        target: Target<'_>,
        deadline: login_drive::Deadline<'_>,
    ) -> Result<Option<native_lock::NativeLock>, NativeError> {
        if target.region != self.target.0 || target.expected != self.target.1 {
            return Err(NativeError::Temporary);
        }
        // Authority callbacks run without the record mutex. A reentrant checkpoint sees
        // unavailable custody, rather than deadlocking or obtaining a second publisher.
        let record = self
            .record
            .lock()
            .map_err(|_| NativeError::Temporary)?
            .take()
            .ok_or(NativeError::Temporary)?;
        let result = record.restore(target, deadline, self.clone());
        *self.record.lock().map_err(|_| NativeError::Temporary)? = Some(record);
        result
    }
    fn require_binding(&self) -> Result<(), NativeError> {
        if self.source.binding_current() {
            Ok(())
        } else {
            Err(NativeError::Temporary)
        }
    }
}

impl aterm_http::Guard for Prepared {
    fn is_authorized(&self) -> bool {
        self.source.is_authorized()
    }
}

impl Authority for Prepared {
    #[cfg(test)]
    fn native_lease_clock(&self) -> Option<Arc<std::sync::atomic::AtomicU64>> {
        self.source.native_lease_clock()
    }
    fn receipt_current(&self) -> bool {
        self.source.receipt_current()
    }
    fn binding_current(&self) -> bool {
        self.source.binding_current()
    }
    fn recovery_context(&self) -> Result<RecoveryContext, String> {
        self.source.recovery_context()
    }
    fn checkpoint_started(&self, config: [u8; 32], predecessor: [u8; 32]) -> Result<(), String> {
        self.require_binding().map_err(|e| e.to_string())?;
        self.record
            .lock()
            .map_err(|_| "Protected Kimi completion is unavailable")?
            .as_mut()
            .ok_or("Protected Kimi completion is unavailable")?
            .arm(config, predecessor)
            .map_err(|e| e.to_string())?;
        self.require_binding().map_err(|e| e.to_string())
    }
    fn checkpoint_completed(&self, status: u16, received: i64, body: &[u8]) -> Result<(), String> {
        self.require_binding().map_err(|e| e.to_string())?;
        self.record
            .lock()
            .map_err(|_| "Protected Kimi completion is unavailable")?
            .as_mut()
            .ok_or("Protected Kimi completion is unavailable")?
            .completed(status, received, body)
            .map_err(|e| e.to_string())?;
        self.require_binding().map_err(|e| e.to_string())
    }
    fn checkpoint_unstarted(&self) -> Result<(), String> {
        self.record
            .lock()
            .map_err(|_| "Protected Kimi completion is unavailable")?
            .as_mut()
            .ok_or("Protected Kimi completion is unavailable")?
            .unstarted()
            .map_err(|e| e.to_string())
    }
    fn checkpoint_dispatching(&self) -> Result<(), String> {
        self.record
            .lock()
            .map_err(|_| "Protected Kimi completion is unavailable")?
            .as_mut()
            .ok_or("Protected Kimi completion is unavailable")?
            .dispatching()
            .map_err(|e| e.to_string())
    }
    fn checkpoint_applied(&self) -> Result<(), String> {
        self.require_binding().map_err(|e| e.to_string())?;
        let target = Target {
            region: self.target.0,
            expected: &self.target.1,
            on_rejected: None,
        };
        self.record
            .lock()
            .map_err(|_| "Protected Kimi completion is unavailable")?
            .as_ref()
            .ok_or("Protected Kimi completion is unavailable")?
            .applied(target)
            .map_err(|e| e.to_string())
    }
}
