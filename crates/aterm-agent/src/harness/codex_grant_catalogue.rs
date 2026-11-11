// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Machine-home custody for new native issuances. The manifest is the canonical
//! membership authority; per-operation witnesses anchor mutable entry phases.
//! Neither a new namespace nor an empty directory proves anything about older
//! or non-cooperating credential consumers. Old issuances are not participants.
use super::*;
use crate::harness::codex_enrollment::{CatalogueAnchorWriter, CataloguePhase, CatalogueWitness};
use crate::harness::codex_source_gate::{SourceNamespace, SourceWorkLease};
use std::collections::BTreeMap;

#[path = "codex_grant_catalogue_file.rs"]
mod observation_file;
#[path = "codex_grant_catalogue_read.rs"]
mod read;
use read::{AuthorityObservation, EntryObservation};

#[cfg(test)]
thread_local! { static MANIFEST_CUT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
thread_local! { static PREPARATION_CUT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
fn cut_preparation(at: u8) -> Result<(), String> {
    PREPARATION_CUT.with(|cut| {
        if cut.get() == at {
            cut.set(0);
            Err("fixture preparation persistence cut".into())
        } else {
            Ok(())
        }
    })
}
#[cfg(test)]
fn cut_manifest(at: u8) -> Result<(), String> {
    MANIFEST_CUT.with(|cut| {
        if cut.get() == at {
            cut.set(0);
            Err("fixture manifest persistence cut".into())
        } else {
            Ok(())
        }
    })
}

const MAX_MANIFEST: usize = 2 * 1024 * 1024;
const MAX_ENTRY: usize = 128 * 1024;
const AUTHORITY: &str = "owner-catalogue-authority-v1";
const OWNERS: &str = "credential-owners-v2";
#[path = "codex_grant_catalogue_anchor.rs"]
mod membership;
use membership::MembershipAnchor;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CatalogueScope {
    namespace: String,
    admission: String,
    source: codex_enrollment::Staging,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    guarded_preparation: bool,
}
impl std::fmt::Debug for CatalogueScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CatalogueScope(redacted)")
    }
}
impl CatalogueScope {
    pub(super) fn valid(&self) -> bool {
        valid_hex(&self.namespace, 64)
            && valid_hex(&self.admission, 64)
            && self.source.request.valid()
            && self.source.store.is_absolute()
            && self.source.store_identity.is_some()
    }
    pub(super) fn request(&self) -> &codex_enrollment::Request {
        &self.source.request
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    namespace: String,
    directory: codex_enrollment::DirectoryIdentity,
    sequence: u64,
    members: BTreeMap<String, CatalogueScope>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unattempted: Vec<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedManifest {
    manifest: Manifest,
    pending: Option<Manifest>,
    mac: String,
}
fn persist_exact(
    store: &mut platform::Store,
    identity: &age::Identity,
    bytes: &[u8],
) -> Result<(), String> {
    if store.unchanged().is_ok() {
        if store.save(bytes).is_err()
            && !store
                .recover_exact_write(identity, bytes)
                .map_err(str::to_owned)?
        {
            return Err("catalogue write has not settled".into());
        }
    } else if !store
        .recover_exact_write(identity, bytes)
        .map_err(str::to_owned)?
    {
        return Err("catalogue changed during write".into());
    }
    // Atomic rename alone is not a durable phase transition. This exact-payload
    // readback flushes the opened file and its parent and rechecks FD/path.
    if !store
        .recover_exact_write(identity, bytes)
        .map_err(str::to_owned)?
    {
        return Err("catalogue write is not durable".into());
    }
    store.unchanged().map_err(str::to_owned)
}

fn manifest_bytes(
    identity: &age::Identity,
    manifest: &Manifest,
    pending: Option<&Manifest>,
) -> Result<SecretBytes, String> {
    let payload =
        aterm_json::to_vec(&(manifest, pending)).map_err(|_| "catalogue manifest invalid")?;
    let mac = machine_mac(
        identity,
        b"aterm/codex-catalogue/manifest/key/v2\0",
        &payload,
    )?;
    Ok(SecretBytes::new(
        aterm_json::to_vec(&SignedManifest {
            manifest: manifest.clone(),
            pending: pending.cloned(),
            mac,
        })
        .map_err(|_| "catalogue manifest invalid")?,
    ))
}
fn membership_successor(old: &Manifest, next: &Manifest) -> bool {
    let immutable_members = old
        .members
        .iter()
        .all(|(id, scope)| next.members.get(id) == Some(scope));
    let insertion = old.members.len().checked_add(1) == Some(next.members.len())
        && immutable_members
        && next
            .members
            .iter()
            .find(|(id, _)| !old.members.contains_key(*id))
            .is_some_and(|(id, scope)| {
                let mut expected = old.unattempted.clone();
                if scope.guarded_preparation {
                    expected.push(id.clone());
                    expected.sort();
                }
                next.unattempted == expected
            });
    let exchange = old.members == next.members
        && next.unattempted.len().checked_add(1) == Some(old.unattempted.len())
        && next
            .unattempted
            .iter()
            .all(|id| old.unattempted.contains(id));
    next.valid()
        && old.namespace == next.namespace
        && old.directory == next.directory
        && old.sequence.checked_add(1) == Some(next.sequence)
        && (insertion || exchange)
}
impl Manifest {
    fn valid(&self) -> bool {
        self.schema == 1
            && valid_hex(&self.namespace, 64)
            && self.sequence > 0
            && self.members.len() <= MAX_PENDING
            && self.unattempted.len() <= self.members.len()
            && self.unattempted.windows(2).all(|pair| pair[0] < pair[1])
            && self.unattempted.iter().all(|id| {
                self.members
                    .get(id)
                    .is_some_and(|scope| scope.guarded_preparation)
            })
            && self.members.iter().all(|(id, scope)| {
                valid_hex(id, 32)
                    && scope.valid()
                    && scope.namespace == self.namespace
                    && scope.request().id == *id
            })
    }
}
struct Authority {
    store: platform::Store,
    identity: age::Identity,
    source: SourceNamespace,
    directory: PathBuf,
    manifest: Manifest,
    membership: MembershipAnchor,
    committed: Option<Manifest>,
}
impl Authority {
    fn open(env: &Env, create: bool) -> Result<Self, String> {
        let home = env.home.as_deref().ok_or("catalogue home unavailable")?;
        let work = if create {
            Some(SourceWorkLease::for_home(home).map_err(str::to_owned)?)
        } else {
            None
        };
        let source = match &work {
            Some(work) => work.namespace().map_err(str::to_owned)?,
            None => crate::harness::codex_source_gate::existing_namespace_for_home(home)
                .map_err(str::to_owned)?,
        };
        let parent = source.directory().map_err(str::to_owned)?.to_path_buf();
        let directory = parent.join(OWNERS);
        let authority = parent.join(AUTHORITY);
        let identity = platform::machine_identity(home)
            .map_err(|_| "catalogue identity unavailable")?
            .ok_or("catalogue identity unavailable")?;
        let _registry = platform::registry_lock(&parent).map_err(|_| "catalogue registry busy")?;
        let existing = platform::Store::open_owned_existing(&authority, home, MAX_MANIFEST)?;
        let bootstrap = if existing.is_none() {
            match MembershipAnchor::open(&parent, home, &identity, false) {
                Ok(anchor) => anchor.bootstrap_manifest(&directory)?,
                Err(_) => None,
            }
        } else {
            None
        };
        let (mut store, bytes, fresh) = match existing {
            Some((store, bytes)) => (store, bytes, false),
            None if bootstrap.is_some() => {
                let (store, bytes) = platform::Store::open_owned(&authority, home, MAX_MANIFEST)?;
                (store, bytes, true)
            }
            None if create => {
                // The outside authority remains when an owner directory is lost.
                // An unexplained existing directory is never an empty catalogue.
                match std::fs::symlink_metadata(&directory) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    _ => return Err("catalogue membership has no authority".into()),
                }
                let (store, bytes) = platform::Store::open_owned(&authority, home, MAX_MANIFEST)?;
                (store, bytes, true)
            }
            None => return Err("catalogue authority absent".into()),
        };
        if store.recipient() != identity.recipient() {
            return Err("catalogue identity changed".into());
        }
        let (mut manifest, pending) = match bytes {
            Some(bytes) => {
                let signed: SignedManifest = aterm_json::from_slice(bytes.as_slice())
                    .map_err(|_| "catalogue authority invalid")?;
                let payload = aterm_json::to_vec(&(&signed.manifest, signed.pending.as_ref()))
                    .map_err(|_| "catalogue authority invalid")?;
                let expected = machine_mac(
                    &identity,
                    b"aterm/codex-catalogue/manifest/key/v2\0",
                    &payload,
                )?;
                if !signed.manifest.valid()
                    || signed
                        .pending
                        .as_ref()
                        .is_some_and(|next| !membership_successor(&signed.manifest, next))
                    || !aterm_digest::ct_eq(expected.as_bytes(), signed.mac.as_bytes())
                {
                    return Err("catalogue authority invalid".into());
                }
                platform::check_private_dir(&directory)
                    .map_err(|_| "catalogue directory unavailable")?;
                if codex_enrollment::DirectoryIdentity::capture(&directory)?
                    != signed.manifest.directory
                {
                    return Err("catalogue directory changed".into());
                }
                (signed.manifest, signed.pending)
            }
            None if bootstrap.is_some() => (bootstrap.expect("checked canonical bootstrap"), None),
            None if fresh => {
                platform::ensure_private_dir(&directory)
                    .map_err(|_| "catalogue directory unavailable")?;
                let mut nonce = [0u8; 32];
                aterm_uds::rand::fill(&mut nonce).map_err(|_| "catalogue namespace unavailable")?;
                let namespace = aterm_codec::hex::encode_lower(&nonce);
                nonce.zeroize();
                (
                    Manifest {
                        schema: 1,
                        namespace,
                        directory: codex_enrollment::DirectoryIdentity::capture(&directory)?,
                        sequence: 1,
                        members: BTreeMap::new(),
                        unattempted: Default::default(),
                    },
                    None,
                )
            }
            None => return Err("catalogue authority lost its manifest".into()),
        };
        let mut membership = MembershipAnchor::open(&parent, home, &identity, fresh)?;
        if let Some(next) = pending {
            // Only exact producer-MACed metadata survives this cut. Never
            // rebuild a membership proposal from a current roster/selection.
            membership.prepare_after(&identity, &manifest, &next)?;
            let bytes = manifest_bytes(&identity, &next, None)?;
            persist_exact(&mut store, &identity, bytes.as_slice())?;
            membership.observe(&identity, &next)?;
            manifest = next;
        } else if !fresh {
            membership.observe(&identity, &manifest)?;
        }
        let committed = (!fresh).then(|| manifest.clone());
        let mut opened = Self {
            membership,
            committed,
            store,
            identity,
            source,
            directory,
            manifest,
        };
        if fresh {
            opened.save()?;
        }
        opened.current()?;
        // Commit the membership/authority directory links before any dependent
        // code exchange can be armed. Older private homes remain untouched.
        aterm_durable::Flush::Durable
            .dir(&opened.directory)
            .map_err(|_| "catalogue namespace durability uncertain")?;
        aterm_durable::Flush::Durable
            .dir(&parent)
            .map_err(|_| "catalogue namespace durability uncertain")?;
        if let Some(ancestor) = parent.parent() {
            aterm_durable::Flush::Durable
                .dir(ancestor)
                .map_err(|_| "catalogue namespace durability uncertain")?;
        }
        Ok(opened)
    }
    fn current(&self) -> Result<(), String> {
        self.source.directory().map_err(str::to_owned)?;
        if codex_enrollment::DirectoryIdentity::capture(&self.directory)? != self.manifest.directory
        {
            return Err("catalogue directory changed".into());
        }
        self.membership.current(&self.manifest)?;
        self.store.unchanged().map_err(str::to_owned)
    }
    fn save(&mut self) -> Result<(), String> {
        if !self.manifest.valid() {
            return Err("catalogue authority invalid".into());
        }
        if let Some(previous) = &self.committed {
            if !membership_successor(previous, &self.manifest) {
                return Err("catalogue membership proposal changed".into());
            }
            let bytes = manifest_bytes(&self.identity, previous, Some(&self.manifest))?;
            persist_exact(&mut self.store, &self.identity, bytes.as_slice())?;
            #[cfg(test)]
            cut_manifest(1)?;
            self.membership
                .prepare_after(&self.identity, previous, &self.manifest)?;
        } else {
            self.membership.prepare(&self.identity, &self.manifest)?;
        }
        #[cfg(test)]
        cut_manifest(2)?;
        let bytes = manifest_bytes(&self.identity, &self.manifest, None)?;
        persist_exact(&mut self.store, &self.identity, bytes.as_slice())?;
        self.membership.observe(&self.identity, &self.manifest)?;
        self.committed = Some(self.manifest.clone());
        self.current()
    }
}

/// Only this module constructs transitions from authenticated entry custody.
/// A decoded witness, directory path or caller-supplied counter is not one.
pub(in crate::harness) struct CatalogueTransition {
    scope: CatalogueScope,
    expected: Option<CatalogueWitness>,
    next: CatalogueWitness,
}
impl CatalogueTransition {
    pub(in crate::harness) fn request(&self) -> &codex_enrollment::Request {
        self.scope.request()
    }
    pub(in crate::harness) fn staging(&self) -> &codex_enrollment::Staging {
        &self.scope.source
    }
    pub(in crate::harness) fn expected(&self) -> Option<&CatalogueWitness> {
        self.expected.as_ref()
    }
    pub(in crate::harness) fn next(&self) -> &CatalogueWitness {
        &self.next
    }
    pub(in crate::harness) fn valid(&self) -> bool {
        self.scope.valid()
            && self.next.valid()
            && self.next.namespace == self.scope.namespace
            && match &self.expected {
                None => self.next.sequence == 1 && self.next.phase == CataloguePhase::Prepared,
                Some(old) => {
                    old.valid()
                        && old.namespace == self.next.namespace
                        && (old == &self.next
                            || old.sequence.checked_add(1) == Some(self.next.sequence))
                        && allowed_phase(old.phase, self.next.phase)
                }
            }
    }
}
fn allowed_phase(old: CataloguePhase, next: CataloguePhase) -> bool {
    old == next
        || matches!(
            (old, next),
            (
                CataloguePhase::Prepared,
                CataloguePhase::Attempted | CataloguePhase::NotDispatched
            ) | (
                CataloguePhase::Attempted,
                CataloguePhase::Received
                    | CataloguePhase::Quarantined
                    | CataloguePhase::Rejected
                    | CataloguePhase::NotDispatched
            )
        )
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LineageFacts {
    issuer: String,
    client: String,
    principal: Identity,
    token: String,
}
impl LineageFacts {
    fn from_verified(record: &Record) -> Result<Option<Self>, String> {
        let response = record
            .response
            .as_ref()
            .ok_or("catalogue response absent")?;
        let principal = response
            .identity
            .as_ref()
            .ok_or("catalogue principal absent")?;
        let document = decoded_document(response, MAX_DOCUMENT)?;
        let value: aterm_json::Value = aterm_json::from_slice(document.as_slice())
            .map_err(|_| "catalogue response invalid")?;
        let Some(authority) = value
            .get("tokens")
            .and_then(|tokens| tokens.get("oauth_provenance"))
            .and_then(aterm_json::Value::as_object)
        else {
            return Ok(None);
        };
        if authority.len() != 2 {
            return Ok(None);
        }
        let issuer = authority
            .get("issuer")
            .and_then(aterm_json::Value::as_str)
            .map(|issuer| issuer.trim_end_matches('/'));
        let client = authority
            .get("client_id")
            .and_then(aterm_json::Value::as_str);
        if issuer != Some(codex_app_login::DEFAULT_ISSUER)
            || client != Some(codex_app_login::CLIENT_ID)
        {
            return Ok(None);
        }
        Ok(Some(Self {
            issuer: codex_app_login::DEFAULT_ISSUER.into(),
            client: codex_app_login::CLIENT_ID.into(),
            principal: principal.clone(),
            token: codex_app_login::owned_refresh_token_digest(document.as_slice())?,
        }))
    }
    fn valid(&self) -> bool {
        self.issuer == codex_app_login::DEFAULT_ISSUER
            && self.client == codex_app_login::CLIENT_ID
            && valid_hex(&self.principal.account, 64)
            && valid_hex(&self.principal.user, 64)
            && valid_hex(&self.token, 64)
    }
    fn overlaps_principal(&self, other: &Self) -> bool {
        self.issuer == other.issuer
            && self.client == other.client
            && (self.principal.account == other.principal.account
                || self.principal.user == other.principal.user)
    }
    fn same_token(&self, other: &Self) -> bool {
        self.issuer == other.issuer && self.client == other.client && self.token == other.token
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    schema: u32,
    scope: CatalogueScope,
    sequence: u64,
    predecessor: Option<CatalogueWitness>,
    phase: CataloguePhase,
    reference: Option<GrantReference>,
    facts: Option<LineageFacts>,
    response_digest: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedEntry {
    entry: Entry,
    mac: String,
}
impl Entry {
    fn valid(&self) -> bool {
        ((self.schema == 1 && !(self.phase == CataloguePhase::Quarantined && self.facts.is_some()))
            || (self.schema == 2
                && self.phase == CataloguePhase::Quarantined
                && self.facts.is_some()))
            && self.scope.valid()
            && self.sequence > 0
            && self.predecessor.as_ref().is_none_or(|old| {
                old.valid()
                    && old.namespace == self.scope.namespace
                    && old.sequence.checked_add(1) == Some(self.sequence)
                    && allowed_phase(old.phase, self.phase)
            })
            && (self.predecessor.is_some()
                || (self.sequence == 1 && self.phase == CataloguePhase::Prepared))
            && self.reference.as_ref().is_none_or(|reference| {
                reference.valid() && reference.operation == self.scope.request().id
            })
            && self.facts.as_ref().is_none_or(LineageFacts::valid)
            && self
                .response_digest
                .as_ref()
                .is_none_or(|digest| valid_hex(digest, 64))
            && match self.phase {
                CataloguePhase::Prepared | CataloguePhase::Attempted => {
                    self.reference.is_none()
                        && self.facts.is_none()
                        && self.response_digest.is_none()
                }
                CataloguePhase::Received => {
                    self.reference.is_some()
                        && self.facts.is_some()
                        && self.response_digest.is_some()
                }
                CataloguePhase::Quarantined => {
                    self.response_digest.is_some()
                        && (self.facts.is_none() || self.reference.is_some())
                }
                CataloguePhase::Rejected => self.facts.is_none() && self.response_digest.is_some(),
                CataloguePhase::NotDispatched => self.reference.is_none() && self.facts.is_none(),
            }
    }
}

struct ClaimedEntry {
    directory: PathBuf,
    store: platform::Store,
    identity: age::Identity,
    scope: CatalogueScope,
    entry: Option<Entry>,
}
impl ClaimedEntry {
    fn open(env: &Env, scope: &CatalogueScope, create: bool) -> Result<Self, String> {
        let authority = Authority::open(env, false)?;
        if !scope.valid() || authority.manifest.members.get(&scope.request().id) != Some(scope) {
            return Err("catalogue membership changed".into());
        }
        let directory = authority
            .directory
            .join(&scope.request().id)
            .join("catalogue");
        let home = env.home.as_deref().ok_or("catalogue home unavailable")?;
        let opened = if create {
            Some(platform::Store::open_owned(&directory, home, MAX_ENTRY)?)
        } else {
            platform::Store::open_owned_existing(&directory, home, MAX_ENTRY)?
        };
        let (store, bytes) = opened.ok_or("catalogue entry absent")?;
        if store.recipient() != authority.identity.recipient() {
            return Err("catalogue identity changed".into());
        }
        let entry = bytes
            .map(|bytes| -> Result<Entry, String> {
                let signed: SignedEntry = aterm_json::from_slice(bytes.as_slice())
                    .map_err(|_| "catalogue entry invalid")?;
                let payload =
                    aterm_json::to_vec(&signed.entry).map_err(|_| "catalogue entry invalid")?;
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
                Ok(signed.entry)
            })
            .transpose()?;
        authority.current()?;
        Ok(Self {
            directory,
            store,
            identity: authority.identity,
            scope: scope.clone(),
            entry,
        })
    }
    fn durable_current(&mut self) -> Result<(), String> {
        let entry = self.entry.as_ref().ok_or("catalogue entry absent")?;
        let payload = aterm_json::to_vec(entry).map_err(|_| "catalogue entry invalid")?;
        let mac = machine_mac(
            &self.identity,
            b"aterm/codex-catalogue/entry/key/v1\0",
            &payload,
        )?;
        let bytes = aterm_json::to_vec(&SignedEntry {
            entry: entry.clone(),
            mac,
        })
        .map_err(|_| "catalogue entry invalid")?;
        if !self
            .store
            .recover_exact_write(&self.identity, &bytes)
            .map_err(str::to_owned)?
        {
            return Err("catalogue entry is not an exact durable payload".into());
        }
        // The leaf flush does not commit newly created operation-directory
        // names in their parents. Commit both links before POST/phase ACK.
        let parent = self
            .directory
            .parent()
            .ok_or("catalogue entry parent absent")?;
        aterm_durable::Flush::Durable
            .dir(parent)
            .map_err(|_| "catalogue entry link durability uncertain")?;
        aterm_durable::Flush::Durable
            .dir(parent.parent().ok_or("catalogue owner root absent")?)
            .map_err(|_| "catalogue owner link durability uncertain")?;
        self.store.unchanged().map_err(str::to_owned)
    }
    fn witness(&self) -> Result<CatalogueWitness, String> {
        let entry = self.entry.as_ref().ok_or("catalogue entry absent")?;
        let digest = self.store.sealed_digest().ok_or("catalogue entry absent")?;
        Ok(CatalogueWitness {
            namespace: self.scope.namespace.clone(),
            sequence: entry.sequence,
            digest: aterm_codec::hex::encode_lower(&digest),
            phase: entry.phase,
        })
    }
    fn save(&mut self, entry: Entry) -> Result<(), String> {
        if !entry.valid() || entry.scope != self.scope {
            return Err("catalogue entry invalid".into());
        }
        let payload =
            aterm_json::to_vec(&entry).map_err(|_| "catalogue entry cannot be encoded")?;
        let mac = machine_mac(
            &self.identity,
            b"aterm/codex-catalogue/entry/key/v1\0",
            &payload,
        )?;
        let bytes = SecretBytes::new(
            aterm_json::to_vec(&SignedEntry { entry, mac })
                .map_err(|_| "catalogue entry cannot be encoded")?,
        );
        persist_exact(&mut self.store, &self.identity, bytes.as_slice())?;
        let signed: SignedEntry =
            aterm_json::from_slice(bytes.as_slice()).map_err(|_| "catalogue entry invalid")?;
        self.entry = Some(signed.entry);
        self.durable_current()
    }
}

fn with_anchor<R>(
    env: &Env,
    scope: &CatalogueScope,
    run: impl FnOnce(&mut CatalogueAnchorWriter<'_>) -> Result<R, String>,
) -> Result<R, String> {
    let store = env
        .accounts_store_for(crate::harness::accounts::Cli::Codex)
        .ok_or("catalogue source unavailable")?;
    if std::fs::canonicalize(&store.dir).ok().as_ref() != Some(&scope.source.store)
        || !scope.source.store_current()
    {
        return Err("catalogue source changed".into());
    }
    // CodeExchange calls this while holding its stage AuthLock. Never wait into
    // an account→owner→Auth cycle; contention refuses before send or retains the
    // already completed response in its existing counted carrier.
    let held = store
        .acquire_within(&[std::time::Duration::ZERO])
        .map_err(|_| "catalogue anchor unavailable")?
        .ok_or("catalogue anchor busy")?;
    let (logins, note) = store.load_logins(env.now)?;
    if note.is_some() || logins.enrollment.catalogue_staging(scope.request())? != scope.source {
        return Err("catalogue source changed".into());
    }
    let mut anchor = CatalogueAnchorWriter::new(&store, &held, scope.request(), env.now);
    run(&mut anchor)
}

/// Register only a new-format actual native admission. A historical record is
/// never promoted because its old custody directory happens to be absent.
pub(super) fn prepare(
    env: &Env,
    request: &codex_enrollment::Request,
    stage: &Path,
) -> Result<Option<CatalogueScope>, String> {
    let Some(source_store) = env.accounts_store_for(crate::harness::accounts::Cli::Codex) else {
        return Ok(None);
    };
    // Admission precedes namespace/membership writes. The live row driver
    // already owns this writer and uses prepare_held instead of reopening it.
    let held = source_store
        .acquire_within(&[std::time::Duration::ZERO])
        .map_err(|_| "catalogue anchor unavailable")?
        .ok_or("catalogue anchor busy")?;
    prepare_held(env, request, stage, &source_store, &held)
}

pub(super) fn prepare_held(
    env: &Env,
    request: &codex_enrollment::Request,
    stage: &Path,
    source_store: &super::super::accounts::Store,
    held: &super::super::accounts::Lock,
) -> Result<Option<CatalogueScope>, String> {
    let canonical = env
        .accounts_store_for(crate::harness::accounts::Cli::Codex)
        .ok_or("catalogue source unavailable")?;
    if std::fs::canonicalize(&canonical.dir).map_err(|_| "catalogue source unavailable")?
        != std::fs::canonicalize(&source_store.dir).map_err(|_| "catalogue writer unavailable")?
    {
        return Err("catalogue writer belongs to another source".into());
    }
    let snapshot = source_store.native_retirement_snapshot_held(held, env.now)?;
    let logins = snapshot.retained_image();
    let Some(_) = logins.enrollment.lookup(request)? else {
        return Ok(None);
    };
    if !logins.enrollment.native_admitted(request)? {
        return Ok(None);
    }
    if !logins.enrollment.requires_global_catalogue(request)? {
        return Err("historical native issuance requires authenticated migration".into());
    }
    logins.enrollment.require_running(request)?;
    let staging = logins.enrollment.catalogue_staging(request)?;
    if !super::super::codex_native_row::staging_shape(&staging)
        || staging.directory.join("auth.json") != stage
        || !staging.directories_current()
        || std::fs::canonicalize(&source_store.dir).ok().as_ref() != Some(&staging.store)
    {
        return Err("catalogue preparation source changed".into());
    }
    let authority = Authority::open(env, true)?;
    let repairs: Vec<_> = authority
        .manifest
        .members
        .values()
        .filter(|scope| {
            scope.source.store == staging.store
                && scope.guarded_preparation
                && authority.manifest.unattempted.contains(&scope.request().id)
        })
        .cloned()
        .collect();
    authority.current()?;
    drop(authority);
    for scope in repairs {
        reconcile_preparation_held(env, &scope, source_store, held)?;
    }
    let mut authority = Authority::open(env, false)?;
    let scope = match authority.manifest.members.get(&request.id) {
        Some(scope)
            if scope.source == staging
                && scope.guarded_preparation
                && authority.manifest.unattempted.contains(&request.id) =>
        {
            scope.clone()
        }
        Some(_) => return Err("catalogue operation already used".into()),
        None => {
            if authority.manifest.members.len() >= MAX_PENDING {
                return Err("catalogue capacity exhausted".into());
            }
            let mut nonce = [0u8; 32];
            aterm_uds::rand::fill(&mut nonce).map_err(|_| "catalogue admission unavailable")?;
            let admission = aterm_codec::hex::encode_lower(&nonce);
            nonce.zeroize();
            let scope = CatalogueScope {
                namespace: authority.manifest.namespace.clone(),
                admission,
                source: staging,
                guarded_preparation: true,
            };
            authority.manifest.sequence = authority
                .manifest
                .sequence
                .checked_add(1)
                .ok_or("catalogue sequence exhausted")?;
            authority
                .manifest
                .members
                .insert(request.id.clone(), scope.clone());
            authority.manifest.unattempted.push(request.id.clone());
            authority.manifest.unattempted.sort();
            authority.save()?;
            #[cfg(test)]
            cut_preparation(1)?;
            scope
        }
    };
    authority.current()?;
    drop(authority);
    let mut claimed = ClaimedEntry::open(env, &scope, true)?;
    // Reuse the actual row writer; opening a second exclusive accounts.lock
    // here would conflict with the caller's own source-pinned snapshot.
    let mut anchor = CatalogueAnchorWriter::new(source_store, held, request, env.now);
    (|anchor: &mut CatalogueAnchorWriter<'_>| {
        let expected = anchor.read()?;
        if claimed.entry.is_none() {
            if expected.is_some() {
                return Err("catalogue entry lost after admission".into());
            }
            claimed.save(Entry {
                schema: 1,
                scope: scope.clone(),
                sequence: 1,
                predecessor: None,
                phase: CataloguePhase::Prepared,
                reference: None,
                facts: None,
                response_digest: None,
            })?;
        }
        claimed.durable_current()?;
        #[cfg(test)]
        cut_preparation(2)?;
        let next = claimed.witness()?;
        if next.phase != CataloguePhase::Prepared
            || (expected.is_some() && expected.as_ref() != Some(&next))
        {
            return Err("catalogue exchange already attempted".into());
        }
        anchor.compare_exchange(&CatalogueTransition {
            scope: scope.clone(),
            expected,
            next,
        })
    })(&mut anchor)?;
    Ok(Some(scope))
}

#[cfg(all(test, unix))]
#[path = "codex_grant_preparation_tests.rs"]
mod preparation_tests;

/// Complete only an original, positively fenced metadata preparation. Missing
/// mutable files alone never prove that a provider exchange did not happen.
fn reconcile_preparation_held(
    env: &Env,
    scope: &CatalogueScope,
    source_store: &super::super::accounts::Store,
    held: &super::super::accounts::Lock,
) -> Result<(), String> {
    let authority = AuthorityObservation::read(env)?;
    if !scope.guarded_preparation
        || authority.manifest.members.get(&scope.request().id) != Some(scope)
        || !authority.manifest.unattempted.contains(&scope.request().id)
    {
        return Err("catalogue preparation has no independent no-attempt fence".into());
    }
    let snapshot = source_store.native_retirement_snapshot_held(held, env.now)?;
    let logins = snapshot.retained_image();
    if std::fs::canonicalize(&source_store.dir).map_err(|_| "catalogue source unavailable")?
        != scope.source.store
        || !scope.source.store_current()
        || logins.enrollment.catalogue_staging(scope.request())? != scope.source
    {
        return Err("catalogue preparation source changed".into());
    }
    if !logins
        .enrollment
        .refused_catalogue_preparation(scope.request())?
    {
        return Ok(());
    }
    // A successfully constructed producer always has its durable issuer body.
    // An active owner refuses this exclusive claim; parked or retained custody
    // refuses below too. Neither is overwritten or converted into no-send proof.
    let issuer = super::Store::open(env, &scope.request().id, true)?;
    if issuer.as_ref().is_some_and(|(_, record)| record.is_some()) {
        return Err("catalogue preparation still has issuer custody".into());
    }
    let mut claimed = ClaimedEntry::open(env, scope, true)?;
    if let Some(entry) = &claimed.entry {
        if entry.phase != CataloguePhase::Prepared
            || entry.sequence != 1
            || entry.predecessor.is_some()
        {
            return Err("catalogue preparation already advanced".into());
        }
    } else {
        claimed.save(Entry {
            schema: 1,
            scope: scope.clone(),
            sequence: 1,
            predecessor: None,
            phase: CataloguePhase::Prepared,
            reference: None,
            facts: None,
            response_digest: None,
        })?;
    }
    claimed.durable_current()?;
    authority.current()?;
    snapshot.current()?;
    let mut anchor = CatalogueAnchorWriter::new(source_store, held, scope.request(), env.now);
    anchor.compare_exchange(&CatalogueTransition {
        scope: scope.clone(),
        expected: None,
        next: claimed.witness()?,
    })?;
    drop(issuer);
    Ok(())
}

pub(super) fn location(env: &Env, scope: &CatalogueScope) -> Result<PathBuf, String> {
    let authority = AuthorityObservation::read(env)?;
    if authority.manifest.members.get(&scope.request().id) != Some(scope) {
        return Err("catalogue membership changed".into());
    }
    let path = authority.directory.join(&scope.request().id);
    authority.current()?;
    Ok(path)
}

fn advance(
    env: &Env,
    scope: &CatalogueScope,
    phase: CataloguePhase,
    reference: Option<GrantReference>,
    facts: Option<LineageFacts>,
    response_digest: Option<String>,
) -> Result<(), String> {
    let mut claimed = ClaimedEntry::open(env, scope, false)?;
    with_anchor(env, scope, |anchor| {
        let expected = anchor.read()?.ok_or("catalogue admission absent")?;
        let old = claimed.entry.as_ref().ok_or("catalogue entry absent")?;
        let actual = claimed.witness()?;
        let matches_requested = old.phase == phase
            && old.reference == reference
            && old.facts == facts
            && old.response_digest == response_digest;
        if actual == expected && matches_requested {
            return claimed.durable_current();
        }
        if !(matches_requested && old.predecessor.as_ref() == Some(&expected)) {
            if actual != expected || !allowed_phase(old.phase, phase) {
                return Err("catalogue phase changed".into());
            }
            claimed.save(Entry {
                schema: if phase == CataloguePhase::Quarantined && facts.is_some() {
                    2
                } else {
                    1
                },
                scope: scope.clone(),
                sequence: expected
                    .sequence
                    .checked_add(1)
                    .ok_or("catalogue sequence exhausted")?,
                predecessor: Some(expected.clone()),
                phase,
                reference,
                facts,
                response_digest,
            })?;
        }
        claimed.durable_current()?;
        anchor.compare_exchange(&CatalogueTransition {
            scope: scope.clone(),
            expected: Some(expected),
            next: claimed.witness()?,
        })
    })
}
pub(super) fn attempted(env: &Env, scope: &CatalogueScope) -> Result<(), String> {
    let inventory = Inventory::read(env, Some(scope))?;
    if inventory.members.iter().any(|member| member.pending) {
        return Err("catalogue refresh requires reconciliation before code exchange".into());
    }
    if scope.guarded_preparation {
        let mut authority = Authority::open(env, false)?;
        if authority.manifest.members.get(&scope.request().id) != Some(scope) {
            return Err("catalogue exchange membership changed".into());
        }
        if let Some(index) = authority
            .manifest
            .unattempted
            .iter()
            .position(|id| id == &scope.request().id)
        {
            authority.manifest.unattempted.remove(index);
            authority.manifest.sequence = authority
                .manifest
                .sequence
                .checked_add(1)
                .ok_or("catalogue sequence exhausted")?;
            authority.save()?;
        }
    }
    advance(env, scope, CataloguePhase::Attempted, None, None, None)
}
/// Called before exchange exclusion may retire. Failure keeps the original
/// PreparedGrant/response/Scope carrier, never a string-only completed result.
pub(super) fn received(
    env: &Env,
    scope: &CatalogueScope,
    store: &Store,
    record: &Record,
) -> Result<(), String> {
    if record.request != *scope.request()
        || record.catalogue_scope.as_ref() != Some(scope)
        || store.catalogue_scope.as_ref() != Some(scope)
    {
        return Err("catalogue response belongs to another operation".into());
    }
    match record.phase {
        Phase::VerifiedResponse => {
            store.verify_origin(record)?;
            let reference = store.issuance(record)?;
            let facts = LineageFacts::from_verified(record)?;
            let independent = match &facts {
                Some(facts) => !Inventory::read(env, Some(scope))?.refuses(facts),
                None => false,
            };
            // Keep verified supported-realm facts even when they conflict.
            // They deny the overlapping group without erasing its history or
            // treating unrelated proven principals as unknown.
            let phase = if independent && facts.is_some() {
                CataloguePhase::Received
            } else {
                CataloguePhase::Quarantined
            };
            advance(
                env,
                scope,
                phase,
                Some(reference),
                facts,
                Some(aterm_codec::hex::encode_lower(
                    &store
                        .journal
                        .sealed_digest()
                        .ok_or("catalogue response absent")?,
                )),
            )
        }
        Phase::NotDispatched if store.authenticates_no_dispatch(record) => {
            advance(env, scope, CataloguePhase::NotDispatched, None, None, None)
        }
        Phase::RejectedResponse if store.authenticates_rejection(record) => advance(
            env,
            scope,
            CataloguePhase::Rejected,
            None,
            None,
            record
                .rejection
                .as_ref()
                .map(|evidence| evidence.body_digest.clone()),
        ),
        Phase::UnverifiedResponse => advance(
            env,
            scope,
            CataloguePhase::Quarantined,
            None,
            None,
            Some(aterm_codec::hex::encode_lower(
                &store
                    .journal
                    .sealed_digest()
                    .ok_or("catalogue response absent")?,
            )),
        ),
        _ => Err("catalogue response unavailable".into()),
    }
}

pub(super) fn existing_scope(env: &Env, id: &str) -> Result<Option<CatalogueScope>, String> {
    let source_store = env
        .accounts_store_for(crate::harness::accounts::Cli::Codex)
        .ok_or("catalogue source unavailable")?;
    let (logins, warning) = source_store.load_logins(env.now)?;
    if warning.is_some() {
        return Err("catalogue source uncertain".into());
    }
    if !logins.enrollment.catalogue_required_id(id) {
        return Ok(None);
    }
    let authority = AuthorityObservation::read(env)?;
    let scope = authority
        .manifest
        .members
        .get(id)
        .ok_or("global catalogue membership lost")?;
    if std::fs::canonicalize(&source_store.dir).ok().as_ref() != Some(&scope.source.store)
        || logins.enrollment.catalogue_staging(scope.request())? != scope.source
    {
        return Err("global catalogue source changed".into());
    }
    authority.current()?;
    Ok(Some(scope.clone()))
}

/// An authenticated observation, not a publication/refresh permission. A caller
/// must retain its real exchange/resource leases and revalidate before effect.
struct InventoryMember {
    scope: CatalogueScope,
    published: Option<GrantBinding>,
    phase: CataloguePhase,
    uncertainty: refresh::LineageUncertainty,
    facts: LineageFacts,
    tokens: std::collections::BTreeSet<String>,
    pending: bool,
}
struct Inventory {
    members: Vec<InventoryMember>,
}
struct InventorySnapshot {
    members: Vec<InventoryMember>,
}
impl InventorySnapshot {
    fn read(env: &Env, own: Option<&CatalogueScope>) -> Result<Self, String> {
        let authority = AuthorityObservation::read(env)?;
        let scopes: Vec<_> = authority.manifest.members.values().cloned().collect();
        let unattempted = authority.manifest.unattempted.clone();
        let sequence = authority.manifest.sequence;
        let namespace = authority.manifest.namespace.clone();
        // Membership is not inferred only from a restored manifest. Actual
        // private per-operation directories must also be accounted for.
        for entry in
            std::fs::read_dir(&authority.directory).map_err(|_| "catalogue entries unavailable")?
        {
            let entry = entry.map_err(|_| "catalogue entry unavailable")?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "catalogue entry invalid")?;
            // The original owner registry owns this one exact kernel-lock
            // basename beside operation directories. It is not an owner entry.
            if name == "registry.lock" {
                let (file, _) = platform::open_private(&authority.directory, &name)
                    .map_err(|_| "catalogue registry lock unavailable")?;
                if !astream_sys::file_matches_path(&file, &entry.path())
                    .map_err(|_| "catalogue registry lock unavailable")?
                {
                    return Err("catalogue registry lock changed".into());
                }
                continue;
            }
            if !valid_hex(&name, 32) || !authority.manifest.members.contains_key(&name) {
                return Err("catalogue contains unaccounted owner custody".into());
            }
            platform::check_private_dir(&entry.path())
                .map_err(|_| "catalogue owner directory unavailable")?;
        }
        authority.current()?;
        drop(authority);
        let mut members = Vec::new();
        for scope in scopes {
            // The current issuer is already held by the caller. This exemption
            // never exempts a different issuance/reference or source store.
            if own == Some(&scope) {
                continue;
            }
            let mut source_env = env.clone();
            source_env.state = scope
                .source
                .store
                .parent()
                .ok_or("catalogue source has no state")?
                .to_path_buf();
            let canonical = source_env
                .accounts_store_for(crate::harness::accounts::Cli::Codex)
                .ok_or("catalogue source unavailable")?;
            if std::fs::canonicalize(&canonical.dir).ok().as_ref() != Some(&scope.source.store)
                || !scope.source.store_current()
            {
                return Err("catalogue source changed".into());
            }
            let (logins, warning) = canonical.load_logins(env.now)?;
            if warning.is_some() {
                return Err("catalogue source uncertain".into());
            }
            let state = logins.enrollment.catalogue_record_state(scope.request())?;
            if logins.enrollment.catalogue_staging(scope.request())? != scope.source {
                return Err("catalogue staging changed".into());
            }
            let claimed = EntryObservation::read(&source_env, &scope)?;
            let observed = claimed.witness()?;
            if state.witness.as_ref() != Some(&observed) {
                return Err("catalogue entry requires canonical reconciliation".into());
            }
            let entry = claimed.entry.as_ref().ok_or("catalogue entry absent")?;
            match entry.phase {
                CataloguePhase::Prepared | CataloguePhase::NotDispatched => {
                    if entry.phase == CataloguePhase::Prepared
                        && (!scope.guarded_preparation
                            || !unattempted.contains(&scope.request().id))
                    {
                        return Err("catalogue exchange requires reconciliation".into());
                    }
                    claimed.store.unchanged().map_err(str::to_owned)?;
                    continue;
                }
                CataloguePhase::Quarantined if entry.facts.is_some() => {}
                CataloguePhase::Attempted
                | CataloguePhase::Quarantined
                | CataloguePhase::Rejected => {
                    // A rejection is not proof that an older independent grant
                    // was unaffected; there is no provider lineage identifier.
                    return Err("catalogue has unresolved provider effects".into());
                }
                CataloguePhase::Received => {}
            }
            let reference = entry
                .reference
                .as_ref()
                .ok_or("catalogue issuance absent")?;
            if state
                .reference
                .as_ref()
                .is_some_and(|canonical| canonical != reference)
            {
                return Err("catalogue canonical issuance changed".into());
            }
            let issuer = issuer_observation::IssuerObservation::read(
                &source_env,
                scope.request(),
                reference,
            )?;
            let record = issuer.record();
            let facts = LineageFacts::from_verified(record)?
                .ok_or("catalogue provider facts unavailable")?;
            if entry.facts.as_ref() != Some(&facts)
                || entry.response_digest.as_ref() != Some(&reference.issuance_digest)
            {
                return Err("catalogue provider facts changed".into());
            }
            let mut tokens = std::collections::BTreeSet::from([facts.token.clone()]);
            let mut pending = entry.phase == CataloguePhase::Quarantined;
            let mut uncertainty = refresh::LineageUncertainty::None;
            if let Some(binding) = &state.published {
                if record
                    .response
                    .as_ref()
                    .is_some_and(|response| response.native_row.is_some())
                {
                    let proof = state
                        .publication
                        .as_ref()
                        .ok_or("catalogue publication proof absent")?;
                    issuer.verify_publication(proof)?;
                    if proof.binding() != binding
                        || proof.binding_fingerprint()
                            != super::super::codex_native_row::binding_digest(
                                &logins,
                                &binding.request.label,
                            )?
                    {
                        return Err("catalogue current publication binding changed".into());
                    }
                    let history = refresh::lineage_history(
                        &source_env,
                        binding,
                        record,
                        state
                            .owner_witness
                            .as_ref()
                            .ok_or("catalogue owner witness absent")?,
                    )?;
                    if history.uncertainty == refresh::LineageUncertainty::Global {
                        return Err("catalogue refresh outcome uncertain".into());
                    }
                    uncertainty = history.uncertainty;
                    tokens.extend(history.tokens);
                    pending |= history.pending;
                    issuer.current()?;
                } else {
                    return Err("catalogue publication lacks row authority".into());
                }
            } else {
                issuer.current()?;
            }
            claimed.store.unchanged().map_err(str::to_owned)?;
            members.push(InventoryMember {
                scope,
                published: state.published.clone(),
                phase: entry.phase,
                uncertainty,
                facts,
                tokens,
                pending,
            });
        }
        let authority = AuthorityObservation::read(env)?;
        authority.current()?;
        if authority.manifest.sequence != sequence || authority.manifest.namespace != namespace {
            return Err("catalogue membership changed during inventory".into());
        }
        Ok(Self { members })
    }
}
impl Inventory {
    /// Complete Code/publication view. A bound managed Attempted observation
    /// is not complete and cannot be reused as a weak Code admission mode.
    fn read(env: &Env, own: Option<&CatalogueScope>) -> Result<Self, String> {
        let snapshot = InventorySnapshot::read(env, own)?;
        if snapshot
            .members
            .iter()
            .any(|member| member.uncertainty != refresh::LineageUncertainty::None)
        {
            return Err("complete catalogue outcome unavailable".into());
        }
        Ok(Self {
            members: snapshot.members,
        })
    }
    fn refuses_alias(&self, candidate: &LineageFacts) -> bool {
        self.members.iter().any(|member| {
            candidate.overlaps_principal(&member.facts)
                || candidate.same_token(&member.facts)
                || member.tokens.contains(&candidate.token)
        })
    }
    fn refuses(&self, candidate: &LineageFacts) -> bool {
        self.members.iter().any(|member| member.pending) || self.refuses_alias(candidate)
    }
}

/// Called only by the owner-private exact-response initializer under its actual
/// derived publication permit. Historical issuances remain recovery-only and
/// are never silently enrolled into the new machine-wide catalogue.
pub(super) fn require_publication(
    env: &Env,
    binding: &GrantBinding,
    identity: &Identity,
    document: &[u8],
) -> Result<(), String> {
    let Some(scope) = existing_scope(env, &binding.request.id)? else {
        return Ok(());
    };
    let claimed = ClaimedEntry::open(env, &scope, false)?;
    let entry = claimed.entry.as_ref().ok_or("catalogue response absent")?;
    let witness = with_anchor_observation(env, &scope)?;
    if witness.as_ref() != Some(&claimed.witness()?)
        || entry.phase != CataloguePhase::Received
        || entry.reference.as_ref() != Some(&binding.reference)
    {
        return Err("catalogue response is not independently publishable".into());
    }
    let facts = entry
        .facts
        .as_ref()
        .ok_or("catalogue provider facts absent")?;
    if facts.principal != *identity
        || facts.token != codex_app_login::owned_refresh_token_digest(document)?
        || Inventory::read(env, Some(&scope))?.refuses(facts)
    {
        return Err("catalogue lineage overlaps another issuance".into());
    }
    claimed.store.unchanged().map_err(str::to_owned)
}
fn with_anchor_observation(
    env: &Env,
    scope: &CatalogueScope,
) -> Result<Option<CatalogueWitness>, String> {
    let store = env
        .accounts_store_for(crate::harness::accounts::Cli::Codex)
        .ok_or("catalogue source unavailable")?;
    let (logins, warning) = store.load_logins(env.now)?;
    if warning.is_some() || logins.enrollment.catalogue_staging(scope.request())? != scope.source {
        return Err("catalogue source uncertain".into());
    }
    logins.enrollment.catalogue_witness(scope.request())
}

#[cfg(all(test, unix))]
#[path = "codex_grant_catalogue_tests.rs"]
mod tests;

/// A negative legacy-reader marker, never source or lineage authority. Its
/// publication remains inside the exact C1/Auth callback and precedes canonical
/// binding visibility. Uncertain durability refuses that binding publication.
pub(super) fn mark_owner(
    env: &Env,
    binding: &GrantBinding,
    witness: &codex_app_login::StagedWitness,
    auth: &codex_app_login::AuthLock,
    document: &[u8],
) -> Result<(), String> {
    if existing_scope(env, &binding.request.id)?.is_none() {
        return Ok(());
    }
    if !codex_app_login::retained_document_current(witness, auth, document) {
        return Err("catalogue owner document changed".into());
    }
    #[cfg(unix)]
    {
        let (path, _, _) = witness.coordinates()?;
        let parent = path.parent().ok_or("catalogue owner parent absent")?;
        let name = format!(
            "{}{}",
            path.file_name()
                .and_then(|name| name.to_str())
                .ok_or("catalogue owner basename unavailable")?,
            codex_app_login::OWNER_MARKER_SUFFIX
        );
        match std::fs::symlink_metadata(parent.join(&name)) {
            Ok(_) => {
                let bytes = platform::read_single_link_private(parent, &name, 64)
                    .map_err(|_| "catalogue owner marker unavailable")?;
                if bytes.as_slice() != codex_app_login::OWNER_MARKER_TEXT {
                    return Err("catalogue owner marker changed".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                aterm_uds::private_file::write_private_flushed(
                    parent,
                    &name,
                    codex_app_login::OWNER_MARKER_TEXT,
                    aterm_durable::Flush::Durable,
                )
                .map_err(|_| "catalogue owner marker durability uncertain")?;
            }
            Err(_) => return Err("catalogue owner marker unavailable".into()),
        }
        let (file, _) = platform::open_private(parent, &name)
            .map_err(|_| "catalogue owner marker unavailable")?;
        aterm_durable::Flush::Durable
            .file(&file)
            .map_err(|_| "catalogue owner marker durability uncertain")?;
        aterm_durable::Flush::Durable
            .dir(parent)
            .map_err(|_| "catalogue owner marker durability uncertain")?;
        if !astream_sys::file_matches_path(&file, &parent.join(&name))
            .map_err(|_| "catalogue owner marker unavailable")?
        {
            return Err("catalogue owner marker changed".into());
        }
        let bytes = platform::read_single_link_private(parent, &name, 64)
            .map_err(|_| "catalogue owner marker unavailable")?;
        if bytes.as_slice() != codex_app_login::OWNER_MARKER_TEXT
            || !astream_sys::file_matches_path(&file, &parent.join(&name))
                .map_err(|_| "catalogue owner marker unavailable")?
            || !codex_app_login::retained_document_current(witness, auth, document)
        {
            return Err("catalogue owner publication changed".into());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Err("catalogue owner marker unsupported".into())
    }
}

pub(super) enum RefreshSourceStatus {
    NoKnownAlias,
    Conflicted,
}

/// A distinct view of already-owned managed refreshes, requiring a sealed
/// observation of actual held resources. Unbound Code never enters this type.
struct OwnedRefreshInventory {
    members: Vec<InventoryMember>,
    own: usize,
}
impl OwnedRefreshInventory {
    fn read(env: &Env, source: &refresh::OwnedRefreshInventorySource<'_>) -> Result<Self, String> {
        source.current()?;
        let snapshot = InventorySnapshot::read(env, None)?;
        let own = snapshot
            .members
            .iter()
            .position(|member| {
                member.published.as_ref() == Some(source.binding())
                    && member.scope.request() == &source.binding().request
            })
            .ok_or("owned refresh membership unavailable")?;
        let current = &snapshot.members[own];
        let token = codex_app_login::owned_refresh_token_digest(source.document())?;
        if current.phase != CataloguePhase::Received
            || current.facts.principal != *source.identity()
            || !current.tokens.contains(&token)
        {
            return Err("owned refresh source changed".into());
        }
        source.current()?;
        Ok(Self {
            members: snapshot.members,
            own,
        })
    }
    fn conflicted(&self) -> bool {
        let own = &self.members[self.own];
        self.members.iter().enumerate().any(|(index, other)| {
            index != self.own
                && (own.facts.overlaps_principal(&other.facts)
                    || !own.tokens.is_disjoint(&other.tokens))
        })
    }
}

pub(super) fn assess_owned_refresh(
    env: &Env,
    source: &refresh::OwnedRefreshInventorySource<'_>,
) -> Result<RefreshSourceStatus, String> {
    let inventory = OwnedRefreshInventory::read(env, source)?;
    source.current()?;
    Ok(if inventory.conflicted() {
        RefreshSourceStatus::Conflicted
    } else {
        RefreshSourceStatus::NoKnownAlias
    })
}

/// Deny historical per-state issuances at consumer-delivery boundaries. This
/// checks existing participation only; it does not mint a source/actor lease.
pub(super) fn require_participant(
    env: &Env,
    binding: &GrantBinding,
    identity: &Identity,
) -> Result<(), String> {
    let scope = existing_scope(env, &binding.request.id)?
        .ok_or("historical owner requires authenticated migration before delivery")?;
    let claimed = EntryObservation::read(env, &scope)?;
    let entry = claimed.entry.as_ref().ok_or("global owner entry absent")?;
    if entry.phase != CataloguePhase::Received
        || entry.reference.as_ref() != Some(&binding.reference)
        || entry.facts.as_ref().map(|facts| &facts.principal) != Some(identity)
        || with_anchor_observation(env, &scope)?.as_ref() != Some(&claimed.witness()?)
    {
        return Err("global owner participation is uncertain".into());
    }
    claimed.store.unchanged().map_err(str::to_owned)
}

/// Read-only test observation; creates no credential or publication capability.
#[cfg(all(test, unix))]
pub(super) fn inventory_current_for_test(env: &Env) -> Result<(), String> {
    Inventory::read(env, None).map(|_| ())
}
