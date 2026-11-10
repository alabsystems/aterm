// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Read-only immutable issuer verification. No Profile claim, signer output,
//! RetainedGrant constructor, AuthLock, publication or provider permission.
use super::*;
pub(super) struct IssuerObservation {
    identity: age::Identity,
    directory: PathBuf,
    digest: [u8; 32],
    record: Record,
    reference: GrantReference,
}
impl IssuerObservation {
    pub(super) fn read(
        env: &Env,
        request: &codex_enrollment::Request,
        reference: &GrantReference,
    ) -> Result<Self, String> {
        if !request.valid() || !reference.valid() || reference.operation != request.id {
            return Err("lineage issuer reference invalid".into());
        }
        let scope = catalogue::existing_scope(env, &request.id)?
            .ok_or("historical issuer is not in the global catalogue")?;
        let directory = catalogue::location(env, &scope)?;
        platform::check_private_dir(&directory)
            .map_err(|_| "lineage issuer directory unavailable")?;
        let home = env
            .home
            .as_deref()
            .ok_or("lineage machine profile unavailable")?;
        let identity = platform::machine_identity(home)
            .map_err(|_| "lineage issuer identity unavailable")?
            .ok_or("lineage issuer identity unavailable")?;
        let cipher = platform::read_single_link_private(&directory, "candidate.age", MAX_ENVELOPE)
            .map_err(|_| "lineage issuer unavailable")?;
        let digest = aterm_digest::Sha256::digest(cipher.as_slice());
        let plaintext = age::decrypt(std::slice::from_ref(&identity), cipher.as_slice())
            .map_err(|_| "lineage issuer unavailable")?;
        let record: Record =
            aterm_json::from_slice(plaintext.as_slice()).map_err(|_| "lineage issuer invalid")?;
        if !record.valid()
            || record.request != *request
            || record.phase != Phase::VerifiedResponse
            || record.catalogue_scope.as_ref() != Some(&scope)
            || record.capability != reference.capability
            || aterm_codec::hex::encode_lower(&digest) != reference.issuance_digest
        {
            return Err("lineage issuer changed".into());
        }
        let crypto = IssuerCryptography {
            identity: &identity,
            catalogue_scope: Some(&scope),
        };
        let origin = crypto.origin_seal(&record)?;
        let saved = record
            .response
            .as_ref()
            .and_then(|response| response.producer_seal.as_ref())
            .ok_or("lineage issuer origin absent")?;
        if !aterm_digest::ct_eq(origin.as_bytes(), saved.as_bytes())
            || !aterm_digest::ct_eq(
                crypto
                    .proof(&record, &reference.issuance_digest)?
                    .as_bytes(),
                reference.producer_mac.as_bytes(),
            )
        {
            return Err("lineage issuer authentication failed".into());
        }
        let observed = Self {
            identity,
            directory,
            digest,
            record,
            reference: reference.clone(),
        };
        observed.current()?;
        Ok(observed)
    }
    pub(super) fn record(&self) -> &Record {
        &self.record
    }
    pub(super) fn current(&self) -> Result<(), String> {
        let bytes =
            platform::read_single_link_private(&self.directory, "candidate.age", MAX_ENVELOPE)
                .map_err(|_| "lineage issuer unavailable")?;
        if aterm_digest::Sha256::digest(bytes.as_slice()) != self.digest {
            return Err("lineage issuer changed during observation".into());
        }
        Ok(())
    }
    pub(super) fn verify_publication(
        &self,
        data: &crate::harness::codex_native_row::PublicationReceiptData,
    ) -> Result<(), String> {
        self.current()?;
        if !data.valid()
            || data.binding().request != self.record.request
            || data.binding().reference != self.reference
            || self
                .record
                .response
                .as_ref()
                .and_then(|response| response.native_row.as_ref())
                != Some(data.staging())
        {
            return Err("lineage publication differs from issuer".into());
        }
        verify_publication_mac(&self.identity, data)?;
        self.current()
    }
}

/// Verification only; this API never returns a MAC or mints a publication.
pub(super) fn verify_publication_mac(
    identity: &age::Identity,
    data: &crate::harness::codex_native_row::PublicationReceiptData,
) -> Result<(), String> {
    if !data.valid() {
        return Err("owned publication data invalid".into());
    }
    let body = aterm_json::to_vec(&(
        "aterm/native-row-publication/v1",
        data.binding(),
        data.staging(),
        data.binding_fingerprint(),
    ))
    .map_err(|_| "lineage publication invalid")?;
    let expected = machine_mac(identity, b"aterm/native-row-publication/key/v1\0", &body)?;
    if !aterm_digest::ct_eq(expected.as_bytes(), data.seal().as_bytes()) {
        return Err("lineage publication authentication failed".into());
    }
    Ok(())
}
