// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Authenticated observations only: no registry/Profile claims, creation,
//! roll-forward, signer result, or mutation permission.
use super::*;

use super::observation_file::CipherObservation;

pub(super) struct AuthorityObservation {
    file: CipherObservation,
    membership: membership::MembershipObservation,
    source: SourceNamespace,
    identity: age::Identity,
    pub(super) directory: PathBuf,
    pub(super) manifest: Manifest,
}
impl AuthorityObservation {
    pub(super) fn read(env: &Env) -> Result<Self, String> {
        let home = env.home.as_deref().ok_or("catalogue home unavailable")?;
        let source = crate::harness::codex_source_gate::existing_namespace_for_home(home)
            .map_err(str::to_owned)?;
        let parent = source.directory().map_err(str::to_owned)?;
        let directory = parent.join(OWNERS);
        let identity = platform::machine_identity(home)
            .map_err(str::to_owned)?
            .ok_or("catalogue identity unavailable")?;
        let (file, cipher) = CipherObservation::read(&parent.join(AUTHORITY), MAX_MANIFEST)?;
        let bytes = age::decrypt(std::slice::from_ref(&identity), cipher.as_slice())
            .map_err(|_| "catalogue authority invalid")?;
        let signed: SignedManifest =
            aterm_json::from_slice(bytes.as_slice()).map_err(|_| "catalogue authority invalid")?;
        let payload = aterm_json::to_vec(&(&signed.manifest, signed.pending.as_ref()))
            .map_err(|_| "catalogue authority invalid")?;
        let mac = machine_mac(
            &identity,
            b"aterm/codex-catalogue/manifest/key/v2\0",
            &payload,
        )?;
        if !signed.manifest.valid()
            || signed.pending.is_some()
            || !aterm_digest::ct_eq(mac.as_bytes(), signed.mac.as_bytes())
            || !signed.manifest.directory.matches(&directory)
        {
            return Err("catalogue authority requires exact reconciliation".into());
        }
        let membership =
            membership::MembershipObservation::read(parent, &identity, &signed.manifest)?;
        let observed = Self {
            file,
            membership,
            source,
            identity,
            directory,
            manifest: signed.manifest,
        };
        observed.current()?;
        Ok(observed)
    }
    pub(super) fn current(&self) -> Result<(), String> {
        self.source.directory().map_err(str::to_owned)?;
        if !self.manifest.directory.matches(&self.directory) {
            return Err("catalogue directory changed".into());
        }
        self.file.unchanged().map_err(str::to_owned)?;
        self.membership.current(&self.manifest)?;
        self.file.unchanged().map_err(str::to_owned)
    }
}

pub(super) struct EntryObservation {
    pub(super) store: CipherObservation,
    pub(super) entry: Option<Entry>,
    scope: CatalogueScope,
}
impl EntryObservation {
    pub(super) fn read(env: &Env, scope: &CatalogueScope) -> Result<Self, String> {
        let authority = AuthorityObservation::read(env)?;
        if !scope.valid() || authority.manifest.members.get(&scope.request().id) != Some(scope) {
            return Err("catalogue membership changed".into());
        }
        let directory = authority
            .directory
            .join(&scope.request().id)
            .join("catalogue");
        let (store, cipher) = CipherObservation::read(&directory, MAX_ENTRY)?;
        let bytes = age::decrypt(std::slice::from_ref(&authority.identity), cipher.as_slice())
            .map_err(|_| "catalogue entry invalid")?;
        let signed: SignedEntry =
            aterm_json::from_slice(bytes.as_slice()).map_err(|_| "catalogue entry invalid")?;
        let payload = aterm_json::to_vec(&signed.entry).map_err(|_| "catalogue entry invalid")?;
        let mac = machine_mac(
            &authority.identity,
            b"aterm/codex-catalogue/entry/key/v1\0",
            &payload,
        )?;
        if !signed.entry.valid()
            || signed.entry.scope != *scope
            || !aterm_digest::ct_eq(mac.as_bytes(), signed.mac.as_bytes())
        {
            return Err("catalogue entry invalid".into());
        }
        authority.current()?;
        store.unchanged().map_err(str::to_owned)?;
        Ok(Self {
            store,
            entry: Some(signed.entry),
            scope: scope.clone(),
        })
    }
    pub(super) fn witness(&self) -> Result<CatalogueWitness, String> {
        let entry = self.entry.as_ref().ok_or("catalogue entry absent")?;
        self.store.unchanged().map_err(str::to_owned)?;
        Ok(CatalogueWitness {
            namespace: self.scope.namespace.clone(),
            sequence: entry.sequence,
            digest: aterm_codec::hex::encode_lower(&self.store.digest()),
            phase: entry.phase,
        })
    }
}
